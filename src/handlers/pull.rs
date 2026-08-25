use eyre::{Result, WrapErr, bail};
use serde::Serialize;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use crate::registry;
use crate::sandbox::has_changes;
use crate::session::{ChangeKind, change_kind, review, summarize};

use super::common;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PullOptions {
    pub(crate) auto_yes: bool,
    pub(crate) auto_no: bool,
    pub(crate) dry_run: bool,
    pub(crate) json: bool,
}

#[derive(Debug)]
pub(crate) struct PullOutcome {
    pub(crate) target: PathBuf,
    pub(crate) applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewDecision {
    Apply,
    Decline,
    Prompt,
}

pub(crate) fn pull(
    session_id: Option<&str>,
    target_override: Option<&Path>,
    options: PullOptions,
) -> Result<()> {
    let record = common::resolve_session(session_id)?;
    let outcome = pull_workspace(&record, target_override, options)?;
    tracing::debug!(
        target = %outcome.target.display(),
        applied = outcome.applied,
        "pull finished"
    );
    Ok(())
}

/// Applies the live sandbox workspace back onto a target directory while the
/// session keeps running. Mirrors the close-time flow: detect changes,
/// summarize, optionally review, then mirror the tree one way. The sandbox is
/// never modified, so pulls can repeat as the agent keeps working.
#[tracing::instrument(skip_all, fields(session = %session.id))]
fn pull_workspace(
    session: &registry::SessionRecord,
    target_override: Option<&Path>,
    options: PullOptions,
) -> Result<PullOutcome> {
    let target = resolve_pull_target(session, target_override)?;
    let sandbox = session.sandbox_path.as_path();
    if !sandbox.is_dir() {
        bail!("session workspace {} no longer exists", sandbox.display());
    }
    let changed = has_changes(&target, sandbox, &session.unmanaged)?;
    if !options.json && !changed.is_empty() {
        summarize(&changed, &target, sandbox);
    }
    let mut applied = false;
    if !changed.is_empty() && !options.dry_run {
        match decide_review(options.auto_yes, options.auto_no, io::stdin().is_terminal())? {
            ReviewDecision::Apply => {
                crate::sandbox::apply_tree(sandbox, &target, &session.unmanaged)?;
                applied = true;
            }
            ReviewDecision::Decline => {}
            ReviewDecision::Prompt => {
                if options.json {
                    bail!("--json cannot prompt for confirmation; pass --yes or --no")
                }
                let mut session_view = None;
                applied = review(
                    &changed,
                    &target,
                    sandbox,
                    session.diff_viewer.as_deref(),
                    &session.unmanaged,
                    &mut session_view,
                )?;
                if applied {
                    crate::sandbox::apply_tree(sandbox, &target, &session.unmanaged)?;
                }
            }
        }
    }
    emit_pull_report(session, &target, sandbox, &changed, applied, options)?;
    Ok(PullOutcome { target, applied })
}

fn decide_review(auto_yes: bool, auto_no: bool, stdin_is_tty: bool) -> Result<ReviewDecision> {
    if auto_yes {
        Ok(ReviewDecision::Apply)
    } else if auto_no {
        Ok(ReviewDecision::Decline)
    } else if stdin_is_tty {
        Ok(ReviewDecision::Prompt)
    } else {
        bail!("stdin is not interactive; pass --yes or --no")
    }
}

fn resolve_pull_target(
    session: &registry::SessionRecord,
    override_path: Option<&Path>,
) -> Result<PathBuf> {
    let Some(path) = override_path else {
        let target = session.repo_path.clone();
        if !target.is_dir() {
            bail!("repository {} no longer exists", target.display());
        }
        return Ok(target);
    };
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let cwd = std::env::current_dir().wrap_err("cannot determine current directory")?;
        cwd.join(path)
    };
    let resolved = fs::canonicalize(&candidate)
        .wrap_err_with(|| format!("cannot access {}", candidate.display()))?;
    if !resolved.is_dir() {
        bail!("{} is not a directory", resolved.display());
    }
    Ok(resolved)
}

#[derive(Serialize)]
struct PullReport {
    session: String,
    target: PathBuf,
    applied: bool,
    changed: Vec<PullChangedPath>,
}

#[derive(Serialize)]
struct PullChangedPath {
    path: PathBuf,
    kind: ChangeKind,
}

fn build_pull_report(
    session_id: &str,
    sandbox: &Path,
    target: &Path,
    changed: &[PathBuf],
    applied: bool,
) -> PullReport {
    PullReport {
        session: session_id.to_string(),
        target: target.to_path_buf(),
        applied,
        changed: changed
            .iter()
            .map(|relative| PullChangedPath {
                path: relative.clone(),
                kind: change_kind(target, sandbox, relative),
            })
            .collect(),
    }
}

fn emit_pull_report(
    session: &registry::SessionRecord,
    target: &Path,
    sandbox: &Path,
    changed: &[PathBuf],
    applied: bool,
    options: PullOptions,
) -> Result<()> {
    if options.json {
        let report = build_pull_report(&session.id, sandbox, target, changed, applied);
        let rendered =
            serde_json::to_string_pretty(&report).wrap_err("cannot serialize pull report")?;
        println!("{rendered}");
        return Ok(());
    }
    if applied {
        println!("pulled {} files into {}", changed.len(), target.display());
    } else if changed.is_empty() {
        println!("no changes to pull");
    } else if options.dry_run {
        println!("dry run: nothing applied");
    } else {
        println!("no changes applied");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::TempDir;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn pull_record(repo: &Path, sandbox: &Path, unmanaged: &[&str]) -> registry::SessionRecord {
        registry::SessionRecord {
            id: "test-0001".to_string(),
            container_name: "pithos-test-0001".to_string(),
            sandbox_path: sandbox.to_path_buf(),
            repo_path: repo.to_path_buf(),
            image_tag: "localhost/pithos-opencode:latest".to_string(),
            workspace: "/workspace".to_string(),
            user: "1000:1000".to_string(),
            unmanaged: unmanaged.iter().map(|path| path.to_string()).collect(),
            diff_viewer: None,
            pid: 0,
            started_at: 0,
        }
    }

    #[test]
    fn decide_review_requires_explicit_flag_without_tty() {
        assert_eq!(
            decide_review(true, false, false).unwrap(),
            ReviewDecision::Apply
        );
        assert_eq!(
            decide_review(false, true, false).unwrap(),
            ReviewDecision::Decline
        );
        assert_eq!(
            decide_review(false, false, true).unwrap(),
            ReviewDecision::Prompt
        );
        let error = decide_review(false, false, false).unwrap_err().to_string();
        assert!(error.contains("--yes"));
    }

    #[test]
    fn pull_applies_mirror_semantics_respecting_unmanaged() {
        let repo = TempDir::create("pithos-pull-repo").unwrap();
        let sandbox = TempDir::create("pithos-pull-sandbox").unwrap();
        write(&repo.0, "modified.txt", "host");
        write(&sandbox.0, "modified.txt", "sandbox");
        write(&repo.0, "removed.txt", "gone");
        write(&sandbox.0, "added.txt", "new");
        write(&repo.0, "scratch/cache.txt", "host cache");
        write(&sandbox.0, "scratch/cache.txt", "sandbox cache");
        let record = pull_record(&repo.0, &sandbox.0, &["scratch"]);

        let outcome = pull_workspace(
            &record,
            None,
            PullOptions {
                auto_yes: true,
                auto_no: false,
                dry_run: false,
                json: false,
            },
        )
        .unwrap();

        assert!(outcome.applied);
        assert_eq!(
            fs::read_to_string(repo.0.join("modified.txt")).unwrap(),
            "sandbox"
        );
        assert_eq!(fs::read_to_string(repo.0.join("added.txt")).unwrap(), "new");
        assert!(!repo.0.join("removed.txt").exists());
        assert_eq!(
            fs::read_to_string(repo.0.join("scratch/cache.txt")).unwrap(),
            "host cache"
        );
        assert_eq!(
            fs::read_to_string(sandbox.0.join("modified.txt")).unwrap(),
            "sandbox"
        );
    }

    #[test]
    fn pull_dry_run_leaves_both_trees_untouched() {
        let repo = TempDir::create("pithos-pull-dry-repo").unwrap();
        let sandbox = TempDir::create("pithos-pull-dry-sandbox").unwrap();
        write(&repo.0, "file.txt", "host");
        write(&sandbox.0, "file.txt", "sandbox");
        write(&sandbox.0, "added.txt", "new");
        let record = pull_record(&repo.0, &sandbox.0, &[]);

        let outcome = pull_workspace(
            &record,
            None,
            PullOptions {
                auto_yes: true,
                auto_no: false,
                dry_run: true,
                json: false,
            },
        )
        .unwrap();

        assert!(!outcome.applied);
        assert_eq!(fs::read_to_string(repo.0.join("file.txt")).unwrap(), "host");
        assert!(!repo.0.join("added.txt").exists());
        assert_eq!(has_changes(&repo.0, &sandbox.0, &[]).unwrap().len(), 2);
    }

    #[test]
    fn pull_targets_override_directory() {
        let sandbox = TempDir::create("pithos-pull-target-sandbox").unwrap();
        let checkout = TempDir::create("pithos-pull-checkout").unwrap();
        fs::create_dir_all(checkout.0.join("nested/deeper")).unwrap();
        write(&sandbox.0, "sub/file.txt", "from sandbox");
        write(&checkout.0, "nested/deeper/stale.txt", "delete me");
        let mut record = pull_record(&checkout.0, &sandbox.0, &[]);
        record.repo_path = checkout.0.join("elsewhere");

        let outcome = pull_workspace(
            &record,
            Some(&checkout.0.join("nested").join(".")),
            PullOptions {
                auto_yes: true,
                auto_no: false,
                dry_run: false,
                json: false,
            },
        )
        .unwrap();

        assert!(outcome.applied);
        assert_eq!(outcome.target, checkout.0.join("nested"));
        assert_eq!(
            fs::read_to_string(outcome.target.join("sub/file.txt")).unwrap(),
            "from sandbox"
        );
        assert!(!outcome.target.join("deeper/stale.txt").exists());
        assert!(!checkout.0.join("elsewhere").is_file());
    }

    #[test]
    fn pull_rejects_missing_targets() {
        let repo = TempDir::create("pithos-pull-missing-repo").unwrap();
        let sandbox = TempDir::create("pithos-pull-missing-sandbox").unwrap();
        let missing_repo_record = {
            let mut record = pull_record(&repo.0, &sandbox.0, &[]);
            record.repo_path = repo.0.join("gone");
            record
        };
        assert!(
            pull_workspace(
                &missing_repo_record,
                None,
                PullOptions {
                    auto_yes: true,
                    auto_no: false,
                    dry_run: false,
                    json: false
                }
            )
            .unwrap_err()
            .to_string()
            .contains("no longer exists")
        );

        let record = pull_record(&repo.0, &sandbox.0, &[]);
        assert!(
            pull_workspace(
                &record,
                Some(&repo.0.join("missing-dir")),
                PullOptions {
                    auto_yes: true,
                    auto_no: false,
                    dry_run: false,
                    json: false
                }
            )
            .unwrap_err()
            .to_string()
            .contains("cannot access")
        );
    }

    #[test]
    fn pull_report_classifies_kinds() {
        let repo = TempDir::create("pithos-pull-report-repo").unwrap();
        let sandbox = TempDir::create("pithos-pull-report-sandbox").unwrap();
        write(&repo.0, "modified.txt", "host");
        write(&sandbox.0, "modified.txt", "sandbox");
        write(&repo.0, "deleted.txt", "gone");
        write(&sandbox.0, "added.txt", "new");
        let changed = has_changes(&repo.0, &sandbox.0, &[]).unwrap();

        let report = build_pull_report("sess-0001", &sandbox.0, &repo.0, &changed, false);

        assert_eq!(report.session, "sess-0001");
        assert!(!report.applied);
        let kinds: Vec<(String, ChangeKind)> = report
            .changed
            .into_iter()
            .map(|entry| (entry.path.display().to_string(), entry.kind))
            .collect();
        assert!(kinds.contains(&(String::from("added.txt"), ChangeKind::Added)));
        assert!(kinds.contains(&(String::from("modified.txt"), ChangeKind::Modified)));
        assert!(kinds.contains(&(String::from("deleted.txt"), ChangeKind::Deleted)));
    }
}

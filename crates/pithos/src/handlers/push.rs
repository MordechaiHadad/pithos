use eyre::{Result, WrapErr, bail};
use serde::Serialize;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use crate::registry;
use crate::sandbox::{CopyMethod, has_changes};
use crate::session::{ChangeKind, change_kind, review_with_prompt, summarize};
use crate::snapshot;
use crate::workspace::parse_override;

use super::common;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PushOptions {
    pub(crate) auto_yes: bool,
    pub(crate) auto_no: bool,
    pub(crate) dry_run: bool,
    pub(crate) json: bool,
}

#[derive(Debug)]
pub(crate) struct PushOutcome {
    pub(crate) source: PathBuf,
    pub(crate) applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewDecision {
    Apply,
    Decline,
    Prompt,
}

pub(crate) fn push(
    session_id: Option<&str>,
    source_override: Option<&Path>,
    options: PushOptions,
) -> Result<()> {
    let record = common::resolve_session(session_id)?;
    let outcome = push_workspace(&record, source_override, options)?;
    tracing::debug!(
        source = %outcome.source.display(),
        applied = outcome.applied,
        "push finished"
    );
    Ok(())
}

/// Applies the host repository state onto the live sandbox workspace while
/// the session keeps running. Mirrors the pull flow in reverse: detect
/// changes, summarize, optionally review, then mirror the tree one way from
/// the host source into the sandbox. The host directory is never modified.
#[tracing::instrument(skip_all, fields(session = %session.identity.id))]
fn push_workspace(
    session: &registry::SessionRecord,
    source_override: Option<&Path>,
    options: PushOptions,
) -> Result<PushOutcome> {
    let source = resolve_push_source(session, source_override)?;
    let sandbox = session.paths.sandbox_path.as_path();
    if !sandbox.is_dir() {
        bail!("session workspace {} no longer exists", sandbox.display());
    }
    let changed = detect_changes(&source, sandbox, session)?;
    if !options.json && !changed.is_empty() {
        summarize(&changed, sandbox, &source);
    }
    let method = copy_method_for_session(session);
    let mut applied = false;
    if !changed.is_empty() && !options.dry_run {
        match decide_review(options.auto_yes, options.auto_no, io::stdin().is_terminal())? {
            ReviewDecision::Apply => {
                apply_with_progress(&source, sandbox, &session.options.unmanaged, method)?;
                applied = true;
            }
            ReviewDecision::Decline => {}
            ReviewDecision::Prompt => {
                if options.json {
                    bail!("--json cannot prompt for confirmation; pass --yes or --no")
                }
                let mut session_view = None;
                applied = review_with_prompt(
                    &changed,
                    sandbox,
                    &source,
                    session.options.diff_viewer.as_deref(),
                    &session.options.unmanaged,
                    &mut session_view,
                    "Apply host changes to the sandbox workspace? [y]es [v]iew diff [n]o: ",
                )?;
                if applied {
                    apply_with_progress(&source, sandbox, &session.options.unmanaged, method)?;
                }
            }
        }
    }
    emit_push_report(session, &source, sandbox, &changed, applied, options)?;
    if applied {
        let _ = update_snapshot(session, sandbox);
    }
    Ok(PushOutcome { source, applied })
}

fn detect_changes(
    source: &Path,
    sandbox: &Path,
    session: &registry::SessionRecord,
) -> Result<Vec<PathBuf>> {
    if let Ok(Some(changed)) = snapshot::try_has_changes_via_snapshot(
        source,
        sandbox,
        &session.options.unmanaged,
        session.options.strategy.as_deref(),
        &session.identity.id,
    ) {
        tracing::debug!(changed = changed.len(), "snapshot push detection succeeded");
        return Ok(changed);
    }
    tracing::debug!("snapshot fallback to full scan");
    let changed = has_changes(source, sandbox, &session.options.unmanaged)?;
    tracing::debug!(changed = changed.len(), "full scan push detection finished");
    Ok(changed)
}

fn update_snapshot(session: &registry::SessionRecord, sandbox: &Path) -> Result<()> {
    let entries = snapshot::capture(sandbox, &session.options.unmanaged)?;
    snapshot::save_snapshot(
        &session.identity.id,
        entries,
        &session.options.unmanaged,
        session.options.strategy.as_deref(),
    )?;
    Ok(())
}

fn copy_method_for_session(session: &registry::SessionRecord) -> CopyMethod {
    match session.options.strategy.as_deref() {
        Some(label) => match parse_override(Some(label)) {
            Ok(Some(strategy)) => strategy.copy_method(),
            _ => match label {
                "reflink" => CopyMethod::Reflink,
                "worktree" => CopyMethod::Copy,
                "copy" => CopyMethod::Copy,
                _ => CopyMethod::Copy,
            },
        },
        None => CopyMethod::Copy,
    }
}

fn apply_with_progress(
    source: &Path,
    sandbox: &Path,
    unmanaged: &[String],
    method: CopyMethod,
) -> Result<()> {
    if crate::utils::progress::is_progress_enabled() {
        crate::utils::progress::with_apply_progress(|progress| {
            crate::sandbox::apply_tree(source, sandbox, unmanaged, method, Some(progress))
        })
    } else {
        crate::sandbox::apply_tree(source, sandbox, unmanaged, method, None)
    }
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

fn resolve_push_source(
    session: &registry::SessionRecord,
    override_path: Option<&Path>,
) -> Result<PathBuf> {
    let Some(path) = override_path else {
        let source = session.paths.repo_path.clone();
        if !source.is_dir() {
            bail!("repository {} no longer exists", source.display());
        }
        return Ok(source);
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
struct PushReport {
    session: String,
    source: PathBuf,
    target: PathBuf,
    applied: bool,
    changed: Vec<PushChangedPath>,
}

#[derive(Serialize)]
struct PushChangedPath {
    path: PathBuf,
    kind: ChangeKind,
}

fn build_push_report(
    session_id: &str,
    sandbox: &Path,
    source: &Path,
    changed: &[PathBuf],
    applied: bool,
) -> PushReport {
    PushReport {
        session: session_id.to_string(),
        source: source.to_path_buf(),
        target: sandbox.to_path_buf(),
        applied,
        changed: changed
            .iter()
            .map(|relative| PushChangedPath {
                path: relative.clone(),
                kind: change_kind(sandbox, source, relative),
            })
            .collect(),
    }
}

fn emit_push_report(
    session: &registry::SessionRecord,
    source: &Path,
    sandbox: &Path,
    changed: &[PathBuf],
    applied: bool,
    options: PushOptions,
) -> Result<()> {
    if options.json {
        let report = build_push_report(&session.identity.id, sandbox, source, changed, applied);
        let rendered =
            serde_json::to_string_pretty(&report).wrap_err("cannot serialize push report")?;
        println!("{rendered}");
        return Ok(());
    }
    if applied {
        tracing::info!(count = changed.len(), source = %source.display(), "pushed files");
    } else if changed.is_empty() {
        tracing::info!("no changes to push");
    } else if options.dry_run {
        tracing::info!("dry run: nothing applied");
    } else {
        tracing::info!("no changes applied");
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

    fn push_record(source: &Path, sandbox: &Path, unmanaged: &[&str]) -> registry::SessionRecord {
        registry::SessionRecord {
            identity: registry::SessionIdentity {
                id: "test-0001".to_string(),
                container_name: "pithos-test-0001".to_string(),
            },
            paths: registry::SessionPaths {
                sandbox_path: sandbox.to_path_buf(),
                repo_path: source.to_path_buf(),
            },
            runtime: registry::SessionRuntime {
                image_tag: "localhost/pithos-opencode:latest".to_string(),
                workspace: "/workspace".to_string(),
                user: "1000:1000".to_string(),
            },
            options: registry::SessionOptions {
                unmanaged: unmanaged.iter().map(|path| path.to_string()).collect(),
                ..Default::default()
            },
            lifecycle: registry::SessionLifecycle {
                pid: 0,
                started_at: 0,
            },
        }
    }

    fn push_options() -> PushOptions {
        PushOptions {
            auto_yes: true,
            auto_no: false,
            dry_run: false,
            json: false,
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
    fn push_applies_mirror_semantics_respecting_unmanaged() {
        let source = TempDir::create("pithos-push-source").unwrap();
        let sandbox = TempDir::create("pithos-push-sandbox").unwrap();
        write(source.path(), "modified.txt", "host");
        write(sandbox.path(), "modified.txt", "sandbox");
        write(source.path(), "added.txt", "new");
        write(sandbox.path(), "removed.txt", "gone");
        write(source.path(), "scratch/cache.txt", "host cache");
        write(sandbox.path(), "scratch/cache.txt", "sandbox cache");
        let record = push_record(source.path(), sandbox.path(), &["scratch"]);

        let outcome = push_workspace(&record, None, push_options()).unwrap();

        assert!(outcome.applied);
        assert_eq!(
            fs::read_to_string(sandbox.path().join("modified.txt")).unwrap(),
            "host"
        );
        assert_eq!(
            fs::read_to_string(sandbox.path().join("added.txt")).unwrap(),
            "new"
        );
        assert!(!sandbox.path().join("removed.txt").exists());
        assert_eq!(
            fs::read_to_string(sandbox.path().join("scratch/cache.txt")).unwrap(),
            "sandbox cache"
        );
        assert_eq!(
            fs::read_to_string(source.path().join("modified.txt")).unwrap(),
            "host"
        );
    }

    #[test]
    fn push_dry_run_leaves_both_trees_untouched() {
        let source = TempDir::create("pithos-push-dry-source").unwrap();
        let sandbox = TempDir::create("pithos-push-dry-sandbox").unwrap();
        write(source.path(), "file.txt", "host");
        write(sandbox.path(), "file.txt", "sandbox");
        write(source.path(), "added.txt", "new");
        let record = push_record(source.path(), sandbox.path(), &[]);

        let outcome = push_workspace(
            &record,
            None,
            PushOptions {
                dry_run: true,
                ..push_options()
            },
        )
        .unwrap();

        assert!(!outcome.applied);
        assert_eq!(
            fs::read_to_string(sandbox.path().join("file.txt")).unwrap(),
            "sandbox"
        );
        assert!(!sandbox.path().join("added.txt").exists());
        assert_eq!(
            has_changes(source.path(), sandbox.path(), &[])
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn push_targets_override_directory_as_source() {
        let sandbox = TempDir::create("pithos-push-target-sandbox").unwrap();
        let checkout = TempDir::create("pithos-push-checkout").unwrap();
        fs::create_dir_all(checkout.path().join("nested/deeper")).unwrap();
        write(checkout.path(), "nested/sub/file.txt", "from host");
        write(sandbox.path(), "stale.txt", "delete me");
        let mut record = push_record(checkout.path(), sandbox.path(), &[]);
        record.paths.repo_path = checkout.path().join("elsewhere");

        let outcome = push_workspace(
            &record,
            Some(&checkout.path().join("nested").join(".")),
            push_options(),
        )
        .unwrap();

        assert!(outcome.applied);
        assert_eq!(outcome.source, checkout.path().join("nested"));
        assert_eq!(
            fs::read_to_string(sandbox.path().join("sub/file.txt")).unwrap(),
            "from host"
        );
        assert!(sandbox.path().join("deeper").is_dir());
        assert!(!sandbox.path().join("stale.txt").exists());
    }

    #[test]
    fn push_rejects_missing_sources() {
        let source = TempDir::create("pithos-push-missing-source").unwrap();
        let sandbox = TempDir::create("pithos-push-missing-sandbox").unwrap();
        let missing_source_record = {
            let mut record = push_record(source.path(), sandbox.path(), &[]);
            record.paths.repo_path = source.path().join("gone");
            record
        };
        assert!(
            push_workspace(&missing_source_record, None, push_options())
                .unwrap_err()
                .to_string()
                .contains("no longer exists")
        );

        let record = push_record(source.path(), sandbox.path(), &[]);
        assert!(
            push_workspace(
                &record,
                Some(&source.path().join("missing-dir")),
                push_options(),
            )
            .unwrap_err()
            .to_string()
            .contains("cannot access")
        );
    }

    #[test]
    fn push_report_classifies_kinds_from_sandbox_perspective() {
        let source = TempDir::create("pithos-push-report-source").unwrap();
        let sandbox = TempDir::create("pithos-push-report-sandbox").unwrap();
        write(source.path(), "modified.txt", "host");
        write(sandbox.path(), "modified.txt", "sandbox");
        write(source.path(), "added.txt", "new");
        write(sandbox.path(), "deleted.txt", "gone");
        let changed = has_changes(source.path(), sandbox.path(), &[]).unwrap();

        let report = build_push_report("sess-0001", sandbox.path(), source.path(), &changed, false);

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

    #[test]
    fn push_preserves_sandbox_gitdir() {
        let source = TempDir::create("pithos-push-git-source").unwrap();
        let sandbox = TempDir::create("pithos-push-git-sandbox").unwrap();
        write(source.path(), "file.txt", "host");
        write(sandbox.path(), "file.txt", "sandbox");
        fs::write(sandbox.path().join(".git"), "gitdir: /elsewhere").unwrap();
        let record = push_record(source.path(), sandbox.path(), &[]);

        push_workspace(&record, None, push_options()).unwrap();

        assert_eq!(
            fs::read_to_string(sandbox.path().join(".git")).unwrap(),
            "gitdir: /elsewhere"
        );
        assert_eq!(
            fs::read_to_string(sandbox.path().join("file.txt")).unwrap(),
            "host"
        );
    }
}

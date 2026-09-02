use eyre::{Result, WrapErr, bail};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::config::Config;
use crate::sandbox::{CopyMethod, TempDir, apply_tree};
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

pub(crate) fn change_kind(source: &Path, sandbox: &Path, relative: &Path) -> ChangeKind {
    let in_source = fs::symlink_metadata(source.join(relative)).is_ok();
    let in_sandbox = fs::symlink_metadata(sandbox.join(relative)).is_ok();
    match (in_source, in_sandbox) {
        (true, false) => ChangeKind::Deleted,
        (false, true) => ChangeKind::Added,
        _ => ChangeKind::Modified,
    }
}

fn change_counts(changed: &[PathBuf], source: &Path, sandbox: &Path) -> (usize, usize, usize) {
    let mut added = 0;
    let mut modified = 0;
    let mut deleted = 0;
    for relative in changed {
        match change_kind(source, sandbox, relative) {
            ChangeKind::Added => added += 1,
            ChangeKind::Modified => modified += 1,
            ChangeKind::Deleted => deleted += 1,
        }
    }
    (added, modified, deleted)
}

fn style_for_kind(kind: ChangeKind) -> console::Style {
    match kind {
        ChangeKind::Added => console::Style::new().green(),
        ChangeKind::Modified => console::Style::new().yellow(),
        ChangeKind::Deleted => console::Style::new().red(),
    }
}

pub(crate) fn summarize(changed: &[PathBuf], source: &Path, sandbox: &Path) {
    let (added, modified, deleted) = change_counts(changed, source, sandbox);
    let noun = if changed.len() == 1 { "file" } else { "files" };
    let mut breakdown = Vec::new();
    if added > 0 {
        breakdown.push(format!(
            "{}",
            console::style(format!("{added} added")).green()
        ));
    }
    if modified > 0 {
        breakdown.push(format!(
            "{}",
            console::style(format!("{modified} modified")).yellow()
        ));
    }
    if deleted > 0 {
        breakdown.push(format!(
            "{}",
            console::style(format!("{deleted} deleted")).red()
        ));
    }
    let mut line = format!("{} {noun} changed", changed.len());
    if !breakdown.is_empty() {
        line.push_str(&format!(" ({})", breakdown.join(", ")));
    }
    println!("{line}");
    for relative in changed.iter().take(20) {
        let kind = change_kind(source, sandbox, relative);
        let styled = style_for_kind(kind).apply_to(format!("  {}", relative.display()));
        println!("{styled}");
    }
    if changed.len() > 20 {
        println!("  ... and {} more", changed.len() - 20);
    }
}

pub(crate) fn review(
    changed: &[PathBuf],
    source: &Path,
    sandbox: &Path,
    diff_viewer: Option<&str>,
    unmanaged: &[String],
    session_view: &mut Option<TempDir>,
) -> Result<bool> {
    loop {
        print!("Apply changes to the host repository? [y]es [v]iew diff [n]o: ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            "v" | "view" => view_changes(
                changed,
                source,
                sandbox,
                diff_viewer,
                unmanaged,
                session_view,
            )?,
            _ => {}
        }
    }
}

fn view_changes(
    changed: &[PathBuf],
    source: &Path,
    sandbox: &Path,
    diff_viewer: Option<&str>,
    unmanaged: &[String],
    session_view: &mut Option<TempDir>,
) -> Result<()> {
    if let Some(viewer) = diff_viewer
        && source.join(".git").exists()
    {
        if session_view.is_none() {
            *session_view = Some(build_session_view(source, sandbox, unmanaged)?);
        }
        let view = session_view.as_ref().expect("session view built above");
        run_viewer(viewer, &view.path().join("repo"))
    } else {
        print_diff(changed, source, sandbox)
    }
}

fn build_session_view(source: &Path, sandbox: &Path, unmanaged: &[String]) -> Result<TempDir> {
    let temp = TempDir::create("pithos-session")?;
    let bundle_path = temp.path().join("session.bundle").display().to_string();
    let branch = current_branch(source)?;
    git_ok(source, &["bundle", "create", &bundle_path, &branch])
        .wrap_err("could not create session bundle")?;
    let repo_dir = temp.path().join("repo");
    let repo_path = repo_dir.display().to_string();
    git_ok(source, &["clone", &bundle_path, &repo_path])
        .wrap_err("could not clone session bundle")?;
    apply_tree(sandbox, &repo_dir, unmanaged, CopyMethod::Copy, None)?;
    Ok(temp)
}

pub(crate) fn unmanaged(config: &Config) -> Vec<String> {
    let mut paths = config.ephemeral.clone();
    paths.extend(config.ignore.iter().cloned());
    paths
}

fn current_branch(sandbox: &Path) -> Result<String> {
    let output = git(sandbox, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if branch.is_empty() || branch == "HEAD" {
        bail!("cannot bundle a detached HEAD")
    }
    Ok(branch)
}

fn run_viewer(viewer: &str, repo: &Path) -> Result<()> {
    let command = viewer.replace("{dir}", &repo.display().to_string());
    let status =
        crate::utils::platform::run_shell(&command).wrap_err("could not run diff_viewer")?;
    if !status.success() {
        eprintln!("diff_viewer exited with {status}");
    }
    Ok(())
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<Output> {
    tracing::trace!(args = args.join(" "), "running git");
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .wrap_err("could not run git")
}

/// Removes every configured remote from `repository`'s git config.
///
/// Sandboxes must not reach the network through the user's remotes. Safe to
/// call on any sandbox because each tier owns a private config file.
pub(crate) fn strip_remotes(repository: &Path) -> Result<()> {
    let output = git(repository, &["remote"])?;
    for name in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|name| !name.is_empty())
    {
        git_ok(repository, &["remote", "remove", name])
            .wrap_err_with(|| format!("could not remove remote {name}"))?;
    }
    Ok(())
}

pub(crate) fn git_ok(dir: &Path, args: &[&str]) -> Result<()> {
    let output = git(dir, args)?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn print_diff(changed: &[PathBuf], source: &Path, sandbox: &Path) -> Result<()> {
    let mut rendered = String::new();
    for relative in changed {
        let source_path = source.join(relative);
        let sandbox_path = sandbox.join(relative);
        let source_type = fs::symlink_metadata(&source_path)
            .ok()
            .map(|meta| meta.file_type());
        let sandbox_type = fs::symlink_metadata(&sandbox_path)
            .ok()
            .map(|meta| meta.file_type());
        let source_is_dir = source_type.as_ref().map(|t| t.is_dir()).unwrap_or(false);
        let sandbox_is_dir = sandbox_type.as_ref().map(|t| t.is_dir()).unwrap_or(false);
        let source_is_link = source_type
            .as_ref()
            .map(|t| t.is_symlink())
            .unwrap_or(false);
        let sandbox_is_link = sandbox_type
            .as_ref()
            .map(|t| t.is_symlink())
            .unwrap_or(false);

        if source_is_dir || sandbox_is_dir {
            let (status, kind) = if source_is_dir && sandbox_is_dir {
                ("changed directory", ChangeKind::Modified)
            } else if source_is_dir {
                ("removed directory", ChangeKind::Deleted)
            } else {
                ("added directory", ChangeKind::Added)
            };
            let line = format!("  {status}: {}\n", relative.display());
            rendered.push_str(&format!("{}", style_for_kind(kind).apply_to(line)));
        } else if source_is_link || sandbox_is_link {
            let target = |path: &Path, is_link: bool| {
                if is_link {
                    fs::read_link(path)
                        .ok()
                        .map(|target| target.display().to_string())
                } else {
                    None
                }
            };
            match (
                target(&source_path, source_is_link),
                target(&sandbox_path, sandbox_is_link),
            ) {
                (Some(old), Some(new)) => {
                    let line = format!(
                        "  symlink changed: {} ({} -> {})\n",
                        relative.display(),
                        old,
                        new
                    );
                    rendered.push_str(&format!(
                        "{}",
                        style_for_kind(ChangeKind::Modified).apply_to(line)
                    ));
                }
                (Some(old), None) => {
                    let line = format!("  removed symlink: {} (-> {})\n", relative.display(), old);
                    rendered.push_str(&format!(
                        "{}",
                        style_for_kind(ChangeKind::Deleted).apply_to(line)
                    ));
                }
                (None, Some(new)) => {
                    let line = format!("  added symlink: {} (-> {})\n", relative.display(), new);
                    rendered.push_str(&format!(
                        "{}",
                        style_for_kind(ChangeKind::Added).apply_to(line)
                    ));
                }
                _ => {
                    let line = format!("  symlink changed: {}\n", relative.display());
                    rendered.push_str(&format!(
                        "{}",
                        style_for_kind(ChangeKind::Modified).apply_to(line)
                    ));
                }
            }
        } else {
            match file_diff(source, sandbox, relative)? {
                Some(diff) => rendered.push_str(&diff),
                None => {
                    let kind = change_kind(source, sandbox, relative);
                    let line = format!("  {}: diff unavailable\n", relative.display());
                    rendered.push_str(&format!("{}", style_for_kind(kind).apply_to(line)));
                }
            }
        }
    }
    print!("{rendered}");
    Ok(())
}

fn file_diff(source: &Path, sandbox: &Path, relative: &Path) -> Result<Option<String>> {
    let source_path = source.join(relative);
    let sandbox_path = sandbox.join(relative);
    let (left, right) = match (
        fs::symlink_metadata(&source_path).is_ok(),
        fs::symlink_metadata(&sandbox_path).is_ok(),
    ) {
        (true, true) => (source_path, sandbox_path),
        (true, false) => (source_path, crate::utils::platform::null_device()),
        _ => (crate::utils::platform::null_device(), sandbox_path),
    };
    git_diff(&left, &right).map(|diff| diff.map(|diff| clean_headers(&diff, relative)))
}

fn git_diff(left: &Path, right: &Path) -> Result<Option<String>> {
    tracing::trace!(left = %left.display(), right = %right.display(), "running git diff");
    let output = Command::new("git")
        .args(["diff", "--no-index", "--no-color", "--"])
        .arg(left)
        .arg(right)
        .output()
        .wrap_err("could not run git diff")?;
    if matches!(output.status.code(), Some(0) | Some(1)) {
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    } else {
        Ok(None)
    }
}

fn clean_headers(diff: &str, relative: &Path) -> String {
    let relative = relative.display().to_string();
    let mut cleaned = String::new();
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            cleaned.push_str(&format!("diff --git a/{relative} b/{relative}\n"));
        } else if line.starts_with("--- ") && !line.starts_with("--- /dev/null") {
            cleaned.push_str(&format!("--- a/{relative}\n"));
        } else if line.starts_with("+++ ") && !line.starts_with("+++ /dev/null") {
            cleaned.push_str(&format!("+++ b/{relative}\n"));
        } else {
            cleaned.push_str(line);
            cleaned.push('\n');
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{CopyMethod, copy_tree, has_changes};

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn change_kind_classifies_add_modified_and_deleted() {
        let source = TempDir::create("pithos-test-kind-source").unwrap();
        let sandbox = TempDir::create("pithos-test-kind-sandbox").unwrap();
        write(source.path(), "modified.txt", "host");
        write(sandbox.path(), "modified.txt", "sandbox");
        write(source.path(), "deleted.txt", "gone");
        write(sandbox.path(), "added.txt", "new");

        assert_eq!(
            change_kind(source.path(), sandbox.path(), Path::new("added.txt")),
            ChangeKind::Added
        );
        assert_eq!(
            change_kind(source.path(), sandbox.path(), Path::new("modified.txt")),
            ChangeKind::Modified
        );
        assert_eq!(
            change_kind(source.path(), sandbox.path(), Path::new("deleted.txt")),
            ChangeKind::Deleted
        );
    }

    #[test]
    fn change_counts_tally_kinds() {
        let source = TempDir::create("pithos-test-count-source").unwrap();
        let sandbox = TempDir::create("pithos-test-count-sandbox").unwrap();
        write(source.path(), "modified.txt", "host");
        write(sandbox.path(), "modified.txt", "sandbox");
        write(source.path(), "deleted.txt", "gone");
        write(sandbox.path(), "added.txt", "new");
        write(sandbox.path(), "also-added.txt", "new");

        let changed = has_changes(source.path(), sandbox.path(), &[]).unwrap();
        assert_eq!(
            change_counts(&changed, source.path(), sandbox.path()),
            (2, 1, 1)
        );
    }

    #[test]
    fn clean_headers_rewrites_paths_to_relative() {
        let diff = "diff --git a/tmp/host/mod.txt b/tmp/sandbox/mod.txt\n\
                    index 94954ab..c152159 100644\n\
                    --- a/tmp/host/mod.txt\n\
                    +++ b/tmp/sandbox/mod.txt\n\
                    @@ -1,2 +1,2 @@\n\
                    hello\n\
                    -world\n\
                    +earth\n";
        let cleaned = clean_headers(diff, Path::new("src/mod.txt"));
        assert!(cleaned.contains("diff --git a/src/mod.txt b/src/mod.txt"));
        assert!(cleaned.contains("--- a/src/mod.txt"));
        assert!(cleaned.contains("+++ b/src/mod.txt"));
        assert!(!cleaned.contains("/tmp/host/mod.txt"));
        assert!(!cleaned.contains("/tmp/sandbox/mod.txt"));
    }

    #[test]
    fn clean_headers_keeps_dev_null_sides() {
        let added = "diff --git a/x b/y\n\
                     new file mode 100644\n\
                     index 0000000..3e75765\n\
                     --- /dev/null\n\
                     +++ b/tmp/sandbox/added.txt\n\
                     @@ -0,0 +1 @@\n\
                     +new\n";
        let cleaned = clean_headers(added, Path::new("added.txt"));
        assert!(cleaned.contains("--- /dev/null"));
        assert!(cleaned.contains("+++ b/added.txt"));
        assert!(!cleaned.contains("tmp/sandbox"));
    }

    fn commit_base(sandbox: &Path) {
        git_ok(sandbox, &["init"]).unwrap();
        git_ok(
            sandbox,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        )
        .unwrap();
    }

    #[test]
    fn build_session_view_clones_base_and_overlays_changes() {
        let source = TempDir::create("pithos-test-view-source").unwrap();
        commit_base(source.path());
        let sandbox = TempDir::create("pithos-test-view-sandbox").unwrap();
        copy_tree(
            source.path(),
            sandbox.path(),
            &[],
            CopyMethod::Reflink,
            None,
        )
        .unwrap();
        write(sandbox.path(), "file.txt", "changed");

        let view = build_session_view(source.path(), sandbox.path(), &[]).unwrap();
        let repo = view.path().join("repo");

        assert!(repo.join(".git").exists());
        assert_eq!(
            fs::read_to_string(repo.join("file.txt")).unwrap(),
            "changed"
        );
        let output = git(&repo, &["rev-list", "--count", "HEAD"]).unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1");
        let output = git(&repo, &["status", "--porcelain"]).unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("file.txt"));
    }

    #[test]
    fn build_session_view_ignores_sandbox_commits() {
        let source = TempDir::create("pithos-test-view-committed-source").unwrap();
        commit_base(source.path());
        let sandbox = TempDir::create("pithos-test-view-committed-sandbox").unwrap();
        copy_tree(
            source.path(),
            sandbox.path(),
            &[],
            CopyMethod::Reflink,
            None,
        )
        .unwrap();
        write(sandbox.path(), "file.txt", "changed");
        git_ok(sandbox.path(), &["add", "-A"]).unwrap();
        git_ok(
            sandbox.path(),
            &[
                "-c",
                "user.name=Pithos",
                "-c",
                "user.email=pithos@localhost",
                "commit",
                "-m",
                "agent commit",
            ],
        )
        .unwrap();

        let view = build_session_view(source.path(), sandbox.path(), &[]).unwrap();
        let repo = view.path().join("repo");

        let output = git(&repo, &["rev-list", "--count", "HEAD"]).unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1");
        assert_eq!(
            fs::read_to_string(repo.join("file.txt")).unwrap(),
            "changed"
        );
        let output = git(&repo, &["status", "--porcelain"]).unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("file.txt"));
    }

    #[test]
    fn build_session_view_respects_unmanaged_paths() {
        let source = TempDir::create("pithos-test-view-exclude-source").unwrap();
        commit_base(source.path());
        let sandbox = TempDir::create("pithos-test-view-exclude").unwrap();
        copy_tree(
            source.path(),
            sandbox.path(),
            &[],
            CopyMethod::Reflink,
            None,
        )
        .unwrap();
        write(sandbox.path(), "keep.txt", "changed");
        write(sandbox.path(), "secret.txt", "changed too");

        let view =
            build_session_view(source.path(), sandbox.path(), &["secret.txt".to_string()]).unwrap();
        let repo = view.path().join("repo");

        assert_eq!(
            fs::read_to_string(repo.join("keep.txt")).unwrap(),
            "changed"
        );
        assert!(!repo.join("secret.txt").exists());
    }
}

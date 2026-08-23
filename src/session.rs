use eyre::{Result, WrapErr, bail};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::config::Config;
use crate::registry;
use crate::sandbox::{TempDir, apply_tree, copy_tree, has_changes};

#[tracing::instrument(skip_all, fields(repository = %repository.display()))]
pub(crate) fn run_session(
    config: &Config,
    repository: &Path,
    auto_yes: bool,
    auto_no: bool,
) -> Result<()> {
    if !repository.join(".git").exists() {
        bail!("current directory is not a git repository")
    }
    let up_to_date = config.image_up_to_date()?;
    tracing::debug!(up_to_date, "image freshness check");
    if !up_to_date {
        config.build_image()?;
    }
    let sandbox = TempDir::create("pithos-workspace")?;
    copy_tree(repository, &sandbox.0)?;
    strip_remotes(&sandbox.0)?;
    let current_user = format!(
        "{}:{}",
        crate::platform::current_uid(),
        crate::platform::current_gid()
    );
    let record = registry::SessionRecord::new(
        repository,
        &sandbox.0,
        &config.image_tag,
        &config.workspace,
        &current_user,
    );
    record.save()?;
    println!(
        "pithos session {}: inspect it live with `pithos shell {}` or open {} in your editor",
        record.id,
        record.id,
        sandbox.0.display()
    );
    let mut command = Command::new("podman");
    command.args([
        "run",
        "--rm",
        "--interactive",
        "--tty",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--userns=keep-id",
        "--name",
        &record.container_name,
    ]);
    command.args([
        "--volume",
        &format!("{}:{}:rw,Z", sandbox.0.display(), config.workspace),
    ]);
    command.args(["--workdir", &config.workspace]);
    command.args([
        "--tmpfs",
        &crate::harness::tmpfs_spec("/tmp"),
        "--user",
        &current_user,
    ]);
    for (key, value) in &config.environment {
        command.args(["--env", &format!("{key}={value}")]);
    }
    command.args(["--env", &format!("HOME={}", crate::agent::AGENT_HOME)]);
    for (key, value) in config.harness.environment() {
        command.args(["--env", &format!("{key}={value}")]);
    }
    for (key, value) in crate::environment::terminal_env() {
        if !config.environment.contains_key(&key) {
            command.args(["--env", &format!("{key}={value}")]);
        }
    }
    config.harness.mount(&mut command)?;
    if let Some(networking) = &config.networking {
        networking.apply_to(&mut command)?;
    }
    if let Some(audio) = crate::audio::passthrough(config.audio) {
        tracing::debug!(volume = %audio.volume, "passing host audio through");
        command.args(["--volume", &audio.volume]);
        for (key, value) in audio.env {
            if !config.environment.contains_key(&key) {
                command.args(["--env", &format!("{key}={value}")]);
            }
        }
    }
    tracing::debug!(?command, "starting harness container");
    let status = command
        .arg(&config.image_tag)
        .status()
        .wrap_err("could not execute podman run")?;
    tracing::debug!(%status, "harness container exited");
    registry::remove(&record.id);
    let changed = has_changes(repository, &sandbox.0, &config.exclusions)?;
    tracing::trace!(changed = changed.len(), "change detection finished");
    let apply = if auto_yes {
        true
    } else if auto_no || changed.is_empty() {
        false
    } else {
        summarize(&changed, repository, &sandbox.0);
        let mut session_view = None;
        review(&changed, repository, &sandbox.0, config, &mut session_view)?
    };
    tracing::debug!(apply, "review decision");
    if apply {
        apply_tree(&sandbox.0, repository, &config.exclusions)?;
    }
    if !status.success() {
        bail!("harness exited with {status}")
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

fn change_kind(source: &Path, sandbox: &Path, relative: &Path) -> ChangeKind {
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

fn summarize(changed: &[PathBuf], source: &Path, sandbox: &Path) {
    let (added, modified, deleted) = change_counts(changed, source, sandbox);
    let noun = if changed.len() == 1 { "file" } else { "files" };
    let mut breakdown = Vec::new();
    if added > 0 {
        breakdown.push(format!("{added} added"));
    }
    if modified > 0 {
        breakdown.push(format!("{modified} modified"));
    }
    if deleted > 0 {
        breakdown.push(format!("{deleted} deleted"));
    }
    let mut line = format!("{} {noun} changed", changed.len());
    if !breakdown.is_empty() {
        line.push_str(&format!(" ({})", breakdown.join(", ")));
    }
    println!("{line}");
    for relative in changed.iter().take(20) {
        println!("  {}", relative.display());
    }
    if changed.len() > 20 {
        println!("  ... and {} more", changed.len() - 20);
    }
}

fn review(
    changed: &[PathBuf],
    source: &Path,
    sandbox: &Path,
    config: &Config,
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
            "v" | "view" => view_changes(changed, source, sandbox, config, session_view)?,
            _ => {}
        }
    }
}

fn view_changes(
    changed: &[PathBuf],
    source: &Path,
    sandbox: &Path,
    config: &Config,
    session_view: &mut Option<TempDir>,
) -> Result<()> {
    if let Some(viewer) = &config.diff_viewer {
        if session_view.is_none() {
            *session_view = Some(build_session_view(source, sandbox, &config.exclusions)?);
        }
        let view = session_view.as_ref().expect("session view built above");
        run_viewer(viewer, &view.0.join("repo"))
    } else {
        print_diff(changed, source, sandbox)
    }
}

fn build_session_view(source: &Path, sandbox: &Path, exclusions: &[String]) -> Result<TempDir> {
    let temp = TempDir::create("pithos-session")?;
    let bundle_path = temp.0.join("session.bundle").display().to_string();
    let branch = current_branch(source)?;
    git_ok(source, &["bundle", "create", &bundle_path, &branch])
        .wrap_err("could not create session bundle")?;
    let repo_dir = temp.0.join("repo");
    let repo_path = repo_dir.display().to_string();
    git_ok(source, &["clone", &bundle_path, &repo_path])
        .wrap_err("could not clone session bundle")?;
    apply_tree(sandbox, &repo_dir, exclusions)?;
    Ok(temp)
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
    let status = crate::platform::run_shell(&command).wrap_err("could not run diff_viewer")?;
    if !status.success() {
        eprintln!("diff_viewer exited with {status}");
    }
    Ok(())
}

fn strip_remotes(repository: &Path) -> Result<()> {
    let output = git(repository, &["remote"])?;
    for name in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|name| !name.is_empty())
    {
        git(repository, &["remote", "remove", name])
            .wrap_err_with(|| format!("could not remove remote {name}"))?;
    }
    Ok(())
}

fn git(dir: &Path, args: &[&str]) -> Result<Output> {
    tracing::trace!(args = args.join(" "), "running git");
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .wrap_err("could not run git")
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<()> {
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
            let status = if source_is_dir && sandbox_is_dir {
                "changed directory"
            } else if source_is_dir {
                "removed directory"
            } else {
                "added directory"
            };
            rendered.push_str(&format!("  {status}: {}\n", relative.display()));
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
                (Some(old), Some(new)) => rendered.push_str(&format!(
                    "  symlink changed: {} ({} -> {})\n",
                    relative.display(),
                    old,
                    new
                )),
                (Some(old), None) => rendered.push_str(&format!(
                    "  removed symlink: {} (-> {})\n",
                    relative.display(),
                    old
                )),
                (None, Some(new)) => rendered.push_str(&format!(
                    "  added symlink: {} (-> {})\n",
                    relative.display(),
                    new
                )),
                _ => rendered.push_str(&format!("  symlink changed: {}\n", relative.display())),
            }
        } else {
            match file_diff(source, sandbox, relative)? {
                Some(diff) => rendered.push_str(&diff),
                None => rendered.push_str(&format!("  {}: diff unavailable\n", relative.display())),
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
        (true, false) => (source_path, crate::platform::null_device()),
        _ => (crate::platform::null_device(), sandbox_path),
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

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn change_kind_classifies_add_modified_and_deleted() {
        let source = TempDir::create("pithos-test-kind-source").unwrap();
        let sandbox = TempDir::create("pithos-test-kind-sandbox").unwrap();
        write(&source.0, "modified.txt", "host");
        write(&sandbox.0, "modified.txt", "sandbox");
        write(&source.0, "deleted.txt", "gone");
        write(&sandbox.0, "added.txt", "new");

        assert_eq!(
            change_kind(&source.0, &sandbox.0, Path::new("added.txt")),
            ChangeKind::Added
        );
        assert_eq!(
            change_kind(&source.0, &sandbox.0, Path::new("modified.txt")),
            ChangeKind::Modified
        );
        assert_eq!(
            change_kind(&source.0, &sandbox.0, Path::new("deleted.txt")),
            ChangeKind::Deleted
        );
    }

    #[test]
    fn change_counts_tally_kinds() {
        let source = TempDir::create("pithos-test-count-source").unwrap();
        let sandbox = TempDir::create("pithos-test-count-sandbox").unwrap();
        write(&source.0, "modified.txt", "host");
        write(&sandbox.0, "modified.txt", "sandbox");
        write(&source.0, "deleted.txt", "gone");
        write(&sandbox.0, "added.txt", "new");
        write(&sandbox.0, "also-added.txt", "new");

        let changed = has_changes(&source.0, &sandbox.0, &[]).unwrap();
        assert_eq!(change_counts(&changed, &source.0, &sandbox.0), (2, 1, 1));
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

    #[test]
    fn strip_remotes_removes_all_remotes() {
        let repo = TempDir::create("pithos-test-remotes").unwrap();
        git_ok(&repo.0, &["init"]).unwrap();
        git_ok(
            &repo.0,
            &["remote", "add", "origin", "https://example.com/repo.git"],
        )
        .unwrap();
        git_ok(
            &repo.0,
            &[
                "remote",
                "add",
                "upstream",
                "https://example.com/upstream.git",
            ],
        )
        .unwrap();

        strip_remotes(&repo.0).unwrap();

        let output = git(&repo.0, &["remote"]).unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).trim().is_empty());
    }

    #[test]
    fn strip_remotes_noop_without_remotes() {
        let repo = TempDir::create("pithos-test-no-remotes").unwrap();
        git_ok(&repo.0, &["init"]).unwrap();

        strip_remotes(&repo.0).unwrap();
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
        commit_base(&source.0);
        let sandbox = TempDir::create("pithos-test-view-sandbox").unwrap();
        copy_tree(&source.0, &sandbox.0).unwrap();
        write(&sandbox.0, "file.txt", "changed");

        let view = build_session_view(&source.0, &sandbox.0, &[]).unwrap();
        let repo = view.0.join("repo");

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
        commit_base(&source.0);
        let sandbox = TempDir::create("pithos-test-view-committed-sandbox").unwrap();
        copy_tree(&source.0, &sandbox.0).unwrap();
        write(&sandbox.0, "file.txt", "changed");
        git_ok(&sandbox.0, &["add", "-A"]).unwrap();
        git_ok(
            &sandbox.0,
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

        let view = build_session_view(&source.0, &sandbox.0, &[]).unwrap();
        let repo = view.0.join("repo");

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
    fn build_session_view_respects_exclusions() {
        let source = TempDir::create("pithos-test-view-exclude-source").unwrap();
        commit_base(&source.0);
        let sandbox = TempDir::create("pithos-test-view-exclude").unwrap();
        copy_tree(&source.0, &sandbox.0).unwrap();
        write(&sandbox.0, "keep.txt", "changed");
        write(&sandbox.0, "secret.txt", "changed too");

        let view = build_session_view(&source.0, &sandbox.0, &["secret.txt".to_string()]).unwrap();
        let repo = view.0.join("repo");

        assert_eq!(
            fs::read_to_string(repo.join("keep.txt")).unwrap(),
            "changed"
        );
        assert!(!repo.join("secret.txt").exists());
    }
}

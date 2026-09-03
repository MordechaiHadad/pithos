//! Tiered workspace population.
//!
//! Startup picks the cheapest population tier the platform supports:
//!
//! 1. **Reflink**: the filesystem provides kernel-level CoW clones
//!    (FICLONE, clonefile, ReFS block cloning); every file is cloned
//!    through [`crate::sandbox::copy_tree`] at metadata cost.
//! 2. **Worktree**: the source is a usable git repository; a real
//!    `git worktree add` shares the object store and checks out `HEAD`
//!    with a git command, then overlays dirty/untracked files with a
//!    plain `fs::copy`.
//! 3. **Copy**: plain parallel byte copy with no reflink attempts.

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr, bail};

use crate::sandbox::{
    CopyMethod, DestinationSafety, copy_entry, copy_tree, matches_any, remove_path, temporary_base,
};
use crate::session::{git, git_ok};

/// Workspace population tier. `Reflink` and `Copy` are direct file-copy
/// tiers that map to [`crate::sandbox::CopyMethod`] via [`Self::copy_method`],
/// while `Worktree` is a git-aware tier that clones with shared objects and
/// overlays dirty/untracked files. `sandbox` never matches on `Worktree`
/// directly; it only sees the resulting `CopyMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyStrategy {
    Reflink,
    Worktree,
    Copy,
}

impl CopyStrategy {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Reflink => "reflink",
            Self::Worktree => "worktree",
            Self::Copy => "copy",
        }
    }

    pub(crate) fn copy_method(self) -> CopyMethod {
        match self {
            Self::Reflink => CopyMethod::Reflink,
            Self::Worktree | Self::Copy => CopyMethod::Copy,
        }
    }

    fn from_label(label: &str) -> Result<Self> {
        match label {
            "reflink" => Ok(Self::Reflink),
            "worktree" => Ok(Self::Worktree),
            "copy" => Ok(Self::Copy),
            other => {
                bail!("invalid copy_strategy {other:?}; expected auto, reflink, worktree or copy")
            }
        }
    }
}

/// Resolves the `copy_strategy` configuration value into a forced strategy.
/// `None` means auto-detection, which is both the unset and the explicit
/// `"auto"` outcome.
pub(crate) fn parse_override(value: Option<&str>) -> Result<Option<CopyStrategy>> {
    match value {
        None => Ok(None),
        Some("auto") => Ok(None),
        Some(label) => CopyStrategy::from_label(label).map(Some),
    }
}

/// Picks the default tier for `source`: reflinks when the filesystem
/// supports them, otherwise a shared-object clone when the repository can
/// host one, otherwise a plain copy.
pub(crate) fn detect(source: &Path) -> Result<CopyStrategy> {
    if reflink_supported()? {
        Ok(CopyStrategy::Reflink)
    } else if worktree_eligible(source) {
        Ok(CopyStrategy::Worktree)
    } else {
        Ok(CopyStrategy::Copy)
    }
}

pub(crate) fn try_remove_worktree(repository: &Path, sandbox: &Path) {
    let sandbox_display = sandbox.display().to_string();
    let _ = git(
        repository,
        &["worktree", "remove", "--force", &sandbox_display],
    );
    let _ = git(repository, &["worktree", "prune"]);
}

/// Populates an empty sandbox directory using the forced strategy or, when
/// none was configured, the auto-detected one. Returns the strategy that was
/// actually applied. Forced strategies fail hard on unsupported platforms.
pub(crate) fn populate_sandbox(
    source: &Path,
    sandbox: &Path,
    ignore: &[String],
    forced: Option<CopyStrategy>,
) -> Result<CopyStrategy> {
    let requested = match forced {
        Some(strategy) => strategy,
        None => detect(source).unwrap_or_else(|error| {
            tracing::debug!(%error, "strategy detection failed; using full copy");
            CopyStrategy::Copy
        }),
    };

    if let Some(strategy) = forced {
        ensure_strategy_supported(source, strategy)?;
    }

    let started = std::time::Instant::now();
    let used = match requested {
        CopyStrategy::Worktree => {
            let result = if crate::utils::progress::is_progress_enabled() {
                crate::utils::progress::with_worktree_progress(|| {
                    populate_worktree(source, sandbox, ignore).map(|()| CopyStrategy::Worktree)
                })
            } else {
                populate_worktree(source, sandbox, ignore).map(|()| CopyStrategy::Worktree)
            };
            match result {
                Ok(strategy) => strategy,
                Err(error) if forced.is_none() => {
                    tracing::warn!(%error, "worktree population failed; falling back to full copy");
                    clear_directory(sandbox)?;
                    let method = CopyMethod::Copy;
                    let stats = if crate::utils::progress::is_progress_enabled() {
                        crate::utils::progress::with_copy_progress("plain copy", |progress| {
                            copy_tree(source, sandbox, ignore, method, Some(progress))
                        })?
                    } else {
                        copy_tree(source, sandbox, ignore, method, None)?
                    };
                    tracing::debug!(
                        files = stats.files,
                        cloned = stats.cloned,
                        symlinks = stats.symlinks,
                        directories = stats.directories,
                        bytes = stats.bytes,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "fallback plain copy finished"
                    );
                    CopyStrategy::Copy
                }
                Err(error) => return Err(error),
            }
        }
        CopyStrategy::Reflink => {
            let method = CopyMethod::Reflink;
            let stats = if crate::utils::progress::is_progress_enabled() {
                crate::utils::progress::with_copy_progress("reflink copy", |progress| {
                    copy_tree(source, sandbox, ignore, method, Some(progress))
                })?
            } else {
                copy_tree(source, sandbox, ignore, method, None)?
            };
            tracing::debug!(
                files = stats.files,
                cloned = stats.cloned,
                symlinks = stats.symlinks,
                directories = stats.directories,
                bytes = stats.bytes,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "reflink copy finished"
            );
            CopyStrategy::Reflink
        }
        CopyStrategy::Copy => {
            let method = CopyMethod::Copy;
            let stats = if crate::utils::progress::is_progress_enabled() {
                crate::utils::progress::with_copy_progress("plain copy", |progress| {
                    copy_tree(source, sandbox, ignore, method, Some(progress))
                })?
            } else {
                copy_tree(source, sandbox, ignore, method, None)?
            };
            tracing::debug!(
                files = stats.files,
                cloned = stats.cloned,
                symlinks = stats.symlinks,
                directories = stats.directories,
                bytes = stats.bytes,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "plain copy finished"
            );
            CopyStrategy::Copy
        }
    };

    tracing::debug!(
        strategy = used.label(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "workspace populated"
    );
    Ok(used)
}

fn ensure_strategy_supported(source: &Path, strategy: CopyStrategy) -> Result<()> {
    match strategy {
        CopyStrategy::Reflink => {
            if !reflink_supported()? {
                bail!("reflink strategy requested but filesystem does not support reflink clones");
            }
        }
        CopyStrategy::Worktree => {
            require_worktree_eligible(source)?;
        }
        CopyStrategy::Copy => {}
    }
    Ok(())
}

fn require_worktree_eligible(source: &Path) -> Result<()> {
    if source.join(".gitmodules").exists() {
        bail!("worktree strategy requested but repository contains submodules");
    }
    let output = git(source, &["rev-parse", "--verify", "HEAD"]);
    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => bail!(
            "worktree strategy requested but HEAD is not resolvable: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(error) => bail!("worktree strategy requested but git is unavailable: {error}"),
    }
}

fn reflink_supported() -> Result<bool> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let base = temporary_base()?;
    let sequence = PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let probe_source = base.join(format!(
        ".pithos-reflink-probe-{}-{sequence}.src",
        std::process::id()
    ));
    let probe_dest = base.join(format!(
        ".pithos-reflink-probe-{}-{sequence}.dst",
        std::process::id()
    ));
    if fs::write(&probe_source, b"pithos").is_err() {
        let _ = fs::remove_file(&probe_source);
        return Ok(false);
    }
    let supported = matches!(
        reflink_copy::reflink_or_copy(&probe_source, &probe_dest),
        Ok(None)
    );
    let _ = fs::remove_file(&probe_source);
    let _ = fs::remove_file(&probe_dest);
    Ok(supported)
}

/// Whether the source repository can back a shared-object sandbox: it must
/// have commits and no submodule content, whose working trees live in nested
/// repositories that alternates cannot cover.
pub(crate) fn worktree_eligible(source: &Path) -> bool {
    if source.join(".gitmodules").exists() {
        tracing::debug!("submodules detected; worktree tier unavailable");
        return false;
    }
    matches!(
        git(source, &["rev-parse", "--verify", "HEAD"]),
        Ok(output) if output.status.success()
    )
}

fn populate_worktree(source: &Path, sandbox: &Path, ignore: &[String]) -> Result<()> {
    if !worktree_eligible(source) {
        bail!("repository cannot back a shared-object sandbox");
    }
    let head = head_commit(source)?;
    if sandbox.exists() {
        fs::remove_dir_all(sandbox).wrap_err("could not clear sandbox for worktree")?;
    }
    let sandbox_display = sandbox.display().to_string();
    git_ok(
        source,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &sandbox_display,
            &head,
        ],
    )
    .wrap_err("worktree add failed")?;
    let overlay_result = (|| -> Result<()> {
        overlay_dirty_files(source, sandbox)?;
        fill_untracked_files(source, sandbox, ignore)
    })();
    if overlay_result.is_err() {
        let _ = git(source, &["worktree", "remove", "--force", &sandbox_display]);
        let _ = fs::remove_dir_all(sandbox);
    }
    overlay_result
}

fn head_commit(source: &Path) -> Result<String> {
    let output = git(source, &["rev-parse", "HEAD"])?;
    if !output.status.success() {
        bail!(
            "could not resolve HEAD: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if commit.is_empty() {
        bail!("HEAD did not resolve to a commit");
    }
    Ok(commit)
}

/// Mirrors worktree-level modifications onto the fresh checkout so the
/// sandbox starts exactly where the user left off, including files deleted
/// in the working tree; without this, apply-back would resurrect deletions.
fn overlay_dirty_files(source: &Path, sandbox: &Path) -> Result<()> {
    for path in dirty_worktree_paths(source)? {
        let source_path = source.join(&path);
        let sandbox_path = sandbox.join(&path);
        if fs::symlink_metadata(&source_path).is_ok() {
            if let Some(parent) = sandbox_path.parent() {
                fs::create_dir_all(parent)?;
            }
            copy_entry(
                &source_path,
                &sandbox_path,
                DestinationSafety::Fresh,
                CopyMethod::Copy,
            )?;
        } else if fs::symlink_metadata(&sandbox_path).is_ok() {
            remove_path(&sandbox_path)?;
        }
    }
    Ok(())
}

/// Paths with index or worktree level changes according to
/// `git status --porcelain=v1 -z`. Under `-z` each rename emits its
/// destination first followed by the bare source path; sources are included
/// so the overlay can drop them from the fresh checkout.
fn dirty_worktree_paths(source: &Path) -> Result<Vec<PathBuf>> {
    let output = git(
        source,
        &["status", "--porcelain=v1", "-z", "--untracked-files=no"],
    )?;
    if !output.status.success() {
        bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut paths = Vec::new();
    let mut pending_rename_source = false;
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        if pending_rename_source {
            pending_rename_source = false;
            let source_path = String::from_utf8_lossy(entry).into_owned();
            paths.push(PathBuf::from(source_path));
            continue;
        }
        if entry[0] == b'R' || entry[0] == b'C' {
            pending_rename_source = true;
        }
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        paths.push(PathBuf::from(path));
    }
    Ok(paths)
}

/// Copies files the user added outside git's index, minus ignored paths, so
/// untracked-but-needed files survive into the session exactly as they do
/// under a full tree copy. Embedded repositories are reported by
/// `ls-files --others` as bare `dir/` entries and are expanded here.
fn fill_untracked_files(source: &Path, sandbox: &Path, ignore: &[String]) -> Result<()> {
    let output = git(source, &["ls-files", "--others", "-z"])?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let relative = String::from_utf8_lossy(entry.trim_ascii_end()).into_owned();
        let relative = PathBuf::from(relative);
        if entry.ends_with(b"/") {
            copy_untracked_directory(source, sandbox, &relative, ignore)?;
        } else if !matches_any(&relative, ignore) {
            let destination = sandbox.join(&relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            copy_entry(
                &source.join(&relative),
                &destination,
                DestinationSafety::Fresh,
                CopyMethod::Copy,
            )?;
        }
    }
    Ok(())
}

fn copy_untracked_directory(
    source: &Path,
    sandbox: &Path,
    relative: &Path,
    ignore: &[String],
) -> Result<()> {
    if matches_any(relative, ignore) {
        return Ok(());
    }
    let source_dir = source.join(relative);
    let destination_dir = sandbox.join(relative);
    fs::create_dir_all(&destination_dir)?;
    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let child_relative = relative.join(entry.file_name());
        let file_type = fs::symlink_metadata(entry.path())?.file_type();
        if file_type.is_dir() {
            copy_untracked_directory(source, sandbox, &child_relative, ignore)?;
        } else {
            copy_entry(
                &entry.path(),
                &sandbox.join(&child_relative),
                DestinationSafety::Fresh,
                CopyMethod::Copy,
            )?;
        }
    }
    Ok(())
}

fn clear_directory(directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        remove_path(&entry?.path())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{TempDir, has_changes};
    use crate::session::git_ok;
    use std::fs;

    struct RepoFixture {
        _dir: TempDir,
        source: PathBuf,
        sandbox: TempDir,
    }

    impl RepoFixture {
        fn new(name: &str) -> Self {
            let dir = TempDir::create(name).unwrap();
            let source = dir.path().join("source");
            let sandbox = TempDir::create(name).unwrap();
            fs::create_dir_all(&source).unwrap();
            Self {
                _dir: dir,
                source,
                sandbox,
            }
        }

        fn init_repo(&self) {
            git_ok(&self.source, &["init", "--quiet"]).unwrap();
            git_ok(&self.source, &["config", "user.email", "pithos@test"]).unwrap();
            git_ok(&self.source, &["config", "user.name", "pithos"]).unwrap();
        }

        fn commit(&self, message: &str) {
            git_ok(&self.source, &["add", "-A"]).unwrap();
            git_ok(&self.source, &["commit", "--quiet", "-m", message]).unwrap();
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.source.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn assert_matches_source(&self, ignore: &[String]) {
            populate_sandbox(
                &self.source,
                self.sandbox.path(),
                ignore,
                Some(CopyStrategy::Worktree),
            )
            .unwrap();
            assert_trees_equal(&self.source, self.sandbox.path(), ignore);
            let changed = has_changes(&self.source, self.sandbox.path(), ignore).unwrap();
            assert!(changed.is_empty(), "unexpected diffs: {changed:?}");
        }
    }

    fn assert_trees_equal(expected: &Path, actual: &Path, ignore: &[String]) {
        let mut expected_paths = Vec::new();
        let mut actual_paths = Vec::new();
        collect_relative(expected, Path::new(""), &mut expected_paths);
        collect_relative(actual, Path::new(""), &mut actual_paths);
        expected_paths.retain(|path| !matches_any(path, ignore));
        actual_paths.retain(|path| !matches_any(path, ignore));
        expected_paths.sort();
        actual_paths.sort();
        assert_eq!(expected_paths, actual_paths, "path sets differ");
        for relative in &expected_paths {
            let expected_type = fs::symlink_metadata(expected.join(relative))
                .unwrap()
                .file_type();
            let actual_type = fs::symlink_metadata(actual.join(relative))
                .unwrap()
                .file_type();
            assert_eq!(
                expected_type.is_symlink(),
                actual_type.is_symlink(),
                "symlink-ness differs for {}",
                relative.display()
            );
            if expected_type.is_file() {
                assert_eq!(
                    fs::read(expected.join(relative)).unwrap(),
                    fs::read(actual.join(relative)).unwrap(),
                    "content differs for {}",
                    relative.display()
                );
            }
        }
    }

    fn collect_relative(root: &Path, relative: &Path, paths: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let entry = entry.unwrap();
            let child = relative.join(entry.file_name());
            if child == Path::new(".git") || child.starts_with(".git/") {
                continue;
            }
            paths.push(child.clone());
            if entry.file_type().unwrap().is_dir() {
                collect_relative(root, &child, paths);
            }
        }
    }

    #[test]
    fn parse_override_accepts_known_values() {
        assert_eq!(parse_override(Some("auto")).unwrap(), None);
        assert_eq!(parse_override(None).unwrap(), None);
        assert_eq!(
            parse_override(Some("worktree")).unwrap(),
            Some(CopyStrategy::Worktree)
        );
        assert_eq!(
            parse_override(Some("reflink")).unwrap(),
            Some(CopyStrategy::Reflink)
        );
        assert_eq!(
            parse_override(Some("copy")).unwrap(),
            Some(CopyStrategy::Copy)
        );
    }

    #[test]
    fn parse_override_rejects_unknown_values() {
        assert!(parse_override(Some("hardlink")).is_err());
    }

    #[test]
    fn clean_repository_populates_identically_to_a_full_copy() {
        let fixture = RepoFixture::new("pithos-test-tier-clean");
        fixture.init_repo();
        fixture.write("a.txt", "hello");
        fixture.write("nested/dir/b.txt", "deep");
        fixture.commit("init");

        fixture.assert_matches_source(&[]);
        assert_eq!(
            fs::read(fixture.sandbox.path().join("a.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn modified_and_deleted_tracked_files_are_overlaid() {
        let fixture = RepoFixture::new("pithos-test-tier-dirty");
        fixture.init_repo();
        fixture.write("moved-later.txt", "carried over");
        fixture.write("modified.txt", "original");
        fixture.write("deleted.txt", "doomed");
        fixture.commit("init");
        fixture.write("modified.txt", "edited in worktree");
        fs::remove_file(fixture.source.join("deleted.txt")).unwrap();
        git_ok(&fixture.source, &["mv", "moved-later.txt", "moved.txt"]).unwrap();
        git_ok(&fixture.source, &["add", "-A"]).unwrap();

        fixture.assert_matches_source(&[]);
        assert!(!fixture.sandbox.path().join("moved-later.txt").exists());
        assert_eq!(
            fs::read(fixture.sandbox.path().join("moved.txt")).unwrap(),
            b"carried over"
        );
    }

    #[test]
    fn untracked_files_survive_and_ignored_paths_do_not() {
        let fixture = RepoFixture::new("pithos-test-tier-untracked");
        fixture.init_repo();
        fixture.write("committed.txt", "committed");
        fixture.commit("init");
        fixture.write("untracked.txt", "loose");
        fixture.write("cache/blob.bin", "junk");

        fixture.assert_matches_source(&["cache".to_string()]);
        assert_eq!(
            fs::read(fixture.sandbox.path().join("untracked.txt")).unwrap(),
            b"loose"
        );
        assert!(!fixture.sandbox.path().join("cache").exists());
    }

    #[test]
    fn untracked_directories_expand_recursively() {
        let fixture = RepoFixture::new("pithos-test-tier-vendored");
        fixture.init_repo();
        fixture.write("root.txt", "root");
        fixture.commit("init");
        fixture.write("vendored/nested/lib.rs", "fn main() {}");

        fixture.assert_matches_source(&[]);
        assert_eq!(
            fs::read(fixture.sandbox.path().join("vendored/nested/lib.rs")).unwrap(),
            b"fn main() {}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_in_untracked_files_are_preserved() {
        let fixture = RepoFixture::new("pithos-test-tier-symlinks");
        fixture.init_repo();
        fixture.write("target.txt", "pointed at");
        fixture.commit("init");
        crate::utils::platform::symlink(
            Path::new("target.txt"),
            &fixture.source.join("linked.txt"),
        )
        .unwrap();

        fixture.assert_matches_source(&[]);
        let link = fs::read_link(fixture.sandbox.path().join("linked.txt")).unwrap();
        assert_eq!(link, Path::new("target.txt"));
    }

    #[test]
    fn empty_repositories_fail_hard_for_explicit_worktree() {
        let fixture = RepoFixture::new("pithos-test-tier-empty");
        fixture.init_repo();
        fixture.write("only.txt", "never committed");

        let result = populate_sandbox(
            &fixture.source,
            fixture.sandbox.path(),
            &[],
            Some(CopyStrategy::Worktree),
        );
        assert!(result.is_err());
    }

    #[test]
    fn submoduled_repositories_fail_hard_for_explicit_worktree() {
        let fixture = RepoFixture::new("pithos-test-tier-submodules");
        fixture.init_repo();
        fixture.write("app/main.rs", "fn main() {}");
        fixture.commit("init");
        fixture.write(".gitmodules", "[submodule \"lib\"]\n\tpath = lib\n");

        assert!(!worktree_eligible(&fixture.source));

        let result = populate_sandbox(
            &fixture.source,
            fixture.sandbox.path(),
            &[],
            Some(CopyStrategy::Worktree),
        );
        assert!(result.is_err());
    }

    #[test]
    fn worktree_creates_gitdir_file() {
        let fixture = RepoFixture::new("pithos-test-tier-alternates");
        fixture.init_repo();
        fixture.write("a.txt", "content");
        fixture.commit("init");

        populate_sandbox(
            &fixture.source,
            fixture.sandbox.path(),
            &[],
            Some(CopyStrategy::Worktree),
        )
        .unwrap();

        let git_path = fixture.sandbox.path().join(".git");
        assert!(git_path.exists(), ".git should exist for worktree");
        let git_content = fs::read_to_string(&git_path).unwrap_or_default();
        assert!(
            git_content.contains("gitdir:"),
            ".git should be a gitdir file for worktree, got: {git_content:?}"
        );
        let output = git(fixture.sandbox.path(), &["log", "--oneline", "-1"]).unwrap();
        assert!(output.status.success(), "worktree git should resolve HEAD");
    }

    #[test]
    fn sandbox_git_reads_objects_through_worktree() {
        let fixture = RepoFixture::new("pithos-test-tier-host-git");
        fixture.init_repo();
        fixture.write("a.txt", "content");
        fixture.commit("init");

        populate_sandbox(
            &fixture.source,
            fixture.sandbox.path(),
            &[],
            Some(CopyStrategy::Worktree),
        )
        .unwrap();

        let output = git(fixture.sandbox.path(), &["log", "--oneline", "-1"]).unwrap();
        assert!(output.status.success(), "worktree git broke");
        let output2 = git(fixture.sandbox.path(), &["status", "--porcelain"]).unwrap();
        assert!(output2.status.success());
    }
}

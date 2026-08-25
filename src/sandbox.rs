use eyre::{Result, eyre};
use rayon::prelude::*;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use tempfile::Builder;

const ORPHAN_MIN_AGE: Duration = Duration::from_secs(60 * 60);

pub(crate) struct TempDir(tempfile::TempDir);

impl TempDir {
    pub(crate) fn create(prefix: &str) -> Result<Self> {
        let dir = Builder::new()
            .prefix(&format!("{prefix}-"))
            .tempdir_in(temporary_base()?)?;
        lock_active().push(dir.path().to_path_buf());
        Ok(Self(dir))
    }

    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        lock_active().retain(|path| path.as_path() != self.0.path());
    }
}

fn active_temp_dirs() -> &'static Mutex<Vec<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock_active() -> MutexGuard<'static, Vec<PathBuf>> {
    match active_temp_dirs().lock() {
        Ok(guard) => guard,
        Err(poisoned) => PoisonError::into_inner(poisoned),
    }
}

pub(crate) fn remove_active_temp_dirs() {
    for path in lock_active().drain(..) {
        let _ = fs::remove_dir_all(path);
    }
}

pub(crate) fn sweep_orphans(live_sandboxes: &[PathBuf]) -> Result<()> {
    let base = temporary_base()?;
    let Some(cutoff) = SystemTime::now().checked_sub(ORPHAN_MIN_AGE) else {
        return Ok(());
    };
    for entry in fs::read_dir(&base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if live_sandboxes.contains(&path) {
            continue;
        }
        if entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified > cutoff)
        {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(&path) {
            tracing::warn!(path = %path.display(), %error, "could not remove orphaned temp directory");
        }
    }
    let _ = fs::remove_dir(&base);
    Ok(())
}

fn temporary_base() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| eyre!("cannot determine data directory"))?
        .join("pithos")
        .join("tmp");
    fs::create_dir_all(&base)?;
    Ok(base)
}

/// Aggregate outcome of a tree copy, reported at debug level for profiling.
#[derive(Debug, Default)]
struct CopyStats {
    files: u64,
    cloned: u64,
    symlinks: u64,
    directories: u64,
    bytes: u64,
}

impl CopyStats {
    fn record_entry(&mut self, entry: CopiedEntry) {
        match entry {
            CopiedEntry::File { bytes, reflinked } => {
                self.files += 1;
                self.bytes += bytes;
                if reflinked {
                    self.cloned += 1;
                }
            }
            CopiedEntry::Symlink => self.symlinks += 1,
        }
    }

    fn merge(&mut self, other: CopyStats) {
        self.files += other.files;
        self.cloned += other.cloned;
        self.symlinks += other.symlinks;
        self.directories += other.directories;
        self.bytes += other.bytes;
    }
}

/// What [`copy_entry`] materialized for a single non-directory entry.
enum CopiedEntry {
    File { bytes: u64, reflinked: bool },
    Symlink,
}

pub(crate) fn copy_tree(source: &Path, destination: &Path, ignore: &[String]) -> Result<()> {
    let started = Instant::now();
    let stats = copy_tree_at(source, destination, Path::new(""), ignore)?;
    tracing::debug!(
        files = stats.files,
        cloned = stats.cloned,
        symlinks = stats.symlinks,
        directories = stats.directories,
        bytes = stats.bytes,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "tree copy finished"
    );
    Ok(())
}

fn copy_tree_at(
    source: &Path,
    destination: &Path,
    relative: &Path,
    ignore: &[String],
) -> Result<CopyStats> {
    fs::create_dir_all(destination)?;
    let mut stats = CopyStats {
        directories: 1,
        ..CopyStats::default()
    };
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut subdirectories: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let child_relative = relative.join(entry.file_name());
        if matches_any(&child_relative, ignore) {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if fs::symlink_metadata(&source_path)?.file_type().is_dir() {
            subdirectories.push((source_path, destination_path, child_relative));
        } else {
            files.push((source_path, destination_path));
        }
    }
    let file_entries = files
        .par_iter()
        .map(|(source_path, destination_path)| copy_entry(source_path, destination_path))
        .collect::<Result<Vec<Option<CopiedEntry>>>>()?;
    for entry in file_entries.into_iter().flatten() {
        stats.record_entry(entry);
    }
    let subtree_stats = subdirectories
        .par_iter()
        .map(|(source_path, destination_path, child_relative)| {
            copy_tree_at(source_path, destination_path, child_relative, ignore)
        })
        .collect::<Result<Vec<CopyStats>>>()?;
    for subtree in subtree_stats {
        stats.merge(subtree);
    }
    Ok(stats)
}

fn copy_entry(source: &Path, destination: &Path) -> Result<Option<CopiedEntry>> {
    tracing::trace!(source = %source.display(), destination = %destination.display(), "copying entry");
    let metadata = fs::symlink_metadata(source)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        let target = fs::read_link(source)?;
        crate::platform::symlink(&target, destination)?;
        Ok(Some(CopiedEntry::Symlink))
    } else if file_type.is_file() {
        let bytes = metadata.len();
        let reflinked = atomic_copy(source, destination)?;
        Ok(Some(CopiedEntry::File { bytes, reflinked }))
    } else {
        eprintln!("warning: skipping special file {}", source.display());
        Ok(None)
    }
}

/// Copies `source` to `destination` through a temporary file so readers never
/// observe partial content. Returns whether the copy was served by a
/// filesystem-level clone; unsupported filesystems silently fall back to a
/// byte copy.
fn atomic_copy(source: &Path, destination: &Path) -> Result<bool> {
    let file_name = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let temp = destination.with_file_name(format!(".{file_name}.pithos-tmp"));
    let _ = fs::remove_file(&temp);
    let reflinked = match reflink_copy::reflink_or_copy(source, &temp) {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(error) => {
            tracing::debug!(
                source = %source.display(),
                %error,
                "clone attempt failed; falling back to byte copy"
            );
            fs::copy(source, &temp)?;
            false
        }
    };
    fs::set_permissions(&temp, fs::metadata(source)?.permissions())?;
    if let Err(error) = fs::rename(&temp, destination) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(reflinked)
}

pub(crate) fn has_changes(
    source: &Path,
    sandbox: &Path,
    unmanaged: &[String],
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_paths(source, Path::new(""), unmanaged, &mut paths)?;
    collect_paths(sandbox, Path::new(""), unmanaged, &mut paths)?;
    paths.sort();
    paths.dedup();
    let mut changed = Vec::new();
    for relative in paths {
        if is_excluded(&relative, unmanaged) {
            continue;
        }
        if !same_file(&source.join(&relative), &sandbox.join(&relative))? {
            changed.push(relative);
        }
    }
    Ok(changed)
}

fn is_excluded(relative: &Path, unmanaged: &[String]) -> bool {
    relative == Path::new(".git")
        || relative.starts_with(".git/")
        || matches_any(relative, unmanaged)
}

fn matches_any(relative: &Path, patterns: &[String]) -> bool {
    let components: Vec<_> = relative.components().map(|c| c.as_os_str()).collect();
    patterns.iter().any(|pattern| {
        let parts: Vec<_> = Path::new(pattern)
            .components()
            .map(|c| c.as_os_str())
            .collect();
        components
            .windows(parts.len())
            .any(|window| window == parts.as_slice())
    })
}

fn collect_paths(
    root: &Path,
    relative: &Path,
    excluded: &[String],
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        if is_excluded(&child, excluded) {
            continue;
        }
        paths.push(child.clone());
        if entry.file_type()?.is_dir() {
            collect_paths(root, &child, excluded, paths)?;
        }
    }
    Ok(())
}

fn same_file(first: &Path, second: &Path) -> Result<bool> {
    match (fs::symlink_metadata(first), fs::symlink_metadata(second)) {
        (Ok(first_metadata), Ok(second_metadata))
            if first_metadata.file_type().is_symlink()
                && second_metadata.file_type().is_symlink() =>
        {
            Ok(fs::read_link(first)? == fs::read_link(second)?)
        }
        (Ok(first_metadata), Ok(second_metadata))
            if first_metadata.is_dir() && second_metadata.is_dir() =>
        {
            Ok(true)
        }
        (Ok(first_metadata), Ok(second_metadata))
            if first_metadata.is_file() && second_metadata.is_file() =>
        {
            Ok(fs::read(first)? == fs::read(second)?)
        }
        (Err(first_error), Err(second_error))
            if first_error.kind() == io::ErrorKind::NotFound
                && second_error.kind() == io::ErrorKind::NotFound =>
        {
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn apply_tree(source: &Path, destination: &Path, unmanaged: &[String]) -> Result<()> {
    apply_tree_at(source, destination, Path::new(""), unmanaged)
}

fn apply_tree_at(
    source: &Path,
    destination: &Path,
    relative: &Path,
    unmanaged: &[String],
) -> Result<()> {
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        let child_relative = relative.join(entry.file_name());
        if is_excluded(&child_relative, unmanaged) {
            continue;
        }
        if !source.join(entry.file_name()).exists() {
            tracing::trace!(path = %child_relative.display(), "removing path missing in sandbox");
            remove_path(&entry.path())?;
        }
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let child_relative = relative.join(entry.file_name());
        if is_excluded(&child_relative, unmanaged) {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&destination_path)?;
            apply_tree_at(&source_path, &destination_path, &child_relative, unmanaged)?;
        } else {
            let _ = copy_entry(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    let file_type = fs::symlink_metadata(path)?.file_type();
    if file_type.is_symlink() || !file_type.is_dir() {
        fs::remove_file(path)?;
    } else {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_temp_dir(name: &str) -> TempDir {
        TempDir::create(name).unwrap()
    }

    fn write_file(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn make_symlink(root: &Path, relative: &str, target: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        crate::platform::symlink(Path::new(target), &path).unwrap();
    }

    fn collect_all_relative(root: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut stack = vec![PathBuf::new()];
        while let Some(relative) = stack.pop() {
            for entry in fs::read_dir(root.join(&relative)).unwrap() {
                let entry = entry.unwrap();
                let child = relative.join(entry.file_name());
                paths.push(child.clone());
                if entry.file_type().unwrap().is_dir() {
                    stack.push(child);
                }
            }
        }
        paths
    }

    fn assert_trees_equal(expected: &Path, actual: &Path) {
        let mut expected_paths = collect_all_relative(expected);
        let mut actual_paths = collect_all_relative(actual);
        expected_paths.sort();
        actual_paths.sort();
        assert_eq!(expected_paths, actual_paths, "path sets differ");
        for relative in expected_paths {
            let expected_path = expected.join(&relative);
            let actual_path = actual.join(&relative);
            let expected_type = fs::symlink_metadata(&expected_path).unwrap().file_type();
            let actual_type = fs::symlink_metadata(&actual_path).unwrap().file_type();
            assert_eq!(
                expected_type.is_symlink(),
                actual_type.is_symlink(),
                "symlink-ness differs for {}",
                relative.display()
            );
            if expected_type.is_symlink() {
                assert_eq!(
                    fs::read_link(&expected_path).unwrap(),
                    fs::read_link(&actual_path).unwrap(),
                    "symlink target differs for {}",
                    relative.display()
                );
            } else if expected_type.is_file() {
                assert_eq!(
                    fs::read(&expected_path).unwrap(),
                    fs::read(&actual_path).unwrap(),
                    "content differs for {}",
                    relative.display()
                );
            }
        }
    }

    #[test]
    fn atomic_copy_isolates_writes_from_source() {
        let dir = test_temp_dir("pithos-test-atomic-isolation");
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        fs::write(&source, b"original").unwrap();

        let _reflinked = atomic_copy(&source, &destination).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"original");
        fs::write(&destination, b"mutated").unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_copy_propagates_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_temp_dir("pithos-test-atomic-permissions");
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        fs::write(&source, b"data").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();

        atomic_copy(&source, &destination).unwrap();

        let mode = fs::metadata(&destination).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn copy_tree_preserves_files_dirs_and_symlinks() {
        let source = test_temp_dir("pithos-test-copy-source");
        let destination = test_temp_dir("pithos-test-copy-destination");
        write_file(source.path(), "root.txt", "root");
        write_file(source.path(), "sub/nested.txt", "nested");
        make_symlink(source.path(), "link", "root.txt");

        copy_tree(source.path(), destination.path(), &[]).unwrap();

        assert_trees_equal(source.path(), destination.path());
        let link_metadata = fs::symlink_metadata(destination.path().join("link")).unwrap();
        assert!(link_metadata.file_type().is_symlink());
        assert_eq!(
            fs::read_link(destination.path().join("link")).unwrap(),
            Path::new("root.txt")
        );
    }

    #[test]
    fn copy_tree_skips_ignored_but_keeps_ephemeral() {
        let source = test_temp_dir("pithos-test-copy-ignore-source");
        let destination = test_temp_dir("pithos-test-copy-ignore-destination");
        write_file(source.path(), "kept.txt", "kept");
        write_file(source.path(), "ephemeral/cache.bin", "cache");
        write_file(source.path(), "ignored/scratch.bin", "scratch");

        copy_tree(source.path(), destination.path(), &["ignored".to_string()]).unwrap();

        assert!(destination.path().join("kept.txt").exists());
        assert!(destination.path().join("ephemeral/cache.bin").exists());
        assert!(!destination.path().join("ignored").exists());
    }

    #[test]
    fn same_file_comparisons() {
        let first = test_temp_dir("pithos-test-same-first");
        let second = test_temp_dir("pithos-test-same-second");
        write_file(first.path(), "equal.txt", "same");
        write_file(second.path(), "equal.txt", "same");
        write_file(first.path(), "different.txt", "one");
        write_file(second.path(), "different.txt", "two");
        write_file(first.path(), "one-sided.txt", "only first");
        make_symlink(first.path(), "same-link", "equal.txt");
        make_symlink(second.path(), "same-link", "equal.txt");
        make_symlink(first.path(), "other-link", "different.txt");
        make_symlink(second.path(), "other-link", "equal.txt");
        fs::create_dir(first.path().join("dir")).unwrap();
        fs::create_dir(second.path().join("dir")).unwrap();

        assert!(
            same_file(
                &first.path().join("equal.txt"),
                &second.path().join("equal.txt")
            )
            .unwrap()
        );
        assert!(
            !same_file(
                &first.path().join("different.txt"),
                &second.path().join("different.txt")
            )
            .unwrap()
        );
        assert!(
            same_file(
                &first.path().join("missing.txt"),
                &second.path().join("missing.txt")
            )
            .unwrap()
        );
        assert!(same_file(&first.path().join("dir"), &second.path().join("dir")).unwrap());
        assert!(
            same_file(
                &first.path().join("same-link"),
                &second.path().join("same-link")
            )
            .unwrap()
        );
        assert!(
            !same_file(
                &first.path().join("other-link"),
                &second.path().join("other-link")
            )
            .unwrap()
        );
        assert!(
            !same_file(
                &first.path().join("one-sided.txt"),
                &second.path().join("equal.txt")
            )
            .unwrap()
        );
    }

    #[test]
    fn has_changes_detects_add_modify_delete_and_unmanaged() {
        let host = test_temp_dir("pithos-test-changes-host");
        let sandbox = test_temp_dir("pithos-test-changes-sandbox");
        write_file(host.path(), "modified.txt", "host");
        write_file(sandbox.path(), "modified.txt", "sandbox");
        write_file(host.path(), "unchanged.txt", "same");
        write_file(sandbox.path(), "unchanged.txt", "same");
        write_file(host.path(), "deleted.txt", "gone");
        write_file(host.path(), "unmanaged/config.txt", "secret");
        write_file(sandbox.path(), "unmanaged/config.txt", "also secret");
        write_file(sandbox.path(), "added.txt", "new");
        let unmanaged = vec!["unmanaged".to_string()];

        let changed = has_changes(host.path(), sandbox.path(), &unmanaged).unwrap();

        assert_eq!(
            changed,
            vec![
                PathBuf::from("added.txt"),
                PathBuf::from("deleted.txt"),
                PathBuf::from("modified.txt"),
            ]
        );
    }

    #[test]
    fn apply_tree_round_trip() {
        let host = test_temp_dir("pithos-test-apply-host");
        let sandbox = test_temp_dir("pithos-test-apply-sandbox");
        write_file(host.path(), "keep.txt", "keep");
        write_file(sandbox.path(), "keep.txt", "keep");
        write_file(host.path(), "sub/modified.txt", "host");
        write_file(sandbox.path(), "sub/modified.txt", "sandbox");
        write_file(host.path(), "sub/removed.txt", "remove me");
        write_file(sandbox.path(), "added.txt", "new");
        write_file(sandbox.path(), "sub/new/deep.txt", "deep");
        make_symlink(host.path(), "removed-link", "keep.txt");
        make_symlink(sandbox.path(), "added-link", "added.txt");

        apply_tree(sandbox.path(), host.path(), &[]).unwrap();

        assert_trees_equal(sandbox.path(), host.path());
    }

    #[test]
    fn apply_tree_respects_unmanaged() {
        let host = test_temp_dir("pithos-test-exclude-host");
        let sandbox = test_temp_dir("pithos-test-exclude-sandbox");
        write_file(host.path(), "keep.txt", "host");
        write_file(sandbox.path(), "keep.txt", "sandbox");
        write_file(host.path(), "unmanaged/config.txt", "host secret");
        write_file(sandbox.path(), "unmanaged/config.txt", "sandbox secret");
        write_file(sandbox.path(), "added.txt", "new");
        let unmanaged = vec!["unmanaged".to_string()];

        apply_tree(sandbox.path(), host.path(), &unmanaged).unwrap();

        assert_eq!(
            fs::read_to_string(host.path().join("keep.txt")).unwrap(),
            "sandbox"
        );
        assert_eq!(
            fs::read_to_string(host.path().join("unmanaged/config.txt")).unwrap(),
            "host secret"
        );
        assert_eq!(
            fs::read_to_string(host.path().join("added.txt")).unwrap(),
            "new"
        );
    }

    #[test]
    fn is_excluded_matches_exact_and_children() {
        let unmanaged = vec![".git".to_string(), "target".to_string()];
        assert!(is_excluded(Path::new(".git"), &unmanaged));
        assert!(is_excluded(Path::new(".git/HEAD"), &unmanaged));
        assert!(is_excluded(Path::new("target"), &unmanaged));
        assert!(is_excluded(Path::new("target/debug/pithos"), &unmanaged));
        assert!(!is_excluded(Path::new("src/main.rs"), &unmanaged));
        assert!(!is_excluded(Path::new(".github"), &unmanaged));
        assert!(!is_excluded(Path::new("targeting"), &unmanaged));
    }

    #[test]
    fn matches_any_matches_nested_paths() {
        let patterns = vec!["target".to_string()];
        assert!(matches_any(
            Path::new("long/ass/path/my/dude/target"),
            &patterns
        ));
        assert!(matches_any(
            Path::new("compiler/target/debug/deep.o"),
            &patterns
        ));
        assert!(!matches_any(
            Path::new("compiler/notarget/deep.o"),
            &patterns
        ));
        let nested = vec!["sub/cache".to_string()];
        assert!(matches_any(Path::new("sub/cache/data.bin"), &nested));
        assert!(matches_any(Path::new("deep/sub/cache/x"), &nested));
        assert!(!matches_any(Path::new("sub/caching/x"), &nested));
    }

    #[test]
    fn is_excluded_always_ignores_git_even_when_absent_from_list() {
        let unmanaged = vec!["target".to_string(), ".serena".to_string()];
        assert!(is_excluded(Path::new(".git"), &unmanaged));
        assert!(is_excluded(Path::new(".git/objects/abc"), &unmanaged));
    }
}

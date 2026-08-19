use eyre::Result;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn create(prefix: &str) -> Result<Self> {
        let path = temporary_path(prefix)?;
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temporary_path(prefix: &str) -> Result<PathBuf> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(env::temp_dir().join(format!("{prefix}-{}-{stamp}", std::process::id())))
}

pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if fs::symlink_metadata(&source_path)?.file_type().is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else {
            copy_entry(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path) -> Result<()> {
    let file_type = fs::symlink_metadata(source)?.file_type();
    if file_type.is_symlink() {
        let target = fs::read_link(source)?;
        std::os::unix::fs::symlink(target, destination)?;
    } else if file_type.is_file() {
        atomic_copy(source, destination)?;
    } else {
        eprintln!("warning: skipping special file {}", source.display());
    }
    Ok(())
}

fn atomic_copy(source: &Path, destination: &Path) -> Result<()> {
    let file_name = destination.file_name().unwrap_or_default().to_string_lossy();
    let temp = destination.with_file_name(format!(".{file_name}.pithos-tmp"));
    fs::copy(source, &temp)?;
    if let Err(error) = fs::rename(&temp, destination) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

pub(crate) fn has_changes(
    source: &Path,
    sandbox: &Path,
    exclusions: &[String],
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_paths(source, Path::new(""), &mut paths)?;
    collect_paths(sandbox, Path::new(""), &mut paths)?;
    paths.sort();
    paths.dedup();
    let mut changed = Vec::new();
    for relative in paths {
        if is_excluded(&relative, exclusions) {
            continue;
        }
        if !same_file(&source.join(&relative), &sandbox.join(&relative))? {
            changed.push(relative);
        }
    }
    Ok(changed)
}

fn is_excluded(relative: &Path, exclusions: &[String]) -> bool {
    exclusions.iter().any(|exclusion| {
        relative == Path::new(exclusion) || relative.starts_with(format!("{exclusion}/"))
    })
}

fn collect_paths(root: &Path, relative: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        paths.push(child.clone());
        if entry.file_type()?.is_dir() {
            collect_paths(root, &child, paths)?;
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

pub(crate) fn apply_tree(source: &Path, destination: &Path, exclusions: &[String]) -> Result<()> {
    apply_tree_at(source, destination, Path::new(""), exclusions)
}

fn apply_tree_at(
    source: &Path,
    destination: &Path,
    relative: &Path,
    exclusions: &[String],
) -> Result<()> {
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        let child_relative = relative.join(entry.file_name());
        if is_excluded(&child_relative, exclusions) {
            continue;
        }
        if !source.join(entry.file_name()).exists() {
            remove_path(&entry.path())?;
        }
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let child_relative = relative.join(entry.file_name());
        if is_excluded(&child_relative, exclusions) {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&destination_path)?;
            apply_tree_at(&source_path, &destination_path, &child_relative, exclusions)?;
        } else {
            copy_entry(&source_path, &destination_path)?;
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
        std::os::unix::fs::symlink(target, path).unwrap();
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
    fn copy_tree_preserves_files_dirs_and_symlinks() {
        let source = test_temp_dir("pithos-test-copy-source");
        let destination = test_temp_dir("pithos-test-copy-destination");
        write_file(&source.0, "root.txt", "root");
        write_file(&source.0, "sub/nested.txt", "nested");
        make_symlink(&source.0, "link", "root.txt");

        copy_tree(&source.0, &destination.0).unwrap();

        assert_trees_equal(&source.0, &destination.0);
        let link_metadata = fs::symlink_metadata(destination.0.join("link")).unwrap();
        assert!(link_metadata.file_type().is_symlink());
        assert_eq!(
            fs::read_link(destination.0.join("link")).unwrap(),
            Path::new("root.txt")
        );
    }

    #[test]
    fn same_file_comparisons() {
        let first = test_temp_dir("pithos-test-same-first");
        let second = test_temp_dir("pithos-test-same-second");
        write_file(&first.0, "equal.txt", "same");
        write_file(&second.0, "equal.txt", "same");
        write_file(&first.0, "different.txt", "one");
        write_file(&second.0, "different.txt", "two");
        write_file(&first.0, "one-sided.txt", "only first");
        make_symlink(&first.0, "same-link", "equal.txt");
        make_symlink(&second.0, "same-link", "equal.txt");
        make_symlink(&first.0, "other-link", "different.txt");
        make_symlink(&second.0, "other-link", "equal.txt");
        fs::create_dir(first.0.join("dir")).unwrap();
        fs::create_dir(second.0.join("dir")).unwrap();

        assert!(same_file(&first.0.join("equal.txt"), &second.0.join("equal.txt")).unwrap());
        assert!(
            !same_file(
                &first.0.join("different.txt"),
                &second.0.join("different.txt")
            )
            .unwrap()
        );
        assert!(same_file(&first.0.join("missing.txt"), &second.0.join("missing.txt")).unwrap());
        assert!(same_file(&first.0.join("dir"), &second.0.join("dir")).unwrap());
        assert!(same_file(&first.0.join("same-link"), &second.0.join("same-link")).unwrap());
        assert!(!same_file(&first.0.join("other-link"), &second.0.join("other-link")).unwrap());
        assert!(!same_file(&first.0.join("one-sided.txt"), &second.0.join("equal.txt")).unwrap());
    }

    #[test]
    fn has_changes_detects_add_modify_delete_and_exclusions() {
        let host = test_temp_dir("pithos-test-changes-host");
        let sandbox = test_temp_dir("pithos-test-changes-sandbox");
        write_file(&host.0, "modified.txt", "host");
        write_file(&sandbox.0, "modified.txt", "sandbox");
        write_file(&host.0, "unchanged.txt", "same");
        write_file(&sandbox.0, "unchanged.txt", "same");
        write_file(&host.0, "deleted.txt", "gone");
        write_file(&host.0, "excluded/config.txt", "secret");
        write_file(&sandbox.0, "excluded/config.txt", "also secret");
        write_file(&sandbox.0, "added.txt", "new");
        let exclusions = vec!["excluded".to_string()];

        let changed = has_changes(&host.0, &sandbox.0, &exclusions).unwrap();

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
        write_file(&host.0, "keep.txt", "keep");
        write_file(&sandbox.0, "keep.txt", "keep");
        write_file(&host.0, "sub/modified.txt", "host");
        write_file(&sandbox.0, "sub/modified.txt", "sandbox");
        write_file(&host.0, "sub/removed.txt", "remove me");
        write_file(&sandbox.0, "added.txt", "new");
        write_file(&sandbox.0, "sub/new/deep.txt", "deep");
        make_symlink(&host.0, "removed-link", "keep.txt");
        make_symlink(&sandbox.0, "added-link", "added.txt");

        apply_tree(&sandbox.0, &host.0, &[]).unwrap();

        assert_trees_equal(&sandbox.0, &host.0);
    }

    #[test]
    fn apply_tree_respects_exclusions() {
        let host = test_temp_dir("pithos-test-exclude-host");
        let sandbox = test_temp_dir("pithos-test-exclude-sandbox");
        write_file(&host.0, "keep.txt", "host");
        write_file(&sandbox.0, "keep.txt", "sandbox");
        write_file(&host.0, "secret/config.txt", "host secret");
        write_file(&sandbox.0, "secret/config.txt", "sandbox secret");
        write_file(&sandbox.0, "added.txt", "new");
        let exclusions = vec!["secret".to_string()];

        apply_tree(&sandbox.0, &host.0, &exclusions).unwrap();

        assert_eq!(
            fs::read_to_string(host.0.join("keep.txt")).unwrap(),
            "sandbox"
        );
        assert_eq!(
            fs::read_to_string(host.0.join("secret/config.txt")).unwrap(),
            "host secret"
        );
        assert_eq!(fs::read_to_string(host.0.join("added.txt")).unwrap(), "new");
    }

    #[test]
    fn is_excluded_matches_exact_and_children() {
        let exclusions = vec![".git".to_string(), "target".to_string()];
        assert!(is_excluded(Path::new(".git"), &exclusions));
        assert!(is_excluded(Path::new(".git/HEAD"), &exclusions));
        assert!(is_excluded(Path::new("target"), &exclusions));
        assert!(is_excluded(Path::new("target/debug/pithos"), &exclusions));
        assert!(!is_excluded(Path::new("src/main.rs"), &exclusions));
        assert!(!is_excluded(Path::new(".github"), &exclusions));
        assert!(!is_excluded(Path::new("targeting"), &exclusions));
    }
}

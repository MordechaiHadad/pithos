use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::sandbox::{matches_any, temporary_base};
use std::sync::atomic::{AtomicU64, Ordering};

const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FileMeta {
    pub(crate) is_dir: bool,
    pub(crate) is_symlink: bool,
    pub(crate) size: u64,
    pub(crate) mtime_nanos: Option<u128>,
    #[cfg(unix)]
    pub(crate) mode: Option<u32>,
    #[cfg(not(unix))]
    pub(crate) mode: Option<u32>,
    pub(crate) symlink_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub(crate) version: u32,
    pub(crate) session_id: String,
    pub(crate) created_at: u64,
    pub(crate) unmanaged: Vec<String>,
    pub(crate) strategy: Option<String>,
    pub(crate) entries: BTreeMap<String, FileMeta>,
}

impl Snapshot {
    fn new(session_id: String, unmanaged: Vec<String>, strategy: Option<String>) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            session_id,
            created_at: unix_now(),
            unmanaged,
            strategy,
            entries: BTreeMap::new(),
        }
    }
}

pub(crate) fn manifest_dir() -> Result<PathBuf> {
    let dir = temporary_base()?.join("manifests");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub(crate) fn manifest_path(session_id: &str) -> Result<PathBuf> {
    let safe = sanitize_id(session_id);
    Ok(manifest_dir()?.join(format!("{safe}.json")))
}

pub(crate) fn capture(root: &Path, unmanaged: &[String]) -> Result<BTreeMap<String, FileMeta>> {
    let mut entries = BTreeMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        let dir = root.join(&relative);
        let read = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).wrap_err(format!("cannot read dir {}", dir.display())),
        };
        for entry in read {
            let entry = entry?;
            let child_relative = relative.join(entry.file_name());
            if is_excluded(&child_relative, unmanaged) {
                continue;
            }
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            let file_type = meta.file_type();
            let is_symlink = file_type.is_symlink();
            let is_dir = file_type.is_dir() && !is_symlink;
            let size = if is_dir || is_symlink { 0 } else { meta.len() };
            let mtime_nanos = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos());
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                Some(meta.permissions().mode())
            };
            #[cfg(not(unix))]
            let mode: Option<u32> = None;
            let symlink_target = if is_symlink {
                fs::read_link(&path).ok().map(|t| t.display().to_string())
            } else {
                None
            };
            let key = child_relative.to_string_lossy().to_string();
            entries.insert(
                key,
                FileMeta {
                    is_dir,
                    is_symlink,
                    size,
                    mtime_nanos,
                    mode,
                    symlink_target,
                },
            );
            if is_dir {
                stack.push(child_relative);
            }
        }
    }
    Ok(entries)
}

static SNAPSHOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn save_snapshot(
    session_id: &str,
    entries: BTreeMap<String, FileMeta>,
    unmanaged: &[String],
    strategy: Option<&str>,
) -> Result<()> {
    let mut snapshot = Snapshot::new(
        session_id.to_string(),
        unmanaged.to_vec(),
        strategy.map(ToString::to_string),
    );
    snapshot.entries = entries;
    let path = manifest_path(session_id)?;
    let unique = SNAPSHOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!(
        "tmp-{}-{}-{}",
        std::process::id(),
        unique,
        unix_now()
    ));
    let contents = serde_json::to_string(&snapshot).wrap_err("cannot serialize snapshot")?;
    fs::write(&tmp, contents).wrap_err("cannot write snapshot tmp")?;
    if let Err(error) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(error).wrap_err("cannot rename snapshot");
    }
    Ok(())
}

pub(crate) fn load_snapshot(session_id: &str) -> Result<Option<Snapshot>> {
    let path = manifest_path(session_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path).wrap_err("cannot read snapshot")?;
    let snapshot: Snapshot = serde_json::from_str(&contents).wrap_err("cannot parse snapshot")?;
    if snapshot.version != SNAPSHOT_VERSION {
        return Ok(None);
    }
    Ok(Some(snapshot))
}

pub(crate) fn remove_snapshot(session_id: &str) {
    if let Ok(path) = manifest_path(session_id) {
        let _ = fs::remove_file(path);
    }
}

pub(crate) fn is_snapshot_valid(
    snapshot: &Snapshot,
    unmanaged: &[String],
    strategy: Option<&str>,
) -> bool {
    if snapshot.unmanaged.len() != unmanaged.len() {
        return false;
    }
    let mut expected = snapshot.unmanaged.clone();
    let mut actual = unmanaged.to_vec();
    expected.sort();
    actual.sort();
    if expected != actual {
        return false;
    }
    let snap_strategy = snapshot.strategy.as_deref();
    match (snap_strategy, strategy) {
        (None, None) => true,
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

#[allow(dead_code)]
pub(crate) fn diff_maps(
    old: &BTreeMap<String, FileMeta>,
    new: &BTreeMap<String, FileMeta>,
) -> Vec<String> {
    let mut changed = Vec::new();
    for (path, old_meta) in old {
        match new.get(path) {
            None => changed.push(path.clone()),
            Some(new_meta) => {
                if old_meta.is_dir && new_meta.is_dir {
                    continue;
                }
                if old_meta != new_meta {
                    changed.push(path.clone());
                }
            }
        }
    }
    for path in new.keys() {
        if !old.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed.dedup();
    changed
}

pub(crate) fn try_has_changes_via_snapshot(
    repo: &Path,
    sandbox: &Path,
    unmanaged: &[String],
    strategy: Option<&str>,
    session_id: &str,
) -> Result<Option<Vec<PathBuf>>> {
    let snapshot = match load_snapshot(session_id)? {
        Some(snapshot) if is_snapshot_valid(&snapshot, unmanaged, strategy) => snapshot,
        Some(_) => {
            tracing::debug!("snapshot invalid for session {session_id}; falling back");
            return Ok(None);
        }
        None => {
            tracing::debug!("no snapshot for session {session_id}; falling back");
            return Ok(None);
        }
    };
    let _ = snapshot;
    has_changes_via_snapshot(repo, sandbox, unmanaged)
}

pub(crate) fn has_changes_via_snapshot(
    repo: &Path,
    sandbox: &Path,
    unmanaged: &[String],
) -> Result<Option<Vec<PathBuf>>> {
    let repo_map = capture(repo, unmanaged)?;
    let sandbox_map = capture(sandbox, unmanaged)?;
    let mut needs_content_check: Vec<String> = Vec::new();
    let mut changed: Vec<PathBuf> = Vec::new();
    for (path, repo_meta) in &repo_map {
        match sandbox_map.get(path) {
            None => changed.push(PathBuf::from(path)),
            Some(sandbox_meta) => {
                if repo_meta.is_dir && sandbox_meta.is_dir {
                    continue;
                }
                if repo_meta.is_symlink || sandbox_meta.is_symlink {
                    if repo_meta.symlink_target != sandbox_meta.symlink_target {
                        changed.push(PathBuf::from(path));
                    }
                    continue;
                }
                if repo_meta.is_dir != sandbox_meta.is_dir {
                    changed.push(PathBuf::from(path));
                    continue;
                }
                if repo_meta.size != sandbox_meta.size {
                    changed.push(PathBuf::from(path));
                    continue;
                }
                if repo_meta.mtime_nanos != sandbox_meta.mtime_nanos {
                    needs_content_check.push(path.clone());
                }
            }
        }
    }
    for path in sandbox_map.keys() {
        if !repo_map.contains_key(path) {
            changed.push(PathBuf::from(path.clone()));
        }
    }
    for path in needs_content_check {
        let repo_path = repo.join(&path);
        let sandbox_path = sandbox.join(&path);
        if !same_file_content(&repo_path, &sandbox_path)? {
            changed.push(PathBuf::from(path));
        }
    }
    changed.sort();
    changed.dedup();
    Ok(Some(changed))
}

fn same_file_content(first: &Path, second: &Path) -> Result<bool> {
    let first_bytes = fs::read(first).wrap_err("cannot read file")?;
    let second_bytes = fs::read(second).wrap_err("cannot read file")?;
    Ok(first_bytes == second_bytes)
}

fn is_excluded(relative: &Path, unmanaged: &[String]) -> bool {
    if relative == Path::new(".git") || relative.starts_with(".git/") {
        return true;
    }
    matches_any(relative, unmanaged)
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(crate) fn sweep_manifests(live_ids: &[String]) -> Result<()> {
    let dir = temporary_base()?.join("manifests");
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if !live_ids.contains(&stem) {
            let metadata = entry.metadata()?;
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default();
            if age.as_secs() > 60 * 60 {
                let _ = fs::remove_file(&path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::TempDir;
    use std::fs;

    fn write_file(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn capture_and_diff_detects_changes() {
        let dir = TempDir::create("pithos-snapshot-capture").unwrap();
        write_file(dir.path(), "a.txt", "hello");
        write_file(dir.path(), "sub/b.txt", "world");
        let first = capture(dir.path(), &[]).unwrap();
        write_file(dir.path(), "a.txt", "modified");
        fs::remove_file(dir.path().join("sub/b.txt")).unwrap();
        write_file(dir.path(), "c.txt", "new");
        let second = capture(dir.path(), &[]).unwrap();
        let diff = diff_maps(&first, &second);
        assert!(diff.contains(&"a.txt".to_string()));
        assert!(diff.contains(&"sub/b.txt".to_string()));
        assert!(!diff.contains(&"sub".to_string()));
        assert!(diff.contains(&"c.txt".to_string()));
    }

    #[test]
    fn has_changes_via_snapshot_matches_has_changes() {
        let repo = TempDir::create("pithos-snap-repo").unwrap();
        let sandbox = TempDir::create("pithos-snap-sandbox").unwrap();
        write_file(repo.path(), "modified.txt", "host");
        write_file(sandbox.path(), "modified.txt", "sandbox");
        write_file(repo.path(), "deleted.txt", "gone");
        write_file(sandbox.path(), "added.txt", "new");
        write_file(repo.path(), "unchanged.txt", "same");
        write_file(sandbox.path(), "unchanged.txt", "same");
        let via_snapshot = has_changes_via_snapshot(repo.path(), sandbox.path(), &[])
            .unwrap()
            .unwrap();
        let via_old = crate::sandbox::has_changes(repo.path(), sandbox.path(), &[]).unwrap();
        let mut via_snapshot_sorted = via_snapshot.clone();
        let mut via_old_sorted = via_old.clone();
        via_snapshot_sorted.sort();
        via_old_sorted.sort();
        assert_eq!(via_snapshot_sorted, via_old_sorted);
    }

    #[test]
    fn mtime_optimization_avoids_read_when_size_differs() {
        let repo = TempDir::create("pithos-snap-mtime-repo").unwrap();
        let sandbox = TempDir::create("pithos-snap-mtime-sandbox").unwrap();
        write_file(repo.path(), "a.txt", "short");
        write_file(sandbox.path(), "a.txt", "much longer content");
        let changed = has_changes_via_snapshot(repo.path(), sandbox.path(), &[])
            .unwrap()
            .unwrap();
        assert_eq!(changed, vec![PathBuf::from("a.txt")]);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::create("pithos-snap-roundtrip").unwrap();
        write_file(dir.path(), "a.txt", "hello");
        let entries = capture(dir.path(), &[]).unwrap();
        let id = "test-snap-1234";
        save_snapshot(id, entries.clone(), &[], Some("reflink")).unwrap();
        let loaded = load_snapshot(id).unwrap().unwrap();
        assert_eq!(loaded.session_id, id);
        assert_eq!(loaded.entries, entries);
        assert!(is_snapshot_valid(&loaded, &[], Some("reflink")));
        assert!(!is_snapshot_valid(
            &loaded,
            &["target".to_string()],
            Some("reflink")
        ));
        remove_snapshot(id);
        assert!(load_snapshot(id).unwrap().is_none());
    }

    #[test]
    fn respects_unmanaged() {
        let dir = TempDir::create("pithos-snap-unmanaged").unwrap();
        write_file(dir.path(), "keep.txt", "keep");
        write_file(dir.path(), "target/cache.bin", "cache");
        let entries = capture(dir.path(), &["target".to_string()]).unwrap();
        assert!(entries.contains_key("keep.txt"));
        assert!(!entries.contains_key("target/cache.bin"));
        assert!(!entries.contains_key("target"));
    }
}

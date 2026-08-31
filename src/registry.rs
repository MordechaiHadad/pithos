use eyre::{Result, WrapErr, bail, eyre};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionRecord {
    #[serde(flatten)]
    pub(crate) identity: SessionIdentity,
    #[serde(flatten)]
    pub(crate) paths: SessionPaths,
    #[serde(flatten)]
    pub(crate) runtime: SessionRuntime,
    #[serde(flatten)]
    pub(crate) options: SessionOptions,
    #[serde(flatten)]
    pub(crate) lifecycle: SessionLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionIdentity {
    pub(crate) id: String,
    pub(crate) container_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionPaths {
    pub(crate) sandbox_path: PathBuf,
    pub(crate) repo_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionRuntime {
    pub(crate) image_tag: String,
    pub(crate) workspace: String,
    pub(crate) user: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct SessionOptions {
    #[serde(default)]
    pub(crate) unmanaged: Vec<String>,
    #[serde(default)]
    pub(crate) diff_viewer: Option<String>,
    #[serde(default)]
    pub(crate) strategy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionLifecycle {
    pub(crate) pid: u32,
    pub(crate) started_at: u64,
}

impl SessionRecord {
    pub(crate) fn new(input: SessionRecordInput<'_>) -> Self {
        let repository = input.repository;
        let repo_name = repository
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string());
        let id = format!("{}-{}", sanitize_name(&repo_name), random_suffix());
        Self {
            identity: SessionIdentity {
                container_name: format!("pithos-{id}"),
                id,
            },
            paths: SessionPaths {
                sandbox_path: input.sandbox_path.to_path_buf(),
                repo_path: repository.to_path_buf(),
            },
            runtime: SessionRuntime {
                image_tag: input.image_tag.to_string(),
                workspace: input.workspace.to_string(),
                user: input.user.to_string(),
            },
            options: SessionOptions {
                unmanaged: input.unmanaged,
                diff_viewer: input.diff_viewer,
                strategy: input.strategy.map(|s| s.label().to_string()),
            },
            lifecycle: SessionLifecycle {
                pid: std::process::id(),
                started_at: unix_now(),
            },
        }
    }

    pub(crate) fn save(&self) -> Result<()> {
        save_in(&runtime_dir(), self)
    }
}

pub(crate) struct SessionRecordInput<'a> {
    pub(crate) repository: &'a Path,
    pub(crate) sandbox_path: &'a Path,
    pub(crate) image_tag: &'a str,
    pub(crate) workspace: &'a str,
    pub(crate) user: &'a str,
    pub(crate) unmanaged: Vec<String>,
    pub(crate) diff_viewer: Option<String>,
    pub(crate) strategy: Option<crate::workspace::CopyStrategy>,
}

pub(crate) fn list() -> Vec<SessionRecord> {
    list_in(&runtime_dir())
}

pub(crate) fn sandbox_paths() -> Vec<PathBuf> {
    list()
        .into_iter()
        .map(|record| record.paths.sandbox_path)
        .collect()
}

pub(crate) fn remove(id: &str) {
    remove_in(&runtime_dir(), id);
    crate::snapshot::remove_snapshot(id);
}

pub(crate) fn prune() -> Result<Vec<SessionRecord>> {
    let sessions = list();
    if sessions.is_empty() {
        return Ok(sessions);
    }
    let running = running_container_names()?;
    let mut live = Vec::with_capacity(sessions.len());
    for session in sessions {
        if is_stale(&session, &running) {
            tracing::debug!(id = %session.identity.id, "pruning stale session record");
            remove(&session.identity.id);
        } else {
            live.push(session);
        }
    }
    Ok(live)
}

pub(crate) fn is_stale(record: &SessionRecord, running_names: &[String]) -> bool {
    !running_names.contains(&record.identity.container_name)
}

pub(crate) fn resolve(
    sessions: &[SessionRecord],
    requested: Option<&str>,
) -> Result<SessionRecord> {
    match sessions.len() {
        0 => bail!("no running pithos sessions"),
        1 => {
            let session = sessions.first().expect("session count is one");
            if let Some(requested) = requested
                && session.identity.id != requested
            {
                bail!(
                    "no session named \"{requested}\"; running session is \"{}\"",
                    session.identity.id
                );
            }
            Ok(session.clone())
        }
        _ => {
            let ids: Vec<&str> = sessions
                .iter()
                .map(|session| session.identity.id.as_str())
                .collect();
            let Some(requested) = requested else {
                bail!(
                    "multiple pithos sessions are running; pick one with its id: {}",
                    ids.join(", ")
                );
            };
            sessions
                .iter()
                .find(|session| session.identity.id == requested)
                .cloned()
                .ok_or_else(|| {
                    eyre!(
                        "no session named \"{requested}\"; running sessions: {}",
                        ids.join(", ")
                    )
                })
        }
    }
}

fn save_in(dir: &Path, record: &SessionRecord) -> Result<()> {
    fs::create_dir_all(dir).wrap_err("cannot create pithos runtime directory")?;
    let contents =
        serde_json::to_string_pretty(record).wrap_err("cannot serialize session record")?;
    fs::write(record_path(dir, &record.identity.id), contents)
        .wrap_err("cannot write session record")?;
    tracing::debug!(id = %record.identity.id, "saved session record");
    Ok(())
}

fn list_in(dir: &Path) -> Vec<SessionRecord> {
    let mut sessions = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return sessions;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(contents) = fs::read_to_string(&path)
            && let Ok(record) = serde_json::from_str::<SessionRecord>(&contents)
        {
            sessions.push(record);
        }
    }
    sessions.sort_by(|left, right| {
        left.lifecycle
            .started_at
            .cmp(&right.lifecycle.started_at)
            .then_with(|| left.identity.id.cmp(&right.identity.id))
    });
    sessions
}

fn remove_in(dir: &Path, id: &str) {
    tracing::debug!(id, "removing session record");
    let _ = fs::remove_file(record_path(dir, id));
}

fn record_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn running_container_names() -> Result<Vec<String>> {
    tracing::trace!("querying running podman containers");
    let output = Command::new("podman")
        .args(["ps", "--format", "{{.Names}}"])
        .output()
        .wrap_err("could not run podman ps")?;
    if !output.status.success() {
        bail!(
            "podman ps failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

pub(crate) fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("pithos")
}

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "repo".to_string()
    } else {
        trimmed.chars().take(32).collect()
    }
}

fn random_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    let mixed = (nanos ^ std::process::id().rotate_left(16)) & 0xffff;
    format!("{mixed:04x}")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::TempDir;

    fn sample_record(id: &str, started_at: u64) -> SessionRecord {
        SessionRecord {
            identity: SessionIdentity {
                id: id.to_string(),
                container_name: format!("pithos-{id}"),
            },
            paths: SessionPaths {
                sandbox_path: PathBuf::from("/tmp/sandbox"),
                repo_path: PathBuf::from("/tmp/repo"),
            },
            runtime: SessionRuntime {
                image_tag: "localhost/pithos-opencode:latest".to_string(),
                workspace: "/workspace".to_string(),
                user: "1000:1000".to_string(),
            },
            options: SessionOptions::default(),
            lifecycle: SessionLifecycle {
                pid: 42,
                started_at,
            },
        }
    }

    #[test]
    fn round_trip_preserves_fields_and_orders_by_start_time() {
        let dir = TempDir::create("pithos-registry-roundtrip").unwrap();
        save_in(dir.path(), &sample_record("b-0002", 200)).unwrap();
        save_in(dir.path(), &sample_record("a-0001", 100)).unwrap();

        let sessions = list_in(dir.path());

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].identity.id, "a-0001");
        assert_eq!(sessions[1].lifecycle.started_at, 200);
        assert_eq!(sessions[1].identity.container_name, "pithos-b-0002");
        assert_eq!(sessions[1].runtime.user, "1000:1000");
        assert_eq!(sessions[1].runtime.workspace, "/workspace");
    }

    #[test]
    fn round_trip_preserves_pull_settings() {
        let dir = TempDir::create("pithos-registry-pull-fields").unwrap();
        let mut record = sample_record("pull-0001", 10);
        record.options.unmanaged = vec!["target".to_string()];
        record.options.diff_viewer = Some("difftool -dir {dir}".to_string());
        save_in(dir.path(), &record).unwrap();

        let sessions = list_in(dir.path());

        assert_eq!(sessions[0].options.unmanaged, vec!["target".to_string()]);
        assert_eq!(
            sessions[0].options.diff_viewer.as_deref(),
            Some("difftool -dir {dir}")
        );
    }

    #[test]
    fn list_skips_corrupt_and_non_json_files() {
        let dir = TempDir::create("pithos-registry-corrupt").unwrap();
        save_in(dir.path(), &sample_record("good", 1)).unwrap();
        fs::write(dir.path().join("broken.json"), "not json").unwrap();
        fs::write(dir.path().join("ignored.txt"), "{}").unwrap();

        let sessions = list_in(dir.path());

        assert_eq!(
            sessions
                .iter()
                .map(|s| s.identity.id.as_str())
                .collect::<Vec<_>>(),
            ["good"]
        );
    }

    #[test]
    fn remove_deletes_only_target_record() {
        let dir = TempDir::create("pithos-registry-remove").unwrap();
        save_in(dir.path(), &sample_record("keep", 1)).unwrap();
        save_in(dir.path(), &sample_record("drop", 2)).unwrap();

        remove_in(dir.path(), "drop");

        let sessions = list_in(dir.path());
        assert_eq!(
            sessions
                .iter()
                .map(|s| s.identity.id.as_str())
                .collect::<Vec<_>>(),
            ["keep"]
        );
    }

    #[test]
    fn is_stale_checks_container_presence() {
        let record = sample_record("x-0001", 0);
        let running = vec!["pithos-x-0001".to_string()];
        assert!(!is_stale(&record, &running));
        assert!(is_stale(&record, &[]));
    }

    #[test]
    fn resolve_auto_picks_single_session() {
        let sessions = vec![sample_record("only-0001", 0)];
        assert_eq!(resolve(&sessions, None).unwrap().identity.id, "only-0001");
        assert_eq!(
            resolve(&sessions, Some("only-0001")).unwrap().identity.id,
            "only-0001"
        );
        assert!(resolve(&sessions, Some("other")).is_err());
    }

    #[test]
    fn resolve_requires_id_for_multiple_sessions() {
        let sessions = vec![sample_record("a-0001", 0), sample_record("b-0002", 1)];
        let error = resolve(&sessions, None).unwrap_err().to_string();
        assert!(error.contains("a-0001"));
        assert!(error.contains("b-0002"));
        assert_eq!(
            resolve(&sessions, Some("b-0002")).unwrap().identity.id,
            "b-0002"
        );
        assert!(resolve(&sessions, Some("c-0003")).is_err());
    }

    #[test]
    fn resolve_errors_without_sessions() {
        assert!(resolve(&[], None).is_err());
    }

    #[test]
    fn sanitize_replaces_unsafe_characters() {
        assert_eq!(sanitize_name("my_repo.v2"), "my_repo.v2");
        assert_eq!(sanitize_name("weird name/slash"), "weird-name-slash");
        assert_eq!(sanitize_name("---"), "repo");
    }

    #[test]
    fn new_builds_prefixed_container_name() {
        let repository = TempDir::create("pithos-registry-new").unwrap();
        let sandbox = TempDir::create("pithos-registry-new-sandbox").unwrap();

        let record = SessionRecord::new(SessionRecordInput {
            repository: repository.path(),
            sandbox_path: sandbox.path(),
            image_tag: "localhost/pithos-opencode:latest",
            workspace: "/workspace",
            user: "1000:1000",
            unmanaged: vec!["target".to_string()],
            diff_viewer: Some("difftool -dir {dir}".to_string()),
            strategy: Some(crate::workspace::CopyStrategy::Copy),
        });

        assert!(record.identity.id.starts_with("pithos-registry-new-"));
        assert_eq!(
            record.identity.container_name,
            format!("pithos-{}", record.identity.id)
        );
        assert_eq!(record.paths.sandbox_path, sandbox.path());
        assert_eq!(record.runtime.user, "1000:1000");
    }
}

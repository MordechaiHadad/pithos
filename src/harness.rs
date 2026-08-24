use eyre::WrapErr;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agent::AGENT_HOME;
use crate::platform;

pub type Allowlist = BTreeMap<String, Value>;

pub(crate) fn tmpfs_spec(target: &str) -> String {
    format!("{target}:rw,mode=1777")
}

struct HarnessPaths {
    data: PathBuf,
    state: PathBuf,
    config: PathBuf,
    credentials: PathBuf,
}

/// Access policy for a harness content entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Bind the host file read-only into the container.
    ReadOnly,
    /// Bind an existing host file read-write; contents sync but the file can
    /// never be created, deleted, or replaced from inside the container.
    Pinned,
}

/// A single host file exposed to the container through the content map.
#[derive(Debug, Clone)]
pub(crate) struct ContentEntry {
    pub(crate) host: PathBuf,
    pub(crate) target: String,
    pub(crate) access: Access,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "name", rename_all = "kebab-case")]
pub enum Harness {
    Opencode {
        #[serde(default = "default_command")]
        command: Vec<String>,
        #[serde(default)]
        allowlist: Option<Allowlist>,
        #[serde(default)]
        credentials: bool,
    },
}

impl Harness {
    pub fn install(&self) -> String {
        match self {
            Self::Opencode { .. } => "RUN npm install --global opencode-ai\n".into(),
        }
    }

    pub fn command(&self) -> &[String] {
        match self {
            Self::Opencode { command, .. } => command,
        }
    }

    pub fn mount(&self, command: &mut Command) -> eyre::Result<()> {
        match self {
            Self::Opencode { .. } => {
                let paths = self.paths()?;

                command.args([
                    "--tmpfs",
                    &tmpfs_spec(&format!("{AGENT_HOME}/.local/share/opencode")),
                ]);
                command.args([
                    "--tmpfs",
                    &tmpfs_spec(&format!("{AGENT_HOME}/.local/state/opencode")),
                ]);
                command.args(["--tmpfs", &tmpfs_spec(&format!("{AGENT_HOME}/.cache"))]);
                command.args(["--tmpfs", &tmpfs_spec(&format!("{AGENT_HOME}/.serena"))]);

                for entry in self.content_map()? {
                    let read_only = entry.access == Access::ReadOnly;
                    if read_only {
                        if !self.credentials_enabled() || !entry.host.exists() {
                            continue;
                        }
                    } else {
                        ensure_pinned_file(&entry.host)?;
                    }
                    mount_path(command, &entry.host, &entry.target, read_only)?;
                }

                if self.config_required() {
                    mount_if_exists(
                        command,
                        &paths.config,
                        &format!("{AGENT_HOME}/.config/opencode"),
                        true,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Files the harness expects to persist across sessions, keyed by the
    /// harness variant. Pinned entries are pre-created on the host so podman
    /// binds regular files instead of inventing directories.
    fn content_map(&self) -> eyre::Result<Vec<ContentEntry>> {
        match self {
            Self::Opencode { .. } => {
                let paths = self.paths()?;
                let share = format!("{AGENT_HOME}/.local/share/opencode");
                let state = format!("{AGENT_HOME}/.local/state/opencode");
                Ok(vec![
                    ContentEntry {
                        host: paths.credentials.clone(),
                        target: format!("{share}/auth.json"),
                        access: Access::ReadOnly,
                    },
                    ContentEntry {
                        host: paths.data.join("opencode.db"),
                        target: format!("{share}/opencode.db"),
                        access: Access::Pinned,
                    },
                    ContentEntry {
                        host: paths.data.join("opencode.db-wal"),
                        target: format!("{share}/opencode.db-wal"),
                        access: Access::Pinned,
                    },
                    ContentEntry {
                        host: paths.data.join("opencode.db-shm"),
                        target: format!("{share}/opencode.db-shm"),
                        access: Access::Pinned,
                    },
                    ContentEntry {
                        host: paths.state.join("kv.json"),
                        target: format!("{state}/kv.json"),
                        access: Access::Pinned,
                    },
                    ContentEntry {
                        host: paths.state.join("session.json"),
                        target: format!("{state}/session.json"),
                        access: Access::Pinned,
                    },
                    ContentEntry {
                        host: paths.state.join("model.json"),
                        target: format!("{state}/model.json"),
                        access: Access::Pinned,
                    },
                    ContentEntry {
                        host: paths.state.join("prompt-history.jsonl"),
                        target: format!("{state}/prompt-history.jsonl"),
                        access: Access::Pinned,
                    },
                ])
            }
        }
    }

    fn paths(&self) -> eyre::Result<HarnessPaths> {
        match self {
            Self::Opencode { .. } => Ok(HarnessPaths {
                data: data_path("opencode")?,
                state: state_path("opencode")?,
                config: home_path(".config/opencode")?,
                credentials: home_path(".local/share/opencode/auth.json")?,
            }),
        }
    }

    pub fn environment(&self) -> Vec<(String, String)> {
        match self {
            Self::Opencode { allowlist, .. } => allowlist
                .as_ref()
                .map(|allowlist| {
                    vec![(
                        "OPENCODE_CONFIG_CONTENT".into(),
                        json!({ "permission": allowlist }).to_string(),
                    )]
                })
                .unwrap_or_default(),
        }
    }

    fn credentials_enabled(&self) -> bool {
        match self {
            Self::Opencode { credentials, .. } => *credentials,
        }
    }

    fn config_required(&self) -> bool {
        match self {
            Self::Opencode { allowlist, .. } => self.credentials_enabled() || allowlist.is_some(),
        }
    }
}

fn default_command() -> Vec<String> {
    vec!["opencode".into(), "/workspace".into()]
}

fn data_path(application: &str) -> eyre::Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| eyre::eyre!("cannot determine harness data directory"))?;
    Ok(data_dir.join(application))
}

fn state_path(application: &str) -> eyre::Result<PathBuf> {
    let state_dir =
        dirs::state_dir().ok_or_else(|| eyre::eyre!("cannot determine harness state directory"))?;
    Ok(state_dir.join(application))
}

fn home_path(relative: &str) -> eyre::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("cannot determine home directory"))?;
    Ok(home.join(relative))
}

/// Create an empty file if it does not exist, without truncating existing
/// content.
fn ensure_pinned_file(path: &Path) -> eyre::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("cannot create directory {}", parent.display()))?;
    }
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .wrap_err_with(|| format!("cannot create pinned file {}", path.display()))?;
    Ok(())
}

fn mount_if_exists(
    command: &mut Command,
    source: &Path,
    target: &str,
    read_only: bool,
) -> eyre::Result<()> {
    if source.exists() {
        mount_path(command, source, target, read_only)?;
    }
    Ok(())
}

fn mount_path(
    command: &mut Command,
    source: &Path,
    target: &str,
    read_only: bool,
) -> eyre::Result<()> {
    command.args([
        "--volume",
        &platform::volume_spec(source, target, read_only),
    ]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn tmpfs_mounts_are_world_writable_with_sticky_bit() {
        assert_eq!(tmpfs_spec("/tmp"), "/tmp:rw,mode=1777");
        assert_eq!(
            tmpfs_spec(&format!("{AGENT_HOME}/.cache")),
            "/home/agent/.cache:rw,mode=1777"
        );
    }

    #[test]
    fn opencode_allowlist_becomes_permission_env() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [harness.allowlist]
            edit = "deny"

            [harness.allowlist.bash]
            "*" = "ask"
            "git *" = "allow"
            "#,
        )
        .unwrap();
        let environment = config.harness.environment();
        let (key, value) = environment.first().unwrap();
        assert_eq!(key, "OPENCODE_CONFIG_CONTENT");
        let parsed: Value = serde_json::from_str(value).unwrap();
        assert_eq!(parsed["permission"]["edit"], "deny");
        assert_eq!(parsed["permission"]["bash"]["*"], "ask");
        assert_eq!(parsed["permission"]["bash"]["git *"], "allow");
    }

    #[test]
    fn harness_without_allowlist_injects_nothing() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        assert!(config.harness.environment().is_empty());
    }

    #[test]
    fn allowlist_mounts_config_folder_without_credentials() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [harness.allowlist]
            edit = "deny"
            "#,
        )
        .unwrap();
        assert!(config.harness.config_required());
        assert!(!config.harness.credentials_enabled());
    }

    #[test]
    fn no_allowlist_no_credentials_mounts_nothing() {
        let config: Config = toml::from_str(
            r#"[harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        assert!(!config.harness.config_required());
        assert!(!config.harness.credentials_enabled());
    }

    #[test]
    fn home_paths_resolve_relative_to_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            home_path(".config/opencode").unwrap(),
            home.join(".config/opencode")
        );
        assert_eq!(
            home_path(".config/opencode").unwrap(),
            home.join(".config/opencode")
        );
    }

    #[test]
    fn credentials_enable_config_folder_and_auth() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            credentials = true
            "#,
        )
        .unwrap();
        assert!(config.harness.config_required());
        assert!(config.harness.credentials_enabled());
    }

    #[test]
    fn runtime_folders_are_tmpfs_and_content_is_file_mounted() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        let mut command = Command::new("true");
        config.harness.mount(&mut command).unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let share_target = format!("{AGENT_HOME}/.local/share/opencode");
        let state_target = format!("{AGENT_HOME}/.local/state/opencode");

        assert!(
            args.iter().any(|arg| arg == &tmpfs_spec(&share_target)),
            "expected {share_target} as tmpfs, got {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == &tmpfs_spec(&state_target)),
            "expected {state_target} as tmpfs, got {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.ends_with(&format!(":{share_target}:rw"))),
            "share dir must not be a whole-directory bind mount, got {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.ends_with(&format!(":{state_target}:rw"))),
            "state dir must not be a whole-directory bind mount, got {args:?}"
        );

        for name in [
            "opencode.db",
            "opencode.db-wal",
            "opencode.db-shm",
            "kv.json",
            "session.json",
            "model.json",
            "prompt-history.jsonl",
        ] {
            let expected_suffix = format!("/{name}:rw");
            assert!(
                args.iter().any(|arg| arg.ends_with(&expected_suffix)),
                "expected a pinned rw mount ending with {expected_suffix}, got {args:?}"
            );
        }
    }

    #[test]
    fn without_credentials_auth_json_is_not_mounted() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            allowlist = { edit = "deny" }
            "#,
        )
        .unwrap();
        let mut command = Command::new("true");
        config.harness.mount(&mut command).unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|arg| arg.contains("auth.json")),
            "auth.json must not be mounted without credentials, got {args:?}"
        );
    }

    #[test]
    fn credentials_mount_auth_json_read_only() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            credentials = true
            "#,
        )
        .unwrap();
        let auth_path = dirs::home_dir()
            .unwrap()
            .join(".local/share/opencode/auth.json");
        if !auth_path.exists() {
            fs::create_dir_all(auth_path.parent().unwrap()).unwrap();
            fs::write(&auth_path, "{}").unwrap();
        }
        let mut command = Command::new("true");
        config.harness.mount(&mut command).unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|arg| arg.ends_with("/auth.json:ro")),
            "expected a read-only auth.json mount, got {args:?}"
        );
    }
}

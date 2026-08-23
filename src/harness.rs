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
            Self::Opencode { credentials, .. } => {
                let paths = self.paths()?;
                fs::create_dir_all(&paths.data)?;
                mount_path(
                    command,
                    &paths.data,
                    &format!("{AGENT_HOME}/.local/share/opencode"),
                    false,
                )?;
                fs::create_dir_all(&paths.state)?;
                mount_path(
                    command,
                    &paths.state,
                    &format!("{AGENT_HOME}/.local/state/opencode"),
                    false,
                )?;
                command.args(["--tmpfs", &tmpfs_spec(&format!("{AGENT_HOME}/.cache"))]);
                command.args(["--tmpfs", &tmpfs_spec(&format!("{AGENT_HOME}/.serena"))]);
                if self.config_required() {
                    mount_if_exists(
                        command,
                        &paths.config,
                        &format!("{AGENT_HOME}/.config/opencode"),
                        true,
                    )?;
                }
                if *credentials {
                    mount_if_exists(
                        command,
                        &paths.credentials,
                        &format!("{AGENT_HOME}/.local/share/opencode/auth.json"),
                        true,
                    )?;
                }
            }
        }
        Ok(())
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
    fn state_dir_is_bind_mounted_not_tmpfs() {
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
        let state_target = format!("{AGENT_HOME}/.local/state/opencode");
        let tmpfs_spec = tmpfs_spec(&state_target);
        assert!(
            args.iter()
                .any(|arg| arg.ends_with(&format!(":{state_target}:rw"))),
            "expected a read-write bind mount for {state_target}, got {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == &tmpfs_spec),
            "state dir must not be a tmpfs, got {args:?}"
        );
    }
}

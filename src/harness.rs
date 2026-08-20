use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::config::Config;

pub type Allowlist = BTreeMap<String, Value>;

#[derive(Debug, Deserialize)]
#[serde(tag = "name", rename_all = "kebab-case")]
pub enum Harness {
    Opencode {
        #[serde(default = "default_command")]
        command: Vec<String>,
        #[serde(default)]
        allowlist: Option<Allowlist>,
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

    pub fn mount(&self, command: &mut Command, config: &Config) -> eyre::Result<()> {
        match self {
            Self::Opencode { .. } => {
                if self.config_required(config) {
                    // The rootfs is read-only; give the node user writable
                    // home dirs so MCP servers (serena state, deno cache,
                    // opencode state) can initialize. Per-directory tmpfs;
                    // the .local/share/opencode mount is what the ro auth
                    // file bind sits on top of.
                    command.args(["--tmpfs", "/home/node/.local/share/opencode"]);
                    command.args(["--tmpfs", "/home/node/.local/state/opencode"]);
                    command.args(["--tmpfs", "/home/node/.cache"]);
                    command.args(["--tmpfs", "/home/node/.serena"]);
                    mount_if_exists(
                        command,
                        "~/.config/opencode",
                        "/home/node/.config/opencode:ro",
                    )?;
                }
                if self.credentials_enabled(config) {
                    mount_if_exists(
                        command,
                        "~/.local/share/opencode/auth.json",
                        "/home/node/.local/share/opencode/auth.json:ro,Z",
                    )?;
                }
            }
        }
        Ok(())
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

    fn credentials_enabled(&self, config: &Config) -> bool {
        match self {
            Self::Opencode { .. } => config.credentials.opencode,
        }
    }

    fn config_required(&self, config: &Config) -> bool {
        match self {
            Self::Opencode { allowlist, .. } => {
                self.credentials_enabled(config) || allowlist.is_some()
            }
        }
    }
}

fn default_command() -> Vec<String> {
    vec!["opencode".into(), "/workspace".into()]
}

fn resolved_source(source: &str) -> eyre::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| eyre::eyre!("cannot determine home directory"))?;
    Ok(home.join(source.trim_start_matches("~/")))
}

fn mount_if_exists(command: &mut Command, source: &str, target: &str) -> eyre::Result<()> {
    let path = resolved_source(source)?;
    if path.exists() {
        command.args(["--volume", &format!("{}:{target}", path.display())]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(config.harness.config_required(&config));
        assert!(!config.harness.credentials_enabled(&config));
    }

    #[test]
    fn no_allowlist_no_credentials_mounts_nothing() {
        let config: Config = toml::from_str(
            r#"[harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        assert!(!config.harness.config_required(&config));
        assert!(!config.harness.credentials_enabled(&config));
    }

    #[test]
    fn source_paths_resolve_relative_to_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            resolved_source("~/.config/opencode").unwrap(),
            home.join(".config/opencode")
        );
        assert_eq!(
            resolved_source(".config/opencode").unwrap(),
            home.join(".config/opencode")
        );
    }

    #[test]
    fn credentials_enable_config_folder_and_auth() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [credentials]
            opencode = true
            "#,
        )
        .unwrap();
        assert!(config.harness.config_required(&config));
        assert!(config.harness.credentials_enabled(&config));
    }
}

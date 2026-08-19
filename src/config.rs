use eyre::{Result, WrapErr, bail, eyre};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::harness::Harness;

#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    #[serde(default = "default_base_image")]
    pub(crate) base_image: String,
    #[serde(default = "default_workspace")]
    pub(crate) workspace: String,
    #[serde(default = "default_image_tag")]
    pub(crate) image_tag: String,
    #[serde(default)]
    pub(crate) install: Vec<String>,
    #[serde(default)]
    pub(crate) toolchains: Vec<Toolchain>,
    #[serde(default)]
    pub(crate) cargo: Vec<String>,
    #[serde(default)]
    pub(crate) npm: Vec<String>,
    #[serde(default)]
    pub(crate) bun: Vec<String>,
    #[serde(default)]
    pub(crate) uv: Vec<UvTool>,
    #[serde(default)]
    pub(crate) downloads: Vec<Download>,
    pub(crate) harness: Harness,
    #[serde(default)]
    pub(crate) credentials: Credentials,
    #[serde(default)]
    pub(crate) environment: BTreeMap<String, String>,
    #[serde(default = "default_exclusions")]
    pub(crate) exclusions: Vec<String>,
    #[serde(default)]
    pub(crate) diff_viewer: Option<String>,
    #[serde(default)]
    pub(crate) networking: Option<Networking>,
}

pub(crate) const DEFAULT_WHITELIST: &[&str] = &["opencode.ai", "mcp.exa.ai", "api.exa.ai"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Toolchain {
    Rust,
    Python,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Networking {
    /// Per-connection upload cap in KiB; unset means no cap.
    #[serde(default)]
    pub(crate) payload_size: Option<u64>,
    /// Per-session egress budget in KiB; unset means no quota.
    #[serde(default)]
    pub(crate) quota: Option<u64>,
    /// Extra hosts that bypass the cap and quota; appended to DEFAULT_WHITELIST.
    #[serde(default)]
    pub(crate) whitelist: Vec<String>,
    #[serde(default = "default_use_default_whitelist")]
    pub(crate) use_default_whitelist: bool,
}

fn default_use_default_whitelist() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UvTool {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) python: Option<String>,
    #[serde(default)]
    pub(crate) run: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Download {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Credentials {
    #[serde(default)]
    pub(crate) opencode: bool,
}

fn default_exclusions() -> Vec<String> {
    vec![]
}

fn default_base_image() -> String {
    "node:22-bookworm-slim".into()
}
fn default_workspace() -> String {
    "/workspace".into()
}
fn default_image_tag() -> String {
    "localhost/pithos-opencode:latest".into()
}

impl Config {
    pub(crate) fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = resolve_config(explicit)?;
        let config: Config =
            toml::from_str(&fs::read_to_string(&path).wrap_err("cannot read config")?)
                .wrap_err("invalid TOML configuration")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.harness.command().is_empty() {
            bail!("harness.command cannot be empty")
        }
        if self.workspace.is_empty() || !self.workspace.starts_with('/') {
            bail!("workspace must be an absolute container path")
        }
        if self.uv.iter().any(|tool| tool.name.is_empty()) {
            bail!("uv tool name cannot be empty")
        }
        if self.downloads.iter().any(|d| d.url.is_empty()) {
            bail!("download url cannot be empty")
        }
        if let Some(viewer) = &self.diff_viewer
            && !viewer.contains("{dir}")
        {
            bail!("diff_viewer must contain the {{dir}} placeholder")
        }
        if self.environment.contains_key("diff_viewer") {
            bail!("diff_viewer must be a top-level key, not inside [environment]")
        }
        if let Some(networking) = &self.networking {
            if networking.payload_size.is_none() && networking.quota.is_none() {
                bail!("[networking] requires at least payload_size or quota")
            }
            if networking.payload_size == Some(0) {
                bail!("networking.payload_size must be greater than 0")
            }
            if networking.quota == Some(0) {
                bail!("networking.quota must be greater than 0")
            }
        }
        Ok(())
    }
}

fn resolve_config(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return fs::canonicalize(path)
            .wrap_err_with(|| format!("cannot find config {}", path.display()));
    }
    let local = Path::new("pithos.toml");
    if local.exists() {
        return fs::canonicalize(local).wrap_err("cannot read local pithos.toml");
    }
    let global = dirs::config_dir()
        .ok_or_else(|| eyre!("cannot determine config directory"))?
        .join("pithos/pithos.toml");
    if global.exists() {
        return fs::canonicalize(global).wrap_err("cannot read global pithos.toml");
    }
    bail!(
        "no config found: tried ./pithos.toml and {}",
        global.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Config {
        fn parse(toml: &str) -> Self {
            toml::from_str(toml).unwrap()
        }

        fn try_parse(toml: &str) -> Result<Self> {
            Ok(toml::from_str(toml)?)
        }
    }

    #[test]
    fn toolchains_parse_known_names() {
        let config = Config::parse(
            r#"
            toolchains = ["rust", "python"]

            [harness]
            name = "opencode"
            "#,
        );
        assert_eq!(
            config.toolchains,
            vec![Toolchain::Rust, Toolchain::Python]
        );
    }

    #[test]
    fn toolchains_default_to_empty() {
        let config = Config::parse(
            r#"
            [harness]
            name = "opencode"
            "#,
        );
        assert!(config.toolchains.is_empty());
    }

    #[test]
    fn toolchains_reject_unknown_names() {
        assert!(
            Config::try_parse(
                r#"
            toolchains = ["rust", "golang"]

            [harness]
            name = "opencode"
            "#,
            )
            .is_err()
        );
    }

    #[test]
    fn diff_viewer_requires_dir_placeholder() {
        assert!(
            Config::parse(
                r#"
            diff_viewer = "lazygit -p {dir}"

            [harness]
            name = "opencode"
            "#,
            )
            .validate()
            .is_ok()
        );
        assert!(
            Config::parse(
                r#"
            diff_viewer = "lazygit -p /tmp"

            [harness]
            name = "opencode"
            "#,
            )
            .validate()
            .is_err()
        );
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"
            "#,
            )
            .validate()
            .is_ok()
        );
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"

            [environment]
            TERM = "xterm-256color"
            diff_viewer = "lazygit -p {dir}"
            "#,
            )
            .validate()
            .is_err()
        );
    }

    #[test]
    fn networking_requires_at_least_one_limiter() {
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            payload_size = 8
            "#,
            )
            .validate()
            .is_ok()
        );
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            quota = 102400
            "#,
            )
            .validate()
            .is_ok()
        );
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            "#,
            )
            .validate()
            .is_err()
        );
    }

    #[test]
    fn networking_rejects_zero_and_negative_limits() {
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            payload_size = 0
            "#,
            )
            .validate()
            .is_err()
        );
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            quota = 0
            "#,
            )
            .validate()
            .is_err()
        );
        assert!(
            Config::try_parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            payload_size = -1
            "#,
            )
            .is_err()
        );
    }

    #[test]
    fn networking_quota_is_a_number_not_a_string() {
        assert!(
            Config::try_parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            quota = "100 mbytes"
            "#,
            )
            .is_err()
        );
    }

    #[test]
    fn networking_rejects_unknown_fields() {
        assert!(
            Config::try_parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            payload_size = 8
            payload_size_kb = 8
            "#,
            )
            .is_err()
        );
    }

    #[test]
    fn networking_defaults_to_use_default_whitelist() {
        let config = Config::parse(
            r#"
            [harness]
            name = "opencode"

            [networking]
            quota = 102400
            "#,
        );
        let networking = config.networking.unwrap();
        assert!(networking.use_default_whitelist);
        assert_eq!(
            DEFAULT_WHITELIST,
            ["opencode.ai", "mcp.exa.ai", "api.exa.ai"]
        );
    }
}

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
    vec![".git".into()]
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

pub(crate) fn load(explicit: Option<&Path>) -> Result<Config> {
    let path = resolve_config(explicit)?;
    let config: Config = toml::from_str(&fs::read_to_string(&path).wrap_err("cannot read config")?)
        .wrap_err("invalid TOML configuration")?;
    validate(&config)?;
    Ok(config)
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

fn validate(config: &Config) -> Result<()> {
    if config.harness.command().is_empty() {
        bail!("harness.command cannot be empty")
    }
    if config.workspace.is_empty() || !config.workspace.starts_with('/') {
        bail!("workspace must be an absolute container path")
    }
    if config.uv.iter().any(|tool| tool.name.is_empty()) {
        bail!("uv tool name cannot be empty")
    }
    if config.downloads.iter().any(|d| d.url.is_empty()) {
        bail!("download url cannot be empty")
    }
    if let Some(viewer) = &config.diff_viewer
        && !viewer.contains("{dir}")
    {
        bail!("diff_viewer must contain the {{dir}} placeholder")
    }
    if config.environment.contains_key("diff_viewer") {
        bail!("diff_viewer must be a top-level key, not inside [environment]")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn diff_viewer_requires_dir_placeholder() {
        assert!(
            validate(&parse(
                r#"
            diff_viewer = "lazygit -p {dir}"

            [harness]
            name = "opencode"
            "#,
            ))
            .is_ok()
        );
        assert!(
            validate(&parse(
                r#"
            diff_viewer = "lazygit -p /tmp"

            [harness]
            name = "opencode"
            "#,
            ))
            .is_err()
        );
        assert!(
            validate(&parse(
                r#"
            [harness]
            name = "opencode"
            "#,
            ))
            .is_ok()
        );
        assert!(
            validate(&parse(
                r#"
            [harness]
            name = "opencode"

            [environment]
            TERM = "xterm-256color"
            diff_viewer = "lazygit -p {dir}"
            "#,
            ))
            .is_err()
        );
    }
}

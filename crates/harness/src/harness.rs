use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{Result, bail};
use serde::Deserialize;

use crate::def::HarnessDef;
use crate::registry;
use crate::types::HarnessDependency;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    name: String,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    sandbox_config: Option<String>,
    #[serde(default)]
    credentials: bool,
}

impl Harness {
    pub fn install(&self) -> String {
        self.definition()
            .map(|definition| definition.install)
            .unwrap_or_default()
    }

    pub fn depends_on(&self) -> Vec<HarnessDependency> {
        self.definition()
            .map(|definition| definition.depends_on)
            .unwrap_or_default()
    }

    pub fn command(&self) -> &[String] {
        &self.command
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn credentials_enabled(&self) -> bool {
        self.credentials
    }

    pub fn sandbox_config_raw(&self) -> Option<&str> {
        self.sandbox_config.as_deref()
    }

    pub fn apply_harness_override(&mut self, harness_name: String) -> Result<()> {
        let definition = registry::find(&harness_name).ok_or_else(|| {
            eyre::eyre!(
                "unknown harness \"{}\"; available: {}",
                harness_name,
                registry::available_names().join(", ")
            )
        })?;
        if definition.command.is_empty() {
            bail!(
                "harness \"{}\" has no default command; add `command` to its definition",
                harness_name
            );
        }
        self.name = harness_name;
        self.command = definition.command.clone();
        self.credentials = false;
        self.sandbox_config = None;
        Ok(())
    }

    pub fn mount(
        &self,
        command: &mut Command,
        session_id: &str,
        runtime_base: &Path,
        config_dir: &Path,
    ) -> Result<()> {
        let definition = self.require_definition()?;
        let override_file = self.resolve_override(config_dir)?;
        crate::mount::apply_mounts(
            &definition,
            command,
            session_id,
            runtime_base,
            override_file.as_deref(),
            self.credentials,
        )
    }

    pub fn validate(&self, config_dir: &Path) -> Result<()> {
        let definition = self.require_definition()?;
        validate_sink(&definition)?;
        if let Some(path) = self.resolve_override(config_dir)? {
            validate_override_file(&definition, &path)?;
        }
        Ok(())
    }

    pub(crate) fn resolve_override(&self, config_dir: &Path) -> Result<Option<PathBuf>> {
        let Some(raw) = self.sandbox_config_raw() else {
            return Ok(None);
        };
        let path = resolve_prefixed_path(raw, config_dir)?;
        if path.is_dir() {
            bail!(
                "harness.sandbox_config must point to a file, got directory {}",
                path.display()
            );
        }
        if !path.exists() {
            bail!(
                "harness.sandbox_config not found: {} (resolved from {raw:?} against {})",
                path.display(),
                config_dir.display(),
            );
        }
        if is_inside_git_worktree(&path) {
            tracing::warn!(
                path = %path.display(),
                "sandbox_config lives inside the git worktree; \
                 move it next to pithos.toml with a gitignore entry or under \
                 ~/.config/pithos to avoid committing secrets"
            );
        }
        Ok(Some(path))
    }

    fn definition(&self) -> Option<HarnessDef> {
        registry::find(&self.name)
    }

    fn require_definition(&self) -> Result<HarnessDef> {
        self.definition().ok_or_else(|| {
            eyre::eyre!(
                "unknown harness \"{}\"; available: {}",
                self.name,
                registry::available_names().join(", ")
            )
        })
    }
}

pub(crate) fn resolve_prefixed_path(raw: &str, config_dir: &Path) -> Result<PathBuf> {
    if let Some(rest) = raw.strip_prefix("config:") {
        if rest.is_empty() {
            bail!("harness.sandbox_config \"config:\" requires a path after the prefix");
        }
        return Ok(config_dir.join(rest));
    }
    if let Some(rest) = raw.strip_prefix("cwd:") {
        if rest.is_empty() {
            bail!("harness.sandbox_config \"cwd:\" requires a path after the prefix");
        }
        return Ok(PathBuf::from(rest));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home =
            dirs::home_dir().ok_or_else(|| eyre::eyre!("cannot determine home directory"))?;
        return Ok(home.join(rest));
    }
    if raw == "~" {
        return dirs::home_dir().ok_or_else(|| eyre::eyre!("cannot determine home directory"));
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return Ok(path);
    }
    bail!(
        "harness.sandbox_config {raw:?} needs an explicit location; \
         use \"config:<path>\" for a file next to pithos.toml, \
         \"cwd:<path>\" for a path relative to the shell, \
         \"~/...\" for home, or an absolute path"
    );
}

fn is_inside_git_worktree(path: &Path) -> bool {
    let anchor = path.parent().unwrap_or(Path::new("."));
    let mut current = Some(anchor);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return true;
        }
        current = dir.parent();
    }
    false
}

fn validate_sink(definition: &HarnessDef) -> Result<()> {
    let sink = &definition.allowlist;
    if !sink.has_sink() {
        return Ok(());
    }
    if !sink.target.starts_with('/') {
        bail!(
            "harness \"{}\" allowlist target must be an absolute container path, got {:?}",
            definition.name,
            sink.target
        );
    }
    Ok(())
}

fn validate_override_file(definition: &HarnessDef, path: &Path) -> Result<()> {
    if !definition.allowlist.has_sink() {
        bail!(
            "harness \"{}\" does not support harness.sandbox_config; remove the key",
            definition.name
        );
    }
    let bytes = std::fs::read(path)
        .map_err(|error| eyre::eyre!("cannot read {}: {error}", path.display()))?;
    match definition.allowlist.format {
        crate::types::AllowlistFormat::Json => {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .map_err(|error| eyre::eyre!("{} is not valid JSON: {error}", path.display()))?;
        }
        crate::types::AllowlistFormat::Toml => {
            toml::from_str::<toml::Value>(
                std::str::from_utf8(&bytes)
                    .map_err(|_| eyre::eyre!("{} is not valid UTF-8 TOML", path.display()))?,
            )
            .map_err(|error| eyre::eyre!("{} is not valid TOML: {error}", path.display()))?;
        }
        crate::types::AllowlistFormat::Raw => {}
    }
    Ok(())
}

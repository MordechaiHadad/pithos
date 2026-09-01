use std::fs;
use std::path::{Path, PathBuf};

use eyre::WrapErr;

use crate::def::{HarnessDef, HarnessToml};

pub fn user_harness_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("pithos").join("harnesses"))
}

pub fn load_user_harnesses() -> Vec<HarnessDef> {
    let Some(dir) = user_harness_dir() else {
        return Vec::new();
    };
    load_from_dir(&dir)
}

pub fn load_from_dir(dir: &Path) -> Vec<HarnessDef> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match load_one(&path) {
            Ok(def) => out.push(def),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "ignoring invalid harness TOML");
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn load_one(path: &Path) -> eyre::Result<HarnessDef> {
    let text =
        fs::read_to_string(path).wrap_err_with(|| format!("cannot read {}", path.display()))?;
    let parsed: HarnessToml =
        toml::from_str(&text).wrap_err_with(|| format!("invalid TOML {}", path.display()))?;
    if parsed.schema_version != 1 {
        eyre::bail!("unsupported schema_version {}", parsed.schema_version);
    }
    if parsed.name.is_empty() {
        eyre::bail!("harness name cannot be empty");
    }
    Ok(parsed.into())
}

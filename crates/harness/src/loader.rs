use std::fs;
use std::path::{Path, PathBuf};

use eyre::WrapErr;

use crate::def::HarnessDef;

pub fn user_harness_dir() -> Option<PathBuf> {
    config_dir_candidates()
        .into_iter()
        .find(|candidate| candidate.join("pithos").join("harnesses").exists())
        .or_else(|| dirs::config_dir().map(|dir| dir.join("pithos").join("harnesses")))
        .or_else(windows_config_fallback)
}

fn config_dir_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = dirs::config_dir() {
        candidates.push(dir);
    }
    if let Some(fallback) = windows_config_fallback()
        && !candidates.contains(&fallback)
    {
        candidates.push(fallback);
    }
    candidates
}

fn windows_config_fallback() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".config"))
}

pub fn load_user_harnesses() -> Vec<HarnessDef> {
    let mut combined: std::collections::BTreeMap<String, HarnessDef> =
        std::collections::BTreeMap::new();
    for base in config_dir_candidates() {
        let dir = base.join("pithos").join("harnesses");
        for def in load_from_dir(&dir) {
            combined.insert(def.name.clone(), def);
        }
    }
    let mut out: Vec<HarnessDef> = combined.into_values().collect();
    out.sort_by(|left, right| left.name.cmp(&right.name));
    out
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
    HarnessDef::from_toml_str(&text).wrap_err_with(|| format!("invalid {}", path.display()))
}

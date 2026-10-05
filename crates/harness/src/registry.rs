use std::collections::BTreeMap;

use crate::def::HarnessDef;
use crate::loader;

const EMBEDDED_TOMLS: &[(&str, &str)] = &[
    ("claude-code", include_str!("../harnesses/claude-code.toml")),
    ("codex", include_str!("../harnesses/codex.toml")),
    ("opencode", include_str!("../harnesses/opencode.toml")),
];

fn builtin_defs() -> Vec<HarnessDef> {
    let mut out = Vec::new();
    for &(name, content) in EMBEDDED_TOMLS {
        match HarnessDef::from_toml_str(content) {
            Ok(def) => out.push(def),
            Err(error) => {
                tracing::error!(name, %error, "invalid embedded harness TOML");
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn all_harnesses() -> Vec<HarnessDef> {
    let mut map: BTreeMap<String, HarnessDef> = BTreeMap::new();
    for def in builtin_defs() {
        map.insert(def.name.clone(), def);
    }
    for def in loader::load_user_harnesses() {
        map.insert(def.name.clone(), def);
    }
    map.into_values().collect()
}

pub fn find(name: &str) -> Option<HarnessDef> {
    let user = loader::load_user_harnesses();
    if let Some(found) = user.into_iter().find(|def| def.name == name) {
        return Some(found);
    }
    builtin_defs().into_iter().find(|def| def.name == name)
}

pub fn available_names() -> Vec<String> {
    all_harnesses().into_iter().map(|def| def.name).collect()
}

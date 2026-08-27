use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::def::HarnessDef;
use super::loader;

static EMBEDDED: OnceLock<Vec<HarnessDef>> = OnceLock::new();

fn embedded() -> &'static Vec<HarnessDef> {
    EMBEDDED.get_or_init(|| {
        let mut out = Vec::new();
        for (name, content) in embedded_toml_files() {
            match HarnessDef::from_toml_str(content) {
                Ok(def) => out.push(def),
                Err(error) => {
                    tracing::error!(name, %error, "invalid embedded harness TOML");
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    })
}

fn embedded_toml_files() -> Vec<(&'static str, &'static str)> {
    generated::EMBEDDED.to_vec()
}

mod generated {
    include!(concat!(env!("OUT_DIR"), "/harnesses.rs"));
}

pub fn all_harnesses() -> Vec<HarnessDef> {
    let mut map: BTreeMap<String, HarnessDef> = BTreeMap::new();
    for def in embedded().iter().cloned() {
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
    embedded().iter().find(|def| def.name == name).cloned()
}

#[cfg(test)]
fn embedded_names() -> Vec<String> {
    embedded().iter().map(|def| def.name.clone()).collect()
}

pub fn available_names() -> Vec<String> {
    all_harnesses().into_iter().map(|def| def.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_contains_opencode_and_claude() {
        let names = embedded_names();
        assert!(names.contains(&"opencode".to_string()));
        assert!(names.contains(&"claude-code".to_string()));
    }

    #[test]
    fn find_returns_cloned() {
        assert!(find("opencode").is_some());
        assert!(find("nonexistent").is_none());
    }
}

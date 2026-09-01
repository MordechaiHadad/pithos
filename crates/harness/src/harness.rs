use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use eyre::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::def::HarnessDef;
use crate::registry;
use crate::types;
use crate::types::HarnessDependency;

pub type Allowlist = BTreeMap<String, Value>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    name: String,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    allowlist: Option<Allowlist>,
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

    pub fn mount(
        &self,
        command: &mut Command,
        session_id: &str,
        runtime_base: &Path,
    ) -> Result<()> {
        let definition = self.require_definition()?;
        crate::mount::apply_mounts(
            &definition,
            command,
            session_id,
            runtime_base,
            self.allowlist.as_ref(),
            self.credentials,
        )
    }

    pub fn validate(&self) -> Result<()> {
        let definition = self.require_definition()?;
        let Some(allowlist) = &self.allowlist else {
            return Ok(());
        };
        validate_allowlist(&definition, allowlist)
    }

    pub fn environment(&self) -> Vec<(String, String)> {
        let Some(definition) = self.definition() else {
            return Vec::new();
        };
        let Some(allowlist) = &self.allowlist else {
            return Vec::new();
        };
        match definition.allowlist.translation {
            types::Translation::PassthroughEnv => {
                let variable = definition
                    .allowlist
                    .env_var
                    .clone()
                    .unwrap_or_else(|| "OPENCODE_CONFIG_CONTENT".to_string());
                vec![(variable, json!({ "permission": allowlist }).to_string())]
            }
            types::Translation::None | types::Translation::ClaudeSettings => Vec::new(),
        }
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

fn validate_allowlist(definition: &HarnessDef, allowlist: &Allowlist) -> Result<()> {
    if definition.allowlist.translation == types::Translation::ClaudeSettings {
        for (key, value) in allowlist {
            if key != "bash" && key != "edit" {
                bail!(
                    "harness.allowlist key \"{key}\" is not supported by the {} harness; supported keys are \"bash\" and \"edit\"",
                    definition.name
                );
            }
            match (key.as_str(), value) {
                ("edit", Value::String(verdict)) => ensure_verdict(verdict)?,
                ("edit", _) => {
                    bail!("harness.allowlist.edit must be \"allow\", \"ask\", or \"deny\"")
                }
                ("bash", Value::Object(patterns)) => {
                    for (pattern, verdict) in patterns {
                        let Value::String(verdict) = verdict else {
                            bail!("harness.allowlist.bash.\"{pattern}\" must be a string")
                        };
                        ensure_verdict(verdict)?;
                    }
                }
                ("bash", _) => {
                    bail!("harness.allowlist.bash must be a table of pattern = verdict")
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

fn ensure_verdict(verdict: &str) -> Result<()> {
    if matches!(verdict, "allow" | "ask" | "deny") {
        Ok(())
    } else {
        bail!("unsupported verdict \"{verdict}\"; use \"allow\", \"ask\", or \"deny\"")
    }
}

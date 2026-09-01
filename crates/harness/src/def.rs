use serde::Deserialize;

use super::types::{Access, HostBase, MountType, Translation};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessToml {
    pub schema_version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub install: String,
    #[serde(default)]
    #[serde(rename = "mount")]
    pub mounts: Vec<MountToml>,
    #[serde(default)]
    pub allowlist: AllowlistToml,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountToml {
    pub host: String,
    pub target: String,
    #[serde(rename = "type")]
    pub mount_type: MountType,
    pub access: Access,
    pub host_base: HostBase,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowlistToml {
    #[serde(default)]
    pub translation: Translation,
    #[serde(default)]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HarnessDef {
    pub name: String,
    pub install: String,
    pub mounts: Vec<MountDef>,
    pub allowlist: AllowlistDef,
}

#[derive(Debug, Clone)]
pub struct MountDef {
    pub host: String,
    pub target: String,
    pub mount_type: MountType,
    pub access: Access,
    pub host_base: HostBase,
}

#[derive(Debug, Clone)]
pub struct AllowlistDef {
    pub translation: Translation,
    pub env_var: Option<String>,
}

impl From<HarnessToml> for HarnessDef {
    fn from(value: HarnessToml) -> Self {
        Self {
            name: value.name,
            install: value.install,
            mounts: value.mounts.into_iter().map(Into::into).collect(),
            allowlist: value.allowlist.into(),
        }
    }
}

impl From<MountToml> for MountDef {
    fn from(value: MountToml) -> Self {
        Self {
            host: value.host,
            target: value.target,
            mount_type: value.mount_type,
            access: value.access,
            host_base: value.host_base,
        }
    }
}

impl From<AllowlistToml> for AllowlistDef {
    fn from(value: AllowlistToml) -> Self {
        Self {
            translation: value.translation,
            env_var: value.env_var,
        }
    }
}

impl HarnessDef {
    pub fn from_toml_str(input: &str) -> eyre::Result<Self> {
        let parsed: HarnessToml = toml::from_str(input)?;
        if parsed.schema_version != 1 {
            eyre::bail!("unsupported schema_version {}", parsed.schema_version);
        }
        if parsed.name.is_empty() {
            eyre::bail!("harness name cannot be empty");
        }
        Ok(parsed.into())
    }
}

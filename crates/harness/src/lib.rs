pub mod agent;
pub mod def;
pub mod harness;
pub mod loader;
pub mod mount;
pub mod platform;
pub mod registry;
pub mod translate;
pub mod types;

pub use def::HarnessDef;
pub use harness::{Allowlist, Harness};
pub use mount::tmpfs_spec;
pub use types::{Access, HostBase, MountType, Translation};

use std::path::Path;
use std::process::Command;

pub trait HarnessSpec {
    fn name(&self) -> &str;
    fn install(&self) -> &str;
    fn mounts(&self) -> &[def::MountDef];
    fn allowlist_def(&self) -> &def::AllowlistDef;
    fn mount(&self, command: &mut Command, session_id: &str, runtime_base: &Path, allowlist: Option<&Allowlist>, credentials_enabled: bool) -> eyre::Result<()>;
    fn environment(&self, allowlist: Option<&Allowlist>) -> Vec<(String, String)>;
}

impl HarnessSpec for HarnessDef {
    fn name(&self) -> &str {
        &self.name
    }

    fn install(&self) -> &str {
        &self.install
    }

    fn mounts(&self) -> &[def::MountDef] {
        &self.mounts
    }

    fn allowlist_def(&self) -> &def::AllowlistDef {
        &self.allowlist
    }

    fn mount(&self, command: &mut Command, session_id: &str, runtime_base: &Path, allowlist: Option<&Allowlist>, credentials_enabled: bool) -> eyre::Result<()> {
        mount::apply_mounts(self, command, session_id, runtime_base, allowlist, credentials_enabled)
    }

    fn environment(&self, allowlist: Option<&Allowlist>) -> Vec<(String, String)> {
        let Some(allowlist) = allowlist else {
            return Vec::new();
        };
        match self.allowlist.translation {
            types::Translation::PassthroughEnv => {
                let variable = self
                    .allowlist
                    .env_var
                    .clone()
                    .unwrap_or_else(|| "OPENCODE_CONFIG_CONTENT".to_string());
                vec![(variable, serde_json::json!({ "permission": allowlist }).to_string())]
            }
            types::Translation::None | types::Translation::ClaudeSettings => Vec::new(),
        }
    }
}

impl HarnessSpec for Harness {
    fn name(&self) -> &str {
        Harness::name(self)
    }

    fn install(&self) -> &str {
        // Harness::install returns owned String; leak reference for trait? return static dispatch via owned.
        // We return empty for trait's str ref and provide owned via inherent method; trait not used for Harness install directly.
        ""
    }

    fn mounts(&self) -> &[def::MountDef] {
        &[]
    }

    fn allowlist_def(&self) -> &def::AllowlistDef {
        // not applicable; return dummy static
        static DUMMY: def::AllowlistDef = def::AllowlistDef {
            translation: Translation::None,
            env_var: None,
        };
        &DUMMY
    }

    fn mount(&self, command: &mut Command, session_id: &str, runtime_base: &Path, _allowlist: Option<&Allowlist>, _credentials_enabled: bool) -> eyre::Result<()> {
        Harness::mount(self, command, session_id, runtime_base)
    }

    fn environment(&self, _allowlist: Option<&Allowlist>) -> Vec<(String, String)> {
        Harness::environment(self)
    }
}

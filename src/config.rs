use eyre::{Result, WrapErr, bail, eyre};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::harness::Harness;

#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    #[serde(default = "default_base_image")]
    pub(crate) base_image: String,
    #[serde(default = "default_workspace")]
    pub(crate) workspace: String,
    #[serde(default)]
    pub(crate) image_tag: Option<String>,
    #[serde(default = "default_install")]
    pub(crate) install: Vec<String>,
    #[serde(default)]
    pub(crate) toolchains: Vec<String>,
    #[serde(default, rename = "toolchain")]
    pub(crate) toolchain_defs: BTreeMap<String, ToolchainDef>,
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
    #[serde(default)]
    pub(crate) mise: Vec<String>,
    pub(crate) harness: Harness,
    #[serde(skip, default)]
    pub(crate) global_toolchains: BTreeMap<String, ToolchainDef>,
    #[serde(default)]
    pub(crate) environment: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) ephemeral: Vec<String>,
    #[serde(default)]
    pub(crate) ignore: Vec<String>,
    #[serde(default)]
    pub(crate) diff_viewer: Option<String>,
    /// Forces a workspace population tier (`reflink`, `worktree` or `copy`)
    /// instead of auto-detecting the fastest one the platform supports.
    #[serde(default)]
    pub(crate) copy_strategy: Option<String>,
    #[serde(default)]
    pub(crate) networking: Networking,
    #[serde(default)]
    pub(crate) audio: bool,
}

pub(crate) const DEFAULT_WHITELIST: &[&str] = &[
    "opencode.ai",
    "mcp.exa.ai",
    "api.exa.ai",
    "api.parallel.ai",
    "search.parallel.ai",
    "task-mcp.parallel.ai",
    "api.tavily.com",
    "api.search.brave.com",
    "google.serper.dev",
    "api.anthropic.com",
];

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolchainDef {
    #[serde(default)]
    pub(crate) includes: Vec<String>,
    #[serde(default)]
    pub(crate) install: Vec<String>,
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) run: Vec<String>,
    #[serde(default)]
    pub(crate) mise: Vec<String>,
    #[serde(default)]
    pub(crate) extra: Vec<String>,
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
}

fn default_enabled() -> bool {
    true
}

/// Per-connection payload cap in KiB.
fn default_payload_size() -> Option<u64> {
    Some(65_536)
}

/// Per-session egress budget in KiB.
fn default_quota() -> Option<u64> {
    Some(2_097_152)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Networking {
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
    /// Per-connection payload cap in KiB.
    #[serde(default = "default_payload_size")]
    pub(crate) payload_size: Option<u64>,
    /// Per-session egress budget in KiB.
    #[serde(default = "default_quota")]
    pub(crate) quota: Option<u64>,
    /// Extra hosts that bypass the cap and quota; appended to DEFAULT_WHITELIST.
    #[serde(default)]
    pub(crate) whitelist: Vec<String>,
    #[serde(default = "default_use_default_whitelist")]
    pub(crate) use_default_whitelist: bool,
    /// Drop traffic to RFC1918/link-local destinations except DNS (port 53).
    #[serde(default = "default_enabled")]
    pub(crate) block_private: bool,
}

impl Default for Networking {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            payload_size: default_payload_size(),
            quota: default_quota(),
            whitelist: Vec::new(),
            use_default_whitelist: true,
            block_private: true,
        }
    }
}

fn default_use_default_whitelist() -> bool {
    true
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UvTool {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) python: Option<String>,
    #[serde(default)]
    pub(crate) run: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Download {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
}

fn default_install() -> Vec<String> {
    vec![
        "git".into(),
        "gcc".into(),
        "libc6-dev".into(),
        "ncurses-term".into(),
    ]
}

fn default_base_image() -> String {
    "node:22-bookworm-slim".into()
}
fn default_workspace() -> String {
    "/workspace".into()
}

impl Config {
    pub(crate) fn init() -> Result<()> {
        let path = Path::new("pithos.toml");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .wrap_err("cannot create pithos.toml")?;
        let contents = starter_config();
        std::io::Write::write_all(&mut file, contents.as_bytes())
            .wrap_err("cannot write pithos.toml")?;
        println!("created {}", path.display());
        Ok(())
    }

    pub(crate) fn load(explicit: Option<&Path>, toolchain: Option<String>) -> Result<Self> {
        let path = resolve_config(explicit)?;
        let mut config: Config =
            toml::from_str(&fs::read_to_string(&path).wrap_err("cannot read config")?)
                .wrap_err("invalid TOML configuration")?;
        config.global_toolchains = load_global_toolchains()?;
        config.with_toolchain(toolchain);
        config.validate()?;
        Ok(config)
    }

    /// Resolves the image tag: an explicit `image_tag` from the config wins,
    /// otherwise the tag is derived from the harness name so each harness
    /// builds into its own image namespace.
    pub(crate) fn image_tag(&self) -> String {
        match &self.image_tag {
            Some(tag) => tag.clone(),
            None => format!("localhost/pithos-{}:latest", self.harness.name()),
        }
    }

    pub(crate) fn with_toolchain(&mut self, toolchain: Option<String>) {
        if let Some(name) = toolchain {
            self.toolchains = vec![name.clone()];
            self.image_tag = Some(format!("{}-{}", self.image_tag(), name));
        }
    }

    pub(crate) fn merged_toolchains(&self) -> BTreeMap<String, ToolchainDef> {
        let mut defs = BTreeMap::new();
        defs.extend(
            self.global_toolchains
                .iter()
                .map(|(name, def)| (name.clone(), def.clone())),
        );
        defs.extend(
            self.toolchain_defs
                .iter()
                .map(|(name, def)| (name.clone(), def.clone())),
        );
        defs
    }

    pub(crate) fn expanded_selection(
        &self,
        defs: &BTreeMap<String, ToolchainDef>,
    ) -> Result<Vec<String>> {
        fn expand_name(
            name: &str,
            defs: &BTreeMap<String, ToolchainDef>,
            out: &mut Vec<String>,
            seen: &mut BTreeSet<String>,
            path: &mut Vec<String>,
        ) -> Result<()> {
            if let Some(start) = path.iter().position(|entry| entry == name) {
                let mut chain = path[start..].to_vec();
                chain.push(name.to_owned());
                bail!("cyclic toolchain includes: {}", chain.join(" -> "));
            }
            if !defs.contains_key(name) {
                let included_by = path
                    .last()
                    .map(|parent| format!(" (included by \"{parent}\")"))
                    .unwrap_or_default();
                bail!(
                    "unknown toolchain \"{name}\"{included_by}; available: {}",
                    defs.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }
            if seen.insert(name.to_owned()) {
                out.push(name.to_owned());
                path.push(name.to_owned());
                for included in &defs[name].includes {
                    expand_name(included, defs, out, seen, path)?;
                }
                path.pop();
            }
            Ok(())
        }

        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for name in &self.toolchains {
            let mut path = Vec::new();
            expand_name(name, defs, &mut out, &mut seen, &mut path)?;
        }
        Ok(out)
    }

    fn validate(&self) -> Result<()> {
        if self.harness.command().is_empty() {
            bail!("harness.command cannot be empty")
        }
        self.harness.validate()?;
        if self.workspace.is_empty() || !self.workspace.starts_with('/') {
            bail!("workspace must be an absolute container path")
        }
        if self.image_tag.as_deref() == Some("") {
            bail!("image_tag cannot be empty")
        }
        let defs = self.merged_toolchains();
        for name in defs.keys() {
            if !valid_toolchain_name(name) {
                bail!("invalid toolchain name \"{name}\": use letters, digits, '-', '_', '.'")
            }
        }
        for name in self.expanded_selection(&defs)? {
            let def = &defs[&name];
            if def.uv.iter().any(|tool| tool.name.is_empty()) {
                bail!("uv tool name cannot be empty (toolchain {name})")
            }
            if def.downloads.iter().any(|download| download.url.is_empty()) {
                bail!("download url cannot be empty (toolchain {name})")
            }
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
        if self.networking.payload_size == Some(0) {
            bail!("networking.payload_size must be greater than 0")
        }
        if self.networking.quota == Some(0) {
            bail!("networking.quota must be greater than 0")
        }
        Ok(())
    }
}

fn starter_config() -> String {
    r#"base_image = "node:22-bookworm-slim"
workspace = "/workspace"

# Install tools from the mise registry (supports name@version and backend:name):
# mise = ["neovim", "lua-language-server"]
# Define your own toolchains:
# [toolchain.example]
# install = ["curl"]
# env = { PATH = "/opt/example/bin:$PATH" }
# run = ["curl -fsSL https://example.com/install.sh | sh"]
# extra = ["example --version"]

[harness]
name = "opencode"
command = ["opencode", "/workspace"]

# Egress is capped by default (64 MiB per connection, 2 GiB per session,
# private networks blocked). Override any knob:
# [networking]
# payload_size = 65536
# quota = 2097152
"#
    .to_string()
}

fn valid_toolchain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[derive(Debug, Default, Deserialize)]
struct ToolchainLibrary {
    #[serde(default, rename = "toolchain")]
    defs: BTreeMap<String, ToolchainDef>,
}

fn load_global_toolchains() -> Result<BTreeMap<String, ToolchainDef>> {
    let Some(dir) = dirs::config_dir() else {
        return Ok(BTreeMap::new());
    };
    let path = dir.join("pithos").join("toolchains.toml");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let library: ToolchainLibrary =
        toml::from_str(&fs::read_to_string(&path).wrap_err("cannot read global toolchains.toml")?)
            .wrap_err("invalid global toolchains.toml")?;
    Ok(library.defs)
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
            command = ["opencode", "/workspace"]
            "#,
        );
        assert_eq!(config.toolchains, ["rust", "python"]);
    }

    #[test]
    fn custom_toolchain_definitions_parse() {
        let config = Config::parse(
            r#"
            toolchains = ["golang"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.golang]
            install = ["ca-certificates"]
            env = { PATH = "/usr/local/go/bin:$PATH" }
            run = ["curl -fsSL https://go.dev/dl/go.tar.gz | tar -C /usr/local -xz"]
            extra = ["go version"]
            "#,
        );
        let def = config.toolchain_defs.get("golang").unwrap();
        assert_eq!(def.install, ["ca-certificates"]);
        assert_eq!(def.env["PATH"], "/usr/local/go/bin:$PATH");
        assert_eq!(def.run.len(), 1);
        assert_eq!(def.extra, ["go version"]);
    }

    #[test]
    fn merged_toolchains_merge_library_and_project_definitions() {
        let mut config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.golang]
            run = ["install-go"]
            "#,
        )
        .unwrap();
        config.global_toolchains.insert(
            "vim".into(),
            ToolchainDef {
                run: vec!["install-vim".into()],
                ..Default::default()
            },
        );
        let defs = config.merged_toolchains();
        assert_eq!(defs["vim"].run, ["install-vim"]);
        assert_eq!(defs["golang"].run, ["install-go"]);
    }

    #[test]
    fn unknown_selected_toolchain_fails_validation_listing_available() {
        let error = Config::try_parse(
            r#"
            toolchains = ["nosuch"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.golang]
            run = ["install-go"]
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("unknown toolchain \"nosuch\""),
            "{message}"
        );
        assert!(message.contains("golang"), "{message}");
    }

    #[test]
    fn invalid_toolchain_names_fail_validation() {
        let error = Config::try_parse(
            r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain."bad name"]
            run = ["true"]
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(
            error.to_string().contains("invalid toolchain name"),
            "{}",
            error
        );
    }

    #[test]
    fn includes_expand_transitively_in_order() {
        let config: Config = toml::from_str(
            r#"
            toolchains = ["fullstack"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.fullstack]
            includes = ["rust", "web"]

            [toolchain.rust]
            run = ["install-rust"]

            [toolchain.web]
            includes = ["python"]

            [toolchain.python]
            run = ["install-python"]
            "#,
        )
        .unwrap();
        let defs = config.merged_toolchains();
        assert_eq!(
            config.expanded_selection(&defs).unwrap(),
            ["fullstack", "rust", "web", "python"]
        );
    }

    #[test]
    fn mutual_includes_are_rejected_with_chain() {
        let error = Config::try_parse(
            r#"
            toolchains = ["a"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.a]
            includes = ["b"]

            [toolchain.b]
            includes = ["a"]
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cyclic toolchain includes: a -> b -> a"),
            "{}",
            error
        );
    }

    #[test]
    fn self_include_is_rejected_as_cycle() {
        let error = Config::try_parse(
            r#"
            toolchains = ["loop"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.loop]
            includes = ["loop"]
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(
            error.to_string().contains("cyclic toolchain includes"),
            "{}",
            error
        );
    }

    #[test]
    fn unknown_include_target_errors_with_context() {
        let error = Config::try_parse(
            r#"
            toolchains = ["meta"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.meta]
            includes = ["ghost"]
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("unknown toolchain \"ghost\" (included by \"meta\")"),
            "{message}"
        );
        assert!(message.contains("available:"), "{message}");
    }

    #[test]
    fn scoped_uv_name_is_validated() {
        let error = Config::try_parse(
            r#"
            toolchains = ["py"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.py]
            uv = [{ name = "" }]
            "#,
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(
            error.to_string().contains("uv tool name cannot be empty"),
            "{}",
            error
        );
    }

    #[test]
    fn toolchains_default_to_empty() {
        let config = Config::parse(
            r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]
            "#,
        );
        assert!(config.toolchains.is_empty());
    }

    #[test]
    fn default_install_ships_terminfo_database() {
        let config = Config::parse(
            r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]
            "#,
        );
        assert_eq!(config.install, ["git", "gcc", "libc6-dev", "ncurses-term"]);
    }

    #[test]
    fn toolchains_accept_arbitrary_defined_names() {
        let config = Config::parse(
            r#"
            toolchains = ["rust", "golang"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [toolchain.golang]
            run = ["install-go"]
            "#,
        );
        assert_eq!(config.toolchains, ["rust", "golang"]);
    }

    #[test]
    fn starter_config_defaults_without_toolchain() {
        let config: Config = toml::from_str(&starter_config()).unwrap();
        assert!(config.toolchains.is_empty());
        assert_eq!(
            config.harness.command(),
            ["opencode", "/workspace"].as_slice()
        );
    }

    #[test]
    fn starter_config_parses_with_commented_example() {
        let config: Config = toml::from_str(&starter_config()).unwrap();
        assert!(config.toolchains.is_empty());
    }

    #[test]
    fn with_toolchain_overrides_config_toolchains_and_retags() {
        let mut config: Config = toml::from_str(
            r#"
            toolchains = ["python"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]
            "#,
        )
        .unwrap();
        config.with_toolchain(Some("golang".into()));
        assert_eq!(config.toolchains, ["golang"]);
        assert_eq!(
            config.image_tag(),
            "localhost/pithos-opencode:latest-golang"
        );
    }

    #[test]
    fn with_toolchain_none_keeps_config_toolchains() {
        let mut config: Config = toml::from_str(
            r#"
            toolchains = ["python"]

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]
            "#,
        )
        .unwrap();
        config.with_toolchain(None);
        assert_eq!(config.toolchains, ["python"]);
        assert_eq!(config.image_tag(), "localhost/pithos-opencode:latest");
    }

    #[test]
    fn image_tag_derives_from_harness_name() {
        let opencode = Config::parse("[harness]\nname = \"opencode\"");
        assert_eq!(opencode.image_tag(), "localhost/pithos-opencode:latest");

        let claude_code = Config::parse("[harness]\nname = \"claude-code\"");
        assert_eq!(
            claude_code.image_tag(),
            "localhost/pithos-claude-code:latest"
        );
    }

    #[test]
    fn explicit_image_tag_overrides_harness_derivation() {
        let config = Config::parse(
            r#"
            image_tag = "localhost/custom:v1"

            [harness]
            name = "claude-code"
            command = ["claude"]
            "#,
        );
        assert_eq!(config.image_tag(), "localhost/custom:v1");
    }

    #[test]
    fn toolchain_retag_applies_to_derived_image_tag() {
        let mut config = Config::parse("[harness]\nname = \"claude-code\"");
        config.with_toolchain(Some("python".into()));
        assert_eq!(
            config.image_tag(),
            "localhost/pithos-claude-code:latest-python"
        );
    }

    #[test]
    fn empty_explicit_image_tag_fails_validation() {
        let config = Config::try_parse(
            r#"
            image_tag = ""

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn diff_viewer_requires_dir_placeholder() {
        assert!(
            Config::parse(
                r#"
            diff_viewer = "lazygit -p {dir}"

            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]
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
            command = ["opencode", "/workspace"]
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
            command = ["opencode", "/workspace"]
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
            command = ["opencode", "/workspace"]

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
    fn networking_defaults_apply_when_section_absent() {
        let config = Config::parse(
            r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]
            "#,
        );
        assert!(config.networking.enabled);
        assert_eq!(config.networking.payload_size, Some(65_536));
        assert_eq!(config.networking.quota, Some(2_097_152));
        assert!(config.networking.use_default_whitelist);
        assert!(config.networking.block_private);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn empty_networking_section_still_gets_defaults() {
        let config = Config::parse(
            r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [networking]
            "#,
        );
        assert!(config.networking.enabled);
        assert_eq!(config.networking.payload_size, Some(65_536));
        assert_eq!(config.networking.quota, Some(2_097_152));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn partial_networking_section_fills_missing_knobs() {
        let config = Config::parse(
            r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [networking]
            quota = 102400
            "#,
        );
        assert_eq!(config.networking.quota, Some(102400));
        assert_eq!(config.networking.payload_size, Some(65_536));
        assert!(config.networking.enabled);
    }

    #[test]
    fn networking_can_be_disabled_explicitly() {
        let config = Config::parse(
            r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [networking]
            enabled = false
            "#,
        );
        assert!(!config.networking.enabled);
    }

    #[test]
    fn networking_rejects_zero_and_negative_limits() {
        assert!(
            Config::try_parse(
                r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [networking]
            payload_size = 0
            "#,
            )
            .unwrap()
            .validate()
            .is_err()
        );
        assert!(
            Config::try_parse(
                r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

            [networking]
            quota = 0
            "#,
            )
            .unwrap()
            .validate()
            .is_err()
        );
        assert!(
            Config::try_parse(
                r#"
            [harness]
            name = "opencode"
            command = ["opencode", "/workspace"]

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
            command = ["opencode", "/workspace"]

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
            command = ["opencode", "/workspace"]

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
            command = ["opencode", "/workspace"]

            [networking]
            quota = 102400
            "#,
        );
        assert!(config.networking.use_default_whitelist);
        assert_eq!(
            DEFAULT_WHITELIST,
            [
                "opencode.ai",
                "mcp.exa.ai",
                "api.exa.ai",
                "api.parallel.ai",
                "search.parallel.ai",
                "task-mcp.parallel.ai",
                "api.tavily.com",
                "api.search.brave.com",
                "google.serper.dev",
                "api.anthropic.com",
            ]
        );
    }
}

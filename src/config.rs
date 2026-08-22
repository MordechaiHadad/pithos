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
    #[serde(default = "default_image_tag")]
    pub(crate) image_tag: String,
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
    #[serde(default = "default_exclusions")]
    pub(crate) exclusions: Vec<String>,
    #[serde(default)]
    pub(crate) diff_viewer: Option<String>,
    #[serde(default)]
    pub(crate) networking: Option<Networking>,
}

pub(crate) const DEFAULT_WHITELIST: &[&str] = &["opencode.ai", "mcp.exa.ai", "api.exa.ai"];

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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Networking {
    /// Per-connection upload cap in KiB; unset means no cap.
    #[serde(default)]
    pub(crate) payload_size: Option<u64>,
    /// Per-session egress budget in KiB; unset means no quota.
    #[serde(default)]
    pub(crate) quota: Option<u64>,
    /// Extra hosts that bypass the cap and quota; appended to DEFAULT_WHITELIST.
    #[serde(default)]
    pub(crate) whitelist: Vec<String>,
    #[serde(default = "default_use_default_whitelist")]
    pub(crate) use_default_whitelist: bool,
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

fn default_exclusions() -> Vec<String> {
    vec![]
}

fn default_install() -> Vec<String> {
    vec!["git".into(), "gcc".into(), "libc6-dev".into()]
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

impl Config {
    pub(crate) fn init(toolchain: Option<String>) -> Result<()> {
        let path = Path::new("pithos.toml");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .wrap_err("cannot create pithos.toml")?;
        let contents = starter_config(toolchain);
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

    pub(crate) fn with_toolchain(&mut self, toolchain: Option<String>) {
        if let Some(name) = toolchain {
            self.toolchains = vec![name.clone()];
            self.image_tag = format!("{}-{}", self.image_tag, name);
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
        if self.workspace.is_empty() || !self.workspace.starts_with('/') {
            bail!("workspace must be an absolute container path")
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
        if let Some(networking) = &self.networking {
            if networking.payload_size.is_none() && networking.quota.is_none() {
                bail!("[networking] requires at least payload_size or quota")
            }
            if networking.payload_size == Some(0) {
                bail!("networking.payload_size must be greater than 0")
            }
            if networking.quota == Some(0) {
                bail!("networking.quota must be greater than 0")
            }
        }
        Ok(())
    }
}

fn starter_config(toolchain: Option<String>) -> String {
    let toolchains = toolchain
        .map(|name| format!("toolchains = [\"{name}\"]\n"))
        .unwrap_or_default();
    format!(
        "base_image = \"node:22-bookworm-slim\"\nimage_tag = \"localhost/pithos-opencode:latest\"\nworkspace = \"/workspace\"\n\n{toolchains}\n# Install tools from the mise registry (supports name@version and backend:name):\n# mise = [\"neovim\", \"lua-language-server\"]\n# Define your own toolchains:\n# [toolchain.example]\n# install = [\"curl\"]\n# env = {{ PATH = \"/opt/example/bin:$PATH\" }}\n# run = [\"curl -fsSL https://example.com/install.sh | sh\"]\n# extra = [\"example --version\"]\n\n[harness]\nname = \"opencode\"\ncommand = [\"opencode\", \"/workspace\"]\n"
    )
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
            "#,
        );
        assert!(config.toolchains.is_empty());
    }

    #[test]
    fn toolchains_accept_arbitrary_defined_names() {
        let config = Config::parse(
            r#"
            toolchains = ["rust", "golang"]

            [harness]
            name = "opencode"

            [toolchain.golang]
            run = ["install-go"]
            "#,
        );
        assert_eq!(config.toolchains, ["rust", "golang"]);
    }

    #[test]
    fn starter_config_defaults_without_toolchain() {
        let config: Config = toml::from_str(&starter_config(None)).unwrap();
        assert!(config.toolchains.is_empty());
        assert_eq!(
            config.harness.command(),
            ["opencode", "/workspace"].as_slice()
        );
    }

    #[test]
    fn starter_config_includes_selected_toolchain() {
        let config: Config = toml::from_str(&starter_config(Some("python".into()))).unwrap();
        assert_eq!(config.toolchains, ["python"]);
    }

    #[test]
    fn starter_config_parses_with_commented_example() {
        let config: Config = toml::from_str(&starter_config(None)).unwrap();
        assert!(config.toolchains.is_empty());
    }

    #[test]
    fn with_toolchain_overrides_config_toolchains_and_retags() {
        let mut config: Config = toml::from_str(
            r#"
            toolchains = ["python"]

            [harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        config.with_toolchain(Some("golang".into()));
        assert_eq!(config.toolchains, ["golang"]);
        assert_eq!(config.image_tag, "localhost/pithos-opencode:latest-golang");
    }

    #[test]
    fn with_toolchain_none_keeps_config_toolchains() {
        let mut config: Config = toml::from_str(
            r#"
            toolchains = ["python"]

            [harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        config.with_toolchain(None);
        assert_eq!(config.toolchains, ["python"]);
        assert_eq!(config.image_tag, "localhost/pithos-opencode:latest");
    }

    #[test]
    fn diff_viewer_requires_dir_placeholder() {
        assert!(
            Config::parse(
                r#"
            diff_viewer = "lazygit -p {dir}"

            [harness]
            name = "opencode"
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
    fn networking_requires_at_least_one_limiter() {
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            payload_size = 8
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

            [networking]
            quota = 102400
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

            [networking]
            "#,
            )
            .validate()
            .is_err()
        );
    }

    #[test]
    fn networking_rejects_zero_and_negative_limits() {
        assert!(
            Config::parse(
                r#"
            [harness]
            name = "opencode"

            [networking]
            payload_size = 0
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

            [networking]
            quota = 0
            "#,
            )
            .validate()
            .is_err()
        );
        assert!(
            Config::try_parse(
                r#"
            [harness]
            name = "opencode"

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

            [networking]
            quota = 102400
            "#,
        );
        let networking = config.networking.unwrap();
        assert!(networking.use_default_whitelist);
        assert_eq!(
            DEFAULT_WHITELIST,
            ["opencode.ai", "mcp.exa.ai", "api.exa.ai"]
        );
    }
}

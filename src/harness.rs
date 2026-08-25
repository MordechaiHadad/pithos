use eyre::{WrapErr, bail, eyre};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agent::AGENT_HOME;
use crate::platform;

pub type Allowlist = BTreeMap<String, Value>;

pub(crate) fn tmpfs_spec(target: &str) -> String {
    format!("{target}:rw,mode=1777")
}

struct HarnessPaths {
    data: PathBuf,
    state: PathBuf,
    config: PathBuf,
    credentials: PathBuf,
}

/// Access policy for a harness content entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Bind the host file read-only into the container.
    ReadOnly,
    /// Bind an existing-or-created host file read-write; contents sync but the
    /// file can never be created, deleted, or replaced from inside the container.
    Pinned,
    /// Bind a host directory read-write; the directory is created when missing.
    /// Contents sync freely in both directions.
    PinnedDir,
}

/// A single host file or directory exposed to the container through the
/// content map.
#[derive(Debug, Clone)]
pub(crate) struct ContentEntry {
    pub(crate) host: PathBuf,
    pub(crate) target: String,
    pub(crate) access: Access,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "name", rename_all = "kebab-case")]
pub enum Harness {
    Opencode {
        #[serde(default = "default_opencode_command")]
        command: Vec<String>,
        #[serde(default)]
        allowlist: Option<Allowlist>,
        #[serde(default)]
        credentials: bool,
    },
    ClaudeCode {
        #[serde(default = "default_claude_code_command")]
        command: Vec<String>,
        #[serde(default)]
        allowlist: Option<Allowlist>,
        #[serde(default)]
        credentials: bool,
    },
}

/// Personal configuration inside ~/.claude that travels read-only into
/// claude-code sessions whenever it exists on the host.
const CLAUDE_CONFIG_PATHS: &[&str] = &[
    "CLAUDE.md",
    "keybindings.json",
    "skills",
    "agents",
    "commands",
    "rules",
    "output-styles",
    "themes",
    "workflows",
];

impl Harness {
    pub fn install(&self) -> String {
        match self {
            Self::Opencode { .. } => "RUN npm install --global opencode-ai\n".into(),
            Self::ClaudeCode { .. } => {
                "RUN npm install --global @anthropic-ai/claude-code\n".into()
            }
        }
    }

    pub fn command(&self) -> &[String] {
        match self {
            Self::Opencode { command, .. } | Self::ClaudeCode { command, .. } => command,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Opencode { .. } => "opencode",
            Self::ClaudeCode { .. } => "claude-code",
        }
    }

    pub fn mount(&self, command: &mut Command, session_id: &str) -> eyre::Result<()> {
        match self {
            Self::Opencode { .. } => self.mount_opencode(command),
            Self::ClaudeCode { .. } => {
                let runtime_base = crate::registry::runtime_dir();
                self.mount_claude_code(command, session_id, &runtime_base)
            }
        }
    }

    pub(crate) fn validate(&self) -> eyre::Result<()> {
        match self {
            Self::Opencode { .. } => Ok(()),
            Self::ClaudeCode { allowlist, .. } => {
                let Some(map) = allowlist else {
                    return Ok(());
                };
                for (key, value) in map {
                    if key != "bash" && key != "edit" {
                        bail!(
                            "harness.allowlist key \"{key}\" is not supported by the claude-code \
                             harness; supported keys are \"bash\" and \"edit\""
                        );
                    }
                    match (key.as_str(), value) {
                        ("edit", Value::String(verdict)) => {
                            ensure_verdict(verdict)?;
                        }
                        ("edit", _) => {
                            bail!("harness.allowlist.edit must be \"allow\", \"ask\", or \"deny\"")
                        }
                        ("bash", Value::Object(patterns)) => {
                            for (pattern, verdict) in patterns {
                                let Value::String(verdict) = verdict else {
                                    bail!("harness.allowlist.bash.\"{pattern}\" must be a string")
                                };
                                ensure_verdict(verdict).wrap_err_with(|| {
                                    format!("harness.allowlist.bash.\"{pattern}\"")
                                })?;
                            }
                        }
                        ("bash", _) => {
                            bail!("harness.allowlist.bash must be a table of pattern = verdict")
                        }
                        _ => unreachable!("key filter above rejects anything else"),
                    }
                }
                Ok(())
            }
        }
    }

    fn mount_opencode(&self, command: &mut Command) -> eyre::Result<()> {
        let paths = self.paths()?;

        command.args([
            "--tmpfs",
            &tmpfs_spec(&format!("{AGENT_HOME}/.local/share/opencode")),
        ]);
        command.args([
            "--tmpfs",
            &tmpfs_spec(&format!("{AGENT_HOME}/.local/state/opencode")),
        ]);
        command.args(["--tmpfs", &tmpfs_spec(&format!("{AGENT_HOME}/.cache"))]);
        command.args(["--tmpfs", &tmpfs_spec(&format!("{AGENT_HOME}/.serena"))]);

        let mut entries = vec![ContentEntry {
            host: paths.credentials.clone(),
            target: format!("{AGENT_HOME}/.local/share/opencode/auth.json"),
            access: Access::ReadOnly,
        }];
        entries.extend([
            ContentEntry {
                host: paths.data.join("opencode.db"),
                target: format!("{AGENT_HOME}/.local/share/opencode/opencode.db"),
                access: Access::Pinned,
            },
            ContentEntry {
                host: paths.data.join("opencode.db-wal"),
                target: format!("{AGENT_HOME}/.local/share/opencode/opencode.db-wal"),
                access: Access::Pinned,
            },
            ContentEntry {
                host: paths.data.join("opencode.db-shm"),
                target: format!("{AGENT_HOME}/.local/share/opencode/opencode.db-shm"),
                access: Access::Pinned,
            },
            ContentEntry {
                host: paths.state.join("kv.json"),
                target: format!("{AGENT_HOME}/.local/state/opencode/kv.json"),
                access: Access::Pinned,
            },
            ContentEntry {
                host: paths.state.join("session.json"),
                target: format!("{AGENT_HOME}/.local/state/opencode/session.json"),
                access: Access::Pinned,
            },
            ContentEntry {
                host: paths.state.join("model.json"),
                target: format!("{AGENT_HOME}/.local/state/opencode/model.json"),
                access: Access::Pinned,
            },
            ContentEntry {
                host: paths.state.join("prompt-history.jsonl"),
                target: format!("{AGENT_HOME}/.local/state/opencode/prompt-history.jsonl"),
                access: Access::Pinned,
            },
        ]);
        self.apply_content_entries(command, entries)?;

        if self.config_required() {
            mount_if_exists(
                command,
                &paths.config,
                &format!("{AGENT_HOME}/.config/opencode"),
                true,
            )?;
        }
        Ok(())
    }

    fn mount_claude_code(
        &self,
        command: &mut Command,
        session_id: &str,
        runtime_base: &Path,
    ) -> eyre::Result<()> {
        let claude_dir = home_path(".claude")?;
        let credentials_file = claude_dir.join(".credentials.json");
        if let Some(warning) =
            macos_credentials_warning(cfg!(target_os = "macos"), &credentials_file)
            && self.credentials_enabled()
        {
            eprintln!("{warning}");
        }

        for churn in ["todos", "shell-snapshots", "statsig"] {
            command.args([
                "--tmpfs",
                &tmpfs_spec(&format!("{AGENT_HOME}/.claude/{churn}")),
            ]);
        }

        let data = data_path("claude-code")?;
        let entries = vec![
            ContentEntry {
                host: credentials_file,
                target: format!("{AGENT_HOME}/.claude/.credentials.json"),
                access: Access::ReadOnly,
            },
            ContentEntry {
                host: data.join("claude.json"),
                target: format!("{AGENT_HOME}/.claude.json"),
                access: Access::Pinned,
            },
            ContentEntry {
                host: data.join("projects"),
                target: format!("{AGENT_HOME}/.claude/projects"),
                access: Access::PinnedDir,
            },
            ContentEntry {
                host: data.join("history.jsonl"),
                target: format!("{AGENT_HOME}/.claude/history.jsonl"),
                access: Access::Pinned,
            },
        ];
        self.apply_content_entries(command, entries)?;

        if let Some(settings) = self.write_claude_settings(session_id, runtime_base)? {
            mount_path(
                command,
                &settings,
                &format!("{AGENT_HOME}/.claude/settings.json"),
                true,
            )?;
        }

        for relative in CLAUDE_CONFIG_PATHS {
            let source = claude_dir.join(relative);
            mount_if_exists(
                command,
                &source,
                &format!("{AGENT_HOME}/.claude/{relative}"),
                true,
            )?;
        }
        Ok(())
    }

    fn apply_content_entries(
        &self,
        command: &mut Command,
        entries: Vec<ContentEntry>,
    ) -> eyre::Result<()> {
        for entry in entries {
            match entry.access {
                Access::ReadOnly => {
                    if !self.credentials_enabled() || !entry.host.exists() {
                        continue;
                    }
                }
                Access::Pinned => ensure_pinned_file(&entry.host)?,
                Access::PinnedDir => ensure_pinned_dir(&entry.host)?,
            }
            mount_path(
                command,
                &entry.host,
                &entry.target,
                entry.access == Access::ReadOnly,
            )?;
        }
        Ok(())
    }

    /// Materializes the effective claude settings for one session: the host's
    /// own ~/.claude/settings.json with its `permissions` key replaced by the
    /// translated pithos allowlist. Returns None when no allowlist is set.
    fn write_claude_settings(
        &self,
        session_id: &str,
        runtime_base: &Path,
    ) -> eyre::Result<Option<PathBuf>> {
        let Some(allowlist) = self.allowlist() else {
            return Ok(None);
        };
        let user_settings_path = home_path(".claude")?.join("settings.json");
        let user_settings = match fs::read_to_string(&user_settings_path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %user_settings_path.display(),
                        "ignoring unparseable user settings.json"
                    );
                    json!({})
                }
            },
            Err(_) => json!({}),
        };
        let merged = merge_claude_settings(user_settings, allowlist);
        let directory = runtime_base.join(session_id);
        fs::create_dir_all(&directory)
            .wrap_err_with(|| format!("cannot create {}", directory.display()))?;
        let path = directory.join("claude-settings.json");
        let contents =
            serde_json::to_vec_pretty(&merged).wrap_err("cannot serialize claude settings")?;
        fs::write(&path, contents).wrap_err_with(|| format!("cannot write {}", path.display()))?;
        Ok(Some(path))
    }

    fn allowlist(&self) -> Option<&Allowlist> {
        match self {
            Self::Opencode { allowlist, .. } | Self::ClaudeCode { allowlist, .. } => {
                allowlist.as_ref()
            }
        }
    }

    /// Files the harness expects to persist across sessions, keyed by the
    /// harness variant. Pinned entries are pre-created on the host so podman
    /// binds regular files instead of inventing directories.
    fn paths(&self) -> eyre::Result<HarnessPaths> {
        match self {
            Self::Opencode { .. } => Ok(HarnessPaths {
                data: data_path("opencode")?,
                state: state_path("opencode")?,
                config: home_path(".config/opencode")?,
                credentials: home_path(".local/share/opencode/auth.json")?,
            }),
            Self::ClaudeCode { .. } => Err(eyre!("claude-code has no opencode-style path set")),
        }
    }

    pub fn environment(&self) -> Vec<(String, String)> {
        match self {
            Self::Opencode { allowlist, .. } => allowlist
                .as_ref()
                .map(|allowlist| {
                    vec![(
                        "OPENCODE_CONFIG_CONTENT".into(),
                        json!({ "permission": allowlist }).to_string(),
                    )]
                })
                .unwrap_or_default(),
            Self::ClaudeCode { .. } => Vec::new(),
        }
    }

    fn credentials_enabled(&self) -> bool {
        match self {
            Self::Opencode { credentials, .. } | Self::ClaudeCode { credentials, .. } => {
                *credentials
            }
        }
    }

    fn config_required(&self) -> bool {
        match self {
            Self::Opencode { allowlist, .. } => self.credentials_enabled() || allowlist.is_some(),
            Self::ClaudeCode { .. } => false,
        }
    }
}

fn ensure_verdict(verdict: &str) -> eyre::Result<()> {
    if matches!(verdict, "allow" | "ask" | "deny") {
        Ok(())
    } else {
        bail!("unsupported verdict \"{verdict}\"; use \"allow\", \"ask\", or \"deny\"")
    }
}

/// Translates a pithos allowlist into a Claude Code `permissions` object.
/// Bash patterns become `Bash(pattern)` rules and the edit verdict covers the
/// Edit and Write tools. Claude evaluates deny, then ask, then allow.
fn merge_claude_settings(user_settings: Value, allowlist: &Allowlist) -> Value {
    let mut merged = user_settings;
    let mut allow = Vec::new();
    let mut ask = Vec::new();
    let mut deny = Vec::new();
    let mut push = |verdict: &str, rule: String| match verdict {
        "allow" => allow.push(Value::String(rule)),
        "ask" => ask.push(Value::String(rule)),
        "deny" => deny.push(Value::String(rule)),
        other => tracing::warn!(verdict = other, %rule, "unknown allowlist verdict ignored"),
    };
    if let Some(Some(edit)) = allowlist.get("edit").map(Value::as_str) {
        for tool in ["Edit", "Write"] {
            push(edit, tool.to_string());
        }
    }
    if let Some(Some(patterns)) = allowlist.get("bash").map(Value::as_object) {
        for (pattern, verdict) in patterns {
            if let Some(verdict) = verdict.as_str() {
                push(verdict, format!("Bash({pattern})"));
            }
        }
    }
    merged["permissions"] = json!({ "allow": allow, "ask": ask, "deny": deny });
    merged
}

/// macOS stores Claude Code OAuth credentials in the Keychain instead of the
/// credentials file the sandbox mounts. Warn once per session start so users
/// are not surprised by an unauthenticated harness.
fn macos_credentials_warning(is_macos: bool, credentials_file: &Path) -> Option<String> {
    if !is_macos || credentials_file.exists() {
        return None;
    }
    let file = credentials_file.display();
    Some(format!(
        "warning: Claude Code stores OAuth credentials in the macOS Keychain, which cannot be\n\
         mounted into this sandbox; the session will start unauthenticated. Fix with either:\n\
         \x20 security find-generic-password -s \"Claude Code-credentials\" -w > '{file}' && chmod 600 '{file}'\n\
         \x20 or run `claude setup-token` and put CLAUDE_CODE_OAUTH_TOKEN under [environment]"
    ))
}

fn default_opencode_command() -> Vec<String> {
    vec!["opencode".into(), "/workspace".into()]
}

fn default_claude_code_command() -> Vec<String> {
    vec!["claude".into()]
}

fn data_path(application: &str) -> eyre::Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| eyre!("cannot determine harness data directory"))?;
    Ok(data_dir.join(application))
}

fn state_path(application: &str) -> eyre::Result<PathBuf> {
    let state_dir =
        dirs::state_dir().ok_or_else(|| eyre!("cannot determine harness state directory"))?;
    Ok(state_dir.join(application))
}

fn home_path(relative: &str) -> eyre::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| eyre!("cannot determine home directory"))?;
    Ok(home.join(relative))
}

/// Create an empty file if it does not exist, without truncating existing
/// content.
fn ensure_pinned_file(path: &Path) -> eyre::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("cannot create directory {}", parent.display()))?;
    }
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .wrap_err_with(|| format!("cannot create pinned file {}", path.display()))?;
    Ok(())
}

/// Create an empty directory if it does not exist.
fn ensure_pinned_dir(path: &Path) -> eyre::Result<()> {
    fs::create_dir_all(path)
        .wrap_err_with(|| format!("cannot create pinned directory {}", path.display()))
}

fn mount_if_exists(
    command: &mut Command,
    source: &Path,
    target: &str,
    read_only: bool,
) -> eyre::Result<()> {
    if source.exists() {
        mount_path(command, source, target, read_only)?;
    }
    Ok(())
}

fn mount_path(
    command: &mut Command,
    source: &Path,
    target: &str,
    read_only: bool,
) -> eyre::Result<()> {
    command.args([
        "--volume",
        &platform::volume_spec(source, target, read_only),
    ]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn tmpfs_args(args: &[String]) -> Vec<&String> {
        args.iter()
            .filter(|arg| arg.ends_with(",mode=1777"))
            .collect()
    }

    #[test]
    fn tmpfs_mounts_are_world_writable_with_sticky_bit() {
        assert_eq!(tmpfs_spec("/tmp"), "/tmp:rw,mode=1777");
        assert_eq!(
            tmpfs_spec(&format!("{AGENT_HOME}/.cache")),
            "/home/agent/.cache:rw,mode=1777"
        );
    }

    #[test]
    fn opencode_allowlist_becomes_permission_env() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [harness.allowlist]
            edit = "deny"

            [harness.allowlist.bash]
            "*" = "ask"
            "git *" = "allow"
            "#,
        )
        .unwrap();
        let environment = config.harness.environment();
        let (key, value) = environment.first().unwrap();
        assert_eq!(key, "OPENCODE_CONFIG_CONTENT");
        let parsed: Value = serde_json::from_str(value).unwrap();
        assert_eq!(parsed["permission"]["edit"], "deny");
        assert_eq!(parsed["permission"]["bash"]["*"], "ask");
        assert_eq!(parsed["permission"]["bash"]["git *"], "allow");
    }

    #[test]
    fn harness_without_allowlist_injects_nothing() {
        let config: Config = toml::from_str("[harness]\nname = \"opencode\"").unwrap();
        assert!(config.harness.environment().is_empty());
    }

    #[test]
    fn allowlist_mounts_config_folder_without_credentials() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [harness.allowlist]
            edit = "deny"
            "#,
        )
        .unwrap();
        assert!(config.harness.config_required());
        assert!(!config.harness.credentials_enabled());
    }

    #[test]
    fn no_allowlist_no_credentials_mounts_nothing() {
        let config: Config = toml::from_str(
            r#"[harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        assert!(!config.harness.config_required());
        assert!(!config.harness.credentials_enabled());
    }

    #[test]
    fn home_paths_resolve_relative_to_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            home_path(".config/opencode").unwrap(),
            home.join(".config/opencode")
        );
    }

    #[test]
    fn credentials_enable_config_folder_and_auth() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            credentials = true
            "#,
        )
        .unwrap();
        assert!(config.harness.config_required());
        assert!(config.harness.credentials_enabled());
    }

    #[test]
    fn runtime_folders_are_tmpfs_and_content_is_file_mounted() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"
            "#,
        )
        .unwrap();
        let mut command = Command::new("true");
        config.harness.mount(&mut command, "test-session").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let share_target = format!("{AGENT_HOME}/.local/share/opencode");
        let state_target = format!("{AGENT_HOME}/.local/state/opencode");

        assert!(
            args.iter().any(|arg| arg == &tmpfs_spec(&share_target)),
            "expected {share_target} as tmpfs, got {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == &tmpfs_spec(&state_target)),
            "expected {state_target} as tmpfs, got {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.ends_with(&format!(":{share_target}:rw"))),
            "share dir must not be a whole-directory bind mount, got {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.ends_with(&format!(":{state_target}:rw"))),
            "state dir must not be a whole-directory bind mount, got {args:?}"
        );

        for name in [
            "opencode.db",
            "opencode.db-wal",
            "opencode.db-shm",
            "kv.json",
            "session.json",
            "model.json",
            "prompt-history.jsonl",
        ] {
            let expected_suffix = format!("/{name}:rw");
            assert!(
                args.iter().any(|arg| arg.ends_with(&expected_suffix)),
                "expected a pinned rw mount ending with {expected_suffix}, got {args:?}"
            );
        }
        assert!(
            tmpfs_args(&args).len() >= 4,
            "expected tmpfs churn mounts, got {args:?}"
        );
    }

    #[test]
    fn without_credentials_auth_json_is_not_mounted() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            allowlist = { edit = "deny" }
            "#,
        )
        .unwrap();
        let mut command = Command::new("true");
        config.harness.mount(&mut command, "test-session").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|arg| arg.contains("auth.json")),
            "auth.json must not be mounted without credentials, got {args:?}"
        );
    }

    #[test]
    fn credentials_mount_auth_json_read_only() {
        let config: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            credentials = true
            "#,
        )
        .unwrap();
        let auth_path = dirs::home_dir()
            .unwrap()
            .join(".local/share/opencode/auth.json");
        if !auth_path.exists() {
            fs::create_dir_all(auth_path.parent().unwrap()).unwrap();
            fs::write(&auth_path, "{}").unwrap();
        }
        let mut command = Command::new("true");
        config.harness.mount(&mut command, "test-session").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|arg| arg.ends_with("/auth.json:ro")),
            "expected a read-only auth.json mount, got {args:?}"
        );
    }

    fn claude_config(toml_body: &str) -> Config {
        toml::from_str(toml_body).unwrap()
    }

    #[test]
    fn claude_code_harness_parses_and_defaults_to_bare_cli() {
        let config = claude_config("[harness]\nname = \"claude-code\"");
        assert_eq!(config.harness.command(), ["claude"]);
        let installed = config.harness.install();
        assert_eq!(
            installed,
            "RUN npm install --global @anthropic-ai/claude-code\n"
        );
    }

    #[test]
    fn claude_code_pins_state_files_and_projects_directory() {
        let config = claude_config("[harness]\nname = \"claude-code\"\ncredentials = true");
        let mut command = Command::new("true");
        config.harness.mount(&mut command, "test-session").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        for suffix in [
            "/.claude.json:rw",
            "/.claude/projects:rw",
            "/.claude/history.jsonl:rw",
        ] {
            assert!(
                args.iter().any(|arg| arg.ends_with(suffix)),
                "expected pinned mount ending with {suffix}, got {args:?}"
            );
        }
    }

    #[test]
    fn claude_code_churn_dirs_are_tmpfs() {
        let config = claude_config("[harness]\nname = \"claude-code\"");
        let mut command = Command::new("true");
        config.harness.mount(&mut command, "test-session").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        for churn in ["todos", "shell-snapshots", "statsig"] {
            let expected = tmpfs_spec(&format!("{AGENT_HOME}/.claude/{churn}"));
            assert!(
                args.contains(&expected),
                "expected {expected} tmpfs, got {args:?}"
            );
        }
    }

    #[test]
    fn claude_code_credentials_are_read_only_when_gated() {
        let credentials_path = dirs::home_dir().unwrap().join(".claude/.credentials.json");
        if !credentials_path.exists() {
            fs::create_dir_all(credentials_path.parent().unwrap()).unwrap();
            fs::write(&credentials_path, "{}").unwrap();
        }
        let gated = claude_config("[harness]\nname = \"claude-code\"");
        let mut command = Command::new("true");
        gated.harness.mount(&mut command, "test-session").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|arg| arg.contains(".credentials.json")),
            "credentials must not mount without credentials = true, got {args:?}"
        );

        let enabled = claude_config("[harness]\nname = \"claude-code\"\ncredentials = true");
        let mut command = Command::new("true");
        enabled.harness.mount(&mut command, "test-session").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter()
                .any(|arg| arg.ends_with("/.claude/.credentials.json:ro")),
            "expected read-only credentials mount, got {args:?}"
        );
    }

    #[test]
    fn claude_code_personal_config_mounts_read_only_when_present() {
        let claude_dir = dirs::home_dir().unwrap().join(".claude");
        let skills_dir = claude_dir.join("skills");
        let created = !skills_dir.exists();
        if created {
            fs::create_dir_all(&skills_dir).unwrap();
        }
        let config = claude_config("[harness]\nname = \"claude-code\"");
        let mut command = Command::new("true");
        config.harness.mount(&mut command, "test-session").unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|arg| arg.ends_with("/.claude/skills:ro")),
            "expected ro skills mount, got {args:?}"
        );
        if created {
            fs::remove_dir_all(&skills_dir).unwrap();
            assert!({
                let mut command = Command::new("true");
                config.harness.mount(&mut command, "test-session").unwrap();
                let args: Vec<String> = command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                !args.iter().any(|arg| arg.contains("/.claude/skills"))
            });
        }
    }

    #[test]
    fn claude_code_settings_file_is_generated_per_session_with_translated_permissions() {
        let config = claude_config(
            r#"
            [harness]
            name = "claude-code"

            [harness.allowlist]
            edit = "allow"

            [harness.allowlist.bash]
            "git *" = "allow"
            "*" = "ask"
            "curl -T *" = "deny"
            "#,
        );
        let runtime_base = tempfile::Builder::new()
            .prefix("pithos-claude-test-")
            .tempdir()
            .unwrap();
        let mut command = Command::new("true");
        config
            .harness
            .mount_claude_code(&mut command, "sess-1234", runtime_base.path())
            .unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let settings_arg = args
            .iter()
            .find(|arg| arg.ends_with("/.claude/settings.json:ro"))
            .expect("generated settings must be mounted ro, got {args:?}");
        assert!(settings_arg.contains("sess-1234"));

        let host_source = settings_arg.split(':').next().unwrap();
        let merged: Value = serde_json::from_str(&fs::read_to_string(host_source).unwrap())
            .expect("generated settings must be valid JSON");
        let permissions = &merged["permissions"];
        assert_eq!(
            permissions["allow"],
            json!(["Edit", "Write", "Bash(git *)"])
        );
        assert_eq!(permissions["ask"], json!(["Bash(*)"]));
        assert_eq!(permissions["deny"], json!(["Bash(curl -T *)"]));
    }

    #[test]
    fn claude_code_without_allowlist_mounts_no_generated_settings() {
        let config = claude_config("[harness]\nname = \"claude-code\"");
        let runtime_base = tempfile::Builder::new()
            .prefix("pithos-claude-none-")
            .tempdir()
            .unwrap();
        let mut command = Command::new("true");
        config
            .harness
            .mount_claude_code(&mut command, "sess-0000", runtime_base.path())
            .unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|arg| arg.contains("settings.json")),
            "no settings should be generated without an allowlist, got {args:?}"
        );
    }

    #[test]
    fn merge_claude_settings_keeps_user_keys_and_replaces_permissions() {
        let allowlist: Allowlist = [
            ("edit".to_string(), json!("deny")),
            ("bash".to_string(), json!({ "git *": "allow", "*": "ask" })),
        ]
        .into_iter()
        .collect();
        let user = json!({ "model": "opus", "permissions": { "allow": ["Stale"] } });
        let merged = merge_claude_settings(user, &allowlist);
        assert_eq!(merged["model"], "opus");
        assert_eq!(merged["permissions"]["allow"], json!(["Bash(git *)"]));
        assert_eq!(merged["permissions"]["ask"], json!(["Bash(*)"]));
        assert_eq!(merged["permissions"]["deny"], json!(["Edit", "Write"]));
    }

    #[test]
    fn claude_code_allowlist_rejects_unsupported_keys_and_verdicts() {
        let invalid_key: Config = toml::from_str(
            r#"
            [harness]
            name = "claude-code"

            [harness.allowlist]
            webfetch = "allow"
            "#,
        )
        .unwrap();
        let error = invalid_key.harness.validate().unwrap_err().to_string();
        assert!(error.contains("webfetch"), "{error}");

        let invalid_verdict: Config = toml::from_str(
            r#"
            [harness]
            name = "claude-code"

            [harness.allowlist]
            edit = "sometimes"
            "#,
        )
        .unwrap();
        let error = invalid_verdict.harness.validate().unwrap_err().to_string();
        assert!(error.contains("sometimes"), "{error}");

        let valid: Config = toml::from_str(
            r#"
            [harness]
            name = "claude-code"

            [harness.allowlist]
            edit = "allow"

            [harness.allowlist.bash]
            "git *" = "allow"
            "#,
        )
        .unwrap();
        assert!(valid.harness.validate().is_ok());

        let passthrough: Config = toml::from_str(
            r#"
            [harness]
            name = "opencode"

            [harness.allowlist]
            webfetch = "allow"
            "#,
        )
        .unwrap();
        assert!(passthrough.harness.validate().is_ok());
    }

    #[test]
    fn macos_warning_appears_only_for_missing_credentials_on_macos() {
        let file = Path::new("/nonexistent/.credentials.json");
        let message = macos_credentials_warning(true, file).unwrap();
        assert!(message.contains("Keychain"));
        assert!(message.contains("security find-generic-password"));
        assert!(macos_credentials_warning(false, file).is_none());

        let existing = std::env::temp_dir().join("pithos-warning-fixture");
        fs::write(&existing, "{}").unwrap();
        assert!(macos_credentials_warning(true, &existing).is_none());
        let _ = fs::remove_file(&existing);
    }

    #[test]
    fn claude_code_environment_is_empty() {
        let config = claude_config(
            r#"
            [harness]
            name = "claude-code"

            [harness.allowlist]
            edit = "allow"
            "#,
        );
        assert!(config.harness.environment().is_empty());
    }
}

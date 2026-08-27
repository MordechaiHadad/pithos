use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{WrapErr, eyre};

use crate::agent::AGENT_HOME;
use crate::platform;

use super::Allowlist;
use super::def::{HarnessDef, MountDef};
use super::types::{Access, HostBase, MountType};

pub fn tmpfs_spec(target: &str) -> String {
    format!("{target}:rw,mode=1777")
}

pub fn resolve_host_path(
    def: &MountDef,
    runtime_base: &Path,
    session_id: &str,
) -> eyre::Result<PathBuf> {
    if def.host.is_empty() {
        return Ok(PathBuf::new());
    }
    match &def.host_base {
        HostBase::Home => {
            let home = dirs::home_dir().ok_or_else(|| eyre!("cannot determine home directory"))?;
            Ok(home.join(&def.host))
        }
        HostBase::Data(app) => {
            let base = dirs::data_dir().ok_or_else(|| eyre!("cannot determine data directory"))?;
            Ok(base.join(app).join(&def.host))
        }
        HostBase::State(app) => {
            let base =
                dirs::state_dir().ok_or_else(|| eyre!("cannot determine state directory"))?;
            Ok(base.join(app).join(&def.host))
        }
        HostBase::Runtime => Ok(runtime_base.join(session_id).join(&def.host)),
    }
}

pub fn apply_mounts(
    def: &HarnessDef,
    command: &mut Command,
    session_id: &str,
    runtime_base: &Path,
    allowlist: Option<&Allowlist>,
    credentials_enabled: bool,
) -> eyre::Result<()> {
    if def.name == "claude-code" {
        let claude_dir = home_path(".claude")?;
        let credentials_file = claude_dir.join(".credentials.json");
        if let Some(warning) =
            macos_credentials_warning(cfg!(target_os = "macos"), &credentials_file)
            && credentials_enabled
        {
            eprintln!("{warning}");
        }
    }

    for mount in &def.mounts {
        match mount.mount_type {
            MountType::Ephemeral => {
                if mount.access != Access::Tmpfs {
                    eyre::bail!(
                        "mount {} type ephemeral must have access tmpfs, got {:?}",
                        mount.target,
                        mount.access
                    );
                }
                command.args(["--tmpfs", &tmpfs_spec(&mount.target)]);
            }
            MountType::Generated => {
                if def.name == "claude-code"
                    && mount.target == format!("{AGENT_HOME}/.claude/settings.json")
                {
                    if let Some(settings) =
                        write_claude_settings(allowlist, session_id, runtime_base)?
                    {
                        mount_path(command, &settings, &mount.target, true)?;
                    }
                    continue;
                }
                tracing::warn!(target = %mount.target, "generated mount not handled for harness {}", def.name);
            }
            _ => {
                let host = resolve_host_path(mount, runtime_base, session_id)?;
                if mount.mount_type == MountType::Credentials && !credentials_enabled {
                    continue;
                }
                if mount.mount_type == MountType::Config && !host.exists() {
                    continue;
                }
                if mount.mount_type == MountType::Config
                    && def.name == "opencode"
                    && !(credentials_enabled || allowlist.is_some())
                {
                    continue;
                }
                if mount.mount_type == MountType::Credentials
                    && mount.access == Access::Ro
                    && !host.exists()
                {
                    continue;
                }
                match mount.access {
                    Access::Ro => {
                        mount_path(command, &host, &mount.target, true)?;
                    }
                    Access::Pinned => {
                        ensure_pinned_file(&host)?;
                        mount_path(command, &host, &mount.target, false)?;
                    }
                    Access::PinnedDir => {
                        ensure_pinned_dir(&host)?;
                        mount_path(command, &host, &mount.target, false)?;
                    }
                    Access::Tmpfs => {
                        command.args(["--tmpfs", &tmpfs_spec(&mount.target)]);
                    }
                }
            }
        }
    }

    Ok(())
}

fn home_path(relative: &str) -> eyre::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| eyre!("cannot determine home directory"))?;
    Ok(home.join(relative))
}

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

fn ensure_pinned_dir(path: &Path) -> eyre::Result<()> {
    fs::create_dir_all(path)
        .wrap_err_with(|| format!("cannot create pinned directory {}", path.display()))
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

fn write_claude_settings(
    allowlist: Option<&Allowlist>,
    session_id: &str,
    runtime_base: &Path,
) -> eyre::Result<Option<PathBuf>> {
    let Some(allowlist) = allowlist else {
        return Ok(None);
    };
    let user_settings_path = home_path(".claude")?.join("settings.json");
    let user_settings = match fs::read_to_string(&user_settings_path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, path = %user_settings_path.display(), "ignoring unparseable user settings.json");
                serde_json::json!({})
            }
        },
        Err(_) => serde_json::json!({}),
    };
    let merged = super::translate::claude_settings_translation(allowlist, user_settings);
    let directory = runtime_base.join(session_id);
    fs::create_dir_all(&directory)
        .wrap_err_with(|| format!("cannot create {}", directory.display()))?;
    let path = directory.join("claude-settings.json");
    let contents =
        serde_json::to_vec_pretty(&merged).wrap_err("cannot serialize claude settings")?;
    fs::write(&path, contents).wrap_err_with(|| format!("cannot write {}", path.display()))?;
    Ok(Some(path))
}

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

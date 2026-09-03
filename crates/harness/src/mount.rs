use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{WrapErr, eyre};

use crate::def::{CredentialDef, HarnessDef, MountDef};
use crate::platform;
use crate::types::{Access, HostBase, MountType, OnMissing, Platform};

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
    let candidates = host_candidates(def, runtime_base, session_id)?;
    Ok(pick_existing(candidates))
}

fn host_candidates(
    def: &MountDef,
    runtime_base: &Path,
    session_id: &str,
) -> eyre::Result<Vec<PathBuf>> {
    match &def.host_base {
        HostBase::Home => {
            let home = dirs::home_dir().ok_or_else(|| eyre!("cannot determine home directory"))?;
            Ok(vec![home.join(&def.host)])
        }
        HostBase::Data(app) => {
            let mut candidates = Vec::new();
            if let Some(base) = dirs::data_dir() {
                candidates.push(base.join(app).join(&def.host));
            }
            if let Some(fallback) = windows_data_fallback(app, &def.host)
                && !candidates.contains(&fallback)
            {
                candidates.push(fallback);
            }
            if candidates.is_empty() {
                eyre::bail!("cannot determine data directory");
            }
            Ok(candidates)
        }
        HostBase::State(app) => {
            let mut candidates = Vec::new();
            if let Some(base) = dirs::state_dir() {
                candidates.push(base.join(app).join(&def.host));
            }
            if let Some(fallback) = windows_state_fallback(app, &def.host)
                && !candidates.contains(&fallback)
            {
                candidates.push(fallback);
            }
            if candidates.is_empty() {
                eyre::bail!("cannot determine state directory");
            }
            Ok(candidates)
        }
        HostBase::Cache(app) => {
            let mut candidates = Vec::new();
            if let Some(base) = dirs::cache_dir() {
                candidates.push(base.join(app).join(&def.host));
            }
            if let Some(fallback) = windows_cache_fallback(app, &def.host)
                && !candidates.contains(&fallback)
            {
                candidates.push(fallback);
            }
            if candidates.is_empty() {
                eyre::bail!("cannot determine cache directory");
            }
            Ok(candidates)
        }
        HostBase::Runtime => Ok(vec![runtime_base.join(session_id).join(&def.host)]),
    }
}

fn pick_existing(candidates: Vec<PathBuf>) -> PathBuf {
    for candidate in &candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }
    candidates.into_iter().next().unwrap_or_default()
}

fn windows_data_fallback(app: &str, host: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".local").join("share").join(app).join(host))
}

fn windows_state_fallback(app: &str, host: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".local").join("state").join(app).join(host))
}

fn windows_cache_fallback(app: &str, host: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".cache").join(app).join(host))
}

pub fn apply_mounts(
    def: &HarnessDef,
    command: &mut Command,
    session_id: &str,
    runtime_base: &Path,
    allowlist_override: Option<&Path>,
    credentials_enabled: bool,
) -> eyre::Result<()> {
    apply_credentials(def, command, session_id, runtime_base, credentials_enabled)?;

    if let Some(override_file) = allowlist_override {
        let Some(path) = write_allowlist_sink(def, override_file, session_id, runtime_base)? else {
            eyre::bail!(
                "harness \"{}\" does not declare an [allowlist] sink; remove harness.sandbox_config",
                def.name
            );
        };
        mount_path(command, &path, &def.allowlist.target, true)?;
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
                if def.allowlist.has_sink() && mount.target == def.allowlist.target {
                    continue;
                }
                eyre::bail!(
                    "harness \"{}\" declares a generated mount at {} without a matching [allowlist] target; \
                     point the sink at the mount target or remove the mount",
                    def.name,
                    mount.target
                );
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
                    && allowlist_override.is_some()
                    && is_ancestor_of(&mount.target, &def.allowlist.target)
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

fn is_ancestor_of(parent: &str, child: &str) -> bool {
    if parent.is_empty() || child.is_empty() {
        return false;
    }
    let parent = parent.trim_end_matches('/');
    child.len() > parent.len()
        && child.starts_with(parent)
        && child.as_bytes()[parent.len()] == b'/'
}

fn write_allowlist_sink(
    def: &HarnessDef,
    override_file: &Path,
    session_id: &str,
    runtime_base: &Path,
) -> eyre::Result<Option<PathBuf>> {
    let sink = &def.allowlist;
    if !sink.has_sink() {
        return Ok(None);
    }
    let contents = fs::read(override_file)
        .wrap_err_with(|| format!("cannot read {}", override_file.display()))?;
    let directory = runtime_base.join(session_id);
    fs::create_dir_all(&directory)
        .wrap_err_with(|| format!("cannot create {}", directory.display()))?;
    let path = directory.join(sanitize_target(&sink.target));
    fs::write(&path, contents).wrap_err_with(|| format!("cannot write {}", path.display()))?;
    Ok(Some(path))
}

#[allow(dead_code)]
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

fn current_platform() -> Platform {
    match std::env::consts::OS {
        "macos" => Platform::Macos,
        "windows" => Platform::Windows,
        _ => Platform::Linux,
    }
}

fn platform_matches(platforms: &Option<Vec<Platform>>) -> bool {
    if let Some(list) = platforms {
        list.contains(&current_platform())
    } else {
        true
    }
}

fn resolve_credential_file_path(
    def: &CredentialDef,
    runtime_base: &Path,
    session_id: &str,
) -> eyre::Result<PathBuf> {
    let host = def.source.strip_prefix("file:").unwrap_or(&def.source);
    let trimmed = host.trim_start_matches("./");
    let candidates = credential_candidates(&def.host_base, trimmed, runtime_base, session_id)?;
    Ok(pick_existing(candidates))
}

fn credential_candidates(
    host_base: &HostBase,
    trimmed: &str,
    runtime_base: &Path,
    session_id: &str,
) -> eyre::Result<Vec<PathBuf>> {
    match host_base {
        HostBase::Home => {
            let home = dirs::home_dir().ok_or_else(|| eyre!("cannot determine home directory"))?;
            Ok(vec![home.join(trimmed)])
        }
        HostBase::Data(app) => {
            let mut candidates = Vec::new();
            if let Some(base) = dirs::data_dir() {
                candidates.push(base.join(app).join(trimmed));
            }
            if let Some(fallback) = windows_data_fallback(app, trimmed)
                && !candidates.contains(&fallback)
            {
                candidates.push(fallback);
            }
            if candidates.is_empty() {
                eyre::bail!("cannot determine data directory");
            }
            Ok(candidates)
        }
        HostBase::State(app) => {
            let mut candidates = Vec::new();
            if let Some(base) = dirs::state_dir() {
                candidates.push(base.join(app).join(trimmed));
            }
            if let Some(fallback) = windows_state_fallback(app, trimmed)
                && !candidates.contains(&fallback)
            {
                candidates.push(fallback);
            }
            if candidates.is_empty() {
                eyre::bail!("cannot determine state directory");
            }
            Ok(candidates)
        }
        HostBase::Cache(app) => {
            let mut candidates = Vec::new();
            if let Some(base) = dirs::cache_dir() {
                candidates.push(base.join(app).join(trimmed));
            }
            if let Some(fallback) = windows_cache_fallback(app, trimmed)
                && !candidates.contains(&fallback)
            {
                candidates.push(fallback);
            }
            if candidates.is_empty() {
                eyre::bail!("cannot determine cache directory");
            }
            Ok(candidates)
        }
        HostBase::Runtime => Ok(vec![runtime_base.join(session_id).join(trimmed)]),
    }
}

fn keychain_export(service: &str) -> eyre::Result<Vec<u8>> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .wrap_err("cannot execute security")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!("security find-generic-password failed: {}", stderr.trim());
    }
    let mut data = output.stdout;
    while data.ends_with(b"\n") || data.ends_with(b"\r") {
        data.pop();
    }
    if data.is_empty() {
        eyre::bail!("keychain service \"{service}\" returned empty data");
    }
    Ok(data)
}

fn sanitize_target(target: &str) -> String {
    target
        .replace(['/', '\\'], "_")
        .trim_start_matches('_')
        .to_string()
}

fn apply_credentials(
    def: &HarnessDef,
    command: &mut Command,
    session_id: &str,
    runtime_base: &Path,
    credentials_enabled: bool,
) -> eyre::Result<()> {
    if !credentials_enabled {
        return Ok(());
    }
    if def.credentials.is_empty() {
        return Ok(());
    }
    let mut mounted_targets = std::collections::HashSet::new();
    for cred in &def.credentials {
        if !platform_matches(&cred.platforms) {
            continue;
        }
        if cred.target.is_empty() || !cred.target.starts_with('/') {
            eyre::bail!(
                "credential target must be absolute path, got \"{}\"",
                cred.target
            );
        }
        if mounted_targets.contains(&cred.target) {
            continue;
        }
        if let Some(service) = cred.source.strip_prefix("keychain:") {
            let service = service.trim();
            if service.is_empty() {
                eyre::bail!("keychain source requires service name");
            }
            match keychain_export(service) {
                Ok(data) => {
                    let dir = runtime_base.join(session_id).join("credentials");
                    fs::create_dir_all(&dir)
                        .wrap_err_with(|| format!("cannot create {}", dir.display()))?;
                    let path = dir.join(sanitize_target(&cred.target));
                    fs::write(&path, &data)
                        .wrap_err_with(|| format!("cannot write {}", path.display()))?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let perm = fs::Permissions::from_mode(0o600);
                        let _ = fs::set_permissions(&path, perm);
                    }
                    if let Err(e) = serde_json::from_slice::<serde_json::Value>(&data) {
                        tracing::warn!(service, %e, "keychain output is not valid JSON");
                    }
                    mount_path(command, &path, &cred.target, true)?;
                    mounted_targets.insert(cred.target.clone());
                }
                Err(error) => match cred.on_missing {
                    OnMissing::Ignore => continue,
                    OnMissing::Warn => {
                        tracing::warn!(
                            service,
                            %error,
                            target = %cred.target,
                            "keychain service not available; skipping credential"
                        );
                        continue;
                    }
                    OnMissing::Error => {
                        eyre::bail!("keychain service \"{service}\" failed: {error}");
                    }
                },
            }
        } else if let Some(path) = cred.source.strip_prefix("file:") {
            let _ = path;
            let host = resolve_credential_file_path(cred, runtime_base, session_id)?;
            if !host.exists() {
                match cred.on_missing {
                    OnMissing::Ignore => continue,
                    OnMissing::Warn => {
                        tracing::warn!(
                            path = %host.display(),
                            target = %cred.target,
                            "credential file not found; skipping"
                        );
                        continue;
                    }
                    OnMissing::Error => {
                        eyre::bail!("credential file {} not found", host.display());
                    }
                }
            }
            mount_path(command, &host, &cred.target, true)?;
            mounted_targets.insert(cred.target.clone());
        } else {
            eyre::bail!(
                "credential source must start with \"keychain:\" or \"file:\", got \"{}\"",
                cred.source
            );
        }
    }
    Ok(())
}

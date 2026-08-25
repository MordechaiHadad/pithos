//! Registration of the embedded egress OCI hook.
//!
//! Podman only scans its built-in hook directories when no explicit value is
//! given, and those defaults never include user-writable locations, so the
//! hook is installed into a pithos-owned directory and passed explicitly via
//! `--hooks-dir` on every native run. Inside a podman machine the hook goes
//! to `/usr/share/containers/oci/hooks.d`, which podman scans by default.

use eyre::{Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

const EGRESS_HOOK_SCRIPT: &str = include_str!("../../host/hooks/pithos-egress-cap.sh");

/// Directories podman scans for OCI hooks during native sessions.
///
/// The pithos-owned directory comes last because later `--hooks-dir` entries
/// take precedence.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn oci_hooks_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/containers/oci/hooks.d"),
        PathBuf::from("/etc/containers/oci/hooks.d"),
    ];
    if let Some(dir) = pithos_hooks_dir() {
        dirs.push(dir);
    }
    dirs
}

/// Directory holding the pithos egress hook registration and script.
#[cfg(all(unix, not(target_os = "macos")))]
fn pithos_hooks_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("pithos").join("hooks.d"))
}

/// Appends the global `--hooks-dir` arguments for a podman invocation.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn append_oci_hooks_args(args: &mut Vec<String>) {
    for dir in oci_hooks_dirs() {
        args.push("--hooks-dir".to_string());
        args.push(dir.display().to_string());
    }
}

/// No-op: remote podman clients cannot pass `--hooks-dir`; inside a podman
/// machine the hook is installed into `/usr/share/containers/oci/hooks.d`,
/// which podman scans by default.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn append_oci_hooks_args(_args: &mut Vec<String>) {}

/// Verifies the host (or podman machine) is ready for pithos networking:
/// the OCI hook is registered, its script is executable, and nft is present.
pub(crate) fn verify_networking_support() -> Result<()> {
    install_networking_hook()?;
    verify_networking_support_impl()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_networking_hook() -> Result<()> {
    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| eyre::eyre!("cannot determine local data directory"))?;
    let hooks_dir =
        pithos_hooks_dir().ok_or_else(|| eyre::eyre!("cannot determine local data directory"))?;
    fs::create_dir_all(&hooks_dir)?;
    let script_path = hooks_dir.join("pithos-egress-cap.sh");
    fs::write(&script_path, EGRESS_HOOK_SCRIPT)?;
    set_executable(&script_path)?;
    fs::write(
        hooks_dir.join("pithos-egress-cap.json"),
        hook_config(&script_path),
    )?;
    remove_legacy_hook_registration(&data_dir);
    Ok(())
}

/// Removes hook files written by older pithos versions to locations podman
/// does not scan. Best effort: leftovers only cause confusion, not harm.
#[cfg(all(unix, not(target_os = "macos")))]
fn remove_legacy_hook_registration(data_dir: &Path) {
    if let Some(config_dir) = dirs::config_dir() {
        let _ = fs::remove_file(config_dir.join("containers/oci/hooks.d/pithos-egress-cap.json"));
    }
    let _ = fs::remove_file(data_dir.join("pithos/pithos-egress-cap.sh"));
}

#[cfg(all(unix, not(target_os = "macos")))]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
fn install_networking_hook() -> Result<()> {
    use eyre::WrapErr;
    let script = shell_literal(EGRESS_HOOK_SCRIPT);
    let install = format!(
        "set -eu\nmkdir -p /usr/local/share/pithos/hooks /usr/share/containers/oci/hooks.d \"$HOME/.config/containers/oci/hooks.d\"\nprintf '%s' {script} > /usr/local/share/pithos/hooks/pithos-egress-cap.sh\nchmod 755 /usr/local/share/pithos/hooks/pithos-egress-cap.sh\nprintf '%s\\n' '{{\n  \\\"version\\\": \\\"1.0.0\\\",\n  \\\"hook\\\": {{ \\\"path\\\": \\\"/usr/local/share/pithos/hooks/pithos-egress-cap.sh\\\", \\\"args\\\": [\\\"pithos-egress-cap.sh\\\"] }},\n  \\\"when\\\": {{ \\\"annotations\\\": {{ \\\"pithos.networking\\\": \\\"1\\\" }} }},\n  \\\"stages\\\": [\\\"createRuntime\\\"]\n}}' > /usr/share/containers/oci/hooks.d/pithos-egress-cap.json\nrm -f \"$HOME/.config/containers/oci/hooks.d/pithos-egress-cap.json\"\n",
    );
    crate::platform::machine_ssh(&install)
        .wrap_err("could not install the embedded networking hook in the podman machine")?;
    Ok(())
}

fn hook_config(script_path: &Path) -> String {
    format!(
        "{{\n  \"version\": \"1.0.0\",\n  \"hook\": {{\n    \"path\": \"{}\",\n    \"args\": [\"pithos-egress-cap.sh\"]\n  }},\n  \"when\": {{\n    \"annotations\": {{\n      \"pithos.networking\": \"1\"\n    }}\n  }},\n  \"stages\": [\"createRuntime\"]\n}}\n",
        script_path.display()
    )
}

#[cfg(any(windows, target_os = "macos"))]
fn shell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn verify_networking_support_impl() -> Result<()> {
    let hooks_dir = pithos_hooks_dir().ok_or_else(|| {
        eyre::eyre!("cannot determine local data directory for the pithos OCI hook")
    })?;
    let search = vec![hooks_dir];
    match registered_hook(&search) {
        Some((json_path, hook_path)) => {
            if !crate::platform::is_executable(&hook_path) {
                bail!(
                    "OCI hook script {} (referenced by {}) is missing or not executable; \
                     fix the path or chmod +x the script",
                    hook_path.display(),
                    json_path.display()
                )
            }
            let script = fs::read_to_string(&hook_path).map_err(|error| {
                eyre::eyre!(
                    "cannot read OCI hook script {}: {error}",
                    hook_path.display()
                )
            })?;
            if !script.contains("pithos.networking-rules") {
                bail!(
                    "installed OCI hook {} (referenced by {}) is outdated: it does not read \
                     the pithos.networking-rules annotation. Reinstall \
                     host/hooks/pithos-egress-cap.sh",
                    hook_path.display(),
                    json_path.display()
                )
            }
        }
        None => bail!(
            "no OCI hook registered for pithos networking after automatic installation; \
             expected one in: {}",
            search
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
    if !nft_available() {
        bail!(
            "nftables not found (expected /usr/sbin/nft or nft on PATH); install with `sudo apt install nftables`"
        )
    }
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
fn verify_networking_support_impl() -> Result<()> {
    use eyre::WrapErr;

    let probe = r#"set -eu
json=/usr/share/containers/oci/hooks.d/pithos-egress-cap.json
[ -e "$json" ] || { echo "no pithos hook registered in the podman machine at $json" >&2; exit 1; }
hook=/usr/local/share/pithos/hooks/pithos-egress-cap.sh
[ -x "$hook" ] || { echo "pithos hook script not executable in the podman machine: $hook" >&2; exit 1; }
grep -q 'pithos.networking-rules' "$hook" || { echo "pithos hook script is outdated (does not read pithos.networking-rules): $hook" >&2; exit 1; }
command -v nft >/dev/null 2>&1 || [ -x /usr/sbin/nft ] || { echo "nft not found in the machine" >&2; exit 1; }
echo "$hook"
"#;
    let output = crate::platform::machine_ssh(probe)?;
    if !output.status.success() {
        bail!(
            "pithos networking is not set up inside the podman machine: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let hook_path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    eprintln!("pithos networking hook: {hook_path}");
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn registered_hook(search: &[PathBuf]) -> Option<(PathBuf, PathBuf)> {
    for dir in search {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let json_path = entry.path();
            if json_path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let content = match fs::read_to_string(&json_path) {
                Ok(content) => content,
                Err(_) => continue,
            };
            let value: serde_json::Value = match serde_json::from_str(&content) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let matches = value
                .get("when")
                .and_then(|when| when.get("annotations"))
                .and_then(|annotations| annotations.get("pithos.networking"))
                .and_then(|annotation| annotation.as_str())
                == Some("1");
            if !matches {
                continue;
            }
            let hook_path = value
                .get("hook")
                .and_then(|hook| hook.get("path"))
                .and_then(|path| path.as_str())
                .map(PathBuf::from);
            if let Some(hook_path) = hook_path {
                return Some((json_path, hook_path));
            }
        }
    }
    None
}

#[cfg(all(unix, not(target_os = "macos")))]
fn nft_available() -> bool {
    let mut candidates = vec![PathBuf::from("/usr/sbin/nft")];
    if let Some(path_env) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path_env).map(|dir| dir.join("nft")));
    }
    candidates.iter().any(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_hook_finds_matching_annotation() {
        let dir = TempDir::create("pithos-hook-test").unwrap();
        fs::write(
            dir.path().join("pithos-egress-cap.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/pithos-egress-cap.sh", "args": [] },
              "when": { "annotations": { "pithos.networking": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("other.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/other.sh", "args": [] },
              "when": { "annotations": { "some.other.annotation": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();

        let (json_path, hook_path) = registered_hook(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(
            json_path.file_name().unwrap().to_str().unwrap(),
            "pithos-egress-cap.json"
        );
        assert_eq!(
            hook_path,
            PathBuf::from("/usr/local/bin/pithos-egress-cap.sh")
        );
    }

    #[test]
    fn registered_hook_returns_none_without_match() {
        let dir = TempDir::create("pithos-hook-test-none").unwrap();
        fs::write(
            dir.path().join("other.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/other.sh", "args": [] },
              "when": { "annotations": { "some.other.annotation": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();
        assert!(registered_hook(&[dir.path().to_path_buf()]).is_none());
    }

    #[test]
    fn generated_hook_config_matches_embedded_script_path() {
        let config = hook_config(Path::new("/tmp/pithos-egress-cap.sh"));
        let value: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(
            value["hook"]["path"],
            serde_json::Value::String("/tmp/pithos-egress-cap.sh".into())
        );
        assert!(EGRESS_HOOK_SCRIPT.contains("pithos.networking-rules"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn pithos_hook_dir_comes_after_the_system_defaults() {
        let dirs = oci_hooks_dirs();
        assert_eq!(
            dirs[..2],
            [
                PathBuf::from("/usr/share/containers/oci/hooks.d"),
                PathBuf::from("/etc/containers/oci/hooks.d"),
            ]
        );
        assert_eq!(dirs.len(), 3);
        assert!(dirs[2].ends_with("pithos/hooks.d"));
    }

    use crate::sandbox::TempDir;
}

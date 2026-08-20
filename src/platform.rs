//! Cross-platform helpers.
//!
//! Every OS-specific call in the crate is routed through this module so the
//! rest of the code can be written without `#[cfg]` at the call sites. Each
//! function provides a functional implementation for both Unix and Windows.

use eyre::{Result, bail};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Creates a symlink at `link` pointing to `target`.
///
/// On Windows, creating a symlink requires Developer Mode or elevated
/// privileges. When the OS denies the operation, the target is copied instead
/// so the sandbox still receives the file content.
pub(crate) fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    symlink_impl(target, link)
}

#[cfg(unix)]
fn symlink_impl(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_impl(target: &Path, link: &Path) -> io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    let is_dir = fs::symlink_metadata(target)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);
    let result = if is_dir {
        symlink_dir(target, link)
    } else {
        symlink_file(target, link)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if privilege_denied(&error) && target.exists() => {
            copy_symlink_target(target, link, is_dir)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn privilege_denied(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(1314) | Some(50) | Some(1) // privilege held, not supported, invalid function
    )
}

#[cfg(windows)]
fn copy_symlink_target(target: &Path, link: &Path, is_dir: bool) -> io::Result<()> {
    if is_dir {
        copy_dir_recursive(target, link)
    } else {
        fs::copy(target, link).map(|_| ())
    }
}

#[cfg(windows)]
fn copy_dir_recursive(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if file_type.is_symlink() {
            let target = fs::read_link(&source_path)?;
            symlink_impl(&target, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

/// Numeric id of the current user.
///
/// On Unix this is the real uid so `--userns=keep-id` maps the container user
/// to the host user. Windows has no uid; the podman machine's rootless user is
/// uid 1000, which is the value that makes the mapping work there.
pub(crate) fn current_uid() -> u32 {
    current_id("-u").unwrap_or(1000)
}

/// Numeric id of the current user's primary group, see [`current_uid`].
pub(crate) fn current_gid() -> u32 {
    current_id("-g").unwrap_or(1000)
}

#[cfg(unix)]
fn current_id(flag: &str) -> Option<u32> {
    std::process::Command::new("id")
        .arg(flag)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
}

#[cfg(windows)]
fn current_id(_flag: &str) -> Option<u32> {
    Some(1000)
}

/// Path of the platform null device, usable as a "nothing" argument for
/// commands such as `git diff --no-index`.
pub(crate) fn null_device() -> PathBuf {
    null_device_impl()
}

#[cfg(unix)]
fn null_device_impl() -> PathBuf {
    PathBuf::from("/dev/null")
}

#[cfg(windows)]
fn null_device_impl() -> PathBuf {
    PathBuf::from("NUL")
}

/// Runs a shell command, returning its exit status.
///
/// `sh -c` on Unix, `%COMSPEC% /C` on Windows.
pub(crate) fn run_shell(command: &str) -> io::Result<std::process::ExitStatus> {
    run_shell_impl(command)
}

#[cfg(unix)]
fn run_shell_impl(command: &str) -> io::Result<std::process::ExitStatus> {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .status()
}

#[cfg(windows)]
fn run_shell_impl(command: &str) -> io::Result<std::process::ExitStatus> {
    let shell = std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd"));
    std::process::Command::new(shell)
        .arg("/C")
        .arg(command)
        .status()
}

#[cfg(unix)]
pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Verifies the host (or podman machine) is ready for pithos networking:
/// the OCI hook is registered, its script is executable, and nft is present.
pub(crate) fn verify_networking_support() -> Result<()> {
    verify_networking_support_impl()
}

#[cfg(unix)]
fn verify_networking_support_impl() -> Result<()> {
    let mut search = vec![
        PathBuf::from("/usr/share/containers/oci/hooks.d"),
        PathBuf::from("/etc/containers/oci/hooks.d"),
    ];
    if let Some(config_dir) = dirs::config_dir() {
        search.push(config_dir.join("containers/oci/hooks.d"));
    }
    if let Some(home) = dirs::home_dir() {
        search.push(home.join(".config/containers/oci/hooks.d"));
    }
    match registered_hook(&search) {
        Some((json_path, hook_path)) => {
            if !is_executable(&hook_path) {
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
            "no OCI hook registered for pithos networking. Install \
             host/oci-hooks.d/pithos-egress-cap.json and its hook script into one of: {}",
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

#[cfg(windows)]
fn verify_networking_support_impl() -> Result<()> {
    use eyre::WrapErr;
    use std::io::Write;

    fn machine_ssh(script: &str) -> Result<std::process::Output> {
        let mut child = std::process::Command::new("podman")
            .args(["machine", "ssh", "sh", "-s"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .wrap_err("could not run `podman machine ssh`; start the podman machine first")?;
        child
            .stdin
            .take()
            .expect("stdin is configured")
            .write_all(script.as_bytes())
            .wrap_err("could not send the probe script to the podman machine")?;
        child
            .wait_with_output()
            .wrap_err("podman machine ssh failed")
    }

    let probe = r#"set -eu
dirs="/usr/share/containers/oci/hooks.d /etc/containers/oci/hooks.d $HOME/.config/containers/oci/hooks.d"
json=""
for d in $dirs; do
  [ -d "$d" ] || continue
  for f in "$d"/*.json; do
    [ -e "$f" ] || continue
    if grep -q pithos.networking "$f"; then json="$f"; fi
  done
done
[ -n "$json" ] || { echo "no hook json referencing pithos.networking in: $dirs" >&2; exit 1; }
hook=$(sed -n 's/.*"path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$json" | head -n1)
[ -n "$hook" ] || { echo "no hook path in $json" >&2; exit 1; }
[ -x "$hook" ] || { echo "hook script not executable: $hook" >&2; exit 1; }
grep -q 'pithos.networking-rules' "$hook" || { echo "hook script is outdated (does not read pithos.networking-rules): $hook" >&2; exit 1; }
command -v nft >/dev/null 2>&1 || [ -x /usr/sbin/nft ] || { echo "nft not found in the machine" >&2; exit 1; }
echo "$hook"
"#;
    let output = machine_ssh(probe)?;
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

#[cfg(unix)]
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

#[cfg(unix)]
fn nft_available() -> bool {
    let mut candidates = vec![PathBuf::from("/usr/sbin/nft")];
    if let Some(path_env) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path_env).map(|dir| dir.join("nft")));
    }
    candidates.iter().any(|candidate| candidate.is_file())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use crate::sandbox::TempDir;

    #[test]
    fn symlink_round_trip() {
        let dir = TempDir::create("pithos-platform-symlink").unwrap();
        fs::write(dir.0.join("file.txt"), "content").unwrap();
        symlink(&dir.0.join("file.txt"), &dir.0.join("link")).unwrap();
        assert_eq!(fs::read_to_string(dir.0.join("link")).unwrap(), "content");
    }

    #[test]
    fn registered_hook_finds_matching_annotation() {
        let dir = TempDir::create("pithos-hook-test").unwrap();
        fs::write(
            dir.0.join("pithos-egress-cap.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/pithos-egress-cap.sh", "args": [] },
              "when": { "annotations": { "pithos.networking": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();
        fs::write(
            dir.0.join("other.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/other.sh", "args": [] },
              "when": { "annotations": { "some.other.annotation": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();

        let (json_path, hook_path) = registered_hook(std::slice::from_ref(&dir.0)).unwrap();
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
            dir.0.join("other.json"),
            r#"{
              "version": "1.0.0",
              "hook": { "path": "/usr/local/bin/other.sh", "args": [] },
              "when": { "annotations": { "some.other.annotation": "1" } },
              "stages": ["createRuntime"]
            }"#,
        )
        .unwrap();
        assert!(registered_hook(std::slice::from_ref(&dir.0)).is_none());
    }
}

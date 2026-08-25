//! Cross-platform helpers.
//!
//! Every OS-specific call in the crate is routed through this module so the
//! rest of the code can be written without `#[cfg]` at the call sites. Each
//! function provides a functional implementation for both Unix and Windows.

#[cfg(any(windows, target_os = "macos"))]
use eyre::Result;
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

pub(crate) fn volume_spec(source: &Path, target: &str, read_only: bool) -> String {
    let mode = if read_only { "ro" } else { "rw" };
    format!("{}:{target}:{mode}", source.display())
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
    tracing::debug!(command, "running shell command");
    let result = run_shell_impl(command);
    match &result {
        Ok(status) => tracing::trace!(command, %status, "shell command finished"),
        Err(error) => tracing::trace!(command, %error, "shell command failed to start"),
    }
    result
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
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn machine_ssh(script: &str) -> Result<std::process::Output> {
    use eyre::WrapErr;
    use std::io::Write;
    tracing::debug!(script, "running podman machine ssh");
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use crate::sandbox::TempDir;

    #[test]
    fn volume_spec_selects_mutability() {
        let source = Path::new("/tmp/opencode");
        assert_eq!(
            volume_spec(source, "/data", false),
            "/tmp/opencode:/data:rw"
        );
        assert_eq!(
            volume_spec(source, "/config", true),
            "/tmp/opencode:/config:ro"
        );
    }

    #[test]
    fn symlink_round_trip() {
        let dir = TempDir::create("pithos-platform-symlink").unwrap();
        fs::write(dir.path().join("file.txt"), "content").unwrap();
        symlink(&dir.path().join("file.txt"), &dir.path().join("link")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("link")).unwrap(),
            "content"
        );
    }
}

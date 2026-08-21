use eyre::{Result, WrapErr, bail};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::registry::{self, SessionRecord};

pub(crate) fn ps() -> Result<()> {
    let sessions = registry::prune()?;
    if sessions.is_empty() {
        println!("no running pithos sessions");
        return Ok(());
    }
    for session in &sessions {
        println!(
            "{:<30} {:<24} {:>8}  {}",
            session.id,
            repo_label(session),
            uptime(unix_now(), session.started_at),
            session.sandbox_path.display()
        );
    }
    Ok(())
}

pub(crate) fn shell(session_id: Option<String>) -> Result<()> {
    let session = resolve_session(session_id.as_deref())?;
    let mut command = Command::new("podman");
    command.args(["exec", "--interactive", "--tty"]);
    push_target(&mut command, &session);
    command.arg("bash");
    run_foreground(command, "shell")
}

pub(crate) fn exec(session_id: Option<String>, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("exec needs a command, e.g. pithos exec -- ls -la");
    }
    let session = resolve_session(session_id.as_deref())?;
    let mut command = Command::new("podman");
    command.arg("exec");
    push_target(&mut command, &session);
    command.args(args);
    run_foreground(command, "exec")
}

pub(crate) fn print_path(session_id: Option<String>) -> Result<()> {
    let session = resolve_session(session_id.as_deref())?;
    println!("{}", session.sandbox_path.display());
    Ok(())
}

fn resolve_session(session_id: Option<&str>) -> Result<SessionRecord> {
    let session = registry::resolve(&registry::prune()?, session_id)?;
    tracing::debug!(id = %session.id, container = %session.container_name, "resolved session");
    Ok(session)
}

fn push_target(command: &mut Command, session: &SessionRecord) {
    command.args(["--workdir", &session.workspace]);
    command.args(["--user", &session.user]);
    command.arg(&session.container_name);
}

fn run_foreground(mut command: Command, label: &str) -> Result<()> {
    tracing::debug!(label, ?command, "running foreground command");
    let status = command.status().wrap_err("could not execute podman exec")?;
    tracing::trace!(label, %status, "foreground command finished");
    if status.success() {
        Ok(())
    } else {
        bail!("{label} exited with {status}")
    }
}

fn repo_label(session: &SessionRecord) -> String {
    session
        .repo_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| session.repo_path.display().to_string())
}

fn uptime(now: u64, started_at: u64) -> String {
    let elapsed = now.saturating_sub(started_at);
    let hours = elapsed / 3600;
    let minutes = (elapsed % 3600) / 60;
    let seconds = elapsed % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_formats_seconds_minutes_hours() {
        assert_eq!(uptime(1000, 1000), "0s");
        assert_eq!(uptime(1061, 1000), "1m01s");
        assert_eq!(uptime(4723, 1000), "1h02m");
    }
}

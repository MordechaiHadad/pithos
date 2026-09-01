use eyre::{Result, WrapErr, bail};
use std::process::Command;

use crate::utils::environment;
use crate::registry::{self, SessionRecord};

pub(crate) fn resolve_session(session_id: Option<&str>) -> Result<SessionRecord> {
    let session = registry::resolve(&registry::prune()?, session_id)?;
    tracing::debug!(id = %session.identity.id, container = %session.identity.container_name, "resolved session");
    Ok(session)
}

pub(crate) fn push_target(command: &mut Command, session: &SessionRecord) {
    command.args(["--workdir", &session.runtime.workspace]);
    command.args(["--user", &session.runtime.user]);
    command.arg(&session.identity.container_name);
}

pub(crate) fn terminal_env_args(command: &mut Command) {
    for (key, value) in environment::terminal_env() {
        command.args(["--env", &format!("{key}={value}")]);
    }
}

pub(crate) fn run_foreground(mut command: Command, label: &str) -> Result<()> {
    tracing::debug!(label, ?command, "running foreground command");
    let status = command.status().wrap_err("could not execute podman exec")?;
    tracing::trace!(label, %status, "foreground command finished");
    if status.success() {
        Ok(())
    } else {
        bail!("{label} exited with {status}")
    }
}

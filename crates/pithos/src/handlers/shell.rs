use eyre::Result;
use std::process::Command;

use super::common;

pub(crate) fn shell(session_id: Option<String>) -> Result<()> {
    let session = common::resolve_session(session_id.as_deref())?;
    let mut command = Command::new("podman");
    command.args(["exec", "--interactive", "--tty"]);
    common::terminal_env_args(&mut command);
    common::push_target(&mut command, &session);
    command.arg("bash");
    common::run_foreground(command, "shell")
}

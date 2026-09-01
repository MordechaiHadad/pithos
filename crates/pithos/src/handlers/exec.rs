use eyre::{Result, bail};
use std::process::Command;

use super::common;

pub(crate) fn exec(session_id: Option<String>, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("exec needs a command, e.g. pithos exec -- ls -la");
    }
    let session = common::resolve_session(session_id.as_deref())?;
    let mut command = Command::new("podman");
    command.arg("exec");
    common::terminal_env_args(&mut command);
    common::push_target(&mut command, &session);
    command.args(args);
    common::run_foreground(command, "exec")
}

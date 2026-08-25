use eyre::Result;

use super::common;

pub(crate) fn path(session_id: Option<String>) -> Result<()> {
    let session = common::resolve_session(session_id.as_deref())?;
    println!("{}", session.sandbox_path.display());
    Ok(())
}

pub(crate) const AGENT_USER: &str = "agent";
pub(crate) const AGENT_HOME: &str = "/home/agent";

/// Environment variables through which the entrypoint learns which
/// unprivileged identity to drop to before exec'ing the harness.
pub(crate) const AGENT_UID_ENV: &str = "PITHOS_AGENT_UID";
pub(crate) const AGENT_GID_ENV: &str = "PITHOS_AGENT_GID";

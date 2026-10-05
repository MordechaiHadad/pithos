use dialoguer::{Confirm, Select, theme::ColorfulTheme};
use eyre::{Result, WrapErr, bail};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::registry::{self, SessionRecord};
use crate::utils::environment;

pub(crate) fn resolve_session(session_id: Option<&str>) -> Result<SessionRecord> {
    let session = registry::resolve(&registry::prune()?, session_id)?;
    tracing::debug!(id = %session.identity.id, container = %session.identity.container_name, "resolved session");
    Ok(session)
}

/// Resolves a session for pull/push, adding interactive selection and a cwd
/// mismatch confirmation when the command is being used interactively.
pub(crate) fn resolve_pull_push_session(
    session_id: Option<&str>,
    has_path_override: bool,
    auto_yes: bool,
    auto_no: bool,
    json: bool,
) -> Result<Option<SessionRecord>> {
    let interactive_guard = interactive_guard_enabled(
        std::io::stdin().is_terminal(),
        auto_yes,
        auto_no,
        json,
        has_path_override,
    );
    let sessions = registry::prune()?;
    let cwd = if interactive_guard {
        Some(std::env::current_dir().wrap_err("cannot determine current directory")?)
    } else {
        None
    };

    let session = if session_id.is_none() && sessions.len() > 1 && interactive_guard {
        let current_dir = cwd.as_deref().expect("interactive guard captures cwd");
        let Some(session) = select_session(&sessions, current_dir)? else {
            return Ok(None);
        };
        session
    } else {
        registry::resolve(&sessions, session_id)?
    };

    if let Some(current_dir) = cwd.as_deref()
        && !path_matches_repo(current_dir, &session.paths.repo_path)
    {
        if !confirm_path_mismatch(current_dir, &session)? {
            return Ok(None);
        }
    }

    tracing::debug!(id = %session.identity.id, container = %session.identity.container_name, "resolved pull/push session");
    Ok(Some(session))
}

fn select_session(sessions: &[SessionRecord], current_dir: &Path) -> Result<Option<SessionRecord>> {
    let labels: Vec<String> = sessions
        .iter()
        .map(|session| {
            let current = path_matches_repo(current_dir, &session.paths.repo_path);
            let marker = if current {
                format!(" {}", console::style("*").green().bold())
            } else {
                String::new()
            };
            format!(
                "{}{}  {}",
                session.identity.id,
                marker,
                session.paths.repo_path.display()
            )
        })
        .collect();
    let default = sessions
        .iter()
        .position(|session| path_matches_repo(current_dir, &session.paths.repo_path))
        .unwrap_or(0);
    let theme = ColorfulTheme::default();
    let selected = Select::with_theme(&theme)
        .with_prompt("Select a running Pithos session (* = current path)")
        .items(&labels)
        .default(default)
        .interact_opt()?;
    Ok(selected.map(|index| sessions[index].clone()))
}

fn confirm_path_mismatch(current_dir: &Path, session: &SessionRecord) -> Result<bool> {
    let theme = ColorfulTheme::default();
    Ok(Confirm::with_theme(&theme)
        .with_prompt(format!(
            "Current path {} does not match session {} repository {}. Proceed?",
            current_dir.display(),
            session.identity.id,
            session.paths.repo_path.display()
        ))
        .default(false)
        .interact_opt()?
        .unwrap_or(false))
}

fn path_matches_repo(current_dir: &Path, repo_path: &Path) -> bool {
    let current_dir = canonical_or_original(current_dir);
    let repo_path = canonical_or_original(repo_path);
    current_dir == repo_path || current_dir.starts_with(repo_path)
}

fn interactive_guard_enabled(
    stdin_is_terminal: bool,
    auto_yes: bool,
    auto_no: bool,
    json: bool,
    has_path_override: bool,
) -> bool {
    stdin_is_terminal && !auto_yes && !auto_no && !json && !has_path_override
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::TempDir;

    #[test]
    fn interactive_guard_only_runs_for_unflagged_tty_without_path_override() {
        assert!(interactive_guard_enabled(true, false, false, false, false));
        assert!(!interactive_guard_enabled(
            false, false, false, false, false
        ));
        assert!(!interactive_guard_enabled(true, true, false, false, false));
        assert!(!interactive_guard_enabled(true, false, true, false, false));
        assert!(!interactive_guard_enabled(true, false, false, true, false));
        assert!(!interactive_guard_enabled(true, false, false, false, true));
    }

    #[test]
    fn cwd_matches_repository_and_its_descendants_only() {
        let root = TempDir::create("pithos-path-guard").unwrap();
        let repo = root.path().join("repo");
        let nested = repo.join("nested");
        let sibling = root.path().join("repo-other");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        assert!(path_matches_repo(&repo, &repo));
        assert!(path_matches_repo(&nested, &repo));
        assert!(!path_matches_repo(&sibling, &repo));
    }
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

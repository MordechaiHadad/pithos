use eyre::{Result, WrapErr};
use std::env;
use std::path::Path;
use std::process::Command;

use crate::config::Config;
use crate::registry;
use crate::sandbox::{TempDir, apply_tree, copy_tree, has_changes, sweep_orphans};
use crate::session;
use crate::session::git;

/// First arguments of the `podman run` invocation: explicit OCI hook
/// directories (a no-op where remote clients cannot take them), then the
/// subcommand itself.
fn run_arg_prefix() -> Vec<String> {
    let mut prefix = Vec::new();
    crate::networking::append_oci_hooks_args(&mut prefix);
    prefix.push("run".to_string());
    prefix
}

pub(crate) fn run(
    config_path: Option<&Path>,
    toolchain: Option<String>,
    auto_yes: bool,
    auto_no: bool,
) -> Result<()> {
    let repository = env::current_dir().wrap_err("cannot determine current directory")?;
    if !repository.join(".git").exists() {
        eyre::bail!("current directory is not a git repository")
    }
    let config = Config::load(config_path, toolchain)?;
    sweep_orphans(&registry::sandbox_paths())?;
    run_session(&config, &repository, auto_yes, auto_no)
}

#[tracing::instrument(skip_all, fields(repository = %repository.display()))]
fn run_session(config: &Config, repository: &Path, auto_yes: bool, auto_no: bool) -> Result<()> {
    let up_to_date = config.image_up_to_date()?;
    tracing::debug!(up_to_date, "image freshness check");
    if !up_to_date {
        config.build_image()?;
    }
    let sandbox = TempDir::create("pithos-workspace")?;
    copy_tree(repository, sandbox.path(), &config.ignore)?;
    strip_remotes(sandbox.path())?;
    let current_user = format!(
        "{}:{}",
        crate::platform::current_uid(),
        crate::platform::current_gid()
    );
    let unmanaged_paths = session::unmanaged(config);
    let record = registry::SessionRecord::new(
        repository,
        sandbox.path(),
        &config.image_tag,
        &config.workspace,
        &current_user,
        unmanaged_paths.clone(),
        config.diff_viewer.clone(),
    );
    record.save()?;
    println!(
        "pithos session {}: inspect it live with `pithos shell {}` or open {} in your editor",
        record.id,
        record.id,
        sandbox.path().display()
    );
    let mut command = Command::new("podman");
    command.args(run_arg_prefix());
    command.args([
        "--rm",
        "--interactive",
        "--tty",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--userns=keep-id",
        "--name",
        &record.container_name,
    ]);
    command.args([
        "--volume",
        &format!("{}:{}:rw,Z", sandbox.path().display(), config.workspace),
    ]);
    command.args(["--workdir", &config.workspace]);
    command.args([
        "--tmpfs",
        &crate::harness::tmpfs_spec("/tmp"),
        "--user",
        &current_user,
    ]);
    command.args(["--volume", crate::agent::AGENT_HOME]);
    for (key, value) in &config.environment {
        command.args(["--env", &format!("{key}={value}")]);
    }
    command.args(["--env", &format!("HOME={}", crate::agent::AGENT_HOME)]);
    for (key, value) in config.harness.environment() {
        command.args(["--env", &format!("{key}={value}")]);
    }
    for (key, value) in crate::environment::terminal_env() {
        if !config.environment.contains_key(&key) {
            command.args(["--env", &format!("{key}={value}")]);
        }
    }
    config.harness.mount(&mut command)?;
    config.networking.apply_to(&mut command)?;
    if let Some(audio) = crate::audio::passthrough(config.audio) {
        tracing::debug!(volume = %audio.volume, "passing host audio through");
        command.args(["--volume", &audio.volume]);
        for (key, value) in audio.env {
            if !config.environment.contains_key(&key) {
                command.args(["--env", &format!("{key}={value}")]);
            }
        }
    }
    crate::networking::enforcement::spawn_check(&config.networking, record.container_name.clone());
    tracing::debug!(?command, "starting harness container");
    let status = command
        .arg(&config.image_tag)
        .status()
        .wrap_err("could not execute podman run")?;
    tracing::debug!(%status, "harness container exited");
    registry::remove(&record.id);
    let changed = has_changes(repository, sandbox.path(), &unmanaged_paths)?;
    tracing::trace!(changed = changed.len(), "change detection finished");
    let apply = if auto_yes {
        true
    } else if auto_no || changed.is_empty() {
        false
    } else {
        session::summarize(&changed, repository, sandbox.path());
        let mut session_view = None;
        session::review(
            &changed,
            repository,
            sandbox.path(),
            config.diff_viewer.as_deref(),
            &unmanaged_paths,
            &mut session_view,
        )?
    };
    tracing::debug!(apply, "review decision");
    if apply {
        apply_tree(sandbox.path(), repository, &unmanaged_paths)?;
    }
    if !status.success() {
        eyre::bail!("harness exited with {status}")
    }
    Ok(())
}

fn strip_remotes(repository: &Path) -> Result<()> {
    let output = git(repository, &["remote"])?;
    for name in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|name| !name.is_empty())
    {
        git(repository, &["remote", "remove", name])
            .wrap_err_with(|| format!("could not remove remote {name}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::git_ok;

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn hook_dirs_are_passed_before_the_run_subcommand() {
        let prefix = run_arg_prefix();
        assert_eq!(prefix.last().map(String::as_str), Some("run"));
        assert!(!prefix.is_empty());
        assert_eq!(prefix.first().map(String::as_str), Some("--hooks-dir"));
        let hook_dir_count = prefix.iter().filter(|arg| *arg == "--hooks-dir").count();
        assert_eq!(hook_dir_count, 3);
        assert!(
            prefix[..prefix.len() - 1]
                .chunks(2)
                .all(|pair| pair[0] == "--hooks-dir" && !pair[1].is_empty())
        );
    }

    #[test]
    fn strip_remotes_removes_all_remotes() {
        let repo = TempDir::create("pithos-test-remotes").unwrap();
        git_ok(repo.path(), &["init"]).unwrap();
        git_ok(
            repo.path(),
            &["remote", "add", "origin", "https://example.com/repo.git"],
        )
        .unwrap();
        git_ok(
            repo.path(),
            &[
                "remote",
                "add",
                "upstream",
                "https://example.com/upstream.git",
            ],
        )
        .unwrap();

        strip_remotes(repo.path()).unwrap();

        let output = git(repo.path(), &["remote"]).unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).trim().is_empty());
    }

    #[test]
    fn strip_remotes_noop_without_remotes() {
        let repo = TempDir::create("pithos-test-no-remotes").unwrap();
        git_ok(repo.path(), &["init"]).unwrap();

        strip_remotes(repo.path()).unwrap();
    }
}

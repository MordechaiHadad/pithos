use eyre::{Result, WrapErr, eyre};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;
use crate::registry;
use crate::sandbox::{TempDir, has_changes, sweep_orphans};
use crate::session;
use crate::session::strip_remotes;
use crate::workspace::{CopyStrategy, parse_override, populate_sandbox, worktree_volume_arg};

pub(crate) fn run(
    config_path: Option<&Path>,
    toolchain: Option<String>,
    auto_yes: bool,
    auto_no: bool,
) -> Result<()> {
    let repository = env::current_dir().wrap_err("cannot determine current directory")?;
    if crate::utils::platform::current_uid() == 0 {
        eyre::bail!(
            "pithos sessions require a non-root host user; running as root would leave \
             system paths writable inside the sandbox"
        );
    }
    let config = {
        let _span = tracing::debug_span!("configuration load").entered();
        Config::load(config_path, toolchain)?
    };
    let forced_strategy = parse_override(config.copy_strategy.as_deref())?;
    {
        let _span = tracing::debug_span!("orphan sweep").entered();
        sweep_orphans(&registry::sandbox_paths())?;
        let live_ids: Vec<String> = registry::list()
            .into_iter()
            .map(|record| record.identity.id)
            .collect();
        let _ = crate::snapshot::sweep_manifests(&live_ids);
    }
    run_session(&config, &repository, forced_strategy, auto_yes, auto_no)
}

#[tracing::instrument(
    level = "debug",
    skip_all,
    fields(repository = %repository.display())
)]
fn run_session(
    config: &Config,
    repository: &Path,
    forced_strategy: Option<CopyStrategy>,
    auto_yes: bool,
    auto_no: bool,
) -> Result<()> {
    let started = std::time::Instant::now();
    let prepared = prepare_session(config, repository, forced_strategy)?;
    let PreparedSession {
        sandbox,
        record,
        strategy,
        uid,
        gid,
        unmanaged_paths,
        whitelist_addresses,
    } = prepared;
    let workspace_name = sandbox
        .path()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| sandbox.path().display().to_string());
    eprintln!(
        "\n{} Pithos ready in {:.2}s\n\n{}   {}\n{} {}\n\n{}: pithos shell {}",
        console::style("✓").green().bold(),
        started.elapsed().as_secs_f64(),
        console::style("Session").dim(),
        console::style(&record.identity.id).cyan().bold(),
        console::style("Workspace").dim(),
        console::style(workspace_name).cyan(),
        console::style("Tip").yellow().bold(),
        console::style(&record.identity.id).cyan()
    );
    let mut command = Command::new("podman");
    command.arg("run");
    command.args([
        "--rm",
        "--interactive",
        "--tty",
        "--read-only",
        "--pull=never",
        "--cap-drop=ALL",
        "--cap-add=SETUID",
        "--cap-add=SETGID",
        "--cap-add=SETPCAP",
        "--security-opt=no-new-privileges",
        "--userns=keep-id",
        "--name",
        &record.identity.container_name,
    ]);
    if config.networking.enabled {
        command.args(["--cap-add=NET_ADMIN"]);
    }
    command.args([
        "--volume",
        &format!("{}:{}:rw,Z", sandbox.path().display(), config.workspace),
    ]);
    if let Some(volume) = worktree_volume_arg(repository, strategy) {
        tracing::debug!(
            volume,
            "mounting origin git objects read-only for the worktree tier"
        );
        command.args(["--volume", &volume]);
    }
    command.args(["--workdir", &config.workspace]);
    command.args(["--tmpfs", &pithos_harness::tmpfs_spec("/tmp")]);
    let runtime_dir = format!("/run/user/{uid}");
    command.args([
        "--tmpfs",
        &pithos_harness::tmpfs_spec(crate::utils::agent::AGENT_HOME),
    ]);
    command.args(["--tmpfs", &pithos_harness::tmpfs_spec(&runtime_dir)]);
    for (key, value) in &config.environment {
        command.args(["--env", &format!("{key}={value}")]);
    }
    command.args([
        "--env",
        &format!("HOME={}", crate::utils::agent::AGENT_HOME),
    ]);
    command.args([
        "--env",
        &format!("{}={uid}", crate::utils::agent::AGENT_UID_ENV),
    ]);
    command.args([
        "--env",
        &format!("{}={gid}", crate::utils::agent::AGENT_GID_ENV),
    ]);
    if !config.environment.contains_key("XDG_RUNTIME_DIR") {
        command.args(["--env", &format!("XDG_RUNTIME_DIR={runtime_dir}")]);
    }
    for (key, value) in live_tier_env() {
        if !config.environment.contains_key(key) {
            command.args(["--env", &format!("{key}={value}")]);
        }
    }
    if !config.environment.contains_key("PATH") {
        command.args(["--env-merge", "PATH=/home/agent/.local/share/mise/shims:/home/agent/.cargo/bin:/home/agent/.local/bin:${PATH}"]);
    }
    for (key, value) in config.harness.environment() {
        command.args(["--env", &format!("{key}={value}")]);
    }
    for (key, value) in crate::utils::environment::terminal_env() {
        if !config.environment.contains_key(&key) {
            command.args(["--env", &format!("{key}={value}")]);
        }
    }
    config.harness.mount(
        &mut command,
        &record.identity.id,
        &crate::registry::runtime_dir(),
    )?;
    config.networking.apply_to_resolved(
        &mut command,
        &whitelist_addresses.0,
        &whitelist_addresses.1,
    );
    if let Some(audio) = crate::utils::audio::passthrough(config.audio) {
        tracing::debug!(volume = %audio.volume, "passing host audio through");
        command.args(["--volume", &audio.volume]);
        for (key, value) in audio.env {
            if !config.environment.contains_key(&key) {
                command.args(["--env", &format!("{key}={value}")]);
            }
        }
    }
    crate::networking::enforcement::spawn_check(
        &config.networking,
        record.identity.container_name.clone(),
    );
    tracing::debug!(?command, "starting harness container");
    let mut child = command
        .arg(config.image_tag())
        .spawn()
        .wrap_err("could not execute podman run")?;
    tracing::debug!(pid = child.id(), "harness container handed off to podman");
    let status = child.wait().wrap_err("could not wait for podman run")?;
    tracing::debug!(%status, "harness container exited");
    registry::remove(&record.identity.id);
    let changed = detect_changes_with_snapshot(
        repository,
        sandbox.path(),
        &unmanaged_paths,
        &record.identity.id,
        &record.options.strategy,
    )?;
    if !changed.is_empty() {
        tracing::debug!(changed = changed.len(), "updating snapshot before review");
    }
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
        let method = strategy.copy_method();
        let progress = if crate::utils::progress::is_progress_enabled() {
            Some(())
        } else {
            None
        };
        if progress.is_some() {
            crate::utils::progress::with_apply_progress(|p| {
                crate::sandbox::apply_tree(
                    sandbox.path(),
                    repository,
                    &unmanaged_paths,
                    method,
                    Some(p),
                )
            })?;
        } else {
            crate::sandbox::apply_tree(sandbox.path(), repository, &unmanaged_paths, method, None)?;
        }
        let _ = update_snapshot(
            &record.identity.id,
            sandbox.path(),
            &unmanaged_paths,
            strategy.label(),
        );
    } else {
        crate::snapshot::remove_snapshot(&record.identity.id);
    }
    if !status.success() {
        eyre::bail!("harness exited with {status}")
    }
    Ok(())
}

/// Sandbox, session record, population tier, and runtime identity needed to
/// start a container.
struct PreparedSession {
    sandbox: TempDir,
    record: registry::SessionRecord,
    strategy: CopyStrategy,
    uid: u32,
    gid: u32,
    unmanaged_paths: Vec<String>,
    whitelist_addresses: (Vec<std::net::Ipv4Addr>, Vec<std::net::Ipv6Addr>),
}

/// Prepares the sandbox while the image freshness check or build runs on a
/// parallel thread. The two steps touch disjoint state, so the workspace copy
/// hides inside the image preparation window and both must succeed before a
/// container starts.
fn prepare_session(
    config: &Config,
    repository: &Path,
    forced_strategy: Option<CopyStrategy>,
) -> Result<PreparedSession> {
    let _span = tracing::debug_span!("session preparation").entered();
    std::thread::scope(|scope| {
        let builder = scope.spawn(|| -> Result<()> {
            let _span = tracing::debug_span!("image preparation").entered();
            let up_to_date = config.image_up_to_date()?;
            tracing::debug!(up_to_date, "image freshness checked");
            if !up_to_date {
                config.build_image()?;
            }
            Ok(())
        });
        let whitelist_resolver = scope.spawn(|| config.networking.resolve_whitelist());
        let prepared = prepare_workspace(config, repository, forced_strategy);
        match prepared {
            Ok(pair) => {
                builder
                    .join()
                    .map_err(|_| eyre!("image preparation thread panicked"))??;
                let whitelist_addresses = whitelist_resolver
                    .join()
                    .map_err(|_| eyre!("whitelist resolver thread panicked"))?;
                Ok(PreparedSession {
                    whitelist_addresses,
                    ..pair
                })
            }
            Err(prepare_error) => {
                let _ = builder.join();
                let _ = whitelist_resolver.join();
                Err(prepare_error)
            }
        }
    })
}

#[tracing::instrument(
    level = "debug",
    skip_all,
    fields(repository = %repository.display())
)]
fn prepare_workspace(
    config: &Config,
    repository: &Path,
    forced_strategy: Option<CopyStrategy>,
) -> Result<PreparedSession> {
    let sandbox = TempDir::create("pithos-workspace")?;
    let strategy = populate_sandbox(repository, sandbox.path(), &config.ignore, forced_strategy)?;
    // strip_remotes is best-effort: non-git sandboxes are fine
    let _ = strip_remotes(sandbox.path());
    let uid = crate::utils::platform::current_uid();
    let gid = crate::utils::platform::current_gid();
    let current_user = format!("{uid}:{gid}");
    let unmanaged_paths = session::unmanaged(config);
    let record = registry::SessionRecord::new(registry::SessionRecordInput {
        repository,
        sandbox_path: sandbox.path(),
        image_tag: &config.image_tag(),
        workspace: &config.workspace,
        user: &current_user,
        unmanaged: unmanaged_paths.clone(),
        diff_viewer: config.diff_viewer.clone(),
        strategy: Some(strategy),
    });
    let _span = tracing::debug_span!("save session record").entered();
    record.save()?;
    let _ = update_snapshot(
        &record.identity.id,
        sandbox.path(),
        &unmanaged_paths,
        strategy.label(),
    );
    Ok(PreparedSession {
        sandbox,
        record,
        strategy,
        uid,
        gid,
        unmanaged_paths,
        whitelist_addresses: (Vec::new(), Vec::new()),
    })
}

fn detect_changes_with_snapshot(
    repository: &Path,
    sandbox: &Path,
    unmanaged: &[String],
    session_id: &str,
    strategy: &Option<String>,
) -> Result<Vec<PathBuf>> {
    if let Ok(Some(changed)) = crate::snapshot::try_has_changes_via_snapshot(
        repository,
        sandbox,
        unmanaged,
        strategy.as_deref(),
        session_id,
    ) {
        tracing::debug!(
            changed = changed.len(),
            "snapshot change detection succeeded"
        );
        return Ok(changed);
    }
    tracing::debug!("snapshot fallback to full scan");
    let changed = has_changes(repository, sandbox, unmanaged)?;
    tracing::debug!(
        changed = changed.len(),
        "full scan change detection finished"
    );
    Ok(changed)
}

fn update_snapshot(
    session_id: &str,
    sandbox: &Path,
    unmanaged: &[String],
    strategy_label: &str,
) -> Result<()> {
    let entries = crate::snapshot::capture(sandbox, unmanaged)?;
    crate::snapshot::save_snapshot(session_id, entries, unmanaged, Some(strategy_label))?;
    Ok(())
}

/// Package-manager state redirected into the ephemeral home so runtime
/// installs work and are discarded with the session. Toolchain *routers*
/// (`MISE_DATA_DIR`, `RUSTUP_HOME`) deliberately stay pointed at the baked
/// tree: mise shims and rustup proxies resolve through them at exec time,
/// and both only read during normal operation.
fn live_tier_env() -> [(&'static str, String); 2] {
    [
        (
            "CARGO_HOME",
            format!("{}/.cargo", crate::utils::agent::AGENT_HOME),
        ),
        (
            "NPM_CONFIG_PREFIX",
            format!("{}/.local", crate::utils::agent::AGENT_HOME),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{git, git_ok, strip_remotes};

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

    #[test]
    fn live_tier_env_redirects_writable_state_only() {
        let home = crate::utils::agent::AGENT_HOME;
        let env: std::collections::BTreeMap<_, _> = live_tier_env().into_iter().collect();
        assert_eq!(env.get("CARGO_HOME").unwrap(), &format!("{home}/.cargo"));
        assert_eq!(
            env.get("NPM_CONFIG_PREFIX").unwrap(),
            &format!("{home}/.local")
        );
        assert!(
            !env.contains_key("MISE_DATA_DIR"),
            "shims resolve through the baked data dir"
        );
        assert!(
            !env.contains_key("RUSTUP_HOME"),
            "rustup proxies resolve through the baked toolchain root"
        );
    }
}

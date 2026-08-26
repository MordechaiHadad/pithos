use eyre::{Result, WrapErr, eyre};
use std::env;
use std::path::Path;
use std::process::Command;

use crate::config::Config;
use crate::registry;
use crate::sandbox::{TempDir, apply_tree, has_changes, sweep_orphans};
use crate::session;
use crate::session::strip_remotes;
use crate::strategy::{CONTAINER_GIT_OBJECTS, CopyStrategy, parse_override, populate_sandbox};

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
    if crate::platform::current_uid() == 0 {
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
    println!(
        "pithos session {}: inspect it live with `pithos shell {}` or open {} in your editor",
        record.id,
        record.id,
        sandbox.path().display()
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
        &record.container_name,
    ]);
    if config.networking.enabled {
        command.args(["--cap-add=NET_ADMIN"]);
    }
    command.args([
        "--volume",
        &format!("{}:{}:rw,Z", sandbox.path().display(), config.workspace),
    ]);
    if strategy == CopyStrategy::Worktree {
        let objects_dir = repository.join(".git").join("objects");
        tracing::debug!(
            volume = %objects_dir.display(),
            "mounting origin git objects read-only for the worktree tier"
        );
        command.args([
            "--volume",
            &format!("{}:{CONTAINER_GIT_OBJECTS}:ro,Z", objects_dir.display()),
        ]);
    }
    command.args(["--workdir", &config.workspace]);
    command.args(["--tmpfs", &crate::harness::tmpfs_spec("/tmp")]);
    let runtime_dir = format!("/run/user/{uid}");
    command.args([
        "--tmpfs",
        &crate::harness::tmpfs_spec(crate::agent::AGENT_HOME),
    ]);
    command.args(["--tmpfs", &crate::harness::tmpfs_spec(&runtime_dir)]);
    for (key, value) in &config.environment {
        command.args(["--env", &format!("{key}={value}")]);
    }
    command.args(["--env", &format!("HOME={}", crate::agent::AGENT_HOME)]);
    command.args(["--env", &format!("{}={uid}", crate::agent::AGENT_UID_ENV)]);
    command.args(["--env", &format!("{}={gid}", crate::agent::AGENT_GID_ENV)]);
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
    for (key, value) in crate::environment::terminal_env() {
        if !config.environment.contains_key(&key) {
            command.args(["--env", &format!("{key}={value}")]);
        }
    }
    config.harness.mount(&mut command, &record.id)?;
    config.networking.apply_to_resolved(
        &mut command,
        &whitelist_addresses.0,
        &whitelist_addresses.1,
    );
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
    let mut child = command
        .arg(config.image_tag())
        .spawn()
        .wrap_err("could not execute podman run")?;
    tracing::debug!(pid = child.id(), "harness container handed off to podman");
    let status = child.wait().wrap_err("could not wait for podman run")?;
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
    let _span = tracing::debug_span!("strip remotes").entered();
    strip_remotes(sandbox.path())?;
    drop(_span);
    let uid = crate::platform::current_uid();
    let gid = crate::platform::current_gid();
    let current_user = format!("{uid}:{gid}");
    let unmanaged_paths = session::unmanaged(config);
    let record = registry::SessionRecord::new(
        repository,
        sandbox.path(),
        &config.image_tag(),
        &config.workspace,
        &current_user,
        unmanaged_paths.clone(),
        config.diff_viewer.clone(),
    );
    let _span = tracing::debug_span!("save session record").entered();
    record.save()?;
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

/// Package-manager state redirected into the ephemeral home so runtime
/// installs work and are discarded with the session. Toolchain *routers*
/// (`MISE_DATA_DIR`, `RUSTUP_HOME`) deliberately stay pointed at the baked
/// tree: mise shims and rustup proxies resolve through them at exec time,
/// and both only read during normal operation.
fn live_tier_env() -> [(&'static str, String); 2] {
    [
        ("CARGO_HOME", format!("{}/.cargo", crate::agent::AGENT_HOME)),
        (
            "NPM_CONFIG_PREFIX",
            format!("{}/.local", crate::agent::AGENT_HOME),
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
        let home = crate::agent::AGENT_HOME;
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

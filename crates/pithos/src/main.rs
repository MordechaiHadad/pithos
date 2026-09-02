use clap::{ArgAction, Parser, Subcommand};
use eyre::{Result, bail};
use std::path::PathBuf;
use std::process::ExitCode;

mod config;
mod handlers;
mod image;
mod networking;
mod registry;
mod sandbox;
mod session;
mod snapshot;
mod utils;
mod workspace;

#[derive(Parser)]
#[command(name = "pithos", version, about = "Run disposable agent workspaces")]
#[derive(Debug)]
struct Cli {
    /// Increase verbosity (-v for DEBUG, -vv for TRACE, -vvv for global TRACE)
    #[arg(short = 'v', long = "verbose", action = ArgAction::Count, global = true)]
    verbose: u8,
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(short = 't', long)]
    toolchain: Option<String>,
    #[arg(long)]
    harness: Option<String>,
    #[arg(long, global = true)]
    yes: bool,
    #[arg(long, global = true)]
    no: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init,
    Build {
        #[arg(short = 't', long)]
        toolchain: Option<String>,
        #[arg(long)]
        harness: Option<String>,
    },
    Run {
        #[arg(short = 't', long)]
        toolchain: Option<String>,
        #[arg(long)]
        harness: Option<String>,
    },
    Ps,
    Shell {
        session: Option<String>,
    },
    Exec {
        session: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    Path {
        session: Option<String>,
    },
    Pull {
        session: Option<String>,
        /// Pull into this directory instead of the origin repository
        #[arg(long)]
        path: Option<PathBuf>,
        /// Report changes without applying them
        #[arg(long)]
        dry_run: bool,
        /// Print a machine-readable JSON report instead of text
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = ?error, "execution failed");
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<()> {
    let cli = Cli::parse();
    utils::progress::init_tracing_with_progress(cli.verbose);
    spawn_signal_cleanup();
    if cli.yes && cli.no {
        bail!("--yes and --no cannot be combined")
    }
    match cli.command {
        Some(Commands::Init) => handlers::init(),
        Some(Commands::Build { toolchain, harness }) => handlers::build(
            cli.config.as_deref(),
            toolchain.or(cli.toolchain),
            harness.or(cli.harness),
        ),
        None => handlers::run(
            cli.config.as_deref(),
            cli.toolchain,
            cli.harness,
            cli.yes,
            cli.no,
        ),
        Some(Commands::Run { toolchain, harness }) => handlers::run(
            cli.config.as_deref(),
            toolchain.or(cli.toolchain),
            harness.or(cli.harness),
            cli.yes,
            cli.no,
        ),
        Some(Commands::Ps) => handlers::ps(),
        Some(Commands::Shell { session }) => handlers::shell(session),
        Some(Commands::Exec { session, command }) => handlers::exec(session, &command),
        Some(Commands::Path { session }) => handlers::path(session),
        Some(Commands::Pull {
            session,
            path,
            dry_run,
            json,
        }) => handlers::pull(
            session.as_deref(),
            path.as_deref(),
            handlers::PullOptions {
                auto_yes: cli.yes,
                auto_no: cli.no,
                dry_run,
                json,
            },
        ),
    }
}

#[cfg(unix)]
fn spawn_signal_cleanup() {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let Ok(mut signals) = Signals::new([SIGINT, SIGTERM, SIGHUP]) else {
        return;
    };
    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            sandbox::remove_active_temp_dirs();
            std::process::exit(128 + signal);
        }
    });
}

#[cfg(not(unix))]
fn spawn_signal_cleanup() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolchain_flag_is_scoped_to_build_and_run() {
        assert!(Cli::try_parse_from(["pithos", "-t", "rust"]).is_ok());
        assert!(Cli::try_parse_from(["pithos", "run", "-t", "rust"]).is_ok());
        assert!(Cli::try_parse_from(["pithos", "build", "-t", "python"]).is_ok());
        assert!(Cli::try_parse_from(["pithos", "init", "-t", "rust"]).is_err());
        assert!(Cli::try_parse_from(["pithos", "ps", "-t", "rust"]).is_err());
        assert!(Cli::try_parse_from(["pithos", "pull", "-t", "rust"]).is_err());
        assert!(Cli::try_parse_from(["pithos", "shell", "-t", "rust"]).is_err());
    }

    #[test]
    fn subcommand_toolchain_overrides_top_level_selection() {
        let cli = Cli::try_parse_from(["pithos", "-t", "old", "run", "-t", "new"]).unwrap();
        let Commands::Run {
            toolchain,
            harness: _,
        } = cli.command.expect("run subcommand")
        else {
            panic!("expected the run subcommand");
        };
        assert_eq!(toolchain.as_deref(), Some("new"));
    }

    #[test]
    fn harness_flag_is_scoped_to_build_and_run() {
        assert!(Cli::try_parse_from(["pithos", "--harness", "opencode"]).is_ok());
        assert!(Cli::try_parse_from(["pithos", "run", "--harness", "opencode"]).is_ok());
        assert!(Cli::try_parse_from(["pithos", "build", "--harness", "codex"]).is_ok());
        assert!(Cli::try_parse_from(["pithos", "init", "--harness", "opencode"]).is_err());
        assert!(Cli::try_parse_from(["pithos", "ps", "--harness", "opencode"]).is_err());
        assert!(Cli::try_parse_from(["pithos", "pull", "--harness", "opencode"]).is_err());
        assert!(Cli::try_parse_from(["pithos", "shell", "--harness", "opencode"]).is_err());
        assert!(Cli::try_parse_from(["pithos", "exec", "--harness", "opencode"]).is_err());
        assert!(Cli::try_parse_from(["pithos", "path", "--harness", "opencode"]).is_err());
    }

    #[test]
    fn subcommand_harness_overrides_top_level_selection() {
        let cli =
            Cli::try_parse_from(["pithos", "--harness", "old", "run", "--harness", "new"]).unwrap();
        let Commands::Run {
            harness,
            toolchain: _,
        } = cli.command.expect("run subcommand")
        else {
            panic!("expected the run subcommand");
        };
        assert_eq!(harness.as_deref(), Some("new"));
    }

    #[test]
    fn implicit_run_harness_is_parsed() {
        let cli = Cli::try_parse_from(["pithos", "--harness", "codex"]).unwrap();
        assert_eq!(cli.harness.as_deref(), Some("codex"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn build_harness_overrides_top_level() {
        let cli = Cli::try_parse_from(["pithos", "--harness", "old", "build", "--harness", "new"])
            .unwrap();
        let Commands::Build {
            harness,
            toolchain: _,
        } = cli.command.expect("build subcommand")
        else {
            panic!("expected the build subcommand");
        };
        assert_eq!(harness.as_deref(), Some("new"));
    }
}

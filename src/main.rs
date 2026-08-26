use clap::{ArgAction, Parser, Subcommand};
use eyre::{Result, bail};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan, prelude::*};

mod agent;
mod audio;
mod config;
mod environment;
mod handlers;
mod harness;
mod image;
mod networking;
mod platform;
mod registry;
mod sandbox;
mod session;
mod strategy;

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
    },
    Run {
        #[arg(short = 't', long)]
        toolchain: Option<String>,
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
            eprintln!("error: {error:?}");
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    spawn_signal_cleanup();
    if cli.yes && cli.no {
        bail!("--yes and --no cannot be combined")
    }
    match cli.command {
        Some(Commands::Init) => handlers::init(),
        Some(Commands::Build { toolchain }) => {
            handlers::build(cli.config.as_deref(), toolchain.or(cli.toolchain))
        }
        None => handlers::run(cli.config.as_deref(), cli.toolchain, cli.yes, cli.no),
        Some(Commands::Run { toolchain }) => handlers::run(
            cli.config.as_deref(),
            toolchain.or(cli.toolchain),
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

fn init_tracing(verbose: u8) {
    let crate_name = env!("CARGO_CRATE_NAME");
    let env_filter = if verbose > 0 {
        match verbose {
            1 => EnvFilter::new(format!("{crate_name}=debug")),
            2 => EnvFilter::new(format!("{crate_name}=trace")),
            _ => EnvFilter::new("trace"),
        }
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("warn,{crate_name}=info")))
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_span_events(FmtSpan::CLOSE),
        )
        .init();
}

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
        let Commands::Run { toolchain } = cli.command.expect("run subcommand") else {
            panic!("expected the run subcommand");
        };
        assert_eq!(toolchain.as_deref(), Some("new"));
    }
}

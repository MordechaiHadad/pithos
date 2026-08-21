use clap::{Parser, Subcommand};
use eyre::{Result, WrapErr, bail};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::Config;

mod attach;
mod config;
mod harness;
mod image;
mod networking;
mod platform;
mod registry;
mod sandbox;
mod session;

#[derive(Parser)]
#[command(name = "pithos", version, about = "Run disposable agent workspaces")]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(short = 't', long, global = true)]
    toolchain: Option<config::Toolchain>,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    no: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Init,
    Build,
    Run,
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
    if cli.yes && cli.no {
        bail!("--yes and --no cannot be combined")
    }
    match cli.command {
        Some(Commands::Init) => Config::init(cli.toolchain),
        Some(Commands::Build) => Config::load(cli.config.as_deref())?
            .with_toolchain(cli.toolchain)
            .build_image(),
        Some(Commands::Run) | None => {
            let repository = env::current_dir().wrap_err("cannot determine current directory")?;
            let config = Config::load(cli.config.as_deref())?.with_toolchain(cli.toolchain);
            session::run_session(&config, &repository, cli.yes, cli.no)
        }
        Some(Commands::Ps) => attach::ps(),
        Some(Commands::Shell { session }) => attach::shell(session),
        Some(Commands::Exec { session, command }) => attach::exec(session, &command),
        Some(Commands::Path { session }) => attach::print_path(session),
    }
}

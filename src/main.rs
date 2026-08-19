use clap::{Parser, Subcommand};
use eyre::{Result, WrapErr, bail};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::Config;

mod config;
mod harness;
mod image;
mod networking;
mod sandbox;
mod session;

#[derive(Parser)]
#[command(name = "pithos", version, about = "Run disposable agent workspaces")]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    no: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Build,
    Run,
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
    let repository = env::current_dir().wrap_err("cannot determine current directory")?;
    let config = Config::load(cli.config.as_deref())?;
    match cli.command {
        Some(Commands::Build) => config.build_image(),
        Some(Commands::Run) | None => session::run_session(&config, &repository, cli.yes, cli.no),
    }
}

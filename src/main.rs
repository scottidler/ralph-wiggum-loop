use clap::Parser;
use eyre::{Context, Result};
use log::info;
use std::fs;
use std::path::PathBuf;

mod cli;
mod commands;
mod config;
mod git;
mod progress;
mod runner;
mod templates;
mod validation;

use cli::{Cli, Commands};

fn setup_logging() -> Result<()> {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rwl")
        .join("logs");

    fs::create_dir_all(&log_dir).context("Failed to create log directory")?;

    let log_file = log_dir.join("rwl.log");

    let target = Box::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .context("Failed to open log file")?,
    );

    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(target))
        .init();

    info!("Logging initialized, writing to: {}", log_file.display());
    Ok(())
}

fn main() -> Result<()> {
    setup_logging().context("Failed to setup logging")?;

    let cli = Cli::parse();

    info!("Starting with config from: {:?}", cli.config);

    match &cli.command {
        Commands::Init => {
            commands::init::run(&cli).context("Init command failed")?;
        }
        Commands::Run(args) => {
            commands::run::run(&cli, args).context("Run command failed")?;
        }
        Commands::Status => {
            commands::status::run(&cli).context("Status command failed")?;
        }
    }

    Ok(())
}

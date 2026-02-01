use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rwl",
    about = "Ralph Wiggum Loop - Iterative AI-assisted development",
    version = env!("GIT_DESCRIBE"),
    after_help = "Logs are written to: ~/.local/share/rwl/logs/rwl.log"
)]
pub struct Cli {
    /// Path to config file (overrides default locations)
    #[arg(short, long, global = true, help = "Path to config file")]
    pub config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long, global = true, help = "Enable verbose output")]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize .rwl/ in current directory
    Init,

    /// Run the loop
    Run(RunArgs),

    /// Show current progress
    Status,
}

#[derive(Parser)]
pub struct RunArgs {
    /// Path to the implementation plan file
    #[arg(short, long, required = true)]
    pub plan: PathBuf,

    /// Maximum number of iterations (overrides config)
    #[arg(short, long)]
    pub max_iterations: Option<u32>,

    /// LLM model to use (overrides config)
    #[arg(short = 'M', long)]
    pub model: Option<String>,

    /// Iteration timeout in minutes (overrides config)
    #[arg(short, long)]
    pub timeout: Option<u32>,
}

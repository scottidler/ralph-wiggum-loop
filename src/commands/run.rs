use crate::cli::{Cli, RunArgs};
use crate::config::Config;
use crate::progress::ProgressTracker;
use crate::runner::{LoopOutcome, LoopRunner};
use colored::*;
use eyre::{Context, Result};
use std::path::Path;

pub fn run(_cli: &Cli, args: &RunArgs) -> Result<()> {
    let work_dir = Path::new(".");

    // 1. Ensure .rwl/ exists
    ensure_initialized(work_dir)?;

    // 2. Load config, apply CLI overrides
    let mut config = Config::load_local(work_dir).context("Failed to load local config")?;

    if let Some(max) = args.max_iterations {
        config.loop_config.max_iterations = max;
    }
    if let Some(ref model) = args.model {
        config.llm.model = model.clone();
    }
    if let Some(timeout) = args.timeout {
        config.loop_config.iteration_timeout_minutes = timeout;
    }

    // Save updated config (with CLI overrides)
    config.save_local(work_dir)?;

    // 3. Validate plan file exists
    ensure_plan_exists(&args.plan)?;

    // 4. Initialize progress tracker
    let progress_path = Config::local_config_dir(work_dir).join("progress.txt");
    let progress = ProgressTracker::new(&progress_path);

    // Only init if this is a fresh run
    if !progress.exists() {
        progress.init(&args.plan)?;
    }

    // 5. Print startup banner
    print_banner(&config, &args.plan)?;

    // 6. Run the loop
    let runner = LoopRunner::new(work_dir, args.plan.clone())?;
    let outcome = runner.run()?;

    // 7. Print result
    print_outcome(&outcome)?;

    Ok(())
}

fn ensure_initialized(work_dir: &Path) -> Result<()> {
    let rwl_dir = Config::local_config_dir(work_dir);
    if !rwl_dir.exists() {
        return Err(eyre::eyre!("Not initialized. Run {} first.", "rwl init".cyan()));
    }
    Ok(())
}

fn ensure_plan_exists(plan_path: &Path) -> Result<()> {
    if !plan_path.exists() {
        return Err(eyre::eyre!("Plan file not found: {}", plan_path.display()));
    }
    Ok(())
}

fn print_banner(config: &Config, plan_path: &Path) -> Result<()> {
    println!();
    println!("{}", "╔════════════════════════════════════════╗".cyan());
    println!("{}", "║     Ralph Wiggum Loop - Starting       ║".cyan());
    println!("{}", "╚════════════════════════════════════════╝".cyan());
    println!();
    println!("  {} {}", "Plan:".bold(), plan_path.display());
    println!("  {} {}", "Model:".bold(), config.llm.model);
    println!("  {} {}", "Max iterations:".bold(), config.loop_config.max_iterations);
    println!(
        "  {} {} minutes",
        "Timeout:".bold(),
        config.loop_config.iteration_timeout_minutes
    );
    println!("  {} {}", "Validation:".bold(), config.validation.command);
    println!("  {} {}", "Quality gates:".bold(), config.quality_gates.len());
    println!();

    Ok(())
}

fn print_outcome(outcome: &LoopOutcome) -> Result<()> {
    println!();
    match outcome {
        LoopOutcome::Complete { iterations } => {
            println!("{}", "╔════════════════════════════════════════╗".green());
            println!("{}", "║           Loop Complete!               ║".green());
            println!("{}", "╚════════════════════════════════════════╝".green());
            println!();
            println!("  {} {} iterations", "Completed in:".bold(), iterations);
        }
        LoopOutcome::MaxIterations { iterations } => {
            println!("{}", "╔════════════════════════════════════════╗".yellow());
            println!("{}", "║       Max Iterations Reached           ║".yellow());
            println!("{}", "╚════════════════════════════════════════╝".yellow());
            println!();
            println!("  {} {} iterations", "Ran:".bold(), iterations);
            println!();
            println!(
                "  Consider increasing {} or checking progress.",
                "max_iterations".cyan()
            );
        }
        LoopOutcome::Stopped { iterations, reason } => {
            println!("{}", "╔════════════════════════════════════════╗".yellow());
            println!("{}", "║           Loop Stopped                 ║".yellow());
            println!("{}", "╚════════════════════════════════════════╝".yellow());
            println!();
            println!("  {} {} iterations", "Ran:".bold(), iterations);
            println!("  {} {}", "Reason:".bold(), reason);
        }
        LoopOutcome::Error { iterations, error } => {
            println!("{}", "╔════════════════════════════════════════╗".red());
            println!("{}", "║              Error                     ║".red());
            println!("{}", "╚════════════════════════════════════════╝".red());
            println!();
            println!("  {} {} iterations", "Ran:".bold(), iterations);
            println!("  {} {}", "Error:".bold(), error);
        }
    }
    println!();

    Ok(())
}

use crate::cli::{Cli, RunArgs};
use crate::config::Config;
use crate::git::reposlug;
use crate::progress::ProgressTracker;
use crate::result::RunResult;
use crate::runner::LoopRunner;
use colored::*;
use eyre::{Context, Result};
use std::path::{Path, PathBuf};

pub fn run(_cli: &Cli, args: &RunArgs) -> Result<RunResult> {
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

    // 4. Create session directory
    let session_dir = create_session_dir(work_dir, args.session_path.as_ref())?;

    // 5. Initialize progress tracker in session directory (always fresh)
    let progress_path = session_dir.join("progress.txt");
    let progress = ProgressTracker::new(&progress_path);
    progress.init(&args.plan)?;

    // 6. Print startup banner
    print_banner(&config, &args.plan, &session_dir)?;

    // 7. Run the loop
    let mut runner = LoopRunner::new(work_dir, args.plan.clone(), session_dir.clone())?;
    let result = runner.run()?;

    // 8. Write result.json to session directory
    result.write_json()?;

    // 9. Print result
    print_outcome(&result)?;

    // 10. Print session path (always, for callers to discover)
    println!("session: {}", session_dir.display());

    Ok(result)
}

fn create_session_dir(work_dir: &Path, session_path: Option<&PathBuf>) -> Result<PathBuf> {
    let base = match session_path {
        Some(p) => p.clone(),
        None => {
            let slug = reposlug(work_dir)?;
            PathBuf::from("/tmp/rwl").join(slug)
        }
    };
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let session_dir = base.join(&timestamp);
    std::fs::create_dir_all(&session_dir).context("Failed to create session directory")?;
    Ok(session_dir)
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

fn print_banner(config: &Config, plan_path: &Path, session_dir: &Path) -> Result<()> {
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
    println!("  {} {}", "Session:".bold(), session_dir.display());
    println!();

    Ok(())
}

fn print_outcome(result: &RunResult) -> Result<()> {
    println!();
    match result.outcome.as_str() {
        "complete" => {
            println!("{}", "╔════════════════════════════════════════╗".green());
            println!("{}", "║           Loop Complete!               ║".green());
            println!("{}", "╚════════════════════════════════════════╝".green());
            println!();
            println!("  {} {} iterations", "Completed in:".bold(), result.iterations);
        }
        "max-iterations" => {
            println!("{}", "╔════════════════════════════════════════╗".yellow());
            println!("{}", "║       Max Iterations Reached           ║".yellow());
            println!("{}", "╚════════════════════════════════════════╝".yellow());
            println!();
            println!("  {} {} iterations", "Ran:".bold(), result.iterations);
            println!();
            println!(
                "  Consider increasing {} or checking progress.",
                "max_iterations".cyan()
            );
        }
        "stopped" => {
            println!("{}", "╔════════════════════════════════════════╗".yellow());
            println!("{}", "║           Loop Stopped                 ║".yellow());
            println!("{}", "╚════════════════════════════════════════╝".yellow());
            println!();
            println!("  {} {} iterations", "Ran:".bold(), result.iterations);
            if let Some(ref reason) = result.error {
                println!("  {} {}", "Reason:".bold(), reason);
            }
        }
        _ => {
            println!("{}", "╔════════════════════════════════════════╗".red());
            println!("{}", "║              Error                     ║".red());
            println!("{}", "╚════════════════════════════════════════╝".red());
            println!();
            println!("  {} {} iterations", "Ran:".bold(), result.iterations);
            if let Some(ref error) = result.error {
                println!("  {} {}", "Error:".bold(), error);
            }
        }
    }
    println!();

    Ok(())
}

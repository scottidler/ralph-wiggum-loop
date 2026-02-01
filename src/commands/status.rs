use crate::cli::Cli;
use crate::config::Config;
use crate::git::GitManager;
use crate::progress::ProgressTracker;
use colored::*;
use eyre::Result;
use std::path::Path;

pub fn run(_cli: &Cli) -> Result<()> {
    let work_dir = Path::new(".");

    // 1. Check if .rwl/ exists
    let rwl_dir = Config::local_config_dir(work_dir);
    if !rwl_dir.exists() {
        println!("{} Not initialized. Run {} first.", "⚠".yellow(), "rwl init".cyan());
        return Ok(());
    }

    println!();
    println!("{}", "╔════════════════════════════════════════╗".cyan());
    println!("{}", "║         RWL Status                     ║".cyan());
    println!("{}", "╚════════════════════════════════════════╝".cyan());
    println!();

    // 2. Read config
    let config = Config::load_local(work_dir)?;

    println!("{}", "Configuration:".bold());
    println!("  Model: {}", config.llm.model.cyan());
    println!("  Max iterations: {}", config.loop_config.max_iterations);
    println!("  Validation: {}", config.validation.command.dimmed());
    println!("  Quality gates: {}", config.quality_gates.len());
    println!();

    // 3. Read progress
    let progress_path = rwl_dir.join("progress.txt");
    let tracker = ProgressTracker::new(&progress_path);

    if !tracker.exists() {
        println!("{}", "Progress:".bold());
        println!(
            "  {} No progress yet. Run {} to start.",
            "·".dimmed(),
            "rwl run --plan <path>".cyan()
        );
        println!();
        return Ok(());
    }

    let progress = tracker.read()?;
    let iteration_count = tracker.iteration_count()?;

    println!("{}", "Progress:".bold());
    if let Some(started) = progress.started {
        println!("  Started: {}", started.format("%Y-%m-%d %H:%M:%S UTC"));
    }
    if let Some(plan_path) = &progress.plan_path {
        println!("  Plan: {}", plan_path.cyan());
    }
    println!("  Iterations: {}", iteration_count.to_string().bold());

    if let Some(status) = &progress.last_status {
        println!("  Status: {}", status);
    }
    println!();

    // 4. Show recent progress entries (last 5 lines of meaningful content)
    let content = tracker.raw_content()?;
    let recent_lines: Vec<&str> = content
        .lines()
        .filter(|l| l.starts_with("##") || l.starts_with("Validation:") || l.starts_with("Promise:"))
        .collect();

    if !recent_lines.is_empty() {
        println!("{}", "Recent Activity:".bold());
        for line in recent_lines.iter().rev().take(10).rev() {
            if line.starts_with("##") {
                println!("  {}", line.cyan());
            } else if line.contains("PASSED") {
                println!("    {}", line.green());
            } else if line.contains("FAILED") {
                println!("    {}", line.red());
            } else {
                println!("    {}", line.dimmed());
            }
        }
        println!();
    }

    // 5. Git status summary
    let git = GitManager::new(work_dir);
    if git.is_repo() {
        println!("{}", "Git Status:".bold());
        if git.has_changes()? {
            println!("  {} Uncommitted changes", "⚠".yellow());
        } else {
            println!("  {} Clean", "✓".green());
        }

        let recent = git.recent_commits(5)?;
        if !recent.is_empty() {
            println!();
            println!("{}", "Recent Commits:".bold());
            for commit in recent {
                if commit.contains("rwl:") {
                    println!("  {}", commit.cyan());
                } else {
                    println!("  {}", commit.dimmed());
                }
            }
        }
        println!();
    }

    Ok(())
}

use crate::cli::{Cli, RunArgs};
use crate::config::Config;
use crate::git::{GitManager, reposlug};
use crate::progress::ProgressTracker;
use crate::result::RunResult;
use crate::runner::LoopRunner;
use crate::safety::{Workdir, resolve_workdir};
use colored::*;
use eyre::{Context, Result};
use log::{debug, warn};
use std::path::{Path, PathBuf};

pub fn run(_cli: &Cli, args: &RunArgs) -> Result<RunResult> {
    let cwd = Path::new(".");
    debug!(
        "run: plan={} isolation_override={:?} unsafe_opt={}",
        args.plan.display(),
        args.isolation,
        args.unsafe_opt
    );

    // 1. Ensure .rwl/ exists in the user's working tree
    ensure_initialized(cwd)?;

    // 2. Load config, apply CLI overrides
    let mut config = Config::load_local(cwd).context("Failed to load local config")?;

    if let Some(max) = args.max_iterations {
        config.loop_config.max_iterations = max;
    }
    if let Some(ref model) = args.model {
        config.llm.model = model.clone();
    }
    if let Some(timeout) = args.timeout {
        config.loop_config.iteration_timeout_minutes = timeout;
    }
    if let Some(isolation) = args.isolation {
        config.safety.isolation = isolation.into();
    }

    // 3. Validate plan file exists, canonicalize to an absolute path so it
    //    resolves regardless of work_dir (the worktree may not contain it).
    ensure_plan_exists(&args.plan)?;
    let plan_path = args
        .plan
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize plan path: {}", args.plan.display()))?;
    debug!("run: canonicalized plan path -> {}", plan_path.display());

    // 4. Create session directory
    let session_dir = create_session_dir(cwd, args.session_path.as_ref())?;
    let timestamp = session_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());

    // 5. Containment preflight: resolve the work_dir (worktree or CWD), enforcing
    //    the fail-closed permission check. A refusal propagates to exit 4.
    let Workdir { path: work_dir, branch } = resolve_workdir(
        cwd,
        &config.safety,
        config.llm.dangerously_skip_permissions,
        args.unsafe_opt,
        &session_dir,
        &plan_path,
        &timestamp,
    )?;

    // 5b. Isolation on + auto-commit off is a stranding hazard: work happens in
    //     the throwaway worktree under /tmp, but with auto-commit disabled nothing
    //     lands on the review branch, so the worktree's commits never reach the
    //     user. Warn (per the design's risk table) recommending auto-commit stay on.
    if branch.is_some() && !config.git.auto_commit {
        warn!(
            "run: isolation produced a worktree but git.auto-commit is off — \
             agent work will be stranded in the throwaway worktree with no commits \
             on the review branch; keep auto-commit on so isolated work is preserved"
        );
    }

    // 6. Write the config (with CLI overrides) into the resolved work_dir, then
    //    make the baseline `rwl: session setup` commit so the protected-path
    //    guard reverts only agent deltas against HEAD.
    config.save_local(&work_dir)?;
    baseline_commit(&work_dir, branch.is_some())?;

    // 7. Initialize progress tracker in session directory (always fresh)
    let progress_path = session_dir.join("progress.txt");
    let progress = ProgressTracker::new(&progress_path);
    progress.init(&plan_path)?;

    // 8. Print startup banner
    print_banner(&config, &plan_path, &session_dir, branch.as_deref())?;

    // 9. Run the loop
    let mut runner = LoopRunner::new(&work_dir, plan_path, session_dir.clone(), branch.clone())?;
    let result = runner.run()?;

    // 10. Write result.json to session directory
    result.write_json()?;

    // 11. Print result
    print_outcome(&result)?;

    // 12. Print session path and (if isolated) the review branch
    println!("session: {}", session_dir.display());
    if let Some(ref branch) = branch {
        println!("branch: {}", branch);
    }

    Ok(result)
}

/// Make the baseline `rwl: session setup` commit in the work_dir so the
/// protected-path guard (Phase 2) reverts only agent deltas against HEAD.
///
/// Only meaningful when isolation produced a worktree; in `isolation: none`
/// committing `rwl`'s setup would pollute the user's working tree, so it is
/// skipped there.
fn baseline_commit(work_dir: &Path, isolated: bool) -> Result<()> {
    debug!("baseline_commit: work_dir={} isolated={}", work_dir.display(), isolated);
    if !isolated {
        debug!("baseline_commit: not isolated -> skipping baseline commit");
        return Ok(());
    }

    let git = GitManager::new(work_dir);
    if !git.is_repo() {
        debug!("baseline_commit: work_dir is not a git repo -> skipping");
        return Ok(());
    }
    if !git.has_changes()? {
        debug!("baseline_commit: no changes to commit");
        return Ok(());
    }
    git.auto_commit("rwl: session setup")?;
    Ok(())
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

fn print_banner(config: &Config, plan_path: &Path, session_dir: &Path, branch: Option<&str>) -> Result<()> {
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
    println!("  {} {:?}", "Isolation:".bold(), config.safety.isolation);
    if let Some(branch) = branch {
        println!("  {} {}", "Branch:".bold(), branch);
    }
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
        "budget-exceeded" => {
            println!("{}", "╔════════════════════════════════════════╗".yellow());
            println!("{}", "║          Budget Exceeded               ║".yellow());
            println!("{}", "╚════════════════════════════════════════╝".yellow());
            println!();
            println!("  {} {} iterations", "Ran:".bold(), result.iterations);
            if let Some(ref reason) = result.error {
                println!("  {} {}", "Reason:".bold(), reason);
            }
            println!();
            println!(
                "  The wall-clock cap ({}) was reached. Adjust it to run longer.",
                "max-total-minutes".cyan()
            );
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

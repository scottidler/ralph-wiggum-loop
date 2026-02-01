use crate::config::Config;
use crate::git::GitManager;
use crate::progress::{IterationResult, ProgressTracker};
use crate::templates::PROMPT_TEMPLATE;
use crate::validation::ValidationRunner;
use colored::*;
use eyre::{Context, Result};
use handlebars::Handlebars;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Outcome of the loop execution
#[derive(Debug)]
pub enum LoopOutcome {
    Complete {
        iterations: u32,
    },
    MaxIterations {
        iterations: u32,
    },
    #[allow(dead_code)]
    Stopped {
        iterations: u32,
        reason: String,
    },
    Error {
        iterations: u32,
        error: String,
    },
}

pub struct LoopRunner {
    work_dir: PathBuf,
    plan_path: PathBuf,
    progress_path: PathBuf,
    config_path: PathBuf,
}

impl LoopRunner {
    pub fn new(work_dir: &Path, plan_path: PathBuf) -> Result<Self> {
        let rwl_dir = Config::local_config_dir(work_dir);

        Ok(Self {
            work_dir: work_dir.to_path_buf(),
            plan_path,
            progress_path: rwl_dir.join("progress.txt"),
            config_path: Config::local_config_path(work_dir),
        })
    }

    pub fn run(&self) -> Result<LoopOutcome> {
        // Load initial config
        let mut config = Config::load(Some(&self.config_path))?;

        // Initialize progress tracker
        let progress = ProgressTracker::new(&self.progress_path);

        // Get starting iteration (resume support)
        let start_iteration = progress.iteration_count()? + 1;

        // Create progress bar
        let pb = ProgressBar::new(config.loop_config.max_iterations as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} iterations ({msg})")
                .unwrap()
                .progress_chars("#>-"),
        );
        pb.set_position((start_iteration - 1) as u64);

        for iteration in start_iteration..=config.loop_config.max_iterations {
            pb.set_message(format!("iteration {}", iteration));
            pb.set_position((iteration - 1) as u64);

            // 1. Re-read config (live editing support)
            config = Config::load(Some(&self.config_path)).unwrap_or(config.clone());

            // 2. Build prompt
            let prompt = self.build_prompt(&config)?;

            // 3. Run Claude with timeout
            println!();
            println!("{} Running iteration {}...", "→".cyan(), iteration.to_string().bold());

            let output = match self.run_claude(&prompt, &config) {
                Ok(output) => output,
                Err(e) => {
                    pb.finish_with_message("error");
                    return Ok(LoopOutcome::Error {
                        iterations: iteration - 1,
                        error: e.to_string(),
                    });
                }
            };

            // 4. Auto-commit changes if enabled
            if config.git.auto_commit {
                self.git_auto_commit(iteration, &config)?;
            }

            // 5. Run validation
            let validation_result = self.run_validation(&config)?;
            let validation_passed = validation_result.passed;

            // 6. Check for completion promise
            let promise_found = self.find_promise(&output, &config);

            // 7. Log progress
            let result = IterationResult {
                iteration,
                validation_passed,
                promise_found,
                summary: if validation_passed && promise_found {
                    "Complete".to_string()
                } else if validation_passed {
                    "Validation passed, waiting for completion".to_string()
                } else {
                    "Validation failed".to_string()
                },
            };
            progress.log_iteration(&result)?;

            // Print status
            self.print_iteration_status(&result);

            // 8. Check exit conditions
            if validation_passed && promise_found {
                println!();
                println!("{} Validation passed and completion promise found!", "✓".green());
                println!("{} Running quality gates...", "→".cyan());

                // Run quality gates as final check
                let validation_runner = ValidationRunner::new(&self.work_dir);
                let gate_result = validation_runner.run_quality_gates(&config.quality_gates)?;

                validation_runner.print_quality_gate_results(&gate_result);

                if gate_result.all_passed {
                    pb.finish_with_message("complete");
                    return Ok(LoopOutcome::Complete { iterations: iteration });
                } else {
                    println!("{} Quality gates failed, continuing loop...", "⚠".yellow());
                }
            }

            // 9. Sleep before next iteration
            if iteration < config.loop_config.max_iterations {
                std::thread::sleep(Duration::from_secs(config.loop_config.sleep_between_secs));
            }
        }

        pb.finish_with_message("max iterations reached");
        Ok(LoopOutcome::MaxIterations {
            iterations: config.loop_config.max_iterations,
        })
    }

    /// Build the prompt for Claude
    fn build_prompt(&self, config: &Config) -> Result<String> {
        let mut handlebars = Handlebars::new();

        // Register the template
        handlebars
            .register_template_string("prompt", PROMPT_TEMPLATE)
            .context("Failed to register prompt template")?;

        // Build template data
        let mut data = HashMap::new();
        data.insert(
            "completion_signal".to_string(),
            config.loop_config.completion_signal.clone(),
        );
        data.insert("plan_path".to_string(), self.plan_path.display().to_string());

        // Render the template
        let prompt = handlebars
            .render("prompt", &data)
            .context("Failed to render prompt template")?;

        Ok(prompt)
    }

    /// Run Claude CLI with the given prompt
    fn run_claude(&self, prompt: &str, config: &Config) -> Result<String> {
        // Check if claude binary exists
        which::which("claude")
            .context("claude CLI not found. Please install it from https://github.com/anthropics/claude-code")?;

        let _timeout_secs = config.loop_config.iteration_timeout_minutes * 60;

        let mut cmd = Command::new("claude");
        cmd.arg("--print")
            .arg("--model")
            .arg(&config.llm.model)
            .arg("--max-turns")
            .arg("1");

        if config.llm.dangerously_skip_permissions {
            cmd.arg("--dangerously-skip-permissions");
        }

        cmd.arg(prompt)
            .current_dir(&self.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd.output().context("Failed to execute claude command")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            // Print stderr for debugging
            if !stderr.is_empty() {
                eprintln!("{}", stderr.dimmed());
            }
        }

        // Return combined output for promise detection
        Ok(format!("{}\n{}", stdout, stderr))
    }

    /// Check for completion promise in output
    fn find_promise(&self, output: &str, config: &Config) -> bool {
        output.contains(&config.loop_config.completion_signal)
    }

    /// Run validation command
    fn run_validation(&self, config: &Config) -> Result<crate::validation::ValidationResult> {
        let runner = ValidationRunner::new(&self.work_dir);
        runner.run_validation(&config.validation.command)
    }

    /// Auto-commit changes
    fn git_auto_commit(&self, iteration: u32, config: &Config) -> Result<()> {
        let git = GitManager::new(&self.work_dir);

        if !git.is_repo() {
            return Ok(());
        }

        if !git.has_changes()? {
            return Ok(());
        }

        let message = config
            .git
            .commit_message_template
            .replace("{iteration}", &iteration.to_string());

        git.auto_commit(&message)?;
        println!("{} Committed changes: {}", "✓".green(), message.dimmed());

        Ok(())
    }

    /// Print iteration status
    fn print_iteration_status(&self, result: &IterationResult) {
        let validation_status = if result.validation_passed { "✓".green() } else { "✗".red() };

        let promise_status = if result.promise_found { "✓".green() } else { "-".dimmed() };

        println!(
            "  Validation: {}  Promise: {}  {}",
            validation_status,
            promise_status,
            result.summary.dimmed()
        );
    }
}

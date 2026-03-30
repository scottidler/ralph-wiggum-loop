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
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Outcome of the loop execution
#[derive(Debug)]
pub enum LoopOutcome {
    Complete { iterations: u32 },
    MaxIterations { iterations: u32 },
    Stopped { iterations: u32, reason: String },
    Error { iterations: u32, error: String },
}

impl LoopOutcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            LoopOutcome::Complete { .. } => 0,
            LoopOutcome::MaxIterations { .. } => 1,
            LoopOutcome::Stopped { .. } => 2,
            LoopOutcome::Error { .. } => 3,
        }
    }
}

pub struct LoopRunner {
    work_dir: PathBuf,
    plan_path: PathBuf,
    progress_path: PathBuf,
    config_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
}

impl LoopRunner {
    pub fn new(work_dir: &Path, plan_path: PathBuf) -> Result<Self> {
        let rwl_dir = Config::local_config_dir(work_dir);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = stop_flag.clone();
        ctrlc::set_handler(move || {
            eprintln!("\n{} Received Ctrl-C, finishing current iteration...", "⚠".yellow());
            flag_clone.store(true, Ordering::SeqCst);
        })
        .context("Failed to set Ctrl-C handler")?;

        Ok(Self {
            work_dir: work_dir.to_path_buf(),
            plan_path,
            progress_path: rwl_dir.join("progress.txt"),
            config_path: Config::local_config_path(work_dir),
            stop_flag,
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
            // 0. Check for stop signal (Ctrl-C)
            if self.stop_flag.load(Ordering::SeqCst) {
                pb.finish_with_message("stopped");
                // Auto-commit WIP before exiting
                if config.git.auto_commit {
                    let _ = self.git_auto_commit(iteration, &config);
                }
                return Ok(LoopOutcome::Stopped {
                    iterations: iteration - 1,
                    reason: "Received Ctrl-C".to_string(),
                });
            }

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
            let validation_runner = ValidationRunner::new(&self.work_dir);
            let validation_result = validation_runner.run_validation(&config.validation.command)?;
            let validation_passed = validation_result.passed;
            validation_runner.print_validation_result(&validation_result);

            // 6. Check for completion promise
            let promise_found = self.find_promise(&output, &config);

            // 7. Log progress (including validation errors for feedback)
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
                validation_output: if validation_passed { String::new() } else { validation_result.output.clone() },
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

    /// Build the prompt for Claude, injecting accumulated progress/feedback
    fn build_prompt(&self, config: &Config) -> Result<String> {
        let mut handlebars = Handlebars::new();

        // Register the template
        handlebars
            .register_template_string("prompt", PROMPT_TEMPLATE)
            .context("Failed to register prompt template")?;

        // Read progress content to inject into prompt
        let progress_content = if self.progress_path.exists() {
            std::fs::read_to_string(&self.progress_path).unwrap_or_default()
        } else {
            String::new()
        };

        // Build template data
        let mut data = HashMap::new();
        data.insert(
            "completion_signal".to_string(),
            config.loop_config.completion_signal.clone(),
        );
        data.insert("plan_path".to_string(), self.plan_path.display().to_string());
        if !progress_content.trim().is_empty() {
            data.insert("progress".to_string(), progress_content);
        }

        // Render the template
        let prompt = handlebars
            .render("prompt", &data)
            .context("Failed to render prompt template")?;

        Ok(prompt)
    }

    /// Run Claude CLI with the given prompt, streaming output and enforcing timeout
    fn run_claude(&self, prompt: &str, config: &Config) -> Result<String> {
        // Check if claude binary exists
        which::which("claude")
            .context("claude CLI not found. Please install it from https://github.com/anthropics/claude-code")?;

        let timeout = Duration::from_secs((config.loop_config.iteration_timeout_minutes * 60) as u64);

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

        let mut child = cmd.spawn().context("Failed to spawn claude command")?;

        // Stream stdout in a background thread while capturing it
        let child_stdout = child.stdout.take();
        let stdout_handle = std::thread::spawn(move || {
            let mut captured = String::new();
            if let Some(stdout) = child_stdout {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            println!("  {}", line.dimmed());
                            captured.push_str(&line);
                            captured.push('\n');
                        }
                        Err(_) => break,
                    }
                }
            }
            captured
        });

        // Stream stderr in a background thread while capturing it
        let child_stderr = child.stderr.take();
        let stderr_handle = std::thread::spawn(move || {
            let mut captured = String::new();
            if let Some(stderr) = child_stderr {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            eprintln!("  {}", line.dimmed());
                            captured.push_str(&line);
                            captured.push('\n');
                        }
                        Err(_) => break,
                    }
                }
            }
            captured
        });

        // Wait with timeout
        let start = std::time::Instant::now();
        loop {
            match child.try_wait().context("Failed to check claude process status")? {
                Some(_status) => {
                    let stdout = stdout_handle.join().unwrap_or_default();
                    let stderr = stderr_handle.join().unwrap_or_default();
                    return Ok(format!("{}\n{}", stdout, stderr));
                }
                None => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(eyre::eyre!(
                            "Claude timed out after {} minutes",
                            config.loop_config.iteration_timeout_minutes
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    /// Check for completion promise in output
    fn find_promise(&self, output: &str, config: &Config) -> bool {
        output.contains(&config.loop_config.completion_signal)
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

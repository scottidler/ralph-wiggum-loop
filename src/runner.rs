use crate::config::Config;
use crate::git::GitManager;
use crate::progress::{IterationResult, ProgressTracker};
use crate::result::RunResult;
use crate::session::SessionLog;
use crate::templates::PROMPT_TEMPLATE;
use crate::validation::ValidationRunner;
use chrono::{DateTime, Utc};
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

    pub fn outcome_name(&self) -> &str {
        match self {
            LoopOutcome::Complete { .. } => "complete",
            LoopOutcome::MaxIterations { .. } => "max-iterations",
            LoopOutcome::Stopped { .. } => "stopped",
            LoopOutcome::Error { .. } => "error",
        }
    }

    pub fn iterations(&self) -> u32 {
        match self {
            LoopOutcome::Complete { iterations }
            | LoopOutcome::MaxIterations { iterations }
            | LoopOutcome::Stopped { iterations, .. }
            | LoopOutcome::Error { iterations, .. } => *iterations,
        }
    }

    pub fn error_message(&self) -> Option<String> {
        match self {
            LoopOutcome::Error { error, .. } => Some(error.clone()),
            LoopOutcome::Stopped { reason, .. } => Some(reason.clone()),
            _ => None,
        }
    }
}

pub struct LoopRunner {
    work_dir: PathBuf,
    plan_path: PathBuf,
    progress_path: PathBuf,
    config_path: PathBuf,
    session_dir: PathBuf,
    stop_flag: Arc<AtomicBool>,
    session: SessionLog,
}

impl LoopRunner {
    pub fn new(work_dir: &Path, plan_path: PathBuf, session_dir: PathBuf) -> Result<Self> {
        let session = SessionLog::new(&session_dir)?;
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
            progress_path: session_dir.join("progress.txt"),
            config_path: Config::local_config_path(work_dir),
            session_dir,
            stop_flag,
            session,
        })
    }

    pub fn run(&mut self) -> Result<RunResult> {
        let started = Utc::now();
        let mut last_validation_passed = false;
        let last_gates_passed = false;

        // Load initial config
        let mut config = Config::load(Some(&self.config_path))?;

        // Initialize progress tracker
        let progress = ProgressTracker::new(&self.progress_path);

        // Create progress bar
        let pb = ProgressBar::new(config.loop_config.max_iterations as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} iterations ({msg})")
                .unwrap()
                .progress_chars("#>-"),
        );

        self.session.log(&format!(
            "=== RWL session started at {} ===",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ))?;

        for iteration in 1..=config.loop_config.max_iterations {
            // 0. Check for stop signal (Ctrl-C)
            if self.stop_flag.load(Ordering::SeqCst) {
                pb.finish_with_message("stopped");
                self.session.println(&format!(
                    "{} Stopped by user (Ctrl-C) after {} iterations",
                    "⚠".yellow(),
                    iteration - 1
                ))?;
                // Auto-commit WIP before exiting
                if config.git.auto_commit {
                    let _ = self.git_auto_commit(iteration, &config);
                }
                let outcome = LoopOutcome::Stopped {
                    iterations: iteration - 1,
                    reason: "Received Ctrl-C".to_string(),
                };
                return Ok(self.build_result(&outcome, started, last_validation_passed, last_gates_passed));
            }

            pb.set_message(format!("iteration {}", iteration));
            pb.set_position((iteration - 1) as u64);

            // 1. Re-read config (live editing support)
            config = Config::load(Some(&self.config_path)).unwrap_or(config.clone());

            // 2. Build prompt
            let prompt = self.build_prompt(&config)?;

            // 3. Run Claude with timeout
            self.session.println("")?;
            self.session.println(&format!(
                "{} Running iteration {}...",
                "→".cyan(),
                iteration.to_string().bold()
            ))?;
            self.session.log(&format!(
                "--- iteration {} started at {} ---",
                iteration,
                Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
            ))?;

            let output = match self.run_claude(&prompt, &config) {
                Ok(output) => output,
                Err(e) => {
                    pb.finish_with_message("error");
                    self.session.log(&format!("ERROR: {}", e))?;
                    let outcome = LoopOutcome::Error {
                        iterations: iteration - 1,
                        error: e.to_string(),
                    };
                    return Ok(self.build_result(&outcome, started, last_validation_passed, last_gates_passed));
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
            last_validation_passed = validation_passed;
            validation_runner.print_validation_result(&validation_result);

            // Log validation to session
            self.session.log(&format!(
                "Validation: {} (exit code: {})",
                if validation_passed { "PASSED" } else { "FAILED" },
                validation_result.exit_code
            ))?;
            if !validation_passed && !validation_result.output.trim().is_empty() {
                self.session.log(&validation_result.output)?;
            }

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

            // Print and log status
            self.print_iteration_status(&result)?;

            self.session.log(&format!(
                "--- iteration {} completed at {} ---",
                iteration,
                Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
            ))?;

            // 8. Check exit conditions
            if validation_passed && promise_found {
                self.session.println("")?;
                self.session.println(&format!(
                    "{} Validation passed and completion promise found!",
                    "✓".green()
                ))?;
                self.session
                    .println(&format!("{} Running quality gates...", "→".cyan()))?;

                // Run quality gates as final check
                let validation_runner = ValidationRunner::new(&self.work_dir);
                let gate_result = validation_runner.run_quality_gates(&config.quality_gates)?;

                validation_runner.print_quality_gate_results(&gate_result);

                // Log quality gates to session
                self.session.log("Quality Gates:")?;
                for (name, passed, output) in &gate_result.results {
                    self.session
                        .log(&format!("  {} {}", if *passed { "PASS" } else { "FAIL" }, name))?;
                    if !passed && !output.trim().is_empty() {
                        self.session.log(output)?;
                    }
                }

                if gate_result.all_passed {
                    pb.finish_with_message("complete");
                    self.session.log("=== Loop complete ===")?;
                    let outcome = LoopOutcome::Complete { iterations: iteration };
                    return Ok(self.build_result(&outcome, started, last_validation_passed, true));
                } else {
                    self.session
                        .println(&format!("{} Quality gates failed, continuing loop...", "⚠".yellow()))?;
                }
            }

            // 9. Sleep before next iteration
            if iteration < config.loop_config.max_iterations {
                std::thread::sleep(Duration::from_secs(config.loop_config.sleep_between_secs));
            }
        }

        pb.finish_with_message("max iterations reached");
        self.session.log("=== Max iterations reached ===")?;
        let outcome = LoopOutcome::MaxIterations {
            iterations: config.loop_config.max_iterations,
        };
        Ok(self.build_result(&outcome, started, last_validation_passed, last_gates_passed))
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
    fn run_claude(&mut self, prompt: &str, config: &Config) -> Result<String> {
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

                    // Log Claude output to session
                    self.session.log("--- claude output ---")?;
                    self.session.log(&stdout)?;
                    if !stderr.trim().is_empty() {
                        self.session.log("--- claude stderr ---")?;
                        self.session.log(&stderr)?;
                    }

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
    fn git_auto_commit(&mut self, iteration: u32, config: &Config) -> Result<()> {
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
        self.session
            .println(&format!("{} Committed changes: {}", "✓".green(), message.dimmed()))?;

        Ok(())
    }

    fn build_result(
        &self,
        outcome: &LoopOutcome,
        started: DateTime<Utc>,
        validation_passed: bool,
        gates_passed: bool,
    ) -> RunResult {
        let finished = Utc::now();
        let duration = finished.signed_duration_since(started);
        RunResult {
            outcome: outcome.outcome_name().to_string(),
            exit_code: outcome.exit_code(),
            iterations: outcome.iterations(),
            plan: self.plan_path.display().to_string(),
            started: started.to_rfc3339(),
            finished: finished.to_rfc3339(),
            duration_secs: duration.num_seconds().max(0) as u64,
            error: outcome.error_message(),
            validation_passed,
            quality_gates_passed: gates_passed,
            session_dir: self.session_dir.clone(),
        }
    }

    /// Print iteration status
    fn print_iteration_status(&mut self, result: &IterationResult) -> Result<()> {
        let validation_status = if result.validation_passed { "✓".green() } else { "✗".red() };
        let promise_status = if result.promise_found { "✓".green() } else { "-".dimmed() };

        self.session.println(&format!(
            "  Validation: {}  Promise: {}  {}",
            validation_status,
            promise_status,
            result.summary.dimmed()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_code_complete() {
        let outcome = LoopOutcome::Complete { iterations: 1 };
        assert_eq!(outcome.exit_code(), 0);
    }

    #[test]
    fn test_exit_code_max_iterations() {
        let outcome = LoopOutcome::MaxIterations { iterations: 10 };
        assert_eq!(outcome.exit_code(), 1);
    }

    #[test]
    fn test_exit_code_stopped() {
        let outcome = LoopOutcome::Stopped {
            iterations: 5,
            reason: "Ctrl-C".to_string(),
        };
        assert_eq!(outcome.exit_code(), 2);
    }

    #[test]
    fn test_exit_code_error() {
        let outcome = LoopOutcome::Error {
            iterations: 3,
            error: "timeout".to_string(),
        };
        assert_eq!(outcome.exit_code(), 3);
    }

    #[test]
    fn test_outcome_name() {
        assert_eq!(LoopOutcome::Complete { iterations: 1 }.outcome_name(), "complete");
        assert_eq!(
            LoopOutcome::MaxIterations { iterations: 1 }.outcome_name(),
            "max-iterations"
        );
        assert_eq!(
            LoopOutcome::Stopped {
                iterations: 1,
                reason: "x".to_string()
            }
            .outcome_name(),
            "stopped"
        );
        assert_eq!(
            LoopOutcome::Error {
                iterations: 1,
                error: "x".to_string()
            }
            .outcome_name(),
            "error"
        );
    }

    #[test]
    fn test_iterations() {
        assert_eq!(LoopOutcome::Complete { iterations: 7 }.iterations(), 7);
        assert_eq!(LoopOutcome::MaxIterations { iterations: 100 }.iterations(), 100);
        assert_eq!(
            LoopOutcome::Stopped {
                iterations: 3,
                reason: "x".to_string()
            }
            .iterations(),
            3
        );
        assert_eq!(
            LoopOutcome::Error {
                iterations: 0,
                error: "x".to_string()
            }
            .iterations(),
            0
        );
    }

    #[test]
    fn test_error_message() {
        assert!(LoopOutcome::Complete { iterations: 1 }.error_message().is_none());
        assert!(LoopOutcome::MaxIterations { iterations: 1 }.error_message().is_none());
        assert_eq!(
            LoopOutcome::Stopped {
                iterations: 1,
                reason: "interrupted".to_string()
            }
            .error_message()
            .as_deref(),
            Some("interrupted")
        );
        assert_eq!(
            LoopOutcome::Error {
                iterations: 1,
                error: "timeout".to_string()
            }
            .error_message()
            .as_deref(),
            Some("timeout")
        );
    }
}

use crate::config::QualityGate;
use colored::*;
use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ValidationRunner {
    work_dir: PathBuf,
}

#[derive(Debug)]
pub struct ValidationResult {
    pub passed: bool,
    #[allow(dead_code)]
    pub output: String,
    #[allow(dead_code)]
    pub exit_code: i32,
}

#[derive(Debug)]
pub struct QualityGateResult {
    pub all_passed: bool,
    pub results: Vec<(String, bool, String)>, // (name, passed, output)
}

impl ValidationRunner {
    pub fn new(work_dir: &Path) -> Self {
        Self {
            work_dir: work_dir.to_path_buf(),
        }
    }

    /// Run the main validation command (e.g., otto ci)
    pub fn run_validation(&self, command: &str) -> Result<ValidationResult> {
        let output = Command::new("sh")
            .args(["-c", command])
            .current_dir(&self.work_dir)
            .output()
            .context(format!("Failed to run validation command: {}", command))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}\n{}", stdout, stderr);

        Ok(ValidationResult {
            passed: output.status.success(),
            output: combined,
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    /// Run all quality gates, return first failure or success
    pub fn run_quality_gates(&self, gates: &[QualityGate]) -> Result<QualityGateResult> {
        let mut results = Vec::new();
        let mut all_passed = true;

        for gate in gates {
            let command = gate.get_command()?;
            let result = self.run_gate_command(&command)?;

            let passed = result.passed;
            results.push((gate.name.clone(), passed, result.output));

            if !passed {
                all_passed = false;
            }
        }

        Ok(QualityGateResult { all_passed, results })
    }

    /// Run a single gate command
    fn run_gate_command(&self, command: &str) -> Result<ValidationResult> {
        let output = Command::new("sh")
            .args(["-c", command])
            .current_dir(&self.work_dir)
            .output()
            .context(format!("Failed to run quality gate command: {}", command))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}\n{}", stdout, stderr);

        Ok(ValidationResult {
            passed: output.status.success(),
            output: combined,
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    /// Print validation result with colors
    #[allow(dead_code)]
    pub fn print_validation_result(&self, result: &ValidationResult) {
        if result.passed {
            println!("{} Validation passed", "✓".green());
        } else {
            println!("{} Validation failed (exit code: {})", "✗".red(), result.exit_code);
            if !result.output.trim().is_empty() {
                println!("{}", result.output.dimmed());
            }
        }
    }

    /// Print quality gate results with colors
    pub fn print_quality_gate_results(&self, result: &QualityGateResult) {
        println!();
        println!("{}", "Quality Gates:".bold());
        for (name, passed, output) in &result.results {
            if *passed {
                println!("  {} {}", "✓".green(), name);
            } else {
                println!("  {} {}", "✗".red(), name);
                if !output.trim().is_empty() {
                    for line in output.lines().take(5) {
                        println!("    {}", line.dimmed());
                    }
                }
            }
        }
        println!();
        if result.all_passed {
            println!("{}", "All quality gates passed!".green().bold());
        } else {
            println!("{}", "Some quality gates failed.".red().bold());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_run_validation_success() {
        let dir = tempdir().unwrap();
        let runner = ValidationRunner::new(dir.path());

        let result = runner.run_validation("true").unwrap();
        assert!(result.passed);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn test_run_validation_failure() {
        let dir = tempdir().unwrap();
        let runner = ValidationRunner::new(dir.path());

        let result = runner.run_validation("false").unwrap();
        assert!(!result.passed);
        assert_eq!(result.exit_code, 1);
    }

    #[test]
    fn test_run_quality_gates_all_pass() {
        let dir = tempdir().unwrap();
        let runner = ValidationRunner::new(dir.path());

        let gates = vec![
            QualityGate {
                name: "gate1".to_string(),
                command: Some("true".to_string()),
                script: None,
            },
            QualityGate {
                name: "gate2".to_string(),
                command: Some("true".to_string()),
                script: None,
            },
        ];

        let result = runner.run_quality_gates(&gates).unwrap();
        assert!(result.all_passed);
        assert_eq!(result.results.len(), 2);
    }

    #[test]
    fn test_run_quality_gates_some_fail() {
        let dir = tempdir().unwrap();
        let runner = ValidationRunner::new(dir.path());

        let gates = vec![
            QualityGate {
                name: "gate1".to_string(),
                command: Some("true".to_string()),
                script: None,
            },
            QualityGate {
                name: "gate2".to_string(),
                command: Some("false".to_string()),
                script: None,
            },
        ];

        let result = runner.run_quality_gates(&gates).unwrap();
        assert!(!result.all_passed);
        assert!(result.results[0].1); // gate1 passed
        assert!(!result.results[1].1); // gate2 failed
    }
}

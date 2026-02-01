use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct ProgressTracker {
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct IterationResult {
    pub iteration: u32,
    pub validation_passed: bool,
    pub promise_found: bool,
    pub summary: String,
}

#[derive(Debug)]
pub struct Progress {
    pub started: Option<DateTime<Utc>>,
    pub plan_path: Option<String>,
    #[allow(dead_code)]
    pub iterations: Vec<IterationLog>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IterationLog {
    pub iteration: u32,
    pub timestamp: DateTime<Utc>,
    pub validation_passed: bool,
    pub promise_found: bool,
    pub summary: String,
}

impl ProgressTracker {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    /// Initialize progress file with header
    pub fn init(&self, plan_path: &Path) -> Result<()> {
        let header = format!(
            "# RWL Progress Log\n\
             # Started: {}\n\
             # Plan: {}\n\
             # ----------------------------------------\n\n",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
            plan_path.display()
        );

        fs::write(&self.path, header).context("Failed to initialize progress file")?;

        Ok(())
    }

    /// Log an iteration result
    pub fn log_iteration(&self, result: &IterationResult) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .context("Failed to open progress file")?;

        let entry = format!(
            "## Iteration {}\n\
             Timestamp: {}\n\
             Validation: {}\n\
             Promise: {}\n\
             Summary: {}\n\n",
            result.iteration,
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
            if result.validation_passed { "PASSED" } else { "FAILED" },
            if result.promise_found { "FOUND" } else { "NOT FOUND" },
            result.summary
        );

        file.write_all(entry.as_bytes())
            .context("Failed to write to progress file")?;

        Ok(())
    }

    /// Read current progress
    pub fn read(&self) -> Result<Progress> {
        if !self.path.exists() {
            return Ok(Progress {
                started: None,
                plan_path: None,
                iterations: vec![],
                last_status: None,
            });
        }

        let content = fs::read_to_string(&self.path).context("Failed to read progress file")?;

        let mut progress = Progress {
            started: None,
            plan_path: None,
            iterations: vec![],
            last_status: None,
        };

        // Parse header
        for line in content.lines() {
            if line.starts_with("# Started:") {
                let date_str = line.trim_start_matches("# Started:").trim();
                progress.started = DateTime::parse_from_str(
                    &format!("{} +0000", date_str.trim_end_matches(" UTC")),
                    "%Y-%m-%d %H:%M:%S %z",
                )
                .ok()
                .map(|dt| dt.with_timezone(&Utc));
            } else if line.starts_with("# Plan:") {
                progress.plan_path = Some(line.trim_start_matches("# Plan:").trim().to_string());
            }
        }

        // Count iterations (simple count of "## Iteration" lines)
        let iteration_count = content.matches("## Iteration").count() as u32;
        if iteration_count > 0 {
            progress.last_status = Some(format!("{} iterations completed", iteration_count));
        }

        Ok(progress)
    }

    /// Get iteration count
    pub fn iteration_count(&self) -> Result<u32> {
        if !self.path.exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&self.path).context("Failed to read progress file")?;

        Ok(content.matches("## Iteration").count() as u32)
    }

    /// Check if progress file exists
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Get the raw content of the progress file
    pub fn raw_content(&self) -> Result<String> {
        if !self.path.exists() {
            return Ok(String::new());
        }
        fs::read_to_string(&self.path).context("Failed to read progress file")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_init_creates_file() {
        let dir = tempdir().unwrap();
        let progress_path = dir.path().join("progress.txt");
        let tracker = ProgressTracker::new(&progress_path);

        tracker.init(Path::new("test-plan.md")).unwrap();

        assert!(progress_path.exists());
        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("# RWL Progress Log"));
        assert!(content.contains("test-plan.md"));
    }

    #[test]
    fn test_log_iteration() {
        let dir = tempdir().unwrap();
        let progress_path = dir.path().join("progress.txt");
        let tracker = ProgressTracker::new(&progress_path);

        tracker.init(Path::new("test-plan.md")).unwrap();

        let result = IterationResult {
            iteration: 1,
            validation_passed: true,
            promise_found: false,
            summary: "Fixed a bug".to_string(),
        };

        tracker.log_iteration(&result).unwrap();

        let content = fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("## Iteration 1"));
        assert!(content.contains("Validation: PASSED"));
        assert!(content.contains("Promise: NOT FOUND"));
        assert!(content.contains("Fixed a bug"));
    }

    #[test]
    fn test_iteration_count() {
        let dir = tempdir().unwrap();
        let progress_path = dir.path().join("progress.txt");
        let tracker = ProgressTracker::new(&progress_path);

        tracker.init(Path::new("test-plan.md")).unwrap();

        assert_eq!(tracker.iteration_count().unwrap(), 0);

        for i in 1..=3 {
            tracker
                .log_iteration(&IterationResult {
                    iteration: i,
                    validation_passed: true,
                    promise_found: false,
                    summary: format!("Iteration {}", i),
                })
                .unwrap();
        }

        assert_eq!(tracker.iteration_count().unwrap(), 3);
    }
}

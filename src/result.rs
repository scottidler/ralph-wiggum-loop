use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct RunResult {
    pub outcome: String,
    pub exit_code: i32,
    pub iterations: u32,
    pub plan: String,
    pub started: String,
    pub finished: String,
    pub duration_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub validation_passed: bool,
    pub quality_gates_passed: bool,
    #[serde(skip)]
    pub session_dir: PathBuf,
}

impl RunResult {
    pub fn write_json(&self) -> Result<()> {
        let path = self.session_dir.join("result.json");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json).context("Failed to write result.json")?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sample_result(dir: &Path) -> RunResult {
        RunResult {
            outcome: "complete".to_string(),
            exit_code: 0,
            iterations: 3,
            plan: "plan.md".to_string(),
            started: "2026-03-30T10:00:00+00:00".to_string(),
            finished: "2026-03-30T10:05:00+00:00".to_string(),
            duration_secs: 300,
            error: None,
            validation_passed: true,
            quality_gates_passed: true,
            session_dir: dir.to_path_buf(),
        }
    }

    #[test]
    fn test_write_json_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = sample_result(dir.path());
        result.write_json().unwrap();
        assert!(dir.path().join("result.json").exists());
    }

    #[test]
    fn test_write_json_content() {
        let dir = tempfile::tempdir().unwrap();
        let result = sample_result(dir.path());
        result.write_json().unwrap();

        let content = fs::read_to_string(dir.path().join("result.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["outcome"], "complete");
        assert_eq!(parsed["exit_code"], 0);
        assert_eq!(parsed["iterations"], 3);
        assert_eq!(parsed["validation_passed"], true);
        assert_eq!(parsed["quality_gates_passed"], true);
        assert!(parsed.get("session_dir").is_none());
    }

    #[test]
    fn test_serialization_skips_session_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = sample_result(dir.path());
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("session_dir"));
    }

    #[test]
    fn test_error_field_skipped_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let result = sample_result(dir.path());
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_error_field_present_when_some() {
        let dir = tempfile::tempdir().unwrap();
        let mut result = sample_result(dir.path());
        result.error = Some("something failed".to_string());
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("something failed"));
    }

    #[test]
    fn test_roundtrip_serialization() {
        let dir = tempfile::tempdir().unwrap();
        let result = sample_result(dir.path());
        let json = serde_json::to_string_pretty(&result).unwrap();
        let deserialized: RunResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.outcome, result.outcome);
        assert_eq!(deserialized.exit_code, result.exit_code);
        assert_eq!(deserialized.iterations, result.iterations);
        assert_eq!(deserialized.validation_passed, result.validation_passed);
        assert_eq!(deserialized.quality_gates_passed, result.quality_gates_passed);
        assert_eq!(deserialized.duration_secs, result.duration_secs);
    }
}

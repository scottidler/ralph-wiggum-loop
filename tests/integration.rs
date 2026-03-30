use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn rwl_binary() -> String {
    let output = Command::new("cargo")
        .args(["build", "--quiet"])
        .output()
        .expect("Failed to build");
    assert!(
        output.status.success(),
        "Build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut path = std::env::current_dir().unwrap();
    path.push("target/debug/rwl");
    path.display().to_string()
}

fn setup_project(dir: &Path, validation_cmd: &str, max_iterations: u32, completion_signal: &str) {
    let rwl_dir = dir.join(".rwl");
    fs::create_dir_all(&rwl_dir).unwrap();

    let config = format!(
        r#"loop:
  max_iterations: {}
  iteration_timeout_minutes: 1
  sleep_between_secs: 0
  completion_signal: "{}"
validation:
  command: "{}"
quality_gates: []
llm:
  model: "sonnet"
  dangerously_skip_permissions: true
git:
  auto_commit: false
"#,
        max_iterations, completion_signal, validation_cmd
    );
    fs::write(rwl_dir.join("rwl.yml"), config).unwrap();

    fs::write(dir.join("plan.md"), "# Test Plan\nDo nothing.").unwrap();
}

fn create_mock_claude(dir: &Path, output: &str) -> String {
    let bin_dir = dir.join("mock-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("claude");
    fs::write(&script, format!("#!/bin/bash\necho '{}'\n", output)).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }

    bin_dir.display().to_string()
}

fn run_rwl(project_dir: &Path, mock_bin: &str, session_dir: &Path) -> std::process::Output {
    let bin = rwl_binary();
    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", mock_bin, current_path);

    Command::new(&bin)
        .args([
            "run",
            "--plan",
            "plan.md",
            "--session-path",
            &session_dir.display().to_string(),
        ])
        .current_dir(project_dir)
        .env("PATH", new_path)
        .output()
        .expect("Failed to run rwl")
}

#[test]
fn test_complete_exits_0_and_writes_result_json() {
    let project = TempDir::new().unwrap();
    let sessions = TempDir::new().unwrap();
    let signal = "<promise>COMPLETE</promise>";

    setup_project(project.path(), "true", 5, signal);
    let mock_bin = create_mock_claude(project.path(), signal);

    let output = run_rwl(project.path(), &mock_bin, sessions.path());

    assert_eq!(
        output.status.code(),
        Some(0),
        "Expected exit 0, got {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Find the session subdirectory (timestamped)
    let entries: Vec<_> = fs::read_dir(sessions.path()).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(entries.len(), 1, "Expected exactly one session directory");
    let session_dir = entries[0].path();

    // Verify result.json exists and has correct content
    let result_path = session_dir.join("result.json");
    assert!(result_path.exists(), "result.json should exist");

    let content = fs::read_to_string(&result_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["outcome"], "complete");
    assert_eq!(parsed["exit_code"], 0);
    assert_eq!(parsed["validation_passed"], true);
    assert_eq!(parsed["quality_gates_passed"], true);

    // Verify session.log exists
    assert!(session_dir.join("session.log").exists());
    // Verify progress.txt exists
    assert!(session_dir.join("progress.txt").exists());
}

#[test]
fn test_max_iterations_exits_1() {
    let project = TempDir::new().unwrap();
    let sessions = TempDir::new().unwrap();

    setup_project(project.path(), "true", 1, "<promise>COMPLETE</promise>");
    // Mock claude does NOT output the completion signal
    let mock_bin = create_mock_claude(project.path(), "I made some changes but not done yet");

    let output = run_rwl(project.path(), &mock_bin, sessions.path());

    assert_eq!(
        output.status.code(),
        Some(1),
        "Expected exit 1 (max iterations), got {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Find session dir and check result.json
    let entries: Vec<_> = fs::read_dir(sessions.path()).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(entries.len(), 1);
    let session_dir = entries[0].path();

    let content = fs::read_to_string(session_dir.join("result.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["outcome"], "max-iterations");
    assert_eq!(parsed["exit_code"], 1);
    assert_eq!(parsed["iterations"], 1);
}

#[test]
fn test_validation_failure_reflected_in_result() {
    let project = TempDir::new().unwrap();
    let sessions = TempDir::new().unwrap();

    // Validation always fails, max 1 iteration
    setup_project(project.path(), "false", 1, "<promise>COMPLETE</promise>");
    let mock_bin = create_mock_claude(project.path(), "did some work");

    let output = run_rwl(project.path(), &mock_bin, sessions.path());

    assert_eq!(output.status.code(), Some(1));

    let entries: Vec<_> = fs::read_dir(sessions.path()).unwrap().filter_map(|e| e.ok()).collect();
    let session_dir = entries[0].path();

    let content = fs::read_to_string(session_dir.join("result.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["outcome"], "max-iterations");
    assert_eq!(parsed["validation_passed"], false);
}

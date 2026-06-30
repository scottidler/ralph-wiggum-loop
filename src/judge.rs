use crate::config::JudgeConfig;
use crate::runner::signal_on_own_line;
use eyre::{Context, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Judge call timeout: 10 minutes. The judge prompt is typically short and the
/// model fast, but a conservative cap prevents hangs.
const JUDGE_TIMEOUT_SECS: u64 = 600;

/// Extract the judge's explanation from its full output by stripping the signal
/// line. Returns the remaining text trimmed. Used to feed the rejection reason
/// back to `progress.txt` for the next iteration.
pub(crate) fn extract_explanation(output: &str, signal: &str) -> String {
    let signal_trimmed = signal.trim();
    output
        .lines()
        .filter(|line| line.trim() != signal_trimmed)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Detect the judge's binary verdict in its output using a line-exact match.
///
/// Reuses [`signal_on_own_line`] from `runner.rs` (no logic duplication). The
/// signal must appear on its own line so that the model mentioning the token in
/// prose does not falsely pass.
///
/// This is a pure function with no I/O - directly unit-testable.
pub(crate) fn detect_verdict(output: &str, signal: &str) -> bool {
    signal_on_own_line(output, signal)
}

/// Run the LLM-as-judge gate.
///
/// Spawns a fresh `claude --print` invocation with the judge's model and
/// prompt, then detects the verdict with a line-exact signal match. Returns
/// `(passed, output)` where `output` is the full judge stdout+stderr (used by
/// the caller to extract an explanation on FAIL).
///
/// The invocation mirrors `run_claude` in `runner.rs`: `--print`, `--model`,
/// optional `--dangerously-skip-permissions`, `stdin(Stdio::null())`.
///
/// # Logging
/// - DEBUG on entry: model and prompt length (never the full prompt - it can be large).
/// - DEBUG/WARN on verdict outcome.
pub fn run_judge(config: &JudgeConfig, work_dir: &Path, dangerously_skip_permissions: bool) -> Result<(bool, String)> {
    log::debug!(
        "run_judge: model={} prompt_len={} signal={:?} work_dir={}",
        config.model,
        config.prompt.len(),
        config.signal,
        work_dir.display()
    );

    which::which("claude").context("claude CLI not found; required for the judge gate")?;

    let mut cmd = Command::new("claude");
    cmd.arg("--print").arg("--model").arg(&config.model);

    if dangerously_skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }

    cmd.arg(&config.prompt)
        .current_dir(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().context("Failed to spawn claude for judge gate")?;

    // Drain stdout and stderr concurrently to avoid deadlocking on a full pipe buffer.
    let child_stdout = child.stdout.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut captured = String::new();
        if let Some(stdout) = child_stdout {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        captured.push_str(&line);
                        captured.push('\n');
                    }
                    Err(_) => break,
                }
            }
        }
        captured
    });

    let child_stderr = child.stderr.take();
    let stderr_handle = std::thread::spawn(move || {
        let mut captured = String::new();
        if let Some(stderr) = child_stderr {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        captured.push_str(&line);
                        captured.push('\n');
                    }
                    Err(_) => break,
                }
            }
        }
        captured
    });

    // Poll with a wall-clock timeout.
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(JUDGE_TIMEOUT_SECS);
    loop {
        match child.try_wait().context("Failed to check judge process status")? {
            Some(_status) => {
                let stdout = stdout_handle.join().unwrap_or_default();
                let stderr = stderr_handle.join().unwrap_or_default();
                let combined = format!("{}\n{}", stdout, stderr);

                let passed = detect_verdict(&combined, &config.signal);
                if passed {
                    log::debug!("run_judge: verdict=PASS model={}", config.model);
                } else {
                    log::warn!(
                        "run_judge: verdict=FAIL model={} output_len={}",
                        config.model,
                        combined.len()
                    );
                }
                return Ok((passed, combined));
            }
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(eyre::eyre!("Judge timed out after {} seconds", JUDGE_TIMEOUT_SECS));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SIGNAL: &str = "<judge>PASS</judge>";

    // --- detect_verdict tests (pure, no subprocess) ---

    #[test]
    fn test_detect_verdict_pass_on_own_line() {
        let output = "looks good overall\n<judge>PASS</judge>\nsome trailing text\n";
        assert!(detect_verdict(output, SIGNAL));
    }

    #[test]
    fn test_detect_verdict_pass_with_surrounding_whitespace() {
        let output = "analysis complete\n   <judge>PASS</judge>   \n";
        assert!(detect_verdict(output, SIGNAL));
    }

    #[test]
    fn test_detect_verdict_fail_when_signal_in_prose() {
        // Signal mentioned inline - must NOT trigger a PASS.
        let output = "The criteria for <judge>PASS</judge> have not yet been met.";
        assert!(!detect_verdict(output, SIGNAL));
    }

    #[test]
    fn test_detect_verdict_fail_when_signal_absent() {
        let output = "The implementation is incomplete. Missing error handling and tests.";
        assert!(!detect_verdict(output, SIGNAL));
    }

    #[test]
    fn test_detect_verdict_fail_when_partial_signal() {
        // Partial or mutated signal must not match.
        let output = "judge>PASS</judge>\n";
        assert!(!detect_verdict(output, SIGNAL));
    }

    #[test]
    fn test_detect_verdict_fail_on_empty_output() {
        assert!(!detect_verdict("", SIGNAL));
    }

    // --- extract_explanation tests ---

    #[test]
    fn test_extract_explanation_removes_signal_line() {
        let output = "Missing tests for edge cases.\n<judge>PASS</judge>\nPlease add them.";
        let explanation = extract_explanation(output, SIGNAL);
        assert!(!explanation.contains(SIGNAL));
        assert!(explanation.contains("Missing tests"));
        assert!(explanation.contains("Please add them"));
    }

    #[test]
    fn test_extract_explanation_empty_when_only_signal() {
        let output = "<judge>PASS</judge>\n";
        let explanation = extract_explanation(output, SIGNAL);
        assert!(explanation.is_empty());
    }

    #[test]
    fn test_extract_explanation_full_text_when_no_signal() {
        let output = "The implementation is incomplete.";
        let explanation = extract_explanation(output, SIGNAL);
        assert_eq!(explanation, "The implementation is incomplete.");
    }
}

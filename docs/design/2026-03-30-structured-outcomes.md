# Design Document: Structured Outcomes and Exit Codes

**Author:** Scott Idler
**Date:** 2026-03-30
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

RWL currently exits 0 for all outcomes - success, max iterations, Ctrl-C, and errors. This makes it impossible for callers (skills, scripts, other tools) to programmatically determine what happened. This design adds distinct exit codes, a structured JSON result file, and a per-session log that gets dumped to stderr on failure - so callers get actionable diagnostics, not just an exit code.

## Problem Statement

### Background

RWL was built as a standalone loop runner. You run it, watch the terminal output, and see what happened. That works when a human is watching. But rwl is now being integrated as a building block - the `/rwl-a-plan` skill invokes `rwl run` and needs to know the outcome to decide what to do next.

### Problem

All four `LoopOutcome` variants (`Complete`, `MaxIterations`, `Stopped`, `Error`) return `Ok(())` from `commands::run::run()`, which means the process always exits 0. The only information about what happened is printed to stdout as colored terminal text - not parseable by a caller.

Specifically:

1. **No exit code differentiation** - `main.rs` returns `Ok(())` for all outcomes. Only eyre errors (config missing, claude CLI not found) exit non-zero.
2. **No structured output** - outcomes are printed as colored banners. A caller would have to parse terminal escape codes to figure out what happened.
3. **No machine-readable result** - there's no `.rwl/result.json` or equivalent that a caller can read after rwl exits.

### Goals

- Distinct exit codes for each outcome category so callers can branch on `$?`
- A structured JSON result file (`.rwl/result.json`) written at the end of every run
- A per-session log (`.rwl/session.log`) that captures everything during the run and gets dumped to stderr on failure
- Preserve the existing human-friendly terminal output (this is additive, not a replacement)

### Non-Goals

- Phase-awareness in progress tracking (that's a separate concern - the LLM iteration tracks phases via the plan file and progress.txt)
- Changes to the prompt template
- Changes to the validation or quality gate system

## Proposed Solution

### Overview

Three changes:
1. Map `LoopOutcome` variants to distinct process exit codes in `main.rs`
2. Bundle all per-session artifacts into `.rwl/session/` - wiped and recreated each `rwl run`
3. On failure, session files are on disk for the caller to read selectively

### Session Directory

Each `rwl run` creates a new timestamped session directory under `/tmp/rwl/`:

```
/tmp/rwl/                                    # default --session-path base
  scottidler/ralph-wiggum-loop/              # reposlug from git remote
    20260330-101522/
      progress.txt
      session.log
      result.json
    20260330-143017/
      progress.txt
      session.log
      result.json
```

Session files live in `/tmp` by default because they're disposable diagnostics. The OS handles cleanup - no `.gitignore`, no accumulation in the repo, no cleanup commands to build. If you need to debug a failure, you do it in the same boot. If you didn't, you don't care.

The base session path is controlled by `--session-path` (defaults to `/tmp/rwl/<reposlug>/`). Each run creates a timestamped subdirectory under it. All file paths (progress, log, result) are derived from it. The runner, progress tracker, and session logger all receive the session path at construction time.

**Reposlug:** derived from the git remote URL using the `reposlug` pattern (e.g. `scottidler/ralph-wiggum-loop`). Falls back to the working directory basename if not in a git repo or no remote is configured. The slug uses `/` as the separator (matching the GitHub URL structure), creating a nested directory: `/tmp/rwl/scottidler/ralph-wiggum-loop/<timestamp>/`.

**On exit (success or failure):** rwl prints the session path to stdout as the last line of output:

```
session: /tmp/rwl/scottidler/ralph-wiggum-loop/20260330-101522
```

This is how callers discover where the files are. The skill agent captures this path from rwl's output and uses it to read `result.json`, `progress.txt`, or `session.log` as needed.

**What stays in `.rwl/`:** just config (`rwl.yml`, `PROMPT.md`) - things that are intentionally per-repo. No session artifacts, no `.gitignore` for generated files.

**session.log** captures everything: Claude output per iteration, validation command output, quality gate output, iteration summaries, timing. It's the complete narrative of the run. Distinct from `~/.local/share/rwl/logs/rwl.log`, which is a global debug log across all runs.

**progress.txt** is now per-session (not persistent). No more resume-from-iteration logic. Each `rwl run` starts at iteration 1. The code on disk (auto-committed by previous runs) IS the resume state - this is the core RWL philosophy. Stale validation errors from a dead session are noise.

This means removing:
- `ProgressTracker::iteration_count()` as the start iteration source
- The `if !progress.exists() { progress.init(...) }` guard in `commands/run.rs` - always init fresh
- The resume logic in the startup banner

What we keep:
- `progress.txt` as the intra-session feedback mechanism (errors from iteration N fed into iteration N+1's prompt)
- `progress.txt` as human-readable history of the session (via `rwl status` or `cat`)

### Exit Codes

| Outcome | Exit Code | Meaning |
|---------|-----------|---------|
| `Complete` | 0 | Plan finished, validation passed, quality gates passed |
| `MaxIterations` | 1 | Hit iteration limit without completion |
| `Stopped` | 2 | Interrupted (Ctrl-C) with WIP committed |
| `Error` | 3 | Runtime error (claude timeout, spawn failure, etc.) |
| eyre propagation | 4 | Setup error (config parse, plan file missing, claude CLI not found) |

Exit code 0 means success. Everything else is a distinct failure mode that the caller can act on.

### Data Model

A new `RunResult` struct. It's intentionally lean - verbose diagnostics live in the session log, not here:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct RunResult {
    pub outcome: String,          // "complete", "max-iterations", "stopped", "error"
    pub exit_code: i32,
    pub iterations: u32,
    pub plan: String,             // path to the plan file
    pub started: String,          // ISO 8601 timestamp
    pub finished: String,         // ISO 8601 timestamp
    pub duration_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,    // error message if outcome is "error" or "stopped"
    pub validation_passed: bool,  // last validation result
    pub quality_gates_passed: bool,
    #[serde(skip)]
    pub session_dir: PathBuf,     // not serialized - used internally for file writes
}
```

Written to `.rwl/sessions/<timestamp>/result.json` at the end of every run. This gives callers the structured "what happened" answer. For the "why", they read `session.log` in the same directory.

### Implementation Plan

#### Phase 1: Exit Codes

**Files:** `src/main.rs`, `src/commands/run.rs`, `src/runner.rs`

Two changes: `commands::run::run()` returns `RunResult` instead of `()`, and `main.rs` maps outcomes to exit codes.

Add an `exit_code()` method to `LoopOutcome`:

```rust
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
```

Split `main()` into thin shell + `run()` that returns the exit code. Setup errors (eyre propagation) get exit code 4:

```rust
fn main() {
    setup_logging().ok(); // best-effort
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("Error: {:#}", e);
            std::process::exit(4);
        }
    }
}

fn run() -> Result<i32> {
    let cli = Cli::parse();
    match &cli.command {
        Commands::Run(args) => {
            let result = commands::run::run(&cli, args)?;
            result.write_json()?;               // <session_dir>/result.json
            commands::run::print_outcome(&result);
            println!("session: {}", result.session_dir.display());
            Ok(result.outcome.exit_code())
        }
        Commands::Init => { commands::init::run(&cli)?; Ok(0) }
        Commands::Status => { commands::status::run(&cli)?; Ok(0) }
    }
}
```

This keeps the shell thin and makes exit code handling explicit.

#### Phase 2: Session Directory and Per-Session Progress

**Files:** `src/runner.rs`, `src/progress.rs`, `src/commands/run.rs`, `src/commands/init.rs`

Add `--session-path` to `RunArgs` in `src/cli.rs`:

```rust
#[derive(Parser)]
pub struct RunArgs {
    // ... existing args ...

    /// Base path for session files (default: /tmp/rwl/<reposlug>)
    #[arg(short = 's', long)]
    pub session_path: Option<PathBuf>,
}
```

Generate a timestamped session directory at the start of each `rwl run`:

```rust
let base = match &args.session_path {
    Some(p) => p.clone(),
    None => {
        let slug = reposlug(work_dir)?;  // from git remote, fallback to dirname
        PathBuf::from("/tmp/rwl").join(slug)
    }
};
let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
let session_dir = base.join(&timestamp);
fs::create_dir_all(&session_dir)?;
```

The `reposlug()` helper extracts `owner/repo` from `git remote get-url origin` (e.g. `scottidler/ralph-wiggum-loop`). Falls back to the working directory basename if no git remote exists.

Pass `session_dir` to `LoopRunner`, `ProgressTracker`, and `SessionLog` at construction. All per-run files live under this path.

Move `progress.txt` into the session directory. Update `ProgressTracker` to accept a path (it already does - just change what gets passed in). Remove the resume logic:

- `ProgressTracker::init()` is always called (no `if !exists()` guard)
- `start_iteration` is always 1
- Remove `iteration_count()` as start-iteration source (keep it for `rwl status` display)

Print the session path on exit (both success and failure):

```rust
println!("session: {}", session_dir.display());
```

The `.rwl/.gitignore` simplifies - no more `progress.txt` or `sessions/` entries:

```rust
const GITIGNORE_CONTENT: &str = r#"# RWL generated files
logs/
"#;
```

#### Phase 3: Session Log

**Files:** `src/runner.rs` (or new `src/session.rs`)

Create a `SessionLog` that tees output to both the terminal and `.rwl/session/session.log`:

```rust
pub struct SessionLog {
    file: BufWriter<File>,
}

impl SessionLog {
    pub fn new(session_dir: &Path) -> Result<Self> { ... }
    pub fn log(&mut self, line: &str) -> Result<()> { ... }  // writes to file
    pub fn println(&mut self, line: &str) -> Result<()> { ... }  // terminal + file
}
```

Everything that currently goes to `println!()` or `eprintln!()` in the runner also gets written to the session log:
- Iteration start/end markers
- Claude output (the streamed lines)
- Validation results and output
- Quality gate results
- Commit messages
- Timing information

No stderr dump on failure. The session path is printed to stdout on exit; callers use it to find the files. This keeps rwl's output clean and lets the caller (skill agent, script, human) decide what level of detail they want.

#### Phase 4: Result File

**Files:** `src/runner.rs` (or new `src/result.rs`), `src/commands/run.rs`

`LoopRunner::run()` returns `Result<RunResult>` instead of `Result<LoopOutcome>`. The `RunResult` struct wraps `LoopOutcome` plus timestamps:

```rust
impl RunResult {
    pub fn write_json(&self) -> Result<()> {
        let path = self.session_dir.join("result.json");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json)?;
        Ok(())
    }
}
```

The runner tracks:
- `start_time: Instant` - captured before the loop
- `last_validation_passed: bool` - updated each iteration
- `last_gates_passed: bool` - updated when gates run

These are local variables in `run()`, not struct fields.

#### Phase 5: Tests

- Unit test `LoopOutcome::exit_code()` mapping
- Unit test `RunResult` serialization to JSON
- Integration test that `rwl run` with a trivially-passing plan exits 0 and writes result.json
- Integration test that `rwl run` with a plan that can't complete exits 1 (set max-iterations to 1)

## Alternatives Considered

### Alternative 1: Structured stdout (JSON mode)

- **Description:** Add `--output json` flag that prints JSON to stdout instead of colored text
- **Pros:** Standard Unix pattern, composable with `jq`
- **Cons:** rwl streams a lot of output during the run (iteration progress, Claude output, validation results). JSON mode would either need to buffer everything or emit JSON lines. The result file approach is simpler and doesn't require changing the streaming output.
- **Why not chosen:** A result file at the end is sufficient for the immediate need. JSON stdout can be added later if needed.

### Alternative 2: Exit codes only, no result file

- **Description:** Just add exit codes, let callers read progress.txt for details
- **Pros:** Minimal change
- **Cons:** progress.txt is a log format, not a structured result. Parsing "how many iterations" or "what was the last validation error" from it is fragile. A dedicated result file is cheap to implement and much easier to consume.
- **Why not chosen:** The result file is low effort and high value for callers.

### Alternative 3: Phase-aware progress tracking

- **Description:** Add phase tracking to progress.txt and/or result.json
- **Pros:** Would let callers know which phase failed
- **Cons:** Phases are a concept in the plan file, not in rwl's core model. rwl doesn't parse the plan - it passes it to Claude. Adding phase awareness would require rwl to understand plan structure, which breaks the separation of concerns. The LLM tracks phases; rwl tracks iterations.
- **Why not chosen:** Out of scope. The LLM writes its own phase markers in progress.txt as part of its workflow. If we need structured phase data, that's a separate design.

## Technical Considerations

### Dependencies

- `serde_json` - not currently a direct dependency, needs `cargo add serde_json`

### Performance

No impact. Writing a small JSON file at the end of a run that already took N iterations of Claude invocations is negligible.

### Security

No new surface. Result file contains the same information already printed to the terminal.

### Testing Strategy

- Unit tests for exit code mapping and JSON serialization
- Integration test using a mock validation command (`true`/`false`) and a trivial plan to verify exit codes and result file contents without needing a real Claude instance

### Rollout Plan

This is additive - existing terminal output doesn't change. Any script or tool that was checking `$?` and assuming 0 (which was always true) will now see non-zero for failures, which is the correct behavior.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Scripts relying on exit 0 for all outcomes break | Low | Low | rwl is new; few external consumers exist. The change is strictly more correct. |
| session.log grows very large (100+ iterations) | Med | Low | Acceptable - it's on disk, callers read selectively. |
| Race condition writing session files on Ctrl-C | Low | Low | Ctrl-C only sets a flag; the loop checks it at iteration start and returns `Stopped`. Session files are written after the loop returns, before `process::exit()`. No race. |
| /tmp cleared on reboot loses session data | Med | Low | By design. If you didn't debug it before rebooting, you don't care. |

## Open Questions

- [ ] Should eyre setup errors also write session files, or is the exit code sufficient? (Leaning toward exit code only - if config can't load, there may not be a `.rwl/` directory to write to.)

## References

- `/home/saidler/repos/scottidler/ralph-wiggum-loop/src/runner.rs` - current LoopOutcome handling
- `/home/saidler/repos/scottidler/ralph-wiggum-loop/src/main.rs` - current main() that always returns Ok(())
- `/home/saidler/.claude/skills/rwl-a-plan/SKILL.md` - the skill that will consume these improvements

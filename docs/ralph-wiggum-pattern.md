# The Ralph Wiggum Pattern: A Complete Technical Specification

**Author:** Scott A. Idler
**Date:** 2026-02-01
**Status:** Design Specification

---

## Executive Summary

The Ralph Wiggum pattern is an iterative AI coding methodology where an agent executes repeatedly with fresh context until validation passes. Named by Geoffrey Huntley, the technique transforms AI coding from "unpredictable success on first attempt" to "predictable failure with guaranteed recovery."

This document distills the pattern into its component parts for implementation in Rust.

---

## Table of Contents

1. [Core Philosophy](#core-philosophy)
2. [The Loop](#the-loop)
3. [Fresh Context Model](#fresh-context-model)
4. [Prompt Management](#prompt-management)
5. [Progress Tracking](#progress-tracking)
6. [Exit Conditions](#exit-conditions)
7. [Validation System](#validation-system)
8. [The Completion Promise](#the-completion-promise)
9. [State Persistence](#state-persistence)
10. [Stop Conditions](#stop-conditions)
11. [Advanced Patterns](#advanced-patterns)
12. [Implementation Architecture](#implementation-architecture)

---

## Core Philosophy

### The Insight

Long conversations degrade LLM performance. Context windows fill with stale information, compaction summarizes away critical details, and the model loses focus. The Ralph Wiggum pattern solves this by:

1. **Starting fresh each iteration** - No conversation history carries over
2. **Persisting state in files** - Git, progress logs, and artifacts are the memory
3. **Accumulating feedback in prompts** - Previous failures guide future attempts
4. **Validating externally** - Objective, programmatic verification

### The Original Formulation

```bash
while :; do cat PROMPT.md | claude-code ; done
```

This deceptively simple bash loop embodies the entire pattern:
- Infinite loop until manual termination
- Fresh process (and context) each iteration
- State persists only in files
- The LLM reads its previous work from disk

### Key Principle: Convergence Over Correctness

The methodology succeeds not because the AI gets it right the first time, but because:
- Wrong outputs get rejected automatically by validation
- Feedback accumulates, guiding subsequent attempts
- Eventually, the system converges on a correct solution

The human role transforms from continuous micromanagement to upfront system design.

---

## The Loop

### Basic Structure

```
┌─────────────────────────────────────────────────────────┐
│                    RALPH WIGGUM LOOP                     │
├─────────────────────────────────────────────────────────┤
│                                                          │
│  1. READ STATE                                           │
│     └─ progress.txt, git log, artifacts                  │
│                                                          │
│  2. BUILD PROMPT                                         │
│     └─ system prompt + task + accumulated feedback       │
│                                                          │
│  3. CALL LLM (fresh context)                             │
│     └─ messages = [user_message_only]                    │
│                                                          │
│  4. EXECUTE TOOLS                                        │
│     └─ write files, run commands                         │
│                                                          │
│  5. RUN VALIDATION (external to LLM)                     │
│     └─ tests, linters, format checks                     │
│                                                          │
│  6. CHECK EXIT CONDITIONS                                │
│     ├─ Validation passed + Promise found → EXIT SUCCESS  │
│     ├─ Max iterations reached → EXIT FAILURE             │
│     ├─ Stop signal received → EXIT STOPPED               │
│     └─ Otherwise → ACCUMULATE FEEDBACK, CONTINUE         │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

### Iteration Lifecycle

```rust
struct LoopIteration {
    /// Which iteration number (0-indexed)
    number: u32,

    /// Prompt sent to LLM this iteration
    prompt: String,

    /// LLM response (text + tool calls)
    response: LlmResponse,

    /// Validation result
    validation: ValidationResult,

    /// Whether completion promise was found
    promise_found: bool,

    /// Timestamp
    timestamp: i64,
}
```

---

## Fresh Context Model

### What "Fresh" Means

| Persists Across Iterations | Discarded Each Iteration |
|---------------------------|--------------------------|
| Loop identity (id, type) | LLM conversation history |
| Iteration count | API request/response |
| Accumulated feedback (progress) | Tool execution context |
| Files on disk / git history | In-memory processing state |
| Output artifacts | |

### Why Fresh Context Matters

**Without fresh context (naive approach):**
```
Iteration 1: messages = [user1, assistant1]
Iteration 2: messages = [user1, assistant1, user2, assistant2]
Iteration 50: messages = [...100+ messages, context window full, LLM confused]
```

**With fresh context (Ralph Wiggum):**
```
Iteration 1: messages = [user_with_no_feedback]
Iteration 2: messages = [user_with_feedback_from_iter1]
Iteration 50: messages = [user_with_accumulated_feedback]  // Still ONE message
```

The feedback is **in the prompt**, not in conversation history.

### The Compaction Problem

Geoffrey Huntley warned: "Compaction is the devil." When using tools with automatic context compaction (like Claude Code's `/compact`), the agent may drift from original objectives as it relies increasingly on summaries rather than pristine initial specifications.

Fresh context per iteration sidesteps this entirely.

---

## Prompt Management

### Prompt Structure

```markdown
# System Role

You are in a Ralph Wiggum loop. You have NO MEMORY of previous runs.
Your state persists ONLY in files on disk.

## CRITICAL RULES

1. **READ STATE FIRST** - Check progress.txt and git log
2. **DO ONE SMALL THING** - Not a phase. One file, one fix, one test.
3. **EXIT IMMEDIATELY** - Do not retry errors. Just exit.

The bash loop will restart you with fresh context. That's the whole point.
The bash loop runs validation EXTERNALLY - you do NOT run validation.

---

## Task

{{task}}

## Previous Iteration Feedback

{{#if progress}}
The following feedback accumulated from previous iterations:

{{progress}}
{{/if}}

## Completion

When ALL work is complete, output: <promise>COMPLETE</promise>
```

### Prompt Evolution Strategies

**Strategy 1: Append Feedback**
```
Initial: "Build feature X"
After fail: "Build feature X\n\n## Iteration 1 Failed:\nError: missing import"
After fail: "Build feature X\n\n## Iteration 1...\n## Iteration 2 Failed:\nTest failed"
```

**Strategy 2: Structured Progress**
```markdown
## Completed Steps
- [x] Created src/foo.rs
- [x] Added basic struct
- [ ] Implement parse method (failed: type error)

## Current Focus
Fix type error in parse method
```

### Convergence-Oriented Prompts

Effective prompts follow a convergence design pattern:

- **Clear success criteria**: Specific, verifiable endpoints
- **Structured requirements**: Enumerated, concrete deliverables
- **Built-in verification**: Leverage existing feedback (test suites, linters)
- **Failure handling**: Explicit recovery procedures
- **Checkpoint structure**: Sequential steps guiding iteration

---

## Progress Tracking

### The Progress File

```bash
# progress.txt
Started: 2026-02-01T10:00:00Z
Branch: main
Prompt: PROMPT.md
---
Iteration 1: Added src/lib.rs with basic structure
Iteration 2: FAIL - cargo check found missing import
Iteration 3: Fixed import, added tests
Iteration 4: FAIL - test assertion failed in test_parse
Iteration 5: Fixed test, all passing
Iteration 5: validation PASS
```

### Git as Memory

Git history serves as the AI's memory and audit trail:

```rust
struct GitState {
    /// Commits made during this loop
    commits: Vec<String>,

    /// Current branch
    branch: String,

    /// Files modified this iteration
    modified_files: Vec<PathBuf>,
}
```

Each iteration can:
- Read git log to see what was done
- Check git diff to see current changes
- Make commits as checkpoints

### Monitoring Progress

```bash
# Watch progress in real-time
tail -f progress.txt

# See commit history
git log --oneline -20

# Check current iteration
grep -c "Iteration" progress.txt
```

---

## Exit Conditions

### Three Exit Paths

```rust
enum LoopOutcome {
    /// Validation passed AND completion promise found
    Complete {
        iterations: u32,
        artifacts: Vec<PathBuf>,
    },

    /// Max iterations exhausted without success
    Failed {
        reason: String,
        iterations: u32,
    },

    /// External signal received (stop, pause, invalidate)
    Stopped {
        signal: Signal,
        iterations: u32,
    },
}
```

### Exit Logic

```rust
fn check_exit_conditions(
    validation_passed: bool,
    promise_found: bool,
    iteration: u32,
    max_iterations: u32,
    signals: &[Signal],
) -> Option<LoopOutcome> {
    // Check for stop signals first
    if let Some(signal) = signals.first() {
        return Some(LoopOutcome::Stopped {
            signal: signal.clone(),
            iterations: iteration
        });
    }

    // Success requires BOTH validation AND promise
    if validation_passed && promise_found {
        return Some(LoopOutcome::Complete {
            iterations: iteration,
            artifacts: vec![],
        });
    }

    // Max iterations is a hard stop
    if iteration >= max_iterations {
        return Some(LoopOutcome::Failed {
            reason: "Max iterations exhausted".into(),
            iterations: iteration,
        });
    }

    // Continue iterating
    None
}
```

### Why Both Validation AND Promise?

| Validation | Promise | Outcome |
|------------|---------|---------|
| PASS | Found | Success - work complete |
| PASS | Not found | Continue - LLM may not be done |
| FAIL | Found | Continue - LLM claimed done but isn't |
| FAIL | Not found | Continue - work in progress |

The LLM claiming completion doesn't make it so. Validation is the ground truth.

---

## Validation System

### Validation Principles

1. **External to LLM** - The loop runner executes validation, not the AI
2. **Programmatically verifiable** - No subjective judgments
3. **Fast feedback** - Quick iteration cycles
4. **Deterministic** - Same code produces same result

### Validation Layers

```
┌─────────────────────────────────────────────────────────┐
│                  VALIDATION PYRAMID                      │
├─────────────────────────────────────────────────────────┤
│                                                          │
│                    ┌─────────────┐                       │
│                    │   LLM-as-   │  ← Subjective        │
│                    │   Judge     │    (use sparingly)    │
│                    └─────────────┘                       │
│                                                          │
│               ┌───────────────────────┐                  │
│               │   Integration Tests   │  ← Behavior      │
│               └───────────────────────┘                  │
│                                                          │
│          ┌─────────────────────────────────┐             │
│          │    Unit Tests / Assertions      │  ← Logic    │
│          └─────────────────────────────────┘             │
│                                                          │
│     ┌───────────────────────────────────────────┐        │
│     │   Type Checking / Compilation             │  ← Syntax│
│     └───────────────────────────────────────────┘        │
│                                                          │
│  ┌───────────────────────────────────────────────────┐   │
│  │   Format / Lint (clippy, fmt, etc.)               │   │
│  └───────────────────────────────────────────────────┘   │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

### Validation Command

```rust
struct ValidationConfig {
    /// Command to run for validation
    /// e.g., "cargo test && cargo clippy"
    command: String,

    /// Working directory (usually worktree)
    cwd: PathBuf,

    /// Timeout in seconds
    timeout_secs: u64,
}

struct ValidationResult {
    /// Did validation pass?
    passed: bool,

    /// Stdout/stderr output
    output: String,

    /// Specific error messages extracted
    errors: Vec<String>,

    /// Exit code
    exit_code: i32,
}
```

### Quality Gates (Additional Checks)

Beyond basic validation, quality gates enforce invariants:

```rust
fn check_quality_gates(worktree: &Path) -> Result<bool> {
    let mut passed = true;

    // Example: No #[allow(dead_code)] markers
    let dead_code = grep(worktree, "allow(dead_code)")?;
    if !dead_code.is_empty() {
        eprintln!("Quality gate FAILED: dead_code markers found");
        passed = false;
    }

    // Example: No TODO comments in main code
    let todos = grep(worktree, "TODO")?;
    if !todos.is_empty() {
        eprintln!("Quality gate FAILED: TODO comments remain");
        passed = false;
    }

    Ok(passed)
}
```

---

## The Completion Promise

### What It Is

A completion promise is a specific token the LLM outputs when it believes all work is done:

```
<promise>COMPLETE</promise>
```

### Why It Exists

1. **Explicit signal** - LLM must consciously decide work is done
2. **Parseable** - Easy to detect programmatically
3. **Decoupled from validation** - LLM opinion vs objective truth

### Detection

```rust
fn find_completion_promise(output: &str, signal: &str) -> bool {
    // Must be on its own line to avoid false positives
    output.lines().any(|line| line.trim() == signal)
}

// Usage
const COMPLETION_SIGNAL: &str = "<promise>COMPLETE</promise>";
let promise_found = find_completion_promise(&llm_output, COMPLETION_SIGNAL);
```

### Promise + Validation Matrix

```
if VALIDATION_PASSED && PROMISE_FOUND {
    // True completion - exit success
    check_quality_gates()?;
    exit(0);
}

if PROMISE_FOUND && !VALIDATION_PASSED {
    // LLM thinks it's done but validation disagrees
    // Continue with feedback showing the validation failure
    log("LLM claimed complete but validation failed");
}

if !PROMISE_FOUND {
    // LLM is still working
    // Continue regardless of validation state
}
```

---

## State Persistence

### What Must Persist

| Data | Storage | Purpose |
|------|---------|---------|
| Loop identity | JSONL/SQLite | Track loop across restarts |
| Iteration count | JSONL/SQLite | Resume from correct point |
| Progress feedback | progress.txt | Accumulate error messages |
| Work artifacts | Git worktree | Code changes |
| Timestamps | JSONL/SQLite | Ordering and debugging |

### Loop Record Structure

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Loop {
    /// Unique identifier
    pub id: String,

    /// Path to prompt template
    pub prompt_path: PathBuf,

    /// Validation command
    pub validation_command: String,

    /// Maximum allowed iterations
    pub max_iterations: u32,

    /// Current iteration (0-indexed)
    pub iteration: u32,

    /// Current status
    pub status: LoopStatus,

    /// Accumulated feedback from failures
    pub progress: String,

    /// Git worktree for this loop
    pub worktree: PathBuf,

    /// When loop was created
    pub created_at: i64,

    /// Last update timestamp
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStatus {
    Pending,      // Waiting to start
    Running,      // Currently executing
    Paused,       // User paused
    Complete,     // Validation passed
    Failed,       // Max iterations or error
    Stopped,      // External stop signal
}
```

### Crash Recovery

On daemon restart:

```rust
async fn recover_interrupted_loops(store: &Store) -> Result<()> {
    let interrupted = store.query::<Loop>(&[
        Filter::eq("status", "running"),
    ])?;

    for loop_record in interrupted {
        if worktree_exists(&loop_record.worktree) {
            // Auto-commit any uncommitted work
            auto_commit(&loop_record.worktree, "WIP: recovery")?;

            // Mark as pending for scheduler
            store.update_status(&loop_record.id, LoopStatus::Pending)?;
        } else {
            // Worktree lost, mark failed
            store.update_status(&loop_record.id, LoopStatus::Failed)?;
        }
    }

    Ok(())
}
```

---

## Stop Conditions

### Configurable Limits

```rust
struct LoopLimits {
    /// Maximum iterations before failure
    max_iterations: u32,

    /// Maximum tokens consumed
    max_tokens: Option<u64>,

    /// Maximum cost in USD
    max_cost: Option<f64>,

    /// Maximum time in seconds
    max_time_secs: Option<u64>,

    /// Timeout per iteration
    iteration_timeout_secs: u64,
}
```

### Example from RalphLoopAgent

From the image showing the JavaScript API:

```javascript
const agent = new RalphLoopAgent({
    model: "anthropic/claude-opus-4.5",
    instructions: `...`,
    tools: { readFile, writeFile, execute },
    stopWhen: [
        iterationCountIs(50),
        tokenCountIs(500_000),
        costIs(5.00)
    ],
    verifyCompletion: async () => {
        const { exitCode } = await execute("pnpm test");
        if (exitCode !== 0) return { complete: false };
        // Additional checks...
        return { complete };
    },
});
```

### Stop Signals

External signals can halt the loop:

```rust
#[derive(Debug, Clone)]
pub enum Signal {
    /// Graceful stop - finish current iteration
    Stop,

    /// Pause - save state, wait for resume
    Pause,

    /// Resume from pause
    Resume,

    /// Invalidate - parent changed, work is stale
    Invalidate,
}
```

---

## Advanced Patterns

### Multi-Phase Workflows

The Paddo.dev playbook defines a three-phase workflow:

```
Phase 1: PLANNING
├── Input: User task description
├── Output: IMPLEMENTATION_PLAN.md
├── Prompt: PROMPT_plan.md
└── Exit: Plan file exists with required sections

Phase 2: SPECIFICATION
├── Input: IMPLEMENTATION_PLAN.md
├── Output: specs/*.md files
├── Prompt: PROMPT_spec.md
└── Exit: All specs generated

Phase 3: BUILDING
├── Input: spec file + IMPLEMENTATION_PLAN.md
├── Output: Code changes
├── Prompt: PROMPT_build.md
├── Validation: otto ci (tests, lint, build)
└── Exit: One task complete, plan updated
```

### One Task Per Iteration

Key insight from both loopr and Paddo playbook:

> "Do ONE small thing. Not a phase. One file, one fix, one test."

This keeps:
- Context usage lean
- Reasoning quality high
- Progress visible
- Failures isolated

### Plan Disposability

Plans drift as requirements evolve or misunderstandings compound. Treat the plan as coordination state, not contract:

> Regenerating a stale plan through a planning-mode restart is cheaper than fighting incorrect assumptions.

### Subagents for Exploration

When exploration would bloat the main context, spawn subagents:

```rust
// Main loop stays focused on implementation
// Subagent handles expensive exploration
let exploration_result = spawn_subagent(
    "Find all usages of deprecated API",
    SubagentConfig { max_iterations: 5 }
).await?;

// Main loop continues with summarized findings
```

---

## Implementation Architecture

### Component Overview

```
┌─────────────────────────────────────────────────────────┐
│                      ralph-wiggum-loop                   │
├─────────────────────────────────────────────────────────┤
│                                                          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐      │
│  │   Config    │  │   Prompt    │  │  Validation │      │
│  │   Loader    │  │  Renderer   │  │   Runner    │      │
│  └─────────────┘  └─────────────┘  └─────────────┘      │
│                                                          │
│  ┌─────────────────────────────────────────────────┐    │
│  │                    LoopRunner                    │    │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐           │    │
│  │  │ LLM     │ │ Tool    │ │ State   │           │    │
│  │  │ Client  │ │ Router  │ │ Manager │           │    │
│  │  └─────────┘ └─────────┘ └─────────┘           │    │
│  └─────────────────────────────────────────────────┘    │
│                                                          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐      │
│  │  Progress   │  │   Git       │  │   Signal    │      │
│  │  Tracker    │  │  Manager    │  │   Handler   │      │
│  └─────────────┘  └─────────────┘  └─────────────┘      │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

### Core Traits

```rust
/// Executes a single loop iteration
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;
}

/// Validates loop output
#[async_trait]
pub trait Validator: Send + Sync {
    async fn validate(&self, worktree: &Path) -> Result<ValidationResult>;
}

/// Renders prompts with context
pub trait PromptRenderer: Send + Sync {
    fn render(&self, template: &str, context: &Context) -> Result<String>;
}

/// Manages loop state persistence
#[async_trait]
pub trait StateManager: Send + Sync {
    async fn save(&self, loop_record: &Loop) -> Result<()>;
    async fn load(&self, id: &str) -> Result<Option<Loop>>;
    async fn update_progress(&self, id: &str, message: &str) -> Result<()>;
}
```

### Main Loop Implementation

```rust
pub async fn run_loop(
    config: &LoopConfig,
    llm: Arc<dyn LlmClient>,
    validator: Arc<dyn Validator>,
    state: Arc<dyn StateManager>,
) -> Result<LoopOutcome> {
    let mut loop_record = state.load(&config.id).await?.unwrap_or_else(|| {
        Loop::new(config)
    });

    loop_record.status = LoopStatus::Running;
    state.save(&loop_record).await?;

    while loop_record.iteration < config.max_iterations {
        // 1. Check for signals
        if let Some(signal) = check_signals(&config.id).await? {
            return handle_signal(signal, &mut loop_record, &state).await;
        }

        // 2. Build prompt (fresh context)
        let prompt = build_prompt(config, &loop_record)?;

        // 3. Call LLM
        let response = llm.complete(CompletionRequest {
            system: prompt.system,
            messages: vec![Message::user(&prompt.user)],
            ..Default::default()
        }).await?;

        // 4. Execute tool calls
        for tool_call in &response.tool_calls {
            execute_tool(tool_call, &loop_record.worktree).await?;
        }

        // 5. Auto-commit changes
        auto_commit(&loop_record.worktree,
            &format!("ralph: iteration {}", loop_record.iteration))?;

        // 6. Run validation (external)
        let validation = validator.validate(&loop_record.worktree).await?;

        // 7. Check for completion promise
        let promise_found = find_completion_promise(
            &response.text,
            &config.completion_signal
        );

        // 8. Update progress
        let progress_entry = format!(
            "Iteration {}: {} - validation {}",
            loop_record.iteration,
            if promise_found { "promise found" } else { "working" },
            if validation.passed { "PASS" } else { "FAIL" }
        );
        state.update_progress(&config.id, &progress_entry).await?;

        // 9. Check exit conditions
        if validation.passed && promise_found {
            // Quality gates as final check
            if check_quality_gates(&loop_record.worktree)? {
                loop_record.status = LoopStatus::Complete;
                state.save(&loop_record).await?;
                return Ok(LoopOutcome::Complete {
                    iterations: loop_record.iteration + 1,
                    artifacts: vec![],
                });
            }
        }

        // 10. Accumulate feedback for next iteration
        if !validation.passed {
            loop_record.progress.push_str(&format!(
                "\n---\nIteration {} Failed:\n{}\n",
                loop_record.iteration + 1,
                validation.output
            ));
        }

        loop_record.iteration += 1;
        loop_record.updated_at = now_ms();
        state.save(&loop_record).await?;
    }

    // Max iterations exhausted
    loop_record.status = LoopStatus::Failed;
    state.save(&loop_record).await?;

    Ok(LoopOutcome::Failed {
        reason: "Max iterations exhausted".into(),
        iterations: loop_record.iteration,
    })
}
```

---

## Configuration Reference

### YAML Configuration

```yaml
# ralph-wiggum.yml
loop:
  # Prompt configuration
  prompt_file: PROMPT.md
  completion_signal: "<promise>COMPLETE</promise>"

  # Limits
  max_iterations: 100
  iteration_timeout_minutes: 10

  # Validation
  validation_command: "otto ci"
  quality_gates:
    - name: no_dead_code
      pattern: "allow(dead_code)"
      forbidden: true
    - name: no_todos
      pattern: "TODO"
      forbidden: true

  # Progress tracking
  progress_file: progress.txt
  auto_commit: true
  commit_message_template: "ralph: iteration {iteration}"

  # Sleep between iterations
  sleep_between_secs: 2

# LLM configuration
llm:
  model: opus
  max_tokens: 8192

# Git configuration
git:
  worktree_base: .ralph/worktrees
  preserve_failed_branches: true
```

### Environment Variables

```bash
# Required
ANTHROPIC_API_KEY=sk-ant-...

# Optional overrides
PROMPT_FILE=PROMPT.md
MAX_ITERATIONS=100
PROGRESS_FILE=progress.txt
MODEL=opus
SLEEP_BETWEEN=2
TIMEOUT_MINUTES=10
VALIDATION_CMD="otto ci"
```

---

## Best Practices

### When to Use Ralph Wiggum

**Good fit:**
- Mechanical refactors (dependency migrations)
- Code generation from specifications
- Test coverage expansion
- Documentation generation
- Format/lint fixes

**Poor fit:**
- UX decisions requiring human judgment
- Business logic with ambiguous requirements
- Performance optimization (needs profiling)
- Security-critical code (needs audit)

### Prompt Engineering Tips

1. **Be specific about completion criteria**
   - Bad: "Make the code better"
   - Good: "All tests pass, coverage > 80%, no clippy warnings"

2. **Include recovery instructions**
   - "If tests fail, read the error and fix the specific issue"
   - "If you can't complete in one iteration, make incremental progress"

3. **Keep prompts stable**
   - Don't change the prompt mid-run
   - Use progress feedback for dynamic information

4. **Trust the validation**
   - Don't instruct LLM to run tests (validation does that)
   - Let failures guide iterations naturally

### Operational Tips

1. **Start with low max_iterations**
   - Discover failure patterns early
   - Tune prompts before long runs

2. **Watch the progress file**
   - `tail -f progress.txt` during runs
   - Identify stuck patterns

3. **Use git history**
   - Each iteration commits
   - Can rollback problematic changes

4. **Schedule overnight runs**
   - Large refactors work well unattended
   - Review results in morning

---

## References

- [Geoffrey Huntley - Ralph](https://ghuntley.com/ralph/) - Original concept
- [Tessl.io - Unpacking Ralph Wiggum](https://tessl.io/blog/unpacking-the-unpossible-logic-of-ralph-wiggumstyle-ai-coding/) - Technical analysis
- [HumanLayer - Brief History of Ralph](https://www.humanlayer.dev/blog/brief-history-of-ralph) - Historical context
- [Ibrahim Pima - The Ralph Wiggum Breakdown](https://dev.to/ibrahimpima/the-ralf-wiggum-breakdown-3mko) - Implementation details
- [Paddo.dev - Ralph Wiggum Playbook](https://paddo.dev/blog/ralph-wiggum-playbook/) - Three-phase workflow
- [loopr](~/repos/scottidler/loopr/) - Reference implementation

---

## Appendix: Quick Reference

### Minimal Loop Script

```bash
#!/usr/bin/env bash
set -e

MAX_ITERATIONS=${MAX_ITERATIONS:-100}
COMPLETION_SIGNAL="<promise>COMPLETE</promise>"
VALIDATION_CMD=${VALIDATION_CMD:-"cargo test"}

for i in $(seq 1 $MAX_ITERATIONS); do
    echo "=== Iteration $i ==="

    # Run Claude with prompt
    OUTPUT=$(claude --print <PROMPT.md 2>&1 | tee /dev/stderr)

    # Commit changes
    git add -A && git commit -m "ralph: iteration $i" || true

    # Run validation
    if eval "$VALIDATION_CMD"; then
        if echo "$OUTPUT" | grep -qx "$COMPLETION_SIGNAL"; then
            echo "Complete!"
            exit 0
        fi
    fi
done

echo "Max iterations reached"
exit 1
```

### Key Formulas

```
Success = Validation.passed AND Promise.found
Continue = NOT Success AND Iteration < MaxIterations
Failure = Iteration >= MaxIterations AND NOT Success
```

### The One Rule

> Each iteration starts fresh. State lives in files.

# Design Document: Containment and Backpressure (Pattern-Fidelity Gaps)

**Author:** Scott Idler
**Date:** 2026-06-29
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

`rwl` faithfully implements the *spine* of the Ralph Wiggum pattern (fresh
context per iteration, files-and-git as memory, external validation, one-task
prompt discipline) but not the *safety envelope and backpressure layers* the
pattern's own research insists on. This document closes every documented gap:
blast-radius containment (worktree isolation + fail-closed permission preflight
+ immutable path boundaries), budget stop conditions (wall-clock / cost /
tokens), and an optional LLM-as-judge gate for subjective criteria.

## Problem Statement

### Background

The canonical pattern (`notes/ralph-wiggum-loop.md`, `docs/ralph-wiggum-pattern.md`)
names three backpressure layers and a set of "essential guardrails." `rwl`
ships the downstream programmatic gate (validation command + quality gates) and
the iteration-count guardrail. It does **not** ship the guardrails that exist
specifically because the loop runs an autonomous agent with permission checks
disabled.

This document assumes the loop is already *functional*: the separate fixes that
removed the `--max-turns 1` strangle, closed Claude's stdin, and made the
completion promise line-exact. Those made `rwl` *run*; this closes the gap
between "runs" and "matches the pattern the research defines."

### Problem

Measured against the documented pattern, `rwl` has these gaps:

1. **Unsafe by default.** `llm.dangerously_skip_permissions` defaults to `true`
   and there is no sandbox. The pattern's loudest guardrail is "sandbox
   everything because `--dangerously-skip-permissions` bypasses ALL safety." A
   live run prints `Commands will run WITHOUT sandboxing`.
2. **No isolation.** `commands/run.rs` sets `work_dir = Path::new(".")`. The
   agent mutates the user's live working tree directly. The spec describes
   per-loop worktrees (`.ralph/worktrees`, `preserve_failed_branches`).
3. **No immutable boundaries.** The agent can edit any path, including the plan,
   design docs, `.rwl/` config, or `.git/`. The spec lists "define immutable
   boundaries for files/directories" as an essential guardrail.
4. **No budget stop conditions.** Only `max_iterations` and a per-iteration
   timeout exist. The spec's own `LoopLimits` lists `max_tokens`, `max_cost`,
   and `max_time_secs`. A stuck-but-progressing loop can burn unbounded cost.
5. **No LLM-as-judge layer.** Validation is programmatic only. The spec's
   backpressure pyramid includes a binary LLM-as-judge for subjective criteria.

### Goals

- Contain blast radius: run in an isolated git worktree by default, and refuse
  to run a permission-bypassed agent against the primary working tree unless the
  user explicitly opts out (`--unsafe`).
- Enforce immutable path boundaries: agent edits to protected globs are reverted
  and fed back, never committed.
- Add budget stop conditions: wall-clock, cost (USD), and token caps, each with
  a distinct, machine-readable outcome.
- Add an optional LLM-as-judge gate that runs only when configured, mirroring
  the quality-gate structure.
- Keep every default *safe* without breaking unattended operation, and keep the
  human-facing terminal UX (live streaming) intact.

### Non-Goals

These appear in the aspirational `ralph-wiggum-pattern.md` spec but are
**deliberately out of scope** - they are daemon/orchestrator features that do
not fit a single-shot CLI:

- **Pause/Resume/Invalidate signals.** `rwl` is single-shot; Ctrl-C → `Stopped`
  is the only signal it needs.
- **Persistent loop records / crash recovery (JSONL/SQLite).** Git history +
  `result.json` are sufficient resume state for a CLI; the code on disk *is* the
  state.
- **Three-phase requirements → planning → building workflow.** `rwl` is the
  Building-phase executor by design; requirements/planning are owned by
  `/create-design-doc` and the `rwl-a-plan` skill.
- **Multi-agent / subagent orchestration.**
- **Independent token/cost metering via a proxy.** We read Claude's own reported
  `total_cost_usd` / `usage`, not a separately metered ledger.

## Proposed Solution

### Overview

Wrap the existing loop with two concerns and one optional gate:

1. **Containment** (pre-run preflight + per-iteration enforcement): worktree
   isolation, a fail-closed permission preflight, and a protected-path guard.
2. **Backpressure budgets** (per-iteration accounting): accumulate cost/tokens
   from Claude's reported usage and track wall-clock; stop with a distinct
   outcome when any cap is exceeded.
3. **Judge gate** (optional): a fresh Claude invocation returning a binary
   verdict, run as a final gate after quality gates, only when `judge:` is
   configured.

None of this changes the spine: fresh context per iteration, files/git as
memory, validation external to the agent.

### Architecture

```
rwl run
  └─ preflight (NEW: containment)
       ├─ resolve isolation: create worktree  OR  use CWD (--isolation none)
       ├─ permission preflight: if bypass && not contained && !--unsafe → REFUSE (exit 4)
       └─ work_dir := worktree path (or ".")
  └─ loop (per iteration)
       ├─ wall-clock budget check (NEW; see Addendum: Option A)
       ├─ build prompt (+ protected-paths boundary text)  (CHANGED)
       ├─ run claude
       ├─ protected-path guard: revert+feedback any protected edits (NEW)
       ├─ auto-commit (in worktree)
       ├─ validation + quality gates
       ├─ judge gate (NEW, optional)
       └─ exit-condition dispatch (+ BudgetExceeded on wall-clock)  (CHANGED)
```

> **Note (supersedes the cost/token text below):** The Addendum at the end of
> this document is the final decision. Cost/token budgets and the `stream-json`
> switch were dropped in favor of a wall-clock cap only (Option A). The Data
> Model, API Design, and Phase 3 sections below have been reconciled to that
> decision; the original streaming proposal is preserved in the Addendum.

`work_dir` is computed once in the preflight and threaded into `LoopRunner`,
`ValidationRunner`, and `GitManager` exactly as today - only its *value*
changes (worktree path vs `.`).

**Worktree location.** The worktree is created at `<session_dir>/worktree`
(i.e. under `/tmp/rwl/<slug>/<timestamp>/`), co-located with the session
artifacts so it auto-cleans on reboot. The branch (`rwl/<plan-slug>-<timestamp>`)
lives in the repo's shared object store, so on `Complete` the user reviews/merges
it from the *real* repo even after the /tmp worktree is gone.

**Definition of "contained" and the fail-closed preflight.** A run is
*contained* if writes cannot escape a throwaway scope. The preflight decides:

| isolation | OS sandbox | permission bypass | `--unsafe` | Result |
|-----------|-----------|-------------------|-----------|--------|
| worktree  | any       | any               | -         | **run** (worktree contains writes) |
| none      | present   | yes               | -         | **run** (sandbox contains writes) |
| none      | absent    | yes               | no        | **REFUSE (exit 4)** |
| none      | absent    | yes               | yes       | **run** (explicit opt-out) |
| none      | any       | no                | -         | **run** (prompts gate writes; may stall) |

So the *default* (`isolation: worktree`) is always safe, and the only path to an
uncontained bypass is an explicit `--unsafe`.

"Permission bypass" is the existing `llm.dangerously_skip_permissions` flag (it
maps to Claude's `--dangerously-skip-permissions`); the preflight reads it, it is
not duplicated into `safety`. "OS sandbox detected" means `bubblewrap` **and**
`socat` are both on `PATH` - the same dependencies Claude's own sandbox requires
(the live probe degraded to "WITHOUT sandboxing" precisely because `socat` was
absent).

**Plan-path resolution.** Because `work_dir` may become the worktree, `--plan`
is canonicalized to an **absolute** path before the loop starts. This is
essential: a relative path would break inside the worktree, and an *uncommitted*
plan does not exist in a freshly created worktree at all - the absolute path
reads it from its real location regardless of `work_dir`.

**Protected-path baseline.** `rwl` writes its own files before the loop
(`.rwl/rwl.yml` with CLI overrides). To keep the protected-path guard from
reverting `rwl`'s *own* setup, `rwl` makes one baseline commit
(`rwl: session setup`) in the worktree before iteration 1. The guard then
reverts only protected-path deltas against `HEAD`, which can therefore only
originate from the agent.

### Data Model

New config sections (`.rwl/rwl.yml`, kebab-case keys per convention):

```yaml
safety:
  isolation: worktree            # worktree | none   (default: worktree)
  protected-paths:               # globs the agent may not modify
    - ".git/"
    - ".rwl/"
    - "docs/design/"

budget:
  max-total-minutes: 0           # 0 = unlimited (wall-clock across the whole run)

# Optional. Absent → no judge runs (mirrors quality_gates: configured = active).
judge:
  model: opus
  signal: "<judge>PASS</judge>"  # binary verdict token, line-exact
  prompt: |
    Review the committed diff against the plan. Output exactly
    "<judge>PASS</judge>" on its own line if it meets the criteria below,
    otherwise explain what is missing. Criteria: <subjective criteria here>.
```

Rust structs (serde `rename_all = "kebab-case"`, `deny_unknown_fields` per
convention; numeric "0 = unlimited" validated as legal):

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct SafetyConfig {
    pub isolation: Isolation,          // Worktree | None
    pub protected_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Isolation { Worktree, None }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct BudgetConfig {
    pub max_total_minutes: u64,        // 0 = unlimited
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct JudgeConfig {
    pub model: String,
    pub signal: String,
    pub prompt: String,
}
// In Config: judge: Option<JudgeConfig>
```

New `LoopOutcome` variant and exit code:

| Outcome | Exit | Meaning |
|---------|------|---------|
| `Complete` | 0 | unchanged |
| `MaxIterations` | 1 | unchanged |
| `Stopped` | 2 | unchanged (Ctrl-C) |
| `Error` | 3 | unchanged |
| eyre / preflight refusal | 4 | setup error **incl. fail-closed containment refusal** |
| `BudgetExceeded` | 5 | **NEW** - the wall-clock cap (`max-total-minutes`) was hit |

`RunResult` gains `branch: Option<String>` (the worktree branch, for callers to
review/merge). Per the Addendum (Option A), the `cost_usd` / `total_tokens`
fields are **not** added - they depended on the dropped `stream-json` capture.

### API Design

- `cli.rs` `RunArgs`: add `--unsafe` (bypass the containment preflight) and
  `--isolation <worktree|none>` (override config).
- `safety` module:
  - `fn resolve_workdir(cwd, &SafetyConfig, unsafe_opt) -> Result<Workdir>` -
    creates the worktree (or returns CWD), enforces the fail-closed preflight.
  - `fn guard_protected(work_dir, &[glob]) -> Result<Vec<String>>` - reverts
    agent edits to protected paths (symlink-safe, canonicalized under root) and
    returns the list of reverted paths for feedback.
- `runner.rs` `run_claude`: **unchanged** (plain `--print`). Per the Addendum
  (Option A) the `stream-json` switch and usage capture were dropped.
- `budget` module: `struct Budget { ... } ; fn exceeded(&self, elapsed) -> Option<Reason>`
  - wall-clock only (`max-total-minutes`).
- `judge` module: `fn run_judge(&JudgeConfig, work_dir) -> Result<bool>` - fresh
  Claude invocation, line-exact verdict detection (reuse the `promise_found`
  helper generalized to any signal).

### Implementation Plan

#### Phase 1: Worktree isolation + fail-closed permission preflight
**Model:** opus
- Add `SafetyConfig`/`Isolation` to `config.rs` with safe defaults.
- New `src/safety.rs`: `resolve_workdir` runs `git worktree add
  <session_dir>/worktree -b rwl/<plan-slug>-<timestamp>` from HEAD and returns
  that path. On `isolation: none`, returns CWD. If the CWD is **not a git repo**,
  `worktree` is impossible → fall back to `none` (the preflight then governs
  whether the bypass is allowed).
- Canonicalize `--plan` to an absolute path before resolving `work_dir` (see
  Plan-path resolution above).
- Preflight: implement the decision table above. The refusal is an `Err`
  returned before the loop, so it propagates to exit 4 with a message naming the
  safe options (`isolation: worktree`, install a sandbox, or `--unsafe`).
- Thread the resolved `work_dir` through `run.rs` → `LoopRunner`. Make the
  baseline `rwl: session setup` commit. On finish/failure, print `branch:` and
  leave the worktree for review (preserve failed branches). Add `--unsafe` /
  `--isolation` to `RunArgs`.

#### Phase 2: Immutable path boundaries
**Model:** opus
- New `guard_protected`: after each iteration's Claude run, `git status
  --porcelain` filtered against `safety.protected-paths` globs; revert matches
  with `git checkout -- <path>` / `git clean` for new files; canonicalize and
  assert under the worktree root, skip symlinks (per safety rules).
- Record reverted paths as a feedback entry in `progress.txt` so the next
  iteration's prompt explains the boundary.
- Inject the protected-path list into the prompt template (`templates/prompt.rs`).

#### Phase 3: Budget stop conditions (wall-clock only - see Addendum, Option A)
**Model:** opus
- Add `BudgetConfig { max_total_minutes }` to `config.rs` (0 = unlimited).
- New `budget` module: track run start time; `exceeded(elapsed) -> Option<Reason>`
  returns a reason when `max_total_minutes > 0` and elapsed exceeds it.
- Check the wall-clock cap at the top of each iteration (before building the
  prompt / spawning Claude).
- Add `LoopOutcome::BudgetExceeded { reason }` → exit 5; surface in
  `print_outcome`. `run_claude` and `RunResult` are unchanged (no cost/token
  capture per Option A).

#### Phase 4: LLM-as-judge gate
**Model:** sonnet
- New `src/judge.rs`: when `judge:` is configured and validation + quality gates
  pass, run a fresh `claude --print` with the judge prompt; detect the verdict
  with the line-exact signal helper. On fail, continue the loop with the judge's
  explanation appended to `progress.txt`.
- No-op when `judge:` is absent.

#### Phase 5: Config migration, docs, skill, tests
**Model:** sonnet
- `rwl init` default config writes the new sections with safe defaults.
- Update `rwl-a-plan` SKILL.md: document exit code 5, the `branch:` output line,
  and that runs are isolated by default.
- Update `ralph-wiggum-pattern.md` to reflect what is implemented vs non-goal.
- Tests: preflight refusal logic, protected-path glob matching (incl. symlink
  rejection), budget `exceeded()` thresholds, judge verdict parsing, and an
  integration test asserting exit 5 under a tiny `max-total-minutes`.

## Alternatives Considered

### Alternative 1: Just flip `dangerously_skip_permissions` to `false`
- **Description:** Default to permission prompts instead of bypass.
- **Pros:** Trivially "safe."
- **Cons:** Prompts stall an unattended loop indefinitely - it defeats the entire
  point of the loop. `permission_denials` in the result would just accumulate.
- **Why not chosen:** Breaks the core use case. The right answer is *contain* the
  bypass, not remove it.

### Alternative 2: Rely solely on Claude's built-in OS sandbox
- **Description:** Require bubblewrap/socat and lean on Claude's sandbox.
- **Pros:** No git-worktree machinery.
- **Cons:** Environment-dependent - the live probe showed it silently degrades to
  "WITHOUT sandboxing" when `socat` is missing; it also does not isolate git
  state or protect the working tree from edits.
- **Why not chosen:** Too fragile to be the primary control. Kept as a
  *complementary* signal in the preflight, not the foundation.

### Alternative 3: Docker / E2B container per run
- **Description:** Run each loop in a throwaway container.
- **Pros:** Strongest isolation, matches the spec's sandboxing note.
- **Cons:** Heavy; image/dependency management; breaks the lightweight "bow on a
  bash loop" ethos.
- **Why not chosen:** A git worktree gives most of the blast-radius benefit at
  CLI weight. Containerization can be a later `isolation: container` variant.

### Alternative 4: `--output-format json` (single result) for budgets
- **Description:** Parse the single end-of-run JSON instead of streaming.
- **Pros:** Simplest parse.
- **Cons:** Loses live streaming - the human-facing UX regresses to a silent wait.
- **Why not chosen:** `stream-json` keeps the live output *and* yields usage in
  the terminal event.

## Technical Considerations

### Dependencies
- No new crates required (`serde_json` already present for stream-json parsing).
- Runtime: `git worktree` (always available); optional `bubblewrap`/`socat` only
  improve the preflight signal.

### Performance
- Negligible. One worktree creation per run; per-iteration JSON parsing and a
  `git status` are dominated by the Claude call.

### Security
- This document *is* the security work. Invariants: defaults fail **closed**
  (bypass + uncontained + no `--unsafe` → refuse); protected-path enforcement
  canonicalizes and rejects symlinks before reverting; the worktree confines
  writes to a throwaway branch the user reviews before merge.

### Testing Strategy
- Unit: preflight decision table, glob/symlink guard, budget thresholds, verdict
  parsing - all with fakes, no real Claude.
- Integration: trivial plan + `max-total-minutes` near zero → exit 5;
  `isolation: none` + bypass + no `--unsafe` → exit 4.

### Rollout Plan
- Additive. Existing `.rwl/rwl.yml` files lack the new sections → serde defaults
  apply (isolation defaults to `worktree`, which changes behavior to *safer*).
  Document that the first run after upgrade will execute in a worktree.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `stream-json` shape changes across Claude versions | Med | Med | Parse defensively; missing usage → `warn!` + zero, never crash |
| Worktree creation fails in this repo's bare-container layout | Med | High | Detect bare-container; fall back to `git worktree add` against the resolved git-dir; covered by integration test |
| Protected-path guard reverts a legitimate edit | Low | Med | Globs are explicit and conservative by default; reverts are logged and recoverable from reflog |
| Judge flakiness causes non-convergence (spec warns re: flaky verification) | Med | Med | Judge is opt-in, binary, line-exact; off by default |
| Cost accounting drifts from real billing | Low | Low | We report Claude's own `total_cost_usd`; documented as advisory |
| `auto_commit: false` + worktree → empty review branch, work stranded in /tmp | Low | Med | Preflight `warn!`s when isolation is on and auto-commit is off; recommend keeping auto-commit on |
| Many runs accumulate worktrees under /tmp/rwl | Med | Low | Worktrees live under the session dir (reboot-cleared); document `git worktree prune` for same-boot cleanup |
| Plan committed in repo but edited uncommitted before run → worktree sees stale plan | Med | Med | Absolute plan-path reads the live file, not the worktree copy (see Plan-path resolution) |

## Open Questions
- [ ] Merge-back: leave the worktree branch for manual review (v1) or add a
  `--merge` flag that fast-forwards into the base branch on `Complete`?
- [ ] Should the OS sandbox be a *hard* requirement when `isolation: none`, or
  remain best-effort with `--unsafe` as the sole override?
- [ ] Judge determinism: pin the judge to a fixed model/temperature, or accept
  per-run variance?

## References
- `notes/ralph-wiggum-loop.md` - the pattern research (guardrails, backpressure)
- `docs/ralph-wiggum-pattern.md` - the aspirational spec (`LoopLimits`, sandboxing)
- `src/runner.rs`, `src/commands/run.rs`, `src/config.rs` - current implementation
- `~/.claude/skills/rwl-a-plan/SKILL.md` - the consuming skill

## Addendum: Streaming Cost Capture (`stream-json`) - Considered, Deferred

This addendum records, in full, the streaming-cost option that Phase 3 originally
proposed, why it was reconsidered, and the decision reached. It is preserved so a
future reader does not re-litigate it from scratch.

### The question

Should `rwl` enforce a **cost ceiling in-process** - i.e. stop the loop when a
run's dollar (or token) spend crosses a configured cap - or bound runs by **time
and iteration count only** and treat cost as an out-of-band concern?

A dollar cap is only possible if `rwl` can *observe* spend mid-run. There are
exactly three ways to get that number, and they define the option space.

### Option space

**Option A - Time + iteration caps only (no cost in `rwl`).**
Keep the current plain-text `claude --print` invocation. Add a wall-clock cap
(`max-total-minutes`) next to the existing `max_iterations`. Spend is bounded
*indirectly*: total cost cannot exceed `max_iterations` turns, each itself
ceiling-bounded by the model's context window and the per-iteration timeout.
Cost *visibility* is left entirely external and optional (the user runs whatever
reporting tool they like, e.g. `clyde cost`, after the run).

- Pros: zero new dependencies; no JSON parser; no coupling to Claude's output
  schema; the live dimmed-line streaming UX is unchanged; smallest change.
- Cons: no hard dollar ceiling; a single pathological iteration (very large
  context, expensive model) could overspend within the time budget; actual cost
  is known only after the run.

**Option B - In-process cost cap via `stream-json`.**
Switch the subprocess to `claude --print --output-format stream-json --verbose`,
parse Claude's reported `total_cost_usd` / `usage` out of the JSONL event stream,
accumulate across iterations, and halt with a distinct outcome when a `$` or
token cap is exceeded.

- Pros: a hard, direct dollar/token ceiling; self-contained (reads Claude's
  *own* reported figures - no second tool); cost is known live during the run.
- Cons: the figure is Claude's self-report, not authoritative billing; and the
  refactor of `run_claude` carries three robustness traps (below).

**Option C - Delegate cost to an external tool (`clyde cost`).**
Rejected outright. `clyde` is observability, not enforcement (it reports from
JSONL logs; it cannot halt a running process), and wiring `rwl` to shell out to
it would give `rwl` a runtime dependency on a second tool with version coupling.
Out of scope by construction.

### What Option B actually involves

It is a **contained, single-function refactor** of `run_claude`
(`src/runner.rs`, roughly lines 316-412), not a large build. Today that function
reads stdout line-by-line, prints each line dimmed, and accumulates the raw text
for `find_promise` to scan. Under `stream-json`, stdout becomes JSONL - one event
object per line:

```
{"type":"system","subtype":"init", ...}
{"type":"assistant","message":{"content":[{"type":"text","text":"Created hello.txt"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write", ...}]}}
{"type":"result","subtype":"success","total_cost_usd":0.041,"usage":{ ... }, "result":"..."}
```

So the same loop must, per line: parse the JSON, branch on `type`, extract
`message.content[].text` from `assistant` events to print (preserving the live
UX), and capture `total_cost_usd` / `usage` from the terminal `result` event.
The function's return type changes from `String` to `(String, Usage)`.

The cost is **not** lines of code; it is three robustness traps:

1. **Non-JSON preamble.** Before the JSON stream, Claude prints human warnings
   (`claude.ai connectors disabled`, `Sandbox disabled`, `no stdin data
   received`). A naive `serde_json::from_str(line)` panics on these. The parser
   must fail soft - skip or pass through any line that is not valid JSON.
2. **UX + promise-detection reconstruction.** The live dimmed-line output and the
   `find_promise` scan both currently rely on raw stdout being prose. Under
   JSONL, both must be fed *reconstructed* assistant text, or the live output
   regresses to silence-then-dump and promise detection breaks.
3. **Schema coupling.** The JSONL event shape (`total_cost_usd`,
   `message.content[].text`, the `result` event) is Claude Code's internal
   contract and can shift across versions. Plain-text mode has almost no schema
   to break; `stream-json` ties `rwl`'s hottest path to an evolving format.

### Decision

**Adopt Option A.** A two-model review panel (Architect / Gemini and Staff
Engineer / Codex) reviewed this question twice. On the first pass they diverged
(Architect: over-engineering; Codex: core correctness). Given the constraints
above - the `clyde` dependency rejected, the wall-clock cap shown to be trivial,
and the `stream-json` work re-measured as a contained refactor whose true cost is
schema coupling rather than effort - they **converged on Option A**, and both
independently proposed the same posture:

- Add the **wall-clock cap now** (`max-total-minutes`): cheap, in-process, no
  parsing, no dependencies. Together with `max_iterations` it bounds a runaway
  loop's spend envelope by construction. The exit code 5 (`BudgetExceeded`)
  defined in the Data Model is retained, now triggered by wall-clock.
- Do **not** make `stream-json` cost enforcement the default backpressure.
- Leave the door open for **passive cost capture/display later**, behind an
  explicit optional `stream-json` diagnostics mode, for users who accept the
  schema-breakage risk - but never as the core stop mechanism.

The decisive reasoning: a dollar cap on a self-reported figure does not justify
coupling `rwl`'s most critical path to an undocumented, version-volatile schema,
when time and iteration count already bound the run. The single objection to A -
a pathological iteration spiking spend before the time cap fires - is mitigated
by the model's context window acting as a natural per-turn ceiling.

### Effect on this design

`max-cost-usd` and `max-total-tokens` are removed from the `budget` config; only
`max-total-minutes` remains. The mandatory `stream-json` switch and its
fail-closed usage-parsing requirement are dropped. If passive cost display is
pursued later, it is a separate, opt-in diagnostics feature - not part of this
design's core loop.

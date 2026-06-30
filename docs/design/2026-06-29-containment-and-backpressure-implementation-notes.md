# Implementation Notes: Containment and Backpressure

Running record of how the implementation diverges from or interprets the design
doc. Append-only.

## Phase 0: Orchestration reconciliation (Option A)

### Design decisions
- The design doc body originally described `stream-json` cost capture with
  `max-cost-usd` / `max-total-tokens` (Data Model, API Design, Phase 3), but the
  doc's own Addendum ("Streaming Cost Capture - Considered, Deferred") is the
  final 5/5-review decision and adopts **Option A**: wall-clock cap only, no
  `stream-json` switch, no cost/token capture. Before executing, the orchestrator
  reconciled the stale normative sections (Architecture diagram, Data Model
  `BudgetConfig` + YAML, `RunResult`, exit-code table, API Design, Phase 3,
  Testing Strategy) to Option A so phase implementers are not misled. The
  Addendum is preserved verbatim as the rationale of record.

### Deviations
- None beyond the in-doc Addendum decision itself.

### Tradeoffs
- Reconciling the doc body vs. only instructing implementers: chose to reconcile
  the doc so it is internally consistent and the committed artifact matches the
  code.

### Open questions
- The three Open Questions in the design doc (merge-back `--merge` flag, hard vs
  best-effort OS sandbox, judge determinism) remain unresolved and out of scope
  for v1.

### Prerequisite (not a phase)
- The three "already done separately" fixes the doc assumes (drop `--max-turns 1`,
  close Claude stdin, line-exact promise detection) were sitting uncommitted in
  `src/runner.rs`; committed as `af93736` before Phase 1 to get a clean tree.

## Phase 1: Worktree isolation + fail-closed permission preflight

### Design decisions
- Split the preflight into a pure decision function `preflight_none(sandbox_present,
  permission_bypass, unsafe_opt) -> Result<(), String>` (`src/safety.rs`) separate
  from the side-effecting `resolve_workdir`. The pure function encodes the decision
  table's `isolation: none` rows and is unit-tested exhaustively with no git, no
  filesystem, no PATH access - exactly the "fakes, no real Claude" requirement.
- Introduced an internal `EffectiveIsolation` enum (`src/safety.rs`) to model the
  documented degradation: requested `worktree` becomes effective `none` when CWD is
  not a git repo. The `worktree` rows of the table are unconditionally "run", so the
  preflight (`preflight_none`) is only consulted on the `none` path.
- "OS sandbox detected" is `bwrap` AND `socat` on PATH - `SANDBOX_BINARIES` const in
  `src/safety.rs::sandbox_present`. The design names the bubblewrap *package*; its
  binary is `bwrap`, so the PATH probe uses `bwrap` (this is what `which bwrap`
  resolves on the host).
- Branch name `rwl/<plan-slug>-<timestamp>` - `src/safety.rs::resolve_workdir` builds
  it; `plan_slug` slugifies the plan file stem (lowercase, non-alphanumeric runs ->
  single hyphen, trimmed, empty -> `plan`). The timestamp reuses the session dir's
  own timestamp component so branch and session line up.
- Worktree created via `git worktree add <session_dir>/worktree -b <branch> HEAD`
  run from CWD (`src/safety.rs::create_worktree`). Verified this works from this
  repo's bare-container layout (`.bare` common dir, `main` worktree) - git resolves
  the shared object store automatically, so the new branch lands in `.bare` and
  survives the /tmp worktree.
- Plan path canonicalized to absolute in `src/commands/run.rs::run` before
  `resolve_workdir`, so the loop reads the live plan regardless of work_dir (the
  worktree may not contain an uncommitted plan).
- Baseline `rwl: session setup` commit made by `src/commands/run.rs::baseline_commit`
  only when isolation produced a worktree (committing rwl's own setup into the user's
  real tree under `isolation: none` would pollute it). The commit reuses
  `GitManager::auto_commit`.
- `--isolation` CLI flag is a `ValueEnum` `IsolationArg` with `ignore_case = true`
  and `rename_all = "kebab-case"` (`src/cli.rs`), per the CLI rules; `From<IsolationArg>
  for Isolation` bridges to config. `--unsafe` is `unsafe_opt: bool` (the field can't
  be named `unsafe`, a Rust keyword) with `#[arg(long = "unsafe")]`.
- `RunResult.branch: Option<String>` added with `skip_serializing_if = "Option::is_none"`
  (`src/result.rs`); only this field added, no cost/token fields (Option A).
- `branch:` line printed after `print_outcome` for every terminal outcome
  (`src/commands/run.rs::run`), satisfying "on finish/failure print branch and leave
  the worktree for review" - the worktree is never removed.

### Deviations
- Added `--unsafe` to the three existing `tests/integration.rs` loop-mechanics tests.
  Those run in a non-git temp dir with `dangerously_skip_permissions: true`; under the
  new safe default (worktree -> degrades to none -> fail-closed refuse) they would now
  exit 4. `--unsafe` is the documented opt-out and keeps each test focused on loop
  outcome, not containment. This is a test-only change forced by the new default.
- `SafetyConfig` carries `protected_paths` now (with the doc's baseline default) even
  though the guard that consumes it is Phase 2. The field is part of the Phase 1 Data
  Model bullet, so it is added now; it is simply unused until Phase 2 (no `#[allow(dead_code)]`
  needed since it is a public serde field).

### Tradeoffs
- Pure `preflight_none(bool, bool, bool)` vs. passing the whole config/env: chose the
  three-bool signature so the decision table is the test surface directly and the
  function has zero I/O. Sandbox/git probing lives in `resolve_workdir`, which the
  integration tests exercise end-to-end.
- Integration test `test_uncontained_bypass_without_unsafe_refuses_exit_4` asserts the
  refusal by forcing `--isolation none`; it is host-dependent (would not refuse if both
  `bwrap` and `socat` were installed). The deterministic coverage is the pure unit tests;
  the integration test is a real-binary smoke check on this host (socat absent).
- Kept Phase 1 tests inline as `#[cfg(test)] mod tests` to match every existing module
  in this repo (config.rs, git.rs, runner.rs, etc.), rather than introducing the
  `mod tests;` + `tests.rs` split mid-feature (that is a tree-wide mechanical pass, not
  a per-phase change).

### Open questions
- None for this phase. The doc's standing Open Questions (merge-back `--merge`, hard vs
  best-effort sandbox, judge determinism) remain out of scope for v1.

## Phase 2: Immutable path boundaries

### Design decisions
- Matching semantics are **conservative directory-prefix**, not full glob - `safety::is_protected` (src/safety.rs). The documented protected-path entries (`.git/`, `.rwl/`, `docs/design/`) are directory prefixes; a candidate matches when it equals the prefix (trailing slash trimmed) or lives under `<prefix>/`. Leading `./` is normalized away on both sides. This deliberately avoids a new glob dependency (none was present) and avoids the footgun where `docs/designs.md` would match `docs/design` - the `/` boundary check rejects it. A unit test pins exactly that non-match.
- `guard_protected` runs after the Claude call and before auto-commit - `runner::LoopRunner::run` step 4 (src/runner.rs), per the Architecture diagram ordering (protected-path guard -> auto-commit). Because Phase 1 makes the `rwl: session setup` baseline commit, every protected-path delta against HEAD can only originate from the agent.
- Untracked additions (`??`) are removed with git's own clean machinery (`safety::git_clean_path`, src/safety.rs); everything else (modifications, deletions, staged changes) is restored with `git checkout -- <path>` (`safety::git_checkout_path`, src/safety.rs). Removal uses git, never shell rm, per the repo safety rule. The `-d` flag is included so a newly created protected directory is cleaned with its contents.
- Symlink and under-root safety invariants live in `guard_protected` (src/safety.rs): each candidate is symlink_metadata-checked and skipped if it is a symlink; then the path (or its nearest existing ancestor, for deletions) is canonicalized and asserted to start with the canonical worktree root before any git operation. A path that resolves outside root is skipped with a warn.
- Reverted paths are recorded as a feedback `IterationResult` appended to `progress.txt` - `runner::LoopRunner::guard_protected_paths` (src/runner.rs) - reusing the existing `ProgressTracker::log_iteration` mechanism so the NEXT iteration's prompt explains the boundary.
- The protected-path list is injected into the prompt template under a new `{{#if protected_paths}}` "Off-Limits Paths" block - `templates/prompt.rs` (PROMPT_TEMPLATE) and `runner::LoopRunner::build_prompt` (src/runner.rs), which renders the list as `- <path>` bullets into the `protected_paths` template var.

### Deviations
- Spec says filter against globs / "glob matching"; implemented as conservative directory-prefix matching instead of full glob, documented above. The configured entries are all directory prefixes, so prefix matching covers every documented case without a glob crate. Recorded here as the matching-semantics decision the phase brief asked for.
- Added a no-op guard when `work_dir` is not a git repo (`GitManager::is_repo()` check at the top of `guard_protected`) - mirrors the existing `git_auto_commit` is_repo guard. Without it, the `isolation: none` non-repo integration paths produced git status failures that propagated to exit 4. Not in the spec text but required for correctness parity with auto-commit.

### Tradeoffs
- Prefix matching vs. a real glob engine: chose prefix matching to keep zero new dependencies and because every documented protected entry is a directory prefix. A future entry needing wildcards would require revisiting `is_protected`; flagged so the next reader knows the boundary.
- Deletion handling canonicalizes the nearest existing ancestor (the path is gone) and asserts IT is under root, then `git checkout` restores from HEAD. Alternative was to skip deletions entirely, but a deleted protected file is exactly the kind of edit the guard must revert, so restoring is correct.
- Kept the Phase 2 tests inline as `#[cfg(test)] mod tests` in src/safety.rs to match every existing module in this repo and the Phase 1 entry's precedent, rather than introducing the `mod tests;` + `tests.rs` split mid-feature.

### Open questions
- None for this phase.

## Phase 3: Budget stop conditions (wall-clock only, Option A)

### Design decisions
- `BudgetConfig { max_total_minutes: u64 }` — src/config.rs — serde `default, rename_all = "kebab-case", deny_unknown_fields`; `0 = unlimited`. Wired into `Config.budget` with `#[serde(default)]` and `Config::default()`. Used a `#[derive(Default)]` (clippy `derivable_impls` rejected a hand-written `impl Default` since `0 == u64::default()`).
- `Budget { start: Instant, cap_minutes: u64 }` — src/budget.rs — monotonic `std::time::Instant`. `exceeded()` delegates to a private `exceeded_at(elapsed)` so the threshold logic is unit-testable without a clock. `Reason { elapsed_minutes, cap_minutes }` carries both numbers and a `Display` that names `max-total-minutes`, so the surfaced/JSON message is machine- and human-readable.
- Budget check placement — src/runner.rs `LoopRunner::run` — `Budget::start(config.budget.max_total_minutes)` after the initial config load; the cap is checked at the TOP of each iteration (step "0b", after the Ctrl-C check, before building the prompt / spawning Claude) per the Architecture diagram. On trip: WIP auto-commit (if enabled), session log, and return `BudgetExceeded { iterations: iteration - 1, reason }`.
- Logging — src/runner.rs `run` — single `log::debug!` when the cap trips, `log::trace!` for the per-iteration "within budget" line (per the per-iteration spam guidance). `exceeded`/`exceeded_at` are trivial helpers and intentionally carry no entry/exit logging.
- `LoopOutcome::BudgetExceeded { iterations, reason }` — src/runner.rs — exit code 5; `outcome_name() == "budget-exceeded"`; `error_message()` returns the reason. Surfaced in src/commands/run.rs `print_outcome` with a yellow "Budget Exceeded" banner, the reason, and a hint to adjust `max-total-minutes`.

### Deviations
- None. `run_claude` and `RunResult` are unchanged (no cost/token/stream-json), per Option A.

### Tradeoffs
- Added a test-only env hook `RWL_BUDGET_PREAGE_SECS` (read once in `Budget::start`) that pre-ages the start instant — src/budget.rs — chosen over a `#[cfg(test)]` constructor (unreachable from an end-to-end integration test through the real binary) and over real-minute sleeping (banned by the task). The integration test sets `max-total-minutes: 1` and `RWL_BUDGET_PREAGE_SECS=120` so the cap trips at the top of iteration 1, deterministically and instantly. The hook is inert unless the env var parses as a `u64`.
- Kept the new unit tests inline as `#[cfg(test)] mod tests` in src/budget.rs and src/config.rs to match every existing module in this repo and the Phase 1/2 precedent, rather than introducing the `mod tests;` + `tests.rs` split mid-feature.

### Open questions
- None for this phase.

## Phase 4: LLM-as-judge gate

### Design decisions
- `JudgeConfig { model, signal, prompt }` added to `src/config.rs` with `serde(rename_all = "kebab-case", deny_unknown_fields)`. In `Config` it is `judge: Option<JudgeConfig>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Absent means no judge runs, exactly mirroring how `quality_gates` being a non-empty vec = active.
- `promise_found` in `src/runner.rs` renamed to `signal_on_own_line` and made `pub(crate)` - `src/runner.rs::signal_on_own_line`. No logic change; the rename makes the shared intent clear (it was always generic, the name was domain-specific). All `runner.rs` tests updated to use the new name.
- `src/judge.rs` contains two layers: `detect_verdict(output, signal) -> bool` (pure, testable, delegates to `signal_on_own_line`) and `run_judge(&JudgeConfig, work_dir, dangerously_skip_permissions) -> Result<(bool, String)>` (spawns the subprocess). The pure layer is separately unit-tested with fixture strings; the subprocess layer is not tested against real Claude (per the constraint).
- `extract_explanation(output, signal) -> String` in `src/judge.rs` strips the signal line from the judge's output and returns the remainder trimmed. Used by the caller to populate `progress.txt` feedback on FAIL.
- Judge wired in `src/runner.rs::LoopRunner::run` - the judge runs only when `config.judge.is_some()` AND `gate_result.all_passed`, inside the quality-gate branch. On judge PASS: falls through to `Complete`. On judge FAIL: appends a `ProgressTracker` feedback entry and `continue`s the loop. On judge error: treats as soft FAIL (warn + continue), avoiding a hard `Error` outcome for a transient subprocess failure.
- The judge invocation mirrors `run_claude` exactly: `--print`, `--model <judge.model>`, `--dangerously-skip-permissions` only when `config.llm.dangerously_skip_permissions`, `stdin(Stdio::null())`, stdout+stderr drained concurrently via background threads. Wall-clock timeout is `JUDGE_TIMEOUT_SECS = 600` (10 minutes) - `src/judge.rs`.
- Logging follows repo rules: DEBUG on `run_judge` entry with `model`, `prompt_len` (never the full prompt), `signal`, and `work_dir`. DEBUG on PASS verdict; WARN on FAIL verdict with `output_len`. Per-iteration "judge gate active" DEBUG in `runner.rs::run`.

### Deviations
- `promise_found` renamed to `signal_on_own_line` to make sharing explicit. This is a rename-only refactor with no behavioral change; it only renames the module-private function and updates its call sites (the `find_promise` method and the inline tests). The design doc says "generalize/share" - a rename + visibility promotion is the minimum intervention.
- `run_judge` signature takes `dangerously_skip_permissions: bool` as a parameter rather than the full `Config`. This keeps `judge.rs` from depending on `Config` beyond `JudgeConfig`, making the module boundary cleaner. The caller in `runner.rs` reads `config.llm.dangerously_skip_permissions` and passes it through.

### Tradeoffs
- Pure `detect_verdict` vs. testing `run_judge` end-to-end: chose the pure function layer so verdict-parsing logic is covered by fast unit tests with no subprocess. `run_judge` itself is integration-grade (requires real Claude); the constraint says "no real Claude in unit tests," so the pure layer is the correct seam.
- Soft vs. hard judge error: a judge subprocess error (timeout, spawn failure) is treated as a soft FAIL (continue the loop) rather than a hard `Error` outcome. Rationale: the judge is optional and its availability should not strand a run that has already passed all programmatic gates. A hard `Error` would be surprising to a user who configured the judge but had a transient Claude outage. The warn log records the event; the next iteration may succeed.
- `skip_serializing_if = "Option::is_none"` on `Config.judge`: keeps serialized YAML clean for users who have not configured the judge (no `judge: null` appearing in output). Mirrors the pattern used for `RunResult.branch` and `RunResult.error`.

### Open questions
- Judge determinism (doc Open Question 3): pinning temperature or model version for reproducibility is not addressed; the `model` field in `JudgeConfig` lets the user control this, but no temperature/seed is exposed. Noted for v2.
- The judge soft-error policy (continue on spawn failure) may mask a misconfigured judge section. A future improvement could add a `--judge-strict` flag that promotes judge errors to hard failures.

## Phase 5: Config migration, docs, tests

### Design decisions
- `rwl init` config append strategy - `src/commands/init.rs` - The `judge:` section is intentionally absent from the serialized YAML (Config.judge is None, serde skips it via `skip_serializing_if`). A `JUDGE_EXAMPLE_COMMENT` const is appended to the file after saving, providing an illustrative commented block so first-time users see the shape without needing to read the design doc. The comment is valid YAML (all lines start with `#`) so the file round-trips through `deny_unknown_fields` without error. Chose append-after-save over a hand-rolled template string for the full config because the serialized output is the authoritative source of truth for the live section defaults.
- `docs/ralph-wiggum-pattern.md` update strategy - Added a new "Safety Envelope and Backpressure (Implemented)" section after the References section describing what `rwl` actually implements (worktree isolation, fail-closed preflight, protected paths, wall-clock budget, optional judge) and an explicit Non-Goals subsection matching the design doc's Non-Goals. Updated the Stop Conditions section to annotate the aspirational `LoopLimits` struct (token/cost caps are not implemented in `rwl`). Updated the State Persistence table to note JSONL/SQLite as aspirational vs. `progress.txt`/`result.json` as implemented. This is accurate and Option-A-correct: no cost/token caps mentioned as implemented.
- Test placement - all Phase 5 tests were added inline in their source modules (`src/commands/init.rs`) matching the repo convention established across all phases.

### Deviations
- None. The design said "commented-out `judge:` example block is fine; document your choice" - chose the comment append approach, documented above.

### Tradeoffs
- Append vs. full template string for init config: append after serde serialization means the live section defaults (safety, budget) are always synchronized with the Rust `Config::default()` values - no drift from a hand-maintained template. The judge comment is additive and does not affect parsing.
- Updated `ralph-wiggum-pattern.md` rather than creating a separate "implementation status" doc: keeping the single doc accurate avoids the "stale spec" problem where a reader sees the aspirational spec and assumes it is all implemented.

### Open questions
- None.

### Test audit: Phase 5 requirements vs. actual coverage

All required Phase 5 tests pre-existed from Phases 1-4. No test was missing.

| Required coverage | Status | Location |
|---|---|---|
| Preflight refusal logic (decision table) | **Pre-existed (Phase 1)** | `src/safety.rs::tests::test_preflight_none_*` (4 tests) |
| Protected-path glob matching | **Pre-existed (Phase 2)** | `src/safety.rs::tests::test_is_protected_*` (4 tests) |
| Symlink rejection in guard | **Pre-existed (Phase 2)** | `src/safety.rs::tests::test_guard_skips_symlink_*` + `test_guard_does_not_revert_path_escaping_root_via_symlink` |
| Budget `exceeded()` thresholds | **Pre-existed (Phase 3)** | `src/budget.rs::tests` (6 tests covering unlimited, under-cap, at-cap, over-cap, pre-aged) |
| Judge verdict parsing | **Pre-existed (Phase 4)** | `src/judge.rs::tests::test_detect_verdict_*` (6 tests) |
| Integration test: exit 5 (budget exceeded) | **Pre-existed (Phase 3)** | `tests/integration.rs::test_budget_exceeded_exits_5` |
| Integration test: exit 4 (fail-closed refusal) | **Pre-existed (Phase 1)** | `tests/integration.rs::test_uncontained_bypass_without_unsafe_refuses_exit_4` |
| `rwl init` round-trip through serde | **ADDED (Phase 5)** | `src/commands/init.rs::tests::test_init_config_round_trips_through_serde` |
| `rwl init` judge comment present | **ADDED (Phase 5)** | `src/commands/init.rs::tests::test_init_config_includes_judge_comment` |

## Post-release audit fixes (v0.2.1)

A review-panel implementation audit of the containment-and-backpressure work found four verified gaps in the protected-path guard. All four were fixed in a single batch; `otto ci` green after.

### Fix #1 [HIGH] — Rename out of a protected dir bypassed the guard

- **What was wrong:** `parse_status_line` (`src/safety.rs`) split a rename/copy line `orig -> dest` and kept ONLY the destination. A `git mv docs/design/plan.md src/plan.md` (porcelain `R  docs/design/plan.md -> src/plan.md`) was therefore judged solely by the unprotected destination, the guard skipped it, and the subsequent `git add .` committed the deletion of the protected source.
- **The fix:** `parse_status_line` now returns a `StatusEntry { xy, dest, orig }` surfacing BOTH sides of a rename/copy (`orig` is `None` for non-rename lines). In `guard_protected`, if EITHER side `is_protected`, the move is fully reverted by `revert_rename` (`src/safety.rs`): the destination is removed from the index and worktree (`git rm -f -- <dest>` via `git_rm_path`) and the source is restored from HEAD (`git checkout HEAD -- <orig>` via `git_checkout_head_path` — plain `git checkout -- <orig>` fails because the rename staged the source as deleted). Both sides are under-root checked first via the shared `under_root` helper.
- **File:function:** `src/safety.rs::parse_status_line`, `::guard_protected`, `::revert_rename`, `::git_rm_path`, `::git_checkout_head_path`.
- **Test:** `src/safety.rs::tests::test_guard_reverts_rename_out_of_protected_dir` against a real temp repo: stages a `git mv` of a committed protected file to an unprotected path, asserts the protected source is restored to its HEAD content and the destination removed, then `git add . && git commit` and `git ls-files` confirms the source remains tracked and the move did not survive. Plus `test_parse_status_line_rename_surfaces_both_sides`.

### Fix #2 [HIGH] — Guard-skipped symlink under a protected path was still committed

- **What was wrong:** the guard refused to traverse/revert through a symlink (security invariant) but only `warn!`+`continue`d, leaving the link on disk; `auto_commit`'s `git add .` then staged and committed it — violating "reverted and fed back, never committed".
- **The fix:** in `revert_candidate` (`src/safety.rs`), a candidate that is a symlink (detected via `symlink_metadata`) now has the LINK ITSELF unlinked with `std::fs::remove_file` (which removes the link, never its target), and is recorded in the reverted list so it is fed back. The link's PARENT is validated under root with `canonicalize` (the link path itself is never canonicalized, which would resolve the target). `git checkout`/`git clean` are never run through it.
- **File:function:** `src/safety.rs::revert_candidate`.
- **Test:** `test_guard_removes_symlink_under_protected_path` (Unix) asserts the link is unlinked, its external target survives untouched, and `git add .` + `git ls-files` confirms the removed symlink is not staged. The former escaping-symlink pinning test became `test_guard_unlinks_symlink_escaping_root_without_touching_target`, asserting the link (whose parent is under root) is unlinked while its escaping target dir survives intact.

### Fix #3 [MEDIUM] — Ignored files under protected paths were neither reverted nor fed back

- **What was wrong:** `git status --porcelain` omits git-ignored files, so an agent writing into an ignored subtree under a protected path was invisible to the guard.
- **The fix:** the guard's status call is now `git status --porcelain --ignored`; ignored entries (status `!!`) are classified `RevertKind::Ignored` and removed with `git clean -f -d -x` (the `-x`, threaded through `git_clean_path`'s new `include_ignored` parameter, is required — without it `git clean` will not remove ignored files). The under-root + symlink-unlink invariants apply to these too. Safe because rwl's own progress/log files live in the session dir OUTSIDE the worktree, so `--ignored` can only surface agent-created files.
- **File:function:** `src/safety.rs::guard_protected`, `::RevertKind`, `::git_clean_path`.
- **Test:** `test_guard_removes_ignored_file_under_protected_path` writes a `.rwl/.gitignore` ignoring `logs/`, then an agent file under `.rwl/logs/`, and asserts it is reverted and reported.

### Fix #4 [LOW] — Missing "isolation on + auto_commit off" preflight warning

- **What was wrong:** the design's risk table promised a `warn!` when isolation is on but auto-commit is off (work would be stranded in the throwaway worktree with no commits on the review branch); it was never implemented.
- **The fix:** in `src/commands/run.rs::run`, after `resolve_workdir`, if a worktree branch was produced (`branch.is_some()`) AND `config.git.auto_commit == false`, a clear `warn!` is emitted recommending auto-commit stay on.
- **File:function:** `src/commands/run.rs::run`.
- **Test:** none (a log line on a config combination, with no natural pure-function seam — `run` orchestrates I/O end to end). Noted as a deviation below.

### Deviations
- Fix #4 has no unit test (log-line-only branch inside the I/O-heavy `run` orchestrator; no pure seam to assert against without a fake-logger harness that does not currently exist).

### Open questions
- None.

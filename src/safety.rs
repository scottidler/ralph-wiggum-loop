use crate::config::{Isolation, SafetyConfig};
use crate::git::GitManager;
use eyre::{Context, Result};
use log::{debug, trace, warn};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Binaries that, when both present on `PATH`, indicate an OS-level sandbox is
/// available - the same dependencies Claude's own sandbox requires. The
/// bubblewrap package installs the `bwrap` binary.
const SANDBOX_BINARIES: [&str; 2] = ["bwrap", "socat"];

/// The resolved working directory for a run, plus the worktree branch (if any).
///
/// When isolation is `worktree`, `path` points at the throwaway worktree and
/// `branch` names the dedicated review branch. When isolation is `none`, `path`
/// is the original CWD and `branch` is `None`.
#[derive(Debug, Clone)]
pub struct Workdir {
    pub path: PathBuf,
    pub branch: Option<String>,
}

/// The effective isolation a run achieves, distinct from the *requested*
/// isolation: `worktree` may degrade to `none` when CWD is not a git repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectiveIsolation {
    Worktree,
    None,
}

/// The fail-closed preflight decision for an *uncontained* (`none`) run.
///
/// Pure function of the four table inputs so the decision table can be unit
/// tested without touching git, the filesystem, or `PATH`. `Ok(())` means the
/// run is allowed to proceed; `Err(reason)` is the refusal message.
fn preflight_none(sandbox_present: bool, permission_bypass: bool, unsafe_opt: bool) -> Result<(), String> {
    debug!(
        "preflight_none: sandbox_present={} permission_bypass={} unsafe_opt={}",
        sandbox_present, permission_bypass, unsafe_opt
    );

    // No permission bypass: prompts gate writes (may stall, but is contained by consent).
    if !permission_bypass {
        debug!("preflight_none: no permission bypass -> allow");
        return Ok(());
    }

    // Permission bypass is active. Allowed only if a sandbox contains writes,
    // or the user explicitly opted out of containment.
    if sandbox_present {
        debug!("preflight_none: bypass + sandbox present -> allow (sandbox contains writes)");
        return Ok(());
    }
    if unsafe_opt {
        warn!("preflight_none: bypass + no sandbox + --unsafe -> allow (explicit opt-out)");
        return Ok(());
    }

    warn!("preflight_none: bypass + no sandbox + no --unsafe -> REFUSE");
    Err(refusal_message())
}

/// The refusal message naming every safe option, per the design's decision table.
fn refusal_message() -> String {
    "refusing to run a permission-bypassed agent against an uncontained working tree.\n\
     The agent runs with --dangerously-skip-permissions and nothing confines its writes.\n\
     Choose one of:\n  \
       - isolation: worktree (the default) to run in a throwaway git worktree\n  \
       - install an OS sandbox (bwrap + socat on PATH)\n  \
       - pass --unsafe to explicitly opt out of containment"
        .to_string()
}

/// Detect an OS sandbox: both bubblewrap (`bwrap`) and `socat` on `PATH`.
fn sandbox_present() -> bool {
    let present = SANDBOX_BINARIES.iter().all(|bin| which::which(bin).is_ok());
    debug!("sandbox_present: binaries={:?} present={}", SANDBOX_BINARIES, present);
    present
}

/// Slugify a plan-file stem into a branch-name-safe component.
///
/// Lowercases, replaces any run of non-alphanumeric characters with a single
/// hyphen, and trims leading/trailing hyphens. Empty result falls back to
/// `plan`.
fn plan_slug(plan_path: &Path) -> String {
    let stem = plan_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut slug = String::with_capacity(stem.len());
    let mut prev_hyphen = false;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            slug.push('-');
            prev_hyphen = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "plan".to_string() } else { slug }
}

/// Resolve the working directory for a run, creating a worktree when requested
/// and enforcing the fail-closed permission preflight.
///
/// * `cwd` - the current working directory (the user's repo).
/// * `safety` - the resolved safety config (isolation + protected paths).
/// * `permission_bypass` - the existing `llm.dangerously_skip_permissions` flag.
/// * `unsafe_opt` - the `--unsafe` CLI flag.
/// * `session_dir` - where to co-locate the worktree (`<session_dir>/worktree`).
/// * `plan_path` - used to derive the branch name (`rwl/<plan-slug>-<timestamp>`).
/// * `timestamp` - the session timestamp component of the branch name.
///
/// Returns the resolved [`Workdir`], or an `Err` (propagating to exit 4) when
/// the preflight refuses an uncontained, permission-bypassed run.
pub fn resolve_workdir(
    cwd: &Path,
    safety: &SafetyConfig,
    permission_bypass: bool,
    unsafe_opt: bool,
    session_dir: &Path,
    plan_path: &Path,
    timestamp: &str,
) -> Result<Workdir> {
    debug!(
        "resolve_workdir: cwd={} isolation={:?} permission_bypass={} unsafe_opt={} session_dir={} plan_path={}",
        cwd.display(),
        safety.isolation,
        permission_bypass,
        unsafe_opt,
        session_dir.display(),
        plan_path.display()
    );

    // Determine the *effective* isolation: worktree degrades to none when CWD
    // is not a git repo (worktree is impossible there).
    let git = GitManager::new(cwd);
    let effective = match safety.isolation {
        Isolation::Worktree => {
            if git.is_repo() {
                EffectiveIsolation::Worktree
            } else {
                warn!("resolve_workdir: isolation=worktree but cwd is not a git repo -> falling back to none");
                EffectiveIsolation::None
            }
        }
        Isolation::None => EffectiveIsolation::None,
    };

    match effective {
        EffectiveIsolation::Worktree => {
            // Worktree contains writes; the preflight always permits it.
            let branch = format!("rwl/{}-{}", plan_slug(plan_path), timestamp);
            let worktree_path = session_dir.join("worktree");
            create_worktree(cwd, &worktree_path, &branch)?;
            debug!(
                "resolve_workdir: created worktree at {} on branch {}",
                worktree_path.display(),
                branch
            );
            Ok(Workdir {
                path: worktree_path,
                branch: Some(branch),
            })
        }
        EffectiveIsolation::None => {
            let sandbox = sandbox_present();
            preflight_none(sandbox, permission_bypass, unsafe_opt).map_err(|reason| eyre::eyre!(reason))?;
            debug!("resolve_workdir: isolation=none -> using cwd {}", cwd.display());
            Ok(Workdir {
                path: cwd.to_path_buf(),
                branch: None,
            })
        }
    }
}

/// Create a git worktree at `worktree_path` on a new `branch` from HEAD.
///
/// Runs from `cwd`; git resolves the shared object store automatically, so this
/// works from a bare-container worktree (the branch lands in the common store).
fn create_worktree(cwd: &Path, worktree_path: &Path, branch: &str) -> Result<()> {
    debug!(
        "create_worktree: cwd={} worktree_path={} branch={}",
        cwd.display(),
        worktree_path.display(),
        branch
    );

    let output = Command::new("git")
        .args(["worktree", "add"])
        .arg(worktree_path)
        .args(["-b", branch])
        .arg("HEAD")
        .current_dir(cwd)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("create_worktree: git worktree add failed: {}", stderr.trim());
        return Err(eyre::eyre!("git worktree add failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Decide whether a changed path matches any protected-path entry.
///
/// Matching is **conservative directory-prefix** matching, not full glob:
/// the documented protected-path entries (`.git/`, `.rwl/`, `docs/design/`) are
/// directory prefixes. A trailing-slash entry matches the directory itself and
/// everything beneath it; a non-slash entry matches that exact path or anything
/// beneath it as a directory. Comparison is on the repo-relative path git emits
/// (always forward-slash separated), normalized to strip any leading `./`.
fn is_protected(rel_path: &str, protected: &[String]) -> bool {
    let candidate = rel_path.trim_start_matches("./");
    protected.iter().any(|entry| {
        let entry = entry.trim_start_matches("./");
        if entry.is_empty() {
            return false;
        }
        let prefix = entry.trim_end_matches('/');
        // Exact match, or `candidate` lives under `prefix/`.
        candidate == prefix || candidate.starts_with(&format!("{}/", prefix))
    })
}

/// A parsed `git status --porcelain` entry.
///
/// `dest` is the live path on disk (for a rename/copy, the destination). `orig`
/// carries the source side of a rename/copy (`R`/`C`) so the guard can judge AND
/// fully revert a move whose source lives under a protected path; it is `None`
/// for every non-rename line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusEntry {
    xy: String,
    dest: String,
    orig: Option<String>,
}

/// Strip git's conservative surrounding quotes from a porcelain path component.
fn unquote_path(path: &str) -> &str {
    path.trim_matches('"')
}

/// Parse a single `git status --porcelain` line into a [`StatusEntry`].
///
/// Porcelain v1 format is `XY <path>` where `XY` is the two-char status code.
/// Renames/copies emit `orig -> dest`; we surface BOTH sides so a rename OUT of a
/// protected directory (judged only by an unprotected destination otherwise) is
/// still caught and fully reverted. Returns `None` for lines that do not parse.
fn parse_status_line(line: &str) -> Option<StatusEntry> {
    if line.len() < 4 {
        return None;
    }
    let (xy, rest) = line.split_at(2);
    let path_part = rest.trim_start();
    // Rename/copy: "orig -> dest" — surface both the source and the destination.
    let (orig, dest) = match path_part.split_once(" -> ") {
        Some((orig, dest)) => (Some(unquote_path(orig).to_string()), unquote_path(dest)),
        None => (None, unquote_path(path_part)),
    };
    if dest.is_empty() {
        return None;
    }
    Some(StatusEntry {
        xy: xy.to_string(),
        dest: dest.to_string(),
        orig,
    })
}

/// Revert agent edits to protected paths after an iteration's Claude run.
///
/// Runs `git status --porcelain --ignored` in `work_dir`, filters changed paths
/// against the protected-path prefixes, and reverts each match: tracked
/// modifications via `git checkout -- <path>`, new untracked files via
/// `git clean -f -d`, and git-ignored files via `git clean -f -d -x` (the `-x`
/// is required because `git clean` without it never removes ignored files).
/// Because Phase 1 makes a baseline `rwl: session setup` commit, every
/// protected-path delta against `HEAD` can only originate from the agent.
///
/// Renames/copies surface BOTH sides; if EITHER the source or the destination is
/// protected, the move is fully reverted: the source is restored from HEAD and
/// the destination removed, so the protected file is back and the move does not
/// survive the subsequent auto-commit.
///
/// Safety invariants (mandatory, per the design's Security section):
/// * each candidate is canonicalized and asserted to resolve UNDER the worktree
///   root before it is touched — a path escaping via `..` or a symlink is
///   skipped, never reverted;
/// * a symlink under a protected path is never followed or reverted *through*;
///   the link itself is unlinked (`std::fs::remove_file` on the link removes the
///   link, never its target) so `git add .` cannot later commit it.
///
/// Returns the list of repo-relative paths that were reverted, for feeding back
/// into the next iteration's prompt.
pub fn guard_protected(work_dir: &Path, protected: &[String]) -> Result<Vec<String>> {
    debug!(
        "guard_protected: work_dir={} protected_count={}",
        work_dir.display(),
        protected.len()
    );

    if protected.is_empty() {
        debug!("guard_protected: no protected paths configured -> nothing to guard");
        return Ok(Vec::new());
    }

    // No git repo -> no HEAD to revert against; nothing to guard (mirrors
    // git_auto_commit's is_repo guard). This is the `isolation: none` non-repo case.
    if !GitManager::new(work_dir).is_repo() {
        debug!("guard_protected: work_dir is not a git repo -> nothing to guard");
        return Ok(Vec::new());
    }

    // `--ignored` surfaces git-ignored files (status `!!`); without it an agent
    // writing into an ignored subtree under a protected path is invisible here.
    let output = Command::new("git")
        .args(["status", "--porcelain", "--ignored"])
        .current_dir(work_dir)
        .output()
        .context("Failed to run git status for protected-path guard")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("guard_protected: git status failed: {}", stderr.trim());
        return Err(eyre::eyre!("git status failed: {}", stderr.trim()));
    }

    // Canonical worktree root for the under-root assertion.
    let root = work_dir
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize work_dir {}", work_dir.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut reverted = Vec::new();

    for line in stdout.lines() {
        let Some(entry) = parse_status_line(line) else {
            trace!("guard_protected: unparsable status line skipped: {:?}", line);
            continue;
        };

        let dest_protected = is_protected(&entry.dest, protected);
        let orig_protected = entry.orig.as_deref().is_some_and(|o| is_protected(o, protected));

        if !dest_protected && !orig_protected {
            trace!("guard_protected: {} not protected, leaving", entry.dest);
            continue;
        }

        // Rename/copy where either side is protected: fully revert the move —
        // restore the original from HEAD and remove the staged destination — so
        // the protected file is back and the move does not survive auto-commit.
        if let Some(orig) = entry.orig.as_deref() {
            trace!(
                "guard_protected: rename/copy {} -> {} touches a protected path -> reverting move",
                orig, entry.dest
            );
            revert_rename(work_dir, &root, orig, &entry.dest, &mut reverted);
            continue;
        }

        // Non-rename line: classify and revert the single destination path.
        let kind = RevertKind::from_status(&entry.xy);
        let _ = revert_candidate(work_dir, &root, &entry.dest, kind, &mut reverted);
    }

    debug!("guard_protected: reverted {} protected path(s)", reverted.len());
    Ok(reverted)
}

/// How a candidate path should be reverted, derived from its porcelain status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevertKind {
    /// Untracked addition (`??`): remove via `git clean -f -d`.
    Untracked,
    /// Git-ignored (`!!`): remove via `git clean -f -d -x`.
    Ignored,
    /// Tracked modification/deletion/staged change: restore from HEAD.
    Tracked,
}

impl RevertKind {
    fn from_status(xy: &str) -> Self {
        match xy {
            "??" => RevertKind::Untracked,
            "!!" => RevertKind::Ignored,
            _ => RevertKind::Tracked,
        }
    }
}

/// Append `path` to `reverted` only if not already present (renames touch two
/// sides and ignored-vs-untracked classification can overlap on retries).
fn push_unique(reverted: &mut Vec<String>, path: String) {
    if !reverted.contains(&path) {
        reverted.push(path);
    }
}

/// Assert `rel_path` (under `root`) resolves UNDER the worktree root.
///
/// For paths that no longer exist on disk (deletions, staged moves) the nearest
/// existing ancestor is canonicalized instead. Returns `false` (and warns) when
/// the path escapes root or cannot be canonicalized.
fn under_root(root: &Path, rel_path: &str) -> bool {
    let abs_path = root.join(rel_path);
    let probe = if abs_path.exists() {
        abs_path
    } else {
        abs_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf())
    };
    match probe.canonicalize() {
        Ok(canonical) if canonical.starts_with(root) => true,
        Ok(canonical) => {
            warn!(
                "under_root: {} resolves outside worktree root ({}) -> skipping",
                rel_path,
                canonical.display()
            );
            false
        }
        Err(e) => {
            warn!("under_root: cannot canonicalize {} ({}) -> skipping", rel_path, e);
            false
        }
    }
}

/// Fully revert a rename/copy whose source or destination is protected.
///
/// Restores `orig` from HEAD (`git checkout HEAD -- <orig>` — `git checkout --`
/// alone fails because the rename staged the source as deleted) and removes the
/// staged destination (`git rm -f <dest>`), so the protected file is back and
/// the move does not survive the subsequent auto-commit. Both sides are
/// under-root checked first. Each successfully handled side is recorded.
fn revert_rename(work_dir: &Path, root: &Path, orig: &str, dest: &str, reverted: &mut Vec<String>) {
    if under_root(root, dest) {
        match git_rm_path(work_dir, dest) {
            Ok(()) => {
                trace!("revert_rename: removed staged destination {}", dest);
                push_unique(reverted, dest.to_string());
            }
            Err(e) => warn!("revert_rename: failed to remove destination {}: {}", dest, e),
        }
    }
    if under_root(root, orig) {
        match git_checkout_head_path(work_dir, orig) {
            Ok(()) => {
                trace!("revert_rename: restored source {} from HEAD", orig);
                push_unique(reverted, orig.to_string());
            }
            Err(e) => warn!("revert_rename: failed to restore source {}: {}", orig, e),
        }
    }
}

/// Revert one candidate path under the worktree root, enforcing the under-root
/// and symlink invariants, and record it in `reverted` on success.
///
/// A symlink under a protected path is unlinked (never followed/reverted
/// through); other candidates are removed via `git clean` (untracked/ignored) or
/// restored from HEAD (tracked).
fn revert_candidate(
    work_dir: &Path,
    root: &Path,
    rel_path: &str,
    kind: RevertKind,
    reverted: &mut Vec<String>,
) -> Result<()> {
    let abs_path = root.join(rel_path);

    // A symlink under a protected path: unlink the link ITSELF (remove_file on a
    // symlink removes the link, not its target) so `git add .` cannot commit it.
    // Validate the link's PARENT is under root using symlink_metadata — never
    // canonicalize the link path, which would resolve the target.
    if let Ok(meta) = std::fs::symlink_metadata(&abs_path)
        && meta.file_type().is_symlink()
    {
        let parent = abs_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf());
        match parent.canonicalize() {
            Ok(canonical_parent) if canonical_parent.starts_with(root) => match std::fs::remove_file(&abs_path) {
                Ok(()) => {
                    trace!("revert_candidate: unlinked symlink {} (not followed)", rel_path);
                    push_unique(reverted, rel_path.to_string());
                    return Ok(());
                }
                Err(e) => {
                    warn!("revert_candidate: failed to unlink symlink {}: {}", rel_path, e);
                    return Err(eyre::eyre!("failed to unlink symlink {}: {}", rel_path, e));
                }
            },
            Ok(canonical_parent) => {
                warn!(
                    "revert_candidate: symlink {} parent resolves outside root ({}) -> skipping",
                    rel_path,
                    canonical_parent.display()
                );
                return Err(eyre::eyre!("symlink parent outside root: {}", rel_path));
            }
            Err(e) => {
                warn!(
                    "revert_candidate: cannot canonicalize symlink parent for {} ({}) -> skipping",
                    rel_path, e
                );
                return Err(eyre::eyre!("cannot canonicalize symlink parent: {}", rel_path));
            }
        }
    }

    // Canonicalize and assert the target resolves UNDER the worktree root.
    if !under_root(root, rel_path) {
        return Err(eyre::eyre!("path outside root or uncanonicalizable: {}", rel_path));
    }

    let revert_result = match kind {
        RevertKind::Untracked => git_clean_path(work_dir, rel_path, false),
        RevertKind::Ignored => git_clean_path(work_dir, rel_path, true),
        RevertKind::Tracked => git_checkout_path(work_dir, rel_path),
    };

    match revert_result {
        Ok(()) => {
            trace!("revert_candidate: reverted {} (kind={:?})", rel_path, kind);
            push_unique(reverted, rel_path.to_string());
            Ok(())
        }
        Err(e) => {
            warn!("revert_candidate: failed to revert {}: {}", rel_path, e);
            Err(e)
        }
    }
}

/// Restore a tracked path from HEAD: `git checkout -- <path>`.
fn git_checkout_path(work_dir: &Path, rel_path: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["checkout", "--", rel_path])
        .current_dir(work_dir)
        .output()
        .context("Failed to run git checkout for protected-path revert")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("git checkout -- {} failed: {}", rel_path, stderr.trim()));
    }
    Ok(())
}

/// Restore a path from HEAD into both the index and the worktree:
/// `git checkout HEAD -- <path>`.
///
/// Unlike `git checkout -- <path>` (which restores the worktree from the index),
/// the explicit `HEAD` tree-ish is required when the index already records the
/// path as deleted — exactly the case for the source side of a staged rename.
fn git_checkout_head_path(work_dir: &Path, rel_path: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["checkout", "HEAD", "--", rel_path])
        .current_dir(work_dir)
        .output()
        .context("Failed to run git checkout HEAD for protected-path revert")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!(
            "git checkout HEAD -- {} failed: {}",
            rel_path,
            stderr.trim()
        ));
    }
    Ok(())
}

/// Remove a staged/tracked path from the index AND the worktree via git:
/// `git rm -f -- <path>`.
///
/// Used to delete the destination side of a reverted rename (a freshly staged
/// file). Uses git's own removal (never shell `rm`), per the repo safety rule.
fn git_rm_path(work_dir: &Path, rel_path: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["rm", "-f", "--", rel_path])
        .current_dir(work_dir)
        .output()
        .context("Failed to run git rm for protected-path rename revert")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("git rm -f -- {} failed: {}", rel_path, stderr.trim()));
    }
    Ok(())
}

/// Remove a new untracked or ignored path via git: `git clean -f -d [-x] -- <path>`.
///
/// Uses git's own removal (never shell `rm`), per the repo safety rule; `-d` is
/// added so a newly created directory is cleaned along with its contents. When
/// `include_ignored` is set, `-x` is added — without it `git clean` will NOT
/// remove git-ignored files, so an ignored agent file under a protected path
/// would survive the revert.
fn git_clean_path(work_dir: &Path, rel_path: &str, include_ignored: bool) -> Result<()> {
    let mut args: Vec<&str> = vec!["clean", "-f", "-d"];
    if include_ignored {
        args.push("-x");
    }
    args.push("--");
    args.push(rel_path);
    let output = Command::new("git")
        .args(&args)
        .current_dir(work_dir)
        .output()
        .context("Failed to run git clean for protected-path revert")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("git clean -- {} failed: {}", rel_path, stderr.trim()));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    // --- Preflight decision table (pure, no git / no PATH) ---

    #[test]
    fn test_preflight_none_no_bypass_always_runs() {
        // isolation=none, permission bypass off -> prompts gate writes -> run.
        assert!(preflight_none(false, false, false).is_ok());
        assert!(preflight_none(true, false, false).is_ok());
        assert!(preflight_none(false, false, true).is_ok());
    }

    #[test]
    fn test_preflight_none_bypass_with_sandbox_runs() {
        // isolation=none, sandbox present, bypass on -> sandbox contains writes -> run.
        assert!(preflight_none(true, true, false).is_ok());
        assert!(preflight_none(true, true, true).is_ok());
    }

    #[test]
    fn test_preflight_none_bypass_no_sandbox_no_unsafe_refuses() {
        // isolation=none, no sandbox, bypass on, no --unsafe -> REFUSE.
        let result = preflight_none(false, true, false);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        // The refusal must name every safe option.
        assert!(msg.contains("isolation: worktree"));
        assert!(msg.contains("sandbox"));
        assert!(msg.contains("--unsafe"));
    }

    #[test]
    fn test_preflight_none_bypass_no_sandbox_with_unsafe_runs() {
        // isolation=none, no sandbox, bypass on, --unsafe -> explicit opt-out -> run.
        assert!(preflight_none(false, true, true).is_ok());
    }

    // --- plan_slug ---

    #[test]
    fn test_plan_slug_basic() {
        assert_eq!(
            plan_slug(Path::new("2026-06-29-containment.md")),
            "2026-06-29-containment"
        );
    }

    #[test]
    fn test_plan_slug_collapses_and_trims() {
        assert_eq!(plan_slug(Path::new("/some/dir/My Plan!!.md")), "my-plan");
    }

    #[test]
    fn test_plan_slug_empty_falls_back() {
        assert_eq!(plan_slug(Path::new("___.md")), "plan");
        assert_eq!(plan_slug(Path::new("")), "plan");
    }

    // --- resolve_workdir integration with a real temp git repo ---

    fn init_repo(dir: &Path) {
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            Command::new("git").args(&args).current_dir(dir).output().unwrap();
        }
        std::fs::write(dir.join("seed.txt"), "seed").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn test_resolve_workdir_worktree_creates_branch() {
        let repo = tempdir().unwrap();
        init_repo(repo.path());
        let session = tempdir().unwrap();

        let safety = SafetyConfig::default(); // isolation = worktree
        let result = resolve_workdir(
            repo.path(),
            &safety,
            true, // permission bypass
            false,
            session.path(),
            Path::new("my-plan.md"),
            "20260629-120000",
        )
        .unwrap();

        let worktree = session.path().join("worktree");
        assert_eq!(result.path, worktree);
        assert_eq!(result.branch.as_deref(), Some("rwl/my-plan-20260629-120000"));
        assert!(worktree.join("seed.txt").exists());
    }

    #[test]
    fn test_resolve_workdir_worktree_falls_back_to_none_when_not_repo() {
        // Non-git CWD: worktree impossible. With bypass + no --unsafe the
        // preflight governs; sandbox presence depends on the host, so we only
        // assert the documented fallback path is exercised by checking the
        // no-bypass case (always allowed) returns the CWD with no branch.
        let not_repo = tempdir().unwrap();
        let session = tempdir().unwrap();

        let safety = SafetyConfig::default(); // isolation = worktree
        let result = resolve_workdir(
            not_repo.path(),
            &safety,
            false, // no permission bypass -> always allowed under none
            false,
            session.path(),
            Path::new("plan.md"),
            "20260629-120000",
        )
        .unwrap();

        assert_eq!(result.path, not_repo.path());
        assert!(result.branch.is_none());
    }

    #[test]
    fn test_resolve_workdir_none_no_bypass_returns_cwd() {
        let repo = tempdir().unwrap();
        init_repo(repo.path());
        let session = tempdir().unwrap();

        let safety = SafetyConfig {
            isolation: Isolation::None,
            ..SafetyConfig::default()
        };
        let result = resolve_workdir(
            repo.path(),
            &safety,
            false, // no bypass -> allowed
            false,
            session.path(),
            Path::new("plan.md"),
            "20260629-120000",
        )
        .unwrap();

        assert_eq!(result.path, repo.path());
        assert!(result.branch.is_none());
    }

    // --- is_protected (pure prefix matching) ---

    #[test]
    fn test_is_protected_directory_prefix_matches() {
        let protected = vec![".git/".to_string(), ".rwl/".to_string(), "docs/design/".to_string()];
        assert!(is_protected(".rwl/rwl.yml", &protected));
        assert!(is_protected("docs/design/plan.md", &protected));
        assert!(is_protected(".git/config", &protected));
        // Exact directory name (no trailing component).
        assert!(is_protected(".rwl", &protected));
    }

    #[test]
    fn test_is_protected_leading_dot_slash_normalized() {
        let protected = vec![".rwl/".to_string()];
        assert!(is_protected("./.rwl/rwl.yml", &protected));
    }

    #[test]
    fn test_is_protected_non_match() {
        let protected = vec![".git/".to_string(), "docs/design/".to_string()];
        assert!(!is_protected("src/main.rs", &protected));
        assert!(!is_protected("docs/readme.md", &protected));
        // A sibling that merely shares a prefix string but not a path boundary.
        assert!(!is_protected("docs/designs.md", &protected));
        assert!(!is_protected(".gitignore", &protected));
    }

    #[test]
    fn test_is_protected_empty_entry_ignored() {
        let protected = vec!["".to_string()];
        assert!(!is_protected("anything", &protected));
    }

    // --- parse_status_line ---

    #[test]
    fn test_parse_status_line_modified() {
        assert_eq!(
            parse_status_line(" M docs/design/plan.md"),
            Some(StatusEntry {
                xy: " M".to_string(),
                dest: "docs/design/plan.md".to_string(),
                orig: None,
            })
        );
    }

    #[test]
    fn test_parse_status_line_untracked() {
        assert_eq!(
            parse_status_line("?? .rwl/new.txt"),
            Some(StatusEntry {
                xy: "??".to_string(),
                dest: ".rwl/new.txt".to_string(),
                orig: None,
            })
        );
    }

    #[test]
    fn test_parse_status_line_ignored() {
        assert_eq!(
            parse_status_line("!! .rwl/logs/agent.log"),
            Some(StatusEntry {
                xy: "!!".to_string(),
                dest: ".rwl/logs/agent.log".to_string(),
                orig: None,
            })
        );
    }

    #[test]
    fn test_parse_status_line_rename_surfaces_both_sides() {
        // Source AND destination both surfaced so a rename out of a protected
        // dir is still judged against the protected source.
        assert_eq!(
            parse_status_line("R  docs/design/plan.md -> src/plan.md"),
            Some(StatusEntry {
                xy: "R ".to_string(),
                dest: "src/plan.md".to_string(),
                orig: Some("docs/design/plan.md".to_string()),
            })
        );
    }

    #[test]
    fn test_parse_status_line_too_short() {
        assert!(parse_status_line("").is_none());
        assert!(parse_status_line(" M ").is_none());
    }

    // --- guard_protected against a real temp git repo ---

    /// Build a repo with `.rwl/` and `docs/design/` committed (the baseline),
    /// returning the repo dir handle.
    fn init_repo_with_protected() -> tempfile::TempDir {
        let repo = tempdir().unwrap();
        init_repo(repo.path());
        std::fs::create_dir_all(repo.path().join(".rwl")).unwrap();
        std::fs::create_dir_all(repo.path().join("docs/design")).unwrap();
        std::fs::write(repo.path().join(".rwl/rwl.yml"), "isolation: worktree\n").unwrap();
        std::fs::write(repo.path().join("docs/design/plan.md"), "# plan\n").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "rwl: session setup"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        repo
    }

    fn protected_paths() -> Vec<String> {
        vec![".git/".to_string(), ".rwl/".to_string(), "docs/design/".to_string()]
    }

    #[test]
    fn test_guard_reverts_tracked_modification() {
        let repo = init_repo_with_protected();
        // Agent modifies a protected tracked file.
        std::fs::write(repo.path().join("docs/design/plan.md"), "# TAMPERED\n").unwrap();

        let reverted = guard_protected(repo.path(), &protected_paths()).unwrap();

        assert!(reverted.iter().any(|p| p == "docs/design/plan.md"));
        let restored = std::fs::read_to_string(repo.path().join("docs/design/plan.md")).unwrap();
        assert_eq!(restored, "# plan\n");
    }

    #[test]
    fn test_guard_removes_untracked_new_file() {
        let repo = init_repo_with_protected();
        // Agent creates a new file under a protected directory.
        std::fs::write(repo.path().join(".rwl/sneaky.txt"), "evil").unwrap();

        let reverted = guard_protected(repo.path(), &protected_paths()).unwrap();

        assert!(reverted.iter().any(|p| p == ".rwl/sneaky.txt"));
        assert!(!repo.path().join(".rwl/sneaky.txt").exists());
    }

    #[test]
    fn test_guard_leaves_unprotected_changes() {
        let repo = init_repo_with_protected();
        std::fs::write(repo.path().join("src.txt"), "legit work").unwrap();

        let reverted = guard_protected(repo.path(), &protected_paths()).unwrap();

        assert!(reverted.is_empty());
        assert!(repo.path().join("src.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_guard_removes_symlink_under_protected_path() {
        use std::os::unix::fs::symlink;
        let repo = init_repo_with_protected();
        // A target file the symlink points at, outside the protected dir.
        std::fs::write(repo.path().join("outside.txt"), "secret").unwrap();
        // Agent plants a symlink UNDER a protected directory.
        let link = repo.path().join(".rwl/link");
        symlink(repo.path().join("outside.txt"), &link).unwrap();

        let reverted = guard_protected(repo.path(), &protected_paths()).unwrap();

        // The link ITSELF is unlinked (so `git add .` cannot commit it) and
        // recorded as reverted — but its target is NEVER followed/touched.
        assert!(
            reverted.iter().any(|p| p == ".rwl/link"),
            "symlink under protected path must be removed and reported, got {:?}",
            reverted
        );
        assert!(
            !link.exists() && std::fs::symlink_metadata(&link).is_err(),
            "the symlink link itself must be unlinked"
        );
        // The target file is untouched (we removed the link, not the target).
        assert_eq!(
            std::fs::read_to_string(repo.path().join("outside.txt")).unwrap(),
            "secret"
        );

        // And it would not survive a subsequent `git add . && commit`.
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo.path())
            .output()
            .unwrap();
        let ls = Command::new("git")
            .args(["ls-files", "--", ".rwl/link"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&ls.stdout).trim().is_empty(),
            "removed symlink must not be staged for commit"
        );
    }

    #[test]
    fn test_guard_reverts_rename_out_of_protected_dir() {
        // git mv of a committed protected file to an unprotected path. The
        // status line is `R  docs/design/plan.md -> src/plan.md`; judged only by
        // the unprotected destination the guard would skip it and `git add .`
        // would commit the deletion of the protected source.
        let repo = init_repo_with_protected();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        Command::new("git")
            .args(["mv", "docs/design/plan.md", "src/plan.md"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        // Sanity: the move staged as a rename.
        assert!(repo.path().join("src/plan.md").exists());
        assert!(!repo.path().join("docs/design/plan.md").exists());

        let reverted = guard_protected(repo.path(), &protected_paths()).unwrap();

        // The protected source is restored and the destination removed.
        assert!(
            reverted.iter().any(|p| p == "docs/design/plan.md"),
            "rename source restore must be reported, got {:?}",
            reverted
        );
        assert!(
            repo.path().join("docs/design/plan.md").exists(),
            "protected rename source must be restored from HEAD"
        );
        let restored = std::fs::read_to_string(repo.path().join("docs/design/plan.md")).unwrap();
        assert_eq!(restored, "# plan\n");
        assert!(
            !repo.path().join("src/plan.md").exists(),
            "rename destination must be removed so the move does not survive"
        );

        // The move must not survive a subsequent auto-commit: staging + committing
        // leaves the protected file present and the destination absent.
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "post-guard"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        let ls = Command::new("git")
            .args(["ls-files", "--", "docs/design/plan.md", "src/plan.md"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        let tracked = String::from_utf8_lossy(&ls.stdout);
        assert!(
            tracked.contains("docs/design/plan.md"),
            "protected source must remain tracked"
        );
        assert!(
            !tracked.contains("src/plan.md"),
            "moved destination must not be committed"
        );
    }

    #[test]
    fn test_guard_removes_ignored_file_under_protected_path() {
        let repo = init_repo_with_protected();
        // Make a subtree under a protected dir git-ignored.
        std::fs::write(repo.path().join(".rwl/.gitignore"), "logs/\n").unwrap();
        std::fs::create_dir_all(repo.path().join(".rwl/logs")).unwrap();
        // Agent writes into the ignored, protected subtree.
        std::fs::write(repo.path().join(".rwl/logs/agent.log"), "noise").unwrap();

        let reverted = guard_protected(repo.path(), &protected_paths()).unwrap();

        // The ignored entry under the protected path is reverted and reported.
        assert!(
            reverted.iter().any(|p| p.starts_with(".rwl/logs")),
            "ignored file under protected path must be reverted, got {:?}",
            reverted
        );
        assert!(
            !repo.path().join(".rwl/logs/agent.log").exists(),
            "ignored agent file must be removed (git clean needs -x)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_guard_unlinks_symlink_escaping_root_without_touching_target() {
        use std::os::unix::fs::symlink;
        // `escape` holds a file/dir the worktree should NEVER be able to touch.
        let escape = tempdir().unwrap();
        std::fs::write(escape.path().join("target.md"), "do not touch").unwrap();

        let repo = init_repo_with_protected();
        // Plant a symlink under the protected `docs/design/` prefix whose target
        // escapes the worktree root. The link's PARENT is under root, so the
        // link ITSELF is unlinked (removed) — but its escaping target is NEVER
        // followed or operated on (remove_file removes the link, not the target).
        let link_dir = repo.path().join("docs/design/ext");
        symlink(escape.path(), &link_dir).unwrap();

        let reverted = guard_protected(repo.path(), &protected_paths()).unwrap();

        // The link itself is removed and reported; the escaping target survives
        // intact (the security invariant: never operate through the symlink).
        assert!(
            reverted.iter().any(|p| p == "docs/design/ext"),
            "symlink under protected path must be unlinked, got {:?}",
            reverted
        );
        assert!(
            std::fs::symlink_metadata(&link_dir).is_err(),
            "the planted symlink link must be removed"
        );
        assert!(
            escape.path().join("target.md").exists(),
            "the escaping target dir must survive"
        );
        assert_eq!(
            std::fs::read_to_string(escape.path().join("target.md")).unwrap(),
            "do not touch"
        );
    }

    #[test]
    fn test_guard_empty_protected_is_noop() {
        let repo = init_repo_with_protected();
        std::fs::write(repo.path().join("docs/design/plan.md"), "changed").unwrap();
        let reverted = guard_protected(repo.path(), &[]).unwrap();
        assert!(reverted.is_empty());
    }
}

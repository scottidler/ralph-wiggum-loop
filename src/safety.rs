use crate::config::{Isolation, SafetyConfig};
use crate::git::GitManager;
use eyre::{Context, Result};
use log::{debug, warn};
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
}

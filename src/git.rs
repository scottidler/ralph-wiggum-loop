use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitManager {
    repo_root: PathBuf,
}

impl GitManager {
    pub fn new(repo_root: &Path) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
        }
    }

    /// Check if there are uncommitted changes
    pub fn has_changes(&self) -> Result<bool> {
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.repo_root)
            .output()
            .context("Failed to run git status")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(eyre::eyre!("git status failed: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(!stdout.trim().is_empty())
    }

    /// Auto-commit all changes
    pub fn auto_commit(&self, message: &str) -> Result<()> {
        // Stage all changes
        let add_output = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&self.repo_root)
            .output()
            .context("Failed to run git add")?;

        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            return Err(eyre::eyre!("git add failed: {}", stderr));
        }

        // Commit
        let commit_output = Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(&self.repo_root)
            .output()
            .context("Failed to run git commit")?;

        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            // It's okay if there's nothing to commit
            if stderr.contains("nothing to commit") {
                return Ok(());
            }
            return Err(eyre::eyre!("git commit failed: {}", stderr));
        }

        Ok(())
    }

    /// Get recent commit log
    pub fn recent_commits(&self, count: usize) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args(["log", "--oneline", &format!("-{}", count)])
            .current_dir(&self.repo_root)
            .output()
            .context("Failed to run git log")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(eyre::eyre!("git log failed: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.lines().map(|s| s.to_string()).collect())
    }

    /// Check if the current directory is a git repository
    pub fn is_repo(&self) -> bool {
        Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&self.repo_root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_is_repo_false_for_non_repo() {
        let dir = tempdir().unwrap();
        let git = GitManager::new(dir.path());
        assert!(!git.is_repo());
    }

    #[test]
    fn test_is_repo_true_for_repo() {
        let dir = tempdir().unwrap();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let git = GitManager::new(dir.path());
        assert!(git.is_repo());
    }

    #[test]
    fn test_has_changes_false_for_clean_repo() {
        let dir = tempdir().unwrap();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Configure git user for commits
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Create initial commit
        std::fs::write(dir.path().join("test.txt"), "test").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let git = GitManager::new(dir.path());
        assert!(!git.has_changes().unwrap());
    }

    #[test]
    fn test_has_changes_true_for_dirty_repo() {
        let dir = tempdir().unwrap();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Create an untracked file
        std::fs::write(dir.path().join("test.txt"), "test").unwrap();

        let git = GitManager::new(dir.path());
        assert!(git.has_changes().unwrap());
    }
}

use crate::cli::Cli;
use crate::config::Config;
use crate::templates::PROMPT_TEMPLATE;
use colored::*;
use eyre::{Context, Result};
use std::fs;
use std::path::Path;

const GITIGNORE_CONTENT: &str = r#"# RWL generated files
logs/
"#;

/// Commented-out judge example block appended after the serialized config.
///
/// `judge:` is an optional section; absent means no judge gate runs (identical
/// to how an empty `quality_gates:` list disables quality-gate checking). We
/// leave the section out of the serialized default (Config.judge is None, which
/// serde skips) and append this illustrative block so first-time users see the
/// shape without having to read the design doc. The block is fully commented so
/// parsing the file with `deny_unknown_fields` remains valid.
const JUDGE_EXAMPLE_COMMENT: &str = r#"
# Optional LLM-as-judge gate.
# When present, a fresh Claude invocation is run as a final gate after
# validation and quality gates pass. Absent -> no judge runs.
#
# judge:
#   model: opus
#   signal: "<judge>PASS</judge>"
#   prompt: |
#     Review the committed diff against the plan. Output exactly
#     "<judge>PASS</judge>" on its own line if it meets the criteria below,
#     otherwise explain what is missing.
#     Criteria: <your subjective criteria here>
"#;

pub fn run(cli: &Cli) -> Result<()> {
    let work_dir = Path::new(".");

    init(work_dir, cli.config.as_ref())
}

pub fn init(work_dir: &Path, config_path: Option<&std::path::PathBuf>) -> Result<()> {
    let rwl_dir = Config::local_config_dir(work_dir);

    // Check if already initialized
    if rwl_dir.exists() {
        println!(
            "{} .rwl/ already exists. Use {} to reinitialize.",
            "⚠".yellow(),
            "rm -rf .rwl".cyan()
        );
        return Ok(());
    }

    // 1. Create .rwl/ directory
    fs::create_dir_all(&rwl_dir).context("Failed to create .rwl directory")?;
    println!("{} Created {}", "✓".green(), ".rwl/".cyan());

    // 2. Copy default config from global or create with defaults
    let config = if let Some(path) = config_path {
        Config::load(Some(path))?
    } else {
        Config::load_global()?
    };
    config.save_local(work_dir)?;

    // Append the commented judge example so users see the shape without the
    // section being parsed (it is valid YAML comment, deny_unknown_fields safe).
    let config_file = Config::local_config_path(work_dir);
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&config_file)
        .with_context(|| format!("Failed to open {} for appending judge example", config_file.display()))?;
    use std::io::Write;
    f.write_all(JUDGE_EXAMPLE_COMMENT.as_bytes())
        .context("Failed to append judge example comment")?;
    log::debug!("init: appended judge example comment to {}", config_file.display());

    println!("{} Created {}", "✓".green(), ".rwl/rwl.yml".cyan());

    // 3. Create PROMPT.md template
    let prompt_path = rwl_dir.join("PROMPT.md");
    fs::write(&prompt_path, PROMPT_TEMPLATE).context("Failed to write PROMPT.md")?;
    println!("{} Created {}", "✓".green(), ".rwl/PROMPT.md".cyan());

    // 4. Create .gitignore for .rwl/
    let gitignore_path = rwl_dir.join(".gitignore");
    fs::write(&gitignore_path, GITIGNORE_CONTENT).context("Failed to write .gitignore")?;
    println!("{} Created {}", "✓".green(), ".rwl/.gitignore".cyan());

    println!();
    println!("{}", "RWL initialized successfully!".green().bold());
    println!();
    println!("Next steps:");
    println!("  1. Edit {} to customize settings", ".rwl/rwl.yml".cyan());
    println!("  2. Edit {} to customize the prompt", ".rwl/PROMPT.md".cyan());
    println!("  3. Run {} to start the loop", "rwl run --plan <path>".cyan());

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::{Config, Isolation};
    use tempfile::tempdir;

    #[test]
    fn test_init_creates_directory_structure() {
        let dir = tempdir().unwrap();
        init(dir.path(), None).unwrap();

        assert!(dir.path().join(".rwl").exists());
        assert!(dir.path().join(".rwl/rwl.yml").exists());
        assert!(dir.path().join(".rwl/PROMPT.md").exists());
        assert!(dir.path().join(".rwl/.gitignore").exists());
    }

    #[test]
    fn test_init_does_not_overwrite() {
        let dir = tempdir().unwrap();
        let rwl_dir = dir.path().join(".rwl");
        fs::create_dir_all(&rwl_dir).unwrap();

        // Should not error, just warn
        init(dir.path(), None).unwrap();
    }

    /// The generated rwl.yml must round-trip through serde (deny_unknown_fields)
    /// without error and include the new safety/budget sections with safe defaults.
    #[test]
    fn test_init_config_round_trips_through_serde() {
        let dir = tempdir().unwrap();
        init(dir.path(), None).unwrap();

        let config_path = dir.path().join(".rwl/rwl.yml");
        let config = Config::load(Some(&config_path)).unwrap();

        // Safety safe defaults.
        assert_eq!(config.safety.isolation, Isolation::Worktree);
        assert!(config.safety.protected_paths.contains(&".git/".to_string()));
        assert!(config.safety.protected_paths.contains(&".rwl/".to_string()));
        assert!(config.safety.protected_paths.contains(&"docs/design/".to_string()));

        // Budget default: 0 = unlimited.
        assert_eq!(config.budget.max_total_minutes, 0);

        // Judge absent by default.
        assert!(config.judge.is_none());
    }

    /// The generated rwl.yml must contain a commented judge example.
    #[test]
    fn test_init_config_includes_judge_comment() {
        let dir = tempdir().unwrap();
        init(dir.path(), None).unwrap();

        let content = fs::read_to_string(dir.path().join(".rwl/rwl.yml")).unwrap();
        // The comment block must be present, key word on the judge: line.
        assert!(
            content.contains("# judge:"),
            "Missing commented judge example in generated config"
        );
    }
}

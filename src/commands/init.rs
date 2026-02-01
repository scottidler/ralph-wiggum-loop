use crate::cli::Cli;
use crate::config::Config;
use crate::templates::PROMPT_TEMPLATE;
use colored::*;
use eyre::{Context, Result};
use std::fs;
use std::path::Path;

const GITIGNORE_CONTENT: &str = r#"# RWL generated files
progress.txt
logs/
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
mod tests {
    use super::*;
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
}

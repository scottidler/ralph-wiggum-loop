use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Loop configuration settings
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LoopConfig {
    pub max_iterations: u32,
    pub iteration_timeout_minutes: u32,
    pub sleep_between_secs: u64,
    pub completion_signal: String,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            iteration_timeout_minutes: 10,
            sleep_between_secs: 2,
            completion_signal: "<promise>COMPLETE</promise>".to_string(),
        }
    }
}

/// Validation configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ValidationConfig {
    pub command: String,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            command: "otto ci".to_string(),
        }
    }
}

/// A quality gate - either an inline command or a script path (mutually exclusive)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QualityGate {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<PathBuf>,
}

impl QualityGate {
    /// Get the command to execute (resolves script to shell invocation)
    pub fn get_command(&self) -> Result<String> {
        match (&self.command, &self.script) {
            (Some(cmd), None) => Ok(cmd.clone()),
            (None, Some(script)) => Ok(format!("bash {}", script.display())),
            (Some(_), Some(_)) => Err(eyre::eyre!("Gate '{}' has both command and script", self.name)),
            (None, None) => Err(eyre::eyre!("Gate '{}' has neither command nor script", self.name)),
        }
    }
}

/// LLM configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LlmConfig {
    pub model: String,
    pub dangerously_skip_permissions: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: "opus".to_string(),
            dangerously_skip_permissions: true,
        }
    }
}

/// Isolation strategy for containing the agent's blast radius.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Isolation {
    /// Run in a throwaway git worktree on a dedicated branch (default, always safe).
    #[default]
    Worktree,
    /// Run directly in the current working tree (writes are NOT contained).
    None,
}

/// Default protected-path baseline: paths the agent may never modify.
fn default_protected_paths() -> Vec<String> {
    vec![".git/".to_string(), ".rwl/".to_string(), "docs/design/".to_string()]
}

/// Safety / containment configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct SafetyConfig {
    /// Isolation strategy (worktree | none). Defaults to worktree.
    pub isolation: Isolation,
    /// Globs the agent may not modify; protected-path deltas are reverted.
    pub protected_paths: Vec<String>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            isolation: Isolation::default(),
            protected_paths: default_protected_paths(),
        }
    }
}

/// Budget / backpressure configuration.
///
/// Wall-clock only (Option A per the design doc's Addendum): no cost or token
/// caps. A `max_total_minutes` of `0` means unlimited.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct BudgetConfig {
    /// Wall-clock cap across the whole run, in minutes. `0` = unlimited.
    pub max_total_minutes: u64,
}

/// LLM-as-judge configuration (optional).
///
/// When present in `Config.judge`, a fresh Claude invocation is run as a final
/// gate after validation and quality gates pass. Absent -> no judge runs,
/// mirroring how `quality_gates` being configured = active.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct JudgeConfig {
    /// Claude model to use for the judge call (e.g. "opus", "sonnet").
    pub model: String,
    /// Binary verdict token that must appear on its own line (line-exact).
    /// E.g. `"<judge>PASS</judge>"`.
    pub signal: String,
    /// The full prompt sent to the judge Claude invocation.
    pub prompt: String,
}

/// Git configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GitConfig {
    pub auto_commit: bool,
    pub commit_message_template: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            auto_commit: true,
            commit_message_template: "rwl: iteration {iteration}".to_string(),
        }
    }
}

/// Main configuration struct
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "loop")]
    pub loop_config: LoopConfig,
    pub validation: ValidationConfig,
    #[serde(default)]
    pub quality_gates: Vec<QualityGate>,
    pub llm: LlmConfig,
    pub git: GitConfig,
    #[serde(default)]
    pub safety: SafetyConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    /// Optional LLM-as-judge gate. Absent -> no judge runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            loop_config: LoopConfig::default(),
            validation: ValidationConfig::default(),
            quality_gates: vec![
                QualityGate {
                    name: "no_dead_code".to_string(),
                    command: Some("! grep -rn 'allow(dead_code)' src/".to_string()),
                    script: None,
                },
                QualityGate {
                    name: "no_todos".to_string(),
                    command: Some("! grep -rn 'TODO' src/".to_string()),
                    script: None,
                },
            ],
            llm: LlmConfig::default(),
            git: GitConfig::default(),
            safety: SafetyConfig::default(),
            budget: BudgetConfig::default(),
            judge: None,
        }
    }
}

impl Config {
    /// Get the global config directory path
    pub fn global_config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("rwl"))
    }

    /// Get the global config file path
    pub fn global_config_path() -> Option<PathBuf> {
        Self::global_config_dir().map(|d| d.join("rwl.yml"))
    }

    /// Get the local config directory path (relative to work_dir)
    pub fn local_config_dir(work_dir: &Path) -> PathBuf {
        work_dir.join(".rwl")
    }

    /// Get the local config file path (relative to work_dir)
    pub fn local_config_path(work_dir: &Path) -> PathBuf {
        Self::local_config_dir(work_dir).join("rwl.yml")
    }

    /// Load configuration with the cascade: global -> local -> defaults
    pub fn load(config_path: Option<&PathBuf>) -> Result<Self> {
        // If explicit config path provided, try to load it
        if let Some(path) = config_path {
            return Self::load_from_file(path).context(format!("Failed to load config from {}", path.display()));
        }

        // Try local config first (.rwl/rwl.yml)
        let local_config = Self::local_config_path(&PathBuf::from("."));
        if local_config.exists() {
            match Self::load_from_file(&local_config) {
                Ok(config) => return Ok(config),
                Err(e) => {
                    log::warn!("Failed to load config from {}: {}", local_config.display(), e);
                }
            }
        }

        // Try global config (~/.config/rwl/rwl.yml)
        if let Some(global_config) = Self::global_config_path()
            && global_config.exists()
        {
            match Self::load_from_file(&global_config) {
                Ok(config) => return Ok(config),
                Err(e) => {
                    log::warn!("Failed to load config from {}: {}", global_config.display(), e);
                }
            }
        }

        // No config file found, use defaults
        log::info!("No config file found, using defaults");
        Ok(Self::default())
    }

    /// Load global config from ~/.config/rwl/rwl.yml
    pub fn load_global() -> Result<Self> {
        if let Some(global_path) = Self::global_config_path()
            && global_path.exists()
        {
            return Self::load_from_file(&global_path);
        }
        Ok(Self::default())
    }

    /// Load local config from .rwl/rwl.yml relative to work_dir
    pub fn load_local(work_dir: &Path) -> Result<Self> {
        let local_path = Self::local_config_path(work_dir);
        if local_path.exists() {
            Self::load_from_file(&local_path)
        } else {
            Err(eyre::eyre!("No local config found at {}", local_path.display()))
        }
    }

    /// Load configuration from a file
    fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(&path).context("Failed to read config file")?;
        let config: Self = serde_yaml::from_str(&content).context("Failed to parse config file")?;
        log::info!("Loaded config from: {}", path.as_ref().display());
        Ok(config)
    }

    /// Save configuration to a file
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let content = serde_yaml::to_string(self).context("Failed to serialize config")?;
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("Failed to create config directory")?;
        }
        fs::write(&path, content).context("Failed to write config file")?;
        log::info!("Saved config to: {}", path.as_ref().display());
        Ok(())
    }

    /// Save to the local config path (.rwl/rwl.yml)
    pub fn save_local(&self, work_dir: &Path) -> Result<()> {
        let local_path = Self::local_config_path(work_dir);
        self.save(&local_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.loop_config.max_iterations, 100);
        assert_eq!(config.loop_config.iteration_timeout_minutes, 10);
        assert_eq!(config.loop_config.sleep_between_secs, 2);
        assert_eq!(config.loop_config.completion_signal, "<promise>COMPLETE</promise>");
        assert_eq!(config.validation.command, "otto ci");
        assert_eq!(config.llm.model, "opus");
        assert!(config.llm.dangerously_skip_permissions);
        assert!(config.git.auto_commit);
        // Safety defaults: worktree isolation + baseline protected paths.
        assert_eq!(config.safety.isolation, Isolation::Worktree);
        assert_eq!(config.safety.protected_paths, vec![".git/", ".rwl/", "docs/design/"]);
    }

    #[test]
    fn test_safety_defaults_when_section_absent() {
        // A config YAML lacking a `safety:` section must default to worktree.
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
llm:
  model: "opus"
  dangerously_skip_permissions: true
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        assert_eq!(config.safety.isolation, Isolation::Worktree);
        assert_eq!(config.safety.protected_paths, vec![".git/", ".rwl/", "docs/design/"]);
    }

    #[test]
    fn test_safety_parses_kebab_case() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
safety:
  isolation: none
  protected-paths:
    - ".git/"
    - "secrets/"
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        assert_eq!(config.safety.isolation, Isolation::None);
        assert_eq!(config.safety.protected_paths, vec![".git/", "secrets/"]);
    }

    #[test]
    fn test_safety_rejects_unknown_field() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
safety:
  isolation: worktree
  bogus-field: true
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        assert!(Config::load_from_file(&config_path).is_err());
    }

    #[test]
    fn test_budget_default_is_unlimited() {
        let config = Config::default();
        assert_eq!(config.budget.max_total_minutes, 0);
    }

    #[test]
    fn test_budget_defaults_when_section_absent() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
llm:
  model: "opus"
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        assert_eq!(config.budget.max_total_minutes, 0);
    }

    #[test]
    fn test_budget_parses_kebab_case() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
budget:
  max-total-minutes: 45
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        assert_eq!(config.budget.max_total_minutes, 45);
    }

    #[test]
    fn test_budget_rejects_unknown_field() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
budget:
  max-total-minutes: 10
  max-cost-usd: 5.0
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        assert!(Config::load_from_file(&config_path).is_err());
    }

    #[test]
    fn test_judge_absent_when_section_missing() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
llm:
  model: "opus"
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        assert!(config.judge.is_none());
    }

    #[test]
    fn test_judge_parses_when_present() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
judge:
  model: "opus"
  signal: "<judge>PASS</judge>"
  prompt: "Review the diff and output the signal if it meets criteria."
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        let judge = config.judge.unwrap();
        assert_eq!(judge.model, "opus");
        assert_eq!(judge.signal, "<judge>PASS</judge>");
        assert!(!judge.prompt.is_empty());
    }

    #[test]
    fn test_judge_rejects_unknown_field() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");
        let yaml = r#"
judge:
  model: "opus"
  signal: "<judge>PASS</judge>"
  prompt: "some prompt"
  bogus-field: true
"#;
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        assert!(Config::load_from_file(&config_path).is_err());
    }

    #[test]
    fn test_judge_default_is_none() {
        let config = Config::default();
        assert!(config.judge.is_none());
    }

    #[test]
    fn test_quality_gate_command() {
        let gate = QualityGate {
            name: "test".to_string(),
            command: Some("echo hello".to_string()),
            script: None,
        };
        assert_eq!(gate.get_command().unwrap(), "echo hello");
    }

    #[test]
    fn test_quality_gate_script() {
        let gate = QualityGate {
            name: "test".to_string(),
            command: None,
            script: Some(PathBuf::from("./scripts/test.sh")),
        };
        assert_eq!(gate.get_command().unwrap(), "bash ./scripts/test.sh");
    }

    #[test]
    fn test_quality_gate_both_error() {
        let gate = QualityGate {
            name: "test".to_string(),
            command: Some("echo hello".to_string()),
            script: Some(PathBuf::from("./scripts/test.sh")),
        };
        assert!(gate.get_command().is_err());
    }

    #[test]
    fn test_quality_gate_neither_error() {
        let gate = QualityGate {
            name: "test".to_string(),
            command: None,
            script: None,
        };
        assert!(gate.get_command().is_err());
    }

    #[test]
    fn test_save_and_load_config() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");

        let config = Config::default();
        config.save(&config_path).unwrap();

        let loaded = Config::load_from_file(&config_path).unwrap();
        assert_eq!(loaded.loop_config.max_iterations, config.loop_config.max_iterations);
        assert_eq!(loaded.llm.model, config.llm.model);
    }

    #[test]
    fn test_load_from_yaml() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("test-config.yml");

        let yaml = r#"
loop:
  max_iterations: 50
  iteration_timeout_minutes: 5
  sleep_between_secs: 1
  completion_signal: "<done>"

validation:
  command: "cargo test"

quality_gates:
  - name: "custom_check"
    command: "echo ok"

llm:
  model: "sonnet"
  dangerously_skip_permissions: false

git:
  auto_commit: false
  commit_message_template: "custom: {iteration}"
"#;

        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        assert_eq!(config.loop_config.max_iterations, 50);
        assert_eq!(config.loop_config.iteration_timeout_minutes, 5);
        assert_eq!(config.loop_config.completion_signal, "<done>");
        assert_eq!(config.validation.command, "cargo test");
        assert_eq!(config.quality_gates.len(), 1);
        assert_eq!(config.quality_gates[0].name, "custom_check");
        assert_eq!(config.llm.model, "sonnet");
        assert!(!config.llm.dangerously_skip_permissions);
        assert!(!config.git.auto_commit);
    }
}

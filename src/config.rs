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

    /// Save to the global config path (~/.config/rwl/rwl.yml)
    #[allow(dead_code)]
    pub fn save_global(&self) -> Result<()> {
        if let Some(global_path) = Self::global_config_path() {
            self.save(&global_path)
        } else {
            Err(eyre::eyre!("Could not determine global config path"))
        }
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

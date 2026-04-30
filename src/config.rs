use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: u32 = 1;
pub const CONFIG_DIR: &str = ".opendevtui";
pub const CONFIG_FILE: &str = "config";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub autostart: bool,
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    workspace_root: PathBuf,
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            services: Vec::new(),
        }
    }
}

impl ServiceConfig {
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    pub fn resolved_cwd(&self, workspace_root: &Path) -> PathBuf {
        resolve_workspace_path(workspace_root, &self.cwd)
    }

    pub fn validate(&self, workspace_root: &Path, seen_ids: &mut HashSet<String>) -> Result<()> {
        let id = self.id.trim();
        if id.is_empty() {
            bail!("service id is required");
        }
        if !seen_ids.insert(id.to_string()) {
            bail!("duplicate service id: {id}");
        }
        if self.cwd.trim().is_empty() {
            bail!("service `{id}` must define cwd");
        }
        let cwd = self.resolved_cwd(workspace_root);
        if !cwd.exists() || !cwd.is_dir() {
            bail!(
                "service `{id}` has invalid cwd `{}` (resolved `{}`)",
                self.cwd,
                cwd.display()
            );
        }
        if self.command.trim().is_empty() {
            bail!("service `{id}` must define command");
        }
        for key in self.env.keys() {
            if key.trim().is_empty() {
                bail!("service `{id}` contains an empty env key");
            }
        }
        Ok(())
    }
}

impl AppConfig {
    pub fn parse(raw: &str) -> Result<Self> {
        let config: Self = toml::from_str(raw).context("failed to parse TOML config")?;
        Ok(config)
    }

    pub fn to_pretty_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to render TOML config")
    }

    pub fn validate(&self, workspace_root: &Path) -> Result<()> {
        if self.version != CONFIG_VERSION {
            bail!(
                "unsupported config version {} (expected {})",
                self.version,
                CONFIG_VERSION
            );
        }
        let mut seen_ids = HashSet::new();
        for service in &self.services {
            service.validate(workspace_root, &mut seen_ids)?;
        }
        Ok(())
    }
}

impl ConfigStore {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn config_dir(&self) -> PathBuf {
        self.workspace_root.join(CONFIG_DIR)
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir().join(CONFIG_FILE)
    }

    pub fn load(&self) -> Result<AppConfig> {
        let path = self.config_path();
        if !path.exists() {
            return Ok(AppConfig::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let config = AppConfig::parse(&raw)?;
        config.validate(&self.workspace_root)?;
        Ok(config)
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        config.validate(&self.workspace_root)?;
        let raw = config.to_pretty_toml()?;
        let dir = self.config_dir();
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| anyhow!("clock error: {err}"))?
            .as_nanos();
        let tmp_path = dir.join(format!(".config.{nonce}.tmp"));
        fs::write(&tmp_path, raw)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        fs::rename(&tmp_path, self.config_path())
            .with_context(|| format!("failed to replace {}", self.config_path().display()))?;
        Ok(())
    }
}

pub fn resolve_workspace_path(workspace_root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    fn sample_service(root: &Path) -> ServiceConfig {
        let cwd = root.join("service");
        fs::create_dir_all(&cwd).unwrap();
        ServiceConfig {
            id: "web".into(),
            name: "Web".into(),
            cwd: cwd.strip_prefix(root).unwrap().display().to_string(),
            command: "npm".into(),
            args: vec!["run".into(), "dev".into()],
            env: BTreeMap::from([("PORT".into(), "3000".into())]),
            autostart: true,
        }
    }

    #[test]
    fn config_round_trip() {
        let dir = tempdir().unwrap();
        let store = ConfigStore::new(dir.path().to_path_buf());
        let config = AppConfig {
            version: CONFIG_VERSION,
            services: vec![sample_service(dir.path())],
        };

        store.save(&config).unwrap();
        let loaded = store.load().unwrap();

        assert_eq!(loaded, config);
    }

    #[test]
    fn validate_missing_id() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().join("service");
        fs::create_dir_all(&cwd).unwrap();
        let config = AppConfig {
            version: CONFIG_VERSION,
            services: vec![ServiceConfig {
                id: String::new(),
                name: String::new(),
                cwd: "service".into(),
                command: "echo".into(),
                args: vec![],
                env: BTreeMap::new(),
                autostart: false,
            }],
        };

        let err = config.validate(dir.path()).unwrap_err().to_string();
        assert!(err.contains("id is required"));
    }

    #[test]
    fn validate_bad_cwd() {
        let dir = tempdir().unwrap();
        let config = AppConfig {
            version: CONFIG_VERSION,
            services: vec![ServiceConfig {
                id: "bad".into(),
                name: String::new(),
                cwd: "missing".into(),
                command: "echo".into(),
                args: vec![],
                env: BTreeMap::new(),
                autostart: false,
            }],
        };

        let err = config.validate(dir.path()).unwrap_err().to_string();
        assert!(err.contains("invalid cwd"));
    }

    #[test]
    fn validate_missing_command() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().join("service");
        fs::create_dir_all(&cwd).unwrap();
        let config = AppConfig {
            version: CONFIG_VERSION,
            services: vec![ServiceConfig {
                id: "bad".into(),
                name: String::new(),
                cwd: "service".into(),
                command: "   ".into(),
                args: vec![],
                env: BTreeMap::new(),
                autostart: false,
            }],
        };

        let err = config.validate(dir.path()).unwrap_err().to_string();
        assert!(err.contains("must define command"));
    }

    #[test]
    fn validate_duplicate_ids() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().join("service");
        fs::create_dir_all(&cwd).unwrap();
        let service = ServiceConfig {
            id: "dup".into(),
            name: String::new(),
            cwd: "service".into(),
            command: "echo".into(),
            args: vec![],
            env: BTreeMap::new(),
            autostart: false,
        };
        let config = AppConfig {
            version: CONFIG_VERSION,
            services: vec![service.clone(), service],
        };

        let err = config.validate(dir.path()).unwrap_err().to_string();
        assert!(err.contains("duplicate service id"));
    }
}

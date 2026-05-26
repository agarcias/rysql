//! App-wide preferences persisted next to `connections.toml`.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Dark,
    Light,
    #[default]
    System,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    #[serde(default)]
    pub theme: ThemeChoice,
    #[serde(default = "default_history_limit")]
    pub history_limit: usize,
    /// Last directory the user navigated to in any file picker
    /// (Open SQL file / Save as…). Used to seed the next picker so
    /// browsing a deep tree once is enough. Serialised on every change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_browse_dir: Option<PathBuf>,
}

fn default_history_limit() -> usize {
    500
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn locate() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "rysql", "rysql").ok_or(CoreError::NoConfigDir)?;
        let path = dirs.config_dir().join("settings.toml");
        Ok(Self { path })
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<AppSettings> {
        if !self.path.exists() {
            return Ok(AppSettings::default());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        let parsed: AppSettings = toml::from_str(&raw).unwrap_or_default();
        Ok(parsed)
    }

    pub fn save(&self, settings: &AppSettings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(settings)?;
        std::fs::write(&self.path, s)?;
        Ok(())
    }
}

//! Core types and traits for RySQL.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod history;
pub mod secret;
pub mod settings;
pub mod store;

pub use history::{HistoryEntry, HistoryStore};
pub use settings::{AppSettings, SettingsStore, ThemeChoice};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("no config directory available for this platform")]
    NoConfigDir,
    #[error("keyring error: {0}")]
    Keyring(#[from] keyring::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Disabled,
    #[default]
    Preferred,
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub socket: Option<String>,
    #[serde(default)]
    pub tls: TlsMode,
}

impl Default for ConnectionProfile {
    fn default() -> Self {
        Self {
            name: "local".into(),
            host: "127.0.0.1".into(),
            port: 3306,
            user: "root".into(),
            database: None,
            socket: None,
            tls: TlsMode::default(),
        }
    }
}

impl ConnectionProfile {
    /// Stable identifier used as the keyring account.
    pub fn keyring_account(&self) -> String {
        format!("{}@{}:{}:{}", self.name, self.host, self.port, self.user)
    }
}

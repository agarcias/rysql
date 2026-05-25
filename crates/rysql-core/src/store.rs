//! Persistence of [`ConnectionProfile`]s as TOML on disk.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::{ConnectionProfile, CoreError, Result};

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default, rename = "connection")]
    connections: Vec<ConnectionProfile>,
}

#[derive(Debug, Clone)]
pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    /// Locate the store at the platform's standard config dir.
    pub fn locate() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "rysql", "rysql").ok_or(CoreError::NoConfigDir)?;
        let path = dirs.config_dir().join("connections.toml");
        Ok(Self { path })
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<ConnectionProfile>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        let parsed: ProfilesFile = toml::from_str(&raw)?;
        Ok(parsed.connections)
    }

    pub fn save(&self, profiles: &[ConnectionProfile]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = ProfilesFile {
            connections: profiles.to_vec(),
        };
        let toml = toml::to_string_pretty(&file)?;
        std::fs::write(&self.path, toml)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty() {
        let dir = tempdir();
        let store = ProfileStore::at(dir.join("connections.toml"));
        assert!(store.load().unwrap().is_empty());
        store.save(&[]).unwrap();
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn roundtrip_profiles() {
        let dir = tempdir();
        let store = ProfileStore::at(dir.join("connections.toml"));
        let profiles = vec![
            ConnectionProfile {
                name: "prod".into(),
                host: "db.example.com".into(),
                port: 3306,
                user: "app".into(),
                database: Some("orders".into()),
                socket: None,
                tls: crate::TlsMode::Required,
            },
            ConnectionProfile::default(),
        ];
        store.save(&profiles).unwrap();
        assert_eq!(store.load().unwrap(), profiles);
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!("rysql-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}

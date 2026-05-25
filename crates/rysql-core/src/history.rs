//! Persistent query history (newest entry last).

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    /// ISO-8601 UTC timestamp.
    pub timestamp: String,
    /// Connection profile name the statement was run against.
    pub profile: String,
    /// SQL the user wrote (without the auto-LIMIT injection).
    pub sql: String,
    pub success: bool,
    /// Status-bar message describing the outcome.
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(default)]
    entries: Vec<HistoryEntry>,
}

#[derive(Debug, Clone)]
pub struct HistoryStore {
    path: PathBuf,
    limit: usize,
}

impl HistoryStore {
    pub fn locate(limit: usize) -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "rysql", "rysql").ok_or(CoreError::NoConfigDir)?;
        let path = dirs.data_local_dir().join("history.json");
        Ok(Self { path, limit })
    }

    pub fn at(path: impl Into<PathBuf>, limit: usize) -> Self {
        Self {
            path: path.into(),
            limit,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<HistoryEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        let parsed: HistoryFile = serde_json::from_str(&raw).unwrap_or_default();
        Ok(parsed.entries)
    }

    /// Append an entry, trim to `limit`, persist.
    pub fn push(&self, entry: HistoryEntry) -> Result<Vec<HistoryEntry>> {
        let mut entries = self.load().unwrap_or_default();
        // Skip exact duplicate of the immediately previous entry to keep noise down.
        if entries
            .last()
            .is_some_and(|last| last.sql == entry.sql && last.profile == entry.profile)
        {
            return Ok(entries);
        }
        entries.push(entry);
        if entries.len() > self.limit {
            let drop_n = entries.len() - self.limit;
            entries.drain(0..drop_n);
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = HistoryFile {
            entries: entries.clone(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| CoreError::InvalidConfig(format!("history serialize: {e}")))?;
        std::fs::write(&self.path, json)?;
        Ok(entries)
    }

    pub fn clear(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

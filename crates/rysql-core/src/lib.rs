//! Core types and traits for RySQL.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: Option<String>,
    pub socket: Option<String>,
    pub tls: bool,
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
            tls: false,
        }
    }
}

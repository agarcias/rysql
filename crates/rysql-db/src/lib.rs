//! MySQL/MariaDB connection and query layer.

use thiserror::Error;

pub mod actor;
pub mod connect;
pub mod errors;
pub mod query;
pub mod schema;

pub use errors::friendly as friendly_error;

pub use actor::{ActorError, ConnectionStats, DbActor, DbHandle, ExecOutcome, ServerInfo};
pub use connect::{build_pool, test_connection};
pub use query::{Cell, ColumnMeta, QueryResult};
pub use schema::{ObjectKind, SchemaObjects};

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Core(#[from] rysql_core::CoreError),
}

impl DbError {
    /// Short, human-readable form for the status bar.
    pub fn friendly(&self) -> String {
        match self {
            DbError::Sqlx(e) => errors::friendly(e),
            DbError::Core(e) => e.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, DbError>;

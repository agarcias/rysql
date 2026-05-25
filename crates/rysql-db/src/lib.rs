//! MySQL/MariaDB connection and query layer.

use thiserror::Error;

pub mod actor;
pub mod connect;

pub use actor::{ActorError, ConnectionStats, DbActor, DbHandle, ServerInfo};
pub use connect::{build_pool, test_connection};

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Core(#[from] rysql_core::CoreError),
}

pub type Result<T> = std::result::Result<T, DbError>;

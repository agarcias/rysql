//! MySQL/MariaDB connection and query layer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

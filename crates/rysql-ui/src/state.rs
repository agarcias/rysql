//! UI-side state for the active connection (schema cache, confirmation modal).

use std::collections::HashMap;

use rysql_db::{DbHandle, SchemaObjects, ServerInfo};

use crate::results::EditRequest;

#[derive(Debug, Clone, Default)]
pub enum LoadState<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded(T),
    Error(String),
}

#[derive(Default)]
pub struct SchemaState {
    pub databases: LoadState<Vec<String>>,
    pub objects: HashMap<String, LoadState<SchemaObjects>>,
}

pub struct ActiveConnection {
    pub profile_name: String,
    pub handle: DbHandle,
    pub info: ServerInfo,
    pub schema: SchemaState,
}

/// A pending destructive action waiting for user confirmation.
#[derive(Debug, Clone)]
pub struct ConfirmAction {
    pub title: String,
    pub message: String,
    pub sql: String,
    pub kind: PendingExec,
}

#[derive(Debug, Clone)]
pub enum PendingExec {
    DropObject { db: String, name: String },
    Truncate { db: String, name: String },
    DropDatabase { db: String },
    EditCell(EditRequest),
}

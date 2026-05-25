//! UI-side state for the active connection (schema cache, confirmation modal).

use std::collections::HashMap;

use rysql_db::{DbHandle, ObjectKind, SchemaObjects, ServerInfo};

use crate::results::EditRequest;

/// Stable identifier for a schema object across the UI: kind + db + name.
/// Used as the key for `RysqlApp::objects` and the discriminator carried by
/// `DockTab::Object`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectKey {
    pub kind: ObjectKind,
    pub db: String,
    pub name: String,
}

impl ObjectKey {
    pub fn new(kind: ObjectKind, db: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind,
            db: db.into(),
            name: name.into(),
        }
    }
}

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
    DropObject {
        db: String,
        name: String,
    },
    Truncate {
        db: String,
        name: String,
    },
    DropDatabase {
        db: String,
    },
    EditCell(EditRequest),
    /// Re-create a routine (procedure / function / view) from the user's
    /// edited Source body. Both statements are executed by
    /// [`rysql_db::DbHandle::replace_routine`] outside the splitter.
    ReplaceSource {
        key: ObjectKey,
        drop_sql: Option<String>,
        create_sql: String,
    },
    /// Insert a row into an editable result tab. After execute we re-issue
    /// the tab's SELECT (offset 0) so auto_increment / default values land.
    /// The SQL itself lives on the parent `ConfirmAction.sql`.
    InsertRow {
        tab_id: u64,
    },
    /// Delete one or many rows from an editable result tab. `rows` are
    /// indices into `tab.result.rows` (original, not display) — used by
    /// `invalidate_after` to drop them from the local grid without a
    /// server round-trip.
    DeleteRows {
        tab_id: u64,
        rows: Vec<usize>,
    },
    /// Set the same value on the same column across several rows. After
    /// execute we patch the cells locally; no server refresh.
    BulkUpdate {
        tab_id: u64,
        rows: Vec<usize>,
        col_idx: usize,
        /// Either the literal the user typed or the string "NULL" — the
        /// post-exec handler turns it into `Cell::Null` / `Cell::Text`.
        new_value: String,
    },
}

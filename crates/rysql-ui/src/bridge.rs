//! Async ↔ UI bridge: spawn futures on the tokio runtime, deliver results to egui.

use std::future::Future;

use eframe::egui;
use rysql_db::{
    ColumnInfo, DbHandle, ExecOutcome, ForeignKeyInfo, IndexInfo, QueryResult, SchemaObjects,
    ServerInfo,
};

use crate::state::ObjectKey;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

#[derive(Debug)]
pub enum UiEvent {
    TestResult {
        profile: String,
        result: Result<std::time::Duration, String>,
    },
    Connected {
        profile: String,
        handle: DbHandle,
        info: ServerInfo,
    },
    ConnectFailed {
        profile: String,
        error: String,
    },
    DatabasesListed {
        profile: String,
        result: Result<Vec<String>, String>,
    },
    ObjectsListed {
        profile: String,
        db: String,
        result: Result<SchemaObjects, String>,
    },
    ShowCreate {
        profile: String,
        name: String,
        result: Result<String, String>,
    },
    ExecResult {
        profile: String,
        kind: ExecKind,
        result: Result<ExecOutcome, String>,
    },
    QueryResult {
        profile: String,
        label: String,
        /// Original SQL the user typed (without auto-pagination).
        sql: String,
        page_size: u64,
        result: Result<QueryResult, String>,
    },
    PageAppended {
        profile: String,
        tab_id: u64,
        page_size: u64,
        result: Result<QueryResult, String>,
    },
    PrimaryKey {
        profile: String,
        tab_id: u64,
        result: Result<Vec<String>, String>,
    },
    CellEdited {
        profile: String,
        tab_id: u64,
        row: usize,
        col: usize,
        new_value: String,
        result: Result<ExecOutcome, String>,
    },
    /// Sent by `spawn_stream` after the streaming task finishes (normally or
    /// because the stream emitted everything it had). The app uses this to
    /// hide the running indicator and clear its abort handle.
    StreamFinished,
    /// A database export advanced one object. Drives the status-bar progress
    /// line; `done`/`total` count objects (SQL dump) or tables (CSV).
    ExportProgress {
        done: usize,
        total: usize,
        label: String,
    },
    /// A database export finished. `Ok` carries a human-readable destination
    /// summary (file path or folder + file count); `Err` a friendly message.
    ExportFinished {
        result: Result<String, String>,
    },
    ObjectColumnsLoaded {
        profile: String,
        key: ObjectKey,
        result: Result<Vec<ColumnInfo>, String>,
    },
    ObjectIndexesLoaded {
        profile: String,
        key: ObjectKey,
        result: Result<Vec<IndexInfo>, String>,
    },
    ObjectForeignKeysLoaded {
        profile: String,
        key: ObjectKey,
        result: Result<Vec<ForeignKeyInfo>, String>,
    },
    ObjectSourceLoaded {
        profile: String,
        key: ObjectKey,
        result: Result<String, String>,
    },
    /// Result of the auto-issued `SELECT * FROM db.name LIMIT 1000` for an
    /// Object view's Data subtab. Distinct from `QueryResult` so the app
    /// does NOT push an extra `DockTab::Results` for it — the grid is
    /// rendered inside the Object tab.
    ObjectDataLoaded {
        profile: String,
        key: ObjectKey,
        label: String,
        sql: String,
        page_size: u64,
        result: Result<QueryResult, String>,
    },
    /// Column metadata loaded specifically for an Insert modal targeting
    /// a Results tab. Keyed by `tab_id` so the modal can match the response
    /// to its open instance.
    TabColumnsLoaded {
        profile: String,
        tab_id: u64,
        result: Result<Vec<ColumnInfo>, String>,
    },
    /// Result of re-issuing a Results tab's original SELECT (offset 0) to
    /// pick up server-side changes (auto_increment values, default values,
    /// rows added by other clients). The handler replaces the tab's rows
    /// in place.
    TabRefreshed {
        profile: String,
        tab_id: u64,
        page_size: u64,
        result: Result<QueryResult, String>,
    },
}

#[derive(Debug, Clone)]
pub enum ExecKind {
    /// Object inside a database changed; invalidate that db's cached objects.
    AlteredDb(String),
    /// A database was dropped; refresh the full database list.
    DroppedDatabase,
    /// Ad-hoc query from the SQL editor — no cache invalidation needed.
    Adhoc,
    /// The user saved an edited Source for the named object; invalidate its
    /// Source `LoadState` so the next render re-runs `SHOW CREATE` and clear
    /// the edit-mode flag on the matching `ObjectViewState`.
    ReplaceSource(ObjectKey),
    /// A row was inserted into the result tab with this id. The handler
    /// refreshes the tab (or its backing Object data subtab) so server-side
    /// changes (auto_increment, defaults) become visible.
    InsertedRow { tab_id: u64 },
    /// One or many rows were deleted from a result tab; their indices into
    /// `tab.result.rows` (original, not display) are carried so the
    /// handler can drop them locally without a server refresh.
    DeletedRows { tab_id: u64, rows: Vec<usize> },
    /// A bulk UPDATE just landed: same `col_idx` across the listed rows
    /// gets `new_value` (turned into `Cell::Null` or `Cell::Text` by the
    /// handler).
    BulkUpdated {
        tab_id: u64,
        rows: Vec<usize>,
        col_idx: usize,
        new_value: String,
    },
    /// `ALTER TABLE` against the named object touched its columns.
    /// Invalidates the matching ObjectViewState's columns / indexes /
    /// foreign_keys / data so the next render re-fetches.
    AlteredColumns(ObjectKey),
}

pub struct Bridge {
    rt: Handle,
    ctx: egui::Context,
    tx: mpsc::UnboundedSender<UiEvent>,
    rx: mpsc::UnboundedReceiver<UiEvent>,
}

/// Handle passed to [`Bridge::spawn_stream`] tasks to emit events back to the
/// UI thread (with an implicit repaint on every send).
#[derive(Clone)]
pub struct EventEmitter {
    tx: mpsc::UnboundedSender<UiEvent>,
    ctx: egui::Context,
}

impl EventEmitter {
    pub fn send(&self, event: UiEvent) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }
}

impl Bridge {
    pub fn new(rt: Handle, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { rt, ctx, tx, rx }
    }

    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = UiEvent> + Send + 'static,
    {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let event = fut.await;
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }

    /// Spawn a task that may emit multiple [`UiEvent`]s over its lifetime.
    /// Each `send` on the provided emitter triggers a repaint. Returns an
    /// [`AbortHandle`] the caller can keep to cancel the in-flight task.
    pub fn spawn_stream<F, Fut>(&self, f: F) -> AbortHandle
    where
        F: FnOnce(EventEmitter) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let emitter = EventEmitter {
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        };
        let finish_tx = self.tx.clone();
        let finish_ctx = self.ctx.clone();
        let handle = self.rt.spawn(async move {
            f(emitter).await;
            let _ = finish_tx.send(UiEvent::StreamFinished);
            finish_ctx.request_repaint();
        });
        handle.abort_handle()
    }

    pub fn drain(&mut self) -> Vec<UiEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            out.push(event);
        }
        out
    }
}

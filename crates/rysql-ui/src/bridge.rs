//! Async ↔ UI bridge: spawn futures on the tokio runtime, deliver results to egui.

use std::future::Future;

use eframe::egui;
use rysql_db::{DbHandle, ExecOutcome, QueryResult, SchemaObjects, ServerInfo};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

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
}

#[derive(Debug, Clone)]
pub enum ExecKind {
    /// Object inside a database changed; invalidate that db's cached objects.
    AlteredDb(String),
    /// A database was dropped; refresh the full database list.
    DroppedDatabase,
    /// Ad-hoc query from the SQL editor — no cache invalidation needed.
    Adhoc,
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
    /// Each `send` on the provided emitter triggers a repaint, so the UI
    /// reacts to per-statement results in a multi-statement script as they
    /// arrive.
    pub fn spawn_stream<F, Fut>(&self, f: F)
    where
        F: FnOnce(EventEmitter) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let emitter = EventEmitter {
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        };
        self.rt.spawn(async move {
            f(emitter).await;
        });
    }

    pub fn drain(&mut self) -> Vec<UiEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            out.push(event);
        }
        out
    }
}

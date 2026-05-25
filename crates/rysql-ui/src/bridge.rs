//! Async ↔ UI bridge: spawn futures on the tokio runtime, deliver results to egui.

use std::future::Future;

use eframe::egui;
use rysql_db::{DbHandle, ExecOutcome, SchemaObjects, ServerInfo};
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
}

#[derive(Debug, Clone)]
pub enum ExecKind {
    /// Object inside a database changed; invalidate that db's cached objects.
    AlteredDb(String),
    /// A database was dropped; refresh the full database list.
    DroppedDatabase,
}

pub struct Bridge {
    rt: Handle,
    ctx: egui::Context,
    tx: mpsc::UnboundedSender<UiEvent>,
    rx: mpsc::UnboundedReceiver<UiEvent>,
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

    pub fn drain(&mut self) -> Vec<UiEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            out.push(event);
        }
        out
    }
}

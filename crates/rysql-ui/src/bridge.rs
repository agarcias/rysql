//! Async ↔ UI bridge: spawn futures on the tokio runtime, deliver results to egui.

use std::future::Future;

use eframe::egui;
use rysql_db::{DbHandle, ServerInfo};
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

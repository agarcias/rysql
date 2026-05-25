//! Long-lived tokio runtime running on its own thread.

use std::sync::OnceLock;
use std::thread;

use tokio::runtime::{Handle, Runtime};
use tokio::sync::oneshot;

static RUNTIME: OnceLock<Handle> = OnceLock::new();

/// Start a multi-thread tokio runtime on a background thread. Idempotent.
pub fn handle() -> Handle {
    RUNTIME
        .get_or_init(|| {
            let (tx, rx) = oneshot::channel();
            thread::Builder::new()
                .name("rysql-tokio".into())
                .spawn(move || {
                    let rt = Runtime::new().expect("tokio runtime");
                    let _ = tx.send(rt.handle().clone());
                    rt.block_on(std::future::pending::<()>());
                })
                .expect("spawn tokio thread");
            rx.blocking_recv().expect("tokio runtime handle")
        })
        .clone()
}

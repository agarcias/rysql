//! Per-connection actor: owns a [`MySqlPool`] and processes commands from the UI.

use std::time::{Duration, Instant};

use sqlx::{MySqlPool, Row};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Error)]
pub enum ActorError {
    #[error("actor has been shut down")]
    Closed,
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub version: String,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    pub last_latency: Option<Duration>,
    pub server: Option<ServerInfo>,
}

enum DbCommand {
    Ping(oneshot::Sender<Result<Duration, ActorError>>),
    ServerInfo(oneshot::Sender<Result<ServerInfo, ActorError>>),
    Shutdown,
}

/// Clone-able handle to send commands to a [`DbActor`].
#[derive(Debug, Clone)]
pub struct DbHandle {
    tx: mpsc::Sender<DbCommand>,
}

impl DbHandle {
    pub async fn ping(&self) -> Result<Duration, ActorError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DbCommand::Ping(tx))
            .await
            .map_err(|_| ActorError::Closed)?;
        rx.await.map_err(|_| ActorError::Closed)?
    }

    pub async fn server_info(&self) -> Result<ServerInfo, ActorError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DbCommand::ServerInfo(tx))
            .await
            .map_err(|_| ActorError::Closed)?;
        rx.await.map_err(|_| ActorError::Closed)?
    }

    pub fn shutdown(&self) {
        let _ = self.tx.try_send(DbCommand::Shutdown);
    }
}

pub struct DbActor {
    pool: MySqlPool,
    rx: mpsc::Receiver<DbCommand>,
}

impl DbActor {
    /// Spawn the actor on the given runtime and return a handle to it.
    pub fn spawn(rt: &Handle, pool: MySqlPool) -> DbHandle {
        let (tx, rx) = mpsc::channel(32);
        let actor = DbActor { pool, rx };
        rt.spawn(actor.run());
        DbHandle { tx }
    }

    async fn run(mut self) {
        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                DbCommand::Ping(reply) => {
                    let result = ping(&self.pool).await;
                    let _ = reply.send(result);
                }
                DbCommand::ServerInfo(reply) => {
                    let result = server_info(&self.pool).await;
                    let _ = reply.send(result);
                }
                DbCommand::Shutdown => break,
            }
        }
        self.pool.close().await;
    }
}

async fn ping(pool: &MySqlPool) -> Result<Duration, ActorError> {
    let start = Instant::now();
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(start.elapsed())
}

async fn server_info(pool: &MySqlPool) -> Result<ServerInfo, ActorError> {
    let row = sqlx::query("SELECT VERSION() AS v").fetch_one(pool).await?;
    let version: String = row.try_get("v")?;
    Ok(ServerInfo { version })
}

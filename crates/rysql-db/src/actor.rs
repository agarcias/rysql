//! Per-connection actor: owns a [`MySqlPool`] and processes commands from the UI.

use std::time::{Duration, Instant};

use sqlx::{AssertSqlSafe, MySqlPool, Row};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

use crate::query::{self, QueryResult};
use crate::schema::{self, ObjectKind, SchemaObjects};

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

#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub affected_rows: u64,
    pub elapsed: Duration,
}

type Reply<T> = oneshot::Sender<Result<T, ActorError>>;

enum DbCommand {
    Ping(Reply<Duration>),
    ServerInfo(Reply<ServerInfo>),
    ListDatabases(Reply<Vec<String>>),
    ListObjects {
        db: String,
        reply: Reply<SchemaObjects>,
    },
    ShowCreate {
        db: String,
        kind: ObjectKind,
        name: String,
        reply: Reply<String>,
    },
    Execute {
        sql: String,
        reply: Reply<ExecOutcome>,
    },
    Query {
        sql: String,
        reply: Reply<QueryResult>,
    },
    Shutdown,
}

/// Clone-able handle to send commands to a [`DbActor`].
#[derive(Debug, Clone)]
pub struct DbHandle {
    tx: mpsc::Sender<DbCommand>,
}

impl DbHandle {
    async fn request<T>(&self, make: impl FnOnce(Reply<T>) -> DbCommand) -> Result<T, ActorError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .await
            .map_err(|_| ActorError::Closed)?;
        rx.await.map_err(|_| ActorError::Closed)?
    }

    pub async fn ping(&self) -> Result<Duration, ActorError> {
        self.request(DbCommand::Ping).await
    }

    pub async fn server_info(&self) -> Result<ServerInfo, ActorError> {
        self.request(DbCommand::ServerInfo).await
    }

    pub async fn list_databases(&self) -> Result<Vec<String>, ActorError> {
        self.request(DbCommand::ListDatabases).await
    }

    pub async fn list_objects(&self, db: String) -> Result<SchemaObjects, ActorError> {
        self.request(|reply| DbCommand::ListObjects { db, reply })
            .await
    }

    pub async fn show_create(
        &self,
        db: String,
        kind: ObjectKind,
        name: String,
    ) -> Result<String, ActorError> {
        self.request(|reply| DbCommand::ShowCreate {
            db,
            kind,
            name,
            reply,
        })
        .await
    }

    pub async fn execute(&self, sql: String) -> Result<ExecOutcome, ActorError> {
        self.request(|reply| DbCommand::Execute { sql, reply })
            .await
    }

    pub async fn query(&self, sql: String) -> Result<QueryResult, ActorError> {
        self.request(|reply| DbCommand::Query { sql, reply }).await
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
        let (tx, rx) = mpsc::channel(64);
        let actor = DbActor { pool, rx };
        rt.spawn(actor.run());
        DbHandle { tx }
    }

    async fn run(mut self) {
        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                DbCommand::Ping(reply) => {
                    let _ = reply.send(ping(&self.pool).await);
                }
                DbCommand::ServerInfo(reply) => {
                    let _ = reply.send(server_info(&self.pool).await);
                }
                DbCommand::ListDatabases(reply) => {
                    let _ = reply.send(schema::list_databases_user(&self.pool).await);
                }
                DbCommand::ListObjects { db, reply } => {
                    let _ = reply.send(schema::list_objects(&self.pool, &db).await);
                }
                DbCommand::ShowCreate {
                    db,
                    kind,
                    name,
                    reply,
                } => {
                    let _ = reply.send(schema::show_create(&self.pool, &db, kind, &name).await);
                }
                DbCommand::Execute { sql, reply } => {
                    let _ = reply.send(execute(&self.pool, &sql).await);
                }
                DbCommand::Query { sql, reply } => {
                    let _ = reply.send(query::run_query(&self.pool, &sql).await);
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

async fn execute(pool: &MySqlPool, sql: &str) -> Result<ExecOutcome, ActorError> {
    let start = Instant::now();
    let result = sqlx::query(AssertSqlSafe(sql.to_string()))
        .execute(pool)
        .await?;
    Ok(ExecOutcome {
        affected_rows: result.rows_affected(),
        elapsed: start.elapsed(),
    })
}

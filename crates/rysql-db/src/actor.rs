//! Per-connection actor: owns a [`MySqlPool`] and processes commands from the UI.

use std::time::{Duration, Instant};

use sqlx::{AssertSqlSafe, MySqlPool, Row};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

use crate::query::{self, QueryResult};
use crate::schema::{self, ColumnInfo, ForeignKeyInfo, IndexInfo, ObjectKind, SchemaObjects};

#[derive(Debug, Error)]
pub enum ActorError {
    #[error("actor has been shut down")]
    Closed,
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

impl ActorError {
    /// Short, human-readable form. Use this for status-bar messages and
    /// reserve `.to_string()` for logs and tooltips.
    pub fn friendly(&self) -> String {
        match self {
            ActorError::Closed => "Connection was closed".into(),
            ActorError::Sqlx(e) => crate::errors::friendly(e),
        }
    }
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
    PrimaryKey {
        db: String,
        table: String,
        reply: Reply<Vec<String>>,
    },
    ListColumns {
        db: String,
        table: String,
        reply: Reply<Vec<ColumnInfo>>,
    },
    ListIndexes {
        db: String,
        table: String,
        reply: Reply<Vec<IndexInfo>>,
    },
    ListForeignKeys {
        db: String,
        table: String,
        reply: Reply<Vec<ForeignKeyInfo>>,
    },
    /// Re-create a routine (procedure / function / view) by running an
    /// optional `DROP` followed by the user-supplied `CREATE` body. The
    /// body is shipped via the text protocol (`raw_sql`) as a single command,
    /// bypassing the client-side statement splitter — which would otherwise
    /// mis-cut `;` inside `BEGIN ... END` — and avoiding the prepared-statement
    /// protocol, which rejects routine bodies with error 1295.
    ReplaceRoutine {
        drop_sql: Option<String>,
        create_sql: String,
        reply: Reply<ExecOutcome>,
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

    pub async fn primary_key(&self, db: String, table: String) -> Result<Vec<String>, ActorError> {
        self.request(|reply| DbCommand::PrimaryKey { db, table, reply })
            .await
    }

    pub async fn list_columns(
        &self,
        db: String,
        table: String,
    ) -> Result<Vec<ColumnInfo>, ActorError> {
        self.request(|reply| DbCommand::ListColumns { db, table, reply })
            .await
    }

    pub async fn list_indexes(
        &self,
        db: String,
        table: String,
    ) -> Result<Vec<IndexInfo>, ActorError> {
        self.request(|reply| DbCommand::ListIndexes { db, table, reply })
            .await
    }

    pub async fn list_foreign_keys(
        &self,
        db: String,
        table: String,
    ) -> Result<Vec<ForeignKeyInfo>, ActorError> {
        self.request(|reply| DbCommand::ListForeignKeys { db, table, reply })
            .await
    }

    pub async fn replace_routine(
        &self,
        drop_sql: Option<String>,
        create_sql: String,
    ) -> Result<ExecOutcome, ActorError> {
        self.request(|reply| DbCommand::ReplaceRoutine {
            drop_sql,
            create_sql,
            reply,
        })
        .await
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
                DbCommand::PrimaryKey { db, table, reply } => {
                    let _ = reply.send(schema::primary_key_columns(&self.pool, &db, &table).await);
                }
                DbCommand::ListColumns { db, table, reply } => {
                    let _ = reply.send(schema::list_columns(&self.pool, &db, &table).await);
                }
                DbCommand::ListIndexes { db, table, reply } => {
                    let _ = reply.send(schema::list_indexes(&self.pool, &db, &table).await);
                }
                DbCommand::ListForeignKeys { db, table, reply } => {
                    let _ = reply.send(schema::list_foreign_keys(&self.pool, &db, &table).await);
                }
                DbCommand::ReplaceRoutine {
                    drop_sql,
                    create_sql,
                    reply,
                } => {
                    let _ = reply.send(replace_routine(&self.pool, drop_sql, create_sql).await);
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
    // Use the text protocol (`COM_QUERY`) rather than `sqlx::query`, which
    // prepares the statement. MySQL rejects several statements in the
    // prepared-statement protocol with error 1295 ("not supported in the
    // prepared statement protocol yet") — notably `CREATE PROCEDURE`,
    // `CREATE FUNCTION`, `CREATE TRIGGER`, `CREATE EVENT` and compound
    // `BEGIN … END` blocks. Ad-hoc SQL carries no bind parameters, so we lose
    // nothing by skipping prepare.
    let result = sqlx::raw_sql(AssertSqlSafe(sql.to_string()))
        .execute(pool)
        .await?;
    Ok(ExecOutcome {
        affected_rows: result.rows_affected(),
        elapsed: start.elapsed(),
    })
}

/// Drop (best-effort) then create a routine in a single command. The CREATE
/// is sent as one `sqlx::query` so the body can contain `;` inside
/// `BEGIN ... END` without our client-side splitter mis-cutting it. The
/// drop step is best-effort: if it fails (typically because the routine
/// doesn't exist) we still attempt the create — the user's CREATE will
/// fail with a server-side error if something else is wrong.
async fn replace_routine(
    pool: &MySqlPool,
    drop_sql: Option<String>,
    create_sql: String,
) -> Result<ExecOutcome, ActorError> {
    if let Some(drop) = drop_sql {
        let _ = sqlx::raw_sql(AssertSqlSafe(drop)).execute(pool).await;
    }
    let start = Instant::now();
    // `raw_sql` (text protocol): a procedure/function/trigger body cannot be
    // shipped through the prepared-statement protocol — MySQL returns 1295.
    let result = sqlx::raw_sql(AssertSqlSafe(create_sql))
        .execute(pool)
        .await?;
    Ok(ExecOutcome {
        affected_rows: result.rows_affected(),
        elapsed: start.elapsed(),
    })
}

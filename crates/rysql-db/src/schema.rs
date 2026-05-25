//! information_schema-backed metadata queries.

use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, MySqlPool, Row};

use crate::actor::ActorError;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaObjects {
    pub tables: Vec<String>,
    pub views: Vec<String>,
    pub procedures: Vec<String>,
    pub functions: Vec<String>,
    pub triggers: Vec<String>,
    pub events: Vec<String>,
}

impl SchemaObjects {
    pub fn total(&self) -> usize {
        self.tables.len()
            + self.views.len()
            + self.procedures.len()
            + self.functions.len()
            + self.triggers.len()
            + self.events.len()
    }
}

/// Skip MySQL/MariaDB internal schemas in the tree.
fn is_system_schema(name: &str) -> bool {
    matches!(
        name,
        "mysql" | "information_schema" | "performance_schema" | "sys"
    )
}

pub async fn list_databases(pool: &MySqlPool) -> Result<Vec<String>, ActorError> {
    let rows =
        sqlx::query("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME")
            .fetch_all(pool)
            .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.try_get(0)?;
        out.push(name);
    }
    Ok(out)
}

pub async fn list_databases_user(pool: &MySqlPool) -> Result<Vec<String>, ActorError> {
    Ok(list_databases(pool)
        .await?
        .into_iter()
        .filter(|s| !is_system_schema(s))
        .collect())
}

pub async fn list_objects(pool: &MySqlPool, db: &str) -> Result<SchemaObjects, ActorError> {
    let mut out = SchemaObjects::default();

    let tables = sqlx::query(
        "SELECT TABLE_NAME, TABLE_TYPE \
         FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? \
         ORDER BY TABLE_NAME",
    )
    .bind(db)
    .fetch_all(pool)
    .await?;
    for row in tables {
        let name: String = row.try_get(0)?;
        let ttype: String = row.try_get(1)?;
        if ttype.eq_ignore_ascii_case("VIEW") {
            out.views.push(name);
        } else {
            out.tables.push(name);
        }
    }

    let routines = sqlx::query(
        "SELECT ROUTINE_NAME, ROUTINE_TYPE \
         FROM information_schema.ROUTINES \
         WHERE ROUTINE_SCHEMA = ? \
         ORDER BY ROUTINE_NAME",
    )
    .bind(db)
    .fetch_all(pool)
    .await?;
    for row in routines {
        let name: String = row.try_get(0)?;
        let rtype: String = row.try_get(1)?;
        match rtype.to_ascii_uppercase().as_str() {
            "PROCEDURE" => out.procedures.push(name),
            "FUNCTION" => out.functions.push(name),
            _ => {}
        }
    }

    let triggers = sqlx::query(
        "SELECT TRIGGER_NAME FROM information_schema.TRIGGERS \
         WHERE TRIGGER_SCHEMA = ? ORDER BY TRIGGER_NAME",
    )
    .bind(db)
    .fetch_all(pool)
    .await?;
    for row in triggers {
        out.triggers.push(row.try_get(0)?);
    }

    let events = sqlx::query(
        "SELECT EVENT_NAME FROM information_schema.EVENTS \
         WHERE EVENT_SCHEMA = ? ORDER BY EVENT_NAME",
    )
    .bind(db)
    .fetch_all(pool)
    .await?;
    for row in events {
        out.events.push(row.try_get(0)?);
    }

    Ok(out)
}

/// Primary-key column names for a table (in key order). Empty if no PK.
pub async fn primary_key_columns(
    pool: &MySqlPool,
    db: &str,
    table: &str,
) -> Result<Vec<String>, ActorError> {
    let rows = sqlx::query(
        "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY' \
         ORDER BY ORDINAL_POSITION",
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(row.try_get(0)?);
    }
    Ok(out)
}

/// `SHOW CREATE TABLE` / `SHOW CREATE VIEW` / etc. Returns the DDL text.
pub async fn show_create(
    pool: &MySqlPool,
    db: &str,
    kind: ObjectKind,
    name: &str,
) -> Result<String, ActorError> {
    let qualified = format!("`{}`.`{}`", db.replace('`', "``"), name.replace('`', "``"));
    let stmt = match kind {
        ObjectKind::Table => format!("SHOW CREATE TABLE {qualified}"),
        ObjectKind::View => format!("SHOW CREATE VIEW {qualified}"),
        ObjectKind::Procedure => format!("SHOW CREATE PROCEDURE {qualified}"),
        ObjectKind::Function => format!("SHOW CREATE FUNCTION {qualified}"),
        ObjectKind::Trigger => format!("SHOW CREATE TRIGGER {qualified}"),
        ObjectKind::Event => format!("SHOW CREATE EVENT {qualified}"),
    };
    let row = sqlx::query(AssertSqlSafe(stmt)).fetch_one(pool).await?;
    // The DDL column index varies by object kind, but it is always the last text column.
    // Use index 1 for tables/views/triggers/events, 2 for procedures/functions.
    let idx = match kind {
        ObjectKind::Procedure | ObjectKind::Function => 2,
        _ => 1,
    };
    let ddl: String = row.try_get(idx)?;
    Ok(ddl)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
    Procedure,
    Function,
    Trigger,
    Event,
}

impl ObjectKind {
    pub fn short_label(self) -> &'static str {
        match self {
            ObjectKind::Table => "T",
            ObjectKind::View => "V",
            ObjectKind::Procedure => "P",
            ObjectKind::Function => "F",
            ObjectKind::Trigger => "Tr",
            ObjectKind::Event => "E",
        }
    }

    pub fn keyword(self) -> &'static str {
        match self {
            ObjectKind::Table => "TABLE",
            ObjectKind::View => "VIEW",
            ObjectKind::Procedure => "PROCEDURE",
            ObjectKind::Function => "FUNCTION",
            ObjectKind::Trigger => "TRIGGER",
            ObjectKind::Event => "EVENT",
        }
    }
}

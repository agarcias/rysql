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

/// One row in the Structure subtab's "Columns" table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// Full column type as MariaDB/MySQL reports it (e.g. `"varchar(255)"`,
    /// `"decimal(10,2) unsigned"`).
    pub data_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub is_pk: bool,
    /// Free-form column extras: `"auto_increment"`,
    /// `"on update CURRENT_TIMESTAMP"`, etc.
    pub extra: String,
    pub comment: String,
}

/// One row in the Structure subtab's "Indexes" table. A composite index
/// shows up as a single entry with multiple `columns`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    /// `"BTREE"`, `"HASH"`, `"FULLTEXT"`, …
    pub index_type: String,
}

/// One row in the Structure subtab's "Foreign keys" section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_db: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

/// Detailed columns for a table or view. Ordered by `ORDINAL_POSITION`.
pub async fn list_columns(
    pool: &MySqlPool,
    db: &str,
    table: &str,
) -> Result<Vec<ColumnInfo>, ActorError> {
    let rows = sqlx::query(
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, \
                COLUMN_KEY, EXTRA, COLUMN_COMMENT \
         FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         ORDER BY ORDINAL_POSITION",
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let nullable: String = row.try_get(2)?;
        let key: String = row.try_get(4)?;
        out.push(ColumnInfo {
            name: row.try_get(0)?,
            data_type: row.try_get(1)?,
            nullable: nullable.eq_ignore_ascii_case("YES"),
            default_value: row.try_get(3).ok(),
            is_pk: key.eq_ignore_ascii_case("PRI"),
            extra: row.try_get(5)?,
            comment: row.try_get(6)?,
        });
    }
    Ok(out)
}

/// Indexes on a table grouped by `INDEX_NAME` (so a composite index appears
/// as a single [`IndexInfo`] with multiple columns).
pub async fn list_indexes(
    pool: &MySqlPool,
    db: &str,
    table: &str,
) -> Result<Vec<IndexInfo>, ActorError> {
    let rows = sqlx::query(
        "SELECT INDEX_NAME, COLUMN_NAME, NON_UNIQUE, INDEX_TYPE, SEQ_IN_INDEX \
         FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         ORDER BY INDEX_NAME, SEQ_IN_INDEX",
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;
    let mut out: Vec<IndexInfo> = Vec::new();
    for row in rows {
        let name: String = row.try_get(0)?;
        let col: String = row.try_get(1)?;
        // `NON_UNIQUE` is reported by MariaDB as a stringy bigint; pull it as
        // i64 first for resilience across server versions.
        let non_unique: i64 = row.try_get(2).unwrap_or(0);
        let index_type: String = row.try_get(3).unwrap_or_default();
        match out.iter_mut().find(|i| i.name == name) {
            Some(existing) => existing.columns.push(col),
            None => out.push(IndexInfo {
                name,
                columns: vec![col],
                unique: non_unique == 0,
                index_type,
            }),
        }
    }
    Ok(out)
}

/// Outgoing foreign keys defined on `db.table`. Joins
/// `KEY_COLUMN_USAGE` and `REFERENTIAL_CONSTRAINTS` so we can carry the
/// `ON DELETE` / `ON UPDATE` rules.
pub async fn list_foreign_keys(
    pool: &MySqlPool,
    db: &str,
    table: &str,
) -> Result<Vec<ForeignKeyInfo>, ActorError> {
    let rows = sqlx::query(
        "SELECT kcu.CONSTRAINT_NAME, kcu.COLUMN_NAME, \
                kcu.REFERENCED_TABLE_SCHEMA, kcu.REFERENCED_TABLE_NAME, \
                kcu.REFERENCED_COLUMN_NAME, kcu.ORDINAL_POSITION, \
                rc.DELETE_RULE, rc.UPDATE_RULE \
         FROM information_schema.KEY_COLUMN_USAGE kcu \
         JOIN information_schema.REFERENTIAL_CONSTRAINTS rc \
           ON rc.CONSTRAINT_SCHEMA = kcu.CONSTRAINT_SCHEMA \
          AND rc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME \
          AND rc.TABLE_NAME = kcu.TABLE_NAME \
         WHERE kcu.TABLE_SCHEMA = ? AND kcu.TABLE_NAME = ? \
           AND kcu.REFERENCED_TABLE_NAME IS NOT NULL \
         ORDER BY kcu.CONSTRAINT_NAME, kcu.ORDINAL_POSITION",
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;
    let mut out: Vec<ForeignKeyInfo> = Vec::new();
    for row in rows {
        let name: String = row.try_get(0)?;
        let col: String = row.try_get(1)?;
        let ref_db: String = row.try_get(2)?;
        let ref_table: String = row.try_get(3)?;
        let ref_col: String = row.try_get(4)?;
        let on_delete: String = row.try_get(6).unwrap_or_default();
        let on_update: String = row.try_get(7).unwrap_or_default();
        match out.iter_mut().find(|fk| fk.name == name) {
            Some(existing) => {
                existing.columns.push(col);
                existing.ref_columns.push(ref_col);
            }
            None => out.push(ForeignKeyInfo {
                name,
                columns: vec![col],
                ref_db,
                ref_table,
                ref_columns: vec![ref_col],
                on_delete,
                on_update,
            }),
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Table,
    View,
    Procedure,
    Function,
    Trigger,
    Event,
}

impl ObjectKind {
    /// Single-glyph badge shown next to the object name in the sidebar and
    /// the Object inspector header. Plain BMP code points so the system
    /// font fallback always finds a glyph (no emoji-font dependency).
    pub fn short_label(self) -> &'static str {
        match self {
            // U+25A6 SQUARE WITH ORTHOGONAL CROSSHATCH FILL — looks like
            // a 2x2 grid of cells, the universal "table" silhouette.
            ObjectKind::Table => "▦",
            ObjectKind::View => "👁",
            ObjectKind::Procedure => "⚙",
            ObjectKind::Function => "ƒ",
            ObjectKind::Trigger => "⚡",
            ObjectKind::Event => "⏰",
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

//! SELECT-result decoding into a UI-friendly [`QueryResult`].

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, Column, ColumnOrigin, Row, TypeInfo};

use crate::actor::ActorError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnMeta {
    /// Name as it appears in the result set (alias if `SELECT a AS b`).
    pub name: String,
    pub type_name: String,
    /// `(database, table)` when sqlx attributes the column to a source table.
    /// The driver returns the table qualified as `"db.table"` when both are
    /// known; we split it back into a pair.
    pub origin: Option<(String, String)>,
    /// Original column name in the source table (when [`Self::origin`] is set).
    pub original_name: Option<String>,
}

/// A scalar cell value, decoded from the wire to a UI-friendly representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Cell {
    Null,
    Int(i64),
    UInt(u64),
    Float(f64),
    Bool(bool),
    Text(String),
    /// Binary blob with full bytes retained; pagination bounds memory.
    Blob(Vec<u8>),
    /// JSON value formatted as a string (pretty-printed if valid).
    Json(String),
}

impl Cell {
    /// User-visible string for the grid; used by display, copy and sort.
    pub fn display(&self) -> String {
        match self {
            Cell::Null => String::new(),
            Cell::Int(v) => v.to_string(),
            Cell::UInt(v) => v.to_string(),
            Cell::Float(v) => format!("{v}"),
            Cell::Bool(v) => (if *v { "1" } else { "0" }).into(),
            Cell::Text(s) => s.clone(),
            Cell::Blob(b) => format!("<BLOB {} bytes>", b.len()),
            Cell::Json(s) => s.clone(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub elapsed: Duration,
}

pub async fn run_query(pool: &sqlx::MySqlPool, sql: &str) -> Result<QueryResult, ActorError> {
    let start = Instant::now();
    let rows = sqlx::query(AssertSqlSafe(sql.to_string()))
        .fetch_all(pool)
        .await?;

    let columns: Vec<ColumnMeta> = if let Some(first) = rows.first() {
        first
            .columns()
            .iter()
            .map(|c| {
                let (origin, original_name) = match c.origin() {
                    ColumnOrigin::Table(tc) => {
                        let table_str: &str = &tc.table;
                        let split = table_str.split_once('.');
                        let origin = match split {
                            Some((db, tbl)) => Some((db.to_string(), tbl.to_string())),
                            None => Some((String::new(), table_str.to_string())),
                        };
                        (origin, Some(tc.name.as_ref().to_string()))
                    }
                    _ => (None, None),
                };
                ColumnMeta {
                    name: c.name().to_string(),
                    type_name: c.type_info().name().to_string(),
                    origin,
                    original_name,
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    let mut out_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut cells = Vec::with_capacity(columns.len());
        for (idx, col) in columns.iter().enumerate() {
            cells.push(decode_cell(row, idx, &col.type_name));
        }
        out_rows.push(cells);
    }

    Ok(QueryResult {
        columns,
        rows: out_rows,
        elapsed: start.elapsed(),
    })
}

fn decode_cell(row: &sqlx::mysql::MySqlRow, idx: usize, type_name: &str) -> Cell {
    let upper = type_name
        .split('(')
        .next()
        .unwrap_or(type_name)
        .trim()
        .to_ascii_uppercase();
    let is_unsigned = upper.contains("UNSIGNED");

    match upper.split_whitespace().next().unwrap_or(upper.as_str()) {
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" | "YEAR" => {
            if is_unsigned {
                row.try_get::<Option<u64>, _>(idx)
                    .ok()
                    .flatten()
                    .map(Cell::UInt)
                    .unwrap_or(Cell::Null)
            } else {
                row.try_get::<Option<i64>, _>(idx)
                    .ok()
                    .flatten()
                    .map(Cell::Int)
                    .unwrap_or(Cell::Null)
            }
        }
        "BOOLEAN" | "BOOL" => row
            .try_get::<Option<bool>, _>(idx)
            .ok()
            .flatten()
            .map(Cell::Bool)
            .unwrap_or(Cell::Null),
        "FLOAT" | "DOUBLE" | "REAL" => row
            .try_get::<Option<f64>, _>(idx)
            .ok()
            .flatten()
            .map(Cell::Float)
            .unwrap_or_else(|| {
                row.try_get::<Option<String>, _>(idx)
                    .ok()
                    .flatten()
                    .map(Cell::Text)
                    .unwrap_or(Cell::Null)
            }),
        // DECIMAL / NUMERIC arrive as an exact decimal string but, like the
        // temporal types, are only decodable through the `BigDecimal` impl —
        // NOT `f64`, `String` or `Vec<u8>` — so the old float arm's String
        // fallback silently produced NULL. Keep the exact textual value
        // (matching the CLI) instead of routing through `f64`, which would
        // lose precision and add rounding artifacts.
        "DECIMAL" | "NUMERIC" => row
            .try_get::<Option<bigdecimal::BigDecimal>, _>(idx)
            .ok()
            .flatten()
            .map(|d| Cell::Text(d.to_string()))
            .unwrap_or(Cell::Null),
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" => row
            .try_get::<Option<Vec<u8>>, _>(idx)
            .ok()
            .flatten()
            .map(Cell::Blob)
            .unwrap_or(Cell::Null),
        "JSON" => row
            .try_get::<Option<String>, _>(idx)
            .ok()
            .flatten()
            .map(|s| Cell::Json(pretty_json(&s)))
            .unwrap_or(Cell::Null),
        // MySQL/MariaDB BIT(M) arrives as raw binary. sqlx decodes it into a
        // `u64` (big-endian over the bytes) and it is NOT decodable as
        // `Vec<u8>` or `String`, so the previous code silently turned every
        // BIT value into NULL. Use the unsigned-int path; fall back to `bool`
        // for the BIT(1) case on servers that don't flag the column UNSIGNED.
        "BIT" => row
            .try_get::<Option<u64>, _>(idx)
            .ok()
            .flatten()
            .map(Cell::UInt)
            .or_else(|| {
                row.try_get::<Option<bool>, _>(idx)
                    .ok()
                    .flatten()
                    .map(|b| Cell::UInt(b as u64))
            })
            .unwrap_or(Cell::Null),
        // Temporal types arrive as binary and are NOT decodable as `String`
        // or `Vec<u8>` in sqlx — only through the chrono impls. Without these
        // arms the `_` fallback tried `String`, failed the type-compatibility
        // check, and the swallowed error produced NULL. We render them as
        // their canonical textual form (matching the CLI) so display, sort,
        // copy and inline-edit all keep working on `Cell::Text`.
        "DATETIME" | "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(idx)
            .ok()
            .flatten()
            .map(|dt| Cell::Text(dt.to_string()))
            .unwrap_or(Cell::Null),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(idx)
            .ok()
            .flatten()
            .map(|d| Cell::Text(d.to_string()))
            .unwrap_or(Cell::Null),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(idx)
            .ok()
            .flatten()
            .map(|t| Cell::Text(t.to_string()))
            .or_else(|| {
                // MySQL TIME spans ±838:59:59; values outside a 24-hour clock
                // (or negative) don't fit a `NaiveTime`, but `TimeDelta` does.
                row.try_get::<Option<chrono::TimeDelta>, _>(idx)
                    .ok()
                    .flatten()
                    .map(|d| Cell::Text(fmt_time_delta(d)))
            })
            .unwrap_or(Cell::Null),
        // Text-like: VARCHAR, CHAR, TEXT, TINYTEXT, MEDIUMTEXT, LONGTEXT, ENUM, SET
        // Anything else: best-effort String, then raw bytes.
        _ => row
            .try_get::<Option<String>, _>(idx)
            .ok()
            .flatten()
            .map(Cell::Text)
            .unwrap_or_else(|| {
                row.try_get::<Option<Vec<u8>>, _>(idx)
                    .ok()
                    .flatten()
                    .map(Cell::Blob)
                    .unwrap_or(Cell::Null)
            }),
    }
}

/// Format a MySQL `TIME` value that falls outside `NaiveTime`'s 24-hour
/// window (negative, or up to `838:59:59`) as `[-]HH:MM:SS`.
fn fmt_time_delta(d: chrono::TimeDelta) -> String {
    let total = d.num_seconds();
    let secs = total.unsigned_abs();
    let sign = if total < 0 { "-" } else { "" };
    format!(
        "{sign}{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn pretty_json(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

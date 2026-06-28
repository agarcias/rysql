//! Whole-database export to a `.sql` dump or one CSV per table.
//!
//! Everything is built from the existing introspection + query primitives
//! ([`DbHandle::list_objects`], [`DbHandle::show_create`], [`DbHandle::query`]),
//! so there is no dependency on an external `mysqldump` binary. The dump aims
//! to be re-importable through the standard `mysql` CLI: routine / trigger /
//! event bodies are wrapped in `DELIMITER` blocks, binary values are emitted
//! as `0x…` hex literals, and an optional portability header toggles
//! `FOREIGN_KEY_CHECKS` off for the duration of the import.
//!
//! Known limitations: a view that depends on another view is not topologically
//! ordered (it is emitted in name order, after tables and routines), so such a
//! dump may need a re-run of the failing view; and the whole `.sql` is built in
//! memory before being written, so a very large database can be memory-heavy.

use std::path::{Path, PathBuf};

use crate::actor::{ActorError, DbHandle};
use crate::query::Cell;
use crate::schema::ObjectKind;

/// Soft cap on the byte length of a single multi-row `INSERT`. A new statement
/// is started before crossing it, keeping each well under a typical
/// `max_allowed_packet`.
const MAX_INSERT_BYTES: usize = 4 * 1024 * 1024;

/// What to include in a `.sql` dump. Each object category is an independent
/// toggle; [`Self::data`] additionally controls whether table rows are
/// emitted as `INSERT`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlDumpOptions {
    pub tables: bool,
    pub views: bool,
    pub procedures: bool,
    pub functions: bool,
    pub triggers: bool,
    pub events: bool,
    /// Emit `INSERT` rows for table data (only meaningful with `tables`).
    pub data: bool,
    /// `DROP <object> IF EXISTS` before each `CREATE`.
    pub drop_if_exists: bool,
    /// Prepend `CREATE DATABASE IF NOT EXISTS` + `USE`. When off, the dump is
    /// schema-relative (import with `mysql <target_db> < dump.sql`).
    pub create_database: bool,
    /// Wrap the body in portability pragmas (`SET NAMES`,
    /// `FOREIGN_KEY_CHECKS=0`, `SQL_MODE`) and restore them at the end.
    pub portability_header: bool,
    /// Multi-row `INSERT … VALUES (…),(…)` instead of one statement per row.
    pub extended_insert: bool,
    /// Strip `DEFINER=\`user\`@\`host\`` clauses so the dump imports on a
    /// server where that account does not exist.
    pub omit_definer: bool,
    /// Leading comment block (tool, database, server version).
    pub comment_header: bool,
}

impl Default for SqlDumpOptions {
    fn default() -> Self {
        Self {
            tables: true,
            views: true,
            procedures: true,
            functions: true,
            triggers: true,
            events: true,
            data: true,
            drop_if_exists: true,
            create_database: false,
            portability_header: true,
            extended_insert: true,
            omit_definer: true,
            comment_header: true,
        }
    }
}

/// Progress tick emitted once per object processed, so the UI can show
/// `(done/total)` and the current object's label.
#[derive(Debug, Clone)]
pub struct ExportProgress {
    pub done: usize,
    pub total: usize,
    pub label: String,
}

/// Failure of a CSV export: either a database error or a filesystem error
/// while writing one of the output files.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Db(#[from] ActorError),
    #[error("{0}")]
    Io(String),
}

impl ExportError {
    /// Short, human-readable form for the status bar.
    pub fn friendly(&self) -> String {
        match self {
            ExportError::Db(e) => e.friendly(),
            ExportError::Io(s) => s.clone(),
        }
    }
}

/// Build a full `.sql` dump of `db` according to `opts`. `progress` is invoked
/// once per object (after it is written) so callers can drive a progress bar.
pub async fn dump_database(
    handle: &DbHandle,
    db: &str,
    opts: &SqlDumpOptions,
    mut progress: impl FnMut(ExportProgress),
) -> Result<String, ActorError> {
    let objs = handle.list_objects(db.to_string()).await?;

    // Worklist in dependency-friendly order: tables before the views,
    // routines, triggers and events that may reference them.
    let mut work: Vec<(ObjectKind, &str)> = Vec::new();
    if opts.tables {
        work.extend(objs.tables.iter().map(|n| (ObjectKind::Table, n.as_str())));
    }
    // Functions / procedures before views: a view's body may call a stored
    // function. (View-on-view ordering is not resolved — see module note.)
    if opts.functions {
        work.extend(
            objs.functions
                .iter()
                .map(|n| (ObjectKind::Function, n.as_str())),
        );
    }
    if opts.procedures {
        work.extend(
            objs.procedures
                .iter()
                .map(|n| (ObjectKind::Procedure, n.as_str())),
        );
    }
    if opts.views {
        work.extend(objs.views.iter().map(|n| (ObjectKind::View, n.as_str())));
    }
    if opts.triggers {
        work.extend(
            objs.triggers
                .iter()
                .map(|n| (ObjectKind::Trigger, n.as_str())),
        );
    }
    if opts.events {
        work.extend(objs.events.iter().map(|n| (ObjectKind::Event, n.as_str())));
    }

    let total = work.len();
    let mut out = String::new();

    if opts.comment_header {
        let version = handle
            .server_info()
            .await
            .map(|s| s.version)
            .unwrap_or_default();
        out.push_str("-- RySQL database export\n");
        out.push_str(&format!("-- Database: {db}\n"));
        if !version.is_empty() {
            out.push_str(&format!("-- Server version: {version}\n"));
        }
        out.push('\n');
    }

    if opts.portability_header {
        out.push_str("SET NAMES utf8mb4;\n");
        out.push_str("SET FOREIGN_KEY_CHECKS=0;\n");
        out.push_str("SET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE='NO_AUTO_VALUE_ON_ZERO';\n\n");
    }

    if opts.create_database {
        out.push_str(&format!(
            "CREATE DATABASE IF NOT EXISTS {};\nUSE {};\n\n",
            ident(db),
            ident(db)
        ));
    }

    for (idx, (kind, name)) in work.iter().enumerate() {
        match kind {
            ObjectKind::Table => {
                section(&mut out, "Table structure for", name);
                if opts.drop_if_exists {
                    out.push_str(&format!("DROP TABLE IF EXISTS {};\n", ident(name)));
                }
                let ddl = handle
                    .show_create(db.to_string(), ObjectKind::Table, name.to_string())
                    .await?;
                out.push_str(&ddl);
                out.push_str(";\n");
                if opts.data {
                    // Generated (VIRTUAL/STORED) columns reject explicit values
                    // on import, so exclude them from both the SELECT and the
                    // INSERT column list — matching mysqldump.
                    let cols = handle
                        .list_columns(db.to_string(), name.to_string())
                        .await?;
                    let insertable: Vec<String> = cols
                        .iter()
                        .filter(|c| !is_generated(&c.extra))
                        .map(|c| c.name.clone())
                        .collect();
                    if !insertable.is_empty() {
                        write_table_data(
                            &mut out,
                            handle,
                            db,
                            name,
                            &insertable,
                            opts.extended_insert,
                        )
                        .await?;
                    }
                }
            }
            ObjectKind::View => {
                section(&mut out, "View structure for", name);
                if opts.drop_if_exists {
                    out.push_str(&format!("DROP VIEW IF EXISTS {};\n", ident(name)));
                }
                let ddl = handle
                    .show_create(db.to_string(), ObjectKind::View, name.to_string())
                    .await?;
                out.push_str(&maybe_strip_definer(&ddl, opts.omit_definer));
                out.push_str(";\n");
            }
            // Routines / triggers / events carry `;`-terminated bodies, so they
            // must be wrapped in a `DELIMITER` block to survive the `mysql` CLI.
            ObjectKind::Procedure
            | ObjectKind::Function
            | ObjectKind::Trigger
            | ObjectKind::Event => {
                let label = routine_section_label(*kind);
                section(&mut out, label, name);
                let ddl = handle
                    .show_create(db.to_string(), *kind, name.to_string())
                    .await?;
                let body = maybe_strip_definer(&ddl, opts.omit_definer);
                // Pick a delimiter the body doesn't contain, so a literal `$$`
                // inside the routine can't terminate the statement early.
                let delim = pick_delimiter(&body);
                out.push_str(&format!("DELIMITER {delim}\n"));
                if opts.drop_if_exists {
                    out.push_str(&format!(
                        "DROP {} IF EXISTS {}{delim}\n",
                        kind.keyword(),
                        ident(name)
                    ));
                }
                out.push_str(&body);
                out.push_str(&format!("{delim}\nDELIMITER ;\n"));
            }
        }

        progress(ExportProgress {
            done: idx + 1,
            total,
            label: format!("{} `{}`", kind.keyword().to_lowercase(), name),
        });
    }

    if opts.portability_header {
        out.push_str("\nSET FOREIGN_KEY_CHECKS=1;\n");
        out.push_str("SET SQL_MODE=@OLD_SQL_MODE;\n");
    }

    Ok(out)
}

/// Export every table in `db` as a `<table>.csv` file under `dest_dir`.
/// Returns the paths written. `progress` ticks once per table.
pub async fn export_csv(
    handle: &DbHandle,
    db: &str,
    dest_dir: &Path,
    mut progress: impl FnMut(ExportProgress),
) -> Result<Vec<PathBuf>, ExportError> {
    let objs = handle.list_objects(db.to_string()).await?;
    let total = objs.tables.len();
    let mut written = Vec::with_capacity(total);

    for (idx, table) in objs.tables.iter().enumerate() {
        // Column names come from metadata so the header is correct even for an
        // empty table. The data SELECT lists the same columns explicitly: a
        // bare `SELECT *` would drop INVISIBLE columns and misalign the header.
        let columns = handle
            .list_columns(db.to_string(), table.to_string())
            .await?;
        let mut content = String::new();
        content.push_str(&csv_record(columns.iter().map(|c| c.name.as_str())));

        if !columns.is_empty() {
            let select_list = columns
                .iter()
                .map(|c| ident(&c.name))
                .collect::<Vec<_>>()
                .join(", ");
            // One consistent read per table (no LIMIT/OFFSET): paginating
            // without a stable order or snapshot can skip or duplicate rows.
            let qr = handle
                .query(format!(
                    "SELECT {select_list} FROM {}",
                    qualified(db, table)
                ))
                .await?;
            for row in &qr.rows {
                content.push_str(&csv_record(row.iter().map(csv_field).collect::<Vec<_>>()));
            }
        }

        let path = dest_dir.join(format!("{}.csv", sanitize_filename(table)));
        std::fs::write(&path, content).map_err(|e| ExportError::Io(e.to_string()))?;
        written.push(path);

        progress(ExportProgress {
            done: idx + 1,
            total,
            label: format!("table `{table}`"),
        });
    }

    Ok(written)
}

/// Read a table's rows (restricted to `columns`) and append `INSERT`
/// statements to `out`. A single `SELECT` gives one consistent read; the rows
/// are then chunked into statements by [`append_inserts`].
async fn write_table_data(
    out: &mut String,
    handle: &DbHandle,
    db: &str,
    table: &str,
    columns: &[String],
    extended: bool,
) -> Result<(), ActorError> {
    let select_list = columns
        .iter()
        .map(|c| ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let qr = handle
        .query(format!(
            "SELECT {select_list} FROM {}",
            qualified(db, table)
        ))
        .await?;
    if qr.rows.is_empty() {
        return Ok(());
    }
    out.push_str(&format!("\n-- Data for `{table}`\n"));
    append_inserts(out, table, columns, &qr.rows, extended);
    Ok(())
}

fn append_inserts(
    out: &mut String,
    table: &str,
    columns: &[String],
    rows: &[Vec<Cell>],
    extended: bool,
) {
    if rows.is_empty() {
        return;
    }
    let cols = columns
        .iter()
        .map(|c| ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let prefix = format!("INSERT INTO {} ({cols})", ident(table));

    if extended {
        // Group rows into multi-row statements, starting a fresh statement
        // before any one grows past `MAX_INSERT_BYTES` (keeps each well under
        // the server's `max_allowed_packet`).
        let mut i = 0;
        while i < rows.len() {
            out.push_str(&prefix);
            out.push_str(" VALUES\n");
            let mut stmt_len = prefix.len() + " VALUES\n".len();
            loop {
                let vals = row_values(&rows[i]);
                out.push('(');
                out.push_str(&vals);
                out.push(')');
                stmt_len += vals.len() + 2;
                i += 1;
                if i >= rows.len() || stmt_len >= MAX_INSERT_BYTES {
                    out.push_str(";\n");
                    break;
                }
                out.push_str(",\n");
            }
        }
    } else {
        for row in rows {
            out.push_str(&prefix);
            out.push_str(" VALUES (");
            out.push_str(&row_values(row));
            out.push_str(");\n");
        }
    }
}

fn row_values(row: &[Cell]) -> String {
    row.iter().map(sql_literal).collect::<Vec<_>>().join(", ")
}

/// Render a cell as a SQL literal for an `INSERT`. Binary is hex-encoded
/// (`0x…`) so blobs survive the round-trip rather than being dropped.
fn sql_literal(c: &Cell) -> String {
    match c {
        Cell::Null => "NULL".into(),
        Cell::Int(v) => v.to_string(),
        Cell::UInt(v) => v.to_string(),
        Cell::Float(v) => {
            if v.is_finite() {
                format!("{v}")
            } else {
                "NULL".into()
            }
        }
        Cell::Bool(v) => (if *v { "1" } else { "0" }).into(),
        Cell::Text(s) | Cell::Json(s) => quote_str(s),
        Cell::Blob(b) => {
            if b.is_empty() {
                "''".into()
            } else {
                hex_literal(b)
            }
        }
    }
}

fn quote_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn hex_literal(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn csv_field(c: &Cell) -> String {
    match c {
        Cell::Null => String::new(),
        Cell::Blob(b) => {
            use std::fmt::Write;
            let mut s = String::with_capacity(b.len() * 2);
            for byte in b {
                let _ = write!(s, "{byte:02x}");
            }
            s
        }
        _ => c.display(),
    }
}

/// Join one CSV record (with trailing newline), quoting fields as needed.
fn csv_record<I, S>(fields: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = String::new();
    let mut first = true;
    for f in fields {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&csv_escape(f.as_ref()));
    }
    out.push('\n');
    out
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Whether a column's `EXTRA` marks it as a generated (computed) column,
/// across MySQL (`VIRTUAL GENERATED` / `STORED GENERATED`) and MariaDB
/// (`VIRTUAL` / `STORED` / `PERSISTENT`). Generated columns reject an explicit
/// value on insert, so their data is omitted from the dump.
///
/// Note: MySQL 8 also reports `DEFAULT_GENERATED` for ordinary columns with an
/// expression default (e.g. `TIMESTAMP DEFAULT CURRENT_TIMESTAMP`). Those are
/// fully writable and must NOT be excluded, so we match the specific generated
/// tokens rather than a bare `GENERATED` substring.
fn is_generated(extra: &str) -> bool {
    let e = extra.to_ascii_uppercase();
    e.contains("VIRTUAL GENERATED")
        || e.contains("STORED GENERATED")
        || e == "VIRTUAL"
        || e == "STORED"
        || e == "PERSISTENT"
}

/// Pick a `DELIMITER` token that does not occur in `body`, so a literal match
/// inside a routine/trigger/event body can't terminate the statement early.
fn pick_delimiter(body: &str) -> &'static str {
    for candidate in ["$$", ";;", "$RYSQL$"] {
        if !body.contains(candidate) {
            return candidate;
        }
    }
    "$RYSQL$"
}

/// Backtick-quote an identifier, doubling any embedded backticks.
fn ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn qualified(db: &str, name: &str) -> String {
    format!("{}.{}", ident(db), ident(name))
}

fn section(out: &mut String, label: &str, name: &str) {
    out.push_str(&format!("\n-- {label} `{name}`\n"));
}

fn routine_section_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Procedure => "Procedure",
        ObjectKind::Function => "Function",
        ObjectKind::Trigger => "Trigger",
        ObjectKind::Event => "Event",
        _ => "Object",
    }
}

fn maybe_strip_definer(ddl: &str, strip: bool) -> String {
    if strip {
        strip_definer(ddl)
    } else {
        ddl.to_string()
    }
}

/// Remove a `DEFINER=user@host` clause from a `CREATE …` statement. Handles
/// backtick-quoted, single-quoted and bare user / host tokens. Returns the
/// input unchanged when no clause is present.
fn strip_definer(ddl: &str) -> String {
    let Some(start) = find_ci(ddl, "DEFINER=") else {
        return ddl.to_string();
    };
    let bytes = ddl.as_bytes();
    let mut i = start + "DEFINER=".len();
    i = skip_token(bytes, i);
    if i < bytes.len() && bytes[i] == b'@' {
        i += 1;
        i = skip_token(bytes, i);
    }
    // Swallow one trailing space so `CREATE DEFINER=… PROCEDURE` collapses
    // cleanly to `CREATE PROCEDURE`.
    let mut end = i;
    if end < bytes.len() && bytes[end] == b' ' {
        end += 1;
    }
    let mut out = String::with_capacity(ddl.len());
    out.push_str(&ddl[..start]);
    out.push_str(&ddl[end..]);
    out
}

/// Advance past one identifier token starting at `i`: a backtick- or
/// single-quote-delimited string (honouring doubled-quote escapes) or a bare
/// run up to the next `@`/whitespace. All stop characters are ASCII, so the
/// returned index is always a UTF-8 boundary.
fn skip_token(s: &[u8], mut i: usize) -> usize {
    if i >= s.len() {
        return i;
    }
    match s[i] {
        b'`' => {
            i += 1;
            while i < s.len() {
                if s[i] == b'`' {
                    if i + 1 < s.len() && s[i + 1] == b'`' {
                        i += 2;
                        continue;
                    }
                    return i + 1;
                }
                i += 1;
            }
        }
        b'\'' => {
            i += 1;
            while i < s.len() {
                if s[i] == b'\'' {
                    if i + 1 < s.len() && s[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    return i + 1;
                }
                i += 1;
            }
        }
        _ => {
            while i < s.len() && s[i] != b'@' && !s[i].is_ascii_whitespace() {
                i += 1;
            }
        }
    }
    i
}

/// ASCII case-insensitive substring search returning the byte offset in the
/// original string (so the result is safe to slice at).
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    'outer: for i in 0..=h.len() - n.len() {
        for j in 0..n.len() {
            if !h[i + j].eq_ignore_ascii_case(&n[j]) {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

/// Make a table name safe to use as a file stem on every platform.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "table".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_escaping() {
        assert_eq!(sql_literal(&Cell::Null), "NULL");
        assert_eq!(sql_literal(&Cell::Int(-7)), "-7");
        assert_eq!(sql_literal(&Cell::UInt(7)), "7");
        assert_eq!(sql_literal(&Cell::Bool(true)), "1");
        assert_eq!(
            sql_literal(&Cell::Text("O'Brien\\x".into())),
            "'O''Brien\\\\x'"
        );
        assert_eq!(sql_literal(&Cell::Text("a\nb".into())), "'a\\nb'");
        assert_eq!(sql_literal(&Cell::Blob(vec![0x00, 0xff, 0x10])), "0x00ff10");
        assert_eq!(sql_literal(&Cell::Blob(vec![])), "''");
    }

    #[test]
    fn strips_backtick_definer() {
        let ddl = "CREATE DEFINER=`root`@`localhost` PROCEDURE `p`() BEGIN END";
        assert_eq!(strip_definer(ddl), "CREATE PROCEDURE `p`() BEGIN END");
    }

    #[test]
    fn strips_view_definer_with_quotes() {
        let ddl =
            "CREATE ALGORITHM=UNDEFINED DEFINER='adm'@'10.0.0.1' SQL SECURITY DEFINER VIEW `v` AS SELECT 1";
        assert_eq!(
            strip_definer(ddl),
            "CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW `v` AS SELECT 1"
        );
    }

    #[test]
    fn strips_bare_definer() {
        let ddl = "CREATE DEFINER=root@localhost TRIGGER `t` BEFORE INSERT ON `x`";
        assert_eq!(
            strip_definer(ddl),
            "CREATE TRIGGER `t` BEFORE INSERT ON `x`"
        );
    }

    #[test]
    fn definer_absent_is_noop() {
        let ddl = "CREATE TABLE `t` (`id` int)";
        assert_eq!(strip_definer(ddl), ddl);
    }

    #[test]
    fn csv_quotes_special_fields() {
        let rec = csv_record(["a,b", "plain", "x\"y", "n\nl"]);
        assert_eq!(rec, "\"a,b\",plain,\"x\"\"y\",\"n\nl\"\n");
    }

    #[test]
    fn extended_insert_groups_rows() {
        let cols = vec!["id".to_string()];
        let rows = vec![vec![Cell::Int(1)], vec![Cell::Int(2)]];
        let mut out = String::new();
        append_inserts(&mut out, "t", &cols, &rows, true);
        assert_eq!(out, "INSERT INTO `t` (`id`) VALUES\n(1),\n(2);\n");
    }

    #[test]
    fn single_row_inserts() {
        let cols = vec!["id".to_string()];
        let rows = vec![vec![Cell::Int(1)], vec![Cell::Int(2)]];
        let mut out = String::new();
        append_inserts(&mut out, "t", &cols, &rows, false);
        assert_eq!(
            out,
            "INSERT INTO `t` (`id`) VALUES (1);\nINSERT INTO `t` (`id`) VALUES (2);\n"
        );
    }

    #[test]
    fn detects_generated_columns() {
        assert!(is_generated("VIRTUAL GENERATED"));
        assert!(is_generated("STORED GENERATED"));
        assert!(is_generated("PERSISTENT"));
        assert!(is_generated("virtual"));
        assert!(!is_generated(""));
        assert!(!is_generated("auto_increment"));
        assert!(!is_generated("on update CURRENT_TIMESTAMP"));
        // MySQL 8 expression-default columns are writable, NOT generated.
        assert!(!is_generated("DEFAULT_GENERATED"));
        assert!(!is_generated(
            "DEFAULT_GENERATED on update CURRENT_TIMESTAMP"
        ));
    }

    #[test]
    fn picks_clear_delimiter() {
        assert_eq!(pick_delimiter("BEGIN SELECT 1; END"), "$$");
        assert_eq!(pick_delimiter("BEGIN SET x = '$$'; END"), ";;");
        assert_eq!(pick_delimiter("weird $$ and ;; body"), "$RYSQL$");
    }

    #[test]
    fn sanitizes_filenames() {
        assert_eq!(sanitize_filename("orders/2024"), "orders_2024");
        assert_eq!(sanitize_filename("a:b*c?"), "a_b_c_");
        assert_eq!(sanitize_filename("  "), "table");
    }
}

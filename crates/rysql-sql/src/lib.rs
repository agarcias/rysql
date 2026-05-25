//! SQL utilities: formatting, syntax highlighting and statement splitting.

pub mod highlight;
pub mod keywords;
pub mod split;

pub use highlight::{HighlightSpan, Highlighter};
pub use keywords::SQL_KEYWORDS;
pub use split::{split_statements, statement_at_cursor};

pub fn format_sql(input: &str) -> String {
    sqlformat::format(
        input,
        &sqlformat::QueryParams::None,
        &sqlformat::FormatOptions::default(),
    )
}

/// Heuristic: does the statement return rows when executed?
/// Looks at the first SQL keyword (after skipping whitespace and `--`/`/* */`
/// comments at the start).
pub fn is_query_returning_rows(sql: &str) -> bool {
    let trimmed = strip_leading_comments(sql).trim_start();
    let kw: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    matches!(
        kw.as_str(),
        "SELECT"
            | "SHOW"
            | "DESCRIBE"
            | "DESC"
            | "EXPLAIN"
            | "WITH"
            | "TABLE"
            | "VALUES"
            | "CALL"
            | "CHECK"
            | "ANALYZE"
            | "HELP"
    )
}

/// Heuristic: does the trimmed statement already end with a `LIMIT` clause?
/// Walks backwards from the last non-comment, non-whitespace character to find
/// `LIMIT <number>` or `LIMIT <number>(,| OFFSET) <number>`.
pub fn has_limit_clause(sql: &str) -> bool {
    let trimmed = sql.trim_end().trim_end_matches(';').trim_end();
    let upper = trimmed.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    // Walk backwards from the end: at minimum we need "LIMIT <digit>".
    // Skip any trailing digits, whitespace, commas, OFFSET keyword and digits.
    let mut i = bytes.len();
    let mut saw_digit = false;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_whitespace() || c.is_ascii_digit() || c == b',' {
            if c.is_ascii_digit() {
                saw_digit = true;
            }
            i -= 1;
            continue;
        }
        // Try OFFSET, otherwise break.
        if upper[..i].ends_with("OFFSET") {
            i -= "OFFSET".len();
            continue;
        }
        break;
    }
    if !saw_digit {
        return false;
    }
    upper[..i].trim_end().ends_with("LIMIT")
}

/// Append `LIMIT <limit> OFFSET <offset>` to a SELECT-like statement.
/// Strips an existing trailing `;` if present.
pub fn inject_pagination(sql: &str, limit: u64, offset: u64) -> String {
    let body = sql.trim_end().trim_end_matches(';').trim_end();
    if offset == 0 {
        format!("{body}\nLIMIT {limit}")
    } else {
        format!("{body}\nLIMIT {limit} OFFSET {offset}")
    }
}

fn strip_leading_comments(s: &str) -> &str {
    let mut rest = s.trim_start();
    loop {
        if let Some(stripped) = rest.strip_prefix("--") {
            match stripped.find('\n') {
                Some(i) => rest = stripped[i + 1..].trim_start(),
                None => return "",
            }
        } else if let Some(stripped) = rest.strip_prefix("/*") {
            match stripped.find("*/") {
                Some(i) => rest = stripped[i + 2..].trim_start(),
                None => return "",
            }
        } else if let Some(stripped) = rest.strip_prefix('#') {
            match stripped.find('\n') {
                Some(i) => rest = stripped[i + 1..].trim_start(),
                None => return "",
            }
        } else {
            return rest;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returning_rows_basic() {
        assert!(is_query_returning_rows("SELECT 1"));
        assert!(is_query_returning_rows("  select * from t"));
        assert!(is_query_returning_rows("SHOW TABLES"));
        assert!(is_query_returning_rows("EXPLAIN SELECT 1"));
        assert!(is_query_returning_rows(
            "WITH x AS (SELECT 1) SELECT * FROM x"
        ));
    }

    #[test]
    fn not_returning_rows() {
        assert!(!is_query_returning_rows("INSERT INTO t VALUES (1)"));
        assert!(!is_query_returning_rows("UPDATE t SET a = 1"));
        assert!(!is_query_returning_rows("DELETE FROM t"));
        assert!(!is_query_returning_rows("CREATE TABLE t (id INT)"));
        assert!(!is_query_returning_rows("DROP TABLE t"));
    }

    #[test]
    fn limit_detection() {
        assert!(has_limit_clause("SELECT * FROM t LIMIT 10"));
        assert!(has_limit_clause("SELECT * FROM t LIMIT 10;"));
        assert!(has_limit_clause("SELECT * FROM t LIMIT 10, 20"));
        assert!(has_limit_clause("SELECT * FROM t LIMIT 10 OFFSET 20"));
        assert!(!has_limit_clause("SELECT * FROM t"));
        assert!(!has_limit_clause("SELECT * FROM t ORDER BY a"));
        assert!(!has_limit_clause("SELECT * FROM t WHERE col = 'limited'"));
    }

    #[test]
    fn pagination_inject() {
        assert_eq!(inject_pagination("SELECT 1", 10, 0), "SELECT 1\nLIMIT 10");
        assert_eq!(
            inject_pagination("SELECT 1;", 100, 200),
            "SELECT 1\nLIMIT 100 OFFSET 200"
        );
    }

    #[test]
    fn skip_leading_comments() {
        assert!(is_query_returning_rows("-- comment\nSELECT 1"));
        assert!(is_query_returning_rows("/* block */ SELECT 1"));
        assert!(!is_query_returning_rows(
            "/* hi */ INSERT INTO t VALUES (1)"
        ));
    }
}

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
    fn skip_leading_comments() {
        assert!(is_query_returning_rows("-- comment\nSELECT 1"));
        assert!(is_query_returning_rows("/* block */ SELECT 1"));
        assert!(!is_query_returning_rows(
            "/* hi */ INSERT INTO t VALUES (1)"
        ));
    }
}

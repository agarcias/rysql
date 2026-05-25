//! Split a SQL script into individual statements while respecting strings,
//! identifiers and comments. Returns byte ranges into the original string.

use std::ops::Range;

#[derive(Copy, Clone, PartialEq, Eq)]
enum State {
    Normal,
    Single,   // 'string'
    Double,   // "identifier"
    Backtick, // `identifier`
    Line,     // -- ... \n   or   # ... \n
    Block,    // /* ... */
}

/// Returns the byte ranges (start..end, end exclusive) of each non-empty
/// statement in `sql`. The trailing `;` is included in the range.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut state = State::Normal;
    let mut stmt_start: Option<usize> = None;
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        match state {
            State::Normal => {
                if stmt_start.is_none() && !b.is_ascii_whitespace() {
                    stmt_start = Some(i);
                }
                match b {
                    b'\'' => state = State::Single,
                    b'"' => state = State::Double,
                    b'`' => state = State::Backtick,
                    b'#' => state = State::Line,
                    b'-' if bytes.get(i + 1) == Some(&b'-') => {
                        state = State::Line;
                        i += 1;
                    }
                    b'/' if bytes.get(i + 1) == Some(&b'*') => {
                        state = State::Block;
                        i += 1;
                    }
                    b';' => {
                        if let Some(start) = stmt_start.take() {
                            out.push(start..i + 1);
                        }
                    }
                    _ => {}
                }
            }
            State::Single => match b {
                b'\\' => {
                    i += 1;
                }
                b'\'' => {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 1;
                    } else {
                        state = State::Normal;
                    }
                }
                _ => {}
            },
            State::Double => match b {
                b'\\' => {
                    i += 1;
                }
                b'"' => {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 1;
                    } else {
                        state = State::Normal;
                    }
                }
                _ => {}
            },
            State::Backtick => {
                if b == b'`' {
                    if bytes.get(i + 1) == Some(&b'`') {
                        i += 1;
                    } else {
                        state = State::Normal;
                    }
                }
            }
            State::Line => {
                if b == b'\n' {
                    state = State::Normal;
                }
            }
            State::Block => {
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    state = State::Normal;
                    i += 1;
                }
            }
        }
        i += 1;
    }

    if let Some(start) = stmt_start {
        // Trailing content with no semicolon — still a statement.
        let end = sql.trim_end().len();
        if end > start {
            out.push(start..end);
        }
    }

    out
}

/// Returns the range of the statement containing the byte offset `cursor`,
/// or the last statement if cursor is at end. Falls back to a range covering
/// the whole text if no semicolons.
pub fn statement_at_cursor(sql: &str, cursor: usize) -> Option<Range<usize>> {
    let stmts = split_statements(sql);
    if stmts.is_empty() {
        return None;
    }
    for r in &stmts {
        if cursor >= r.start && cursor <= r.end {
            return Some(r.clone());
        }
    }
    Some(stmts.last().unwrap().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_no_semicolon() {
        let s = "SELECT 1";
        assert_eq!(split_statements(s), vec![0..8]);
    }

    #[test]
    fn two_statements() {
        let s = "SELECT 1; SELECT 2;";
        assert_eq!(split_statements(s), vec![0..9, 10..19]);
    }

    #[test]
    fn semicolon_inside_string_ignored() {
        let s = "SELECT 'a;b'; SELECT 1";
        let parts = split_statements(s);
        assert_eq!(parts.len(), 2);
        assert_eq!(&s[parts[0].clone()], "SELECT 'a;b';");
        assert_eq!(&s[parts[1].clone()], "SELECT 1");
    }

    #[test]
    fn semicolon_inside_line_comment_ignored() {
        let s = "SELECT 1 -- ; not a stmt\n; SELECT 2;";
        let parts = split_statements(s);
        assert_eq!(parts.len(), 2);
        assert_eq!(&s[parts[1].clone()], "SELECT 2;");
    }

    #[test]
    fn semicolon_inside_block_comment_ignored() {
        let s = "SELECT /* ; ; */ 1; SELECT 2;";
        let parts = split_statements(s);
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn statement_at_cursor_basic() {
        let s = "SELECT 1; SELECT 2;";
        assert_eq!(statement_at_cursor(s, 3), Some(0..9));
        assert_eq!(statement_at_cursor(s, 12), Some(10..19));
    }
}

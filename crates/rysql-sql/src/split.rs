//! Split a SQL script into individual statements while respecting strings,
//! identifiers, comments and `BEGIN ... END` / `CASE ... END` compound
//! blocks. Returns byte ranges into the original string.
//!
//! Block-aware splitting means scripts containing stored routines work
//! without the client-side `DELIMITER` directive: any `;` inside a
//! compound `BEGIN ... END` is part of that compound statement, not a
//! statement separator. Matches the behaviour expected by DBForge,
//! DataGrip, MySQL Workbench's "executes as single statement" mode, etc.

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

/// Compound blocks we need to balance against `END`. `Begin` is the only
/// one that suppresses `;` as a statement boundary — the others exist so
/// `END` matches the right opener (e.g. `CASE WHEN … END` inside a
/// procedure body doesn't accidentally close the enclosing `BEGIN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Begin,
    Case,
}

/// Returns the byte ranges (start..end, end exclusive) of each non-empty
/// statement in `sql`. The trailing `;` is included in the range.
///
/// Compound statements (`CREATE PROCEDURE … BEGIN … END;`) are emitted as
/// a single range — their internal `;` are not treated as boundaries.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut state = State::Normal;
    let mut stmt_start: Option<usize> = None;
    let mut block_stack: Vec<BlockKind> = Vec::new();
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
                        let inside_compound = block_stack.contains(&BlockKind::Begin);
                        if !inside_compound {
                            if let Some(start) = stmt_start.take() {
                                out.push(start..i + 1);
                            }
                        }
                    }
                    _ if is_word_first_byte(b) && is_word_start(bytes, i) => {
                        let end = word_end(bytes, i);
                        let word = &bytes[i..end];
                        handle_keyword(bytes, end, word, &mut block_stack);
                        i = end;
                        continue;
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

/// Mutate `block_stack` in response to the keyword `word`, peeking ahead
/// from `after` (the byte just past `word`) for the disambiguating token
/// when needed.
fn handle_keyword(bytes: &[u8], after: usize, word: &[u8], block_stack: &mut Vec<BlockKind>) {
    if word.eq_ignore_ascii_case(b"BEGIN") {
        // `BEGIN;`, `BEGIN WORK`, `BEGIN TRANSACTION` are transactional
        // starts and do NOT open a compound block. Anything else after
        // BEGIN (including `BEGIN ATOMIC`, `BEGIN NOT ATOMIC`, identifiers
        // for declared variables, or just a newline before the body) is
        // a compound BEGIN.
        let next = skip_ws_and_comments(bytes, after);
        let transactional = match bytes.get(next) {
            None => false, // dangling BEGIN — treat as compound, lets the server complain
            Some(&b';') => true,
            Some(_) => {
                let w_end = word_end(bytes, next);
                let nw = &bytes[next..w_end];
                nw.eq_ignore_ascii_case(b"WORK") || nw.eq_ignore_ascii_case(b"TRANSACTION")
            }
        };
        if !transactional {
            block_stack.push(BlockKind::Begin);
        }
        return;
    }
    if word.eq_ignore_ascii_case(b"CASE") {
        block_stack.push(BlockKind::Case);
        return;
    }
    if word.eq_ignore_ascii_case(b"END") {
        let next = skip_ws_and_comments(bytes, after);
        let next_word_end = word_end(bytes, next);
        let next_word = &bytes[next..next_word_end];
        if next_word.eq_ignore_ascii_case(b"IF")
            || next_word.eq_ignore_ascii_case(b"LOOP")
            || next_word.eq_ignore_ascii_case(b"WHILE")
            || next_word.eq_ignore_ascii_case(b"REPEAT")
        {
            // Inner control-flow block close — doesn't pop the outer
            // BEGIN/CASE stack.
            return;
        }
        if next_word.eq_ignore_ascii_case(b"CASE") {
            // Compound `END CASE` close.
            if let Some(idx) = block_stack.iter().rposition(|k| *k == BlockKind::Case) {
                block_stack.remove(idx);
            }
            return;
        }
        // Bare END: closes the most recent CASE (inline expression) or,
        // failing that, the most recent BEGIN.
        if let Some(top) = block_stack.last().copied() {
            match top {
                BlockKind::Case => {
                    block_stack.pop();
                }
                BlockKind::Begin => {
                    block_stack.pop();
                }
            }
        }
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_word_first_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// True iff `i` is the start of a word and isn't preceded by an
/// identifier-prefix character (`@` for user vars, `$`, `.` for qualified
/// names). Without this guard `@while` or `tbl.end` would trigger
/// keyword handling.
fn is_word_start(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = bytes[i - 1];
    !(is_word_byte(prev) || prev == b'@' || prev == b'$' || prev == b'.')
}

fn word_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    end
}

fn skip_ws_and_comments(bytes: &[u8], mut i: usize) -> usize {
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i < bytes.len() && bytes[i] == b'#' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            }
            continue;
        }
        break;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(s: &str) -> Vec<&str> {
        split_statements(s).into_iter().map(|r| &s[r]).collect()
    }

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

    // === Block-awareness tests =============================================

    #[test]
    fn create_procedure_with_internal_semicolons_is_one_statement() {
        let s = "\
DROP PROCEDURE IF EXISTS foo;

CREATE PROCEDURE foo()
BEGIN
    SELECT 1;
    SELECT 2;
END;
";
        let p = parts(s);
        assert_eq!(p.len(), 2);
        assert!(p[0].starts_with("DROP"));
        assert!(p[1].starts_with("CREATE"));
        assert!(p[1].trim_end().ends_with("END;"));
    }

    #[test]
    fn nested_if_inside_begin_does_not_close_outer_block() {
        let s = "\
CREATE PROCEDURE foo()
BEGIN
    IF x THEN
        SELECT 1;
    END IF;
    SELECT 2;
END;
";
        let p = parts(s);
        assert_eq!(p.len(), 1);
        assert!(p[0].trim_end().ends_with("END;"));
    }

    #[test]
    fn while_loop_repeat_blocks_do_not_close_outer_begin() {
        let s = "\
CREATE PROCEDURE foo()
BEGIN
    WHILE x DO
        SELECT 1;
    END WHILE;
    LOOP
        SELECT 2;
    END LOOP;
    REPEAT
        SELECT 3;
    UNTIL x END REPEAT;
    SELECT 4;
END;
";
        let p = parts(s);
        assert_eq!(p.len(), 1);
        assert!(p[0].trim_end().ends_with("END;"));
    }

    #[test]
    fn inline_case_expression_outside_begin_does_not_decrement_stack() {
        // bare END here closes the CASE expression; `;` must still emit.
        let s = "SELECT CASE WHEN x THEN 1 ELSE 0 END; SELECT 2;";
        let p = parts(s);
        assert_eq!(p.len(), 2);
        assert!(p[0].starts_with("SELECT CASE"));
        assert_eq!(p[1], "SELECT 2;");
    }

    #[test]
    fn inline_case_inside_begin_does_not_close_begin() {
        let s = "\
CREATE PROCEDURE foo()
BEGIN
    SELECT CASE WHEN x THEN 1 ELSE 0 END;
    SELECT 2;
END;
";
        let p = parts(s);
        assert_eq!(p.len(), 1);
        assert!(p[0].trim_end().ends_with("END;"));
    }

    #[test]
    fn nested_begin_for_declare_handler() {
        let s = "\
CREATE PROCEDURE foo()
BEGIN
    DECLARE CONTINUE HANDLER FOR SQLEXCEPTION BEGIN SELECT 1; END;
    SELECT 2;
END;
";
        let p = parts(s);
        assert_eq!(p.len(), 1);
        assert!(p[0].trim_end().ends_with("END;"));
    }

    #[test]
    fn transactional_begin_is_not_compound() {
        let s = "BEGIN; UPDATE t SET x = 1; COMMIT;";
        let p = parts(s);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], "BEGIN;");
        assert_eq!(p[1], "UPDATE t SET x = 1;");
        assert_eq!(p[2], "COMMIT;");
    }

    #[test]
    fn begin_work_and_begin_transaction_are_not_compound() {
        let s = "BEGIN WORK; SELECT 1; COMMIT; BEGIN TRANSACTION; SELECT 2; COMMIT;";
        let p = parts(s);
        assert_eq!(p.len(), 6);
    }

    #[test]
    fn begin_atomic_is_compound() {
        let s = "BEGIN ATOMIC SELECT 1; SELECT 2; END;";
        let p = parts(s);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn user_variable_named_while_does_not_open_block() {
        // @while is a user variable; the `while` part must not be treated
        // as a WHILE keyword (otherwise `END WHILE` wouldn't be needed and
        // the trailing `;` of the SELECT would be suppressed).
        let s = "SELECT @while; SELECT 1;";
        let p = parts(s);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn keyword_inside_identifier_is_not_a_keyword() {
        // BEGINS / ENDED / BEGINNING should not match BEGIN / END.
        let s = "SELECT BEGINNING; SELECT ENDED;";
        let p = parts(s);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn dotted_column_ending_in_end_is_not_a_keyword() {
        // a column literally named `end` would be quoted, but a column
        // ending in `.end_at` should not match END as a keyword either.
        let s = "BEGIN SELECT t.end_at FROM t; END;";
        let p = parts(s);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn case_inside_string_is_ignored() {
        let s = "SELECT 'BEGIN END' AS x; SELECT 'CASE END' AS y;";
        let p = parts(s);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn semicolons_inside_block_comment_inside_begin_ignored() {
        let s = "\
CREATE PROCEDURE foo()
BEGIN
    /* ; ; ; */
    SELECT 1;
END;
";
        let p = parts(s);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn keyword_after_block_comment_still_detected() {
        // `END /* comment */ IF` should still match `END IF`.
        let s = "\
CREATE PROCEDURE foo()
BEGIN
    IF x THEN
        SELECT 1;
    END /* note */ IF;
    SELECT 2;
END;
";
        let p = parts(s);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn statement_at_cursor_inside_procedure_returns_full_procedure() {
        let s = "CREATE PROCEDURE foo() BEGIN SELECT 1; SELECT 2; END;";
        let inner = s.find("SELECT 2").unwrap() + 2;
        let r = statement_at_cursor(s, inner).unwrap();
        assert_eq!(&s[r], s);
    }
}

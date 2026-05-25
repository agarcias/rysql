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

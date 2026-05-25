//! SQL utilities: formatting and syntax highlighting.

pub fn format_sql(input: &str) -> String {
    sqlformat::format(
        input,
        &sqlformat::QueryParams::None,
        &sqlformat::FormatOptions::default(),
    )
}

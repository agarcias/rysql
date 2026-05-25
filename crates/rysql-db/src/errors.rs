//! Map low-level sqlx/MySQL errors to short, human-readable messages.
//!
//! The full driver message is preserved as a tooltip / log line; the friendly
//! string is what the status bar shows.

use sqlx::Error;

pub fn friendly(err: &Error) -> String {
    match err {
        Error::Database(db_err) => {
            let code = db_err.code().map(|c| c.to_string()).unwrap_or_default();
            let raw = db_err.message();
            if let Some(mysql_err) = db_err.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>() {
                if let Some(short) = friendly_mysql(mysql_err.number(), raw) {
                    return short;
                }
                format!("[{code}/{}] {raw}", mysql_err.number())
            } else {
                if code.is_empty() {
                    raw.to_string()
                } else {
                    format!("[{code}] {raw}")
                }
            }
        }
        Error::PoolTimedOut => "Connection pool timed out".into(),
        Error::PoolClosed => "Connection pool is closed".into(),
        Error::Io(e) => format!("I/O: {e}"),
        Error::Tls(e) => format!("TLS: {e}"),
        Error::Protocol(msg) => format!("Protocol error: {msg}"),
        Error::RowNotFound => "No row returned".into(),
        Error::ColumnNotFound(c) => format!("Column not found: {c}"),
        Error::ColumnIndexOutOfBounds { index, len } => {
            format!("Column index {index} out of bounds (have {len})")
        }
        Error::Decode(e) => format!("Decode error: {e}"),
        Error::Configuration(e) => format!("Configuration: {e}"),
        other => other.to_string(),
    }
}

fn friendly_mysql(number: u16, raw: &str) -> Option<String> {
    let s: String = match number {
        // Connection family
        1045 => "Access denied (check user / password)".into(),
        1130 => "Host not allowed to connect".into(),
        2002 => "Can't connect to MySQL through socket".into(),
        2003 => "Can't connect to MySQL on TCP".into(),
        2006 => "MySQL server has gone away (connection lost)".into(),
        2013 => "Lost connection to MySQL during query".into(),

        // Schema / object missing
        1044 => "Access denied to that database".into(),
        1046 => "No database selected (run `USE <db>;` first)".into(),
        1049 => format!("Unknown database — {raw}"),
        1051 => format!("Unknown table — {raw}"),
        1054 => format!("Unknown column — {raw}"),
        1146 => format!("Table doesn't exist — {raw}"),
        1305 => format!("Routine doesn't exist — {raw}"),
        1364 => format!("Field has no default value — {raw}"),

        // Constraints
        1062 => format!("Duplicate entry — {raw}"),
        1216 => "Cannot add or update child row: foreign key constraint fails".into(),
        1217 => "Cannot delete/update parent row: child rows exist".into(),
        1451 => "Cannot delete/update parent row: foreign key constraint fails".into(),
        1452 => "Cannot add child row: foreign key constraint fails".into(),
        1048 => format!("Column cannot be NULL — {raw}"),

        // Locking
        1205 => "Lock wait timeout exceeded — try again".into(),
        1213 => "Deadlock detected — transaction was rolled back, retry".into(),

        // SQL syntax / privileges
        1064 => format!("Syntax error — {raw}"),
        1142 => format!("Command denied (privileges) — {raw}"),
        1149 => "SQL syntax error".into(),
        1227 => format!("Access denied — {raw}"),

        _ => return None,
    };
    Some(s)
}

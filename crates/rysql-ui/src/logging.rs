//! Tracing setup: stdout + rotated daily file in the user's cache dir.
//! Also installs a panic hook that writes a crash report next to the logs.

use std::backtrace::Backtrace;
use std::io::Write;
use std::path::PathBuf;

use directories::ProjectDirs;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

/// Initialise tracing and the panic hook. Returns a guard that must be kept
/// alive for the lifetime of the process so the non-blocking writer flushes
/// on drop.
pub fn init() -> Option<WorkerGuard> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_thread_names(false);

    let mut guard: Option<WorkerGuard> = None;
    let file_layer = log_dir().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let appender = tracing_appender::rolling::daily(&dir, "rysql.log");
        let (nb, g) = tracing_appender::non_blocking(appender);
        guard = Some(g);
        Some(
            tracing_subscriber::fmt::layer()
                .with_writer(nb)
                .with_ansi(false)
                .with_target(false)
                .with_thread_names(false),
        )
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer.with_filter(EnvFilter::new("info")))
        .with(file_layer)
        .init();

    install_panic_hook();
    if let Some(dir) = log_dir() {
        tracing::info!(log_dir = %dir.display(), "rysql logging initialised");
    }

    guard
}

pub fn log_dir() -> Option<PathBuf> {
    let dirs = ProjectDirs::from("dev", "rysql", "rysql")?;
    Some(dirs.cache_dir().join("logs"))
}

pub fn crash_dir() -> Option<PathBuf> {
    let dirs = ProjectDirs::from("dev", "rysql", "rysql")?;
    Some(dirs.cache_dir().join("crashes"))
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Try to write a crash report; fall back silently if anything fails.
        if let Some(dir) = crash_dir() {
            if std::fs::create_dir_all(&dir).is_ok() {
                let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
                let path = dir.join(format!("panic-{ts}.txt"));
                if let Ok(mut f) = std::fs::File::create(&path) {
                    let _ = writeln!(f, "RySQL panic at {ts}");
                    let _ = writeln!(f, "Build: rysql-ui v{}", env!("CARGO_PKG_VERSION"));
                    let _ = writeln!(f, "---");
                    let _ = writeln!(f, "{info}");
                    let _ = writeln!(f, "---");
                    let _ = writeln!(f, "{}", Backtrace::force_capture());
                    tracing::error!(path = %path.display(), "panic written to crash log");
                }
            }
        }
        // Also log the panic via tracing so it ends up in rysql.log.
        tracing::error!(?info, "panic");
        default(info);
    }));
}

//! Build [`sqlx::MySqlPool`] instances from a [`ConnectionProfile`].

use std::time::{Duration, Instant};

use rysql_core::{ConnectionProfile, TlsMode};
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlSslMode};

use crate::Result;

/// Build a pool with sensible defaults for an interactive client (low timeouts,
/// small pool — RySQL multiplexes queries from a single user, not a web app).
pub async fn build_pool(profile: &ConnectionProfile, password: &str) -> Result<MySqlPool> {
    let mut opts = MySqlConnectOptions::new()
        .host(&profile.host)
        .port(profile.port)
        .username(&profile.user)
        .password(password)
        .ssl_mode(match profile.tls {
            TlsMode::Disabled => MySqlSslMode::Disabled,
            TlsMode::Preferred => MySqlSslMode::Preferred,
            TlsMode::Required => MySqlSslMode::Required,
        });

    if let Some(db) = profile.database.as_deref().filter(|s| !s.is_empty()) {
        opts = opts.database(db);
    }
    if let Some(sock) = profile.socket.as_deref().filter(|s| !s.is_empty()) {
        opts = opts.socket(sock);
    }

    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Some(Duration::from_secs(300)))
        .connect_with(opts)
        .await?;

    Ok(pool)
}

/// One-shot connection check: opens a pool, runs `SELECT 1`, closes it.
pub async fn test_connection(profile: &ConnectionProfile, password: &str) -> Result<Duration> {
    let start = Instant::now();
    let pool = build_pool(profile, password).await?;
    let res = sqlx::query("SELECT 1").execute(&pool).await;
    pool.close().await;
    res?;
    Ok(start.elapsed())
}

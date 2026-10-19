//! Nova's state store: PostgreSQL through `sqlx` (ticket stardelt-nova#103).
//!
//! The store is **optional**. `NOVA_DATABASE_URL` unset means "disabled": no pool, no
//! migration, and every route that does not need it behaves exactly as before. Set, it
//! must work: a URL that does not parse or a database that cannot be reached at start-up
//! refuses to start (the Deployment then keeps the previous pod serving) instead of
//! running half-configured. A database that goes away *after* start-up only turns
//! `/api/health` into `"db":"down"`; the routes that need it answer 5xx, the rest keep
//! working.
//!
//! The URL carries the password. It is never logged and never put in an error message.
//!
//! Schema changes are numbered files in `backend/migrations/`, embedded at compile time
//! and applied by `migrate()` at start-up. sqlx holds a Postgres advisory lock while it
//! migrates, so two replicas starting together do not race.

use std::str::FromStr;
use std::time::Duration;

use serde::Serialize;
use sqlx::PgPool;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// Every file in `backend/migrations/`, in version order, embedded in the binary.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// What `/api/health` reports about the state store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DbStatus {
    /// A `SELECT 1` answered within two seconds.
    Up,
    /// Configured, but it did not answer.
    Down,
    /// `NOVA_DATABASE_URL` is not set.
    Disabled,
}

/// The configured URL, or `None` when the variable is unset or blank.
pub fn url_from_env() -> Option<String> {
    clean_url(std::env::var("NOVA_DATABASE_URL").ok())
}

fn clean_url(raw: Option<String>) -> Option<String> {
    raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Open the pool. Errors name the problem, never the URL (it holds the password).
pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    let options = PgConnectOptions::from_str(url)
        .map_err(|_| anyhow::anyhow!("NOVA_DATABASE_URL is not a PostgreSQL connection URL"))?;
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .map_err(|e| anyhow::anyhow!("cannot connect to the state store: {e}"))
}

/// Apply every migration that has not run yet. Idempotent.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .map_err(|e| anyhow::anyhow!("state store migration failed: {e}"))
}

/// `Disabled` without a pool; otherwise `Up` only if a round trip succeeds in 2 s.
pub async fn status(pool: Option<&PgPool>) -> DbStatus {
    let Some(pool) = pool else {
        return DbStatus::Disabled;
    };
    match tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query("SELECT 1").execute(pool),
    )
    .await
    {
        Ok(Ok(_)) => DbStatus::Up,
        _ => DbStatus::Down,
    }
}

/// Helpers for the tests that need a real PostgreSQL. They read `NOVA_TEST_DATABASE_URL`
/// and the tests that use them are `#[ignore]`d, so a plain `cargo test` stays
/// database-free; CI runs them with `cargo test -- --ignored`.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// A pool on a schema of its own, so tests running in parallel never see each
    /// other's tables (sqlx puts `_sqlx_migrations` in the first schema of the
    /// `search_path` too). Nothing is migrated yet.
    pub(crate) async fn fresh_pool() -> PgPool {
        let url = std::env::var("NOVA_TEST_DATABASE_URL")
            .expect("NOVA_TEST_DATABASE_URL must point at a throwaway PostgreSQL database");
        let schema = format!(
            "t_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let admin = PgPool::connect(&url)
            .await
            .expect("connect to the test database");
        // sqlx 0.9 refuses a runtime-built SQL string unless it is wrapped; this one is
        // made of a process id and a counter, never of input.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .expect("create the test schema");
        admin.close().await;
        let options = PgConnectOptions::from_str(&url)
            .expect("NOVA_TEST_DATABASE_URL is a PostgreSQL URL")
            .options([("search_path", schema.as_str())]);
        PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect with the test schema")
    }

    /// `fresh_pool()` with every migration applied.
    pub(crate) async fn migrated_pool() -> PgPool {
        let pool = fresh_pool().await;
        migrate(&pool).await.expect("apply the migrations");
        pool
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::fresh_pool;
    use super::*;

    #[test]
    fn blank_or_missing_url_means_disabled() {
        assert_eq!(clean_url(None), None);
        assert_eq!(clean_url(Some(String::new())), None);
        assert_eq!(clean_url(Some("  \n".into())), None);
        assert_eq!(
            clean_url(Some(" postgres://u@h/d\n".into())).as_deref(),
            Some("postgres://u@h/d")
        );
    }

    #[tokio::test]
    async fn status_is_disabled_without_a_pool() {
        assert_eq!(status(None).await, DbStatus::Disabled);
    }

    #[tokio::test]
    async fn a_bad_url_is_refused_without_echoing_the_password() {
        let err = connect("postgres://nova:hunter2@db:notaport/nova")
            .await
            .expect_err("a URL with a bad port must not connect");
        let message = err.to_string();
        assert!(message.contains("NOVA_DATABASE_URL"), "got {message}");
        assert!(
            !message.contains("hunter2"),
            "the password leaked: {message}"
        );
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn migrate_creates_schema_info_and_is_idempotent() {
        let pool = fresh_pool().await;
        migrate(&pool).await.expect("first run applies 0001");
        migrate(&pool).await.expect("second run is a no-op");
        let store: String = sqlx::query_scalar("SELECT value FROM schema_info WHERE key = 'store'")
            .fetch_one(&pool)
            .await
            .expect("schema_info has the store row");
        assert_eq!(store, "nova");
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn status_is_up_then_down_when_the_pool_closes() {
        let pool = fresh_pool().await;
        assert_eq!(status(Some(&pool)).await, DbStatus::Up);
        pool.close().await;
        assert_eq!(status(Some(&pool)).await, DbStatus::Down);
    }
}

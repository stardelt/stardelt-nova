//! Shared accounts (stardelt-nova#725, the public demo): one sign-in that many visitors use.
//! The state store cannot tell one visitor from the next, so Nova says on `/api/me` that the
//! account is shared — the UI warns that others see what is typed and saved — and forgets the
//! account's saved queries, personal settings and query-log rows every night.

use std::time::Duration;

use chrono::Timelike;
use sqlx::PgPool;
use tracing::{info, warn};

/// How often the nightly reset looks at the clock. A restart inside the reset hour still
/// finds its tick, so the reset runs once per night.
const TICK_EVERY: Duration = Duration::from_secs(600);

/// The UTC hour (03:00) in which a shared account's data is removed.
const RESET_HOUR: u32 = 3;

/// The sign-in names `NOVA_SHARED_ACCOUNTS` lists, comma-separated. Unset or empty gives an
/// empty list: no account is shared, and the nightly reset does not run.
pub fn from_env() -> Vec<String> {
    let raw = std::env::var("NOVA_SHARED_ACCOUNTS").unwrap_or_default();
    parse(&raw)
}

/// What `from_env` makes of the variable's value: split on `,`, trimmed, empties dropped,
/// lower-cased so the comparison against a display name never depends on the case.
fn parse(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Whether `display_name` is one of the shared accounts. Case-insensitive, like `parse`.
pub fn is_shared(accounts: &[String], display_name: &str) -> bool {
    accounts
        .iter()
        .any(|account| account.eq_ignore_ascii_case(display_name))
}

/// Remove the shared accounts' saved queries (with their `query_share` rows), personal
/// settings and query-log rows in one transaction; returns how many rows that removed.
pub async fn purge(pool: &PgPool, accounts: &[String]) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let saved = sqlx::query(
        "DELETE FROM saved_query WHERE owner_sub IN \
            (SELECT sub FROM person WHERE lower(username) = ANY($1))",
    )
    .bind(accounts)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let settings = sqlx::query(
        "DELETE FROM user_setting WHERE owner_sub IN \
            (SELECT sub FROM person WHERE lower(username) = ANY($1))",
    )
    .bind(accounts)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let history = sqlx::query("DELETE FROM query_log WHERE lower(principal) = ANY($1)")
        .bind(accounts)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(saved + settings + history)
}

/// Reset the shared accounts once a night for as long as Nova runs. Without a configured
/// shared account there is nothing to reset, and the loop is not started.
pub fn spawn_nightly(pool: PgPool, accounts: Vec<String>) {
    if accounts.is_empty() {
        info!("no shared accounts (NOVA_SHARED_ACCOUNTS)");
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK_EVERY);
        let mut last: Option<chrono::NaiveDate> = None;
        loop {
            tick.tick().await;
            // The workspace builds `chrono` without its `clock` feature (`Utc::now()` does
            // not exist here); the current time comes from the system clock, as in
            // `workspace.rs`.
            let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
            if now.hour() != RESET_HOUR {
                continue;
            }
            let today = now.date_naive();
            if last == Some(today) {
                continue;
            }
            // Counts only: the log never names the accounts.
            match purge(&pool, &accounts).await {
                Ok(rows) => {
                    info!(rows, "shared accounts reset");
                    last = Some(today);
                }
                // Not marked done: the next tick in this hour tries again.
                Err(e) => warn!(error = %e, "shared-account reset failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::migrated_pool;

    #[test]
    fn from_env_parsing_splits_on_commas_trims_and_lower_cases() {
        // `from_env` is this parser over `NOVA_SHARED_ACCOUNTS`; the variable itself is
        // process-global, so the test calls the parser it reads through.
        assert_eq!(parse(" Demo, ,x "), ["demo", "x"]);
        assert_eq!(parse(""), Vec::<String>::new());
        assert_eq!(parse(" , , "), Vec::<String>::new());
    }

    #[test]
    fn is_shared_ignores_case() {
        let accounts = parse("demo");
        assert!(is_shared(&accounts, "demo"));
        assert!(is_shared(&accounts, "Demo"));
        assert!(!is_shared(&accounts, "ada"));
        assert!(!is_shared(&[], "demo"));
    }

    /// One person, one saved query, one setting and one logged query for `name`.
    async fn seed(pool: &PgPool, sub: &str, name: &str) {
        sqlx::query("INSERT INTO person (sub, username, display_name) VALUES ($1, $2, $2)")
            .bind(sub)
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO saved_query (owner_sub, title, sql) VALUES ($1, $2, $3)")
            .bind(sub)
            .bind(format!("{name} query"))
            .bind("select 1")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user_setting (owner_sub, key, value) VALUES ($1, $2, $3)")
            .bind(sub)
            .bind(format!("{name}.key"))
            .bind(serde_json::json!({"theme": "dark"}))
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO query_log (query_id, state, principal, sql_text, sql_hash, created_at) \
             VALUES ($1, 'FINISHED', $2, 'select 1', 'hash', now())",
        )
        .bind(format!("{sub}_q"))
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn purge_removes_the_shared_accounts_rows_and_leaves_the_others() {
        let pool = migrated_pool().await;
        seed(&pool, "sub-demo", "demo").await;
        seed(&pool, "sub-ada", "ada").await;
        // The person is named `demo` in the directory, whatever case `NOVA_SHARED_ACCOUNTS`
        // carries; the comparison is lower on both sides.
        assert_eq!(purge(&pool, &parse(" Demo ")).await.unwrap(), 3);

        let owners: Vec<String> = sqlx::query_scalar("SELECT owner_sub FROM saved_query")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(owners, ["sub-ada"]);
        let settings: Vec<String> = sqlx::query_scalar("SELECT owner_sub FROM user_setting")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(settings, ["sub-ada"]);
        let principals: Vec<String> = sqlx::query_scalar("SELECT principal FROM query_log")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(principals, ["ada"]);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM person")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2,
            "the directory keeps both people: only their data goes"
        );
    }
}

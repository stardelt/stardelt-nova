//! Browser sessions in the state store (stardelt-nova#103 step 4, slice b).
//!
//! One row per signed-in browser, so that a Nova restart does not sign everyone out.
//! The in-memory session store in `auth.rs` stays the cache; slice c wires it to this
//! module.
//!
//! - The row key is `id_hash(session id)`: the SHA-256 of the cookie's random id, never
//!   the id. A copy of the table cannot be replayed as a cookie.
//! - `sealed` is opaque here: `seal.rs` seals the tokens before they reach this module.
//! - **Best effort.** Without a pool every call is a no-op or `None`. A database error
//!   is logged (never with a value) and read as "no row". The in-memory store stays the
//!   fallback, so a state store outage signs out nobody who is still in memory.

use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// The row key for a session id: the lowercase hex SHA-256, 64 characters.
pub fn id_hash(session_id: &str) -> String {
    Sha256::digest(session_id.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One stored session, as `load` returns it. No `Debug`: it holds credentials, even if
/// sealed.
pub struct StoredSession {
    /// The sealed credentials.
    pub sealed: Vec<u8>,
    /// Unix seconds of the absolute session end. A refresh never moves it.
    pub expires_at: i64,
}

/// Insert the row of a new session, or replace its sealed credentials after a refresh.
/// `expires_at` is written on insert only: a refresh never moves the session's end.
pub async fn save(pool: Option<&PgPool>, id_hash: &str, sealed: &[u8], expires_at: i64, now: i64) {
    let Some(pool) = pool else { return };
    let result = sqlx::query(
        "INSERT INTO browser_session (id_hash, sealed, expires_at, updated_at) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (id_hash) DO UPDATE SET \
             sealed = EXCLUDED.sealed, updated_at = EXCLUDED.updated_at",
    )
    .bind(id_hash)
    .bind(sealed)
    .bind(expires_at)
    .bind(now)
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::warn!("session not saved to the state store: {e}");
    }
}

/// The live session behind `id_hash`. `None` when there is no row, when the row ended
/// at or before `now`, or when the database cannot answer.
pub async fn load(pool: Option<&PgPool>, id_hash: &str, now: i64) -> Option<StoredSession> {
    let pool = pool?;
    let row: Result<Option<(Vec<u8>, i64)>, sqlx::Error> = sqlx::query_as(
        "SELECT sealed, expires_at FROM browser_session \
         WHERE id_hash = $1 AND expires_at > $2",
    )
    .bind(id_hash)
    .bind(now)
    .fetch_optional(pool)
    .await;
    match row {
        Ok(row) => row.map(|(sealed, expires_at)| StoredSession { sealed, expires_at }),
        Err(e) => {
            tracing::warn!("session not read from the state store: {e}");
            None
        }
    }
}

/// Remove one session: a sign-out, or a refresh the identity provider refused.
pub async fn delete(pool: Option<&PgPool>, id_hash: &str) {
    let Some(pool) = pool else { return };
    let result = sqlx::query("DELETE FROM browser_session WHERE id_hash = $1")
        .bind(id_hash)
        .execute(pool)
        .await;
    if let Err(e) = result {
        tracing::warn!("session not removed from the state store: {e}");
    }
}

/// Remove every session that ended at or before `now`, and say how many. Slice c calls
/// it at each sign-in, so the table holds the live sessions and little else.
pub async fn purge_expired(pool: Option<&PgPool>, now: i64) -> u64 {
    let Some(pool) = pool else { return 0 };
    match sqlx::query("DELETE FROM browser_session WHERE expires_at <= $1")
        .bind(now)
        .execute(pool)
        .await
    {
        Ok(done) => done.rows_affected(),
        Err(e) => {
            tracing::warn!("expired sessions not purged from the state store: {e}");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_key_is_the_sha256_of_the_id_and_never_the_id() {
        let hash = id_hash("abc");
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(id_hash("abc").len(), 64);
        assert_ne!(id_hash("abc"), id_hash("abd"));
    }

    #[tokio::test]
    async fn without_a_pool_nothing_is_stored_or_found() {
        save(None, &id_hash("s"), b"sealed", 2_000, 1_000).await;
        assert!(load(None, &id_hash("s"), 1_000).await.is_none());
        delete(None, &id_hash("s")).await;
        assert_eq!(purge_expired(None, 1_000).await, 0);
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn a_saved_session_is_found_until_it_ends() {
        let pool = crate::db::test_support::migrated_pool().await;
        let key = id_hash("session-a");
        save(Some(&pool), &key, b"sealed-1", 2_000, 1_000).await;
        let found = load(Some(&pool), &key, 1_999)
            .await
            .expect("the row is live");
        assert_eq!(found.sealed, b"sealed-1");
        assert_eq!(found.expires_at, 2_000);
        assert!(load(Some(&pool), &key, 2_000).await.is_none());
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn a_second_save_replaces_the_credentials_and_keeps_the_end() {
        let pool = crate::db::test_support::migrated_pool().await;
        let key = id_hash("session-b");
        save(Some(&pool), &key, b"sealed-1", 2_000, 1_000).await;
        save(Some(&pool), &key, b"sealed-2", 9_999, 1_500).await;
        let found = load(Some(&pool), &key, 1_500)
            .await
            .expect("the row is live");
        assert_eq!(found.sealed, b"sealed-2");
        assert_eq!(found.expires_at, 2_000, "a refresh never moves the end");
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn delete_and_purge_remove_only_their_rows() {
        let pool = crate::db::test_support::migrated_pool().await;
        let (a, b, c) = (id_hash("a"), id_hash("b"), id_hash("c"));
        save(Some(&pool), &a, b"a", 3_000, 1_000).await;
        save(Some(&pool), &b, b"b", 3_000, 1_000).await;
        save(Some(&pool), &c, b"c", 1_200, 1_000).await;
        delete(Some(&pool), &a).await;
        assert!(load(Some(&pool), &a, 1_100).await.is_none());
        assert!(load(Some(&pool), &b, 1_100).await.is_some());
        assert_eq!(purge_expired(Some(&pool), 2_500).await, 1);
        let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM browser_session")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 1, "only b is left");
    }
}

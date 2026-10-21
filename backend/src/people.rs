//! The person directory (stardelt-nova#235, person 4/4): who a subject is, in names.
//!
//! Once the engine runs every query as the person's `sub` (authz 8/8, with Superset
//! impersonation from stardelt-platform#310), the query log records an opaque UUID as the
//! engine user. Nova already knows the same person at sign-in — `Identity` carries the `sub`,
//! the sign-in name and the human name — so this module keeps a small, best-effort row per
//! subject and hands the display name back to the query log. It is not a second identity
//! source: nothing here is authoritative, every field comes from the token Nova just verified,
//! and a missing row only means the raw value is shown.
//!
//! Writes are throttled to one per person per ten minutes (in memory, like the other Nova rate
//! limits) because a directory does not need to see every request. The store is optional:
//! without `NOVA_DATABASE_URL` both functions do nothing, and no caller fails because of it.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tracing::warn;

use crate::AppState;
use crate::auth::Identity;

/// At most one write per person per this window.
const TOUCH_INTERVAL: Duration = Duration::from_secs(600);

/// When each subject was last written, so a page load cannot turn into a write storm.
/// The map is per process and only a throttle: a restart writes once more per person.
pub type TouchLimiter = Mutex<HashMap<String, Instant>>;

/// Record a signed-in person, best effort. Skipped without a state store and at most once
/// per subject per [`TOUCH_INTERVAL`]. A failed write is logged and otherwise ignored: the
/// sign-in it belongs to has already succeeded and must not depend on the directory.
pub async fn touch(state: &AppState, identity: &Identity) {
    let Some(db) = state.db.as_ref() else {
        return;
    };
    if !claim_slot(&state.people_touched, &identity.sub, Instant::now()) {
        return;
    }
    if let Err(e) = write(db, identity).await {
        // Best effort: the sign-in it belongs to has already succeeded, and `Identity`
        // carries no token to leak here.
        warn!(sub = %identity.sub, error = %e, "could not record a person in the directory");
    }
}

/// May this request write the person? `false` while another write is less than
/// [`TOUCH_INTERVAL`] old. A poisoned lock still answers (the map is only a throttle).
fn claim_slot(limiter: &TouchLimiter, sub: &str, now: Instant) -> bool {
    let mut seen = limiter.lock().unwrap_or_else(PoisonError::into_inner);
    match seen.get(sub) {
        Some(last) if now.saturating_duration_since(*last) < TOUCH_INTERVAL => false,
        _ => {
            seen.insert(sub.to_string(), now);
            true
        }
    }
}

/// `username` is the sign-in name the engine knows; `display_name` is what a person reads.
/// The token's `name` is the human name; `Identity::display_name` (preferred_username, else
/// email) is the fallback, so a provider that sends no `name` still shows something better
/// than the `sub`.
async fn write(db: &PgPool, identity: &Identity) -> sqlx::Result<()> {
    let display_name = identity
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&identity.display_name);
    sqlx::query(
        "INSERT INTO person (sub, username, display_name, email, last_seen) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (sub) DO UPDATE SET \
            username = EXCLUDED.username, display_name = EXCLUDED.display_name, \
            email = EXCLUDED.email, last_seen = now()",
    )
    .bind(&identity.sub)
    .bind(&identity.display_name)
    .bind(display_name)
    .bind(&identity.email)
    .execute(db)
    .await
    .map(|_| ())
}

/// Display names for `subs`, one query. Unknown subjects are simply absent from the map (the
/// caller keeps the raw engine user), and an unreadable directory answers the same way: this
/// is decoration on a log row, never a reason to fail the page.
pub async fn names(db: &PgPool, subs: &[String]) -> HashMap<String, String> {
    if subs.is_empty() {
        return HashMap::new();
    }
    match sqlx::query_as::<_, (String, String)>(
        "SELECT sub, display_name FROM person WHERE sub = ANY($1)",
    )
    .bind(subs)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows.into_iter().collect(),
        Err(e) => {
            warn!(error = %e, "could not read names from the person directory");
            HashMap::new()
        }
    }
}

/// Is this engine user a UUID, i.e. a person's `sub` rather than a sign-in name? Only the
/// canonical hyphenated form counts; the directory is never asked about anything else. No
/// `uuid` dependency for one shape check.
pub fn is_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(sub: &str, username: &str, name: Option<&str>) -> Identity {
        Identity {
            sub: sub.into(),
            display_name: username.into(),
            email: Some(format!("{username}@example.com")),
            name: name.map(str::to_string),
            groups: vec![],
            roles: vec![],
            groups_overage: false,
            is_admin: false,
        }
    }

    #[test]
    fn only_the_hyphenated_uuid_shape_is_a_subject() {
        assert!(is_uuid("0b6b1f0e-4f0e-4d4e-8e3a-1f2a3b4c5d6e"));
        assert!(is_uuid("0B6B1F0E-4F0E-4D4E-8E3A-1F2A3B4C5D6E"));
        assert!(!is_uuid("alice"));
        assert!(!is_uuid("0b6b1f0e4f0e4d4e8e3a1f2a3b4c5d6e"));
        assert!(!is_uuid("0b6b1f0e-4f0e-4d4e-8e3a-1f2a3b4c5d6"));
        assert!(!is_uuid("0b6b1f0e-4f0e-4d4e-8e3a-1f2a3b4c5d6z"));
    }

    #[tokio::test]
    async fn touch_skips_without_a_state_store() {
        let state = crate::router_tests::test_state();
        let alice = identity(
            "0b6b1f0e-4f0e-4d4e-8e3a-1f2a3b4c5d6e",
            "alice",
            Some("Alice"),
        );
        touch(&state, &alice).await;
        assert!(
            state.people_touched.lock().unwrap().is_empty(),
            "there is nothing to write and nothing to throttle without a database"
        );
    }

    #[test]
    fn a_second_touch_within_ten_minutes_does_not_write() {
        let limiter = TouchLimiter::default();
        let now = Instant::now();
        // Five subjects, so the throttle is visibly per person and not per process.
        for sub in [
            "0b6b1f0e-4f0e-4d4e-8e3a-1f2a3b4c5d6e",
            "1c7c2f1f-5a1f-4e5f-9f4b-2a3b4c5d6e7f",
            "2d8d3a20-6b20-4f60-a05c-3b4c5d6e7f80",
            "3e9e4b31-7c31-4071-b16d-4c5d6e7f8091",
            "4faf5c42-8d42-4182-c27e-5d6e7f8091a2",
        ] {
            assert!(claim_slot(&limiter, sub, now), "first touch of {sub}");
            assert!(
                !claim_slot(&limiter, sub, now + Duration::from_secs(599)),
                "a second touch of {sub} inside ten minutes must not write"
            );
            assert!(
                claim_slot(&limiter, sub, now + Duration::from_secs(600)),
                "ten minutes later {sub} may be written again"
            );
        }
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn names_returns_only_known_subs_and_touch_throttles_the_write() {
        let pool = crate::db::test_support::migrated_pool().await;
        let state = crate::router_tests::test_state_with_db(Some(pool.clone()));
        let alice = identity(
            "0b6b1f0e-4f0e-4d4e-8e3a-1f2a3b4c5d6e",
            "alice",
            Some("Alice Example"),
        );
        let unknown = "9a9a9a9a-9a9a-4a9a-8a9a-9a9a9a9a9a9a".to_string();

        touch(&state, &alice).await;
        let resolved = names(&pool, &[alice.sub.clone(), unknown.clone()]).await;
        assert_eq!(
            resolved.get(&alice.sub).map(String::as_str),
            Some("Alice Example")
        );
        assert!(
            !resolved.contains_key(&unknown),
            "an unknown subject has no display name"
        );

        // The row carries the sign-in name and the email the token supplied.
        let (username, email): (String, Option<String>) =
            sqlx::query_as("SELECT username, email FROM person WHERE sub = $1")
                .bind(&alice.sub)
                .fetch_one(&pool)
                .await
                .expect("the person row");
        assert_eq!(username, "alice");
        assert_eq!(email.as_deref(), Some("alice@example.com"));

        // A second call inside the window leaves the row alone: age `last_seen` first so a
        // write would be visible, then touch again while the throttle still holds the subject.
        sqlx::query("UPDATE person SET last_seen = '2000-01-01' WHERE sub = $1")
            .bind(&alice.sub)
            .execute(&pool)
            .await
            .expect("age the row");
        touch(&state, &alice).await;
        let untouched: i64 = sqlx::query_scalar(
            "SELECT extract(epoch FROM last_seen)::bigint FROM person WHERE sub = $1",
        )
        .bind(&alice.sub)
        .fetch_one(&pool)
        .await
        .expect("the row is still there");
        assert_eq!(untouched, 946_684_800, "2000-01-01T00:00:00Z");

        // Once the throttle forgets the subject, the next touch writes again.
        state.people_touched.lock().unwrap().clear();
        touch(&state, &alice).await;
        let touched: i64 = sqlx::query_scalar(
            "SELECT extract(epoch FROM last_seen)::bigint FROM person WHERE sub = $1",
        )
        .bind(&alice.sub)
        .fetch_one(&pool)
        .await
        .expect("the row is still there");
        assert!(touched > 946_684_800, "the second window writes again");
    }
}

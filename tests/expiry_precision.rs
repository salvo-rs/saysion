#![cfg(feature = "sqlx-sqlite")]

use saysion::{Session, SessionStore, SqlxSqliteStore, time::OffsetDateTime};

#[tokio::test]
async fn sqlite_preserves_subsecond_expiry_with_legacy_seconds_schema() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let store = SqlxSqliteStore::new(pool.clone());
    store.migrate().await.unwrap();

    // Start near the beginning of a second, leaving ample time for in-memory I/O.
    let now = OffsetDateTime::now_utc();
    if now.nanosecond() > 100_000_000 {
        tokio::time::sleep(std::time::Duration::from_nanos(
            1_100_000_000 - u64::from(now.nanosecond()),
        ))
        .await;
    }
    let now = OffsetDateTime::now_utc();
    let expiry = now.replace_nanosecond(999_999_999).unwrap();
    let mut session = Session::new();
    session.set_expiry(expiry);
    let id = session.id().to_owned();
    let cookie = store.store_session(session).await.unwrap().unwrap();

    let (stored_seconds,): (i64,) =
        sqlx::query_as("SELECT expires FROM saysion_sessions WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_seconds, expiry.unix_timestamp());
    assert!(store.load_session(cookie.clone()).await.unwrap().is_some());

    // The coarse SQL predicate must not replace exact Session validation.
    let mut expired = Session::new();
    expired.set_expiry(OffsetDateTime::now_utc() - std::time::Duration::from_nanos(1));
    let expired_cookie = store.store_session(expired).await.unwrap().unwrap();
    assert!(store.load_session(expired_cookie).await.unwrap().is_none());

    let remaining = expiry - OffsetDateTime::now_utc();
    if let Ok(remaining) = std::time::Duration::try_from(remaining) {
        tokio::time::sleep(remaining + std::time::Duration::from_millis(5)).await;
    }
    assert!(store.load_session(cookie).await.unwrap().is_none());
}

#![cfg(feature = "sqlx-sqlite")]

use saysion::{SqlxSqliteStore, time::OffsetDateTime};

#[tokio::test]
async fn cleanup_upgrades_existing_tables_and_uses_the_expiry_index() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query(
        "CREATE TABLE saysion_sessions (id TEXT PRIMARY KEY NOT NULL, \
         expires INTEGER NULL, session TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let store = SqlxSqliteStore::new(pool.clone());
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();

    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT id FROM saysion_sessions \
         WHERE expires IS NOT NULL AND expires < ? ORDER BY expires, id LIMIT ?",
    )
    .bind(OffsetDateTime::now_utc().unix_timestamp())
    .bind(100_i64)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        plan.iter().any(|row| row.3.contains("saysion_expiry_")),
        "expiry cleanup should use its index: {plan:?}"
    );
}

#[tokio::test]
async fn cleanup_batches_are_bounded_and_preserve_live_rows() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let store = SqlxSqliteStore::new(pool.clone());
    store.migrate().await.unwrap();
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut transaction = pool.begin().await.unwrap();
    for index in 0..1_012 {
        let expiry = match index {
            0..1_007 => Some(now - 60),
            1_007..1_010 => Some(now + 3_600),
            _ => None,
        };
        sqlx::query("INSERT INTO saysion_sessions (id, expires, session) VALUES (?, ?, '{}')")
            .bind(format!("session-{index}"))
            .bind(expiry)
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    assert!(store.cleanup_batch(0).await.is_err());
    assert!(store.cleanup_batch(10_001).await.is_err());
    store.cleanup().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 12);
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 3);
    assert_eq!(store.count().await.unwrap(), 9);
    assert_eq!(store.cleanup_batch(10_000).await.unwrap(), 4);
    assert_eq!(store.count().await.unwrap(), 5);
    assert_eq!(store.cleanup_batch(10_000).await.unwrap(), 0);
}

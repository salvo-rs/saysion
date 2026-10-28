#![cfg(feature = "sqlx-mysql")]
//! Integration tests for `SqlxMySqlStore`.
//!
//! Set `MYSQL_URL` (default
//! `mysql://root:root@127.0.0.1/saysion_test`).
//! Run with `cargo test --features sqlx-mysql -- --ignored`.

use std::time::Duration;

use saysion::{Session, SessionStore, SqlxMySqlStore};

fn url() -> String {
    std::env::var("MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:root@127.0.0.1/saysion_test".to_string())
}

async fn fresh_store() -> SqlxMySqlStore {
    let store = SqlxMySqlStore::from_url(&url())
        .await
        .expect("connect to mysql")
        .with_table("saysion_test_sessions");
    store.migrate().await.expect("migrate");
    store.clear_store().await.expect("clear");
    store
}

#[tokio::test]
#[ignore]
async fn mysql_roundtrip() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.insert("hello", "world").unwrap();
    let cookie = store.store_session(session).await.unwrap().unwrap();

    let loaded = store.load_session(cookie).await.unwrap().unwrap();
    assert_eq!(loaded.get::<String>("hello").unwrap(), "world");

    store.clear_store().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn mysql_expiry_and_cleanup() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.expire_in(Duration::from_secs(1));
    let cookie = store.store_session(session).await.unwrap().unwrap();

    assert!(store.load_session(cookie.clone()).await.unwrap().is_some());
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(store.load_session(cookie).await.unwrap().is_none());

    store.cleanup().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
}

#[tokio::test]
#[ignore]
async fn mysql_destroy_and_clear() {
    let store = fresh_store().await;

    for _ in 0..3 {
        store.store_session(Session::new()).await.unwrap();
    }
    let cookie = store.store_session(Session::new()).await.unwrap().unwrap();
    assert_eq!(store.count().await.unwrap(), 4);

    let loaded = store.load_session(cookie.clone()).await.unwrap().unwrap();
    store.destroy_session(loaded).await.unwrap();
    assert!(store.load_session(cookie).await.unwrap().is_none());

    store.clear_store().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
}

#[tokio::test]
#[ignore]
async fn mysql_migration_is_idempotent_and_cleanup_is_bounded() {
    let pool = sqlx::MySqlPool::connect(&url()).await.unwrap();
    let table = format!(
        "saysion_cleanup_{}",
        saysion::time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let store = SqlxMySqlStore::new(pool.clone()).with_table(&table);
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();
    let (first, second) = tokio::join!(store.migrate(), store.migrate());
    first.unwrap();
    second.unwrap();
    for index in 0..7 {
        sqlx::query(&format!(
            "INSERT INTO {table} (id, expires, session) VALUES (?, ?, '{{}}')"
        ))
        .bind(format!("expired-{index}"))
        .bind(saysion::time::OffsetDateTime::now_utc().unix_timestamp() - 60)
        .execute(&pool)
        .await
        .unwrap();
    }
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 3);
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 3);
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 1);
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 0);
    sqlx::query(&format!("DROP TABLE {table}"))
        .execute(&pool)
        .await
        .unwrap();
}

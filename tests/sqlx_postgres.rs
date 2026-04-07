#![cfg(feature = "sqlx-postgres")]
//! Integration tests for `SqlxPostgresStore`.
//!
//! Set `POSTGRES_URL` (default
//! `postgres://postgres:postgres@127.0.0.1/saysion_test`).
//! Run with `cargo test --features sqlx-postgres -- --ignored`.

use std::time::Duration;

use saysion::{Session, SessionStore, SqlxPostgresStore};

fn url() -> String {
    std::env::var("POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1/saysion_test".to_string())
}

async fn fresh_store() -> SqlxPostgresStore {
    let store = SqlxPostgresStore::from_url(&url())
        .await
        .expect("connect to postgres")
        .with_table("saysion_test_sessions");
    store.migrate().await.expect("migrate");
    store.clear_store().await.expect("clear");
    store
}

#[tokio::test]
#[ignore]
async fn pg_roundtrip() {
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
async fn pg_expiry_and_cleanup() {
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
async fn pg_destroy_and_clear() {
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

#![cfg(feature = "sqlx-sqlite")]
//! Integration tests for `SqlxSqliteStore`.
//!
//! Uses an isolated in-memory database per test and runs without services:
//! `cargo test --features sqlx-sqlite --test sqlx_sqlite`.

mod common;

use std::time::Duration;

use saysion::{Session, SessionStore, SqlxSqliteStore};

async fn fresh_store() -> SqlxSqliteStore {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect to sqlite");
    let store = SqlxSqliteStore::new(pool);
    store.migrate().await.expect("migrate");
    store
}

#[tokio::test]
async fn sqlite_corrupt_session_is_an_error() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let store = SqlxSqliteStore::new(pool.clone());
    store.migrate().await.unwrap();
    let cookie = store.store_session(Session::new()).await.unwrap().unwrap();
    let id = Session::id_from_cookie_value(&cookie).unwrap();
    sqlx::query("UPDATE saysion_sessions SET session = ? WHERE id = ?")
        .bind("invalid JSON")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();

    let error = store.load_session(cookie).await.unwrap_err();
    assert!(error.downcast_ref::<serde_json::Error>().is_some());
    let missing = Session::new().into_cookie_value().unwrap();
    assert!(store.load_session(missing).await.unwrap().is_none());
}

#[tokio::test]
async fn sqlite_roundtrip() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.insert("hello", "world").unwrap();
    let cookie = store.store_session(session).await.unwrap().unwrap();

    let loaded = store.load_session(cookie).await.unwrap().unwrap();
    assert_eq!(loaded.get::<String>("hello").unwrap(), "world");
    common::assert_contract(&store).await;
}

#[tokio::test]
async fn sqlite_expiry_and_cleanup() {
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
async fn sqlite_destroy_and_clear() {
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

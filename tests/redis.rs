#![cfg(feature = "redis")]
//! Integration tests for `RedisStore`.
//!
//! Requires a running Redis instance. Set `REDIS_URL` to override the
//! default of `redis://127.0.0.1:6379`. All tests are `#[ignore]` —
//! run with `cargo test --features redis -- --ignored`.

mod common;

use std::time::Duration;

use saysion::{RedisStore, Session, SessionStore};

fn url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

async fn fresh_store() -> RedisStore {
    let store = RedisStore::from_url(&url())
        .await
        .expect("connect to redis")
        .with_prefix(format!("saysion-test/{:016x}/", rand::random::<u64>()));
    store.clear_store().await.expect("clear");
    store
}

#[tokio::test]
#[ignore]
async fn redis_rejects_a_valid_blob_bound_to_another_record() {
    use redis::AsyncCommands;

    let prefix = format!("saysion-binding/{:016x}/", rand::random::<u64>());
    let store = RedisStore::from_url(&url())
        .await
        .unwrap()
        .with_prefix(&prefix);
    common::assert_identity_binding(&store, |id, blob| async move {
        let mut conn = redis::Client::open(url())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let _: () = conn.set(format!("{prefix}{id}"), blob).await.unwrap();
    })
    .await;
    store.clear_store().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn redis_roundtrip() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.insert("hello", "world").unwrap();
    let cookie = store.store_session(session).await.unwrap().unwrap();

    let loaded = store.load_session(cookie).await.unwrap().unwrap();
    assert_eq!(loaded.get::<String>("hello").unwrap(), "world");
    common::assert_contract(&store).await;

    store.clear_store().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn redis_expiry() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.expire_in(Duration::from_secs(1));
    session.insert("k", "v").unwrap();
    let cookie = store.store_session(session).await.unwrap().unwrap();

    assert!(store.load_session(cookie.clone()).await.unwrap().is_some());
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(store.load_session(cookie).await.unwrap().is_none());

    store.clear_store().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn redis_destroy_and_clear() {
    let store = fresh_store().await;

    for _ in 0..3 {
        store.store_session(Session::new()).await.unwrap();
    }
    let mut s = Session::new();
    s.insert("a", 1).unwrap();
    let cookie = store.store_session(s).await.unwrap().unwrap();
    assert_eq!(store.count().await.unwrap(), 4);

    let loaded = store.load_session(cookie.clone()).await.unwrap().unwrap();
    store.destroy_session(loaded).await.unwrap();
    assert!(store.load_session(cookie).await.unwrap().is_none());
    assert_eq!(store.count().await.unwrap(), 3);

    store.clear_store().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
}

#[tokio::test]
#[ignore]
async fn redis_expired_write_removes_key_instead_of_persisting_it() {
    use redis::AsyncCommands;

    let prefix = format!("saysion-expired-{}/", Session::new().id());
    let store = RedisStore::from_url(&url())
        .await
        .unwrap()
        .with_prefix(&prefix);
    let mut session = Session::new();
    let key = format!("{}{}", prefix, session.id());
    store.store_session(session.clone()).await.unwrap();

    session.set_expiry(saysion::time::OffsetDateTime::now_utc() - Duration::from_secs(1));
    assert!(store.store_session(session).await.unwrap().is_none());
    let mut conn = redis::Client::open(url())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let exists: bool = conn.exists(&key).await.unwrap();
    assert!(!exists);

    let permanent = Session::new();
    let permanent_key = format!("{}{}", prefix, permanent.id());
    store.store_session(permanent).await.unwrap();
    let ttl: i64 = conn.pttl(&permanent_key).await.unwrap();
    assert_eq!(ttl, -1, "sessions without expiry must remain persistent");
    store.clear_store().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn redis_clear_batches_and_preserves_literal_prefix_isolation() {
    use redis::AsyncCommands;

    let root = format!("saysion-scan-{}/", Session::new().id());
    let prefix = format!("{root}*?[]\\/");
    let neighbor = format!("{root}neighbor");
    let store = RedisStore::from_url(&url())
        .await
        .unwrap()
        .with_prefix(&prefix);
    let mut conn = redis::Client::open(url())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let mut pipeline = redis::pipe();
    for index in 0..700 {
        pipeline.set(format!("{prefix}{index}"), "session").ignore();
    }
    pipeline.set(&neighbor, "keep").ignore();
    let _: () = pipeline.query_async(&mut conn).await.unwrap();

    assert_eq!(store.count().await.unwrap(), 700);
    store.clear_store().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
    let value: String = conn.get(&neighbor).await.unwrap();
    assert_eq!(value, "keep");
    let _: usize = conn.del(&neighbor).await.unwrap();
}

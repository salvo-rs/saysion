#![cfg(feature = "mongodb")]
//! Integration tests for `MongoDbStore`.
//!
//! Requires a running MongoDB instance. Set `MONGODB_URL` to override
//! the default of `mongodb://127.0.0.1:27017`. All tests are
//! `#[ignore]` — run with `cargo test --features mongodb -- --ignored`.

mod common;

use std::time::Duration;

use saysion::{MongoDbStore, Session, SessionStore};

fn url() -> String {
    std::env::var("MONGODB_URL").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_string())
}

struct TestStore {
    store: MongoDbStore,
    client: mongodb::Client,
    collection: String,
}

impl std::ops::Deref for TestStore {
    type Target = MongoDbStore;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

impl TestStore {
    async fn finish(self) {
        self.client
            .database("saysion_test")
            .collection::<mongodb::bson::Document>(&self.collection)
            .drop()
            .await
            .expect("drop test collection");
    }
}

async fn fresh_store() -> TestStore {
    let client = mongodb::Client::with_uri_str(url())
        .await
        .expect("connect to mongodb");
    let collection = format!("sessions_test_{:016x}", rand::random::<u64>());
    let store = MongoDbStore::new(client.clone(), "saysion_test").with_collection(&collection);
    store.initialize().await.expect("initialize TTL index");
    TestStore {
        store,
        client,
        collection,
    }
}

#[tokio::test]
#[ignore]
async fn mongodb_roundtrip() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.insert("hello", "world").unwrap();
    let cookie = store.store_session(session).await.unwrap().unwrap();

    let loaded = store.load_session(cookie).await.unwrap().unwrap();
    assert_eq!(loaded.get::<String>("hello").unwrap(), "world");
    common::assert_contract(&*store).await;

    store.finish().await;
}

#[tokio::test]
#[ignore]
async fn mongodb_expiry() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.expire_in(Duration::from_secs(1));
    let cookie = store.store_session(session).await.unwrap().unwrap();

    assert!(store.load_session(cookie.clone()).await.unwrap().is_some());
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(store.load_session(cookie).await.unwrap().is_none());

    store.finish().await;
}

#[tokio::test]
#[ignore]
async fn mongodb_destroy_and_clear() {
    let store = fresh_store().await;

    for _ in 0..3 {
        store.store_session(Session::new()).await.unwrap();
    }
    let cookie = store.store_session(Session::new()).await.unwrap().unwrap();
    assert_eq!(store.count().await.unwrap(), 4);

    let loaded = store.load_session(cookie.clone()).await.unwrap().unwrap();
    store.destroy_session(loaded).await.unwrap();
    assert!(store.load_session(cookie).await.unwrap().is_none());
    assert_eq!(store.count().await.unwrap(), 3);

    store.clear_store().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
    store.finish().await;
}

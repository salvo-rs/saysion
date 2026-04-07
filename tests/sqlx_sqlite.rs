#![cfg(feature = "sqlx-sqlite")]
//! Integration tests for `SqlxSqliteStore`.
//!
//! Uses a temporary on-disk SQLite database. Run with
//! `cargo test --features sqlx-sqlite -- --ignored`.

use std::time::Duration;

use saysion::{Session, SessionStore, SqlxSqliteStore};

async fn fresh_store() -> (SqlxSqliteStore, tempfile_path::TempDb) {
    let temp = tempfile_path::TempDb::new();
    let store = SqlxSqliteStore::from_url(&temp.url())
        .await
        .expect("connect to sqlite");
    store.migrate().await.expect("migrate");
    (store, temp)
}

mod tempfile_path {
    pub struct TempDb {
        path: std::path::PathBuf,
    }
    impl TempDb {
        pub fn new() -> Self {
            let mut p = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            p.push(format!("saysion-test-{nanos}.sqlite"));
            Self { path: p }
        }
        pub fn url(&self) -> String {
            format!("sqlite://{}?mode=rwc", self.path.display())
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[tokio::test]
#[ignore]
async fn sqlite_roundtrip() {
    let (store, _g) = fresh_store().await;

    let mut session = Session::new();
    session.insert("hello", "world").unwrap();
    let cookie = store.store_session(session).await.unwrap().unwrap();

    let loaded = store.load_session(cookie).await.unwrap().unwrap();
    assert_eq!(loaded.get::<String>("hello").unwrap(), "world");
}

#[tokio::test]
#[ignore]
async fn sqlite_expiry_and_cleanup() {
    let (store, _g) = fresh_store().await;

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
async fn sqlite_destroy_and_clear() {
    let (store, _g) = fresh_store().await;

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

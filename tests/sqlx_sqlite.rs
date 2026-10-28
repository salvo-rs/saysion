#![cfg(feature = "sqlx-sqlite")]
//! Integration tests for `SqlxSqliteStore`.
//!
//! Uses a temporary on-disk SQLite database. Run with
//! `cargo test --features sqlx-sqlite -- --ignored`.

use std::time::Duration;

use saysion::{Session, SessionStore, SqlxSqliteStore};

#[tokio::test]
async fn sqlite_save_only_acknowledges_the_snapshot_written_before_io() -> saysion::Result {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    let store = SqlxSqliteStore::new(pool.clone());
    store.migrate().await?;

    let mut session = Session::new();
    session.insert("value", 1)?;
    let mut editor = session.clone();
    // Occupy the only connection, then poll the save until it waits for I/O.
    // No sleeps or scheduler timing assumptions are needed.
    let blocker = pool.acquire().await?;
    let mut save = std::pin::pin!(store.store_session(session));
    poll_fn(|cx| match save.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!("save completed while the pool was blocked: {result:?}"),
    })
    .await;
    editor.insert("value", 2)?;
    drop(blocker);
    let cookie = save.await?.unwrap();

    assert!(editor.data_changed(), "the later edit was not persisted");
    assert_eq!(
        store
            .load_session(cookie.clone())
            .await?
            .unwrap()
            .get::<u64>("value"),
        Some(1)
    );
    store.store_session(editor.clone()).await?;
    assert!(!editor.data_changed());
    assert_eq!(
        store
            .load_session(cookie)
            .await?
            .unwrap()
            .get::<u64>("value"),
        Some(2)
    );
    Ok(())
}

#[tokio::test]
async fn sqlite_failed_save_keeps_changes_pending() -> saysion::Result {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
    let store = SqlxSqliteStore::new(pool);
    let mut session = Session::new();
    session.insert("value", 1)?;
    let observer = session.clone();
    // No migration: the write must fail because the table does not exist.
    assert!(store.store_session(session).await.is_err());
    assert!(observer.data_changed());
    Ok(())
}

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

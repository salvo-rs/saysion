#![cfg(feature = "sqlx-sqlite")]

use std::{
    future::{Future, poll_fn},
    task::Poll,
};

use saysion::{Session, SessionConflict, SessionStore, SqlxSqliteStore};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::test]
async fn sqlite_first_save_and_same_lineage_deletion_have_a_consistent_outcome() -> saysion::Result
{
    let filename = std::env::temp_dir().join(format!(
        "saysion-first-save-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let options = SqliteConnectOptions::new()
        .filename(&filename)
        .create_if_missing(true);
    let pool_a = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await?;
    let pool_b = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let store_a = SqlxSqliteStore::new(pool_a.clone());
    let store_b = SqlxSqliteStore::new(pool_b.clone());
    store_a.migrate().await?;
    let mut session = Session::new();
    session.insert("value", 42_u64)?;
    let logout = session.clone();
    let observer = session.clone();
    let blocker = pool_a.acquire().await?;
    let mut save = Box::pin(store_a.store_session(session));
    poll_fn(|cx| match save.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("first save should wait for the only connection"),
    })
    .await;
    let mut deletion = Box::pin(store_b.destroy_session(logout));
    poll_fn(|cx| match deletion.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("deletion should wait for the pending first save"),
    })
    .await;
    assert!(observer.is_destroyed());
    drop(blocker);
    let (saved, deleted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(save, deletion)
    })
    .await?;
    assert!(
        saved
            .unwrap_err()
            .downcast_ref::<SessionConflict>()
            .is_some()
    );
    deleted?;
    assert_eq!(store_a.count().await?, 0);
    assert!(observer.data_changed());
    pool_a.close().await;
    pool_b.close().await;
    std::fs::remove_file(filename)?;
    Ok(())
}

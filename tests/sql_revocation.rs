#![cfg(feature = "sqlx-sqlite")]
//! A revoked SQL session cannot be recreated by a late save.

use saysion::{Session, SessionConflict, SessionStore, SqlxSqliteStore};

async fn fresh_store() -> saysion::Result<(SqlxSqliteStore, sqlx::SqlitePool)> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    let store = SqlxSqliteStore::new(pool.clone());
    store.migrate().await?;
    Ok((store, pool))
}

#[tokio::test]
async fn independently_loaded_session_cannot_resurrect_a_destroyed_record() -> saysion::Result {
    let (store, _) = fresh_store().await?;
    let cookie = store.store_session(Session::new()).await?.unwrap();
    let logout = store.load_session(cookie.clone()).await?.unwrap();
    let mut stale = store.load_session(cookie.clone()).await?.unwrap();
    stale.insert("late_update", 1)?;
    let observer = stale.clone();
    store.destroy_session(logout).await?;

    let error = store.store_session(stale).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(
        observer.data_changed(),
        "a rejected write must not acknowledge changes"
    );
    assert!(store.load_session(cookie).await?.is_none());
    assert_eq!(store.count().await?, 0);
    Ok(())
}

#[tokio::test]
async fn a_stale_save_cannot_recreate_a_cleared_record() -> saysion::Result {
    let (store, _) = fresh_store().await?;
    let cookie = store.store_session(Session::new()).await?.unwrap();
    let stale = store.load_session(cookie.clone()).await?.unwrap();
    store.clear_store().await?;

    let error = store.store_session(stale).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(store.load_session(cookie).await?.is_none());
    assert_eq!(store.count().await?, 0);
    Ok(())
}

#[tokio::test]
async fn an_original_new_session_clone_cannot_reinsert_after_first_save_and_delete()
-> saysion::Result {
    let (store, _) = fresh_store().await?;
    let session = Session::new();
    let stale_clone = session.clone();
    let cookie = store.store_session(session).await?.unwrap();
    let loaded = store.load_session(cookie.clone()).await?.unwrap();
    store.destroy_session(loaded).await?;

    let error = store.store_session(stale_clone).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(store.load_session(cookie).await?.is_none());
    assert_eq!(store.count().await?, 0);
    Ok(())
}

#[tokio::test]
async fn metadata_only_and_unchanged_updates_succeed_for_existing_records() -> saysion::Result {
    let (store, _) = fresh_store().await?;
    let cookie = store.store_session(Session::new()).await?.unwrap();
    let mut loaded = store.load_session(cookie.clone()).await?.unwrap();
    loaded.expire_in(std::time::Duration::from_secs(60));
    let expiry = *loaded.expiry().unwrap();
    assert!(loaded.data_changed());
    assert!(store.store_session(loaded).await?.is_none());

    let unchanged = store.load_session(cookie.clone()).await?.unwrap();
    assert!(!unchanged.data_changed());
    assert_eq!(unchanged.expiry(), Some(&expiry));
    assert!(store.store_session(unchanged).await?.is_none());
    assert!(store.load_session(cookie).await?.is_some());
    assert_eq!(store.count().await?, 1);
    Ok(())
}

#[tokio::test]
async fn a_first_insert_collision_returns_conflict_without_overwriting() -> saysion::Result {
    let (store, pool) = fresh_store().await?;
    let mut session = Session::new();
    session.insert("value", "existing")?;
    let existing_json = serde_json::to_string(&session)?;
    sqlx::query("INSERT INTO saysion_sessions (id, expires, session) VALUES (?, NULL, ?)")
        .bind(session.id())
        .bind(&existing_json)
        .execute(&pool)
        .await?;
    session.insert("value", "replacement")?;
    let retry = session.clone();

    let error = store.store_session(session).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    let error = store.store_session(retry).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    let (stored,): (String,) = sqlx::query_as("SELECT session FROM saysion_sessions")
        .fetch_one(&pool)
        .await?;
    assert_eq!(stored, existing_json);
    assert_eq!(store.count().await?, 1);
    Ok(())
}

#[tokio::test]
async fn cancellation_never_restores_first_insert_authority() -> saysion::Result {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    let (store, pool) = fresh_store().await?;
    let session = Session::new();
    let retry = session.clone();
    let blocker = pool.acquire().await?;
    let mut save = Box::pin(store.store_session(session));
    poll_fn(|cx| match save.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!("the occupied pool must block the write: {result:?}"),
    })
    .await;
    drop(save);
    drop(blocker);
    let error = store.store_session(retry).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert_eq!(store.count().await?, 0);
    Ok(())
}

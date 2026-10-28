#![cfg(feature = "sqlx-sqlite")]
//! Atomic SQL session identity rotation.

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
async fn rotation_replaces_the_old_cookie_and_preserves_data() -> saysion::Result {
    let (store, _) = fresh_store().await?;
    let mut session = Session::new();
    session.insert("user_id", 42)?;
    let old_cookie = store.store_session(session).await?.unwrap();
    let mut session = store.load_session(old_cookie.clone()).await?.unwrap();
    session.regenerate();
    let new_cookie = store.store_session(session).await?.unwrap();

    assert!(store.load_session(old_cookie).await?.is_none());
    let loaded = store.load_session(new_cookie).await?.unwrap();
    assert_eq!(loaded.get::<u32>("user_id"), Some(42));
    assert_eq!(store.count().await?, 1);
    Ok(())
}

#[tokio::test]
async fn repeated_regeneration_replaces_the_original_persisted_id() -> saysion::Result {
    let (store, pool) = fresh_store().await?;
    let old_cookie = store.store_session(Session::new()).await?.unwrap();
    let mut session = store.load_session(old_cookie.clone()).await?.unwrap();
    session.regenerate();
    let intermediate_id = session.id().to_owned();
    session.regenerate();
    let new_cookie = store.store_session(session).await?.unwrap();

    assert!(store.load_session(old_cookie).await?.is_none());
    assert!(store.load_session(new_cookie).await?.is_some());
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM saysion_sessions WHERE id = ?")
        .bind(intermediate_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    assert_eq!(store.count().await?, 1);
    Ok(())
}

#[tokio::test]
async fn saving_a_rotated_clone_advances_the_shared_persistence_origin() -> saysion::Result {
    let (store, _) = fresh_store().await?;
    let old_cookie = store.store_session(Session::new()).await?.unwrap();
    let mut session = store.load_session(old_cookie.clone()).await?.unwrap();
    session.regenerate();
    session.insert("value", 1)?;
    // Clone deliberately does not carry the cookie, but does share the record's
    // persistence lineage. The second save must not try to delete old_id again.
    assert!(store.store_session(session.clone()).await?.is_none());
    session.insert("value", 2)?;
    let new_cookie = store.store_session(session).await?.unwrap();

    assert!(store.load_session(old_cookie).await?.is_none());
    let loaded = store.load_session(new_cookie).await?.unwrap();
    assert_eq!(loaded.get::<u32>("value"), Some(2));
    assert_eq!(store.count().await?, 1);
    Ok(())
}

#[tokio::test]
async fn rotation_of_a_missing_origin_returns_a_conflict() -> saysion::Result {
    let (store, _) = fresh_store().await?;
    let old_cookie = store.store_session(Session::new()).await?.unwrap();
    let mut stale = store.load_session(old_cookie.clone()).await?.unwrap();
    let current = store.load_session(old_cookie.clone()).await?.unwrap();
    store.destroy_session(current).await?;
    stale.regenerate();

    let error = store.store_session(stale).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(store.load_session(old_cookie).await?.is_none());
    assert_eq!(store.count().await?, 0);
    Ok(())
}

#[tokio::test]
async fn a_failed_replacement_write_rolls_back_the_old_id_deletion() -> saysion::Result {
    let (store, pool) = fresh_store().await?;
    let old_cookie = store.store_session(Session::new()).await?.unwrap();
    let mut session = store.load_session(old_cookie.clone()).await?.unwrap();
    session.regenerate();
    sqlx::query(
        "CREATE TRIGGER reject_replacement BEFORE INSERT ON saysion_sessions \
         BEGIN SELECT RAISE(FAIL, 'injected replacement failure'); END",
    )
    .execute(&pool)
    .await?;

    assert!(store.store_session(session).await.is_err());
    assert!(store.load_session(old_cookie).await?.is_some());
    assert_eq!(store.count().await?, 1);
    Ok(())
}

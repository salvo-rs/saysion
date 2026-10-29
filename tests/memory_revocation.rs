use saysion::{MemoryStore, Session, SessionConflict, SessionStore};

#[tokio::test]
async fn an_independent_request_cannot_restore_a_logged_out_session() -> saysion::Result {
    let store = MemoryStore::new();
    let mut session = Session::new();
    session.insert("user_id", 42)?;
    let cookie = store.store_session(session).await?.unwrap();
    let mut late = store.load_session(cookie.clone()).await?.unwrap();
    let logout = store.load_session(cookie.clone()).await?.unwrap();
    store.destroy_session(logout).await?;
    late.insert("late", true)?;
    let error = store.store_session(late.clone()).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(late.data_changed());
    assert!(store.load_session(cookie).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn a_cleared_store_rejects_loaded_sessions_and_original_new_clones() -> saysion::Result {
    let store = MemoryStore::new();
    let original = Session::new();
    let cookie = store.store_session(original.clone()).await?;
    assert!(cookie.is_none());
    // The original owns the cookie, and shares knowledge of the successful save.
    let token = original.clone();
    store.clear_store().await?;
    let error = store.store_session(token).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert_eq!(store.count().await, 0);
    Ok(())
}

#[tokio::test]
async fn destroying_an_unsaved_rotation_revokes_its_persisted_source() -> saysion::Result {
    let store = MemoryStore::new();
    let cookie = store.store_session(Session::new()).await?.unwrap();
    let mut loaded = store.load_session(cookie.clone()).await?.unwrap();
    loaded.regenerate();
    store.destroy_session(loaded).await?;
    assert!(store.load_session(cookie).await?.is_none());
    Ok(())
}

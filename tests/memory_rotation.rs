use saysion::{MemoryStore, Session, SessionConflict, SessionStore};

#[tokio::test]
async fn an_independently_loaded_request_cannot_restore_the_rotated_identity() -> saysion::Result {
    let store = MemoryStore::new();
    let old_cookie = store.store_session(Session::new()).await?.unwrap();
    let mut stale = store.load_session(old_cookie.clone()).await?.unwrap();
    let mut rotating = store.load_session(old_cookie.clone()).await?.unwrap();
    rotating.regenerate();
    rotating.insert("user_id", 42)?;
    let new_cookie = store.store_session(rotating).await?.unwrap();

    stale.insert("late", true)?;
    let error = store.store_session(stale.clone()).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(stale.data_changed());
    assert!(store.load_session(old_cookie).await?.is_none());
    assert_eq!(
        store
            .load_session(new_cookie)
            .await?
            .unwrap()
            .get::<u64>("user_id"),
        Some(42)
    );
    assert_eq!(store.count().await, 1);
    Ok(())
}

#[tokio::test]
async fn destroying_an_unsaved_rotation_targets_its_persisted_source() -> saysion::Result {
    let store = MemoryStore::new();
    let old_cookie = store.store_session(Session::new()).await?.unwrap();
    let mut rotating = store.load_session(old_cookie.clone()).await?.unwrap();
    rotating.regenerate();
    rotating.destroy();
    store.destroy_session(rotating).await?;
    assert!(store.load_session(old_cookie).await?.is_none());
    assert_eq!(store.count().await, 0);
    Ok(())
}

#[tokio::test]
async fn login_rotation_revokes_old_cookie_and_keeps_new_identity() -> saysion::Result {
    let store = MemoryStore::new();
    let old_cookie = store.store_session(Session::new()).await?.unwrap();
    let mut login = store.load_session(old_cookie.clone()).await?.unwrap();
    login.regenerate();
    login.regenerate();
    login.insert("user_id", 42)?;
    let new_cookie = store.store_session(login).await?.unwrap();
    assert_ne!(old_cookie, new_cookie);
    assert!(store.load_session(old_cookie).await?.is_none());
    assert_eq!(
        store
            .load_session(new_cookie)
            .await?
            .unwrap()
            .get::<i32>("user_id"),
        Some(42)
    );
    assert_eq!(store.count().await, 1);
    Ok(())
}

#[tokio::test]
async fn clones_can_save_rotated_identity_but_cannot_rotate_back() -> saysion::Result {
    let store = MemoryStore::new();
    let old_cookie = store.store_session(Session::new()).await?.unwrap();
    let mut rotating = store.load_session(old_cookie.clone()).await?.unwrap();
    let stale_clone = rotating.clone();
    rotating.regenerate();
    assert!(store.store_session(rotating.clone()).await?.is_none());
    let new_cookie = store.store_session(rotating).await?.unwrap();
    let error = store.store_session(stale_clone).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(store.load_session(old_cookie).await?.is_none());
    assert!(store.load_session(new_cookie).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn deserialized_sessions_do_not_have_creation_authority() -> saysion::Result {
    let store = MemoryStore::new();
    let session: Session = serde_json::from_str(&serde_json::to_string(&Session::new())?)?;
    let error = store.store_session(session).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    Ok(())
}

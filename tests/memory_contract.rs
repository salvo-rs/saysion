use saysion::{MemoryStore, Session, SessionStore};

#[tokio::test]
async fn loaded_changes_require_explicit_save() -> saysion::Result {
    let store = MemoryStore::new();
    let mut initial = Session::new();
    initial.insert("value", 1)?;
    let cookie = store.store_session(initial).await?.unwrap();
    let mut request = store.load_session(cookie.clone()).await?.unwrap();
    let other_request = store.load_session(cookie.clone()).await?.unwrap();
    request.insert("value", 2)?;
    assert_eq!(other_request.get::<i32>("value"), Some(1));
    assert_eq!(
        store
            .load_session(cookie.clone())
            .await?
            .unwrap()
            .get::<i32>("value"),
        Some(1)
    );
    store.store_session(request).await?;
    assert_eq!(
        store
            .load_session(cookie)
            .await?
            .unwrap()
            .get::<i32>("value"),
        Some(2)
    );
    Ok(())
}

#[tokio::test]
async fn saving_one_request_does_not_reset_another_requests_changes() -> saysion::Result {
    let store = MemoryStore::new();
    let cookie = store.store_session(Session::new()).await?.unwrap();
    let mut first = store.load_session(cookie.clone()).await?.unwrap();
    let mut second = store.load_session(cookie).await?.unwrap();
    first.insert("first", true)?;
    second.insert("second", true)?;
    store.store_session(first).await?;
    assert!(second.data_changed());
    Ok(())
}

#[tokio::test]
async fn request_local_clones_still_share_data() -> saysion::Result {
    let mut request = Session::new();
    let cloned = request.clone();
    request.insert("shared", true)?;
    assert_eq!(cloned.get::<bool>("shared"), Some(true));
    Ok(())
}

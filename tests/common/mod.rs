use saysion::{Session, SessionStore};

/// A syntactically valid but misbound blob must never grant another identity.
pub async fn assert_identity_binding<F, Fut>(store: &(impl SessionStore + Sync), misbind: F)
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let first_cookie = store.store_session(Session::new()).await.unwrap().unwrap();
    let first_id = Session::id_from_cookie_value(&first_cookie).unwrap();
    let mut other = Session::new();
    other.insert("binding_owner", "other").unwrap();
    let other_cookie = store.store_session(other).await.unwrap().unwrap();
    let other = store
        .load_session(other_cookie.clone())
        .await
        .unwrap()
        .unwrap();
    let other_blob = serde_json::to_string(&other).unwrap();

    misbind(first_id, other_blob).await;

    assert!(store.load_session(first_cookie).await.is_err());
    let other = store.load_session(other_cookie).await.unwrap().unwrap();
    assert_eq!(
        other.get::<String>("binding_owner").as_deref(),
        Some("other")
    );
    assert!(!other.needs_persist());
}

/// Backend-independent persistence guarantees exercised against every database.
pub async fn assert_contract(store: &(impl SessionStore + Sync)) {
    assert!(store.load_session("invalid!cookie".into()).await.is_err());
    let missing = Session::new().into_cookie_value().unwrap();
    assert!(store.load_session(missing).await.unwrap().is_none());

    let mut session = Session::new();
    session.insert("contract_value", 1).unwrap();
    let cookie = store.store_session(session).await.unwrap().unwrap();
    let mut loaded = store.load_session(cookie.clone()).await.unwrap().unwrap();
    assert!(!loaded.data_changed());
    loaded.insert("contract_value", 2).unwrap();
    assert_eq!(
        store
            .load_session(cookie.clone())
            .await
            .unwrap()
            .unwrap()
            .get::<u32>("contract_value"),
        Some(1),
        "unsaved mutations must stay local to the loaded session",
    );
    assert!(store.store_session(loaded).await.unwrap().is_none());
    let loaded = store.load_session(cookie.clone()).await.unwrap().unwrap();
    assert_eq!(loaded.get::<u32>("contract_value"), Some(2));
    assert!(!loaded.data_changed());
    store.destroy_session(loaded).await.unwrap();
    assert!(store.load_session(cookie).await.unwrap().is_none());
}

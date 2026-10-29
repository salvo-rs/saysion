use saysion::{Session, SessionStore};

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

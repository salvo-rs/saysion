use saysion::{CookieStore, Session, SessionStore, SignedCookieStore};

#[tokio::test]
async fn unsigned_cookie_store_acknowledges_a_successfully_saved_snapshot() -> saysion::Result {
    let mut session = Session::new();
    session.insert("value", 42_u32)?;
    let observer = session.clone();
    assert!(observer.data_changed());
    let store = CookieStore::new();
    let cookie = store.store_session(session).await?.unwrap();
    assert_eq!(
        store
            .load_session(cookie)
            .await?
            .unwrap()
            .get::<u32>("value"),
        Some(42)
    );
    assert!(!observer.data_changed());
    Ok(())
}

#[tokio::test]
async fn signed_cookie_store_should_acknowledge_a_successfully_saved_snapshot() -> saysion::Result {
    let mut session = Session::new();
    session.insert("value", 42_u32)?;
    let observer = session.clone();
    assert!(observer.data_changed());
    let store = SignedCookieStore::new([42; 32])?;
    let cookie = store.store_session(session).await?.unwrap();
    assert_eq!(
        store
            .load_session(cookie)
            .await?
            .unwrap()
            .get::<u32>("value"),
        Some(42)
    );
    assert!(
        !observer.data_changed(),
        "successful signed-cookie save left the shared revision dirty"
    );
    Ok(())
}

#[tokio::test]
async fn failed_signed_cookie_save_keeps_the_change_pending() -> saysion::Result {
    let mut session = Session::new();
    session.insert_raw("large", "A".repeat(4_000));
    let observer = session.clone();
    assert!(
        SignedCookieStore::new([42; 32])?
            .store_session(session)
            .await
            .is_err()
    );
    assert!(observer.data_changed());
    Ok(())
}

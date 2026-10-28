use saysion::{MemoryStore, Session, SessionStore};

#[test]
fn metadata_only_edits_require_persistence() {
    let mut session = Session::new();
    assert!(!session.data_changed());
    assert!(!session.needs_persist());

    session.regenerate();
    assert!(session.data_changed());
    assert!(session.needs_persist());
    session.reset_data_changed();
    assert!(!session.data_changed());

    let expiry = time::OffsetDateTime::now_utc() + time::Duration::HOUR;
    session.set_expiry(expiry);
    assert!(session.data_changed());
    session.reset_data_changed();
    session.set_expiry(expiry);
    assert!(
        !session.data_changed(),
        "unchanged expiry is not a mutation"
    );

    session.expire_in(std::time::Duration::from_secs(60));
    assert!(session.needs_persist());
    session.reset_data_changed();
    session.set_cookie_value("replacement-cookie".to_string());
    assert!(session.needs_persist());
    session.reset_data_changed();
    session.set_cookie_value("replacement-cookie".to_string());
    assert!(!session.data_changed());

    session.destroy();
    assert!(
        session.needs_persist(),
        "destruction still requires handling"
    );
}

#[test]
fn resetting_one_clone_does_not_acknowledge_another_clones_metadata() {
    let mut first = Session::new();
    let mut second = first.clone();
    let expiry = time::OffsetDateTime::now_utc() + time::Duration::HOUR;

    first.set_expiry(expiry);
    assert!(!second.data_changed());
    assert_eq!(second.expiry(), None);
    second.regenerate();
    second.reset_data_changed();
    assert!(first.data_changed());
    assert!(!second.data_changed());

    second.set_cookie_value("other-cookie".to_string());
    first.reset_data_changed();
    assert!(!first.data_changed());
    assert!(second.data_changed());
}

#[tokio::test]
async fn metadata_only_expiry_changes_are_saved() -> saysion::Result {
    let store = MemoryStore::new();
    let cookie = store.store_session(Session::new()).await?.unwrap();
    let mut session = store.load_session(cookie.clone()).await?.unwrap();
    let expiry = time::OffsetDateTime::now_utc() + time::Duration::HOUR;
    session.set_expiry(expiry);
    assert!(session.needs_persist());
    store.store_session(session).await?;
    let loaded = store.load_session(cookie).await?.unwrap();
    assert_eq!(loaded.expiry(), Some(&expiry));
    assert!(!loaded.data_changed());
    Ok(())
}

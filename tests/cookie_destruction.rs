use saysion::{CookieStore, Session, SessionConflict, SessionStore, SignedCookieStore};

async fn refuses_destroyed_input(store: impl SessionStore) -> saysion::Result {
    let mut session = Session::new();
    session.insert("user_id", 42_u32)?;
    let observer = session.clone();
    let pending_save = session.clone();
    session.destroy();
    let error = store.store_session(pending_save).await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(observer.data_changed());
    assert!(observer.is_destroyed());
    Ok(())
}

#[tokio::test]
async fn unsigned_store_does_not_issue_a_cookie_from_destroyed_lineage() -> saysion::Result {
    refuses_destroyed_input(CookieStore::new()).await
}

#[tokio::test]
async fn signed_store_does_not_issue_a_cookie_from_destroyed_lineage() -> saysion::Result {
    refuses_destroyed_input(SignedCookieStore::new([42; 32])?).await
}

#[tokio::test]
async fn a_previously_issued_cookie_keeps_its_documented_replay_semantics() -> saysion::Result {
    let store = SignedCookieStore::new([42; 32])?;
    let mut session = Session::new();
    session.insert("user_id", 42_u32)?;
    let mut logout = session.clone();
    let cookie = store.store_session(session).await?.unwrap();
    logout.destroy();
    assert!(store.load_session(cookie).await?.is_some());
    Ok(())
}

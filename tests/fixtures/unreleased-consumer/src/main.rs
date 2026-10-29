use saysion::{Session, SessionConflict, SessionStore, SignedCookieStore};

fn main() -> saysion::Result {
    let store = SignedCookieStore::new([42; 32])?;
    let mut session = Session::new();
    session.insert("user_id", 42_u64)?;
    assert!(session.needs_persist());
    let _save = store.store_session(session);
    let _conflict = SessionConflict;
    Ok(())
}

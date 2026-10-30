use saysion::{
    Session, SessionConflict, SessionSave, SessionSnapshot, SessionStore, SignedCookieStore,
};

fn main() -> saysion::Result {
    let store = SignedCookieStore::new([42; 32])?;
    let mut session = Session::new();
    session.insert("user_id", 42_u64)?;
    assert!(session.needs_persist());
    let _save = store.store_session(session);
    let _conflict = SessionConflict;

    let pending: SessionSave = Session::new().prepare_save()?;
    pending.validate_current()?;
    let snapshot: &SessionSnapshot = pending.session();
    let cloned_snapshot: SessionSnapshot = snapshot.clone();
    assert_eq!(cloned_snapshot.id(), snapshot.id());
    let _captured_value: Option<&str> = cloned_snapshot.get_raw("user_id");
    let _captured_expiry = cloned_snapshot.expiry();
    assert!(pending.origin().is_none());
    // No backend write is performed here, so do not complete this token.
    // Type-check completion, trusted loading, and source-aware deletion too.
    let _complete: fn(SessionSave) -> Option<String> = SessionSave::complete;
    let _load: fn(Session, &str) -> saysion::Result<Option<Session>> = Session::validate_loaded;
    let _delete: fn(&mut Session) -> String = Session::prepare_delete;
    Ok(())
}

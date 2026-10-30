use saysion::{
    Session, SessionConflict, SessionDeleteGuard, SessionSave, SessionSnapshot, SessionStore,
    SessionWriteGuard, SignedCookieStore,
};

// Type-check the asynchronous coordination API without driving any future or
// pretending to perform a backend write/deletion in this consumer fixture.
async fn typecheck_guarded_write(save: SessionSave) -> saysion::Result {
    let write: SessionWriteGuard = save.begin_write().await?;
    let _destroyed = write.is_destroyed();
    let _complete: fn(SessionSave, SessionWriteGuard) -> saysion::Result<Option<String>> =
        SessionSave::complete_with_guard;
    let _removed: fn(SessionSave, SessionWriteGuard) -> saysion::Result =
        SessionSave::confirm_removed;
    drop(write);
    drop(save);
    Ok(())
}

async fn typecheck_guarded_delete(mut session: Session) -> saysion::Result {
    let deletion: SessionDeleteGuard = session.begin_delete().await?;
    let _target: &str = deletion.id();
    Ok(())
}

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
    let _guarded_protocol = (typecheck_guarded_write, typecheck_guarded_delete);
    Ok(())
}

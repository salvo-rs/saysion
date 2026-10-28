use saysion::{MemoryStore, Session, SessionStore};

#[test]
fn session_debug_redacts_tokens_identifiers_and_data() -> saysion::Result {
    let mut session = Session::new();
    session.insert("private-session-key", "confidential-profile-value")?;
    let id = session.id().to_owned();
    // Supply a known token without consuming the session to inspect both
    // ordinary and alternate Debug formatting.
    session.set_cookie_value("secret-cookie-token".to_string());
    for output in [format!("{session:?}"), format!("{session:#?}")] {
        assert!(output.contains("Session"));
        assert!(output.contains("expiry"));
        assert!(!output.contains("data_changed"));
        assert!(!output.contains("secret-cookie-token"));
        assert!(!output.contains(&id));
        assert!(!output.contains("private-session-key"));
        assert!(!output.contains("confidential-profile-value"));
    }
    Ok(())
}

#[tokio::test]
async fn memory_store_debug_never_traverses_stored_sessions() -> saysion::Result {
    let store = MemoryStore::new();
    let mut session = Session::new();
    session.insert("private-session-key", "confidential-profile-value")?;
    let id = session.id().to_owned();
    let cookie = store.store_session(session).await?.unwrap();

    for output in [format!("{store:?}"), format!("{store:#?}")] {
        assert!(output.contains("MemoryStore"));
        assert!(!output.contains(&id));
        assert!(!output.contains(&cookie));
        assert!(!output.contains("private-session-key"));
        assert!(!output.contains("confidential-profile-value"));
    }
    assert!(store.load_session(cookie).await?.is_some());
    Ok(())
}

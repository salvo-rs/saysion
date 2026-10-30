use saysion::base64::{Engine as _, engine::general_purpose::STANDARD};
use saysion::{CookieStore, MemoryStore, Session, SessionStore, SignedCookieStore};

#[test]
fn standard_server_cookies_keep_the_existing_hash_mapping() {
    let bytes = [0x7b; 64];
    let cookie = STANDARD.encode(bytes);
    let expected = STANDARD.encode(saysion::blake3::hash(&bytes).as_bytes());
    assert_eq!(Session::id_from_cookie_value(&cookie).unwrap(), expected);

    for _ in 0..32 {
        let mut session = Session::new();
        let original_id = session.id().to_owned();
        session.regenerate();
        let id = session.id().to_owned();
        assert_ne!(id, original_id);
        let cookie = session.into_cookie_value().unwrap();
        assert_eq!(cookie.len(), 88);
        assert_eq!(STANDARD.decode(&cookie).unwrap().len(), 64);
        assert_eq!(Session::id_from_cookie_value(&cookie).unwrap(), id);
    }
}

#[test]
fn nonstandard_lengths_and_oversized_values_are_rejected_before_decoding() {
    for bytes in [0, 1, 32, 63, 65, 66, 67, 128] {
        let cookie = STANDARD.encode(vec![0; bytes]);
        assert!(Session::id_from_cookie_value(&cookie).is_err());
    }
    for cookie in ["A".repeat(8 << 20), "!".repeat(8 << 20)] {
        assert_eq!(
            Session::id_from_cookie_value(&cookie),
            Err(saysion::base64::DecodeError::InvalidLength(cookie.len()))
        );
    }
}

#[test]
fn canonical_base64_validation_is_retained_for_fixed_size_tokens() {
    let mut invalid = STANDARD.encode([0; 64]);
    invalid.replace_range(..1, "!");
    assert!(matches!(
        Session::id_from_cookie_value(&invalid),
        Err(saysion::base64::DecodeError::InvalidByte(0, b'!'))
    ));
    let mut trailing_bits = STANDARD.encode([0; 64]);
    trailing_bits.replace_range(85..86, "B");
    assert!(matches!(
        Session::id_from_cookie_value(&trailing_bits),
        Err(saysion::base64::DecodeError::InvalidLastSymbol(85, b'B'))
    ));
}

#[tokio::test]
async fn rejected_server_cookie_does_not_affect_existing_sessions() -> saysion::Result {
    let store = MemoryStore::new();
    let cookie = store.store_session(Session::new()).await?.unwrap();
    assert!(store.load_session("AA==".to_owned()).await.is_err());
    assert!(store.load_session("A".repeat(1 << 20)).await.is_err());
    assert!(store.load_session(cookie).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn variable_length_client_cookie_formats_remain_independent() -> saysion::Result {
    let unsigned = CookieStore::new();
    let mut session = Session::new();
    session.insert("payload", "x".repeat(1_200))?;
    let cookie = unsigned.store_session(session).await?.unwrap();
    assert!(cookie.len() > 88);
    let loaded = unsigned.load_session(cookie).await?.unwrap();
    assert_eq!(loaded.get::<String>("payload").unwrap().len(), 1_200);

    let signed = SignedCookieStore::new([42; 32])?;
    let mut session = Session::new();
    session.insert("payload", "x".repeat(1_200))?;
    let cookie = signed.store_session(session).await?.unwrap();
    assert!(cookie.len() > 88);
    let loaded = signed.load_session(cookie).await?.unwrap();
    assert_eq!(loaded.get::<String>("payload").unwrap().len(), 1_200);
    Ok(())
}

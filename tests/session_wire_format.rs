use std::collections::HashMap;

use saysion::Session;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct LegacySession {
    id: String,
    expiry: Option<time::OffsetDateTime>,
    data: HashMap<String, String>,
}

#[test]
fn revision_bookkeeping_preserves_json_and_postcard_wire_formats() -> saysion::Result {
    let mut session = Session::new();
    session.insert("value", 42_u64)?;
    assert!(session.data_changed());

    let json = serde_json::to_string(&session)?;
    let legacy: LegacySession = serde_json::from_str(&json)?;
    assert_eq!(legacy.data.get("value").map(String::as_str), Some("42"));
    let loaded: Session = serde_json::from_str(&serde_json::to_string(&legacy)?)?;
    assert!(!loaded.data_changed());
    assert_eq!(loaded.get::<u64>("value"), Some(42));

    let bytes = postcard::to_stdvec(&session)?;
    let legacy: LegacySession = postcard::from_bytes(&bytes)?;
    let loaded: Session = postcard::from_bytes(&postcard::to_stdvec(&legacy)?)?;
    assert_eq!(loaded.id(), session.id());
    assert_eq!(loaded.get::<u64>("value"), Some(42));
    assert!(!loaded.data_changed());
    Ok(())
}

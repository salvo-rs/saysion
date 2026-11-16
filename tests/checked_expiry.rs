use saysion::Session;
use std::time::Duration;

#[test]
fn excessive_ttls_preserve_expiry_and_change_tracking() -> saysion::Result {
    let mut session = Session::new();
    session.expire_in(Duration::from_secs(60));
    session.reset_data_changed();
    let expiry = session.expiry().copied();
    for ttl in [Duration::MAX, Duration::from_secs(i64::MAX as u64)] {
        assert!(session.try_expire_in(ttl).is_err());
        assert_eq!(session.expiry().copied(), expiry);
        assert!(!session.needs_persist());
    }
    Ok(())
}

#[test]
fn checked_expiry_sets_a_finite_deadline_and_marks_metadata() -> saysion::Result {
    let mut session = Session::new();
    session.try_expire_in(Duration::from_secs(60))?;
    assert!(session.needs_persist());
    assert!(
        session
            .expires_in()
            .is_some_and(|ttl| ttl <= Duration::from_secs(60))
    );
    Ok(())
}

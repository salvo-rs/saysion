use saysion::{Result, Session, SessionConflict, SessionSave};
use std::collections::HashMap;

fn write(records: &mut HashMap<String, String>, save: &SessionSave) -> Result {
    // Exclusive access models the backend write lock. Completion can happen
    // afterward, in a different order from the confirmed writes.
    save.validate_current()?;
    let id = save.session().id().to_owned();
    if let Some(origin) = save.origin()
        && !records.contains_key(origin)
    {
        return Err(SessionConflict.into());
    }
    if save.origin() != Some(id.as_str()) && records.contains_key(&id) {
        return Err(SessionConflict.into());
    }
    let encoded = serde_json::to_string(save.session())?;
    if let Some(origin) = save.origin().filter(|origin| *origin != id) {
        records.remove(origin);
    }
    records.insert(id, encoded);
    Ok(())
}

fn persisted() -> Result<(Session, HashMap<String, String>)> {
    let mut source = Session::new();
    source.insert("value", 1_u32)?;
    let mut records = HashMap::new();
    let initial = source.clone().prepare_save()?;
    write(&mut records, &initial)?;
    initial.complete();
    assert!(!source.data_changed());
    Ok((source, records))
}

#[test]
fn reversed_writes_and_completions_stay_dirty_then_a_standalone_save_recovers() -> Result {
    for complete_new_first in [false, true] {
        let (mut source, mut records) = persisted()?;
        let old = source.clone().prepare_save()?;
        source.insert("value", 2_u32)?;
        let new = source.clone().prepare_save()?;
        let snapshot_copy = new.session().clone();
        write(&mut records, &new)?;
        write(&mut records, &old)?;
        if complete_new_first {
            new.complete();
            old.complete();
        } else {
            old.complete();
            new.complete();
        }
        let stored: Session = serde_json::from_str(&records[source.id()])?;
        assert_eq!(stored.get::<u32>("value"), Some(1));
        assert_eq!(source.get::<u32>("value"), Some(2));
        assert!(
            source.data_changed(),
            "overlap cannot establish the last write"
        );
        let repair = source.clone().prepare_save()?;
        write(&mut records, &repair)?;
        repair.complete();
        assert!(!source.data_changed());
        let stored: Session = serde_json::from_str(&records[source.id()])?;
        assert_eq!(stored.get::<u32>("value"), Some(2));
        assert_eq!(snapshot_copy.get_raw("value"), Some("2"));
    }
    Ok(())
}

#[test]
fn an_unconfirmed_write_keeps_dirty_and_releases_the_active_slot() -> Result {
    let (source, mut records) = persisted()?;
    let pending = source.clone().prepare_save()?;
    let snapshot_copy = pending.session().clone();
    write(&mut records, &pending)?;
    drop(pending); // Write committed, but its success callback was cancelled.
    assert!(source.data_changed());
    let repair = source.clone().prepare_save()?;
    write(&mut records, &repair)?;
    repair.complete();
    assert!(!source.data_changed());
    assert_eq!(snapshot_copy.id(), source.id());
    Ok(())
}

#[test]
fn dropping_either_overlapping_handle_keeps_the_other_completion_dirty() -> Result {
    for drop_new in [false, true] {
        let (mut source, mut records) = persisted()?;
        let old = source.clone().prepare_save()?;
        source.insert("value", 2_u32)?;
        let new = source.clone().prepare_save()?;
        if drop_new {
            drop(new);
            write(&mut records, &old)?;
            old.complete();
        } else {
            drop(old);
            write(&mut records, &new)?;
            new.complete();
        }
        assert!(source.data_changed());
        let repair = source.clone().prepare_save()?;
        write(&mut records, &repair)?;
        repair.complete();
        assert!(!source.data_changed());
    }
    Ok(())
}

#[test]
fn cancelling_both_handles_allows_later_independent_acknowledgement() -> Result {
    let (source, mut records) = persisted()?;
    let first = source.clone().prepare_save()?;
    let second = source.clone().prepare_save()?;
    drop(first);
    drop(second);
    assert!(source.data_changed());
    let repair = source.clone().prepare_save()?;
    write(&mut records, &repair)?;
    repair.complete();
    assert!(!source.data_changed());
    Ok(())
}

#[test]
fn cancellation_never_refunds_fresh_creation_authority() -> Result {
    let source = Session::new();
    let pending = source.clone().prepare_save()?;
    drop(pending);
    assert!(source.data_changed());
    assert!(source.prepare_save().is_err());
    Ok(())
}

#[test]
fn rotation_keeps_stale_id_rejection_while_uncertainty_can_recover() -> Result {
    let (source, mut records) = persisted()?;
    let old = source.clone().prepare_save()?;
    let mut rotating = source.clone();
    rotating.regenerate();
    let new = rotating.clone().prepare_save()?;
    write(&mut records, &new)?;
    new.complete();
    assert!(old.validate_current().is_err());
    drop(old);
    assert!(source.data_changed());
    let repair = rotating.prepare_save()?;
    write(&mut records, &repair)?;
    repair.complete();
    assert!(!source.data_changed());
    assert!(source.prepare_save().is_err());
    Ok(())
}

#[test]
fn explicit_reset_overrides_uncertainty_but_later_callbacks_restore_it() -> Result {
    let (source, mut records) = persisted()?;
    let first = source.clone().prepare_save()?;
    let second = source.clone().prepare_save()?;
    source.reset_data_changed();
    assert!(!source.data_changed());
    write(&mut records, &first)?;
    first.complete();
    assert!(source.data_changed());
    drop(second);
    assert!(source.data_changed());
    source.reset_data_changed();
    assert!(!source.data_changed());
    Ok(())
}

//! These integration tests are an external crate using only public APIs.
use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use saysion::{Result, Session, SessionConflict, SessionStore, async_trait};

#[derive(Default)]
struct CustomStore {
    records: Mutex<HashMap<String, String>>,
    gate: Option<Arc<Gate>>,
}

#[derive(Default)]
struct Gate {
    captured: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

#[async_trait]
impl SessionStore for CustomStore {
    async fn load_session(&self, cookie: String) -> Result<Option<Session>> {
        let id = Session::id_from_cookie_value(&cookie)?;
        let encoded = self.records.lock().unwrap().get(&id).cloned();
        match encoded {
            Some(encoded) => serde_json::from_str::<Session>(&encoded)?.validate_loaded(&cookie),
            None => Ok(None),
        }
    }

    async fn store_session(&self, session: Session) -> Result<Option<String>> {
        let save = session.prepare_save()?;
        let write = save.begin_write().await?;
        let encoded = serde_json::to_string(save.session())?;
        save.validate_current()?;
        if let Some(gate) = &self.gate {
            // Model an asynchronous backend operation already accepted before
            // its completion is released. Lifecycle coordination must also
            // handle destruction arriving during this interval.
            gate.captured.notify_one();
            gate.resume.notified().await;
        }
        let id = save.session().id().to_owned();
        {
            let mut records = self.records.lock().unwrap();
            if let Some(origin) = save.origin()
                && !records.contains_key(origin)
            {
                return Err(SessionConflict.into());
            }
            if save.origin() != Some(id.as_str()) && records.contains_key(&id) {
                return Err(SessionConflict.into());
            }
            if let Some(origin) = save.origin().filter(|origin| *origin != id) {
                records.remove(origin);
            }
            records.insert(id, encoded);
        }
        if write.is_destroyed() {
            self.records.lock().unwrap().remove(save.session().id());
        }
        save.complete_with_guard(write)
    }

    async fn destroy_session(&self, mut session: Session) -> Result {
        let deletion = session.begin_delete().await?;
        self.records.lock().unwrap().remove(deletion.id());
        Ok(())
    }

    async fn clear_store(&self) -> Result {
        self.records.lock().unwrap().clear();
        Ok(())
    }
}

#[tokio::test]
async fn same_lineage_deletion_waits_for_an_accepted_first_write() -> Result {
    let gate = Arc::new(Gate::default());
    let store = CustomStore {
        gate: Some(gate.clone()),
        ..CustomStore::default()
    };
    let mut session = Session::new();
    session.insert("value", 42_u64)?;
    let logout = session.clone();
    let observer = session.clone();
    let mut save = Box::pin(store.store_session(session));
    poll_fn(|cx| match save.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("save did not wait for completion"),
    })
    .await;
    let mut deletion = Box::pin(store.destroy_session(logout));
    poll_fn(|cx| match deletion.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("deletion must wait for the accepted write"),
    })
    .await;
    assert!(observer.is_destroyed());
    gate.resume.notify_one();
    let (saved, deleted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(save, deletion)
    })
    .await?;
    assert!(
        saved
            .unwrap_err()
            .downcast_ref::<SessionConflict>()
            .is_some()
    );
    deleted?;
    assert!(store.records.lock().unwrap().is_empty());
    assert!(observer.data_changed());
    Ok(())
}

#[tokio::test]
async fn cancelling_a_guard_before_io_does_not_promise_successful_deletion() -> Result {
    let session = Session::new();
    let mut observer = session.clone();
    let save = session.prepare_save()?;
    let write = save.begin_write().await?;
    drop(write);
    drop(save);
    // The generic public protocol cannot determine whether I/O was dispatched.
    let error = observer.begin_delete().await.unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    assert!(observer.is_destroyed());
    Ok(())
}

#[tokio::test]
async fn cancelling_after_a_mock_write_requires_reloading_confirmed_state() -> Result {
    let store = CustomStore::default();
    let session = Session::new();
    let mut observer = session.clone();
    let writer = session.clone();
    let cookie = session.into_cookie_value().unwrap();
    let save = writer.prepare_save()?;
    let write = save.begin_write().await?;
    store.records.lock().unwrap().insert(
        save.session().id().to_owned(),
        serde_json::to_string(save.session())?,
    );
    drop(write);
    drop(save);
    assert!(observer.begin_delete().await.is_err());
    assert_eq!(store.records.lock().unwrap().len(), 1);
    // A trusted lookup creates an isolated lifecycle for the confirmed record.
    let confirmed = store.load_session(cookie).await?.unwrap();
    store.destroy_session(confirmed).await?;
    assert!(store.records.lock().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_write_guard_cannot_confirm_another_save_token() -> Result {
    let first = Session::new().prepare_save()?;
    let second = Session::new().prepare_save()?;
    let write = first.begin_write().await?;
    let error = second.complete_with_guard(write).unwrap_err();
    assert!(error.downcast_ref::<SessionConflict>().is_some());
    Ok(())
}

#[tokio::test]
async fn external_store_rotates_and_rejects_independent_stale_requests() -> Result {
    let store = CustomStore::default();
    let mut session = Session::new();
    session.insert("user_id", 42_u64)?;
    let cookie = store.store_session(session).await?.unwrap();
    let mut rotating = store.load_session(cookie.clone()).await?.unwrap();
    let stale = store.load_session(cookie.clone()).await?.unwrap();
    rotating.regenerate();
    let replacement = store.store_session(rotating).await?.unwrap();
    assert!(store.load_session(cookie).await?.is_none());
    assert!(store.store_session(stale).await.is_err());
    let loaded = store.load_session(replacement.clone()).await?.unwrap();
    assert_eq!(loaded.get::<u64>("user_id"), Some(42));
    assert!(!loaded.needs_persist());
    store.store_session(loaded).await?;
    let late = store.load_session(replacement.clone()).await?.unwrap();
    let mut logout = store.load_session(replacement.clone()).await?.unwrap();
    logout.regenerate();
    logout.destroy();
    store.destroy_session(logout).await?;
    assert!(store.load_session(replacement).await?.is_none());
    assert!(store.store_session(late).await.is_err());
    Ok(())
}

#[tokio::test]
async fn external_store_acknowledges_only_the_snapshot_before_io() -> Result {
    let gate = Arc::new(Gate::default());
    let store = CustomStore {
        gate: Some(gate.clone()),
        ..CustomStore::default()
    };
    let mut session = Session::new();
    session.insert("value", 1_u64)?;
    let mut editor = session.clone();
    let save = store.store_session(session);
    let edit = async {
        gate.captured.notified().await;
        editor.insert("value", 2_u64).unwrap();
        gate.resume.notify_one();
    };
    let (saved, ()) = tokio::join!(save, edit);
    let cookie = saved?.unwrap();
    assert!(editor.data_changed());
    assert_eq!(
        store
            .load_session(cookie)
            .await?
            .unwrap()
            .get::<u64>("value"),
        Some(1)
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_first_save_keeps_changes_and_consumes_creation_authority() -> Result {
    let store = CustomStore {
        gate: Some(Arc::new(Gate::default())),
        ..CustomStore::default()
    };
    let mut session = Session::new();
    session.insert("value", true)?;
    let observer = session.clone();
    let mut save = Box::pin(store.store_session(session));
    poll_fn(|cx| match save.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("save did not wait for the backend gate"),
    })
    .await;
    drop(save);
    assert!(store.records.lock().unwrap().is_empty());
    assert!(observer.data_changed());
    assert!(observer.prepare_save().is_err());
    Ok(())
}

#[tokio::test]
async fn failed_first_insert_never_overwrites_a_collision_or_refunds_authority() -> Result {
    let store = CustomStore::default();
    let mut session = Session::new();
    session.insert("value", true)?;
    let observer = session.clone();
    let id = session.id().to_owned();
    store
        .records
        .lock()
        .unwrap()
        .insert(id.clone(), "existing record".into());
    assert!(store.store_session(session).await.is_err());
    assert_eq!(store.records.lock().unwrap()[&id], "existing record");
    assert!(observer.data_changed());
    assert!(observer.prepare_save().is_err());
    Ok(())
}

#[tokio::test]
async fn trusted_load_checks_identity_expiry_and_isolates_existing_clones() -> Result {
    let store = CustomStore::default();
    let mut session = Session::new();
    session.insert("value", 1_u64)?;
    let cookie = store.store_session(session).await?.unwrap();
    let id = Session::id_from_cookie_value(&cookie)?;
    let encoded = store.records.lock().unwrap()[&id].clone();
    let mut decoded: Session = serde_json::from_str(&encoded)?;
    assert!(decoded.clone().validate_loaded(&id).is_err());
    let mut loaded = decoded.clone().validate_loaded(&cookie)?.unwrap();
    assert!(!loaded.needs_persist());
    loaded.insert("value", 2_u64)?;
    assert_eq!(decoded.get::<u64>("value"), Some(1));
    assert!(!decoded.data_changed());
    assert!(loaded.data_changed());
    loaded.destroy();
    assert!(!decoded.is_destroyed());
    assert!(decoded.clone().prepare_save().is_err());
    decoded.set_expiry(saysion::time::OffsetDateTime::now_utc() - saysion::time::Duration::SECOND);
    assert!(decoded.validate_loaded(&cookie)?.is_none());
    Ok(())
}

#[tokio::test]
async fn a_request_local_destroy_or_rotation_invalidates_a_pending_save() -> Result {
    let store = CustomStore::default();
    let cookie = store.store_session(Session::new()).await?.unwrap();
    let mut current = store.load_session(cookie).await?.unwrap();
    let save = current.clone().prepare_save()?;
    current.regenerate();
    store.store_session(current.clone()).await?;
    assert!(save.validate_current().is_err());
    let save = current.clone().prepare_save()?;
    current.destroy();
    assert!(save.validate_current().is_err());
    Ok(())
}

#[test]
fn save_debug_does_not_expose_identifiers_or_data() -> Result {
    let mut session = Session::new();
    session.insert("secret", "private-session-value")?;
    let id = session.id().to_owned();
    let save = session.prepare_save()?;
    let debug = format!("{save:?} {save:#?}");
    assert!(!debug.contains(&id));
    assert!(!debug.contains("private-session-value"));
    Ok(())
}

#[test]
fn public_snapshot_preserves_wire_format_and_isolated_contents() -> Result {
    let mut session = Session::new();
    session.insert("value", 1_u64)?;
    session.expire_in(std::time::Duration::from_secs(3600));
    let json = serde_json::to_value(&session)?;
    let binary = postcard::to_stdvec(&session)?;
    let mut editor = session.clone();
    let save = session.prepare_save()?;
    assert_eq!(serde_json::to_value(save.session())?, json);
    assert_eq!(postcard::to_stdvec(save.session())?, binary);
    let copy = save.session().clone();
    editor.insert("value", 2_u64)?;
    assert_eq!(copy.get_raw("value"), Some("1"));
    assert_eq!(save.session().get_raw("value"), Some("1"));
    assert_eq!(editor.get::<u64>("value"), Some(2));
    Ok(())
}

#[tokio::test]
async fn external_store_preserves_the_source_on_replacement_collision() -> Result {
    let store = CustomStore::default();
    let cookie = store.store_session(Session::new()).await?.unwrap();
    let mut rotating = store.load_session(cookie.clone()).await?.unwrap();
    rotating.regenerate();
    let replacement = rotating.id().to_owned();
    store
        .records
        .lock()
        .unwrap()
        .insert(replacement.clone(), "unrelated record".into());
    assert!(store.store_session(rotating).await.is_err());
    assert!(store.load_session(cookie).await?.is_some());
    assert_eq!(
        store.records.lock().unwrap()[&replacement],
        "unrelated record"
    );
    Ok(())
}

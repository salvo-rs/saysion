use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use async_lock::RwLock;

use crate::{Result, Session, SessionStore, async_trait};

const CLEANUP_BATCH_SIZE: usize = 256;
const DEFAULT_CLEANUP_LIMIT: u32 = 1_000;
const MAX_CLEANUP_LIMIT: u32 = 10_000;

#[derive(Default)]
struct MemoryState {
    records: HashMap<String, Session>,
    expiries: BTreeSet<(time::OffsetDateTime, String)>,
    #[cfg(test)]
    cleanup_entries_examined: std::sync::atomic::AtomicUsize,
}

impl MemoryState {
    fn insert(&mut self, id: String, session: Session) -> Option<Session> {
        let previous = self.remove(&id);
        if let Some(expiry) = session.expiry() {
            self.expiries.insert((*expiry, id.clone()));
        }
        self.records.insert(id, session);
        previous
    }

    fn remove(&mut self, id: &str) -> Option<Session> {
        let previous = self.records.remove(id)?;
        if let Some(expiry) = previous.expiry() {
            self.expiries.remove(&(*expiry, id.to_owned()));
        }
        Some(previous)
    }

    fn clear(&mut self) {
        self.records.clear();
        self.expiries.clear();
    }

    fn expired_candidates(&self, now: time::OffsetDateTime, limit: usize) -> Vec<String> {
        // Start at the oldest expiry. Permanent records are never indexed, and
        // the first live entry terminates the scan regardless of store size.
        let mut expiries = self.expiries.iter();
        let mut ids = Vec::new();
        for _ in 0..limit {
            let Some((expiry, id)) = expiries.next() else {
                break;
            };
            #[cfg(test)]
            self.cleanup_entries_examined
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if *expiry >= now {
                break;
            }
            ids.push(id.clone());
        }
        ids
    }
}

/// # in-memory session store
/// Because there is no external
/// persistence, this session store is ephemeral and will be cleared
/// on server restart.
///
/// # ***READ THIS BEFORE USING IN A PRODUCTION DEPLOYMENT***
///
/// Storing sessions only in memory brings the following problems:
///
/// 1. All sessions must fit in available memory (important for high load services)
/// 2. Expired records require periodic [MemoryStore::cleanup] or
///    [MemoryStore::cleanup_batch] calls; expiry only prevents loading them.
///    If sessions are not cleaned up properly it might result in OOM.
/// 3. All sessions will be lost on shutdown
/// 4. If the service is clustered particular session will be stored only on a single instance.
///    This might be solved by using load balancers with sticky sessions.
///    Unfortunately, this solution brings additional complexity especially if the connection is
///    using secure transport since the load balancer has to perform SSL termination to understand
///    where should it forward packets to
///
/// Example crates providing alternative implementations:
/// - [async-sqlx-session](https://crates.io/crates/async-sqlx-session) postgres & sqlite
/// - [async-redis-session](https://crates.io/crates/async-redis-session)
/// - [async-mongodb-session](https://crates.io/crates/async-mongodb-session)
///
#[derive(Default, Clone)]
pub struct MemoryStore {
    inner: Arc<RwLock<MemoryState>>,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Do not traverse sessions or acquire the store lock for diagnostics.
        f.debug_struct("MemoryStore").finish_non_exhaustive()
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
        let id = Session::id_from_cookie_value(&cookie_value)?;
        tracing::debug!("loading session by id `{}`", id);
        Ok(self
            .inner
            .read()
            .await
            .records
            .get(&id)
            .filter(|session| !session.is_expired())
            .map(Session::snapshot)
            .and_then(Session::validate)
            .map(Session::mark_loaded))
    }

    async fn store_session(&self, session: Session) -> Result<Option<String>> {
        tracing::debug!("storing session by id `{}`", session.id());
        let attempt = session.persistence_attempt()?;
        let (snapshot, revision) = session.persistence_snapshot();
        let mut state = self.inner.write().await;
        let origin = if attempt.origin.is_none() {
            None
        } else {
            session.persistence_origin()?
        };
        if session.is_destroyed() {
            return Err(crate::SessionConflict.into());
        }
        if let Some(source) = origin.as_ref()
            && !state.records.contains_key(source)
        {
            return Err(crate::SessionConflict.into());
        }
        if origin.as_deref() != Some(session.id()) && state.records.contains_key(session.id()) {
            return Err(crate::SessionConflict.into());
        }
        if let Some(old_id) = origin.as_deref().filter(|old_id| *old_id != session.id())
            && state.remove(old_id).is_none()
        {
            return Err(crate::SessionConflict.into());
        }
        state.insert(session.id().to_string(), snapshot.mark_loaded());
        drop(state);

        session.mark_persisted();
        session.acknowledge_revision(revision);
        Ok(session.into_cookie_value())
    }

    async fn destroy_session(&self, mut session: Session) -> Result {
        tracing::debug!("destroying session by id `{}`", session.id());
        let origin = session
            .persistence_origin()
            .ok()
            .flatten()
            .unwrap_or_else(|| session.id().to_owned());
        session.destroy();
        self.inner.write().await.remove(&origin);
        Ok(())
    }

    async fn clear_store(&self) -> Result {
        tracing::debug!("clearing memory store");
        self.inner.write().await.clear();
        Ok(())
    }
}

impl MemoryStore {
    /// Create a new instance of MemoryStore
    pub fn new() -> Self {
        Self::default()
    }

    /// Remove at most 1,000 expired sessions in one cleanup batch.
    ///
    /// Schedule this periodically, or use [`Self::cleanup_batch`] to choose a
    /// limit and observe progress. Expired records beyond this batch remain
    /// inaccessible and will be removed by subsequent calls.
    pub async fn cleanup(&self) -> Result {
        self.cleanup_batch(DEFAULT_CLEANUP_LIMIT).await?;
        Ok(())
    }

    /// Remove up to `limit` expired sessions and return the number removed.
    ///
    /// `limit` must be between 1 and 10,000. An ordered expiry index bounds
    /// candidate inspection and allocation by this limit without scanning live
    /// or permanent records. Concurrent renewals can reduce the removal count.
    /// Each write lock covers at most 256 candidates; work also depends on the
    /// payload sizes of removed sessions, so this is not a wall-clock deadline.
    pub async fn cleanup_batch(&self, limit: u32) -> Result<u64> {
        anyhow::ensure!(
            (1..=MAX_CLEANUP_LIMIT).contains(&limit),
            "cleanup batch size must be between 1 and {MAX_CLEANUP_LIMIT}"
        );
        tracing::debug!("cleaning up memory store...");
        let now = time::OffsetDateTime::now_utc();
        let ids_to_delete = self.expired_candidates(now, limit as usize).await;
        tracing::debug!("found {} expired sessions", ids_to_delete.len());
        Ok(self.remove_expired_candidates(&ids_to_delete, now).await)
    }

    async fn expired_candidates(&self, now: time::OffsetDateTime, limit: usize) -> Vec<String> {
        self.inner.read().await.expired_candidates(now, limit)
    }

    async fn remove_expired_candidates(&self, ids: &[String], now: time::OffsetDateTime) -> u64 {
        let mut count = 0;
        for batch in ids.chunks(CLEANUP_BATCH_SIZE) {
            let mut state = self.inner.write().await;
            let mut removed = Vec::new();
            for id in batch {
                // A request may have renewed this ID after candidate selection.
                // Check the current entry while holding the same lock as remove.
                let expired = state
                    .records
                    .get(id)
                    .is_some_and(|session| session.expiry().is_some_and(|expiry| *expiry < now));
                if expired && let Some(session) = state.remove(id) {
                    removed.push(session);
                    count += 1;
                }
            }
            // Destructing a large payload need not hold the global store lock.
            drop(state);
            drop(removed);
        }
        count
    }

    /// returns the number of elements in the memory store
    /// # Example
    /// ```rust
    /// # use saysion::{MemoryStore, Session, SessionStore};
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut store = MemoryStore::new();
    /// assert_eq!(store.count().await, 0);
    /// store.store_session(Session::new()).await?;
    /// assert_eq!(store.count().await, 1);
    /// # Ok(()) }
    /// ```
    pub async fn count(&self) -> usize {
        let data = self.inner.read().await;
        data.records.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn examined(state: &MemoryState) -> usize {
        state
            .cleanup_entries_examined
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn assert_index_consistent(state: &MemoryState) {
        let expected: BTreeSet<_> = state
            .records
            .iter()
            .filter_map(|(id, session)| session.expiry().map(|expiry| (*expiry, id.clone())))
            .collect();
        assert_eq!(state.expiries, expected);
    }

    #[test]
    fn cleanup_cutoff_keeps_equal_timestamps_and_removes_past_ones() {
        let mut state = MemoryState::default();
        let now = time::OffsetDateTime::now_utc();
        let mut session = Session::new();
        session.set_expiry(now);
        let id = session.id().to_owned();
        state.insert(id.clone(), session);
        assert!(state.expired_candidates(now, 10).is_empty());
        assert_eq!(
            state.expired_candidates(now + time::Duration::NANOSECOND, 10),
            vec![id]
        );
    }

    #[tokio::test]
    async fn cleanup_default_and_explicit_limits_bound_inspection_and_removal() -> Result {
        let store = MemoryStore::new();
        let expiry = time::OffsetDateTime::now_utc() - Duration::from_secs(1);
        {
            let mut state = store.inner.write().await;
            for _ in 0..1_007 {
                let mut session = Session::new();
                session.set_expiry(expiry);
                state.insert(session.id().to_owned(), session);
            }
        }
        assert!(store.cleanup_batch(0).await.is_err());
        assert!(store.cleanup_batch(10_001).await.is_err());
        assert_eq!(examined(&*store.inner.read().await), 0);
        store.cleanup().await?;
        assert_eq!(store.count().await, 7);
        assert_eq!(examined(&*store.inner.read().await), 1_000);
        assert_eq!(store.cleanup_batch(3).await?, 3);
        assert_eq!(store.count().await, 4);
        assert_eq!(examined(&*store.inner.read().await), 1_003);
        assert_eq!(store.cleanup_batch(10_000).await?, 4);
        assert_eq!(store.cleanup_batch(10_000).await?, 0);
        let state = store.inner.read().await;
        assert!(state.records.is_empty());
        assert!(state.expiries.is_empty());
        assert_index_consistent(&state);
        Ok(())
    }

    #[tokio::test]
    async fn overlapping_cleanup_batches_do_not_delete_additional_records() -> Result {
        let store = MemoryStore::new();
        let now = time::OffsetDateTime::now_utc();
        {
            let mut state = store.inner.write().await;
            for _ in 0..5 {
                let mut session = Session::new();
                session.set_expiry(now - Duration::from_secs(1));
                state.insert(session.id().to_owned(), session);
            }
        }
        // Model both callers selecting before either gets the removal lock.
        let first = store.expired_candidates(now, 3).await;
        let second = store.expired_candidates(now, 3).await;
        assert_eq!(first, second);
        assert_eq!(store.remove_expired_candidates(&first, now).await, 3);
        assert_eq!(store.remove_expired_candidates(&second, now).await, 0);
        assert_eq!(store.count().await, 2);
        assert_index_consistent(&*store.inner.read().await);
        assert_eq!(store.cleanup_batch(3).await?, 2);
        assert_eq!(store.count().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_does_not_scan_large_live_or_permanent_record_sets() -> Result {
        let store = MemoryStore::new();
        let now = time::OffsetDateTime::now_utc();
        {
            let mut state = store.inner.write().await;
            for _ in 0..20_000 {
                let mut live = Session::new();
                live.set_expiry(now + Duration::from_secs(3_600));
                state.insert(live.id().to_owned(), live);
                let permanent = Session::new();
                state.insert(permanent.id().to_owned(), permanent);
            }
            for _ in 0..3 {
                let mut expired = Session::new();
                expired.set_expiry(now - Duration::from_secs(1));
                state.insert(expired.id().to_owned(), expired);
            }
            assert_eq!(state.expiries.len(), 20_003);
        }
        assert_eq!(store.cleanup_batch(2).await?, 2);
        assert_eq!(examined(&*store.inner.read().await), 2);
        assert_eq!(store.cleanup_batch(10).await?, 1);
        // One remaining expired entry plus only the first live expiry.
        assert_eq!(examined(&*store.inner.read().await), 4);
        assert_eq!(store.cleanup_batch(10_000).await?, 0);
        assert_eq!(examined(&*store.inner.read().await), 5);
        assert_eq!(store.count().await, 40_000);
        assert_index_consistent(&*store.inner.read().await);
        Ok(())
    }

    #[tokio::test]
    async fn expiry_index_tracks_updates_rotations_destruction_and_clear() -> Result {
        let store = MemoryStore::new();
        let mut session = Session::new();
        session.expire_in(Duration::from_secs(3_600));
        let old_expiry = *session.expiry().unwrap();
        let old_id = session.id().to_owned();
        let old_cookie = store.store_session(session).await?.unwrap();
        let mut loaded = store.load_session(old_cookie.clone()).await?.unwrap();
        loaded.set_expiry(old_expiry + Duration::from_secs(3_600));
        store.store_session(loaded).await?;
        {
            let state = store.inner.read().await;
            assert!(!state.expiries.contains(&(old_expiry, old_id)));
            assert_eq!(state.expiries.len(), 1);
            assert_index_consistent(&state);
        }
        let mut rotating = store.load_session(old_cookie).await?.unwrap();
        rotating.regenerate();
        let cookie = store.store_session(rotating).await?.unwrap();
        assert_index_consistent(&*store.inner.read().await);
        let loaded = store.load_session(cookie).await?.unwrap();
        store.destroy_session(loaded).await?;
        assert!(store.inner.read().await.expiries.is_empty());

        let mut session = Session::new();
        session.expire_in(Duration::from_secs(3_600));
        store.store_session(session).await?;
        store.store_session(Session::new()).await?;
        store.clear_store().await?;
        let state = store.inner.read().await;
        assert!(state.records.is_empty());
        assert!(state.expiries.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn loading_expired_records_never_copies_the_session_payload() -> Result {
        let store = MemoryStore::new();
        let mut session = Session::new();
        session.insert_raw("large", "x".repeat(1 << 20));
        session.set_expiry(time::OffsetDateTime::now_utc() - Duration::from_secs(1));
        let cookie = store.store_session(session).await?.unwrap();
        let before = crate::session::SNAPSHOT_CALLS.with(|calls| calls.get());
        for _ in 0..3 {
            assert!(store.load_session(cookie.clone()).await?.is_none());
        }
        let after = crate::session::SNAPSHOT_CALLS.with(|calls| calls.get());
        assert_eq!(after, before, "expired records must not enter snapshot()");

        // Instrumentation also observes the real load path for valid records.
        let cookie = store.store_session(Session::new()).await?.unwrap();
        assert!(store.load_session(cookie).await?.is_some());
        let after_valid = crate::session::SNAPSHOT_CALLS.with(|calls| calls.get());
        assert_eq!(after_valid, before + 1);
        Ok(())
    }

    #[tokio::test]
    async fn saving_a_snapshot_does_not_clear_a_later_clone_edit() -> Result {
        use std::future::{Future, poll_fn};
        use std::task::Poll;

        let store = MemoryStore::new();
        let mut session = Session::new();
        session.insert("value", 1)?;
        let mut editor = session.clone();
        let blocker = store.inner.write().await;
        let mut save = std::pin::pin!(store.store_session(session));
        poll_fn(|cx| match save.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(result) => panic!("save completed while the store was blocked: {result:?}"),
        })
        .await;
        editor.insert("value", 2)?;
        drop(blocker);
        let cookie = save.await?.unwrap();

        assert!(editor.data_changed());
        assert_eq!(
            store
                .load_session(cookie.clone())
                .await?
                .unwrap()
                .get::<u64>("value"),
            Some(1)
        );
        store.store_session(editor.clone()).await?;
        assert!(!editor.data_changed());
        assert_eq!(
            store
                .load_session(cookie)
                .await?
                .unwrap()
                .get::<u64>("value"),
            Some(2)
        );
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_rechecks_renewed_candidates_before_removal() -> Result {
        let store = MemoryStore::new();
        let now = time::OffsetDateTime::now_utc();
        let mut session = Session::new();
        session.set_expiry(now + Duration::from_secs(3_600));
        let mut renewed = session.clone();
        let cookie = store.store_session(session).await?.unwrap();
        // Model expiry while an already-loaded request is still running.
        {
            let mut state = store.inner.write().await;
            let mut expired = state.remove(renewed.id()).unwrap();
            expired.set_expiry(now - Duration::from_secs(1));
            state.insert(expired.id().to_owned(), expired);
        }
        let candidates = store.expired_candidates(now, 10).await;
        assert_eq!(candidates, vec![renewed.id().to_owned()]);

        renewed.set_expiry(now + Duration::from_secs(3_600));
        store.store_session(renewed).await?;
        assert_eq!(store.remove_expired_candidates(&candidates, now).await, 0);

        assert_eq!(store.count().await, 1);
        assert!(store.load_session(cookie).await?.is_some());
        assert_index_consistent(&*store.inner.read().await);
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_removes_expired_sessions_across_batches() -> Result {
        let store = MemoryStore::new();
        let now = time::OffsetDateTime::now_utc();
        for _ in 0..(CLEANUP_BATCH_SIZE + 1) {
            let mut session = Session::new();
            session.set_expiry(now - Duration::from_secs(1));
            store
                .inner
                .write()
                .await
                .insert(session.id().to_owned(), session.snapshot());
        }
        let mut live = Session::new();
        live.set_expiry(now + Duration::from_secs(3_600));
        let live_cookie = store.store_session(live).await?.unwrap();
        let permanent_cookie = store.store_session(Session::new()).await?.unwrap();

        store.cleanup().await?;

        assert_eq!(store.count().await, 2);
        assert!(store.load_session(live_cookie).await?.is_some());
        assert!(store.load_session(permanent_cookie).await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn creating_a_new_session_with_no_expiry() -> Result {
        let store = MemoryStore::new();
        let mut session = Session::new();
        session.insert("key", "Hello")?;
        let cloned = session.clone();
        let cookie_value = store.store_session(session).await?.unwrap();
        let loaded_session = store.load_session(cookie_value).await?.unwrap();
        assert_eq!(cloned.id(), loaded_session.id());
        assert_eq!("Hello", &loaded_session.get::<String>("key").unwrap());
        assert!(!loaded_session.is_expired());
        assert!(loaded_session.validate().is_some());
        Ok(())
    }

    #[tokio::test]
    async fn updating_a_session() -> Result {
        let store = MemoryStore::new();
        let mut session = Session::new();

        session.insert("key", "value")?;
        let cookie_value = store.store_session(session).await?.unwrap();

        let mut session = store.load_session(cookie_value.clone()).await?.unwrap();
        session.insert("key", "other value")?;

        assert_eq!(store.store_session(session).await?, None);
        let session = store.load_session(cookie_value).await?.unwrap();
        assert_eq!(&session.get::<String>("key").unwrap(), "other value");

        Ok(())
    }

    #[tokio::test]
    async fn updating_a_session_extending_expiry() -> Result {
        let store = MemoryStore::new();
        let mut session = Session::new();
        session.expire_in(Duration::from_secs(1));
        let original_expires = *session.expiry().unwrap();
        let cookie_value = store.store_session(session).await?.unwrap();

        let mut session = store.load_session(cookie_value.clone()).await?.unwrap();

        assert_eq!(session.expiry().unwrap(), &original_expires);
        session.expire_in(Duration::from_secs(3));
        let new_expires = *session.expiry().unwrap();
        assert_eq!(None, store.store_session(session).await?);

        let session = store.load_session(cookie_value.clone()).await?.unwrap();
        assert_eq!(session.expiry().unwrap(), &new_expires);

        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(None, store.load_session(cookie_value).await?);

        Ok(())
    }

    #[tokio::test]
    async fn creating_a_new_session_with_expiry() -> Result {
        let store = MemoryStore::new();
        let mut session = Session::new();
        session.expire_in(Duration::from_secs(3));
        session.insert("key", "value")?;
        let cloned = session.clone();

        let cookie_value = store.store_session(session).await?.unwrap();

        let loaded_session = store.load_session(cookie_value.clone()).await?.unwrap();
        assert_eq!(cloned.id(), loaded_session.id());
        assert_eq!("value", &*loaded_session.get::<String>("key").unwrap());

        assert!(!loaded_session.is_expired());

        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(None, store.load_session(cookie_value).await?);

        Ok(())
    }

    #[tokio::test]
    async fn destroying_a_single_session() -> Result {
        let store = MemoryStore::new();
        for _ in 0..3i8 {
            store.store_session(Session::new()).await?;
        }

        let cookie = store.store_session(Session::new()).await?.unwrap();
        assert_eq!(4, store.count().await);
        let session = store.load_session(cookie.clone()).await?.unwrap();
        store.destroy_session(session.clone()).await?;
        assert_eq!(None, store.load_session(cookie).await?);
        assert_eq!(3, store.count().await);

        // attempting to destroy the session again is not an error
        assert!(store.destroy_session(session).await.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn clearing_the_whole_store() -> Result {
        let store = MemoryStore::new();
        for _ in 0..3i8 {
            store.store_session(Session::new()).await?;
        }

        assert_eq!(3, store.count().await);
        store.clear_store().await.unwrap();
        assert_eq!(0, store.count().await);

        Ok(())
    }
}

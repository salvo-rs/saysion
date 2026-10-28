use std::{collections::HashMap, sync::Arc};

use async_lock::RwLock;

use crate::{Result, Session, SessionStore, async_trait};

const CLEANUP_BATCH_SIZE: usize = 256;

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
/// 2. Sessions stored in memory are cleared only if a client calls [MemoryStore::destroy_session] or [MemoryStore::clear_store].
///    If sessions are not cleaned up properly it might result in OOM
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
    inner: Arc<RwLock<HashMap<String, Session>>>,
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
            .get(&id)
            .map(Session::snapshot)
            .and_then(Session::validate))
    }

    async fn store_session(&self, session: Session) -> Result<Option<String>> {
        tracing::debug!("storing session by id `{}`", session.id());
        let (snapshot, revision) = session.persistence_snapshot();
        self.inner
            .write()
            .await
            .insert(session.id().to_string(), snapshot);

        session.acknowledge_revision(revision);
        Ok(session.into_cookie_value())
    }

    async fn destroy_session(&self, session: Session) -> Result {
        tracing::debug!("destroying session by id `{}`", session.id());
        self.inner.write().await.remove(session.id());
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

    /// Performs session cleanup. This should be run on an
    /// intermittent basis if this store is run for long enough that
    /// memory accumulation is a concern
    pub async fn cleanup(&self) -> Result {
        tracing::debug!("cleaning up memory store...");
        let now = time::OffsetDateTime::now_utc();
        let ids_to_delete = self.expired_candidates(now).await;
        tracing::debug!("found {} expired sessions", ids_to_delete.len());
        self.remove_expired_candidates(&ids_to_delete, now).await;
        Ok(())
    }

    async fn expired_candidates(&self, now: time::OffsetDateTime) -> Vec<String> {
        self.inner
            .read()
            .await
            .values()
            .filter_map(|session| {
                if session.expiry().is_some_and(|expiry| *expiry < now) {
                    Some(session.id().to_owned())
                } else {
                    None
                }
            })
            .collect()
    }

    async fn remove_expired_candidates(&self, ids: &[String], now: time::OffsetDateTime) {
        for batch in ids.chunks(CLEANUP_BATCH_SIZE) {
            let mut sessions = self.inner.write().await;
            for id in batch {
                // A request may have renewed this ID after candidate selection.
                // Check the current entry while holding the same lock as remove.
                let expired = sessions
                    .get(id)
                    .is_some_and(|session| session.expiry().is_some_and(|expiry| *expiry < now));
                if expired {
                    sessions.remove(id);
                }
            }
        }
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
        data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
        store
            .inner
            .write()
            .await
            .get_mut(renewed.id())
            .unwrap()
            .set_expiry(now - Duration::from_secs(1));
        let candidates = store.expired_candidates(now).await;
        assert_eq!(candidates, vec![renewed.id().to_owned()]);

        renewed.set_expiry(now + Duration::from_secs(3_600));
        store.store_session(renewed).await?;
        store.remove_expired_candidates(&candidates, now).await;

        assert_eq!(store.count().await, 1);
        assert!(store.load_session(cookie).await?.is_some());
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

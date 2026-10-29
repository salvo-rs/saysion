use crate::{Result, Session, SessionStore, async_trait};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

/// An unauthenticated codec that serializes the entire session into a cookie.
///
/// # Security
///
/// This type does **not** sign, authenticate, or encrypt its output. Clients
/// can forge every field, including identity, privileges, and expiry, unless
/// the middleware authenticates the entire value before calling `load_session`.
/// Use [`crate::SignedCookieStore`] when the middleware does not provide this
/// protection. Keeping this codec separate supports middleware such as Salvo's
/// session handler, which signs cookie values itself.
///
/// Data is serialized with [`postcard`] and Base64, and remains readable by the
/// client. Do not store secrets or confidential personal data. The middleware
/// must enforce an appropriate cookie size limit.
///
/// `destroy_session` and `clear_store` are no-ops. Removing a browser cookie
/// does not revoke a copied value: it can be replayed until its authenticated
/// expiry. Use a server-side store when immediate revocation is required.

#[derive(Default, Debug, Clone, Copy)]
pub struct CookieStore;

impl CookieStore {
    /// constructs a new CookieStore
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionStore for CookieStore {
    async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
        let serialized = BASE64_STANDARD.decode(cookie_value)?;
        let session: Session = postcard::from_bytes(&serialized)?;
        Ok(session.validate())
    }

    async fn store_session(&self, session: Session) -> Result<Option<String>> {
        let (snapshot, revision) = session.persistence_snapshot();
        let serialized = postcard::to_stdvec(&snapshot)?;
        let cookie = BASE64_STANDARD.encode(serialized);
        session.acknowledge_revision(revision);
        Ok(Some(cookie))
    }

    async fn destroy_session(&self, _session: Session) -> Result {
        Ok(())
    }

    async fn clear_store(&self) -> Result {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn creating_a_new_session_with_no_expiry() -> Result {
        let store = CookieStore::new();
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
        let store = CookieStore::new();
        let mut session = Session::new();

        session.insert("key", "value")?;
        let cookie_value = store.store_session(session).await?.unwrap();

        let mut session = store.load_session(cookie_value.clone()).await?.unwrap();
        session.insert("key", "other value")?;

        let new_cookie_value = store.store_session(session).await?.unwrap();
        let session = store.load_session(new_cookie_value).await?.unwrap();
        assert_eq!(&session.get::<String>("key").unwrap(), "other value");

        Ok(())
    }

    #[tokio::test]
    async fn updating_a_session_extending_expiry() -> Result {
        let store = CookieStore::new();
        let mut session = Session::new();
        session.expire_in(Duration::from_secs(1));
        let original_expires = *session.expiry().unwrap();
        let cookie_value = store.store_session(session).await?.unwrap();

        let mut session = store.load_session(cookie_value.clone()).await?.unwrap();

        assert_eq!(session.expiry().unwrap(), &original_expires);
        session.expire_in(Duration::from_secs(3));
        let new_expires = *session.expiry().unwrap();
        let cookie_value = store.store_session(session).await?.unwrap();

        let session = store.load_session(cookie_value.clone()).await?.unwrap();
        assert_eq!(session.expiry().unwrap(), &new_expires);

        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(None, store.load_session(cookie_value).await?);

        Ok(())
    }

    #[tokio::test]
    async fn creating_a_new_session_with_expiry() -> Result {
        let store = CookieStore::new();
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
}

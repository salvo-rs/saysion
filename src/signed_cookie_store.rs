use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{Result, Session, SessionStore, async_trait};

const PREFIX: &str = "s1.";
const DOMAIN: &[u8] = b"saysion:signed-cookie:v1:";
const TAG_LEN: usize = 32;
const MAX_VALUE_LEN: usize = 3800;
const MAX_BINARY_LEN: usize = (MAX_VALUE_LEN - PREFIX.len()) / 4 * 3;

/// A client-side session store authenticated with HMAC-SHA256.
///
/// The signing key must contain at least 32 bytes of cryptographically random
/// secret material. Keep it separate from keys used by other protocols.
/// [`Self::with_fallback_key`] permits existing cookies to survive key rotation;
/// only the current key signs new values. Remove fallback keys after the maximum
/// lifetime of cookies signed by them.
///
/// This store verifies the MAC before deserializing and limits encoded values
/// to 3800 bytes. Middleware must still budget for the cookie name, attributes,
/// and any outer signature when enforcing browser/header limits.
///
/// # Security
///
/// Signing provides integrity, **not encryption**. Session contents are readable
/// by clients. Use a server-side store for confidential data or immediate
/// revocation: `destroy_session` and `clear_store` are no-ops, and removing a
/// browser cookie does not revoke copies. Set a finite expiry for sessions.
///
/// This format is distinct from [`crate::CookieStore`]. Unsigned legacy values
/// are never accepted, including during key rotation. Invalid, forged, or
/// expired cookies load as `None`.
///
/// ```
/// use saysion::{Session, SessionStore, SignedCookieStore};
/// # #[tokio::main]
/// # async fn main() -> saysion::Result {
/// // Load a persistent, random secret from your application's secret manager.
/// # let secret_key = [42_u8; 32];
/// let store = SignedCookieStore::new(secret_key)?;
/// let mut session = Session::new();
/// session.expire_in(std::time::Duration::from_secs(1800));
/// session.insert("user_id", 42)?;
/// let cookie = store.store_session(session).await?.unwrap();
/// assert_eq!(store.load_session(cookie).await?.unwrap().get::<u64>("user_id"), Some(42));
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct SignedCookieStore {
    signing_key: Hmac<Sha256>,
    fallback_keys: Vec<Hmac<Sha256>>,
}

impl fmt::Debug for SignedCookieStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignedCookieStore")
            .field("fallback_key_count", &self.fallback_keys.len())
            .finish_non_exhaustive()
    }
}

impl SignedCookieStore {
    /// Create a store using at least 32 bytes of random secret signing material.
    pub fn new(key: impl AsRef<[u8]>) -> Result<Self> {
        Ok(Self {
            signing_key: Self::key(key.as_ref())?,
            fallback_keys: Vec::new(),
        })
    }

    /// Accept cookies signed by an old key without using it for new cookies.
    ///
    /// Each fallback key must contain at least 32 bytes of secret material.
    pub fn with_fallback_key(mut self, key: impl AsRef<[u8]>) -> Result<Self> {
        self.fallback_keys.push(Self::key(key.as_ref())?);
        Ok(self)
    }

    fn key(bytes: &[u8]) -> Result<Hmac<Sha256>> {
        anyhow::ensure!(
            bytes.len() >= 32,
            "cookie signing keys require at least 32 bytes"
        );
        Ok(Hmac::<Sha256>::new_from_slice(bytes)?)
    }

    fn mac(key: &Hmac<Sha256>, payload: &[u8]) -> Hmac<Sha256> {
        let mut mac = key.clone();
        mac.update(DOMAIN);
        mac.update(payload);
        mac
    }
}

#[async_trait]
impl SessionStore for SignedCookieStore {
    async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
        if cookie_value.len() > MAX_VALUE_LEN {
            return Ok(None);
        }
        let Some(encoded) = cookie_value.strip_prefix(PREFIX) else {
            return Ok(None);
        };
        let Ok(bytes) = BASE64_STANDARD.decode(encoded) else {
            return Ok(None);
        };
        let Some((tag, payload)) = bytes.split_at_checked(TAG_LEN) else {
            return Ok(None);
        };
        let authenticated = std::iter::once(&self.signing_key)
            .chain(&self.fallback_keys)
            .any(|key| Self::mac(key, payload).verify_slice(tag).is_ok());
        if !authenticated {
            return Ok(None);
        }
        let Ok((session, rest)) = postcard::take_from_bytes::<Session>(payload) else {
            return Ok(None);
        };
        if !rest.is_empty() {
            return Ok(None);
        }
        Ok(session.validate())
    }

    async fn store_session(&self, session: Session) -> Result<Option<String>> {
        // A bounded buffer rejects oversized sessions without allocating an
        // arbitrarily large serialized payload first.
        let mut buffer = vec![0; MAX_BINARY_LEN - TAG_LEN];
        let payload = postcard::to_slice(&session, &mut buffer)?;
        let tag = Self::mac(&self.signing_key, payload)
            .finalize()
            .into_bytes();
        let mut bytes = Vec::with_capacity(TAG_LEN + payload.len());
        bytes.extend_from_slice(&tag);
        bytes.extend_from_slice(payload);
        Ok(Some(format!("{PREFIX}{}", BASE64_STANDARD.encode(bytes))))
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

    const KEY: [u8; 32] = [42; 32];

    #[tokio::test]
    async fn authenticated_roundtrip_and_expiry() -> Result {
        let store = SignedCookieStore::new(KEY)?;
        let mut session = Session::new();
        session.insert("user_id", 42_u64)?;
        session.expire_in(std::time::Duration::from_secs(1800));
        let expiry = *session.expiry().unwrap();
        let cookie = store.store_session(session).await?.unwrap();
        assert!(cookie.len() <= MAX_VALUE_LEN);
        let loaded = store.load_session(cookie).await?.unwrap();
        assert_eq!(loaded.get::<u64>("user_id"), Some(42));
        assert_eq!(loaded.expiry(), Some(&expiry));
        assert!(!loaded.data_changed());

        let mut expired = Session::new();
        expired.set_expiry(time::OffsetDateTime::now_utc() - time::Duration::SECOND);
        let cookie = store.store_session(expired).await?.unwrap();
        assert!(store.load_session(cookie).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn rejects_forgery_tampering_and_wrong_keys() -> Result {
        let store = SignedCookieStore::new(KEY)?;
        let mut forged = Session::new();
        forged.insert("role", "admin")?;
        let unsigned = crate::CookieStore::new()
            .store_session(forged)
            .await?
            .unwrap();
        assert!(store.load_session(unsigned.clone()).await?.is_none());
        assert!(
            store
                .load_session(format!("{PREFIX}{unsigned}"))
                .await?
                .is_none()
        );

        let cookie = store.store_session(Session::new()).await?.unwrap();
        let wrong_key = SignedCookieStore::new([99; 32])?;
        assert!(wrong_key.load_session(cookie.clone()).await?.is_none());
        let original = BASE64_STANDARD.decode(cookie.strip_prefix(PREFIX).unwrap())?;
        for position in [0, TAG_LEN, original.len() - 1] {
            let mut bytes = original.clone();
            bytes[position] ^= 1;
            let tampered = format!("{PREFIX}{}", BASE64_STANDARD.encode(bytes));
            assert!(store.load_session(tampered).await?.is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn fallback_keys_verify_but_never_sign() -> Result {
        let old = SignedCookieStore::new(KEY)?;
        let current = SignedCookieStore::new([99; 32])?;
        let rotating = current.clone().with_fallback_key(KEY)?;
        let old_cookie = old.store_session(Session::new()).await?.unwrap();
        assert!(rotating.load_session(old_cookie.clone()).await?.is_some());
        assert!(current.load_session(old_cookie).await?.is_none());
        let new_cookie = rotating.store_session(Session::new()).await?.unwrap();
        assert!(current.load_session(new_cookie.clone()).await?.is_some());
        assert!(old.load_session(new_cookie).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn rejects_malformed_versions_and_oversized_cookies() -> Result {
        let store = SignedCookieStore::new(KEY)?;
        for value in ["", "s2.AAAA", "s1.!", "s1.AAAA"] {
            assert!(store.load_session(value.to_string()).await?.is_none());
        }
        assert!(
            store
                .load_session("A".repeat(MAX_VALUE_LEN + 1))
                .await?
                .is_none()
        );
        let mut large = Session::new();
        large.insert_raw("large", "A".repeat(MAX_VALUE_LEN));
        assert!(store.store_session(large).await.is_err());

        for payload in [&b"invalid postcard"[..], &b""[..]] {
            let tag = SignedCookieStore::mac(&store.signing_key, payload)
                .finalize()
                .into_bytes();
            let mut bytes = tag.to_vec();
            bytes.extend_from_slice(payload);
            assert!(
                store
                    .load_session(format!("{PREFIX}{}", BASE64_STANDARD.encode(bytes)))
                    .await?
                    .is_none()
            );
        }

        // Even authenticated payloads must contain exactly one session.
        let mut trailing = postcard::to_stdvec(&Session::new())?;
        trailing.push(0);
        let tag = SignedCookieStore::mac(&store.signing_key, &trailing)
            .finalize()
            .into_bytes();
        let mut bytes = tag.to_vec();
        bytes.extend_from_slice(&trailing);
        assert!(
            store
                .load_session(format!("{PREFIX}{}", BASE64_STANDARD.encode(bytes)))
                .await?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn requires_strong_keys_and_redacts_debug() -> Result {
        assert!(SignedCookieStore::new([]).is_err());
        assert!(SignedCookieStore::new([0; 31]).is_err());
        assert!(
            SignedCookieStore::new(KEY)?
                .with_fallback_key([0; 31])
                .is_err()
        );
        let store = SignedCookieStore::new(b"a-secret-key-that-must-never-be-logged")?;
        assert!(!format!("{store:?}").contains("secret"));
        Ok(())
    }
}

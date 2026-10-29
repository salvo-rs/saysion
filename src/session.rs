use std::{
    collections::HashMap,
    convert::TryFrom,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime as DateTime;

#[cfg(test)]
thread_local! {
    pub(crate) static SNAPSHOT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// The serialized representation stays a plain map. Revisions are request-local
// bookkeeping and share the map's lock so a snapshot cannot mix two revisions.
#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(transparent)]
struct SessionData {
    values: HashMap<String, String>,
    #[serde(skip)]
    revision: u64,
    #[serde(skip)]
    persisted_revision: u64,
}

#[derive(Debug)]
pub(crate) struct PersistenceAttempt {
    pub(crate) origin: Option<String>,
}

/// # The main session type.
///
/// ## Cloning and Serialization
///
/// The `cookie_value` field is not cloned or serialized, and it can
/// only be read through `into_cookie_value`. The intent of this field
/// is that it is set either by initialization or by a session store,
/// and read exactly once in order to set the cookie value.
///
/// ## Change tracking
///
/// Built-in stores acknowledge only the data snapshot they successfully saved.
/// Changes made through a request-local clone while a save awaits I/O remain
/// dirty. [`Session::reset_data_changed`] explicitly acknowledges the current
/// data and must not be used to acknowledge an older asynchronous save.
///
/// ### Change tracking example
/// ```rust
/// # use saysion::Session;
/// # #[tokio::main]
/// # async fn main() -> saysion::Result {
/// let mut session = Session::new();
/// assert!(!session.data_changed());
///
/// session.insert("key", 1)?;
/// assert!(session.data_changed());
///
/// session.reset_data_changed();
/// assert_eq!(session.get::<usize>("key").unwrap(), 1);
/// assert!(!session.data_changed());
///
/// session.insert("key", 2)?;
/// assert!(session.data_changed());
/// assert_eq!(session.get::<usize>("key").unwrap(), 2);
///
/// session.insert("key", 1)?;
/// assert!(session.data_changed(), "reverting the data still counts as a change");
///
/// session.reset_data_changed();
/// assert!(!session.data_changed());
/// session.remove("nonexistent key");
/// assert!(!session.data_changed());
/// session.remove("key");
/// assert!(session.data_changed());
/// # Ok(()) }
/// ```
#[derive(Serialize, Deserialize)]
pub struct Session {
    id: String,
    expiry: Option<DateTime>,
    data: Arc<RwLock<SessionData>>,

    #[serde(skip)]
    cookie_value: Option<String>,
    #[serde(skip)]
    metadata_changed: AtomicBool,
    #[serde(skip)]
    persisted_id: Arc<RwLock<Option<String>>>,
    #[serde(skip)]
    rotation_from: Option<String>,
    #[serde(skip)]
    fresh: bool,
    #[serde(skip)]
    creation_claimed: Arc<AtomicBool>,
    #[serde(skip)]
    destroy: Arc<AtomicBool>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid both secrets and data locks: formatting can happen while an
        // application holds a session lock or handles a poisoned lock.
        f.debug_struct("Session")
            .field("expiry", &self.expiry())
            .field("destroyed", &self.is_destroyed())
            .finish_non_exhaustive()
    }
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            cookie_value: None,
            id: self.id.clone(),
            data: self.data.clone(),
            expiry: self.expiry,
            destroy: self.destroy.clone(),
            metadata_changed: AtomicBool::new(self.metadata_changed.load(Ordering::Acquire)),
            persisted_id: self.persisted_id.clone(),
            rotation_from: self.rotation_from.clone(),
            fresh: self.fresh,
            creation_claimed: self.creation_claimed.clone(),
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// generates a random cookie value
fn generate_cookie(len: usize) -> String {
    let mut key = vec![0u8; len];
    rand::rng().fill_bytes(&mut key);
    BASE64_STANDARD.encode(key)
}

impl Session {
    /// An independent persistence snapshot; request-local clones remain shared.
    pub(crate) fn snapshot(&self) -> Self {
        #[cfg(test)]
        SNAPSHOT_CALLS.with(|calls| calls.set(calls.get() + 1));
        self.persistence_snapshot().0
    }

    /// Capture data and its revision under the same lock before starting I/O.
    pub(crate) fn persistence_snapshot(&self) -> (Self, u64) {
        let data = self.data.read().unwrap();
        let snapshot = Self {
            id: self.id.clone(),
            expiry: self.expiry,
            data: Arc::new(RwLock::new(SessionData {
                values: data.values.clone(),
                ..SessionData::default()
            })),
            cookie_value: None,
            metadata_changed: AtomicBool::new(false),
            persisted_id: Arc::new(RwLock::new(None)),
            rotation_from: None,
            fresh: false,
            creation_claimed: Arc::default(),
            destroy: Arc::new(AtomicBool::new(self.is_destroyed())),
        };
        (snapshot, data.revision)
    }

    /// Acknowledge only the data represented by a successfully saved snapshot.
    pub(crate) fn acknowledge_revision(&self, revision: u64) {
        self.data.write().unwrap().persisted_revision = revision;
        self.metadata_changed.store(false, Ordering::Release);
    }

    pub(crate) fn persistence_origin(&self) -> crate::Result<Option<String>> {
        let origin = self.persisted_id.read().unwrap().clone();
        if origin.is_none() && (!self.fresh || self.creation_claimed.load(Ordering::Acquire)) {
            return Err(crate::SessionConflict.into());
        }
        if let Some(id) = origin.as_deref()
            && id != self.id
            && self.rotation_from.as_deref() != Some(id)
        {
            return Err(crate::SessionConflict.into());
        }
        Ok(origin)
    }

    pub(crate) fn mark_persisted(&self) {
        let mut origin = self.persisted_id.write().unwrap();
        // Never let a late update completion roll back a rotated identity.
        if origin.is_none() || self.rotation_from.as_deref() == origin.as_deref() {
            *origin = Some(self.id.clone());
        }
    }

    pub(crate) fn persistence_attempt(&self) -> crate::Result<PersistenceAttempt> {
        if self.is_destroyed() {
            return Err(crate::SessionConflict.into());
        }
        let attempt = PersistenceAttempt {
            origin: self.persistence_origin()?,
        };
        if attempt.origin.is_none() {
            self.creation_claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| crate::SessionConflict)?;
            // Never return creation authority after starting a write: a failed
            // or cancelled network future may already have created the record.
            // Only a confirmed successful save publishes a persisted identity.
        }
        Ok(attempt)
    }

    pub(crate) fn mark_loaded(self) -> Self {
        self.mark_persisted();
        self
    }

    /// Create a new session. Generates a random id and matching
    /// cookie value. Does not set an expiry by default
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let session = Session::new();
    /// assert_eq!(None, session.expiry());
    /// assert!(session.into_cookie_value().is_some());
    /// # Ok(())
    /// # }
    /// ```
    pub fn new() -> Self {
        let cookie_value = generate_cookie(64);
        let id = Session::id_from_cookie_value(&cookie_value).unwrap();

        Self {
            expiry: None,
            data: Arc::new(RwLock::new(SessionData::default())),
            cookie_value: Some(cookie_value),
            metadata_changed: AtomicBool::new(false),
            persisted_id: Arc::new(RwLock::new(None)),
            rotation_from: None,
            fresh: true,
            creation_claimed: Arc::default(),
            id,
            destroy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// applies a cryptographic hash function on a cookie value
    /// returned by [`Session::into_cookie_value`] to obtain the
    /// session id for that cookie. Returns an error if the cookie
    /// format is not recognized
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let session = Session::new();
    /// let id = session.id().to_string();
    /// let cookie_value = session.into_cookie_value().unwrap();
    /// assert_eq!(id, Session::id_from_cookie_value(&cookie_value)?);
    /// # Ok(())
    /// # }
    /// ```
    pub fn id_from_cookie_value(string: &str) -> Result<String, base64::DecodeError> {
        let decoded = BASE64_STANDARD.decode(string)?;
        let hash = blake3::hash(&decoded);
        Ok(BASE64_STANDARD.encode(hash.as_bytes()))
    }

    /// mark this session for destruction. the actual session record
    /// is not destroyed until the end of this response cycle.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert!(!session.is_destroyed());
    /// session.destroy();
    /// assert!(session.is_destroyed());
    /// # Ok(()) }
    /// ```
    pub fn destroy(&mut self) {
        self.destroy.store(true, Ordering::SeqCst);
    }

    /// returns true if this session is marked for destruction
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert!(!session.is_destroyed());
    /// session.destroy();
    /// assert!(session.is_destroyed());
    /// # Ok(()) }
    /// ```
    pub fn is_destroyed(&self) -> bool {
        self.destroy.load(Ordering::SeqCst)
    }

    /// Gets the session id
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let session = Session::new();
    /// let id = session.id().to_owned();
    /// let cookie_value = session.into_cookie_value().unwrap();
    /// assert_eq!(id, Session::id_from_cookie_value(&cookie_value)?);
    /// # Ok(()) }
    /// ```
    pub fn id(&self) -> &str {
        &self.id
    }

    /// inserts a serializable value into the session hashmap. returns
    /// an error if the serialization was unsuccessful.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use serde::{Serialize, Deserialize};
    /// # use saysion::Session;
    /// #[derive(Serialize, Deserialize)]
    /// struct User {
    ///     name: String,
    ///     legs: u8
    /// }
    /// # fn main() {
    /// let mut session = Session::new();
    /// session.insert("user", User { name: "chashu".into(), legs: 4 }).expect("serializable");
    /// assert_eq!(r#"{"name":"chashu","legs":4}"#, session.get_raw("user").unwrap());
    /// # }
    /// ```
    pub fn insert(&mut self, key: &str, value: impl Serialize) -> Result<(), serde_json::Error> {
        self.insert_raw(key, serde_json::to_string(&value)?);
        Ok(())
    }

    /// inserts a string into the session hashmap
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # fn main() {
    /// let mut session = Session::new();
    /// session.insert_raw("ten", "10".to_string());
    /// let ten: usize = session.get("ten").unwrap();
    /// assert_eq!(ten, 10);
    /// # }
    /// ```
    pub fn insert_raw(&mut self, key: &str, value: String) {
        let mut data = self.data.write().unwrap();
        if data.values.get(key) != Some(&value) {
            data.values.insert(key.to_string(), value);
            data.revision = data.revision.wrapping_add(1);
        }
    }

    /// deserializes a type T out of the session hashmap
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// session.insert("key", vec![1, 2, 3])?;
    /// let numbers: Vec<usize> = session.get("key").unwrap();
    /// assert_eq!(vec![1, 2, 3], numbers);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        let data = self.data.read().unwrap();
        let string = data.values.get(key)?;
        serde_json::from_str(string).ok()
    }

    /// returns the String value contained in the session hashmap
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// session.insert("key", vec![1, 2, 3])?;
    /// assert_eq!("[1,2,3]", session.get_raw("key").unwrap());
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_raw(&self, key: &str) -> Option<String> {
        let data = self.data.read().unwrap();
        data.values.get(key).cloned()
    }

    /// removes an entry from the session hashmap
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// session.insert("key", "value")?;
    /// session.remove("key");
    /// assert!(session.get_raw("key").is_none());
    /// assert_eq!(session.len(), 0);
    /// # Ok(())
    /// # }
    /// ```
    pub fn remove(&mut self, key: &str) {
        let mut data = self.data.write().unwrap();
        if data.values.remove(key).is_some() {
            data.revision = data.revision.wrapping_add(1);
        }
    }

    /// returns the number of elements in the session hashmap
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert_eq!(session.len(), 0);
    /// session.insert("key", 0)?;
    /// assert_eq!(session.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn len(&self) -> usize {
        self.data.read().unwrap().values.len()
    }

    /// returns a boolean indicating whether there are zero elements in the session hashmap
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert!(session.is_empty());
    /// session.insert("key", 0)?;
    /// assert!(!session.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_empty(&self) -> bool {
        return self.data.read().unwrap().values.is_empty();
    }

    /// Generates a new id and cookie for this session
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// let old_id = session.id().to_string();
    /// session.regenerate();
    /// assert!(session.id() != &old_id);
    /// let new_id = session.id().to_string();
    /// let cookie_value = session.into_cookie_value().unwrap();
    /// assert_eq!(new_id, Session::id_from_cookie_value(&cookie_value)?);
    /// # Ok(())
    /// # }
    /// ```
    pub fn regenerate(&mut self) {
        let origin = self.persisted_id.read().unwrap().clone();
        // A clone that has already been superseded must not turn a stale
        // identity into a new rotation of another clone's saved identity.
        if origin.as_deref() == Some(self.id.as_str()) || self.rotation_from == origin {
            self.rotation_from = origin;
        }
        let cookie_value = generate_cookie(64);
        self.id = Session::id_from_cookie_value(&cookie_value).unwrap();
        self.cookie_value = Some(cookie_value);
        self.metadata_changed.store(true, Ordering::Release);
    }

    /// sets the cookie value that this session will use to serialize
    /// itself. this should only be called by cookie stores. any other
    /// uses of this method will result in the cookie not getting
    /// correctly deserialized on subsequent requests.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// session.set_cookie_value("hello".to_owned());
    /// let cookie_value = session.into_cookie_value().unwrap();
    /// assert_eq!(cookie_value, "hello".to_owned());
    /// # Ok(()) }
    /// ```
    pub fn set_cookie_value(&mut self, cookie_value: String) {
        if self.cookie_value.as_ref() != Some(&cookie_value) {
            self.cookie_value = Some(cookie_value);
            self.metadata_changed.store(true, Ordering::Release);
        }
    }

    /// returns the expiry timestamp of this session, if there is one
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert_eq!(None, session.expiry());
    /// session.expire_in(std::time::Duration::from_secs(1));
    /// assert!(session.expiry().is_some());
    /// # Ok(()) }
    /// ```
    pub fn expiry(&self) -> Option<&DateTime> {
        self.expiry.as_ref()
    }

    /// assigns an expiry timestamp to this session
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert_eq!(None, session.expiry());
    /// session.set_expiry(time::OffsetDateTime::now_utc());
    /// assert!(session.expiry().is_some());
    /// # Ok(()) }
    /// ```
    pub fn set_expiry(&mut self, expiry: DateTime) {
        if self.expiry != Some(expiry) {
            self.expiry = Some(expiry);
            self.metadata_changed.store(true, Ordering::Release);
        }
    }

    /// assigns the expiry timestamp to a duration from the current time.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert_eq!(None, session.expiry());
    /// session.expire_in(std::time::Duration::from_secs(1));
    /// assert!(session.expiry().is_some());
    /// # Ok(()) }
    /// ```
    pub fn expire_in(&mut self, ttl: std::time::Duration) {
        self.set_expiry(DateTime::now_utc() + ttl);
    }

    /// Returns true when the expiry timestamp is in the past, and false
    /// when no expiry is set.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # use std::time::Duration;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert_eq!(None, session.expiry());
    /// assert!(!session.is_expired());
    /// session.expire_in(Duration::from_secs(1));
    /// assert!(!session.is_expired());
    /// tokio::time::sleep(Duration::from_secs(2)).await;
    /// assert!(session.is_expired());
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_expired(&self) -> bool {
        match self.expiry {
            Some(expiry) => expiry < DateTime::now_utc(),
            None => false,
        }
    }

    /// Ensures that this session is not expired. Returns None if it is expired
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # use std::time::Duration;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let session = Session::new();
    /// let mut session = session.validate().unwrap();
    /// session.expire_in(Duration::from_secs(1));
    /// let session = session.validate().unwrap();
    /// tokio::time::sleep(Duration::from_secs(2)).await;
    /// assert_eq!(None, session.validate());
    /// # Ok(())
    /// # }
    /// ```
    pub fn validate(self) -> Option<Self> {
        if self.is_expired() { None } else { Some(self) }
    }

    /// Checks whether data, identity, expiry, or the outgoing cookie changed.
    ///
    /// Data changes are shared between request-local clones. Identity and expiry
    /// are copied when cloning, so metadata change tracking is independent for
    /// each clone. Saving or resetting one clone cannot clear another clone's
    /// metadata changes. A newly created, untouched session remains unchanged.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert!(!session.data_changed(), "new session is not changed");
    /// session.insert("key", 1)?;
    /// assert!(session.data_changed());
    ///
    /// session.reset_data_changed();
    /// assert!(!session.data_changed());
    /// session.remove("key");
    /// assert!(session.data_changed());
    /// # Ok(())
    /// # }
    /// ```
    pub fn data_changed(&self) -> bool {
        if self.metadata_changed.load(Ordering::Acquire) {
            return true;
        }
        let data = self.data.read().unwrap();
        data.revision != data.persisted_revision
    }

    /// Whether this session has changes to save or is marked for destruction.
    ///
    /// Middleware must perform deletion when [`Self::is_destroyed`] is true;
    /// otherwise it can save changed sessions. Untouched new sessions return
    /// false, preserving middleware that creates sessions only after use.
    pub fn needs_persist(&self) -> bool {
        self.data_changed() || self.is_destroyed()
    }

    /// Marks the current data and this clone's metadata as unchanged.
    /// Built-in stores instead acknowledge
    /// the exact snapshot they persisted, so edits made during I/O stay dirty.
    /// Custom stores must not call this after asynchronous I/O when clones may
    /// have modified the session in the meantime.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// assert!(!session.data_changed(), "new session is not changed");
    /// session.insert("key", 1)?;
    /// assert!(session.data_changed());
    ///
    /// session.reset_data_changed();
    /// assert!(!session.data_changed());
    /// session.remove("key");
    /// assert!(session.data_changed());
    /// # Ok(())
    /// # }
    /// ```
    pub fn reset_data_changed(&self) {
        let mut data = self.data.write().unwrap();
        data.persisted_revision = data.revision;
        self.metadata_changed.store(false, Ordering::Release);
    }

    /// Duration from now to the expiry time of this session
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # use std::time::Duration;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// session.expire_in(Duration::from_secs(123));
    /// let expires_in = session.expires_in().unwrap();
    /// assert!(123 - expires_in.as_secs() < 2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn expires_in(&self) -> Option<std::time::Duration> {
        let dur = self.expiry? - DateTime::now_utc();
        if dur.is_negative() {
            None
        } else {
            std::time::Duration::try_from(dur).ok()
        }
    }

    /// takes the cookie value and consume this session.
    /// this is generally only performed by the session store
    ///
    /// # Example
    ///
    /// ```rust
    /// # use saysion::Session;
    /// # #[tokio::main]
    /// # async fn main() -> saysion::Result {
    /// let mut session = Session::new();
    /// session.set_cookie_value("hello".to_owned());
    /// let cookie_value = session.into_cookie_value().unwrap();
    /// assert_eq!(cookie_value, "hello".to_owned());
    /// # Ok(()) }
    /// ```
    pub fn into_cookie_value(mut self) -> Option<String> {
        self.cookie_value.take()
    }
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        other.id == self.id
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    #[test]
    fn creation_authority_is_consumed_even_after_failure_or_cancellation() {
        let session = Session::new();
        let clone = session.clone();
        let _attempt = session.persistence_attempt().unwrap();
        assert!(clone.persistence_attempt().is_err());
        assert!(session.persistence_origin().is_err());
        assert!(clone.persistence_origin().is_err());
    }

    #[test]
    fn a_late_update_completion_cannot_roll_back_a_rotated_marker() {
        let mut session = Session::new().mark_loaded();
        let mut old = session.clone();
        session.regenerate();
        session.mark_persisted();
        old.mark_persisted();
        assert_eq!(
            session.persistence_origin().unwrap().as_deref(),
            Some(session.id())
        );
        assert!(old.persistence_origin().is_err());
        old.regenerate();
        assert!(old.persistence_origin().is_err());
    }
}

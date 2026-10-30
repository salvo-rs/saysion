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

const SERVER_COOKIE_BYTES: usize = 64;
const SERVER_COOKIE_ENCODED_LEN: usize = SERVER_COOKIE_BYTES.div_ceil(3) * 4;

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
    #[serde(skip)]
    active_saves: usize,
    #[serde(skip)]
    overlapping_saves: bool,
    #[serde(skip)]
    uncertain_save: bool,
}

/// One captured save's acknowledgement and cancellation bookkeeping.
///
/// Data and this state share a lock. Once saves overlap, callback order cannot
/// identify the last backend write, so the entire cohort stays dirty. A later
/// standalone confirmed save can acknowledge its snapshot normally.
pub(crate) struct SaveRevision {
    data: Option<Arc<RwLock<SessionData>>>,
    revision: u64,
}

impl std::fmt::Debug for SaveRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveRevision").finish_non_exhaustive()
    }
}

impl SaveRevision {
    fn capture(shared: &Arc<RwLock<SessionData>>, data: &mut SessionData) -> Self {
        if data.active_saves == 0 {
            data.overlapping_saves = false;
        } else {
            data.overlapping_saves = true;
            data.uncertain_save = true;
        }
        data.active_saves += 1;
        Self {
            data: Some(shared.clone()),
            revision: data.revision,
        }
    }

    fn acknowledge(mut self) {
        let shared = self.data.take().expect("save revision already completed");
        let mut data = shared.write().unwrap();
        if data.overlapping_saves {
            data.uncertain_save = true;
        } else {
            data.persisted_revision = self.revision;
            data.uncertain_save = false;
        }
        data.active_saves -= 1;
    }
}

impl Drop for SaveRevision {
    fn drop(&mut self) {
        if let Some(shared) = self.data.take() {
            // A failed/cancelled save may have reached the backend. Do not
            // acknowledge it, and never strand the cohort's active count.
            let mut data = shared.write().unwrap_or_else(|error| error.into_inner());
            data.uncertain_save = true;
            data.active_saves -= 1;
        }
    }
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
/// Overlapping or unconfirmed saves conservatively keep their shared data
/// dirty until a later standalone save succeeds (or the caller explicitly
/// resets change tracking).
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
        let data = self.data.read().unwrap();
        self.snapshot_values(data.values.clone())
    }

    fn snapshot_values(&self, values: HashMap<String, String>) -> Self {
        Self {
            id: self.id.clone(),
            expiry: self.expiry,
            data: Arc::new(RwLock::new(SessionData {
                values,
                ..SessionData::default()
            })),
            cookie_value: None,
            metadata_changed: AtomicBool::new(false),
            persisted_id: Arc::new(RwLock::new(None)),
            rotation_from: None,
            fresh: false,
            creation_claimed: Arc::default(),
            destroy: Arc::new(AtomicBool::new(self.is_destroyed())),
        }
    }

    /// Capture data and a tracked revision before starting I/O.
    pub(crate) fn persistence_snapshot(&self) -> (Self, SaveRevision) {
        let mut data = self.data.write().unwrap();
        let snapshot = self.snapshot_values(data.values.clone());
        let revision = SaveRevision::capture(&self.data, &mut data);
        (snapshot, revision)
    }

    /// Encode one bounded data snapshot together with its revision. Borrowing
    /// the values under their lock avoids cloning an oversized map before the
    /// serializer can reject it. The wire fields match Session's representation.
    pub(crate) fn serialize_bounded<'buffer>(
        &self,
        buffer: &'buffer mut [u8],
    ) -> Result<(&'buffer mut [u8], SaveRevision), postcard::Error> {
        #[derive(Serialize)]
        struct WireSession<'a> {
            id: &'a str,
            expiry: Option<DateTime>,
            data: &'a HashMap<String, String>,
        }

        let mut data = self.data.write().unwrap();
        let wire = WireSession {
            id: &self.id,
            expiry: self.expiry,
            data: &data.values,
        };
        let payload = postcard::to_slice(&wire, buffer)?;
        let revision = SaveRevision::capture(&self.data, &mut data);
        Ok((payload, revision))
    }

    /// Acknowledge only the data represented by a successfully saved snapshot.
    pub(crate) fn acknowledge_revision(&self, revision: SaveRevision) {
        revision.acknowledge();
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

    /// Prepare an immutable snapshot for a server-side store write.
    ///
    /// The returned [`crate::SessionSave`] owns this session and captures its
    /// data before I/O. A fresh session's creation authority is consumed here:
    /// dropping the save after a failed or cancelled write does not restore it,
    /// since the backend may already have committed the record.
    ///
    /// Loaded sessions require a conditional update. Rotations require removing
    /// the source and inserting the replacement atomically when supported by
    /// the backend. See [`crate::SessionSave::origin`].
    pub fn prepare_save(self) -> crate::Result<crate::SessionSave> {
        crate::SessionSave::new(self)
    }

    pub(crate) fn immutable_snapshot(&self) -> (crate::SessionSnapshot, SaveRevision) {
        let mut data = self.data.write().unwrap();
        let snapshot =
            crate::SessionSnapshot::new(self.id.clone(), self.expiry, data.values.clone());
        let revision = SaveRevision::capture(&self.data, &mut data);
        (snapshot, revision)
    }

    /// Mark this session for destruction and return its persisted deletion ID.
    ///
    /// A pending, unsaved rotation still deletes its original persisted source.
    /// A superseded clone targets only its own old ID, never a newer clone's
    /// replacement. Callers must delete this ID in the backend and remove the
    /// browser cookie. This method does not perform storage I/O or revoke every
    /// descendant of an already completed rotation.
    pub fn prepare_delete(&mut self) -> String {
        let target = self
            .persistence_origin()
            .ok()
            .flatten()
            .unwrap_or_else(|| self.id.clone());
        self.destroy();
        target
    }

    /// Validate a record obtained by a trusted server-side store.
    ///
    /// Call after deserializing a record fetched using `cookie_value`. This
    /// verifies that the record's ID matches the lookup cookie, rejects expired
    /// records, and initializes isolated persistence bookkeeping. It does not
    /// authenticate arbitrary client-supplied serialized data. Client-side
    /// cookie stores must authenticate their own complete cookie format.
    ///
    /// A malformed cookie or mismatched record returns an error. Expired records
    /// return `Ok(None)`. Loading never shares mutable data or lifecycle state
    /// with another request-local clone of the input.
    pub fn validate_loaded(mut self, cookie_value: &str) -> crate::Result<Option<Self>> {
        let expected = Self::id_from_cookie_value(cookie_value)?;
        anyhow::ensure!(
            self.id == expected,
            "stored session ID does not match cookie"
        );
        if self.is_expired() {
            return Ok(None);
        }
        if self.is_destroyed() {
            return Err(crate::SessionConflict.into());
        }
        if Arc::strong_count(&self.data) > 1 {
            let values = self.data.read().unwrap().values.clone();
            self.data = Arc::new(RwLock::new(SessionData {
                values,
                ..SessionData::default()
            }));
        } else {
            let mut data = self.data.write().unwrap();
            data.persisted_revision = data.revision;
            data.uncertain_save = false;
            data.overlapping_saves = false;
        }
        self.cookie_value = None;
        self.metadata_changed = AtomicBool::new(false);
        self.persisted_id = Arc::new(RwLock::new(Some(self.id.clone())));
        self.rotation_from = None;
        self.fresh = false;
        self.creation_claimed = Arc::default();
        self.destroy = Arc::default();
        Ok(Some(self))
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
        let cookie_value = generate_cookie(SERVER_COOKIE_BYTES);
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
    /// format is not recognized. Standard server-side cookies must encode
    /// exactly 64 bytes as 88 characters of canonical padded Base64. Other
    /// lengths are rejected before decoding, using a fixed-size stack buffer.
    /// Client-side [`crate::CookieStore`] and [`crate::SignedCookieStore`] use
    /// separate, variable-length formats and do not use this parser.
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
        if string.len() != SERVER_COOKIE_ENCODED_LEN {
            return Err(base64::DecodeError::InvalidLength(string.len()));
        }
        let mut decoded = [0; SERVER_COOKIE_BYTES];
        let decoded_len = BASE64_STANDARD
            .decode_slice(string, &mut decoded)
            .map_err(|error| match error {
                base64::DecodeSliceError::DecodeError(error) => error,
                base64::DecodeSliceError::OutputSliceTooSmall => {
                    base64::DecodeError::InvalidLength(string.len())
                }
            })?;
        if decoded_len != SERVER_COOKIE_BYTES {
            return Err(base64::DecodeError::InvalidLength(string.len()));
        }
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
        // User-defined Deserialize implementations may reenter this session.
        // Keep only the string copy inside the critical section.
        let string = {
            let data = self.data.read().unwrap();
            data.values.get(key)?.clone()
        };
        serde_json::from_str(&string).ok()
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
        let cookie_value = generate_cookie(SERVER_COOKIE_BYTES);
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
        data.uncertain_save || data.revision != data.persisted_revision
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
    /// This explicit override also clears current persistence uncertainty.
    /// Completing or dropping an overlapping save afterward can mark the data
    /// dirty again; use a standalone confirmed save for normal recovery.
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
        data.uncertain_save = false;
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
mod read_tests {
    use std::cell::RefCell;

    use serde::{Deserializer, de::Error as _};

    use super::*;

    thread_local! {
        static REENTRANT_SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    }

    struct ReentrantValue(u64);

    impl<'de> Deserialize<'de> for ReentrantValue {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let value = u64::deserialize(deserializer)?;
            REENTRANT_SESSION.with(|slot| {
                let mut slot = slot.borrow_mut();
                let session = slot
                    .as_mut()
                    .ok_or_else(|| D::Error::custom("missing reentrant session"))?;
                // Fail immediately if get retained the read lock. Calling
                // insert directly in the old implementation would hang, so
                // this probe makes the regression deterministic and bounded.
                let probe = session
                    .data
                    .try_write()
                    .map_err(|_| D::Error::custom("get retained its data lock"))?;
                drop(probe);
                session
                    .insert("written_during_deserialization", true)
                    .map_err(D::Error::custom)
            })?;
            Ok(Self(value))
        }
    }

    #[test]
    fn deserialize_can_reenter_the_same_session_for_a_write() -> crate::Result {
        let mut session = Session::new();
        session.insert("value", 42_u64)?;
        session.reset_data_changed();
        REENTRANT_SESSION.with(|slot| *slot.borrow_mut() = Some(session.clone()));
        let loaded = session.get::<ReentrantValue>("value");
        REENTRANT_SESSION.with(|slot| *slot.borrow_mut() = None);

        assert_eq!(loaded.map(|value| value.0), Some(42));
        assert_eq!(
            session.get::<bool>("written_during_deserialization"),
            Some(true)
        );
        assert!(session.data_changed());
        Ok(())
    }

    #[test]
    fn ordinary_and_failed_reads_preserve_values_and_change_tracking() -> crate::Result {
        let mut session = Session::new();
        session.insert("numbers", vec![1_u64, 2, 3])?;
        session.insert_raw("malformed", "{".to_string());
        session.reset_data_changed();

        assert_eq!(session.get::<Vec<u64>>("numbers"), Some(vec![1, 2, 3]));
        assert_eq!(session.get::<u64>("numbers"), None);
        assert_eq!(session.get::<serde_json::Value>("malformed"), None);
        assert_eq!(session.get::<u64>("missing"), None);
        assert_eq!(session.get_raw("numbers").as_deref(), Some("[1,2,3]"));
        assert_eq!(session.get_raw("malformed").as_deref(), Some("{"));
        assert!(!session.data_changed());
        Ok(())
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    #[test]
    fn native_and_bounded_capture_paths_share_overlap_tracking() {
        let mut session = Session::new();
        session.insert("value", 1_u32).unwrap();
        session.reset_data_changed();
        let (_, native) = session.persistence_snapshot();
        let mut buffer = [0; 1_024];
        let (_, bounded) = session.serialize_bounded(&mut buffer).unwrap();
        session.acknowledge_revision(native);
        session.acknowledge_revision(bounded);
        assert!(session.data_changed());
        let passive = session.snapshot();
        assert!(!passive.data_changed());
        let (_, repair) = session.persistence_snapshot();
        session.acknowledge_revision(repair);
        assert!(!session.data_changed());
        drop(passive);
        assert!(!session.data_changed());
    }

    #[test]
    fn bounded_failure_before_capture_does_not_create_a_pending_save() {
        let mut session = Session::new();
        session.insert("value", 1_u32).unwrap();
        session.reset_data_changed();
        assert!(session.serialize_bounded(&mut [0; 1]).is_err());
        assert!(!session.data_changed());
        let (_, repair) = session.persistence_snapshot();
        session.acknowledge_revision(repair);
        assert!(!session.data_changed());
    }

    #[test]
    fn bounded_serialization_captures_the_wire_data_and_revision_together() {
        let mut session = Session::new();
        session.insert("value", 1_u64).unwrap();
        let expected = postcard::to_stdvec(&session).unwrap();
        let mut buffer = [0; 4096];
        let (payload, revision) = session.serialize_bounded(&mut buffer).unwrap();
        assert_eq!(payload, expected);
        let saved: Session = postcard::from_bytes(payload).unwrap();

        let mut editor = session.clone();
        editor.insert("value", 2_u64).unwrap();
        session.acknowledge_revision(revision);
        assert!(editor.data_changed());
        assert_eq!(saved.get::<u64>("value"), Some(1));
        assert_eq!(editor.get::<u64>("value"), Some(2));
    }

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

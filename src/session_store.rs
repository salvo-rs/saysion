use crate::{Result, Session, async_trait};

/// A persisted session was removed, replaced, or concurrently rotated.
///
/// Discard the stale session rather than retrying it as a newly created one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConflict;

impl std::fmt::Display for SessionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session was removed or concurrently replaced")
    }
}

impl std::error::Error for SessionConflict {}

/// An async session backend.
///
/// External server-side stores can use [`Session::validate_loaded`] after a
/// trusted lookup and [`Session::prepare_save`] to capture data and persistence
/// state. [`crate::SessionSave`] supplies the source ID and acknowledges only a
/// confirmed saved snapshot. Use [`crate::SessionSave::begin_write`] and
/// [`Session::begin_delete`] symmetrically around asynchronous backend I/O.
/// Deletion identifies the source to revoke, including an unsaved rotation.
/// Backend existence and collision
/// checks must remain atomic; see [`crate::SessionSave::origin`].
#[async_trait]
pub trait SessionStore {
    /// Get a session from the storage backend.
    ///
    /// The input is expected to be the value of an identifying
    /// cookie. This will then be parsed by the session middleware
    /// into a session if possible
    async fn load_session(&self, cookie_value: String) -> Result<Option<Session>>;

    /// Store a session on the storage backend.
    ///
    /// The return value is the value of the cookie to store for the
    /// user that represents this session
    ///
    /// Built-in server-side stores create records only for fresh sessions and
    /// update existing records for loaded sessions. A removed or superseded
    /// record returns [`SessionConflict`]; discard it instead of recreating its
    /// authentication data. Client-side stores have no revocation state.
    async fn store_session(&self, session: Session) -> Result<Option<String>>;

    /// Remove a session from the session store
    async fn destroy_session(&self, session: Session) -> Result;

    /// Empties the entire store, destroying all sessions
    async fn clear_store(&self) -> Result;
}

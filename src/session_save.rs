use crate::{Result, Session, SessionConflict};

/// The immutable serialized representation captured for a server-side save.
///
/// Its JSON and postcard fields match [`Session`], but it has no mutation or
/// lifecycle methods. Cloning it cannot modify the pending save's contents.
#[derive(Clone, serde::Serialize)]
pub struct SessionSnapshot {
    id: String,
    expiry: Option<time::OffsetDateTime>,
    data: std::collections::HashMap<String, String>,
}

impl std::fmt::Debug for SessionSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSnapshot")
            .field("expiry", &self.expiry)
            .finish_non_exhaustive()
    }
}

impl SessionSnapshot {
    pub(crate) fn new(
        id: String,
        expiry: Option<time::OffsetDateTime>,
        data: std::collections::HashMap<String, String>,
    ) -> Self {
        Self { id, expiry, data }
    }

    /// The target ID to use for this snapshot's backend record.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The captured expiry to store alongside the serialized data.
    pub fn expiry(&self) -> Option<&time::OffsetDateTime> {
        self.expiry.as_ref()
    }

    /// Read a captured raw JSON value without acquiring a session lock.
    pub fn get_raw(&self, key: &str) -> Option<&str> {
        self.data.get(key).map(String::as_str)
    }
}

/// An immutable server-side save snapshot and its opaque completion token.
///
/// Obtain this with [`Session::prepare_save`], serialize [`Self::session`] before
/// I/O, acquire [`Self::begin_write`] before asynchronous backend work, and call
/// [`Self::complete_with_guard`] only after the backend confirms success.
/// Completion acknowledges only the captured data, preserving later clone edits.
/// This type cannot be cloned, and its revision cannot be changed by a store.
///
/// Dropping it never refunds first-insert authority: a cancelled or failed write
/// may already have reached storage. Load the actual record through its cookie
/// or authenticate again and create a new session when the outcome is uncertain.
#[must_use = "complete a confirmed save, or drop it without acknowledging changes"]
pub struct SessionSave {
    source: Session,
    snapshot: SessionSnapshot,
    revision: u64,
    origin: Option<String>,
    write_receipt: std::sync::Arc<()>,
    write_started: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for SessionSave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSave")
            .field("session", &self.snapshot)
            .finish_non_exhaustive()
    }
}

impl SessionSave {
    pub(crate) fn new(source: Session) -> Result<Self> {
        let attempt = source.persistence_attempt()?;
        let (snapshot, revision) = source.immutable_snapshot();
        Ok(Self {
            source,
            snapshot,
            revision,
            origin: attempt.origin,
            write_receipt: std::sync::Arc::new(()),
            write_started: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The captured session to serialize, including its target ID and expiry.
    ///
    /// Unlike a request-local Session clone, this snapshot has no mutation API:
    ///
    /// ```compile_fail
    /// let save = saysion::Session::new().prepare_save().unwrap();
    /// let mut snapshot = save.session().clone();
    /// snapshot.insert_raw("key", "changed".to_owned());
    /// ```
    pub fn session(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    /// The persisted source ID, or `None` for a fresh insertion.
    ///
    /// If this equals [`Self::session`]'s ID, update an existing record only;
    /// return [`SessionConflict`] when it no longer exists. A different source
    /// means rotation: remove that source and insert the target without
    /// overwriting a collision. Use one transaction or equivalent atomic
    /// operation when possible. With `None`, insert without overwriting any
    /// existing record. Never use an unconditional upsert for loaded sessions.
    pub fn origin(&self) -> Option<&str> {
        self.origin.as_deref()
    }

    /// Acquire same-lineage write coordination before starting backend I/O.
    ///
    /// Hold this owned asynchronous guard through the write, then pass it to
    /// [`Self::complete_with_guard`]. Deletion must symmetrically use
    /// [`Session::begin_delete`]. A save token permits only one guarded attempt.
    /// Dropping a creation/rotation guard without confirmation conservatively
    /// makes subsequent coordinated deletion fail, since a network write may
    /// still complete. No synchronous mutex is held across an await.
    pub async fn begin_write(&self) -> Result<crate::SessionWriteGuard> {
        self.write_started
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map_err(|_| SessionConflict)?;
        self.source
            .begin_guarded_write(&self.origin, Some(self.write_receipt.clone()), true)
            .await
    }

    /// Complete a successful guarded write, consuming its matching guard.
    ///
    /// Destruction requested during I/O prevents acknowledgment and cookie
    /// issuance. If [`crate::SessionWriteGuard::is_destroyed`] is true after the
    /// write, remove the written target before calling this method, to avoid a
    /// leftover record when no deletion operation was separately scheduled.
    pub fn complete_with_guard(
        self,
        mut guard: crate::SessionWriteGuard,
    ) -> Result<Option<String>> {
        if !guard.matches(&self.write_receipt) {
            return Err(SessionConflict.into());
        }
        guard.finish()?;
        self.source.mark_persisted();
        self.source.acknowledge_revision(self.revision);
        Ok(self.source.into_cookie_value())
    }

    /// Recheck request-local destruction and rotation before writing.
    ///
    /// In a lock-based store, call this inside the lock used for the write.
    /// Backend existence and collision checks described by [`Self::origin`]
    /// must still be atomic: this check alone cannot detect independent requests
    /// deleting records in another process or a different loaded session.
    pub fn validate_current(&self) -> Result {
        if self.source.is_destroyed() {
            return Err(SessionConflict.into());
        }
        if self.origin.is_some() && self.source.persistence_origin()? != self.origin {
            return Err(SessionConflict.into());
        }
        Ok(())
    }

    /// Acknowledge a confirmed backend write and return the outgoing cookie.
    ///
    /// Call only after a successful insertion, conditional update, or committed
    /// rotation. This compatibility method is for stores that supply their own
    /// write/deletion coordination. Asynchronous stores should instead use
    /// [`Self::begin_write`] and [`Self::complete_with_guard`]. A destroyed
    /// session, or a token that began the guarded protocol, is not acknowledged
    /// and returns no cookie here. On an error or cancellation, drop the save instead. Later data
    /// edits through a clone remain dirty. Completing an older update cannot
    /// roll back a newer request-local identity marker.
    pub fn complete(self) -> Option<String> {
        if self.source.is_destroyed()
            || self
                .write_started
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        self.source.mark_persisted();
        self.source.acknowledge_revision(self.revision);
        self.source.into_cookie_value()
    }
}

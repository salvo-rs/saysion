use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_lock::{Mutex, MutexGuardArc};

use crate::{Result, SessionConflict};

#[derive(Default)]
struct WriteState {
    uncertain: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Lifecycle {
    state: Arc<Mutex<WriteState>>,
}

impl Lifecycle {
    pub(crate) async fn write(
        &self,
        destroyed: Arc<AtomicBool>,
        receipt: Option<Arc<()>>,
        uncertain_on_drop: bool,
    ) -> Result<SessionWriteGuard> {
        let state = self.state.lock_arc().await;
        if state.uncertain {
            return Err(uncertain_write());
        }
        if destroyed.load(Ordering::Acquire) {
            return Err(SessionConflict.into());
        }
        Ok(SessionWriteGuard {
            state,
            destroyed,
            receipt,
            uncertain_on_drop,
            settled: false,
        })
    }

    pub(crate) async fn delete(&self) -> Result<SessionDeleteGuard> {
        let state = self.state.lock_arc().await;
        if state.uncertain {
            return Err(uncertain_write());
        }
        Ok(SessionDeleteGuard {
            _state: state,
            id: String::new(),
        })
    }
}

fn uncertain_write() -> crate::Error {
    crate::Error::new(SessionConflict).context(
        "a previous session creation or rotation has an uncertain outcome; reload confirmed backend state before retrying",
    )
}

/// An asynchronous same-lineage write guard, held across backend I/O.
///
/// Obtain this with [`crate::SessionSave::begin_write`] and pass it to
/// [`crate::SessionSave::complete_with_guard`] after confirmed success. Keep it
/// alive through every backend operation, including compensating deletion.
/// This guard holds no synchronous lock across an await.
///
/// Dropping an unconfirmed creation or rotation conservatively records an
/// uncertain outcome, even if no I/O was actually dispatched. A later coordinated
/// deletion of this lineage returns an error rather than claiming to have
/// revoked a write that may still finish. Reload actual backend state or start
/// a new session after authentication. Creation authority is never refunded.
#[must_use = "hold the write guard through backend I/O and confirmed completion"]
pub struct SessionWriteGuard {
    state: MutexGuardArc<WriteState>,
    destroyed: Arc<AtomicBool>,
    receipt: Option<Arc<()>>,
    uncertain_on_drop: bool,
    settled: bool,
}

impl SessionWriteGuard {
    /// Whether a same-lineage handle requested destruction during this write.
    ///
    /// After a successful write, stores should remove the written target when
    /// this is true, before completing the guarded save. Do not issue a cookie
    /// for that target. Coordinated deletion waits until this guard is released.
    pub fn is_destroyed(&self) -> bool {
        self.destroyed.load(Ordering::Acquire)
    }

    pub(crate) fn matches(&self, receipt: &Arc<()>) -> bool {
        self.receipt
            .as_ref()
            .is_some_and(|own| Arc::ptr_eq(own, receipt))
    }

    pub(crate) fn finish(&mut self) -> Result {
        // The backend has confirmed its outcome. Even if destruction won,
        // deletion can now proceed without racing an unconfirmed insert.
        self.settled = true;
        if self.is_destroyed() {
            Err(SessionConflict.into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn require_cleanup(&mut self) {
        self.settled = false;
    }

    pub(crate) fn confirm_removed(&mut self) {
        self.settled = true;
    }
}

impl Drop for SessionWriteGuard {
    fn drop(&mut self) {
        if !self.settled && self.uncertain_on_drop {
            self.state.uncertain = true;
        }
    }
}

impl std::fmt::Debug for SessionWriteGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionWriteGuard").finish_non_exhaustive()
    }
}

/// A same-lineage deletion guard held until backend deletion has completed.
///
/// Obtain this with [`crate::Session::begin_delete`], delete [`Self::id`] in the
/// backend, then drop this guard. Awaiting it waits for earlier guarded writes.
/// Cancellation of a deletion does not permit future saves: destruction has
/// already been marked. An uncertain earlier creation or rotation returns an
/// error from `begin_delete` instead of returning this guard.
#[must_use = "keep the deletion guard alive until backend deletion completes"]
pub struct SessionDeleteGuard {
    _state: MutexGuardArc<WriteState>,
    pub(crate) id: String,
}

impl SessionDeleteGuard {
    /// The persisted record to delete, including an unsaved rotation's source.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl std::fmt::Debug for SessionDeleteGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionDeleteGuard").finish_non_exhaustive()
    }
}

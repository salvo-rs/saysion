# Implementing a server-side session store

External crates can use the same lifecycle and snapshot protocol as built-in
stores. The implementation in [tests/custom_store.rs](../tests/custom_store.rs)
compiles as an external crate and uses only public APIs.

## Trusted loading

After fetching a record by its identifying cookie, deserialize the record and
call `session.validate_loaded(&cookie)?`. Expired records return None; mismatched
IDs return an error. This establishes isolated request-local persistence state.
It does not authenticate arbitrary client-supplied JSON. Client-side cookie
stores must authenticate their own complete format.

## Saving

1. Obtain an owned token with `session.prepare_save()?`.
2. Acquire `let write = save.begin_write().await?` **before backend I/O** and
   retain this asynchronous guard until completion. Deletion must use the
   matching `begin_delete` protocol. No synchronous mutex is held across awaits.
3. Serialize `save.session()`, a read-only `SessionSnapshot` that captures data
   and revision before I/O. Cloning it cannot mutate the pending snapshot.
4. Check `save.origin()`. None permits only insertion into an absent target.
   An origin equal to the target ID requires an existing-record update. A
   different origin requires removing the source and inserting the target
   without overwriting a collision, atomically when the backend supports it.
5. Call `save.validate_current()?` inside the write lock or before the write.
   Backend existence and collision checks must still be atomic.
6. After durable success, if `write.is_destroyed()` is true, remove the written
   target, call `save.confirm_removed(write)?`, and return a conflict. This
   explicit confirmation releases waiting deletion without acknowledging data
   or issuing a cookie. Never confirm removal before its I/O has succeeded.
7. Return `save.complete_with_guard(write)`. Only the captured revision is
   acknowledged; later edits remain dirty. Destruction requested during the
   write returns an error and never issues a new cookie. Coordinated deletion
   waits for the write/compensation to finish before reporting success. If
   destruction arrives between the last pre-check and completion, an unremoved
   creation/rotation stays uncertain: the waiting delete also returns an error,
   rather than reporting that a residual target was revoked.

Save handles from one request-local clone family may overlap without blocking
preparation. Since callback order need not match backend commit order, an
overlapping cohort remains dirty even after all its successful completions.
Dropping an unconfirmed handle releases its tracking slot but also keeps the
family dirty. Once those handles have finished, a standalone confirmed save
can acknowledge its snapshot normally. Snapshot clones alone do not keep a
save active. This tracks local acknowledgement uncertainty; it does not fence
cancelled operations that a remote backend may still execute. Follow the
backend's transaction/cancellation guarantees when confirming actual state.

Missing sources, removed records and collisions return SessionConflict. Never
replace those checks with an unconditional upsert. Drop the token on error or
cancellation. First-insert authority is consumed when preparation starts and is
never refunded, because the write may already have committed. Load actual state
through its cookie, or authenticate again and create a new Session when the
outcome is uncertain. Document availability and revocation tradeoffs if atomic
rotation is unavailable, as for standalone MongoDB.

A creation or rotation guard dropped without confirmation makes that lineage's
write outcome uncertain. Even cancellation before dispatch is treated
conservatively, because the protocol cannot observe a backend's dispatch point.
Subsequent `begin_delete` returns an error instead of falsely confirming
revocation while a network write could still finish. This sacrifices retry
availability for a truthful completion result. Reload actual, confirmed backend
state using the cookie or authenticate again. A normal existing-record update
cannot recreate a deleted row and does not require this creation uncertainty
fence. None of this coordinates independently loaded requests across processes.

Native asynchronous stores use the same conservative rule for a failed guarded
creation or rotation, including reported collisions or a transaction later
rolled back. They do not currently classify every backend error as proof that
no write can remain in flight. A same-lineage logout may therefore return an
error after a known rejection; reload trusted backend state rather than
weakening the uncertainty fence. Pure in-memory writes cannot outlive their
cancelled future and do not need that additional uncertainty flag.

## Deletion

Use `let deletion = session.begin_delete().await?`, delete `deletion.id()` in the
backend, and retain the guard until the delete has completed. Destruction is
marked before waiting for any same-lineage write. It also handles an unsaved
regeneration or an already marked session. Remove the browser cookie only after
successful deletion. A superseded clone targets
only its own old identity. Revoking a whole family after a completed rotation
requires application-level generations or session-family tracking.

The synchronous `prepare_delete()` helper only selects an ID and marks
destruction. It does not wait for pending writes. The old `save.complete()`
helper assumes the store supplies its own equivalent coordination; asynchronous
implementations must not combine these unguarded helpers and claim coordinated
deletion. It withholds cookies for destroyed sessions but cannot undo backend
writes on its own.

## Compatibility and regression tests

Persistence state is not serialized; existing JSON and postcard formats remain
unchanged. SessionSave hides its revision, cannot be cloned, consumes itself on
completion, and redacts identifiers and values from Debug.

Test stale saves after rotation and deletion, atomic replacement rollback,
collisions, edits during I/O, first-write cancellation, expired or mismatched
loads, and deletion of an unsaved rotation. Client-side stores cannot provide
immediate server-side revocation merely by implementing SessionStore.

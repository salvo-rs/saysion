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
2. Serialize `save.session()`, a read-only `SessionSnapshot` that captures data
   and revision before I/O. Cloning it cannot mutate the pending snapshot.
3. Check `save.origin()`. None permits only insertion into an absent target.
   An origin equal to the target ID requires an existing-record update. A
   different origin requires removing the source and inserting the target
   without overwriting a collision, atomically when the backend supports it.
4. Call `save.validate_current()?` inside the write lock or before the write.
   Backend existence and collision checks must still be atomic.
5. After durable success, return `save.complete()`. Only the captured revision
   is acknowledged; later edits remain dirty.

Missing sources, removed records and collisions return SessionConflict. Never
replace those checks with an unconditional upsert. Drop the token on error or
cancellation. First-insert authority is consumed when preparation starts and is
never refunded, because the write may already have committed. Load actual state
through its cookie, or authenticate again and create a new Session when the
outcome is uncertain. Document availability and revocation tradeoffs if atomic
rotation is unavailable, as for standalone MongoDB.

## Deletion

`session.prepare_delete()` marks destruction and returns the persisted source ID
to delete. It also handles an unsaved regeneration or an already marked session.
Delete the returned ID and remove the browser cookie. A superseded clone targets
only its own old identity. Revoking a whole family after a completed rotation
requires application-level generations or session-family tracking.

## Compatibility and regression tests

Persistence state is not serialized; existing JSON and postcard formats remain
unchanged. SessionSave hides its revision, cannot be cloned, consumes itself on
completion, and redacts identifiers and values from Debug.

Test stale saves after rotation and deletion, atomic replacement rollback,
collisions, edits during I/O, first-write cancellation, expired or mismatched
loads, and deletion of an unsaved rotation. Client-side stores cannot provide
immediate server-side revocation merely by implementing SessionStore.

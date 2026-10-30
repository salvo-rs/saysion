# Session identity and persistence

Server-side stores distinguish sessions created with `Session::new()` from
sessions returned by `load_session()`. Persistence bookkeeping is not part of
the serialized format. Deserializing a Session does not grant the authority to
create a server-side record; use the store's load API for an existing session.

After `regenerate()`, saving a loaded session replaces its original persisted
ID. Repeated regeneration before a save still revokes the original ID. The old
cookie cannot load the replacement, and request-local clones cannot save a
superseded identity as if it were current. Handle `SessionConflict` by
discarding the stale session, not by recreating its authentication data.

MemoryStore performs replacement under one write lock. SQL stores use a
transaction, preserving the original record if insertion of the replacement
fails. Redis uses a two-key Lua script: deployments need EVAL permission, and
both keys must occupy the same hash slot if a cluster-aware client is added in
the future. The current ConnectionManager client targets standalone Redis.

MongoDB also supports standalone servers, which have no multi-document
transactions. Rotation deletes the original first and then inserts the new
record. An insertion failure leaves the original credential revoked; discard
that session and start a fresh authentication flow rather than retrying the
old identity. There is an availability gap between the two writes.

These guarantees concern a particular persisted ID. Revoking every descendant
of an already completed rotation, or all sessions belonging to an account,
requires application-level account/session generations or a stable session
family. Client-side cookie stores have no server-side revocation state.

Loaded server-side sessions are updated only while their record still exists.
Saving an independent request after logout or clear returns `SessionConflict`
and cannot recreate that ID. Only a fresh `Session::new()` can create a record;
request-local clones share the knowledge that their first save has completed.
Concurrent first saves of the same fresh session are rejected. Creation authority
is consumed when the first save starts: a failed or cancelled network future
may already have written the record. If the outcome is uncertain, load through
the store using its cookie, or create a fresh session after authentication;
never retry the old object as a fresh insertion. A confirmed successful save
allows its clones to perform conditional updates.
Deleting a session after an unsaved regeneration targets its persisted source.

Same-lineage asynchronous writes and deletion now hold an asynchronous lifecycle
guard across backend work. Deletion marks destruction before waiting. A pending
first write therefore cannot issue a new usable cookie after coordinated
deletion has completed. Native stores remove a newly written target when
destruction wins during I/O. MemoryStore uses the same lock order as remote
stores: lifecycle guard first, store lock second.

An interrupted creation or rotation may have reached the backend. The shared
lifecycle records this uncertain outcome, and later coordinated deletion of
that lineage returns an error rather than reporting a guarantee it cannot
provide. Reload confirmed backend state to resolve it. The public protocol uses
`SessionSave::begin_write` / `complete_with_guard` and `Session::begin_delete`;
synchronous ID selection alone does not provide this coordination. No
synchronous lock is held across backend awaits.

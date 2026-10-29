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

Saving a loaded session updates only an existing record. If another request has
already rotated or removed that identity, the stale save returns
`SessionConflict` instead of recreating the old ID. Discard the stale session;
do not copy its authentication data into a fresh session to retry. Only fresh
sessions use insertion, and insertion cannot overwrite a colliding identity.
Deleting a session after an unsaved regeneration targets its persisted source.

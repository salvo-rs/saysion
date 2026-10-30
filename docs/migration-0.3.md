# Migrating from 0.2 to 0.3

**0.3.0 is unreleased.** These notes describe Git `main`, rather than the already
published 0.2.0 package. Install the development source with Rust **1.95 or newer**:

```toml
[dependencies]
saysion = { git = "https://github.com/salvo-rs/saysion", branch = "main" }
```

Add the needed backend feature, such as `features = ["redis"]`. Commit your
application's `Cargo.lock` and update the resolved Git revision deliberately.
The crates.io dependency `saysion = "0.2"` does not provide `SignedCookieStore`,
`SessionConflict`, or these persistence guarantees. The development manifest
uses 0.3.0 to distinguish it from the immutable published 0.2.0 version.

## Salvo middleware compatibility

Check the middleware dependency before adding this Git dependency to a Salvo
application. Salvo's current `saysion = "0.2"` dependency and this unreleased
0.3 package expose different `SessionStore` traits. Adding a direct 0.3
dependency does not upgrade the middleware's transitive 0.2 dependency; passing
a 0.3 `MemoryStore` to that middleware fails to compile.

| Middleware dependency | Application store dependency | Compatibility |
| --- | --- | --- |
| registry saysion 0.2 | registry saysion 0.2 | Existing 0.2 API only |
| registry saysion 0.2 | Git saysion 0.3 | Unsupported: distinct traits |
| the same Git revision of saysion 0.3 | that Git revision of saysion 0.3 | Coordinated development integration |
| registry saysion 0.3 | registry saysion 0.3 | Requires future coordinated releases |

For development integration, use a checkout of the Salvo middleware and change
its workspace `saysion` dependency to **the same Git revision and source** used
by the application. Point the application at that middleware checkout. Pin the
revision and commit the application lockfile. This changes the middleware
dependency requirement as well as the source; a 0.3 `[patch]` alone cannot
satisfy `^0.2`. A registry package and a Git package can also have different type
identities even when their version numbers match. Run `cargo tree -d` to inspect
duplicates. Do not mix the existing 0.2 middleware with 0.3 stores.

CI runs an [actual Salvo middleware integration fixture](../tests/fixtures/salvo-compat/README.md)
with a pinned upstream source and its dependency explicitly aligned to this
checkout. It verifies creation, loading and destruction through SessionHandler.
This is a development compatibility check; it does not upgrade the official
Salvo dependency or imply that a compatible middleware release is published.

## Rejected and uncertain writes

Server-side stores create records only from a fresh `Session::new()`. Loaded
records are updated conditionally. After logout, clearing, or rotation removes
the persisted ID, a stale save returns `SessionConflict` instead of upserting
the old authentication state. Detect it with
`error.downcast_ref::<saysion::SessionConflict>()` and discard stale state.
Do not automatically copy its authentication data into a fresh session.

A fresh session and its clones share one first-save authority. Its first
persistence attempt consumes that authority, including on failure or
cancellation: the backend might already have written the record. Confirm the
result by loading with its cookie, if available, or authenticate again. Never
retry the old object as a fresh insertion. A confirmed save allows conditional
updates. Session arguments are consumed; clones do not carry outgoing cookies.

Persist a regenerated session and send the returned cookie. Its old cookie
cannot load the replacement. SQL rotation uses a transaction. MongoDB standalone
revokes the source before inserting a replacement; insertion failure requires
recovery through authentication. Revoking all account sessions or every
descendant of a rotation still needs application-level generations. See the
[lifecycle contract](session-lifecycle.md).

## Change tracking and custom backends

`data_changed()` now includes identity, expiry, and outgoing-cookie changes.
`needs_persist()` also reports destruction. Middleware must delete destroyed
sessions and may otherwise save pending changes. Refreshing an expiry marks
metadata changed even when application data is unchanged, so review any policy
that suppresses writes for read-only requests.

Request-local clones share data but retain independent metadata flags.
MemoryStore loads independent snapshots; mutations require a successful save
before another request sees them. The serialized JSON/postcard data remains
readable, but deserializing a Session does not establish persistence authority.

Custom stores must capture the exact snapshot they write and acknowledge only
that successful revision. Calling `reset_data_changed()` after asynchronous I/O
can erase newer clone edits.

Use the [public custom-store protocol](custom-stores.md):

1. Consume the input with `session.prepare_save()?` to obtain a `SessionSave`.
   Its `session()` is a read-only `SessionSnapshot`; cloning it cannot mutate
   the pending save. `origin()` identifies a persisted source, if present.
2. Acquire `let write = save.begin_write().await?` before backend I/O, and keep
   the asynchronous guard through the write and its completion.
3. Call `save.validate_current()?` under the backend write lock or before the
   write, and use atomic backend existence and collision checks as described
   in the custom-store guide. This validation alone is not a storage lock.
4. After durable success, if `write.is_destroyed()` is true, delete the written
   target, call `save.confirm_removed(write)?`, and return a conflict without a
   cookie. Only confirm removal after its backend operation has succeeded.
5. Otherwise call `save.complete_with_guard(write)?` to acknowledge the captured
   revision, publish the saved identity, and return any outgoing cookie. If
   destruction races with this completion, a creation/rotation remains uncertain
   and waiting deletion returns an error instead of claiming success.
   Dropping a failed or cancelled token never refunds first-insert authority.

On load, `session.validate_loaded(&cookie)?` checks the ID and expiry and
establishes loaded persistence state. For deletion, acquire
`let deletion = session.begin_delete().await?`, delete `deletion.id()` in the
backend, and retain the guard until the operation completes. It marks
destruction before waiting for same-lineage writes, and selects the persisted
source, including an unsaved rotation or an already marked session. An
unconfirmed creation/rotation may have reached storage; its same-lineage
deletion fails conservatively and requires a trusted reload or authentication.
A superseded clone targets its
old ID rather than another clone's newly persisted identity. Do not call
crate-private helpers or infer state from an empty application data map.

The legacy synchronous `prepare_delete()` only selects an ID; it does not wait
for an in-flight write. Likewise, `complete()` assumes equivalent coordination
provided by the store. Asynchronous stores should use the guarded protocol
above, not combine the legacy helpers and claim coordinated completion.

## Cookies and stored data

`CookieStore` remains an unsigned, unencrypted codec for middleware that already
authenticates the complete value. Otherwise use `SignedCookieStore` with at
least 32 random secret key bytes, persisted across restarts. Fallback keys only
verify; the current key signs new values. Neither cookie store encrypts data or
revokes copied values. Set a finite expiry and use server-side storage when
confidentiality or immediate revocation is required.

The signed cookie format differs from the raw codec, and unsigned values are
never accepted as a fallback. Invalid signed values load as `None`. SQL corrupt
records return an error rather than a normal miss. Avoid logging raw cookies,
backend credentials, or complete errors that may include them.

## Cleanup and expiry

Standard server-side cookies generated by `Session::new` or `regenerate` keep
their existing format and ID mapping: 64 random bytes encoded as 88 characters
of canonical padded Base64. `Session::id_from_cookie_value` now rejects other
lengths before decoding and uses a fixed-size decoding buffer, retaining its
`base64::DecodeError` return type. Custom stores that intentionally use another
token protocol must parse it with their own explicit size budget. This change
does not restrict the separate variable-length `CookieStore` or
`SignedCookieStore` formats.

Run SQL `migrate()` to add expiry indexes to existing tables as well as new ones.
SQL `cleanup()` removes at most 1,000 expired rows per call.
`cleanup_batch(limit)` accepts 1 through 10,000 and returns the number removed;
repeat with a deliberate work budget when draining. MemoryStore now has the
same default batch size and limit range, backed by an expiry index rather
than a scan of all live records. Do not assume one call removes an entire
backlog; see the [backend guide](en/stores.md) for scheduling examples.

SQL retains the compatible Unix-seconds column and validates the exact expiry
from serialized data. Redis uses millisecond expiry and literal key prefixes.
Session Debug output is redacted; application logs still need the same care.

## Release verification

The release workflow checks tag/manifest agreement and uses a read-only
crates.io lookup before entering its publishing job. Existing versions and
failed lookups are rejected. Offline tests check installation sources, and CI
compiles an external consumer against this unreleased package. Preparing a
release requires deliberately updating the unreleased notices, dependency
examples, and guide policy tests together. These checks neither publish a
package nor create a tag.

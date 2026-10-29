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
can erase newer clone edits. Use the public save/load protocol:

1. Consume the input with `Session::prepare_save()` to obtain a `SessionSave`.
   `session()` exposes its snapshot and `origin()` identifies a persisted source,
   if present. Preparation claims a fresh session's first attempt.
2. Under the backend's write lock or transaction, call `validate_current()`
   before writing. Keep conditional creation/update and rotation atomic in the
   backend too; this validation is not a replacement for storage conditions.
3. After durable success, consume the handle with `complete()`. It publishes
   the saved identity, acknowledges that snapshot, and returns the outgoing
   cookie, if one is owed. Dropping a failed or cancelled handle does not return
   first-insert authority.

On load, pass deserialized data to `Session::validate_loaded(cookie_value)` to
validate the stored ID against the cookie, check expiry, and establish loaded
persistence state. Do not infer state from an empty or unchanged application
data map. No public setter grants arbitrary revision or fresh-session authority.

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

Run SQL `migrate()` to add expiry indexes to existing tables as well as new ones.
SQL `cleanup()` removes at most 1,000 expired rows per call.
`cleanup_batch(limit)` accepts 1 through 10,000 and returns the number removed;
repeat with a deliberate work budget when draining. Do not assume one call
removes an entire backlog. Consult each backend's cleanup API for its limits.

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

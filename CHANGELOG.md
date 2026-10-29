# Changelog

## 0.3.0 — Unreleased

This version is not published on crates.io. Use the Git `main` installation in
the README while evaluating it. Published 0.2.0 does not contain these new APIs.

### Migration and compatibility

- Minimum supported Rust version: **1.95** (previously 1.92).
- Server-side creation and updates are distinct. A stale save cannot recreate
  a deleted or rotated ID; handle `SessionConflict` by discarding stale state.
- A fresh session's first persistence attempt consumes its creation authority,
  even if the outcome is uncertain or the future is cancelled. Confirm through
  the store or authenticate again instead of retrying it as a fresh insertion.
- Identity, expiry, and outgoing-cookie changes now count as `data_changed()`;
  `needs_persist()` also includes destruction. Review write-suppression policies.
- SQL `cleanup()` removes one bounded batch. Repeat `cleanup_batch(limit)` with
  an application work budget when draining a large backlog.

### Added and corrected

- `SignedCookieStore` authenticates cookies with HMAC-SHA256 and fallback keys;
  it does not encrypt contents or support server-side revocation.
- `SessionConflict` reports rejected stale or superseded writes.
- `SessionSave` and the public prepare/complete and validated-load APIs allow
  custom stores to participate in the same persistence protocol.
- Memory loads use independent snapshots; unsaved changes stay request-local.
- Revision snapshots preserve edits made while asynchronous saves are running.
- Server-side rotation revokes the original ID; logout rejects late saves.
- Subsecond expiry, literal Redis prefixes, bounded clearing, and SQL indexes
  avoid premature expiry and unbounded mutation batches.
- Debug output omits session secrets; SQL deserialization errors propagate.
- CI runs isolated real-backend contracts, feature combinations, and an external
  consumer. Dependency security floors are part of the published manifest.

See the [0.3 migration guide](docs/migration-0.3.md). Publication and release
tagging remain separate maintainer actions.

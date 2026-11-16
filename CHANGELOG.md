# Changelog

## 0.4.1 — 2026-11-17

### Correctness and lifecycle

- MongoDB authentication lookups use the primary even when the shared client
  prefers secondaries. Read/write concerns still determine visibility and
  failover durability.
- Redis counting and clearing preserve binary key names, including non-UTF-8
  keys under an empty or literal prefix.
- PostgreSQL migrations serialize table and index creation in one transaction,
  avoiding concurrent first-start system-catalog conflicts.
- MySQL rejects complete session JSON larger than 65,535 bytes before writing
  or consuming a fresh session's creation authority, preventing silent `TEXT`
  truncation in non-strict SQL mode.
- MySQL new tables use InnoDB and a `utf8mb4` session column. Migration rejects
  incompatible legacy tables without converting data automatically; operators
  must explicitly convert those tables before startup.
- Both cookie stores reject locally destroyed sessions before issuing and
  acknowledging a new value. Existing copied cookies are still not revocable.
- `Session::try_expire_in` reports unrepresentable TTLs without changing the
  session. `expire_in` retains its existing API and documented panic behavior.

### Performance and tooling

- MemoryStore copies request data and releases displaced payloads outside the
  global store lock while preserving record and expiry-index consistency.
- Backend JSON and unsigned-cookie encoding avoid an intermediate full data-map
  clone; revision capture remains atomic with serialization.
- Signed cookies use bounded stack storage and one output allocation while
  preserving the `s1` format and 3,800-byte limit.
- MongoDB reuses collection handles and hints the `_id_` index for exact counts.
- SQLite-only builds no longer enable the network databases' TLS stack.
- Document last-write-wins field updates, backend renewal limits, Redis `PX`
  expiry and namespace scan costs. Fix the README contribution link.
- Build fixtures respect Cargo metadata and the caller's environment. CI avoids
  duplicate automatic lifecycle runs. Release jobs carry verified inputs,
  retain the dependency lock and toolchain, and compare package payloads.

### Known dependency issue

The optional SQLite backend still resolves an affected bundled SQLite through
SQLx 0.8.6. [RUSTSEC-2026-0459](https://rustsec.org/advisories/RUSTSEC-2026-0459.html)
requires opening a database/WAL pair writable by a less trusted party; ordinary
remote session values do not establish that precondition. Restrict access to
these files and assess the actual linked SQLite version. This release does not
resolve the advisory.

See the [0.4.1 upgrade notes](docs/migration-0.4.md#updating-from-040-to-041).

## 0.4.0 — 2026-10-30

This release includes the session persistence and security changes developed
since 0.2.0. The development version 0.3.0 was not published.

### Migration and compatibility

- Minimum supported Rust version: **1.95** (previously 1.92).
- Server-side creation and updates are distinct. A stale save cannot recreate
  a deleted or rotated ID; handle `SessionConflict` by discarding stale state.
- A fresh session's first persistence attempt consumes its creation authority,
  even if the outcome is uncertain or the future is cancelled. Confirm through
  the store or authenticate again instead of retrying it as a fresh insertion.
- Identity, expiry, and outgoing-cookie changes now count as `data_changed()`;
  `needs_persist()` also includes destruction. Review write-suppression policies.
- SQL and MemoryStore `cleanup()` remove one bounded batch. Repeat
  `cleanup_batch(limit)` with a work budget when draining a large backlog.

### Added and corrected

- `SignedCookieStore` authenticates cookies with HMAC-SHA256 and fallback keys;
  it does not encrypt contents or support server-side revocation.
- `SessionConflict` reports rejected stale or superseded writes.
- `SessionSave` and read-only `SessionSnapshot` expose the custom-backend
  prepare/complete protocol, validated loading, and source-aware deletion.
- Memory loads use independent snapshots; unsaved changes stay request-local.
- Revision snapshots preserve edits made while asynchronous saves are running.
- Server-side rotation revokes the original ID; logout rejects late saves.
- Subsecond expiry, literal Redis prefixes, bounded clearing, and SQL indexes
  avoid premature expiry and unbounded mutation batches.
- Debug output omits session secrets; SQL deserialization errors propagate.
- CI runs isolated real-backend contracts, feature combinations, and an external
  consumer. Dependency security floors are part of the published manifest.

See the [0.4 migration guide](docs/migration-0.4.md).

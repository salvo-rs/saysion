# Real Salvo middleware integration

The check uses the unchanged `crates/session/src/lib.rs` from the Apache-2.0
licensed [Salvo repository](https://github.com/salvo-rs/salvo), revision
`9eacc0f8a5de8de23a151ebdedd3a7c6243eb644`. The script verifies its SHA-256,
stages it inside `CARGO_TARGET_DIR`, and generates a standalone manifest that
aligns its saysion dependency to this repository. The core dependency is pinned
to published `salvo_core = 1.0.1`. No copied third-party source or generated build
files are stored in this repository.

`compatibility.rs` uses that real SessionHandler with this crate's MemoryStore,
then sends test requests that create, read and destroy a session. It checks
cookie replay after deletion and the middleware's re-exported trait identity.

On Windows, set `CARGO_HOME`, `CARGO_TARGET_DIR` and `TEMP`/`TMP` to D-drive
directories, then run `python scripts/check_salvo_compat.py`. The script checks
the actual Cargo target directory before compiling. CI runs the same check.

This fixture models an explicitly coordinated development dependency. The
upstream middleware still requires 0.2 at the pinned revision; installing it
unchanged alongside 0.3 is not supported. See the migration guide's compatibility
matrix. Updating the source pin requires reviewing the upstream dependency/API
changes and updating its checksum together.

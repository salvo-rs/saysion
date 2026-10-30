# Saysion Session Stores

> **0.3.0 is unreleased.** This guide describes Git `main`, not the published
> 0.2.0 package. The new APIs require Rust 1.95 or newer. Until 0.3.0 is published,
> use the Git dependency below. See the [migration guide](../migration-0.3.md).

Saysion ships with several pluggable session stores. Most are gated
behind cargo features so you only pull in the dependencies you need.

Salvo middleware using saysion 0.2 cannot accept Git 0.3 stores. Follow the
[middleware compatibility matrix](../migration-0.3.md#salvo-middleware-compatibility)
to align both dependency version and source before integrating these stores.

| Store               | Feature flag     | Backend          |
|---------------------|------------------|------------------|
| `MemoryStore`       | *(always on)*    | In-process       |
| `CookieStore`       | *(always on)*    | Client cookie    |
| `SignedCookieStore` | *(always on)*    | HMAC-authenticated client cookie |
| `RedisStore`        | `redis`          | Redis            |
| `MongoDbStore`      | `mongodb`        | MongoDB          |
| `SqlxPostgresStore` | `sqlx-postgres`  | PostgreSQL       |
| `SqlxSqliteStore`   | `sqlx-sqlite`    | SQLite           |
| `SqlxMySqlStore`    | `sqlx-mysql`     | MySQL / MariaDB  |

All stores implement the [`SessionStore`] trait, so they are
interchangeable subject to their security and revocation requirements.

## In-memory expiry cleanup

`MemoryStore` rejects expired sessions on load, but applications must schedule
physical cleanup. `cleanup()` now removes at most 1,000 expired records per call;
code that previously relied on one call draining every expired record must
schedule additional batches.

```rust
use saysion::MemoryStore;

#[tokio::main]
async fn main() -> saysion::Result {
    let store = MemoryStore::new();
    let removed = store.cleanup_batch(500).await?;
    println!("removed {removed} expired sessions");
    Ok(())
}
```

`cleanup_batch(limit)` accepts 1..=10,000 and returns the number removed. An
ordered expiry index limits candidate inspection and temporary ID allocation to
that batch size, without scanning unrelated live or permanent records. Record
writes, expiry changes, rotation, destruction, and clearing update the index
under the same store lock. Deletion rechecks the current expiry so concurrent
renewals survive, and each write lock covers at most 256 candidates. Concurrent
renewals can reduce a batch's removal count, so a zero result is not a global
completion barrier while writers remain active.

The index adds one entry per record with an expiry and logarithmic maintenance
work when such records are saved or removed. Cleanup work depends on the chosen
batch and index operations, rather than a scan of all stored records.

This is a record-count budget rather than a wall-clock deadline: dropping large
payloads still takes time. No background cleanup task or store capacity limit
is created automatically; applications remain responsible for scheduling and
memory budgets.

## Client-side cookies

`CookieStore` is an **unsigned, unencrypted codec** for middleware that already
authenticates the complete cookie value, such as Salvo's session handler. Do not
accept its raw output from an untrusted client without that protection.

Use `SignedCookieStore::new(secret_key)?` when the middleware does not provide
authentication. Supply at least 32 bytes of random secret key material, persist
the key across restarts, and set a finite session expiry. To rotate keys, use
`SignedCookieStore::new(new_key)?.with_fallback_key(old_key)?`; old keys verify
existing values while new values use the current key. Unsigned `CookieStore`
values are never accepted as a migration fallback.

Signed values are limited to 3800 bytes; middleware must also account for cookie
names, attributes, and any outer signature. Neither store encrypts its contents
or supports server-side revocation. Removing the browser cookie does not revoke
copied values. Use a server-side store for confidential data or immediate logout
revocation, and set `Secure`, `HttpOnly`, and an appropriate `SameSite` policy in
the HTTP middleware.

## Redis

```toml
[dependencies]
saysion = { git = "https://github.com/salvo-rs/saysion", branch = "main", features = ["redis"] }
```

```rust
use saysion::{RedisStore, Session, SessionStore};

#[tokio::main]
async fn main() -> saysion::Result {
    let store = RedisStore::from_url("redis://127.0.0.1:6379")
        .await?
        .with_prefix("myapp/");

    let mut session = Session::new();
    session.insert("user_id", 42)?;
    let cookie = store.store_session(session).await?.unwrap();

    let loaded = store.load_session(cookie).await?.unwrap();
    assert_eq!(loaded.get::<i32>("user_id"), Some(42));
    Ok(())
}
```

`RedisStore` uses [`redis::aio::ConnectionManager`] internally, which
provides connection pooling and automatic reconnection. Sessions with
an expiry are stored using `SET ... EX <ttl>` so Redis evicts them
automatically.

The default Redis connection manager has no connection or response timeout.
Set both explicitly for your request budget, or pass an existing configured
manager with `RedisStore::from_connection_manager`. An outer request deadline
can additionally bound the full operation, including reconnection and retries.

```rust
use std::time::Duration;
use redis::aio::ConnectionManagerConfig;

let config = ConnectionManagerConfig::new()
    .set_connection_timeout(Duration::from_secs(2))
    .set_response_timeout(Duration::from_millis(250));
let store = saysion::RedisStore::new_with_config(
    redis::Client::open("redis://127.0.0.1:6379")?,
    config,
).await?;
```

## MongoDB

```toml
[dependencies]
saysion = { git = "https://github.com/salvo-rs/saysion", branch = "main", features = ["mongodb"] }
```

```rust
use saysion::{MongoDbStore, Session, SessionStore};

#[tokio::main]
async fn main() -> saysion::Result {
    let store = MongoDbStore::from_uri("mongodb://127.0.0.1:27017", "myapp")
        .await?
        .with_collection("sessions");

    // Create the TTL index. Idempotent — safe to call on every startup.
    store.initialize().await?;

    let mut session = Session::new();
    session.insert("user_id", 42)?;
    let cookie = store.store_session(session).await?.unwrap();

    let loaded = store.load_session(cookie).await?.unwrap();
    assert_eq!(loaded.get::<i32>("user_id"), Some(42));
    Ok(())
}
```

The official `mongodb` driver maintains a connection pool internally,
so a single `MongoDbStore` can be cloned and shared. Calling
`initialize()` creates a TTL index on `expires_at` so MongoDB purges
expired sessions automatically.

## SQLx (PostgreSQL / SQLite / MySQL)

Pick the feature(s) for the database(s) you actually use:

```toml
[dependencies]
saysion = { git = "https://github.com/salvo-rs/saysion", branch = "main", features = ["sqlx-postgres"] }
# or "sqlx-sqlite", "sqlx-mysql" — multiple are allowed
```

All three SQLx stores share the same API and use a `sqlx::Pool` for
connection pooling.

```rust
use saysion::{Session, SessionStore, SqlxPostgresStore};

#[tokio::main]
async fn main() -> saysion::Result {
    let store = SqlxPostgresStore::from_url(
        "postgres://postgres:postgres@127.0.0.1/myapp",
    )
    .await?;

    // Create the table on first run.
    store.migrate().await?;

    let mut session = Session::new();
    session.insert("user_id", 42)?;
    let cookie = store.store_session(session).await?.unwrap();

    let loaded = store.load_session(cookie).await?.unwrap();
    assert_eq!(loaded.get::<i32>("user_id"), Some(42));

    // Periodically remove one bounded batch (at most 1,000 expired rows).
    store.cleanup().await?;
    // Or choose 1..=10,000 rows and inspect progress for scheduling.
    let removed = store.cleanup_batch(500).await?;
    println!("removed {removed} expired sessions");
    Ok(())
}
```

For SQLite and MySQL, swap the type and connection URL:

```rust
let store = saysion::SqlxSqliteStore::from_url("sqlite://sessions.db?mode=rwc").await?;
let store = saysion::SqlxMySqlStore::from_url("mysql://root:root@127.0.0.1/myapp").await?;
```

### Schema

All three SQLx stores use the same schema (table name configurable
via `with_table`, defaults to `saysion_sessions`):

| Column    | Type                          | Notes                          |
|-----------|-------------------------------|--------------------------------|
| `id`      | `TEXT` / `VARCHAR(128)` PK    | Hashed session id              |
| `expires` | `BIGINT` / `INTEGER` nullable | Unix seconds, `NULL` = forever |
| `session` | `TEXT`                        | JSON-serialized `Session`      |

Storing the expiry as a unix-second integer keeps queries portable
across all three databases without pulling in extra type-conversion
features.

`migrate()` also adds an expiry index to existing tables and is safe to repeat.
`cleanup()` deletes at most 1,000 rows per call. Use `cleanup_batch(limit)` with
a limit from 1 to 10,000 to control each delete statement and obtain its deleted
row count. Schedule further calls as needed; neither method loops until the
whole table is clean. The seconds column remains compatible with older data;
loads validate the precise expiry from the serialized session, and cleanup
leaves the current second alone to avoid deleting a still-valid session.

## Sharing a store between handlers

All stores are `Clone` and cheap to clone (they hold an internal pool
or connection manager), so wrapping them in an `Arc` is unnecessary —
just clone into your application state.

## Running integration tests

SQLite integration tests run automatically using isolated in-memory databases.
The service-backed tests are marked `#[ignore]` for local development; CI runs
all of them against real services. Each test has its own Redis prefix, MongoDB
collection, or SQL table, so tests can run in parallel:

```bash
# Redis
REDIS_URL=redis://127.0.0.1:6379 \
    cargo test --features redis -- --ignored

# MongoDB
MONGODB_URL=mongodb://127.0.0.1:27017 \
    cargo test --features mongodb -- --ignored

# PostgreSQL
POSTGRES_URL=postgres://postgres:postgres@127.0.0.1/saysion_test \
    cargo test --features sqlx-postgres -- --ignored

# SQLite (runs automatically; no service required)
cargo test --features sqlx-sqlite --test sqlx_sqlite

# MySQL
MYSQL_URL=mysql://root:root@127.0.0.1/saysion_test \
    cargo test --features sqlx-mysql -- --ignored
```

[`SessionStore`]: https://docs.rs/saysion/latest/saysion/trait.SessionStore.html
[`redis::aio::ConnectionManager`]: https://docs.rs/redis/latest/redis/aio/struct.ConnectionManager.html

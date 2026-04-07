# Saysion Session Stores

Saysion ships with several pluggable session stores. Most are gated
behind cargo features so you only pull in the dependencies you need.

| Store               | Feature flag     | Backend          |
|---------------------|------------------|------------------|
| `MemoryStore`       | *(always on)*    | In-process       |
| `CookieStore`       | *(always on)*    | Client cookie    |
| `RedisStore`        | `redis`          | Redis            |
| `MongoDbStore`      | `mongodb`        | MongoDB          |
| `SqlxPostgresStore` | `sqlx-postgres`  | PostgreSQL       |
| `SqlxSqliteStore`   | `sqlx-sqlite`    | SQLite           |
| `SqlxMySqlStore`    | `sqlx-mysql`     | MySQL / MariaDB  |

All stores implement the [`SessionStore`] trait, so they are
interchangeable.

## Redis

```toml
[dependencies]
saysion = { version = "0.1", features = ["redis"] }
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

## MongoDB

```toml
[dependencies]
saysion = { version = "0.1", features = ["mongodb"] }
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
saysion = { version = "0.1", features = ["sqlx-postgres"] }
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

    // Periodically remove expired rows.
    store.cleanup().await?;
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

## Sharing a store between handlers

All stores are `Clone` and cheap to clone (they hold an internal pool
or connection manager), so wrapping them in an `Arc` is unnecessary —
just clone into your application state.

## Running integration tests

The crate ships integration tests for every backend. They are marked
`#[ignore]` so they will not run unless you opt in:

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

# SQLite (no service required)
cargo test --features sqlx-sqlite -- --ignored

# MySQL
MYSQL_URL=mysql://root:root@127.0.0.1/saysion_test \
    cargo test --features sqlx-mysql -- --ignored
```

[`SessionStore`]: https://docs.rs/saysion/latest/saysion/trait.SessionStore.html
[`redis::aio::ConnectionManager`]: https://docs.rs/redis/latest/redis/aio/struct.ConnectionManager.html

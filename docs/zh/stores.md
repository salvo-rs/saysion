# Saysion 会话存储

Saysion 提供多种可插拔的会话存储后端,大多数通过 cargo feature
启用,你只会拉取实际用到的依赖。

| 存储                | Feature 标志     | 后端             |
|---------------------|------------------|------------------|
| `MemoryStore`       | *(始终可用)*     | 进程内内存       |
| `CookieStore`       | *(始终可用)*     | 客户端 Cookie    |
| `RedisStore`        | `redis`          | Redis            |
| `MongoDbStore`      | `mongodb`        | MongoDB          |
| `SqlxPostgresStore` | `sqlx-postgres`  | PostgreSQL       |
| `SqlxSqliteStore`   | `sqlx-sqlite`    | SQLite           |
| `SqlxMySqlStore`    | `sqlx-mysql`     | MySQL / MariaDB  |

所有存储都实现了 [`SessionStore`] trait,可以互相替换。

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

`RedisStore` 内部使用 [`redis::aio::ConnectionManager`],自带连接复用
与自动重连。带有过期时间的会话通过 `SET ... EX <ttl>` 写入,Redis 会
自动清理过期键。

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

    // 创建 TTL 索引,幂等操作,可以在每次启动时调用。
    store.initialize().await?;

    let mut session = Session::new();
    session.insert("user_id", 42)?;
    let cookie = store.store_session(session).await?.unwrap();

    let loaded = store.load_session(cookie).await?.unwrap();
    assert_eq!(loaded.get::<i32>("user_id"), Some(42));
    Ok(())
}
```

官方 `mongodb` 驱动内部维护连接池,因此 `MongoDbStore` 可以直接克隆
共享。调用 `initialize()` 会在 `expires_at` 字段上创建 TTL 索引,
MongoDB 会自动清理过期会话。

## SQLx (PostgreSQL / SQLite / MySQL)

按需启用对应数据库的 feature(可以同时启用多个):

```toml
[dependencies]
saysion = { version = "0.1", features = ["sqlx-postgres"] }
# 或者 "sqlx-sqlite", "sqlx-mysql"
```

三种 SQLx store API 完全一致,均使用 `sqlx::Pool` 进行连接池管理。

```rust
use saysion::{Session, SessionStore, SqlxPostgresStore};

#[tokio::main]
async fn main() -> saysion::Result {
    let store = SqlxPostgresStore::from_url(
        "postgres://postgres:postgres@127.0.0.1/myapp",
    )
    .await?;

    // 首次运行时建表。
    store.migrate().await?;

    let mut session = Session::new();
    session.insert("user_id", 42)?;
    let cookie = store.store_session(session).await?.unwrap();

    let loaded = store.load_session(cookie).await?.unwrap();
    assert_eq!(loaded.get::<i32>("user_id"), Some(42));

    // 定期清理过期记录。
    store.cleanup().await?;
    Ok(())
}
```

SQLite 和 MySQL 用法相同,只需替换类型与连接 URL:

```rust
let store = saysion::SqlxSqliteStore::from_url("sqlite://sessions.db?mode=rwc").await?;
let store = saysion::SqlxMySqlStore::from_url("mysql://root:root@127.0.0.1/myapp").await?;
```

### 表结构

三种 SQLx store 使用同一套表结构(表名通过 `with_table` 自定义,
默认为 `saysion_sessions`):

| 字段      | 类型                          | 说明                              |
|-----------|-------------------------------|-----------------------------------|
| `id`      | `TEXT` / `VARCHAR(128)` 主键  | 会话 id 的哈希                    |
| `expires` | `BIGINT` / `INTEGER` 可空     | Unix 秒,`NULL` 表示永不过期      |
| `session` | `TEXT`                        | JSON 序列化后的 `Session`         |

过期时间存为 unix 秒整数,使三种数据库的查询完全一致,也避免了
为 sqlx 启用额外的时间类型转换 feature。

## 在多个 handler 之间共享 store

所有 store 都实现了 `Clone`,克隆代价很低(内部已经是连接池或连接
管理器),无需再用 `Arc` 包装,直接克隆到应用状态里即可。

## 运行集成测试

每个后端都附带集成测试,默认带 `#[ignore]` 标记,需要显式开启:

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

# SQLite(无需外部服务)
cargo test --features sqlx-sqlite -- --ignored

# MySQL
MYSQL_URL=mysql://root:root@127.0.0.1/saysion_test \
    cargo test --features sqlx-mysql -- --ignored
```

[`SessionStore`]: https://docs.rs/saysion/latest/saysion/trait.SessionStore.html
[`redis::aio::ConnectionManager`]: https://docs.rs/redis/latest/redis/aio/struct.ConnectionManager.html

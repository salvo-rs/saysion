# Saysion 会话存储

> **0.3.0 尚未发布（unreleased）。** 本指南描述 Git `main`，不是已发布的 0.2.0。
> 新 API 要求 Rust 1.95 或更新版本。在 0.3.0 发布前，请使用下面的 Git 依赖。
> 变更和迁移说明见 [迁移指南](../migration-0.3.md)。

Saysion 提供多种可插拔的会话存储后端,大多数通过 cargo feature
启用,你只会拉取实际用到的依赖。

| 存储                | Feature 标志     | 后端             |
|---------------------|------------------|------------------|
| `MemoryStore`       | *(始终可用)*     | 进程内内存       |
| `CookieStore`       | *(始终可用)*     | 客户端 Cookie    |
| `SignedCookieStore` | *(始终可用)*     | HMAC 认证的客户端 Cookie |
| `RedisStore`        | `redis`          | Redis            |
| `MongoDbStore`      | `mongodb`        | MongoDB          |
| `SqlxPostgresStore` | `sqlx-postgres`  | PostgreSQL       |
| `SqlxSqliteStore`   | `sqlx-sqlite`    | SQLite           |
| `SqlxMySqlStore`    | `sqlx-mysql`     | MySQL / MariaDB  |

所有存储都实现了 [`SessionStore`] trait,替换时仍须满足各自的安全和撤销要求。

## 客户端 Cookie

`CookieStore` 是**不签名、不加密的编码器**,仅适用于已对完整 Cookie 值认证的
中间件,例如 Salvo 会话中间件。没有这一保护时,不能直接接受客户端提交的值。

中间件未提供认证时,使用 `SignedCookieStore::new(secret_key)?`,传入至少 32 字节
的随机秘密密钥,跨重启保存密钥,并为会话设置有限过期时间。轮换时使用
`SignedCookieStore::new(new_key)?.with_fallback_key(old_key)?`:旧密钥仅验证已有
Cookie,新 Cookie 始终使用当前密钥签名。不会接受未签名的 `CookieStore` 值作为
迁移回退。

签名后的值最多 3800 字节,中间件还需计算 Cookie 名称、属性和外层签名的大小。
两种存储都不加密内容,也不支持服务端撤销;删除浏览器 Cookie 无法使已复制的值
失效。机密数据或需要注销后立即撤销的会话应使用服务端存储。HTTP 中间件还应
设置 `Secure`、`HttpOnly` 和合适的 `SameSite` 策略。

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

`RedisStore` 内部使用 [`redis::aio::ConnectionManager`],自带连接复用
与自动重连。带有过期时间的会话通过 `SET ... EX <ttl>` 写入,Redis 会
自动清理过期键。

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
saysion = { git = "https://github.com/salvo-rs/saysion", branch = "main", features = ["sqlx-postgres"] }
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

    // 定期清理一批过期记录（每次至多 1,000 条）。
    store.cleanup().await?;
    // 也可以选择 1..=10,000 条的批量上限，并根据返回数量安排后续清理。
    let removed = store.cleanup_batch(500).await?;
    println!("已清理 {removed} 条过期会话");
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

`migrate()` 还会为已有表添加过期索引，可以重复调用。
`cleanup()` 每次最多删除 1,000 条记录；`cleanup_batch(limit)` 允许指定
1 到 10,000 条的上限，并返回删除数量。两者每次只执行一批，不会循环清空
整张表，调用方应按需安排后续清理。秒级字段继续兼容旧数据；加载时会校验
序列化会话中的精确过期时间，清理时暂时保留当前秒的记录，避免提前删除。

## 在多个 handler 之间共享 store

所有 store 都实现了 `Clone`,克隆代价很低(内部已经是连接池或连接
管理器),无需再用 `Arc` 包装,直接克隆到应用状态里即可。

## 运行集成测试

SQLite 集成测试使用独立的内存数据库，默认自动运行。其他后端测试在本地
带 `#[ignore]` 标记，CI 会使用真实服务运行。每个测试使用独立的 Redis
前缀、MongoDB 集合或 SQL 表，可以并行运行：

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
cargo test --features sqlx-sqlite --test sqlx_sqlite

# MySQL
MYSQL_URL=mysql://root:root@127.0.0.1/saysion_test \
    cargo test --features sqlx-mysql -- --ignored
```

[`SessionStore`]: https://docs.rs/saysion/latest/saysion/trait.SessionStore.html
[`redis::aio::ConnectionManager`]: https://docs.rs/redis/latest/redis/aio/struct.ConnectionManager.html

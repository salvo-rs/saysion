#![cfg(feature = "sqlx-postgres")]
//! Integration tests for `SqlxPostgresStore`.
//!
//! Set `POSTGRES_URL` (default
//! `postgres://postgres:postgres@127.0.0.1/saysion_test`).
//! Run with `cargo test --features sqlx-postgres -- --ignored`.

mod common;

use std::time::Duration;

use saysion::{Session, SessionStore, SqlxPostgresStore};

fn url() -> String {
    std::env::var("POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1/saysion_test".to_string())
}

struct TestStore {
    store: SqlxPostgresStore,
    pool: sqlx::PgPool,
    table: String,
}

impl std::ops::Deref for TestStore {
    type Target = SqlxPostgresStore;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

impl TestStore {
    async fn finish(self) {
        sqlx::query(&format!("DROP TABLE {}", self.table))
            .execute(&self.pool)
            .await
            .expect("drop test table");
        self.pool.close().await;
    }
}

async fn fresh_store() -> TestStore {
    let pool = sqlx::PgPool::connect(&url())
        .await
        .expect("connect to postgres");
    let table = format!("saysion_test_{:016x}", rand::random::<u64>());
    let store = SqlxPostgresStore::new(pool.clone()).with_table(&table);
    store.migrate().await.expect("migrate");
    TestStore { store, pool, table }
}

#[tokio::test]
#[ignore]
async fn pg_rejects_a_valid_blob_bound_to_another_record() {
    let store = fresh_store().await;
    let pool = store.pool.clone();
    let table = store.table.clone();
    common::assert_identity_binding(&*store, |id, blob| async move {
        sqlx::query(&format!("UPDATE {table} SET session = $1 WHERE id = $2"))
            .bind(blob)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    })
    .await;
    store.finish().await;
}

#[tokio::test]
#[ignore]
async fn pg_roundtrip() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.insert("hello", "world").unwrap();
    let cookie = store.store_session(session).await.unwrap().unwrap();

    let loaded = store.load_session(cookie).await.unwrap().unwrap();
    assert_eq!(loaded.get::<String>("hello").unwrap(), "world");
    common::assert_contract(&*store).await;

    store.finish().await;
}

#[tokio::test]
#[ignore]
async fn pg_expiry_and_cleanup() {
    let store = fresh_store().await;

    let mut session = Session::new();
    session.expire_in(Duration::from_secs(1));
    let cookie = store.store_session(session).await.unwrap().unwrap();

    assert!(store.load_session(cookie.clone()).await.unwrap().is_some());
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(store.load_session(cookie).await.unwrap().is_none());

    store.cleanup().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
    store.finish().await;
}

#[tokio::test]
#[ignore]
async fn pg_destroy_and_clear() {
    let store = fresh_store().await;

    for _ in 0..3 {
        store.store_session(Session::new()).await.unwrap();
    }
    let cookie = store.store_session(Session::new()).await.unwrap().unwrap();
    assert_eq!(store.count().await.unwrap(), 4);

    let loaded = store.load_session(cookie.clone()).await.unwrap().unwrap();
    store.destroy_session(loaded).await.unwrap();
    assert!(store.load_session(cookie).await.unwrap().is_none());

    store.clear_store().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
    store.finish().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exercised by the backend CI job"]
async fn pg_concurrent_first_migrations_create_one_usable_table() {
    use std::sync::Arc;

    // Exercise separate connections and actual migrate() calls, rather than
    // substituting an external CREATE that does not participate in coordination.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&url())
        .await
        .unwrap();
    let table = format!("saysion_migration_{:016x}", rand::random::<u64>());
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let store = SqlxPostgresStore::new(pool.clone()).with_table(&table);
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            store.migrate().await
        });
    }
    let outcome = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(result) = tasks.join_next().await {
            result.unwrap().unwrap();
        }
    })
    .await;
    let store = SqlxPostgresStore::new(pool.clone()).with_table(&table);
    if outcome.is_ok() {
        let cookie = store.store_session(Session::new()).await.unwrap().unwrap();
        assert!(store.load_session(cookie).await.unwrap().is_some());
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    sqlx::query(&format!("DROP TABLE IF EXISTS {table}"))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    outcome.expect("concurrent migrations must complete without deadlock");
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exercised by the backend CI job"]
async fn pg_failed_migration_rolls_back_and_releases_coordination() {
    let pool = sqlx::PgPool::connect(&url()).await.unwrap();
    let table = format!("saysion_failed_migration_{:016x}", rand::random::<u64>());
    // An existing incompatible relation makes CREATE INDEX fail after the lock
    // has been acquired. Dropping the transaction must not strand that lock.
    sqlx::query(&format!("CREATE TABLE {table} (id TEXT PRIMARY KEY)"))
        .execute(&pool)
        .await
        .unwrap();
    let store = SqlxPostgresStore::new(pool.clone()).with_table(&table);
    assert!(store.migrate().await.is_err());
    sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN expires BIGINT"))
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), store.migrate())
        .await
        .expect("the failed migration must release its advisory lock")
        .unwrap();
    sqlx::query(&format!("DROP TABLE {table}"))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

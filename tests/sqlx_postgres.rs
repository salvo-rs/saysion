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
async fn pg_equivalent_table_names_reuse_the_expiry_index() {
    let store = fresh_store().await;
    let (schema,): (String,) = sqlx::query_as("SELECT current_schema()")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    let qualified = format!("\"{}\".\"{}\"", schema.replace('"', "\"\""), store.table);
    SqlxPostgresStore::new(store.pool.clone())
        .with_table(qualified)
        .migrate()
        .await
        .unwrap();
    let (indexes,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pg_index AS i JOIN pg_class AS c ON c.oid = i.indexrelid \
         WHERE i.indrelid = to_regclass($1) AND c.relname LIKE 'saysion_expiry_%'",
    )
    .bind(&store.table)
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(indexes, 1);
    store.finish().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exercised by the backend CI job"]
async fn pg_unrelated_partial_index_is_preserved_without_replacing_cleanup_index() {
    let pool = sqlx::PgPool::connect(&url()).await.unwrap();
    let table = format!("saysion_partial_{:016x}", rand::random::<u64>());
    sqlx::query(&format!(
        "CREATE TABLE {table} (id TEXT PRIMARY KEY NOT NULL, expires BIGINT, session TEXT NOT NULL)"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "CREATE INDEX user_{table} ON {table} (expires,id) WHERE expires > 100"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let store = SqlxPostgresStore::new(pool.clone()).with_table(&table);
    store.migrate().await.unwrap();
    let (indexes,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pg_index WHERE indrelid=to_regclass($1) AND NOT indisprimary",
    )
    .bind(&table)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(&format!("DROP TABLE {table}"))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert_eq!(indexes, 2);
}

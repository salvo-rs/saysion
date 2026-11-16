#![cfg(feature = "sqlx-mysql")]
//! Integration tests for `SqlxMySqlStore`.
//!
//! Set `MYSQL_URL` (default
//! `mysql://root:root@127.0.0.1/saysion_test`).
//! Run with `cargo test --features sqlx-mysql -- --ignored`.

mod common;

use std::time::Duration;

use saysion::{Session, SessionStore, SqlxMySqlStore};

fn url() -> String {
    std::env::var("MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:root@127.0.0.1/saysion_test".to_string())
}

struct TestStore {
    store: SqlxMySqlStore,
    pool: sqlx::MySqlPool,
    table: String,
}

impl std::ops::Deref for TestStore {
    type Target = SqlxMySqlStore;

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
    let pool = sqlx::MySqlPool::connect(&url())
        .await
        .expect("connect to mysql");
    let table = format!("saysion_test_{:016x}", rand::random::<u64>());
    let store = SqlxMySqlStore::new(pool.clone()).with_table(&table);
    store.migrate().await.expect("migrate");
    TestStore { store, pool, table }
}

#[tokio::test]
#[ignore]
async fn mysql_rejects_a_valid_blob_bound_to_another_record() {
    let store = fresh_store().await;
    let pool = store.pool.clone();
    let table = store.table.clone();
    common::assert_identity_binding(&*store, |id, blob| async move {
        sqlx::query(&format!("UPDATE {table} SET session = ? WHERE id = ?"))
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
async fn mysql_roundtrip() {
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
async fn mysql_expiry_and_cleanup() {
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
async fn mysql_destroy_and_clear() {
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
#[ignore]
async fn mysql_migration_is_idempotent_and_cleanup_is_bounded() {
    let pool = sqlx::MySqlPool::connect(&url()).await.unwrap();
    let table = format!(
        "saysion_cleanup_{}",
        saysion::time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let store = SqlxMySqlStore::new(pool.clone()).with_table(&table);
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();
    let (first, second) = tokio::join!(store.migrate(), store.migrate());
    first.unwrap();
    second.unwrap();
    for index in 0..7 {
        sqlx::query(&format!(
            "INSERT INTO {table} (id, expires, session) VALUES (?, ?, '{{}}')"
        ))
        .bind(format!("expired-{index}"))
        .bind(saysion::time::OffsetDateTime::now_utc().unix_timestamp() - 60)
        .execute(&pool)
        .await
        .unwrap();
    }
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 3);
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 3);
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 1);
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 0);
    sqlx::query(&format!("DROP TABLE {table}"))
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires MySQL with MyISAM; exercised by the backend CI job"]
async fn mysql_migration_uses_innodb_despite_a_nontransactional_default() {
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .connect(&url())
        .await
        .unwrap();
    sqlx::query("SET SESSION default_storage_engine = 'MyISAM'")
        .execute(&pool)
        .await
        .unwrap();
    let table = format!("saysion_engine_{:016x}", rand::random::<u64>());
    let store = SqlxMySqlStore::new(pool.clone()).with_table(&table);
    store.migrate().await.unwrap();
    let old_cookie = store.store_session(Session::new()).await.unwrap().unwrap();
    let mut session = store
        .load_session(old_cookie.clone())
        .await
        .unwrap()
        .unwrap();
    session.regenerate();
    sqlx::query(&format!(
        "INSERT INTO {table} (id, session) VALUES (?, '{{}}')"
    ))
    .bind(session.id())
    .execute(&pool)
    .await
    .unwrap();
    let result = store.store_session(session).await;
    let old_still_loads = store.load_session(old_cookie).await.unwrap().is_some();
    sqlx::query(&format!("DROP TABLE {table}"))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(result.is_err(), "the collision must reject rotation");
    assert!(old_still_loads, "failed rotation must restore the source");
}

#[tokio::test]
#[ignore = "requires MySQL with MyISAM; exercised by the backend CI job"]
async fn mysql_migration_rejects_an_existing_nontransactional_table() {
    let pool = sqlx::MySqlPool::connect(&url()).await.unwrap();
    let (database,): (String,) = sqlx::query_as("SELECT DATABASE()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let name = format!("saysion.engine_{:016x}", rand::random::<u64>());
    let table = format!("`{}`.`{name}`", database.replace('`', "``"));
    sqlx::query(&format!(
        "CREATE TABLE {table} (id VARCHAR(128) PRIMARY KEY NOT NULL, \
         expires BIGINT NULL, session TEXT NOT NULL) ENGINE=MyISAM"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "INSERT INTO {table} (id, session) VALUES ('original', 'keep this value')"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let store = SqlxMySqlStore::new(pool.clone()).with_table(&table);
    let result = store.migrate().await;
    let (value,): (String,) = sqlx::query_as(&format!("SELECT session FROM {table}"))
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query(&format!("DROP TABLE {table}"))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(result.unwrap_err().to_string().contains("InnoDB"));
    assert_eq!(value, "keep this value");
}

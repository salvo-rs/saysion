#![cfg(feature = "sqlx-sqlite")]

use saysion::{Session, SessionStore, SqlxSqliteStore, time::OffsetDateTime};

#[tokio::test]
async fn cleanup_indexes_support_main_and_attached_schema_table_names() {
    // ATTACH applies per connection; use one connection throughout the test.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("ATTACH DATABASE ':memory:' AS aux")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ATTACH DATABASE ':memory:' AS \"aux.db\"")
        .execute(&pool)
        .await
        .unwrap();

    for (qualified, schema, name) in [
        ("main.sessions", "main", "sessions"),
        ("aux.sessions", "aux", "sessions"),
        (r#""aux.db"."quoted.table""#, r#""aux.db""#, "quoted.table"),
        (r#""unqualified.table""#, "main", "unqualified.table"),
        ("[aux.db].[bracket.table]", "[aux.db]", "bracket.table"),
    ] {
        let store = SqlxSqliteStore::new(pool.clone()).with_table(qualified);
        store.migrate().await.unwrap();
        store.migrate().await.unwrap();
        let (indexes,): (i64,) = sqlx::query_as(&format!(
            "SELECT COUNT(*) FROM {schema}.sqlite_master \
             WHERE type = 'index' AND tbl_name = ? AND name LIKE 'saysion_expiry_%'"
        ))
        .bind(name)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(indexes, 1, "expiry index for {qualified}");

        let cookie = store.store_session(Session::new()).await.unwrap().unwrap();
        assert!(store.load_session(cookie).await.unwrap().is_some());
        assert_eq!(store.cleanup_batch(10).await.unwrap(), 0);
        assert_eq!(store.count().await.unwrap(), 1);
    }
}

#[tokio::test]
async fn cleanup_upgrades_existing_tables_and_uses_the_expiry_index() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query(
        "CREATE TABLE saysion_sessions (id TEXT PRIMARY KEY NOT NULL, \
         expires INTEGER NULL, session TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let store = SqlxSqliteStore::new(pool.clone());
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();

    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT id FROM saysion_sessions \
         WHERE expires IS NOT NULL AND expires < ? ORDER BY expires, id LIMIT ?",
    )
    .bind(OffsetDateTime::now_utc().unix_timestamp())
    .bind(100_i64)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        plan.iter().any(|row| row.3.contains("saysion_expiry_")),
        "expiry cleanup should use its index: {plan:?}"
    );
}

#[tokio::test]
async fn cleanup_batches_are_bounded_and_preserve_live_rows() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let store = SqlxSqliteStore::new(pool.clone());
    store.migrate().await.unwrap();
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut transaction = pool.begin().await.unwrap();
    for index in 0..1_012 {
        let expiry = match index {
            0..1_007 => Some(now - 60),
            1_007..1_010 => Some(now + 3_600),
            _ => None,
        };
        sqlx::query("INSERT INTO saysion_sessions (id, expires, session) VALUES (?, ?, '{}')")
            .bind(format!("session-{index}"))
            .bind(expiry)
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    assert!(store.cleanup_batch(0).await.is_err());
    assert!(store.cleanup_batch(10_001).await.is_err());
    store.cleanup().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 12);
    assert_eq!(store.cleanup_batch(3).await.unwrap(), 3);
    assert_eq!(store.count().await.unwrap(), 9);
    assert_eq!(store.cleanup_batch(10_000).await.unwrap(), 4);
    assert_eq!(store.count().await.unwrap(), 5);
    assert_eq!(store.cleanup_batch(10_000).await.unwrap(), 0);
}

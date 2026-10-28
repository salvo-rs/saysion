#![cfg(any(
    feature = "redis",
    feature = "mongodb",
    feature = "sqlx-postgres",
    feature = "sqlx-mysql"
))]

use saysion::{Session, SessionStore};

fn namespace() -> String {
    format!(
        "saysion_lifecycle_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

async fn rotation_contract(store: &impl SessionStore) -> saysion::Result {
    let mut session = Session::new();
    session.insert("user_id", 42)?;
    let old_cookie = store.store_session(session).await?.unwrap();
    let mut rotating = store.load_session(old_cookie.clone()).await?.unwrap();
    rotating.regenerate();
    rotating.regenerate();
    let new_cookie = store.store_session(rotating).await?.unwrap();
    assert!(store.load_session(old_cookie).await?.is_none());
    assert_eq!(
        store
            .load_session(new_cookie.clone())
            .await?
            .unwrap()
            .get::<i32>("user_id"),
        Some(42)
    );
    // An unchanged save must succeed, including MySQL matched-row semantics.
    let loaded = store.load_session(new_cookie).await?.unwrap();
    store.store_session(loaded).await?;
    store.clear_store().await?;
    Ok(())
}

#[cfg(feature = "redis")]
#[tokio::test]
#[ignore = "requires Redis; exercised by the backend CI job"]
async fn redis_lifecycle_contract() -> saysion::Result {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let store = saysion::RedisStore::from_url(&url)
        .await?
        .with_prefix(format!("{}/", namespace()));
    rotation_contract(&store).await
}

#[cfg(feature = "mongodb")]
#[tokio::test]
#[ignore = "requires MongoDB; exercised by the backend CI job"]
async fn mongodb_lifecycle_contract() -> saysion::Result {
    let url = std::env::var("MONGODB_URL").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".into());
    let store = saysion::MongoDbStore::from_uri(&url, "saysion_test")
        .await?
        .with_collection(namespace());
    store.initialize().await?;
    rotation_contract(&store).await
}

#[cfg(feature = "sqlx-postgres")]
#[tokio::test]
#[ignore = "requires PostgreSQL; exercised by the backend CI job"]
async fn postgres_lifecycle_contract() -> saysion::Result {
    let url = std::env::var("POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1/saysion_test".into());
    let store = saysion::SqlxPostgresStore::from_url(&url)
        .await?
        .with_table(namespace());
    store.migrate().await?;
    rotation_contract(&store).await
}

#[cfg(feature = "sqlx-mysql")]
#[tokio::test]
#[ignore = "requires MySQL; exercised by the backend CI job"]
async fn mysql_lifecycle_contract() -> saysion::Result {
    let url = std::env::var("MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:root@127.0.0.1/saysion_test".into());
    let store = saysion::SqlxMySqlStore::from_url(&url)
        .await?
        .with_table(namespace());
    store.migrate().await?;
    rotation_contract(&store).await
}

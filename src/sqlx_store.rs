//! SQLx-backed session stores.
//!
//! Three concrete stores are exposed, each gated behind its own
//! cargo feature:
//!
//! - `SqlxPostgresStore` (`sqlx-postgres`)
//! - `SqlxSqliteStore`   (`sqlx-sqlite`)
//! - `SqlxMySqlStore`    (`sqlx-mysql`)
//!
//! All three use a `sqlx::Pool` internally, so connections are
//! pooled and reused across tasks. Expiry timestamps are stored as
//! signed unix-second integers (`NULL` means "no expiry"), keeping
//! the schema and queries portable across backends.
//!
//! Before first use call `migrate()` once to create the table.

use time::OffsetDateTime;

use crate::Session;

const DEFAULT_CLEANUP_BATCH_SIZE: u32 = 1_000;
const MAX_CLEANUP_BATCH_SIZE: u32 = 10_000;

fn cleanup_limit(limit: u32) -> crate::Result<i64> {
    anyhow::ensure!(
        (1..=MAX_CLEANUP_BATCH_SIZE).contains(&limit),
        "cleanup batch size must be between 1 and {MAX_CLEANUP_BATCH_SIZE}"
    );
    Ok(i64::from(limit))
}

fn expiry_index_name(table: &str) -> String {
    // Fixed-length, SQL-safe names also fit MySQL's identifier length limit.
    let hash = blake3::hash(table.as_bytes()).to_hex();
    format!("saysion_expiry_{}", &hash.as_str()[..16])
}

fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn expiry_unix(session: &Session) -> Option<i64> {
    // Keep the existing seconds schema. Loads include this entire second and
    // validate the exact timestamp from the serialized session afterward.
    session.expiry().map(|e| e.unix_timestamp())
}

fn parse_session(json: &str) -> crate::Result<Option<Session>> {
    Ok(serde_json::from_str::<Session>(json)?.validate())
}

// A duplicate identity must never overwrite another session, including when a
// caller races a first insert or a rotated identity happens to collide.
fn map_insert_error(error: sqlx::Error) -> crate::Error {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        crate::SessionConflict.into()
    } else {
        error.into()
    }
}

// ---------- Postgres ----------

#[cfg(feature = "sqlx-postgres")]
mod pg {
    use super::*;
    use sqlx::PgPool;

    use crate::{Result, Session, SessionStore, async_trait};

    /// PostgreSQL-backed session store.
    #[derive(Clone, Debug)]
    pub struct SqlxPostgresStore {
        pool: PgPool,
        table: String,
    }

    impl SqlxPostgresStore {
        /// Create a store from an existing connection pool.
        pub fn new(pool: PgPool) -> Self {
            Self {
                pool,
                table: "saysion_sessions".to_string(),
            }
        }

        /// Connect to Postgres using a connection URL.
        pub async fn from_url(url: &str) -> Result<Self> {
            Ok(Self::new(PgPool::connect(url).await?))
        }

        /// Override the table name. Defaults to `saysion_sessions`.
        pub fn with_table(mut self, table: impl Into<String>) -> Self {
            self.table = table.into();
            self
        }

        /// Create the sessions table if it does not yet exist.
        pub async fn migrate(&self) -> Result {
            let sql = format!(
                "CREATE TABLE IF NOT EXISTS {table} (\
                    id TEXT PRIMARY KEY NOT NULL, \
                    expires BIGINT NULL, \
                    session TEXT NOT NULL\
                )",
                table = self.table
            );
            sqlx::query(&sql).execute(&self.pool).await?;
            let sql = format!(
                "CREATE INDEX IF NOT EXISTS {} ON {} (expires, id) WHERE expires IS NOT NULL",
                expiry_index_name(&self.table),
                self.table
            );
            sqlx::query(&sql).execute(&self.pool).await?;
            Ok(())
        }

        /// Delete at most 1,000 expired rows in one bounded batch.
        ///
        /// Call periodically, or use [`Self::cleanup_batch`] for a different
        /// batch size and the number of removed rows. Rows in the current
        /// second are retained until their exact expiry is certainly past.
        pub async fn cleanup(&self) -> Result {
            self.cleanup_batch(DEFAULT_CLEANUP_BATCH_SIZE).await?;
            Ok(())
        }

        /// Delete one batch of expired rows, returning the number removed.
        ///
        /// `limit` must be between 1 and 10,000. Each call performs a single
        /// delete statement; callers control scheduling of subsequent batches.
        pub async fn cleanup_batch(&self, limit: u32) -> Result<u64> {
            let limit = cleanup_limit(limit)?;
            let sql = format!(
                "DELETE FROM {0} WHERE expires IS NOT NULL AND expires < $1 \
                 AND id IN (SELECT id FROM {0} WHERE expires IS NOT NULL AND expires < $1 \
                 ORDER BY expires, id LIMIT $2)",
                self.table
            );
            Ok(sqlx::query(&sql)
                .bind(now_unix())
                .bind(limit)
                .execute(&self.pool)
                .await?
                .rows_affected())
        }

        /// Returns the number of stored sessions.
        pub async fn count(&self) -> Result<i64> {
            let sql = format!("SELECT COUNT(*) FROM {}", self.table);
            let (n,): (i64,) = sqlx::query_as(&sql).fetch_one(&self.pool).await?;
            Ok(n)
        }
    }

    #[async_trait]
    impl SessionStore for SqlxPostgresStore {
        async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
            let id = Session::id_from_cookie_value(&cookie_value)?;
            let sql = format!(
                "SELECT session FROM {} WHERE id = $1 AND (expires IS NULL OR expires >= $2)",
                self.table
            );
            let row: Option<(String,)> = sqlx::query_as(&sql)
                .bind(&id)
                .bind(now_unix())
                .fetch_optional(&self.pool)
                .await?;
            Ok(row
                .map(|(s,)| parse_session(&s))
                .transpose()?
                .flatten()
                .map(Session::mark_loaded))
        }

        async fn store_session(&self, session: Session) -> Result<Option<String>> {
            let id = session.id().to_string();
            if session.is_destroyed() {
                return Err(crate::SessionConflict.into());
            }
            let origin = session.persistence_origin()?;
            let (snapshot, revision) = session.persistence_snapshot();
            let json = serde_json::to_string(&snapshot)?;
            let expires = expiry_unix(&snapshot);
            if let Some(origin) = origin.as_deref().filter(|origin| *origin != id.as_str()) {
                let mut tx = self.pool.begin().await?;
                let delete = format!("DELETE FROM {} WHERE id = $1", self.table);
                let removed = sqlx::query(&delete).bind(origin).execute(&mut *tx).await?;
                if removed.rows_affected() != 1 {
                    return Err(crate::SessionConflict.into());
                }
                let insert = format!(
                    "INSERT INTO {} (id, expires, session) VALUES ($1, $2, $3)",
                    self.table
                );
                sqlx::query(&insert)
                    .bind(&id)
                    .bind(expires)
                    .bind(&json)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_insert_error)?;
                tx.commit().await?;
            } else if origin.is_some() {
                // A loaded/saved session may only update an existing record.
                // Deletion (including clear_store) must win over a stale save.
                let update = format!(
                    "UPDATE {} SET expires = $1, session = $2 WHERE id = $3",
                    self.table
                );
                let updated = sqlx::query(&update)
                    .bind(expires)
                    .bind(&json)
                    .bind(&id)
                    .execute(&self.pool)
                    .await?;
                if updated.rows_affected() != 1 {
                    return Err(crate::SessionConflict.into());
                }
            } else {
                let insert = format!(
                    "INSERT INTO {} (id, expires, session) VALUES ($1, $2, $3)",
                    self.table
                );
                sqlx::query(&insert)
                    .bind(&id)
                    .bind(expires)
                    .bind(&json)
                    .execute(&self.pool)
                    .await
                    .map_err(map_insert_error)?;
            }
            session.mark_persisted();
            session.acknowledge_revision(revision);
            Ok(session.into_cookie_value())
        }

        async fn destroy_session(&self, mut session: Session) -> Result {
            let origin = session
                .persistence_origin()
                .ok()
                .flatten()
                .unwrap_or_else(|| session.id().to_owned());
            session.destroy();
            let sql = format!("DELETE FROM {} WHERE id = $1", self.table);
            sqlx::query(&sql).bind(&origin).execute(&self.pool).await?;
            Ok(())
        }

        async fn clear_store(&self) -> Result {
            let sql = format!("DELETE FROM {}", self.table);
            sqlx::query(&sql).execute(&self.pool).await?;
            Ok(())
        }
    }
}

#[cfg(feature = "sqlx-postgres")]
pub use pg::SqlxPostgresStore;

// ---------- SQLite ----------

#[cfg(feature = "sqlx-sqlite")]
#[allow(missing_docs)]
mod sqlite {
    use super::*;
    use sqlx::SqlitePool;

    use crate::{Result, Session, SessionStore, async_trait};

    fn index_target(table: &str) -> (String, &str) {
        let index = expiry_index_name(table);
        let mut quote = None;
        let mut characters = table.char_indices().peekable();
        while let Some((position, character)) = characters.next() {
            if let Some(delimiter) = quote {
                if character == delimiter {
                    if delimiter != ']'
                        && characters
                            .peek()
                            .is_some_and(|(_, next)| *next == delimiter)
                    {
                        characters.next();
                    } else {
                        quote = None;
                    }
                }
            } else {
                match character {
                    '"' | '\'' | '`' => quote = Some(character),
                    '[' => quote = Some(']'),
                    '.' => {
                        // SQLite qualifies the index, not the table in ON.
                        // Preserve quoted identifiers, including embedded dots.
                        return (
                            format!("{}.{}", table[..position].trim(), index),
                            table[position + 1..].trim(),
                        );
                    }
                    _ => {}
                }
            }
        }
        (index, table)
    }

    /// SQLite-backed session store.
    #[derive(Clone, Debug)]
    pub struct SqlxSqliteStore {
        pool: SqlitePool,
        table: String,
    }

    impl SqlxSqliteStore {
        pub fn new(pool: SqlitePool) -> Self {
            Self {
                pool,
                table: "saysion_sessions".to_string(),
            }
        }

        pub async fn from_url(url: &str) -> Result<Self> {
            Ok(Self::new(SqlitePool::connect(url).await?))
        }

        pub fn with_table(mut self, table: impl Into<String>) -> Self {
            self.table = table.into();
            self
        }

        pub async fn migrate(&self) -> Result {
            let sql = format!(
                "CREATE TABLE IF NOT EXISTS {table} (\
                    id TEXT PRIMARY KEY NOT NULL, \
                    expires INTEGER NULL, \
                    session TEXT NOT NULL\
                )",
                table = self.table
            );
            sqlx::query(&sql).execute(&self.pool).await?;
            let (index, table) = index_target(&self.table);
            let sql = format!(
                "CREATE INDEX IF NOT EXISTS {index} ON {table} (expires, id) WHERE expires IS NOT NULL"
            );
            sqlx::query(&sql).execute(&self.pool).await?;
            Ok(())
        }

        /// Delete at most 1,000 expired rows in one bounded batch.
        pub async fn cleanup(&self) -> Result {
            self.cleanup_batch(DEFAULT_CLEANUP_BATCH_SIZE).await?;
            Ok(())
        }

        /// Delete 1..=10,000 expired rows at most and return the number removed.
        pub async fn cleanup_batch(&self, limit: u32) -> Result<u64> {
            let limit = cleanup_limit(limit)?;
            let now = now_unix();
            let sql = format!(
                "DELETE FROM {0} WHERE expires IS NOT NULL AND expires < ? \
                 AND id IN (SELECT id FROM {0} WHERE expires IS NOT NULL AND expires < ? \
                 ORDER BY expires, id LIMIT ?)",
                self.table
            );
            Ok(sqlx::query(&sql)
                .bind(now)
                .bind(now)
                .bind(limit)
                .execute(&self.pool)
                .await?
                .rows_affected())
        }

        pub async fn count(&self) -> Result<i64> {
            let sql = format!("SELECT COUNT(*) FROM {}", self.table);
            let (n,): (i64,) = sqlx::query_as(&sql).fetch_one(&self.pool).await?;
            Ok(n)
        }
    }

    #[async_trait]
    impl SessionStore for SqlxSqliteStore {
        async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
            let id = Session::id_from_cookie_value(&cookie_value)?;
            let sql = format!(
                "SELECT session FROM {} WHERE id = ? AND (expires IS NULL OR expires >= ?)",
                self.table
            );
            let row: Option<(String,)> = sqlx::query_as(&sql)
                .bind(&id)
                .bind(now_unix())
                .fetch_optional(&self.pool)
                .await?;
            Ok(row
                .map(|(s,)| parse_session(&s))
                .transpose()?
                .flatten()
                .map(Session::mark_loaded))
        }

        async fn store_session(&self, session: Session) -> Result<Option<String>> {
            let id = session.id().to_string();
            if session.is_destroyed() {
                return Err(crate::SessionConflict.into());
            }
            let origin = session.persistence_origin()?;
            let (snapshot, revision) = session.persistence_snapshot();
            let json = serde_json::to_string(&snapshot)?;
            let expires = expiry_unix(&snapshot);
            if let Some(origin) = origin.as_deref().filter(|origin| *origin != id.as_str()) {
                let mut tx = self.pool.begin().await?;
                let delete = format!("DELETE FROM {} WHERE id = ?", self.table);
                let removed = sqlx::query(&delete).bind(origin).execute(&mut *tx).await?;
                if removed.rows_affected() != 1 {
                    return Err(crate::SessionConflict.into());
                }
                let insert = format!(
                    "INSERT INTO {} (id, expires, session) VALUES (?, ?, ?)",
                    self.table
                );
                sqlx::query(&insert)
                    .bind(&id)
                    .bind(expires)
                    .bind(&json)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_insert_error)?;
                tx.commit().await?;
            } else if origin.is_some() {
                // A loaded/saved session may only update an existing record.
                // Deletion (including clear_store) must win over a stale save.
                let update = format!(
                    "UPDATE {} SET expires = ?, session = ? WHERE id = ?",
                    self.table
                );
                let updated = sqlx::query(&update)
                    .bind(expires)
                    .bind(&json)
                    .bind(&id)
                    .execute(&self.pool)
                    .await?;
                if updated.rows_affected() != 1 {
                    return Err(crate::SessionConflict.into());
                }
            } else {
                let insert = format!(
                    "INSERT INTO {} (id, expires, session) VALUES (?, ?, ?)",
                    self.table
                );
                sqlx::query(&insert)
                    .bind(&id)
                    .bind(expires)
                    .bind(&json)
                    .execute(&self.pool)
                    .await
                    .map_err(map_insert_error)?;
            }
            session.mark_persisted();
            session.acknowledge_revision(revision);
            Ok(session.into_cookie_value())
        }

        async fn destroy_session(&self, mut session: Session) -> Result {
            let origin = session
                .persistence_origin()
                .ok()
                .flatten()
                .unwrap_or_else(|| session.id().to_owned());
            session.destroy();
            let sql = format!("DELETE FROM {} WHERE id = ?", self.table);
            sqlx::query(&sql).bind(&origin).execute(&self.pool).await?;
            Ok(())
        }

        async fn clear_store(&self) -> Result {
            let sql = format!("DELETE FROM {}", self.table);
            sqlx::query(&sql).execute(&self.pool).await?;
            Ok(())
        }
    }
}

#[cfg(feature = "sqlx-sqlite")]
pub use sqlite::SqlxSqliteStore;

// ---------- MySQL ----------

#[cfg(feature = "sqlx-mysql")]
#[allow(missing_docs)]
mod mysql {
    use super::*;
    use sqlx::MySqlPool;

    use crate::{Result, Session, SessionStore, async_trait};

    /// MySQL/MariaDB-backed session store.
    #[derive(Clone, Debug)]
    pub struct SqlxMySqlStore {
        pool: MySqlPool,
        table: String,
    }

    impl SqlxMySqlStore {
        pub fn new(pool: MySqlPool) -> Self {
            Self {
                pool,
                table: "saysion_sessions".to_string(),
            }
        }

        pub async fn from_url(url: &str) -> Result<Self> {
            Ok(Self::new(MySqlPool::connect(url).await?))
        }

        pub fn with_table(mut self, table: impl Into<String>) -> Self {
            self.table = table.into();
            self
        }

        pub async fn migrate(&self) -> Result {
            let sql = format!(
                "CREATE TABLE IF NOT EXISTS {table} (\
                    id VARCHAR(128) PRIMARY KEY NOT NULL, \
                    expires BIGINT NULL, \
                    session TEXT NOT NULL\
                )",
                table = self.table
            );
            sqlx::query(&sql).execute(&self.pool).await?;
            let sql = format!(
                "CREATE INDEX {} ON {} (expires, id)",
                expiry_index_name(&self.table),
                self.table
            );
            if let Err(error) = sqlx::query(&sql).execute(&self.pool).await {
                // MySQL has no CREATE INDEX IF NOT EXISTS. Only suppress the
                // precise duplicate-index error, including concurrent startups.
                let duplicate = error
                    .as_database_error()
                    .and_then(|error| error.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
                    .is_some_and(|error| error.number() == 1061);
                if !duplicate {
                    return Err(error.into());
                }
            }
            Ok(())
        }

        /// Delete at most 1,000 expired rows in one bounded batch.
        pub async fn cleanup(&self) -> Result {
            self.cleanup_batch(DEFAULT_CLEANUP_BATCH_SIZE).await?;
            Ok(())
        }

        /// Delete 1..=10,000 expired rows at most and return the number removed.
        pub async fn cleanup_batch(&self, limit: u32) -> Result<u64> {
            let limit = cleanup_limit(limit)?;
            let sql = format!(
                "DELETE FROM {} WHERE expires IS NOT NULL AND expires < ? \
                 ORDER BY expires, id LIMIT ?",
                self.table
            );
            Ok(sqlx::query(&sql)
                .bind(now_unix())
                .bind(limit)
                .execute(&self.pool)
                .await?
                .rows_affected())
        }

        pub async fn count(&self) -> Result<i64> {
            let sql = format!("SELECT COUNT(*) FROM {}", self.table);
            let (n,): (i64,) = sqlx::query_as(&sql).fetch_one(&self.pool).await?;
            Ok(n)
        }
    }

    #[async_trait]
    impl SessionStore for SqlxMySqlStore {
        async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
            let id = Session::id_from_cookie_value(&cookie_value)?;
            let sql = format!(
                "SELECT session FROM {} WHERE id = ? AND (expires IS NULL OR expires >= ?)",
                self.table
            );
            let row: Option<(String,)> = sqlx::query_as(&sql)
                .bind(&id)
                .bind(now_unix())
                .fetch_optional(&self.pool)
                .await?;
            Ok(row
                .map(|(s,)| parse_session(&s))
                .transpose()?
                .flatten()
                .map(Session::mark_loaded))
        }

        async fn store_session(&self, session: Session) -> Result<Option<String>> {
            let id = session.id().to_string();
            if session.is_destroyed() {
                return Err(crate::SessionConflict.into());
            }
            let origin = session.persistence_origin()?;
            let (snapshot, revision) = session.persistence_snapshot();
            let json = serde_json::to_string(&snapshot)?;
            let expires = expiry_unix(&snapshot);
            if let Some(origin) = origin.as_deref().filter(|origin| *origin != id.as_str()) {
                let mut tx = self.pool.begin().await?;
                let delete = format!("DELETE FROM {} WHERE id = ?", self.table);
                let removed = sqlx::query(&delete).bind(origin).execute(&mut *tx).await?;
                if removed.rows_affected() != 1 {
                    return Err(crate::SessionConflict.into());
                }
                let insert = format!(
                    "INSERT INTO {} (id, expires, session) VALUES (?, ?, ?)",
                    self.table
                );
                sqlx::query(&insert)
                    .bind(&id)
                    .bind(expires)
                    .bind(&json)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_insert_error)?;
                tx.commit().await?;
            } else if origin.is_some() {
                // A loaded/saved session may only update an existing record.
                // Deletion (including clear_store) must win over a stale save.
                let update = format!(
                    "UPDATE {} SET expires = ?, session = ? WHERE id = ?",
                    self.table
                );
                let updated = sqlx::query(&update)
                    .bind(expires)
                    .bind(&json)
                    .bind(&id)
                    .execute(&self.pool)
                    .await?;
                if updated.rows_affected() != 1 {
                    return Err(crate::SessionConflict.into());
                }
            } else {
                let insert = format!(
                    "INSERT INTO {} (id, expires, session) VALUES (?, ?, ?)",
                    self.table
                );
                sqlx::query(&insert)
                    .bind(&id)
                    .bind(expires)
                    .bind(&json)
                    .execute(&self.pool)
                    .await
                    .map_err(map_insert_error)?;
            }
            session.mark_persisted();
            session.acknowledge_revision(revision);
            Ok(session.into_cookie_value())
        }

        async fn destroy_session(&self, mut session: Session) -> Result {
            let origin = session
                .persistence_origin()
                .ok()
                .flatten()
                .unwrap_or_else(|| session.id().to_owned());
            session.destroy();
            let sql = format!("DELETE FROM {} WHERE id = ?", self.table);
            sqlx::query(&sql).bind(&origin).execute(&self.pool).await?;
            Ok(())
        }

        async fn clear_store(&self) -> Result {
            let sql = format!("DELETE FROM {}", self.table);
            sqlx::query(&sql).execute(&self.pool).await?;
            Ok(())
        }
    }
}

#[cfg(feature = "sqlx-mysql")]
pub use mysql::SqlxMySqlStore;

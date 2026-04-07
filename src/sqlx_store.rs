//! SQLx-backed session stores.
//!
//! Three concrete stores are exposed, each gated behind its own
//! cargo feature:
//!
//! - [`SqlxPostgresStore`] (`sqlx-postgres`)
//! - [`SqlxSqliteStore`]   (`sqlx-sqlite`)
//! - [`SqlxMySqlStore`]    (`sqlx-mysql`)
//!
//! All three use a `sqlx::Pool` internally, so connections are
//! pooled and reused across tasks. Expiry timestamps are stored as
//! signed unix-second integers (`NULL` means "no expiry"), keeping
//! the schema and queries portable across backends.
//!
//! Before first use call `migrate()` once to create the table.

use time::OffsetDateTime;

use crate::Session;

fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn expiry_unix(session: &Session) -> Option<i64> {
    session.expiry().map(|e| e.unix_timestamp())
}

fn parse_session(json: &str) -> Option<Session> {
    serde_json::from_str::<Session>(json)
        .ok()
        .and_then(Session::validate)
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
            Ok(())
        }

        /// Delete all expired session rows.
        pub async fn cleanup(&self) -> Result {
            let sql = format!(
                "DELETE FROM {} WHERE expires IS NOT NULL AND expires < $1",
                self.table
            );
            sqlx::query(&sql)
                .bind(now_unix())
                .execute(&self.pool)
                .await?;
            Ok(())
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
                "SELECT session FROM {} WHERE id = $1 AND (expires IS NULL OR expires > $2)",
                self.table
            );
            let row: Option<(String,)> = sqlx::query_as(&sql)
                .bind(&id)
                .bind(now_unix())
                .fetch_optional(&self.pool)
                .await?;
            Ok(row.and_then(|(s,)| parse_session(&s)))
        }

        async fn store_session(&self, session: Session) -> Result<Option<String>> {
            let id = session.id().to_string();
            let json = serde_json::to_string(&session)?;
            let expires = expiry_unix(&session);
            let sql = format!(
                "INSERT INTO {table} (id, expires, session) VALUES ($1, $2, $3) \
                 ON CONFLICT (id) DO UPDATE SET expires = EXCLUDED.expires, session = EXCLUDED.session",
                table = self.table
            );
            sqlx::query(&sql)
                .bind(&id)
                .bind(expires)
                .bind(&json)
                .execute(&self.pool)
                .await?;
            session.reset_data_changed();
            Ok(session.into_cookie_value())
        }

        async fn destroy_session(&self, session: Session) -> Result {
            let sql = format!("DELETE FROM {} WHERE id = $1", self.table);
            sqlx::query(&sql)
                .bind(session.id())
                .execute(&self.pool)
                .await?;
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
            Ok(())
        }

        pub async fn cleanup(&self) -> Result {
            let sql = format!(
                "DELETE FROM {} WHERE expires IS NOT NULL AND expires < ?",
                self.table
            );
            sqlx::query(&sql)
                .bind(now_unix())
                .execute(&self.pool)
                .await?;
            Ok(())
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
                "SELECT session FROM {} WHERE id = ? AND (expires IS NULL OR expires > ?)",
                self.table
            );
            let row: Option<(String,)> = sqlx::query_as(&sql)
                .bind(&id)
                .bind(now_unix())
                .fetch_optional(&self.pool)
                .await?;
            Ok(row.and_then(|(s,)| parse_session(&s)))
        }

        async fn store_session(&self, session: Session) -> Result<Option<String>> {
            let id = session.id().to_string();
            let json = serde_json::to_string(&session)?;
            let expires = expiry_unix(&session);
            let sql = format!(
                "INSERT INTO {table} (id, expires, session) VALUES (?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET expires = excluded.expires, session = excluded.session",
                table = self.table
            );
            sqlx::query(&sql)
                .bind(&id)
                .bind(expires)
                .bind(&json)
                .execute(&self.pool)
                .await?;
            session.reset_data_changed();
            Ok(session.into_cookie_value())
        }

        async fn destroy_session(&self, session: Session) -> Result {
            let sql = format!("DELETE FROM {} WHERE id = ?", self.table);
            sqlx::query(&sql)
                .bind(session.id())
                .execute(&self.pool)
                .await?;
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
            Ok(())
        }

        pub async fn cleanup(&self) -> Result {
            let sql = format!(
                "DELETE FROM {} WHERE expires IS NOT NULL AND expires < ?",
                self.table
            );
            sqlx::query(&sql)
                .bind(now_unix())
                .execute(&self.pool)
                .await?;
            Ok(())
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
                "SELECT session FROM {} WHERE id = ? AND (expires IS NULL OR expires > ?)",
                self.table
            );
            let row: Option<(String,)> = sqlx::query_as(&sql)
                .bind(&id)
                .bind(now_unix())
                .fetch_optional(&self.pool)
                .await?;
            Ok(row.and_then(|(s,)| parse_session(&s)))
        }

        async fn store_session(&self, session: Session) -> Result<Option<String>> {
            let id = session.id().to_string();
            let json = serde_json::to_string(&session)?;
            let expires = expiry_unix(&session);
            let sql = format!(
                "INSERT INTO {table} (id, expires, session) VALUES (?, ?, ?) \
                 ON DUPLICATE KEY UPDATE expires = VALUES(expires), session = VALUES(session)",
                table = self.table
            );
            sqlx::query(&sql)
                .bind(&id)
                .bind(expires)
                .bind(&json)
                .execute(&self.pool)
                .await?;
            session.reset_data_changed();
            Ok(session.into_cookie_value())
        }

        async fn destroy_session(&self, session: Session) -> Result {
            let sql = format!("DELETE FROM {} WHERE id = ?", self.table);
            sqlx::query(&sql)
                .bind(session.id())
                .execute(&self.pool)
                .await?;
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

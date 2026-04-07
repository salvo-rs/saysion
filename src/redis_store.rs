//! Redis-backed session store.
//!
//! Enabled by the `redis` cargo feature.

use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Client};

use crate::{Result, Session, SessionStore, async_trait};

const DEFAULT_PREFIX: &str = "saysion/";

/// A session store backed by Redis.
///
/// Internally uses [`redis::aio::ConnectionManager`] which provides
/// connection pooling/reuse and automatic reconnection.
#[derive(Clone)]
pub struct RedisStore {
    conn: ConnectionManager,
    prefix: String,
}

impl std::fmt::Debug for RedisStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisStore")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl RedisStore {
    /// Create a new `RedisStore` from an existing redis [`Client`].
    pub async fn new(client: Client) -> Result<Self> {
        let conn = ConnectionManager::new(client).await?;
        Ok(Self {
            conn,
            prefix: DEFAULT_PREFIX.to_string(),
        })
    }

    /// Connect to redis using a connection URL such as
    /// `redis://127.0.0.1:6379`.
    pub async fn from_url(url: &str) -> Result<Self> {
        Self::new(Client::open(url)?).await
    }

    /// Set the key prefix used for stored sessions. Defaults to
    /// `saysion/`.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    fn key(&self, id: &str) -> String {
        format!("{}{}", self.prefix, id)
    }

    /// Returns the number of stored sessions matching this store's
    /// prefix. Uses `KEYS` and is intended for tests / small
    /// deployments only.
    pub async fn count(&self) -> Result<usize> {
        let mut conn = self.conn.clone();
        let pattern = format!("{}*", self.prefix);
        let keys: Vec<String> = conn.keys(pattern).await?;
        Ok(keys.len())
    }
}

#[async_trait]
impl SessionStore for RedisStore {
    async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
        let id = Session::id_from_cookie_value(&cookie_value)?;
        tracing::debug!("loading session by id `{}`", id);
        let mut conn = self.conn.clone();
        let value: Option<String> = conn.get(self.key(&id)).await?;
        Ok(match value {
            None => None,
            Some(v) => serde_json::from_str::<Session>(&v)?.validate(),
        })
    }

    async fn store_session(&self, session: Session) -> Result<Option<String>> {
        let id = session.id().to_string();
        tracing::debug!("storing session by id `{}`", id);
        let json = serde_json::to_string(&session)?;
        let mut conn = self.conn.clone();
        match session.expires_in() {
            Some(ttl) => {
                let _: () = conn
                    .set_ex(self.key(&id), json, ttl.as_secs().max(1))
                    .await?;
            }
            None => {
                let _: () = conn.set(self.key(&id), json).await?;
            }
        }
        session.reset_data_changed();
        Ok(session.into_cookie_value())
    }

    async fn destroy_session(&self, session: Session) -> Result {
        tracing::debug!("destroying session by id `{}`", session.id());
        let mut conn = self.conn.clone();
        let _: () = conn.del(self.key(session.id())).await?;
        Ok(())
    }

    async fn clear_store(&self) -> Result {
        tracing::debug!("clearing redis store with prefix `{}`", self.prefix);
        let mut conn = self.conn.clone();
        let pattern = format!("{}*", self.prefix);
        let keys: Vec<String> = conn.keys(pattern).await?;
        if !keys.is_empty() {
            let _: () = conn.del(keys).await?;
        }
        Ok(())
    }
}

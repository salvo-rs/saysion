//! Redis-backed session store.
//!
//! Enabled by the `redis` cargo feature.

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{AsyncCommands, Client};

use crate::{Result, Session, SessionStore, async_trait};

const DEFAULT_PREFIX: &str = "saysion/";
const SCAN_BATCH_SIZE: usize = 256;

fn prefix_pattern(prefix: &str) -> String {
    let mut pattern = String::with_capacity(prefix.len() + 1);
    for character in prefix.chars() {
        if matches!(character, '*' | '?' | '[' | ']' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('*');
    pattern
}

fn ttl_millis(ttl: std::time::Duration) -> Result<u64> {
    Ok(u64::try_from(ttl.as_nanos().div_ceil(1_000_000))?.max(1))
}

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
        Self::new_with_config(client, ConnectionManagerConfig::default()).await
    }

    /// Create a store with explicit connection, response timeout and retry settings.
    ///
    /// The default manager has no connection or response timeout. Applications
    /// should choose deadlines appropriate for their request budget; an outer
    /// request deadline can also bound the complete operation, including retries.
    pub async fn new_with_config(client: Client, config: ConnectionManagerConfig) -> Result<Self> {
        let conn = ConnectionManager::new_with_config(client, config).await?;
        Ok(Self::from_connection_manager(conn))
    }

    /// Use an existing manager, preserving its timeout and reconnection settings.
    pub fn from_connection_manager(conn: ConnectionManager) -> Self {
        Self {
            conn,
            prefix: DEFAULT_PREFIX.to_string(),
        }
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

    /// Count sessions using incremental `SCAN` calls.
    ///
    /// The prefix is matched literally, including Redis glob characters.
    /// The count is an estimate while keys are concurrently modified, since
    /// `SCAN` can return duplicates or omit keys added during iteration.
    pub async fn count(&self) -> Result<usize> {
        let mut conn = self.conn.clone();
        let pattern = prefix_pattern(&self.prefix);
        let mut cursor = 0;
        let mut total = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(SCAN_BATCH_SIZE)
                .query_async(&mut conn)
                .await?;
            total += keys
                .iter()
                .filter(|key| key.starts_with(&self.prefix))
                .count();
            cursor = next;
            if cursor == 0 {
                return Ok(total);
            }
        }
    }
}

#[async_trait]
impl SessionStore for RedisStore {
    async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
        let id = Session::id_from_cookie_value(&cookie_value)?;
        tracing::debug!("loading session by id `{}`", id);
        let mut conn = self.conn.clone();
        let value: Option<String> = conn.get(self.key(&id)).await?;
        match value {
            None => Ok(None),
            Some(v) => serde_json::from_str::<Session>(&v)?.validate_loaded(&cookie_value),
        }
    }

    async fn store_session(&self, session: Session) -> Result<Option<String>> {
        let id = session.id().to_string();
        tracing::debug!("storing session by id `{}`", id);
        let attempt = session.persistence_attempt()?;
        let origin = attempt.origin.clone();
        if session.is_destroyed() {
            return Err(crate::SessionConflict.into());
        }
        let (snapshot, revision) = session.persistence_snapshot();
        let json = serde_json::to_string(&snapshot)?;
        let mut conn = self.conn.clone();
        if let Some(old_id) = origin.as_deref().filter(|old_id| *old_id != id) {
            let ttl = match snapshot.expiry() {
                Some(_) => {
                    let Some(ttl) = snapshot.expires_in() else {
                        // Expired rotations revoke the source without creating
                        // a permanent replacement for an already-expired session.
                        let _: () = conn.del(self.key(old_id)).await?;
                        return Ok(None);
                    };
                    ttl_millis(ttl)?
                }
                None => 0,
            };
            let rotated: i64 = redis::cmd("EVAL")
                .arg(
                    r#"
                    if redis.call('EXISTS', KEYS[1]) == 0 or redis.call('EXISTS', KEYS[2]) == 1 then
                        return 0
                    end
                    if tonumber(ARGV[2]) > 0 then
                        redis.call('SET', KEYS[2], ARGV[1], 'PX', ARGV[2])
                    else
                        redis.call('SET', KEYS[2], ARGV[1])
                    end
                    redis.call('DEL', KEYS[1])
                    return 1
                "#,
                )
                .arg(2)
                .arg(self.key(old_id))
                .arg(self.key(&id))
                .arg(&json)
                .arg(ttl)
                .query_async(&mut conn)
                .await?;
            if rotated == 0 {
                return Err(crate::SessionConflict.into());
            }
            session.mark_persisted();
            session.acknowledge_revision(revision);
            return Ok(session.into_cookie_value());
        }
        let mut command = redis::cmd("SET");
        command.arg(self.key(&id)).arg(&json);
        let ttl = snapshot.expires_in();
        if snapshot.expiry().is_some() && ttl.is_none() {
            let _: () = conn.del(self.key(&id)).await?;
            return Ok(None);
        }
        if let Some(ttl) = ttl {
            let millis = ttl_millis(ttl)?;
            command.arg("PX").arg(millis);
        }
        command.arg(if origin.is_some() { "XX" } else { "NX" });
        let saved: Option<String> = command.query_async(&mut conn).await?;
        if saved.is_none() {
            return Err(crate::SessionConflict.into());
        }
        session.mark_persisted();
        session.acknowledge_revision(revision);
        Ok(session.into_cookie_value())
    }

    async fn destroy_session(&self, mut session: Session) -> Result {
        tracing::debug!("destroying session by id `{}`", session.id());
        let origin = session
            .persistence_origin()
            .ok()
            .flatten()
            .unwrap_or_else(|| session.id().to_owned());
        session.destroy();
        let mut conn = self.conn.clone();
        let _: () = conn.del(self.key(&origin)).await?;
        Ok(())
    }

    /// Clear this prefix using incremental scans and bounded `UNLINK` batches.
    ///
    /// Requires Redis 4 or later. This operation is not atomic: writes made
    /// during the scan may survive or be removed. Quiesce writers if a complete
    /// clear is required. An empty prefix intentionally matches the whole DB.
    async fn clear_store(&self) -> Result {
        tracing::debug!("clearing redis store with prefix `{}`", self.prefix);
        let mut conn = self.conn.clone();
        let pattern = prefix_pattern(&self.prefix);
        let mut cursor = 0;
        loop {
            let (next, mut keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(SCAN_BATCH_SIZE)
                .query_async(&mut conn)
                .await?;
            keys.retain(|key| key.starts_with(&self.prefix));
            // COUNT is a scan hint, not a strict response-size guarantee.
            for batch in keys.chunks(SCAN_BATCH_SIZE) {
                let _: usize = redis::cmd("UNLINK")
                    .arg(batch)
                    .query_async(&mut conn)
                    .await?;
            }
            cursor = next;
            if cursor == 0 {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn redis_ttl_rounds_up_to_milliseconds() {
        assert_eq!(ttl_millis(Duration::ZERO).unwrap(), 1);
        assert_eq!(ttl_millis(Duration::from_nanos(1)).unwrap(), 1);
        assert_eq!(ttl_millis(Duration::from_micros(1_001)).unwrap(), 2);
        assert_eq!(ttl_millis(Duration::from_millis(1_999)).unwrap(), 1_999);
    }

    #[test]
    fn scan_prefix_escapes_glob_metacharacters() {
        assert_eq!(prefix_pattern(r"app*?[]\/"), r"app\*\?\[\]\\/*");
        assert_eq!(prefix_pattern("plain/"), "plain/*");
        assert_eq!(prefix_pattern(""), "*");
    }
}

//! MongoDB-backed session store.
//!
//! Enabled by the `mongodb` cargo feature.

use mongodb::bson::{DateTime as BsonDateTime, doc};
use mongodb::options::IndexOptions;
use mongodb::{Client, Collection, IndexModel};
use serde::{Deserialize, Serialize};

use crate::{Result, Session, SessionStore, async_trait};

const DEFAULT_COLLECTION: &str = "saysion_sessions";

fn expiry_datetime(expiry: &time::OffsetDateTime) -> BsonDateTime {
    // Round up so MongoDB's TTL monitor cannot delete a still-valid session.
    let millis =
        expiry.unix_timestamp() * 1000 + i64::from(expiry.nanosecond().div_ceil(1_000_000));
    BsonDateTime::from_millis(millis)
}

/// A session store backed by MongoDB.
///
/// The official `mongodb` driver maintains its own connection pool
/// internally, so a single [`Client`] (and therefore a single
/// `MongoDbStore`) can be cloned cheaply and shared between tasks.
#[derive(Clone)]
pub struct MongoDbStore {
    client: Client,
    db: String,
    coll_name: String,
}

impl std::fmt::Debug for MongoDbStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Driver feature unification can enable options whose Debug includes
        // credentials (for example SOCKS5 proxy authentication). Never expand
        // the client, even if a particular driver's ordinary credentials redact.
        f.debug_struct("MongoDbStore")
            .field("db", &self.db)
            .field("coll_name", &self.coll_name)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct SessionDoc {
    #[serde(rename = "_id")]
    id: String,
    expires_at: Option<BsonDateTime>,
    session: String,
}

impl MongoDbStore {
    /// Create a store from an existing [`Client`].
    pub fn new(client: Client, db: impl Into<String>) -> Self {
        Self {
            client,
            db: db.into(),
            coll_name: DEFAULT_COLLECTION.to_string(),
        }
    }

    /// Connect using a MongoDB URI such as `mongodb://localhost:27017`.
    pub async fn from_uri(uri: &str, db: impl Into<String>) -> Result<Self> {
        Ok(Self::new(Client::with_uri_str(uri).await?, db))
    }

    /// Use a custom collection name. Defaults to `saysion_sessions`.
    pub fn with_collection(mut self, coll: impl Into<String>) -> Self {
        self.coll_name = coll.into();
        self
    }

    fn coll(&self) -> Collection<SessionDoc> {
        self.client.database(&self.db).collection(&self.coll_name)
    }

    /// Create a TTL index on `expires_at` so MongoDB will purge
    /// expired sessions automatically. Idempotent — safe to call on
    /// every startup.
    pub async fn initialize(&self) -> Result {
        let model = IndexModel::builder()
            .keys(doc! { "expires_at": 1 })
            .options(
                IndexOptions::builder()
                    .expire_after(std::time::Duration::from_secs(0))
                    .build(),
            )
            .build();
        self.coll().create_index(model).await?;
        Ok(())
    }

    /// Returns the number of stored sessions.
    pub async fn count(&self) -> Result<u64> {
        Ok(self.coll().count_documents(doc! {}).await?)
    }
}

#[async_trait]
impl SessionStore for MongoDbStore {
    async fn load_session(&self, cookie_value: String) -> Result<Option<Session>> {
        let id = Session::id_from_cookie_value(&cookie_value)?;
        tracing::debug!("loading session by id `{}`", id);
        let found = self.coll().find_one(doc! { "_id": &id }).await?;
        match found {
            None => Ok(None),
            Some(d) => serde_json::from_str::<Session>(&d.session)?.validate_loaded(&cookie_value),
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
        let expires_at = snapshot.expiry().map(expiry_datetime);
        let doc_value = SessionDoc {
            id: id.clone(),
            expires_at,
            session: json,
        };
        let mut write = session.begin_backend_write(&origin, true).await?;
        if let Some(old_id) = origin.as_deref().filter(|old_id| *old_id != id) {
            // Standalone MongoDB has no multi-document transactions. Revoke
            // first, so any failure leaves the old credential unusable.
            if self
                .coll()
                .delete_one(doc! { "_id": old_id })
                .await?
                .deleted_count
                == 0
            {
                return Err(crate::SessionConflict.into());
            }
            self.coll().insert_one(&doc_value).await?;
        } else {
            if origin.is_some() {
                let result = self
                    .coll()
                    .replace_one(doc! { "_id": &id }, &doc_value)
                    .await?;
                if result.matched_count != 1 {
                    return Err(crate::SessionConflict.into());
                }
            } else {
                self.coll().insert_one(&doc_value).await?;
            }
        }
        if let Err(error) = write.finish() {
            write.require_cleanup();
            self.coll().delete_one(doc! { "_id": &id }).await?;
            write.confirm_removed();
            return Err(error);
        }
        session.mark_persisted();
        session.acknowledge_revision(revision);
        Ok(session.into_cookie_value())
    }

    async fn destroy_session(&self, mut session: Session) -> Result {
        tracing::debug!("destroying session by id `{}`", session.id());
        let deletion = session.begin_delete().await?;
        self.coll()
            .delete_one(doc! { "_id": deletion.id() })
            .await?;
        Ok(())
    }

    async fn clear_store(&self) -> Result {
        tracing::debug!("clearing mongodb store");
        self.coll().delete_many(doc! {}).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_timestamp_rounds_up_including_before_epoch() {
        for (nanos, millis) in [
            (0, 0),
            (1, 1),
            (1_000_000, 1),
            (1_000_001, 2),
            (999_999_999, 1000),
            (-1, 0),
            (-1_000_001, -1),
        ] {
            let expiry = time::OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap();
            assert_eq!(expiry_datetime(&expiry).timestamp_millis(), millis);
        }
    }
}

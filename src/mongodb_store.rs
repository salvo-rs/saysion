//! MongoDB-backed session store.
//!
//! Enabled by the `mongodb` cargo feature.

use mongodb::bson::{DateTime as BsonDateTime, doc};
use mongodb::options::IndexOptions;
use mongodb::{Client, Collection, IndexModel};
use serde::{Deserialize, Serialize};

use crate::{Result, Session, SessionStore, async_trait};

const DEFAULT_COLLECTION: &str = "saysion_sessions";

/// A session store backed by MongoDB.
///
/// The official `mongodb` driver maintains its own connection pool
/// internally, so a single [`Client`] (and therefore a single
/// `MongoDbStore`) can be cloned cheaply and shared between tasks.
#[derive(Clone, Debug)]
pub struct MongoDbStore {
    client: Client,
    db: String,
    coll_name: String,
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
        Ok(match found {
            None => None,
            Some(d) => serde_json::from_str::<Session>(&d.session)?.validate(),
        })
    }

    async fn store_session(&self, session: Session) -> Result<Option<String>> {
        let id = session.id().to_string();
        tracing::debug!("storing session by id `{}`", id);
        let json = serde_json::to_string(&session)?;
        let expires_at = session
            .expiry()
            .map(|e| BsonDateTime::from_millis(e.unix_timestamp() * 1000));
        let doc_value = SessionDoc {
            id: id.clone(),
            expires_at,
            session: json,
        };
        self.coll()
            .replace_one(doc! { "_id": &id }, &doc_value)
            .upsert(true)
            .await?;
        session.reset_data_changed();
        Ok(session.into_cookie_value())
    }

    async fn destroy_session(&self, session: Session) -> Result {
        tracing::debug!("destroying session by id `{}`", session.id());
        self.coll().delete_one(doc! { "_id": session.id() }).await?;
        Ok(())
    }

    async fn clear_store(&self) -> Result {
        tracing::debug!("clearing mongodb store");
        self.coll().delete_many(doc! {}).await?;
        Ok(())
    }
}

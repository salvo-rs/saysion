//! Async HTTP sessions.
//!
//! This crate provides a generic interface between cookie values and
//! storage backends to create a concept of sessions. It provides an
//! interface that can be used to encode and store sessions, and
//! decode and load sessions generating cookies in the process.
//!
//! # Example
//!
//! ```
//! use saysion::{Session, SessionStore, MemoryStore};
//!
//! # #[tokio::main]
//! # async fn main() -> saysion::Result {
//! #
//! // Init a new session store we can persist sessions to.
//! let mut store = MemoryStore::new();
//!
//! // Create a new session.
//! let mut session = Session::new();
//! session.insert("user_id", 1)?;
//! assert!(session.data_changed());
//!
//! // retrieve the cookie value to store in a session cookie
//! let cookie_value = store.store_session(session).await?.unwrap();
//!
//! // Retrieve the session using the cookie.
//! let session = store.load_session(cookie_value).await?.unwrap();
//! assert_eq!(session.get::<usize>("user_id").unwrap(), 1);
//! assert!(!session.data_changed());
//! #
//! # Ok(()) }
//! ```

// #![forbid(unsafe_code, future_incompatible)]
// #![deny(missing_debug_implementations, nonstandard_style)]
// #![warn(missing_docs, missing_doc_code_examples, unreachable_pub)]
#![forbid(unsafe_code)]
#![deny(
    future_incompatible,
    missing_debug_implementations,
    nonstandard_style,
    missing_docs,
    unreachable_pub,
    missing_copy_implementations,
    unused_qualifications
)]

pub use anyhow::Error;
pub use async_trait::async_trait;
/// An anyhow::Result with default return type of ()
pub type Result<T = ()> = std::result::Result<T, Error>;

mod cookie_store;
mod memory_store;
mod session;
mod session_store;

#[cfg(feature = "mongodb")]
mod mongodb_store;
#[cfg(feature = "redis")]
mod redis_store;
#[cfg(any(
    feature = "sqlx-postgres",
    feature = "sqlx-sqlite",
    feature = "sqlx-mysql"
))]
mod sqlx_store;

pub use cookie_store::CookieStore;
pub use memory_store::MemoryStore;
pub use session::Session;
pub use session_store::SessionStore;

#[cfg(feature = "mongodb")]
pub use mongodb_store::MongoDbStore;
#[cfg(feature = "redis")]
pub use redis_store::RedisStore;
#[cfg(feature = "sqlx-mysql")]
pub use sqlx_store::SqlxMySqlStore;
#[cfg(feature = "sqlx-postgres")]
pub use sqlx_store::SqlxPostgresStore;
#[cfg(feature = "sqlx-sqlite")]
pub use sqlx_store::SqlxSqliteStore;

pub use base64;
pub use blake3;
pub use hmac;
pub use sha2;
pub use time;

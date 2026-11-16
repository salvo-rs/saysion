#![cfg(feature = "mongodb")]
//! A real-driver replica-set protocol fixture; no MongoDB service is required.

use mongodb::bson::{self, Document, doc};
use saysion::{MongoDbStore, Session, SessionStore};
use std::{
    io,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

async fn command(socket: &mut TcpStream) -> io::Result<(i32, i32, Document)> {
    let len = socket.read_i32_le().await?;
    let request_id = socket.read_i32_le().await?;
    socket.read_i32_le().await?;
    let opcode = socket.read_i32_le().await?;
    let mut bytes = vec![0; usize::try_from(len - 16).unwrap()];
    socket.read_exact(&mut bytes).await?;
    let offset = match opcode {
        2013 => 5,
        2004 => 4 + bytes[4..].iter().position(|b| *b == 0).unwrap() + 1 + 8,
        other => panic!("unsupported opcode {other}"),
    };
    let value = Document::from_reader(&mut &bytes[offset..]).unwrap();
    Ok((request_id, opcode, value))
}

async fn reply(
    socket: &mut TcpStream,
    request: i32,
    opcode: i32,
    value: Document,
) -> io::Result<()> {
    let bytes = bson::to_vec(&value).unwrap();
    let mut body = Vec::new();
    body.extend_from_slice(&0_i32.to_le_bytes());
    if opcode == 2004 {
        body.extend_from_slice(&0_i64.to_le_bytes());
        body.extend_from_slice(&0_i32.to_le_bytes());
        body.extend_from_slice(&1_i32.to_le_bytes());
    } else {
        body.push(0);
    }
    body.extend_from_slice(&bytes);
    socket
        .write_i32_le(i32::try_from(body.len() + 16).unwrap())
        .await?;
    socket.write_i32_le(request + 1000).await?;
    socket.write_i32_le(request).await?;
    socket
        .write_i32_le(if opcode == 2004 { 1 } else { 2013 })
        .await?;
    socket.write_all(&body).await
}

async fn member(
    listener: TcpListener,
    primary: bool,
    hosts: Vec<String>,
    record: Document,
    deleted: Arc<AtomicBool>,
) {
    let me = listener.local_addr().unwrap().to_string();
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((mut socket, _)) = accepted else { break };
                let hosts = hosts.clone();
                let me = me.clone();
                let record = record.clone();
                let deleted = deleted.clone();
                tasks.spawn(async move {
                    while let Ok((request, opcode, command)) = command(&mut socket).await {
                        let name = command.keys().next().unwrap().to_ascii_lowercase();
                        let response = match name.as_str() {
                            "hello" | "ismaster" => doc! {
                                "ok": 1.0,
                                "isWritablePrimary": primary,
                                "ismaster": primary,
                                "secondary": !primary,
                                "setName": "audit",
                                "hosts": &hosts,
                                "primary": &hosts[0],
                                "me": &me,
                                "minWireVersion": 0,
                                "maxWireVersion": 17,
                                "maxBsonObjectSize": 16_777_216,
                                "maxMessageSizeBytes": 48_000_000,
                                "maxWriteBatchSize": 100_000,
                            },
                            "find" => {
                                let batch = if primary && deleted.load(Ordering::SeqCst) {
                                    vec![]
                                } else {
                                    vec![record.clone()]
                                };
                                doc! { "ok": 1.0, "cursor": {
                                    "id": 0_i64,
                                    "ns": "audit.saysion_sessions",
                                    "firstBatch": batch,
                                }}
                            },
                            "delete" => {
                                assert!(primary, "driver must send writes to primary");
                                deleted.store(true, Ordering::SeqCst);
                                doc! { "ok": 1.0, "n": 1 }
                            },
                            _ => doc! { "ok": 1.0 },
                        };
                        if reply(&mut socket, request, opcode, response).await.is_err() {
                            break;
                        }
                    }
                });
            },
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}

#[tokio::test]
async fn secondary_client_preference_cannot_reload_a_revoked_session() {
    let mut session = Session::new();
    session.insert("user_id", 42).unwrap();
    let record = doc! {
        "_id": session.id(),
        "expires_at": bson::Bson::Null,
        "session": serde_json::to_string(&session).unwrap(),
    };
    let cookie = session.into_cookie_value().unwrap();
    let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let secondary = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hosts = vec![
        primary.local_addr().unwrap().to_string(),
        secondary.local_addr().unwrap().to_string(),
    ];
    let url = format!(
        "mongodb://{}/?replicaSet=audit&readPreference=secondary&serverSelectionTimeoutMS=2000",
        hosts.join(",")
    );
    let deleted = Arc::new(AtomicBool::new(false));
    let primary_task = tokio::spawn(member(
        primary,
        true,
        hosts.clone(),
        record.clone(),
        deleted.clone(),
    ));
    let secondary_task = tokio::spawn(member(secondary, false, hosts, record, deleted.clone()));
    let store = MongoDbStore::from_uri(&url, "audit").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let loaded = store.load_session(cookie.clone()).await.unwrap().unwrap();
        store.destroy_session(loaded).await.unwrap();
        assert!(
            deleted.load(Ordering::SeqCst),
            "primary acknowledged delete"
        );
        assert!(store.load_session(cookie).await.unwrap().is_none());
    })
    .await
    .unwrap();
    primary_task.abort();
    secondary_task.abort();
}

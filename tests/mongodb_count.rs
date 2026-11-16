#![cfg(feature = "mongodb")]
//! Verify the exact indexed-count command through the actual MongoDB driver.

use mongodb::bson::{self, Document, doc};
use saysion::MongoDbStore;
use std::{io, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

struct Peer {
    url: String,
    commands: mpsc::UnboundedReceiver<Document>,
    task: JoinHandle<()>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

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

async fn peer() -> io::Result<Peer> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!(
        "mongodb://{}/?directConnection=true&serverSelectionTimeoutMS=2000",
        listener.local_addr()?
    );
    let (sender, commands) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut socket, _)) = accepted else { break };
                    let sender = sender.clone();
                    tasks.spawn(async move {
                        while let Ok((request, opcode, command)) = command(&mut socket).await {
                            let name = command.keys().next().unwrap().to_ascii_lowercase();
                            let response = match name.as_str() {
                                "hello" | "ismaster" => doc! {
                                    "ok": 1.0, "isWritablePrimary": true, "ismaster": true,
                                    "minWireVersion": 0, "maxWireVersion": 17,
                                    "maxBsonObjectSize": 16_777_216,
                                    "maxMessageSizeBytes": 48_000_000,
                                    "maxWriteBatchSize": 100_000,
                                },
                                "aggregate" => {
                                    let collection = command.get_str("aggregate").unwrap().to_owned();
                                    sender.send(command).unwrap();
                                    doc! { "ok": 1.0, "cursor": {
                                        "id": 0_i64, "ns": format!("audit.{collection}"),
                                        "firstBatch": [{ "n": 7_i64 }],
                                    }}
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
    });
    Ok(Peer {
        url,
        commands,
        task,
    })
}

#[tokio::test]
async fn exact_counts_use_id_index_and_keep_cloned_collection_names() -> saysion::Result {
    let mut peer = peer().await?;
    let original = MongoDbStore::from_uri(&peer.url, "audit").await?;
    let renamed = original.clone().with_collection("custom_sessions");
    for (store, expected_name) in [
        (&renamed, "custom_sessions"),
        (&original, "saysion_sessions"),
    ] {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), store.count()).await??,
            7
        );
        let command = tokio::time::timeout(Duration::from_secs(2), peer.commands.recv())
            .await?
            .unwrap();
        assert_eq!(command.get_str("aggregate")?, expected_name);
        assert_eq!(command.get_str("$db")?, "audit");
        assert_eq!(command.get_str("hint")?, "_id_");
        assert!(
            command.get_array("pipeline")?.iter().any(|stage| {
                stage
                    .as_document()
                    .is_some_and(|doc| doc.contains_key("$group"))
            }),
            "count must remain an exact aggregation rather than metadata estimate"
        );
    }
    Ok(())
}

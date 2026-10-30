#![cfg(feature = "redis")]

use std::io;
use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use saysion::{RedisStore, Session, SessionStore};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

struct UnresponsiveRedis {
    url: String,
    task: JoinHandle<()>,
}

impl Drop for UnresponsiveRedis {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_command(reader: &mut BufReader<TcpStream>) -> io::Result<Vec<Vec<u8>>> {
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let count: usize = line.trim().strip_prefix('*').unwrap().parse().unwrap();
    let mut args = Vec::new();
    for _ in 0..count {
        line.clear();
        reader.read_line(&mut line).await?;
        let size: usize = line.trim().strip_prefix('$').unwrap().parse().unwrap();
        let mut arg = vec![0; size];
        reader.read_exact(&mut arg).await?;
        let mut ending = [0; 2];
        reader.read_exact(&mut ending).await?;
        args.push(arg);
    }
    Ok(args)
}

async fn unresponsive_redis() -> io::Result<UnresponsiveRedis> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("redis://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let mut peers = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((socket, _)) = accepted else { break };
                    peers.spawn(async move {
                        let mut reader = BufReader::new(socket);
                        while let Ok(args) = read_command(&mut reader).await {
                            if args[0].eq_ignore_ascii_case(b"GET") {
                                // The connection remains alive after a successful
                                // handshake, but this command never receives a reply.
                                std::future::pending::<()>().await;
                            }
                            if reader.get_mut().write_all(b"+OK\r\n").await.is_err() {
                                break;
                            }
                        }
                    });
                }
                _ = peers.join_next(), if !peers.is_empty() => {}
            }
        }
    });
    Ok(UnresponsiveRedis { url, task })
}

async fn assert_response_timeout(store: RedisStore) -> saysion::Result {
    let cookie = Session::new().into_cookie_value().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), store.load_session(cookie))
        .await
        .expect("configured Redis response timeout must finish before the outer deadline");
    let error = result.expect_err("the peer never answered GET");
    let redis_error = error.downcast_ref::<redis::RedisError>().unwrap();
    assert!(redis_error.is_timeout(), "unexpected error: {redis_error}");
    Ok(())
}

fn config() -> ConnectionManagerConfig {
    ConnectionManagerConfig::new()
        .set_connection_timeout(Duration::from_secs(2))
        .set_response_timeout(Duration::from_millis(100))
}

#[tokio::test]
async fn configured_store_times_out_after_handshake_without_external_service() -> saysion::Result {
    let peer = unresponsive_redis().await?;
    let store =
        RedisStore::new_with_config(redis::Client::open(peer.url.as_str())?, config()).await?;
    assert_response_timeout(store).await
}

#[tokio::test]
async fn supplied_manager_preserves_response_timeout() -> saysion::Result {
    let peer = unresponsive_redis().await?;
    let manager =
        ConnectionManager::new_with_config(redis::Client::open(peer.url.as_str())?, config())
            .await?;
    assert_response_timeout(RedisStore::from_connection_manager(manager)).await
}

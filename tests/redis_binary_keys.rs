#![cfg(feature = "redis")]
//! Redis key names are binary-safe, including during management operations.

use std::io;
use std::time::Duration;

use redis::aio::ConnectionManagerConfig;
use saysion::{RedisStore, SessionStore};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

struct BinaryKeyPeer {
    url: String,
    unlinked: mpsc::UnboundedReceiver<Vec<Vec<u8>>>,
    task: JoinHandle<()>,
}

impl Drop for BinaryKeyPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn peer(keys: Vec<Vec<u8>>, pattern: &'static str) -> io::Result<BinaryKeyPeer> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("redis://{}", listener.local_addr()?);
    let (sender, unlinked) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(socket);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let count: usize = line.trim().strip_prefix('*').unwrap().parse().unwrap();
            let mut args = Vec::new();
            for _ in 0..count {
                line.clear();
                reader.read_line(&mut line).await.unwrap();
                let size: usize = line.trim().strip_prefix('$').unwrap().parse().unwrap();
                let mut arg = vec![0; size];
                reader.read_exact(&mut arg).await.unwrap();
                reader.read_exact(&mut [0; 2]).await.unwrap();
                args.push(arg);
            }
            let response = if args[0].eq_ignore_ascii_case(b"SCAN") {
                assert_eq!(args[3], pattern.as_bytes());
                let mut response = format!("*2\r\n$1\r\n0\r\n*{}\r\n", keys.len()).into_bytes();
                for key in &keys {
                    response.extend_from_slice(format!("${}\r\n", key.len()).as_bytes());
                    response.extend_from_slice(key);
                    response.extend_from_slice(b"\r\n");
                }
                response
            } else if args[0].eq_ignore_ascii_case(b"UNLINK") {
                sender.send(args[1..].to_vec()).unwrap();
                format!(":{}\r\n", args.len() - 1).into_bytes()
            } else {
                b"+OK\r\n".to_vec()
            };
            if reader.get_mut().write_all(&response).await.is_err() {
                break;
            }
        }
    });
    Ok(BinaryKeyPeer {
        url,
        unlinked,
        task,
    })
}

async fn assert_binary_keys(prefix: &str, pattern: &'static str) -> saysion::Result {
    let keys = vec![
        [prefix.as_bytes(), b"\xff\x00"].concat(),
        [prefix.as_bytes(), b"ascii"].concat(),
    ];
    let mut peer = peer(keys.clone(), pattern).await?;
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(Duration::from_secs(2))
        .set_response_timeout(Duration::from_secs(2));
    let store = RedisStore::new_with_config(redis::Client::open(peer.url.as_str())?, config)
        .await?
        .with_prefix(prefix);
    assert_eq!(store.count().await?, keys.len());
    store.clear_store().await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), peer.unlinked.recv())
            .await?
            .unwrap(),
        keys
    );
    Ok(())
}

#[tokio::test]
async fn whole_database_management_preserves_binary_key_bytes() -> saysion::Result {
    assert_binary_keys("", "*").await
}

#[tokio::test]
async fn literal_prefix_management_preserves_binary_key_suffixes() -> saysion::Result {
    assert_binary_keys("app*/", r"app\*/*").await
}

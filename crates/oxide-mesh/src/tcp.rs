//! JSON-line framed TCP transport.
//!
//! `TcpMesh` is a thin server: it accepts connections, frames messages as one
//! JSON-encoded [`PeerMessage`] per line, and forwards each into the supplied
//! handler. Outbound dispatch happens through the same [`LocalMesh`] used by
//! in-process peers, so a TCP peer behaves identically to a local one once
//! its `Hello` has been processed.
//!
//! The transport is intentionally not encrypted / authenticated; production
//! deployments should put it behind TLS or run it inside a trusted network.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::error::Result;
use crate::local::LocalMesh;
use crate::message::PeerMessage;

/// TCP mesh server bound to a [`LocalMesh`] for routing.
pub struct TcpMesh {
    local: LocalMesh,
}

impl TcpMesh {
    /// Build a TCP wrapper around `local`.
    pub fn new(local: LocalMesh) -> Self {
        Self { local }
    }

    /// Bind to `addr` and serve connections until cancellation.
    pub async fn serve(self, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await?;
        tracing::info!(%addr, "tcp mesh listening");
        loop {
            let (socket, peer) = listener.accept().await?;
            tracing::debug!(?peer, "tcp peer connected");
            let local = self.local.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(socket, local).await {
                    tracing::warn!(?peer, ?e, "tcp connection ended");
                }
            });
        }
    }

    /// Connect to a remote `addr`, send `hello` immediately, and return a
    /// [`TcpClient`] that can transmit further messages on the same socket.
    pub async fn connect(addr: SocketAddr, hello: PeerMessage) -> Result<TcpClient> {
        let mut socket = TcpStream::connect(addr).await?;
        let line = format!("{}\n", serde_json::to_string(&hello)?);
        socket.write_all(line.as_bytes()).await?;
        Ok(TcpClient {
            inner: Arc::new(Mutex::new(socket)),
        })
    }
}

/// Client handle returned by [`TcpMesh::connect`].
#[derive(Clone)]
pub struct TcpClient {
    inner: Arc<Mutex<TcpStream>>,
}

impl TcpClient {
    /// Send `msg` as a single JSON line.
    pub async fn send(&self, msg: &PeerMessage) -> Result<()> {
        let line = format!("{}\n", serde_json::to_string(msg)?);
        self.inner.lock().await.write_all(line.as_bytes()).await?;
        Ok(())
    }
}

async fn handle_connection(socket: TcpStream, local: LocalMesh) -> Result<()> {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    let mut sender_id: Option<String> = None;
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: PeerMessage = match serde_json::from_str(trimmed) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(?e, "discarding malformed line");
                continue;
            }
        };
        // Auto-join on first Hello so the peer participates in routing.
        if let PeerMessage::Hello {
            from,
            capabilities,
        } = &msg
        {
            sender_id = Some(from.clone());
            let _ = local
                .join(from.clone(), capabilities.clone(), Vec::new())
                .await;
        }
        // Forward to whichever local handle is registered for the sender; if
        // no handle exists yet (Hello hasn't run for this connection) we
        // route through a fresh ephemeral handle.
        let sender = sender_id.clone().unwrap_or_else(|| msg.sender().clone());
        let (_p, handle) = local
            .join(format!("ephemeral:{sender}"), Vec::new(), Vec::new())
            .await?;
        handle.publish(msg).await?;
        local.leave(&handle.id).await?;
    }
    if let Some(id) = sender_id {
        let _ = local.leave(&id).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::PeerCapability;
    use serde_json::json;
    use std::net::Ipv4Addr;

    fn caps(name: &str) -> Vec<PeerCapability> {
        vec![PeerCapability {
            name: name.into(),
            version: None,
        }]
    }

    #[tokio::test]
    async fn tcp_round_trip_delivers_broadcast() {
        let local = LocalMesh::new();
        let (mut listener_handle, _h) =
            local.join("listener", caps("x"), vec![]).await.unwrap();

        let server = TcpMesh::new(local.clone());
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        // Spawn an accept loop manually so we can shut down cleanly.
        let local_clone = local.clone();
        let accept_task = tokio::spawn(async move {
            let _ = server; // server.serve binds again; we use the explicit listener instead
            loop {
                match listener.accept().await {
                    Ok((socket, _)) => {
                        let local = local_clone.clone();
                        tokio::spawn(async move {
                            let _ = handle_connection(socket, local).await;
                        });
                    }
                    Err(_) => break,
                }
            }
        });

        let hello = PeerMessage::Hello {
            from: "remote".into(),
            capabilities: caps("remote"),
        };
        let client = TcpMesh::connect(addr, hello).await.unwrap();
        client
            .send(&PeerMessage::broadcast("remote", "topic", json!({"v": 1})))
            .await
            .unwrap();

        // Drain non-Broadcast messages (Hello may arrive first because the
        // server auto-joins the remote peer and republishes the Hello).
        let mut saw_broadcast = false;
        for _ in 0..6 {
            let recv = tokio::time::timeout(
                std::time::Duration::from_millis(400),
                listener_handle.receiver.recv(),
            )
            .await;
            match recv {
                Ok(Some(PeerMessage::Broadcast { from, topic, .. })) => {
                    assert_eq!(from, "remote");
                    assert_eq!(topic, "topic");
                    saw_broadcast = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(saw_broadcast, "expected a Broadcast to arrive");
        accept_task.abort();
    }
}

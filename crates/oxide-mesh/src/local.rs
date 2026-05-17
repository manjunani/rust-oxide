//! In-process mesh built on `tokio::sync::mpsc` channels.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};

use crate::error::{MeshError, Result};
use crate::message::{PeerCapability, PeerId, PeerMessage};

/// Per-peer channel handle.
pub struct PeerHandle {
    /// Receiver for messages routed to this peer (Direct + Broadcast +
    /// Tasks). Tests / agents pop from here.
    pub receiver: mpsc::Receiver<PeerMessage>,
    /// Peer id.
    pub id: PeerId,
}

/// Shared handle returned by [`LocalMesh::join`]. Keeps the channel alive and
/// lets the peer publish back into the mesh.
#[derive(Clone)]
pub struct MeshHandle {
    mesh: Arc<MeshInner>,
    /// Peer id.
    pub id: PeerId,
}

struct MeshInner {
    peers: RwLock<HashMap<PeerId, PeerEntry>>,
}

struct PeerEntry {
    sender: mpsc::Sender<PeerMessage>,
    capabilities: Vec<PeerCapability>,
    /// Topics this peer subscribes to (empty = all).
    topics: Vec<String>,
}

const CHANNEL_CAPACITY: usize = 256;

/// Default in-process mesh. Cheaply cloneable.
#[derive(Clone)]
pub struct LocalMesh {
    inner: Arc<MeshInner>,
}

impl LocalMesh {
    /// Build an empty mesh.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MeshInner {
                peers: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Number of joined peers.
    pub async fn peer_count(&self) -> usize {
        self.inner.peers.read().await.len()
    }

    /// Snapshot of every peer's capabilities.
    pub async fn directory(&self) -> Vec<(PeerId, Vec<PeerCapability>)> {
        self.inner
            .peers
            .read()
            .await
            .iter()
            .map(|(id, entry)| (id.clone(), entry.capabilities.clone()))
            .collect()
    }

    /// Join the mesh as `id` with the given capabilities. Returns a
    /// [`PeerHandle`] (the receiver end) and a [`MeshHandle`] used to
    /// publish.
    ///
    /// Subscribing to `topics` is optional; an empty list means "receive all
    /// broadcasts".
    pub async fn join(
        &self,
        id: impl Into<PeerId>,
        capabilities: Vec<PeerCapability>,
        topics: Vec<String>,
    ) -> Result<(PeerHandle, MeshHandle)> {
        let id: PeerId = id.into();
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mut peers = self.inner.peers.write().await;
        peers.insert(
            id.clone(),
            PeerEntry {
                sender: tx,
                capabilities,
                topics,
            },
        );
        Ok((
            PeerHandle {
                receiver: rx,
                id: id.clone(),
            },
            MeshHandle {
                mesh: self.inner.clone(),
                id,
            },
        ))
    }

    /// Disconnect a peer.
    pub async fn leave(&self, id: &PeerId) -> Result<()> {
        self.inner.peers.write().await.remove(id);
        Ok(())
    }
}

impl Default for LocalMesh {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshHandle {
    /// Publish a message to whichever peers it is destined for.
    ///
    /// Routing rules:
    /// - [`PeerMessage::Hello`] / [`PeerMessage::Result`] go to every peer
    ///   except the sender.
    /// - [`PeerMessage::Direct`] goes to `to` only.
    /// - [`PeerMessage::Broadcast`] goes to every peer subscribed to the
    ///   topic (empty subscription list = all topics).
    /// - [`PeerMessage::Task`] goes to every peer except the sender — first
    ///   to claim it wins, by convention.
    pub async fn publish(&self, msg: PeerMessage) -> Result<()> {
        let peers = self.mesh.peers.read().await;
        match &msg {
            PeerMessage::Direct { to, .. } => {
                let entry = peers
                    .get(to)
                    .ok_or_else(|| MeshError::UnknownPeer(to.clone()))?;
                let _ = entry.sender.try_send(msg.clone());
            }
            PeerMessage::Broadcast { topic, .. } => {
                for (id, entry) in peers.iter() {
                    if id == self.sender() {
                        continue;
                    }
                    if entry.topics.is_empty() || entry.topics.contains(topic) {
                        let _ = entry.sender.try_send(msg.clone());
                    }
                }
            }
            _ => {
                for (id, entry) in peers.iter() {
                    if id == self.sender() {
                        continue;
                    }
                    let _ = entry.sender.try_send(msg.clone());
                }
            }
        }
        Ok(())
    }

    /// Sender's peer id.
    pub fn sender(&self) -> &PeerId {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn caps(name: &str) -> Vec<PeerCapability> {
        vec![PeerCapability {
            name: name.into(),
            version: None,
        }]
    }

    #[tokio::test]
    async fn join_and_leave_peers() {
        let mesh = LocalMesh::new();
        let (_h, handle_a) = mesh.join("a", caps("browser"), vec![]).await.unwrap();
        let (_h, _handle_b) = mesh.join("b", caps("mirror"), vec![]).await.unwrap();
        assert_eq!(mesh.peer_count().await, 2);
        mesh.leave(&"a".to_string()).await.unwrap();
        assert_eq!(mesh.peer_count().await, 1);
        let _ = handle_a;
    }

    #[tokio::test]
    async fn direct_message_routes_to_one_peer() {
        let mesh = LocalMesh::new();
        let (_h_a, handle_a) = mesh.join("a", caps("x"), vec![]).await.unwrap();
        let (mut peer_b, _handle_b) = mesh.join("b", caps("x"), vec![]).await.unwrap();
        let (mut peer_c, _handle_c) = mesh.join("c", caps("x"), vec![]).await.unwrap();

        handle_a
            .publish(PeerMessage::direct("a", "b", json!({"hello": 1})))
            .await
            .unwrap();

        let msg = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            peer_b.receiver.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(msg.sender(), "a");

        let no_msg = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            peer_c.receiver.recv(),
        )
        .await;
        assert!(no_msg.is_err(), "peer c should not have received the direct");
    }

    #[tokio::test]
    async fn broadcast_filters_by_topic() {
        let mesh = LocalMesh::new();
        let (_pa, ha) = mesh.join("a", caps("x"), vec![]).await.unwrap();
        let (mut pb, _hb) = mesh
            .join("b", caps("x"), vec!["pets".into()])
            .await
            .unwrap();
        let (mut pc, _hc) = mesh
            .join("c", caps("x"), vec!["other".into()])
            .await
            .unwrap();

        ha.publish(PeerMessage::broadcast("a", "pets", json!({"id": 1})))
            .await
            .unwrap();

        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(200),
            pb.receiver.recv()
        )
        .await
        .unwrap()
        .is_some());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(100),
            pc.receiver.recv()
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn task_round_trip_uses_direct_or_broadcast() {
        let mesh = LocalMesh::new();
        let (_pa, ha) = mesh.join("a", caps("x"), vec![]).await.unwrap();
        let (mut pb, hb) = mesh.join("b", caps("x"), vec![]).await.unwrap();
        ha.publish(PeerMessage::task("a", json!({"do": "x"})))
            .await
            .unwrap();
        let msg = pb.receiver.recv().await.unwrap();
        let task_id = match &msg {
            PeerMessage::Task { task_id, .. } => task_id.clone(),
            _ => panic!("expected task"),
        };
        hb.publish(PeerMessage::Result {
            from: "b".into(),
            task_id,
            result: json!({"ok": 1}),
            ok: true,
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn directory_lists_capabilities() {
        let mesh = LocalMesh::new();
        mesh.join("a", caps("browser"), vec![]).await.unwrap();
        mesh.join("b", caps("mirror"), vec![]).await.unwrap();
        let dir = mesh.directory().await;
        assert_eq!(dir.len(), 2);
    }
}

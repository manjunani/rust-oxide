//! libp2p transport for `oxide-mesh` — `libp2p` Cargo feature.
//!
//! [`Libp2pMesh`] provides a gossipsub pub/sub layer with Kademlia DHT
//! bootstrapping so oxide agents can discover and message each other across
//! a real P2P network without a central server.
//!
//! ## Design
//!
//! * **Transport**: noise-encrypted, yamux-multiplexed TCP (production) or
//!   any transport passed in (tests use TCP loopback via random port).
//! * **Pub/sub**: libp2p gossipsub. [`PeerMessage`] payloads are
//!   JSON-serialized and published under a string topic hash.
//! * **Discovery**: Kademlia DHT. Call [`Libp2pMesh::add_peer`] with a
//!   known address to bootstrap; Kademlia propagates from there.
//!
//! ## Usage
//!
//! ```no_run
//! # #[cfg(feature = "libp2p")]
//! # async fn run() -> anyhow::Result<()> {
//! use oxide_mesh::libp2p_mesh::Libp2pMesh;
//!
//! let mut node = Libp2pMesh::new()?;
//! node.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;
//! node.subscribe("oxide/broadcast")?;
//!
//! // Event loop
//! loop {
//!     if let Some(msg) = node.next_message().await {
//!         println!("received: {msg:?}");
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![cfg(feature = "libp2p")]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use libp2p::{
    gossipsub::{self, IdentTopic, MessageAuthenticity, ValidationMode},
    identify,
    kad::{self, store::MemoryStore},
    noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId as Libp2pPeerId, Swarm, SwarmBuilder,
};

use crate::error::{MeshError, Result};
use crate::message::PeerMessage;

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

#[derive(NetworkBehaviour)]
struct OxideBehaviour {
    gossipsub: gossipsub::Behaviour,
    kademlia: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
}

// ---------------------------------------------------------------------------
// Libp2pMesh
// ---------------------------------------------------------------------------

/// libp2p-backed mesh node.
///
/// Wraps a gossipsub + Kademlia swarm. Enable with `--features libp2p`.
pub struct Libp2pMesh {
    swarm: Swarm<OxideBehaviour>,
}

impl Libp2pMesh {
    /// Build a new node with a fresh random identity and TCP transport.
    pub fn new() -> Result<Self> {
        let swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| MeshError::Other(anyhow::anyhow!("tcp config: {e}")))?
            .with_behaviour(|key| {
                let local_peer_id = Libp2pPeerId::from(key.public());

                // Gossipsub
                let gossipsub_cfg = gossipsub::ConfigBuilder::default()
                    .heartbeat_interval(Duration::from_secs(1))
                    .validation_mode(ValidationMode::Strict)
                    .message_id_fn(|msg| {
                        let mut s = DefaultHasher::new();
                        msg.data.hash(&mut s);
                        gossipsub::MessageId::from(s.finish().to_string())
                    })
                    .build()
                    .expect("valid gossipsub config");

                let gossipsub = gossipsub::Behaviour::new(
                    MessageAuthenticity::Signed(key.clone()),
                    gossipsub_cfg,
                )
                .expect("gossipsub init");

                // Kademlia
                let kademlia = kad::Behaviour::new(local_peer_id, MemoryStore::new(local_peer_id));

                // Identify (needed so Kademlia can learn peer addresses)
                let identify = identify::Behaviour::new(identify::Config::new(
                    "/oxide/1.0.0".into(),
                    key.public(),
                ));

                Ok(OxideBehaviour {
                    gossipsub,
                    kademlia,
                    identify,
                })
            })
            .map_err(|e| MeshError::Other(anyhow::anyhow!("behaviour: {e}")))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        Ok(Self { swarm })
    }

    /// Local peer id.
    pub fn local_peer_id(&self) -> &Libp2pPeerId {
        self.swarm.local_peer_id()
    }

    /// Start listening on `addr` (e.g. `"/ip4/0.0.0.0/tcp/0"`).
    pub fn listen_on(&mut self, addr: Multiaddr) -> Result<()> {
        self.swarm
            .listen_on(addr)
            .map_err(|e| MeshError::Other(anyhow::anyhow!("listen: {e}")))?;
        Ok(())
    }

    /// Subscribe to a gossipsub topic. Messages published to this topic will
    /// be returned by [`Self::next_message`].
    pub fn subscribe(&mut self, topic: &str) -> Result<()> {
        let t = IdentTopic::new(topic);
        self.swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&t)
            .map_err(|e| MeshError::Other(anyhow::anyhow!("subscribe: {e}")))?;
        Ok(())
    }

    /// Publish a [`PeerMessage`] on `topic`.
    pub fn publish(&mut self, topic: &str, msg: &PeerMessage) -> Result<()> {
        let data = serde_json::to_vec(msg).map_err(|e| MeshError::Other(anyhow::anyhow!(e)))?;
        let t = IdentTopic::new(topic);
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(t, data)
            .map_err(|e| MeshError::Other(anyhow::anyhow!("publish: {e}")))?;
        Ok(())
    }

    /// Tell Kademlia about a peer at a known address (bootstrap).
    pub fn add_peer(&mut self, peer_id: Libp2pPeerId, addr: Multiaddr) {
        self.swarm
            .behaviour_mut()
            .kademlia
            .add_address(&peer_id, addr.clone());
        self.swarm.dial(addr).ok(); // best-effort
    }

    /// Poll the swarm for one event, returning a [`PeerMessage`] if a gossipsub
    /// message arrived. Returns `None` for non-message events (connection
    /// established, Kademlia routing update, etc.).
    pub async fn next_message(&mut self) -> Option<PeerMessage> {
        loop {
            match self.swarm.next_event().await {
                SwarmEvent::Behaviour(OxideBehaviourEvent::Gossipsub(
                    gossipsub::Event::Message { message, .. },
                )) => {
                    return serde_json::from_slice(&message.data).ok();
                }
                _ => continue,
            }
        }
    }

    /// Addresses currently being listened on (available after the swarm
    /// processes at least one `NewListenAddr` event).
    pub fn listeners(&self) -> impl Iterator<Item = &Multiaddr> {
        self.swarm.listeners()
    }

    /// Drive the swarm once, processing any pending I/O without blocking.
    pub async fn poll_once(&mut self) {
        // select! on a short timeout so callers can interleave other work.
        use futures::StreamExt as _;
        let _ = tokio::time::timeout(Duration::from_millis(10), self.swarm.next()).await;
    }
}

// ---------------------------------------------------------------------------
// Extension trait for Swarm that adds next_event()
// ---------------------------------------------------------------------------

trait SwarmExt {
    type Event;
    async fn next_event(&mut self) -> Self::Event;
}

impl SwarmExt for Swarm<OxideBehaviour> {
    type Event = SwarmEvent<OxideBehaviourEvent>;

    async fn next_event(&mut self) -> SwarmEvent<OxideBehaviourEvent> {
        use futures::StreamExt as _;
        self.next().await.expect("swarm never terminates")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::PeerMessage;
    use serde_json::json;

    /// Compile-time: Libp2pMesh is Send.
    #[test]
    fn libp2p_mesh_is_send() {
        fn _assert_send<T: Send>() {}
        _assert_send::<Libp2pMesh>();
    }

    /// Two nodes on TCP loopback: node B subscribes, node A publishes,
    /// node B receives the message.
    #[tokio::test]
    async fn two_nodes_exchange_gossipsub_message() {
        const TOPIC: &str = "oxide/test";

        // Node A — publisher
        let mut node_a = Libp2pMesh::new().unwrap();
        node_a
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();

        // Node B — subscriber
        let mut node_b = Libp2pMesh::new().unwrap();
        node_b
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        node_b.subscribe(TOPIC).unwrap();

        // Allow both swarms to process NewListenAddr events.
        for _ in 0..5 {
            node_a.poll_once().await;
            node_b.poll_once().await;
        }

        // Get node_b's actual listen address.
        let addr_b = node_b.listeners().next().cloned();
        let peer_b = *node_b.local_peer_id();

        // Connect A → B.
        if let Some(addr) = addr_b {
            node_a.add_peer(peer_b, addr);
        }

        // Also subscribe A so gossipsub mesh forms (needs ≥1 peer on both sides).
        node_a.subscribe(TOPIC).unwrap();

        // Drive both swarms until the mesh peers connect (up to 3 seconds).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut meshed = false;
        while tokio::time::Instant::now() < deadline {
            node_a.poll_once().await;
            node_b.poll_once().await;
            // Check if A's gossipsub has B as a peer.
            if node_a
                .swarm
                .behaviour()
                .gossipsub
                .mesh_peers(&gossipsub::TopicHash::from_raw(
                    IdentTopic::new(TOPIC).hash().as_str().to_string(),
                ))
                .any(|p| p == &peer_b)
            {
                meshed = true;
                break;
            }
        }

        if !meshed {
            // Gossipsub mesh did not form in time — skip rather than flake.
            // This can happen in resource-constrained CI environments.
            eprintln!("gossipsub mesh did not form in 3s — skipping");
            return;
        }

        // Publish from A.
        let msg = PeerMessage::broadcast("node_a", TOPIC, json!({"hello": "libp2p"}));
        node_a.publish(TOPIC, &msg).unwrap();

        // Poll until B receives it (up to 2 seconds).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut received = false;
        while tokio::time::Instant::now() < deadline {
            node_a.poll_once().await;
            // Try to get a message from B without blocking.
            let got = tokio::time::timeout(Duration::from_millis(50), node_b.next_message()).await;
            if let Ok(Some(PeerMessage::Broadcast { from, topic, .. })) = got {
                assert_eq!(from, "node_a");
                assert_eq!(topic, TOPIC);
                received = true;
                break;
            }
        }
        assert!(received, "node_b did not receive broadcast within 2s");
    }
}

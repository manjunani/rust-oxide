//! # `oxide-mesh` — Inter-Agent Communication
//!
//! Lightweight, transport-agnostic peer-to-peer fabric for Rust Oxide agents.
//! Two transports ship today:
//!
//! * [`local`] — in-process `tokio::sync::mpsc` channels. Used by tests and
//!   embedded multi-agent setups where every peer shares the same Tokio
//!   runtime.
//! * [`tcp`] — JSON-line framed TCP. Used for cross-process / cross-host
//!   peers. The wire format is one [`PeerMessage`] per line, so it inter-
//!   operates with hand-rolled clients (netcat for debugging is fine).
//!
//! The protocol is intentionally small — five message kinds — and is
//! deliberately *not* libp2p. Future federated-learning work will pile gossip
//! and CRDT primitives on top of this base layer rather than adopting a
//! whole networking stack.
//!
//! [`MeshModule`] exposes the mesh on the `oxide-k` bus so agents can send
//! broadcasts, direct messages, and task assignments through standard kernel
//! plumbing.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod error;
pub mod kernel;
pub mod local;
pub mod message;
pub mod tcp;

pub use error::{MeshError, Result};
pub use kernel::MeshModule;
pub use local::{LocalMesh, MeshHandle, PeerHandle};
pub use message::{PeerCapability, PeerId, PeerMessage};
pub use tcp::TcpMesh;

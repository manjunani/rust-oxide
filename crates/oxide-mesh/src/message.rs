//! Wire protocol types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable peer identifier (human-chosen or auto-generated UUID).
pub type PeerId = String;

/// What a peer claims to be able to do. Free-form so federated-learning
/// extensions can introduce new capability strings without a wire-format
/// change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCapability {
    /// Capability slug (`"browser"`, `"mirror"`, `"compute"`, …).
    pub name: String,
    /// Optional semantic version.
    #[serde(default)]
    pub version: Option<String>,
}

/// One message on the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerMessage {
    /// First message after a connection establishes — peers identify
    /// themselves and advertise capabilities.
    Hello {
        /// Sender id.
        from: PeerId,
        /// Capability set.
        capabilities: Vec<PeerCapability>,
    },
    /// Fanout publish to every peer subscribed to `topic`.
    Broadcast {
        /// Sender id.
        from: PeerId,
        /// Free-form topic.
        topic: String,
        /// Arbitrary JSON payload.
        payload: serde_json::Value,
    },
    /// Point-to-point delivery.
    Direct {
        /// Sender.
        from: PeerId,
        /// Intended recipient.
        to: PeerId,
        /// Arbitrary JSON payload.
        payload: serde_json::Value,
    },
    /// Distributed task assignment.
    Task {
        /// Sender (originator).
        from: PeerId,
        /// Task id (caller-chosen or auto).
        task_id: String,
        /// Task description.
        spec: serde_json::Value,
        /// Wall-clock time the task was published.
        issued_at: DateTime<Utc>,
    },
    /// Result of a previously-issued [`PeerMessage::Task`].
    Result {
        /// Responder.
        from: PeerId,
        /// Same `task_id` as the originating Task.
        task_id: String,
        /// Result payload.
        result: serde_json::Value,
        /// Whether the task completed successfully.
        ok: bool,
    },
}

impl PeerMessage {
    /// Sender id, useful for routing decisions.
    pub fn sender(&self) -> &PeerId {
        match self {
            PeerMessage::Hello { from, .. }
            | PeerMessage::Broadcast { from, .. }
            | PeerMessage::Direct { from, .. }
            | PeerMessage::Task { from, .. }
            | PeerMessage::Result { from, .. } => from,
        }
    }

    /// Convenience: build a broadcast message.
    pub fn broadcast(
        from: impl Into<PeerId>,
        topic: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self::Broadcast {
            from: from.into(),
            topic: topic.into(),
            payload,
        }
    }

    /// Convenience: build a direct message.
    pub fn direct(
        from: impl Into<PeerId>,
        to: impl Into<PeerId>,
        payload: serde_json::Value,
    ) -> Self {
        Self::Direct {
            from: from.into(),
            to: to.into(),
            payload,
        }
    }

    /// Convenience: build a task with an auto-generated id.
    pub fn task(from: impl Into<PeerId>, spec: serde_json::Value) -> Self {
        Self::Task {
            from: from.into(),
            task_id: Uuid::new_v4().to_string(),
            spec,
            issued_at: Utc::now(),
        }
    }
}

//! # Secure Message Bus
//!
//! An asynchronous message-passing layer for inter-module communication.
//!
//! The bus is built on top of `tokio::sync::mpsc` channels. Modules publish
//! [`Envelope`]s onto the bus and receive routed messages on their own receivers.
//! Every envelope carries provenance metadata (source, timestamp, correlation id)
//! so the kernel can enforce access control and provide an audit trail.
//!
//! ## Topology
//!
//! ```text
//! +--------+   send()   +-----------+   route   +-----------+
//! | Sender | ---------> | MessageBus| --------> | Subscriber|
//! +--------+            +-----------+           +-----------+
//! ```
//!
//! For the bootstrap implementation the bus performs broadcast-style routing:
//! every subscriber receives a clone of each published envelope. This is the
//! simplest model that satisfies the kernel's needs for now; targeted routing
//! and capability-based access control will be layered on top in later
//! iterations.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::error::{KernelError, Result};

/// A command instructs a module (or the kernel) to perform an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Request a module to start.
    Start {
        /// The target module id.
        module_id: String,
    },
    /// Request a module to stop.
    Stop {
        /// The target module id.
        module_id: String,
    },
    /// Invoke a named method on a module with an arbitrary JSON payload.
    Invoke {
        /// The target module id.
        module_id: String,
        /// The method name to invoke.
        method: String,
        /// Method arguments encoded as JSON.
        payload: serde_json::Value,
    },
    /// A heartbeat ping used to verify subscribers are alive.
    Ping,
}

/// An event reports something that has already happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A module has finished starting.
    ModuleStarted {
        /// The module id that just started.
        module_id: String,
    },
    /// A module has stopped.
    ModuleStopped {
        /// The module id that just stopped.
        module_id: String,
    },
    /// A module emitted a generic, semi-structured event.
    Custom {
        /// The module id that emitted the event.
        module_id: String,
        /// Event kind, free-form for now.
        kind: String,
        /// Payload encoded as JSON.
        payload: serde_json::Value,
    },
    /// A heartbeat pong matching a [`Command::Ping`].
    Pong {
        /// The id of the responder.
        from: String,
    },
}

/// The payload carried by an [`Envelope`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    /// A command sent to a module.
    Command(Command),
    /// An event emitted by a module.
    Event(Event),
}

/// An envelope wraps a [`Message`] with provenance metadata.
///
/// Every message routed through the bus is wrapped in an [`Envelope`]. The
/// metadata fields make the bus auditable and enable future capability checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Unique identifier of this envelope.
    pub id: Uuid,
    /// Source module / subsystem name.
    pub source: String,
    /// Optional correlation id to tie a command to its response event.
    pub correlation_id: Option<Uuid>,
    /// Time the envelope was created.
    pub timestamp: DateTime<Utc>,
    /// The actual message payload.
    pub message: Message,
}

impl Envelope {
    /// Wrap a [`Message`] in a new envelope tagged with the given source.
    pub fn new(source: impl Into<String>, message: Message) -> Self {
        Self {
            id: Uuid::new_v4(),
            source: source.into(),
            correlation_id: None,
            timestamp: Utc::now(),
            message,
        }
    }

    /// Builder helper to attach a correlation id.
    #[must_use]
    pub fn with_correlation_id(mut self, correlation_id: Uuid) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }
}

/// The capacity of every subscriber channel.
///
/// Small enough to surface back-pressure quickly, large enough to absorb short
/// bursts. Tuned later based on real workloads.
const DEFAULT_SUBSCRIBER_CAPACITY: usize = 128;

/// A subscription returned by [`MessageBus::subscribe`].
///
/// Holding the [`Subscription`] alive keeps the underlying channel open. When it
/// is dropped, the subscriber is removed from the bus on the next publish.
pub struct Subscription {
    /// Receiver end of the subscriber channel.
    pub receiver: mpsc::Receiver<Envelope>,
    /// Stable id for this subscription. Useful for diagnostics.
    pub id: Uuid,
}

/// The kernel-internal message bus.
///
/// `MessageBus` is cheaply cloneable; clones share the same underlying state and
/// can be handed out to modules and subsystems freely.
#[derive(Clone, Default)]
pub struct MessageBus {
    inner: Arc<BusInner>,
}

#[derive(Default)]
struct BusInner {
    subscribers: RwLock<Vec<Subscriber>>,
}

struct Subscriber {
    id: Uuid,
    tx: mpsc::Sender<Envelope>,
}

impl MessageBus {
    /// Construct a new, empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new subscriber and return a [`Subscription`] handle.
    pub async fn subscribe(&self) -> Subscription {
        let (tx, rx) = mpsc::channel(DEFAULT_SUBSCRIBER_CAPACITY);
        let id = Uuid::new_v4();
        self.inner.subscribers.write().await.push(Subscriber { id, tx });
        Subscription { receiver: rx, id }
    }

    /// Publish an [`Envelope`] to every subscriber.
    ///
    /// Subscribers whose channels are closed are silently dropped. If a
    /// subscriber's channel is full the message is dropped *for that
    /// subscriber only* and a warning is traced; this prevents one slow
    /// subscriber from blocking the whole bus.
    pub async fn publish(&self, envelope: Envelope) -> Result<()> {
        let mut subs = self.inner.subscribers.write().await;
        let mut alive = Vec::with_capacity(subs.len());

        for sub in subs.drain(..) {
            if sub.tx.is_closed() {
                tracing::debug!(subscriber = %sub.id, "dropping closed subscriber");
                continue;
            }
            match sub.tx.try_send(envelope.clone()) {
                Ok(()) => alive.push(sub),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(subscriber = %sub.id, "subscriber channel full; message dropped");
                    alive.push(sub);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!(subscriber = %sub.id, "subscriber closed during publish");
                }
            }
        }

        *subs = alive;
        Ok(())
    }

    /// Convenience helper: wrap a [`Command`] in an [`Envelope`] and publish it.
    pub async fn send_command(&self, source: impl Into<String>, command: Command) -> Result<Uuid> {
        let envelope = Envelope::new(source, Message::Command(command));
        let id = envelope.id;
        self.publish(envelope).await?;
        Ok(id)
    }

    /// Convenience helper: wrap an [`Event`] in an [`Envelope`] and publish it.
    pub async fn emit_event(&self, source: impl Into<String>, event: Event) -> Result<Uuid> {
        let envelope = Envelope::new(source, Message::Event(event));
        let id = envelope.id;
        self.publish(envelope).await?;
        Ok(id)
    }

    /// Returns the number of live subscribers (best-effort).
    pub async fn subscriber_count(&self) -> usize {
        self.inner.subscribers.read().await.len()
    }
}

impl std::fmt::Debug for MessageBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageBus").finish_non_exhaustive()
    }
}

// `From<mpsc::error::SendError<T>>` is hard to implement generically because
// `T` would have to be `'static`. Instead we expose a small helper that callers
// can use when they explicitly want to convert a send failure.
impl<T> From<mpsc::error::SendError<T>> for KernelError {
    fn from(err: mpsc::error::SendError<T>) -> Self {
        KernelError::Bus(format!("channel send failed: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_delivers_to_all_subscribers() {
        let bus = MessageBus::new();
        let mut sub_a = bus.subscribe().await;
        let mut sub_b = bus.subscribe().await;
        assert_eq!(bus.subscriber_count().await, 2);

        bus.send_command("test", Command::Ping).await.unwrap();

        let a = sub_a.receiver.recv().await.expect("a received");
        let b = sub_b.receiver.recv().await.expect("b received");
        assert!(matches!(a.message, Message::Command(Command::Ping)));
        assert!(matches!(b.message, Message::Command(Command::Ping)));
    }

    #[tokio::test]
    async fn closed_subscribers_are_pruned() {
        let bus = MessageBus::new();
        {
            let _sub = bus.subscribe().await;
            assert_eq!(bus.subscriber_count().await, 1);
        }
        // Dropping the subscription closes the receiver end. The bus prunes it
        // on the next publish.
        bus.emit_event("test", Event::Pong { from: "x".into() }).await.unwrap();
        assert_eq!(bus.subscriber_count().await, 0);
    }

    #[tokio::test]
    async fn envelope_carries_provenance() {
        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;

        bus.emit_event(
            "kernel",
            Event::ModuleStarted {
                module_id: "echo".into(),
            },
        )
        .await
        .unwrap();

        let env = sub.receiver.recv().await.unwrap();
        assert_eq!(env.source, "kernel");
        assert!(env.id != Uuid::nil());
        match env.message {
            Message::Event(Event::ModuleStarted { module_id }) => {
                assert_eq!(module_id, "echo");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn correlation_id_round_trips() {
        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        let cid = Uuid::new_v4();

        let env = Envelope::new("test", Message::Command(Command::Ping)).with_correlation_id(cid);
        bus.publish(env).await.unwrap();

        let received = sub.receiver.recv().await.unwrap();
        assert_eq!(received.correlation_id, Some(cid));
    }
}

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

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::error::{KernelError, Result};

// ---------------------------------------------------------------------------
// Capability tokens
// ---------------------------------------------------------------------------

/// An opaque capability token that grants a module the right to publish on
/// the bus.
///
/// Capabilities are strings (e.g. `"browser:navigate"`, `"llm:complete"`).
/// The bus maintains a **grant set**; `publish_with_capability` rejects any
/// token not present in the set. Use [`MessageBus::grant_capability`] to
/// register allowed tokens.
///
/// The unguarded [`MessageBus::publish`] remains available for internal
/// kernel use and backward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capability(pub String);

impl Capability {
    /// Create a capability from any string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

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
    /// Granted capability tokens. Empty means "no ACL enforced" for the
    /// unguarded `publish`; `publish_with_capability` always checks this set.
    granted: RwLock<HashSet<String>>,
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

    // -----------------------------------------------------------------------
    // Capability management
    // -----------------------------------------------------------------------

    /// Grant a capability token. After this call, any source holding `cap`
    /// may call [`Self::publish_with_capability`] successfully.
    pub async fn grant_capability(&self, cap: Capability) {
        self.inner.granted.write().await.insert(cap.0);
    }

    /// Revoke a previously granted capability.
    pub async fn revoke_capability(&self, cap: &Capability) {
        self.inner.granted.write().await.remove(&cap.0);
    }

    /// Publish an [`Envelope`] **only if `cap` has been granted**.
    ///
    /// Returns [`KernelError::Denied`] if the capability is not in the grant
    /// set. On success, routes the envelope identically to [`Self::publish`].
    pub async fn publish_with_capability(
        &self,
        envelope: Envelope,
        cap: &Capability,
    ) -> Result<()> {
        let granted = self.inner.granted.read().await;
        if !granted.contains(&cap.0) {
            return Err(KernelError::Denied {
                publisher: envelope.source.clone(),
                capability: cap.0.clone(),
            });
        }
        drop(granted);
        self.publish(envelope).await
    }

    // -----------------------------------------------------------------------
    // Subscribers
    // -----------------------------------------------------------------------

    /// Register a new subscriber and return a [`Subscription`] handle.
    pub async fn subscribe(&self) -> Subscription {
        let (tx, rx) = mpsc::channel(DEFAULT_SUBSCRIBER_CAPACITY);
        let id = Uuid::new_v4();
        self.inner
            .subscribers
            .write()
            .await
            .push(Subscriber { id, tx });
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
        bus.emit_event("test", Event::Pong { from: "x".into() })
            .await
            .unwrap();
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

    // -------------------------------------------------------------------
    // Capability ACL tests (R-19)
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn granted_capability_allows_publish() {
        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;

        let cap = Capability::new("browser:navigate");
        bus.grant_capability(cap.clone()).await;

        let env = Envelope::new("browser-module", Message::Command(Command::Ping));
        bus.publish_with_capability(env, &cap).await.unwrap();

        let recv = sub.receiver.recv().await.unwrap();
        assert!(matches!(recv.message, Message::Command(Command::Ping)));
    }

    #[tokio::test]
    async fn unganted_capability_returns_denied() {
        let bus = MessageBus::new();
        let cap = Capability::new("llm:complete");
        // Not granted — no call to grant_capability.
        let env = Envelope::new("llm-module", Message::Command(Command::Ping));
        let err = bus.publish_with_capability(env, &cap).await.unwrap_err();
        assert!(
            matches!(err, KernelError::Denied { .. }),
            "expected Denied, got {err}"
        );
    }

    #[tokio::test]
    async fn revoked_capability_is_denied() {
        let bus = MessageBus::new();
        let cap = Capability::new("mirror:sync");
        bus.grant_capability(cap.clone()).await;
        bus.revoke_capability(&cap).await;

        let env = Envelope::new("mirror-module", Message::Command(Command::Ping));
        let err = bus.publish_with_capability(env, &cap).await.unwrap_err();
        assert!(matches!(err, KernelError::Denied { .. }));
    }

    #[tokio::test]
    async fn unguarded_publish_bypasses_acl() {
        // The plain publish() must still work regardless of grant set.
        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        bus.send_command("kernel-internal", Command::Ping)
            .await
            .unwrap();
        let recv = sub.receiver.recv().await.unwrap();
        assert!(matches!(recv.message, Message::Command(Command::Ping)));
    }
}

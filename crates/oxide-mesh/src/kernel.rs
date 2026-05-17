//! `oxide-k` integration: expose the mesh on the kernel bus.

use std::sync::Arc;

use async_trait::async_trait;
use oxide_k::bus::{Command, Event, Message, MessageBus};
use oxide_k::module::{Module, ModuleKind, ModuleMetadata};
use oxide_k::{KernelError, Result as KernelResult};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::local::{LocalMesh, MeshHandle};
use crate::message::{PeerCapability, PeerMessage};

/// Default module id.
pub const DEFAULT_MODULE_ID: &str = "mesh";

/// Mesh module wrapping a [`LocalMesh`].
pub struct MeshModule {
    id: String,
    mesh: LocalMesh,
    /// Identity this module uses when it publishes on behalf of the kernel.
    self_handle: Arc<Mutex<Option<MeshHandle>>>,
    listener: Option<JoinHandle<()>>,
    forwarder: Option<JoinHandle<()>>,
}

impl MeshModule {
    /// Build with default id wrapping a fresh [`LocalMesh`].
    pub fn new() -> Self {
        Self::with_mesh(LocalMesh::new())
    }

    /// Build with an explicit mesh.
    pub fn with_mesh(mesh: LocalMesh) -> Self {
        Self {
            id: DEFAULT_MODULE_ID.into(),
            mesh,
            self_handle: Arc::new(Mutex::new(None)),
            listener: None,
            forwarder: None,
        }
    }

    /// Read-only access to the underlying mesh.
    pub fn mesh(&self) -> &LocalMesh {
        &self.mesh
    }
}

impl Default for MeshModule {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Module for MeshModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: self.id.clone(),
            name: "Oxide Mesh".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: ModuleKind::Native,
            description: Some(
                "Inter-agent communication mesh; routes peer messages and republishes inbound peer traffic onto the kernel bus.".into(),
            ),
        }
    }

    async fn init(&mut self, bus: MessageBus) -> KernelResult<()> {
        // Register the kernel itself as a peer on the mesh so external
        // peers can address it by name. The receiver is forwarded onto the
        // kernel bus.
        let (mut peer, handle) = self
            .mesh
            .join(
                "kernel",
                vec![PeerCapability {
                    name: "kernel".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                }],
                Vec::new(),
            )
            .await
            .map_err(|e| KernelError::Other(anyhow::anyhow!(e)))?;
        *self.self_handle.lock().await = Some(handle);

        let id = self.id.clone();
        let bus_for_inbound = bus.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(msg) = peer.receiver.recv().await {
                let kind = match &msg {
                    PeerMessage::Hello { .. } => "peer.hello",
                    PeerMessage::Broadcast { .. } => "peer.broadcast",
                    PeerMessage::Direct { .. } => "peer.direct",
                    PeerMessage::Task { .. } => "peer.task",
                    PeerMessage::Result { .. } => "peer.result",
                };
                let payload = match serde_json::to_value(&msg) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let _ = bus_for_inbound
                    .emit_event(
                        id.clone(),
                        Event::Custom {
                            module_id: id.clone(),
                            kind: kind.into(),
                            payload,
                        },
                    )
                    .await;
            }
        });
        self.forwarder = Some(forwarder);

        // Listen on the bus for outbound commands.
        let mut sub = bus.subscribe().await;
        let mesh = self.mesh.clone();
        let self_handle = self.self_handle.clone();
        let id = self.id.clone();
        let bus_emit = bus.clone();
        let listener = tokio::spawn(async move {
            while let Some(env) = sub.receiver.recv().await {
                let Message::Command(Command::Invoke {
                    module_id,
                    method,
                    payload,
                }) = env.message
                else {
                    continue;
                };
                if module_id != id {
                    continue;
                }
                let result = dispatch(&mesh, &self_handle, &method, payload).await;
                let event = match result {
                    Ok(value) => Event::Custom {
                        module_id: id.clone(),
                        kind: format!("{method}.ok"),
                        payload: value,
                    },
                    Err(err) => Event::Custom {
                        module_id: id.clone(),
                        kind: format!("{method}.err"),
                        payload: serde_json::json!({ "error": err.to_string() }),
                    },
                };
                let _ = bus_emit.emit_event(id.clone(), event).await;
            }
        });
        self.listener = Some(listener);
        Ok(())
    }

    async fn start(&mut self) -> KernelResult<()> {
        Ok(())
    }

    async fn stop(&mut self) -> KernelResult<()> {
        if let Some(h) = self.listener.take() {
            h.abort();
        }
        if let Some(h) = self.forwarder.take() {
            h.abort();
        }
        let _ = self.mesh.leave(&"kernel".to_string()).await;
        Ok(())
    }
}

#[derive(Deserialize)]
struct PublishPayload {
    message: PeerMessage,
}

#[derive(Deserialize)]
struct DirectoryPayload {}

async fn dispatch(
    mesh: &LocalMesh,
    self_handle: &Arc<Mutex<Option<MeshHandle>>>,
    method: &str,
    payload: serde_json::Value,
) -> KernelResult<serde_json::Value> {
    let to_kernel = |e: crate::error::MeshError| KernelError::Other(anyhow::anyhow!(e));
    match method {
        "publish" => {
            let p: PublishPayload = serde_json::from_value(payload)?;
            let guard = self_handle.lock().await;
            let handle = guard
                .as_ref()
                .ok_or_else(|| KernelError::Other(anyhow::anyhow!("mesh not initialised")))?;
            handle.publish(p.message).await.map_err(to_kernel)?;
            Ok(serde_json::json!({"ok": true}))
        }
        "directory" => {
            let _p: DirectoryPayload = serde_json::from_value(payload).unwrap_or(DirectoryPayload {});
            let dir = mesh.directory().await;
            Ok(serde_json::to_value(dir)?)
        }
        "peer_count" => Ok(serde_json::json!({"count": mesh.peer_count().await})),
        other => Err(KernelError::Other(anyhow::anyhow!(
            "unknown mesh method `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_k::bus::{Event, Message};
    use serde_json::json;

    #[tokio::test]
    async fn module_publishes_broadcast_via_bus() {
        let mesh = LocalMesh::new();
        let (mut peer_x, _h) = mesh
            .join(
                "x",
                vec![PeerCapability {
                    name: "x".into(),
                    version: None,
                }],
                vec![],
            )
            .await
            .unwrap();

        let mut module = MeshModule::with_mesh(mesh.clone());
        let bus = MessageBus::new();
        Module::init(&mut module, bus.clone()).await.unwrap();
        Module::start(&mut module).await.unwrap();

        let msg = PeerMessage::broadcast("kernel", "topic", json!({"hi": 1}));
        bus.send_command(
            "test",
            Command::Invoke {
                module_id: DEFAULT_MODULE_ID.into(),
                method: "publish".into(),
                payload: json!({"message": msg}),
            },
        )
        .await
        .unwrap();

        let received = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            peer_x.receiver.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        match received {
            PeerMessage::Broadcast { from, .. } => assert_eq!(from, "kernel"),
            other => panic!("unexpected: {other:?}"),
        }
        Module::stop(&mut module).await.unwrap();
    }

    #[tokio::test]
    async fn forwarder_emits_peer_event_for_direct() {
        let mesh = LocalMesh::new();
        let mut module = MeshModule::with_mesh(mesh.clone());
        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        Module::init(&mut module, bus.clone()).await.unwrap();
        Module::start(&mut module).await.unwrap();

        // External peer joins after kernel registers itself.
        let (_pa, handle_a) = mesh
            .join(
                "a",
                vec![PeerCapability {
                    name: "a".into(),
                    version: None,
                }],
                vec![],
            )
            .await
            .unwrap();
        handle_a
            .publish(PeerMessage::direct("a", "kernel", json!({"ping": true})))
            .await
            .unwrap();

        let mut saw = false;
        for _ in 0..15 {
            match tokio::time::timeout(
                std::time::Duration::from_millis(300),
                sub.receiver.recv(),
            )
            .await
            {
                Ok(Some(env)) => {
                    if let Message::Event(Event::Custom { kind, payload, .. }) = env.message {
                        if kind == "peer.direct" {
                            assert_eq!(payload["from"], json!("a"));
                            saw = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(saw, "expected peer.direct event");
        Module::stop(&mut module).await.unwrap();
    }
}

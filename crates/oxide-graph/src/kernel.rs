//! `oxide-k` bus integration for the graph.

use std::sync::Arc;

use async_trait::async_trait;
use oxide_k::bus::{Command, Event, Message, MessageBus};
use oxide_k::module::{Module, ModuleKind, ModuleMetadata};
use oxide_k::{KernelError, Result as KernelResult};
use serde::Deserialize;
use tokio::task::JoinHandle;

use crate::graph::{Edge, GraphStore, InMemoryGraph, Node, NodeId};
use crate::ingest::{ingest_record, RecordRef};
use crate::query::{traverse, EdgeQuery, NodeQuery};

/// Default module id.
pub const DEFAULT_MODULE_ID: &str = "graph";

/// Knowledge-graph module wrapped around an [`InMemoryGraph`].
pub struct GraphModule {
    id: String,
    store: Arc<InMemoryGraph>,
    listener: Option<JoinHandle<()>>,
}

impl GraphModule {
    /// Build with the default id and a fresh in-memory store.
    pub fn new() -> Self {
        Self::with_store(Arc::new(InMemoryGraph::new()))
    }

    /// Build with an explicit store.
    pub fn with_store(store: Arc<InMemoryGraph>) -> Self {
        Self {
            id: DEFAULT_MODULE_ID.into(),
            store,
            listener: None,
        }
    }

    /// Access the underlying store.
    pub fn store(&self) -> Arc<InMemoryGraph> {
        self.store.clone()
    }
}

impl Default for GraphModule {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Module for GraphModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: self.id.clone(),
            name: "Oxide Knowledge Graph".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: ModuleKind::Native,
            description: Some(
                "In-memory typed property graph; ingests mirrored records, answers pattern + traversal queries.".into(),
            ),
        }
    }

    async fn init(&mut self, bus: MessageBus) -> KernelResult<()> {
        let mut sub = bus.subscribe().await;
        let store = self.store.clone();
        let id = self.id.clone();
        let bus_emit = bus.clone();
        let handle = tokio::spawn(async move {
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
                let result = dispatch(&store, &method, payload).await;
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
        self.listener = Some(handle);
        Ok(())
    }

    async fn start(&mut self) -> KernelResult<()> {
        Ok(())
    }

    async fn stop(&mut self) -> KernelResult<()> {
        if let Some(h) = self.listener.take() {
            h.abort();
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct IngestPayload {
    resource: String,
    record_id: String,
    source: String,
    payload: serde_json::Value,
}

#[derive(Deserialize)]
struct UpsertNodePayload {
    node: Node,
}

#[derive(Deserialize)]
struct AddEdgePayload {
    edge: Edge,
}

#[derive(Deserialize)]
struct GetNodePayload {
    id: NodeId,
}

#[derive(Deserialize)]
struct NodeQueryPayload {
    label: String,
    #[serde(default)]
    property_eq: Vec<(String, serde_json::Value)>,
}

#[derive(Deserialize)]
struct EdgeQueryPayload {
    anchor: NodeId,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    direction: Option<String>,
}

#[derive(Deserialize)]
struct TraversePayload {
    start: NodeId,
    #[serde(default)]
    edge_label: Option<String>,
    #[serde(default = "default_depth")]
    max_depth: usize,
}

fn default_depth() -> usize {
    2
}

async fn dispatch(
    store: &Arc<InMemoryGraph>,
    method: &str,
    payload: serde_json::Value,
) -> KernelResult<serde_json::Value> {
    let to_kernel = |e: crate::error::GraphError| KernelError::Other(anyhow::anyhow!(e));

    match method {
        "ingest" => {
            let p: IngestPayload = serde_json::from_value(payload)?;
            let id = ingest_record(
                store.as_ref(),
                RecordRef {
                    resource: &p.resource,
                    record_id: &p.record_id,
                    payload: &p.payload,
                    source: &p.source,
                },
            )
            .await
            .map_err(to_kernel)?;
            Ok(serde_json::json!({ "id": id }))
        }
        "upsert_node" => {
            let p: UpsertNodePayload = serde_json::from_value(payload)?;
            store.upsert_node(p.node).await.map_err(to_kernel)?;
            Ok(serde_json::json!({"ok": true}))
        }
        "add_edge" => {
            let p: AddEdgePayload = serde_json::from_value(payload)?;
            let id = store.add_edge(p.edge).await.map_err(to_kernel)?;
            Ok(serde_json::json!({"id": id}))
        }
        "get_node" => {
            let p: GetNodePayload = serde_json::from_value(payload)?;
            let node = store.get_node(&p.id).await.map_err(to_kernel)?;
            Ok(serde_json::to_value(node)?)
        }
        "node_query" => {
            let p: NodeQueryPayload = serde_json::from_value(payload)?;
            let mut q = NodeQuery::label(p.label);
            q.property_eq = p.property_eq;
            let nodes = q.run(store.as_ref()).await.map_err(to_kernel)?;
            Ok(serde_json::to_value(nodes)?)
        }
        "edge_query" => {
            let p: EdgeQueryPayload = serde_json::from_value(payload)?;
            let direction = match p.direction.as_deref() {
                Some("in") => crate::query::EdgeDirection::In,
                Some("either") => crate::query::EdgeDirection::Either,
                _ => crate::query::EdgeDirection::Out,
            };
            let q = EdgeQuery {
                label: p.label,
                direction,
            };
            let edges = q.run(store.as_ref(), &p.anchor).await.map_err(to_kernel)?;
            Ok(serde_json::to_value(edges)?)
        }
        "traverse" => {
            let p: TraversePayload = serde_json::from_value(payload)?;
            let nodes = traverse(
                store.as_ref(),
                &p.start,
                p.edge_label.as_deref(),
                p.max_depth,
            )
            .await
            .map_err(to_kernel)?;
            Ok(serde_json::to_value(nodes)?)
        }
        "stats" => {
            let (n, e) = store.stats().await.map_err(to_kernel)?;
            Ok(serde_json::json!({"nodes": n, "edges": e}))
        }
        other => Err(KernelError::Other(anyhow::anyhow!(
            "unknown graph method `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_k::bus::{Event, Message};
    use serde_json::json;

    #[tokio::test]
    async fn bus_ingest_then_query() {
        let mut module = GraphModule::new();
        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        Module::init(&mut module, bus.clone()).await.unwrap();
        Module::start(&mut module).await.unwrap();

        bus.send_command(
            "test",
            Command::Invoke {
                module_id: DEFAULT_MODULE_ID.into(),
                method: "ingest".into(),
                payload: json!({
                    "resource": "pet",
                    "record_id": "1",
                    "source": "petstore",
                    "payload": {"name": "Rex", "status": "available"}
                }),
            },
        )
        .await
        .unwrap();

        let mut saw_ingest = false;
        for _ in 0..15 {
            match tokio::time::timeout(std::time::Duration::from_millis(400), sub.receiver.recv())
                .await
            {
                Ok(Some(env)) => {
                    if let Message::Event(Event::Custom { kind, .. }) = env.message {
                        if kind == "ingest.ok" {
                            saw_ingest = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(saw_ingest);

        bus.send_command(
            "test",
            Command::Invoke {
                module_id: DEFAULT_MODULE_ID.into(),
                method: "node_query".into(),
                payload: json!({"label": "pet", "property_eq": [["status", "available"]]}),
            },
        )
        .await
        .unwrap();

        let mut saw_query = false;
        for _ in 0..15 {
            match tokio::time::timeout(std::time::Duration::from_millis(400), sub.receiver.recv())
                .await
            {
                Ok(Some(env)) => {
                    if let Message::Event(Event::Custom { kind, payload, .. }) = env.message {
                        if kind == "node_query.ok" {
                            let nodes = payload.as_array().unwrap();
                            assert_eq!(nodes.len(), 1);
                            assert_eq!(nodes[0]["id"], json!("pet:1"));
                            saw_query = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(saw_query);
        Module::stop(&mut module).await.unwrap();
    }
}

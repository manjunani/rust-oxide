//! Node / Edge types and the [`InMemoryGraph`] store.

use std::collections::{BTreeSet, HashMap};
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{GraphError, Result};

/// Stable, user-supplied id for a node (`"resource:record_id"` typical).
pub type NodeId = String;

/// Auto-generated edge id.
pub type EdgeId = String;

/// A property-graph node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Stable id.
    pub id: NodeId,
    /// One or more semantic labels (e.g. `["pet"]`, `["user", "agent"]`).
    pub labels: Vec<String>,
    /// Free-form properties.
    pub properties: serde_json::Map<String, serde_json::Value>,
}

impl Node {
    /// Build a node with a single label.
    pub fn new(id: impl Into<NodeId>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            labels: vec![label.into()],
            properties: serde_json::Map::new(),
        }
    }

    /// Builder helper.
    #[must_use]
    pub fn with_property(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.properties.insert(key.into(), value);
        self
    }

    /// Builder helper.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.labels.push(label.into());
        self
    }
}

/// A directed labelled edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    /// Auto-generated id.
    pub id: EdgeId,
    /// Tail node id.
    pub from: NodeId,
    /// Head node id.
    pub to: NodeId,
    /// Semantic label (`"owns"`, `"references"`, `"mentions"`).
    pub label: String,
    /// Free-form properties.
    pub properties: serde_json::Map<String, serde_json::Value>,
}

impl Edge {
    /// Build an edge with a generated id.
    pub fn new(from: impl Into<NodeId>, to: impl Into<NodeId>, label: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            from: from.into(),
            to: to.into(),
            label: label.into(),
            properties: serde_json::Map::new(),
        }
    }

    /// Builder helper.
    #[must_use]
    pub fn with_property(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.properties.insert(key.into(), value);
        self
    }
}

/// Storage abstraction. Concrete implementations: [`InMemoryGraph`].
#[async_trait]
pub trait GraphStore: Send + Sync {
    /// Insert / replace a node by id.
    async fn upsert_node(&self, node: Node) -> Result<()>;
    /// Insert an edge.
    async fn add_edge(&self, edge: Edge) -> Result<EdgeId>;
    /// Look up a node by id.
    async fn get_node(&self, id: &NodeId) -> Result<Option<Node>>;
    /// Look up an edge by id.
    async fn get_edge(&self, id: &EdgeId) -> Result<Option<Edge>>;
    /// All nodes carrying any of `labels`.
    async fn nodes_by_label(&self, label: &str) -> Result<Vec<Node>>;
    /// All edges leaving `from` (optionally filtered by label).
    async fn edges_from(&self, from: &NodeId, label: Option<&str>) -> Result<Vec<Edge>>;
    /// All edges entering `to` (optionally filtered by label).
    async fn edges_to(&self, to: &NodeId, label: Option<&str>) -> Result<Vec<Edge>>;
    /// Counts.
    async fn stats(&self) -> Result<(usize, usize)>;
}

struct InnerGraph {
    nodes: HashMap<NodeId, Node>,
    edges: HashMap<EdgeId, Edge>,
    by_label: HashMap<String, BTreeSet<NodeId>>,
    out_edges: HashMap<NodeId, BTreeSet<EdgeId>>,
    in_edges: HashMap<NodeId, BTreeSet<EdgeId>>,
}

/// In-process knowledge graph. Cheap to clone (wraps `Arc<RwLock<…>>`).
#[derive(Clone)]
pub struct InMemoryGraph {
    inner: std::sync::Arc<RwLock<InnerGraph>>,
}

impl InMemoryGraph {
    /// Build an empty graph.
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(RwLock::new(InnerGraph {
                nodes: HashMap::new(),
                edges: HashMap::new(),
                by_label: HashMap::new(),
                out_edges: HashMap::new(),
                in_edges: HashMap::new(),
            })),
        }
    }
}

impl Default for InMemoryGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GraphStore for InMemoryGraph {
    async fn upsert_node(&self, node: Node) -> Result<()> {
        let mut g = self.inner.write().unwrap();
        // Remove prior label indexing if replacing.
        if let Some(prev) = g.nodes.get(&node.id) {
            for label in prev.labels.clone() {
                if let Some(set) = g.by_label.get_mut(&label) {
                    set.remove(&node.id);
                }
            }
        }
        for label in &node.labels {
            g.by_label
                .entry(label.clone())
                .or_default()
                .insert(node.id.clone());
        }
        g.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    async fn add_edge(&self, edge: Edge) -> Result<EdgeId> {
        let mut g = self.inner.write().unwrap();
        if !g.nodes.contains_key(&edge.from) {
            return Err(GraphError::UnknownNode(edge.from.clone()));
        }
        if !g.nodes.contains_key(&edge.to) {
            return Err(GraphError::UnknownNode(edge.to.clone()));
        }
        let id = edge.id.clone();
        g.out_edges
            .entry(edge.from.clone())
            .or_default()
            .insert(id.clone());
        g.in_edges
            .entry(edge.to.clone())
            .or_default()
            .insert(id.clone());
        g.edges.insert(id.clone(), edge);
        Ok(id)
    }

    async fn get_node(&self, id: &NodeId) -> Result<Option<Node>> {
        Ok(self.inner.read().unwrap().nodes.get(id).cloned())
    }

    async fn get_edge(&self, id: &EdgeId) -> Result<Option<Edge>> {
        Ok(self.inner.read().unwrap().edges.get(id).cloned())
    }

    async fn nodes_by_label(&self, label: &str) -> Result<Vec<Node>> {
        let g = self.inner.read().unwrap();
        Ok(g.by_label
            .get(label)
            .map(|ids| ids.iter().filter_map(|i| g.nodes.get(i).cloned()).collect())
            .unwrap_or_default())
    }

    async fn edges_from(&self, from: &NodeId, label: Option<&str>) -> Result<Vec<Edge>> {
        let g = self.inner.read().unwrap();
        Ok(g.out_edges
            .get(from)
            .map(|ids| {
                ids.iter()
                    .filter_map(|i| g.edges.get(i).cloned())
                    .filter(|e| label.is_none_or(|l| e.label == l))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn edges_to(&self, to: &NodeId, label: Option<&str>) -> Result<Vec<Edge>> {
        let g = self.inner.read().unwrap();
        Ok(g.in_edges
            .get(to)
            .map(|ids| {
                ids.iter()
                    .filter_map(|i| g.edges.get(i).cloned())
                    .filter(|e| label.is_none_or(|l| e.label == l))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn stats(&self) -> Result<(usize, usize)> {
        let g = self.inner.read().unwrap();
        Ok((g.nodes.len(), g.edges.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn upsert_and_query_by_label() {
        let g = InMemoryGraph::new();
        g.upsert_node(Node::new("pet:1", "pet").with_property("name", json!("Rex")))
            .await
            .unwrap();
        g.upsert_node(Node::new("pet:2", "pet").with_property("name", json!("Buddy")))
            .await
            .unwrap();
        g.upsert_node(Node::new("user:1", "user")).await.unwrap();

        let pets = g.nodes_by_label("pet").await.unwrap();
        assert_eq!(pets.len(), 2);
        let users = g.nodes_by_label("user").await.unwrap();
        assert_eq!(users.len(), 1);
    }

    #[tokio::test]
    async fn edges_link_existing_nodes_only() {
        let g = InMemoryGraph::new();
        g.upsert_node(Node::new("a", "node")).await.unwrap();
        g.upsert_node(Node::new("b", "node")).await.unwrap();
        let id = g.add_edge(Edge::new("a", "b", "links")).await.unwrap();
        assert!(g.get_edge(&id).await.unwrap().is_some());

        let err = g
            .add_edge(Edge::new("a", "missing", "links"))
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::UnknownNode(_)));
    }

    #[tokio::test]
    async fn directional_edge_queries() {
        let g = InMemoryGraph::new();
        for n in ["a", "b", "c"] {
            g.upsert_node(Node::new(n, "n")).await.unwrap();
        }
        g.add_edge(Edge::new("a", "b", "knows")).await.unwrap();
        g.add_edge(Edge::new("a", "c", "knows")).await.unwrap();
        g.add_edge(Edge::new("b", "c", "owns")).await.unwrap();

        let from_a = g.edges_from(&"a".into(), None).await.unwrap();
        assert_eq!(from_a.len(), 2);
        let from_a_owns = g.edges_from(&"a".into(), Some("owns")).await.unwrap();
        assert_eq!(from_a_owns.len(), 0);
        let to_c = g.edges_to(&"c".into(), None).await.unwrap();
        assert_eq!(to_c.len(), 2);
        let to_c_owns = g.edges_to(&"c".into(), Some("owns")).await.unwrap();
        assert_eq!(to_c_owns.len(), 1);
    }

    #[tokio::test]
    async fn stats_reflect_inserts() {
        let g = InMemoryGraph::new();
        g.upsert_node(Node::new("a", "n")).await.unwrap();
        g.upsert_node(Node::new("b", "n")).await.unwrap();
        g.add_edge(Edge::new("a", "b", "x")).await.unwrap();
        assert_eq!(g.stats().await.unwrap(), (2, 1));
    }
}

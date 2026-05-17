//! Query primitives.
//!
//! Two builder structs (`NodeQuery`, `EdgeQuery`) plus a small `traverse`
//! function for breadth-first walks. The query language is intentionally tiny;
//! the goal is to expose composable predicates rather than to reinvent
//! Cypher / Gremlin.

use std::collections::{HashSet, VecDeque};

use crate::error::Result;
use crate::graph::{Edge, GraphStore, Node, NodeId};

/// Filter for [`Node`]s.
#[derive(Debug, Clone, Default)]
pub struct NodeQuery {
    /// Required label (any node carrying this label matches).
    pub label: Option<String>,
    /// Required `(property, value)` pairs — exact JSON equality.
    pub property_eq: Vec<(String, serde_json::Value)>,
}

impl NodeQuery {
    /// Build a query that matches any node carrying `label`.
    pub fn label(label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            ..Self::default()
        }
    }

    /// Builder helper.
    #[must_use]
    pub fn property_eq(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.property_eq.push((key.into(), value));
        self
    }

    /// Run the query against `store`.
    pub async fn run(&self, store: &dyn GraphStore) -> Result<Vec<Node>> {
        let label = self.label.as_deref().unwrap_or("");
        let candidates: Vec<Node> = if label.is_empty() {
            return Ok(Vec::new()); // require at least a label or property filter
        } else {
            store.nodes_by_label(label).await?
        };
        Ok(candidates
            .into_iter()
            .filter(|n| {
                self.property_eq
                    .iter()
                    .all(|(k, v)| n.properties.get(k) == Some(v))
            })
            .collect())
    }
}

/// Filter for [`Edge`]s incident to a given node.
#[derive(Debug, Clone, Default)]
pub struct EdgeQuery {
    /// Required label.
    pub label: Option<String>,
    /// Direction relative to the anchor node.
    pub direction: EdgeDirection,
}

/// Edge direction relative to an anchor node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EdgeDirection {
    /// Edges where the anchor is the tail.
    #[default]
    Out,
    /// Edges where the anchor is the head.
    In,
    /// Both directions.
    Either,
}

impl EdgeQuery {
    /// Build an outbound edge query for `label`.
    pub fn outbound(label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            direction: EdgeDirection::Out,
        }
    }

    /// Builder helper.
    #[must_use]
    pub fn direction(mut self, d: EdgeDirection) -> Self {
        self.direction = d;
        self
    }

    /// Run the query against `store` anchored at `node`.
    pub async fn run(&self, store: &dyn GraphStore, node: &NodeId) -> Result<Vec<Edge>> {
        let label = self.label.as_deref();
        match self.direction {
            EdgeDirection::Out => store.edges_from(node, label).await,
            EdgeDirection::In => store.edges_to(node, label).await,
            EdgeDirection::Either => {
                let mut out = store.edges_from(node, label).await?;
                out.extend(store.edges_to(node, label).await?);
                Ok(out)
            }
        }
    }
}

/// Breadth-first traversal that walks edges matching `edge_label` outward from
/// `start`, returning every node reached within `max_depth`.
pub async fn traverse(
    store: &dyn GraphStore,
    start: &NodeId,
    edge_label: Option<&str>,
    max_depth: usize,
) -> Result<Vec<Node>> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
    queue.push_back((start.clone(), 0));
    seen.insert(start.clone());
    let mut out = Vec::new();

    while let Some((current, depth)) = queue.pop_front() {
        if let Some(node) = store.get_node(&current).await? {
            out.push(node);
        }
        if depth >= max_depth {
            continue;
        }
        let edges = store.edges_from(&current, edge_label).await?;
        for e in edges {
            if seen.insert(e.to.clone()) {
                queue.push_back((e.to.clone(), depth + 1));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, InMemoryGraph, Node};
    use serde_json::json;

    async fn fixture() -> InMemoryGraph {
        let g = InMemoryGraph::new();
        g.upsert_node(
            Node::new("pet:1", "pet")
                .with_property("name", json!("Rex"))
                .with_property("status", json!("available")),
        )
        .await
        .unwrap();
        g.upsert_node(
            Node::new("pet:2", "pet")
                .with_property("name", json!("Buddy"))
                .with_property("status", json!("sold")),
        )
        .await
        .unwrap();
        g.upsert_node(Node::new("owner:1", "owner")).await.unwrap();
        g.upsert_node(Node::new("owner:2", "owner")).await.unwrap();
        g.add_edge(Edge::new("owner:1", "pet:1", "owns"))
            .await
            .unwrap();
        g.add_edge(Edge::new("owner:2", "pet:2", "owns"))
            .await
            .unwrap();
        g.add_edge(Edge::new("owner:1", "owner:2", "knows"))
            .await
            .unwrap();
        g
    }

    #[tokio::test]
    async fn node_query_filters_by_property() {
        let g = fixture().await;
        let q = NodeQuery::label("pet").property_eq("status", json!("available"));
        let results = q.run(&g).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "pet:1");
    }

    #[tokio::test]
    async fn edge_query_directions() {
        let g = fixture().await;
        let owns_out = EdgeQuery::outbound("owns")
            .run(&g, &"owner:1".into())
            .await
            .unwrap();
        assert_eq!(owns_out.len(), 1);

        let either = EdgeQuery {
            label: None,
            direction: EdgeDirection::Either,
        }
        .run(&g, &"owner:1".into())
        .await
        .unwrap();
        // owner:1 → pet:1 (owns), owner:1 → owner:2 (knows). No inbound.
        assert_eq!(either.len(), 2);
    }

    #[tokio::test]
    async fn traverse_walks_depth() {
        let g = fixture().await;
        let nodes = traverse(&g, &"owner:1".into(), None, 2).await.unwrap();
        let ids: Vec<_> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"owner:1"));
        assert!(ids.contains(&"pet:1"));
        assert!(ids.contains(&"owner:2"));
        // pet:2 reachable only at depth 2 via owner:1 → owner:2 → pet:2.
        assert!(ids.contains(&"pet:2"));
    }
}

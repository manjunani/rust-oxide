//! Neo4j [`GraphStore`] adapter — `neo4j` Cargo feature.
//!
//! Uses neo4rs 0.8. `DetachedRowStream::into_stream()` returns a non-`Unpin`
//! TryStream, so we use `try_collect::<Vec<Row>>()` (which does NOT require
//! `Unpin`) and index into the collected vector.

#![cfg(feature = "neo4j")]

use async_trait::async_trait;
use futures_util::TryStreamExt as _;
use neo4rs::{query, Graph, Row};

use crate::error::{GraphError, Result};
use crate::graph::{Edge, EdgeId, GraphStore, Node, NodeId};

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

macro_rules! nerr {
    ($ctx:literal) => {
        |e| GraphError::Other(anyhow::anyhow!(concat!("neo4j ", $ctx, ": {}"), e))
    };
}
macro_rules! jerr {
    ($ctx:literal) => {
        |e: serde_json::Error| GraphError::Other(anyhow::anyhow!(concat!($ctx, ": {}"), e))
    };
}
macro_rules! gerr {
    ($ctx:literal) => {
        |e: neo4rs::DeError| GraphError::Other(anyhow::anyhow!(concat!("row.", $ctx, ": {}"), e))
    };
}

/// Execute a query and collect all rows.
macro_rules! exec_collect {
    ($graph:expr, $query:expr) => {
        $graph
            .execute($query)
            .await
            .map_err(nerr!("execute"))?
            .into_stream()
            .try_collect::<Vec<Row>>()
            .await
            .map_err(nerr!("collect"))?
    };
}

// ---------------------------------------------------------------------------
// Neo4jGraph
// ---------------------------------------------------------------------------

/// Neo4j-backed [`GraphStore`]. Cheaply cloneable.
#[derive(Clone)]
pub struct Neo4jGraph {
    graph: Graph,
}

impl Neo4jGraph {
    /// Connect and ensure the node uniqueness constraint.
    pub async fn connect(uri: &str, user: &str, password: &str) -> Result<Self> {
        let graph = Graph::new(uri, user, password)
            .await
            .map_err(nerr!("connect"))?;
        graph
            .run(query(
                "CREATE CONSTRAINT oxide_node_id IF NOT EXISTS \
                 FOR (n:OxideNode) REQUIRE n.node_id IS UNIQUE",
            ))
            .await
            .map_err(nerr!("constraint"))?;
        Ok(Self { graph })
    }
}

// ---------------------------------------------------------------------------
// GraphStore impl
// ---------------------------------------------------------------------------

#[async_trait]
impl GraphStore for Neo4jGraph {
    async fn upsert_node(&self, node: Node) -> Result<()> {
        let labels_json = serde_json::to_string(&node.labels).map_err(jerr!("labels_json"))?;
        let props_json = serde_json::to_string(&node.properties).map_err(jerr!("props_json"))?;
        self.graph
            .run(
                query(
                    "MERGE (n:OxideNode {node_id: $id}) \
                     SET n.oxide_labels = $labels, n.oxide_props = $props",
                )
                .param("id", node.id)
                .param("labels", labels_json)
                .param("props", props_json),
            )
            .await
            .map_err(nerr!("upsert_node"))?;
        Ok(())
    }

    async fn add_edge(&self, edge: Edge) -> Result<EdgeId> {
        let props_json = serde_json::to_string(&edge.properties).map_err(jerr!("edge_props"))?;
        let rows = exec_collect!(
            self.graph,
            query(
                "MATCH (a:OxideNode {node_id: $from}), (b:OxideNode {node_id: $to}) \
                 CREATE (a)-[r:OXIDE_EDGE { \
                   edge_id: $eid, edge_label: $label, \
                   from_id: $from, to_id: $to, edge_props: $props \
                 }]->(b) \
                 RETURN r.edge_id AS eid",
            )
            .param("from", edge.from.clone())
            .param("to", edge.to.clone())
            .param("eid", edge.id.clone())
            .param("label", edge.label.clone())
            .param("props", props_json)
        );
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| GraphError::UnknownNode(edge.from.clone()))?;
        let eid: String = row.get("eid").map_err(gerr!("eid"))?;
        Ok(eid)
    }

    async fn get_node(&self, id: &NodeId) -> Result<Option<Node>> {
        let rows = exec_collect!(
            self.graph,
            query(
                "MATCH (n:OxideNode {node_id: $id}) \
                 RETURN n.oxide_labels AS labels, n.oxide_props AS props",
            )
            .param("id", id.clone())
        );
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let labels_str: String = row.get("labels").map_err(gerr!("labels"))?;
        let props_str: String = row.get("props").map_err(gerr!("props"))?;
        Ok(Some(Node {
            id: id.clone(),
            labels: serde_json::from_str(&labels_str).map_err(jerr!("labels"))?,
            properties: serde_json::from_str(&props_str).map_err(jerr!("props"))?,
        }))
    }

    async fn get_edge(&self, id: &EdgeId) -> Result<Option<Edge>> {
        let rows = exec_collect!(
            self.graph,
            query(
                "MATCH ()-[r:OXIDE_EDGE {edge_id: $id}]->() \
                 RETURN r.from_id AS from_id, r.to_id AS to_id, \
                        r.edge_label AS label, r.edge_props AS props",
            )
            .param("id", id.clone())
        );
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let props_str: String = row.get("props").map_err(gerr!("props"))?;
        Ok(Some(Edge {
            id: id.clone(),
            from: row.get("from_id").map_err(gerr!("from_id"))?,
            to: row.get("to_id").map_err(gerr!("to_id"))?,
            label: row.get("label").map_err(gerr!("label"))?,
            properties: serde_json::from_str(&props_str).map_err(jerr!("props"))?,
        }))
    }

    async fn nodes_by_label(&self, label: &str) -> Result<Vec<Node>> {
        let rows = exec_collect!(
            self.graph,
            query(
                "MATCH (n:OxideNode) \
                 RETURN n.node_id AS id, n.oxide_labels AS labels, n.oxide_props AS props",
            )
        );
        let mut nodes = Vec::new();
        for row in rows {
            let labels_str: String = row.get("labels").map_err(gerr!("labels"))?;
            let labels: Vec<String> = serde_json::from_str(&labels_str).map_err(jerr!("labels"))?;
            if !labels.contains(&label.to_string()) {
                continue;
            }
            let props_str: String = row.get("props").map_err(gerr!("props"))?;
            nodes.push(Node {
                id: row.get("id").map_err(gerr!("id"))?,
                labels,
                properties: serde_json::from_str(&props_str).map_err(jerr!("props"))?,
            });
        }
        Ok(nodes)
    }

    async fn edges_from(&self, from: &NodeId, label: Option<&str>) -> Result<Vec<Edge>> {
        let cypher = if label.is_some() {
            "MATCH (:OxideNode {node_id: $from})-[r:OXIDE_EDGE {edge_label: $label}]->(b:OxideNode) \
             RETURN r.edge_id AS eid, b.node_id AS to_id, r.edge_label AS lbl, r.edge_props AS props"
        } else {
            "MATCH (:OxideNode {node_id: $from})-[r:OXIDE_EDGE]->(b:OxideNode) \
             RETURN r.edge_id AS eid, b.node_id AS to_id, r.edge_label AS lbl, r.edge_props AS props"
        };
        let mut q = query(cypher).param("from", from.clone());
        if let Some(l) = label {
            q = q.param("label", l.to_string());
        }
        let rows = exec_collect!(self.graph, q);
        rows.into_iter()
            .map(|row| {
                let to: String = row.get("to_id").map_err(gerr!("to_id"))?;
                decode_edge(row, from.clone(), to)
            })
            .collect()
    }

    async fn edges_to(&self, to: &NodeId, label: Option<&str>) -> Result<Vec<Edge>> {
        let cypher = if label.is_some() {
            "MATCH (a:OxideNode)-[r:OXIDE_EDGE {edge_label: $label}]->(:OxideNode {node_id: $to}) \
             RETURN r.edge_id AS eid, a.node_id AS from_id, r.edge_label AS lbl, r.edge_props AS props"
        } else {
            "MATCH (a:OxideNode)-[r:OXIDE_EDGE]->(:OxideNode {node_id: $to}) \
             RETURN r.edge_id AS eid, a.node_id AS from_id, r.edge_label AS lbl, r.edge_props AS props"
        };
        let mut q = query(cypher).param("to", to.clone());
        if let Some(l) = label {
            q = q.param("label", l.to_string());
        }
        let rows = exec_collect!(self.graph, q);
        rows.into_iter()
            .map(|row| {
                let from: String = row.get("from_id").map_err(gerr!("from_id"))?;
                decode_edge(row, from, to.clone())
            })
            .collect()
    }

    async fn stats(&self) -> Result<(usize, usize)> {
        let node_count: i64 = self
            .graph
            .execute(query("MATCH (n:OxideNode) RETURN count(n) AS cnt"))
            .await
            .map_err(nerr!("node count"))?
            .column_into_stream::<i64>("cnt")
            .try_fold(0i64, |_, x| async move { Ok(x) })
            .await
            .map_err(nerr!("node count fold"))?;

        let edge_count: i64 = self
            .graph
            .execute(query("MATCH ()-[r:OXIDE_EDGE]->() RETURN count(r) AS cnt"))
            .await
            .map_err(nerr!("edge count"))?
            .column_into_stream::<i64>("cnt")
            .try_fold(0i64, |_, x| async move { Ok(x) })
            .await
            .map_err(nerr!("edge count fold"))?;

        Ok((node_count as usize, edge_count as usize))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn decode_edge(row: Row, from: String, to: String) -> Result<Edge> {
    let eid: String = row.get("eid").map_err(gerr!("eid"))?;
    let lbl: String = row.get("lbl").map_err(gerr!("lbl"))?;
    let props_str: String = row.get("props").map_err(gerr!("props"))?;
    Ok(Edge {
        id: eid,
        from,
        to,
        label: lbl,
        properties: serde_json::from_str(&props_str).map_err(jerr!("props"))?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Edge;
    use serde_json::json;

    async fn connect() -> Option<Neo4jGraph> {
        Neo4jGraph::connect("bolt://localhost:7687", "neo4j", "password")
            .await
            .ok()
    }

    #[tokio::test]
    async fn neo4j_satisfies_graph_store_trait() {
        fn _assert<G: GraphStore + Send + Sync>() {}
        _assert::<Neo4jGraph>();
    }

    #[ignore]
    #[tokio::test]
    async fn neo4j_upsert_and_get_node() {
        let Some(g) = connect().await else {
            return;
        };
        let node = Node::new("neo4j:test:1", "pet").with_property("name", json!("Fido"));
        g.upsert_node(node).await.unwrap();
        let n = g
            .get_node(&"neo4j:test:1".to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n.labels, vec!["pet"]);
        assert_eq!(n.properties["name"], json!("Fido"));
    }

    #[ignore]
    #[tokio::test]
    async fn neo4j_add_edge_and_traverse() {
        let Some(g) = connect().await else {
            return;
        };
        g.upsert_node(Node::new("neo4j:a", "thing")).await.unwrap();
        g.upsert_node(Node::new("neo4j:b", "thing")).await.unwrap();
        let eid = g
            .add_edge(Edge::new("neo4j:a", "neo4j:b", "knows"))
            .await
            .unwrap();
        assert!(!eid.is_empty());
        let from_a = g
            .edges_from(&"neo4j:a".to_string(), Some("knows"))
            .await
            .unwrap();
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_a[0].to, "neo4j:b");
    }

    #[ignore]
    #[tokio::test]
    async fn neo4j_nodes_by_label() {
        let Some(g) = connect().await else {
            return;
        };
        g.upsert_node(Node::new("lbl:1", "animal")).await.unwrap();
        g.upsert_node(Node::new("lbl:2", "animal")).await.unwrap();
        g.upsert_node(Node::new("lbl:3", "plant")).await.unwrap();
        let animals = g.nodes_by_label("animal").await.unwrap();
        assert!(animals.len() >= 2);
    }

    #[ignore]
    #[tokio::test]
    async fn neo4j_stats() {
        let Some(g) = connect().await else {
            return;
        };
        g.upsert_node(Node::new("st:1", "x")).await.unwrap();
        g.upsert_node(Node::new("st:2", "x")).await.unwrap();
        g.add_edge(Edge::new("st:1", "st:2", "link")).await.unwrap();
        let (n, e) = g.stats().await.unwrap();
        assert!(n >= 2);
        assert!(e >= 1);
    }
}

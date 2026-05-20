//! SQLite-backed [`PersistentGraph`].

use std::str::FromStr;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::error::{GraphError, Result};
use crate::graph::{Edge, EdgeId, GraphStore, Node, NodeId};

/// Persistent, SQLite-backed knowledge graph.
#[derive(Clone)]
pub struct PersistentGraph {
    pool: SqlitePool,
}

impl PersistentGraph {
    /// Open an in-memory database for testing.
    pub async fn in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// Open or create an on-disk graph at `path`.
    pub async fn open(path: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS nodes (
                id TEXT PRIMARY KEY,
                labels TEXT NOT NULL,
                properties TEXT NOT NULL
            )"#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::Other(e.into()))?;

        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS edges (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                target TEXT NOT NULL,
                label TEXT NOT NULL,
                properties TEXT NOT NULL
            )"#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::Other(e.into()))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source)")
            .execute(&self.pool)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target)")
            .execute(&self.pool)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;

        Ok(())
    }
}

#[async_trait]
impl GraphStore for PersistentGraph {
    async fn upsert_node(&self, node: Node) -> Result<()> {
        let labels_json =
            serde_json::to_string(&node.labels).map_err(|e| GraphError::Other(e.into()))?;
        let props_json =
            serde_json::to_string(&node.properties).map_err(|e| GraphError::Other(e.into()))?;

        sqlx::query(
            r#"INSERT INTO nodes (id, labels, properties)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(id) DO UPDATE SET
                 labels = excluded.labels,
                 properties = excluded.properties"#,
        )
        .bind(&node.id)
        .bind(labels_json)
        .bind(props_json)
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::Other(e.into()))?;
        Ok(())
    }

    async fn add_edge(&self, edge: Edge) -> Result<EdgeId> {
        let props_json =
            serde_json::to_string(&edge.properties).map_err(|e| GraphError::Other(e.into()))?;

        // Ensure nodes exist
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| GraphError::Other(e.into()))?;
        let source_exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM nodes WHERE id = ?1")
            .bind(&edge.from)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;
        if source_exists.is_none() {
            return Err(GraphError::UnknownNode(edge.from));
        }

        let target_exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM nodes WHERE id = ?1")
            .bind(&edge.to)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;
        if target_exists.is_none() {
            return Err(GraphError::UnknownNode(edge.to));
        }

        sqlx::query(
            r#"INSERT INTO edges (id, source, target, label, properties)
               VALUES (?1, ?2, ?3, ?4, ?5)"#,
        )
        .bind(&edge.id)
        .bind(&edge.from)
        .bind(&edge.to)
        .bind(&edge.label)
        .bind(props_json)
        .execute(&mut *tx)
        .await
        .map_err(|e| GraphError::Other(e.into()))?;

        tx.commit().await.map_err(|e| GraphError::Other(e.into()))?;
        Ok(edge.id)
    }

    async fn get_node(&self, id: &NodeId) -> Result<Option<Node>> {
        let row = sqlx::query("SELECT labels, properties FROM nodes WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;

        if let Some(r) = row {
            let labels_str: String = r
                .try_get("labels")
                .map_err(|e| GraphError::Other(e.into()))?;
            let props_str: String = r
                .try_get("properties")
                .map_err(|e| GraphError::Other(e.into()))?;

            let labels: Vec<String> = serde_json::from_str(&labels_str).unwrap_or_default();
            let properties: serde_json::Map<String, Value> =
                serde_json::from_str(&props_str).unwrap_or_default();

            Ok(Some(Node {
                id: id.clone(),
                labels,
                properties,
            }))
        } else {
            Ok(None)
        }
    }

    async fn get_edge(&self, id: &EdgeId) -> Result<Option<Edge>> {
        let row = sqlx::query("SELECT source, target, label, properties FROM edges WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;

        if let Some(r) = row {
            let from: String = r
                .try_get("source")
                .map_err(|e| GraphError::Other(e.into()))?;
            let to: String = r
                .try_get("target")
                .map_err(|e| GraphError::Other(e.into()))?;
            let label: String = r
                .try_get("label")
                .map_err(|e| GraphError::Other(e.into()))?;
            let props_str: String = r
                .try_get("properties")
                .map_err(|e| GraphError::Other(e.into()))?;

            let properties: serde_json::Map<String, Value> =
                serde_json::from_str(&props_str).unwrap_or_default();

            Ok(Some(Edge {
                id: id.clone(),
                from,
                to,
                label,
                properties,
            }))
        } else {
            Ok(None)
        }
    }

    async fn nodes_by_label(&self, label: &str) -> Result<Vec<Node>> {
        let pattern = format!("%\"{}\"%", label);
        let rows = sqlx::query("SELECT id, labels, properties FROM nodes WHERE labels LIKE ?1")
            .bind(pattern)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;

        let mut nodes = Vec::new();
        for r in rows {
            let id: String = r.try_get("id").map_err(|e| GraphError::Other(e.into()))?;
            let labels_str: String = r
                .try_get("labels")
                .map_err(|e| GraphError::Other(e.into()))?;
            let props_str: String = r
                .try_get("properties")
                .map_err(|e| GraphError::Other(e.into()))?;

            let labels: Vec<String> = serde_json::from_str(&labels_str).unwrap_or_default();
            if labels.contains(&label.to_string()) {
                let properties: serde_json::Map<String, Value> =
                    serde_json::from_str(&props_str).unwrap_or_default();
                nodes.push(Node {
                    id,
                    labels,
                    properties,
                });
            }
        }
        Ok(nodes)
    }

    async fn edges_from(&self, from: &NodeId, label: Option<&str>) -> Result<Vec<Edge>> {
        let query = if label.is_some() {
            "SELECT id, target, label, properties FROM edges WHERE source = ?1 AND label = ?2"
        } else {
            "SELECT id, target, label, properties FROM edges WHERE source = ?1"
        };

        let mut q = sqlx::query(query).bind(from);
        if let Some(l) = label {
            q = q.bind(l);
        }

        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;

        let mut edges = Vec::new();
        for r in rows {
            let id: String = r.try_get("id").map_err(|e| GraphError::Other(e.into()))?;
            let to: String = r
                .try_get("target")
                .map_err(|e| GraphError::Other(e.into()))?;
            let l: String = r
                .try_get("label")
                .map_err(|e| GraphError::Other(e.into()))?;
            let props_str: String = r
                .try_get("properties")
                .map_err(|e| GraphError::Other(e.into()))?;

            let properties: serde_json::Map<String, Value> =
                serde_json::from_str(&props_str).unwrap_or_default();
            edges.push(Edge {
                id,
                from: from.clone(),
                to,
                label: l,
                properties,
            });
        }
        Ok(edges)
    }

    async fn edges_to(&self, to: &NodeId, label: Option<&str>) -> Result<Vec<Edge>> {
        let query = if label.is_some() {
            "SELECT id, source, label, properties FROM edges WHERE target = ?1 AND label = ?2"
        } else {
            "SELECT id, source, label, properties FROM edges WHERE target = ?1"
        };

        let mut q = sqlx::query(query).bind(to);
        if let Some(l) = label {
            q = q.bind(l);
        }

        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| GraphError::Other(e.into()))?;

        let mut edges = Vec::new();
        for r in rows {
            let id: String = r.try_get("id").map_err(|e| GraphError::Other(e.into()))?;
            let from: String = r
                .try_get("source")
                .map_err(|e| GraphError::Other(e.into()))?;
            let l: String = r
                .try_get("label")
                .map_err(|e| GraphError::Other(e.into()))?;
            let props_str: String = r
                .try_get("properties")
                .map_err(|e| GraphError::Other(e.into()))?;

            let properties: serde_json::Map<String, Value> =
                serde_json::from_str(&props_str).unwrap_or_default();
            edges.push(Edge {
                id,
                from,
                to: to.clone(),
                label: l,
                properties,
            });
        }
        Ok(edges)
    }

    async fn stats(&self) -> Result<(usize, usize)> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nodes")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
        let e: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM edges")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
        Ok((n as usize, e as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn upsert_and_query_by_label() {
        let g = PersistentGraph::in_memory().await.unwrap();
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
        let g = PersistentGraph::in_memory().await.unwrap();
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
        let g = PersistentGraph::in_memory().await.unwrap();
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
        let g = PersistentGraph::in_memory().await.unwrap();
        g.upsert_node(Node::new("a", "n")).await.unwrap();
        g.upsert_node(Node::new("b", "n")).await.unwrap();
        g.add_edge(Edge::new("a", "b", "x")).await.unwrap();
        assert_eq!(g.stats().await.unwrap(), (2, 1));
    }
}

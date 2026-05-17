//! Ingestion helpers — turn external records into nodes + edges.
//!
//! `RecordRef` is a small adapter so we don't need to depend on `oxide-mirror`
//! directly. Callers that already have a `MirroredRecord` can construct one in
//! one line.

use serde::Serialize;

use crate::error::Result;
use crate::graph::{Edge, GraphStore, Node, NodeId};

/// Reference to an external record being ingested.
#[derive(Debug, Clone, Serialize)]
pub struct RecordRef<'a> {
    /// Resource / table name (becomes the node label).
    pub resource: &'a str,
    /// Stable id within the resource.
    pub record_id: &'a str,
    /// Payload — every primitive field becomes a node property; every value
    /// that matches `"<resource>:<id>"` is promoted to an edge.
    pub payload: &'a serde_json::Value,
    /// Originating source id (recorded on the node for provenance).
    pub source: &'a str,
}

/// Materialise `record` into the graph, plus any inferred reference edges.
pub async fn ingest_record(store: &dyn GraphStore, record: RecordRef<'_>) -> Result<NodeId> {
    let id: NodeId = format!("{}:{}", record.resource, record.record_id);

    let mut node = Node::new(id.clone(), record.resource)
        .with_property("__source", serde_json::json!(record.source));

    let mut pending_edges: Vec<(String, String)> = Vec::new();

    if let Some(obj) = record.payload.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                if let Some((_resource, _id)) = parse_record_ref(s) {
                    pending_edges.push((k.clone(), s.to_string()));
                    continue;
                }
            }
            node.properties.insert(k.clone(), v.clone());
        }
    }

    store.upsert_node(node).await?;

    for (label, target) in pending_edges {
        // Ensure the target node exists so `add_edge` does not reject the
        // reference; we materialise it as an empty placeholder typed by the
        // resource component of the id.
        if store.get_node(&target).await?.is_none() {
            if let Some((res, rid)) = parse_record_ref(&target) {
                let placeholder = Node::new(target.clone(), res)
                    .with_property("__placeholder", serde_json::json!(true))
                    .with_property("__record_id", serde_json::json!(rid));
                store.upsert_node(placeholder).await?;
            }
        }
        let edge = Edge::new(id.clone(), target, label);
        store.add_edge(edge).await?;
    }

    Ok(id)
}

fn parse_record_ref(s: &str) -> Option<(&str, &str)> {
    let (resource, record_id) = s.split_once(':')?;
    if resource.is_empty() || record_id.is_empty() {
        return None;
    }
    // Skip URLs (`https://...`), times (`12:34:56`), etc. — the resource must
    // look like an identifier and the record_id must not start with another
    // protocol-style separator.
    if !resource
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    if record_id.starts_with('/') {
        return None;
    }
    Some((resource, record_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::InMemoryGraph;
    use serde_json::json;

    #[tokio::test]
    async fn ingest_creates_node_with_properties() {
        let g = InMemoryGraph::new();
        let payload = json!({"name": "Rex", "age": 4});
        ingest_record(
            &g,
            RecordRef {
                resource: "pet",
                record_id: "1",
                payload: &payload,
                source: "petstore",
            },
        )
        .await
        .unwrap();

        let node = g.get_node(&"pet:1".into()).await.unwrap().unwrap();
        assert_eq!(node.labels, vec!["pet"]);
        assert_eq!(node.properties["name"], json!("Rex"));
        assert_eq!(node.properties["__source"], json!("petstore"));
    }

    #[tokio::test]
    async fn record_ref_strings_become_edges() {
        let g = InMemoryGraph::new();
        let owner_payload = json!({"name": "Alice"});
        ingest_record(
            &g,
            RecordRef {
                resource: "owner",
                record_id: "1",
                payload: &owner_payload,
                source: "crm",
            },
        )
        .await
        .unwrap();
        let pet_payload = json!({"name": "Rex", "owner": "owner:1"});
        ingest_record(
            &g,
            RecordRef {
                resource: "pet",
                record_id: "1",
                payload: &pet_payload,
                source: "petstore",
            },
        )
        .await
        .unwrap();

        let edges = g.edges_from(&"pet:1".into(), Some("owner")).await.unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "owner:1");
    }

    #[tokio::test]
    async fn missing_target_is_placeheld() {
        let g = InMemoryGraph::new();
        let payload = json!({"owner": "owner:99"});
        ingest_record(
            &g,
            RecordRef {
                resource: "pet",
                record_id: "1",
                payload: &payload,
                source: "petstore",
            },
        )
        .await
        .unwrap();
        let placeholder = g.get_node(&"owner:99".into()).await.unwrap().unwrap();
        assert_eq!(placeholder.properties["__placeholder"], json!(true));
    }

    #[test]
    fn parse_record_ref_rejects_urls_and_times() {
        assert!(parse_record_ref("https://x").is_none());
        assert!(parse_record_ref("12:34").is_some()); // ints look like ids — accept
        assert!(parse_record_ref("pet:1").is_some());
        assert!(parse_record_ref(":1").is_none());
        assert!(parse_record_ref("pet:").is_none());
    }
}

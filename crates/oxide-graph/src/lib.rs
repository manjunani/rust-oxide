//! # `oxide-graph` — Semantic Knowledge Graph
//!
//! In-memory typed property graph used by Rust Oxide to fuse data from
//! `oxide-mirror` (structured API rows), `oxide-browser-sh` (web
//! extractions), and any other module that emits facts. The graph stays
//! pure-Rust + dependency-free so it works in tests and embedded contexts;
//! the same shape can later back onto a remote Neo4j / Dgraph cluster via an
//! alternate [`GraphStore`] implementation.
//!
//! Layout:
//!
//! * [`graph`] — `Node`, `Edge`, in-memory [`InMemoryGraph`] with id-keyed
//!   storage, label / property indices, and undirected lookup.
//! * [`query`] — `NodeQuery`, `EdgeQuery`, traversal primitives.
//! * [`ingest`] — build helpers that translate mirrored records into nodes
//!   and follow JSON references to seed edges.
//! * [`kernel`] — [`GraphModule`](kernel::GraphModule) wires the graph onto
//!   the `oxide-k` message bus.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod error;
pub mod graph;
pub mod ingest;
pub mod kernel;
#[cfg(feature = "persist")]
pub mod persist;
pub mod query;

pub use error::{GraphError, Result};
pub use graph::{Edge, EdgeId, GraphStore, InMemoryGraph, Node, NodeId};
pub use ingest::{ingest_record, RecordRef};
pub use kernel::GraphModule;
pub use query::{EdgeQuery, NodeQuery};

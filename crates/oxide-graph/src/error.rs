//! Error type for the graph layer.

use thiserror::Error;

/// All errors produced by the graph.
#[derive(Debug, Error)]
pub enum GraphError {
    /// Node id was not present in the store.
    #[error("unknown node `{0}`")]
    UnknownNode(String),

    /// Edge id was not present.
    #[error("unknown edge `{0}`")]
    UnknownEdge(String),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Wrapped kernel error.
    #[error("kernel error: {0}")]
    Kernel(#[from] oxide_k::KernelError),

    /// Catch-all.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, GraphError>;

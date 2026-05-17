//! Error type.

use thiserror::Error;

/// All errors produced by the mesh.
#[derive(Debug, Error)]
pub enum MeshError {
    /// I/O failure (TCP transport).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Peer was not found in the local registry.
    #[error("unknown peer `{0}`")]
    UnknownPeer(String),

    /// Wrapped kernel error.
    #[error("kernel error: {0}")]
    Kernel(#[from] oxide_k::KernelError),

    /// Catch-all.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, MeshError>;

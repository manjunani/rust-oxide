//! Error type for the MCP server.

use thiserror::Error;

/// All errors produced by the MCP server.
#[derive(Debug, Error)]
pub enum McpError {
    /// JSON (de)serialization failed (e.g. malformed JSON-RPC envelope).
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Tool not found in the registry.
    #[error("unknown tool `{0}`")]
    UnknownTool(String),

    /// Tool execution failed at runtime.
    #[error("tool `{tool}` failed: {message}")]
    ToolFailed {
        /// Tool name.
        tool: String,
        /// Detail.
        message: String,
    },

    /// I/O error from spawning a subprocess or reading its output.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Wrapped kernel error (bus dispatch).
    #[error("kernel error: {0}")]
    Kernel(#[from] oxide_k::KernelError),

    /// Catch-all.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, McpError>;

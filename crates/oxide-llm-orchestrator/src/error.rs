//! Error type for the orchestrator.

use thiserror::Error;

/// Errors produced by the LLM orchestrator.
#[derive(Debug, Error)]
pub enum LlmError {
    /// HTTP-level failure when talking to the provider.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// Provider returned a non-2xx HTTP status. The body is included for
    /// diagnostics.
    #[error("provider returned {status}: {body}")]
    Provider {
        /// HTTP status.
        status: u16,
        /// Response body (truncated to 4 KiB).
        body: String,
    },

    /// The provider replied with no choices / completion content.
    #[error("provider returned empty completion")]
    EmptyCompletion,

    /// Could not parse the provider's response.
    #[error("invalid provider response: {0}")]
    InvalidResponse(String),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Kernel / bus failure.
    #[error("kernel error: {0}")]
    Kernel(#[from] oxide_k::KernelError),

    /// Catch-all.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, LlmError>;

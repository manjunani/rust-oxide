//! Error type for `oxide-browser-sh`.

use thiserror::Error;

use crate::action::Selector;

/// All errors produced by the browser module.
#[derive(Debug, Error)]
pub enum BrowserError {
    /// The backend rejected a navigation request.
    #[error("navigation failed for `{url}`: {message}")]
    Navigation {
        /// URL that failed to load.
        url: String,
        /// Backend-supplied detail.
        message: String,
    },

    /// A selector did not match any element on the page.
    #[error("element not found: {0:?}")]
    NotFound(Selector),

    /// Self-healing exhausted its retry budget without succeeding.
    #[error("self-healing gave up after {attempts} attempt(s); last error: {last_error}")]
    HealingExhausted {
        /// Number of attempts made.
        attempts: usize,
        /// Stringified last underlying error.
        last_error: String,
    },

    /// The active backend does not support the requested operation (e.g. a
    /// mock backend asked for a real screenshot).
    #[error("backend does not support `{0}`")]
    Unsupported(&'static str),

    /// A backend-specific I/O / driver failure.
    #[error("backend error: {0}")]
    Backend(String),

    /// HTML / Markdown extraction failed.
    #[error("extraction error: {0}")]
    Extraction(String),

    /// Bus / kernel integration failure.
    #[error("kernel error: {0}")]
    Kernel(#[from] oxide_k::KernelError),

    /// JSON (de)serialization.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Underlying chromiumoxide CDP error.
    #[error("chromium error: {0}")]
    Chromium(String),

    /// Catch-all.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, BrowserError>;

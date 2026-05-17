//! Error types for `oxide-gen`.

use std::path::PathBuf;
use thiserror::Error;

/// All errors produced by `oxide-gen`.
#[derive(Debug, Error)]
pub enum GenError {
    /// Failed to read the input spec from disk.
    #[error("failed to read spec {path}: {source}")]
    ReadSpec {
        /// Path that failed.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to write generated output to disk.
    #[error("failed to write {path}: {source}")]
    WriteOutput {
        /// Path that failed.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// The spec could not be parsed.
    #[error("parse error ({kind}): {message}")]
    Parse {
        /// Which parser failed.
        kind: &'static str,
        /// Human-readable detail.
        message: String,
    },

    /// JSON (de)serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// YAML (de)serialization failed.
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// A generic catch-all.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, GenError>;

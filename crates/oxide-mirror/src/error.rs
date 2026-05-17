//! Error type for `oxide-mirror`.

use thiserror::Error;

/// All errors produced by the mirror.
#[derive(Debug, Error)]
pub enum MirrorError {
    /// Underlying SQLite / sqlx error.
    #[error("sqlite error: {0}")]
    Sql(#[from] sqlx::Error),

    /// JSON (de)serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The `query` API rejected the supplied SQL because it is not a
    /// read-only `SELECT` / `WITH` / `PRAGMA` statement.
    #[error("read-only query rejected: {0}")]
    QueryNotReadOnly(String),

    /// A source returned a delta for a resource that has not been
    /// pre-registered with the store.
    #[error("unknown resource `{resource}` (register it via `MirrorStore::register_resource`)")]
    UnknownResource {
        /// Resource name that was not registered.
        resource: String,
    },

    /// A source was requested by id but is not registered with the
    /// [`Syncer`](crate::sync::Syncer).
    #[error("unknown source `{0}`")]
    UnknownSource(String),

    /// Wrapped `oxide-k` kernel error (bus / registry).
    #[error("kernel error: {0}")]
    Kernel(#[from] oxide_k::KernelError),

    /// Anything else.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, MirrorError>;

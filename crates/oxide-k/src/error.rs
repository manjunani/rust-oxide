//! Unified error and result types for the Oxide Kernel.

use thiserror::Error;

/// All errors that can be produced by the kernel and its subsystems.
#[derive(Debug, Error)]
pub enum KernelError {
    /// A module operation failed (load, start, stop, unload, etc.).
    #[error("module error: {0}")]
    Module(String),

    /// A message could not be routed on the bus, e.g. because no receiver is
    /// listening or the channel was closed.
    #[error("bus error: {0}")]
    Bus(String),

    /// The state registry returned a database-level error.
    #[error("registry error: {0}")]
    Registry(#[from] sqlx::Error),

    /// A configuration key was not found in the registry.
    #[error("configuration key not found: {0}")]
    ConfigNotFound(String),

    /// A duplicate module id was registered with the [`ModuleManager`](crate::module::ModuleManager).
    #[error("module already registered: {0}")]
    DuplicateModule(String),

    /// A module id was referenced that is not known to the manager.
    #[error("unknown module: {0}")]
    UnknownModule(String),

    /// JSON (de)serialization failed.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// A publish was rejected because the publisher lacks the required capability.
    #[error("access denied: `{publisher}` lacks capability `{capability}`")]
    Denied {
        /// The publisher that attempted the operation.
        publisher: String,
        /// The capability required.
        capability: String,
    },

    /// An unexpected, unclassified error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenient `Result` alias used throughout the kernel.
pub type Result<T> = std::result::Result<T, KernelError>;

//! # Oxide Kernel (`oxide-k`)
//!
//! The `oxide-k` crate is the central micro-kernel of the Rust Oxide Agent-Native OS.
//! It orchestrates modules, mediates inter-module communication through a secure
//! message bus, and maintains a global state registry backed by SQLite.
//!
//! The kernel is designed around three independent yet cooperating subsystems:
//!
//! * [`module`] – Module orchestration. Defines the [`Module`](module::Module) and
//!   [`WasmModule`](module::WasmModule) traits and provides a
//!   [`ModuleManager`](module::ModuleManager) that handles lifecycle (load, start,
//!   stop, unload) for both native Rust and WebAssembly plugins.
//! * [`bus`] – Secure message bus. An asynchronous message-passing layer built on
//!   `tokio::sync::mpsc` with strongly-typed [`Command`](bus::Command) and
//!   [`Event`](bus::Event) messages, plus an [`Envelope`](bus::Envelope) that
//!   carries provenance metadata.
//! * [`registry`] – Global state registry. A `sqlx`-backed SQLite store (in-memory
//!   by default for development) for module metadata and configuration values.
//!
//! These are tied together by the [`Kernel`](kernel::Kernel) façade, which exposes
//! a single entry point for kernel initialization and orchestration.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod bus;
pub mod error;
pub mod kernel;
pub mod manifest;
pub mod module;
pub mod registry;
pub mod xai;

#[cfg(feature = "wasmtime-runtime")]
pub mod wasm_exec;

pub use error::{KernelError, Result};
pub use kernel::Kernel;

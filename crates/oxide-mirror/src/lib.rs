//! # `oxide-mirror` — Local, Event-Sourced Data Mirror
//!
//! `oxide-mirror` keeps a SQLite-backed local copy of remote API data so AI
//! agents can run complex SQL queries (joins, aggregations, time-window
//! filters) across many services without paying the latency or rate-limit
//! cost of going over the wire every time.
//!
//! The crate is organised around four cooperating pieces:
//!
//! * [`event`] — the durable, append-only [`Delta`](event::Delta) log shape
//!   that every sync source emits. Each delta carries an explicit
//!   [`Provenance`](event::Provenance) (source id + confidence) so callers can
//!   reason about who wrote what.
//! * [`source`] — the [`SyncSource`](source::SyncSource) trait that
//!   `oxide-gen` generated clients implement. A source knows how to pull a
//!   batch of deltas starting from a cursor and to advance the cursor when
//!   the batch has been applied.
//! * [`store`] — [`MirrorStore`](store::MirrorStore) owns the SQLite pool,
//!   the schema migrations, the event log, the materialised
//!   [`MirroredRecord`](event::MirroredRecord) table, and the per-source
//!   cursor table. Exposes [`MirrorStore::query`] for read-only SQL.
//! * [`sync`] — [`Syncer`](sync::Syncer) ties the previous two together,
//!   pulling deltas from a `SyncSource`, applying them through a
//!   [`ConflictStrategy`](conflict::ConflictStrategy), and persisting the
//!   new cursor.
//!
//! [`kernel::MirrorModule`] wires everything into the `oxide-k` kernel so
//! sync runs and queries can be triggered via the kernel message bus.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod conflict;
pub mod error;
pub mod event;
pub mod kernel;
pub mod source;
pub mod store;
pub mod sync;

pub use conflict::{
    ConflictResolution, ConflictStrategy, HighestConfidence, KeepLocal, LastWriteWins, MergeJson,
};
pub use error::{MirrorError, Result};
pub use event::{Delta, DeltaOp, MirroredRecord, Provenance};
pub use kernel::MirrorModule;
pub use source::{PullResult, StaticSource, SyncSource};
pub use store::MirrorStore;
pub use sync::{SyncReport, Syncer};

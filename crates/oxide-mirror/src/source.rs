//! The [`SyncSource`] trait + reusable test helpers.

use async_trait::async_trait;
use std::sync::Mutex;

use crate::error::Result;
use crate::event::Delta;

/// A batch of deltas returned by [`SyncSource::pull`].
#[derive(Debug, Clone, Default)]
pub struct PullResult {
    /// The deltas to apply, in the order the source observed them.
    pub deltas: Vec<Delta>,
    /// Opaque cursor that callers should store and pass back on the next
    /// pull. `None` means "no further pages" — the syncer treats this as
    /// "leave the existing cursor untouched" so a future call resumes from
    /// the same point.
    pub next_cursor: Option<String>,
    /// Whether the source has more pages immediately available.
    pub has_more: bool,
}

/// Anything that can emit deltas into the local mirror.
///
/// Generated `oxide-gen` clients implement this by translating their API's
/// list / change-feed endpoints into delta batches.
#[async_trait]
pub trait SyncSource: Send + Sync {
    /// Stable id of the source (matches `Provenance::source`).
    fn id(&self) -> &str;

    /// Pull the next batch of deltas. `cursor` is the value the source
    /// returned from the previous call, or `None` for the first pull.
    async fn pull(&self, cursor: Option<String>) -> Result<PullResult>;
}

/// In-memory [`SyncSource`] used by tests.
///
/// Holds a list of pre-baked [`PullResult`] batches and serves them in order;
/// once exhausted, subsequent calls return an empty batch with `has_more =
/// false`.
pub struct StaticSource {
    id: String,
    batches: Mutex<Vec<PullResult>>,
}

impl StaticSource {
    /// Build a static source with the given id and batches.
    pub fn new(id: impl Into<String>, batches: Vec<PullResult>) -> Self {
        Self {
            id: id.into(),
            batches: Mutex::new(batches),
        }
    }

    /// Convenience: build a single-batch source from a flat delta list.
    pub fn from_deltas(id: impl Into<String>, deltas: Vec<Delta>) -> Self {
        Self::new(
            id,
            vec![PullResult {
                deltas,
                next_cursor: None,
                has_more: false,
            }],
        )
    }

    /// Push another batch to be served on the next pull.
    pub fn enqueue(&self, batch: PullResult) {
        self.batches.lock().unwrap().push(batch);
    }
}

#[async_trait]
impl SyncSource for StaticSource {
    fn id(&self) -> &str {
        &self.id
    }

    async fn pull(&self, _cursor: Option<String>) -> Result<PullResult> {
        let mut batches = self.batches.lock().unwrap();
        if batches.is_empty() {
            Ok(PullResult::default())
        } else {
            Ok(batches.remove(0))
        }
    }
}

//! Glue layer that drives a [`SyncSource`] against a [`MirrorStore`].

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::conflict::{ConflictStrategy, LastWriteWins};
use crate::error::{MirrorError, Result};
use crate::source::SyncSource;
use crate::store::MirrorStore;

/// What happened during a single [`Syncer::sync_source`] run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncReport {
    /// Source id that was synced.
    pub source: String,
    /// Total deltas pulled.
    pub pulled: usize,
    /// Deltas that actually changed `mirror_records`.
    pub applied: usize,
    /// Deltas that were rejected by the conflict strategy.
    pub skipped: usize,
    /// Cursor returned by the source's final batch.
    pub final_cursor: Option<String>,
    /// Per-resource counters (handy for cross-resource sources).
    pub per_resource: HashMap<String, ResourceCounters>,
}

/// Per-resource counters inside a [`SyncReport`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceCounters {
    /// Deltas pulled.
    pub pulled: usize,
    /// Deltas applied.
    pub applied: usize,
    /// Deltas skipped.
    pub skipped: usize,
}

/// Holds the registered [`SyncSource`]s and runs them against a
/// [`MirrorStore`].
pub struct Syncer {
    store: MirrorStore,
    sources: HashMap<String, Arc<dyn SyncSource>>,
    strategy: Arc<dyn ConflictStrategy>,
}

impl Syncer {
    /// Build a syncer with the [`LastWriteWins`] default strategy.
    pub fn new(store: MirrorStore) -> Self {
        Self::with_strategy(store, Arc::new(LastWriteWins))
    }

    /// Build a syncer with a custom conflict strategy.
    pub fn with_strategy(store: MirrorStore, strategy: Arc<dyn ConflictStrategy>) -> Self {
        Self {
            store,
            sources: HashMap::new(),
            strategy,
        }
    }

    /// Register a source so it can be referenced by id later.
    pub fn register_source(&mut self, source: Arc<dyn SyncSource>) {
        self.sources.insert(source.id().to_string(), source);
    }

    /// Read-only access to the store, e.g. for queries.
    pub fn store(&self) -> &MirrorStore {
        &self.store
    }

    /// Look up a source by id.
    pub fn source(&self, id: &str) -> Option<Arc<dyn SyncSource>> {
        self.sources.get(id).cloned()
    }

    /// Sync every batch a source is able to produce in one go. Stops when the
    /// source reports `has_more = false` or returns an empty batch.
    pub async fn sync_source(&self, source_id: &str) -> Result<SyncReport> {
        let source = self
            .sources
            .get(source_id)
            .ok_or_else(|| MirrorError::UnknownSource(source_id.to_string()))?;
        self.sync_with(source.as_ref()).await
    }

    /// Sync a single concrete [`SyncSource`] (handy when callers manage their
    /// own registry).
    pub async fn sync_with(&self, source: &dyn SyncSource) -> Result<SyncReport> {
        let id = source.id().to_string();
        let mut report = SyncReport {
            source: id.clone(),
            ..SyncReport::default()
        };

        // Resume from the most recent global cursor for this source. Per-
        // resource cursors are still tracked in `mirror_cursors`; this is the
        // "everything" wildcard.
        let mut cursor = self.store.get_cursor(&id, "*").await?;

        loop {
            let batch = source.pull(cursor.clone()).await?;
            if batch.deltas.is_empty() && !batch.has_more {
                if let Some(next) = batch.next_cursor.clone() {
                    self.store.set_cursor(&id, "*", Some(&next)).await?;
                    report.final_cursor = Some(next);
                }
                break;
            }
            for delta in &batch.deltas {
                let counters = report
                    .per_resource
                    .entry(delta.resource.clone())
                    .or_default();
                counters.pulled += 1;
                report.pulled += 1;
                let outcome = self
                    .store
                    .apply_delta(delta, self.strategy.as_ref())
                    .await?;
                if outcome.applied {
                    counters.applied += 1;
                    report.applied += 1;
                    self.store
                        .set_cursor(&id, &delta.resource, Some(&delta.record_id))
                        .await?;
                } else {
                    counters.skipped += 1;
                    report.skipped += 1;
                }
            }
            if let Some(next) = batch.next_cursor.clone() {
                self.store.set_cursor(&id, "*", Some(&next)).await?;
                cursor = Some(next.clone());
                report.final_cursor = Some(next);
            }
            if !batch.has_more {
                break;
            }
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conflict::{HighestConfidence, LastWriteWins};
    use crate::event::Delta;
    use crate::source::{PullResult, StaticSource};
    use serde_json::json;

    fn upsert(id: &str, payload: serde_json::Value) -> Delta {
        Delta::upsert("pets", id, payload, "static")
    }

    #[tokio::test]
    async fn sync_pulls_and_applies_single_batch() {
        let store = MirrorStore::in_memory().await.unwrap();
        let source = Arc::new(StaticSource::from_deltas(
            "static",
            vec![
                upsert("1", json!({"name": "Rex"})),
                upsert("2", json!({"name": "Buddy"})),
            ],
        ));
        let mut syncer = Syncer::new(store.clone());
        syncer.register_source(source);

        let report = syncer.sync_source("static").await.unwrap();
        assert_eq!(report.pulled, 2);
        assert_eq!(report.applied, 2);
        assert_eq!(report.skipped, 0);
        assert_eq!(store.list_records("pets").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sync_paginates_until_has_more_false() {
        let store = MirrorStore::in_memory().await.unwrap();
        let source = Arc::new(StaticSource::new(
            "src",
            vec![
                PullResult {
                    deltas: vec![upsert("1", json!({"n": 1}))],
                    next_cursor: Some("page-2".into()),
                    has_more: true,
                },
                PullResult {
                    deltas: vec![upsert("2", json!({"n": 2}))],
                    next_cursor: Some("page-3".into()),
                    has_more: false,
                },
            ],
        ));
        let mut syncer = Syncer::new(store.clone());
        syncer.register_source(source);

        let report = syncer.sync_source("src").await.unwrap();
        assert_eq!(report.pulled, 2);
        assert_eq!(report.final_cursor.as_deref(), Some("page-3"));
        assert_eq!(
            store.get_cursor("src", "*").await.unwrap().as_deref(),
            Some("page-3")
        );
    }

    #[tokio::test]
    async fn sync_honours_conflict_strategy() {
        let store = MirrorStore::in_memory().await.unwrap();
        let high = upsert("1", json!({"v": "good"})).with_confidence(0.9);
        let low = upsert("1", json!({"v": "bad"})).with_confidence(0.1);

        let source = Arc::new(StaticSource::from_deltas("src", vec![high, low]));
        let mut syncer = Syncer::with_strategy(store.clone(), Arc::new(HighestConfidence));
        syncer.register_source(source);

        let report = syncer.sync_source("src").await.unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.skipped, 1);
        let rec = store.get_record("pets", "1").await.unwrap().unwrap();
        assert_eq!(rec.payload["v"], json!("good"));
    }

    #[tokio::test]
    async fn unknown_source_errors() {
        let store = MirrorStore::in_memory().await.unwrap();
        let _last = LastWriteWins;
        let syncer = Syncer::new(store);
        let err = syncer.sync_source("missing").await.unwrap_err();
        assert!(matches!(err, MirrorError::UnknownSource(_)));
    }
}

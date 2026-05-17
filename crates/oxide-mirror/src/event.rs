//! Delta + record shapes shared across the mirror.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Whether a delta inserts/updates or deletes a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaOp {
    /// Insert the record if it does not exist, or replace the payload.
    Upsert,
    /// Delete the record. `payload` is ignored.
    Delete,
}

/// A single change-set emitted by a [`SyncSource`](crate::source::SyncSource).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    /// Resource (table) the delta applies to, e.g. `"pets"`.
    pub resource: String,
    /// Stable id of the record within the resource.
    pub record_id: String,
    /// Operation kind.
    pub op: DeltaOp,
    /// Record payload (JSON object). For `Delete` this is typically
    /// [`serde_json::Value::Null`].
    pub payload: serde_json::Value,
    /// When the source observed the change.
    pub occurred_at: DateTime<Utc>,
    /// Source + confidence metadata.
    pub provenance: Provenance,
}

impl Delta {
    /// Convenience: build an upsert delta.
    pub fn upsert(
        resource: impl Into<String>,
        record_id: impl Into<String>,
        payload: serde_json::Value,
        source: impl Into<String>,
    ) -> Self {
        Self {
            resource: resource.into(),
            record_id: record_id.into(),
            op: DeltaOp::Upsert,
            payload,
            occurred_at: Utc::now(),
            provenance: Provenance::new(source),
        }
    }

    /// Convenience: build a delete delta.
    pub fn delete(
        resource: impl Into<String>,
        record_id: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            resource: resource.into(),
            record_id: record_id.into(),
            op: DeltaOp::Delete,
            payload: serde_json::Value::Null,
            occurred_at: Utc::now(),
            provenance: Provenance::new(source),
        }
    }

    /// Builder helper to override the default confidence.
    #[must_use]
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.provenance.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    /// Builder helper to set a specific occurrence time.
    #[must_use]
    pub fn at(mut self, ts: DateTime<Utc>) -> Self {
        self.occurred_at = ts;
        self
    }
}

/// Source + confidence metadata attached to every delta and stored alongside
/// every mirrored record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// Stable id of the source that produced the delta.
    pub source: String,
    /// Confidence in the payload, in `[0.0, 1.0]`. Sources that have no
    /// notion of confidence should report `1.0`.
    pub confidence: f32,
}

impl Provenance {
    /// Build a provenance with `confidence = 1.0`.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            confidence: 1.0,
        }
    }
}

/// A materialised record in the local mirror.
///
/// Every column on this struct maps to a column in `mirror_records`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirroredRecord {
    /// Resource the record belongs to.
    pub resource: String,
    /// Stable record id.
    pub record_id: String,
    /// Last successfully-applied payload.
    pub payload: serde_json::Value,
    /// Source that wrote the latest version.
    pub source: String,
    /// Wall-clock time of the latest successful apply.
    pub last_synced_at: DateTime<Utc>,
    /// Confidence of the latest payload.
    pub confidence: f32,
    /// Monotonic per-record version, incremented on every successful apply.
    pub version: i64,
}

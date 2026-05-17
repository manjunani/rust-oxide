//! Conflict resolution strategies for incoming deltas.
//!
//! When a [`Delta`](crate::event::Delta) arrives for a record that already
//! exists in the local mirror, the [`MirrorStore`](crate::store::MirrorStore)
//! asks a [`ConflictStrategy`] what to do. The strategy returns a
//! [`ConflictResolution`] which is one of:
//!
//! * [`Apply`](ConflictResolution::Apply) — overwrite the existing record
//!   with the incoming delta verbatim.
//! * [`Skip`](ConflictResolution::Skip) — leave the existing record
//!   untouched. The delta is still recorded in the event log for audit, but
//!   `mirror_records` is not updated.
//! * [`ApplyMerged(value)`](ConflictResolution::ApplyMerged) — apply a
//!   strategy-computed payload (e.g. a JSON-level merge of existing and
//!   incoming).
//!
//! Four built-in strategies ship today:
//!
//! | Strategy | Behaviour |
//! |----------|-----------|
//! | [`LastWriteWins`] | Always `Apply` — newer wins. Default. |
//! | [`HighestConfidence`] | `Apply` when incoming confidence ≥ existing, else `Skip`. |
//! | [`KeepLocal`] | Always `Skip` — local writes are authoritative. |
//! | [`MergeJson`] | Deep-merge JSON objects: incoming keys override, missing keys preserved. |
//!
//! Custom strategies implement [`ConflictStrategy`] directly.

use serde_json::Value;

use crate::event::{Delta, MirroredRecord};

/// What the store should do with an incoming delta.
#[derive(Debug, Clone)]
pub enum ConflictResolution {
    /// Overwrite the existing record with the incoming delta payload.
    Apply,
    /// Leave the existing record alone; log the delta only.
    Skip,
    /// Overwrite the existing record with a custom payload.
    ApplyMerged(Value),
}

/// Pluggable strategy used by [`MirrorStore::apply_delta`](crate::store::MirrorStore::apply_delta).
pub trait ConflictStrategy: Send + Sync {
    /// Decide what to do when `incoming` collides with `existing`.
    ///
    /// `existing` is `None` when the record does not yet exist; strategies
    /// should typically return `Apply` in that case.
    fn resolve(&self, existing: Option<&MirroredRecord>, incoming: &Delta) -> ConflictResolution;

    /// Human-readable label for logs / events.
    fn label(&self) -> &'static str {
        "custom"
    }
}

/// Always overwrite; the most recent delta wins.
#[derive(Debug, Default, Clone, Copy)]
pub struct LastWriteWins;

impl ConflictStrategy for LastWriteWins {
    fn resolve(&self, _existing: Option<&MirroredRecord>, _incoming: &Delta) -> ConflictResolution {
        ConflictResolution::Apply
    }
    fn label(&self) -> &'static str {
        "last-write-wins"
    }
}

/// Apply the incoming delta only if its [`Provenance::confidence`] is
/// greater than or equal to the existing record's confidence.
#[derive(Debug, Default, Clone, Copy)]
pub struct HighestConfidence;

impl ConflictStrategy for HighestConfidence {
    fn resolve(&self, existing: Option<&MirroredRecord>, incoming: &Delta) -> ConflictResolution {
        match existing {
            None => ConflictResolution::Apply,
            Some(rec) if incoming.provenance.confidence >= rec.confidence => {
                ConflictResolution::Apply
            }
            _ => ConflictResolution::Skip,
        }
    }
    fn label(&self) -> &'static str {
        "highest-confidence"
    }
}

/// Always skip when the record already exists. Useful when the mirror is
/// authoritative and external syncs should only fill in missing rows.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeepLocal;

impl ConflictStrategy for KeepLocal {
    fn resolve(&self, existing: Option<&MirroredRecord>, _incoming: &Delta) -> ConflictResolution {
        match existing {
            None => ConflictResolution::Apply,
            Some(_) => ConflictResolution::Skip,
        }
    }
    fn label(&self) -> &'static str {
        "keep-local"
    }
}

/// Deep-merge JSON objects. Incoming keys overwrite existing ones; keys
/// missing from the incoming payload survive. Non-object payloads fall back
/// to [`LastWriteWins`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MergeJson;

impl ConflictStrategy for MergeJson {
    fn resolve(&self, existing: Option<&MirroredRecord>, incoming: &Delta) -> ConflictResolution {
        let Some(rec) = existing else {
            return ConflictResolution::Apply;
        };
        match (&rec.payload, &incoming.payload) {
            (Value::Object(_), Value::Object(_)) => {
                let mut merged = rec.payload.clone();
                merge(&mut merged, incoming.payload.clone());
                ConflictResolution::ApplyMerged(merged)
            }
            _ => ConflictResolution::Apply,
        }
    }
    fn label(&self) -> &'static str {
        "merge-json"
    }
}

fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DeltaOp, Provenance};
    use chrono::Utc;
    use serde_json::json;

    fn rec(payload: Value, confidence: f32) -> MirroredRecord {
        MirroredRecord {
            resource: "pets".into(),
            record_id: "1".into(),
            payload,
            source: "old".into(),
            last_synced_at: Utc::now(),
            confidence,
            version: 1,
        }
    }

    fn delta(payload: Value, confidence: f32) -> Delta {
        Delta {
            resource: "pets".into(),
            record_id: "1".into(),
            op: DeltaOp::Upsert,
            payload,
            occurred_at: Utc::now(),
            provenance: Provenance {
                source: "new".into(),
                confidence,
            },
        }
    }

    #[test]
    fn last_write_wins_always_applies() {
        let s = LastWriteWins;
        let existing = rec(json!({"a": 1}), 1.0);
        let incoming = delta(json!({"a": 2}), 0.1);
        assert!(matches!(s.resolve(Some(&existing), &incoming), ConflictResolution::Apply));
    }

    #[test]
    fn highest_confidence_skips_when_lower() {
        let s = HighestConfidence;
        let existing = rec(json!({"a": 1}), 0.9);
        let incoming = delta(json!({"a": 2}), 0.5);
        assert!(matches!(s.resolve(Some(&existing), &incoming), ConflictResolution::Skip));
        let incoming2 = delta(json!({"a": 3}), 0.95);
        assert!(matches!(s.resolve(Some(&existing), &incoming2), ConflictResolution::Apply));
    }

    #[test]
    fn keep_local_skips_when_exists() {
        let s = KeepLocal;
        let existing = rec(json!({}), 1.0);
        let incoming = delta(json!({}), 1.0);
        assert!(matches!(s.resolve(Some(&existing), &incoming), ConflictResolution::Skip));
        assert!(matches!(s.resolve(None, &incoming), ConflictResolution::Apply));
    }

    #[test]
    fn merge_json_deep_merges_objects() {
        let s = MergeJson;
        let existing = rec(json!({"a": 1, "nested": {"x": 1, "y": 2}}), 1.0);
        let incoming = delta(json!({"b": 2, "nested": {"y": 99, "z": 3}}), 1.0);
        match s.resolve(Some(&existing), &incoming) {
            ConflictResolution::ApplyMerged(v) => {
                assert_eq!(v["a"], 1);
                assert_eq!(v["b"], 2);
                assert_eq!(v["nested"]["x"], 1);
                assert_eq!(v["nested"]["y"], 99);
                assert_eq!(v["nested"]["z"], 3);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}

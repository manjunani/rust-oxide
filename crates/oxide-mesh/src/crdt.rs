//! Tiny CRDT primitives for federated state.
//!
//! Three building blocks ship today, all eventually-consistent under the
//! standard CRDT assumptions (commutative + idempotent merges):
//!
//! * [`GSet`] — grow-only set. Adds win; deletes are out of scope.
//! * [`LwwRegister`] — last-write-wins register. Conflicts resolved by
//!   `(timestamp_micros, peer_id)` so concurrent writes with the same
//!   timestamp are still deterministic.
//! * [`PnCounter`] — positive/negative counter. Each peer tracks its own
//!   increments and decrements; the value is `sum(p) - sum(n)`.
//!
//! All three are `Serialize`/`Deserialize` so they ride [`PeerMessage`]
//! payloads naturally.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::message::PeerId;

/// Grow-only set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GSet<T: Ord> {
    /// Underlying storage. Public so callers can iterate cheaply.
    pub items: BTreeSet<T>,
}

impl<T: Ord + Clone> GSet<T> {
    /// Build an empty set.
    pub fn new() -> Self {
        Self {
            items: BTreeSet::new(),
        }
    }

    /// Insert one element.
    pub fn add(&mut self, item: T) {
        self.items.insert(item);
    }

    /// Merge another set in-place. The CRDT merge is the set union.
    pub fn merge(&mut self, other: &Self) {
        for v in &other.items {
            self.items.insert(v.clone());
        }
    }

    /// Element count.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Last-write-wins register.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LwwRegister<V> {
    /// Most-recent value.
    pub value: V,
    /// Logical timestamp (caller-supplied — typically wall-clock micros).
    pub timestamp: u64,
    /// Peer id that wrote `value`. Used as a deterministic tie-breaker.
    pub author: PeerId,
}

impl<V: Clone> LwwRegister<V> {
    /// Build a register seeded with `value` written by `author` at
    /// `timestamp`.
    pub fn new(value: V, author: impl Into<PeerId>, timestamp: u64) -> Self {
        Self {
            value,
            timestamp,
            author: author.into(),
        }
    }

    /// Submit a write; later timestamps win, ties broken by peer id.
    pub fn write(&mut self, value: V, author: impl Into<PeerId>, timestamp: u64) {
        let author = author.into();
        if (timestamp, author.as_str()) > (self.timestamp, self.author.as_str()) {
            self.value = value;
            self.timestamp = timestamp;
            self.author = author;
        }
    }

    /// Merge another register in-place. Picks the higher
    /// `(timestamp, author)` pair.
    pub fn merge(&mut self, other: &Self) {
        if (other.timestamp, other.author.as_str()) > (self.timestamp, self.author.as_str()) {
            self.value = other.value.clone();
            self.timestamp = other.timestamp;
            self.author = other.author.clone();
        }
    }
}

/// PN-counter (positive/negative counter).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PnCounter {
    /// Per-peer positive increments.
    pub p: HashMap<PeerId, u64>,
    /// Per-peer negative decrements.
    pub n: HashMap<PeerId, u64>,
}

impl PnCounter {
    /// Build an empty counter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment by `amount` (signed) attributed to `peer`. Each peer is the
    /// authoritative writer for its own slot so concurrent writes commute.
    pub fn change(&mut self, peer: impl Into<PeerId>, amount: i64) {
        let peer = peer.into();
        if amount >= 0 {
            *self.p.entry(peer).or_default() += amount as u64;
        } else {
            *self.n.entry(peer).or_default() += amount.unsigned_abs();
        }
    }

    /// Current value.
    pub fn value(&self) -> i64 {
        let pos: u64 = self.p.values().sum();
        let neg: u64 = self.n.values().sum();
        pos as i64 - neg as i64
    }

    /// Merge another counter in-place. Each (peer, side) slot takes the max
    /// of the two values — the standard PN-counter merge.
    pub fn merge(&mut self, other: &Self) {
        for (k, v) in &other.p {
            let slot = self.p.entry(k.clone()).or_default();
            if *v > *slot {
                *slot = *v;
            }
        }
        for (k, v) in &other.n {
            let slot = self.n.entry(k.clone()).or_default();
            if *v > *slot {
                *slot = *v;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gset_merge_is_commutative_and_idempotent() {
        let mut a: GSet<i32> = GSet::new();
        let mut b: GSet<i32> = GSet::new();
        a.add(1);
        a.add(2);
        b.add(2);
        b.add(3);

        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab.items, ba.items);
        assert_eq!(ab.items, [1, 2, 3].into_iter().collect());

        // Idempotent.
        let before = ab.clone();
        ab.merge(&before);
        assert_eq!(ab.items, before.items);
    }

    #[test]
    fn lww_register_resolves_by_timestamp_then_author() {
        let mut reg = LwwRegister::new("v1".to_string(), "a", 10);
        reg.write("v2".to_string(), "b", 5);
        assert_eq!(reg.value, "v1", "older write must lose");
        reg.write("v3".to_string(), "c", 10);
        assert_eq!(reg.value, "v3", "tie broken by author > a");
        reg.write("v4".to_string(), "z", 20);
        assert_eq!(reg.value, "v4");

        // Merge symmetry.
        let mut r1: LwwRegister<String> = LwwRegister::new("x".to_string(), "a", 1u64);
        let mut r2: LwwRegister<String> = LwwRegister::new("y".to_string(), "b", 2u64);
        let r2_copy = r2.clone();
        r1.merge(&r2);
        r2.merge(&r1);
        assert_eq!(r1.value, r2_copy.value);
        assert_eq!(r1.value, "y");
    }

    #[test]
    fn pn_counter_commutes_and_merges_via_max() {
        let mut c = PnCounter::new();
        c.change("a", 5);
        c.change("b", 3);
        c.change("a", -2);
        assert_eq!(c.value(), 6);

        let mut other = PnCounter::new();
        other.change("a", 10);
        let merged_value = {
            let mut copy = c.clone();
            copy.merge(&other);
            copy.value()
        };
        // `a` positive slot was 5, now max(5, 10) = 10. b stays at 3. a
        // negative slot stays at 2. Final = 10 + 3 - 2 = 11.
        assert_eq!(merged_value, 11);
    }
}

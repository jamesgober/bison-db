//! Secondary indexes: an ordered map from a field's value to the document ids
//! that carry it.
//!
//! A [`SecondaryIndex`] lets the store answer equality and range queries over a
//! single field without scanning every document. It is a `BTreeMap` keyed by an
//! ordered view of the field's [`Value`], so equality is a point lookup and a
//! range query is a contiguous walk in sorted order.
//!
//! Indexes are an in-memory structure rebuilt from the log on demand (see
//! [`Db::create_index`](crate::Db::create_index)); they are not part of the
//! on-disk format, which keeps the file layout free to evolve until it is frozen.
//!
//! ## Ordering
//!
//! Index keys need a *total* order, but [`Value`] holds `f64`, which is only
//! partially ordered, and mixes types that have no natural cross-type ordering.
//! [`total_cmp_value`] imposes a total order: first by a fixed per-variant rank
//! (`null < bool < int < float < string < bytes < array < object`), then within
//! a variant by the natural order — using [`f64::total_cmp`] for floats so that
//! `NaN` and signed zeros are ordered deterministically. Because the same
//! function backs both equality and ordering of [`IndexKey`], the two are always
//! consistent. One consequence worth knowing: integers and floats sit in
//! separate rank bands, so index a field with a consistent numeric type.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::ops::Bound;

use crate::value::{Document, Value};

/// Imposes a total order over every [`Value`], including floats and mixed types.
///
/// See the [module docs](self) for the ordering rules. This is the single source
/// of truth for both index key ordering and field-equality comparisons, so the
/// indexed and scan query paths always agree.
pub(crate) fn total_cmp_value(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.total_cmp(y),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
        (Value::Array(x), Value::Array(y)) => cmp_slices(x, y),
        (Value::Object(x), Value::Object(y)) => cmp_documents(x, y),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Per-variant ordering rank, used to order values of differing types.
fn rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Int(_) => 2,
        Value::Float(_) => 3,
        Value::Str(_) => 4,
        Value::Bytes(_) => 5,
        Value::Array(_) => 6,
        Value::Object(_) => 7,
    }
}

/// Lexicographic comparison of two value slices, then by length.
fn cmp_slices(a: &[Value], b: &[Value]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = total_cmp_value(x, y);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// Lexicographic comparison of two documents by `(key, value)` pairs in order.
fn cmp_documents(a: &Document, b: &Document) -> Ordering {
    for ((ak, av), (bk, bv)) in a.iter().zip(b.iter()) {
        let key_ord = ak.cmp(bk);
        if key_ord != Ordering::Equal {
            return key_ord;
        }
        let val_ord = total_cmp_value(av, bv);
        if val_ord != Ordering::Equal {
            return val_ord;
        }
    }
    a.len().cmp(&b.len())
}

/// A [`Value`] wrapped to act as a totally ordered `BTreeMap` key.
///
/// Both equality and ordering route through [`total_cmp_value`], so `Eq` and
/// `Ord` are mutually consistent even where [`Value`]'s own `PartialEq` is not
/// (for example, `0.0` and `-0.0` are distinct keys here).
#[derive(Clone, Debug)]
pub(crate) struct IndexKey(Value);

impl PartialEq for IndexKey {
    fn eq(&self, other: &Self) -> bool {
        total_cmp_value(&self.0, &other.0) == Ordering::Equal
    }
}

impl Eq for IndexKey {}

impl PartialOrd for IndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexKey {
    fn cmp(&self, other: &Self) -> Ordering {
        total_cmp_value(&self.0, &other.0)
    }
}

/// An ordered secondary index over one document field.
///
/// Maps each distinct field value to the set of document ids carrying it.
/// Documents lacking the field are simply absent — the index is sparse.
pub(crate) struct SecondaryIndex {
    map: BTreeMap<IndexKey, BTreeSet<u64>>,
}

impl SecondaryIndex {
    /// Creates an empty index.
    pub(crate) fn new() -> Self {
        SecondaryIndex {
            map: BTreeMap::new(),
        }
    }

    /// Records that document `id` has field value `value`.
    pub(crate) fn add(&mut self, value: &Value, id: u64) {
        let _ = self
            .map
            .entry(IndexKey(value.clone()))
            .or_default()
            .insert(id);
    }

    /// Removes the association of `id` with `value`, dropping the value's entry
    /// entirely once no documents reference it.
    pub(crate) fn remove(&mut self, value: &Value, id: u64) {
        let key = IndexKey(value.clone());
        if let Some(set) = self.map.get_mut(&key) {
            let _ = set.remove(&id);
            if set.is_empty() {
                let _ = self.map.remove(&key);
            }
        }
    }

    /// Returns the ids whose field value equals `value`, in id order.
    pub(crate) fn equal(&self, value: &Value) -> Vec<u64> {
        self.map
            .get(&IndexKey(value.clone()))
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Returns the ids whose field value falls within `[lo, hi]`, ordered by
    /// field value and then by id. An empty (start-after-end) range yields no
    /// ids rather than panicking.
    pub(crate) fn range(&self, lo: Bound<&Value>, hi: Bound<&Value>) -> Vec<u64> {
        if is_empty_range(lo, hi) {
            return Vec::new();
        }
        let low = clone_bound(lo);
        let high = clone_bound(hi);
        let mut out = Vec::new();
        for set in self.map.range((low, high)).map(|(_, set)| set) {
            out.extend(set.iter().copied());
        }
        out
    }
}

/// Clones a borrowed value bound into an owned [`IndexKey`] bound.
fn clone_bound(bound: Bound<&Value>) -> Bound<IndexKey> {
    match bound {
        Bound::Included(v) => Bound::Included(IndexKey(v.clone())),
        Bound::Excluded(v) => Bound::Excluded(IndexKey(v.clone())),
        Bound::Unbounded => Bound::Unbounded,
    }
}

/// Reports whether the bounds describe an empty range, which `BTreeMap::range`
/// would otherwise panic on.
fn is_empty_range(lo: Bound<&Value>, hi: Bound<&Value>) -> bool {
    let (low, high) = match (lo, hi) {
        (Bound::Unbounded, _) | (_, Bound::Unbounded) => return false,
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) => {
            (a, b)
        }
    };
    match total_cmp_value(low, high) {
        Ordering::Greater => true,
        Ordering::Equal => {
            // Equal endpoints are empty unless both sides include them.
            !matches!((lo, hi), (Bound::Included(_), Bound::Included(_)))
        }
        Ordering::Less => false,
    }
}

/// Reports whether `value` falls within `[lo, hi]`, used by the scan query path.
pub(crate) fn in_bounds(value: &Value, lo: Bound<&Value>, hi: Bound<&Value>) -> bool {
    let above_low = match lo {
        Bound::Unbounded => true,
        Bound::Included(b) => total_cmp_value(value, b) != Ordering::Less,
        Bound::Excluded(b) => total_cmp_value(value, b) == Ordering::Greater,
    };
    let below_high = match hi {
        Bound::Unbounded => true,
        Bound::Included(b) => total_cmp_value(value, b) != Ordering::Greater,
        Bound::Excluded(b) => total_cmp_value(value, b) == Ordering::Less,
    };
    above_low && below_high
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_cross_type_order_follows_rank() {
        assert_eq!(
            total_cmp_value(&Value::Null, &Value::from(0_i64)),
            Ordering::Less
        );
        assert_eq!(
            total_cmp_value(&Value::from(1_i64), &Value::from("a")),
            Ordering::Less
        );
        assert_eq!(
            total_cmp_value(&Value::from(true), &Value::Null),
            Ordering::Greater
        );
    }

    #[test]
    fn test_int_and_float_are_separate_bands() {
        // 2 (int) sorts before 1.0 (float) purely by rank, not numeric value.
        assert_eq!(
            total_cmp_value(&Value::from(2_i64), &Value::from(1.0_f64)),
            Ordering::Less
        );
    }

    #[test]
    fn test_index_equal_and_remove() {
        let mut idx = SecondaryIndex::new();
        idx.add(&Value::from("x"), 1);
        idx.add(&Value::from("x"), 2);
        idx.add(&Value::from("y"), 3);
        assert_eq!(idx.equal(&Value::from("x")), vec![1, 2]);

        idx.remove(&Value::from("x"), 1);
        assert_eq!(idx.equal(&Value::from("x")), vec![2]);
        idx.remove(&Value::from("x"), 2);
        assert!(idx.equal(&Value::from("x")).is_empty());
    }

    #[test]
    fn test_index_range_is_sorted_and_inclusive() {
        let mut idx = SecondaryIndex::new();
        for n in [10_i64, 20, 30, 40] {
            idx.add(&Value::from(n), n as u64);
        }
        let lo = Value::from(20_i64);
        let hi = Value::from(30_i64);
        assert_eq!(
            idx.range(Bound::Included(&lo), Bound::Included(&hi)),
            vec![20, 30]
        );
        assert_eq!(
            idx.range(Bound::Excluded(&lo), Bound::Included(&hi)),
            vec![30]
        );
    }

    #[test]
    fn test_empty_range_yields_nothing() {
        let mut idx = SecondaryIndex::new();
        idx.add(&Value::from(5_i64), 5);
        let lo = Value::from(10_i64);
        let hi = Value::from(1_i64);
        assert!(
            idx.range(Bound::Included(&lo), Bound::Included(&hi))
                .is_empty()
        );
    }

    #[test]
    fn test_in_bounds_matches_range_semantics() {
        let lo = Value::from(0_i64);
        let hi = Value::from(10_i64);
        assert!(in_bounds(
            &Value::from(5_i64),
            Bound::Included(&lo),
            Bound::Excluded(&hi)
        ));
        assert!(!in_bounds(
            &Value::from(10_i64),
            Bound::Included(&lo),
            Bound::Excluded(&hi)
        ));
        assert!(in_bounds(
            &Value::from(5_i64),
            Bound::Unbounded,
            Bound::Unbounded
        ));
    }
}

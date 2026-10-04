//! Picking and ordering without hidden tie-breaks (DESIGN.md 7.4).
//!
//! Python's `max` and `min` return the first of equal elements; Rust's `Iterator::max_by_key`
//! returns the last. Porting one to the other silently changes which unit attacks, which tile a
//! city works, which city a settler picks. [`argmax_first`] and [`argmin_first`] keep Python's
//! rule. Every decision should still end its key in an id or a tile index, so that ties do not
//! depend on iteration order at all (README.md, rule 4).
//!
//! Floats in sort keys and heaps go through [`TotalF64`], an `Ord` by `f64::total_cmp`.

use core::cmp::Ordering;

/// The first item with the largest key, as Python's `max(items, key=...)` picks it.
///
/// An item replaces the current best only when its key is strictly greater, so among equal keys
/// the first wins; this is also exactly how Python treats `-0.0 == 0.0` and NaN keys.
pub fn argmax_first<T, K: PartialOrd>(
    items: impl IntoIterator<Item = T>,
    mut key: impl FnMut(&T) -> K,
) -> Option<T> {
    let mut iter = items.into_iter();
    let first = iter.next()?;
    let mut best_key = key(&first);
    let mut best = first;
    for item in iter {
        let k = key(&item);
        if k > best_key {
            best_key = k;
            best = item;
        }
    }
    Some(best)
}

/// The first item with the smallest key, as Python's `min(items, key=...)` picks it.
pub fn argmin_first<T, K: PartialOrd>(
    items: impl IntoIterator<Item = T>,
    mut key: impl FnMut(&T) -> K,
) -> Option<T> {
    let mut iter = items.into_iter();
    let first = iter.next()?;
    let mut best_key = key(&first);
    let mut best = first;
    for item in iter {
        let k = key(&item);
        if k < best_key {
            best_key = k;
            best = item;
        }
    }
    Some(best)
}

/// An `f64` ordered by `total_cmp`: usable as a sort key, a `BTreeMap` key or a
/// [`MinHeap`](super::collections::MinHeap) priority.
///
/// The order is total: `-0.0 < 0.0`, and NaNs sort to the ends by sign. Equality follows the
/// same order, so `TotalF64(-0.0) != TotalF64(0.0)`. State floats are finite (README.md, rule 2),
/// so NaN only matters in that it cannot break a sort.
#[derive(Clone, Copy, Debug, Default)]
pub struct TotalF64(pub f64);

impl PartialEq for TotalF64 {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for TotalF64 {}

impl PartialOrd for TotalF64 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TotalF64 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_of_equals_wins() {
        let items = [(1, 'a'), (3, 'b'), (3, 'c'), (0, 'd'), (0, 'e')];
        assert_eq!(argmax_first(items, |x| x.0), Some((3, 'b')));
        assert_eq!(argmin_first(items, |x| x.0), Some((0, 'd')));
        // Rust's own max_by_key takes the last of equals.
        assert_eq!(items.iter().max_by_key(|x| x.0), Some(&(3, 'c')));
        assert_eq!(argmax_first(Vec::<u8>::new(), |x| *x), None);
    }

    #[test]
    fn float_keys_behave_like_python() {
        let items = [-0.0, 0.0];
        assert_eq!(argmax_first(items, |x| *x).map(f64::to_bits), Some((-0.0f64).to_bits()));
        let mut keys = [TotalF64(1.5), TotalF64(-0.0), TotalF64(0.0), TotalF64(-2.0)];
        keys.sort();
        let sorted: Vec<u64> = keys.iter().map(|k| k.0.to_bits()).collect();
        let want: Vec<u64> = [-2.0f64, -0.0, 0.0, 1.5].iter().map(|x| x.to_bits()).collect();
        assert_eq!(sorted, want);
    }
}

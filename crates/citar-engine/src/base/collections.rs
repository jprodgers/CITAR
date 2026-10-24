//! Collections whose order never depends on a hash (DESIGN.md 7.4 and 7.5).
//!
//! Python dicts iterate in insertion order, and much of the engine leaned on that. Rust's
//! `HashMap` iterates in an order that differs between two maps in one process, so iterating one
//! can fork a game. Clippy bans the std hash types outright (the root `clippy.toml`); these are
//! what to use instead:
//! - [`DetMap`] and [`DetSet`]: insertion order, like a Python dict. Remove with `shift_remove`;
//!   the order-breaking removals are banned too.
//! - [`LookupMap`]: a hash map for lookups only. It has no way to iterate at all.
//! - [`MinHeap`]: a priority queue that pops equal keys in push order, where `BinaryHeap` pops
//!   them in an order its algorithm leaves unspecified.

use core::borrow::Borrow;
use core::fmt;
use core::hash::Hash;

use rustc_hash::FxBuildHasher;

/// A map that iterates in insertion order, like a Python dict. Remove with `shift_remove`.
pub type DetMap<K, V> = indexmap::IndexMap<K, V, FxBuildHasher>;

/// A set that iterates in insertion order. Remove with `shift_remove`.
pub type DetSet<K> = indexmap::IndexSet<K, FxBuildHasher>;

/// A hash map for lookups only: no method iterates it, so its order can never leak into a game.
///
/// The one sanctioned use of `std::collections::HashMap` in the engine (DESIGN.md 2.5).
#[derive(Clone)]
pub struct LookupMap<K, V> {
    #[allow(
        clippy::disallowed_types,
        reason = "LookupMap exposes no iteration, so order cannot leak"
    )]
    map: std::collections::HashMap<K, V, FxBuildHasher>,
}

impl<K: Eq + Hash, V> LookupMap<K, V> {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self { map: Default::default() }
    }

    /// An empty map with room for `n` entries.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        #[allow(clippy::disallowed_types, reason = "LookupMap exposes no iteration")]
        let map = std::collections::HashMap::with_capacity_and_hasher(n, FxBuildHasher);
        Self { map }
    }

    /// The value for `key`.
    #[must_use]
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.get(key)
    }

    /// The value for `key`.
    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.get_mut(key)
    }

    /// Whether `key` has a value.
    #[must_use]
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.contains_key(key)
    }

    /// Sets the value for `key`, returning the old one.
    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.map.insert(key, value)
    }

    /// Removes the value for `key`, returning it.
    #[inline]
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.remove(key)
    }

    /// The value for `key`, inserting `make()` first if there is none.
    pub fn get_or_insert_with(&mut self, key: K, make: impl FnOnce() -> V) -> &mut V {
        self.map.entry(key).or_insert_with(make)
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Removes every entry, keeping the allocation.
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

impl<K: Eq + Hash, V> Default for LookupMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Shows only the size: printing the entries would print them in hash order.
impl<K, V> fmt::Debug for LookupMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LookupMap").field("len", &self.map.len()).finish_non_exhaustive()
    }
}

// ---- MinHeap ----------------------------------------------------------------------------------

struct HeapEntry<K, V> {
    key: K,
    seq: u64,
    value: V,
}

impl<K: Ord, V> HeapEntry<K, V> {
    /// Whether `self` pops before `other`: the smaller key, and on equal keys the earlier push.
    #[inline]
    fn before(&self, other: &Self) -> bool {
        (&self.key, self.seq) < (&other.key, other.seq)
    }
}

/// A binary min-heap that pops the smallest key first, and equal keys in the order they were
/// pushed.
///
/// Keys and push sequence together are a total order, so the pop order is fully determined by the
/// pushes, whatever the heap's internal layout. For float priorities use
/// [`TotalF64`](super::order::TotalF64) as the key.
pub struct MinHeap<K, V> {
    items: Vec<HeapEntry<K, V>>,
    seq: u64,
}

impl<K: Ord, V> MinHeap<K, V> {
    /// An empty heap.
    #[must_use]
    pub const fn new() -> Self {
        Self { items: Vec::new(), seq: 0 }
    }

    /// An empty heap with room for `n` entries.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self { items: Vec::with_capacity(n), seq: 0 }
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the heap is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Removes every entry. The push sequence restarts, since nothing is left to tie with.
    pub fn clear(&mut self) {
        self.items.clear();
        self.seq = 0;
    }

    /// Adds `value` with priority `key`.
    pub fn push(&mut self, key: K, value: V) {
        let seq = self.seq;
        self.seq += 1;
        self.items.push(HeapEntry { key, seq, value });
        self.sift_up(self.items.len() - 1);
    }

    /// The entry that [`pop`](Self::pop) would return next.
    #[must_use]
    pub fn peek(&self) -> Option<(&K, &V)> {
        self.items.first().map(|e| (&e.key, &e.value))
    }

    /// Removes and returns the entry with the smallest key; the earliest pushed among equals.
    pub fn pop(&mut self) -> Option<(K, V)> {
        if self.items.is_empty() {
            return None;
        }
        let last = self.items.len() - 1;
        self.items.swap(0, last);
        let top = self.items.pop()?;
        if !self.items.is_empty() {
            self.sift_down(0);
        }
        Some((top.key, top.value))
    }

    fn sift_up(&mut self, mut i: usize) {
        while i > 0 {
            let parent = (i - 1) / 2;
            if self.items[i].before(&self.items[parent]) {
                self.items.swap(i, parent);
                i = parent;
            } else {
                break;
            }
        }
    }

    fn sift_down(&mut self, mut i: usize) {
        let n = self.items.len();
        loop {
            let (left, right) = (2 * i + 1, 2 * i + 2);
            let mut smallest = i;
            if left < n && self.items[left].before(&self.items[smallest]) {
                smallest = left;
            }
            if right < n && self.items[right].before(&self.items[smallest]) {
                smallest = right;
            }
            if smallest == i {
                break;
            }
            self.items.swap(i, smallest);
            i = smallest;
        }
    }
}

impl<K: Ord, V> Default for MinHeap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> fmt::Debug for MinHeap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MinHeap").field("len", &self.items.len()).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_heap_pops_equal_keys_in_push_order() {
        let mut h = MinHeap::new();
        for (k, v) in [(3, 'a'), (1, 'b'), (3, 'c'), (1, 'd'), (2, 'e'), (1, 'f')] {
            h.push(k, v);
        }
        let order: Vec<char> = core::iter::from_fn(|| h.pop().map(|(_, v)| v)).collect();
        assert_eq!(order, ['b', 'd', 'f', 'e', 'a', 'c']);
    }

    #[test]
    fn det_map_keeps_insertion_order_through_shift_remove() {
        let mut m: DetMap<&str, u8> = DetMap::default();
        for (i, k) in ["c", "a", "d", "b"].into_iter().enumerate() {
            m.insert(k, i as u8);
        }
        m.shift_remove("a");
        assert_eq!(m.keys().copied().collect::<Vec<_>>(), ["c", "d", "b"]);
    }

    #[test]
    fn lookup_map_looks_up() {
        let mut m: LookupMap<String, u32> = LookupMap::new();
        m.insert("Rome".into(), 1);
        assert_eq!(m.get("Rome"), Some(&1));
        *m.get_or_insert_with("Athens".into(), || 0) += 2;
        assert_eq!(m.get("Athens"), Some(&2));
        assert_eq!(m.remove("Rome"), Some(1));
        assert_eq!(m.len(), 1);
    }
}

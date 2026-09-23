//! [`EntityStore`]: units and cities by id, in id order (DESIGN.md 4.2).
//!
//! Python kept `GameState.units` and `GameState.cities` as dicts keyed by id (`state.py:339-340`),
//! which iterate in insertion order: the same as id order only because ids were handed out in
//! order and nothing was ever re-inserted. Here that order is the store's contract. Ids are
//! monotonic and never reused, so an insert is always a push; a removal leaves a tombstone that a
//! later compaction drops.

use core::fmt;

use crate::base::ids::Id;

/// A value that knows its own id, so a store can never file it under another.
pub trait Entity {
    /// The id type.
    type Id: Id;

    /// The value's id.
    fn id(&self) -> Self::Id;
}

/// Why a store refused an insert.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The id is not above every id the store has held: ids are never reused.
    #[error("{kind} {id} is not new: this store has held ids up to {high}")]
    NotNew {
        /// The id type.
        kind: &'static str,
        /// The id offered.
        id: u64,
        /// The highest id the store has held.
        high: u64,
    },
}

/// Slot number meaning "not in the store".
const ABSENT: u32 = u32::MAX;

/// Tombstones tolerated before a compaction: more than this many, and more than a quarter of the
/// live count.
const COMPACT_MIN_DEAD: u32 = 64;

/// Units or cities by id.
///
/// - `slots` holds the values in ascending id order, `None` for a removed one;
/// - `ids` holds each slot's id;
/// - `pos` maps an id's index (less `I::FIRST_INDEX`) to its slot, `u32::MAX` if it is not in the
///   store. Its length is the high-water mark of ids ever inserted, which is what makes "a new id
///   is above every id the store has held" one comparison;
/// - `live` counts the values present.
///
/// `get` is O(1), `insert` a push, and iteration ascends by id. Equality is by the live values in
/// id order, so two stores holding the same values are equal however many tombstones either has.
#[derive(Clone)]
pub struct EntityStore<I, T> {
    slots: Vec<Option<T>>,
    ids: Vec<I>,
    pos: Vec<u32>,
    live: u32,
}

impl<I: Id, T: Entity<Id = I>> EntityStore<I, T> {
    /// An empty store.
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: Vec::new(), ids: Vec::new(), pos: Vec::new(), live: 0 }
    }

    /// The position of `id` in `pos`.
    fn key(id: I) -> usize {
        id.index() - I::FIRST_INDEX
    }

    /// The slot holding `id`, if the store holds it.
    fn slot(&self, id: I) -> Option<usize> {
        let s = *self.pos.get(Self::key(id))?;
        (s != ABSENT).then_some(s as usize)
    }

    /// The number of values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live as usize
    }

    /// Whether the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Whether the store holds `id`.
    #[must_use]
    pub fn contains(&self, id: I) -> bool {
        self.slot(id).is_some()
    }

    /// The value with this id.
    #[must_use]
    pub fn get(&self, id: I) -> Option<&T> {
        self.slots.get(self.slot(id)?)?.as_ref()
    }

    /// The value with this id.
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        let s = self.slot(id)?;
        self.slots.get_mut(s)?.as_mut()
    }

    /// Two different values at once, such as an attacker and a defender; `None` if either is
    /// missing or they are the same.
    pub fn get2_mut(&mut self, a: I, b: I) -> Option<(&mut T, &mut T)> {
        let (sa, sb) = (self.slot(a)?, self.slot(b)?);
        if sa == sb {
            return None;
        }
        let (lo, hi, swapped) = if sa < sb { (sa, sb, false) } else { (sb, sa, true) };
        let (left, right) = self.slots.split_at_mut(hi);
        let x = left.get_mut(lo)?.as_mut()?;
        let y = right.first_mut()?.as_mut()?;
        Some(if swapped { (y, x) } else { (x, y) })
    }

    /// The highest id the store has ever held, as its index.
    fn high(&self) -> usize {
        self.pos.len() + I::FIRST_INDEX
    }

    /// Adds a value. Its id must be above every id the store has held.
    pub fn insert(&mut self, value: T) -> Result<(), StoreError> {
        let id = value.id();
        let key = Self::key(id);
        if key < self.pos.len() {
            return Err(StoreError::NotNew {
                kind: I::NAME,
                id: id.index() as u64,
                high: (self.high() - 1) as u64,
            });
        }
        // A store past u32::MAX slots would need more memory than any game has.
        let slot = u32::try_from(self.slots.len()).unwrap_or(ABSENT);
        self.pos.resize(key, ABSENT);
        self.pos.push(slot);
        self.slots.push(Some(value));
        self.ids.push(id);
        self.live += 1;
        Ok(())
    }

    /// Removes the value with this id and returns it.
    pub fn remove(&mut self, id: I) -> Option<T> {
        let s = self.slot(id)?;
        let value = self.slots.get_mut(s)?.take()?;
        if let Some(p) = self.pos.get_mut(Self::key(id)) {
            *p = ABSENT;
        }
        self.live -= 1;
        self.maybe_compact();
        Some(value)
    }

    /// Drops the tombstones once there are more than `COMPACT_MIN_DEAD` of them and they
    /// outnumber a quarter of the live values.
    fn maybe_compact(&mut self) {
        let dead = self.slots.len() as u32 - self.live;
        if dead > COMPACT_MIN_DEAD && dead > self.live / 4 {
            self.compact();
        }
    }

    /// Drops every tombstone now.
    pub fn compact(&mut self) {
        let mut slots = Vec::with_capacity(self.live as usize);
        let mut ids = Vec::with_capacity(self.live as usize);
        for (value, &id) in self.slots.drain(..).zip(&self.ids) {
            if let Some(value) = value {
                let slot = u32::try_from(slots.len()).unwrap_or(ABSENT);
                if let Some(p) = self.pos.get_mut(Self::key(id)) {
                    *p = slot;
                }
                slots.push(Some(value));
                ids.push(id);
            }
        }
        self.slots = slots;
        self.ids = ids;
    }

    /// The number of tombstones waiting for a compaction.
    #[must_use]
    pub fn tombstones(&self) -> usize {
        self.slots.len() - self.live as usize
    }

    /// Every value with its id, ascending by id.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (I, &T)> {
        self.ids.iter().zip(&self.slots).filter_map(|(&id, v)| v.as_ref().map(|v| (id, v)))
    }

    /// Every value, ascending by id.
    pub fn values(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.slots.iter().filter_map(Option::as_ref)
    }

    /// Every value, ascending by id.
    pub fn values_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut T> {
        self.slots.iter_mut().filter_map(Option::as_mut)
    }

    /// The ids, ascending: a snapshot, for loops that add or remove values as they go.
    #[must_use]
    pub fn ids(&self) -> Vec<I> {
        self.iter().map(|(id, _)| id).collect()
    }

    /// The highest id held now.
    #[must_use]
    pub fn last_id(&self) -> Option<I> {
        self.iter().next_back().map(|(id, _)| id)
    }
}

impl<I: Id, T: Entity<Id = I>> Default for EntityStore<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Id, T: Entity<Id = I> + PartialEq> PartialEq for EntityStore<I, T> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<I: Id, T: Entity<Id = I> + fmt::Debug> fmt::Debug for EntityStore<I, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<I: Id, T: Entity<Id = I>> EntityStore<I, T> {
    /// A store of these values, which must come in ascending id order with no id repeated, as a
    /// save lists them.
    pub fn try_from_values(values: impl IntoIterator<Item = T>) -> Result<Self, StoreError> {
        let mut store = Self::new();
        for v in values {
            store.insert(v)?;
        }
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::ids::UnitId;

    #[derive(Clone, Debug, PartialEq)]
    struct Thing(UnitId, u32);

    impl Entity for Thing {
        type Id = UnitId;
        fn id(&self) -> UnitId {
            self.0
        }
    }

    fn uid(n: u32) -> UnitId {
        UnitId::new(n).unwrap_or(UnitId::FIRST)
    }

    #[test]
    fn inserts_must_be_new_ids() {
        let mut s = EntityStore::new();
        assert_eq!(s.insert(Thing(uid(3), 30)), Ok(()));
        assert!(s.insert(Thing(uid(3), 31)).is_err());
        assert!(s.insert(Thing(uid(2), 20)).is_err());
        assert_eq!(s.insert(Thing(uid(9), 90)), Ok(()));
        assert_eq!(s.remove(uid(9)), Some(Thing(uid(9), 90)));
        // A removed id is never taken back.
        assert!(s.insert(Thing(uid(9), 91)).is_err());
        assert_eq!(s.insert(Thing(uid(10), 100)), Ok(()));
        assert_eq!(s.ids(), [uid(3), uid(10)]);
        assert_eq!(s.last_id(), Some(uid(10)));
    }

    #[test]
    fn get2_mut_hands_out_both_in_the_order_asked() {
        let mut s =
            EntityStore::try_from_values((1..=4).map(|n| Thing(uid(n), n))).expect("ascending");
        let (a, b) = s.get2_mut(uid(4), uid(2)).expect("both are there");
        assert_eq!((a.1, b.1), (4, 2));
        a.1 = 40;
        assert_eq!(s.get(uid(4)).map(|t| t.1), Some(40));
        assert!(s.get2_mut(uid(3), uid(3)).is_none());
        assert!(s.get2_mut(uid(3), uid(7)).is_none());
    }

    #[test]
    fn compaction_keeps_order_and_lookups() {
        let mut s =
            EntityStore::try_from_values((1..=300).map(|n| Thing(uid(n), n))).expect("ascending");
        for n in (1..=300).filter(|n| n % 3 != 0) {
            assert!(s.remove(uid(n)).is_some());
        }
        assert_eq!(s.len(), 100);
        assert!(s.tombstones() <= COMPACT_MIN_DEAD as usize + 1, "{}", s.tombstones());
        let left: Vec<u32> = s.values().map(|t| t.1).collect();
        assert_eq!(left, (1..=100).map(|n| n * 3).collect::<Vec<_>>());
        assert_eq!(s.get(uid(150)).map(|t| t.1), Some(150));
        assert!(s.get(uid(151)).is_none());
        let fresh = EntityStore::try_from_values(s.values().cloned()).expect("ascending");
        assert_eq!(fresh, s);
    }
}

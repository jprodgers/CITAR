//! Typed ids for every entity and every rule, and [`IdVec`], a vector indexed by one
//! (DESIGN.md 3.1 and 4.2).
//!
//! Python used bare ints for entities (units, cities and camps shared one `next_id`,
//! `state.py:343`, `game.py:551-555`) and display names for rules. Here each is its own type, so
//! a `CityId` can never index units and a `TechId` can never index buildings. The unique
//! evaluator (layer 1) needs the entity ids too, which is why they live in `base` and not in
//! `state`.
//!
//! Entity ids are drawn from persisted counters that start at 1 and are never reused, so they sit
//! on `NonZeroU32` and `Option<UnitId>` costs nothing. Rule ids index their table in the ruleset,
//! in file order: `u16` in general, `u8` where tiles or other hot arrays store them.
//!
//! Serialisation: the entity ids serialise as their integer in both encodings. Rule ids have no
//! serde impls here, because the save writes them as names (DESIGN.md 4.9); `save` adds those.

use core::fmt;
use core::hash::Hash;
use core::marker::PhantomData;
use core::num::NonZeroU32;
use core::ops::{Index, IndexMut};

use super::rng::KeyPart;

/// A turn number. Negative values are sentinels (`sacked_turn = -1000` in Python).
pub type Turn = i32;

/// What every id type offers: a dense index for vectors and bitsets, and the way back.
pub trait Id: Copy + Eq + Ord + Hash + fmt::Debug + 'static {
    /// The type's name, for messages.
    const NAME: &'static str;

    /// The smallest index an id of this type has: 0, or 1 for the entity ids, whose counters
    /// start at 1. [`IdVec`] stores the id with this index in its first slot.
    const FIRST_INDEX: usize;

    /// The id as a bitset index: its raw value, so bit 0 of a set of entity ids is never used.
    fn index(self) -> usize;

    /// The id at this index, or `None` if the type cannot hold it.
    fn from_index(index: usize) -> Option<Self>;
}

/// Defines one id type. Two shapes:
/// - `Name(pub u8 | u16 | u32)`: every value is an id, 0 included (players, tiles, rule ids);
/// - `Name(NonZeroU32)`: an entity id from a counter that starts at 1.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident(pub $raw:ty);) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub $raw);

        impl $name {
            /// The raw value.
            #[must_use]
            #[inline]
            pub const fn get(self) -> $raw {
                self.0
            }
        }

        impl Id for $name {
            const NAME: &'static str = stringify!($name);
            const FIRST_INDEX: usize = 0;

            #[inline]
            fn index(self) -> usize {
                self.0 as usize
            }

            #[inline]
            fn from_index(index: usize) -> Option<Self> {
                <$raw>::try_from(index).ok().map(Self)
            }
        }

        impl KeyPart for $name {
            #[inline]
            fn key(self) -> u64 {
                u64::from(self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
    ($(#[$meta:meta])* $name:ident(NonZeroU32);) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(NonZeroU32);

        impl $name {
            /// The first id a counter hands out.
            pub const FIRST: Self = Self(NonZeroU32::MIN);

            /// The id with this raw value; `None` for 0, which is never an id.
            #[must_use]
            #[inline]
            pub const fn new(raw: u32) -> Option<Self> {
                match NonZeroU32::new(raw) {
                    Some(n) => Some(Self(n)),
                    None => None,
                }
            }

            /// The id with this raw value.
            #[must_use]
            #[inline]
            pub const fn from_nonzero(raw: NonZeroU32) -> Self {
                Self(raw)
            }

            /// The raw value, at least 1.
            #[must_use]
            #[inline]
            pub const fn get(self) -> u32 {
                self.0.get()
            }

            /// The id a counter hands out after this one; `None` once `u32` is exhausted.
            #[must_use]
            #[inline]
            pub const fn next(self) -> Option<Self> {
                match self.0.checked_add(1) {
                    Some(n) => Some(Self(n)),
                    None => None,
                }
            }
        }

        impl Id for $name {
            const NAME: &'static str = stringify!($name);
            const FIRST_INDEX: usize = 1;

            #[inline]
            fn index(self) -> usize {
                self.0.get() as usize
            }

            #[inline]
            fn from_index(index: usize) -> Option<Self> {
                u32::try_from(index).ok().and_then(Self::new)
            }
        }

        impl KeyPart for $name {
            #[inline]
            fn key(self) -> u64 {
                u64::from(self.0.get())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

// ---- Entities ---------------------------------------------------------------------------------

define_id! {
    /// A seat in the game, in `GameState.players` order. At most 64, because a [`PlayerSet`] is
    /// one `u64`; today's largest game has 24 majors, 32 city-states and the barbarians, 57.
    ///
    /// [`PlayerSet`]: super::sets::PlayerSet
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    PlayerId(pub u8);
}

define_id! {
    /// A tile: `y * width + x` in odd-r offset coordinates (`hexmap.py:1-11`).
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    TileIdx(pub u32);
}

define_id! {
    /// A unit.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    UnitId(NonZeroU32);
}

define_id! {
    /// A city.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    CityId(NonZeroU32);
}

define_id! {
    /// A barbarian encampment.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    CampId(NonZeroU32);
}

define_id! {
    /// A standing diplomatic deal.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    DealId(NonZeroU32);
}

define_id! {
    /// A negotiation (a diplomacy chat) in progress or closed.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    NegotiationId(NonZeroU32);
}

define_id! {
    /// An event in the feed that engine and host events share (DESIGN.md 4.7).
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    EventId(NonZeroU32);
}

define_id! {
    /// A diplomatic message.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    MessageId(NonZeroU32);
}

define_id! {
    /// A founded religion: its index in `World::religions`, which is founding order.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    ReligionId(pub u8);
}

// ---- Rule ids, u16 ----------------------------------------------------------------------------

define_id! {
    /// A row of `techs.json`.
    TechId(pub u16);
}

define_id! {
    /// A row of `units.json` (UnCiv's "base unit"; 127 of them).
    BaseUnitId(pub u16);
}

define_id! {
    /// A row of `unit_types.json` (Melee, Mounted, ...; 28 of them).
    UnitTypeId(pub u16);
}

define_id! {
    /// A row of `buildings.json`, wonders included.
    BuildingId(pub u16);
}

define_id! {
    /// A row of `promotions.json`.
    PromotionId(pub u16);
}

define_id! {
    /// A policy branch or a policy of `policies.json`: the branches first, then the policies.
    PolicyId(pub u16);
}

define_id! {
    /// A row of `beliefs.json`.
    BeliefId(pub u16);
}

define_id! {
    /// A nation: `nations.json` merged with `custom/nations.json`.
    NationId(pub u16);
}

define_id! {
    /// A row of `personalities.json`.
    PersonalityId(pub u16);
}

define_id! {
    /// A kind of city-state quest, a row of `quests.json`.
    QuestKindId(pub u16);
}

define_id! {
    /// A reward of `ruins.json`.
    RuinId(pub u16);
}

define_id! {
    /// A compiled unique in the ruleset's unique table.
    UniqueId(pub u16);
}

define_id! {
    /// A compiled conditional in the ruleset's conditional table.
    CondId(pub u16);
}

define_id! {
    /// An interned `Stats` value in the ruleset.
    StatsId(pub u16);
}

define_id! {
    /// An interned fraction in the ruleset's fracs table.
    FracId(pub u16);
}

define_id! {
    /// An interned text: a name or a unique's text. `u32`, since texts outnumber everything.
    TextId(pub u32);
}

define_id! {
    /// A unit ability key.
    AbilityKey(pub u16);
}

define_id! {
    /// A compiled unit filter.
    UnitFilterId(pub u16);
}

define_id! {
    /// A compiled tile filter.
    TileFilterId(pub u16);
}

define_id! {
    /// A compiled city filter.
    CityFilterId(pub u16);
}

define_id! {
    /// A compiled civilization filter.
    CivFilterId(pub u16);
}

define_id! {
    /// A compiled combatant filter.
    CombatantFilterId(pub u16);
}

// ---- Rule ids, u8: stored in tiles and other hot arrays ---------------------------------------

define_id! {
    /// A row of `terrains.json`: base terrains, features and natural wonders.
    TerrainId(pub u8);
}

define_id! {
    /// A terrain feature: an index into the rules' feature list (Hill included), which a
    /// `FeatureSet` holds as bits.
    FeatureId(pub u8);
}

define_id! {
    /// A row of `resources.json`.
    ResourceId(pub u8);
}

define_id! {
    /// A row of `improvements.json`.
    ImprovementId(pub u8);
}

define_id! {
    /// A row of `eras.json`.
    EraId(pub u8);
}

define_id! {
    /// A row of `speeds.json`.
    SpeedId(pub u8);
}

define_id! {
    /// A row of `difficulties.json`.
    DifficultyId(pub u8);
}

define_id! {
    /// A row of `victories.json`.
    VictoryId(pub u8);
}

define_id! {
    /// A row of `specialists.json`.
    SpecialistId(pub u8);
}

define_id! {
    /// A row of `city_state_types.json`.
    CityStateTypeId(pub u8);
}

define_id! {
    /// A row of `religions.json`: a religion a player can found, not a founded one
    /// ([`ReligionId`]).
    RulesReligionId(pub u8);
}

define_id! {
    /// An interned unique tag.
    TagId(pub u8);
}

// ---- IdVec ------------------------------------------------------------------------------------

/// A vector indexed by an id type rather than by `usize`.
///
/// Slot `i` holds the id with index `i + I::FIRST_INDEX`: the first slot is `TechId(0)` for a
/// rule table and `CityId(1)` for an entity table, so neither wastes a slot and [`push`]
/// hands out the first id of either kind.
///
/// Rule tables hold one entry per id, pushed in file order. Derived per-entity data is indexed
/// by raw entity id and grown on demand with [`ensure`]: ids are never reused, so the vector
/// stays within a few tens of thousands of entries even for a converted Python game.
///
/// [`push`]: IdVec::push
/// [`ensure`]: IdVec::ensure
pub struct IdVec<I, T> {
    items: Vec<T>,
    // fn(I) keeps IdVec Send and Sync whatever I is, and says nothing about owning an I.
    _id: PhantomData<fn(I)>,
}

impl<I: Id, T> IdVec<I, T> {
    /// An empty vector.
    #[must_use]
    pub const fn new() -> Self {
        Self { items: Vec::new(), _id: PhantomData }
    }

    /// An empty vector with room for `n` entries.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self { items: Vec::with_capacity(n), _id: PhantomData }
    }

    /// Takes a vector whose position `i` belongs to the id with index `i + I::FIRST_INDEX`.
    #[must_use]
    pub fn from_vec(items: Vec<T>) -> Self {
        Self { items, _id: PhantomData }
    }

    /// `n` copies of `value`.
    #[must_use]
    pub fn from_elem(value: T, n: usize) -> Self
    where
        T: Clone,
    {
        Self::from_vec(vec![value; n])
    }

    /// The number of slots, used or not: for entity ids, the largest id the vector reaches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there are no slots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The slot of `id`. Every id's index is at least `FIRST_INDEX`, so this never underflows.
    #[inline]
    fn slot(id: I) -> usize {
        id.index() - I::FIRST_INDEX
    }

    /// The id of slot `slot`, if the id type reaches it.
    #[inline]
    fn id_at(slot: usize) -> Option<I> {
        slot.checked_add(I::FIRST_INDEX).and_then(I::from_index)
    }

    /// The entry for `id`, if the vector reaches that far.
    #[must_use]
    #[inline]
    pub fn get(&self, id: I) -> Option<&T> {
        self.items.get(Self::slot(id))
    }

    /// The entry for `id`, if the vector reaches that far.
    #[inline]
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        self.items.get_mut(Self::slot(id))
    }

    /// Appends an entry and returns its id (`TechId(0)`, `CityId(1)`, then onwards), or gives
    /// the value back if the id type is full.
    pub fn push(&mut self, value: T) -> Result<I, T> {
        match Self::id_at(self.items.len()) {
            Some(id) => {
                self.items.push(value);
                Ok(id)
            }
            None => Err(value),
        }
    }

    /// The entry for `id`, growing the vector with defaults to reach it.
    ///
    /// It allocates up to the id, so an id from outside (a save, a request) must be checked
    /// against its counter first: a corrupt `CityId(4_000_000_000)` would otherwise ask for
    /// billions of slots.
    pub fn ensure(&mut self, id: I) -> &mut T
    where
        T: Default,
    {
        let i = Self::slot(id);
        if i >= self.items.len() {
            self.items.resize_with(i + 1, T::default);
        }
        &mut self.items[i]
    }

    /// Every slot that has an id, with its id, in ascending order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (I, &T)> {
        self.items.iter().enumerate().filter_map(|(i, v)| Self::id_at(i).map(|id| (id, v)))
    }

    /// Every slot that has an id, with its id, in ascending order.
    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (I, &mut T)> {
        self.items.iter_mut().enumerate().filter_map(|(i, v)| Self::id_at(i).map(|id| (id, v)))
    }

    /// The ids of every slot, in ascending order.
    pub fn ids(&self) -> impl DoubleEndedIterator<Item = I> + use<I, T> {
        (0..self.items.len()).filter_map(Self::id_at)
    }

    /// The entries in id order, without their ids: position `i` is the id with index
    /// `i + I::FIRST_INDEX`.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.items
    }

    /// The entries in id order, without their ids, as [`as_slice`](Self::as_slice).
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.items
    }

    /// Removes every entry.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The underlying vector.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.items
    }
}

impl<I: Id, T> Default for IdVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I, T: Clone> Clone for IdVec<I, T> {
    fn clone(&self) -> Self {
        Self { items: self.items.clone(), _id: PhantomData }
    }
}

impl<I, T: PartialEq> PartialEq for IdVec<I, T> {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

impl<I, T: Eq> Eq for IdVec<I, T> {}

impl<I: Id, T: fmt::Debug> fmt::Debug for IdVec<I, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// Panics if the vector does not reach `id`. Use it where the id was validated against the
/// same table; use [`IdVec::get`] for anything that came from outside.
impl<I: Id, T> Index<I> for IdVec<I, T> {
    type Output = T;

    #[inline]
    fn index(&self, id: I) -> &T {
        &self.items[Self::slot(id)]
    }
}

/// Panics if the vector does not reach `id`, like [`Index`].
impl<I: Id, T> IndexMut<I> for IdVec<I, T> {
    #[inline]
    fn index_mut(&mut self, id: I) -> &mut T {
        &mut self.items[Self::slot(id)]
    }
}

impl<I: Id, T> FromIterator<T> for IdVec<I, T> {
    fn from_iter<It: IntoIterator<Item = T>>(iter: It) -> Self {
        Self::from_vec(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ids_round_trip_through_their_index() {
        assert_eq!(TechId::from_index(7), Some(TechId(7)));
        assert_eq!(TechId(7).index(), 7);
        assert_eq!(TerrainId::from_index(255), Some(TerrainId(255)));
        assert_eq!(TerrainId::from_index(256), None);
        assert_eq!(format!("{:?}", PlayerId(3)), "PlayerId(3)");
        assert_eq!(PlayerId(3).to_string(), "3");
    }

    #[test]
    fn entity_ids_start_at_one() {
        assert_eq!(UnitId::new(0), None);
        assert_eq!(UnitId::FIRST.get(), 1);
        assert_eq!(UnitId::from_index(0), None);
        assert_eq!(CityId::from_index(5).map(CityId::get), Some(5));
        assert_eq!(CityId::new(u32::MAX).and_then(CityId::next), None);
        assert_eq!(size_of::<Option<UnitId>>(), 4);
    }

    #[test]
    fn entity_ids_serialise_as_their_integer() -> Result<(), serde_json::Error> {
        let id = CityId::new(42).expect("42 is an id");
        assert_eq!(serde_json::to_string(&id)?, "42");
        assert_eq!(serde_json::from_str::<CityId>("42")?, id);
        assert!(serde_json::from_str::<CityId>("0").is_err());
        assert_eq!(serde_json::to_string(&TileIdx(9))?, "9");
        Ok(())
    }

    #[test]
    fn id_vec_over_entities_starts_at_the_first_id() {
        let mut v: IdVec<CityId, u8> = IdVec::new();
        let seven = CityId::new(7).expect("7 is an id");
        *v.ensure(seven) = 9;
        assert_eq!(v.len(), 7);
        assert_eq!(v.get(seven), Some(&9));
        assert_eq!(v[seven], 9);
        assert_eq!(v.as_slice(), &[0, 0, 0, 0, 0, 0, 9]);
        assert_eq!(v.iter().count(), 7);
        assert_eq!(v.ids().next(), Some(CityId::FIRST));
        assert_eq!(v.ids().last(), Some(seven));
        assert_eq!(v.get(CityId::new(8).expect("8 is an id")), None);
    }

    #[test]
    fn id_vec_push_hands_out_entity_ids_from_one() {
        let mut v: IdVec<CityId, u8> = IdVec::new();
        assert_eq!(v.push(1), Ok(CityId::FIRST));
        assert_eq!(v.push(2), CityId::new(2).ok_or(0));
        assert_eq!(v[CityId::FIRST], 1);
        assert_eq!(v.iter().map(|(id, &x)| (id.get(), x)).collect::<Vec<_>>(), [(1, 1), (2, 2)]);
        let collected: IdVec<UnitId, char> = ['a', 'b'].into_iter().collect();
        assert_eq!(collected.get(UnitId::FIRST), Some(&'a'));
    }

    #[test]
    fn id_vec_push_hands_out_ids_in_order() {
        let mut v: IdVec<TerrainId, u32> = IdVec::new();
        for i in 0..256 {
            assert_eq!(v.push(i), Ok(TerrainId(i as u8)));
        }
        assert_eq!(v.push(256), Err(256));
        assert_eq!(v[TerrainId(10)], 10);
    }
}

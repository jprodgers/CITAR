//! Sets over ids, as bits (DESIGN.md 4.2).
//!
//! Python kept these as lists and sets of names or ints (`Player.techs`, `Player.met`,
//! `Player.explored`, ...). As bits they iterate in ascending id order without sorting, compare
//! and combine a word at a time, and digest as raw words.
//!
//! - [`IdSet`] has a fixed width for a rule table: [`TechSet`] is `IdSet<TechId, TECH_WORDS>`
//!   and holds 128 techs. The ruleset loader refuses a table larger than its set, naming the
//!   constant to raise, so an id never exceeds the capacity at runtime.
//! - [`FeatureSet`] is one `u16` over the terrain features, in layer order.
//! - [`BitSet`] grows, for tile sets such as a player's explored tiles.
//! - [`PlayerSet`] is one `u64`, which is why a game has at most 64 players.

use core::fmt;
use core::marker::PhantomData;
use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Sub, SubAssign};

use super::ids::{
    BaseUnitId, BeliefId, BuildingId, EraId, FeatureId, Id, IdVec, ImprovementId, PlayerId,
    PolicyId, PromotionId, ResourceId, TechId, TerrainId,
};

/// A vector with one entry per player, indexed by [`PlayerId`].
pub type PlayerVec<T> = IdVec<PlayerId, T>;

/// The indices of the set bits of `words`, ascending.
#[derive(Clone, Debug)]
pub struct Bits<'a> {
    rest: &'a [u64],
    word: u64,
    base: usize,
}

impl<'a> Bits<'a> {
    fn new(words: &'a [u64]) -> Self {
        match words.split_first() {
            Some((&first, rest)) => Self { rest, word: first, base: 0 },
            None => Self { rest: &[], word: 0, base: 0 },
        }
    }
}

impl Iterator for Bits<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        while self.word == 0 {
            let (&next, rest) = self.rest.split_first()?;
            self.word = next;
            self.rest = rest;
            self.base += 64;
        }
        let bit = self.word.trailing_zeros() as usize;
        self.word &= self.word - 1;
        Some(self.base + bit)
    }
}

// ---- IdSet ------------------------------------------------------------------------------------

/// A set of rule ids in `W` words: capacity `64 * W`.
pub struct IdSet<I, const W: usize> {
    words: [u64; W],
    _id: PhantomData<fn(I)>,
}

impl<I: Id, const W: usize> IdSet<I, W> {
    /// How many ids fit: the largest index is one less.
    pub const CAPACITY: usize = 64 * W;

    /// The empty set.
    #[must_use]
    pub const fn new() -> Self {
        Self { words: [0; W], _id: PhantomData }
    }

    /// The set with these raw words, bit `i` of word `w` being index `64 * w + i`.
    #[must_use]
    pub const fn from_words(words: [u64; W]) -> Self {
        Self { words, _id: PhantomData }
    }

    /// The raw words.
    #[must_use]
    pub const fn words(&self) -> &[u64; W] {
        &self.words
    }

    #[inline]
    fn slot(id: I) -> Option<(usize, u64)> {
        let i = id.index();
        (i < Self::CAPACITY).then(|| (i / 64, 1u64 << (i % 64)))
    }

    /// Adds `id`; true if it was not there already.
    ///
    /// An id past the capacity is not added (and fails a debug assertion): the ruleset loader
    /// refuses tables larger than their sets, so it can only be a bug.
    #[inline]
    pub fn insert(&mut self, id: I) -> bool {
        debug_assert!(id.index() < Self::CAPACITY, "{id:?} does not fit an IdSet of {W} words");
        let Some((w, bit)) = Self::slot(id) else { return false };
        let fresh = self.words[w] & bit == 0;
        self.words[w] |= bit;
        fresh
    }

    /// Removes `id`; true if it was there.
    #[inline]
    pub fn remove(&mut self, id: I) -> bool {
        let Some((w, bit)) = Self::slot(id) else { return false };
        let had = self.words[w] & bit != 0;
        self.words[w] &= !bit;
        had
    }

    /// Whether `id` is in the set.
    #[must_use]
    #[inline]
    pub fn contains(&self, id: I) -> bool {
        Self::slot(id).is_some_and(|(w, bit)| self.words[w] & bit != 0)
    }

    /// The number of ids in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    /// Removes every id.
    pub fn clear(&mut self) {
        self.words = [0; W];
    }

    /// The ids, ascending.
    pub fn iter(&self) -> impl Iterator<Item = I> + '_ {
        Bits::new(&self.words).filter_map(I::from_index)
    }

    /// Whether every id of `self` is in `other`.
    #[must_use]
    pub fn is_subset(&self, other: &Self) -> bool {
        self.words.iter().zip(&other.words).all(|(a, b)| a & !b == 0)
    }

    /// Whether the two sets share no id.
    #[must_use]
    pub fn is_disjoint(&self, other: &Self) -> bool {
        self.words.iter().zip(&other.words).all(|(a, b)| a & b == 0)
    }

    fn zip_with(&self, other: &Self, f: impl Fn(u64, u64) -> u64) -> Self {
        let mut words = self.words;
        for (w, o) in words.iter_mut().zip(&other.words) {
            *w = f(*w, *o);
        }
        Self::from_words(words)
    }
}

impl<I: Id, const W: usize> Default for IdSet<I, W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I, const W: usize> Clone for IdSet<I, W> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<I, const W: usize> Copy for IdSet<I, W> {}

impl<I, const W: usize> PartialEq for IdSet<I, W> {
    fn eq(&self, other: &Self) -> bool {
        self.words == other.words
    }
}

impl<I, const W: usize> Eq for IdSet<I, W> {}

impl<I, const W: usize> core::hash::Hash for IdSet<I, W> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.words.hash(state);
    }
}

impl<I: Id, const W: usize> fmt::Debug for IdSet<I, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<I: Id, const W: usize> FromIterator<I> for IdSet<I, W> {
    fn from_iter<It: IntoIterator<Item = I>>(iter: It) -> Self {
        let mut set = Self::new();
        set.extend(iter);
        set
    }
}

impl<I: Id, const W: usize> Extend<I> for IdSet<I, W> {
    fn extend<It: IntoIterator<Item = I>>(&mut self, iter: It) {
        for id in iter {
            self.insert(id);
        }
    }
}

impl<I: Id, const W: usize> BitOr for IdSet<I, W> {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.zip_with(&rhs, |a, b| a | b)
    }
}

impl<I: Id, const W: usize> BitAnd for IdSet<I, W> {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        self.zip_with(&rhs, |a, b| a & b)
    }
}

impl<I: Id, const W: usize> Sub for IdSet<I, W> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.zip_with(&rhs, |a, b| a & !b)
    }
}

impl<I: Id, const W: usize> BitOrAssign for IdSet<I, W> {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = *self | rhs;
    }
}

impl<I: Id, const W: usize> BitAndAssign for IdSet<I, W> {
    fn bitand_assign(&mut self, rhs: Self) {
        *self = *self & rhs;
    }
}

impl<I: Id, const W: usize> SubAssign for IdSet<I, W> {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

// ---- The rule sets and their widths (DESIGN.md 4.2) --------------------------------------------
//
// The ruleset loader refuses a table larger than its set and names the constant to raise, so an
// id never exceeds its set at runtime. The widths leave mods room above the shipped counts.

/// Words in a [`TechSet`]: 128 techs (80 shipped).
pub const TECH_WORDS: usize = 2;
/// Words in a [`PolicySet`]: 128 branches and policies (10 + 60 shipped).
pub const POLICY_WORDS: usize = 2;
/// Words in a [`BuildingSet`]: 256 buildings (124 shipped).
pub const BUILDING_WORDS: usize = 4;
/// Words in a [`BaseUnitSet`]: 256 units (127 shipped).
pub const BASE_UNIT_WORDS: usize = 4;
/// Words in a [`PromotionSet`]: 256 promotions (106 shipped).
pub const PROMOTION_WORDS: usize = 4;
/// Words in a [`BeliefSet`]: 128 beliefs (56 shipped).
pub const BELIEF_WORDS: usize = 2;
/// Words in a [`TerrainSet`]: 64 terrains, features and natural wonders included (33 shipped).
pub const TERRAIN_WORDS: usize = 1;
/// Words in a [`ResourceSet`]: 64 resources (35 shipped).
pub const RESOURCE_WORDS: usize = 1;
/// Words in an [`ImprovementSet`]: 64 improvements (35 shipped).
pub const IMPROVEMENT_WORDS: usize = 1;
/// Words in an [`EraSet`]: 64 eras (9 shipped).
pub const ERA_WORDS: usize = 1;

/// Techs, such as a player's known techs.
pub type TechSet = IdSet<TechId, TECH_WORDS>;
/// Policy branches and policies.
pub type PolicySet = IdSet<PolicyId, POLICY_WORDS>;
/// Buildings, such as a city's buildings.
pub type BuildingSet = IdSet<BuildingId, BUILDING_WORDS>;
/// Units of `units.json`.
pub type BaseUnitSet = IdSet<BaseUnitId, BASE_UNIT_WORDS>;
/// Promotions, such as a unit's promotions.
pub type PromotionSet = IdSet<PromotionId, PROMOTION_WORDS>;
/// Beliefs.
pub type BeliefSet = IdSet<BeliefId, BELIEF_WORDS>;
/// Terrains of `terrains.json`.
pub type TerrainSet = IdSet<TerrainId, TERRAIN_WORDS>;
/// Resources.
pub type ResourceSet = IdSet<ResourceId, RESOURCE_WORDS>;
/// Improvements.
pub type ImprovementSet = IdSet<ImprovementId, IMPROVEMENT_WORDS>;
/// Eras.
pub type EraSet = IdSet<EraId, ERA_WORDS>;

// ---- FeatureSet -------------------------------------------------------------------------------

/// The terrain features on one tile, in one `u16`: bit `i` is `FeatureId(i)`.
///
/// The ruleset numbers its features in layer order, Hill lowest and Fallout highest, so the
/// highest set bit is the feature on top ([`top`](Self::top)). Python kept a list in the order
/// features were added and read its last non-Hill entry (`state.py:101-107`); for every
/// combination the map generator and the rules can produce, the two agree.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FeatureSet(u16);

impl FeatureSet {
    /// How many features fit (10 shipped, Hill included). The ruleset loader refuses more.
    pub const CAPACITY: usize = 16;

    /// No features.
    pub const EMPTY: Self = Self(0);

    /// The set with these raw bits.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// The raw bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    #[inline]
    fn bit(f: FeatureId) -> Option<u16> {
        (usize::from(f.0) < Self::CAPACITY).then(|| 1u16 << f.0)
    }

    /// Adds `f`; true if it was not there already. A feature past the capacity is not added (and
    /// fails a debug assertion): the loader refuses rulesets with more features than fit.
    pub fn insert(&mut self, f: FeatureId) -> bool {
        debug_assert!(usize::from(f.0) < Self::CAPACITY, "{f:?} does not fit a FeatureSet");
        let Some(bit) = Self::bit(f) else { return false };
        let fresh = self.0 & bit == 0;
        self.0 |= bit;
        fresh
    }

    /// Removes `f`; true if it was there.
    pub fn remove(&mut self, f: FeatureId) -> bool {
        let Some(bit) = Self::bit(f) else { return false };
        let had = self.0 & bit != 0;
        self.0 &= !bit;
        had
    }

    /// Whether `f` is in the set.
    #[must_use]
    #[inline]
    pub fn contains(self, f: FeatureId) -> bool {
        Self::bit(f).is_some_and(|bit| self.0 & bit != 0)
    }

    /// The number of features.
    #[must_use]
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The feature in the highest layer, if any.
    #[must_use]
    pub const fn top(self) -> Option<FeatureId> {
        if self.0 == 0 { None } else { Some(FeatureId((15 - self.0.leading_zeros()) as u8)) }
    }

    /// The features, lowest layer first.
    pub fn iter(self) -> impl Iterator<Item = FeatureId> {
        let mut rest = self.0;
        core::iter::from_fn(move || {
            if rest == 0 {
                return None;
            }
            // Below 16, so it fits a u8.
            let i = rest.trailing_zeros() as u8;
            rest &= rest - 1;
            Some(FeatureId(i))
        })
    }
}

impl fmt::Debug for FeatureSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

// ---- BitSet -----------------------------------------------------------------------------------

/// A growable set of `u32` indices, for tiles: explored tiles, visible tiles, visited nodes.
///
/// Two sets with the same members are equal however many trailing zero words either holds, and
/// [`words`](Self::words) and serde, the only ways the words leave the set, drop those zero words.
/// So equal sets digest the same (DESIGN.md 4.10), whether one was sized for the map with
/// [`with_capacity`](Self::with_capacity) and the other read back from a save.
#[derive(Clone, Default)]
pub struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    /// The empty set.
    #[must_use]
    pub const fn new() -> Self {
        Self { words: Vec::new() }
    }

    /// The empty set, with words already allocated for indices below `bits`.
    #[must_use]
    pub fn with_capacity(bits: u32) -> Self {
        Self { words: vec![0; (bits as usize).div_ceil(64)] }
    }

    /// The set with these raw words, bit `i` of word `w` being index `64 * w + i`.
    #[must_use]
    pub const fn from_words(words: Vec<u64>) -> Self {
        Self { words }
    }

    /// The raw words up to the last one with a bit set: the canonical form, the same for any two
    /// equal sets. Bit `i` of word `w` is index `64 * w + i`.
    #[must_use]
    pub fn words(&self) -> &[u64] {
        let end = self.words.iter().rposition(|&w| w != 0).map_or(0, |i| i + 1);
        &self.words[..end]
    }

    /// Adds `i`, growing the set if needed; true if it was not there already.
    #[inline]
    pub fn insert(&mut self, i: u32) -> bool {
        let (w, bit) = ((i / 64) as usize, 1u64 << (i % 64));
        if w >= self.words.len() {
            self.words.resize(w + 1, 0);
        }
        let fresh = self.words[w] & bit == 0;
        self.words[w] |= bit;
        fresh
    }

    /// Removes `i`; true if it was there.
    #[inline]
    pub fn remove(&mut self, i: u32) -> bool {
        let (w, bit) = ((i / 64) as usize, 1u64 << (i % 64));
        match self.words.get_mut(w) {
            Some(word) => {
                let had = *word & bit != 0;
                *word &= !bit;
                had
            }
            None => false,
        }
    }

    /// Whether `i` is in the set.
    #[must_use]
    #[inline]
    pub fn contains(&self, i: u32) -> bool {
        self.words.get((i / 64) as usize).is_some_and(|w| w & (1u64 << (i % 64)) != 0)
    }

    /// The number of indices in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    /// Removes every index, keeping the allocation.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }

    /// The indices, ascending.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        // Every index came from a u32, so it converts back.
        Bits::new(&self.words).map(|i| u32::try_from(i).unwrap_or(u32::MAX))
    }

    /// Adds every index of `other`.
    pub fn union_with(&mut self, other: &Self) {
        if other.words.len() > self.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        for (w, o) in self.words.iter_mut().zip(&other.words) {
            *w |= o;
        }
    }

    /// Keeps only the indices also in `other`.
    pub fn intersect_with(&mut self, other: &Self) {
        for (i, w) in self.words.iter_mut().enumerate() {
            *w &= other.words.get(i).copied().unwrap_or(0);
        }
    }

    /// Removes every index of `other`.
    pub fn difference_with(&mut self, other: &Self) {
        for (w, o) in self.words.iter_mut().zip(&other.words) {
            *w &= !o;
        }
    }

    /// Whether every index of `self` is in `other`.
    #[must_use]
    pub fn is_subset(&self, other: &Self) -> bool {
        self.words
            .iter()
            .enumerate()
            .all(|(i, w)| w & !other.words.get(i).copied().unwrap_or(0) == 0)
    }
}

impl PartialEq for BitSet {
    fn eq(&self, other: &Self) -> bool {
        self.words() == other.words()
    }
}

/// [`words`](BitSet::words) as a sequence of `u64`, in both encodings.
impl serde::Serialize for BitSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.words().serialize(serializer)
    }
}

/// A sequence of `u64` words; trailing zero words are accepted and change nothing.
impl<'de> serde::Deserialize<'de> for BitSet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<u64>::deserialize(deserializer).map(Self::from_words)
    }
}

impl Eq for BitSet {}

impl fmt::Debug for BitSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl FromIterator<u32> for BitSet {
    fn from_iter<It: IntoIterator<Item = u32>>(iter: It) -> Self {
        let mut set = Self::new();
        set.extend(iter);
        set
    }
}

impl Extend<u32> for BitSet {
    fn extend<It: IntoIterator<Item = u32>>(&mut self, iter: It) {
        for i in iter {
            self.insert(i);
        }
    }
}

// ---- PlayerSet --------------------------------------------------------------------------------

/// A set of players in one `u64`.
///
/// A player id of 64 or more cannot be a member: the game setup refuses more than 64 seats.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PlayerSet(u64);

impl PlayerSet {
    /// How many players fit.
    pub const CAPACITY: usize = 64;

    /// No players.
    pub const EMPTY: Self = Self(0);

    /// The set with these raw bits, bit `i` being `PlayerId(i)`.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The raw bits.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// The first `n` players (all of them from 64 on).
    #[must_use]
    pub const fn first(n: u8) -> Self {
        if n >= 64 { Self(u64::MAX) } else { Self((1u64 << n) - 1) }
    }

    /// Just `p`.
    #[must_use]
    pub fn single(p: PlayerId) -> Self {
        let mut s = Self::EMPTY;
        s.insert(p);
        s
    }

    #[inline]
    fn bit(p: PlayerId) -> Option<u64> {
        (p.0 < 64).then(|| 1u64 << p.0)
    }

    /// Adds `p`; true if it was not there already. A player id of 64 or more is not added (and
    /// fails a debug assertion).
    #[inline]
    pub fn insert(&mut self, p: PlayerId) -> bool {
        debug_assert!(p.0 < 64, "{p:?} does not fit a PlayerSet");
        let Some(bit) = Self::bit(p) else { return false };
        let fresh = self.0 & bit == 0;
        self.0 |= bit;
        fresh
    }

    /// Removes `p`; true if it was there.
    #[inline]
    pub fn remove(&mut self, p: PlayerId) -> bool {
        let Some(bit) = Self::bit(p) else { return false };
        let had = self.0 & bit != 0;
        self.0 &= !bit;
        had
    }

    /// Whether `p` is in the set.
    #[must_use]
    #[inline]
    pub fn contains(self, p: PlayerId) -> bool {
        Self::bit(p).is_some_and(|bit| self.0 & bit != 0)
    }

    /// The number of players in the set.
    #[must_use]
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The players, ascending.
    pub fn iter(self) -> impl Iterator<Item = PlayerId> {
        let mut bits = self.0;
        core::iter::from_fn(move || {
            if bits == 0 {
                return None;
            }
            // trailing_zeros of a nonzero u64 is below 64, so it fits a u8.
            let p = bits.trailing_zeros() as u8;
            bits &= bits - 1;
            Some(PlayerId(p))
        })
    }

    /// Whether every player of `self` is in `other`.
    #[must_use]
    pub const fn is_subset(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }
}

impl fmt::Debug for PlayerSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter().map(|p| p.0)).finish()
    }
}

impl FromIterator<PlayerId> for PlayerSet {
    fn from_iter<It: IntoIterator<Item = PlayerId>>(iter: It) -> Self {
        let mut set = Self::EMPTY;
        set.extend(iter);
        set
    }
}

impl Extend<PlayerId> for PlayerSet {
    fn extend<It: IntoIterator<Item = PlayerId>>(&mut self, iter: It) {
        for p in iter {
            self.insert(p);
        }
    }
}

impl BitOr for PlayerSet {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitAnd for PlayerSet {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl Sub for PlayerSet {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 & !rhs.0)
    }
}

impl BitOrAssign for PlayerSet {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAndAssign for PlayerSet {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl SubAssign for PlayerSet {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 &= !rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_set_iterates_ascending_across_words() {
        let set: TechSet = [TechId(127), TechId(3), TechId(64), TechId(0)].into_iter().collect();
        assert_eq!(set.iter().collect::<Vec<_>>(), [TechId(0), TechId(3), TechId(64), TechId(127)]);
        assert_eq!(set.len(), 4);
        assert!(set.contains(TechId(64)));
        assert!(!set.contains(TechId(65)));
        assert!(!set.contains(TechId(500)));
    }

    #[test]
    fn id_set_algebra() {
        let a: TechSet = [TechId(1), TechId(70)].into_iter().collect();
        let b: TechSet = [TechId(70), TechId(71)].into_iter().collect();
        assert_eq!((a | b).len(), 3);
        assert_eq!((a & b).iter().collect::<Vec<_>>(), [TechId(70)]);
        assert_eq!((a - b).iter().collect::<Vec<_>>(), [TechId(1)]);
        assert!((a & b).is_subset(&a));
        assert!(!a.is_disjoint(&b));
    }

    #[test]
    fn bit_set_equality_ignores_trailing_zero_words() {
        let mut a = BitSet::with_capacity(1000);
        let mut b = BitSet::new();
        a.insert(5);
        b.insert(5);
        assert_eq!(a, b);
        b.insert(900);
        b.remove(900);
        assert_eq!(a, b);
        assert_eq!(b.words(), &[1 << 5]);
        a.clear();
        assert_eq!(a.words(), &[] as &[u64]);
    }

    /// Equal sets write the same bytes, so a set sized for the map and the same set read back
    /// from a save digest alike.
    #[test]
    fn equal_bit_sets_serialise_alike() -> Result<(), Box<dyn std::error::Error>> {
        use crate::base::digest::to_canon_vec;

        let mut sized = BitSet::with_capacity(16_000);
        sized.insert(70);
        sized.insert(9_000);
        sized.remove(9_000);
        let loaded = BitSet::from_words(vec![0, 1 << 6]);
        assert_eq!(sized, loaded);
        assert_eq!(to_canon_vec(&sized)?, to_canon_vec(&loaded)?);
        assert_eq!(
            to_canon_vec(&loaded)?,
            [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0]
        );
        let json = serde_json::to_string(&sized)?;
        assert_eq!(json, "[0,64]");
        let back: BitSet = serde_json::from_str("[0,64,0,0]")?;
        assert_eq!(back, sized);
        assert_eq!(to_canon_vec(&back)?, to_canon_vec(&sized)?);
        assert_eq!(to_canon_vec(&BitSet::with_capacity(640))?, to_canon_vec(&BitSet::new())?);
        Ok(())
    }

    #[test]
    fn feature_set_top_is_the_highest_layer() {
        let mut s = FeatureSet::EMPTY;
        assert_eq!(s.top(), None);
        assert!(s.insert(FeatureId(0)));
        assert!(s.insert(FeatureId(4)));
        assert!(!s.insert(FeatureId(4)));
        assert_eq!(s.top(), Some(FeatureId(4)));
        assert!(s.insert(FeatureId(15)));
        assert_eq!(s.top(), Some(FeatureId(15)));
        assert_eq!(s.iter().collect::<Vec<_>>(), [FeatureId(0), FeatureId(4), FeatureId(15)]);
        assert!(s.remove(FeatureId(15)));
        assert!(!s.contains(FeatureId(15)));
        assert_eq!(s.len(), 2);
        assert!(!s.contains(FeatureId(16)));
    }

    #[test]
    fn set_widths_hold_the_shipped_tables() {
        assert_eq!(TechSet::CAPACITY, 128);
        assert_eq!(PromotionSet::CAPACITY, 256);
        assert_eq!(TerrainSet::CAPACITY, 64);
    }

    #[test]
    fn player_set_basics() {
        let mut s = PlayerSet::first(3);
        assert_eq!(s.iter().collect::<Vec<_>>(), [PlayerId(0), PlayerId(1), PlayerId(2)]);
        assert!(s.insert(PlayerId(63)));
        assert!(!s.insert(PlayerId(63)));
        assert!(s.remove(PlayerId(1)));
        assert_eq!(s.len(), 3);
        assert!(!s.contains(PlayerId(64)));
        assert_eq!(PlayerSet::first(64).len(), 64);
    }
}

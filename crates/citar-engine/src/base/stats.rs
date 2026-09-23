//! The seven yields as a fixed array (DESIGN.md 3.2).
//!
//! Python carried yields as dicts keyed by name (`{"food": 2.0, "gold": 1.0}`), with
//! `uniques.py:19-22` mapping UnCiv's stat names ("Food") to those keys and `rules.py:18`,
//! `cities.py:22` fixing their order. Here a [`Stat`] is an index, a [`Stats`] is seven floats
//! in that order, and a [`StatMask`] is a set of stats in one byte. Names appear only where text
//! is read or written.

use core::fmt;
use core::ops::{
    Add, AddAssign, BitAnd, BitOr, BitOrAssign, Index, IndexMut, Mul, MulAssign, Neg, Sub,
    SubAssign,
};

/// One yield, in Python's order (`rules.py:18`, `cities.py:22`).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stat {
    Food = 0,
    Production = 1,
    Gold = 2,
    Science = 3,
    Culture = 4,
    Happiness = 5,
    Faith = 6,
}

impl Stat {
    /// How many stats there are.
    pub const COUNT: usize = 7;

    /// Every stat, in order.
    pub const ALL: [Stat; Self::COUNT] = [
        Self::Food,
        Self::Production,
        Self::Gold,
        Self::Science,
        Self::Culture,
        Self::Happiness,
        Self::Faith,
    ];

    /// The stat's position in a [`Stats`].
    #[must_use]
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// UnCiv's name, as the ruleset and unique texts write it: `"Food"`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Food => "Food",
            Self::Production => "Production",
            Self::Gold => "Gold",
            Self::Science => "Science",
            Self::Culture => "Culture",
            Self::Happiness => "Happiness",
            Self::Faith => "Faith",
        }
    }

    /// The key Python's dicts and the client JSON use: `"food"`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Food => "food",
            Self::Production => "production",
            Self::Gold => "gold",
            Self::Science => "science",
            Self::Culture => "culture",
            Self::Happiness => "happiness",
            Self::Faith => "faith",
        }
    }

    /// The stat UnCiv calls `name` (`"Food"`), exactly. For reading the ruleset, not rule code.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// The stat whose key is `key` (`"food"`), exactly. For reading the ruleset, not rule code.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    /// Whether the stat is pooled per civilization rather than per city
    /// (`uniques.py:21`, `CIV_WIDE_STATS`): everything but food and production.
    #[must_use]
    pub const fn is_civ_wide(self) -> bool {
        !matches!(self, Self::Food | Self::Production)
    }
}

impl fmt::Display for Stat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

/// A value for each of the seven stats.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats(pub [f64; Stat::COUNT]);

impl Stats {
    /// All zero.
    pub const ZERO: Self = Self([0.0; Stat::COUNT]);

    /// Just `value` of `stat`.
    #[must_use]
    pub fn single(stat: Stat, value: f64) -> Self {
        let mut s = Self::ZERO;
        s[stat] = value;
        s
    }

    /// The value of `stat`.
    #[must_use]
    #[inline]
    pub const fn get(&self, stat: Stat) -> f64 {
        self.0[stat.index()]
    }

    /// Sets the value of `stat`.
    #[inline]
    pub fn set(&mut self, stat: Stat, value: f64) {
        self.0[stat.index()] = value;
    }

    /// Each stat with its value, in order.
    pub fn iter(&self) -> impl Iterator<Item = (Stat, f64)> + '_ {
        Stat::ALL.into_iter().map(|s| (s, self.get(s)))
    }

    /// The stats whose value is not zero, with their values, in order.
    pub fn nonzero(&self) -> impl Iterator<Item = (Stat, f64)> + '_ {
        self.iter().filter(|(_, v)| *v != 0.0)
    }

    /// Whether every value is zero (either sign).
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|v| *v == 0.0)
    }

    /// Adds `other * k` stat by stat.
    pub fn add_scaled(&mut self, other: &Self, k: f64) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a += b * k;
        }
    }

    /// The stats in `mask`, with the others zeroed.
    #[must_use]
    pub fn masked(&self, mask: StatMask) -> Self {
        let mut out = Self::ZERO;
        for s in mask.iter() {
            out[s] = self[s];
        }
        out
    }

    fn zip_with(self, rhs: Self, f: impl Fn(f64, f64) -> f64) -> Self {
        let mut out = self;
        for (a, b) in out.0.iter_mut().zip(rhs.0) {
            *a = f(*a, b);
        }
        out
    }
}

impl Index<Stat> for Stats {
    type Output = f64;
    #[inline]
    fn index(&self, stat: Stat) -> &f64 {
        &self.0[stat.index()]
    }
}

impl IndexMut<Stat> for Stats {
    #[inline]
    fn index_mut(&mut self, stat: Stat) -> &mut f64 {
        &mut self.0[stat.index()]
    }
}

impl Add for Stats {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        self.zip_with(rhs, |a, b| a + b)
    }
}

impl Sub for Stats {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.zip_with(rhs, |a, b| a - b)
    }
}

impl AddAssign for Stats {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl SubAssign for Stats {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Mul<f64> for Stats {
    type Output = Self;
    fn mul(self, k: f64) -> Self {
        Self(self.0.map(|v| v * k))
    }
}

impl MulAssign<f64> for Stats {
    fn mul_assign(&mut self, k: f64) {
        *self = *self * k;
    }
}

impl Neg for Stats {
    type Output = Self;
    fn neg(self) -> Self {
        Self(self.0.map(|v| -v))
    }
}

/// A set of stats in one byte, bit `i` being the stat with index `i`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StatMask(u8);

impl StatMask {
    /// No stats.
    pub const EMPTY: Self = Self(0);
    /// All seven.
    pub const ALL: Self = Self(0x7f);
    /// The stats pooled per civilization (`uniques.py:21`).
    pub const CIV_WIDE: Self = Self(0x7f & !0b11);

    /// The mask with these bits; bits past the seventh are dropped.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0x7f)
    }

    /// The raw bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Just `stat`.
    #[must_use]
    pub const fn single(stat: Stat) -> Self {
        Self(1 << stat as u8)
    }

    /// Whether `stat` is in the mask.
    #[must_use]
    #[inline]
    pub const fn contains(self, stat: Stat) -> bool {
        self.0 & (1 << stat as u8) != 0
    }

    /// Adds `stat`.
    pub fn insert(&mut self, stat: Stat) {
        self.0 |= 1 << stat as u8;
    }

    /// Removes `stat`.
    pub fn remove(&mut self, stat: Stat) {
        self.0 &= !(1 << stat as u8);
    }

    /// The number of stats in the mask.
    #[must_use]
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether the mask is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The stats in the mask, in order.
    pub fn iter(self) -> impl Iterator<Item = Stat> {
        Stat::ALL.into_iter().filter(move |s| self.contains(*s))
    }
}

impl fmt::Debug for StatMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl FromIterator<Stat> for StatMask {
    fn from_iter<It: IntoIterator<Item = Stat>>(iter: It) -> Self {
        let mut m = Self::EMPTY;
        for s in iter {
            m.insert(s);
        }
        m
    }
}

impl BitOr for StatMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitAnd for StatMask {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitOrAssign for StatMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_keys() {
        for s in Stat::ALL {
            assert_eq!(Stat::from_name(s.name()), Some(s));
            assert_eq!(Stat::from_key(s.key()), Some(s));
            assert_eq!(s.name().to_lowercase(), s.key());
        }
        assert_eq!(Stat::from_name("food"), None);
        assert_eq!(
            StatMask::CIV_WIDE.iter().collect::<Vec<_>>(),
            [Stat::Gold, Stat::Science, Stat::Culture, Stat::Happiness, Stat::Faith]
        );
        assert!(StatMask::CIV_WIDE.iter().all(Stat::is_civ_wide));
    }

    #[test]
    fn arithmetic() {
        let mut a = Stats::single(Stat::Food, 2.0) + Stats::single(Stat::Gold, 1.0);
        a *= 1.5;
        assert_eq!(a[Stat::Food].to_bits(), 3.0f64.to_bits());
        assert_eq!(a.nonzero().count(), 2);
        a.add_scaled(&Stats::single(Stat::Gold, 2.0), -0.75);
        assert!(a.masked(StatMask::single(Stat::Gold)).is_zero());
    }
}

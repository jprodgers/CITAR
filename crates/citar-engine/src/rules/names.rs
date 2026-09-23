//! Looking ruleset objects up by name or id, loosely, as tools do (`rules.py:28-30, 182-191,
//! 263-270`).
//!
//! A tool call may name a technology as `Bronze Working`, `bronze_working` or `BRONZEWORKING`.
//! Python kept one dict per table from each normalised name and id to the name, filled in table
//! order so that a later object wins a clash, and looked the exact text up first. Here each table
//! has two sorted slices, one of exact names and one of normalised keys with the same last-wins
//! rule, searched by bisection.
//!
//! Only tools and the lobby resolve loosely. Rule code holds ids, and a save names objects
//! exactly.

use crate::base::text::norm;

/// A table that names can be resolved in: the keys of Python's `Rules.tables()`
/// (`rules.py:230-236`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NameKind {
    Tech,
    Unit,
    Building,
    Promotion,
    Terrain,
    Resource,
    Improvement,
    Belief,
    /// Policy branches and policies, in one table, branches first.
    Policy,
    Nation,
    Era,
    Specialist,
    Speed,
    Difficulty,
    UnitType,
    Victory,
}

impl NameKind {
    /// Every kind, in Python's order.
    pub const ALL: [Self; 16] = [
        Self::Tech,
        Self::Unit,
        Self::Building,
        Self::Promotion,
        Self::Terrain,
        Self::Resource,
        Self::Improvement,
        Self::Belief,
        Self::Policy,
        Self::Nation,
        Self::Era,
        Self::Specialist,
        Self::Speed,
        Self::Difficulty,
        Self::UnitType,
        Self::Victory,
    ];

    /// The name tools and the facade use: `tech`, `unit_type`, ...
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tech => "tech",
            Self::Unit => "unit",
            Self::Building => "building",
            Self::Promotion => "promotion",
            Self::Terrain => "terrain",
            Self::Resource => "resource",
            Self::Improvement => "improvement",
            Self::Belief => "belief",
            Self::Policy => "policy",
            Self::Nation => "nation",
            Self::Era => "era",
            Self::Specialist => "specialist",
            Self::Speed => "speed",
            Self::Difficulty => "difficulty",
            Self::UnitType => "unit_type",
            Self::Victory => "victory",
        }
    }

    /// The kind called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// One table's names, for exact and loose lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NameIndex {
    /// Each object's name, by position in the table.
    names: Box<[Box<str>]>,
    /// (name, position), sorted by name.
    exact: Box<[(Box<str>, u16)]>,
    /// (normalised name or id, position), sorted by key; where two objects normalise alike, the
    /// later one in the table, as Python's dict assignment left it.
    loose: Box<[(Box<str>, u16)]>,
}

impl NameIndex {
    /// The index of a table whose objects have these names and ids, in table order. The table
    /// fits a `u16` (the loader checked the capacities first).
    pub(crate) fn new<'a>(objects: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> Self {
        let mut names = Vec::new();
        let mut loose: Vec<(Box<str>, u16)> = Vec::new();
        for (pos, (name, key)) in objects.into_iter().enumerate() {
            let pos = u16::try_from(pos).unwrap_or(u16::MAX);
            names.push(Box::<str>::from(name));
            loose.push((norm(name).into(), pos));
            if let Some(key) = key.filter(|k| !k.is_empty()) {
                loose.push((norm(key).into(), pos));
            }
        }
        // A stable sort keeps assignment order among equal keys, so the last of each run is the
        // one Python's dict kept.
        loose.sort_by(|a, b| a.0.cmp(&b.0));
        let mut deduped: Vec<(Box<str>, u16)> = Vec::with_capacity(loose.len());
        for entry in loose {
            match deduped.last_mut() {
                Some(last) if last.0 == entry.0 => *last = entry,
                _ => deduped.push(entry),
            }
        }
        let mut exact: Vec<(Box<str>, u16)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), u16::try_from(i).unwrap_or(u16::MAX)))
            .collect();
        exact.sort_by(|a, b| a.0.cmp(&b.0));
        Self { names: names.into(), exact: exact.into(), loose: deduped.into() }
    }

    /// The position of the object called exactly `name`.
    pub(crate) fn lookup(&self, name: &str) -> Option<usize> {
        let i = self.exact.binary_search_by(|(n, _)| (**n).cmp(name)).ok()?;
        Some(usize::from(self.exact[i].1))
    }

    /// The position of the object `text` names exactly, or else loosely: lower case, with
    /// everything but ASCII letters and digits dropped (`rules.py:263-270`).
    pub(crate) fn resolve(&self, text: &str) -> Option<usize> {
        if let Some(i) = self.lookup(text) {
            return Some(i);
        }
        let key = norm(text);
        let i = self.loose.binary_search_by(|(k, _)| (**k).cmp(key.as_str())).ok()?;
        Some(usize::from(self.loose[i].1))
    }

    /// The name of the object at `pos`.
    pub(crate) fn name(&self, pos: usize) -> Option<&str> {
        self.names.get(pos).map(|n| &**n)
    }

    /// Every name, in table order.
    pub(crate) fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.names.iter().map(|n| &**n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_names_win_then_the_last_loose_match() {
        let idx = NameIndex::new([
            ("Atomic Bomber", Some("atomic_bomber")),
            ("Fighter", Some("fighter")),
            ("AtomicBomber", Some("atomicbomber")),
        ]);
        assert_eq!(idx.resolve("Atomic Bomber"), Some(0), "an exact name comes first");
        assert_eq!(idx.resolve("atomic_bomber"), Some(2), "the later object wins the clash");
        assert_eq!(idx.resolve("ATOMIC BOMBER"), Some(2));
        assert_eq!(idx.resolve("fighter!"), Some(1));
        assert_eq!(idx.resolve("figher"), None);
        assert_eq!(idx.resolve(""), None);
        assert_eq!(idx.lookup("fighter"), None, "lookup is exact");
        assert_eq!(idx.name(2), Some("AtomicBomber"));
        assert_eq!(idx.names().count(), 3);
    }

    #[test]
    fn kinds_by_name() {
        for k in NameKind::ALL {
            assert_eq!(NameKind::from_name(k.as_str()), Some(k));
        }
        assert_eq!(NameKind::from_name("techs"), None);
    }
}

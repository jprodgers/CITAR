//! Layer 1: the unique language, compiled once at load and evaluated without string compares.
//!
//! Unique types, the compiler, filters, conditionals, countables, triggers and the unique
//! indexes (DESIGN.md 5, packages 1a-05 to 1a-07). It shares layer 1 with [`crate::rules`].
//!
//! Replaces `citar/engine/unique_types.py`, `uniques.py:28-1083`, `economy.py:64-147`,
//! `triggers.py:13-74` and `cities.py:1169-1193`.
//!
//! - [`generated`] (`gen.rs`): written by `cargo xtask gen-uniques` from `unique_types.tsv` and
//!   `unique_supported.toml`: [`UniqueType`], its [`TypeInfo`], [`ParamKind`], the payloads,
//!   and [`UniqueData`], [`CondData`], [`TriggerCond`] and [`ModifierData`];
//! - [`text`]: taking a unique's text apart;
//! - [`params`]: compiling parameters by kind;
//! - [`countable`]: the countables a unique compares;
//! - `compile`: the compiler, run by the ruleset loader;
//! - [`table`]: what it produces, [`UniqueTable`] and each source's [`SourceUniques`];
//! - [`filter`]: the filters, compiled to sets and trees (package 1a-06);
//! - [`world`]: the facts a filter reads of a world, [`TileFacts`] and [`FilterFacts`].
//!
//! Packages 1a-05 and 1a-06 compile; evaluation (1a-07) comes next.

pub mod countable;
pub mod filter;
#[rustfmt::skip]
#[path = "gen.rs"]
pub mod generated;
pub mod params;
pub mod table;
pub mod text;
pub mod world;

pub(crate) mod compile;

pub use self::filter::{
    CityLeaf, CivLeaf, Combatant, CombatantFilter, Expr, Filters, GenFilter, TileFilter, TileLeaf,
    UnitLeaf, UnitScope,
};
pub use self::generated::{
    BY_PLACEHOLDER, CondData, ModifierData, ParamKind, Stage, Support, TYPE_INFO, TriggerCond,
    TypeInfo, UniqueData, UniqueType,
};
pub use self::table::{
    ActionMods, Cond, CondDeps, CondSpan, ObjectFilter, Role, Source, SourceUniques, StaticDomain,
    StaticFilter, StaticId, UFlags, Unique, UniqueMeta, UniqueTable,
};
pub use self::world::{FilterFacts, TileFacts};

impl UniqueType {
    /// The type whose placeholder is `placeholder`, exactly: `[]% Strength` gives `Strength`.
    #[must_use]
    pub fn from_placeholder(placeholder: &str) -> Option<Self> {
        BY_PLACEHOLDER
            .binary_search_by(|(p, _)| p.as_bytes().cmp(placeholder.as_bytes()))
            .ok()
            .map(|i| BY_PLACEHOLDER[i].1)
    }

    /// What the engine knows about the type.
    #[must_use]
    #[inline]
    pub const fn info(self) -> &'static TypeInfo {
        &TYPE_INFO[self as usize]
    }

    /// The type's name: `StatsFromTiles`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.info().name
    }

    /// The type's placeholder: `[] from [] tiles []`.
    #[must_use]
    pub const fn placeholder(self) -> &'static str {
        self.info().placeholder
    }

    /// The type's role, or `None` if the engine does not support it.
    #[must_use]
    pub const fn role(self) -> Option<Role> {
        match self.info().support {
            Some(s) => Some(s.role),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_are_found_by_placeholder() {
        assert_eq!(UniqueType::from_placeholder("[]% Strength"), Some(UniqueType::Strength));
        assert_eq!(UniqueType::from_placeholder("[]"), Some(UniqueType::Stats));
        assert_eq!(UniqueType::from_placeholder("Aircraft"), None);
        for ty in UniqueType::ALL {
            assert_eq!(UniqueType::from_placeholder(ty.placeholder()), Some(ty), "{}", ty.name());
            assert_eq!(TYPE_INFO[ty as usize].name, ty.name());
        }
        assert!(BY_PLACEHOLDER.windows(2).all(|w| w[0].0.as_bytes() < w[1].0.as_bytes()));
    }

    #[test]
    fn the_supported_types_match_the_census() {
        // DESIGN.md 5.1 and unique_supported.toml: the 402 types the ruleset uses (342 as
        // uniques, 60 as modifiers), and the 21 triggers the engine fires that it does not use.
        let supported: Vec<UniqueType> =
            UniqueType::ALL.into_iter().filter(|t| t.role().is_some()).collect();
        assert_eq!(UniqueType::COUNT, 637);
        assert_eq!(supported.len(), 423);
        let modifiers = supported
            .iter()
            .filter(|t| {
                matches!(t.role(), Some(Role::Cond | Role::Trigger | Role::ActionMod | Role::Meta))
            })
            .count();
        assert_eq!(modifiers, 60 + 21);
        for t in supported {
            let s = t.info().support.expect("supported");
            assert_eq!(s.params.len(), s.fields.len(), "{}", t.name());
            if s.role == Role::Inert {
                assert!(s.reason.is_some() && s.stages.is_empty(), "{}", t.name());
            }
        }
    }
}

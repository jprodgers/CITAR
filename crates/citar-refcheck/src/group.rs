//! The refcheck groups, in dependency order (DESIGN.md 9.2).
//!
//! Eleven groups were recorded by `scripts/refcheck/queries.py` (`GROUPS`); three are synthetic,
//! derived by their answer modules rather than read from a fixture's `queries`. Reports list the
//! groups in this order, so the first difference a reader meets is the one most likely to cause
//! the others: a wrong unique text shows up again in every yield and stat built on it.

use std::fmt;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// Rust compiles every unique text in `refcheck/uniques.json.gz` (package 1a-05).
    Uniques,
    /// A projection of the fixture's own state, read back from the converted game (1a-10).
    StateEcho,
    /// The settle on load changes neither explored tiles nor who has met whom (1c-01).
    FixedPoint,
    TileYields,
    CityStats,
    Civs,
    Buildable,
    Movement,
    Visible,
    CombatPreviews,
    DealChecks,
    ToolErrors,
    Views,
    Briefing,
}

/// What a group is compared once per: each fixture, or the whole run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Fixture,
    /// Compared once, not once per fixture: the ruleset's unique texts are the same in all of
    /// them. Reports and `cases` globs name this subject [`Group::RUN_CASE`].
    Run,
}

impl Group {
    /// Every group, in dependency order.
    pub const ALL: [Group; 14] = [
        Group::Uniques,
        Group::StateEcho,
        Group::FixedPoint,
        Group::TileYields,
        Group::CityStats,
        Group::Civs,
        Group::Buildable,
        Group::Movement,
        Group::Visible,
        Group::CombatPreviews,
        Group::DealChecks,
        Group::ToolErrors,
        Group::Views,
        Group::Briefing,
    ];

    /// The case name a run-scope group reports under.
    pub const RUN_CASE: &'static str = "ruleset";

    /// The name used in fixtures (`meta.query_order`), `intended.toml`, `enforced.toml`,
    /// `ratchet.json` and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Group::Uniques => "uniques",
            Group::StateEcho => "state_echo",
            Group::FixedPoint => "fixed_point",
            Group::TileYields => "tile_yields",
            Group::CityStats => "city_stats",
            Group::Civs => "civs",
            Group::Buildable => "buildable",
            Group::Movement => "movement",
            Group::Visible => "visible",
            Group::CombatPreviews => "combat_previews",
            Group::DealChecks => "deal_checks",
            Group::ToolErrors => "tool_errors",
            Group::Views => "views",
            Group::Briefing => "briefing",
        }
    }

    pub fn from_name(name: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|g| g.name() == name)
    }

    /// Like [`Group::from_name`], with an error that lists the names there are.
    pub fn parse(name: &str) -> Result<Group> {
        Group::from_name(name).ok_or_else(|| {
            let names: Vec<&str> = Group::ALL.iter().map(|g| g.name()).collect();
            Error::new(format!("unknown group `{name}` (the groups are {})", names.join(", ")))
        })
    }

    /// Whether the Python answer was recorded in each fixture's `queries`, rather than derived
    /// by the answer module.
    pub fn is_recorded(self) -> bool {
        !matches!(self, Group::Uniques | Group::StateEcho | Group::FixedPoint)
    }

    pub fn scope(self) -> Scope {
        match self {
            Group::Uniques => Scope::Run,
            _ => Scope::Fixture,
        }
    }

    /// The groups whose answers are recorded, in `queries.py`'s order.
    pub fn recorded() -> impl Iterator<Item = Group> {
        Group::ALL.into_iter().filter(|g| g.is_recorded())
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_follow_dependency_order() {
        for (i, g) in Group::ALL.into_iter().enumerate() {
            assert_eq!(Group::from_name(g.name()), Some(g));
            if i > 0 {
                assert!(Group::ALL[i - 1] < g, "ALL is in declaration order");
            }
        }
        assert!(Group::parse("combat").unwrap_err().message().contains("combat_previews"));
    }

    #[test]
    fn recorded_groups_match_the_recorder() {
        // scripts/refcheck/queries.py GROUPS, in order.
        let recorded: Vec<&str> = Group::recorded().map(Group::name).collect();
        assert_eq!(
            recorded,
            [
                "tile_yields",
                "city_stats",
                "civs",
                "buildable",
                "movement",
                "visible",
                "combat_previews",
                "deal_checks",
                "tool_errors",
                "views",
                "briefing"
            ]
        );
    }
}

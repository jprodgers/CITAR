//! `refcheck/ratchet.json`: the counts per group, which may only fall (DESIGN.md 9.2,
//! "Enforcement").
//!
//! Enforced groups must be clean; the ratchet protects the rest. It holds, for every compared
//! group over its own fixture sets (the committed ones), two counts:
//!
//! - `unexplained`: unexplained differences found by comparing;
//! - `failed`: subjects whose answer module failed or panicked. They are counted apart because
//!   one failure replaces all of a fixture's differences, so the first count would fall.
//!
//! `cargo refcheck ratchet` fails when a count rises, when a recorded group is no longer
//! compared, and also when the file is out of date: a count that fell, or a group compared for
//! the first time, is recorded with `--update` and committed, so the file always holds the
//! current counts and a later rise has no slack to hide in. `--update` refuses a rise: a rise is
//! fixed or explained in `intended.toml`, never recorded. The file is committed, so a missing
//! one is an error, and only `--update` writes a new one.
//!
//! ```json
//! {
//!   "fixtures": ["refcheck/fixtures-mini", "refcheck/fixtures-late"],
//!   "unexplained": {"tile_yields": 12, "city_stats": 40},
//!   "failed": {"city_stats": 1}
//! }
//! ```
//!
//! `unexplained` names every recorded group; `failed` only those with failures, the others
//! having none.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Error, Group, Result};

/// The committed fixture sets, which the ratchet counts over by default.
pub const DEFAULT_FIXTURES: [&str; 2] = ["refcheck/fixtures-mini", "refcheck/fixtures-late"];

/// One group's counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Count {
    /// Unexplained differences found by comparing, failures not included.
    pub unexplained: u64,
    /// Subjects (a group on a fixture) whose answer module failed or panicked.
    pub failed: u64,
}

/// Which count of a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Unexplained,
    Failed,
}

impl Measure {
    pub fn name(self) -> &'static str {
        match self {
            Measure::Unexplained => "unexplained",
            Measure::Failed => "failed",
        }
    }

    fn of(self, c: Count) -> u64 {
        match self {
            Measure::Unexplained => c.unexplained,
            Measure::Failed => c.failed,
        }
    }
}

/// One count that moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub group: Group,
    pub measure: Measure,
    pub was: u64,
    pub now: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ratchet {
    /// The fixture sets the counts are over, relative to the repository root.
    pub fixtures: Vec<String>,
    /// Every recorded group, in dependency order.
    pub counts: Vec<(Group, Count)>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    fixtures: Vec<String>,
    unexplained: Map<String, Value>,
    failed: Map<String, Value>,
}

/// How a run's counts compare with the ratchet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Counts that rose. Refused, with `--update` too.
    pub rises: Vec<Change>,
    /// Groups the ratchet records but the run did not compare. Refused too: a group that lost
    /// its answer module would otherwise drop out unnoticed.
    pub missing: Vec<Group>,
    /// Counts that fell. The file is out of date until `--update` records them.
    pub falls: Vec<Change>,
    /// Groups compared for the first time, with their counts. Out of date in the same way.
    pub new: Vec<(Group, Count)>,
}

impl Verdict {
    /// A rise, or a recorded group no longer compared: never recorded.
    pub fn refused(&self) -> bool {
        !self.rises.is_empty() || !self.missing.is_empty()
    }

    /// Counts that changed without rising: `--update` records them.
    pub fn out_of_date(&self) -> bool {
        !self.falls.is_empty() || !self.new.is_empty()
    }

    /// Whether `cargo refcheck ratchet`, without `--update`, fails.
    pub fn fails(&self) -> bool {
        self.refused() || self.out_of_date()
    }
}

impl Default for Ratchet {
    /// An empty ratchet over the committed fixtures: what `--update` starts a new file from.
    fn default() -> Self {
        Ratchet {
            fixtures: DEFAULT_FIXTURES.iter().map(|s| s.to_string()).collect(),
            counts: Vec::new(),
        }
    }
}

impl Ratchet {
    /// Loads the committed ratchet. A missing file is an error: deleting it must not pass.
    pub fn load(path: &Path) -> Result<Ratchet> {
        if !path.exists() {
            return Err(Error::new(format!(
                "{} does not exist; `cargo refcheck ratchet --update` writes it, to be committed",
                path.display()
            )));
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        Ratchet::parse(&text).map_err(|e| e.context(path.display()))
    }

    pub fn parse(text: &str) -> Result<Ratchet> {
        let raw: RawFile = serde_json::from_str(text).map_err(|e| Error::new(e.to_string()))?;
        if raw.fixtures.is_empty() {
            return Err(Error::new("`fixtures` must name at least one fixture set"));
        }
        let number = |name: &str, count: &Value| {
            count
                .as_u64()
                .ok_or_else(|| Error::new(format!("{name}: the count must be a whole number")))
        };
        let mut counts: Vec<(Group, Count)> = Vec::new();
        for (name, count) in &raw.unexplained {
            let c = Count { unexplained: number(name, count)?, failed: 0 };
            counts.push((Group::parse(name)?, c));
        }
        for (name, count) in &raw.failed {
            let group = Group::parse(name)?;
            let Some((_, c)) = counts.iter_mut().find(|(g, _)| *g == group) else {
                return Err(Error::new(format!(
                    "failed: {name} is not recorded under `unexplained`"
                )));
            };
            c.failed = number(name, count)?;
        }
        counts.sort_by_key(|(g, _)| *g);
        Ok(Ratchet { fixtures: raw.fixtures, counts })
    }

    /// The file's text: stable, in dependency order, with a final newline.
    pub fn to_text(&self) -> String {
        let mut sorted = self.counts.clone();
        sorted.sort_by_key(|(g, _)| *g);
        let mut unexplained = Map::new();
        let mut failed = Map::new();
        for (g, c) in &sorted {
            unexplained.insert(g.name().to_string(), Value::from(c.unexplained));
            if c.failed > 0 {
                failed.insert(g.name().to_string(), Value::from(c.failed));
            }
        }
        let raw = RawFile { fixtures: self.fixtures.clone(), unexplained, failed };
        let mut text = serde_json::to_string_pretty(&raw).unwrap_or_default();
        text.push('\n');
        text
    }

    pub fn recorded(&self, group: Group) -> Option<Count> {
        self.counts.iter().find(|(g, _)| *g == group).map(|(_, c)| *c)
    }

    /// Compares a run's counts (every compared group, in any order) with the ratchet.
    pub fn check(&self, counts: &[(Group, Count)]) -> Verdict {
        let mut v = Verdict::default();
        let mut counts = counts.to_vec();
        counts.sort_by_key(|(g, _)| *g);
        for &(group, now) in &counts {
            let Some(was) = self.recorded(group) else {
                v.new.push((group, now));
                continue;
            };
            for measure in [Measure::Unexplained, Measure::Failed] {
                let change = Change { group, measure, was: measure.of(was), now: measure.of(now) };
                if change.now > change.was {
                    v.rises.push(change);
                } else if change.now < change.was {
                    v.falls.push(change);
                }
            }
        }
        for &(g, _) in &self.counts {
            if !counts.iter().any(|(c, _)| *c == g) {
                v.missing.push(g);
            }
        }
        v
    }

    /// The ratchet with a run's counts recorded, or the verdict that refuses it.
    pub fn updated(&self, counts: &[(Group, Count)]) -> std::result::Result<Ratchet, Verdict> {
        let v = self.check(counts);
        if v.refused() {
            return Err(v);
        }
        let mut counts = counts.to_vec();
        counts.sort_by_key(|(g, _)| *g);
        Ok(Ratchet { fixtures: self.fixtures.clone(), counts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(unexplained: u64) -> Count {
        Count { unexplained, failed: 0 }
    }

    fn ratchet(counts: &[(Group, Count)]) -> Ratchet {
        Ratchet { counts: counts.to_vec(), ..Ratchet::default() }
    }

    fn change(group: Group, measure: Measure, was: u64, now: u64) -> Change {
        Change { group, measure, was, now }
    }

    #[test]
    fn counts_may_only_fall_and_every_change_is_recorded() {
        let r = ratchet(&[(Group::TileYields, n(10)), (Group::Civs, n(5))]);
        assert_eq!(r.check(&[(Group::Civs, n(5)), (Group::TileYields, n(10))]), Verdict::default());

        let fall = r.check(&[(Group::Civs, n(5)), (Group::TileYields, n(7))]);
        assert!(!fall.refused());
        assert!(fall.fails(), "a fall not yet recorded leaves slack for a later rise");
        assert_eq!(fall.falls, [change(Group::TileYields, Measure::Unexplained, 10, 7)]);

        let rise = r.check(&[(Group::TileYields, n(11)), (Group::Civs, n(5))]);
        assert!(rise.refused() && rise.fails());
        assert_eq!(rise.rises, [change(Group::TileYields, Measure::Unexplained, 10, 11)]);

        let gone = r.check(&[(Group::TileYields, n(10))]);
        assert!(gone.refused());
        assert_eq!(gone.missing, [Group::Civs]);

        let first =
            r.check(&[(Group::TileYields, n(10)), (Group::Civs, n(5)), (Group::Views, n(0))]);
        assert!(!first.refused());
        assert!(first.fails(), "a group compared for the first time must be recorded");
        assert_eq!(first.new, [(Group::Views, n(0))]);
    }

    #[test]
    fn failures_are_counted_apart_and_may_not_rise() {
        let r = ratchet(&[(Group::TileYields, n(40))]);
        // One failed fixture takes its differences with it: the plain count falls, yet it fails.
        let failing = [(Group::TileYields, Count { unexplained: 25, failed: 1 })];
        let v = r.check(&failing);
        assert!(v.refused());
        assert_eq!(v.rises, [change(Group::TileYields, Measure::Failed, 0, 1)]);
        assert_eq!(v.falls, [change(Group::TileYields, Measure::Unexplained, 40, 25)]);
        assert!(r.updated(&failing).is_err());
    }

    #[test]
    fn update_records_falls_and_new_groups_and_refuses_a_rise() {
        let r = ratchet(&[(Group::TileYields, n(10))]);
        let next = r.updated(&[(Group::TileYields, n(4)), (Group::CityStats, n(30))]).unwrap();
        assert_eq!(next.counts, [(Group::TileYields, n(4)), (Group::CityStats, n(30))]);
        assert!(!next.check(&next.counts).fails(), "recorded, the file is up to date");
        let refused =
            next.updated(&[(Group::TileYields, n(5)), (Group::CityStats, n(30))]).unwrap_err();
        assert_eq!(refused.rises, [change(Group::TileYields, Measure::Unexplained, 4, 5)]);
    }

    #[test]
    fn the_file_round_trips_in_dependency_order() {
        let r = ratchet(&[
            (Group::Views, n(2)),
            (Group::TileYields, Count { unexplained: 1, failed: 3 }),
        ]);
        let text = r.to_text();
        assert!(text.find("tile_yields").unwrap() < text.find("views").unwrap(), "{text}");
        let back = Ratchet::parse(&text).unwrap();
        assert_eq!(
            back.counts,
            [(Group::TileYields, Count { unexplained: 1, failed: 3 }), (Group::Views, n(2))]
        );
        assert_eq!(back.to_text(), text);
        let empty = r#"{"fixtures": ["x"], "unexplained": {}, "failed": {}}"#;
        assert!(Ratchet::parse(empty).unwrap().counts.is_empty());
        for bad in [
            r#"{"fixtures": ["x"], "unexplained": {"nope": 1}, "failed": {}}"#,
            r#"{"fixtures": ["x"], "unexplained": {"civs": -1}, "failed": {}}"#,
            r#"{"fixtures": [], "unexplained": {}, "failed": {}}"#,
            r#"{"fixtures": ["x"], "unexplained": {}, "failed": {}, "extra": 1}"#,
            r#"{"fixtures": ["x"], "unexplained": {}}"#,
            r#"{"fixtures": ["x"], "unexplained": {}, "failed": {"civs": 1}}"#,
        ] {
            assert!(Ratchet::parse(bad).is_err(), "should refuse: {bad}");
        }
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let e = Ratchet::load(Path::new("no/such/ratchet.json")).unwrap_err();
        assert!(e.message().contains("--update"), "{e}");
    }
}

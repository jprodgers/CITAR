//! `refcheck/ratchet.json`: unexplained differences per group, which may only fall
//! (DESIGN.md 9.2, "Enforcement").
//!
//! Enforced groups must be clean; the ratchet protects the rest. It holds the unexplained count
//! of every compared group over its own fixture sets (the committed ones), and `cargo refcheck
//! ratchet` fails when any count rises. `--update` records falls and newly compared groups, and
//! refuses a rise: a rise is fixed or explained in `intended.toml`, never recorded.
//!
//! ```json
//! {
//!   "fixtures": ["refcheck/fixtures-mini", "refcheck/fixtures-late"],
//!   "unexplained": {"tile_yields": 12, "city_stats": 40}
//! }
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Error, Group, Result};

/// The committed fixture sets, which the ratchet counts over by default.
pub const DEFAULT_FIXTURES: [&str; 2] = ["refcheck/fixtures-mini", "refcheck/fixtures-late"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ratchet {
    /// The fixture sets the counts are over, relative to the repository root.
    pub fixtures: Vec<String>,
    /// Unexplained differences per group, in dependency order.
    pub unexplained: Vec<(Group, u64)>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    fixtures: Vec<String>,
    unexplained: Map<String, Value>,
}

/// How a run's counts compare with the ratchet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Groups whose count rose: (group, recorded, now). Any of these fails.
    pub rises: Vec<(Group, u64, u64)>,
    /// Groups the ratchet records but the run did not compare. Any of these fails too: a group
    /// that lost its answer module would otherwise drop out unnoticed.
    pub missing: Vec<Group>,
    /// Groups whose count fell: (group, recorded, now). `--update` records them.
    pub falls: Vec<(Group, u64, u64)>,
    /// Groups compared for the first time: (group, now). `--update` records them.
    pub new: Vec<(Group, u64)>,
}

impl Verdict {
    pub fn fails(&self) -> bool {
        !self.rises.is_empty() || !self.missing.is_empty()
    }

    pub fn changes(&self) -> bool {
        !self.falls.is_empty() || !self.new.is_empty()
    }
}

impl Default for Ratchet {
    fn default() -> Self {
        Ratchet {
            fixtures: DEFAULT_FIXTURES.iter().map(|s| s.to_string()).collect(),
            unexplained: Vec::new(),
        }
    }
}

impl Ratchet {
    /// Loads the ratchet; a missing file is an empty ratchet over the committed fixtures.
    pub fn load(path: &Path) -> Result<Ratchet> {
        if !path.exists() {
            return Ok(Ratchet::default());
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
        let mut unexplained = Vec::new();
        for (name, count) in &raw.unexplained {
            let group = Group::parse(name)?;
            let count = count
                .as_u64()
                .ok_or_else(|| Error::new(format!("{name}: the count must be a whole number")))?;
            unexplained.push((group, count));
        }
        unexplained.sort();
        Ok(Ratchet { fixtures: raw.fixtures, unexplained })
    }

    /// The file's text: stable, in dependency order, with a final newline.
    pub fn to_text(&self) -> String {
        let mut sorted = self.unexplained.clone();
        sorted.sort();
        let mut counts = Map::new();
        for (g, n) in &sorted {
            counts.insert(g.name().to_string(), Value::from(*n));
        }
        let raw = RawFile { fixtures: self.fixtures.clone(), unexplained: counts };
        let mut text = serde_json::to_string_pretty(&raw).unwrap_or_default();
        text.push('\n');
        text
    }

    fn recorded(&self, group: Group) -> Option<u64> {
        self.unexplained.iter().find(|(g, _)| *g == group).map(|(_, n)| *n)
    }

    /// Compares a run's counts (every compared group, in any order) with the ratchet.
    pub fn check(&self, counts: &[(Group, u64)]) -> Verdict {
        let mut v = Verdict::default();
        let mut counts = counts.to_vec();
        counts.sort();
        for &(g, now) in &counts {
            match self.recorded(g) {
                Some(was) if now > was => v.rises.push((g, was, now)),
                Some(was) if now < was => v.falls.push((g, was, now)),
                Some(_) => {}
                None => v.new.push((g, now)),
            }
        }
        for &(g, _) in &self.unexplained {
            if !counts.iter().any(|(c, _)| *c == g) {
                v.missing.push(g);
            }
        }
        v
    }

    /// The ratchet with a run's counts recorded, or the verdict that refuses it.
    pub fn updated(&self, counts: &[(Group, u64)]) -> std::result::Result<Ratchet, Verdict> {
        let v = self.check(counts);
        if v.fails() {
            return Err(v);
        }
        let mut unexplained = counts.to_vec();
        unexplained.sort();
        Ok(Ratchet { fixtures: self.fixtures.clone(), unexplained })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ratchet(counts: &[(Group, u64)]) -> Ratchet {
        Ratchet { unexplained: counts.to_vec(), ..Ratchet::default() }
    }

    #[test]
    fn counts_may_only_fall() {
        let r = ratchet(&[(Group::TileYields, 10), (Group::Civs, 5)]);
        let fine = r.check(&[(Group::Civs, 5), (Group::TileYields, 7)]);
        assert!(!fine.fails());
        assert_eq!(fine.falls, [(Group::TileYields, 10, 7)]);

        let rise = r.check(&[(Group::TileYields, 11), (Group::Civs, 5)]);
        assert!(rise.fails());
        assert_eq!(rise.rises, [(Group::TileYields, 10, 11)]);

        let gone = r.check(&[(Group::TileYields, 1)]);
        assert!(gone.fails());
        assert_eq!(gone.missing, [Group::Civs]);
    }

    #[test]
    fn update_records_falls_and_new_groups_and_refuses_a_rise() {
        let r = ratchet(&[(Group::TileYields, 10)]);
        let next = r.updated(&[(Group::TileYields, 4), (Group::CityStats, 30)]).unwrap();
        assert_eq!(next.unexplained, [(Group::TileYields, 4), (Group::CityStats, 30)]);
        let refused = next.updated(&[(Group::TileYields, 5), (Group::CityStats, 30)]).unwrap_err();
        assert_eq!(refused.rises, [(Group::TileYields, 4, 5)]);
    }

    #[test]
    fn the_file_round_trips_in_dependency_order() {
        let r = ratchet(&[(Group::Views, 2), (Group::TileYields, 1)]);
        let text = r.to_text();
        assert!(text.find("tile_yields").unwrap() < text.find("views").unwrap(), "{text}");
        let back = Ratchet::parse(&text).unwrap();
        assert_eq!(back.unexplained, [(Group::TileYields, 1), (Group::Views, 2)]);
        assert_eq!(back.to_text(), Ratchet::parse(&back.to_text()).unwrap().to_text());
        assert!(Ratchet::parse(r#"{"fixtures": ["x"], "unexplained": {"nope": 1}}"#).is_err());
        assert!(Ratchet::parse(r#"{"fixtures": ["x"], "unexplained": {"civs": -1}}"#).is_err());
        assert!(Ratchet::parse(r#"{"fixtures": [], "unexplained": {}}"#).is_err());
        assert!(Ratchet::parse(r#"{"fixtures": ["x"], "unexplained": {}, "extra": 1}"#).is_err());
    }
}

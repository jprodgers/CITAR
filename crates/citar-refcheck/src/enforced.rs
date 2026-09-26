//! `refcheck/enforced.toml`: the groups whose unexplained differences fail a run
//! (DESIGN.md 9.2, "Enforcement").
//!
//! ```toml
//! [[enforce]]
//! group = "tile_yields"              # the whole group
//!
//! [[enforce]]
//! group = "civs"                     # only these paths of it
//! paths = ["civs[*].resource_supply.**", "civs[*].unit_supply"]
//! ```
//!
//! Paths are patterns in the path grammar, matched against the whole path of a difference, so a
//! subtree is `prefix.**`. Every system package adds its groups or paths here once its answer
//! module is clean (DESIGN.md 3.4). A group listed here must have an answer module, or the run
//! stops with a configuration error: enforcing what is never compared would pass silently.
//!
//! A difference above the enforced places counts too, because nothing below it was compared. So
//! with `civs[*].unit_supply` enforced, the run also fails on
//!
//! - an answer module that failed or panicked (an `error`, at the root);
//! - a civilization missing or extra (`civs[pid=0]`), or a value of the wrong type above
//!   `unit_supply`, when that value holds an enforced place;
//! - `civs` itself when it could not be keyed (a `key` difference), since its elements were then
//!   compared in order and their paths no longer carry `[pid=...]`.
//!
//! A missing value that holds no enforced place, such as a missing `civs[pid=0].era`, stays
//! outside.

use std::path::Path as FsPath;

use serde::Deserialize;

use crate::compare::path::{accepted, matches_inside, walk};
use crate::compare::{Diff, DiffKind, Pattern};
use crate::{Error, Group, Result};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    enforce: Vec<RawEnforce>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnforce {
    group: String,
    paths: Option<Vec<String>>,
}

/// One enforced group, whole or narrowed to some paths.
#[derive(Debug, Clone)]
pub struct Enforce {
    pub group: Group,
    /// `None` for the whole group.
    pub paths: Option<Vec<Pattern>>,
}

#[derive(Debug, Clone, Default)]
pub struct Enforced {
    entries: Vec<Enforce>,
}

impl Enforced {
    pub fn load(path: &FsPath) -> Result<Enforced> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        Enforced::parse(&text).map_err(|e| e.context(path.display()))
    }

    pub fn parse(text: &str) -> Result<Enforced> {
        let raw: RawFile =
            toml::from_str(text).map_err(|e| Error::new(e.to_string().trim_end().to_string()))?;
        let mut entries: Vec<Enforce> = Vec::new();
        for r in raw.enforce {
            let group = Group::parse(&r.group)?;
            if entries.iter().any(|e| e.group == group) {
                return Err(Error::new(format!(
                    "{group} is listed twice; give one entry all its paths"
                )));
            }
            let paths = match r.paths {
                None => None,
                Some(p) if p.is_empty() => {
                    return Err(Error::new(format!(
                        "{group}: leave `paths` out to enforce the whole group"
                    )));
                }
                Some(p) => Some(
                    p.iter()
                        .map(|s| Pattern::parse(s).map_err(|e| e.context(group)))
                        .collect::<Result<Vec<_>>>()?,
                ),
            };
            entries.push(Enforce { group, paths });
        }
        Ok(Enforced { entries })
    }

    pub fn entries(&self) -> &[Enforce] {
        &self.entries
    }

    /// Whether the group is enforced at all, whole or in part.
    pub fn has_group(&self, group: Group) -> bool {
        self.entries.iter().any(|e| e.group == group)
    }

    /// Whether this difference, unexplained, fails the run: it lies at an enforced place, or
    /// above one that it hides (see the module's documentation).
    pub fn covers(&self, group: Group, diff: &Diff) -> bool {
        // Entries are one per group (`parse` refuses a second), so their paths are one set.
        let Some(entry) = self.entries.iter().find(|e| e.group == group) else { return false };
        let Some(patterns) = &entry.paths else { return true };
        if diff.kind == DiffKind::Error {
            return true;
        }
        let cursor = walk(patterns, &diff.path);
        if cursor.is_empty() {
            return false;
        }
        if accepted(patterns, &cursor).next().is_some() {
            return true;
        }
        match diff.kind {
            DiffKind::Key => true,
            _ => [&diff.python, &diff.rust]
                .into_iter()
                .flatten()
                .any(|v| matches_inside(patterns, &cursor, v)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{Path, Scalar, Seg};
    use serde_json::{Value, json};

    fn diff(segs: Vec<Seg>, kind: DiffKind, python: Option<Value>, rust: Option<Value>) -> Diff {
        Diff { path: Path(segs), kind, python, rust, detail: None }
    }

    fn number_at(segs: Vec<Seg>) -> Diff {
        diff(segs, DiffKind::Number, Some(json!(1)), Some(json!(2)))
    }

    fn key(k: &str) -> Seg {
        Seg::Key(k.into())
    }

    fn pid(n: i64) -> Seg {
        Seg::Field("pid".into(), Scalar::Num(n.into()))
    }

    const FILE: &str = r#"
[[enforce]]
group = "tile_yields"

[[enforce]]
group = "civs"
paths = ["civs[*].resource_supply.**", "civs[*].unit_supply"]
"#;

    #[test]
    fn whole_groups_and_path_globs() {
        let e = Enforced::parse(FILE).unwrap();
        let anywhere = number_at(vec![key("owned"), Seg::Index(3)]);
        assert!(e.covers(Group::TileYields, &anywhere));
        let at = |k: &str| number_at(vec![key("civs"), pid(0), key(k)]);
        assert!(e.covers(Group::Civs, &at("resource_supply")));
        assert!(e.covers(Group::Civs, &at("unit_supply")));
        assert!(!e.covers(Group::Civs, &at("happiness")));
        let deep = number_at(vec![key("civs"), pid(0), key("resource_supply"), key("Iron")]);
        assert!(e.covers(Group::Civs, &deep));
        assert!(!e.covers(Group::Views, &anywhere));
        assert!(e.has_group(Group::Civs) && !e.has_group(Group::Views));
    }

    #[test]
    fn a_difference_above_an_enforced_place_is_enforced_when_it_hides_one() {
        let e = Enforced::parse(FILE).unwrap();
        let civ = json!({"pid": 0, "era": 2, "unit_supply": 5, "resource_supply": {"Iron": 1}});
        let covers = |segs: Vec<Seg>, kind, python: Option<&Value>, rust: Option<&Value>| {
            e.covers(Group::Civs, &diff(segs, kind, python.cloned(), rust.cloned()))
        };
        // An answer module that failed hides everything.
        assert!(e.covers(Group::Civs, &Diff::error("the Rust answer panicked")));
        // A whole civilization, missing or extra, holds enforced places.
        let one = || vec![key("civs"), pid(0)];
        assert!(covers(one(), DiffKind::Missing, Some(&civ), None));
        assert!(covers(one(), DiffKind::Extra, None, Some(&civ)));
        // So does an answer of the wrong type, or one without `civs`.
        let answer = json!({"civs": [civ.clone()]});
        assert!(covers(Vec::new(), DiffKind::Type, Some(&answer), Some(&json!(null))));
        assert!(covers(vec![key("civs")], DiffKind::Missing, Some(&answer["civs"]), None));
        // A list that could not be keyed was compared in order, so its `[pid=0]` paths are gone.
        assert!(covers(vec![key("civs")], DiffKind::Key, None, None));
        // A missing value that holds no enforced place is outside, and so is a subtree elsewhere.
        let era = vec![key("civs"), pid(0), key("era")];
        assert!(!covers(era, DiffKind::Missing, Some(&json!(2)), None));
        let elsewhere = json!({"unit_supply": 1, "resource_supply": {"Iron": 1}});
        assert!(!covers(vec![key("world")], DiffKind::Missing, Some(&elsewhere), None));
        // A civilization without the enforced keys holds no enforced place either.
        assert!(!covers(one(), DiffKind::Missing, Some(&json!({"pid": 0, "era": 2})), None));
        // A number above an enforced place has nothing below it on either side.
        assert!(!covers(one(), DiffKind::Number, Some(&json!(1)), Some(&json!(2))));
        // Nothing of a group that is not enforced.
        assert!(!e.covers(Group::Views, &Diff::error("x")));
    }

    #[test]
    fn bad_files_are_refused() {
        for bad in [
            "[[enforce]]\ngroup = \"nope\"\n",
            "[[enforce]]\ngroup = \"civs\"\n[[enforce]]\ngroup = \"civs\"\n",
            "[[enforce]]\ngroup = \"civs\"\npaths = []\n",
            "[[enforce]]\ngroup = \"civs\"\npaths = [\"a..b\"]\n",
            "[[enforce]]\ngroup = \"civs\"\nnote = \"x\"\n",
            "enforced = []\n",
        ] {
            assert!(Enforced::parse(bad).is_err(), "should refuse: {bad}");
        }
        assert!(Enforced::parse("# nothing enforced yet\n").unwrap().entries().is_empty());
    }
}

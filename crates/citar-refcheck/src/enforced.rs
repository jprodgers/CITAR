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

use std::path::Path as FsPath;

use serde::Deserialize;

use crate::compare::{Path, Pattern};
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

    /// Whether an unexplained difference at this place fails the run.
    pub fn covers(&self, group: Group, path: &Path) -> bool {
        self.entries.iter().any(|e| {
            e.group == group && e.paths.as_ref().is_none_or(|ps| ps.iter().any(|p| p.matches(path)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{Scalar, Seg};

    fn path(segs: Vec<Seg>) -> Path {
        Path(segs)
    }

    #[test]
    fn whole_groups_and_path_globs() {
        let e = Enforced::parse(
            r#"
[[enforce]]
group = "tile_yields"

[[enforce]]
group = "civs"
paths = ["civs[*].resource_supply.**", "civs[*].unit_supply"]
"#,
        )
        .unwrap();
        let anywhere = path(vec![Seg::Key("owned".into()), Seg::Index(3)]);
        assert!(e.covers(Group::TileYields, &anywhere));
        let pid = |k: &str| {
            path(vec![
                Seg::Key("civs".into()),
                Seg::Field("pid".into(), Scalar::Num(0.into())),
                Seg::Key(k.into()),
            ])
        };
        assert!(e.covers(Group::Civs, &pid("resource_supply")));
        assert!(e.covers(Group::Civs, &pid("unit_supply")));
        assert!(!e.covers(Group::Civs, &pid("happiness")));
        let mut deep = pid("resource_supply");
        deep.0.push(Seg::Key("Iron".into()));
        assert!(e.covers(Group::Civs, &deep));
        assert!(!e.covers(Group::Views, &anywhere));
        assert!(e.has_group(Group::Civs) && !e.has_group(Group::Views));
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

//! Fixtures: recorded Python states and their answers (refcheck/README.md, "The fixture format").
//!
//! A fixture set is a folder of `<case>/t<turn>.json.gz` files: `refcheck/fixtures-mini` and
//! `refcheck/fixtures-late` are committed, `refcheck/corpus` is generated. A fixture's name is
//! `<case>/t<turn>`; `cases` globs and reports use it. The late states are copies of corpus
//! states, so the same name can come from two sets, and results carry the set as well.
//!
//! Loading keeps the state as unparsed JSON text for the converter (package 1a-10), types the
//! meta block strictly, so a change to the recorder is noticed rather than half read, and keeps
//! the answers as JSON values.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::compare::Grid;
use crate::{Error, Group, Result};

/// A folder of fixtures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureSet {
    /// The folder's own name (`fixtures-mini`), which results carry.
    pub label: String,
    pub dir: PathBuf,
    /// The folder as reports print it: relative to the repository root when it lies inside.
    pub shown: String,
}

impl FixtureSet {
    pub fn new(dir: &Path, root: &Path) -> FixtureSet {
        let label = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        let shown = match (dir.canonicalize(), root.canonicalize()) {
            (Ok(d), Ok(r)) => match d.strip_prefix(&r) {
                Ok(rel) => slashes(rel),
                Err(_) => slashes(dir),
            },
            _ => slashes(dir),
        };
        FixtureSet { label, dir: dir.to_path_buf(), shown }
    }
}

/// A path with forward slashes, so reports read the same on every platform.
fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// A fixture file, found but not loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureRef {
    /// `<case>/t<turn>`.
    pub name: String,
    pub case: String,
    pub turn: u32,
    /// The index of its set in the run's list of sets.
    pub set: usize,
    pub path: PathBuf,
}

/// Every fixture in the sets, sorted by case, then turn (as a number), then set.
pub fn discover(sets: &[FixtureSet]) -> Result<Vec<FixtureRef>> {
    let mut out = Vec::new();
    for (set, fs) in sets.iter().enumerate() {
        let entries = std::fs::read_dir(&fs.dir).map_err(|e| {
            Error::new(format!("cannot read fixture folder {}: {e}", fs.dir.display()))
        })?;
        let mut found = 0;
        for entry in entries {
            let entry = entry.map_err(|e| Error::new(format!("{}: {e}", fs.dir.display())))?;
            let case_dir = entry.path();
            if !case_dir.is_dir() {
                continue;
            }
            let case = entry.file_name().to_string_lossy().into_owned();
            let files = std::fs::read_dir(&case_dir)
                .map_err(|e| Error::new(format!("cannot read {}: {e}", case_dir.display())))?;
            for file in files {
                let file = file.map_err(|e| Error::new(format!("{}: {e}", case_dir.display())))?;
                let Some(turn) = turn_of(&file.file_name().to_string_lossy()) else { continue };
                out.push(FixtureRef {
                    name: format!("{case}/t{turn}"),
                    case: case.clone(),
                    turn,
                    set,
                    path: file.path(),
                });
                found += 1;
            }
        }
        if found == 0 {
            return Err(Error::new(format!(
                "no fixtures (<case>/t<turn>.json.gz) in {}",
                fs.dir.display()
            )));
        }
    }
    out.sort_by(|a, b| (&a.case, a.turn, a.set).cmp(&(&b.case, b.turn, b.set)));
    Ok(out)
}

/// The turn of a file named `t<turn>.json.gz`.
fn turn_of(file_name: &str) -> Option<u32> {
    let digits = file_name.strip_prefix('t')?.strip_suffix(".json.gz")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The `meta` block (refcheck/README.md, "meta fields").
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub case: String,
    /// A hash of `citar/engine` and the ruleset at recording time.
    pub engine: String,
    /// A hash of `citar/bots/basic.py` at recording time.
    pub bot: String,
    pub seed: i64,
    pub config: RecordedConfig,
    pub turn: u32,
    /// The player whose turn it was.
    pub current: u32,
    pub checkpoints: Vec<u32>,
    pub query_order: Vec<String>,
    /// Per group, the top-level state fields answering changed in Python (information only).
    pub side_effects: Map<String, Value>,
    /// Groups whose Python answer raised: reported as `python-crashed`, never compared.
    pub query_crashes: Vec<String>,
    pub bot_errors: Vec<String>,
    /// The scenario steps, for scenario cases.
    pub setup: Option<Vec<Value>>,
    pub python: String,
    pub hash_seed: String,
}

/// The `Game.new` configuration the recorder played.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedConfig {
    pub map_type: String,
    pub map_size: String,
    pub seed: i64,
    pub barbarians: String,
    pub speed: String,
    pub difficulty: String,
    pub turn_limit: Option<u32>,
    pub players: Vec<RecordedSeat>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedSeat {
    pub controller: String,
    pub nation: Option<String>,
}

/// One loaded fixture.
#[derive(Debug)]
pub struct Fixture {
    pub name: String,
    /// The label of its set.
    pub set: String,
    pub meta: Meta,
    /// `GameState.to_dict()`, unparsed; the converter reads it (package 1a-10).
    pub state: Box<RawValue>,
    /// The recorded answers, by group name.
    pub queries: Map<String, Value>,
    /// The map's geometry, read from the state, for checking routes.
    pub grid: Grid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureFile {
    meta: Meta,
    state: Box<RawValue>,
    queries: Map<String, Value>,
}

#[derive(Deserialize)]
struct MetaOnly {
    meta: Meta,
    #[serde(rename = "state")]
    _state: IgnoredAny,
    #[serde(rename = "queries")]
    _queries: IgnoredAny,
}

/// The few state fields a route check needs.
#[derive(Deserialize)]
struct StateHead {
    width: u32,
    height: u32,
    config: StateConfig,
}

#[derive(Deserialize)]
struct StateConfig {
    #[serde(default)]
    wrap_x: bool,
    #[serde(default)]
    wrap_y: bool,
}

/// The bytes of a gzipped file.
pub(crate) fn read_gz(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|e| Error::new(e.to_string()))?;
    let mut bytes = Vec::new();
    GzDecoder::new(BufReader::new(file))
        .read_to_end(&mut bytes)
        .map_err(|e| Error::new(format!("not a gzip file: {e}")))?;
    Ok(bytes)
}

impl Fixture {
    /// Loads a fixture and checks it against its file name and the known groups.
    pub fn load(r: &FixtureRef, sets: &[FixtureSet]) -> Result<Fixture> {
        let at = |e: Error| e.context(r.path.display());
        let bytes = read_gz(&r.path).map_err(at)?;
        let file: FixtureFile = serde_json::from_slice(&bytes)
            .map_err(|e| at(Error::new(format!("bad fixture: {e}"))))?;
        check_meta(r, &file.meta).map_err(at)?;
        check_queries(&file.meta, &file.queries).map_err(at)?;
        let head: StateHead = serde_json::from_str(file.state.get())
            .map_err(|e| at(Error::new(format!("bad state: {e}"))))?;
        Ok(Fixture {
            name: r.name.clone(),
            set: sets.get(r.set).map(|s| s.label.clone()).unwrap_or_default(),
            meta: file.meta,
            state: file.state,
            queries: file.queries,
            grid: Grid::new(head.width, head.height, head.config.wrap_x, head.config.wrap_y),
        })
    }

    /// Loads only the meta block, for `list`.
    pub fn load_meta(r: &FixtureRef) -> Result<Meta> {
        let at = |e: Error| e.context(r.path.display());
        let bytes = read_gz(&r.path).map_err(at)?;
        let file: MetaOnly = serde_json::from_slice(&bytes)
            .map_err(|e| at(Error::new(format!("bad fixture: {e}"))))?;
        check_meta(r, &file.meta).map_err(at)?;
        Ok(file.meta)
    }

    /// The recorded Python answer of a group, unless the group crashed in Python.
    pub fn recorded(&self, group: Group) -> Option<&Value> {
        if self.python_crash(group).is_some() {
            return None;
        }
        self.queries.get(group.name())
    }

    /// The one-line error, if the group's Python answer raised while recording.
    pub fn python_crash(&self, group: Group) -> Option<String> {
        let answer = self.queries.get(group.name());
        if let Some(line) = answer.and_then(|a| a.get("crash")) {
            return Some(line.as_str().map_or_else(|| line.to_string(), str::to_string));
        }
        self.meta
            .query_crashes
            .iter()
            .any(|g| g == group.name())
            .then(|| "listed in meta.query_crashes".to_string())
    }
}

fn check_meta(r: &FixtureRef, meta: &Meta) -> Result<()> {
    if meta.case != r.case || meta.turn != r.turn {
        return Err(Error::new(format!(
            "the file is named for {} but its meta says {}/t{}",
            r.name, meta.case, meta.turn
        )));
    }
    for name in &meta.query_order {
        let group = Group::parse(name).map_err(|e| e.context("meta.query_order"))?;
        if !group.is_recorded() {
            return Err(Error::new(format!(
                "meta.query_order names {name}, which is not recorded"
            )));
        }
    }
    Ok(())
}

fn check_queries(meta: &Meta, queries: &Map<String, Value>) -> Result<()> {
    for name in &meta.query_order {
        if !queries.contains_key(name) {
            return Err(Error::new(format!("no recorded answer for {name}")));
        }
    }
    if let Some(extra) = queries.keys().find(|k| !meta.query_order.contains(k)) {
        return Err(Error::new(format!(
            "queries holds {extra}, which meta.query_order does not list"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_turn_files_are_fixtures() {
        assert_eq!(turn_of("t50.json.gz"), Some(50));
        assert_eq!(turn_of("t1.json.gz"), Some(1));
        assert_eq!(turn_of("t.json.gz"), None);
        assert_eq!(turn_of("t5.json"), None);
        assert_eq!(turn_of("x5.json.gz"), None);
        assert_eq!(turn_of("t-5.json.gz"), None);
    }
}

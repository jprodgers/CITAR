//! The conformance fixtures of protocol 2 (PROTOCOL.md, "The conformance fixtures"), read for
//! this crate's tests and for the players of the transcripts (4-03 on): the frames under
//! `protocol/v2/frames/`, the transcripts under `protocol/v2/transcripts/` ([`transcript`]),
//! their matcher language ([`matcher`]) and PROTOCOL.md's frame table ([`frame_table`]).
//!
//! The server reads the same files through `tests/helper_fixtures.py`.

pub mod matcher;
pub mod transcript;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::protocol::{ErrorCode, Sender};

/// This crate's directory, where PROTOCOL.md and the fixtures are.
#[must_use]
pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `protocol/v2/`.
#[must_use]
pub fn fixtures_dir() -> PathBuf {
    crate_dir().join("protocol").join("v2")
}

/// One frame fixture.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameFixture {
    /// Its file name without `.json`: `helper.hello.cpu-only`.
    pub name: String,
    /// Who sends it.
    pub from: Sender,
    pub kind: FixtureKind,
}

/// What a frame fixture says.
#[derive(Clone, Debug, PartialEq)]
pub enum FixtureKind {
    /// Parsed by both sides; what they serialize again equals `expect`.
    Valid { frame: Value, expect: Value },
    /// Refused by both sides with this code at this path.
    Invalid { text: String, code: ErrorCode, path: String },
    /// Ignored by both sides.
    Ignored { frame: Value },
}

impl FrameFixture {
    /// The frame's text as the fixture gives it.
    #[must_use]
    pub fn text(&self) -> String {
        match &self.kind {
            FixtureKind::Valid { frame, .. } | FixtureKind::Ignored { frame } => frame.to_string(),
            FixtureKind::Invalid { text, .. } => text.clone(),
        }
    }

    /// Whether this is a type's canonical example, `<from>.<type>.json`.
    #[must_use]
    pub fn is_canonical(&self) -> bool {
        matches!(self.kind, FixtureKind::Valid { .. }) && self.name.split('.').count() == 2
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    out.sort();
    Ok(out)
}

fn keys_only(map: &Map<String, Value>, allowed: &[&str], path: &Path) -> Result<(), String> {
    match map.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("{}: unknown key {k:?}", path.display())),
        None => Ok(()),
    }
}

/// Reads one frame fixture of the category `valid`, `invalid` or `ignored`.
///
/// # Errors
/// A file that is not a fixture of that category, naming it.
pub fn load_frame_fixture(category: &str, path: &Path) -> Result<FrameFixture, String> {
    let shown = path.display();
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
    let from = match name.split('.').next() {
        Some("helper") => Sender::Helper,
        Some("server") => Sender::Server,
        _ => return Err(format!("{shown}: the name must start with helper. or server.")),
    };
    let value = read_json(path)?;
    let map = value.as_object().ok_or_else(|| format!("{shown}: not an object"))?;
    let frame = map.get("frame").cloned();
    let kind = match category {
        "valid" => {
            keys_only(map, &["frame", "expect"], path)?;
            let frame = frame.ok_or_else(|| format!("{shown}: no frame"))?;
            let t = frame.get("t").and_then(Value::as_str).unwrap_or_default();
            if name.split('.').nth(1) != Some(t) {
                return Err(format!(
                    "{shown}: a valid fixture is named <from>.<t>…, and t is {t:?}"
                ));
            }
            let expect = map.get("expect").cloned().unwrap_or_else(|| frame.clone());
            FixtureKind::Valid { frame, expect }
        }
        "invalid" => {
            keys_only(map, &["frame", "raw", "pad_bytes", "code", "path", "why"], path)?;
            let text = match (map.get("raw"), &frame, map.get("pad_bytes")) {
                (Some(Value::String(raw)), None, None) => raw.clone(),
                (None, Some(f), None) => f.to_string(),
                (None, Some(f), Some(pad)) => {
                    let n = pad
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(|| format!("{shown}: pad_bytes must be a count"))?;
                    format!("{f}{}", " ".repeat(n))
                }
                _ => return Err(format!("{shown}: give exactly one of frame and raw")),
            };
            let code = map
                .get("code")
                .and_then(Value::as_str)
                .and_then(ErrorCode::parse)
                .ok_or_else(|| format!("{shown}: code must be a protocol error code"))?;
            let path_at = map
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{shown}: path must be a string"))?
                .to_owned();
            if map.get("why").and_then(Value::as_str).is_none_or(str::is_empty) {
                return Err(format!("{shown}: why must say what the defect is"));
            }
            FixtureKind::Invalid { text, code, path: path_at }
        }
        "ignored" => {
            keys_only(map, &["frame", "why"], path)?;
            FixtureKind::Ignored { frame: frame.ok_or_else(|| format!("{shown}: no frame"))? }
        }
        other => return Err(format!("no fixture category {other:?}")),
    };
    Ok(FrameFixture { name, from, kind })
}

/// Every frame fixture of a category, sorted by name.
///
/// # Errors
/// The first file that does not read.
pub fn frame_fixtures(category: &str) -> Result<Vec<FrameFixture>, String> {
    json_files(&fixtures_dir().join("frames").join(category))?
        .iter()
        .map(|p| load_frame_fixture(category, p))
        .collect()
}

/// Every transcript, sorted by name.
///
/// # Errors
/// The first that does not load.
pub fn transcripts() -> Result<Vec<transcript::Transcript>, String> {
    json_files(&fixtures_dir().join("transcripts"))?
        .iter()
        .map(|p| transcript::Transcript::load(p))
        .collect()
}

/// The canonical example of each frame type, by sender and type.
///
/// # Errors
/// A valid fixture that does not read.
pub fn canonical_examples() -> Result<BTreeMap<(Sender, String), Value>, String> {
    let mut out = BTreeMap::new();
    for f in frame_fixtures("valid")? {
        if let (true, FixtureKind::Valid { frame, .. }) = (f.is_canonical(), &f.kind) {
            let t = frame.get("t").and_then(Value::as_str).unwrap_or_default().to_owned();
            out.insert((f.from, t), frame.clone());
        }
    }
    Ok(out)
}

/// PROTOCOL.md's frame table: each row's type and sender, in order.
///
/// # Errors
/// A table that cannot be found or read.
pub fn frame_table(markdown: &str) -> Result<Vec<(String, Sender)>, String> {
    let mut lines = markdown.lines().skip_while(|l| l.trim() != "## The frame table");
    if lines.next().is_none() {
        return Err("PROTOCOL.md has no \"## The frame table\" section".to_owned());
    }
    let mut lines = lines.skip_while(|l| !l.starts_with('|'));
    let header = lines.next().ok_or("the frame table has no header")?;
    let columns: Vec<&str> = header.trim_matches('|').split('|').map(str::trim).collect();
    if columns.first() != Some(&"Frame") || columns.get(1) != Some(&"From") {
        return Err(format!("the frame table's header is {header:?}, not | Frame | From | …"));
    }
    lines.next(); // the separator row
    let mut out = Vec::new();
    for line in lines.take_while(|l| l.starts_with('|')) {
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        let t = cells
            .first()
            .and_then(|c| c.strip_prefix('`'))
            .and_then(|c| c.strip_suffix('`'))
            .ok_or_else(|| format!("a frame table row without a `type`: {line:?}"))?;
        let from = match cells.get(1) {
            Some(&"helper") => Sender::Helper,
            Some(&"server") => Sender::Server,
            _ => {
                return Err(format!(
                    "a frame table row whose From is not helper or server: {line:?}"
                ));
            }
        };
        out.push((t.to_owned(), from));
    }
    if out.is_empty() {
        return Err("the frame table has no rows".to_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_table_parser_reads_rows_and_refuses_a_broken_table() {
        let md = "# x\n\n## The frame table\n\nText.\n\n| Frame | From | Purpose |\n|---|---|---|\n\
                  | `hello` | helper | the first |\n| `ping` | server | alive |\n\nAfter.\n| `x` | helper | no |\n";
        let rows = frame_table(md).expect("reads");
        assert_eq!(rows, vec![("hello".into(), Sender::Helper), ("ping".into(), Sender::Server)]);
        assert!(frame_table("# no table").is_err());
        let bad = md.replace("| `ping` | server |", "| ping | server |");
        assert!(frame_table(&bad).is_err());
        let bad = md.replace("| `ping` | server |", "| `ping` | hub |");
        assert!(frame_table(&bad).is_err());
    }
}

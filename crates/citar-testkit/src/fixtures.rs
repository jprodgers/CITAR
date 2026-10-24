//! The Python states refcheck records (`refcheck/README.md`), for the converter's tests and the
//! `convert` golden set (package 1a-10).
//!
//! A fixture is `<case>/t<turn>.json.gz` under a fixture folder: gzipped JSON whose `state` is
//! Python's `GameState.to_dict()`. The committed folders are `refcheck/fixtures-mini` (9 states)
//! and `refcheck/fixtures-late` (3); the corpus (250 more) is local only, and tests read it when
//! [`CORPUS_ENV`] names its folder.

use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::value::RawValue;

/// The committed fixture folders, from the repository root.
pub const COMMITTED: [&str; 2] = ["refcheck/fixtures-mini", "refcheck/fixtures-late"];

/// The environment variable that names the corpus folder (as for citar-refcheck's tests).
pub const CORPUS_ENV: &str = "CITAR_REFCHECK_CORPUS";

/// A fixture file, found but not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixture {
    /// `<case>/t<turn>`.
    pub name: String,
    pub case: String,
    pub turn: u32,
    pub path: PathBuf,
}

/// The repository root.
#[must_use]
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The committed fixtures, sorted by case, then turn: the twelve DESIGN.md 2.2 lists.
///
/// # Errors
/// If a folder cannot be read.
pub fn committed() -> Result<Vec<Fixture>, String> {
    let root = repo_root();
    let mut out = Vec::new();
    for dir in COMMITTED {
        out.extend(in_folder(&root.join(dir))?);
    }
    out.sort_by(|a, b| (&a.case, a.turn).cmp(&(&b.case, b.turn)));
    Ok(out)
}

/// The corpus's fixtures, if [`CORPUS_ENV`] names a folder.
///
/// # Errors
/// If the folder cannot be read.
#[allow(clippy::disallowed_methods, reason = "a test's switch, not a game's input")]
pub fn corpus() -> Result<Option<Vec<Fixture>>, String> {
    match std::env::var_os(CORPUS_ENV) {
        None => Ok(None),
        Some(dir) => in_folder(Path::new(&dir)).map(Some),
    }
}

/// Every `<case>/t<turn>.json.gz` in a fixture folder, sorted by case, then turn.
///
/// # Errors
/// If the folder cannot be read.
#[allow(clippy::disallowed_methods, reason = "the fixtures are files")]
pub fn in_folder(dir: &Path) -> Result<Vec<Fixture>, String> {
    let mut out = Vec::new();
    let cases = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for case in cases {
        let case = case.map_err(|e| format!("{}: {e}", dir.display()))?;
        if !case.path().is_dir() {
            continue;
        }
        let name = case.file_name().to_string_lossy().into_owned();
        let files = std::fs::read_dir(case.path()).map_err(|e| format!("{name}: {e}"))?;
        for file in files {
            let file = file.map_err(|e| format!("{name}: {e}"))?;
            let fname = file.file_name().to_string_lossy().into_owned();
            let Some(turn) = fname
                .strip_prefix('t')
                .and_then(|t| t.strip_suffix(".json.gz"))
                .and_then(|t| t.parse::<u32>().ok())
            else {
                continue;
            };
            out.push(Fixture {
                name: format!("{name}/t{turn}"),
                case: name.clone(),
                turn,
                path: file.path(),
            });
        }
    }
    out.sort_by(|a, b| (&a.case, a.turn).cmp(&(&b.case, b.turn)));
    Ok(out)
}

#[derive(Deserialize)]
struct FixtureDoc<'a> {
    #[serde(borrow)]
    state: &'a RawValue,
    #[serde(rename = "meta")]
    _meta: IgnoredAny,
    #[serde(rename = "queries")]
    _queries: IgnoredAny,
}

/// A fixture's `state`, as the JSON text Python wrote.
///
/// # Errors
/// If the file cannot be read or is not a fixture.
#[allow(clippy::disallowed_types, reason = "the fixtures are files")]
pub fn read_state(f: &Fixture) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(&f.path).map_err(|e| format!("{}: {e}", f.name))?;
    let mut bytes = Vec::new();
    GzDecoder::new(std::io::BufReader::new(file))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: not gzip: {e}", f.name))?;
    let doc: FixtureDoc<'_> =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: not a fixture: {e}", f.name))?;
    Ok(doc.state.get().as_bytes().to_vec())
}

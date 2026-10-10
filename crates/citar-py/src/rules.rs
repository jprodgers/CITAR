//! The process's ruleset (DESIGN.md P2.8.4): the one compiled into the engine, or the one in the
//! directory `CITAR_RULESET_DIR` names, read once when the module loads.
//!
//! Every game, every ruleset function, the bots' fingerprints and `build_info` use [`rules`], so
//! a modded process's games, saves and fingerprints carry its `RulesetId` and are told apart from
//! the shipped ruleset's. A directory that does not load fails the import with every problem
//! found, rather than leaving the process on the shipped rules with the mod silently unread.
//! [`check_ruleset`] loads a directory without adopting it, for `citar ruleset check`.
//!
//! Replaces nothing in the engine, which does no I/O: Python read `citar/data/` at import
//! (`rules.py:44-85`), and modders ran two scripts over it to find what it would not read.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use citar_engine::rules::{Ruleset, RulesetErrors, RulesetFiles};
use pyo3::exceptions::PyImportError;
use pyo3::prelude::*;
use serde_json::json;

use crate::Bytes;
use crate::errors::{Failure, guarded};

/// The variable naming a ruleset directory in the data layout: `ruleset/*.json`,
/// `custom/*.json` and `game.json`.
pub const ENV: &str = "CITAR_RULESET_DIR";

/// The ruleset `CITAR_RULESET_DIR` gave the process, and the directory it came from.
struct FromDir {
    rules: &'static Ruleset,
    dir: PathBuf,
}

static FROM_DIR: OnceLock<FromDir> = OnceLock::new();

/// The process's ruleset: `CITAR_RULESET_DIR`'s when the module loaded with one, else the one
/// compiled in (loaded on first use).
pub fn rules() -> &'static Ruleset {
    FROM_DIR.get().map_or_else(Ruleset::shared, |d| d.rules)
}

/// The directory the process's ruleset came from, or `None` for the one compiled in.
pub fn dir() -> Option<&'static Path> {
    FROM_DIR.get().map(|d| d.dir.as_path())
}

/// Reads `CITAR_RULESET_DIR`, when it names anything, and makes that directory's ruleset the
/// process's: called once, by the module's initialisation. An empty value is no value, as for
/// CITAR's other variables. `ImportError` when the directory does not load.
pub fn adopt_from_env(py: Python<'_>) -> PyResult<()> {
    let Some(dir) = std::env::var_os(ENV).filter(|v| !v.to_string_lossy().trim().is_empty()) else {
        return Ok(());
    };
    let dir = PathBuf::from(dir);
    let loaded = guarded(py, || {
        let files = read_dir(&dir).map_err(Failure::Os)?;
        Ok(Ruleset::leak(&files.as_ruleset_files()))
    });
    let shown = shown(dir.as_os_str());
    match loaded {
        Ok(Ok(rules)) => match FROM_DIR.set(FromDir { rules, dir }) {
            Ok(()) => Ok(()),
            // A second initialisation in the process (a module initialised again) keeps the
            // ruleset its games already play, and says so if it was told another.
            Err(_) if FROM_DIR.get().is_some_and(|d| d.rules.id() == rules.id()) => Ok(()),
            Err(_) => Err(PyImportError::new_err(format!(
                "{ENV}={shown}: this process already plays another ruleset, read when citar._engine \
                 first loaded"
            ))),
        },
        Ok(Err(errors)) => Err(PyImportError::new_err(format!(
            "{ENV}={shown}: {errors}\n(`citar ruleset check {shown}` reports the same; unset \
             {ENV} for the ruleset CITAR ships with)"
        ))),
        Err(f) => Err(PyImportError::new_err(format!("{ENV}={shown}: {}", failure_text(f)))),
    }
}

/// Loads the ruleset in `dir` without adopting it (`citar ruleset check DIR`): JSON bytes
/// `{dir, id, version, counts, errors}`, the errors each `{kind, file, object, text}` and none
/// when it loads (then `id`, `version` and `counts` say what it holds; else they are null).
/// Problems are found stage by stage, so a later stage's appear once the earlier ones are fixed.
/// `OSError` for a directory that cannot be read.
#[pyfunction]
pub fn check_ruleset(py: Python<'_>, dir: PathBuf) -> PyResult<Bytes> {
    Ok(guarded(py, || {
        let files = read_dir(&dir).map_err(Failure::Os)?;
        let (rules, errors) = match Ruleset::load(&files.as_ruleset_files()) {
            Ok(r) => (Some(r), Vec::new()),
            Err(RulesetErrors(errors)) => (None, errors),
        };
        let errors: Vec<_> = errors
            .iter()
            .map(|e| {
                json!({"kind": format!("{:?}", e.kind), "file": &*e.file,
                       "object": &*e.object, "text": e.text})
            })
            .collect();
        let counts = rules.as_ref().map(|r| serde_json::to_value(r.counts())).transpose();
        let out = json!({
            "dir": shown(dir.as_os_str()),
            "id": rules.as_ref().map(|r| r.id().to_hex()),
            "version": rules.as_ref().map(Ruleset::version),
            "counts": counts.map_err(|e| Failure::Runtime(e.to_string()))?,
            "errors": errors,
        });
        serde_json::to_vec(&out).map(Bytes).map_err(|e| Failure::Runtime(e.to_string()))
    })?)
}

/// A ruleset directory's files, read whole, by the names the loader knows them by.
struct DirFiles(Vec<(String, Vec<u8>)>);

impl DirFiles {
    fn as_ruleset_files(&self) -> RulesetFiles<'_> {
        RulesetFiles::new(self.0.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect())
    }
}

/// Reads a ruleset directory: every `.json` file of `ruleset/` and `custom/`, and `game.json`.
/// A file the ruleset does not have is read too, so the loader names it (`UnknownFile`) rather
/// than leaving a mod's table unread; a missing one is the loader's to report. Other files (the
/// tables' NOTICE.md) and folders are left alone. The files go in name order, so the problems
/// come out in the same order everywhere.
fn read_dir(dir: &Path) -> Result<DirFiles, String> {
    let fail = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    if !dir.is_dir() {
        return Err(format!("{}: not a directory", dir.display()));
    }
    let mut files = Vec::new();
    for folder in ["ruleset", "custom"] {
        let path = dir.join(folder);
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(fail(&path, e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| fail(&path, e))?;
            let file = entry.path();
            if !file.is_file() || file.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let name = format!("{folder}/{}", entry.file_name().to_string_lossy());
            files.push((name, std::fs::read(&file).map_err(|e| fail(&file, e))?));
        }
    }
    let game = dir.join("game.json");
    match std::fs::read(&game) {
        Ok(bytes) => files.push(("game.json".to_owned(), bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(fail(&game, e)),
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(DirFiles(files))
}

/// A path as the user wrote it, for messages.
fn shown(path: &std::ffi::OsStr) -> String {
    path.to_string_lossy().into_owned()
}

/// A failure's message, for an `ImportError` that carries it.
fn failure_text(f: Failure) -> String {
    match f {
        Failure::Action(e) => e.message,
        Failure::Crash(m)
        | Failure::Value(m)
        | Failure::Map(m)
        | Failure::Load(m)
        | Failure::Os(m)
        | Failure::Type(m)
        | Failure::Runtime(m) => m,
    }
}

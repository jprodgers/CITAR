//! Divergence artifacts (DESIGN.md 9.6): when a golden game leaves its committed file, the state
//! where it first does, so that two targets' states can be compared field by field rather than
//! digest by digest.
//!
//! - `golden check --states DIR` sets [`set_dir`]. Each whole-game set (`turns`, `pass`,
//!   `random`, `long`) then watches every round of each game against the committed file's
//!   digest for that round ([`Watch`]); at the first round that differs it writes the state as
//!   it ended that round, and the state of the round before (the last that agreed), as save JSON
//!   (format v1, `Snapshot::to_json`). The single-state sets (`newgame`, `load`) write the state
//!   of a row that differs. Each file written is listed in `DIR/divergence.jsonl`. Each check
//!   starts the list afresh: the list a check before it left in the folder, and the states that
//!   list names, are removed first, so a folder used twice lists only the last check's states.
//! - `golden dump SET:GAME [TURN]` plays a game of a set on this machine and writes its state as
//!   that round ended ([`super::dump`]): the same state from a target that agrees with the file.
//! - `golden diff A B` with two states lists the places they differ, exactly
//!   ([`diff_states`]): integers, floats by their shortest round-trip text (so `-0.0` and `0.0`
//!   differ, as the digest has them), strings, keys on one side only.
//!
//! The determinism workflow uploads each target's folder when its check fails, and its compare
//! job, when the targets disagree, dumps the same states on linux-x64 and diffs them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use citar_engine::base::digest::Digest;
use citar_engine::base::ids::Turn;
use citar_engine::game::Game;
use citar_engine::save::Snapshot;
use serde_json::{Value, json};

/// The folder the check writes divergent states to, if it was given one.
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The list of the states a check wrote, in its folder.
const LIST: &str = "divergence.jsonl";

/// Sets the folder `golden check` writes divergent states to, or none. A folder given starts
/// afresh: the list a check before left there and the states it names are removed.
///
/// # Errors
/// A list or a state it names that is there but cannot be read or removed.
pub fn set_dir(dir: Option<PathBuf>) -> Result<(), String> {
    if let Some(d) = &dir {
        clear(d)?;
    }
    if let Ok(mut slot) = DIR.lock() {
        *slot = dir;
    }
    Ok(())
}

/// Removes the list a check left in `dir`, and the states it names: only plain file names in
/// the folder, as [`file_name`] makes them, so a list edited by hand removes nothing outside it.
#[allow(clippy::disallowed_methods, clippy::disallowed_types, reason = "the artifacts are files")]
fn clear(dir: &Path) -> Result<(), String> {
    use std::io::ErrorKind;
    let list = dir.join(LIST);
    let text = match std::fs::read_to_string(&list) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", list.display())),
    };
    let named = text.lines().filter_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        v.get("file")?.as_str().map(str::to_owned)
    });
    for file in named
        .filter(|f| f.ends_with(".json") && !f.starts_with('.') && !f.contains(['/', '\\', ':']))
    {
        match std::fs::remove_file(dir.join(&file)) {
            Err(e) if e.kind() != ErrorKind::NotFound => return Err(format!("{file}: {e}")),
            _ => {}
        }
    }
    std::fs::remove_file(&list).map_err(|e| format!("{}: {e}", list.display()))
}

/// The folder divergent states go to, if one was set.
#[must_use]
pub fn dir() -> Option<PathBuf> {
    DIR.lock().ok().and_then(|d| d.clone())
}

/// A file name for a game's state: the set, the game with anything but letters, digits and `-`
/// written as `_`, and the turn.
#[must_use]
pub fn file_name(set: &str, game: &str, turn: Option<Turn>, suffix: &str) -> String {
    let safe: String =
        game.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    match turn {
        Some(t) => format!("{set}--{safe}--t{t}{suffix}.json"),
        None => format!("{set}--{safe}{suffix}.json"),
    }
}

/// Writes a state to the folder and lists it in `divergence.jsonl`, with what the committed
/// file holds there and what this build computed. A failure to write is reported in the list's
/// place: the check has already failed on the digest, so this only loses the artifact.
#[allow(clippy::disallowed_methods, clippy::disallowed_types, reason = "the artifacts are files")]
fn write(dir: &Path, entry: &Value, file: &str, snapshot: &Snapshot) -> Result<(), String> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let bytes = snapshot.to_json().map_err(|e| format!("{file}: does not save: {e}"))?;
    std::fs::write(dir.join(file), bytes).map_err(|e| format!("{file}: {e}"))?;
    let mut list = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LIST))
        .map_err(|e| format!("{LIST}: {e}"))?;
    writeln!(list, "{entry}").map_err(|e| format!("{LIST}: {e}"))
}

/// Writes the state of a single-state set's row that differs from the committed file (`newgame`,
/// `load`), if a folder is set.
#[must_use]
pub fn whole(
    set: &str,
    game: &str,
    g: &Game,
    committed: Option<&str>,
    computed: &str,
) -> Vec<String> {
    let Some(dir) = dir() else { return Vec::new() };
    let file = file_name(set, game, None, "");
    let entry = json!({
        "set": set, "game": game, "turn": null, "file": file,
        "committed": committed, "computed": computed, "agrees": false,
    });
    write(&dir, &entry, &file, &g.snapshot()).err().into_iter().collect()
}

/// Watches one game of a whole-game set against the committed digest of each of its rounds,
/// and writes the state of the first round that differs, and of the round before it.
pub struct Watch {
    dir: PathBuf,
    set: String,
    game: String,
    want: BTreeMap<Turn, String>,
    /// The last round that agreed, kept until one does not.
    before: Option<(Turn, Snapshot)>,
    done: bool,
    /// What could not be written.
    pub problems: Vec<String>,
}

impl Watch {
    /// A watch over game `game` of set `set`, whose committed rounds are `(turn, digest)`; none
    /// when no folder is set or the file has no rounds for it.
    #[must_use]
    pub fn new(
        set: &str,
        game: &str,
        rounds: impl IntoIterator<Item = (Turn, String)>,
    ) -> Option<Self> {
        let dir = dir()?;
        let want: BTreeMap<Turn, String> = rounds.into_iter().collect();
        (!want.is_empty()).then(|| Self {
            dir,
            set: set.to_owned(),
            game: game.to_owned(),
            want,
            before: None,
            done: false,
            problems: Vec::new(),
        })
    }

    /// The committed rounds of `game` in a file's round rows, `[game, turn, digest]` each.
    #[must_use]
    pub fn rows_of(rows: Option<&Value>, game: &str) -> Vec<(Turn, String)> {
        rows.and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter(|r| r.get(0).and_then(Value::as_str) == Some(game))
                    .filter_map(|r| {
                        let turn = Turn::try_from(r.get(1)?.as_i64()?).ok()?;
                        Some((turn, r.get(2)?.as_str()?.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Looks at a round as it ended: the first that differs from the file is written out, with
    /// the one before it.
    pub fn round(&mut self, g: &Game, turn: Turn, digest: &Digest) {
        if self.done {
            return;
        }
        let computed = digest.to_hex();
        let committed = self.want.get(&turn);
        if committed == Some(&computed) {
            self.before = Some((turn, g.snapshot()));
            return;
        }
        self.done = true;
        let file = file_name(&self.set, &self.game, Some(turn), "");
        let entry = json!({
            "set": self.set, "game": self.game, "turn": turn, "file": file,
            "committed": committed, "computed": computed, "agrees": false,
        });
        self.problems.extend(write(&self.dir, &entry, &file, &g.snapshot()).err());
        if let Some((t, snap)) = self.before.take() {
            let file = file_name(&self.set, &self.game, Some(t), "-agrees");
            let entry = json!({
                "set": self.set, "game": self.game, "turn": t, "file": file,
                "committed": self.want.get(&t), "computed": self.want.get(&t), "agrees": true,
            });
            self.problems.extend(write(&self.dir, &entry, &file, &snap).err());
        }
    }
}

/// Every place two states differ, exactly, up to `limit` of them, as `path: a against b`.
#[must_use]
pub fn diff_states(a: &Value, b: &Value, limit: usize) -> (Vec<String>, usize) {
    let mut found = Found { lines: Vec::new(), count: 0, limit };
    walk(a, b, &mut String::new(), &mut found);
    (found.lines, found.count)
}

/// A value as a diff line shows it, cut short.
fn shown(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 120 {
        format!("{}...", s.chars().take(117).collect::<String>())
    } else {
        s
    }
}

/// The differences found so far, and how many there are in all.
struct Found {
    lines: Vec<String>,
    count: usize,
    limit: usize,
}

impl Found {
    fn note(&mut self, path: &str, what: String) {
        self.count += 1;
        if self.lines.len() < self.limit {
            self.lines.push(format!("{}: {what}", if path.is_empty() { "(root)" } else { path }));
        }
    }
}

fn walk(a: &Value, b: &Value, path: &mut String, found: &mut Found) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, va) in x {
                let len = path.len();
                path.push('.');
                path.push_str(k);
                match y.get(k) {
                    Some(vb) => walk(va, vb, path, found),
                    None => found.note(path, format!("{} against nothing", shown(va))),
                }
                path.truncate(len);
            }
            for (k, vb) in y {
                if !x.contains_key(k) {
                    found.note(&format!("{path}.{k}"), format!("nothing against {}", shown(vb)));
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                let len = path.len();
                path.push('[');
                path.push_str(&i.to_string());
                path.push(']');
                walk(va, vb, path, found);
                path.truncate(len);
            }
            if x.len() != y.len() {
                found.note(path, format!("{} elements against {}", x.len(), y.len()));
            }
        }
        // Numbers by their text: with `float_roundtrip` that is the float's every bit, and it
        // keeps -0.0 apart from 0.0, as the digest does.
        (Value::Number(x), Value::Number(y)) if x.to_string() == y.to_string() => {}
        _ if a == b && !a.is_number() => {}
        _ => found.note(path, format!("{} against {}", shown(a), shown(b))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_diff_is_exact_and_names_each_place() {
        let a = json!({"x": 1, "f": 0.1, "z": 0.0, "l": [1, 2, 3], "o": {"k": "v"}, "only": true});
        let b = json!({"x": 1, "f": 0.1, "z": -0.0, "l": [1, 2], "o": {"k": "w"}, "new": null});
        let (lines, n) = diff_states(&a, &b, 10);
        assert_eq!(n, 5, "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(".z: 0.0 against -0.0")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(".l: 3 elements against 2")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(".o.k: \"v\" against \"w\"")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(".only: true against nothing")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(".new: nothing against null")), "{lines:?}");
        assert_eq!(diff_states(&a, &a, 10).1, 0);
        // A float's last bit counts.
        let (lines, n) = diff_states(&json!(0.1 + 0.2), &json!(0.3), 10);
        assert_eq!(n, 1, "{lines:?}");
    }

    #[test]
    fn file_names_keep_to_safe_characters() {
        assert_eq!(
            file_name("pass", "small-pangaea-raging/t50", Some(57), ""),
            "pass--small-pangaea-raging_t50--t57.json"
        );
        assert_eq!(
            file_name("newgame", "newgame-duel-continents", None, ""),
            "newgame--newgame-duel-continents.json"
        );
    }
}

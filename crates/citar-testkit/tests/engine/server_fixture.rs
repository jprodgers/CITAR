//! The late fixture as the server loads it (package 2-09's gate 4): an engine-format copy of
//! `refcheck/fixtures-late/small-continents-normal-s1025/t280`, converted from the state Python
//! wrote (`Game::from_python`) and saved whole (`Game::save_whole`): its state JSON in
//! `testdata/server/late-t280.state.json.gz` and its history, one journal chunk, in
//! `late-t280.journal.gz`. The Python suite's server tests load it through
//! `EngineGame.from_save` (the base64 of the chunk is the facade's) and time the god view of
//! `/api/games/{gid}/view` on it; the server cannot convert a Python state itself, since the
//! bindings never carry the `legacy` converter.
//!
//! The copy need not follow every change of the engine (a game it loads is all the server tests
//! ask of it), but it must load: after a change of the save format, rewrite it with
//! `CITAR_BLESS_SERVER_FIXTURE=1 cargo nextest run -p citar-testkit --test engine server_fixture`.

use std::io::{Read, Write};
use std::path::PathBuf;

use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_testkit::fixtures;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

/// The fixture the copy is made from, as `crates/citar-bench`'s `fixtures::LATE` names it.
const LATE: (&str, u32) = ("small-continents-normal-s1025", 280);

fn folder() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata").join("server")
}

fn gunzip(path: &PathBuf) -> Vec<u8> {
    let packed = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = Vec::new();
    GzDecoder::new(packed.as_slice())
        .read_to_end(&mut out)
        .unwrap_or_else(|e| panic!("{}: not gzip: {e}", path.display()));
    out
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut z = GzEncoder::new(Vec::new(), Compression::best());
    z.write_all(bytes).expect("gzip into memory");
    z.finish().expect("gzip into memory")
}

/// The copy made now: the converted fixture saved whole, as (state JSON, journal chunk).
fn converted() -> (Vec<u8>, Vec<u8>) {
    let all = fixtures::committed().unwrap_or_else(|e| panic!("{e}"));
    let f = all
        .iter()
        .find(|f| f.case == LATE.0 && f.turn == LATE.1)
        .unwrap_or_else(|| panic!("no committed fixture {}/t{}", LATE.0, LATE.1));
    let bytes = fixtures::read_state(f).unwrap_or_else(|e| panic!("{e}"));
    let (g, _) = Game::from_python(Ruleset::shared(), &bytes)
        .unwrap_or_else(|e| panic!("{}: {e}", f.name));
    let whole = g.save_whole().unwrap_or_else(|e| panic!("{}: {e}", f.name));
    (whole.state, whole.history.expect("a fixture at turn 280 has a history"))
}

#[test]
fn the_server_copy_of_the_late_fixture_loads() {
    let dir = folder();
    let (state_path, journal_path) =
        (dir.join("late-t280.state.json.gz"), dir.join("late-t280.journal.gz"));
    if std::env::var_os("CITAR_BLESS_SERVER_FIXTURE").is_some() {
        let (state, chunk) = converted();
        std::fs::create_dir_all(&dir).expect("the testdata folder");
        std::fs::write(&state_path, gzip(&state)).expect("the state");
        std::fs::write(&journal_path, gzip(&chunk)).expect("the journal chunk");
    }
    let (state, chunk) = (gunzip(&state_path), gunzip(&journal_path));
    let (g, report) = Game::load(Ruleset::shared(), &state, &mut std::iter::once(chunk.as_slice()))
        .unwrap_or_else(|e| {
            panic!(
                "the server's copy of the late fixture no longer loads ({e}): rewrite it with \
                 CITAR_BLESS_SERVER_FIXTURE=1 (this file's doc)"
            )
        });
    assert!(!report.chronicle_incomplete, "the copy's history comes back whole");
    assert_eq!(g.turn(), i32::try_from(LATE.1).expect("a turn"));
    assert!(g.majors(true).count() >= 2, "a game with its civilizations");
    assert!(
        g.chronicle().events().len() > 150,
        "a late game's history: more events than a view shows"
    );
}

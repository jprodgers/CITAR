//! The states the suites time: the reference fixtures loaded as refcheck loads them
//! (`Game::from_python`, converted and settled once), and the synthetic gargantuan state.
//!
//! The committed fixtures are always there; the corpus (250 states, DESIGN.md 2.2) only when
//! `CITAR_REFCHECK_CORPUS` names its folder, and a part that needs it falls back to a committed
//! state or is skipped, saying so.

use std::io::Read;

use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::{Phase, State};
use citar_testkit::fixtures::{self as tk, Fixture};
use citar_testkit::states::{self, Shape};

/// Whether the corpus folder is named and readable.
#[must_use]
pub fn has_corpus() -> bool {
    matches!(tk::corpus(), Ok(Some(ref all)) if !all.is_empty())
}

/// The corpus's fixtures, or none.
#[must_use]
pub fn corpus() -> Vec<Fixture> {
    tk::corpus().ok().flatten().unwrap_or_default()
}

/// The committed fixtures.
///
/// # Panics
///
/// If the committed folders cannot be read.
#[must_use]
pub fn committed() -> Vec<Fixture> {
    tk::committed().expect("the committed fixtures")
}

/// Fixture `case` at `turn` among `all`.
#[must_use]
pub fn find(all: &[Fixture], case: &str, turn: u32) -> Option<Fixture> {
    all.iter().find(|f| f.case == case && f.turn == turn).cloned()
}

/// A fixture's game, loaded through the Python converter and settled once, its checks off
/// ([`crate::unchecked`]).
///
/// # Panics
///
/// If the fixture does not read or convert.
#[must_use]
pub fn load(f: &Fixture) -> Game {
    let bytes = tk::read_state(f).unwrap_or_else(|e| panic!("{e}"));
    crate::unchecked(
        Game::from_python(Ruleset::shared(), &bytes)
            .unwrap_or_else(|e| panic!("{}: {e}", f.name))
            .0,
    )
}

/// What Python recorded for group `group` of a fixture (its `queries`), as JSON.
///
/// # Panics
///
/// If the fixture does not read.
#[must_use]
pub fn recorded(f: &Fixture, group: &str) -> serde_json::Value {
    let file = std::fs::File::open(&f.path).unwrap_or_else(|e| panic!("{}: {e}", f.name));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::io::BufReader::new(file))
        .read_to_end(&mut bytes)
        .unwrap_or_else(|e| panic!("{}: {e}", f.name));
    let doc: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{}: {e}", f.name));
    doc.get("queries").and_then(|q| q.get(group)).cloned().unwrap_or_default()
}

/// Committed fixture `case` at `turn`, loaded.
///
/// # Panics
///
/// If there is no such fixture.
#[must_use]
pub fn committed_game(case: &str, turn: u32) -> Game {
    let f = find(&committed(), case, turn).unwrap_or_else(|| panic!("no fixture {case}/t{turn}"));
    load(&f)
}

/// Corpus fixture `case` at `turn`, loaded, when the corpus is there.
#[must_use]
pub fn corpus_game(case: &str, turn: u32) -> Option<(Game, String)> {
    let f = find(&corpus(), case, turn)?;
    Some((load(&f), f.name))
}

/// The late committed fixture, `small-continents-normal-s1025/t280`: the latest small map, where
/// most kernels are timed (the corpus has no small map at turn 300).
#[must_use]
pub fn late() -> Game {
    committed_game(LATE.0, LATE.1)
}

/// The late committed fixture's case and turn.
pub const LATE: (&str, u32) = ("small-continents-normal-s1025", 280);

/// The synthetic gargantuan state (24 majors, 32 city-states, 400 cities, 2,500 units on 160 by
/// 100 tiles, most of the map explored), as the digest, the save and the god view time it.
#[must_use]
pub fn gargantuan_state() -> State {
    states::build(Ruleset::shared(), 2026, &Shape::GARGANTUAN)
}

/// The synthetic gargantuan state as a game that has just played a round, its history not yet
/// taken into the journal: what a session's save takes under its lock as a round ends (package
/// 2-11, DESIGN.md P2.5.3's "a round passed on the synthetic state"), the state and the round's
/// chunk: the round's events, its stats row and its frame, a keyframe (a game's first frame, the
/// largest a round records; a delta is a few kilobytes). The round is played at the state's turn
/// from its first living major's turn to the next round's, every living major ending its turn
/// once, with the victories off and the turn limit out of reach, since the synthetic state holds
/// what no game reaches. Its first end of turn hung until `fbf1edc` (one great person of a kind a
/// turn) and `af82e1b` (a city-state's ally is a major). The checks are off
/// ([`crate::unchecked`]).
///
/// # Panics
///
/// If the synthetic state is not sound, which the testkit's own tests rule out, or its round
/// does not end.
#[must_use]
pub fn gargantuan_round_game() -> Game {
    let r = Ruleset::shared();
    let mut parts = gargantuan_state().into_parts();
    let first = parts.players.iter().find(|(_, p)| p.is_major() && p.alive()).map(|(id, _)| id);
    parts.clock.current = first.expect("a living major");
    parts.clock.turn_started = false;
    parts.config.turn_limit = 10_000;
    parts.config.disabled_victories = r.victories().iter().map(|(id, _)| id).collect();
    let seats = parts.players.len();
    let st = State::from_parts(parts).expect("the synthetic state fits");
    let mut g = crate::unchecked(Game::from_state(r, st, Chronicle::new()).expect("a sound state"));
    let turn = g.turn();
    for _ in 0..=seats {
        if g.turn() != turn {
            break;
        }
        let who = g.current();
        if let Err(e) = g.end_turn(who) {
            panic!("the gargantuan round, turn {turn}, {who:?}: {}", e.message);
        }
    }
    assert_eq!((g.turn(), g.phase()), (turn + 1, Phase::Playing), "the round ended in play");
    g
}

/// The synthetic gargantuan state as a game, its checks off ([`crate::unchecked`]).
///
/// # Panics
///
/// If the synthetic state is not sound, which the testkit's own tests rule out.
#[must_use]
pub fn gargantuan_game() -> Game {
    crate::unchecked(
        Game::from_state(Ruleset::shared(), gargantuan_state(), Chronicle::new())
            .expect("a sound state"),
    )
}

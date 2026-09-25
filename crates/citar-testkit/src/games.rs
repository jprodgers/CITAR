//! Whole games, for the whole-game tests and the golden sets of package 1c-10 (DESIGN.md 9.5,
//! 9.6): a new game with a [`RandomAgent`] in every major civilization's seat, or a Python state
//! passed round after round, each played a seat at a time so that the caller sees every round
//! end, with that round's digest from the game's chain (DESIGN.md 4.10).
//!
//! A caller's hook runs after each round and may do anything a host may do there: read, save,
//! even put a loaded game in the played one's place, which is how the save-and-load-every-round
//! test runs ([`play_random`], [`pass_rounds`]).

use citar_engine::base::digest::Digest;
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::game::{DebugOptions, DriveOptions, Drivers, Game, Stop};
use citar_engine::rules::Ruleset;
use citar_engine::save::chain::DigestChain;
use citar_engine::state::Phase;
use serde_json::{Map, Value};

use crate::agents::RandomAgent;
use crate::fixtures::{self, Fixture};
use crate::golden::newgame::generated_settings;
use crate::script;

/// One round the chain took: its turn number and the state's digest as it ended.
pub type Round = (Turn, Digest);

/// A hook run after every round: it may read the game, save it, or replace it with another (a
/// game loaded from its save, with the chain resumed). An error stops the game.
pub type Hook<'a> = dyn FnMut(&mut Game, Round) -> Result<(), String> + 'a;

/// The settings of a generated game (the lobby's size, map type and edges, the first seat a
/// person's and the rest the bot's) that ends at `turn_limit`.
#[must_use]
pub fn random_settings(
    size: &str,
    map_type: &str,
    edges: &str,
    seed: u64,
    turn_limit: u32,
) -> Value {
    let mut v = generated_settings(size, map_type, edges, seed);
    if let Some(o) = v.as_object_mut() {
        o.insert("turn_limit".to_owned(), Value::from(turn_limit));
    }
    v
}

/// A new game from settings as a lobby sends them, keeping a chain of round digests under
/// `spec`, with the checks that run at every settle set to `debug`.
///
/// # Errors
/// The engine's refusal of the settings.
pub fn new_game(settings: &Value, spec: &[u8], debug: DebugOptions) -> Result<Game, String> {
    let cfg: Map<String, Value> = settings.as_object().cloned().unwrap_or_default();
    let mut g = script::new_game(Ruleset::shared(), &cfg)?;
    g.set_debug_options(debug);
    g.set_chain(Some(DigestChain::new(spec)));
    Ok(g)
}

/// A committed or corpus fixture loaded as refcheck loads it (`Game::from_python`: converted,
/// then settled once), keeping a chain of round digests under `spec`.
///
/// # Errors
/// If the fixture cannot be read or converted.
pub fn from_fixture(f: &Fixture, spec: &[u8], debug: DebugOptions) -> Result<Game, String> {
    let bytes = fixtures::read_state(f)?;
    let (mut g, _) =
        Game::from_python(Ruleset::shared(), &bytes).map_err(|e| format!("{}: {e}", f.name))?;
    g.set_debug_options(debug);
    g.set_chain(Some(DigestChain::new(spec)));
    Ok(g)
}

/// One agent per player of `g`: the agent draws from streams keyed by the game's seed and the
/// seat, so a fresh agent plays a loaded game as the one that played it before would.
#[must_use]
pub fn agents_for(g: &Game) -> Vec<RandomAgent> {
    vec![RandomAgent::new(); g.state().players().len()]
}

/// Whatever the checks found since the last look, as text: the violations the game reported at
/// its settles, and the invariants it breaks now.
#[must_use]
pub fn problems(g: &mut Game) -> Vec<String> {
    let mut out: Vec<String> =
        g.take_violations().into_iter().map(|v| format!("turn {}: {v:?}", g.turn())).collect();
    out.extend(
        g.check_invariants().into_iter().map(|v| format!("turn {}: breaks {v:?}", g.turn())),
    );
    out
}

/// The rounds the game's chain has taken.
fn rounds_taken(g: &Game) -> u32 {
    g.chain().map_or(0, DigestChain::rounds)
}

/// Calls `hook` for the round the chain took, if it took one since `before`.
fn after_round(g: &mut Game, before: u32, hook: &mut Hook<'_>) -> Result<bool, String> {
    if rounds_taken(g) == before {
        return Ok(false);
    }
    let round = g.last_round().ok_or("a round was taken, but the chain has no last round")?;
    hook(g, round)?;
    Ok(true)
}

/// Plays `g` with `agents` (one per player, as [`agents_for`] gives) in every major's seat, a
/// seat at a time, until the game is over or `max_rounds` rounds have ended, calling `hook`
/// after every round. Returns the rounds played.
///
/// # Errors
/// A refusal by the engine, a stop no driven game makes, or the hook's error.
pub fn play_random(
    g: &mut Game,
    agents: &mut [RandomAgent],
    max_rounds: u32,
    hook: &mut Hook<'_>,
) -> Result<u32, String> {
    let mut played = 0;
    while played < max_rounds && g.phase() == Phase::Playing {
        let before = rounds_taken(g);
        let n = g.state().players().len();
        let mut d = Drivers::none(n);
        for (i, a) in agents.iter_mut().enumerate().take(n) {
            let p = PlayerId(u8::try_from(i).map_err(|_| "too many players")?);
            if g.player(p).is_some_and(|x| x.is_major()) {
                d = d.with(p, a);
            }
        }
        let (stop, _) = g
            .drive(&mut d, DriveOptions::default().with_seat_limit(1))
            .map_err(|e| format!("turn {}: {}", g.turn(), e.message))?;
        drop(d);
        match stop {
            Stop::SeatLimit | Stop::GameOver => {}
            other => {
                return Err(format!("turn {}: a driven game stopped with {other:?}", g.turn()));
            }
        }
        if after_round(g, before, hook)? {
            played += 1;
        }
    }
    Ok(played)
}

/// Passes `rounds` rounds of `g`: every seat ends its turn with nothing played, a turn at a
/// time, as a host that ends every turn at once would; `hook` runs after every round. Returns
/// the rounds played, fewer if the game ends first.
///
/// # Errors
/// A refusal by the engine, or the hook's error.
pub fn pass_rounds(g: &mut Game, rounds: u32, hook: &mut Hook<'_>) -> Result<u32, String> {
    let mut played = 0;
    // A round ends after at most every player's turn, and a game whose last major is gone ends.
    let mut guard = (rounds as usize + 1) * (g.state().players().len() + 1);
    while played < rounds && g.phase() == Phase::Playing && guard > 0 {
        guard -= 1;
        let before = rounds_taken(g);
        g.end_turn(g.current()).map_err(|e| format!("turn {}: {}", g.turn(), e.message))?;
        if after_round(g, before, hook)? {
            played += 1;
        }
    }
    if played < rounds && g.phase() == Phase::Playing {
        return Err(format!(
            "turn {}: {played} of {rounds} rounds ended; the turns stall",
            g.turn()
        ));
    }
    Ok(played)
}

/// Saves `g` as a host saves it, the state as JSON and the journal's new chunk added to
/// `chunks`, and puts the game loaded back from them in its place, with its chain resumed and
/// its checks as they were: what a host that saves every round and plays on from the save does.
///
/// # Errors
/// If the game does not save, the save does not load, the history does not rebuild whole, or
/// the loaded state is not the saved one.
pub fn save_and_load(g: &mut Game, chunks: &mut Vec<Vec<u8>>) -> Result<(), String> {
    let at = g.turn();
    // The journal first, as a host appends it before it writes the snapshot (DESIGN.md 4.11):
    // the snapshot's heads then count the chunk just taken.
    let chunk = g.take_journal_chunk().map_err(|e| format!("turn {at}: no journal: {e}"))?;
    chunks.extend(chunk.map(|c| c.json));
    let json = g.snapshot().to_json().map_err(|e| format!("turn {at}: does not save: {e}"))?;
    let mut it = chunks.iter().map(Vec::as_slice);
    let (mut back, report) = Game::load(g.rules(), &json, &mut it)
        .map_err(|e| format!("turn {at}: the save does not load: {e}"))?;
    if report.chronicle_incomplete {
        return Err(format!("turn {at}: the journal did not rebuild the history whole"));
    }
    if back.digest().ok() != g.digest().ok() {
        return Err(format!("turn {at}: the loaded state is not the one saved"));
    }
    back.set_debug_options(g.debug_options());
    back.set_chain(g.chain().map(|c| DigestChain::resume(c.head(), c.rounds())));
    *g = back;
    Ok(())
}

/// A hook that keeps each round.
pub fn keep(rounds: &mut Vec<Round>) -> impl FnMut(&mut Game, Round) -> Result<(), String> + '_ {
    move |_, r| {
        rounds.push(r);
        Ok(())
    }
}

//! The golden sets of package 1c-10 (DESIGN.md 9.6), whole games and loaded states:
//! - **`load.json`**: the twelve committed refcheck fixtures after `Game::from_python`'s settle,
//!   the counterpart of Python's refresh on load: each state's digest, with a few counts beside
//!   it. A digest that moves (and `convert.json`'s does not) is a change to the settle on load:
//!   sight rebuilt from nothing, happiness committed, the caches. Each loaded state must also
//!   keep every invariant and agree with a cold rebuild of its caches.
//! - **`pass.json`**: twenty rounds passed from three of those fixtures, every seat ending its
//!   turn with nothing played, each round's digest chained (one row per round). A digest that
//!   moves is a change to a turn's stages: what the engine does for everyone every turn.
//! - **`random.json`**: games on generated maps with `RandomAgent` in every seat (duel for 200
//!   turns twice, small for 120 twice, standard for 60, large for 30), each round's digest
//!   chained. A digest that moves is a change to anything the agent's actions reach, which is
//!   every action; one that differs between targets is a determinism bug in it.
//!
//! `pass` and `random` depend on every stage of setup and of a turn, as `turns.json` does, so
//! `golden bless` refuses them while one is pending (none is from package 1c-10 on, when `cargo
//! xtask check` fails on any). Written by `golden bless` when the engine changes on purpose.

use citar_engine::game::DebugOptions;
use citar_engine::rules::Ruleset;
use citar_engine::save::canon;
use citar_engine::state::Phase;
use serde_json::{Value, json};

use super::{SetReport, capped, diff_rows, digest_of, read_committed, render_rows, turns};
use crate::fixtures;
use crate::games::{self, Round};

/// The fixtures `pass.json` passes from: early and late, a duel, raging barbarians, and a small
/// map at turn 280 with the most cities.
pub const PASS_FROM: [&str; 3] = [
    "duel-continents-normal/t50",
    "small-pangaea-raging/t50",
    "small-continents-normal-s1025/t280",
];

/// How many rounds `pass.json` passes.
pub const PASS_ROUNDS: u32 = 20;

/// The games `random.json` plays: (lobby size, map type, edges, seed, turns).
pub const RANDOM_GAMES: [(&str, &str, &str, u64, u32); 6] = [
    ("duel", "continents", "wrap_x", 101, 200),
    ("duel", "pangaea", "ice_caps", 102, 200),
    ("small", "fractal", "boxed", 103, 120),
    ("small", "archipelago", "wrap_x", 104, 120),
    ("standard", "continents", "ice_caps", 105, 60),
    ("large", "pangaea", "wrap_x", 106, 30),
];

/// The name a random game has in the file and its chain's spec.
#[must_use]
pub fn random_name(size: &str, map_type: &str, seed: u64) -> String {
    format!("random-{size}-{map_type}-s{seed}")
}

/// What the games' checks found, one problem per line, prefixed with the game's name.
fn soundness(name: &str, g: &mut citar_engine::game::Game) -> Vec<String> {
    games::problems(g).into_iter().map(|p| format!("{name}: {p}")).collect()
}

/// The rows of a chained game: one per round, `[name, turn, digest]`.
fn round_rows(name: &str, rounds: &[Round]) -> impl Iterator<Item = Value> {
    rounds.iter().map(move |(turn, d)| json!([name, turn, d.to_hex()]))
}

// ---- load.json --------------------------------------------------------------------------------

fn load_answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    let found = match fixtures::committed() {
        Ok(f) => f,
        Err(e) => return (json!({"format": 1}), vec![e]),
    };
    for f in &found {
        let mut g = match games::from_fixture(f, b"golden:load", DebugOptions::default()) {
            Ok(g) => g,
            Err(e) => {
                problems.push(format!("{} does not load: {e}", f.name));
                continue;
            }
        };
        let st = g.state();
        match (g.digest(), canon::state_bytes(st)) {
            (Ok(d), Ok(bytes)) => rows.push(json!([
                f.name,
                d.to_hex(),
                bytes.len(),
                [st.players().len(), st.cities().len(), st.units().len()],
                st.players().iter().map(|(_, p)| p.explored.len()).sum::<usize>(),
                g.chronicle().events().len(),
            ])),
            (Err(e), _) | (_, Err(e)) => problems.push(format!("{} does not digest: {e}", f.name)),
        }
        problems.extend(soundness(&f.name, &mut g));
        problems.extend(g.verify_caches().into_iter().map(|e| format!("{}: caches: {e}", f.name)));
    }
    let v = json!({
        "format": 1,
        "states": "[fixture, digest after Game::from_python's settle, canonical length, [players, cities, units], explored tiles (summed over players), events]",
        "ruleset_id": r.id().to_hex(),
        "rows": rows,
    });
    (v, problems)
}

/// The `load` set, checked against `load.json`.
#[must_use]
pub fn check_load() -> SetReport {
    let (got, problems) = load_answers();
    compare("load", got, problems, Vec::new(), &["rows"])
}

// ---- pass.json --------------------------------------------------------------------------------

fn pass_answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let mut games_rows = Vec::new();
    let mut rounds_rows = Vec::new();
    let mut problems = Vec::new();
    let found = match fixtures::committed() {
        Ok(f) => f,
        Err(e) => return (json!({"format": 1}), vec![e]),
    };
    for name in PASS_FROM {
        let Some(f) = found.iter().find(|f| f.name == name) else {
            problems.push(format!("no committed fixture {name}"));
            continue;
        };
        let spec = format!("golden:pass:{name}");
        let mut g = match games::from_fixture(f, spec.as_bytes(), DebugOptions::default()) {
            Ok(g) => g,
            Err(e) => {
                problems.push(format!("{name} does not load: {e}"));
                continue;
            }
        };
        let mut rounds = Vec::new();
        let start = g.turn();
        if let Err(e) = games::pass_rounds(&mut g, PASS_ROUNDS, &mut games::keep(&mut rounds)) {
            problems.push(format!("{name}: {e}"));
        }
        let head = g.chain().map(|c| c.head().to_hex());
        games_rows.push(json!([
            name,
            start,
            rounds.len(),
            head,
            g.chronicle().events().len(),
            g.phase() == Phase::Over,
        ]));
        rounds_rows.extend(round_rows(name, &rounds));
        problems.extend(soundness(name, &mut g));
    }
    let v = json!({
        "format": 1,
        "games": "[fixture, turn it starts on, rounds passed, chain head, events, over]",
        "rounds": "[fixture, the round's turn, the state's digest as it ended]",
        "ruleset_id": r.id().to_hex(),
        "game_rows": games_rows,
        "round_rows": rounds_rows,
    });
    (v, problems)
}

/// The `pass` set, checked against `pass.json` once nothing it depends on is pending.
#[must_use]
pub fn check_pass() -> SetReport {
    let (got, problems) = pass_answers();
    compare("pass", got, problems, turns::waiting(), &["game_rows", "round_rows"])
}

// ---- random.json ------------------------------------------------------------------------------

fn random_answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let mut games_rows = Vec::new();
    let mut rounds_rows = Vec::new();
    let mut problems = Vec::new();
    for (size, ty, edges, seed, turns) in RANDOM_GAMES {
        let name = random_name(size, ty, seed);
        let settings = games::random_settings(size, ty, edges, seed, turns);
        let mut g = match games::new_game(&settings, name.as_bytes(), DebugOptions::default()) {
            Ok(g) => g,
            Err(e) => {
                problems.push(format!("{name} does not set up: {e}"));
                continue;
            }
        };
        let mut agents = games::agents_for(&g);
        let mut rounds = Vec::new();
        let played = games::play_random(&mut g, &mut agents, turns, &mut games::keep(&mut rounds));
        if let Err(e) = played {
            problems.push(format!("{name}: {e}"));
        }
        let st = g.state();
        let head = g.chain().map(|c| c.head().to_hex());
        let digest = g.digest().map(|d| d.to_hex()).unwrap_or_default();
        let winner = st.clock().winner.map(|p| p.0);
        games_rows.push(json!([
            name,
            seed,
            [g.grid().width(), g.grid().height()],
            rounds.len(),
            head,
            digest,
            [st.cities().len(), st.units().len(), st.diplo().deals.len()],
            g.chronicle().events().len(),
            winner,
        ]));
        rounds_rows.extend(round_rows(&name, &rounds));
        problems.extend(soundness(&name, &mut g));
    }
    let v = json!({
        "format": 1,
        "games": "[name, seed, [width, height], rounds, chain head, final digest, [cities, units, deals], events, winner]",
        "rounds": "[name, the round's turn, the state's digest as it ended]",
        "ruleset_id": r.id().to_hex(),
        "game_rows": games_rows,
        "round_rows": rounds_rows,
    });
    (v, problems)
}

/// The `random` set, checked against `random.json` once nothing it depends on is pending.
#[must_use]
pub fn check_random() -> SetReport {
    let (got, problems) = random_answers();
    compare("random", got, problems, turns::waiting(), &["game_rows", "round_rows"])
}

// ---- Shared -----------------------------------------------------------------------------------

/// Checks a set against its file: the ruleset id and every list, unless the set waits for a
/// stage, when it is computed but not compared.
fn compare(
    name: &'static str,
    got: Value,
    mut problems: Vec<String>,
    waiting: Vec<String>,
    lists: &[&str],
) -> SetReport {
    let file = format!("{name}.json");
    if waiting.is_empty() {
        match read_committed(&file) {
            Err(e) => problems.push(e),
            Ok(want) => {
                if want.get("ruleset_id") != got.get("ruleset_id") {
                    problems.push(format!("{file}: the ruleset id differs from this build's"));
                }
                for key in lists {
                    problems.extend(diff_rows(&file, key, want.get(*key), &got[*key]));
                }
            }
        }
    }
    SetReport { name, computed: digest_of(&got), problems: capped(problems), waiting }
}

/// The files `golden bless` writes: `load.json` always, `pass.json` and `random.json` once
/// nothing they depend on is pending.
#[must_use]
pub fn blessed() -> Vec<(&'static str, String)> {
    let mut out = vec![("load.json", render_rows(&load_answers().0, &["rows"]))];
    if turns::waiting().is_empty() {
        let lists = ["game_rows", "round_rows"];
        out.push(("pass.json", render_rows(&pass_answers().0, &lists)));
        out.push(("random.json", render_rows(&random_answers().0, &lists)));
    }
    out
}

/// Why `golden bless` refuses `pass.json` and `random.json`, if it does.
#[must_use]
pub fn refusals() -> Vec<(&'static str, String)> {
    turns::refusal()
        .map(|why| {
            let why = why.replacen("turns.json", "the whole-game sets", 1);
            vec![("pass.json", why.clone()), ("random.json", why)]
        })
        .unwrap_or_default()
}

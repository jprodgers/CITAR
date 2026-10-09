//! The golden sets of whole games and loaded states (DESIGN.md 9.6). Package 1c-10 brings:
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
//! Package 1e-02 adds the long set, which the nightly run checks on every target and the
//! pull-request run does not (`golden check --long`):
//! - **`long.json`**: a Quick game's 330 turns with `RandomAgent` in every seat on every map
//!   size, duel to gargantuan, two more on the kitchen-sink ruleset (the unique types the shipped
//!   ruleset never uses), and the three late fixtures passed on toward their end: about 80
//!   seconds a target in the ci profile on the laptop, where the short sets take 5.
//!
//! The whole-game sets share [`Play`], which also lets `golden dump` play any of their games to
//! any round, and [`played`], which watches each round against the committed file when
//! `golden check --states` asks for the state where a game first leaves it ([`divergence`]).
//!
//! `pass`, `random` and `long` depend on every stage of setup and of a turn, as `turns.json`
//! does, so `golden bless` refuses them while one is pending (none is from package 1c-10 on,
//! when `cargo xtask check` fails on any). Written by `golden bless` when the engine changes on
//! purpose.
//!
//! [`divergence`]: super::divergence

use std::sync::Arc;

use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::game::{DebugOptions, Game};
use citar_engine::rules::Ruleset;
use citar_engine::save::canon;
use citar_engine::state::Phase;
use serde_json::{Value, json};

use super::divergence::{self, Watch};
use super::{SetReport, diff_rows, read_committed, render_rows, turns};
use crate::fixtures::{self, Fixture};
use crate::games::{self, Round};

/// A game a whole-game set plays, round after round, each round's digest chained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Play {
    /// `RandomAgent` in every major's seat on a generated map, to its turn limit.
    Random {
        size: &'static str,
        map_type: &'static str,
        edges: &'static str,
        seed: u64,
        turns: u32,
    },
    /// The same on the kitchen-sink ruleset, the Kitchen Sink nation in the first seat
    /// (`games::kitchen_sink_game`).
    KitchenSink { size: &'static str, seed: u64, turns: u32 },
    /// A committed fixture, every seat ending its turn with nothing played, for some rounds.
    Pass { fixture: &'static str, rounds: u32 },
    /// `basic-1` in every major's seat on a generated map with the statistical baseline's
    /// settings ([`baseline_settings`]), each seat's aggression the baseline's
    /// ([`baseline_aggression`]), for `rounds` rounds or to the game's end at Quick's 330 turns.
    Bots { size: &'static str, map_type: &'static str, seed: u64, rounds: u32 },
    /// A committed fixture played on by `basic-1` in every major's seat, for some rounds.
    BotPass { fixture: &'static str, rounds: u32 },
}

impl Play {
    /// The game's name in its set's rows.
    #[must_use]
    pub fn name(&self, set: &str) -> String {
        match *self {
            Self::Random { size, map_type, seed, .. } => format!("{set}-{size}-{map_type}-s{seed}"),
            Self::KitchenSink { size, seed, .. } => format!("{set}-kitchen-sink-{size}-s{seed}"),
            Self::Pass { fixture, .. } => fixture.to_owned(),
            Self::Bots { size, map_type, seed, .. } => {
                format!("{set}-basic1-{size}-{map_type}-s{seed}")
            }
            Self::BotPass { fixture, .. } => format!("basic1-{fixture}"),
        }
    }

    /// Whether bots play it: its rounds then carry the seats' memory hashes.
    #[must_use]
    pub const fn has_bots(&self) -> bool {
        matches!(self, Self::Bots { .. } | Self::BotPass { .. })
    }

    /// The game as it starts, its chain of rounds keyed by its set and name.
    ///
    /// # Errors
    /// Settings the engine refuses, or a fixture that cannot be found or read.
    pub fn start(&self, set: &str, found: &[Fixture]) -> Result<Game, String> {
        let name = self.name(set);
        let debug = DebugOptions::default();
        match *self {
            Self::Random { size, map_type, edges, seed, turns } => {
                let settings = games::random_settings(size, map_type, edges, seed, turns);
                games::new_game(&settings, name.as_bytes(), debug)
            }
            Self::KitchenSink { size, seed, turns } => {
                games::kitchen_sink_game(size, seed, turns, name.as_bytes(), debug)
            }
            Self::Pass { fixture, .. } | Self::BotPass { fixture, .. } => {
                let f = found
                    .iter()
                    .find(|f| f.name == fixture)
                    .ok_or_else(|| format!("no committed fixture {fixture}"))?;
                games::from_fixture(f, format!("golden:{set}:{name}").as_bytes(), debug)
            }
            Self::Bots { size, map_type, seed, .. } => {
                games::new_game(&baseline_settings(size, map_type, seed), name.as_bytes(), debug)
            }
        }
    }

    /// Plays the game on from `g`, calling `hook` after every round.
    ///
    /// # Errors
    /// The engine's refusal, a stall, or the hook's error.
    pub fn play(&self, g: &mut Game, hook: &mut games::Hook<'_>) -> Result<u32, String> {
        match *self {
            Self::Random { turns, .. } | Self::KitchenSink { turns, .. } => {
                let mut agents = games::agents_for(g);
                games::play_random(g, &mut agents, turns, hook)
            }
            Self::Pass { rounds, .. } => games::pass_rounds(g, rounds, hook),
            Self::Bots { rounds, .. } | Self::BotPass { rounds, .. } => {
                let mut bots = bot_seats(g);
                games::play_random(g, &mut bots, rounds, hook)
            }
        }
    }
}

/// The settings of the statistical baseline's games (`citar_sim::baseline::GameSpec::config`,
/// `common.game_config`): every seat the bot controller's with its nation drawn, Prince, Quick,
/// barbarians normal, the map size's own number of players, and Quick's turn limit.
#[must_use]
pub fn baseline_settings(size: &str, map_type: &str, seed: u64) -> Value {
    let c = Ruleset::shared().constants();
    let players = c.map_size_id(size).map_or(2, |id| usize::from(c.map_sizes[id].players));
    let seat = json!({"controller": "bot", "nation": null});
    json!({
        "seed": seed,
        "map_size": size,
        "map_type": map_type,
        "barbarians": "normal",
        "speed": "Quick",
        "difficulty": "Prince",
        "players": vec![seat; players],
    })
}

/// The aggression the statistical baseline gives seat `pid` of the game of `seed`:
/// `0.25 + 0.5 * ((pid * 37 + seed) % 10) / 9` (`common.py:292`, and
/// `citar_sim::baseline::aggression`, which testkit may not depend on), so that a golden game's
/// bots play with the spread of aggressions the baseline's do.
#[must_use]
pub fn baseline_aggression(pid: PlayerId, seed: u64) -> f64 {
    let step = (u64::from(pid.0) * 37 + seed % 10) % 10;
    // step < 10, exact in an f64.
    0.25 + 0.5 * (step as f64) / 9.0
}

/// A `basic-1` bot at its defaults for every player of `g` (only the majors' are seated), each
/// with the baseline's aggression for its seat and the game's seed.
fn bot_seats(g: &Game) -> Vec<Bot> {
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
    let seed = g.state().seed();
    (0..g.state().players().len())
        .map(|i| {
            let pid = PlayerId(u8::try_from(i).unwrap_or(u8::MAX));
            let a = baseline_aggression(pid, seed);
            Bot::new(Arc::new(BotSpec::new(VersionId::Basic1, Arc::clone(&tuning), None, Some(a))))
        })
        .collect()
}

/// A short hash of each major's driver memory, in seat order (`null` for a seat whose driver
/// kept none): where a game that leaves its file first does so in a bot's plans rather than in
/// the state the bots act on, these name the seat.
#[must_use]
pub fn memory_hashes(g: &Game) -> Value {
    g.majors(false)
        .map(|p| {
            p.seat().driver().map_or(Value::Null, |m| {
                let mut h = blake3::Hasher::new();
                h.update(&m.kind().to_le_bytes());
                h.update(&m.version().to_le_bytes());
                h.update(&(m.bytes().len() as u64).to_le_bytes());
                h.update(m.bytes());
                Value::from(h.finalize().to_hex()[..16].to_owned())
            })
        })
        .collect()
}

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

/// The games of `long.json`: every map size to a Quick game's 330 turns, two kitchen-sink games,
/// and the three late fixtures passed on.
pub const LONG_GAMES: [Play; 12] = [
    Play::Random { size: "duel", map_type: "fractal", edges: "wrap_y", seed: 201, turns: 330 },
    Play::Random {
        size: "small",
        map_type: "inland_sea",
        edges: "wrap_both",
        seed: 202,
        turns: 330,
    },
    Play::Random {
        size: "standard",
        map_type: "archipelago",
        edges: "boxed",
        seed: 203,
        turns: 330,
    },
    Play::Random { size: "large", map_type: "continents", edges: "wrap_x", seed: 204, turns: 330 },
    Play::Random { size: "huge", map_type: "fractal", edges: "ice_caps", seed: 205, turns: 330 },
    Play::Random {
        size: "gargantuan",
        map_type: "pangaea",
        edges: "wrap_x",
        seed: 206,
        turns: 330,
    },
    Play::KitchenSink { size: "duel", seed: 207, turns: 330 },
    Play::KitchenSink { size: "small", seed: 208, turns: 330 },
    Play::Pass { fixture: "small-continents-normal-s1025/t280", rounds: 60 },
    Play::Pass { fixture: "standard-pangaea-normal-s1031/t120", rounds: 100 },
    Play::Pass { fixture: "scenario-small-continents-s3001/t61", rounds: 100 },
    // Package 2-07: a whole 4-bot game, the baseline's game 4 (seed 5004, fractal), to its end.
    Play::Bots { size: "small", map_type: "fractal", seed: 5004, rounds: 330 },
];

/// The games of `bot.json` (package 2-07): `basic-1` in every major's seat, a duel for 120
/// rounds, two small games for 100 (the baseline's games 1 and 2: pangaea and archipelago), and
/// the late fixture played on for 10 rounds.
pub const BOT_GAMES: [Play; 4] = [
    Play::Bots { size: "duel", map_type: "continents", seed: 5100, rounds: 120 },
    Play::Bots { size: "small", map_type: "pangaea", seed: 5001, rounds: 100 },
    Play::Bots { size: "small", map_type: "archipelago", seed: 5002, rounds: 100 },
    Play::BotPass { fixture: "small-continents-normal-s1025/t280", rounds: 10 },
];

/// The games of `pass.json`.
#[must_use]
pub fn pass_games() -> Vec<Play> {
    PASS_FROM.iter().map(|&fixture| Play::Pass { fixture, rounds: PASS_ROUNDS }).collect()
}

/// The games of `random.json`.
#[must_use]
pub fn random_games() -> Vec<Play> {
    RANDOM_GAMES
        .iter()
        .map(|&(size, map_type, edges, seed, turns)| Play::Random {
            size,
            map_type,
            edges,
            seed,
            turns,
        })
        .collect()
}

/// The name a random game has in the file and its chain's spec.
#[must_use]
pub fn random_name(size: &str, map_type: &str, seed: u64) -> String {
    format!("random-{size}-{map_type}-s{seed}")
}

/// What the games' checks found, one problem per line, prefixed with the game's name.
fn soundness(name: &str, g: &mut Game) -> Vec<String> {
    games::problems(g).into_iter().map(|p| format!("{name}: {p}")).collect()
}

/// The rows of a chained game: one per round, `[name, turn, digest]`, and for a game bots play
/// the seats' memory hashes after it (`[name, turn, digest, memories]`).
fn round_rows<'a>(
    name: &'a str,
    rounds: &'a [Round],
    memories: &'a [Value],
) -> impl Iterator<Item = Value> + 'a {
    rounds.iter().enumerate().map(move |(i, (turn, d))| match memories.get(i) {
        Some(m) => json!([name, turn, d.to_hex(), m]),
        None => json!([name, turn, d.to_hex()]),
    })
}

/// The committed file, if `golden check --states` asked for divergent states: what each game's
/// rounds are watched against.
fn watched(file: &str) -> Option<Value> {
    divergence::dir().and_then(|_| read_committed(file).ok())
}

/// A game a set played: the game as it ended, the turn it started on, its rounds, and what went
/// wrong.
pub struct Played {
    pub game: Option<Game>,
    pub start: Turn,
    pub rounds: Vec<Round>,
    /// For a game bots play, each round's [`memory_hashes`]; empty for any other.
    pub memories: Vec<Value>,
    pub problems: Vec<String>,
}

/// Plays one game of set `set`, watching its rounds against `committed` (the file's round
/// rows, when divergent states were asked for).
#[must_use]
pub fn played(set: &str, play: &Play, found: &[Fixture], committed: Option<&Value>) -> Played {
    let name = play.name(set);
    let mut g = match play.start(set, found) {
        Ok(g) => g,
        Err(e) => {
            return Played {
                game: None,
                start: 0,
                rounds: Vec::new(),
                memories: Vec::new(),
                problems: vec![format!("{name} does not set up: {e}")],
            };
        }
    };
    let start = g.turn();
    let mut watch =
        committed.and_then(|rows| Watch::new(set, &name, Watch::rows_of(Some(rows), &name)));
    let mut rounds = Vec::new();
    let mut memories = Vec::new();
    let mut problems = Vec::new();
    let bots = play.has_bots();
    let mut hook = |g: &mut Game, (turn, d): Round| -> Result<(), String> {
        if let Some(w) = watch.as_mut() {
            w.round(g, turn, &d);
        }
        rounds.push((turn, d));
        if bots {
            memories.push(memory_hashes(g));
        }
        Ok(())
    };
    if let Err(e) = play.play(&mut g, &mut hook) {
        problems.push(format!("{name}: {e}"));
    }
    problems.extend(watch.map(|w| w.problems).unwrap_or_default());
    problems.extend(soundness(&name, &mut g));
    Played { game: Some(g), start, rounds, memories, problems }
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
    let committed = watched("load.json");
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
            (Ok(d), Ok(bytes)) => {
                let row = json!([
                    f.name,
                    d.to_hex(),
                    bytes.len(),
                    [st.players().len(), st.cities().len(), st.units().len()],
                    st.players().iter().map(|(_, p)| p.explored.len()).sum::<usize>(),
                    g.chronicle().events().len(),
                ]);
                if let Some(want) = committed.as_ref() {
                    let theirs = row_named(want, "rows", &f.name);
                    if theirs != Some(&row) {
                        let digest = theirs.and_then(|r| r.get(1)).and_then(Value::as_str);
                        problems.extend(divergence::whole(
                            "load",
                            &f.name,
                            &g,
                            digest,
                            &d.to_hex(),
                        ));
                    }
                }
                rows.push(row);
            }
            (Err(e), _) | (_, Err(e)) => problems.push(format!("{} does not digest: {e}", f.name)),
        }
        problems.extend(soundness(&f.name, &mut g));
        problems.extend(
            crate::checks::caches(&g).into_iter().map(|e| format!("{}: caches: {e}", f.name)),
        );
    }
    let v = json!({
        "format": 1,
        "states": "[fixture, digest after Game::from_python's settle, canonical length, [players, cities, units], explored tiles (summed over players), events]",
        "ruleset_id": r.id().to_hex(),
        "rows": rows,
    });
    (v, problems)
}

/// The row of a file's list `key` whose first cell is `name`.
fn row_named<'v>(file: &'v Value, key: &str, name: &str) -> Option<&'v Value> {
    file.get(key)?.as_array()?.iter().find(|r| r.get(0).and_then(Value::as_str) == Some(name))
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
    let committed = watched("pass.json");
    for play in pass_games() {
        let name = play.name("pass");
        let p = played("pass", &play, &found, committed.as_ref().and_then(|c| c.get("round_rows")));
        problems.extend(p.problems);
        let Some(g) = p.game else { continue };
        let head = g.chain().map(|c| c.head().to_hex());
        games_rows.push(json!([
            name,
            p.start,
            p.rounds.len(),
            head,
            g.chronicle().events().len(),
            g.phase() == Phase::Over,
        ]));
        rounds_rows.extend(round_rows(&name, &p.rounds, &p.memories));
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
    let committed = watched("random.json");
    for play in random_games() {
        let Play::Random { seed, .. } = play else { continue };
        let name = play.name("random");
        let p = played("random", &play, &[], committed.as_ref().and_then(|c| c.get("round_rows")));
        problems.extend(p.problems);
        let Some(g) = p.game else { continue };
        let st = g.state();
        let head = g.chain().map(|c| c.head().to_hex());
        let digest = g.digest().map(|d| d.to_hex()).unwrap_or_default();
        let winner = st.clock().winner.map(|p| p.0);
        games_rows.push(json!([
            name,
            seed,
            [g.grid().width(), g.grid().height()],
            p.rounds.len(),
            head,
            digest,
            [st.cities().len(), st.units().len(), st.diplo().deals.len()],
            g.chronicle().events().len(),
            winner,
        ]));
        rounds_rows.extend(round_rows(&name, &p.rounds, &p.memories));
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

// ---- long.json --------------------------------------------------------------------------------

fn long_answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let mut games_rows = Vec::new();
    let mut rounds_rows = Vec::new();
    let mut problems = Vec::new();
    let found = match fixtures::committed() {
        Ok(f) => f,
        Err(e) => return (json!({"format": 1}), vec![e]),
    };
    let committed = watched("long.json");
    for play in LONG_GAMES {
        let name = play.name("long");
        let p = played("long", &play, &found, committed.as_ref().and_then(|c| c.get("round_rows")));
        problems.extend(p.problems);
        let Some(g) = p.game else { continue };
        let st = g.state();
        games_rows.push(json!([
            name,
            p.start,
            p.rounds.len(),
            g.chain().map(|c| c.head().to_hex()),
            g.digest().map(|d| d.to_hex()).unwrap_or_default(),
            [st.cities().len(), st.units().len(), st.diplo().deals.len()],
            g.chronicle().events().len(),
            g.phase() == Phase::Over,
            st.clock().winner.map(|p| p.0),
        ]));
        rounds_rows.extend(round_rows(&name, &p.rounds, &p.memories));
    }
    let v = json!({
        "format": 1,
        "games": "[name, turn it starts on, rounds, chain head, final digest, [cities, units, deals], events, over, winner]",
        "rounds": "[name, the round's turn, the state's digest as it ended]",
        "ruleset_id": r.id().to_hex(),
        "game_rows": games_rows,
        "round_rows": rounds_rows,
    });
    (v, problems)
}

/// The `long` set, checked against `long.json` once nothing it depends on is pending: the
/// nightly run's (`golden check --long`).
#[must_use]
pub fn check_long() -> SetReport {
    let (got, problems) = long_answers();
    compare("long", got, problems, turns::waiting(), &["game_rows", "round_rows"])
}

// ---- bot.json ---------------------------------------------------------------------------------

fn bot_answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let mut games_rows = Vec::new();
    let mut rounds_rows = Vec::new();
    let mut problems = Vec::new();
    let found = match fixtures::committed() {
        Ok(f) => f,
        Err(e) => return (json!({"format": 1}), vec![e]),
    };
    let committed = watched("bot.json");
    for play in BOT_GAMES {
        let name = play.name("bot");
        let p = played("bot", &play, &found, committed.as_ref().and_then(|c| c.get("round_rows")));
        problems.extend(p.problems);
        let Some(g) = p.game else { continue };
        let st = g.state();
        games_rows.push(json!([
            name,
            p.start,
            p.rounds.len(),
            g.chain().map(|c| c.head().to_hex()),
            g.digest().map(|d| d.to_hex()).unwrap_or_default(),
            [st.cities().len(), st.units().len(), st.diplo().deals.len()],
            g.chronicle().events().len(),
            g.phase() == Phase::Over,
            st.clock().winner.map(|p| p.0),
            memory_hashes(&g),
        ]));
        rounds_rows.extend(round_rows(&name, &p.rounds, &p.memories));
    }
    let v = json!({
        "format": 1,
        "bot": "basic-1 at its defaults in every major's seat, each seat's aggression the statistical baseline's",
        "games": "[name, turn it starts on, rounds, chain head, final digest, [cities, units, deals], events, over, winner, each major's memory hash at the end]",
        "rounds": "[name, the round's turn, the state's digest as it ended, each major's memory hash then]",
        "ruleset_id": r.id().to_hex(),
        "game_rows": games_rows,
        "round_rows": rounds_rows,
    });
    (v, problems)
}

/// The `bot` set, checked against `bot.json` once nothing it depends on is pending: the bot's
/// determinism on every target (package 2-07). A change to the bot that plays differently moves
/// its rows, and is blessed on purpose, as a change to the engine's rules is.
#[must_use]
pub fn check_bot() -> SetReport {
    let (got, problems) = bot_answers();
    compare("bot", got, problems, turns::waiting(), &["game_rows", "round_rows"])
}

/// The file `golden bless long` writes, once nothing it depends on is pending.
#[must_use]
pub fn blessed_long() -> Vec<(&'static str, String)> {
    if turns::waiting().is_empty() {
        vec![("long.json", render_rows(&long_answers().0, &["game_rows", "round_rows"]))]
    } else {
        Vec::new()
    }
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
    SetReport::new(name, &got, lists, problems, waiting)
}

/// The files `golden bless` writes: `load.json` always, `pass.json`, `random.json` and
/// `bot.json` once nothing they depend on is pending. `long.json` is [`blessed_long`]'s.
#[must_use]
pub fn blessed() -> Vec<(&'static str, String)> {
    let mut out = vec![("load.json", render_rows(&load_answers().0, &["rows"]))];
    if turns::waiting().is_empty() {
        let lists = ["game_rows", "round_rows"];
        out.push(("pass.json", render_rows(&pass_answers().0, &lists)));
        out.push(("random.json", render_rows(&random_answers().0, &lists)));
        out.push(("bot.json", render_rows(&bot_answers().0, &lists)));
    }
    out
}

/// Why `golden bless` refuses `pass.json`, `random.json`, `bot.json` and `long.json`, if it
/// does.
#[must_use]
pub fn refusals() -> Vec<(&'static str, String)> {
    turns::refusal()
        .map(|why| {
            let why = why.replacen("turns.json", "the whole-game sets", 1);
            vec![
                ("pass.json", why.clone()),
                ("random.json", why.clone()),
                ("bot.json", why.clone()),
                ("long.json", why),
            ]
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_random_set_names_its_games_as_before() {
        for (play, &(size, map_type, _, seed, _)) in random_games().iter().zip(&RANDOM_GAMES) {
            assert_eq!(play.name("random"), random_name(size, map_type, seed));
        }
    }

    #[test]
    fn the_long_set_plays_every_size_and_names_each_game_once() {
        let r = Ruleset::shared();
        for (_, m) in r.constants().map_sizes.iter() {
            let key = &*m.key;
            assert!(
                LONG_GAMES.iter().any(|p| matches!(p, Play::Random { size, .. } if *size == key)),
                "{key}"
            );
        }
        let mut names: Vec<String> = LONG_GAMES.iter().map(|p| p.name("long")).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), LONG_GAMES.len());
    }
}

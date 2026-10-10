//! The chaos driver (DESIGN.md 9.5): whole games of seat turns with tool calls of every kind,
//! right and wrong, mixed in ([`ActionSpec`]s drawn at random), every step checked for the
//! properties P1 to P7 ([`Run`]), under `catch_unwind`. The seats are `RandomAgent`s, `basic-1`
//! bots or half of each (the run's lineup, [`Options::drivers`]; package 2-13): with bots, the
//! calls put the bot's own empire in states no game of bots alone reaches.
//!
//! Each failure, a broken property or a panic, is kept as a [`Replay`]: how the game started, the
//! bug planted if any, every step taken up to the one that failed, and what failed. The `chaos`
//! binary writes each to `target/chaos/`, and `chaos --replay FILE` plays it again ([`replay`]):
//! the steps are recorded, not drawn, so a replay makes the same calls, and the engine, which
//! keys every draw from the game's seed, fails at the same step in the same way.
//!
//! Games start from generated maps (duel and small, of every map type, one in four on the
//! kitchen-sink ruleset with the Kitchen Sink nation among the civilizations), or with
//! `--from-fixtures` from the committed fixtures (and the local corpus when
//! `CITAR_REFCHECK_CORPUS` names it), each of whose cities is first flagged for a citizen
//! recheck, so that the citizen oracle covers every city (DESIGN.md 6.8).

use std::panic::{AssertUnwindSafe, catch_unwind};

use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::Game;
use citar_engine::game::seeded::{self, SeededBug};
use citar_engine::state::Phase;
use serde::{Deserialize, Serialize};

use crate::fixtures::{self, Fixture};
use crate::games;
use crate::spec::ActionSpec;
use crate::stability::{Options, Property, Run, Step};

/// The version of the replay file format. Version 2 (package 2-13) records the lineup in the
/// options; version 1 had none, and its seats were `RandomAgent`s, which is what a version-1
/// file reads as.
pub const REPLAY_VERSION: u32 = 2;

/// How a chaos game starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Start {
    /// A new game on a generated map, from the lobby's settings.
    Generated { size: String, map_type: String, edges: String, seed: u64, turn_limit: u32 },
    /// A new game on the kitchen-sink ruleset, the Kitchen Sink nation in the first seat
    /// (`games::kitchen_sink_game`), whose extra unique types the shipped ruleset never meets.
    KitchenSink { size: String, seed: u64, turn_limit: u32 },
    /// A committed fixture, or one of the local corpus, by name (`<case>/t<turn>`).
    Fixture { name: String },
}

impl Start {
    /// The game this start gives, as chaos plays it: every city of a fixture flagged for a
    /// citizen recheck first.
    ///
    /// # Errors
    /// Settings the engine refuses, or a fixture that cannot be found or read.
    pub fn game(&self) -> Result<Game, String> {
        match self {
            Self::Generated { size, map_type, edges, seed, turn_limit } => {
                let settings = games::random_settings(size, map_type, edges, *seed, *turn_limit);
                games::new_game(&settings, b"chaos", citar_engine::game::DebugOptions::default())
            }
            Self::KitchenSink { size, seed, turn_limit } => games::kitchen_sink_game(
                size,
                *seed,
                *turn_limit,
                b"chaos",
                citar_engine::game::DebugOptions::default(),
            ),
            Self::Fixture { name } => {
                let f = find_fixture(name)?;
                let mut g =
                    games::from_fixture(&f, b"chaos", citar_engine::game::DebugOptions::default())?;
                let _events = g.assign_every_city_for_test();
                Ok(g)
            }
        }
    }
}

/// The fixture called `name`, among the committed ones and the corpus's.
fn find_fixture(name: &str) -> Result<Fixture, String> {
    let mut all = fixtures::committed()?;
    all.extend(fixtures::corpus()?.unwrap_or_default());
    all.into_iter().find(|f| f.name == name).ok_or_else(|| format!("no fixture {name}"))
}

/// What failed, and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// The step that failed, from 0; the number of steps for a check made after the last.
    pub step: usize,
    /// The property broken; P1 for a panic.
    pub property: Property,
    /// What the check or the panic said.
    pub what: String,
}

/// A failed chaos game, to play again (`chaos --replay`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    pub version: u32,
    pub start: Start,
    /// The bug planted in the engine (`game::seeded`), if any.
    pub bug: Option<SeededBug>,
    /// The checks' cadence: a save and a load (P6) put a loaded game in the played one's place.
    pub options: Options,
    /// Every step taken, the one that failed last.
    pub steps: Vec<Step>,
    pub failure: Failure,
}

/// What a chaos run is asked to do.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The seed every game's start and steps are drawn from.
    pub seed: u64,
    /// Start from the fixtures rather than generated maps.
    pub from_fixtures: bool,
    /// The most rounds a game plays before the next begins.
    pub rounds: u32,
    /// How many tool calls, at most, come before each seat's turn.
    pub calls_per_turn: u32,
    /// A bug to plant in the engine, to show that chaos finds it.
    pub bug: Option<SeededBug>,
    /// The checks' cadence.
    pub options: Options,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            seed: 1,
            from_fixtures: false,
            rounds: 30,
            calls_per_turn: 12,
            bug: None,
            options: Options::default(),
        }
    }
}

/// What a chaos run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub games: u64,
    pub rounds: u64,
    pub steps: u64,
    /// Tool calls the game refused, of all the calls made.
    pub refused: u64,
    pub calls: u64,
    /// The games that failed.
    pub failures: Vec<Replay>,
}

/// The map sizes and types generated games draw from.
const SIZES: [&str; 2] = ["duel", "small"];
const MAP_TYPES: [&str; 5] = ["continents", "pangaea", "fractal", "archipelago", "inland_sea"];
const EDGES: [&str; 3] = ["wrap_x", "boxed", "wrap_y"];

/// The start of game number `n` of a run.
///
/// # Errors
/// If the fixtures cannot be listed.
pub fn start_of(settings: &Settings, n: u64) -> Result<Start, String> {
    let mut rng = Rng::keyed(settings.seed, Purpose::TestAgent, &[0xc4a0, n]);
    if settings.from_fixtures {
        let mut all = fixtures::committed()?;
        all.extend(fixtures::corpus()?.unwrap_or_default());
        let f = rng.pick(&all).ok_or("no fixtures")?;
        return Ok(Start::Fixture { name: f.name.clone() });
    }
    let size = rng.pick(&SIZES).copied().unwrap_or("duel");
    // One game in four on the kitchen-sink ruleset, for the unique types only it has.
    if rng.below(4) == 0 {
        return Ok(Start::KitchenSink {
            size: size.to_owned(),
            seed: rng.next_u64() >> 16,
            turn_limit: settings.rounds + 20,
        });
    }
    let map_type = rng.pick(&MAP_TYPES).copied().unwrap_or("continents");
    let edges = rng.pick(&EDGES).copied().unwrap_or("wrap_x");
    Ok(Start::Generated {
        size: size.to_owned(),
        map_type: map_type.to_owned(),
        edges: edges.to_owned(),
        seed: rng.next_u64() >> 16,
        turn_limit: settings.rounds + 20,
    })
}

/// What a step's panic said.
fn panic_text(payload: &(dyn core::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// Plays one step under `catch_unwind`, and says whether the game refused it: a panic is
/// property P1.
fn guarded(run: &mut Run, s: &Step) -> Result<bool, Failure> {
    let at = run.steps();
    match catch_unwind(AssertUnwindSafe(|| run.step(s))) {
        Ok(Ok(t)) => Ok(t.refused.is_some()),
        Ok(Err(b)) => Err(Failure { step: b.step, property: b.property, what: b.what }),
        Err(p) => Err(Failure { step: at, property: Property::P1, what: panic_text(&*p) }),
    }
}

/// The checks after the last step, under `catch_unwind`.
fn guarded_finish(run: &mut Run) -> Result<(), Failure> {
    let at = run.steps();
    match catch_unwind(AssertUnwindSafe(|| run.finish())) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(b)) => Err(Failure { step: b.step, property: b.property, what: b.what }),
        Err(p) => Err(Failure { step: at, property: Property::P1, what: panic_text(&*p) }),
    }
}

/// Plays one chaos game from `start`: before each seat's turn up to `calls_per_turn` tool calls
/// drawn at random, then the seat's turn played by its driver, until the game ends,
/// `rounds` rounds have passed or `keep_going` says to stop. Returns what it did, and the
/// replay if it failed.
///
/// # Errors
/// If the start cannot be built.
pub fn play_game(
    settings: &Settings,
    n: u64,
    start: Start,
    keep_going: &mut dyn FnMut() -> bool,
) -> Result<(Report, Option<Replay>), String> {
    let _bug = seeded::seed(settings.bug);
    let g = start.game()?;
    let limit = g.turn().saturating_add(i32::try_from(settings.rounds).unwrap_or(i32::MAX));
    let mut run = Run::new(g, settings.options);
    let mut rng = Rng::keyed(settings.seed, Purpose::TestAgent, &[0xc4a0, n, 1]);
    let mut steps: Vec<Step> = Vec::new();
    let mut report = Report { games: 1, ..Report::default() };
    let failed = 'play: {
        while run.game().phase() == Phase::Playing && run.game().turn() < limit && keep_going() {
            for _ in 0..rng.below(u64::from(settings.calls_per_turn) + 1) {
                let s = Step::Call(ActionSpec::draw(&mut rng));
                steps.push(s);
                match guarded(&mut run, &s) {
                    Ok(refused) => {
                        report.calls += 1;
                        report.refused += u64::from(refused);
                    }
                    Err(f) => break 'play Some(f),
                }
            }
            let turn = run.game().turn();
            steps.push(Step::Agent);
            if let Err(f) = guarded(&mut run, &Step::Agent) {
                break 'play Some(f);
            }
            report.rounds += u64::from(run.game().turn() > turn);
        }
        guarded_finish(&mut run).err()
    };
    report.steps = steps.len() as u64;
    let replay = failed.map(|failure| Replay {
        version: REPLAY_VERSION,
        start,
        bug: settings.bug,
        options: settings.options,
        steps,
        failure,
    });
    Ok((report, replay))
}

/// Plays chaos games one after another while `keep_going` says to, each from [`start_of`].
///
/// # Errors
/// If a game's start cannot be built.
pub fn run(settings: &Settings, keep_going: &mut dyn FnMut() -> bool) -> Result<Report, String> {
    let mut total = Report::default();
    let mut n = 0;
    while keep_going() {
        let start = start_of(settings, n)?;
        let (r, failed) = play_game(settings, n, start, keep_going)?;
        total.games += r.games;
        total.rounds += r.rounds;
        total.steps += r.steps;
        total.calls += r.calls;
        total.refused += r.refused;
        total.failures.extend(failed);
        n += 1;
    }
    Ok(total)
}

/// Plays a replay's steps again, with its bug planted and its checks' cadence, and returns how
/// it failed this time, or `None` if it did not.
///
/// # Errors
/// A replay of a format version this build does not read (1 to [`REPLAY_VERSION`]), or a start
/// that cannot be built.
pub fn replay(r: &Replay) -> Result<Option<Failure>, String> {
    if !(1..=REPLAY_VERSION).contains(&r.version) {
        return Err(format!(
            "a replay of version {}, where this build reads 1 to {REPLAY_VERSION}",
            r.version
        ));
    }
    let _bug = seeded::seed(r.bug);
    let mut run = Run::new(r.start.game()?, r.options);
    for s in &r.steps {
        if let Err(f) = guarded(&mut run, s) {
            return Ok(Some(f));
        }
    }
    Ok(guarded_finish(&mut run).err())
}

/// A replay as the file `chaos` writes.
///
/// # Errors
/// If it does not serialise, which a replay always does.
pub fn to_json(r: &Replay) -> Result<String, String> {
    serde_json::to_string_pretty(r).map(|s| s + "\n").map_err(|e| e.to_string())
}

/// A replay read from its file's text.
///
/// # Errors
/// If the text is not a replay.
pub fn from_json(text: &str) -> Result<Replay, String> {
    serde_json::from_str(text).map_err(|e| format!("not a chaos replay: {e}"))
}

//! The runner (DESIGN.md P2.4.1): one game, one driver per major, stepped one driven seat at a
//! time.
//!
//! [`Runner::step`] is one `Game::drive` with a seat limit of 1, so control comes back after each
//! driven seat: a host checks its time budget between seats, calls its round hook when the turn
//! changes (as `common.play`'s `on_round` does), and in the bindings takes the GIL back to
//! deliver the step's events. A panic out of a drive is caught: the runner poisons the game,
//! records a [`Crash`] with the message and the seat's label, and ends the game; with
//! `raise_errors` it returns the crash to its caller instead (`citar sim`'s contract). There is
//! no `max_errors`: a Rust bot does not raise, and a panic is a bug.
//!
//! [`run_game`] plays a game to its end and returns `engine_api.run_game`'s dict as a
//! [`RunResult`]. Replaces `citar/bots/headless.py`.

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{
    ActionError, DriveOptions, Drivers, EngineError, EventBatch, Game, SeatDriver, Stop,
};
use citar_engine::rules::Ruleset;
use serde::Serialize;
use serde_json::Value;

/// What to play.
#[derive(Clone, Debug, Default)]
pub struct RunSpec {
    /// The game's configuration, as the lobby sends it (`EngineGame.new`'s).
    pub config: Value,
    /// A label per seat, added to its crash record after the player (a profile's name).
    pub labels: BTreeMap<PlayerId, String>,
    /// Return the first crash to the caller instead of recording it and ending the game.
    pub raise_errors: bool,
    /// The most a game may take, checked between steps.
    pub budget: Option<Duration>,
}

/// A drive that panicked: the game is poisoned and over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crash {
    pub turn: Turn,
    /// The seat whose driver was playing or answering, if one was.
    pub player: Option<PlayerId>,
    /// That seat's label from the spec.
    pub label: Option<String>,
    /// The panic's message.
    pub message: String,
}

impl fmt::Display for Crash {
    /// `T<turn> P<player> <label>: panic: <message>`, the shape of Python's crash lines.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T{}", self.turn)?;
        if let Some(p) = self.player {
            write!(f, " P{}", p.0)?;
        }
        if let Some(l) = &self.label {
            write!(f, " {l}")?;
        }
        write!(f, ": panic: {}", self.message)
    }
}

/// Why the runner stopped short.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SimError {
    /// The configuration does not make a game.
    #[error("the game could not be created: {0}")]
    Config(#[from] EngineError),
    /// The game refused to be driven: it is poisoned.
    #[error("the game refused to go on: {0}")]
    Refused(#[from] ActionError),
    /// A drive panicked, and the spec asked for crashes to be raised.
    #[error("{0}")]
    Crashed(Crash),
    /// The game ran past its budget.
    #[error("the game ran past its budget of {budget:?} on turn {turn}")]
    TimedOut { turn: Turn, budget: Duration },
    /// What package 2-04 writes; it removes the variant.
    #[error("not built yet: {0} (package 2-04)")]
    NotYet(&'static str),
}

/// What one step did.
#[derive(Debug)]
pub struct Step {
    /// Why the drive returned. A crash recorded rather than raised ends the game, and reads as
    /// `GameOver`.
    pub stop: Stop,
    /// The events the step appended, in order.
    pub events: EventBatch,
    /// Whether the turn changed during the step: the host's round hook runs.
    pub new_round: bool,
}

/// What a round hook is told (`on_turn`'s dict): the turn that began, the phase, the turn limit
/// and the last row of stats.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RoundInfo {
    pub turn: Turn,
    pub phase: String,
    pub turn_limit: Option<Turn>,
    pub last_stats: Option<Value>,
}

/// A major civilization's standing at the end (`headless.result`'s extra keys).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MajorRow {
    pub difficulty: String,
    pub techs: u32,
    pub future_techs: u32,
    pub policies: u32,
    pub religion: Value,
    pub great_people: u32,
    pub cities: u32,
    pub spaceship: Value,
    /// 0 once eliminated, whatever it had built.
    pub score: i64,
}

/// One civilization at the end of a run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlayerRow {
    pub id: PlayerId,
    pub kind: String,
    pub name: String,
    pub alive: bool,
    /// For a major civilization.
    #[serde(flatten)]
    pub major: Option<MajorRow>,
}

/// How a game went: `engine_api.run_game`'s dict.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunResult {
    pub turn: Turn,
    /// Turns played.
    pub turns: Turn,
    pub phase: String,
    pub winner: Option<PlayerId>,
    pub victory: Option<String>,
    pub turn_limit: Option<Turn>,
    /// A row per turn.
    pub stats: Value,
    pub players: Vec<PlayerRow>,
    /// A line per crash.
    pub errors: Vec<String>,
}

/// One game and its drivers.
pub struct Runner {
    game: Game,
    drivers: Vec<(PlayerId, Box<dyn SeatDriver>)>,
    labels: BTreeMap<PlayerId, String>,
    raise_errors: bool,
    budget: Option<Duration>,
    started: Instant,
    crashes: Vec<Crash>,
    over: bool,
}

impl Runner {
    /// A new game from `spec`'s configuration under `rules`, with `drivers` playing their seats.
    ///
    /// # Errors
    /// The configuration does not make a game.
    pub fn new(
        rules: &'static Ruleset,
        spec: RunSpec,
        drivers: Vec<(PlayerId, Box<dyn SeatDriver>)>,
    ) -> Result<Self, SimError> {
        let setup = config_from_value(rules, spec.config)?;
        // The events of the game's creation are not a step's: Python's on_event heard only those
        // after it.
        let (game, _created) = Game::new(rules, &setup)?;
        Ok(Self {
            game,
            drivers,
            labels: spec.labels,
            raise_errors: spec.raise_errors,
            budget: spec.budget,
            started: Instant::now(),
            crashes: Vec::new(),
            over: false,
        })
    }

    /// The game.
    #[must_use]
    pub const fn game(&self) -> &Game {
        &self.game
    }

    /// The game, given up.
    #[must_use]
    pub fn into_game(self) -> Game {
        self.game
    }

    /// Whether the game is over: it ended, or a crash ended it.
    #[must_use]
    pub const fn is_over(&self) -> bool {
        self.over
    }

    /// The crashes recorded.
    #[must_use]
    pub fn crashes(&self) -> &[Crash] {
        &self.crashes
    }

    /// Plays on until one driven seat's turn has ended, or the game stops for another reason.
    ///
    /// # Errors
    /// The game ran past its budget; a drive panicked and the spec raises crashes; the game is
    /// poisoned.
    pub fn step(&mut self) -> Result<Step, SimError> {
        if let Some(budget) = self.budget
            && self.started.elapsed() > budget
        {
            return Err(SimError::TimedOut { turn: self.game.turn(), budget });
        }
        let before = self.game.turn();
        let mut d = Drivers::none(self.game.state().players().len());
        for (pid, driver) in &mut self.drivers {
            d = d.with(*pid, driver.as_mut());
        }
        let opts = DriveOptions::default().with_seat_limit(1);
        let game = &mut self.game;
        let run = catch_unwind(AssertUnwindSafe(|| game.drive(&mut d, opts)));
        drop(d);
        match run {
            Ok(drove) => {
                let (stop, events) = drove?;
                self.over = stop == Stop::GameOver;
                Ok(Step { stop, events, new_round: self.game.turn() != before })
            }
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                let player = self.game.driving().or_else(|| Some(self.game.current()));
                self.game.poison(&message);
                self.over = true;
                let label = player.and_then(|p| self.labels.get(&p).cloned());
                let crash = Crash { turn: self.game.turn(), player, label, message };
                if self.raise_errors {
                    return Err(SimError::Crashed(crash));
                }
                self.crashes.push(crash);
                Ok(Step { stop: Stop::GameOver, events: EventBatch::default(), new_round: false })
            }
        }
    }

    /// How the game went.
    ///
    /// # Errors
    /// [`SimError::NotYet`] until package 2-04.
    pub fn result(&self) -> Result<RunResult, SimError> {
        Err(SimError::NotYet("the run's result"))
    }
}

/// The text of a panic's payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// Plays a game to its end with `drivers` in their seats: `on_round` hears each turn as it
/// begins (and once more if the game ended on a new turn), `on_events` every step's events.
/// Returns `engine_api.run_game`'s dict.
///
/// # Errors
/// As [`Runner::new`] and [`Runner::step`]; [`SimError::NotYet`] until package 2-04.
pub fn run_game(
    _rules: &'static Ruleset,
    _spec: RunSpec,
    _drivers: Vec<(PlayerId, Box<dyn SeatDriver>)>,
    _on_round: &mut dyn FnMut(&RoundInfo),
    _on_events: &mut dyn FnMut(&EventBatch),
) -> Result<RunResult, SimError> {
    Err(SimError::NotYet("run_game"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
    use citar_engine::base::ids::NegotiationId;
    use citar_engine::game::DriverOutcome;
    use citar_engine::state::players::DriverMemory;
    use serde_json::json;
    use std::sync::Arc;

    fn duel(turn_limit: u32) -> RunSpec {
        RunSpec {
            config: json!({"seed": 3, "map_size": "duel", "map_type": "pangaea",
                           "players": [{"controller": "bot"}, {"controller": "bot"}],
                           "city_states": 0, "barbarians": "off", "turn_limit": turn_limit}),
            ..RunSpec::default()
        }
    }

    fn idle() -> Box<dyn SeatDriver> {
        let tuning = Arc::new(Tuning::new(VersionId::Idle, Overrides::default()));
        Box::new(Bot::new(Arc::new(BotSpec::new(VersionId::Idle, tuning, None, None))))
    }

    #[test]
    fn a_step_ends_one_driven_seats_turn_and_the_steps_reach_the_end() {
        let rules = Ruleset::shared();
        let drivers = vec![(PlayerId(0), idle()), (PlayerId(1), idle())];
        let mut r = Runner::new(rules, duel(3), drivers).expect("a game");
        let mut steps = 0;
        let mut rounds = 0;
        while !r.is_over() {
            let s = r.step().expect("steps");
            steps += 1;
            rounds += usize::from(s.new_round);
            assert!(steps < 20, "a 3-turn duel ends");
        }
        // Six turns, one per step: the second seat's ends each round, the sixth the game too.
        assert_eq!(steps, 6);
        assert_eq!(rounds, 3);
        assert!(r.crashes().is_empty());
        assert!(r.game().player_cities(PlayerId(0)).next().is_some(), "idle founded its capital");
    }

    /// A driver that panics on its first turn.
    struct Panicker;

    impl SeatDriver for Panicker {
        fn play_turn(&mut self, _: &mut Game, _: PlayerId, _: &mut DriverMemory) -> DriverOutcome {
            panic!("the test driver panics");
        }

        fn respond(
            &mut self,
            _: &mut Game,
            _: PlayerId,
            _: NegotiationId,
            _: &mut DriverMemory,
        ) -> DriverOutcome {
            DriverOutcome::Done
        }
    }

    #[test]
    fn a_panicking_driver_poisons_the_game_and_is_recorded_or_raised() {
        let rules = Ruleset::shared();
        let mut spec = duel(5);
        spec.labels.insert(PlayerId(0), "panicker".to_owned());
        let drivers =
            vec![(PlayerId(0), Box::new(Panicker) as Box<dyn SeatDriver>), (PlayerId(1), idle())];
        let mut r = Runner::new(rules, spec.clone(), drivers).expect("a game");
        let s = r.step().expect("recorded, not raised");
        assert_eq!(s.stop, Stop::GameOver);
        assert!(r.is_over());
        assert!(r.game().poisoned().is_some());
        assert_eq!(r.crashes().len(), 1);
        assert_eq!(r.crashes()[0].to_string(), "T1 P0 panicker: panic: the test driver panics");
        assert!(matches!(r.step(), Err(SimError::Refused(_))), "a poisoned game refuses");

        spec.raise_errors = true;
        let drivers =
            vec![(PlayerId(0), Box::new(Panicker) as Box<dyn SeatDriver>), (PlayerId(1), idle())];
        let mut r = Runner::new(rules, spec, drivers).expect("a game");
        match r.step() {
            Err(SimError::Crashed(c)) => assert_eq!(c.label.as_deref(), Some("panicker")),
            other => panic!("{other:?}"),
        }
    }
}

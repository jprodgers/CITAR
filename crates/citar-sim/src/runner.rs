//! The runner (DESIGN.md P2.4.1): one game, one driver per major, stepped one driven seat at a
//! time. Replaces `citar/bots/headless.py`.
//!
//! [`Runner::step`] is one `Game::drive` with a seat limit of 1, so control comes back after each
//! driven seat: a host checks its time budget between seats, calls its round hook when the turn
//! changes (as `common.play`'s `on_round` does), and in the bindings takes the GIL back to
//! deliver the step's events. A panic out of a drive is caught: the runner poisons the game,
//! records a [`Crash`] with the message, the seat's label and where it happened ([`panics`]), and
//! ends the game; with `raise_errors` it returns the crash to its caller instead (`citar sim`'s
//! contract). There is no `max_errors`: a Rust bot does not raise, and a panic is a bug.
//!
//! What `drive` leaves to its host, the runner does as Python's headless loop did, so a game
//! with no one at the table cannot stall: a seat with no driver passes its turn
//! (`headless.py:61-62`), and a chat that waits on the host (a seat with no driver, or a bot
//! that left it to a language model) is closed as expired.
//!
//! [`run_game`] plays a game to its end and returns `engine_api.run_game`'s dict as a
//! [`RunResult`].
//!
//! [`panics`]: crate::panics

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use citar_engine::api::views::client::stats_row_json;
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{
    ActionError, DebugOptions, DriveOptions, Drivers, EngineError, EventBatch, Game, SeatDriver,
    Stop,
};
use citar_engine::rules::Ruleset;
use citar_engine::state::Phase;
use citar_engine::state::diplo::NegStatus;
use serde::Serialize;
use serde_json::Value;

use crate::panics;
use crate::result::RunResult;

/// The most backtrace frames a crash record keeps when the spec gives no limit (Python's
/// `traceback_limit` default, `engine_api.py:341`).
pub const TRACEBACK_LIMIT: usize = 5;

/// What a chat closed by the runner says: nobody at the table could answer it.
const EXPIRED_NOTE: &str = "No answer came: the game is played headless.";

/// The drivers of a game's seats, by player: a seat left out has none, and passes its turns.
pub type Seats = Vec<(PlayerId, Box<dyn SeatDriver>)>;

/// What to play.
#[derive(Clone, Debug)]
pub struct RunSpec {
    /// The game's configuration, as the lobby sends it (`EngineGame.new`'s).
    pub config: Value,
    /// A label per seat, added to its crash record after the player (a profile's name).
    pub labels: BTreeMap<PlayerId, String>,
    /// Return the first crash to the caller instead of recording it and ending the game.
    pub raise_errors: bool,
    /// The most a game may take, checked between steps.
    pub budget: Option<Duration>,
    /// The checks the game runs on itself; `None` keeps the build's default (DESIGN.md 9.4).
    pub debug: Option<DebugOptions>,
    /// The most backtrace frames a crash record keeps (`traceback_limit`).
    pub traceback_limit: usize,
}

impl Default for RunSpec {
    fn default() -> Self {
        Self {
            config: Value::Null,
            labels: BTreeMap::new(),
            raise_errors: false,
            budget: None,
            debug: None,
            traceback_limit: TRACEBACK_LIMIT,
        }
    }
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
    /// Where it happened, when the panic hook saw it ([`panics::install`]): the location and
    /// the first frames of the backtrace. Empty otherwise.
    pub trace: String,
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

impl Crash {
    /// The crash as a `run_game` error line: the line, then where it happened, as Python wrote
    /// a line and its traceback.
    #[must_use]
    pub fn record(&self) -> String {
        if self.trace.is_empty() { self.to_string() } else { format!("{self}\n{}", self.trace) }
    }
}

/// Why the runner stopped short.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SimError {
    /// The configuration does not make a game.
    #[error("the game could not be created: {0}")]
    Config(#[from] EngineError),
    /// The game refused to go on: it is poisoned.
    #[error("the game refused to go on: {0}")]
    Refused(#[from] ActionError),
    /// A drive panicked, and the spec asked for crashes to be raised.
    #[error("{0}")]
    Crashed(Crash),
    /// The game ran past its budget.
    #[error("the game ran past its budget of {budget:?} on turn {turn}")]
    TimedOut { turn: Turn, budget: Duration },
}

/// What one step did.
#[derive(Debug)]
pub struct Step {
    /// Why the drive returned. A crash recorded rather than raised ends the game, and reads as
    /// `GameOver`. `External` names a seat with no driver, whose turn the runner passed.
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
    /// `playing`, or `over` for the call after the game ended on a new turn.
    pub phase: &'static str,
    pub turn_limit: Turn,
    /// The last round's row, as `EngineGame.stats` gives it; `None` before the first.
    pub last_stats: Option<Value>,
}

impl RoundInfo {
    /// Where `g` stands.
    #[must_use]
    pub fn of(g: &Game) -> Self {
        let st = g.state();
        Self {
            turn: st.clock().turn,
            phase: match st.clock().phase {
                Phase::Playing => "playing",
                Phase::Over => "over",
            },
            turn_limit: st.config().turn_limit,
            last_stats: g.stats(Some(1)).first().map(stats_row_json),
        }
    }
}

/// One game and its drivers.
pub struct Runner {
    game: Game,
    drivers: Seats,
    labels: BTreeMap<PlayerId, String>,
    raise_errors: bool,
    budget: Option<Duration>,
    traceback_limit: usize,
    started: Instant,
    crashes: Vec<Crash>,
    over: bool,
}

impl Runner {
    /// A new game from `spec`'s configuration under `rules`, with `drivers` playing their seats.
    ///
    /// # Errors
    /// The configuration does not make a game.
    pub fn new(rules: &'static Ruleset, spec: RunSpec, drivers: Seats) -> Result<Self, SimError> {
        Self::new_with(rules, spec, |_| drivers)
    }

    /// A new game from `spec`'s configuration under `rules`, with the drivers `drivers` makes for
    /// it once it exists: a host that seats a bot in every major's seat learns who they are
    /// from the game.
    ///
    /// # Errors
    /// The configuration does not make a game.
    pub fn new_with(
        rules: &'static Ruleset,
        spec: RunSpec,
        drivers: impl FnOnce(&Game) -> Seats,
    ) -> Result<Self, SimError> {
        let setup = config_from_value(rules, spec.config)?;
        // The events of the game's creation are not a step's: Python's on_event heard only those
        // after it.
        let (mut game, _created) = Game::new(rules, &setup)?;
        if let Some(d) = spec.debug {
            game.set_debug_options(d);
        }
        let drivers = drivers(&game);
        Ok(Self {
            game,
            drivers,
            labels: spec.labels,
            raise_errors: spec.raise_errors,
            budget: spec.budget,
            traceback_limit: spec.traceback_limit,
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

    /// What the game's checks have found since the last take (DESIGN.md 9.4): nothing unless
    /// the spec turned them on in a build that has them.
    pub fn take_violations(&mut self) -> Vec<citar_engine::game::Violation> {
        self.game.take_violations()
    }

    /// Where the game stands, as a round hook is told.
    #[must_use]
    pub fn round_info(&self) -> RoundInfo {
        RoundInfo::of(&self.game)
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
        // A trace left by a panic someone caught earlier on this thread is not this drive's.
        panics::forget();
        let run = catch_unwind(AssertUnwindSafe(|| {
            let (stop, mut events) = game.drive(&mut d, opts)?;
            match &stop {
                // Nobody plays the seat: it passes, as in Python's headless loop.
                Stop::External(pid) => events.extend(game.end_turn(*pid)?),
                // Nobody can answer: the chats expire, and the next drive ends the turn.
                Stop::AwaitingReply { nids, .. } => {
                    for &nid in nids {
                        let open =
                            game.negotiation(nid).is_some_and(|n| n.status == NegStatus::Open);
                        if open {
                            let (_, more) = game.close_negotiation(
                                nid,
                                NegStatus::Expired,
                                EXPIRED_NOTE,
                                None,
                            )?;
                            events.extend(more);
                        }
                    }
                }
                _ => {}
            }
            Ok::<_, ActionError>((stop, events))
        }));
        drop(d);
        match run {
            Ok(drove) => {
                let (stop, events) = drove?;
                // A pass can end the game too (the last seat of the last round had no driver).
                self.over = stop == Stop::GameOver || self.game.phase() != Phase::Playing;
                Ok(Step { stop, events, new_round: self.game.turn() != before })
            }
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                let trace =
                    panics::take().map(|t| t.text(self.traceback_limit)).unwrap_or_default();
                let player = self.game.driving().or_else(|| Some(self.game.current()));
                self.game.poison(&message);
                self.over = true;
                let label = player.and_then(|p| self.labels.get(&p).cloned());
                let crash = Crash { turn: self.game.turn(), player, label, message, trace };
                if self.raise_errors {
                    return Err(SimError::Crashed(crash));
                }
                self.crashes.push(crash);
                Ok(Step { stop: Stop::GameOver, events: EventBatch::default(), new_round: false })
            }
        }
    }

    /// How the game went, as it stands: `engine_api.run_game`'s dict, with a line per crash
    /// recorded.
    #[must_use]
    pub fn result(&self) -> RunResult {
        RunResult::of(&self.game, self.crashes.iter().map(Crash::record).collect())
    }
}

/// The text of a panic's payload.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// Plays a game to its end with `drivers` in their seats (`engine_api.run_game`,
/// `headless.play`): `on_round` hears the first turn, each turn as it begins, and the turn the
/// game ended on if it ended on a new one; `on_events` hears every step's events, before the
/// round hook of the turn the step began. Returns `engine_api.run_game`'s dict.
///
/// # Errors
/// As [`Runner::new`] and [`Runner::step`]: a configuration that makes no game, a game past its
/// budget, and with `raise_errors` the first crash.
pub fn run_game(
    rules: &'static Ruleset,
    spec: RunSpec,
    drivers: Seats,
    on_round: &mut dyn FnMut(&RoundInfo),
    on_events: &mut dyn FnMut(&EventBatch),
) -> Result<RunResult, SimError> {
    let mut r = Runner::new(rules, spec, drivers)?;
    let mut shown = r.game().turn();
    on_round(&r.round_info());
    while !r.is_over() {
        let s = r.step()?;
        if !s.events.is_empty() {
            on_events(&s.events);
        }
        if r.game().turn() != shown {
            shown = r.game().turn();
            on_round(&r.round_info());
        }
    }
    Ok(r.result())
}

#[cfg(test)]
mod tests {
    use super::*;
    use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
    use citar_engine::base::ids::NegotiationId;
    use citar_engine::game::diplomacy::actions::OpenNegotiation;
    use citar_engine::game::{Action, DriverOutcome};
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

    #[test]
    fn a_seat_with_no_driver_passes_its_turn() {
        let rules = Ruleset::shared();
        let mut r = Runner::new(rules, duel(3), vec![(PlayerId(0), idle())]).expect("a game");
        let mut external = 0;
        let mut steps = 0;
        while !r.is_over() {
            let s = r.step().expect("steps");
            external += usize::from(matches!(s.stop, Stop::External(PlayerId(1))));
            steps += 1;
            assert!(steps < 20, "a 3-turn duel ends with a seat nobody plays");
        }
        assert_eq!(external, 3, "player 1's turn passed each round");
        assert_eq!(steps, 3, "the last pass ended the game");
        assert_eq!(r.result().phase, "over");
        assert!(r.game().player_cities(PlayerId(1)).next().is_none(), "nobody founded its city");
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
        assert_eq!(r.result().errors.len(), 1, "the result reads a poisoned game");

        spec.raise_errors = true;
        let drivers =
            vec![(PlayerId(0), Box::new(Panicker) as Box<dyn SeatDriver>), (PlayerId(1), idle())];
        let mut r = Runner::new(rules, spec, drivers).expect("a game");
        match r.step() {
            Err(SimError::Crashed(c)) => assert_eq!(c.label.as_deref(), Some("panicker")),
            other => panic!("{other:?}"),
        }
    }

    /// A driver that, on its first turn, meets `to` and opens a chat with it, and answers
    /// nothing.
    struct Opener {
        to: PlayerId,
        opened: bool,
    }

    impl SeatDriver for Opener {
        fn play_turn(
            &mut self,
            g: &mut Game,
            pid: PlayerId,
            _: &mut DriverMemory,
        ) -> DriverOutcome {
            if !std::mem::replace(&mut self.opened, true) {
                g.meet(pid, self.to).expect("they meet");
                let offer = OpenNegotiation {
                    to: i64::from(self.to.0),
                    message: json!("Friends?"),
                    give: None,
                    receive: None,
                };
                g.act(pid, Action::OpenNegotiation(offer)).expect("it opens");
            }
            DriverOutcome::Done
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

    /// A driver that leaves every chat put to it to the host, as a hybrid seat's bot leaves one
    /// its model owns.
    struct Deferrer;

    impl SeatDriver for Deferrer {
        fn play_turn(&mut self, _: &mut Game, _: PlayerId, _: &mut DriverMemory) -> DriverOutcome {
            DriverOutcome::Done
        }

        fn respond(
            &mut self,
            _: &mut Game,
            _: PlayerId,
            _: NegotiationId,
            _: &mut DriverMemory,
        ) -> DriverOutcome {
            DriverOutcome::Deferred
        }
    }

    /// Steps `r` to the game's end, returning the chats each `AwaitingReply` named.
    fn play_out(r: &mut Runner) -> Vec<NegotiationId> {
        let mut waited = Vec::new();
        let mut steps = 0;
        while !r.is_over() {
            let s = r.step().expect("steps");
            if let Stop::AwaitingReply { pid, nids } = &s.stop {
                assert_eq!(*pid, PlayerId(0), "the opener's turn waits");
                waited.extend(nids.iter().copied());
            }
            steps += 1;
            assert!(steps < 30, "a 5-turn duel ends though a chat waited on the host");
        }
        waited
    }

    fn expired_by_the_runner(r: &Runner, nid: NegotiationId) {
        let n = r.game().negotiation(nid).expect("the chat");
        assert_eq!(n.status, NegStatus::Expired);
        let note = n.history.last().and_then(|e| e.note.as_deref());
        assert_eq!(note, Some(EXPIRED_NOTE));
    }

    #[test]
    fn a_chat_with_a_seat_nobody_plays_expires_and_the_game_goes_on() {
        let rules = Ruleset::shared();
        let opener = Opener { to: PlayerId(1), opened: false };
        let drivers = vec![(PlayerId(0), Box::new(opener) as Box<dyn SeatDriver>)];
        let mut r = Runner::new(rules, duel(5), drivers).expect("a game");
        let s = r.step().expect("steps");
        let Stop::AwaitingReply { pid: PlayerId(0), nids } = &s.stop else {
            panic!("the opener's turn waits on the host: {:?}", s.stop)
        };
        assert_eq!(nids.len(), 1);
        let nid = nids[0];
        expired_by_the_runner(&r, nid);
        assert!(s.events.events().iter().any(|e| e.kind.name() == "negotiation"));
        assert_eq!(play_out(&mut r), Vec::<NegotiationId>::new(), "it waited once");
        assert_eq!(r.result().phase, "over");
        assert_eq!(r.result().turns, 5);
    }

    #[test]
    fn a_chat_a_driver_leaves_to_the_host_expires_and_the_game_goes_on() {
        let rules = Ruleset::shared();
        let opener = Opener { to: PlayerId(1), opened: false };
        let drivers = vec![
            (PlayerId(0), Box::new(opener) as Box<dyn SeatDriver>),
            (PlayerId(1), Box::new(Deferrer) as Box<dyn SeatDriver>),
        ];
        let mut r = Runner::new(rules, duel(5), drivers).expect("a game");
        let waited = play_out(&mut r);
        assert_eq!(waited.len(), 1, "the deferred chat stopped the drive once");
        expired_by_the_runner(&r, waited[0]);
        assert_eq!(r.result().phase, "over");
        assert_eq!(r.result().turns, 5);
    }

    #[test]
    fn a_game_past_its_budget_stops_between_steps() {
        let rules = Ruleset::shared();
        let mut spec = duel(5);
        spec.budget = Some(Duration::ZERO);
        let drivers = vec![(PlayerId(0), idle()), (PlayerId(1), idle())];
        let mut r = Runner::new(rules, spec, drivers).expect("a game");
        std::thread::sleep(Duration::from_millis(2));
        assert!(matches!(r.step(), Err(SimError::TimedOut { turn: 1, .. })));
    }
}

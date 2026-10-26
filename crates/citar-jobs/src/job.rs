//! One job played (design/phase4-helper.md P4.4.1-P4.4.5): [`Job`], stepped one seat at a time
//! on citar-sim's runner, and [`run`], which plays one to its end under a [`Control`].
//!
//! **What a job's game is:** the engine's ordinary game, with the runner's seats, under
//! `Game::set_chain(DigestChain::new(spec_hash))` from its first round, with no wall-clock budget
//! (`RunSpec.budget` is never set, so `SimError::TimedOut` is never an outcome: a slow, capped or
//! paused machine computes what a fast one does; a deadline is the host's, and ends a run as a
//! failure, never as a result) and without the engine's invariant checks (they watch a game and
//! never change it; the jobs build records whether a build has them, P4.4.5).
//!
//! **Between seat steps** [`run`] asks its [`Control`] whether to go on: the job host (4-08) stops
//! a run there for an abort or a deadline and holds it there while paused, so each takes effect
//! within one seat step; the bindings deliver a round to Python there.
//!
//! **A panic is the outcome:** a drive that panics is the runner's crash record, made the job's
//! crash outcome (P4.4.2); a panic anywhere else in playing or summing up the game (making its
//! map, seating its bots, building its result) is caught here and is the crash outcome too, so
//! every deterministic failure of a spec is a result a replica reproduces. What is not a result:
//! a spec the engine refuses ([`RunError::Spec`]), a run its control stopped
//! ([`RunError::Stopped`]), and a fault of this code ([`RunError::Internal`]: the chain and the
//! digests disagree, a poisoned game refused a step).
//!
//! **Measured, never hashed:** [`Output::wall_s`] and [`Output::cpu_s`] (the CPU time of the thread
//! that started the job, which is the thread that plays it in every host) and a crash's
//! [`Output::trace`], which a host logs.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use citar_engine::base::digest::Digest;
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::game::DebugOptions;
use citar_engine::rules::Ruleset;
use citar_engine::save::chain::DigestChain;
use citar_sim::{Crash, RunSpec, Runner, Seats, SimError};
use cpu_time::ThreadTime;
use serde::Serialize;

use crate::hash::{Chain, JobResult};
use crate::lab::{LabCrashed, LabOutcome, LabPlay};
use crate::reference::{ReferenceOutcome, ReferencePlay};
use crate::spec::{Checked, Kind, Spec, SpecError};

/// Where a run stands, as its control is told between seat steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// The turn being played.
    pub round: Turn,
    /// The game's last turn.
    pub of: Turn,
    /// Whether the last step began this turn (or this is the run's first look).
    pub new_round: bool,
}

/// Whether a run goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Go,
    Stop,
}

/// What a run asks between seat steps (P4.4.3): whether to go on. It may hold the run there (a
/// pause) by not returning.
pub trait Control {
    fn between(&mut self, at: &Progress) -> Flow;
}

impl<F: FnMut(&Progress) -> Flow> Control for F {
    fn between(&mut self, at: &Progress) -> Flow {
        self(at)
    }
}

/// A control that never stops a run.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unattended;

impl Control for Unattended {
    fn between(&mut self, _: &Progress) -> Flow {
        Flow::Go
    }
}

/// Why a run gave no result.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// The spec does not make a game (`bad_spec`).
    #[error("bad spec: {0}")]
    Spec(#[from] SpecError),
    /// Its control stopped it.
    #[error("the run was stopped")]
    Stopped,
    /// A fault of the job's own code, not of the game.
    #[error("internal: {0}")]
    Internal(String),
}

/// A finished job: its result and chain, hashed, and what was measured beside them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Output {
    /// The kind's outcome: a finished game's result, or its crash.
    pub result: JobResult,
    /// [`crate::result_hash`] of the result.
    pub result_hash: String,
    pub chain: Chain,
    /// Seconds from the start to the end of the run.
    pub wall_s: f64,
    /// CPU seconds of the thread that started it.
    pub cpu_s: f64,
    /// Where a crash happened, when the panic hook saw it: for a host's log, never hashed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

/// A game being played for a job.
#[derive(Debug)]
enum Play {
    Lab(Box<LabPlay>),
    Reference(Box<ReferencePlay>),
}

/// A panic that ended a job's game: where, and its message.
#[derive(Clone, Debug)]
struct Ended {
    turn: Turn,
    player: Option<PlayerId>,
    /// The runner's crash record (`T<turn> P<player> <label>: panic: <message>`), or `panic:
    /// <message>` for a panic outside a drive.
    record: String,
    message: String,
    trace: Option<String>,
}

impl Ended {
    fn of_crash(c: Crash) -> Self {
        let record = c.to_string();
        let trace = (!c.trace.is_empty()).then_some(c.trace);
        Self { turn: c.turn, player: c.player, record, message: c.message, trace }
    }

    fn of_panic(turn: Turn, message: String) -> Self {
        let trace = citar_sim::panics::take().map(|t| t.text(citar_sim::TRACEBACK_LIMIT));
        let record = format!("T{turn}: panic: {message}");
        Self { turn, player: None, record, message, trace: trace.filter(|t| !t.is_empty()) }
    }
}

/// One job, played step by step.
pub struct Job {
    kind: Kind,
    hash: crate::spec::SpecHash,
    play: Play,
    /// `None` when making the game panicked.
    runner: Option<Runner>,
    digests: Vec<Digest>,
    /// The turn of the last round whose digest was taken.
    last: Option<Turn>,
    /// Why the digests no longer follow the rounds, if they do not.
    broken: Option<String>,
    ended: Option<Ended>,
    new_round: bool,
    started: Instant,
    /// The starting thread's CPU time when the job started (a `ThreadTime` is not `Send`, and a
    /// job moves into the bindings' detached calls, which run on the same thread).
    cpu0: Option<Duration>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("kind", &self.kind)
            .field("hash", &self.hash)
            .field("rounds", &self.digests.len())
            .field("over", &self.is_over())
            .finish_non_exhaustive()
    }
}

/// The text of a panic's payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// Runs `work`, a panic its message.
fn caught<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    citar_sim::panics::forget();
    catch_unwind(AssertUnwindSafe(work)).map_err(|p| panic_text(p.as_ref()))
}

impl Job {
    /// The job `checked` describes, its game made under `rules` and ready for its first step.
    ///
    /// # Errors
    /// A spec whose game the engine refuses; a fault of the runner.
    pub fn start(rules: &'static Ruleset, checked: &Checked) -> Result<Self, RunError> {
        Self::start_seated(rules, checked, &mut |seats| seats)
    }

    /// [`start`](Self::start) with the seats' drivers passed through `seats` once made: the
    /// test operations' way to seat a driver that panics on purpose (`citar._engine.jobs.run`'s
    /// `test_panic` in a build with them; this crate's tests). A host never passes anything but
    /// the seats as they are.
    ///
    /// # Errors
    /// As [`start`](Self::start).
    pub fn start_seated(
        rules: &'static Ruleset,
        checked: &Checked,
        seats: &mut dyn FnMut(Seats) -> Seats,
    ) -> Result<Self, RunError> {
        let started = Instant::now();
        let cpu0 = ThreadTime::try_now().ok().map(|t| t.as_duration());
        let chain = Some(DigestChain::new(&checked.hash.0));
        let base = || RunSpec {
            raise_errors: true,
            debug: Some(DebugOptions::OFF),
            chain,
            ..RunSpec::default()
        };
        let (play, made) = match &checked.spec {
            Spec::Lab(s) => {
                let (play, made_seats) = LabPlay::start(rules, s)?;
                let run = RunSpec { config: s.config(), labels: play.labels(), ..base() };
                let made = caught(|| Runner::new(rules, run, seats(made_seats)));
                (Play::Lab(Box::new(play)), made)
            }
            Spec::Reference(s) => {
                let play = ReferencePlay::start(rules, s)?;
                let run = RunSpec { config: play.config(), ..base() };
                let made = caught(|| Runner::new_with(rules, run, |g| seats(play.seats(g))));
                (Play::Reference(Box::new(play)), made)
            }
        };
        let (runner, ended) = match made {
            Ok(Ok(r)) => (Some(r), None),
            Ok(Err(SimError::Config(e))) => {
                return Err(RunError::Spec(SpecError(format!("the game's settings: {e}"))));
            }
            Ok(Err(other)) => return Err(RunError::Internal(other.to_string())),
            // Making the game (its map, its seats) panicked: deterministic, so the outcome.
            Err(message) => (None, Some(Ended::of_panic(0, message))),
        };
        Ok(Self {
            kind: checked.kind,
            hash: checked.hash,
            play,
            runner,
            digests: Vec::new(),
            last: None,
            broken: None,
            ended,
            new_round: true,
            started,
            cpu0,
        })
    }

    /// Whether the game is over: it ended, or a panic ended it.
    #[must_use]
    pub fn is_over(&self) -> bool {
        self.ended.is_some() || self.runner.as_ref().is_none_or(Runner::is_over)
    }

    /// Where the game stands.
    #[must_use]
    pub fn progress(&self) -> Progress {
        let (round, of) = self.runner.as_ref().map_or((0, 0), |r| {
            let st = r.game().state();
            (st.clock().turn, st.config().turn_limit)
        });
        Progress { round, of, new_round: self.new_round }
    }

    /// The rounds played so far.
    #[must_use]
    pub fn rounds(&self) -> usize {
        self.digests.len()
    }

    /// Plays one driven seat's turn. Nothing once the game is over.
    ///
    /// # Errors
    /// A fault of the runner (a poisoned game that refused a step).
    pub fn step(&mut self) -> Result<(), RunError> {
        if self.is_over() {
            return Ok(());
        }
        let Some(runner) = self.runner.as_mut() else { return Ok(()) };
        let play = &mut self.play;
        let stepped = caught(|| {
            let step = runner.step()?;
            match play {
                Play::Lab(p) => p.hear(runner.game(), &step.events),
                Play::Reference(p) => p.hear(runner.game(), &step.events),
            }
            Ok::<_, SimError>(step.new_round)
        });
        match stepped {
            Ok(Ok(new_round)) => self.new_round = new_round,
            Ok(Err(SimError::Crashed(c))) => self.ended = Some(Ended::of_crash(c)),
            Ok(Err(other)) => return Err(RunError::Internal(other.to_string())),
            Err(message) => {
                let turn = runner.game().turn();
                self.ended = Some(Ended::of_panic(turn, message));
            }
        }
        self.take_digest();
        Ok(())
    }

    /// Takes the digest of the round the last step ended, if it ended one: the round that ended
    /// on turn `t` must be the `t`-th.
    fn take_digest(&mut self) {
        let Some((turn, d)) = self.runner.as_ref().and_then(|r| r.game().last_round()) else {
            return;
        };
        if self.last == Some(turn) {
            return;
        }
        let want = self.digests.len() + 1;
        if usize::try_from(turn).ok() != Some(want) && self.broken.is_none() {
            self.broken = Some(format!("round {want} ended on turn {turn}"));
        }
        self.digests.push(d);
        self.last = Some(turn);
    }

    /// The finished job: its outcome, hashed, with its chain and what was measured.
    ///
    /// # Errors
    /// The chain and the digests taken disagree (a step that ended two rounds, or rounds out of
    /// order): a fault of this code, never a result.
    pub fn finish(mut self) -> Result<Output, RunError> {
        if let Some(why) = &self.broken {
            return Err(RunError::Internal(format!("the digests do not follow the rounds: {why}")));
        }
        let chain = self.runner.as_ref().and_then(|r| r.game().chain().copied());
        let chain = chain.unwrap_or_else(|| DigestChain::new(&self.hash.0));
        if usize::try_from(chain.rounds()).ok() != Some(self.digests.len()) {
            return Err(RunError::Internal(format!(
                "the chain holds {} rounds and the job took {} digests",
                chain.rounds(),
                self.digests.len()
            )));
        }
        if self.ended.is_none() {
            // A panic summing up the game is its outcome too.
            let built = caught(|| match (&self.play, &self.runner) {
                (Play::Lab(p), Some(r)) => {
                    Some(JobResult::Lab(LabOutcome::Finished(Box::new(p.result(r.game())))))
                }
                (Play::Reference(p), Some(r)) => Some(JobResult::Reference(
                    ReferenceOutcome::Finished(Box::new(p.result(r.game()))),
                )),
                _ => None,
            });
            match built {
                Ok(Some(result)) => return self.output(result, chain, None),
                Ok(None) => return Err(RunError::Internal("no game to sum up".to_owned())),
                Err(message) => {
                    let turn = self.runner.as_ref().map_or(0, |r| r.game().turn());
                    self.ended = Some(Ended::of_panic(turn, message));
                }
            }
        }
        let ended = self.ended.clone().ok_or_else(|| RunError::Internal("no outcome".into()))?;
        let result = match &self.play {
            Play::Lab(p) => JobResult::Lab(LabOutcome::Crashed(LabCrashed {
                crash: p.crash(ended.turn, ended.player, &ended.message),
            })),
            Play::Reference(_) => {
                JobResult::Reference(ReferenceOutcome::Crashed(ReferencePlay::crash(&ended.record)))
            }
        };
        self.output(result, chain, ended.trace)
    }

    fn output(
        &self,
        result: JobResult,
        chain: DigestChain,
        trace: Option<String>,
    ) -> Result<Output, RunError> {
        let result_hash = result.hash(self.kind).map_err(|e| RunError::Internal(e.0))?;
        let now = ThreadTime::try_now().ok().map(|t| t.as_duration());
        let cpu_s = self.cpu0.zip(now).map_or(0.0, |(a, b)| b.saturating_sub(a).as_secs_f64());
        Ok(Output {
            result,
            result_hash,
            chain: Chain::of(&chain, &self.digests),
            wall_s: self.started.elapsed().as_secs_f64(),
            cpu_s,
            trace,
        })
    }
}

/// Plays the job `checked` describes to its end under `rules`, asking `control` before each seat
/// step whether to go on.
///
/// # Errors
/// As [`Job::start`] and [`Job::finish`], and [`RunError::Stopped`] when the control stops it.
pub fn run(
    rules: &'static Ruleset,
    checked: &Checked,
    control: &mut dyn Control,
) -> Result<Output, RunError> {
    let mut job = Job::start(rules, checked)?;
    while !job.is_over() {
        if control.between(&job.progress()) == Flow::Stop {
            return Err(RunError::Stopped);
        }
        job.step()?;
    }
    job.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job moves between threads: into the bindings' detached calls, and onto a job host's
    /// run thread (4-08).
    use citar_engine::base::ids::NegotiationId;
    use citar_engine::game::{DriverOutcome, Game, SeatDriver};
    use citar_engine::state::players::DriverMemory;
    use serde_json::json;

    use crate::hash::chain_check;
    use crate::spec::check_spec;

    /// A seat's driver that plays its bot until turn `at`, then panics.
    struct PanicAt {
        bot: Box<dyn SeatDriver>,
        at: Turn,
    }

    impl SeatDriver for PanicAt {
        fn play_turn(
            &mut self,
            g: &mut Game,
            pid: PlayerId,
            m: &mut DriverMemory,
        ) -> DriverOutcome {
            assert!(
                g.turn() < self.at,
                "the test bot panics at 0x7ffd00001234 on turn {}",
                g.turn()
            );
            self.bot.play_turn(g, pid, m)
        }

        fn respond(
            &mut self,
            g: &mut Game,
            pid: PlayerId,
            nid: NegotiationId,
            m: &mut DriverMemory,
        ) -> DriverOutcome {
            self.bot.respond(g, pid, nid, m)
        }
    }

    fn panicking(at: Turn) -> impl FnMut(Seats) -> Seats {
        move |seats: Seats| {
            seats
                .into_iter()
                .map(|(p, bot)| {
                    let d: Box<dyn SeatDriver> =
                        if p == PlayerId(1) { Box::new(PanicAt { bot, at }) } else { bot };
                    (p, d)
                })
                .collect()
        }
    }

    fn crashed(kind: Kind, spec: &serde_json::Value) -> Output {
        let rules = Ruleset::shared();
        let checked = check_spec(rules, kind, spec, None).expect("a good spec");
        let mut job = Job::start_seated(rules, &checked, &mut panicking(4)).expect("started");
        let mut steps = 0;
        while !job.is_over() {
            job.step().expect("a step");
            steps += 1;
            assert!(steps < 200, "the crash ends the game");
        }
        let out = job.finish().expect("an outcome");
        chain_check(&checked.hash, &out.chain).expect("the chain to the crash folds");
        assert_eq!(out.chain.rounds, 3, "three rounds ended before the crash");
        out
    }

    /// A panic in a seat's turn is the crash outcome: the turn, the seat, its label and the
    /// message without its address; the rounds before it chained; the same twice.
    #[test]
    fn a_panic_is_the_crash_outcome_with_the_chain_to_it() {
        let lab = json!({"exp": "c", "i": 0, "seed": 3, "map_type": "continents", "size": "duel",
                         "speed": "Quick", "turns": 10, "difficulty": "Prince",
                         "seats": [{"label": "a"}, {"label": "b"}]});
        let out = crashed(Kind::LabGame, &lab);
        let JobResult::Lab(LabOutcome::Crashed(c)) = &out.result else {
            panic!("a crash outcome: {:?}", out.result)
        };
        assert_eq!(
            (c.crash.turn, c.crash.player, c.crash.label.as_deref()),
            (4, Some(1), Some("b"))
        );
        assert_eq!(c.crash.message, "the test bot panics at 0x? on turn 4");
        assert_eq!(crashed(Kind::LabGame, &lab).result_hash, out.result_hash, "reproduced");
        let v = serde_json::to_value(&out.result).expect("a value");
        assert_eq!(crate::hash::result_hash(Kind::LabGame, &v), Ok(out.result_hash.clone()));

        let reference = json!({"i": 0, "seed": 3, "size": "duel", "map_type": "continents",
                               "barbarians": "normal", "speed": "Quick", "turn_limit": 10});
        let out = crashed(Kind::ReferenceGame, &reference);
        let JobResult::Reference(ReferenceOutcome::Crashed(c)) = &out.result else {
            panic!("a crash outcome: {:?}", out.result)
        };
        assert_eq!(c.crash, "T4 P1: panic: the test bot panics at 0x? on turn 4");
    }

    #[test]
    fn a_job_is_send() {
        fn send<T: Send>() {}
        send::<Job>();
        send::<Output>();
    }
}

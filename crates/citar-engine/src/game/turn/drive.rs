//! Drivers: whoever plays a seat that no person or model plays (DESIGN.md 6.12).
//!
//! The engine owns the state machine, [`Game::drive`]; the drivers are trait objects the host
//! supplies ([`SeatDriver`]): testkit's `RandomAgent` in Phase 1, the bot in Phase 2. So the
//! engine never depends on bot code, and Python's `run_ai` loop (`bots/headless.py:47-63`)
//! becomes `drive` plus drivers.
//!
//! `drive` plays seat after seat: it hands the driver the turn and a copy of the seat's
//! [`DriverMemory`], writes the memory back when the driver changed it, and then ends the turn,
//! as `run_ai` did (`play_turn(end_turn=False)`, then `g.end_turn`). The seat keeps its memory
//! the whole time, so a digest, a snapshot or a round's end never sees the seat without it
//! (DESIGN.md 6.12). Ending the turn is `drive`'s alone: while a driver plays, the game refuses
//! to end or force a turn, so the round a driver's turn closes is digested with the memory that
//! turn left, whoever asked for the turn to end. It stops at a seat with no driver
//! ([`Stop::External`]: a person, a model, an MCP client) and when the game is over
//! ([`Stop::GameOver`]). Package 1c-09 adds the other stops (a hybrid seat's diplomat, a reply
//! awaited, a limit on seats per call) and answers negotiations through [`SeatDriver::respond`].

use super::super::Game;
use crate::base::ids::{NegotiationId, PlayerId};
use crate::base::sets::PlayerVec;
use crate::game::derive::rev::PlayerTouch;
use crate::game::error::{ActionError, ErrCode};
use crate::game::events::EventBatch;
use crate::game::invariants::{Code, Violation};
use crate::state::Phase;
use crate::state::players::DriverMemory;

/// What a driver did with its turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DriverOutcome {
    /// It has played: [`Game::drive`] ends the turn.
    Done,
}

/// Whoever plays a seat for the host: `Send`, since hosts drive games inside
/// `py.allow_threads`, whose closure must be (DESIGN.md 6.12).
pub trait SeatDriver: Send {
    /// Plays `pid`'s turn: acts through [`Game::act`] and returns, and [`Game::drive`] ends the
    /// turn. A driver does not end the turn itself: while it plays, [`Game::end_turn`] and
    /// [`Game::force_turn`] refuse. `mem` is a copy of the seat's own memory, kept in the save and
    /// written back when the driver changes it; a driver that keeps none leaves it as it was
    /// given.
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome;

    /// Answers negotiation `nid`, which waits on `pid` (package 1c-09 calls it).
    fn respond(
        &mut self,
        g: &mut Game,
        pid: PlayerId,
        nid: NegotiationId,
        mem: &mut DriverMemory,
    ) -> DriverOutcome;
}

/// The drivers of a game's seats, by player: `None` for a seat the host plays itself.
pub struct Drivers<'a> {
    pub seats: PlayerVec<Option<&'a mut dyn SeatDriver>>,
}

impl<'a> Drivers<'a> {
    /// No drivers yet, for `n` players.
    #[must_use]
    pub fn none(n: usize) -> Self {
        let mut seats = PlayerVec::with_capacity(n);
        for _ in 0..n {
            // At most 64 players, which an IdVec of PlayerIds holds.
            if seats.push(None).is_err() {
                break;
            }
        }
        Self { seats }
    }

    /// Gives seat `p` a driver.
    #[must_use]
    pub fn with(mut self, p: PlayerId, d: &'a mut dyn SeatDriver) -> Self {
        if let Some(slot) = self.seats.get_mut(p) {
            *slot = Some(d);
        }
        self
    }
}

/// Why [`Game::drive`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Stop {
    /// It is the turn of a seat the host plays: a person, a model, an MCP client.
    External(PlayerId),
    /// The game is over, or no major civilization is left to play.
    GameOver,
}

/// How [`Game::drive`] drives. Package 1c-09 adds the limit on seats per call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DriveOptions {}

/// The memory a seat's driver starts from when the seat has kept none: kind 0, which no driver
/// uses, so a driver that keeps nothing leaves the seat without memory.
fn no_memory() -> DriverMemory {
    DriverMemory::empty(0, 0)
}

impl Game {
    /// Plays the seats that have drivers, turn after turn, until a seat without one is to play or
    /// the game is over (DESIGN.md 6.12). Returns why it stopped, and every event the turns
    /// appended. City-states and the barbarians play inside the turns, as ever.
    ///
    /// Refused on a game stopped by an internal error, and from inside a driver.
    pub fn drive(
        &mut self,
        d: &mut Drivers<'_>,
        _opts: DriveOptions,
    ) -> Result<(Stop, EventBatch), ActionError> {
        self.ensure_live()?;
        self.ensure_not_driving()?;
        let first = self.st.host().next_event_id;
        let stop = loop {
            if self.phase() != Phase::Playing || self.majors(true).next().is_none() {
                // With no major civilization left there is nobody to drive: a game whose last
                // one is gone ends there, whether or not a victory ended it.
                break Stop::GameOver;
            }
            let pid = self.current();
            if !self.st.clock().turn_started {
                self.begin_turn();
                self.settle();
                continue;
            }
            let automatic = self.player(pid).is_none_or(|p| !p.is_major() || !p.alive());
            if automatic {
                // Only a game set up so, as a probe's forced turn, stops on another's turn.
                self.end_turn_now(pid)?;
                self.settle();
                continue;
            }
            let Some(driver) = d.seats.get_mut(pid).and_then(Option::as_mut) else {
                break Stop::External(pid);
            };
            // The seat keeps its memory while the driver works on a copy.
            let mut mem =
                self.player(pid).and_then(|p| p.seat().driver()).cloned().unwrap_or_else(no_memory);
            self.driving = Some(pid);
            let outcome = driver.play_turn(self, pid, &mut mem);
            self.driving = None;
            match outcome {
                DriverOutcome::Done => {}
            }
            let kept = (mem != no_memory()).then_some(mem);
            let changed = self.player(pid).is_some_and(|p| p.seat().driver() != kept.as_ref());
            if changed && let Some(p) = self.player_mut(pid, PlayerTouch::OTHER) {
                p.put_driver(kept);
            }
            self.ensure_live()?;
            if self.phase() != Phase::Playing {
                continue;
            }
            if self.current() == pid {
                self.end_turn_now(pid)?;
            } else if self.debug.invariants {
                // Nothing a driver may call passes the turn while it plays.
                self.report(Violation::new(
                    Code::Turn1,
                    format!("player {}'s turn passed while its driver played it", pid.0),
                ));
            }
            self.settle();
        };
        self.batch_start = first;
        Ok((stop, self.take_batch()))
    }

    /// Refuses to end, force or drive a turn while a seat's driver plays inside [`Game::drive`]:
    /// `drive` ends that turn once the driver has returned and the seat has its memory back.
    pub(crate) fn ensure_not_driving(&self) -> Result<(), ActionError> {
        match self.driving {
            None => Ok(()),
            Some(p) => Err(ActionError::new(
                ErrCode::Rule,
                format!(
                    "Player {} is being played by its driver, whose turn ends when it returns: a \
                     driver neither ends nor passes the turn itself.",
                    p.0
                ),
            )),
        }
    }
}

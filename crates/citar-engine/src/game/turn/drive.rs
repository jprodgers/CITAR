//! Drivers: whoever plays a seat that no person or model plays (DESIGN.md 6.12).
//!
//! The engine owns the state machine, [`Game::drive`]; the drivers are trait objects the host
//! supplies ([`SeatDriver`]): testkit's `RandomAgent` in Phase 1, the bot in Phase 2. So the
//! engine never depends on bot code, and Python's `run_ai` loop (`bots/headless.py:47-63`)
//! becomes `drive` plus drivers.
//!
//! `drive` plays seat after seat: it takes the seat's [`DriverMemory`] out of the seat, hands it
//! to the driver with the turn, puts it back, and ends the turn if the driver has not. It stops
//! at a seat with no driver ([`Stop::External`]: a person, a model, an MCP client) and when the
//! game is over ([`Stop::GameOver`]). Package 1c-09 adds the other stops (a hybrid seat's
//! diplomat, a reply awaited, a limit on seats per call) and answers negotiations through
//! [`SeatDriver::respond`].

use super::super::Game;
use crate::base::ids::{NegotiationId, PlayerId};
use crate::base::sets::PlayerVec;
use crate::game::derive::rev::PlayerTouch;
use crate::game::error::ActionError;
use crate::game::events::EventBatch;
use crate::state::Phase;
use crate::state::players::DriverMemory;

/// What a driver did with its turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DriverOutcome {
    /// It has played: [`Game::drive`] ends the turn, if it is still the seat's.
    Done,
}

/// Whoever plays a seat for the host: `Send`, since hosts drive games inside
/// `py.allow_threads`, whose closure must be (DESIGN.md 6.12).
pub trait SeatDriver: Send {
    /// Plays `pid`'s turn: acts through [`Game::act`], and may end the turn itself. `mem` is the
    /// seat's own memory, kept in the save; a driver that keeps none leaves it as it was given.
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
    /// Refused only on a game stopped by an internal error.
    pub fn drive(
        &mut self,
        d: &mut Drivers<'_>,
        _opts: DriveOptions,
    ) -> Result<(Stop, EventBatch), ActionError> {
        self.ensure_live()?;
        let first = self.st.host().next_event_id;
        let stop = loop {
            if self.phase() != Phase::Playing || self.majors(true).next().is_none() {
                // A game whose last major civilization is gone ends at its eliminations
                // (package 1c-08); until then there is nobody to drive.
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
            let mut mem = self
                .player_mut(pid, PlayerTouch::OTHER)
                .and_then(|p| p.take_driver())
                .unwrap_or_else(no_memory);
            let outcome = driver.play_turn(self, pid, &mut mem);
            let kept = (mem != no_memory()).then_some(mem);
            if let Some(p) = self.player_mut(pid, PlayerTouch::OTHER) {
                p.put_driver(kept);
            }
            match outcome {
                DriverOutcome::Done => {}
            }
            self.ensure_live()?;
            if self.phase() == Phase::Playing && self.current() == pid {
                self.end_turn_now(pid)?;
            }
            self.settle();
        };
        self.batch_start = first;
        Ok((stop, self.take_batch()))
    }
}

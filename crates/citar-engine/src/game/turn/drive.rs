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
//! turn left, whoever asked for the turn to end.
//!
//! A negotiation that waits on a seat with a driver is put to that driver
//! ([`SeatDriver::respond`]), whoever's turn it is, as Python's `resolve_negotiations`
//! (`bots/headless.py:14-22`) and the session's responders (`server/session.py:268-301`) did:
//! at every step of `drive`, so a chat the host's seat opened gets the driver's answer on the
//! next call, and those a driver opened are answered before its turn ends. A driver may leave a
//! chat to the host ([`DriverOutcome::Deferred`]), as a hybrid seat's bot leaves a question to
//! the seat's language model; until the chat moves, it waits on the host as a chat with a seat
//! the host plays does.
//!
//! It stops ([`Stop`]):
//! - at a seat with no driver (`External`: a person, a model, an MCP client), which the host
//!   plays and ends;
//! - after the driver of a hybrid seat has played (`HybridDiplomat`), so that the host runs the
//!   seat's language model for its diplomacy (plan H1); the next `drive` ends the turn;
//! - when the seat whose driver has played is in a chat that waits on the host: on a seat with
//!   no driver, or on a seat, itself included, whose driver left it to the host (`AwaitingReply`,
//!   rule T3: ending the turn waits for the answer). The host has it answered, or closes it when
//!   its timeout runs out, and drives again;
//! - once it has ended the turns of as many driven seats as [`DriveOptions::seat_limit`] allows
//!   (`SeatLimit`), so a server can let go of a large game between seats;
//! - when the game is over (`GameOver`).
//!
//! A stop inside a turn (`HybridDiplomat`, `AwaitingReply`) leaves a mark in the host heads
//! (`DriveMark`: saved, never digested), so the next `drive`, even on a game loaded from a save
//! taken there, goes on from where it stopped rather than playing the turn again.

use core::num::NonZeroU32;
use std::collections::BTreeMap;

use smallvec::SmallVec;

use super::super::Game;
use crate::base::ids::{NegotiationId, PlayerId};
use crate::base::sets::PlayerVec;
use crate::game::derive::rev::PlayerTouch;
use crate::game::error::{ActionError, ErrCode};
use crate::game::events::EventBatch;
use crate::game::invariants::{Code, Violation};
use crate::state::Phase;
use crate::state::chronicle::DriveMark;
use crate::state::diplo::{NegStatus, Negotiation};
use crate::state::players::{Controller, DriverMemory};

/// What a driver did with its turn, or with a negotiation it was asked to answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DriverOutcome {
    /// It has played, or answered as it will: [`Game::drive`] goes on. A negotiation a driver
    /// leaves unanswered this way is its answer: the chat waits on the seat until it expires
    /// with its opener's turn, as in Python's headless loop.
    Done,
    /// From [`SeatDriver::respond`]: it has left the negotiation to the host, as a hybrid seat's
    /// bot leaves a question to the seat's language model (plan H1). Until the negotiation moves,
    /// the drive counts it as waiting on the host (rule T3): a driven seat in it, the deferring
    /// seat included, does not end its turn but stops the drive with [`Stop::AwaitingReply`].
    /// The next `drive` asks the driver again, so a host whose wait has run out has the bot
    /// decide by driving with a driver that answers this time. From
    /// [`SeatDriver::play_turn`], the same as `Done`.
    Deferred,
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

    /// Answers negotiation `nid`, which waits on `pid`, whoever's turn it is: through
    /// [`Game::act`] with `respond_negotiation`, which may be used at any time. `mem` is as for
    /// [`play_turn`](Self::play_turn). A driver that leaves it unanswered is not asked again in
    /// the same drive until the chat moves; one that leaves it to the host (a hybrid seat's bot
    /// leaving a question to its language model) says so, [`DriverOutcome::Deferred`].
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

    /// Whether seat `p` has a driver.
    #[must_use]
    pub fn drives(&self, p: PlayerId) -> bool {
        self.seats.get(p).is_some_and(Option::is_some)
    }
}

/// Why [`Game::drive`] returned.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Stop {
    /// It is the turn of a seat the host plays: a person, a model, an MCP client. The host plays
    /// it and ends it ([`Game::end_turn`]), then drives on.
    External(PlayerId),
    /// The driver of a hybrid seat has played its turn, which is still the seat's: the host runs
    /// the seat's language model for its diplomacy now, then drives on, which ends the turn
    /// (or ends it itself).
    HybridDiplomat(PlayerId),
    /// The driven seat `pid`, whose driver has played, is in negotiations `nids` that wait on the
    /// host (rule T3): on a seat the host plays, or on a driven seat, `pid` itself included,
    /// whose driver left them to the host ([`DriverOutcome::Deferred`]; a hybrid seat's
    /// language model answers them). The host has each answered by whoever plays the seat it
    /// waits on, or closes one whose wait runs out ([`Game::close_negotiation`]), or has a
    /// deferring seat's bot decide on the next drive, then drives on: an answer that comes back
    /// to a driven seat is put to its driver, and the turn ends once nothing waits on the host.
    AwaitingReply { pid: PlayerId, nids: SmallVec<[NegotiationId; 2]> },
    /// It has ended the turns of as many driven seats as the options allow; the game goes on
    /// with the next `drive`.
    SeatLimit,
    /// The game is over, or no major civilization is left to play.
    GameOver,
}

/// How [`Game::drive`] drives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DriveOptions {
    /// The most turns of driven seats one call ends before it returns [`Stop::SeatLimit`]; no
    /// limit when `None`.
    pub seat_limit: Option<NonZeroU32>,
}

impl DriveOptions {
    /// These options, returning after `n` driven seats' turns (no limit for 0).
    #[must_use]
    pub const fn with_seat_limit(mut self, n: u32) -> Self {
        self.seat_limit = NonZeroU32::new(n);
        self
    }
}

/// The most rounds of answers one step of [`Game::drive`] asks for. Each answer either adds an
/// entry to its chat, whose message cap closes it, or is not asked for again in the drive until
/// the chat moves, so the rounds run out on their own; this only bounds a pair of drivers that
/// talk to each other forever under a very large cap.
const ANSWER_ROUNDS: usize = 64;

/// The memory a seat's driver starts from when the seat has kept none: kind 0, which no driver
/// uses, so a driver that keeps nothing leaves the seat without memory.
fn no_memory() -> DriverMemory {
    DriverMemory::empty(0, 0)
}

/// What one call of [`Game::drive`] last put to a driver about a negotiation.
#[derive(Clone, Copy, Debug)]
struct Asked {
    /// The entries the negotiation had.
    len: usize,
    /// The seat it waited on.
    seat: PlayerId,
    /// Whether the driver left it to the host ([`DriverOutcome::Deferred`]).
    deferred: bool,
}

impl Asked {
    /// Whether this is where `n` still stands: it has not moved since.
    fn stands(&self, n: &Negotiation) -> bool {
        n.status == NegStatus::Open && n.awaiting == Some(self.seat) && n.history.len() == self.len
    }
}

/// The negotiations one call of `drive` has put to drivers, by id: only open ones are kept
/// (ids are never reused, and a closed negotiation never opens again), so a lookup costs the
/// logarithm of the open chats however long the call plays.
type AskedMap = BTreeMap<NegotiationId, Asked>;

/// Whether the driver of the seat `n` waits on has left it to the host, and it has not moved.
fn left_to_host(asked: &AskedMap, n: &Negotiation) -> bool {
    asked.get(&n.id).is_some_and(|a| a.deferred && a.stands(n))
}

impl Game {
    /// Plays the seats that have drivers, turn after turn, until the host has something to do or
    /// the game is over (DESIGN.md 6.12; the stops are [`Stop`]'s). Returns why it stopped, and
    /// every event the turns appended. City-states and the barbarians play inside the turns, as
    /// ever, and the negotiations that wait on driven seats are put to their drivers.
    ///
    /// Refused on a game stopped by an internal error, and from inside a driver.
    pub fn drive(
        &mut self,
        d: &mut Drivers<'_>,
        opts: DriveOptions,
    ) -> Result<(Stop, EventBatch), ActionError> {
        self.ensure_live()?;
        self.ensure_not_driving()?;
        let first = self.st.host().next_event_id;
        let mut ended: u32 = 0;
        let mut asked = AskedMap::new();
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
            self.answer_waiting(d, &mut asked)?;
            if self.phase() != Phase::Playing || self.current() != pid {
                continue;
            }
            if !d.drives(pid) {
                break Stop::External(pid);
            }
            let Some(mark) = self.drive_mark(pid) else {
                if opts.seat_limit.is_some_and(|n| ended >= n.get()) {
                    break Stop::SeatLimit;
                }
                self.play(d, pid)?;
                if self.phase() == Phase::Playing && self.current() != pid && self.debug.invariants
                {
                    // Nothing a driver may call passes the turn while it plays.
                    self.report(Violation::new(
                        Code::Turn1,
                        format!("player {}'s turn passed while its driver played it", pid.0),
                    ));
                }
                self.settle();
                // Around again: the answers its turn asked for come first.
                continue;
            };
            let hybrid =
                self.player(pid).is_some_and(|p| p.seat().controller() == Controller::Hybrid);
            if hybrid && !mark.diplomat {
                self.st.host_mut().drive = Some(DriveMark { diplomat: true, ..mark });
                break Stop::HybridDiplomat(pid);
            }
            let nids = self.awaiting_the_host(d, &asked, pid);
            if !nids.is_empty() {
                break Stop::AwaitingReply { pid, nids };
            }
            self.end_turn_now(pid)?;
            ended = ended.saturating_add(1);
            self.settle();
        };
        self.batch_start = first;
        Ok((stop, self.take_batch()))
    }

    /// The mark of a stop inside `pid`'s turn: its driver has played this turn.
    fn drive_mark(&self, pid: PlayerId) -> Option<DriveMark> {
        self.st.host().drive.filter(|m| m.turn == self.turn() && m.player == pid)
    }

    /// Forgets where `drive` stopped inside a turn: the turn has begun anew or ended.
    pub(crate) fn clear_drive_mark(&mut self) {
        if self.st.host().drive.is_some() {
            self.st.host_mut().drive = None;
        }
    }

    /// `pid`'s driver plays its turn, and the turn is marked played.
    fn play(&mut self, d: &mut Drivers<'_>, pid: PlayerId) -> Result<(), ActionError> {
        let Some(driver) = d.seats.get_mut(pid).and_then(Option::as_mut) else { return Ok(()) };
        self.with_driver(pid, |g, mem| driver.play_turn(g, pid, mem))?;
        let turn = self.turn();
        self.st.host_mut().drive = Some(DriveMark { turn, player: pid, diplomat: false });
        Ok(())
    }

    /// Puts every open negotiation that waits on a driven seat to that seat's driver, round
    /// after round while the answers bring more, as Python's `resolve_negotiations` did
    /// (`bots/headless.py:14-22`). A chat is put to a driver once in a drive for each entry it
    /// has (`asked`): one the driver leaves unanswered, or leaves to the host, is not asked
    /// again until it moves.
    fn answer_waiting(
        &mut self,
        d: &mut Drivers<'_>,
        asked: &mut AskedMap,
    ) -> Result<(), ActionError> {
        asked.retain(|&nid, _| self.negotiation(nid).is_some_and(|n| n.status == NegStatus::Open));
        for _ in 0..ANSWER_ROUNDS {
            let waiting: Vec<(NegotiationId, Asked)> = self
                .st
                .diplo()
                .negotiations
                .iter()
                .filter(|n| n.status == NegStatus::Open)
                .filter_map(|n| {
                    let seat = n.awaiting.filter(|&p| d.drives(p))?;
                    let put = asked.get(&n.id).is_some_and(|a| a.stands(n));
                    (!put).then_some((n.id, Asked { len: n.history.len(), seat, deferred: false }))
                })
                .collect();
            if waiting.is_empty() {
                break;
            }
            for (nid, now) in waiting {
                if self.phase() != Phase::Playing {
                    return Ok(());
                }
                // An answer earlier in the round may have settled it.
                if !self.negotiation(nid).is_some_and(|n| now.stands(n)) {
                    continue;
                }
                let p = now.seat;
                let Some(driver) = d.seats.get_mut(p).and_then(Option::as_mut) else { continue };
                let outcome = self.with_driver(p, |g, mem| driver.respond(g, p, nid, mem))?;
                asked.insert(nid, Asked { deferred: outcome == DriverOutcome::Deferred, ..now });
                self.settle();
            }
        }
        Ok(())
    }

    /// The open negotiations `pid` is in that wait on the host, which rule T3 makes the end of
    /// its turn wait for: on another seat without a driver, or on a seat, `pid` included, whose
    /// driver left them to the host and that has not moved since.
    fn awaiting_the_host(
        &self,
        d: &Drivers<'_>,
        asked: &AskedMap,
        pid: PlayerId,
    ) -> SmallVec<[NegotiationId; 2]> {
        self.st
            .diplo()
            .negotiations
            .iter()
            .filter(|n| n.status == NegStatus::Open && (n.initiator == pid || n.responder == pid))
            .filter(|n| {
                n.awaiting.is_some_and(|q| (q != pid && !d.drives(q)) || left_to_host(asked, n))
            })
            .map(|n| n.id)
            .collect()
    }

    /// Runs `f`, a driver's call for seat `p`, as a driver plays: `driving` set, so the game
    /// refuses to end or force a turn inside it; on a copy of the seat's memory, written back
    /// only when it changed (kind 0 and empty is kept as none).
    fn with_driver(
        &mut self,
        p: PlayerId,
        f: impl FnOnce(&mut Self, &mut DriverMemory) -> DriverOutcome,
    ) -> Result<DriverOutcome, ActionError> {
        let mut mem =
            self.player(p).and_then(|x| x.seat().driver()).cloned().unwrap_or_else(no_memory);
        self.driving = Some(p);
        let outcome = f(self, &mut mem);
        self.driving = None;
        let kept = (mem != no_memory()).then_some(mem);
        let changed = self.player(p).is_some_and(|x| x.seat().driver() != kept.as_ref());
        if changed && let Some(x) = self.player_mut(p, PlayerTouch::OTHER) {
            x.put_driver(kept);
        }
        self.ensure_live()?;
        Ok(outcome)
    }

    /// The seat whose driver is playing its turn or answering a negotiation inside
    /// [`Game::drive`], if any: while there is one, the game ends, forces and drives no turn.
    #[must_use]
    pub const fn driving(&self) -> Option<PlayerId> {
        self.driving
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

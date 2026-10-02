//! The bot as the engine's `SeatDriver` (DESIGN.md P2.3.6), and [`Turn`], the only holder of
//! `&mut Game` in the crate (`cargo xtask check` refuses the text anywhere else): every other
//! file reads `&Game` and acts through `Turn::act`, so the bot cannot reach the engine's public
//! `&mut Game` functions except through `Game::act`.
//!
//! `Turn` replaces Python's `ex` (basic.py:697-707): the bot proposes freely and the rules
//! refuse, so a refusal is normal, never an error or a panic, and is counted by tool
//! ([`Refusals`]). The bot never ends a turn: `Game::drive` does.
//!
//! The stub of package 2-00a plays every version as `idle`; 2-01a calls `basic-1`'s phases in
//! Python's order (`play_turn`, basic.py:740-757), each filled in by its package.

use std::sync::Arc;

use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::game::{Action, DriverOutcome, Game, Outcome, SeatDriver};
use citar_engine::state::players::DriverMemory;

use crate::versions::VersionId;
use crate::{Bot, Refusals, idle};

/// One call of a bot on a game: the game, the seat it plays and its counts.
pub(crate) struct Turn<'g> {
    g: &'g mut Game,
    pid: PlayerId,
    refused: &'g mut Refusals,
}

impl Turn<'_> {
    /// The game, to read.
    pub(crate) fn game(&self) -> &Game {
        self.g
    }

    /// The seat the bot plays.
    pub(crate) const fn pid(&self) -> PlayerId {
        self.pid
    }

    /// Takes `a` for the seat through `Game::act`: what it did, or `None` when the rules refused
    /// it, counted either way by its tool.
    pub(crate) fn act(&mut self, a: Action) -> Option<Outcome> {
        let tool = a.tool();
        let done = self.g.act(self.pid, a).ok().map(|(out, _events)| out);
        self.refused.record(tool, done.is_some());
        done
    }
}

impl SeatDriver for Bot {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        let spec = Arc::clone(&self.spec);
        let mut t = Turn { g, pid, refused: &mut self.refusals };
        match spec.version {
            // basic-1 plays as idle until package 2-01a gives it its phases.
            VersionId::Basic1 | VersionId::Idle => idle::play_turn(&mut t),
        }
        // Neither keeps memory yet: the seat's stays as it was.
        let _ = mem;
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        g: &mut Game,
        pid: PlayerId,
        nid: NegotiationId,
        mem: &mut DriverMemory,
    ) -> DriverOutcome {
        let spec = Arc::clone(&self.spec);
        let _ = mem;
        if spec.version != VersionId::Idle
            && g.negotiation(nid).is_some_and(|n| !spec.owners.owns(n))
        {
            // The seat's language model owns it: the drive holds it for the host (DESIGN.md
            // P2.3.8).
            return DriverOutcome::Deferred;
        }
        let mut t = Turn { g, pid, refused: &mut self.refusals };
        idle::respond(&mut t, nid);
        DriverOutcome::Done
    }
}

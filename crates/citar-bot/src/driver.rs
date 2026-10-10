//! The bot as the engine's `SeatDriver` (DESIGN.md P2.3.6), and [`Turn`], the only holder of
//! `&mut Game` in the crate (`cargo xtask check` refuses the text anywhere else): every other
//! file reads `&Game` and acts through `Turn::act`, so the bot cannot reach the engine's public
//! `&mut Game` functions except through `Game::act`.
//!
//! `Turn` replaces Python's `ex` (basic.py:697-707): the bot proposes freely and the rules
//! refuse, so a refusal is normal, never an error or a panic, and is counted by tool
//! ([`Refusals`]). The bot never ends a turn: `Game::drive` does.
//!
//! A `basic-1` seat's turn decodes the seat's memory, prunes it, plays the phases in Python's
//! order (`basic1::play_turn`) and keeps what it then remembers; a seat that has remembered
//! nothing yet is left without memory, so a seat's save carries none until there is something
//! to keep. An answer decodes the memory too (a deal's worth reads the war the seat plans or
//! prepares) and keeps it.
//! The idle bot keeps none.

use std::sync::Arc;

use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::game::{Action, DriverOutcome, Game, Outcome, SeatDriver};
use citar_engine::state::players::DriverMemory;

use crate::basic1::{self, Seat};
use crate::memory::{MEMORY_KIND, Memory};
use crate::versions::VersionId;
use crate::{Bot, BotSpec, Refusals, idle};

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

/// Runs `f` as `basic-1` on the seat with its memory: decoded from `mem` (fresh when it is not
/// basic-1's), pruned when `prune` (at the start of a turn), and written back after, unless the
/// seat had none and still remembers nothing.
fn with_memory(
    t: &mut Turn<'_>,
    spec: &BotSpec,
    mem: &mut DriverMemory,
    prune: bool,
    f: impl FnOnce(&mut Turn<'_>, &mut Seat<'_>),
) {
    let mut memory = Memory::decode(mem);
    let params = spec.tuning.params();
    if prune {
        memory.prune(t.game(), t.pid(), params.site_blacklist_turns, params.c_boat_retry_turns);
    }
    let resolved = spec.tuning.resolved(t.game().rules());
    let mut seat = Seat::new(spec, &resolved, &mut memory);
    f(t, &mut seat);
    if mem.kind() != MEMORY_KIND && memory == Memory::default() {
        return;
    }
    // Pruning holds a seat's memory far under the limit (tests/bot/memory.rs works out the
    // largest a seat of a 64-player game could keep); were it ever over, the seat keeps what
    // it had rather than nothing.
    if let Ok(kept) = memory.encode()
        && kept != *mem
    {
        *mem = kept;
    }
}

impl SeatDriver for Bot {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        let spec = Arc::clone(&self.spec);
        let mut t = Turn { g, pid, refused: &mut self.refusals };
        match spec.version {
            VersionId::Basic1 => with_memory(&mut t, &spec, mem, true, basic1::play_turn),
            VersionId::Idle => idle::play_turn(&mut t),
        }
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
        if spec.version == VersionId::Idle {
            idle::respond(&mut Turn { g, pid, refused: &mut self.refusals }, nid);
            return DriverOutcome::Done;
        }
        if g.negotiation(nid).is_some_and(|n| !spec.owners.owns(n)) {
            // The seat's language model owns it: the drive holds it for the host (DESIGN.md
            // P2.3.8).
            return DriverOutcome::Deferred;
        }
        let mut t = Turn { g, pid, refused: &mut self.refusals };
        with_memory(&mut t, &spec, mem, false, |t, s| basic1::respond(t, s, nid));
        DriverOutcome::Done
    }
}

//! Seat drivers for tests (DESIGN.md 9.5): [`RandomAgent`] plays any seat through the engine's
//! own pipeline, drawing from `Purpose::TestAgent` keyed by `[player, turn]`.
//!
//! It starts in package 1b-03 able only to end its turn; each system package teaches it its own
//! actions by adding a [`Move`] to [`MOVES`], so that the agent exercises new actions as soon as
//! they exist (DESIGN.md 3.4, rule 2). In the end it researches, builds, founds cities, fights
//! when a preview allows it, adopts policies, negotiates, declares war and ends its turn.

use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::{DriverOutcome, Game, SeatDriver};
use citar_engine::state::players::DriverMemory;

/// One kind of move: what the agent may do with its turn, drawing from the turn's stream.
pub type Move = fn(&mut Game, PlayerId, &mut Rng);

/// Every move the agent knows, in the order it tries them each turn. None yet: the agent ends
/// its turn, which [`Game::drive`] does for it once it returns.
pub const MOVES: &[Move] = &[];

/// A driver that plays at random among the actions the engine has, reproducibly: the same game
/// and seat give the same moves.
#[derive(Clone, Debug, Default)]
pub struct RandomAgent {
    turns: u64,
}

impl RandomAgent {
    /// A new agent.
    #[must_use]
    pub const fn new() -> Self {
        Self { turns: 0 }
    }

    /// How many turns it has played.
    #[must_use]
    pub const fn turns(&self) -> u64 {
        self.turns
    }

    /// The stream a turn draws from: the game's seed, keyed by the player and the turn.
    #[must_use]
    pub fn stream(g: &Game, pid: PlayerId) -> Rng {
        let turn = u64::try_from(g.turn()).unwrap_or(0);
        Rng::keyed(g.state().seed(), Purpose::TestAgent, &[u64::from(pid.0), turn])
    }
}

impl SeatDriver for RandomAgent {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, _: &mut DriverMemory) -> DriverOutcome {
        let mut rng = Self::stream(g, pid);
        for m in MOVES {
            m(g, pid, &mut rng);
        }
        self.turns += 1;
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        _: &mut Game,
        _: PlayerId,
        _: NegotiationId,
        _: &mut DriverMemory,
    ) -> DriverOutcome {
        // It opens no negotiation and answers none until package 1c-05 teaches it.
        DriverOutcome::Done
    }
}

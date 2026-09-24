//! Seat drivers for tests (DESIGN.md 9.5): [`RandomAgent`] plays any seat through the engine's
//! own pipeline, drawing from `Purpose::TestAgent` keyed by `[player, turn]`.
//!
//! It started in package 1b-03 able only to end its turn; each system package teaches it its own
//! actions by adding a [`Move`] to [`MOVES`], so that the agent exercises new actions as soon as
//! they exist (DESIGN.md 3.4, rule 2). Package 1c-02 taught it to move, order, promote and
//! upgrade its units. In the end it researches, builds, founds cities, fights
//! when a preview allows it, adopts policies, negotiates, declares war and ends its turn.

use citar_engine::base::ids::{NegotiationId, PlayerId, TileIdx, UnitId};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::path::Mover;
use citar_engine::game::units::actions::{MoveUnit, PromoteUnit, UnitOrder, UpgradeUnit};
use citar_engine::game::units::{promotions, upgrades};
use citar_engine::game::{Action, DriverOutcome, Game, SeatDriver};
use citar_engine::state::players::DriverMemory;

/// One kind of move: what the agent may do with its turn, drawing from the turn's stream.
pub type Move = fn(&mut Game, PlayerId, &mut Rng);

/// Every move the agent knows, in the order it tries them each turn; then it ends its turn, which
/// [`Game::drive`] does for it once it returns.
///
/// - Package 1c-02: [`promote_units`], [`upgrade_units`], [`order_units`] and [`move_units`].
pub const MOVES: &[Move] = &[promote_units, upgrade_units, order_units, move_units];

/// The player's units, in id order: what each unit move goes through. An action may use one up,
/// so each move looks it up again.
fn units_of(g: &Game, pid: PlayerId) -> Vec<UnitId> {
    g.player_units(pid).map(|u| u.id()).collect()
}

/// Takes an action. A refusal is an answer like any other, which an agent playing at random has
/// no use for; what the action did is the game's to keep.
fn play(g: &mut Game, pid: PlayerId, a: Action) {
    let _refused = g.act(pid, a).is_err();
}

/// A unit's id as the tools take it.
fn tool_id(u: UnitId) -> i64 {
    i64::from(u.get())
}

/// Takes a promotion for each unit that may, one of those open to it at random (`promote_unit`).
pub fn promote_units(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        if !promotions::can_promote(g, u) {
            continue;
        }
        let open = promotions::available_promotions(g, u);
        let Some(&p) = rng.pick(&open) else { continue };
        let Some(promotion) = g.rules().name(p).map(str::to_owned) else { continue };
        play(g, pid, Action::PromoteUnit(PromoteUnit { unit_id: tool_id(u), promotion }));
    }
}

/// Upgrades, half the time, each unit whose upgrade its owner could have now (`upgrade_unit`).
pub fn upgrade_units(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        if upgrades::check_upgrade(g, u).refusal.is_some() || !rng.chance(0.5) {
            continue;
        }
        play(g, pid, Action::UpgradeUnit(UpgradeUnit { unit_id: tool_id(u) }));
    }
}

/// The standing orders the agent gives: those that neither end the unit nor wait on a system not
/// ported yet.
const ORDERS: [&str; 5] = ["fortify", "sleep", "heal", "skip", "wake"];

/// Gives one unit in ten a standing order at random (`unit_order`); a civilian told to fortify
/// is refused, which is part of the play.
pub fn order_units(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        if !rng.chance(0.1) {
            continue;
        }
        let Some(&order) = rng.pick(&ORDERS) else { continue };
        play(g, pid, Action::UnitOrder(UnitOrder { unit_id: tool_id(u), order: order.into() }));
    }
}

/// Moves each unit with movement left (`move_unit`): half the time to a tile it reaches this
/// turn, otherwise toward any tile up to eight away, which leaves a standing goto when the path
/// takes more than this turn (or a refusal, when there is none).
pub fn move_units(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        let Some(from) = g.unit(u).filter(|x| x.moves > 0).map(|x| x.tile()) else { continue };
        let target = if rng.chance(0.5) {
            let reach: Vec<TileIdx> = Mover::unit(g, u)
                .map(|m| m.reachable().into_iter().map(|(t, _)| t).collect())
                .unwrap_or_default();
            rng.pick(&reach).copied()
        } else {
            rng.pick(&g.grid().within(from, 8)).copied()
        };
        let Some(t) = target else { continue };
        let (x, y) = g.grid().xy(t);
        let (x, y) = (i64::from(x), i64::from(y));
        play(g, pid, Action::MoveUnit(MoveUnit { unit_id: tool_id(u), x, y }));
    }
}

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

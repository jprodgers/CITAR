//! Seat drivers for tests (DESIGN.md 9.5): [`RandomAgent`] plays any seat through the engine's
//! own pipeline, drawing from `Purpose::TestAgent` keyed by `[player, turn]`.
//!
//! It starts in package 1b-03 able only to end its turn; each system package teaches it its own
//! actions by adding a [`Move`] to [`MOVES`], so that the agent exercises new actions as soon as
//! they exist (DESIGN.md 3.4, rule 2). In the end it researches, builds, founds cities, fights
//! when a preview allows it, adopts policies, negotiates, declares war and ends its turn.
//!
//! Package 1b-07 teaches it to research, to fill its cities' queues, to adopt policies, and now
//! and then to buy what a city builds and a tile beside its borders.

use citar_engine::base::ids::{CityId, NegotiationId, PlayerId, TileIdx};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::cities::borders::{BuyTile, can_buy_tile};
use citar_engine::game::cities::construction::{buildable_items, item_name};
use citar_engine::game::cities::purchase::Buy;
use citar_engine::game::cities::queue::SetProduction;
use citar_engine::game::policies::{AdoptPolicy, adoptable_policies, can_adopt_any};
use citar_engine::game::research::{SetResearch, available_techs};
use citar_engine::game::{Action, DriverOutcome, Game, SeatDriver};
use citar_engine::state::cities::Constructible;
use citar_engine::state::players::DriverMemory;
use serde_json::json;

/// One kind of move: what the agent may do with its turn, drawing from the turn's stream.
pub type Move = fn(&mut Game, PlayerId, &mut Rng);

/// Every move the agent knows, in the order it tries them each turn; then it ends its turn, which
/// [`Game::drive`] does for it once it returns.
pub const MOVES: &[Move] = &[research, production, policies, purchases];

/// Picks one of `v` from the turn's stream.
fn pick<T: Copy>(rng: &mut Rng, v: &[T]) -> Option<T> {
    rng.pick(v).copied()
}

/// With nothing researched, researches a tech it could research now.
fn research(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    if g.player(pid).is_none_or(|p| !p.tech.queue.is_empty()) {
        return;
    }
    let Some(t) = pick(rng, &available_techs(g, pid)) else { return };
    let name = g.rules().name(t).unwrap_or_default().to_owned();
    // A refusal is an answer too: the agent moves on.
    let _refused = g.act(pid, Action::SetResearch(SetResearch { tech: json!(name), append: None }));
}

/// Each city with an empty queue builds something it can build.
fn production(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    for c in cities {
        if g.city(c).is_none_or(|x| !x.queue.is_empty()) {
            continue;
        }
        let items = buildable_items(g, c);
        let all: Vec<Constructible> = items
            .units
            .iter()
            .map(Constructible::Unit)
            .chain(items.buildings.iter().chain(items.wonders.iter()).map(Constructible::Building))
            .collect();
        let Some(item) = pick(rng, &all) else { continue };
        let name = item_name(g.rules(), item).to_owned();
        let a = SetProduction { city_id: i64::from(c.get()), item: json!(name), append: None };
        let _refused = g.act(pid, Action::SetProduction(a));
    }
}

/// Adopts a policy it could adopt, when it has the culture or a free policy.
fn policies(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    if !can_adopt_any(g, pid) {
        return;
    }
    let Some(q) = pick(rng, &adoptable_policies(g, pid)) else { return };
    let name = g.rules().name(q).unwrap_or_default().to_owned();
    let _refused = g.act(pid, Action::AdoptPolicy(AdoptPolicy { policy: json!(name) }));
}

/// Now and then buys what a city builds, and a tile beside a city's borders.
fn purchases(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    let Some(c) = pick(rng, &cities) else { return };
    if rng.below(4) == 0
        && let Some(item) = g.city(c).and_then(|x| x.queue.first().copied())
        && !matches!(item, Constructible::Perpetual(_))
    {
        let name = item_name(g.rules(), item).to_owned();
        let a = Buy { city_id: i64::from(c.get()), item: json!(name), currency: None };
        let _refused = g.act(pid, Action::Buy(a));
    }
    if rng.below(8) == 0
        && let Some(centre) = g.city(c).map(citar_engine::state::cities::City::tile)
    {
        let near: Vec<TileIdx> = g
            .grid()
            .within(centre, 3)
            .into_iter()
            .filter(|&t| can_buy_tile(g, c, t).is_none())
            .collect();
        if let Some(t) = pick(rng, &near) {
            let (x, y) = g.xy(t);
            let a = BuyTile { city_id: i64::from(c.get()), x: i64::from(x), y: i64::from(y) };
            let _refused = g.act(pid, Action::BuyTile(a));
        }
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

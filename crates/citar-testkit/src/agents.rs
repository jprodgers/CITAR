//! Seat drivers for tests (DESIGN.md 9.5): [`RandomAgent`] plays any seat through the engine's
//! own pipeline, drawing from `Purpose::TestAgent` keyed by `[player, turn]`.
//!
//! It started in package 1b-03 able only to end its turn; each system package teaches it its own
//! actions by adding a [`Move`] to [`MOVES`], so that the agent exercises new actions as soon as
//! they exist (DESIGN.md 3.4, rule 2). Package 1c-02 taught it to move, order, promote and
//! upgrade its units. In the end it researches, builds, founds cities, fights
//! when a preview allows it, adopts policies, negotiates, declares war and ends its turn.
//!
//! Package 1b-07 teaches it every action of its own: to research (now and then a far goal, an
//! appended tech, a tech dropped from the queue, the free techs it is owed), to fill its cities'
//! queues (appending now and then, a conversion of production among what it appends), to edit
//! them, to switch a city's automatic production, to rename a city, to adopt policies, and to buy
//! what a city builds and a tile beside its borders.

use citar_engine::base::ids::{CityId, NegotiationId, PlayerId, TechId, TileIdx, UnitId};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::cities::borders::{BuyTile, can_buy_tile};
use citar_engine::game::cities::construction::{buildable_items, item_name};
use citar_engine::game::cities::purchase::Buy;
use citar_engine::game::cities::queue::{
    ChangeQueue, QueueEdit, RenameCity, SetAutoProduction, SetProduction,
};
use citar_engine::game::path::Mover;
use citar_engine::game::policies::{AdoptPolicy, adoptable_policies, can_adopt_any};
use citar_engine::game::research::{
    ChooseFreeTech, DequeueResearch, SetResearch, available_techs, is_unresearchable,
};
use citar_engine::game::units::actions::{MoveUnit, PromoteUnit, UnitOrder, UpgradeUnit};
use citar_engine::game::units::{promotions, upgrades};
use citar_engine::game::{Action, DriverOutcome, Game, SeatDriver};
use citar_engine::state::cities::{Constructible, Perpetual};
use citar_engine::state::players::DriverMemory;
use serde_json::json;

/// One kind of move: what the agent may do with its turn, drawing from the turn's stream.
pub type Move = fn(&mut Game, PlayerId, &mut Rng);

/// Every move the agent knows, in the order it tries them each turn; then it ends its turn, which
/// [`Game::drive`] does for it once it returns.
///
/// - Package 1b-07: `research`, `production`, `queues`, `policies`, `purchases` and `names`.
/// - Package 1c-02: [`promote_units`], [`upgrade_units`], [`order_units`] and [`move_units`].
pub const MOVES: &[Move] = &[
    research,
    production,
    queues,
    policies,
    purchases,
    names,
    promote_units,
    upgrade_units,
    order_units,
    move_units,
];

/// Picks one of `v` from the turn's stream.
fn pick<T: Copy>(rng: &mut Rng, v: &[T]) -> Option<T> {
    rng.pick(v).copied()
}

/// A tech's name, as a player types it.
fn tech_name(g: &Game, t: TechId) -> String {
    g.rules().name(t).unwrap_or_default().to_owned()
}

/// Researches: the free techs it is owed, then, with nothing researched, a tech it could research
/// now or now and then a far one, whose path is queued; with a queue, now and then a tech
/// appended or one dropped.
fn research(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let Some(pl) = g.player(pid) else { return };
    let (queue, free) = (pl.tech.queue.clone(), pl.tech.free_techs);
    // A refusal is an answer too: the agent moves on.
    if free > 0
        && let Some(t) = pick(rng, &available_techs(g, pid))
    {
        let tech = json!(tech_name(g, t));
        let _refused = g.act(pid, Action::ChooseFreeTech(ChooseFreeTech { tech }));
    }
    let far = || -> Vec<TechId> {
        g.rules()
            .techs()
            .ids()
            .filter(|&t| !g.has_tech(pid, Some(t)) && !is_unresearchable(g, pid, t))
            .collect()
    };
    if queue.is_empty() {
        let options = if rng.below(4) == 0 { far() } else { available_techs(g, pid) };
        let Some(t) = pick(rng, &options) else { return };
        let tech = json!(tech_name(g, t));
        let _refused = g.act(pid, Action::SetResearch(SetResearch { tech, append: None }));
    } else if rng.below(8) == 0 {
        let Some(t) = pick(rng, &far()) else { return };
        let a = SetResearch { tech: json!(tech_name(g, t)), append: Some(json!(true)) };
        let _refused = g.act(pid, Action::SetResearch(a));
    } else if rng.below(16) == 0
        && let Some(t) = pick(rng, &queue)
    {
        let tech = json!(tech_name(g, t));
        let _refused = g.act(pid, Action::DequeueResearch(DequeueResearch { tech }));
    }
}

/// Fills its cities' queues: a city with an empty queue builds something it can build, and one
/// converting its production now and then builds something again; now and then a city appends
/// an item to its queue, or a conversion of production when it may.
fn production(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    for c in cities {
        let Some(first) = g.city(c).map(|x| x.queue.first().copied()) else { continue };
        let append = match first {
            None => false,
            Some(Constructible::Perpetual(_)) if rng.below(3) == 0 => false,
            Some(_) if rng.below(6) == 0 => true,
            Some(_) => continue,
        };
        let items = buildable_items(g, c);
        let mut all: Vec<Constructible> = items
            .units
            .iter()
            .map(Constructible::Unit)
            .chain(items.buildings.iter().chain(items.wonders.iter()).map(Constructible::Building))
            .collect();
        if append && rng.below(4) == 0 {
            all = [(items.gold, Perpetual::Gold), (items.science, Perpetual::Science)]
                .into_iter()
                .filter(|&(ok, _)| ok)
                .map(|(_, k)| Constructible::Perpetual(k))
                .collect();
        }
        let Some(item) = pick(rng, &all) else { continue };
        let name = item_name(g.rules(), item).to_owned();
        let a = SetProduction {
            city_id: i64::from(c.get()),
            item: json!(name),
            append: append.then(|| json!(true)),
        };
        let _refused = g.act(pid, Action::SetProduction(a));
    }
}

/// Now and then edits a city's queue (moves or removes an entry, or clears it), and switches a
/// city's automatic production.
fn queues(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    let Some(c) = pick(rng, &cities) else { return };
    let len = g.city(c).map_or(0, |x| x.queue.len());
    if len >= 2 && rng.below(4) == 0 {
        let edits = [QueueEdit::Up, QueueEdit::Down, QueueEdit::First, QueueEdit::Last];
        let edit = if rng.below(16) == 0 {
            QueueEdit::Clear
        } else if rng.below(4) == 0 {
            QueueEdit::Remove
        } else {
            pick(rng, &edits).unwrap_or(QueueEdit::Up)
        };
        let index = i64::try_from(rng.below(len as u64)).unwrap_or(0);
        let a = ChangeQueue {
            city_id: i64::from(c.get()),
            action: json!(edit.name()),
            index: Some(index),
        };
        let _refused = g.act(pid, Action::ChangeQueue(a));
    }
    if rng.below(20) == 0 {
        let on = g.city(c).is_some_and(|x| !x.auto_production);
        let a = SetAutoProduction { city_id: i64::from(c.get()), enabled: json!(on) };
        let _refused = g.act(pid, Action::SetAutoProduction(a));
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

/// Now and then renames a city, which a player may do at any time.
fn names(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    if rng.below(30) != 0 {
        return;
    }
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    let Some(c) = pick(rng, &cities) else { return };
    let name = format!("Town {} {}", pid.0, rng.below(1000));
    let _refused = g.act(
        pid,
        Action::RenameCity(RenameCity { city_id: i64::from(c.get()), name: json!(name) }),
    );
}

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

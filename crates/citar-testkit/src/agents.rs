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
//!
//! Package 1c-03 teaches it to fight ([`fight`]): to attack what its units may attack when the
//! preview promises more than it costs (and now and then regardless), to bombard from its cities,
//! to sweep with its fighters, to decide what becomes of the cities it has taken, and to answer
//! the offers to return the civilians it took back from the barbarians.
//!
//! Package 1c-05 teaches it diplomacy ([`diplomacy`]) and espionage ([`spies`]): to answer the
//! negotiations that wait on it (accept, counter, reply or reject, at random), now and then to
//! message a civilization it has met, to open a negotiation with a proposal drawn from a small
//! pool (some of which it cannot give, which is part of the play), rarely to denounce, and after
//! turn 50 rarely to declare war; before it returns it withdraws what it opened and still waits on
//! an answer, as the `end_turn` rule would have it. Its spies go now and then to a city it has
//! explored, or home. [`RandomAgent`] answers a negotiation it is asked about the same way.
//!
//! Until package 1c-09's `drive` dispatches [`SeatDriver::respond`], nothing would answer a chat
//! the agent opens before it withdraws it, so the other side answers at once ([`converse`]), as
//! its own `respond` would: the deals the agent strikes are carried out, and what they leave
//! runs its course when rounds end.

use citar_engine::base::ids::{CityId, NegotiationId, PlayerId, TechId, TileIdx, UnitId};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::cities::borders::{BuyTile, can_buy_tile};
use citar_engine::game::cities::construction::{buildable_items, item_name};
use citar_engine::game::cities::purchase::Buy;
use citar_engine::game::cities::queue::{
    ChangeQueue, QueueEdit, RenameCity, SetAutoProduction, SetProduction,
};
use citar_engine::game::combat::actions::{
    AirSweep, Attack, CityAttack, CityStatus, ReturnCivilian, plan_attack,
};
use citar_engine::game::combat::{city, combatant_at, resolve};
use citar_engine::game::diplomacy::actions::{
    DeclareWar, Denounce, OpenNegotiation, RespondNegotiation, SendMessage,
};
use citar_engine::game::espionage::{MoveSpy, spies as spies_of};
use citar_engine::game::path::Mover;
use citar_engine::game::policies::{AdoptPolicy, adoptable_policies, can_adopt_any};
use citar_engine::game::research::{
    ChooseFreeTech, DequeueResearch, SetResearch, available_techs, is_unresearchable,
};
use citar_engine::game::units::actions::{MoveUnit, PromoteUnit, UnitOrder, UpgradeUnit};
use citar_engine::game::units::{promotions, upgrades};
use citar_engine::game::{Action, DriverOutcome, Game, SeatDriver};
use citar_engine::state::cities::{City, Constructible, Perpetual};
use citar_engine::state::diplo::{NegStatus, Negotiation};
use citar_engine::state::players::DriverMemory;
use citar_engine::state::players::Player;
use serde_json::json;

/// One kind of move: what the agent may do with its turn, drawing from the turn's stream.
pub type Move = fn(&mut Game, PlayerId, &mut Rng);

/// Every move the agent knows, in the order it tries them each turn; then it ends its turn, which
/// [`Game::drive`] does for it once it returns.
///
/// - Package 1b-07: `research`, `production`, `queues`, `policies`, `purchases` and `names`.
/// - Package 1c-02: [`promote_units`], [`upgrade_units`], [`order_units`] and [`move_units`].
/// - Package 1c-03: [`fight`], before the units move, so that those beside an enemy attack it.
/// - Package 1c-05: [`spies`] and [`diplomacy`], the chats last, so that it withdraws what it
///   opened once it has done everything else.
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
    fight,
    move_units,
    spies,
    diplomacy,
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

/// The fates the agent picks for a city it has taken, as `city_status` names them.
const FATES: [&str; 5] = ["annex", "puppet", "raze", "stop_razing", "liberate"];

/// Fights (`attack`, `air_sweep`, `city_attack`, `city_status`, `return_civilian`): each unit
/// with movement left attacks one of the tiles in its range it may attack, when the preview says
/// it deals more than it can take back, or one time in four whatever it says (aircraft and
/// nuclear weapons, which have no preview, one time in four); a fighter sweeps a tile in its
/// range one time in ten; each city that may bombard fires at one of its targets; each city in
/// its hands that is a puppet or burning gets a fate one time in five; and each civilian it took
/// back from the barbarians goes back, or stays, as a coin says.
pub fn fight(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        let Some((from, moves)) = g.unit(u).map(|x| (x.tile(), x.moves)) else { continue };
        if moves <= 0 || !citar_engine::game::units::is_military(g, u) {
            continue;
        }
        let range =
            u32::try_from(citar_engine::game::units::health::attack_range(g, u)).unwrap_or(0);
        let targets: Vec<TileIdx> = g
            .grid()
            .within(from, range)
            .into_iter()
            .filter(|&t| {
                t != from
                    && combatant_at(g, t).is_some_and(|d| {
                        g.at_war(pid, citar_engine::game::combat::combatant::owner(g, d))
                    })
                    && plan_attack(g, u, t).is_ok()
            })
            .collect();
        if citar_engine::game::units::unit_has(
            g,
            u,
            citar_engine::unique::UniqueType::CanAirsweep,
            false,
        ) && rng.chance(0.1)
        {
            let tiles = g.grid().within(from, range);
            if let Some(&t) = rng.pick(&tiles) {
                let (x, y) = g.grid().xy(t);
                let a = AirSweep { unit_id: tool_id(u), x: i64::from(x), y: i64::from(y) };
                play(g, pid, Action::AirSweep(a));
            }
            continue;
        }
        let Some(&t) = rng.pick(&targets) else { continue };
        let worth = resolve::preview(g, u, t).ok().is_some_and(|pv| {
            let dealt = pv["damage_to_defender"][0].as_i64().unwrap_or(0);
            let taken = pv["damage_to_attacker"][1].as_i64().unwrap_or(0);
            dealt > taken
        });
        if worth || rng.chance(0.25) {
            let (x, y) = g.grid().xy(t);
            play(
                g,
                pid,
                Action::Attack(Attack { unit_id: tool_id(u), x: i64::from(x), y: i64::from(y) }),
            );
        }
    }
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    for c in cities {
        let targets = city::bombard_targets(g, c);
        if city::can_bombard(g, c).is_none()
            && let Some(&t) = rng.pick(&targets)
        {
            let (x, y) = g.grid().xy(t);
            let a = CityAttack { city_id: i64::from(c.get()), x: i64::from(x), y: i64::from(y) };
            play(g, pid, Action::CityAttack(a));
        }
        let taken = g.city(c).is_some_and(|x| x.puppet || x.razing);
        if taken
            && rng.chance(0.2)
            && let Some(&fate) = rng.pick(&FATES)
        {
            let a = CityStatus { city_id: i64::from(c.get()), status: json!(fate) };
            play(g, pid, Action::CityStatus(a));
        }
    }
    for u in units_of(g, pid) {
        if g.unit(u).is_some_and(|x| x.return_offer.is_some()) {
            let keep = rng.chance(0.5);
            play(
                g,
                pid,
                Action::ReturnCivilian(ReturnCivilian {
                    unit_id: tool_id(u),
                    keep: Some(json!(keep)),
                }),
            );
        }
    }
}

/// A small pool of deal items, some of which no side can give, as the agent proposes them.
fn deal_items(rng: &mut Rng) -> serde_json::Value {
    let pool = [
        json!([]),
        json!([{"type": "gold", "amount": 10 + rng.below(40)}]),
        json!([{"type": "gold_per_turn", "amount": 1 + rng.below(3), "turns": 10}]),
        json!([{"type": "share_map"}]),
        json!([{"type": "embassy"}]),
        json!([{"type": "open_borders", "turns": 10 + rng.below(20)}]),
        json!([{"type": "declaration_of_friendship"}]),
        json!([{"type": "peace_treaty"}]),
        json!([{"type": "research_agreement"}]),
    ];
    rng.pick(&pool).cloned().unwrap_or_default()
}

/// The responses the agent picks from, accepting twice as often as it does anything else, so
/// that the deals it can carry out are struck.
const RESPONSES: [&str; 6] = ["accept", "accept", "counter", "reply", "reject", "withdraw"];

/// The stream an answer by `pid` in negotiation `nid` draws from: keyed by the negotiation and
/// its length too, so that an answer out of turn draws nothing a turn's moves would, and each
/// answer in a chat draws afresh.
fn answer_stream(g: &Game, pid: PlayerId, nid: NegotiationId) -> Rng {
    let turn = u64::try_from(g.turn()).unwrap_or(0);
    let entries = g.negotiation(nid).map_or(0, |n| u64::try_from(n.history.len()).unwrap_or(0));
    let keys = [u64::from(pid.0), turn, u64::from(nid.get()), entries];
    Rng::keyed(g.state().seed(), Purpose::TestAgent, &keys)
}

/// The most answers [`converse`] plays in one chat.
const EXCHANGES: usize = 4;

/// Plays out a chat just opened: the side it waits on answers, as its driver's `respond` would,
/// until it closes or [`EXCHANGES`] answers have passed.
pub fn converse(g: &mut Game, nid: NegotiationId) {
    for _ in 0..EXCHANGES {
        let open = g.negotiation(nid).filter(|n| n.status == NegStatus::Open);
        let Some(who) = open.and_then(|n| n.awaiting) else { return };
        let mut rng = answer_stream(g, who, nid);
        answer(g, who, nid, &mut rng);
    }
}

/// Answers negotiation `nid` at random: accept, counter with a proposal from the pool, reply or
/// reject (`respond_negotiation`). An answer the game refuses (a deal a side cannot carry out, a
/// counter it cannot give) ends the chat instead, rather than leave it waiting.
fn answer(g: &mut Game, pid: PlayerId, nid: NegotiationId, rng: &mut Rng) {
    let Some(&response) = rng.pick(&RESPONSES) else { return };
    let counter = response == "counter";
    let give = counter.then(|| deal_items(rng));
    let receive = counter.then(|| deal_items(rng));
    let a = RespondNegotiation {
        negotiation_id: i64::from(nid.get()),
        action: json!(response),
        message: Some(json!(format!("{response}, from {}", pid.0))),
        give,
        receive,
    };
    if g.act(pid, Action::RespondNegotiation(a)).is_err() {
        let a = RespondNegotiation {
            negotiation_id: i64::from(nid.get()),
            action: json!("reject"),
            message: Some(json!("Never mind.")),
            give: None,
            receive: None,
        };
        play(g, pid, Action::RespondNegotiation(a));
    }
}

/// The negotiations still open that `pid` is part of and that `f` picks.
fn open_ones(g: &Game, pid: PlayerId, f: impl Fn(&Negotiation) -> bool) -> Vec<NegotiationId> {
    g.negotiations()
        .iter()
        .filter(|n| {
            n.status == NegStatus::Open && (n.initiator == pid || n.responder == pid) && f(n)
        })
        .map(|n| n.id)
        .collect()
}

/// Diplomacy (`respond_negotiation`, `send_message`, `open_negotiation`, `denounce`,
/// `declare_war`): answers what waits on it; one time in ten sends a message to a civilization it
/// has met; one time in ten opens a negotiation with one of them, which plays out at once
/// ([`converse`]); one time in two hundred denounces one; after turn 50 declares war on one one
/// time in two hundred; and withdraws what it opened and still waits on an answer.
pub fn diplomacy(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for nid in open_ones(g, pid, |n| n.awaiting == Some(pid)) {
        answer(g, pid, nid, rng);
    }
    let met: Vec<PlayerId> =
        g.majors(true).map(Player::id).filter(|&q| q != pid && g.has_met(pid, q)).collect();
    if let Some(&to) = rng.pick(&met) {
        if rng.chance(0.1) {
            let a = SendMessage { to: json!(to.0), text: json!("Greetings.") };
            play(g, pid, Action::SendMessage(a));
        }
        if rng.chance(0.1) {
            let give = deal_items(rng);
            let receive = deal_items(rng);
            let a = OpenNegotiation {
                to: i64::from(to.0),
                message: json!("Shall we deal?"),
                give: Some(give),
                receive: Some(receive),
            };
            if let Ok((out, _)) = g.act(pid, Action::OpenNegotiation(a)) {
                let nid = out["negotiation_id"].as_u64().and_then(|n| u32::try_from(n).ok());
                if let Some(nid) = nid.and_then(NegotiationId::new) {
                    converse(g, nid);
                }
            }
        }
        if rng.chance(0.005) {
            play(g, pid, Action::Denounce(Denounce { player_id: i64::from(to.0) }));
        }
        if g.turn() > 50 && rng.chance(0.005) {
            let a = DeclareWar { player_id: i64::from(to.0), message: Some(json!("War!")) };
            play(g, pid, Action::DeclareWar(a));
        }
    }
    for nid in open_ones(g, pid, |n| n.awaiting != Some(pid)) {
        let a = RespondNegotiation {
            negotiation_id: i64::from(nid.get()),
            action: json!("withdraw"),
            message: Some(json!("Another time.")),
            give: None,
            receive: None,
        };
        play(g, pid, Action::RespondNegotiation(a));
    }
}

/// Espionage (`move_spy`): one time in ten, each spy goes to a city its civilization has
/// explored, or home one time in four of those.
pub fn spies(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let names: Vec<String> = spies_of(g, pid).iter().map(|s| s.name.to_string()).collect();
    for name in names {
        if !rng.chance(0.1) {
            continue;
        }
        let city_id = if rng.below(4) == 0 {
            json!("hideout")
        } else {
            let explored: Vec<CityId> = g
                .state()
                .cities()
                .iter()
                .filter(|c| g.player(pid).is_some_and(|p| p.explored.contains(c.tile().0)))
                .map(City::id)
                .collect();
            let Some(c) = pick(rng, &explored) else { continue };
            json!(c.get())
        };
        play(g, pid, Action::MoveSpy(MoveSpy { spy: json!(name), city_id }));
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
        g: &mut Game,
        pid: PlayerId,
        nid: NegotiationId,
        _: &mut DriverMemory,
    ) -> DriverOutcome {
        let mut rng = answer_stream(g, pid, nid);
        answer(g, pid, nid, &mut rng);
        DriverOutcome::Done
    }
}

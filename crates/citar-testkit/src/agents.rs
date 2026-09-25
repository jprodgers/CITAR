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
//! Package 1c-04 teaches it to found cities (`found_city`, and now and then through
//! `unit_action`), to set its workers building (`build_improvement`, a repair, a cancellation or
//! an instant improvement among the choices) or automated, to explore and pillage, and to take
//! its units' special actions (`unit_action`: religions founded, enhanced and spread, great
//! people spent, paradrops, the one-time effects a unit carries).
//!
//! Package 1c-05 teaches it diplomacy ([`diplomacy`]) and espionage ([`spies`]): to answer the
//! negotiations that wait on it (accept, counter, reply or reject, at random), now and then to
//! message a civilization it has met, to open a negotiation with a proposal drawn from a small
//! pool (some of which it cannot give, which is part of the play), rarely to denounce, and after
//! turn 50 rarely to declare war. Its spies go now and then to a city it has explored, or home.
//! [`RandomAgent`] answers a negotiation it is asked about the same way.
//!
//! Package 1c-06 teaches it to deal with the city-states it has met ([`city_states`]: gifts of
//! gold and units, protection pledged and withdrawn, tribute demanded, peace, marriage), and its
//! spies to stage coups now and then.
//!
//! Package 1c-08 teaches it to vote in the United Nations ([`un_vote`]) while voting is open:
//! for a living civilization it has met, for itself, or to abstain.
//!
//! Package 1c-09's `drive` puts a negotiation that waits on a driven seat to its driver
//! ([`SeatDriver::respond`]), so the agent leaves a chat it opens to `drive`: another agent
//! answers it before the opener's turn ends, and one with a seat the host plays stops the drive
//! until the host answers or closes it (rule T3). The agent answered at once for the other side
//! until then (`converse`, gone), drawing from the stream `respond` draws from, so the deals it
//! strikes are the same kind: carried out, and running their course as rounds end.
//!
//! Package 1c-10 completes it across every action a seat has (DESIGN.md 9.5): it sets its
//! cities' focus, locks and releases their citizens' tiles and places specialists by hand
//! ([`citizens`]), takes the great people it is owed free and founds a pantheon when it may
//! ([`free_choices`]), and renames its civilization, keeps notes and logs thoughts ([`notes`]),
//! where now and then it also asks for its own turn to end, which a driven seat is refused; and
//! now and then it tries an action whose moment has not come ([`untimely`]), so that a short
//! game tries every action. It trains settlers when it is short of cities, and sends them to
//! good sites, since at random it would stay at a city or two for a whole game. The
//! whole-game tests and the `random` golden set play it (`tests/engine/whole_game.rs`,
//! `golden::games`). A noisy agent ([`RandomAgent::noisy`]) also reads the game, saves
//! it and makes calls it refuses between its moves ([`reads_and_refusals`]), which must change
//! nothing: the early form of property P8.

use citar_engine::base::ids::{CityId, NegotiationId, PlayerId, TechId, TileIdx, UnitId};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::actions::{ActionKind, FoundCity, UnitAction, unit_actions};
use citar_engine::game::automation::suggest_city_sites;
use citar_engine::game::cities::borders::{BuyTile, can_buy_tile};
use citar_engine::game::cities::citizens::{SetCityFocus, SetSpecialists, WorkTile};
use citar_engine::game::cities::construction::{buildable_items, item_name};
use citar_engine::game::cities::purchase::Buy;
use citar_engine::game::cities::queue::{
    ChangeQueue, QueueEdit, RenameCity, SetAutoProduction, SetProduction,
};
use citar_engine::game::cities::stats::{max_specialists, workable_tiles};
use citar_engine::game::city_states::CityStateAction;
use citar_engine::game::combat::actions::{
    AirSweep, Attack, CityAttack, CityStatus, ReturnCivilian, plan_attack,
};
use citar_engine::game::combat::{city, combatant_at, resolve};
use citar_engine::game::diplomacy::actions::{
    DeclareWar, Denounce, EndTurn, OpenNegotiation, RespondNegotiation, SendMessage,
};
use citar_engine::game::espionage::{MoveSpy, StageCoup, spies as spies_of};
use citar_engine::game::great_people::{ChooseGreatPerson, great_people_types};
use citar_engine::game::meta::{LogThought, SetCivName, WriteNotes};
use citar_engine::game::path::Mover;
use citar_engine::game::policies::{AdoptPolicy, adoptable_policies, can_adopt_any};
use citar_engine::game::religion::beliefs_available;
use citar_engine::game::religion::found::{
    FoundPantheon, ai_choose_beliefs, beliefs_to_choose, can_found_pantheon,
};
use citar_engine::game::research::{
    ChooseFreeTech, DequeueResearch, SetResearch, available_techs, is_unresearchable,
};
use citar_engine::game::units::actions::{MoveUnit, PromoteUnit, UnitOrder, UpgradeUnit};
use citar_engine::game::units::{promotions, upgrades};
use citar_engine::game::victory::UnVote;
use citar_engine::game::workers::{self, BuildImprovement, Builder};
use citar_engine::game::{Action, DriverOutcome, Game, SeatDriver};
use citar_engine::rules::defs::{BeliefKind, BeliefType};
use citar_engine::state::cities::{City, CityFocus, Constructible, Perpetual};
use citar_engine::state::diplo::{NegStatus, Negotiation};
use citar_engine::state::players::DriverMemory;
use citar_engine::state::players::Player;
use citar_engine::state::units::Activity;
use serde_json::json;

/// One kind of move: what the agent may do with its turn, drawing from the turn's stream.
pub type Move = fn(&mut Game, PlayerId, &mut Rng);

/// Every move the agent knows, in the order it tries them each turn; then it ends its turn, which
/// [`Game::drive`] does for it once it returns.
///
/// - Package 1b-07: `research`, `production`, `queues`, `policies`, `purchases` and `names`.
/// - Package 1c-02: [`promote_units`], [`upgrade_units`], [`order_units`] and [`move_units`].
/// - Package 1c-03: [`fight`], before the units move, so that those beside an enemy attack it.
/// - Package 1c-04: [`found_cities`], [`build_improvements`] and [`special_actions`], and the
///   orders `explore`, `automate` and `pillage` among [`order_units`]'.
/// - Package 1c-05: [`spies`] and [`diplomacy`], the chats last, which the drive answers once
///   the agent has done everything else.
/// - Package 1c-06: [`city_states`], and coups among its spies' moves.
/// - Package 1c-08: [`un_vote`], so that the moves before it draw as they did.
/// - Package 1c-10 completes it with the actions no package had taught it, after the rest for
///   the same reason: [`citizens`] (1b-06's `set_city_focus`, `work_tile` and
///   `set_specialists`), [`free_choices`] (1b-08's `choose_great_person` and `found_pantheon`)
///   and [`notes`] (1c-09's `set_civ_name`, `write_notes` and `log_thought`, and the `end_turn`
///   tool, which a driven seat is refused); and [`untimely`], the actions whose moment has not
///   come, which a short game would otherwise never try.
pub const MOVES: &[Move] = &[
    research,
    production,
    queues,
    policies,
    purchases,
    names,
    found_cities,
    build_improvements,
    special_actions,
    promote_units,
    upgrade_units,
    order_units,
    fight,
    move_units,
    spies,
    city_states,
    diplomacy,
    un_vote,
    citizens,
    free_choices,
    notes,
    untimely,
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
        play(g, pid, Action::ChooseFreeTech(ChooseFreeTech { tech }));
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
        play(g, pid, Action::SetResearch(SetResearch { tech, append: None }));
    } else if rng.below(8) == 0 {
        let Some(t) = pick(rng, &far()) else { return };
        let a = SetResearch { tech: json!(tech_name(g, t)), append: Some(json!(true)) };
        play(g, pid, Action::SetResearch(a));
    } else if rng.below(16) == 0
        && let Some(t) = pick(rng, &queue)
    {
        let tech = json!(tech_name(g, t));
        play(g, pid, Action::DequeueResearch(DequeueResearch { tech }));
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
        // A civilization short of cities trains a settler a third of the time it may: at random
        // among everything, it would so rarely that a game stays at a city or two.
        let settler = g.rules().derived().known.settler.map(Constructible::Unit);
        let item = match settler.filter(|s| !append && wants_settlers(g, pid) && all.contains(s)) {
            Some(s) if rng.chance(1.0 / 3.0) => s,
            _ => {
                let Some(item) = pick(rng, &all) else { continue };
                item
            }
        };
        let name = item_name(g.rules(), item).to_owned();
        let a = SetProduction {
            city_id: i64::from(c.get()),
            item: json!(name),
            append: append.then(|| json!(true)),
        };
        play(g, pid, Action::SetProduction(a));
    }
}

/// Whether a civilization wants more settlers: its cities and settlers together are fewer than
/// three, and one more every forty turns, up to eight.
fn wants_settlers(g: &Game, pid: PlayerId) -> bool {
    let settler = g.rules().derived().known.settler;
    let cities = g.player_cities(pid).count();
    let settlers = g.player_units(pid).filter(|u| Some(u.base) == settler).count();
    let goal = (3 + usize::try_from(g.turn()).unwrap_or(0) / 40).min(8);
    cities + settlers < goal
}

/// Whether unit `u` could found a city, somewhere.
fn founds_cities(g: &Game, u: UnitId) -> bool {
    unit_actions(g, u).iter().any(|a| a.id == "found_city")
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
        play(g, pid, Action::ChangeQueue(a));
    }
    if rng.below(20) == 0 {
        let on = g.city(c).is_some_and(|x| !x.auto_production);
        let a = SetAutoProduction { city_id: i64::from(c.get()), enabled: json!(on) };
        play(g, pid, Action::SetAutoProduction(a));
    }
}

/// Adopts a policy it could adopt, when it has the culture or a free policy.
fn policies(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    if !can_adopt_any(g, pid) {
        return;
    }
    let Some(q) = pick(rng, &adoptable_policies(g, pid)) else { return };
    let name = g.rules().name(q).unwrap_or_default().to_owned();
    play(g, pid, Action::AdoptPolicy(AdoptPolicy { policy: json!(name) }));
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
        play(g, pid, Action::Buy(a));
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
            play(g, pid, Action::BuyTile(a));
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
    play(g, pid, Action::RenameCity(RenameCity { city_id: i64::from(c.get()), name: json!(name) }));
}

/// The player's units, in id order: what each unit move goes through. An action may use one up,
/// so each move looks it up again.
fn units_of(g: &Game, pid: PlayerId) -> Vec<UnitId> {
    g.player_units(pid).map(|u| u.id()).collect()
}

/// How often the agents on this thread have tried each tool, and how often the game took it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tried {
    /// Calls made.
    pub tried: u32,
    /// Calls the game carried out: the rest it refused.
    pub taken: u32,
}

std::thread_local! {
    /// The tools this thread's agents have tried, by name. Only ever read by tests, to see that
    /// the agent reaches every action: nothing in its play reads it.
    static TALLY: core::cell::RefCell<std::collections::BTreeMap<&'static str, Tried>> =
        const { core::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

/// The tools the agents on this thread have tried since the last [`take_tally`], by name, and
/// how many times the game took each.
#[must_use]
pub fn take_tally() -> std::collections::BTreeMap<&'static str, Tried> {
    TALLY.with(|t| core::mem::take(&mut *t.borrow_mut()))
}

/// Takes an action, and says whether the game carried it out. A refusal is an answer like any
/// other, which an agent playing at random mostly has no use for; what the action did is the
/// game's to keep.
fn play(g: &mut Game, pid: PlayerId, a: Action) -> bool {
    let tool = a.tool();
    let taken = g.act(pid, a).is_ok();
    TALLY.with(|t| {
        let mut t = t.borrow_mut();
        let e = t.entry(tool).or_default();
        e.tried += 1;
        e.taken += u32::from(taken);
    });
    taken
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

/// The standing orders the agent gives: those that do not end the unit.
const ORDERS: [&str; 8] =
    ["fortify", "sleep", "heal", "skip", "wake", "explore", "automate", "pillage"];

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

/// Founds a city with each settler that may found one where it stands, half the time, now and
/// then through `unit_action` rather than `found_city`.
pub fn found_cities(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        let can = unit_actions(g, u).iter().any(|a| a.id == "found_city" && a.available());
        if !can || !rng.chance(0.5) {
            continue;
        }
        let a = if rng.chance(0.2) {
            Action::UnitAction(UnitAction {
                unit_id: tool_id(u),
                action: "found_city".into(),
                name: None,
                beliefs: None,
                x: None,
                y: None,
            })
        } else {
            Action::FoundCity(FoundCity { unit_id: tool_id(u), name: None })
        };
        play(g, pid, a);
    }
}

/// Sets a third of the units that can build to work where they stand (`build_improvement`): one of
/// the improvements they could start or make at once, now and then a cancellation or a name no
/// improvement has, which is refused.
pub fn build_improvements(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        let Some((b, t)) = Builder::unit(g, u).zip(g.unit(u).map(|x| x.tile())) else { continue };
        let mut names: Vec<String> = workers::build_options(g, &b, t, None)
            .into_iter()
            .filter_map(|o| g.rules().name(o.imp).map(str::to_owned))
            .collect();
        names.extend(
            workers::water_options(g, u)
                .into_iter()
                .chain(workers::great_options(g, u))
                .filter_map(|o| g.rules().name(o.imp).map(str::to_owned)),
        );
        if names.is_empty() || !rng.chance(0.35) {
            continue;
        }
        let pick = if rng.chance(0.05) {
            Some("cancel".to_owned())
        } else if rng.chance(0.05) {
            Some("No Such Improvement".to_owned())
        } else {
            rng.pick(&names).cloned()
        };
        let Some(improvement) = pick else { continue };
        play(
            g,
            pid,
            Action::BuildImprovement(BuildImprovement { unit_id: tool_id(u), improvement }),
        );
    }
}

/// Takes one of each unit's special actions a fifth of the time (`unit_action`): a religion is
/// founded or enhanced with the beliefs an AI would choose, a paradrop aims at a tile within five,
/// the rest need nothing more. Refusals (a spread into a city of the religion, a hurry with
/// nothing built) are part of the play.
pub fn special_actions(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        let list = unit_actions(g, u);
        let open: Vec<usize> = (0..list.len())
            .filter(|&i| list[i].available() && list[i].id != "found_city")
            .collect();
        if open.is_empty() || !rng.chance(0.2) {
            continue;
        }
        let Some(&i) = rng.pick(&open) else { continue };
        let entry = &list[i];
        let mut a = UnitAction {
            unit_id: tool_id(u),
            action: entry.id.clone(),
            name: None,
            beliefs: None,
            x: None,
            y: None,
        };
        match entry.kind {
            ActionKind::FoundReligion(_) | ActionKind::EnhanceReligion(_) => {
                let enhancing = matches!(entry.kind, ActionKind::EnhanceReligion(_));
                let needed = beliefs_to_choose(g, pid, enhancing);
                let beliefs: Vec<serde_json::Value> = ai_choose_beliefs(g, pid, &needed)
                    .into_iter()
                    .filter_map(|b| g.rules().name(b).map(|n| json!(n)))
                    .collect();
                a.beliefs = Some(serde_json::Value::Array(beliefs));
                a.name = Some(json!(format!("Faith of {}", pid.0)));
            }
            ActionKind::Paradrop(_) => {
                let Some(at) = g.unit(u).map(|x| x.tile()) else { continue };
                let Some(&t) = rng.pick(&g.grid().within(at, 5)) else { continue };
                let (x, y) = g.grid().xy(t);
                a.x = Some(i64::from(x));
                a.y = Some(i64::from(y));
            }
            _ => {}
        }
        play(g, pid, Action::UnitAction(a));
    }
}

/// Moves each unit with movement left (`move_unit`): half the time to a tile it reaches this
/// turn, otherwise toward any tile up to eight away, which leaves a standing goto when the path
/// takes more than this turn (or a refusal, when there is none). A unit that founds cities heads
/// three times in four for one of the best city sites within six instead. A unit at work
/// (building, automated, exploring) is left to it, nine times in ten.
pub fn move_units(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for u in units_of(g, pid) {
        let working = g.unit(u).is_some_and(|x| {
            matches!(x.activity, Some(Activity::Build | Activity::Automate | Activity::Explore))
        });
        if working && rng.chance(0.9) {
            continue;
        }
        let Some(from) = g.unit(u).filter(|x| x.moves > 0).map(|x| x.tile()) else { continue };
        let site = if founds_cities(g, u) && rng.chance(0.75) {
            let sites: Vec<TileIdx> =
                suggest_city_sites(g, pid, from, 6, 3).into_iter().map(|(t, _)| t).collect();
            rng.pick(&sites).copied()
        } else {
            None
        };
        let target = if site.is_some() {
            site
        } else if rng.chance(0.5) {
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
    if !play(g, pid, Action::RespondNegotiation(a)) {
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
/// has met; one time in ten opens a negotiation with one of them, which the drive puts to the
/// other side once the turn's moves are done; one time in two hundred denounces one; and after
/// turn 50 declares war on one one time in two hundred.
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
            play(g, pid, Action::OpenNegotiation(a));
        }
        if rng.chance(0.005) {
            play(g, pid, Action::Denounce(Denounce { player_id: i64::from(to.0) }));
        }
        if g.turn() > 50 && rng.chance(0.005) {
            let a = DeclareWar { player_id: i64::from(to.0), message: Some(json!("War!")) };
            play(g, pid, Action::DeclareWar(a));
        }
    }
}

/// Espionage (`move_spy`, `stage_coup`): one time in ten, each spy goes to a city its
/// civilization has explored, or home one time in four of those; one time in twenty it stages a
/// coup, which is refused unless it is set up in a city-state's capital.
pub fn spies(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let names: Vec<String> = spies_of(g, pid).iter().map(|s| s.name.to_string()).collect();
    for name in names {
        if rng.chance(0.05) {
            play(g, pid, Action::StageCoup(StageCoup { spy: json!(name) }));
            continue;
        }
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

/// City-states (`city_state_action`): one time in five, with a city-state it has met drawn at
/// random, an action drawn from the tool's eight: gold (a gift of up to 300), one of its units
/// (which is refused unless it stands in a city-state's land), a pledge, a withdrawal, tribute in
/// gold or a worker, peace, or marriage. Most are refused, which is part of the play.
pub fn city_states(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    if !rng.chance(0.2) {
        return;
    }
    let met: Vec<PlayerId> =
        g.city_states(true).map(Player::id).filter(|&q| g.has_met(pid, q)).collect();
    let Some(cs) = pick(rng, &met) else { return };
    let actions = [
        "gift_gold",
        "gift_unit",
        "pledge",
        "withdraw",
        "tribute_gold",
        "tribute_worker",
        "make_peace",
        "marry",
    ];
    let Some(action) = pick(rng, &actions) else { return };
    let amount = (action == "gift_gold").then(|| i64::try_from(rng.below(300)).unwrap_or(0) + 1);
    let unit_id = if action == "gift_unit" {
        let mine = units_of(g, pid);
        let Some(u) = pick(rng, &mine) else { return };
        Some(tool_id(u))
    } else {
        None
    };
    let a = CityStateAction { player_id: i64::from(cs.0), action: json!(action), amount, unit_id };
    play(g, pid, Action::CityStateAction(a));
}

/// The United Nations (`un_vote`): while voting is open, one time in two, an abstention one time
/// in four, else a vote for itself, a living major civilization it has met, or now and then a
/// player the game does not have, which is refused as part of the play.
pub fn un_vote(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    if !citar_engine::game::victory::un::vote_open(g) || !rng.chance(0.5) {
        return;
    }
    let candidate = if rng.below(4) == 0 {
        json!("abstain")
    } else {
        let mut them: Vec<PlayerId> =
            g.majors(true).map(Player::id).filter(|&q| q == pid || g.has_met(pid, q)).collect();
        them.push(PlayerId(u8::try_from(g.state().players().len()).unwrap_or(u8::MAX)));
        let Some(q) = pick(rng, &them) else { return };
        json!(q.0)
    };
    play(g, pid, Action::UnVote(UnVote { candidate }));
}

/// A tile's coordinates, as the tools take them.
fn xy_of(g: &Game, t: TileIdx) -> (i64, i64) {
    let (x, y) = g.grid().xy(t);
    (i64::from(x), i64::from(y))
}

/// The citizens (`set_city_focus`, `work_tile`, `set_specialists`), for each city: one time in
/// twenty a focus drawn from every focus, a quarter of those with growth avoided or not; one
/// time in twenty-five a citizen locked onto a tile it may work, or one lock released; one time
/// in forty specialists by hand, a count up to the slots of a kind the city has, or back to
/// automatic. Now and then a count past the slots, which is refused.
pub fn citizens(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    for c in cities {
        let city_id = i64::from(c.get());
        if rng.chance(0.05) {
            let Some(focus) = pick(rng, &CityFocus::ALL) else { continue };
            let avoid_growth = rng.chance(0.25).then(|| json!(rng.chance(0.5)));
            let a = SetCityFocus { city_id, focus: Some(json!(focus.name())), avoid_growth };
            play(g, pid, Action::SetCityFocus(a));
        }
        if rng.chance(0.04) {
            let locked: Vec<TileIdx> = g.city(c).map(|x| x.locked.to_vec()).unwrap_or_default();
            let (t, lock) = if !locked.is_empty() && rng.chance(0.5) {
                (pick(rng, &locked), false)
            } else {
                (pick(rng, &workable_tiles(g, c)), true)
            };
            if let Some(t) = t {
                let (x, y) = xy_of(g, t);
                let a = WorkTile { city_id, x, y, locked: Some(json!(lock)) };
                play(g, pid, Action::WorkTile(a));
            }
        }
        if rng.chance(0.025) {
            let slots = max_specialists(g, c);
            let mut asked = serde_json::Map::new();
            if !slots.is_empty() && rng.chance(0.75) {
                let Some(&(s, n)) = rng.pick(&slots) else { continue };
                let most = u64::try_from(n).unwrap_or(0) + u64::from(rng.chance(0.05));
                let name = g.rules().specialists().get(s).map_or("?", |d| &*d.name).to_owned();
                asked.insert(name, json!(rng.below(most + 1)));
            }
            let a = SetSpecialists { city_id, specialists: serde_json::Value::Object(asked) };
            play(g, pid, Action::SetSpecialists(a));
        }
    }
}

/// The free choices (`choose_great_person`, `found_pantheon`): a great person of a kind the
/// civilization can have for each one it is owed; and, when it may found a pantheon, one time in
/// two a pantheon belief nobody has taken. Now and then a name that is neither, which is refused.
pub fn free_choices(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let owed = g.player(pid).map_or(0, |p| p.gp.free);
    for _ in 0..owed.clamp(0, 4) {
        let kinds = great_people_types(g, pid);
        let name = match pick(rng, &kinds) {
            Some(u) if !rng.chance(0.05) => g.rules().name(u).unwrap_or_default().to_owned(),
            _ => "No Such Person".to_owned(),
        };
        play(g, pid, Action::ChooseGreatPerson(ChooseGreatPerson { great_person: json!(name) }));
    }
    if can_found_pantheon(g, pid).is_none() && rng.chance(0.5) {
        let open = beliefs_available(g, BeliefKind::Type(BeliefType::Pantheon));
        let name = match pick(rng, &open) {
            Some(b) if !rng.chance(0.05) => g.rules().name(b).unwrap_or_default().to_owned(),
            _ => "No Such Belief".to_owned(),
        };
        play(g, pid, Action::FoundPantheon(FoundPantheon { belief: json!(name) }));
    }
}

/// The seat's own words (`set_civ_name`, `write_notes`, `log_thought`, `end_turn`): one time in
/// a hundred a new name for the civilization, now and then with a new leader (and one time in
/// ten a name another civilization has, which is refused); one time in twenty a line in its
/// notebook, appended or replacing it; one time in ten a thought. One time in fifty, while its driver plays it, the
/// `end_turn` tool, which the game refuses: the drive ends a driven seat's turn.
pub fn notes(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    if rng.chance(0.01) {
        let name = if rng.chance(0.1) {
            let others: Vec<PlayerId> = g.majors(false).map(Player::id).collect();
            pick(rng, &others)
                .and_then(|q| g.player(q))
                .map_or_else(String::new, |p| p.name.to_string())
        } else {
            format!("Realm {} {}", pid.0, rng.below(1000))
        };
        let leader = rng.chance(0.3).then(|| json!(format!("Leader {}", rng.below(100))));
        play(g, pid, Action::SetCivName(SetCivName { name: json!(name), leader }));
    }
    if rng.chance(0.05) {
        let mode = rng.chance(0.7).then(|| json!("append"));
        let text = json!(format!("Turn {}: note {}.", g.turn(), rng.below(100)));
        play(g, pid, Action::WriteNotes(WriteNotes { text, mode }));
    }
    if rng.chance(0.1) {
        let text = json!(format!("Thinking about turn {}.", g.turn()));
        play(g, pid, Action::LogThought(LogThought { text }));
    }
    if g.driving() == Some(pid) && rng.chance(0.02) {
        let refused = !play(g, pid, Action::EndTurn(EndTurn {}));
        debug_assert!(refused, "a driven seat does not end its own turn");
    }
}

/// One time in ten, an action whose moment may not have come, which the game refuses unless it
/// has: a free tech or great person the civilization may not be owed, a vote while voting may be
/// closed, a fate for one of its cities that may be neither a puppet nor burning, an upgrade or
/// an air sweep by any of its units, and a spy it may not have, moved or sent to stage a coup.
/// The other moves try these only when they may succeed, which a short game or an early era
/// never offers; a refusal is part of the play.
pub fn untimely(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    if !rng.chance(0.1) {
        return;
    }
    let units = units_of(g, pid);
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    let a = match rng.below(8) {
        0 => {
            let tech = pick(rng, &available_techs(g, pid)).map(|t| tech_name(g, t));
            Action::ChooseFreeTech(ChooseFreeTech { tech: json!(tech.unwrap_or_default()) })
        }
        1 => {
            let kind = pick(rng, &great_people_types(g, pid));
            let name = kind.and_then(|u| g.rules().name(u)).unwrap_or_default();
            Action::ChooseGreatPerson(ChooseGreatPerson { great_person: json!(name) })
        }
        2 => Action::UnVote(UnVote { candidate: json!(pid.0) }),
        3 => {
            let (Some(c), Some(fate)) = (pick(rng, &cities), pick(rng, &FATES)) else { return };
            Action::CityStatus(CityStatus { city_id: i64::from(c.get()), status: json!(fate) })
        }
        4 => {
            let Some(u) = pick(rng, &units) else { return };
            Action::UpgradeUnit(UpgradeUnit { unit_id: tool_id(u) })
        }
        5 => {
            let Some(u) = pick(rng, &units) else { return };
            let Some(at) = g.unit(u).map(|x| x.tile()) else { return };
            let Some(t) = pick(rng, &g.grid().within(at, 2)) else { return };
            let (x, y) = xy_of(g, t);
            Action::AirSweep(AirSweep { unit_id: tool_id(u), x, y })
        }
        6 => Action::MoveSpy(MoveSpy { spy: json!("Nobody"), city_id: json!("hideout") }),
        _ => Action::StageCoup(StageCoup { spy: json!("Nobody") }),
    };
    play(g, pid, a);
}

// ---- Reads and refusals (property P8) -----------------------------------------------------------

/// One of anything a game has of a kind, by a draw: `None` when it has none.
fn any_of<T: Copy>(rng: &mut Rng, v: &[T]) -> Option<T> {
    rng.pick(v).copied()
}

/// An `inspect` query of a kind drawn from every kind there is, about things drawn from the game:
/// its players, units, cities, tiles and negotiations (some of which do not exist, which the
/// query refuses).
fn some_query(g: &Game, pid: PlayerId, rng: &mut Rng) -> serde_json::Value {
    let players: Vec<u8> = g.state().players().ids().map(|p| p.0).collect();
    let q = any_of(rng, &players).unwrap_or(pid.0);
    let units: Vec<u32> = g.state().units().iter().map(|u| u.id().get()).collect();
    let u = any_of(rng, &units).unwrap_or(1);
    let cities: Vec<u32> = g.state().cities().iter().map(|c| c.id().get()).collect();
    let c = any_of(rng, &cities).unwrap_or(1);
    let nids: Vec<u32> = g.negotiations().iter().map(|n| n.id.get()).collect();
    let nid = any_of(rng, &nids).unwrap_or(1);
    let t = TileIdx(u32::try_from(rng.below(g.grid().size() as u64)).unwrap_or(0));
    let (x, y) = g.grid().xy(t);
    match rng.below(27) {
        0 => json!({"what": "briefing", "player": pid.0}),
        1 => json!({"what": "build_options", "unit": u}),
        2 => json!({"what": "buildable", "city": c}),
        3 => json!({"what": "camps"}),
        4 => json!({"what": "city", "city": c}),
        5 => json!({"what": "city_state", "player": q}),
        6 => json!({"what": "costs", "player": q}),
        7 => json!({"what": "events", "since": rng.below(50), "player": q}),
        8 => json!({"what": "find_tiles", "x": x, "y": y, "radius": 2}),
        9 => json!({"what": "game"}),
        10 => json!({"what": "great_people", "player": q}),
        11 => json!({"what": "negotiation", "negotiation": nid, "player": q}),
        12 => json!({"what": "ops"}),
        13 => json!({"what": "pending"}),
        14 => json!({"what": "player", "player": q}),
        15 => json!({"what": "preview", "unit": u, "x": x, "y": y}),
        16 => json!({"what": "relation", "a": pid.0, "b": q}),
        17 => json!({"what": "religion", "player": q}),
        18 => json!({"what": "religion", "city": c}),
        19 => json!({"what": "spies", "player": q}),
        20 => json!({"what": "tile", "x": x, "y": y}),
        21 => json!({"what": "un"}),
        22 => json!({"what": "unit", "unit": u}),
        23 => json!({"what": "unit_actions", "unit": u}),
        24 => json!({"what": "units", "player": q}),
        25 => json!({"what": "victory", "player": q}),
        _ => json!({"what": "view", "player": pid.0}),
    }
}

/// Reads a game as a host or a model would between two actions: `inspect` queries (the views
/// and the briefing among them, refused until they are ported), what the tools read (a unit's
/// reach, a preview, a city's list, the advisor's pick), the chronicle, and the digest.
fn read_something(g: &Game, pid: PlayerId, rng: &mut Rng) {
    let q = some_query(g, pid, rng);
    let _answer = citar_engine::api::inspect::inspect(g, &q);
    let units = units_of(g, pid);
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    match rng.below(6) {
        0 => {
            if let Some(u) = any_of(rng, &units)
                && let Some(m) = Mover::unit(g, u)
            {
                let _reach = m.reachable();
            }
        }
        1 => {
            if let Some(u) = any_of(rng, &units)
                && let Some(t) = g.unit(u).map(|x| x.tile())
            {
                let near = g.grid().within(t, 2);
                if let Some(to) = any_of(rng, &near) {
                    let _preview = resolve::preview(g, u, to);
                    let _plan = plan_attack(g, u, to);
                }
            }
        }
        2 => {
            if let Some(c) = any_of(rng, &cities) {
                let _items = buildable_items(g, c);
                let params = citar_engine::game::advisor::AdvisorParams::default();
                let _pick = citar_engine::game::advisor::advise_production(g, pid, c, &params);
            }
        }
        3 => {
            let _rows = g.stats(Some(5));
            let _events = g.events(0, 50);
            let _thoughts = g.thoughts(Some(pid), 0);
            let _refusal = g.end_turn_refusal(pid);
        }
        4 => {
            for n in g.negotiations().iter().filter(|n| n.status == NegStatus::Open) {
                let _view = g.negotiation_view(n.id, pid);
            }
        }
        _ => {
            let _digest = g.digest();
        }
    }
}

/// A unit id no game hands out.
const NO_UNIT: i64 = -7;

/// A call the game refuses, drawn from host commands and tools: an action on a unit or a
/// negotiation the game does not have, by a player whose turn it is not, or with arguments of
/// the wrong kind; a turn ended for the wrong player or forced on one that does not exist;
/// scenario operations that fail halfway, which the game takes back whole.
///
/// # Panics
/// If the game carries out a call this means it to refuse: that is the bug property P8 is after.
fn refuse_something(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    let others: Vec<PlayerId> = g.state().players().ids().filter(|&q| q != g.current()).collect();
    let own = units_of(g, pid);
    let (what, refused) = match rng.below(9) {
        0 => {
            let a = MoveUnit { unit_id: NO_UNIT, x: 0, y: 0 };
            ("a move of no unit", g.act(pid, Action::MoveUnit(a)).is_err())
        }
        1 => match (any_of(rng, &others), own.first()) {
            (Some(q), Some(&u)) => {
                let (x, y) = g.unit(u).map_or((0, 0), |x| g.grid().xy(x.tile()));
                let a = MoveUnit { unit_id: tool_id(u), x: i64::from(x), y: i64::from(y) };
                ("a move out of turn", g.act(q, Action::MoveUnit(a)).is_err())
            }
            _ => return,
        },
        2 => {
            let a = SetResearch { tech: json!("No Such Tech"), append: None };
            ("research of no tech", g.act(pid, Action::SetResearch(a)).is_err())
        }
        3 => {
            let a = AdoptPolicy { policy: json!(17) };
            ("a policy given as a number", g.act(pid, Action::AdoptPolicy(a)).is_err())
        }
        4 => {
            let a = RespondNegotiation {
                negotiation_id: 999_999,
                action: json!("accept"),
                message: None,
                give: None,
                receive: None,
            };
            ("an answer to no negotiation", g.act(pid, Action::RespondNegotiation(a)).is_err())
        }
        5 => match any_of(rng, &others) {
            Some(q) => ("a turn ended for another", g.end_turn(q).is_err()),
            None => return,
        },
        6 => ("a turn forced on nobody", g.force_turn(PlayerId(200)).is_err()),
        7 => {
            let ops = json!([
                {"op": "set_player", "player": pid.0, "gold": 1},
                {"op": "remove_city", "city": 999_999},
            ]);
            ("operations that fail halfway", g.apply_ops(&ops).is_err())
        }
        _ => {
            let a = BuildImprovement { unit_id: NO_UNIT, improvement: "Farm".into() };
            ("a build by no unit", g.act(pid, Action::BuildImprovement(a)).is_err())
        }
    };
    assert!(refused, "the game carried out {what}, which it must refuse");
}

/// What a host does besides: saves the game (the journal's chunk, then a snapshot written as
/// JSON) and reads the summary back.
fn save_it(g: &mut Game) {
    let _chunk = g.take_journal_chunk();
    if let Ok(bytes) = g.snapshot().to_json() {
        let _summary = citar_engine::save::summary(&bytes);
    }
}

/// Reads, refused calls and a save now and then, as many hosts and models make between two
/// actions: property P8 says none of them changes the game (DESIGN.md 9.5). Draws from `rng`
/// alone, which is not a stream the agent's moves draw from.
pub fn reads_and_refusals(g: &mut Game, pid: PlayerId, rng: &mut Rng) {
    for _ in 0..2 {
        read_something(g, pid, rng);
    }
    if rng.chance(0.5) {
        refuse_something(g, pid, rng);
    }
    if rng.chance(0.02) {
        save_it(g);
    }
}

/// The stream [`reads_and_refusals`] draws from for a seat's turn, or an answer: keyed apart from
/// every stream the agent's moves and answers draw from.
fn noise_stream(g: &Game, pid: PlayerId, what: u64) -> Rng {
    let turn = u64::try_from(g.turn()).unwrap_or(0);
    Rng::keyed(g.state().seed(), Purpose::TestAgent, &[u64::from(pid.0), turn, u64::MAX, what])
}

/// A driver that plays at random among the actions the engine has, reproducibly: the same game
/// and seat give the same moves. A noisy one ([`RandomAgent::noisy`]) also reads and makes
/// refused calls between its moves, which must change nothing (property P8).
#[derive(Clone, Debug, Default)]
pub struct RandomAgent {
    turns: u64,
    noisy: bool,
}

impl RandomAgent {
    /// A new agent.
    #[must_use]
    pub const fn new() -> Self {
        Self { turns: 0, noisy: false }
    }

    /// A new agent that plays as [`new`](Self::new)'s does, and between every two of its moves,
    /// and before every answer, reads the game and makes calls the game refuses
    /// ([`reads_and_refusals`]).
    #[must_use]
    pub const fn noisy() -> Self {
        Self { turns: 0, noisy: true }
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
        let mut noise = self.noisy.then(|| noise_stream(g, pid, 0));
        for m in MOVES {
            if let Some(n) = noise.as_mut() {
                reads_and_refusals(g, pid, n);
            }
            m(g, pid, &mut rng);
        }
        if let Some(n) = noise.as_mut() {
            reads_and_refusals(g, pid, n);
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
        if self.noisy {
            let mut n = noise_stream(g, pid, 1 + u64::from(nid.get()));
            reads_and_refusals(g, pid, &mut n);
        }
        let mut rng = answer_stream(g, pid, nid);
        answer(g, pid, nid, &mut rng);
        DriverOutcome::Done
    }
}

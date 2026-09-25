//! The production advisor: what a city builds next when nobody chooses for it
//! (`cities.auto_pick_production`, `cities.py:1696-1717`), and the production half of the live
//! bot it asks (`bots/basic.py`): the facts of a civilization's turn production reads
//! (`BasicBot.context`, `basic.py:777-835`), a city's defence and danger (`836-873`), what the
//! civilization already has (`_counts`, `1113-1144`), where it would found cities
//! (`expansion_sites`, `1069-1111`), and the choice itself (`advise_production`,
//! `_choose_production_unciv` and `_classic`, the buildings' values from a what-if of the city
//! with each, and the military unit a role wants; `1146-1590`).
//!
//! Every number is a parameter of [`AdvisorParams`], whose defaults are the live bot's
//! (`basic.py:PARAM_GROUPS`); `auto_pick` asks with the aggression automatic production always
//! gave the bot (0.25). The advisor reads the game and writes nothing: the same game gives the
//! same answer, and the bot of Phase 2 asks it too (DESIGN.md 6.12).
//!
//! What differs from Python, on purpose (`tests/rules/intended.toml`):
//! - a building's value comes from `cities::what_if::what_if_building`, which reads the game with
//!   the building added and writes nothing, where `_simulate` added it and threw every cache
//!   away;
//! - the one random choice, whether to prefer a ranged unit, draws from `Purpose::Advisor`
//!   keyed by the city and the turn, where Python drew from a bot seeded with the city's id,
//!   made afresh for each pick (`advisor-draws-by-city-and-turn`);
//! - candidates are read in the ruleset's order, where Python iterated a set of unit names, so
//!   the first unit of a kind (a settler, a worker) and the best of equally valued military units
//!   were whichever the set gave first; and of equally valued choices the one with the largest id
//!   wins, where Python's `max` over `(value, name)` took the name last in code point order
//!   (`advisor-ties-by-id`);
//! - a military unit that costs nothing is valued as if it cost one, where Python divided by
//!   zero and the city picked nothing.
//!
//! What a civilization's turn looks like to production, what it has and where it would found
//! cities are gathered once in an [`Advisor`], which the bot keeps for a civilization's turn and
//! asks for each city; the sites are asked only when a city could start a settler otherwise.
//! The sites wait for the scoring of city sites (package 1c-04, `automation.city_site_score`):
//! until it lands no site scores, so no settler is chosen.

use core::cell::OnceCell;

use smallvec::SmallVec;

use super::Game;
use super::cities::borders::within_order;
use super::cities::construction::buildable_items;
use super::cities::founding::found_check;
use super::cities::stats::{city_strength, max_health, remaining_work};
use super::cities::what_if::{CityWhatIf, StatsDelta};
use super::derive::{civ, stats as memo};
use super::{Porting, pending_or};
use crate::base::ids::{BaseUnitId, BuildingId, CityId, PlayerId, TileIdx, UnitId};
use crate::base::num;
use crate::base::rng::{Purpose, Rng};
use crate::base::sets::{BaseUnitSet, ResourceSet};
use crate::base::stats::Stat;
use crate::rules::defs::{BaseUnitDef, Domain, ResourceType};
use crate::state::cities::{Constructible, Perpetual};
use crate::state::units::Activity;

// ---- Parameters (basic.py:143-403) ----------------------------------------------------------

/// How the advisor chooses (`prod_mode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProductionMode {
    /// UnCiv's `ConstructionAutomation`: every option valued, the best value per production
    /// left wins (`_choose_production_unciv`).
    #[default]
    Unciv,
    /// The older fixed priorities (`_choose_production_classic`).
    Classic,
}

/// Which cities want a unit in them (`garrison_mode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GarrisonMode {
    /// Every city.
    #[default]
    All,
    /// The capital, and cities near a foreign major's city, a barbarian camp or enemies in sight.
    Exposed,
}

/// The advisor's parameters: the live bot's that its production reads (`basic.py:PARAM_GROUPS`),
/// with their defaults. Integers stay integers, as Python compared and counted with them.
#[derive(Clone, Debug, PartialEq)]
#[allow(missing_docs, reason = "each is the live bot's parameter of the same name")]
pub struct AdvisorParams {
    /// Army size and willingness to fight, 0 to 1 (`BasicBot(aggression=...)`).
    pub aggression: f64,
    pub prod_mode: ProductionMode,
    // Building value (UnCiv mode).
    pub u_food: f64,
    pub u_production: f64,
    pub u_gold: f64,
    pub u_science: f64,
    pub u_culture: f64,
    pub u_faith: f64,
    pub u_happiness: f64,
    pub u_happiness_low: f64,
    pub u_happiness_low_below: i32,
    pub u_gold_broke_mult: f64,
    pub u_culture_first_mult: f64,
    pub u_culture_first_below: f64,
    pub u_carryover_share: f64,
    pub u_gpp: f64,
    pub u_specialist: f64,
    pub u_defense_war: f64,
    pub u_defense_peace: f64,
    pub u_defense_threat_mult: f64,
    pub u_city_health: f64,
    pub u_city_strength: f64,
    pub u_city_strength_offset: f64,
    pub u_wonder_bonus: f64,
    pub u_national_wonder_share: f64,
    pub u_wonder_gate: bool,
    pub wonder_gate_pop: i32,
    pub u_victory_building: f64,
    // Build priorities (UnCiv mode).
    pub u_settler: f64,
    pub settler_site_range: u32,
    pub settler_per_cities: i32,
    pub settler_min_pop: i32,
    pub settler_min_hap: i32,
    pub settler_min_hap_small: i32,
    pub settler_small_empire: i32,
    pub u_worker: f64,
    pub workers_per_city: f64,
    pub workers_full_until: i32,
    pub workers_extra_share: f64,
    pub worker_count_offset: f64,
    pub worker_unimproved: f64,
    pub u_boat: f64,
    pub boat_search_radius: u32,
    pub u_scout: f64,
    pub scout_until_turn: i32,
    pub u_seeker: f64,
    pub seek_after_turn: i32,
    pub seek_military_after: i32,
    pub u_military: f64,
    pub u_offense: f64,
    pub mil_base: f64,
    pub mil_scale: f64,
    pub mil_war_mult: f64,
    pub mil_offense_aggr_base: f64,
    pub mil_barbarian_min: f64,
    pub mil_below_avg_div: f64,
    pub mil_army_full_div: f64,
    pub peace_army_min: i32,
    pub peace_army_per_city: i32,
    pub military_min_gold: f64,
    pub u_spaceship: f64,
    pub u_space_program: f64,
    pub space_reserve: i32,
    pub garrison_after_turn: i32,
    pub danger_ratio: f64,
    // Classic production.
    pub c_danger: f64,
    pub c_garrison: f64,
    pub c_military_min_gpt: f64,
    pub c_army_offense: f64,
    pub c_army_offense_aggr: f64,
    pub c_army_peace: f64,
    pub c_army_peace_aggr: f64,
    pub c_army_short_base: f64,
    pub c_army_short_scale: f64,
    pub settler_prio: i32,
    pub settler_prio_late: i32,
    pub settler_prio_until: i32,
    pub settler_prio_per_city: i32,
    pub c_scout_needed: f64,
    pub c_scout_after_turn: i32,
    pub c_scout: f64,
    pub c_scout_early_turns: i32,
    pub c_seeker: f64,
    pub c_worker_extra: i32,
    pub c_worker_extra_until: i32,
    pub c_worker_urgent_div: i32,
    pub c_worker_urgent: f64,
    pub c_worker: f64,
    pub c_boat: f64,
    pub c_boat_retry_turns: i32,
    pub c_building_scale: f64,
    pub c_building_turns: f64,
    pub c_spaceship: f64,
    // Classic building value.
    pub w_food: f64,
    pub w_production: f64,
    pub w_gold: f64,
    pub w_science: f64,
    pub w_culture: f64,
    pub w_faith: f64,
    pub w_happiness: f64,
    pub w_hap_margin: i32,
    pub w_hap_unhappy: f64,
    pub w_hap_low: f64,
    pub w_hap_ok: f64,
    pub w_gold_broke: f64,
    pub w_gpp: f64,
    pub w_specialist: f64,
    pub w_def_strength_div: f64,
    pub w_def_health_div: f64,
    pub w_def_border: f64,
    pub w_def_interior: f64,
    pub unique_bonus: f64,
    pub wonder_mult: f64,
    pub wonder_bonus: f64,
    pub wonder_min_pop: i32,
    pub wonder_avg_prod: bool,
    pub w_small_city_pop: i32,
    pub w_small_city_mult: f64,
    // Expansion.
    pub site_min_score: f64,
    pub site_new_lux: f64,
    pub site_lux_radius: u32,
    pub site_distance_cost: f64,
    pub site_radius: u32,
    pub site_min_distance: u32,
    pub site_spacing: u32,
    pub site_candidates: usize,
    pub target_cities: i32,
    // Army.
    pub army_per_city: f64,
    pub army_per_city_aggr: f64,
    pub army_base: f64,
    pub army_war_per_city: f64,
    pub army_war_extra: f64,
    pub army_per_threatened_city: f64,
    pub siege_per_units: i32,
    pub min_field_melee: i32,
    pub ranged_chance: f64,
    pub unit_ranged_pref: f64,
    pub unit_ranged_nopref: f64,
    pub unit_siege_defensive: f64,
    pub unit_mobile_defensive: f64,
    pub unit_cost_exp: f64,
    // Tactics.
    pub threat_radius: u32,
    pub threat_near_dist: u32,
    pub threat_near_weight: f64,
    pub threat_far_weight: f64,
    pub garrison_mode: GarrisonMode,
    pub garrison_exposed_radius: u32,
    // Research.
    pub tech_space_era: usize,
}

impl Default for AdvisorParams {
    /// The live bot's defaults (`DEFAULT_PARAMS`), with its default aggression (0.4).
    fn default() -> Self {
        Self {
            aggression: 0.4,
            prod_mode: ProductionMode::Unciv,
            u_food: 3.6,
            u_production: 2.0,
            u_gold: 0.67,
            u_science: 2.0,
            u_culture: 1.0,
            u_faith: 1.0,
            u_happiness: 1.0,
            u_happiness_low: 6.0,
            u_happiness_low_below: 10,
            u_gold_broke_mult: 3.0,
            u_culture_first_mult: 2.0,
            u_culture_first_below: 2.0,
            u_carryover_share: 0.5,
            u_gpp: 1.5,
            u_specialist: 0.5,
            u_defense_war: 1.0,
            u_defense_peace: 0.5,
            u_defense_threat_mult: 2.0,
            u_city_health: 4.0,
            u_city_strength: 4.0,
            u_city_strength_offset: 3.0,
            u_wonder_bonus: 12.0,
            u_national_wonder_share: 0.5,
            u_wonder_gate: false,
            wonder_gate_pop: 12,
            u_victory_building: 1500.0,
            u_settler: 30.0,
            settler_site_range: 12,
            settler_per_cities: 3,
            settler_min_pop: 2,
            settler_min_hap: 2,
            settler_min_hap_small: 0,
            settler_small_empire: 3,
            u_worker: 1.0,
            workers_per_city: 1.8,
            workers_full_until: 5,
            workers_extra_share: 0.72,
            worker_count_offset: 0.17,
            worker_unimproved: 0.0,
            u_boat: 6.0,
            boat_search_radius: 6,
            u_scout: 4.0,
            scout_until_turn: 150,
            u_seeker: 6.0,
            seek_after_turn: 30,
            seek_military_after: 45,
            u_military: 1.0,
            u_offense: 3.0,
            mil_base: 1.0,
            mil_scale: 0.5,
            mil_war_mult: 2.0,
            mil_offense_aggr_base: 0.5,
            mil_barbarian_min: 2.0,
            mil_below_avg_div: 5.0,
            mil_army_full_div: 3.0,
            peace_army_min: 7,
            peace_army_per_city: 5,
            military_min_gold: -50.0,
            u_spaceship: 1500.0,
            u_space_program: 1500.0,
            space_reserve: 3,
            garrison_after_turn: 12,
            danger_ratio: 0.5,
            c_danger: 1000.0,
            c_garrison: 300.0,
            c_military_min_gpt: 1.0,
            c_army_offense: 90.0,
            c_army_offense_aggr: 70.0,
            c_army_peace: 25.0,
            c_army_peace_aggr: 35.0,
            c_army_short_base: 0.6,
            c_army_short_scale: 1.4,
            settler_prio: 130,
            settler_prio_late: 75,
            settler_prio_until: 90,
            settler_prio_per_city: 6,
            c_scout_needed: 80.0,
            c_scout_after_turn: 15,
            c_scout: 30.0,
            c_scout_early_turns: 30,
            c_seeker: 150.0,
            c_worker_extra: 1,
            c_worker_extra_until: 2,
            c_worker_urgent_div: 2,
            c_worker_urgent: 100.0,
            c_worker: 50.0,
            c_boat: 45.0,
            c_boat_retry_turns: 25,
            c_building_scale: 12.0,
            c_building_turns: 12.0,
            c_spaceship: 400.0,
            w_food: 1.2,
            w_production: 1.3,
            w_gold: 0.9,
            w_science: 1.1,
            w_culture: 0.8,
            w_faith: 0.6,
            w_happiness: 1.8,
            w_hap_margin: 2,
            w_hap_unhappy: 3.0,
            w_hap_low: 1.8,
            w_hap_ok: 0.4,
            w_gold_broke: 1.5,
            w_gpp: 1.5,
            w_specialist: 1.2,
            w_def_strength_div: 2.0,
            w_def_health_div: 25.0,
            w_def_border: 3.0,
            w_def_interior: 0.3,
            unique_bonus: 0.5,
            wonder_mult: 1.2,
            wonder_bonus: 2.0,
            wonder_min_pop: 0,
            wonder_avg_prod: false,
            w_small_city_pop: 2,
            w_small_city_mult: 0.8,
            site_min_score: 14.0,
            site_new_lux: 0.0,
            site_lux_radius: 2,
            site_distance_cost: 1.5,
            site_radius: 9,
            site_min_distance: 4,
            site_spacing: 4,
            site_candidates: 6,
            target_cities: 0,
            army_per_city: 1.0,
            army_per_city_aggr: 0.6,
            army_base: 1.0,
            army_war_per_city: 1.0,
            army_war_extra: 3.0,
            army_per_threatened_city: 1.0,
            siege_per_units: 3,
            min_field_melee: 2,
            ranged_chance: 0.3,
            unit_ranged_pref: 1.15,
            unit_ranged_nopref: 0.85,
            unit_siege_defensive: 0.5,
            unit_mobile_defensive: 0.8,
            unit_cost_exp: 0.35,
            threat_radius: 4,
            threat_near_dist: 2,
            threat_near_weight: 1.0,
            threat_far_weight: 0.6,
            garrison_mode: GarrisonMode::All,
            garrison_exposed_radius: 6,
            tech_space_era: 5,
        }
    }
}

impl AdvisorParams {
    /// What automatic production asks with: the live bot's defaults at aggression 0.25
    /// (`BasicBot(aggression=0.25, seed=city.id)`, `cities.py:1699`).
    #[must_use]
    pub fn auto_production() -> Self {
        Self { aggression: 0.25, ..Self::default() }
    }

    /// The aggression the advisor reads: [`aggression`](Self::aggression) held to 0 to 1, as
    /// `BasicBot.__init__` held it (`max(0.0, min(1.0, aggression))`, `basic.py:675`), NaN
    /// counting as 1 there too.
    #[must_use]
    pub fn aggr(&self) -> f64 {
        let a = if self.aggression < 1.0 { self.aggression } else { 1.0 };
        if a > 0.0 { a } else { 0.0 }
    }
}

// ---- The civilization's situation (basic.py:777-873) ------------------------------------------

/// What a civilization's turn looks like to production (`BasicBot.context`, `basic.py:777-835`),
/// gathered once.
#[derive(Clone, Debug)]
struct Situation {
    /// Its cities, by id.
    cities: Vec<CityId>,
    /// The enemy military weight near each city, as `cities` lists them.
    threat: Vec<f64>,
    /// Its units, by id.
    units: Vec<UnitId>,
    /// Its military units that are not scouts.
    military: Vec<UnitId>,
    /// The enemy military units it can see: whether any is a barbarian.
    hostile: bool,
    barbarians_near: bool,
    /// Gold per turn (`civ_stats(...)["gold"]`).
    gpt: f64,
    /// Happiness (`happiness(...)["total"]`).
    hap: i32,
    /// Its era, by index.
    era: usize,
    /// At war with a major civilization it has met.
    wars: bool,
    gold: f64,
    supply: i32,
    army_target: i32,
    /// At war or preparing one (a bot that asks only for production prepares none).
    offense: bool,
    /// With `GarrisonMode::Exposed`, the cities that want a garrison.
    exposed: Option<Vec<CityId>>,
    /// The luxuries it has, for a site's new luxuries.
    lux_owned: ResourceSet,
}

impl Situation {
    fn threat(&self, c: CityId) -> f64 {
        self.cities.iter().position(|&x| x == c).map_or(0.0, |i| self.threat[i])
    }
}

/// A unit's combat weight, the larger of its strengths (`_power`, `basic.py:653-655`).
fn power(d: &BaseUnitDef) -> f64 {
    f64::from(d.strength.max(d.ranged_strength))
}

/// Whether a unit is a scout (`_is_recon`, `basic.py:642-644`).
fn is_recon(g: &Game, d: &BaseUnitDef) -> bool {
    g.rules().derived().advisor.scout == Some(d.unit_type)
}

/// A civilization's era, by index.
fn era_index(g: &Game, p: PlayerId) -> usize {
    usize::from(civ::era(g, p).0)
}

/// The tiles within `radius` of `t` in Python's order (`hexmap.within`), where a sum over them
/// adds in that order.
fn within_py(g: &Game, t: TileIdx, radius: u32) -> Vec<TileIdx> {
    let mut v = g.grid().within(t, radius);
    v.sort_by_key(|&n| within_order(g, t, n));
    v
}

/// What civilization `p`'s turn looks like to production (`BasicBot.context`,
/// `basic.py:777-835`): the enemies it sees and how near each city they are, its army, its
/// economy, and how big an army it wants.
fn situation(g: &Game, p: PlayerId, pp: &AdvisorParams) -> Situation {
    let r = g.rules();
    let units: Vec<UnitId> = g.player_units(p).map(crate::state::units::Unit::id).collect();
    let cities: Vec<CityId> = g.player_cities(p).map(crate::state::cities::City::id).collect();
    let vis = g.derived().vis();
    let hostile: Vec<&crate::state::units::Unit> = g
        .state()
        .units()
        .iter()
        .filter(|u| {
            vis.sees(p, u.tile())
                && g.at_war(p, u.owner())
                && r.base_units()[u.base].military
                && super::vis::sight::unit_visible_to(g, p, u.id())
        })
        .collect();
    let grid = g.grid();
    let threat: Vec<f64> = cities
        .iter()
        .map(|&c| {
            let at = g.city(c).map_or(TileIdx(0), crate::state::cities::City::tile);
            let mut t = 0.0;
            for u in &hostile {
                let d = grid.distance(u.tile(), at);
                if d <= pp.threat_radius {
                    let w = if d <= pp.threat_near_dist {
                        pp.threat_near_weight
                    } else {
                        pp.threat_far_weight
                    };
                    t += power(&r.base_units()[u.base]) * f64::from(u.hp) / 100.0 * w;
                }
            }
            t
        })
        .collect();
    let military: Vec<UnitId> = units
        .iter()
        .copied()
        .filter(|&u| {
            g.unit(u).is_some_and(|x| {
                let d = &r.base_units()[x.base];
                d.military && !is_recon(g, d)
            })
        })
        .collect();
    let wars = g
        .state()
        .players()
        .iter()
        .any(|(q, x)| q != p && x.alive() && x.is_major() && g.has_met(p, q) && g.at_war(p, q));
    let n = f64::from(u32::try_from(cities.len()).unwrap_or(u32::MAX));
    let threatened =
        f64::from(u32::try_from(threat.iter().filter(|&&t| t > 0.0).count()).unwrap_or(0));
    let war_extra = if wars { n * pp.army_war_per_city + pp.army_war_extra } else { 0.0 };
    let army_target = num::trunc_i32(
        n * (pp.army_per_city + pp.army_per_city_aggr * pp.aggr())
            + pp.army_base
            + war_extra
            + pp.army_per_threatened_city * threatened,
    );
    let happiness = memo::happiness(g, p);
    let (hap, lux_owned) = (happiness.total, happiness.luxury_types.iter().copied().collect());
    drop(happiness);
    let exposed = (pp.garrison_mode == GarrisonMode::Exposed).then(|| {
        let capital = g.player(p).and_then(|x| x.capital);
        let others: Vec<TileIdx> = g
            .state()
            .cities()
            .iter()
            .filter(|x| x.owner() != p && !g.is_city_state(x.owner()))
            .map(crate::state::cities::City::tile)
            .collect();
        let camps: Vec<TileIdx> =
            g.state().world().camps.values().filter(|x| !x.destroyed).map(|x| x.tile).collect();
        let rr = pp.garrison_exposed_radius;
        cities
            .iter()
            .zip(&threat)
            .filter(|&(&c, &t)| {
                let at = g.city(c).map_or(TileIdx(0), crate::state::cities::City::tile);
                capital.is_none_or(|x| x == c)
                    || t > 0.0
                    || others.iter().chain(&camps).any(|&o| grid.distance(at, o) <= rr)
            })
            .map(|(&c, _)| c)
            .collect()
    });
    Situation {
        threat,
        hostile: !hostile.is_empty(),
        barbarians_near: hostile.iter().any(|u| g.is_barbarian(u.owner())),
        gpt: memo::civ_stats(g, p).total[Stat::Gold],
        hap,
        era: era_index(g, p),
        wars,
        gold: g.player(p).map_or(0.0, |x| x.econ.gold),
        supply: super::economy::unit_supply(g, p),
        army_target,
        offense: wars,
        exposed,
        lux_owned,
        cities,
        units,
        military,
    }
}

/// How well a city is defended (`BasicBot.city_defense`, `basic.py:836-844`): its strength by
/// its health, and its own military units on and beside it.
fn city_defense(g: &Game, c: CityId) -> f64 {
    let Some(city) = g.city(c) else { return 0.0 };
    let r = g.rules();
    let mut s = f64::from(city_strength(g, c)) * f64::from(city.health)
        / f64::from(max_health(g, c).max(1));
    for t in within_py(g, city.tile(), 1) {
        if let Some(m) = g.military_at(t).filter(|m| m.owner() == city.owner()) {
            s += power(&r.base_units()[m.base]) * f64::from(m.hp) / 100.0;
        }
    }
    s
}

/// Whether the enemies near a city outweigh its defence (`BasicBot.in_danger`,
/// `basic.py:869-871`).
fn in_danger(g: &Game, c: CityId, s: &Situation, pp: &AdvisorParams) -> bool {
    s.threat(c) > city_defense(g, c) * pp.danger_ratio
}

/// Whether a city wants a unit in it (`BasicBot.needs_garrison`, `basic.py:865-867`).
fn needs_garrison(c: CityId, s: &Situation) -> bool {
    s.exposed.as_ref().is_none_or(|e| e.contains(&c))
}

/// Whether building an item would use a resource kept for the spaceship
/// (`BasicBot._breaks_space_reserve`, `basic.py:846-863`): from the space era, while the
/// scientific victory is on, an item that needs one of the resources the spaceship's parts need,
/// unless it is a part, when fewer than `space_reserve` would be left.
fn breaks_space_reserve(
    g: &Game,
    p: PlayerId,
    item: Constructible,
    era: usize,
    pp: &AdvisorParams,
) -> bool {
    let r = g.rules();
    let a = &r.derived().advisor;
    let scientific = a.scientific.is_none_or(|v| g.victory_enabled(v));
    if pp.space_reserve == 0 || era < pp.tech_space_era || !scientific {
        return false;
    }
    let res = match item {
        Constructible::Unit(u) if a.parts.contains(u) => return false,
        Constructible::Unit(u) => r.base_units()[u].required_resource,
        Constructible::Building(b) => r.buildings()[b].required_resource,
        Constructible::Perpetual(_) => None,
    };
    let Some(res) = res.filter(|&x| a.space_resources.contains(x)) else { return false };
    let available = if r.resources()[res].kind == ResourceType::Strategic {
        super::economy::resource_amount(g, p, res)
    } else {
        0
    };
    available - 1 < pp.space_reserve
}

/// What a civilization already has, by kind of unit, counting what its cities are building
/// (`BasicBot._counts`, `basic.py:1113-1144`).
#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    settler: i32,
    worker: i32,
    recon: i32,
    boat: i32,
    /// Its military units that are not scouts, and those its cities are building.
    army: i32,
}

/// The kinds `_counts` sorts units into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Settler,
    Worker,
    Boat,
    Recon,
    Army,
}

fn kind(g: &Game, u: BaseUnitId) -> Option<Kind> {
    let r = g.rules();
    let a = &r.derived().advisor;
    let d = &r.base_units()[u];
    if a.founders.contains(u) {
        Some(Kind::Settler)
    } else if a.workers.contains(u) {
        Some(Kind::Worker)
    } else if a.boats.contains(u) {
        Some(Kind::Boat)
    } else if is_recon(g, d) {
        Some(Kind::Recon)
    } else if d.military {
        Some(Kind::Army)
    } else {
        None
    }
}

fn counts(g: &Game, s: &Situation) -> Counts {
    let mut out =
        Counts { army: i32::try_from(s.military.len()).unwrap_or(i32::MAX), ..Counts::default() };
    let mut add = |k: Option<Kind>, queued: bool| match k {
        Some(Kind::Settler) => out.settler += 1,
        Some(Kind::Worker) => out.worker += 1,
        Some(Kind::Boat) => out.boat += 1,
        Some(Kind::Recon) => out.recon += 1,
        Some(Kind::Army) if queued => out.army += 1,
        _ => {}
    };
    for &u in &s.units {
        if let Some(x) = g.unit(u) {
            add(kind(g, x.base), false);
        }
    }
    for city in s.cities.iter().filter_map(|&c| g.city(c)) {
        for &item in &city.queue {
            if let Constructible::Unit(u) = item {
                add(kind(g, u), true);
            }
        }
    }
    out
}

// ---- Expansion (basic.py:1069-1111, 1210-1219, 1890-1894) ---------------------------------------

/// How good a site tile `t` is for a city of `p` (`automation.city_site_score`), or `None` where
/// no city can go: package 1c-04's.
fn site_score(_g: &Game, _p: PlayerId, _t: TileIdx) -> Option<f64> {
    pending_or(Porting::Pending("1c-04"), None)
}

/// Where a civilization would found its next cities, best first (`BasicBot.expansion_sites`,
/// `basic.py:1069-1111`): land within reach of its cities (or of its settlers, before it has a
/// city), on a landmass it is on, far enough from its cities, scoring enough, less for distance,
/// spaced apart. A bot that asks only for production has no sites cached and none it gave up
/// on.
fn expansion_sites(g: &Game, p: PlayerId, s: &Situation, pp: &AdvisorParams) -> Vec<TileIdx> {
    let r = g.rules();
    let a = &r.derived().advisor;
    let grid = g.grid();
    let mut centers: Vec<TileIdx> =
        s.cities.iter().filter_map(|&c| g.city(c).map(crate::state::cities::City::tile)).collect();
    if centers.is_empty() {
        centers = s
            .units
            .iter()
            .filter_map(|&u| g.unit(u))
            .filter(|u| a.founders.contains(u.base))
            .map(crate::state::units::Unit::tile)
            .collect();
    }
    let reachable: SmallVec<[Option<u16>; 8]> = centers.iter().map(|&t| g.continent(t)).collect();
    let mut seen = crate::base::sets::BitSet::new();
    let mut scored: Vec<(f64, TileIdx)> = Vec::new();
    for &center in &centers {
        for t in grid.within(center, pp.site_radius) {
            if seen.contains(t.0) {
                continue;
            }
            seen.insert(t.0);
            if g.is_water(t)
                || grid.distance(center, t) < pp.site_min_distance
                || !reachable.contains(&g.continent(t))
            {
                continue;
            }
            let Some(mut score) = site_score(g, p, t).filter(|&x| x >= pp.site_min_score) else {
                continue;
            };
            if pp.site_new_lux != 0.0 {
                let mut new = ResourceSet::new();
                for n in grid.within(t, pp.site_lux_radius) {
                    let Some(tile) = g.tile(n) else { continue };
                    let Some(res) = tile.resource() else { continue };
                    let rd = &r.resources()[res];
                    if tile.owner().is_none_or(|o| o == p)
                        && g.has_tech(p, rd.revealed_by)
                        && rd.kind == ResourceType::Luxury
                        && !s.lux_owned.contains(res)
                    {
                        new.insert(res);
                    }
                }
                score += pp.site_new_lux * f64::from(u32::try_from(new.len()).unwrap_or(0));
            }
            let d = centers.iter().map(|&c| grid.distance(c, t)).min().unwrap_or(0);
            scored.push((score - f64::from(d) * pp.site_distance_cost, t));
        }
    }
    // Python sorted the (score, tile) pairs from the largest down.
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut picked: Vec<TileIdx> = Vec::new();
    for (_, t) in scored {
        if picked.iter().all(|&o| grid.distance(t, o) >= pp.site_spacing) {
            picked.push(t);
        }
        if picked.len() >= pp.site_candidates {
            break;
        }
    }
    picked
}

/// Whether civilization `p` has seen a rival's city (`BasicBot._knows_rival_city`,
/// `basic.py:1890-1894`).
fn knows_rival_city(g: &Game, p: PlayerId) -> bool {
    let Some(pl) = g.player(p) else { return false };
    g.state().cities().iter().any(|c| {
        c.owner() != p
            && g.player(c.owner()).is_some_and(crate::state::players::Player::is_major)
            && pl.explored.contains(c.tile().0)
    })
}

/// Whether a city may start a settler (`BasicBot._may_build_settler`, `basic.py:1210-1219`):
/// room under the city and settler caps, a big enough city, enough happiness, and a site in
/// range. The sites are asked last, and only then (Python asked them first, for every choice):
/// scoring them walks the `site_radius` of every city.
fn may_build_settler(adv: &Advisor, g: &Game, c: CityId, check_size: bool) -> bool {
    let (s, k, pp) = (&adv.s, &adv.k, &adv.pp);
    let Some(city) = g.city(c) else { return false };
    let n = i32::try_from(s.cities.len()).unwrap_or(i32::MAX);
    let cap = if pp.target_cities == 0 { 99 } else { pp.target_cities };
    n + k.settler < cap
        && k.settler < (n.div_euclid(pp.settler_per_cities.max(1))).max(1)
        && (!check_size || i32::from(city.pop) >= pp.settler_min_pop)
        && (s.hap >= pp.settler_min_hap
            || (s.hap >= pp.settler_min_hap_small && n < pp.settler_small_empire))
        && adv.sites(g).iter().any(|&x| {
            g.grid().distance(x, city.tile()) <= pp.settler_site_range
                && found_check(g, adv.p, x).is_none()
        })
}

// ---- Units (basic.py:1325-1336, 1543-1575) ---------------------------------------------------

/// What `best_military` looks for.
#[derive(Clone, Copy, Default)]
struct Role {
    prefer_ranged: bool,
    offense: bool,
    siege_only: bool,
    melee_only: bool,
}

/// The best military unit a city can build for a role (`BasicBot.best_military`,
/// `basic.py:1543-1575`): land units that fight, not scouts, nuclear weapons or missiles, by
/// strength (ranged strength weighed by the role) over cost; siege units less when defending,
/// mounted and armoured units less when not attacking.
fn best_military(
    g: &Game,
    c: CityId,
    units: &BaseUnitSet,
    role: Role,
    pp: &AdvisorParams,
) -> Option<BaseUnitId> {
    let r = g.rules();
    let a = &r.derived().advisor;
    let owner = g.city(c)?.owner();
    let mut best = None;
    let mut best_v = -1.0;
    let mut era = None;
    for u in units.iter() {
        let d = &r.base_units()[u];
        if pp.space_reserve != 0 && d.required_resource.is_some() {
            let e = *era.get_or_insert_with(|| era_index(g, owner));
            if breaks_space_reserve(g, owner, Constructible::Unit(u), e, pp) {
                continue;
            }
        }
        if !d.military || is_recon(g, d) || d.domain != Domain::Land || a.not_army.contains(u) {
            continue;
        }
        let siege = a.siege == Some(d.unit_type);
        if (role.siege_only && !siege) || (role.melee_only && d.ranged) {
            continue;
        }
        let v = unit_value(g, d, role, pp);
        if v > best_v {
            best = Some(u);
            best_v = v;
        }
    }
    best
}

/// What a military unit is worth to a role (`best_military`'s value, `basic.py:1561-1570`): its
/// strength, or its ranged strength weighed by the role, over a power of its cost; a siege unit
/// less when defending, a mounted or armoured one less when not attacking.
fn unit_value(g: &Game, d: &BaseUnitDef, role: Role, pp: &AdvisorParams) -> f64 {
    let a = &g.rules().derived().advisor;
    let ranged_w = if role.prefer_ranged { pp.unit_ranged_pref } else { pp.unit_ranged_nopref };
    let mut v = f64::from(d.strength).max(f64::from(d.ranged_strength) * ranged_w);
    if a.siege == Some(d.unit_type) {
        v *= if role.offense { 1.0 } else { pp.unit_siege_defensive };
    }
    let mobile = a.mounted == Some(d.unit_type) || a.armored == Some(d.unit_type);
    if mobile && !role.offense {
        v *= pp.unit_mobile_defensive;
    }
    // refcheck: advisor-counts-a-free-unit-as-costing-one
    v / num::pow(f64::from(d.cost.max(1)), pp.unit_cost_exp)
}

/// Whether city `c` prefers a ranged unit this turn (`self.rng.random() < ranged_chance`,
/// `basic.py:1336-1337`): a draw of its own, keyed by the city and the turn.
// refcheck: advisor-draws-by-city-and-turn
fn prefers_ranged(g: &Game, c: CityId, pp: &AdvisorParams) -> bool {
    let turn = u64::try_from(g.turn()).unwrap_or(0);
    let mut rng = Rng::keyed(g.state().seed(), Purpose::Advisor, &[u64::from(c.get()), turn]);
    rng.unit() < pp.ranged_chance
}

/// Which military unit to build (`BasicBot._pick_military`, `basic.py:1325-1336`): melee while
/// the field army has too few to take cities, siege when attacking with too few, else the best
/// unit, preferring a ranged one by chance.
fn pick_military(
    g: &Game,
    c: CityId,
    s: &Situation,
    units: &BaseUnitSet,
    pp: &AdvisorParams,
) -> Option<BaseUnitId> {
    let r = g.rules();
    let a = &r.derived().advisor;
    let bases = || s.military.iter().filter_map(|&u| g.unit(u)).map(|u| &r.base_units()[u.base]);
    let mil = i32::try_from(s.military.len()).unwrap_or(i32::MAX);
    let n = i32::try_from(s.cities.len()).unwrap_or(i32::MAX);
    let siege =
        i32::try_from(bases().filter(|d| a.siege == Some(d.unit_type)).count()).unwrap_or(i32::MAX);
    let want_siege = s.offense && siege.saturating_mul(pp.siege_per_units) < mil - n + 1;
    // A bot that asks only for production keeps no garrisons: every melee unit is in the field.
    let field_melee = i32::try_from(bases().filter(|d| !d.ranged).count()).unwrap_or(i32::MAX);
    let want_melee = s.offense && field_melee < pp.min_field_melee;
    if want_melee
        && let Some(u) = best_military(
            g,
            c,
            units,
            Role { offense: true, melee_only: true, ..Role::default() },
            pp,
        )
    {
        return Some(u);
    }
    if want_siege
        && let Some(u) =
            best_military(g, c, units, Role { siege_only: true, ..Role::default() }, pp)
    {
        return Some(u);
    }
    let prefer_ranged = prefers_ranged(g, c, pp);
    best_military(g, c, units, Role { prefer_ranged, offense: s.offense, ..Role::default() }, pp)
}

// ---- Building values (basic.py:1338-1392, 1506-1541) -------------------------------------------

/// A building's great person points and specialist slots, summed.
fn points_and_slots(g: &Game, b: BuildingId) -> (f64, f64) {
    let bd = &g.rules().buildings()[b];
    let gpp: i32 = bd.great_person_points.iter().map(|&(_, n)| n).sum();
    let slots: i32 = bd.specialist_slots.iter().map(|&(_, n)| n).sum();
    (f64::from(gpp), f64::from(slots))
}

/// A building's value the way UnCiv's `getValueOfBuilding` weighs it
/// (`BasicBot._building_value_unciv_raw`, `basic.py:1353-1392`): the weighted difference of the
/// city's stats and happiness with it, food carried over, great person points and specialist
/// slots, defence by the city's need of it, wonders, and what wins the game or opens the
/// spaceship in a strong city.
fn building_value_unciv(
    g: &Game,
    wi: Option<&CityWhatIf<'_>>,
    c: CityId,
    b: BuildingId,
    s: &Situation,
    over_avg: bool,
    pp: &AdvisorParams,
) -> f64 {
    let Some(StatsDelta { before: base, after }) = wi.and_then(|w| w.with(b)) else {
        return 0.0;
    };
    let r = g.rules();
    let a = &r.derived().advisor;
    let bd = &r.buildings()[b];
    let d = |k: Stat| after[k] - base[k];
    let n = i32::try_from(s.cities.len()).unwrap_or(i32::MAX);
    let gold_w = pp.u_gold * if s.gold < 0.0 && s.gpt <= 0.0 { pp.u_gold_broke_mult } else { 1.0 };
    let cult_w = pp.u_culture
        * if base[Stat::Culture] < pp.u_culture_first_below {
            pp.u_culture_first_mult
        } else {
            1.0
        };
    let low = s.hap < pp.u_happiness_low_below || s.hap < n;
    let hap_w = pp.u_happiness * if low { pp.u_happiness_low } else { 1.0 };
    let mut v = d(Stat::Food) * pp.u_food
        + d(Stat::Production) * pp.u_production
        + d(Stat::Gold) * gold_w
        + d(Stat::Science) * pp.u_science
        + d(Stat::Culture) * cult_w
        + d(Stat::Faith) * pp.u_faith
        + d(Stat::Happiness) * hap_w;
    for &pct in a.carry_over.get(b).map_or(&[][..], |x| &x[..]) {
        v += base[Stat::Food].max(0.0) * f64::from(pct) / 100.0 * pp.u_food * pp.u_carryover_share;
    }
    let (gpp, slots) = points_and_slots(g, b);
    v += gpp * pp.u_gpp;
    v += slots * pp.u_specialist;
    let mut war = if s.wars { pp.u_defense_war } else { pp.u_defense_peace };
    if s.threat(c) > 0.0 {
        war *= pp.u_defense_threat_mult;
    }
    if bd.city_health != 0 {
        v +=
            war * f64::from(bd.city_health) / f64::from(max_health(g, c).max(1)) * pp.u_city_health;
    }
    if bd.city_strength != 0.0 {
        v += war * bd.city_strength / (f64::from(city_strength(g, c)) + pp.u_city_strength_offset)
            * pp.u_city_strength;
    }
    if bd.is_wonder {
        v += pp.u_wonder_bonus;
    } else if bd.is_national_wonder {
        v += pp.u_wonder_bonus * pp.u_national_wonder_share;
    }
    if over_avg && a.victory.contains(b) {
        v += pp.u_victory_building;
    }
    if pp.u_space_program != 0.0 && over_avg && a.space_program.contains(b) {
        v += pp.u_space_program;
    }
    v
}

/// A building's value in the classic valuation (`BasicBot._building_value`,
/// `basic.py:1506-1541`): the weighted difference of the city's stats and happiness with it,
/// gold when broke, great person points, specialist slots, defence, its rules the simulation
/// cannot see, wonders, and less in a small city.
fn building_value_classic(
    g: &Game,
    wi: Option<&CityWhatIf<'_>>,
    c: CityId,
    b: BuildingId,
    s: &Situation,
    pp: &AdvisorParams,
) -> f64 {
    let Some(StatsDelta { before: base, after }) = wi.and_then(|w| w.with(b)) else {
        return 0.0;
    };
    let r = g.rules();
    let bd = &r.buildings()[b];
    let d = |k: Stat| after[k] - base[k];
    let weights = [
        (Stat::Food, pp.w_food),
        (Stat::Production, pp.w_production),
        (Stat::Gold, pp.w_gold),
        (Stat::Science, pp.w_science),
        (Stat::Culture, pp.w_culture),
        (Stat::Faith, pp.w_faith),
    ];
    let mut v = 0.0;
    for (k, w) in weights {
        v += d(k) * w;
    }
    let want = i32::try_from(s.cities.len()).unwrap_or(i32::MAX).saturating_add(pp.w_hap_margin);
    let mood = if s.hap < 0 {
        pp.w_hap_unhappy
    } else if s.hap < want {
        pp.w_hap_low
    } else {
        pp.w_hap_ok
    };
    v += d(Stat::Happiness) * pp.w_happiness * mood;
    if s.gpt < 0.0 && d(Stat::Gold) > 0.0 {
        v += d(Stat::Gold) * pp.w_gold_broke;
    }
    let (gpp, slots) = points_and_slots(g, b);
    v += gpp * pp.w_gpp;
    v += slots * pp.w_specialist;
    if bd.city_strength != 0.0 || bd.city_health != 0 {
        let border = s.wars || s.threat(c) > 0.0;
        v += (bd.city_strength / pp.w_def_strength_div
            + f64::from(bd.city_health) / pp.w_def_health_div)
            * if border { pp.w_def_border } else { pp.w_def_interior };
    }
    v += pp.unique_bonus * f64::from(u32::try_from(bd.uniques.all.len()).unwrap_or(u32::MAX));
    if bd.is_wonder {
        v = v * pp.wonder_mult + pp.wonder_bonus;
    }
    if g.city(c).is_some_and(|x| i32::from(x.pop) <= pp.w_small_city_pop) && !bd.is_wonder {
        v *= pp.w_small_city_mult;
    }
    v
}

// ---- The choice (basic.py:1146-1150, 1204-1336, 1394-1489) -------------------------------------

/// A candidate and its value.
#[derive(Clone, Copy, Debug)]
struct Choice {
    value: f64,
    item: Constructible,
}

/// The first unit of a set a predicate holds for, in the ruleset's order.
fn first(units: &BaseUnitSet, mut f: impl FnMut(BaseUnitId) -> bool) -> Option<BaseUnitId> {
    units.iter().find(|&u| f(u))
}

/// Whether a sea resource near a city waits for a work boat (`basic.py:1284-1289, 1455-1462`):
/// on a water tile of the civilization's within `boat_search_radius`, unimproved and seen.
fn boat_wanted(g: &Game, p: PlayerId, c: CityId, pp: &AdvisorParams) -> bool {
    let Some(at) = g.city(c).map(crate::state::cities::City::tile) else { return false };
    let r = g.rules();
    g.grid().within(at, pp.boat_search_radius).into_iter().any(|t| {
        g.tile(t).is_some_and(|tile| {
            tile.owner() == Some(p)
                && tile.resource().is_some_and(|res| g.has_tech(p, r.resources()[res].revealed_by))
                && g.is_water(t)
                && tile.improvement().is_none()
        })
    })
}

/// Whether a civilization that has met no rival sends a unit to look (`basic.py:1270-1271,
/// 1446-1450`): late enough, no scout about or coming, its army complete, and nobody exploring.
fn seek(g: &Game, s: &Situation, k: &Counts, met: bool, turn: i32, pp: &AdvisorParams) -> bool {
    !met && turn > pp.seek_after_turn
        && k.recon == 0
        && k.army <= i32::try_from(s.military.len()).unwrap_or(i32::MAX)
        && !s
            .units
            .iter()
            .any(|&u| g.unit(u).is_some_and(|x| x.activity == Some(Activity::Explore)))
}

/// What city `c` of civilization `p` should build next, in UnCiv's way
/// (`BasicBot._choose_production_unciv`, `basic.py:1225-1323`): a defender first in danger or
/// for an empty city; otherwise every option valued, settlers, scouts, workers, work boats,
/// military units by need, buildings and wonders by their value, spaceship parts, each by its
/// value per production left; a conversion of production, or a defender, when nothing is worth
/// building.
// refcheck: advisor-ties-by-id
#[allow(clippy::too_many_lines, reason = "one choice, in Python's order")]
fn choose_unciv(adv: &Advisor, g: &Game, c: CityId, danger: bool) -> Option<Constructible> {
    let (p, s, k, pp) = (adv.p, &adv.s, &adv.k, &adv.pp);
    let r = g.rules();
    let a = &r.derived().advisor;
    let city = g.city(c)?;
    let n = i32::try_from(s.cities.len()).unwrap_or(i32::MAX);
    let items = buildable_items(g, c);
    let units = items.units;
    let mut prods: Vec<(CityId, f64)> = Vec::new();
    for &x in &s.cities {
        if g.city(x).is_some_and(|y| !y.puppet) {
            prods.push((x, memo::city_stats(g, x).production().max(1.0)));
        }
    }
    let prod = prods.iter().find(|(x, _)| *x == c).map_or(1.0, |&(_, v)| v);
    let total: f64 = prods.iter().map(|&(_, v)| v).sum();
    let over_avg = prod >= total / f64::from(u32::try_from(prods.len().max(1)).unwrap_or(1));
    let total_pop: i32 = s.cities.iter().filter_map(|&x| g.city(x)).map(|x| i32::from(x.pop)).sum();
    let turn = g.turn();
    let defender = best_military(g, c, &units, Role { prefer_ranged: true, ..Role::default() }, pp);
    if let Some(d) = defender {
        if danger {
            return Some(Constructible::Unit(d));
        }
        if g.military_at(city.tile()).is_none()
            && (turn > pp.garrison_after_turn || s.hostile)
            && needs_garrison(c, s)
        {
            return Some(Constructible::Unit(d));
        }
    }
    let mut choices: Vec<Choice> = Vec::new();
    let mut add = |item: Option<Constructible>, modifier: f64| {
        if let Some(item) = item.filter(|_| modifier > 0.0) {
            let left = remaining_work(g, c, item).max(1.0);
            choices.push(Choice { value: modifier / left, item });
        }
    };
    let unit = |u: Option<BaseUnitId>| u.map(Constructible::Unit);
    let settler = first(&units, |u| a.founders.contains(u));
    if settler.is_some() && may_build_settler(adv, g, c, true) {
        add(unit(settler), pp.u_settler);
    }
    let met = knows_rival_city(g, p);
    let scout = first(&units, |u| is_recon(g, &r.base_units()[u]));
    if scout.is_some() && k.recon == 0 && !met && turn < pp.scout_until_turn {
        add(unit(scout), pp.u_scout);
    }
    if seek(g, s, k, met, turn, pp) {
        let seeker =
            if turn > pp.seek_military_after || scout.is_none() { defender } else { scout };
        add(unit(seeker), pp.u_seeker);
    }
    // Workers (UnCiv's addWorkerChoice).
    let worker = first(&units, |u| a.workers.contains(u));
    if worker.is_some() {
        let (wpc, full) = (pp.workers_per_city, pp.workers_full_until);
        let mut want = if n <= 1 {
            1.0
        } else if n <= full {
            wpc * f64::from(n)
        } else {
            wpc * f64::from(full) + wpc * pp.workers_extra_share * f64::from(n - full)
        };
        let unimproved = s
            .cities
            .iter()
            .filter_map(|&x| g.city(x))
            .flat_map(|x| x.worked.iter().copied())
            .filter(|&t| g.tile(t).is_some_and(|x| x.improvement().is_none()) && !g.is_water(t))
            .count();
        want += pp.worker_unimproved * f64::from(u32::try_from(unimproved).unwrap_or(u32::MAX));
        if f64::from(k.worker) < want {
            add(unit(worker), pp.u_worker * want / (f64::from(k.worker) + pp.worker_count_offset));
        }
    }
    // Work boats.
    let boat = first(&units, |u| a.boats.contains(u));
    if boat.is_some() && k.boat == 0 && boat_wanted(g, p, c, pp) {
        add(unit(boat), pp.u_boat);
    }
    // Military units (UnCiv's addMilitaryUnitChoice, and war preparation).
    let mil = i32::try_from(s.military.len()).unwrap_or(i32::MAX);
    let n_units = i32::try_from(s.units.len()).unwrap_or(i32::MAX);
    let at_war = s.wars;
    let peace_cap = pp.peace_army_min.max(n.saturating_mul(pp.peace_army_per_city));
    if defender.is_some()
        && (at_war || s.offense || over_avg)
        && n_units < s.supply
        && (at_war || s.offense || (s.gpt >= 0.0 && mil <= peace_cap))
        && s.gold > pp.military_min_gold
    {
        let mut modifier = pp.mil_base + (f64::from(n) / f64::from(mil + 1)).sqrt() * pp.mil_scale;
        if at_war {
            modifier *= pp.mil_war_mult;
        }
        if s.offense && k.army < s.army_target {
            modifier *= pp.u_offense * (pp.mil_offense_aggr_base + pp.aggr());
        }
        if s.barbarians_near {
            modifier = modifier.max(pp.mil_barbarian_min);
        }
        if !over_avg {
            modifier /= pp.mil_below_avg_div;
        }
        if k.army >= s.army_target && !at_war {
            modifier /= pp.mil_army_full_div;
        }
        let picked = pick_military(g, c, s, &units, pp).or(defender);
        add(unit(picked), pp.u_military * modifier);
    }
    // Buildings, and wonders unless the city is a puppet or the empire too small.
    let gate = !pp.u_wonder_gate || (over_avg && total_pop >= pp.wonder_gate_pop);
    let wonders = if !city.puppet && gate { items.wonders } else { Default::default() };
    let wi = CityWhatIf::new(g, c);
    for b in items.buildings.iter().chain(wonders.iter()) {
        let item = Constructible::Building(b);
        if !breaks_space_reserve(g, p, item, s.era, pp) {
            add(Some(item), building_value_unciv(g, wi.as_ref(), c, b, s, over_avg, pp));
        }
    }
    // Projects: Python looked for buildings among the conversions of production, and found none.
    for u in units.iter() {
        if a.space.contains(u) && over_avg {
            add(Some(Constructible::Unit(u)), pp.u_spaceship);
        }
    }
    if choices.is_empty() {
        if items.science {
            return Some(Constructible::Perpetual(Perpetual::Science));
        }
        if items.gold {
            return Some(Constructible::Perpetual(Perpetual::Gold));
        }
        return defender.map(Constructible::Unit);
    }
    best(&choices)
}

/// The best choice: the largest value, then the largest item (`max` over `(value, name)`).
fn best(choices: &[Choice]) -> Option<Constructible> {
    choices
        .iter()
        .max_by(|a, b| a.value.total_cmp(&b.value).then(a.item.cmp(&b.item)))
        .map(|x| x.item)
}

/// What city `c` of civilization `p` should build next by the older fixed priorities
/// (`BasicBot._choose_production_classic`, `basic.py:1394-1489`): needs first, a defender in
/// danger or for an empty city, the army when it is short, settlers, scouts, workers and work
/// boats; then buildings by value per turn, and spaceship parts. The first of equal priorities
/// wins, as Python's stable sort kept them.
#[allow(clippy::too_many_lines, reason = "one choice, in Python's order")]
fn choose_classic(adv: &Advisor, g: &Game, c: CityId, danger: bool) -> Option<Constructible> {
    let (p, s, k, pp) = (adv.p, &adv.s, &adv.k, &adv.pp);
    let r = g.rules();
    let a = &r.derived().advisor;
    let city = g.city(c)?;
    let n = i32::try_from(s.cities.len()).unwrap_or(i32::MAX);
    let items = buildable_items(g, c);
    let units = items.units;
    let prod = memo::city_stats(g, c).production().max(1.0);
    let turns = |item| (remaining_work(g, c, item) / prod).max(1.0);
    let turn = g.turn();
    let mut options: Vec<Choice> = Vec::new();
    let mut add = |value: f64, item: BaseUnitId| {
        options.push(Choice { value, item: Constructible::Unit(item) });
    };
    let defender = best_military(g, c, &units, Role { prefer_ranged: true, ..Role::default() }, pp);
    if let Some(d) = defender {
        if danger {
            add(pp.c_danger, d);
        } else if g.military_at(city.tile()).is_none()
            && (turn > pp.garrison_after_turn || s.hostile)
            && needs_garrison(c, s)
        {
            add(pp.c_garrison, d);
        }
        let affordable = i32::try_from(s.units.len()).unwrap_or(i32::MAX) < s.supply
            && s.gpt >= pp.c_military_min_gpt;
        let target = s.army_target;
        if k.army < target && (affordable || s.offense) {
            let attacker = pick_military(g, c, s, &units, pp).unwrap_or(d);
            let shortfall = 1.0 - f64::from(k.army) / f64::from(target.max(1));
            let base = if s.offense {
                pp.c_army_offense + pp.c_army_offense_aggr * pp.aggr()
            } else {
                pp.c_army_peace + pp.c_army_peace_aggr * pp.aggr()
            };
            add(base * (pp.c_army_short_base + pp.c_army_short_scale * shortfall), attacker);
        }
    }
    let settler = first(&units, |u| a.founders.contains(u));
    if let Some(x) = settler
        && !danger
        && may_build_settler(adv, g, c, false)
    {
        let prio =
            if turn < pp.settler_prio_until { pp.settler_prio } else { pp.settler_prio_late };
        add(f64::from(prio - n.saturating_mul(pp.settler_prio_per_city)), x);
    }
    let met = knows_rival_city(g, p);
    let scout = first(&units, |u| is_recon(g, &r.base_units()[u]));
    if let Some(x) = scout
        && k.recon == 0
        && (!met || turn < pp.c_scout_early_turns)
        && turn < pp.scout_until_turn
    {
        add(if !met && turn > pp.c_scout_after_turn { pp.c_scout_needed } else { pp.c_scout }, x);
    }
    if seek(g, s, k, met, turn, pp) {
        let seeker =
            if turn > pp.seek_military_after || scout.is_none() { defender } else { scout };
        if let Some(x) = seeker {
            add(pp.c_seeker, x);
        }
    }
    if let Some(x) = first(&units, |u| a.workers.contains(u)) {
        let want = n + if n <= pp.c_worker_extra_until { pp.c_worker_extra } else { 0 };
        if k.worker < want {
            let urgent = k.worker < n.div_euclid(pp.c_worker_urgent_div.max(1)).max(1);
            add(if urgent { pp.c_worker_urgent } else { pp.c_worker }, x);
        }
    }
    // A bot that asks only for production has queued no boat anywhere, so any turn is past the
    // retry wait.
    if let Some(x) = first(&units, |u| a.boats.contains(u))
        && k.boat == 0
        && turn + 99 > pp.c_boat_retry_turns
        && boat_wanted(g, p, c, pp)
    {
        add(pp.c_boat, x);
    }
    let mut wonders = items.wonders;
    if !wonders.is_empty() {
        let pop: i32 = s.cities.iter().filter_map(|&x| g.city(x)).map(|x| i32::from(x.pop)).sum();
        let weak = pp.wonder_avg_prod && {
            let all: f64 =
                s.cities.iter().map(|&x| memo::city_stats(g, x).production().max(1.0)).sum();
            prod < all / f64::from(n.max(1))
        };
        if pop < pp.wonder_min_pop || weak {
            wonders = Default::default();
        }
    }
    let wi = CityWhatIf::new(g, c);
    for b in items.buildings.iter().chain(wonders.iter()) {
        let item = Constructible::Building(b);
        if breaks_space_reserve(g, p, item, s.era, pp) {
            continue;
        }
        let v = building_value_classic(g, wi.as_ref(), c, b, s, pp);
        if v <= 0.0 {
            continue;
        }
        let value = v * pp.c_building_scale / (1.0 + turns(item) / pp.c_building_turns);
        options.push(Choice { value, item });
    }
    for u in units.iter() {
        if a.space.contains(u) {
            options.push(Choice { value: pp.c_spaceship, item: Constructible::Unit(u) });
        }
    }
    if options.is_empty() {
        if items.gold {
            return Some(Constructible::Perpetual(Perpetual::Gold));
        }
        return best_military(g, c, &units, Role::default(), pp).map(Constructible::Unit);
    }
    // The first of the highest, as Python's stable sort on the value alone left them.
    let mut top = options[0];
    for o in &options[1..] {
        if o.value > top.value {
            top = *o;
        }
    }
    Some(top.item)
}

/// The production advisor for one civilization's turn: what it gathers once and reads for each
/// of its cities (`BasicBot.context`, `_counts` and the cached `expansion_sites`, as
/// `manage_cities` shared them, `basic.py:1152-1180`). The bot of Phase 2 keeps one for a
/// civilization's turn, asks it for each city ([`advise`](Self::advise)) and tells it what each
/// started ([`started`](Self::started)); [`advise_production`] asks a fresh one, as automatic
/// production does for each pick (a fresh `BasicBot`, `cities.py:1699`).
///
/// It holds no game: the game it is asked with may have moved since it was made, as Python's
/// context did while the bot set production.
#[derive(Clone, Debug)]
pub struct Advisor {
    p: PlayerId,
    pp: AdvisorParams,
    s: Situation,
    k: Counts,
    /// Where the civilization would found its next cities, asked the first time a city could
    /// start a settler, then kept for the advisor's turn (`_sites_cache`, `site_cache_turns`);
    /// each read drops a site a city can no longer be founded on, as Python's cache did.
    sites: OnceCell<Vec<TileIdx>>,
}

impl Advisor {
    /// The advisor for civilization `p`'s turn as the game is now, with parameters `pp`.
    #[must_use]
    pub fn new(g: &Game, p: PlayerId, pp: &AdvisorParams) -> Self {
        let s = situation(g, p, pp);
        let k = counts(g, &s);
        Self { p, pp: pp.clone(), s, k, sites: OnceCell::new() }
    }

    /// What city `c` should build next (`BasicBot._choose_production`, `basic.py:1204-1208`),
    /// in danger when the enemies near it outweigh its defence: `None` when it would build
    /// nothing. Reads only.
    #[must_use]
    pub fn advise(&self, g: &Game, c: CityId) -> Option<Constructible> {
        g.city(c)?;
        let danger = in_danger(g, c, &self.s, &self.pp);
        match self.pp.prod_mode {
            ProductionMode::Unciv => choose_unciv(self, g, c, danger),
            ProductionMode::Classic => choose_classic(self, g, c, danger),
        }
    }

    /// Counts `item` as started by one of its cities (`manage_cities`, `basic.py:1166-1179`): the
    /// next city's choice sees one more settler, worker, work boat, scout or military unit.
    pub fn started(&mut self, g: &Game, item: Constructible) {
        let Constructible::Unit(u) = item else { return };
        let k = &mut self.k;
        match kind(g, u) {
            Some(Kind::Settler) => k.settler += 1,
            Some(Kind::Worker) => k.worker += 1,
            Some(Kind::Boat) => k.boat += 1,
            Some(Kind::Recon) => k.recon += 1,
            Some(Kind::Army) => k.army += 1,
            None => {}
        }
    }

    /// Where the civilization would found its next cities ([`expansion_sites`]), computed the
    /// first time they are asked.
    fn sites(&self, g: &Game) -> &[TileIdx] {
        self.sites.get_or_init(|| expansion_sites(g, self.p, &self.s, &self.pp))
    }
}

/// What city `c` of civilization `p` would build next by the live bot's production
/// (`BasicBot.advise_production`, `basic.py:1146-1150`): `None` when it would build nothing.
/// Reads only. It gathers what [`Advisor`] keeps afresh: asking for several cities of a
/// civilization in one turn, keep an `Advisor`.
#[must_use]
pub fn advise_production(
    g: &Game,
    p: PlayerId,
    c: CityId,
    pp: &AdvisorParams,
) -> Option<Constructible> {
    g.city(c)?;
    Advisor::new(g, p, pp).advise(g, c)
}

/// What a city whose queue ran empty starts on its own (`cities.auto_pick_production`,
/// `cities.py:1696-1717`): a puppet what [`puppet_pick`] picks, any other city what
/// [`advise_production`] advises at automatic production's aggression. Reads only.
#[must_use]
pub fn auto_pick(g: &Game, c: CityId) -> Option<Constructible> {
    let city = g.city(c)?;
    if city.puppet {
        return puppet_pick(g, c);
    }
    advise_production(g, city.owner(), c, &AdvisorParams::auto_production())
}

/// What a puppet builds (`cities.py:1700-1705`): the building of the most value in the classic
/// valuation, never a wonder or a unit, or Gold when none is worth building (the puppet rule,
/// UnCiv's: a puppet builds buildings, or gold). Reads only; it asks whatever the city is.
// refcheck: advisor-ties-by-id
#[must_use]
pub fn puppet_pick(g: &Game, c: CityId) -> Option<Constructible> {
    let pp = AdvisorParams::auto_production();
    let p = g.city(c)?.owner();
    let items = buildable_items(g, c);
    let s = situation(g, p, &pp);
    let mut scored: Vec<Choice> = Vec::new();
    let wi = CityWhatIf::new(g, c);
    for b in items.buildings.iter() {
        let value = building_value_classic(g, wi.as_ref(), c, b, &s, &pp);
        if value > 0.0 {
            scored.push(Choice { value, item: Constructible::Building(b) });
        }
    }
    best(&scored).or_else(|| items.gold.then_some(Constructible::Perpetual(Perpetual::Gold)))
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;

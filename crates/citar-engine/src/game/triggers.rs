//! Firing triggers and applying one-time effects (`triggers.py:38-374`, DESIGN.md 5.9).
//!
//! - [`fire`] finds the uniques that fire for an event at a site (`unique::trigger::fire`, the
//!   matching half of `triggers.fire`, `triggers.py:38-65`) and applies each;
//! - [`apply`] is `apply_one_time`: what one one-time unique does, for every kind of
//!   [`OneTimeEffect`] (`triggers.trigger`, `triggers.py:75-367`), keyed by the unique, the
//!   civilization, the tile and the turn (`Purpose::Trigger`);
//! - [`on_gain`] applies what a source gives once when it is gained: a tech researched, a policy
//!   adopted, an era entered, a building built, a belief taken (the loops of `research.py:315-319`,
//!   `policies.py:139-143`, `research.py:368-374`, `cities.py:1849-1854`, `religion.py:525-533`);
//! - `starting_triggers`: the global and nation uniques of a new game (`game.py:294-300`), and the
//!   stages S4 and E1, `upon turn start` and `upon turn end` (`turns.py:49, 86`).
//!
//! What differs from Python, on purpose:
//! - a timed unique is granted whatever its conditionals say, which are its effect's: `[+25]%
//!   Strength <when attacking> <for [50] turns>` was never granted by Python, whose check at the
//!   grant asked whether the civilization was attacking (UnCiv lets timed uniques through there),
//!   whether its source was gained or its trigger fired (`unique::trigger::fire`);
//! - `Adopt [belief]` adds the belief to the civilization's religion when it fits the religion's
//!   progress, where Python did nothing;
//! - `Adopt [policy]` adopts the policy whatever it requires, as UnCiv does, where Python refused
//!   one the civilization could not adopt by raising out of the trigger with a free policy
//!   granted;
//! - effects nest [`TRIGGER_DEPTH`] deep at most: a ruleset whose effects feed themselves is
//!   stopped, where Python recursed until it raised and failed whatever caused the first.
//!
//! A few effects reach systems later packages port, and are carried out here with the least of
//! them: the next world leader vote scheduled (1c-08) and the city-states' first great-person
//! gift brought forward (1c-06). A spy recruited or promoted is `espionage`'s. What a one-time
//! effect does to a unit is package 1c-02's `units::health::apply_unit_effect`, and a promotion
//! given free is its `units::promotions::add_promotion`.

use smallvec::SmallVec;

use super::cities::borders::{expand_borders, take_ownership};
use super::cities::construction::complete_construction;
use super::cities::founding::equivalent_building;
use super::cities::free_buildings::{self, add_free};
use super::cities::lifecycle::add_population;
use super::cities::uniques::contains_building;
use super::derive::rev::{PlayerTouch, WorldTouch};
use super::invariants::{Code, Violation};
use super::research::{self, TechSource};
use super::units::{add_unit_in_city, place_unit_near};
use super::{Game, espionage, great_people, policies, religion, units};
use crate::base::ids::{BaseUnitId, CityId, PlayerId, TileIdx, UniqueId, UnitId};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::PlayerSet;
use crate::base::stats::Stat;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::{City, Constructible};
use crate::unique::params::PolicyOrBelief;
#[cfg(feature = "test-ops")]
use crate::unique::trigger::TriggerKind;
use crate::unique::trigger::{CityScope, OneTimeEffect, TriggerEvent, TriggerSite};
use crate::unique::{SourceUniques, UniqueData, UniqueType, applies};

#[cfg(feature = "test-ops")]
std::thread_local! {
    /// What fired on this thread since the last [`take_fired_for_test`]: the tests of the sites
    /// that fire triggers whose effects are not ported yet read it (feature `test-ops`).
    static FIRED: core::cell::RefCell<Vec<(TriggerKind, UniqueId)>> =
        const { core::cell::RefCell::new(Vec::new()) };
}

/// Every unique that fired on this thread since the last call, with the kind of its trigger, in
/// the order they fired (feature `test-ops`): what a test of a site whose effects wait for
/// another package can observe.
#[cfg(feature = "test-ops")]
#[must_use]
pub fn take_fired_for_test() -> Vec<(TriggerKind, UniqueId)> {
    FIRED.with(|f| core::mem::take(&mut *f.borrow_mut()))
}

/// Fires the uniques that wait for `event` at `site` (`triggers.fire`): the civilization's, the
/// city's local ones and its religion's, then, with `include_unit`, the unit's; each is applied
/// in turn. `note` is what caused them, for their announcements (`due to expending our Great
/// Prophet`).
pub fn fire(
    g: &mut Game,
    site: &TriggerSite,
    event: &TriggerEvent,
    include_unit: bool,
    note: Option<&str>,
) {
    for id in find(g, site, event, include_unit) {
        apply(g, id, site, note);
    }
}

/// Applies what a source gives once when it is gained (`research.py:315-319`,
/// `policies.py:139-143`, `research.py:368-374`, `cities.py:1849-1854`, `religion.py:525-533`,
/// `game.py:294-300`): each of its one-time effects without a trigger, its timed uniques and its
/// standing effects that also happen on gain, in the order it lists them, if its conditionals
/// hold at the site when its turn comes.
pub fn on_gain(g: &mut Game, src: &SourceUniques, site: &TriggerSite, note: Option<&str>) {
    for &id in src.on_gain.iter() {
        if holds_at(g, id, site) {
            apply(g, id, site, note);
        }
    }
}

/// The uniques [`fire`] would apply, found and counted as fired but not applied: for a site
/// that changes before they apply, as a unit that is found while it stands and gone when its
/// civilization's effects apply.
#[must_use]
pub fn find(
    g: &Game,
    site: &TriggerSite,
    event: &TriggerEvent,
    include_unit: bool,
) -> SmallVec<[UniqueId; 4]> {
    let found = crate::unique::trigger::fire(&g.view(), site, event, include_unit);
    #[cfg(feature = "test-ops")]
    FIRED.with(|f| f.borrow_mut().extend(found.iter().map(|&id| (event.kind(), id))));
    found
}

/// Whether a unique that happens once holds at `site` now: its conditionals, but for a timed
/// unique, whose conditionals are its effect's and are asked when the effect is.
fn holds_at(g: &Game, id: UniqueId, site: &TriggerSite) -> bool {
    // refcheck: timed-uniques-granted-whatever-their-conditionals
    if g.rules().uniques().meta(id).timed.is_some() {
        return true;
    }
    let v = g.view();
    applies(id, &site.ctx(&v), &v)
}

/// The setup stage `starting triggers` (`game.py:294-300`): each major's global and nation
/// uniques that happen once, applied at its start.
pub(crate) fn starting_triggers(g: &mut Game, p: PlayerId, start: Option<TileIdx>) {
    let r = g.rules();
    let Some(nation) = g.player(p).filter(|x| x.is_major()).map(|x| x.nation) else { return };
    let site = TriggerSite { civ: p, city: None, unit: None, tile: start };
    on_gain(g, r.global_uniques(), &site, None);
    on_gain(g, &r.nations()[nation].uniques, &site, None);
}

/// Stage S4, `upon turn start` (`turns.py:49`).
pub(crate) fn turn_start(g: &mut Game, p: PlayerId) {
    fire(g, &TriggerSite::civ(p), &TriggerEvent::TurnStart, true, None);
}

/// Stage E1, `upon turn end` (`turns.py:86`).
pub(crate) fn turn_end(g: &mut Game, p: PlayerId) {
    fire(g, &TriggerSite::civ(p), &TriggerEvent::TurnEnd, true, None);
}

/// Where a one-time effect happens (`triggers.py:80-85`): the site's tile, else its city's or
/// its unit's; and the site's city, else the civilization's city whose territory the tile is.
fn locate(g: &Game, site: &TriggerSite) -> TriggerSite {
    let mut s = *site;
    if s.tile.is_none() {
        s.tile = s
            .city
            .and_then(|c| g.city(c).map(City::tile))
            .or_else(|| s.unit.and_then(|u| g.unit(u).map(crate::state::units::Unit::tile)));
    }
    if s.city.is_none()
        && let Some(c) = s.tile.and_then(|t| g.tile(t)).and_then(crate::state::map::Tile::city)
        && g.city(c).is_some_and(|x| x.owner() == s.civ)
    {
        s.city = Some(c);
    }
    s
}

/// The civilization's cities an effect reaches (`triggers._cities_for`, `triggers.py:68-72`): the
/// city in context for `[in this city]`, else those the filter selects, by id.
fn cities_for(g: &Game, p: PlayerId, city: Option<CityId>, scope: CityScope) -> Vec<CityId> {
    match scope {
        CityScope::ThisCity => city.into_iter().collect(),
        CityScope::Matching(f) => {
            let v = g.view();
            let filters = g.rules().uniques().filters();
            g.player_cities(p)
                .map(City::id)
                .filter(|&c| filters.city_matches(f, &v, c, None))
                .collect()
        }
    }
}

/// A civilization's name, for announcements.
fn civ_name(g: &Game, p: PlayerId) -> String {
    g.player(p).map(|x| x.name.to_string()).unwrap_or_default()
}

/// How deeply one-time effects may nest: an effect that fires a trigger whose effect fires
/// another, and so on. The chains real rulesets make are a few deep; a ruleset whose effects feed
/// themselves (`Free [Warrior] appears <upon gaining a [Warrior] unit>`) is stopped here, where
/// Python recursed until it raised.
pub const TRIGGER_DEPTH: u8 = 8;

/// Applies the one-time effect of the unique `id` at `site` (`triggers.trigger`,
/// `triggers.py:75-367`): `apply_one_time`. `note` is what its announcement says caused it.
/// Whether anything happened, which ruins read to know a reward was found.
///
/// An effect nested more than [`TRIGGER_DEPTH`] deep inside others is not applied, and is
/// reported as SETTLE-1 where the checks run: a chain of effects that does not end.
pub fn apply(g: &mut Game, id: UniqueId, site: &TriggerSite, note: Option<&str>) -> bool {
    // refcheck: trigger-chains-stop
    if g.trigger_depth >= TRIGGER_DEPTH {
        if g.debug.invariants {
            let text = g.rules().uniques().text_of(id).to_owned();
            g.report(Violation::new(
                Code::Settle1,
                format!("one-time effects nested {TRIGGER_DEPTH} deep; {text:?} was not applied"),
            ));
        }
        return false;
    }
    g.trigger_depth += 1;
    let happened = apply_one_time(g, id, site, note);
    g.trigger_depth -= 1;
    happened
}

/// [`apply`] within the depth allowed.
#[allow(clippy::too_many_lines, reason = "one arm per kind of one-time effect, as Python's")]
fn apply_one_time(g: &mut Game, id: UniqueId, site: &TriggerSite, note: Option<&str>) -> bool {
    let Some(effect) = OneTimeEffect::decode(g.rules(), id) else { return false };
    let p = site.civ;
    if g.player(p).is_none() {
        return false;
    }
    let site = locate(g, site);
    let (city, tile) = (site.city, site.tile);
    let suffix = note.map(|n| format!(" ({n})")).unwrap_or_default();
    let key = g.rules().uniques().meta(id).key;
    let mut rng =
        Rng::keyed(g.state().seed(), Purpose::Trigger, &[key, p.key(), tile.key(), g.turn().key()]);
    let who = civ_name(g, p);
    let told = Some(PlayerSet::single(p));
    match effect {
        OneTimeEffect::Timed { variant, turns } => {
            if let Some(x) = g.player_mut(p, PlayerTouch::INDEX) {
                let turns = i16::try_from(turns).unwrap_or(i16::MAX);
                x.civ
                    .temp_uniques
                    .push(crate::state::players::TempUnique { unique: variant, turns });
            }
            true
        }
        OneTimeEffect::FreeUnits { unit, count, near_tile } => {
            free_units(g, p, &site, unit, count, near_tile, &suffix)
        }
        OneTimeEffect::FreePolicies { count } => {
            if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
                x.policy.free_policies += count;
            }
            let text = if count == 1 {
                format!("{who} may choose a free social policy{suffix}.")
            } else {
                format!("{who} may choose {count} free social policies{suffix}.")
            };
            g.emit(EngineEvent::PolicyAvailable, &text, told, None, EventData::default(), &[]);
            true
        }
        OneTimeEffect::Adopt(PolicyOrBelief::Policy(policy)) => {
            if g.player(p).is_some_and(|x| x.policy.adopted.contains(policy)) {
                return false;
            }
            // A free policy granted and spent at once (`triggers.py:141-143`), whatever it
            // requires.
            // refcheck: adopt-a-policy-whatever-it-requires
            policies::adopt_now(g, p, policy, false);
            true
        }
        OneTimeEffect::Adopt(PolicyOrBelief::Belief(b)) => {
            // refcheck: adopt-a-belief-joins-the-religion
            religion::found::adopt_belief(g, p, b)
        }
        OneTimeEffect::GoldenAge { turns } => {
            great_people::enter_golden_age(g, p, turns);
            true
        }
        OneTimeEffect::FreeGreatPerson => {
            if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
                x.gp.free += 1;
            }
            let text = format!("{who} may choose a free Great Person{suffix}.");
            g.emit(EngineEvent::GreatPersonAvailable, &text, told, None, EventData::default(), &[]);
            if g.player(p).is_some_and(|x| x.seat().auto().free_picks) {
                great_people::ai_choose_free(g, p);
            }
            true
        }
        OneTimeEffect::GainPopulation { count, cities } => {
            let cs = cities_for(g, p, city, cities);
            for &c in &cs {
                add_population(g, c, count);
            }
            !cs.is_empty()
        }
        OneTimeEffect::GainPopulationRandomCity { count } => {
            let cs: Vec<CityId> = g.player_cities(p).map(City::id).collect();
            let Some(&c) = rng.pick(&cs) else { return false };
            add_population(g, c, count);
            let (name, at) = g.city(c).map(|x| (x.name.to_string(), x.tile())).unzip();
            let text = format!(
                "Survivors joined {} (+{count} population){suffix}.",
                name.unwrap_or_default()
            );
            g.emit(EngineEvent::Ruins, &text, told, at, EventData::default(), &[]);
            true
        }
        OneTimeEffect::FreeTechs { count } => {
            if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
                x.tech.free_techs += count;
            }
            let noun = if count == 1 { "technology" } else { "technologies" };
            let text = format!("{who} may choose {count} free {noun}{suffix}.");
            g.emit(EngineEvent::FreeTech, &text, told, None, EventData::default(), &[]);
            if g.player(p).is_some_and(|x| x.seat().auto().free_picks) {
                for _ in 0..count.max(0) {
                    auto_free_tech(g, p);
                }
            }
            true
        }
        OneTimeEffect::DiscoverTech(tech) => {
            if g.has_tech(p, Some(tech)) {
                return false;
            }
            research::add_tech(g, p, tech, TechSource::Free);
            true
        }
        OneTimeEffect::FreeTechsFromEras { count, eras } => {
            let t = g.rules().uniques();
            let mut cands: Vec<_> = g
                .rules()
                .techs()
                .iter()
                .filter(|&(tech, d)| t.in_set(eras, d.era) && research::can_research(g, p, tech))
                .map(|(tech, _)| tech)
                .collect();
            if cands.is_empty() {
                return false;
            }
            rng.shuffle(&mut cands);
            for &tech in cands.iter().take(usize::try_from(count).unwrap_or(0)) {
                research::add_tech(g, p, tech, TechSource::Ruins);
            }
            true
        }
        OneTimeEffect::RevealEntireMap => {
            let all: Vec<TileIdx> = g.grid().tiles().collect();
            g.reveal_tiles(p, &all);
            true
        }
        OneTimeEffect::FreeBelief(kind) => {
            if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
                let n = x.religion.free(kind);
                x.religion.set_free(kind, n.saturating_add(1));
            }
            true
        }
        OneTimeEffect::TriggerVoting => {
            schedule_vote(g);
            true
        }
        OneTimeEffect::GainStat { stat, min, max, speed } => {
            if matches!(stat, Stat::Food | Stat::Production) {
                return false;
            }
            let drawn = if min == max { i64::from(min) } else { rng.range(min.into(), max.into()) };
            #[allow(clippy::cast_precision_loss, reason = "a unique's amount is small")]
            let mut amount = drawn as f64;
            if speed {
                amount = num::round_half_even(amount * stat_speed(g, stat));
            }
            g.add_stat(p, stat, amount);
            let n = num::trunc_i64(amount);
            let text = format!("{who} gained {n} {}{suffix}.", stat.name());
            g.emit(EngineEvent::Gain, &text, told, tile, EventData::default(), &[]);
            true
        }
        OneTimeEffect::GainPantheon => {
            let none = g
                .player(p)
                .is_some_and(|x| x.religion.progress == crate::rules::defs::ReligionProgress::None);
            if !none {
                return false;
            }
            let amount = religion::prophets::faith_for_pantheon(g, 2);
            gain_faith(g, p, amount, tile, &suffix)
        }
        OneTimeEffect::GainProphet { percent } => {
            if religion::prophets::prophet_unit(g, p).is_none() {
                return false;
            }
            let cost = f64::from(religion::prophets::faith_for_next_prophet(g, p));
            let amount = num::trunc_i32(cost * f64::from(percent) / 100.0);
            gain_faith(g, p, amount, tile, &suffix)
        }
        OneTimeEffect::GainTechPercent { percent, tech } => {
            if g.has_tech(p, Some(tech)) {
                return false;
            }
            let cost = f64::from(research::tech_cost(g, p, tech));
            let add = num::round_half_even(cost * f64::from(percent) / 100.0);
            if let Some(x) = g.player_mut(p, PlayerTouch::RESEARCH) {
                *x.tech.progress.entry(tech).or_insert(0.0) += add;
            }
            true
        }
        OneTimeEffect::TakeOverTilesInRadius { tiles, radius } => {
            take_over_tiles(g, p, tile, tiles, radius)
        }
        OneTimeEffect::TakeOverTilesInCity { count, cities } => {
            for c in cities_for(g, p, city, cities) {
                for _ in 0..count.max(0) {
                    if expand_borders(g, c).is_none() {
                        break;
                    }
                }
            }
            true
        }
        OneTimeEffect::RevealTiles { count, tiles, radius } => {
            let Some(t) = tile else { return false };
            let explored = g.player(p).map(|x| x.explored.clone()).unwrap_or_default();
            let mut cands: Vec<TileIdx> = {
                let v = g.view();
                let filters = g.rules().uniques().filters();
                g.grid()
                    .within(t, u32::try_from(radius).unwrap_or(0))
                    .into_iter()
                    .filter(|&i| {
                        !explored.contains(i.0) && filters.tile_matches(tiles, &v, i, None)
                    })
                    .collect()
            };
            if cands.is_empty() {
                return false;
            }
            if let Some(n) = count.get() {
                rng.shuffle(&mut cands);
                cands.truncate(usize::try_from(n).unwrap_or(0));
            }
            g.reveal_tiles(p, &cands);
            let text = format!("{who} revealed {} tiles{suffix}.", cands.len());
            g.emit(EngineEvent::Ruins, &text, told, Some(t), EventData::default(), &[]);
            true
        }
        OneTimeEffect::RevealCrudeMap { distance, radius, percent } => {
            let Some(t) = tile else { return false };
            let explored = g.player(p).map(|x| x.explored.clone()).unwrap_or_default();
            let ring: Vec<TileIdx> = g
                .grid()
                .ring(t, u32::try_from(distance).unwrap_or(0))
                .into_iter()
                .filter(|&i| !explored.contains(i.0))
                .collect();
            let Some(&centre) = rng.pick(&ring) else { return false };
            let chance = f64::from(percent) / 100.0;
            let shown: Vec<TileIdx> = g
                .grid()
                .within(centre, u32::try_from(radius).unwrap_or(0))
                .into_iter()
                .filter(|_| rng.unit() < chance)
                .collect();
            g.reveal_tiles(p, &shown);
            true
        }
        OneTimeEffect::GlobalSpiesWhenEnteringEra => {
            if !espionage::spies_play(g, p) {
                return false;
            }
            let era = super::derive::civ::era(g, p);
            let majors: Vec<PlayerId> =
                g.majors(true).map(crate::state::players::Player::id).collect();
            for q in majors {
                let earned = g
                    .player(q)
                    .and_then(|x| x.major.as_deref())
                    .is_some_and(|m| m.spy_eras_earned.contains(era));
                if !earned {
                    if let Some(m) =
                        g.player_mut(q, PlayerTouch::SPIES).and_then(|x| x.major.as_deref_mut())
                    {
                        m.spy_eras_earned.insert(era);
                    }
                    espionage::add_spy(g, q);
                }
            }
            true
        }
        OneTimeEffect::SpiesLevelUp { times } => {
            if !espionage::spies_play(g, p) {
                return false;
            }
            let n = espionage::spies(g, p).len();
            for i in 0..n {
                espionage::level_up(g, p, i, times);
            }
            true
        }
        OneTimeEffect::GainSpy => {
            if !espionage::spies_play(g, p) {
                return false;
            }
            espionage::add_spy(g, p);
            true
        }
        OneTimeEffect::FreeBuilding { building, cities } => {
            let b = equivalent_building(g, p, building);
            let cs = cities_for(g, p, city, cities);
            for &c in &cs {
                add_free(g, c, b);
                if !contains_building(g, c, b) {
                    complete_construction(g, c, Constructible::Building(b), None);
                }
            }
            !cs.is_empty()
        }
        OneTimeEffect::FreeStatBuildings { stat, cities } => {
            free_buildings::add_free_stat_buildings(g, p, stat, cities);
            true
        }
        OneTimeEffect::FreeSpecificBuildings { building, cities } => {
            free_buildings::add_free_specific_buildings(g, p, building, cities);
            true
        }
        OneTimeEffect::PromoteUnits { units, promotion } => {
            let r = g.rules();
            let types = &r.promotions()[promotion].unit_types;
            let chosen: Vec<UnitId> = {
                let v = g.view();
                let filters = r.uniques().filters();
                g.player_units(p)
                    .filter(|x| {
                        filters.unit_matches(units, &v, x.id(), Default::default())
                            && (types.is_empty()
                                || types.contains(&r.base_units()[x.base].unit_type))
                            && !x.promotions.contains(promotion)
                    })
                    .map(crate::state::units::Unit::id)
                    .collect()
            };
            for &u in &chosen {
                units::promotions::add_promotion(g, u, promotion, true);
            }
            !chosen.is_empty()
        }
        OneTimeEffect::CityStateGreatPersonGift => {
            let gift = turns_for_gp_gift(g, p) / 2;
            let Some(m) = g.player_mut(p, PlayerTouch::OTHER).and_then(|x| x.major.as_deref_mut())
            else {
                return false;
            };
            m.cs_gp_gift = Some(i16::try_from(gift).unwrap_or(i16::MAX));
            true
        }
        OneTimeEffect::Unit(e) => {
            site.unit.is_some_and(|u| units::health::apply_unit_effect(g, u, e, note))
        }
    }
}

/// `Free [unit] appears`, `[n] free [unit] units appear` and `Free [unit] found in the ruins`
/// (`triggers.py:99-129`): the civilization's own unit of the kind, as many as its limit leaves
/// room for (none of a city-founder for a city-state), in the city in context or the capital,
/// near the tile in context (the ruins' always), or near its first unit.
fn free_units(
    g: &mut Game,
    p: PlayerId,
    site: &TriggerSite,
    unit: BaseUnitId,
    count: i32,
    near_tile: bool,
    suffix: &str,
) -> bool {
    let r = g.rules();
    let t = r.uniques();
    let name = super::cities::construction::equivalent_unit(g, p, unit);
    let def = &r.base_units()[name];
    if g.is_city_state(p)
        && super::cities::construction::unit_has_type(r, name, UniqueType::FoundCity)
    {
        return false;
    }
    let limits: Vec<i32> = def
        .uniques
        .ids()
        .filter_map(|id| match t.get(id).data {
            UniqueData::MaxNumberBuildable(x) => Some(x.limit),
            _ => None,
        })
        .collect();
    let mut count = count;
    if let Some(&least) = limits.iter().min() {
        let have =
            i32::try_from(g.player_units(p).filter(|x| x.base == name).count()).unwrap_or(i32::MAX);
        count = count.min(least - have);
    }
    let capital = g.player(p).and_then(|x| x.capital).filter(|&c| g.city(c).is_some());
    let chosen = site.city.or(capital);
    let mut placed = 0;
    for _ in 0..count.max(0) {
        let made = if near_tile {
            let at = site.tile.or_else(|| chosen.and_then(|c| g.city(c).map(City::tile)));
            at.and_then(|at| place_unit_near(g, p, name, at))
        } else if let Some(c) = chosen.filter(|_| site.city.is_some() || site.tile.is_none()) {
            add_unit_in_city(g, c, name)
        } else if let Some(at) = site.tile {
            place_unit_near(g, p, name, at)
        } else {
            let first = g.player_units(p).next().map(crate::state::units::Unit::tile);
            first.and_then(|at| place_unit_near(g, p, name, at))
        };
        if made.is_some() {
            placed += 1;
        }
    }
    if placed > 0 {
        let plural = if placed > 1 { "s" } else { "" };
        let text = format!("{} gained {placed} {}{plural}{suffix}.", civ_name(g, p), def.name);
        let told = Some(PlayerSet::single(p));
        g.emit(EngineEvent::FreeUnit, &text, told, site.tile, EventData::default(), &[]);
    }
    placed > 0
}

/// A free tech a civilization that lets the engine pick takes: the cheapest it could research
/// (`triggers.py:173-178`).
fn auto_free_tech(g: &mut Game, p: PlayerId) {
    let best = research::available_techs(g, p)
        .into_iter()
        .map(|t| (research::tech_cost(g, p, t), t))
        .fold(None, |best: Option<(i32, _)>, x| match best {
            Some(b) if b.0 <= x.0 => Some(b),
            _ => Some(x),
        });
    let Some((_, tech)) = best else { return };
    if research::plan_free_tech(g, p, Some(tech), "").is_err() {
        return;
    }
    if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
        x.tech.free_techs -= 1;
    }
    research::add_tech(g, p, tech, TechSource::Free);
}

/// Faith for a civilization, announced (`triggers.py:218-237`); nothing for none.
fn gain_faith(g: &mut Game, p: PlayerId, amount: i32, at: Option<TileIdx>, suffix: &str) -> bool {
    if amount <= 0 {
        return false;
    }
    if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
        x.econ.faith += f64::from(amount);
    }
    let text = format!("{} gained {amount} faith{suffix}.", civ_name(g, p));
    g.emit(EngineEvent::Faith, &text, Some(PlayerSet::single(p)), at, EventData::default(), &[]);
    true
}

/// How a gain of a stat scales with the game's speed (`triggers._stat_speed`,
/// `triggers.py:370-374`).
fn stat_speed(g: &Game, stat: Stat) -> f64 {
    let s = g.speed();
    match stat {
        Stat::Gold => s.gold_cost_modifier,
        Stat::Culture => s.culture_cost_modifier,
        Stat::Faith => s.faith_cost_modifier,
        Stat::Science => s.science_cost_modifier,
        Stat::Production => s.production_cost_modifier,
        Stat::Food | Stat::Happiness => s.modifier,
    }
}

/// `Gain control over [tiles] tiles in a [n]-tile radius` (`triggers.py:269-288`): the tiles
/// around the tile in context with no city on them that pass the filter and are not the
/// civilization's go to its city nearest the tile (one of its cities next to them first, a
/// razing city counted five tiles further); a city-state that loses one thinks less of it.
fn take_over_tiles(
    g: &mut Game,
    p: PlayerId,
    tile: Option<TileIdx>,
    filter: crate::base::ids::TileFilterId,
    radius: i32,
) -> bool {
    let Some(t) = tile else { return false };
    if g.player_cities(p).next().is_none() {
        return false;
    }
    let tiles: Vec<TileIdx> = {
        let v = g.view();
        let filters = g.rules().uniques().filters();
        g.grid()
            .within(t, u32::try_from(radius).unwrap_or(0))
            .into_iter()
            .filter(|&i| {
                g.city_at(i).is_none()
                    && filters.tile_matches(filter, &v, i, None)
                    && g.tile(i).and_then(crate::state::map::Tile::owner) != Some(p)
            })
            .collect()
    };
    if tiles.is_empty() {
        return false;
    }
    let mut near: Vec<CityId> = Vec::new();
    for &i in &tiles {
        for n in core::iter::once(i).chain(g.grid().neighbors(i)) {
            let Some(x) = g.tile(n) else { continue };
            if x.owner() == Some(p)
                && let Some(c) = x.city().filter(|&c| g.city(c).is_some())
            {
                near.push(c);
            }
        }
    }
    let pool: Vec<CityId> =
        if near.is_empty() { g.player_cities(p).map(City::id).collect() } else { near };
    let score = |c: CityId| {
        g.city(c).map_or(u32::MAX, |x| {
            g.grid().distance(x.tile(), t).saturating_add(if x.razing { 5 } else { 0 })
        })
    };
    let Some(target) = pool.iter().copied().fold(None, |best: Option<CityId>, c| match best {
        Some(b) if score(b) <= score(c) => Some(b),
        _ => Some(c),
    }) else {
        return false;
    };
    for i in tiles {
        let other = g.tile(i).and_then(crate::state::map::Tile::owner);
        if let Some(cs) = other.filter(|&o| g.is_city_state(o)) {
            let refused = super::city_states::influence::add_influence(g, cs, p, -15.0);
            debug_assert!(refused.is_ok(), "influence refused: {refused:?}");
        }
        take_ownership(g, target, i);
    }
    true
}

/// The next world leader vote is held in fifteen turns, scaled by speed
/// (`victory.schedule_vote`, `victory.py:107-117`).
fn schedule_vote(g: &mut Game) {
    let turn = g.turn().saturating_add(num::trunc_i32(15.0 * g.speed().modifier));
    let un = &mut g.edit_world(WorldTouch::UN).un;
    un.next_vote = Some(turn);
    un.votes.clear();
    g.emit(
        EngineEvent::UnVote,
        &format!("The United Nations will hold a vote for world leader on turn {turn}."),
        None,
        None,
        EventData::default(),
        &[],
    );
}

/// How long until a city-state's great-person gift (`city_states.turns_for_gp_gift`,
/// `city_states.py:354-357`): 37 to 43 turns, scaled by speed.
fn turns_for_gp_gift(g: &Game, major: PlayerId) -> i32 {
    let mut rng = Rng::keyed(g.state().seed(), Purpose::CsGp, &[major.key(), g.turn().key()]);
    num::trunc_i32(f64::from(37 + i32::try_from(rng.below(7)).unwrap_or(0)) * g.speed().modifier)
}

//! What happens to city-states over time and as the world acts on them (`city_states.py:34-62,
//! 250-466, 710-765`): setup, the end of a city-state's turn (influence drifting to its resting
//! point, countdowns, unit gifts, border tension, free techs, quests, elections), the great
//! people allies give, first contact, and the reactions to attacks, conquests and kills.
//!
//! Draws: the setup from `Purpose::CsInit` keyed by the city-state; a unit gift's countdown from
//! `Purpose::CsUnit` and the unit from `Purpose::CsGiftUnit`, keyed by the city-state, the major
//! and the turn; the great person gift from `Purpose::CsGp` and `Purpose::CsGpGiver`, keyed by
//! the major and the turn; an attack's wariness from `Purpose::CsAttacked`, keyed by the
//! city-state, the attacker and the turn.

use super::actions::refused;
use super::influence::{
    add_influence, data, data_mut, is_aggressor, is_warmonger, pair, pair_mut, raw_influence,
    relationship, resting_point, set_influence,
};
use super::quests::{quest_event, quests_end_turn};
use crate::base::ids::{BaseUnitId, NationId, PlayerId, StatsId, TileIdx, UnitId};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::{NationSet, PlayerSet};
use crate::base::stats::Stat;
use crate::game::derive::rev::{PlayerTouch, UnitTouch};
use crate::game::diplomacy::relations::{add_opinion, civ_has, name};
use crate::game::units::{add_construction_bonuses, place_unit_near, type_has};
use crate::game::{Game, research};
use crate::rules::defs::{CityStatePersonality, Domain};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::Constructible;
use crate::state::diplo::OpinionKey;
use crate::state::players::{Player, WarQuest};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

// ---- Setup (city_states.py:37-61) ------------------------------------------------------------------

/// Sets a city-state up for a new game (`init_city_state`, `city_states.py:37-61`): its
/// personality (its nation's if that names one, else drawn), its unique luxury where its type
/// provides one (a resource only city-states have), and the unit it gifts where its type gifts
/// units (a land unit unique to a major nation no seat took, of the starting era or later).
pub fn init_city_state(g: &mut Game, cs: PlayerId, used_majors: &NationSet) {
    let r = g.rules();
    let Some((nation, cs_type)) =
        g.player(cs).and_then(|p| Some((p.nation, p.city_state.as_deref()?.cs_type)))
    else {
        return;
    };
    let mut rng = Rng::keyed(g.state().seed(), Purpose::CsInit, &[cs.key()]);
    let t = r.uniques();
    let has = |ty: UniqueType| {
        cs_type.is_some_and(|ct| {
            let def = &r.city_state_types()[ct];
            def.friend.ids().chain(def.ally.ids()).any(|id| t.meta(id).ty == Some(ty))
        })
    };
    let named = r.nations()[nation]
        .personality
        .and_then(|p| CityStatePersonality::from_name(&r.personalities()[p].name));
    let personality = named.or_else(|| rng.pick(&CityStatePersonality::ALL).copied());
    let mut resource = None;
    if has(UniqueType::CityStateUniqueLuxury) {
        let merc: Vec<_> = r
            .resources()
            .iter()
            .filter(|(_, d)| {
                crate::game::core::has_type(r, &d.uniques, UniqueType::CityStateOnlyResource)
            })
            .map(|(id, _)| id)
            .collect();
        resource = rng.pick(&merc).copied();
    }
    let mut unique_unit = None;
    if has(UniqueType::CityStateMilitaryUnits) {
        let era = g.state().config().starting_era;
        let cands: Vec<BaseUnitId> = r
            .base_units()
            .iter()
            .filter(|(_, u)| {
                u.unique_to.is_some_and(|n: NationId| {
                    r.nations()[n].kind == crate::rules::defs::NationKind::Major
                        && !used_majors.contains(n)
                }) && u.domain == Domain::Land
                    && u.military
                    && u.era >= era
            })
            .map(|(id, _)| id)
            .collect();
        unique_unit = rng.pick(&cands).copied();
    }
    if let Some(d) = data_mut(g, cs) {
        d.personality = personality;
        d.resource = resource;
        d.unique_unit = unique_unit;
    }
}

// ---- The end of a city-state's turn (city_states.py:253-416) -----------------------------------

/// Stage E3: a city-state's own end of turn (`end_turn`, `city_states.py:253-290`): with each
/// major it has met, influence drifts toward the resting point (the major told if it stops being
/// a friend), countdowns run and a unit may be gifted; then border tension, free techs, quests,
/// the war quests of wars that ended, and an election where espionage is on.
pub fn end_turn(g: &mut Game, cs: PlayerId) {
    if !g.player(cs).is_some_and(|p| p.alive() && p.is_city_state()) {
        return;
    }
    let majors: Vec<PlayerId> = g.majors(true).map(Player::id).collect();
    for q in majors {
        if !g.has_met(cs, q) {
            continue;
        }
        let before = relationship(g, cs, q);
        let rp = resting_point(g, cs, q);
        let cur = raw_influence(g, cs, q);
        if cur > rp {
            let to = rp.max(cur - super::influence::degrade(g, cs, q));
            refused(set_influence(g, cs, q, to));
        } else if cur < rp {
            let to = rp.min(cur + super::influence::recovery(g, cs, q));
            refused(set_influence(g, cs, q, to));
        }
        let after = relationship(g, cs, q);
        if before.friendly() && !after.friendly() {
            let text = format!("Your relationship with {} degraded.", name(g, cs));
            let data = EventData { player: Some(cs), ..EventData::default() };
            let audience = Some(PlayerSet::single(q));
            g.emit(EngineEvent::CsRelationship, &text, audience, None, data, &[]);
        }
        if let Some(p) = pair_mut(g, cs, q) {
            for c in [
                &mut p.bullied,
                &mut p.pledged,
                &mut p.withdrew,
                &mut p.border_conflict,
                &mut p.anger_free,
                &mut p.recently_attacked,
                &mut p.marriage_cooldown,
            ] {
                if *c > 0 {
                    *c -= 1;
                }
            }
        }
        military_unit_gift(g, cs, q);
    }
    update_border_intrusion(g, cs);
    free_techs(g, cs);
    quests_end_turn(g, cs);
    let ended: Vec<PlayerId> = data(g, cs)
        .map(|d| {
            d.war_quests
                .keys()
                .copied()
                .filter(|&k| !g.at_war(cs, k) || !g.player(k).is_some_and(Player::alive))
                .collect()
        })
        .unwrap_or_default();
    if let Some(d) = data_mut(g, cs) {
        for k in ended {
            d.war_quests.remove(&k);
        }
        if d.recently_bullied > 0 {
            d.recently_bullied -= 1;
        }
    }
    if g.espionage_enabled() {
        crate::game::espionage::city_state_election_tick(g, cs);
    }
}

/// Stage E3's row for a city-state.
pub(crate) fn end_turn_stage(g: &mut Game, cs: PlayerId) {
    end_turn(g, cs);
}

/// Whether a major and a city-state are at war with a common civilization: the barbarians, at
/// war with everyone, are no such civilization.
pub(crate) fn common_enemy(g: &Game, major: PlayerId, cs: PlayerId) -> bool {
    // refcheck: city-state-gifts-need-a-common-nation
    g.state()
        .players()
        .iter()
        .any(|(e, p)| p.alive() && !p.is_barbarian() && g.at_war(major, e) && g.at_war(cs, e))
}

/// A friend or ally may be gifted a military unit (`_military_unit_gift`,
/// `city_states.py:293-317`): its type's `Provides military units every ≈[n] turns` for that
/// level sets a countdown of n, give or take a turn, which runs faster by the major's
/// `Militaristic City-States grant units [n] times as fast ...` while they fight a common enemy.
fn military_unit_gift(g: &mut Game, cs: PlayerId, major: PlayerId) {
    let lvl = relationship(g, cs, major);
    let clear = |g: &mut Game| {
        if let Some(p) = pair_mut(g, cs, major) {
            p.unit_timer = None;
        }
    };
    if !lvl.friendly() {
        clear(g);
        return;
    }
    let Some(ct) = data(g, cs).and_then(|d| d.cs_type) else {
        clear(g);
        return;
    };
    let turns: Vec<i32> = {
        let r = g.rules();
        let def = &r.city_state_types()[ct];
        let uniques =
            if lvl == super::influence::Relationship::Ally { &def.ally } else { &def.friend };
        let v = g.view();
        uq::object(&v, uniques, UniqueType::CityStateMilitaryUnits, &Ctx::civ(major))
            .filter_map(|h| match *h.data() {
                UniqueData::CityStateMilitaryUnits(x) => Some(x.turns),
                _ => None,
            })
            .collect()
    };
    if turns.is_empty() {
        clear(g);
        return;
    }
    let mut rng =
        Rng::keyed(g.state().seed(), Purpose::CsUnit, &[cs.key(), major.key(), g.turn().key()]);
    let mut timer = pair(g, cs, major).unit_timer.map(i32::from);
    for n in turns {
        if timer.is_none_or(|t| t > n) {
            let jitter = [-1, 0, 1][usize::try_from(rng.below(3)).unwrap_or(1)];
            timer = Some(n + jitter);
        }
    }
    let mut left = timer.unwrap_or(0) - 1;
    if common_enemy(g, major, cs) {
        let v = g.view();
        for h in uq::civ(&v, major, UniqueType::CityStateMoreGiftedUnits, &Ctx::civ(major)) {
            if let UniqueData::CityStateMoreGiftedUnits(x) = *h.data() {
                for _ in 0..h.n {
                    left -= x.times - 1;
                }
            }
        }
    }
    if left <= 0 {
        clear(g);
        give_military_unit(g, cs, major);
    } else if let Some(p) = pair_mut(g, cs, major) {
        p.unit_timer = Some(i16::try_from(left).unwrap_or(i16::MAX));
    }
}

/// A city-state gives a major a military unit near the major's city nearest its capital
/// (`give_military_unit`, `city_states.py:320-351`): its unique unit once the major knows its
/// tech and while not obsolete, else a land unit the major's city could build now and has the
/// resource for; with the capital's training bonuses and the major's `Military Units gifted from
/// City-States start with [n] XP`.
pub fn give_military_unit(g: &mut Game, cs: PlayerId, major: PlayerId) -> Option<UnitId> {
    let cap =
        g.player(cs).and_then(|p| p.capital).and_then(|c| g.city(c)).map(|c| (c.id(), c.tile()))?;
    let target = g
        .player_cities(major)
        .min_by_key(|c| g.grid().distance(c.tile(), cap.1))
        .map(|c| (c.id(), c.tile(), c.name.clone()))?;
    let r = g.rules();
    let uu = data(g, cs).and_then(|d| d.unique_unit).filter(|&u| {
        let d = &r.base_units()[u];
        g.has_tech(major, d.required_tech)
            && !d.obsolete_tech.is_some_and(|t| g.has_tech(major, Some(t)))
    });
    let unit = match uu {
        Some(u) => u,
        None => {
            let mut rng = Rng::keyed(
                g.state().seed(),
                Purpose::CsGiftUnit,
                &[cs.key(), major.key(), g.turn().key()],
            );
            let cands: Vec<BaseUnitId> = r
                .base_units()
                .iter()
                .filter(|&(u, d)| {
                    d.military
                        && d.domain == Domain::Land
                        && d.unique_to.is_none()
                        && crate::game::cities::construction::rejection_kinds(
                            g,
                            target.0,
                            Constructible::Unit(u),
                        )
                        .is_empty()
                        && d.required_resource.is_none_or(|res| {
                            crate::game::economy::resource_amount(g, major, res) > 0
                        })
                })
                .map(|(u, _)| u)
                .collect();
            *rng.pick(&cands)?
        }
    };
    let u = place_unit_near(g, major, unit, target.1)?;
    add_construction_bonuses(g, u, cap.0);
    let xp = {
        let v = g.view();
        uq::sum_i32(
            uq::civ(&v, major, UniqueType::CityStateGiftedUnitsStartWithXp, &Ctx::civ(major)),
            |d| match d {
                UniqueData::CityStateGiftedUnitsStartWithXp(x) => Some(x.xp),
                _ => None,
            },
        )
    };
    if xp != 0
        && let Some(x) = g.unit_mut(u, UnitTouch::CORE)
    {
        x.xp = x.xp.saturating_add(xp);
    }
    let at = g.unit(u).map(crate::state::units::Unit::tile);
    let text = format!(
        "{} gave you a {} near {}!",
        name(g, cs),
        g.rules().base_units()[unit].name,
        target.2
    );
    let data = EventData { unit: Some(u), ..EventData::default() };
    g.emit(EngineEvent::CsGift, &text, Some(PlayerSet::single(major)), at, data, &[]);
    Some(u)
}

/// How long until the allied city-states' next great person (`turns_for_gp_gift`,
/// `city_states.py:354-357`): 37 to 43 turns, scaled by the speed.
#[must_use]
pub fn turns_for_gp_gift(g: &Game, major: PlayerId) -> i32 {
    let mut rng = Rng::keyed(g.state().seed(), Purpose::CsGp, &[major.key(), g.turn().key()]);
    num::trunc_i32(f64::from(37 + i32::try_from(rng.below(7)).unwrap_or(0)) * g.speed().modifier)
}

/// Stage S3: allied city-states now and then give a great person to a civilization that has
/// Patronage's finisher (`great_person_gift_tick`, `city_states.py:360-380`): the countdown runs
/// while it has allies, and when it falls below their number (ten at most) one of them gives a
/// great person that founds no religion and belongs to no nation, near the major's city nearest
/// its capital.
pub fn great_person_gift_tick(g: &mut Game, major: PlayerId) {
    let Some(mut left) =
        g.player(major).and_then(|p| p.major.as_deref()).and_then(|m| m.cs_gp_gift)
    else {
        return;
    };
    let allies = crate::game::economy::allied_city_states(g, major);
    if allies.is_empty() {
        return;
    }
    left -= 1;
    let set = |g: &mut Game, v: i32| {
        if let Some(m) =
            g.player_mut(major, PlayerTouch::OTHER).and_then(|p| p.major.as_deref_mut())
        {
            m.cs_gp_gift = Some(i16::try_from(v).unwrap_or(i16::MAX));
        }
    };
    let enough = usize::try_from(left).is_ok_and(|l| l < allies.len().min(10)) || left < 0;
    if !enough || g.player_cities(major).next().is_none() {
        set(g, i32::from(left));
        return;
    }
    let mut rng = Rng::keyed(g.state().seed(), Purpose::CsGpGiver, &[major.key(), g.turn().key()]);
    let giver = rng.pick(&allies).copied();
    let r = g.rules();
    let mut gps: Vec<BaseUnitId> = r
        .derived()
        .great_person_units
        .iter()
        .copied()
        .filter(|&u| {
            !type_has(g, u, UniqueType::MayFoundReligion) && r.base_units()[u].unique_to.is_none()
        })
        .collect();
    gps.sort();
    if let Some(giver) = giver
        && !gps.is_empty()
    {
        let cap = g.player(giver).and_then(|p| p.capital).and_then(|c| g.city(c)).map(|c| c.tile());
        let target = match cap {
            Some(t) => g.player_cities(major).min_by_key(|c| g.grid().distance(c.tile(), t)),
            None => g.player_cities(major).next(),
        }
        .map(crate::state::cities::City::tile);
        let gp = rng.pick(&gps).copied();
        if let (Some(t), Some(gp)) = (target, gp)
            && place_unit_near(g, major, gp, t).is_some()
        {
            let text = format!(
                "{} gave you a {} as a gift!",
                name(g, giver),
                g.rules().base_units()[gp].name
            );
            g.emit(
                EngineEvent::CsGift,
                &text,
                Some(PlayerSet::single(major)),
                Some(t),
                EventData::default(),
                &[],
            );
        }
    }
    let next = turns_for_gp_gift(g, major);
    set(g, next);
}

/// Stage S3's row for a major.
pub(crate) fn great_person_gift_stage(g: &mut Game, major: PlayerId) {
    great_person_gift_tick(g, major);
}

/// Military units loitering in a city-state's land anger it (`_update_border_intrusion`,
/// `city_states.py:383-401`): 10 influence a turn from a major at peace with it that is no friend,
/// unless barbarians threaten it, the major's `City-State territory always counts as friendly
/// territory`, or it recently killed barbarians for it.
fn update_border_intrusion(g: &mut Game, cs: PlayerId) {
    if threatening_barbarians(g, cs) > 0 {
        return;
    }
    let majors: Vec<PlayerId> = g.majors(true).map(Player::id).collect();
    let r = g.rules();
    for q in majors {
        if !g.has_met(cs, q) || g.at_war(cs, q) {
            continue;
        }
        if civ_has(g, q, UniqueType::CityStateTerritoryAlwaysFriendly) {
            continue;
        }
        if pair(g, cs, q).anger_free > 0 {
            continue;
        }
        let inside = g.player_units(q).any(|u| {
            r.base_units()[u.base].military
                && g.tile(u.tile()).and_then(crate::state::map::Tile::owner) == Some(cs)
        });
        if inside && !relationship(g, cs, q).friendly() {
            refused(add_influence(g, cs, q, -10.0));
            if pair(g, cs, q).border_conflict == 0 {
                if let Some(p) = pair_mut(g, cs, q) {
                    p.border_conflict = 10;
                }
                let text = format!(
                    "{} is angered by your military units in its territory (-10 influence).",
                    name(g, cs)
                );
                let data = EventData { player: Some(cs), ..EventData::default() };
                g.emit(EngineEvent::CsBorder, &text, Some(PlayerSet::single(q)), None, data, &[]);
            }
        }
    }
}

/// A city-state learns the techs most majors know (`_free_techs`, `city_states.py:404-416`), in
/// the tech order: it does no research of its own.
fn free_techs(g: &mut Game, cs: PlayerId) {
    let r = g.rules();
    let majors: Vec<PlayerId> = g.majors(true).map(Player::id).collect();
    for &t in &r.derived().tech_order {
        if research::is_repeatable(r, t) || !research::can_research(g, cs, t) {
            continue;
        }
        let knowing = majors.iter().filter(|&&m| g.has_tech(m, Some(t))).count();
        if knowing * 2 > majors.len() {
            research::add_tech_silently(g, cs, &[t]);
        }
    }
}

/// How many barbarian military units are within 6 tiles of a city-state's capital
/// (`threatening_barbarians`, `city_states.py:419-428`).
#[must_use]
pub fn threatening_barbarians(g: &Game, cs: PlayerId) -> usize {
    let Some(bid) = g.barbarian_id() else { return 0 };
    let Some(cap) = g.player(cs).and_then(|p| p.capital).and_then(|c| g.city(c)) else {
        return 0;
    };
    let r = g.rules();
    g.player_units(bid)
        .filter(|u| r.base_units()[u.base].military && g.grid().distance(u.tile(), cap.tile()) <= 6)
        .count()
}

/// A major that kills a barbarian near a city-state it knows and is at peace with earns 12
/// influence, and five turns of forgiveness for its units in the city-state's land
/// (`barbarian_killed_near`, `city_states.py:431-442`).
pub fn barbarian_killed_near(g: &mut Game, major: PlayerId, t: TileIdx) {
    let css: Vec<(PlayerId, TileIdx)> = g
        .city_states(true)
        .filter_map(|q| Some((q.id(), q.capital.and_then(|c| g.city(c))?.tile())))
        .collect();
    for (q, cap) in css {
        if !g.has_met(major, q) || g.at_war(major, q) || g.grid().distance(cap, t) > 6 {
            continue;
        }
        refused(add_influence(g, q, major, 12.0));
        if let Some(p) = pair_mut(g, q, major) {
            p.anger_free = p.anger_free.saturating_add(5);
        }
        let text = format!(
            "{} is grateful that you killed a barbarian threatening them (+12 influence).",
            name(g, q)
        );
        let data = EventData { player: Some(q), ..EventData::default() };
        g.emit(EngineEvent::CsGrateful, &text, Some(PlayerSet::single(major)), None, data, &[]);
    }
}

/// A stats parameter of a unique, where it has one first (Python's `Unique.stats`).
fn stats_param(d: &UniqueData) -> Option<StatsId> {
    match *d {
        UniqueData::Stats(x) => Some(x.stats),
        UniqueData::StatsPerCity(x) => Some(x.stats),
        UniqueData::StatsFromSpecialist(x) => Some(x.stats),
        UniqueData::StatsPerPopulation(x) => Some(x.stats),
        UniqueData::StatsPerPolicies(x) => Some(x.stats),
        UniqueData::StatsFromCitiesOnSpecificTiles(x) => Some(x.stats),
        UniqueData::StatsFromBuildings(x) => Some(x.stats),
        UniqueData::StatsFromTiles(x) => Some(x.stats),
        UniqueData::StatsFromTilesWithout(x) => Some(x.stats),
        UniqueData::StatsFromObject(x) => Some(x.stats),
        UniqueData::StatsFromTradeRoute(x) => Some(x.stats),
        UniqueData::StatsFromGlobalCitiesFollowingReligion(x) => Some(x.stats),
        UniqueData::StatsFromGlobalFollowers(x) => Some(x.stats),
        _ => None,
    }
}

/// Whether a city-state's type gives its ally a stat (`_provides_stat`, `city_states.py:462-465`).
fn provides_stat(g: &Game, cs: PlayerId, stat: Stat) -> bool {
    let Some(ct) = data(g, cs).and_then(|d| d.cs_type) else { return false };
    let r = g.rules();
    let t = r.uniques();
    r.city_state_types()[ct]
        .ally
        .ids()
        .filter_map(|id| stats_param(&t.get(id).data))
        .any(|s| t.stats(s)[stat] > 0.0)
}

/// First contact with a city-state (`on_meet`, `city_states.py:445-459`): unless the major has
/// attacked city-states, it gives 30 gold to the first major it meets and 15 to the others, and
/// 4 faith more if its type gives faith to its ally.
pub fn on_meet(g: &mut Game, a: PlayerId, b: PlayerId) {
    for (cs, major) in [(a, b), (b, a)] {
        if !g.is_city_state(cs) || !g.player(major).is_some_and(Player::is_major) {
            continue;
        }
        if is_aggressor(g, major) {
            continue;
        }
        let met = g.majors(false).filter(|q| g.has_met(cs, q.id()) && q.id() != cs).count();
        let gold = if met == 1 { 30 } else { 15 };
        g.add_stat(major, Stat::Gold, f64::from(gold));
        let mut text = format!("{} gave you {gold} gold as a token of goodwill.", name(g, cs));
        if provides_stat(g, cs, Stat::Faith) {
            g.add_stat(major, Stat::Faith, 4.0);
            text.push_str(" (+4 faith)");
        }
        let data = EventData { player: Some(cs), ..EventData::default() };
        g.emit(EngineEvent::CsMeet, &text, Some(PlayerSet::single(major)), None, data, &[]);
    }
}

// ---- Attacks and conquest (city_states.py:710-765) -----------------------------------------------

/// A major attacks a city-state (`on_attacked`, UnCiv's `cityStateAttacked`,
/// `city_states.py:710-739`): it counts as an aggressor, a warmonger or (one time in two) an
/// aggressor makes the city-state wary of it, its protectors and ally think less of the
/// attacker, and the city-state asks the others to kill the attacker's units.
pub fn on_attacked(g: &mut Game, cs: PlayerId, attacker: PlayerId) {
    if !g.player(attacker).is_some_and(Player::is_major) {
        return;
    }
    if let Some(m) = g.player_mut(attacker, PlayerTouch::OTHER).and_then(|p| p.major.as_deref_mut())
    {
        m.cs_attacks = m.cs_attacks.saturating_add(1);
    }
    let mut rng = Rng::keyed(
        g.state().seed(),
        Purpose::CsAttacked,
        &[cs.key(), attacker.key(), g.turn().key()],
    );
    if (is_warmonger(g, attacker) || (is_aggressor(g, attacker) && rng.unit() < 0.5))
        && !pair(g, cs, attacker).wary
    {
        if let Some(p) = pair_mut(g, cs, attacker) {
            p.wary = true;
        }
        let text = format!(
            "City-states grow wary of your aggression (resting influence -20 with {}).",
            name(g, cs)
        );
        let audience = Some(PlayerSet::single(attacker));
        g.emit(EngineEvent::CsWary, &text, audience, None, EventData::default(), &[]);
    }
    let (protectors, ally) = data(g, cs).map(|d| (d.protectors, d.ally())).unwrap_or_default();
    for pr in protectors.iter() {
        if pr != attacker && g.has_met(pr, attacker) {
            add_opinion(g, pr, attacker, OpinionKey::AttackedProtectedMinor, -20.0);
        }
    }
    if let Some(al) = ally
        && al != attacker
        && g.has_met(al, attacker)
    {
        add_opinion(g, al, attacker, OpinionKey::AttackedAlliedMinor, -10.0);
    }
    if let Some(p) = pair_mut(g, cs, attacker) {
        p.recently_attacked = 2;
    }
    let r = g.rules();
    let army = g.player_units(attacker).filter(|u| r.base_units()[u.base].military).count();
    let need = u16::try_from((army / 4).max(3)).unwrap_or(u16::MAX);
    if data(g, cs).is_some_and(|d| d.war_quests.contains_key(&attacker)) {
        return;
    }
    if let Some(d) = data_mut(g, cs) {
        d.war_quests.insert(attacker, WarQuest { needed: need, kills: Default::default() });
    }
    let text = format!(
        "{} is being attacked by {}! Kill {need} of the attacker's military units and they will \
         be immensely grateful.",
        name(g, cs),
        name(g, attacker)
    );
    let others: Vec<PlayerId> = g
        .majors(true)
        .map(Player::id)
        .filter(|&q| q != attacker && g.has_met(cs, q) && !g.at_war(cs, q))
        .collect();
    for q in others {
        let data = EventData { player: Some(cs), ..EventData::default() };
        g.emit(EngineEvent::CsQuest, &text, Some(PlayerSet::single(q)), None, data, &[]);
    }
}

/// A city-state destroyed (`on_destroyed`, `city_states.py:742-749`): its protectors who know the
/// destroyer think much less of it, and the city-states that wanted it conquered reward the
/// destroyer. Package 1c-08's eliminations call it.
pub fn on_destroyed(g: &mut Game, cs: PlayerId, attacker: PlayerId) {
    let protectors = data(g, cs).map(|d| d.protectors).unwrap_or_default();
    for pr in protectors.iter() {
        if g.has_met(pr, attacker) {
            add_opinion(g, pr, attacker, OpinionKey::DestroyedProtectedMinor, -40.0);
        }
    }
    let css: Vec<PlayerId> = g.city_states(true).map(Player::id).collect();
    for q in css {
        quest_event(g, q, crate::rules::defs::QuestKind::ConquerCityState, attacker, Some(cs));
    }
}

/// A unit of `victim` killed by `killer` counts toward the war quests of the city-states `victim`
/// attacked (`on_military_unit_killed`, `city_states.py:752-765`): once the killer has killed as
/// many as a city-state asked, it earns 100 influence there.
pub fn on_military_unit_killed(g: &mut Game, killer: PlayerId, victim: PlayerId) {
    let css: Vec<PlayerId> = g.city_states(true).map(Player::id).collect();
    for q in css {
        if !data(g, q).is_some_and(|d| d.war_quests.contains_key(&victim))
            || !g.has_met(q, killer)
            || g.at_war(q, killer)
        {
            continue;
        }
        let mut done = false;
        if let Some(d) = data_mut(g, q)
            && let Some(w) = d.war_quests.get_mut(&victim)
        {
            let k = w.kills.entry(killer).or_insert(0);
            *k = k.saturating_add(1);
            done = *k >= w.needed;
            if done {
                d.war_quests.remove(&victim);
            }
        }
        if done {
            refused(add_influence(g, q, killer, 100.0));
            let text = format!(
                "{} is deeply grateful for your help against {} (+100 influence).",
                name(g, q),
                name(g, victim)
            );
            let data = EventData { player: Some(q), ..EventData::default() };
            g.emit(EngineEvent::CsQuest, &text, Some(PlayerSet::single(killer)), None, data, &[]);
        }
    }
}

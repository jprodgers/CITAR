//! Great people and golden ages (`great_people.py`, UnCiv's `GreatPersonManager`,
//! `GreatPersonPointsBreakdown`, `GoldenAgeManager` and `UnitActionsGreatPerson`).
//!
//! - **Points** ([`city_gpp`], stage E3's `end_turn`): each city's specialists and buildings
//!   earn points toward great people, raised by `[n]% Great Person generation [cities]`, by
//!   friendships under `... with declared friendships`, and by `[great person] is earned [n]%
//!   faster`, each rounded to a whole point in fixed point (`great_people.py:35-62`).
//! - **Births** (`start_turn`, stage S2): a great person whose points reach their threshold is
//!   born in the capital; a pool's threshold doubles with each (`great_people.py:65-140`). Combat
//!   points ([`add_combat_points`], for package 1c-03) have thresholds of their own, rising by 50.
//! - **Free great people** ([`ChooseGreatPerson`], the tool `choose_great_person`, and
//!   [`ai_choose_free`]), and **the Maya long count** ([`maya_long_count`], stage S2): a free
//!   great person at the end of every b'ak'tun once the tech is known, each kind once.
//! - **Golden ages** ([`enter_golden_age`], stage E6's `golden_age_stage`): happiness piles up
//!   toward the next one, which lasts longer with `[n]% Golden Age length`, and ends counted down.
//! - **The great person actions** ([`plan_hurry_research`], [`plan_hurry_construction`],
//!   [`plan_trade_mission`] and their applies), which the unit actions of package 1c-04 wrap, and
//!   [`consume_unit`], a unit spent (`units.consume`).
//!
//! What differs from Python, on purpose:
//! - a great person spent fires `upon expending a [unit]` once; Python fired it again after the
//!   unit was consumed (`great_people.py:305, 331, 358`);
//! - a great person born fires `upon gaining a [unit]` once, as any unit made in a city does;
//!   Python fired it again, for every such unique whatever unit it named (`great_people.py:140`);
//! - an AI's free great person is the first preferred kind the Maya calendar allows, where Python
//!   picked among every kind and took nothing when the calendar refused its pick
//!   (`great_people.py:209-218`);
//! - combat experience earns points toward the civilization's own kinds of great person alone,
//!   where Python credited every one of the ruleset, a Khan beside a general
//!   (`great_people.py:153-175`);
//! - either `Can speed up construction of a building` or `Can speed up the construction of a
//!   wonder` hurries construction, the second only while the city builds a wonder, where Python
//!   offered the second and then refused it (`actions.py:87`, `great_people.py:313`);
//! - reading a threshold writes nothing, where Python stored the default it read.

use serde_json::{Value, json};
use smallvec::SmallVec;

use super::action::{OutcomeSpec, Rule};
use super::cities::construction::{
    construct_if_enough, equivalent_unit, unit_has_type, unit_placement,
};
use super::cities::stats::{current_construction, remaining_work};
use super::derive::rev::{CityTouch, PlayerTouch};
use super::error::{ActionError, ErrCode};
use super::units::add_unit_in_city;
use super::{Game, research, triggers};
use crate::base::ids::{BaseUnitId, CityId, Id, PlayerId, TextId, UnitId};
use crate::base::num;
use crate::base::py;
use crate::base::sets::PlayerSet;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::Constructible;
use crate::unique::filter::UnitFacts;
use crate::unique::trigger::{TriggerEvent, TriggerSite};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

// ---- Points ------------------------------------------------------------------------------------

/// The great person points a city earns next turn, by great person
/// (`great_people.city_gpp`, `great_people.py:35-62`): its specialists' and buildings' points,
/// each kind raised by the city's bonus ([`super::cities::citizens::city_gpp_bonus`]) and by its
/// owner's `[great person] is earned [n]% faster`, in fixed point, rounded half up to whole
/// points; none of a kind that rounds to nothing.
#[must_use]
pub fn city_gpp(g: &Game, c: CityId) -> SmallVec<[(BaseUnitId, i64); 4]> {
    let mut out: SmallVec<[(BaseUnitId, i64); 4]> = SmallVec::new();
    let Some(city) = g.city(c) else { return out };
    let r = g.rules();
    let mut base: SmallVec<[(BaseUnitId, i64); 4]> = SmallVec::new();
    let mut add = |u: BaseUnitId, n: i64| match base.iter_mut().find(|(x, _)| *x == u) {
        Some((_, m)) => *m += n,
        None => base.push((u, n)),
    };
    for (i, &n) in city.specialists.iter().enumerate() {
        let Some(sp) = crate::base::ids::SpecialistId::from_index(i)
            .and_then(|s| r.specialists().get(s))
            .filter(|_| n > 0)
        else {
            continue;
        };
        for &(u, v) in sp.great_person_points.iter() {
            add(u, i64::from(v) * i64::from(n));
        }
    }
    for b in city.buildings.iter() {
        for &(u, v) in r.buildings()[b].great_person_points.iter() {
            add(u, i64::from(v));
        }
    }
    if base.is_empty() {
        return out;
    }
    let all_pct = i64::from(super::cities::citizens::city_gpp_bonus(g, c));
    let mut specific: SmallVec<[(BaseUnitId, i64); 2]> = SmallVec::new();
    {
        let v = g.view();
        let ctx = Ctx::city(&v, c);
        for h in uq::civ(&v, city.owner(), UniqueType::GreatPersonEarnedFaster, &ctx) {
            if let UniqueData::GreatPersonEarnedFaster(x) = *h.data()
                && base.iter().any(|&(u, _)| u == x.great_person)
            {
                let n = i64::from(x.percent) * i64::from(h.n);
                match specific.iter_mut().find(|(u, _)| *u == x.great_person) {
                    Some((_, m)) => *m += n,
                    None => specific.push((x.great_person, n)),
                }
            }
        }
    }
    for (u, v) in base {
        let extra = specific.iter().find(|&&(x, _)| x == u).map_or(0, |&(_, n)| n);
        let mut fixed = v * 1000;
        fixed += num::floor_div(fixed * (all_pct + extra), 100);
        let val = num::floor_div(fixed + 500, 1000);
        if val != 0 {
            out.push((u, val));
        }
    }
    out
}

/// The pool a great person's points count toward (`great_people.pool_key`,
/// `great_people.py:65-72`): its `Is part of Great Person group []`, the civilization's own unit's
/// first; `None` for the shared pool.
#[must_use]
pub fn pool_key(g: &Game, p: PlayerId, gp: BaseUnitId) -> Option<TextId> {
    let r = g.rules();
    let t = r.uniques();
    let own = equivalent_unit(g, p, gp);
    r.base_units()[own].uniques.ids().find_map(|id| match t.get(id).data {
        UniqueData::GPPointPool(x) => Some(x.pool),
        _ => None,
    })
}

/// The points the next great person of a kind needs (`great_people.points_required`,
/// `great_people.py:75-81`): its pool's threshold, 100 at first, scaled by speed.
#[must_use]
pub fn points_required(g: &Game, p: PlayerId, gp: BaseUnitId) -> i64 {
    let key = pool_key(g, p, gp);
    let base = g
        .player(p)
        .and_then(|x| x.gp.pool_threshold.get(&key).copied())
        .filter(|&n| n != 0)
        .unwrap_or(100);
    #[allow(clippy::cast_precision_loss, reason = "a threshold is far below 2^53")]
    let scaled = base as f64 * g.speed().modifier;
    num::trunc_i64(scaled)
}

/// The kinds of great person a civilization can have: its own unit for each great person that
/// belongs to no nation, and no religious one without religion (`great_people.great_people_types`,
/// `great_people.py:84-97`).
#[must_use]
pub fn great_people_types(g: &Game, p: PlayerId) -> Vec<BaseUnitId> {
    let r = g.rules();
    let mut out = Vec::new();
    for &n in &r.derived().great_person_units {
        if r.base_units()[n].unique_to.is_some() {
            continue;
        }
        let e = equivalent_unit(g, p, n);
        if !g.religion_enabled() && unit_has_type(r, e, UniqueType::ReligiousUnit) {
            continue;
        }
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

/// The great person a civilization has just earned, if any, its points paid
/// (`great_people._new_great_person`, `great_people.py:100-117`): combat points first, each
/// kind's threshold 200 at first and 50 more each time; then the others, each pool's threshold
/// doubling.
fn new_great_person(g: &mut Game, p: PlayerId) -> Option<BaseUnitId> {
    let pl = g.player(p)?;
    let combat: Vec<(BaseUnitId, f64)> =
        pl.gp.combat_points.iter().map(|(&u, &v)| (u, v)).collect();
    for (u, value) in combat {
        let need = g
            .player(p)
            .and_then(|x| x.gp.combat_threshold.get(&u).copied())
            .filter(|&n| n != 0)
            .unwrap_or(200);
        #[allow(clippy::cast_precision_loss, reason = "a threshold is far below 2^53")]
        let reached = value >= need as f64;
        if reached {
            if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
                #[allow(clippy::cast_precision_loss, reason = "a threshold is far below 2^53")]
                let paid = need as f64;
                *x.gp.combat_points.entry(u).or_insert(0.0) -= paid;
                x.gp.combat_threshold.insert(u, need + 50);
            }
            return Some(u);
        }
    }
    let points: Vec<(BaseUnitId, f64)> =
        g.player(p)?.gp.points.iter().map(|(&u, &v)| (u, v)).collect();
    for (u, value) in points {
        let need = points_required(g, p, u);
        #[allow(clippy::cast_precision_loss, reason = "a threshold is far below 2^53")]
        let need_f = need as f64;
        if value >= need_f {
            let key = pool_key(g, p, u);
            if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
                *x.gp.points.entry(u).or_insert(0.0) -= need_f;
                let t = x.gp.pool_threshold.get(&key).copied().unwrap_or(100);
                x.gp.pool_threshold.insert(key, t.saturating_mul(2));
            }
            return Some(u);
        }
    }
    None
}

/// Stage S2, great people born (`great_people.start_turn`, `great_people.py:120-140`): each great
/// person a major has earned is born in its capital, where `upon gaining a [unit]` fires once, as
/// for any unit made in a city. Python fired it a second time for a great person, and then every
/// such unique of the civilization whatever unit it named.
pub(crate) fn start_turn(g: &mut Game, p: PlayerId) {
    if !g.player(p).is_some_and(crate::state::players::Player::is_major) {
        return;
    }
    while let Some(gp) = new_great_person(g, p) {
        let Some(cap) = g.player(p).and_then(|x| x.capital).filter(|&c| g.city(c).is_some()) else {
            break;
        };
        let gp = equivalent_unit(g, p, gp);
        // refcheck: great-person-born-fires-gaining-once
        let Some(u) = add_unit_in_city(g, cap, gp) else { continue };
        if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
            x.gp.earned += 1;
        }
        let Some((name, at)) = g.city(cap).map(|x| (x.name.to_string(), x.tile())) else { break };
        let what = g.rules().base_units()[gp].name.clone();
        let data = EventData { unit: Some(u), ..EventData::default() };
        g.emit(
            EngineEvent::GreatPersonBorn,
            &format!("A {what} has been born in {name}!"),
            Some(PlayerSet::single(p)),
            Some(at),
            data,
            &[],
        );
    }
}

/// Stage E3, great person points (`great_people.end_turn`, `great_people.py:143-150`): each city
/// of a major adds its points.
pub(crate) fn end_turn(g: &mut Game, p: PlayerId) {
    if !g.player(p).is_some_and(crate::state::players::Player::is_major) {
        return;
    }
    let cities: Vec<CityId> = g.player_cities(p).map(crate::state::cities::City::id).collect();
    let mut earned: SmallVec<[(BaseUnitId, i64); 4]> = SmallVec::new();
    for c in cities {
        for (u, v) in city_gpp(g, c) {
            match earned.iter_mut().find(|(x, _)| *x == u) {
                Some((_, m)) => *m += v,
                None => earned.push((u, v)),
            }
        }
    }
    if earned.is_empty() {
        return;
    }
    if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
        for (u, v) in earned {
            #[allow(clippy::cast_precision_loss, reason = "points are far below 2^53")]
            let v = v as f64;
            *x.gp.points.entry(u).or_insert(0.0) += v;
        }
    }
}

/// Great general and admiral points from experience earned in combat (`great_people.
/// add_combat_points`, `Battle.addXp`, `great_people.py:153-175`): toward each of the
/// civilization's own kinds of great person earned through combat ([`great_people_types`]: the
/// Mongols' Khan for the great general, nobody else's) whose `<for [units] units>` the unit that
/// fought passes, raised by `[great person] is earned [n]% faster`. Python credited every such
/// unit of the ruleset, so any civilization earned Khans beside its generals, as UnCiv does not.
pub fn add_combat_points(g: &mut Game, p: PlayerId, unit: BaseUnitId, xp: i32) {
    if !g.player(p).is_some_and(crate::state::players::Player::is_major) {
        return;
    }
    let r = g.rules();
    let t = r.uniques();
    let mut gains: SmallVec<[(BaseUnitId, i64); 2]> = SmallVec::new();
    // refcheck: combat-points-for-the-civilizations-own-great-people
    for gp in great_people_types(g, p) {
        for id in r.base_units()[gp].uniques.ids() {
            if t.meta(id).ty != Some(UniqueType::GreatPersonFromCombat) {
                continue;
            }
            // Only `<for [units] units>` is asked, of the unit's kind alone
            // (`base_unit_matches`, `great_people.py:165-168`).
            let facts = UnitFacts {
                owner: p,
                base: unit,
                promotions: crate::base::sets::PromotionSet::new(),
                wounded: false,
                embarked: false,
                set_up: false,
            };
            let v = g.view();
            let fits = t.conds(t.get(id)).iter().all(|c| match c.data {
                crate::unique::CondData::ConditionalOurUnit(x) => {
                    t.filters().unit_facts_match(x.units, &v, &facts, None)
                }
                _ => true,
            });
            if !fits {
                continue;
            }
            let mut gain = f64::from(xp);
            let v = g.view();
            for h in uq::civ(&v, p, UniqueType::GreatPersonEarnedFaster, &Ctx::civ(p)) {
                if let UniqueData::GreatPersonEarnedFaster(x) = *h.data()
                    && x.great_person == gp
                {
                    for _ in 0..h.n {
                        gain += f64::from(xp) * f64::from(x.percent) / 100.0;
                    }
                }
            }
            gains.push((gp, num::trunc_i64(gain)));
        }
    }
    if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
        for (gp, n) in gains {
            #[allow(clippy::cast_precision_loss, reason = "points are far below 2^53")]
            let n = n as f64;
            *x.gp.combat_points.entry(gp).or_insert(0.0) += n;
        }
    }
}

// ---- Free great people -------------------------------------------------------------------------

/// The great people a civilization may choose free now: every kind, or, while the Maya calendar
/// owes some, those of its pool not chosen yet (`great_people.py:184-188`).
fn free_options(g: &Game, p: PlayerId) -> Vec<BaseUnitId> {
    let opts = great_people_types(g, p);
    let Some(pl) = g.player(p) else { return opts };
    if pl.gp.maya_limited > 0 {
        let pool = &pl.gp.long_count_pool;
        if pool.is_empty() {
            return opts;
        }
        return opts.into_iter().filter(|o| pool.contains(o)).collect();
    }
    opts
}

/// Whether a civilization may take great person `text` free now, and where it goes
/// (`great_people.choose_free`'s checks, `great_people.py:178-200`). Reads only.
///
/// # Errors
/// No free great person, one it may not choose, no capital, or no room near it.
pub fn plan_free_great_person(
    g: &Game,
    p: PlayerId,
    text: &str,
) -> Result<(BaseUnitId, CityId), ActionError> {
    plan_free_unit(g, p, g.rules().resolve::<BaseUnitId>(text))
}

/// [`plan_free_great_person`] for a unit the ruleset knows, or none.
fn plan_free_unit(
    g: &Game,
    p: PlayerId,
    unit: Option<BaseUnitId>,
) -> Result<(BaseUnitId, CityId), ActionError> {
    let Some(pl) = g.player(p) else { return Err(ActionError::rule("No such player.")) };
    if pl.gp.free <= 0 {
        return Err(ActionError::rule("You have no free Great Person to choose."));
    }
    let opts = free_options(g, p);
    let name = unit.map(|u| equivalent_unit(g, p, u));
    let Some(name) = name.filter(|n| opts.contains(n)) else {
        let names: Vec<&str> = opts.iter().filter_map(|&u| g.rules().name(u)).collect();
        return Err(ActionError::rule(format!("Choose one of: {}.", names.join(", "))));
    };
    let Some(cap) = pl.capital.filter(|&c| g.city(c).is_some()) else {
        return Err(ActionError::rule("You need a capital to receive a Great Person."));
    };
    if unit_placement(g, cap, name).is_none() {
        return Err(ActionError::rule("There is no room near the capital for the Great Person."));
    }
    Ok((name, cap))
}

/// Takes the free great person [`plan_free_great_person`] allowed (`great_people.choose_free`,
/// `great_people.py:198-206`): it is born near the capital, and a kind the Maya calendar owed
/// leaves its pool.
pub fn apply_free_great_person(
    g: &mut Game,
    p: PlayerId,
    name: BaseUnitId,
    cap: CityId,
) -> Option<UnitId> {
    let opts = free_options(g, p);
    let u = add_unit_in_city(g, cap, name)?;
    if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
        x.gp.free -= 1;
        if x.gp.maya_limited > 0 {
            x.gp.maya_limited -= 1;
            let pool =
                if x.gp.long_count_pool.is_empty() { opts } else { x.gp.long_count_pool.clone() };
            x.gp.long_count_pool = pool.into_iter().filter(|&o| o != name).collect();
        }
    }
    Some(u)
}

/// `choose_great_person`: takes a free great person (`tools.choose_great_person`,
/// `tools.py:879-884`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChooseGreatPerson {
    pub great_person: Value,
}

impl Rule for ChooseGreatPerson {
    type Plan = (BaseUnitId, CityId);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        plan_free_great_person(g, pid, &py::str_of(&self.great_person))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (name, cap): Self::Plan) -> OutcomeSpec {
        let u = apply_free_great_person(g, pid, name, cap);
        OutcomeSpec::render(move |g| {
            let at = u.and_then(|u| g.unit(u)).map(|x| g.xy(x.tile()));
            json!({
                "great_person": g.rules().name(name),
                "unit_id": u.map(UnitId::get),
                "at": at.map(|(x, y)| json!([x, y])),
            })
        })
    }
}

/// A civilization that lets the engine pick takes its free great people
/// (`great_people.ai_choose_free`, `great_people.py:209-218`): the first of the scientist,
/// engineer, merchant, artist and prophet it may choose, else the first kind. Python picked among
/// every kind and took nothing when the Maya calendar refused its pick.
pub fn ai_choose_free(g: &mut Game, p: PlayerId) {
    while g.player(p).is_some_and(|x| x.gp.free > 0) {
        // refcheck: ai-free-great-person-within-the-calendar
        let opts = free_options(g, p);
        let known = g.rules().derived().known.preferred_great_people;
        let pick =
            known.iter().flatten().copied().find(|u| opts.contains(u)).or(opts.first().copied());
        let Some(pick) = pick else { break };
        let Ok((u, cap)) = plan_free_unit(g, p, Some(pick)) else { break };
        if apply_free_great_person(g, p, u, cap).is_none() {
            break;
        }
    }
}

/// Stage S2, the Maya long count (`great_people.maya_long_count`, `great_people.py:221-236`):
/// with the tech its unique names, a civilization gets a free great person each time a b'ak'tun
/// of 394 years (from 3114 BC) ends between last turn and this one, of the kinds not taken yet.
pub fn maya_long_count(g: &mut Game, p: PlayerId) {
    let hits: SmallVec<[(crate::base::ids::TechId, u16); 1]> = {
        let v = g.view();
        uq::civ(&v, p, UniqueType::MayanGainGreatPerson, &Ctx::civ(p))
            .filter_map(|h| match *h.data() {
                UniqueData::MayanGainGreatPerson(x) => Some((x.tech, h.n)),
                _ => None,
            })
            .collect()
    };
    let turn = g.turn();
    let baktun = |year: f64| num::trunc_i64(((year + 3114.0) / 394.0).floor());
    for (tech, n) in hits {
        if !g.has_tech(p, Some(tech)) {
            continue;
        }
        for _ in 0..n {
            if baktun(g.year(Some(turn))) == baktun(g.year(Some(turn - 1))) {
                continue;
            }
            let types = great_people_types(g, p);
            if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
                if x.gp.long_count_pool.is_empty() {
                    x.gp.long_count_pool = types;
                }
                x.gp.free += 1;
                x.gp.maya_limited += 1;
            }
            g.emit(
                EngineEvent::GreatPersonAvailable,
                "A new b'ak'tun has begun: a Great Person joins you!",
                Some(PlayerSet::single(p)),
                None,
                EventData::default(),
                &[],
            );
        }
    }
}

// ---- Golden ages -------------------------------------------------------------------------------

/// The happiness a civilization must pile up for its next golden age
/// (`great_people.happiness_for_golden_age`, `great_people.py:242-248`): 500, and 250 more for
/// each it has had, one percent more for each city, scaled by speed.
#[must_use]
pub fn happiness_for_golden_age(g: &Game, p: PlayerId) -> i64 {
    let Some(pl) = g.player(p) else { return 0 };
    let mut cost = 500.0 + f64::from(pl.econ.golden_ages) * 250.0;
    let cities = f64::from(u32::try_from(g.player_cities(p).count()).unwrap_or(u32::MAX));
    cost *= 1.0 + cities / 100.0;
    cost *= g.speed().modifier;
    num::trunc_i64(cost)
}

/// How long a golden age of `turns` lasts for a civilization (`great_people.golden_age_length`,
/// `great_people.py:251-257`): longer with `[n]% Golden Age length`, scaled by speed.
#[must_use]
pub fn golden_age_length(g: &Game, p: PlayerId, turns: i32) -> i32 {
    let mut t = f64::from(turns);
    let v = g.view();
    for h in uq::civ(&v, p, UniqueType::GoldenAgeLength, &Ctx::civ(p)) {
        if let UniqueData::GoldenAgeLength(x) = *h.data() {
            for _ in 0..h.n {
                t *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    t *= g.speed().golden_age_length_modifier;
    num::trunc_i32(t)
}

/// A civilization enters a golden age of `turns` turns, ten by default, lengthened
/// (`great_people.enter_golden_age`, `great_people.py:260-267`); it is told, and `upon entering a
/// Golden Age` fires.
pub fn enter_golden_age(g: &mut Game, p: PlayerId, turns: Option<i32>) {
    let Some(now) = g.player(p).map(|x| x.econ.golden_age_turns) else { return };
    let add = golden_age_length(g, p, turns.unwrap_or(10));
    g.set_golden_age_turns(p, now.saturating_add(add));
    let who = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let data = EventData { player: Some(p), ..EventData::default() };
    g.emit(
        EngineEvent::GoldenAge,
        &format!("{who} has entered a Golden Age!"),
        Some(PlayerSet::single(p)),
        None,
        data,
        &[],
    );
    triggers::fire(g, &TriggerSite::civ(p), &TriggerEvent::EnteringGoldenAge, true, None);
}

/// A golden age counts down, or happiness piles up toward the next
/// (`great_people.golden_age_end_turn`, `great_people.py:270-283`).
pub fn golden_age_end_turn(g: &mut Game, p: PlayerId, happiness: i32) {
    let Some((turns, points)) =
        g.player(p).map(|x| (x.econ.golden_age_turns, x.econ.golden_age_points))
    else {
        return;
    };
    let mut points = points;
    if turns <= 0 {
        points = (points + f64::from(happiness)).max(0.0);
        if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
            x.econ.golden_age_points = points;
        }
    }
    if turns > 0 {
        g.set_golden_age_turns(p, turns - 1);
        if turns == 1 {
            let who = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
            g.emit(
                EngineEvent::GoldenAgeEnd,
                &format!("{who}'s Golden Age has ended."),
                Some(PlayerSet::single(p)),
                None,
                EventData::default(),
                &[],
            );
        }
        return;
    }
    #[allow(clippy::cast_precision_loss, reason = "a threshold is far below 2^53")]
    let need = happiness_for_golden_age(g, p) as f64;
    if points >= need {
        if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
            x.econ.golden_age_points -= need;
        }
        enter_golden_age(g, p, None);
        if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
            x.econ.golden_ages += 1;
        }
    }
}

/// Stage E6, golden-age progress (`turns.py:109-110`): the civilization's happiness now.
pub(crate) fn golden_age_stage(g: &mut Game, p: PlayerId) {
    let happiness = super::query::happiness(g, p).total;
    golden_age_end_turn(g, p, happiness);
}

// ---- The great person actions ------------------------------------------------------------------

/// A unit is spent (`units.consume`, `units.py:477-482`): `upon expending a [unit]` fires for its
/// owner, once, then it leaves the game. Python fired it a second time after a great person's
/// action (`great_people.triggers_expend`); here the one place is [`units::abilities::consume`],
/// which every action that spends a unit reaches.
///
/// [`units::abilities::consume`]: super::units::abilities::consume
// refcheck: expending-a-unit-fires-once
pub fn consume_unit(g: &mut Game, u: UnitId) {
    super::units::abilities::consume(g, u);
}

/// A unit's owner and whether its type has a unique of type `ty`.
fn unit_can(g: &Game, u: UnitId, ty: UniqueType) -> Option<(PlayerId, bool)> {
    let x = g.unit(u)?;
    Some((x.owner(), unit_has_type(g.rules(), x.base, ty)))
}

/// A unit with no movement left may not act.
fn moves_left(g: &Game, u: UnitId) -> Result<(), ActionError> {
    if g.unit(u).is_some_and(|x| x.moves <= 0) {
        return Err(ActionError::rule("The unit has no movement left."));
    }
    Ok(())
}

/// Whether a great scientist may hurry research now, and how much science it adds
/// (`great_people.hurry_research`'s checks, `great_people.py:289-302`). Reads only.
///
/// # Errors
/// A unit that cannot, has no movement left, no research, or research that cannot be hurried.
pub fn plan_hurry_research(g: &Game, u: UnitId) -> Result<i32, ActionError> {
    let Some((p, can)) = unit_can(g, u, UniqueType::CanHurryResearch) else {
        return Err(ActionError::new(ErrCode::BadParam, "No such unit."));
    };
    if !can {
        return Err(ActionError::rule("This unit cannot hurry research."));
    }
    moves_left(g, u)?;
    let Some(cur) = research::current(g, p) else {
        return Err(ActionError::rule("Choose a technology to research first."));
    };
    let r = g.rules();
    if super::core::has_type(r, &r.techs()[cur].uniques, UniqueType::CannotBeHurried) {
        return Err(ActionError::rule(format!("{} cannot be hurried.", r.techs()[cur].name)));
    }
    Ok(research::science_from_great_scientist(g, p))
}

/// A great scientist hurries research with the science [`plan_hurry_research`] found, and is
/// spent (`great_people.py:303-306`).
pub fn apply_hurry_research(g: &mut Game, u: UnitId, science: i32) -> Value {
    let Some(p) = g.unit(u).map(crate::state::units::Unit::owner) else { return Value::Null };
    research::add_science(g, p, f64::from(science));
    consume_unit(g, u);
    json!({
        "science_added": science,
        "researching": research::current(g, p).and_then(|t| g.rules().name(t)),
    })
}

/// Whether a unit may hurry the construction of the city it stands in now, and with how much
/// production (`great_people.hurry_construction`'s checks, `great_people.py:309-327`): a great
/// engineer anything but a unit, or a unit that speeds up wonders a wonder. Reads only.
///
/// # Errors
/// A unit that cannot, is not in one of its cities, has no movement left, or a construction that
/// is not a building, cannot be hurried, or is nearly done.
pub fn plan_hurry_construction(
    g: &Game,
    u: UnitId,
) -> Result<(CityId, Constructible, i32), ActionError> {
    let r = g.rules();
    let Some((p, any)) = unit_can(g, u, UniqueType::CanSpeedupConstruction) else {
        return Err(ActionError::new(ErrCode::BadParam, "No such unit."));
    };
    let wonders = unit_can(g, u, UniqueType::CanSpeedupWonderConstruction).is_some_and(|x| x.1);
    let at = g.unit(u).map(crate::state::units::Unit::tile);
    let city = at.and_then(|t| g.city_at(t)).filter(|c| c.owner() == p);
    let cur = city.and_then(current_construction);
    // Either unique hurries construction, the wonder one only a wonder.
    // refcheck: hurry-wonder-construction
    let building_wonder =
        matches!(cur, Some(Constructible::Building(b)) if r.buildings()[b].is_wonder);
    if !any && !(wonders && building_wonder) {
        return Err(ActionError::rule("This unit cannot hurry construction."));
    }
    let Some(city) = city else {
        return Err(ActionError::rule("Move the Great Engineer into one of your cities."));
    };
    moves_left(g, u)?;
    let Some(item @ Constructible::Building(b)) = cur else {
        return Err(ActionError::rule(format!(
            "{} must be building a building or wonder to hurry it.",
            city.name
        )));
    };
    let bd = &r.buildings()[b];
    if super::core::has_type(r, &bd.uniques, UniqueType::CannotBeHurried) {
        return Err(ActionError::rule(format!("{} cannot be hurried.", bd.name)));
    }
    let c = city.id();
    let most = f64::from(300 + 30 * i32::from(city.pop)) * g.speed().production_cost_modifier;
    let add = num::trunc_i32(most.min(remaining_work(g, c, item) - 1.0));
    if add <= 0 {
        return Err(ActionError::rule(format!("{} is nearly complete already.", bd.name)));
    }
    Ok((c, item, add))
}

/// The unit hurries the construction [`plan_hurry_construction`] found, and is spent
/// (`great_people.py:328-332`).
pub fn apply_hurry_construction(
    g: &mut Game,
    u: UnitId,
    (c, item, add): (CityId, Constructible, i32),
) -> Value {
    if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
        *x.progress.entry(item).or_insert(0.0) += f64::from(add);
    }
    construct_if_enough(g, c);
    consume_unit(g, u);
    json!({
        "production_added": add,
        "city": g.city(c).map(|x| x.name.to_string()),
        "item": super::cities::construction::item_name(g.rules(), item),
    })
}

/// Whether a great merchant may conduct a trade mission where it stands now, and for how much
/// gold and influence (`great_people.trade_mission`'s checks, `great_people.py:335-353`): in the
/// land of a city-state at peace with its owner. Reads only.
///
/// # Errors
/// A unit that cannot, is not in such land, or has no movement left.
pub fn plan_trade_mission(g: &Game, u: UnitId) -> Result<(PlayerId, i32, i32), ActionError> {
    let r = g.rules();
    let t = r.uniques();
    let Some(unit) = g.unit(u) else {
        return Err(ActionError::new(ErrCode::BadParam, "No such unit."));
    };
    let p = unit.owner();
    let influence = r.base_units()[unit.base].uniques.ids().find_map(|id| match t.get(id).data {
        UniqueData::CanTradeWithCityStateForGoldAndInfluence(x) => Some(x.influence),
        _ => None,
    });
    let Some(influence) = influence else {
        return Err(ActionError::rule("This unit cannot conduct trade missions."));
    };
    let owner = g.tile(unit.tile()).and_then(crate::state::map::Tile::owner);
    let Some(cs) = owner.filter(|&o| g.is_city_state(o) && o != p && !g.at_war(o, p)) else {
        return Err(ActionError::rule(
            "Move the Great Merchant into the territory of a city-state you are at peace with.",
        ));
    };
    moves_left(g, u)?;
    let era = f64::from(super::derive::civ::era(g, p).0);
    let mut gold = (350.0 + 50.0 * era) * g.speed().gold_cost_modifier;
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    for h in uq::unit_and_civ(&v, u, UniqueType::PercentGoldFromTradeMissions, &ctx) {
        if let UniqueData::PercentGoldFromTradeMissions(x) = *h.data() {
            for _ in 0..h.n {
                gold *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    Ok((cs, num::trunc_i32(gold), influence))
}

/// The great merchant's trade mission [`plan_trade_mission`] found: the gold, the influence, and
/// the merchant spent (`great_people.py:354-361`).
pub fn apply_trade_mission(
    g: &mut Game,
    u: UnitId,
    (cs, gold, influence): (PlayerId, i32, i32),
) -> Value {
    let Some(p) = g.unit(u).map(crate::state::units::Unit::owner) else { return Value::Null };
    if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
        x.econ.gold += f64::from(gold);
    }
    let refused = super::city_states::influence::add_influence(g, cs, p, f64::from(influence));
    debug_assert!(refused.is_ok(), "influence refused: {refused:?}");
    consume_unit(g, u);
    let name = g.player(cs).map(|x| x.name.to_string()).unwrap_or_default();
    g.emit(
        EngineEvent::TradeMission,
        &format!("Your trade mission to {name} earned {gold} gold and {influence} influence."),
        Some(PlayerSet::single(p)),
        None,
        EventData::default(),
        &[],
    );
    json!({ "gold": gold, "influence": influence, "city_state": name })
}

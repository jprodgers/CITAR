//! What religion costs, and the great prophets faith brings (`religion.py:331-445`, UnCiv's
//! `ReligionManager`).
//!
//! - [`faith_for_pantheon`], [`max_religions`], [`founded_religions`] and [`remaining_foundable`]:
//!   how dear a pantheon is and how many religions the game still has room and beliefs for;
//! - [`prophet_unit`], [`prophets_earned`] and [`faith_for_next_prophet`]: a civilization's great
//!   prophet, and what the next one costs (`200 + 100 n(n+1)/2` faith for the `n`th, scaled);
//! - [`can_generate_prophet`] and [`start_turn`], stage S2: while a civilization has faith for the
//!   next prophet, it appears with a chance of `(5 + faith - cost)%` a turn, drawn from
//!   `Purpose::Prophet` keyed by the turn and the civilization, in its holy city if it holds it,
//!   else in its capital.

use super::super::Game;
use super::super::cities::construction::{
    add_construction_bonuses, add_unit_in_city, equivalent_unit, unit_has_type,
};
use super::super::derive::rev::{PlayerTouch, UnitTouch};
use super::{beliefs_available, holy_city};
use crate::base::ids::{BaseUnitId, PlayerId};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::PlayerSet;
use crate::rules::defs::{BeliefKind, BeliefType, ReligionProgress};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::Constructible;
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// The faith a pantheon costs, with `additional` more founded than now
/// (`religion.faith_for_pantheon`, `religion.py:331-335`): the ruleset's base and its growth for
/// each civilization that has a pantheon or a religion, scaled by speed, rounded.
#[must_use]
pub fn faith_for_pantheon(g: &Game, additional: i32) -> i32 {
    let k = &g.rules().constants().formulas;
    let started = g.majors(false).filter(|p| p.religion.founded.is_some()).count();
    let n = additional + i32::try_from(started).unwrap_or(i32::MAX);
    let cost = f64::from(k.pantheon_base + n * k.pantheon_growth) * g.speed().faith_cost_modifier;
    num::round_half_even_i32(cost)
}

/// How many religions the game allows (`religion.max_religions`, `religion.py:338-341`): the
/// ruleset's base and its share of the major civilizations, and no more than it has names for.
#[must_use]
pub fn max_religions(g: &Game) -> i32 {
    let k = &g.rules().constants().formulas;
    let majors = f64::from(u32::try_from(g.majors(false).count()).unwrap_or(u32::MAX));
    let names = i32::try_from(g.rules().religions().len()).unwrap_or(i32::MAX);
    names.min(k.religion_limit_base + num::trunc_i32(majors * k.religion_limit_multiplier))
}

/// How many religions have been founded (`religion.founded_religions`, `religion.py:344-346`).
#[must_use]
pub fn founded_religions(g: &Game) -> i32 {
    let n = g
        .majors(false)
        .filter(|p| {
            p.religion.founded.is_some() && p.religion.progress >= ReligionProgress::Religion
        })
        .count();
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// How many more religions could be founded (`religion.remaining_foundable`,
/// `religion.py:349-355`): the room the limit leaves, and the follower and founder beliefs left.
#[must_use]
pub fn remaining_foundable(g: &Game) -> i32 {
    let left = |t| i32::try_from(beliefs_available(g, BeliefKind::Type(t)).len()).unwrap_or(0);
    (max_religions(g) - founded_religions(g))
        .min(left(BeliefType::Follower))
        .min(left(BeliefType::Founder))
}

/// A civilization's great prophet (`religion.prophet_unit`, `religion.py:358-364`): its own
/// unit for the first unit that may found a religion and belongs to no nation.
#[must_use]
pub fn prophet_unit(g: &Game, p: PlayerId) -> Option<BaseUnitId> {
    let r = g.rules();
    r.base_units()
        .iter()
        .find(|&(u, d)| d.unique_to.is_none() && unit_has_type(r, u, UniqueType::MayFoundReligion))
        .map(|(u, _)| equivalent_unit(g, p, u))
}

/// How many great prophets a civilization has had (`religion.prophets_earned`,
/// `religion.py:367-370`): the times it bought its prophet at an increasing price.
#[must_use]
pub fn prophets_earned(g: &Game, p: PlayerId) -> i32 {
    let Some(pu) = prophet_unit(g, p) else { return 0 };
    g.player(p)
        .and_then(|x| x.civ.bought_increasing.get(&Constructible::Unit(pu)).copied())
        .map_or(0, i32::from)
}

/// The faith a civilization's next great prophet costs (`religion.faith_for_next_prophet`,
/// `religion.py:373-379`).
#[must_use]
pub fn faith_for_next_prophet(g: &Game, p: PlayerId) -> i32 {
    let n = f64::from(prophets_earned(g, p));
    let mut cost = (200.0 + 100.0 * n * (n + 1.0) / 2.0) * g.speed().faith_cost_modifier;
    let v = g.view();
    for h in uq::civ(&v, p, UniqueType::FaithCostOfGreatProphetChange, &Ctx::civ(p)) {
        if let UniqueData::FaithCostOfGreatProphetChange(x) = *h.data() {
            for _ in 0..h.n {
                cost *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    num::trunc_i32(cost)
}

/// Whether a civilization is due a great prophet (`religion.can_generate_prophet`,
/// `religion.py:382-397`): a major with a pantheon or a religion, a prophet unit, the faith for
/// it, nothing forbidding it, and room for its religion if it has only a pantheon.
#[must_use]
pub fn can_generate_prophet(g: &Game, p: PlayerId, ignore_faith: bool) -> bool {
    let Some(pl) = g.player(p) else { return false };
    if !g.religion_enabled() || !pl.is_major() {
        return false;
    }
    if pl.religion.founded.is_none() || pl.religion.progress == ReligionProgress::None {
        return false;
    }
    if prophet_unit(g, p).is_none() {
        return false;
    }
    if !ignore_faith && pl.econ.faith < f64::from(faith_for_next_prophet(g, p)) {
        return false;
    }
    let v = g.view();
    if uq::any(uq::civ(&v, p, UniqueType::MayNotGenerateGreatProphet, &Ctx::civ(p))) {
        return false;
    }
    !(pl.religion.progress == ReligionProgress::Pantheon && remaining_foundable(g) == 0)
}

/// Stage S2's religion (`religion.start_turn`, `religion.py:400-430`): a civilization due a great
/// prophet has it with a chance of `(5 + faith - cost)%`, in its holy city if it holds it and has
/// a religion, else in its capital; it pays the faith, and its next prophet costs more.
pub fn start_turn(g: &mut Game, p: PlayerId) {
    if !can_generate_prophet(g, p, false) {
        return;
    }
    let Some(pu) = prophet_unit(g, p) else { return };
    let Some(pl) = g.player(p) else { return };
    let cost = faith_for_next_prophet(g, p);
    let chance = (5.0 + pl.econ.faith - f64::from(cost)) / 100.0;
    let mut rng = Rng::keyed(g.state().seed(), Purpose::Prophet, &[g.turn().key(), p.key()]);
    if rng.unit() >= chance {
        return;
    }
    let capital = pl.capital.filter(|&c| g.city(c).is_some());
    let birth = if pl.religion.progress < ReligionProgress::Religion {
        capital
    } else {
        pl.religion
            .founded
            .and_then(|r| holy_city(g, r))
            .filter(|&c| g.city(c).is_some_and(|x| x.owner() == p))
            .or(capital)
    };
    let Some(birth) = birth else { return };
    let Some(u) = add_unit_in_city(g, birth, pu) else { return };
    add_construction_bonuses(g, u, birth);
    let religion = g.player(p).and_then(|x| x.religion.founded);
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.religion = religion;
    }
    if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS | PlayerTouch::RELIGION) {
        x.econ.faith -= f64::from(cost);
        let n = x.civ.bought_increasing.entry(Constructible::Unit(pu)).or_insert(0);
        *n = n.saturating_add(1);
        x.gp.prophets_earned += 1;
    }
    let Some((name, at)) = g.city(birth).map(|x| (x.name.to_string(), x.tile())) else { return };
    let data = EventData { unit: Some(u), ..EventData::default() };
    g.emit(
        EngineEvent::GreatPersonBorn,
        &format!("A Great Prophet has appeared in {name}!"),
        Some(PlayerSet::single(p)),
        Some(at),
        data,
        &[],
    );
}

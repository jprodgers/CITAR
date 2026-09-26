//! What a major may do with a city-state (`city_states.py:468-802`; `tools.city_state_action`,
//! `tools.py:998-1033`): gifts of gold and units, protection pledged and withdrawn, tribute
//! demanded, peace, and marriage into the ruling family of a long-time ally.
//!
//! The tool is one action, [`CityStateAction`], whose check reads everything Python refused, with
//! Python's messages, before anything is written: `plan_*` read, the apply that follows cannot
//! fail. Tribute is a contest of fear ([`tribute_modifiers`]): a city-state pays a civilization
//! whose army near its capital outweighs its own, whose military ranks high, and whom nothing
//! else keeps it from fearing.

use serde_json::{Value, json};

use super::influence::{
    add_influence, data, data_mut, influence, pair, pair_mut, raw_influence, relationship,
};
use super::quests::{complete_quests, quest_event};
use crate::base::ids::{BaseUnitId, PlayerId, UnitId};
use crate::base::num;
use crate::base::py;
use crate::base::sets::PlayerSet;
use crate::base::stats::Stat;
use crate::game::action::{OutcomeSpec, Rule};
use crate::game::cities::purchase::base_gold_cost;
use crate::game::derive::rev::CityTouch;
use crate::game::diplomacy::relations::{
    add_opinion, civ_has, name, peace_with_city_state, plan_peace_with_city_state,
};
use crate::game::error::{ActionError, ErrCode};
use crate::game::lookup::own_unit;
use crate::game::units::{self, place_unit_near};
use crate::game::{Game, conquest, victory};
use crate::rules::defs::{CityStatePersonality, QuestKind, QuestScope};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::Constructible;
use crate::state::diplo::OpinionKey;
use crate::state::players::Player;
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// A living city-state the major has met, or the refusal (`_check_cs`,
/// `city_states.py:471-476`).
fn check_cs(g: &Game, major: PlayerId, cs: PlayerId) -> Result<(), ActionError> {
    if !g.player(cs).is_some_and(|p| p.is_city_state() && p.alive()) {
        return Err(ActionError::rule("That is not a living city-state."));
    }
    if !g.has_met(major, cs) {
        return Err(ActionError::rule("You have not met that city-state."));
    }
    Ok(())
}

/// A city-state the caller has met, by the id the tool gives (`tools._city_state`,
/// `tools.py:178-184`).
fn city_state_of(g: &Game, pid: PlayerId, id: i64) -> Result<PlayerId, ActionError> {
    let cs = u8::try_from(id)
        .ok()
        .map(PlayerId)
        .filter(|&q| g.player(q).is_some_and(Player::is_city_state))
        .ok_or_else(|| ActionError::rule(format!("Player {id} is not a city-state.")))?;
    if !g.has_met(pid, cs) {
        return Err(ActionError::rule(format!("You have not met {}.", name(g, cs))));
    }
    Ok(cs)
}

// ---- Gold (city_states.py:479-513) -----------------------------------------------------------------

/// The influence a gift of gold buys (`influence_from_gold`, `city_states.py:479-493`):
/// `gold^1.01 / 9.8`, falling by up to two thirds as the game goes on, scaled by the speed's gift
/// modifier, the donor's `Gifts of Gold to City-States generate [n]% more Influence` and its
/// investment quest; rounded down to a multiple of 5, at least 5.
#[must_use]
pub fn influence_from_gold(g: &Game, cs: PlayerId, donor: PlayerId, gold: i32) -> i32 {
    let sp = g.speed();
    let mut inf = num::pow(f64::from(gold), 1.01) / 9.8;
    let progress = (f64::from(g.turn()) / (400.0 * sp.modifier)).min(1.0);
    inf *= 1.0 - (2.0 / 3.0) * progress;
    inf *= sp.gold_gift_modifier;
    let v = g.view();
    for h in
        uq::civ(&v, donor, UniqueType::CityStateGoldGiftsProvideMoreInfluence, &Ctx::civ(donor))
    {
        if let UniqueData::CityStateGoldGiftsProvideMoreInfluence(x) = *h.data() {
            for _ in 0..h.n {
                inf *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    inf *= super::quests::investment_multiplier(g, cs, donor);
    inf -= inf.rem_euclid(5.0);
    num::trunc_i32(inf.max(5.0))
}

/// Checks a gift of gold (`gift_gold`, `city_states.py:496-506`).
///
/// # Errors
/// No living city-state met, at war with it, an amount below 1, or more than the donor has.
pub fn plan_gift_gold(
    g: &Game,
    major: PlayerId,
    cs: PlayerId,
    gold: i64,
) -> Result<(i32, i32), ActionError> {
    check_cs(g, major, cs)?;
    if g.at_war(major, cs) {
        return Err(ActionError::rule(
            "You cannot give gifts to a city-state you are at war with.",
        ));
    }
    if gold < 1 {
        return Err(ActionError::rule("Gift an amount of gold (UnCiv offers 250, 500 or 1000)."));
    }
    let have = g.player(major).map_or(0.0, |p| p.econ.gold);
    #[allow(clippy::cast_precision_loss, reason = "a gift of gold, far below 2^53")]
    if have < gold as f64 {
        return Err(ActionError::rule(format!("You only have {} gold.", num::trunc_i64(have))));
    }
    let gold = i32::try_from(gold).unwrap_or(i32::MAX);
    Ok((gold, influence_from_gold(g, cs, major, gold)))
}

/// A major gives a city-state gold for influence (`gift_gold`, `city_states.py:507-513`).
pub fn gift_gold(g: &mut Game, major: PlayerId, cs: PlayerId, gold: i32, inf: i32) -> Value {
    g.add_stat(major, Stat::Gold, -f64::from(gold));
    g.add_stat(cs, Stat::Gold, f64::from(gold));
    refused(add_influence(g, cs, major, f64::from(inf)));
    complete_quests(g, cs, major, QuestKind::GiveGold);
    json!({
        "city_state": name(g, cs),
        "gold": gold,
        "influence_gained": inf,
        "influence": num::round_ndigits(raw_influence(g, cs, major), 1),
        "relationship": relationship(g, cs, major).name(),
    })
}

/// A write of influence the state refused, which is an engine bug.
pub(crate) fn refused(r: Result<(), crate::state::StateError>) {
    debug_assert!(r.is_ok(), "a city-state's influence refused: {r:?}");
}

// ---- Units (city_states.py:516-538) ----------------------------------------------------------------

/// A unit gift [`plan_gift_unit`] allowed: to the city-state whose land it stands in, for so much
/// influence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitGift {
    pub unit: UnitId,
    pub cs: PlayerId,
    pub gain: f64,
}

/// Checks a gift of a unit standing in a city-state's land (`gift_unit`,
/// `city_states.py:516-531`): a military unit, or one the civilization's `Gain [n] Influence with
/// a [units] gift to a City-State` names, with movement left, to a city-state at peace with it.
///
/// # Errors
/// Why it may not.
pub fn plan_gift_unit(g: &Game, major: PlayerId, u: UnitId) -> Result<UnitGift, ActionError> {
    let Some(x) = g.unit(u) else { return Err(ActionError::rule("No such unit.")) };
    let cs = g
        .tile(x.tile())
        .and_then(crate::state::map::Tile::owner)
        .filter(|&o| g.is_city_state(o))
        .ok_or_else(|| {
            ActionError::rule("Move the unit into a city-state's territory to gift it.")
        })?;
    if g.at_war(major, cs) {
        return Err(ActionError::rule("No gifts to a city-state you are at war with."));
    }
    let special: Option<i32> = {
        let v = g.view();
        let ctx = units::unit_ctx(g, u);
        let t = g.rules().uniques();
        uq::unit_and_civ(&v, u, UniqueType::GainInfluenceWithUnitGiftToCityState, &ctx)
            .filter_map(|h| match *h.data() {
                UniqueData::GainInfluenceWithUnitGiftToCityState(x) => Some(x),
                _ => None,
            })
            .find(|x| t.in_set(x.units, x_base(g, u)))
            .map(|x| x.influence)
    };
    if !g.rules().base_units()[x.base].military && special.is_none() {
        return Err(ActionError::rule(
            "City-states only accept military units (or units your civilization may gift).",
        ));
    }
    if x.moves <= 0 {
        return Err(ActionError::rule("The unit has no movement left."));
    }
    let gain = 5.0 + special.map_or(0.0, |n| f64::from(n) - 5.0);
    Ok(UnitGift { unit: u, cs, gain })
}

/// A unit's base unit.
fn x_base(g: &Game, u: UnitId) -> BaseUnitId {
    g.unit(u).map_or(BaseUnitId(0), |x| x.base)
}

/// A major gives a unit to a city-state, which [`plan_gift_unit`] allowed (`gift_unit`,
/// `city_states.py:532-538`): a great person is used up, anything else changes hands.
pub fn gift_unit(g: &mut Game, major: PlayerId, gift: UnitGift) -> Value {
    let base = g.unit(gift.unit).map_or(BaseUnitId(0), |x| x.base);
    refused(add_influence(g, gift.cs, major, gift.gain));
    if g.rules().base_units()[base].great_person {
        units::remove_unit(g, gift.unit);
    } else {
        let moved = g.change_unit_owner(gift.unit, gift.cs);
        debug_assert!(moved.is_ok(), "a unit of the game changes hands: {moved:?}");
    }
    json!({
        "gifted": &*g.rules().base_units()[base].name,
        "city_state": name(g, gift.cs),
        "influence_gained": gift.gain,
    })
}

// ---- Protection (city_states.py:541-585) -------------------------------------------------------------

/// Why a major cannot pledge to protect a city-state, or `None` (`can_pledge`,
/// `city_states.py:541-553`).
#[must_use]
pub fn can_pledge(g: &Game, cs: PlayerId, major: PlayerId) -> Option<String> {
    let p = pair(g, cs, major);
    if p.withdrew > 0 {
        return Some(format!("You withdrew protection recently ({} turns left).", p.withdrew));
    }
    if influence(g, cs, major) < 0.0 {
        return Some("Influence must be at least 0.".to_owned());
    }
    if g.at_war(cs, major) {
        return Some("You are at war with them.".to_owned());
    }
    if data(g, cs).is_some_and(|d| d.protectors.contains(major)) {
        return Some("You already protect them.".to_owned());
    }
    None
}

/// Checks a pledge to protect (`pledge`, `city_states.py:556-561`).
///
/// # Errors
/// No living city-state met, or [`can_pledge`]'s reason.
pub fn plan_pledge(g: &Game, major: PlayerId, cs: PlayerId) -> Result<(), ActionError> {
    check_cs(g, major, cs)?;
    match can_pledge(g, cs, major) {
        Some(why) => Err(ActionError::rule(why)),
        None => Ok(()),
    }
}

/// A major pledges to protect a city-state (`pledge`, `city_states.py:562-566`): it may not
/// withdraw for ten turns.
pub fn pledge(g: &mut Game, major: PlayerId, cs: PlayerId) -> Value {
    if let Some(d) = data_mut(g, cs) {
        d.protectors.insert(major);
    }
    if let Some(p) = pair_mut(g, cs, major) {
        p.pledged = 10;
    }
    complete_quests(g, cs, major, QuestKind::PledgeToProtect);
    json!({"pledged_protection": name(g, cs)})
}

/// Checks a withdrawal of protection (`withdraw_protection`, `city_states.py:569-581`).
///
/// # Errors
/// No living city-state met, not a protector, or pledged too recently.
pub fn plan_withdraw(g: &Game, major: PlayerId, cs: PlayerId) -> Result<(), ActionError> {
    check_cs(g, major, cs)?;
    if !data(g, cs).is_some_and(|d| d.protectors.contains(major)) {
        return Err(ActionError::rule("You are not protecting them."));
    }
    let p = pair(g, cs, major);
    if p.pledged > 0 {
        return Err(ActionError::rule(format!(
            "You pledged recently and cannot withdraw for {} more turns.",
            p.pledged
        )));
    }
    Ok(())
}

/// A major's protection ends, at a cost of 20 influence, and it may not pledge again for 20 turns
/// (`withdraw_protection`, `city_states.py:582-585`): withdrawn, or broken by attacking the
/// city-state.
pub fn withdraw(g: &mut Game, major: PlayerId, cs: PlayerId) -> Value {
    if let Some(d) = data_mut(g, cs) {
        d.protectors.remove(major);
    }
    if let Some(p) = pair_mut(g, cs, major) {
        p.withdrew = 20;
    }
    refused(add_influence(g, cs, major, -20.0));
    json!({"withdrew_protection": name(g, cs)})
}

// ---- Tribute (city_states.py:588-707) ----------------------------------------------------------------

/// A unit's military weight (`_force`, `city_states.py:653-657`).
fn force(g: &Game, u: &crate::state::units::Unit) -> f64 {
    let d = &g.rules().base_units()[u.base];
    num::pow(f64::from(d.strength.max(d.ranged_strength)), 1.5) * f64::from(u.hp) / 100.0
}

/// Why a city-state would or would not pay a major tribute, reason by reason, in Python's order
/// (`tribute_modifiers`, `city_states.py:593-650`): a base of -110; its personality, ally and
/// protectors; a worker demanded (and from a small city-state); tribute paid recently; influence
/// below -30; then the major's military rank among the majors, and last the weight of its army
/// near the capital against the capital's own.
#[must_use]
pub fn tribute_modifiers(
    g: &Game,
    cs: PlayerId,
    major: PlayerId,
    worker: bool,
) -> Vec<(&'static str, i32)> {
    let Some(d) = data(g, cs) else { return vec![("No Cities", -999)] };
    let Some(cap) = g.player(cs).and_then(|p| p.capital).and_then(|c| g.city(c)) else {
        return vec![("No Cities", -999)];
    };
    let mut mods: Vec<(&'static str, i32)> = vec![("Base value", -110)];
    if d.personality == Some(CityStatePersonality::Hostile) {
        mods.push(("Hostile", -10));
    }
    if d.ally().is_some_and(|a| a != major) {
        mods.push(("Has Ally", -10));
    }
    if d.protectors.iter().any(|x| x != major) {
        mods.push(("Has Protector", -20));
    }
    if worker {
        mods.push(("Demanding a Worker", -30));
        if cap.pop < 4 {
            mods.push(("Demanding a Worker from small City-State", -300));
        }
    }
    let recent = d.recently_bullied;
    if recent > 10 {
        mods.push(("Very recently paid tribute", -300));
    } else if recent > 0 {
        mods.push(("Recently paid tribute", -40));
    }
    if influence(g, cs, major) < -30.0 {
        mods.push(("Influence below -30", -300));
    }
    let sum = |m: &[(&str, i32)]| m.iter().map(|&(_, v)| v).sum::<i32>();
    if sum(&mods) < -200 {
        return mods;
    }
    let mut majors: Vec<(i32, PlayerId)> =
        g.majors(true).map(|q| (victory::military_strength(g, q.id()), q.id())).collect();
    majors.sort_by_key(|&(s, _)| core::cmp::Reverse(s));
    let rank = majors.iter().position(|&(_, q)| q == major).unwrap_or(majors.len());
    let n = majors.len().max(1);
    let f = &g.rules().constants().formulas;
    let (n, rank) = (i32::try_from(n).unwrap_or(1), i32::try_from(rank).unwrap_or(0));
    mods.push(("Military Rank", num::floor_div(f.tribute_global_modifier * (n - rank), n)));
    if sum(&mods) < -100 {
        return mods;
    }
    let range = u32::from(g.grid().height() / 10).clamp(5, 10);
    let mut near = 0.0;
    let mut own = num::pow(f64::from(crate::game::cities::stats::city_strength(g, cap.id())), 1.5);
    for t in g.grid().within(cap.tile(), range) {
        if t == cap.tile() {
            continue;
        }
        let Some(m) = g.military_at(t) else { continue };
        if m.owner() == major {
            near += force(g, m);
        } else if m.owner() == cs {
            own += force(g, m);
        }
    }
    let ratio = near / own.max(1.0);
    let lm = f.tribute_local_modifier;
    let local = if ratio > 3.0 {
        lm
    } else if ratio > 2.0 {
        num::floor_div(lm * 4, 5)
    } else if ratio > 1.5 {
        num::floor_div(lm * 3, 5)
    } else if ratio > 1.0 {
        num::floor_div(lm * 2, 5)
    } else if ratio > 0.5 {
        num::floor_div(lm, 5)
    } else {
        0
    };
    mods.push(("Military near City-State", local));
    mods
}

/// How willing a city-state is to pay a major tribute: positive means it will
/// (`tribute_willingness`, `city_states.py:588-590`).
#[must_use]
pub fn tribute_willingness(g: &Game, cs: PlayerId, major: PlayerId, worker: bool) -> i32 {
    tribute_modifiers(g, cs, major, worker).iter().map(|&(_, v)| v).sum()
}

/// The gold a demand extracts (`tribute_gold_amount`, `city_states.py:660-665`): 50 at the
/// standard speed, and 5 more every so many turns.
#[must_use]
pub fn tribute_gold_amount(g: &Game) -> i32 {
    let sp = g.speed();
    num::trunc_i32(10.0 * sp.gold_gift_modifier) * 5
        + 5 * num::trunc_i32(f64::from(g.turn()) / sp.city_state_tribute_scaling_interval)
}

/// Checks a demand for tribute (`demand_tribute`, `city_states.py:668-675`).
///
/// # Errors
/// No living city-state met, or it is not afraid enough, with the reasons.
pub fn plan_tribute(
    g: &Game,
    major: PlayerId,
    cs: PlayerId,
    worker: bool,
) -> Result<(), ActionError> {
    check_cs(g, major, cs)?;
    let mods = tribute_modifiers(g, cs, major, worker);
    let will: i32 = mods.iter().map(|&(_, v)| v).sum();
    if will <= 0 {
        let list: Vec<String> = mods.iter().map(|&(k, v)| format!("{k} {v:+}")).collect();
        return Err(ActionError::rule(format!(
            "{} refuses to pay tribute (willingness {will}: {}).",
            name(g, cs),
            list.join(", ")
        )));
    }
    Ok(())
}

/// A city-state pays a major tribute, which [`plan_tribute`] allowed (`demand_tribute`,
/// `city_states.py:676-690`): a worker at 50 influence, or gold at 15; either way it has been
/// bullied, and pays nobody again for a while.
pub fn tribute(g: &mut Game, major: PlayerId, cs: PlayerId, worker: bool) -> Value {
    let res = if worker {
        let cap = g.player(cs).and_then(|p| p.capital).and_then(|c| g.city(c)).map(|c| c.tile());
        if let (Some(w), Some(t)) = (g.rules().derived().known.worker, cap) {
            place_unit_near(g, major, w, t);
        }
        refused(add_influence(g, cs, major, -50.0));
        json!({"tribute": "Worker"})
    } else {
        let gold = tribute_gold_amount(g);
        g.add_stat(major, Stat::Gold, f64::from(gold));
        refused(add_influence(g, cs, major, -15.0));
        json!({"tribute": format!("{gold} gold")})
    };
    bullied(g, cs, major);
    if let Some(d) = data_mut(g, cs) {
        d.recently_bullied = 20;
    }
    res
}

/// A city-state was bullied (`_bullied`, `city_states.py:693-707`): its protectors who know the
/// bully think less of it, it remembers for 20 turns, the city-states that wanted it bullied
/// reward the bully, and it cancels the bully's own quests and its investment.
fn bullied(g: &mut Game, cs: PlayerId, bully: PlayerId) {
    let protectors: Vec<PlayerId> =
        data(g, cs).map(|d| d.protectors.iter().collect()).unwrap_or_default();
    for pr in protectors {
        if g.has_met(pr, bully) {
            add_opinion(g, pr, bully, OpinionKey::BulliedProtectedMinor, -15.0);
        }
    }
    if let Some(p) = pair_mut(g, cs, bully) {
        p.bullied = 20;
    }
    let others: Vec<PlayerId> = g.city_states(true).map(Player::id).collect();
    for q in others {
        quest_event(g, q, QuestKind::BullyCityState, bully, Some(cs));
    }
    let r = g.rules();
    let Some(d) = data(g, cs) else { return };
    let keep: Vec<_> = d
        .quests
        .iter()
        .copied()
        .filter(|x| {
            !(x.assignee == bully
                && (x.scope == QuestScope::Individual
                    || r.quests()[x.kind].kind == QuestKind::Invest))
        })
        .collect();
    if keep.len() != d.quests.len() {
        if let Some(d) = data_mut(g, cs) {
            d.quests = keep;
        }
        let text =
            format!("{} cancelled its quests for you because you demanded tribute.", name(g, cs));
        let data = EventData { player: Some(cs), ..EventData::default() };
        g.emit(EngineEvent::CsQuest, &text, Some(PlayerSet::single(bully)), None, data, &[]);
    }
}

// ---- Marriage (city_states.py:768-802) ---------------------------------------------------------------

/// The gold a marriage into a city-state's ruling family costs (`marriage_cost`,
/// `city_states.py:768-774`): 500 at the standard speed, and a twentieth of what each of its
/// units would cost to buy; a multiple of 5.
#[must_use]
pub fn marriage_cost(g: &Game, cs: PlayerId) -> i32 {
    let mut cost = num::trunc_i32(500.0 * g.speed().gold_cost_modifier);
    for u in g.player_units(cs) {
        let each = num::trunc_i32(base_gold_cost(g, cs, Constructible::Unit(u.base), None));
        cost += num::floor_div(each, 20);
    }
    num::floor_div(cost, 5) * 5
}

/// Checks a marriage (`buyout`, `city_states.py:777-789`): a long-time ally of a civilization
/// that `Can spend Gold to annex or puppet a City-State that has been your Ally for [n] turns`,
/// with the gold. Returns the cost.
///
/// # Errors
/// Why it may not.
pub fn plan_marriage(g: &Game, major: PlayerId, cs: PlayerId) -> Result<i32, ActionError> {
    check_cs(g, major, cs)?;
    let ally = data(g, cs).and_then(crate::state::players::CityStateData::ally);
    if ally != Some(major) || relationship(g, cs, major) != super::influence::Relationship::Ally {
        return Err(ActionError::rule("They must be your ally."));
    }
    if !civ_has(g, major, UniqueType::CityStateCanBeBoughtForGold) {
        return Err(ActionError::rule("Your civilization cannot annex city-states."));
    }
    let cooldown = pair(g, cs, major).marriage_cooldown;
    if cooldown > 0 {
        return Err(ActionError::rule(format!("You must be allied for {cooldown} more turns.")));
    }
    let cost = marriage_cost(g, cs);
    if g.player(major).map_or(0.0, |p| p.econ.gold) < f64::from(cost) {
        return Err(ActionError::rule(format!("This costs {cost} gold.")));
    }
    Ok(cost)
}

/// A major marries into a city-state's ruling family (`buyout`, `city_states.py:790-802`): its
/// units and cities become the major's, the cities puppets founded by it.
pub fn marry(g: &mut Game, major: PlayerId, cs: PlayerId, cost: i32) -> Value {
    g.add_stat(major, Stat::Gold, -f64::from(cost));
    let units: Vec<UnitId> = g.player_units(cs).map(crate::state::units::Unit::id).collect();
    for u in units {
        let moved = g.change_unit_owner(u, major);
        debug_assert!(moved.is_ok(), "a unit of the game changes hands: {moved:?}");
    }
    let cities: Vec<_> = g.player_cities(cs).map(crate::state::cities::City::id).collect();
    for c in cities {
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.founder = major;
            x.original_capital = false;
        }
        conquest::move_to_civ(g, c, major);
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.puppet = true;
        }
    }
    // The city-state, left with nothing, is gone (city_states.py:800).
    crate::game::victory::eliminate_if_defeated(g, cs, None);
    let text = format!("{} married into the ruling family of {}.", name(g, major), name(g, cs));
    g.emit(EngineEvent::CsMarried, &text, None, None, EventData::default(), &[]);
    json!({"annexed": name(g, cs), "gold_spent": cost})
}

// ---- The tool (tools.py:998-1033) ---------------------------------------------------------------

/// `city_state_action`: every way of dealing with a city-state, behind one tool
/// (`tools.city_state_action`, `tools.py:998-1033`): `gift_gold` (`amount`), `gift_unit`
/// (`unit_id`), `pledge`, `withdraw`, `tribute_gold` (or `tribute`), `tribute_worker`,
/// `make_peace` or `marry`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CityStateAction {
    pub player_id: i64,
    pub action: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_id: Option<i64>,
}

/// What a [`CityStateAction`] will do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CityStatePlan {
    GiftGold { cs: PlayerId, gold: i32, influence: i32 },
    GiftUnit(UnitGift),
    Pledge(PlayerId),
    Withdraw(PlayerId),
    Tribute { cs: PlayerId, worker: bool },
    Peace(PlayerId),
    Marry { cs: PlayerId, cost: i32 },
}

impl Rule for CityStateAction {
    type Plan = CityStatePlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<CityStatePlan, ActionError> {
        let cs = city_state_of(g, pid, self.player_id)?;
        let action = py::str_of(&self.action).to_lowercase();
        match action.as_str() {
            "gift_gold" => {
                let amount = self
                    .amount
                    .filter(|&a| a != 0)
                    .ok_or_else(|| ActionError::rule("Give an amount of gold."))?;
                let (gold, influence) = plan_gift_gold(g, pid, cs, amount)?;
                Ok(CityStatePlan::GiftGold { cs, gold, influence })
            }
            "gift_unit" => {
                let id =
                    self.unit_id.ok_or_else(|| ActionError::rule("Give the unit_id to gift."))?;
                let u = own_unit(g, pid, id)?;
                Ok(CityStatePlan::GiftUnit(plan_gift_unit(g, pid, u)?))
            }
            "pledge" => plan_pledge(g, pid, cs).map(|()| CityStatePlan::Pledge(cs)),
            "withdraw" => plan_withdraw(g, pid, cs).map(|()| CityStatePlan::Withdraw(cs)),
            "tribute_gold" | "tribute" => plan_tribute(g, pid, cs, false)
                .map(|()| CityStatePlan::Tribute { cs, worker: false }),
            "tribute_worker" => {
                plan_tribute(g, pid, cs, true).map(|()| CityStatePlan::Tribute { cs, worker: true })
            }
            "make_peace" => {
                plan_peace_with_city_state(g, pid, cs).map(|()| CityStatePlan::Peace(cs))
            }
            "marry" => plan_marriage(g, pid, cs).map(|cost| CityStatePlan::Marry { cs, cost }),
            _ => Err(ActionError::new(
                ErrCode::BadParam,
                "action must be gift_gold, gift_unit, pledge, withdraw, tribute_gold, \
                 tribute_worker, make_peace or marry.",
            )),
        }
    }

    fn apply(self, g: &mut Game, pid: PlayerId, plan: CityStatePlan) -> OutcomeSpec {
        OutcomeSpec::value(match plan {
            CityStatePlan::GiftGold { cs, gold, influence } => {
                gift_gold(g, pid, cs, gold, influence)
            }
            CityStatePlan::GiftUnit(gift) => gift_unit(g, pid, gift),
            CityStatePlan::Pledge(cs) => pledge(g, pid, cs),
            CityStatePlan::Withdraw(cs) => withdraw(g, pid, cs),
            CityStatePlan::Tribute { cs, worker } => tribute(g, pid, cs, worker),
            CityStatePlan::Peace(cs) => peace_with_city_state(g, pid, cs),
            CityStatePlan::Marry { cs, cost } => marry(g, pid, cs, cost),
        })
    }
}

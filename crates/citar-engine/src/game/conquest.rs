//! Moving cities between civilizations (`conquest.py:1-288`): a city taken by a melee unit,
//! what changes hands with it, and what its conqueror decides: annex it, keep it as a puppet,
//! raze it, stop razing it, or give it back to the civilization that founded it.
//!
//! A captured city starts as a puppet of its conqueror, or with its seat's automatic conquest
//! decisions, razed or liberated at once as UnCiv's AI decides. The plunder and the buildings
//! lost are drawn from `Purpose::CaptureGold` and `Purpose::CaptureBuildings`, keyed by the city
//! and the turn, as Python's `state_rng` keyed them.
//!
//! Also here, since only a capture reads it: the opinions a capture moves
//! (`diplomacy.on_city_captured`, `diplomacy.py:680-691`).
//!
//! **Deliberate differences** (`tests/rules/intended.toml`): a civilization that `May not annex
//! cities` is refused `annex` (`may-not-annex-refuses-annexing`), where Python only made the
//! cities it captured puppets and let it annex them afterwards.

use serde_json::{Map, Value, json};
use smallvec::SmallVec;

use super::cities::founding::{
    add_building, capital_indicator, equivalent_building, remove_building,
};
use super::cities::free_buildings::try_add_free_buildings;
use super::cities::lifecycle::add_population;
use super::cities::stats::max_health;
use super::cities::uniques::contains_building;
use super::city_states::influence::{influence, set_influence};
use super::combat::resolve::player_name;
use super::core::has_type;
use super::derive::rev::{CityTouch, PlayerTouch};
use super::diplomacy::relations::{add_opinion, make_peace};
use super::error::ActionError;
use super::units::{self, capture::capture_civilian};
use super::{Game, Porting, movement, pending, triggers};
use crate::base::ids::{BuildingId, CityId, PlayerId, UnitId};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::rules::defs::Domain;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::{CityFocus, Constructible};
use crate::state::diplo::{OpinionKey, side};
use crate::state::world::ReligionName;
use crate::unique::trigger::{TriggerEvent, TriggerSite};
use crate::unique::{CombatCtx, Combatant, Ctx, UniqueData, UniqueType, uq};

/// The stream of a capture's draws of one purpose: the city and the turn.
fn stream(g: &Game, purpose: Purpose, c: CityId) -> Rng {
    Rng::keyed(g.state().seed(), purpose, &[u64::from(c.get()), g.turn().key()])
}

/// A city's name, for messages.
fn city_name(g: &Game, c: CityId) -> String {
    g.city(c).map(|x| x.name.to_string()).unwrap_or_default()
}

/// Whether a city is a religion's holy city that still works as one (`religion.is_holy_city`,
/// `religion.py:303-305`): its religion's pressure is not blocked.
#[must_use]
pub fn is_holy_city(g: &Game, c: CityId) -> bool {
    g.city(c)
        .and_then(|x| x.holy_city_of)
        .is_some_and(|r| g.state().world().religion(r).is_none_or(|rel| !rel.blocked_holy))
}

/// Whether a city may be razed or destroyed (`conquest.can_be_destroyed`, `conquest.py:20-29`):
/// never an original capital or a holy city, nor a capital unless it was just captured.
#[must_use]
pub fn can_be_destroyed(g: &Game, c: CityId, just_captured: bool) -> bool {
    let Some(city) = g.city(c) else { return false };
    if city.original_capital || is_holy_city(g, c) {
        return false;
    }
    let capital = g.player(city.owner()).and_then(|p| p.capital);
    just_captured || capital != Some(c)
}

/// The gold plundered when a city falls (`conquest._gold_for_capture`, `conquest.py:32-44`):
/// more for a larger city, less for one its owner took lately, with the city's `[n]% Gold from
/// capturing cities` and the conqueror's `[n]% Gold from Barbarian encampments and pillaging
/// Cities`.
fn gold_for_capture(g: &Game, c: CityId, conqueror: PlayerId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let mut rng = stream(g, Purpose::CaptureGold, c);
    #[allow(clippy::cast_precision_loss, reason = "a draw below 40")]
    let roll = rng.below(40) as f64;
    let base = 20.0 + 10.0 * f64::from(city.pop) + roll;
    let turn_mod = f64::from((g.turn() - city.turn_acquired).clamp(0, 50)) / 50.0;
    let v = g.view();
    let mut city_mod = 1.0f64;
    let ctx = Ctx::city(&v, c);
    for h in uq::city(&v, c, UniqueType::GoldFromCapturingCity, &ctx) {
        if let UniqueData::GoldFromCapturingCity(x) = h.data() {
            for _ in 0..h.n {
                city_mod *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    let mut civ_mod = 1.0f64;
    for h in uq::civ(&v, conqueror, UniqueType::GoldFromEncampmentsAndCities, &Ctx::civ(conqueror))
    {
        if let UniqueData::GoldFromEncampmentsAndCities(x) = h.data() {
            for _ in 0..h.n {
                civ_mod *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    num::trunc_i32(base * turn_mod * city_mod * civ_mod)
}

/// Destroys the buildings a capture does not leave standing (`conquest._destroy_buildings_on_capture`,
/// `conquest.py:47-58`): never a world wonder, a capital's marker, or what is `Never destroyed
/// when the city is captured`; always what is `Destroyed when the city is captured`; anything
/// else one time in three.
fn destroy_buildings_on_capture(g: &mut Game, c: CityId) {
    let Some(buildings) = g.city(c).map(|x| x.buildings) else { return };
    let r = g.rules();
    let mut rng = stream(g, Purpose::CaptureBuildings, c);
    let mut lost: SmallVec<[BuildingId; 8]> = SmallVec::new();
    for b in buildings.iter() {
        let bd = &r.buildings()[b];
        if has_type(r, &bd.uniques, UniqueType::NotDestroyedWhenCityCaptured) || bd.is_wonder {
            continue;
        }
        if has_type(r, &bd.uniques, UniqueType::IndicatesCapital) {
            continue;
        }
        if has_type(r, &bd.uniques, UniqueType::DestroyedWhenCityCaptured) || rng.below(100) < 34 {
            lost.push(b);
        }
    }
    for b in lost {
        remove_building(g, c, b);
    }
}

/// Drops the pressure of pantheons that are not the new owner's (`religion.remove_unknown_pantheons`,
/// `religion.py:230-238`).
fn remove_unknown_pantheons(g: &mut Game, c: CityId) {
    let Some(city) = g.city(c) else { return };
    let owner = city.owner();
    let world = g.state().world();
    let foreign: SmallVec<[_; 2]> = city
        .pressures
        .iter()
        .filter_map(|&(r, _)| r)
        .filter(|&r| {
            world.religion(r).is_some_and(|rel| {
                matches!(rel.name, ReligionName::Pantheon(_)) && rel.founder != owner
            })
        })
        .collect();
    if foreign.is_empty() {
        return;
    }
    if let Some(x) = g.city_mut(c, CityTouch::RELIGION) {
        x.pressures.retain(|(r, _)| !r.is_some_and(|r| foreign.contains(&r)));
    }
}

/// Hands a city to another civilization (`conquest.move_to_civ`, `conquest.py:61-121`): its
/// capital's marker goes, it and its tiles change hands, the old owner's free buildings in it and
/// its national wonders go (but what is never destroyed on capture), a building its new owner may
/// have only so many of goes once it has as many, the old owner moves its capital to its largest
/// city left, and a new owner with no capital makes this one its capital. Its buildings become
/// the new owner's own versions, its pantheons that are not the new owner's lose their pressure,
/// and a civilization that `May not annex cities` keeps it as a puppet.
pub fn move_to_civ(g: &mut Game, c: CityId, new_owner: PlayerId) {
    let Some(old) = g.city(c).map(crate::state::cities::City::owner) else { return };
    let r = g.rules();
    let was_capital = g.player(old).and_then(|p| p.capital) == Some(c);
    let palaces: SmallVec<[BuildingId; 1]> = g.city(c).map_or_else(SmallVec::new, |x| {
        x.buildings
            .iter()
            .filter(|&b| has_type(r, &r.buildings()[b].uniques, UniqueType::IndicatesCapital))
            .collect()
    });
    for b in palaces {
        remove_building(g, c, b);
    }
    let moved = g.transfer_city(c, new_owner);
    debug_assert!(moved.is_ok(), "a city of the game changes hands: {moved:?}");
    let turn = g.turn();
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.turn_acquired = turn;
        x.previous_owner = Some(old);
        x.wltkd = 0;
        x.demanded_resource = None;
    }
    if let Some(x) = g.city_mut(c, CityTouch::BUILDINGS) {
        let free = core::mem::take(&mut x.free_buildings);
        for b in free.iter() {
            x.buildings.remove(b);
        }
    }
    let buildings = g.city(c).map(|x| x.buildings).unwrap_or_default();
    for b in buildings.iter() {
        let bd = &r.buildings()[b];
        if bd.is_national_wonder
            && !has_type(r, &bd.uniques, UniqueType::NotDestroyedWhenCityCaptured)
        {
            remove_building(g, c, b);
            continue;
        }
        let limits: SmallVec<[i32; 1]> = bd
            .uniques
            .ids()
            .filter_map(|id| match r.uniques().get(id).data {
                UniqueData::MaxNumberBuildable(x) => Some(x.limit),
                _ => None,
            })
            .collect();
        for limit in limits {
            let n = g
                .player_cities(new_owner)
                .filter(|x| {
                    contains_building(g, x.id(), b) || x.queue.contains(&Constructible::Building(b))
                })
                .count();
            if i64::try_from(n).unwrap_or(i64::MAX) >= i64::from(limit)
                && g.city(c).is_some_and(|x| x.buildings.contains(b))
            {
                remove_building(g, c, b);
            }
        }
    }
    // espionage.city_removed: the spies in it flee home (conquest.py:94).
    pending(Porting::Pending("1c-05"));
    if was_capital {
        if let Some(p) = g.player_mut(old, PlayerTouch::CAPITAL) {
            p.capital = None;
        }
        // The first of the largest, as Python's max kept.
        let mut newcap: Option<(CityId, u16)> = None;
        for x in g.player_cities(old).filter(|x| x.id() != c) {
            if newcap.is_none_or(|(_, pop)| x.pop > pop) {
                newcap = Some((x.id(), x.pop));
            }
        }
        if let Some((nc, _)) = newcap {
            if let Some(ind) = capital_indicator(g, old) {
                add_building(g, nc, ind, false);
            }
            if let Some(p) = g.player_mut(old, PlayerTouch::CAPITAL) {
                p.capital = Some(nc);
            }
            let text =
                format!("{} moved its capital to {}.", player_name(g, old), city_name(g, nc));
            let at = g.city(nc).map(crate::state::cities::City::tile);
            let audience = core::iter::once(old).collect();
            g.emit(EngineEvent::CapitalMoved, &text, Some(audience), at, EventData::default(), &[]);
        }
    }
    let alone = !g.player_cities(new_owner).any(|x| x.id() != c);
    let capital = g.player(new_owner).and_then(|p| p.capital);
    if alone || capital.and_then(|k| g.city(k)).is_none() {
        if let Some(ind) = capital_indicator(g, new_owner) {
            add_building(g, c, ind, false);
        }
        if let Some(p) = g.player_mut(new_owner, PlayerTouch::CAPITAL) {
            p.capital = Some(c);
        }
    }
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.razing = false;
    }
    let buildings = g.city(c).map(|x| x.buildings).unwrap_or_default();
    for b in buildings.iter() {
        let eb = equivalent_building(g, new_owner, b);
        if eb != b
            && let Some(x) = g.city_mut(c, CityTouch::BUILDINGS)
        {
            x.buildings.remove(b);
            x.buildings.insert(eb);
        }
    }
    if g.religion_enabled() {
        remove_unknown_pantheons(g, c);
    }
    let v = g.view();
    let no_annex =
        uq::any(uq::civ(&v, new_owner, UniqueType::MayNotAnnexCities, &Ctx::civ(new_owner)));
    if no_annex && let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.puppet = true;
        x.queue.clear();
    }
    if let Some(p) = g.player_mut(new_owner, PlayerTouch::OTHER) {
        p.founded_city = true;
    }
    try_add_free_buildings(g, new_owner);
}

/// What happens to a city whenever it changes hands, whatever becomes of it next
/// (`conquest._conquer_common`, `conquest.py:124-150`): the conqueror plunders its gold, some of
/// its buildings are lost, it goes to `receiver` (the conqueror, or the founder it is given back
/// to), it is at half health with its citizens placed afresh, a citizen and a quarter of the rest
/// are lost, and it resists for as many turns as it has citizens unless it was retaken during its
/// resistance or it goes back to its founder. `upon losing a city` fires for the old owner. The
/// gold plundered.
fn conquer_common(g: &mut Game, c: CityId, conqueror: PlayerId, receiver: PlayerId) -> i32 {
    let Some(old) = g.city(c).map(crate::state::cities::City::owner) else { return 0 };
    let gold = gold_for_capture(g, c, conqueror);
    if let Some(p) = g.player_mut(conqueror, PlayerTouch::STOCKS) {
        p.econ.gold += f64::from(gold);
    }
    let retaken = g.city(c).is_some_and(|x| x.previous_owner == Some(receiver) && x.resistance > 0);
    destroy_buildings_on_capture(g, c);
    move_to_civ(g, c, receiver);
    let health = max_health(g, c) / 2;
    if let Some(x) = g.city_mut(c, CityTouch::CORE | CityTouch::WORK) {
        x.health = health;
        x.avoid_growth = false;
        x.focus = CityFocus::Balanced;
        // `assign_citizens(reset=True)`: the old owner's locks go; the settle places them.
        x.locked.clear();
    }
    let pop = g.city(c).map_or(1, |x| i32::from(x.pop));
    if pop > 1 {
        add_population(g, c, -1 - pop / 4);
    }
    let (pop, founder) = g.city(c).map_or((1, receiver), |x| (x.pop, x.founder));
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.resistance = if !retaken && founder != receiver {
            i16::try_from(pop).unwrap_or(i16::MAX)
        } else {
            0
        };
    }
    triggers::fire(g, &TriggerSite::civ(old), &TriggerEvent::LosingCity, true, None);
    // victory.check_elimination(old, by=conqueror) (conquest.py:149).
    pending(Porting::Pending("1c-08"));
    gold
}

/// The opinions a capture moves (`diplomacy.on_city_captured`, `diplomacy.py:680-691`): the old
/// owner's of the conqueror by how much of its people the city held, and every other major's
/// that knows the conqueror, up if it is at war with the old owner and down otherwise.
fn on_city_captured(g: &mut Game, attacker: PlayerId, old: PlayerId, c: CityId) {
    let total: i32 = g.player_cities(old).map(|x| i32::from(x.pop)).sum();
    let total = if total == 0 { 1 } else { total };
    let pop = g.city(c).map_or(0, |x| i32::from(x.pop));
    let aggro = 10.0 + num::round_half_even(f64::from(pop) * 100.0 / f64::from(total));
    add_opinion(g, old, attacker, OpinionKey::CapturedOurCities, -aggro);
    let others: Vec<PlayerId> = g
        .majors(true)
        .map(crate::state::players::Player::id)
        .filter(|&q| q != attacker && q != old)
        .collect();
    for q in others {
        if !g.has_met(q, attacker) {
            continue;
        }
        let share = num::round_half_even(aggro / 10.0);
        if g.at_war(q, old) {
            add_opinion(g, q, attacker, OpinionKey::SharedEnemy, share);
        } else {
            add_opinion(g, q, attacker, OpinionKey::Warmonger, -share);
        }
    }
}

/// A melee unit takes a city with no defences left (`conquest.conquer`, `conquest.py:153-195`):
/// the other side's military units and aircraft in it are lost and its civilians captured; the
/// unit's `Upon capturing a city, receive [n] times its [stat] production as [stat]`; the unit
/// moves in; opinions move; then the city changes hands: an original capital back to its
/// founder, anything else as a puppet, or as the conqueror's automatic decisions have it. `upon
/// conquering a city` fires for the conqueror. What happened, for the attack's result.
pub fn conquer(g: &mut Game, c: CityId, u: UnitId) -> Map<String, Value> {
    let mut result = Map::new();
    let (Some(attacker), Some((old, at))) =
        (g.unit(u).map(crate::state::units::Unit::owner), g.city(c).map(|x| (x.owner(), x.tile())))
    else {
        return result;
    };
    let old_name = player_name(g, old);
    let r = g.rules();
    let here: SmallVec<[(UnitId, bool); 4]> = g
        .units_at(at)
        .filter(|o| o.owner() != attacker)
        .map(|o| {
            let d = &r.base_units()[o.base];
            (o.id(), d.domain == Domain::Air || d.military)
        })
        .collect();
    for (o, fights) in here {
        if fights {
            units::remove_unit(g, o);
        } else {
            capture_civilian(g, u, o);
        }
    }
    let plunder: SmallVec<[(i32, crate::base::stats::Stat, crate::base::stats::Stat); 2]> = {
        let v = g.view();
        let ctx = Ctx {
            civ: Some(attacker),
            city: Some(c),
            unit: Some(u),
            combat: Some(CombatCtx {
                our: Combatant::Unit(u),
                their: None,
                attacked_tile: Some(at),
                action: None,
            }),
            ..Ctx::default()
        }
        .resolve(&v);
        let mut out = SmallVec::new();
        for h in uq::unit_and_civ(&v, u, UniqueType::CaptureCityPlunder, &ctx) {
            if let UniqueData::CaptureCityPlunder(x) = *h.data() {
                out.extend(core::iter::repeat_n((x.times, x.stat, x.into), usize::from(h.n)));
            }
        }
        out
    };
    if !plunder.is_empty() {
        let total = crate::game::query::city_stats(g, c).total;
        for (times, stat, into) in plunder {
            let amount = num::trunc_i32(f64::from(times) * total.get(stat));
            g.add_stat(attacker, into, f64::from(amount));
        }
    }
    let moved = g.relocate_unit(u, at);
    debug_assert!(moved.is_ok(), "the conqueror moves into the city: {moved:?}");
    on_city_captured(g, attacker, old, c);
    let name = city_name(g, c);
    result.insert("captured_city".into(), json!(name));
    result.insert("from".into(), json!(old_name));
    let (original, founder) = g.city(c).map_or((false, old), |x| (x.original_capital, x.founder));
    let gold = conquer_common(g, c, attacker, attacker);
    if original && founder == attacker {
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.puppet = false;
        }
        result.insert("result".into(), json!("recaptured"));
    } else {
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.puppet = true;
            x.queue.clear();
        }
        result.insert("result".into(), json!("puppet"));
        if g.player(attacker).is_some_and(|p| p.seat().auto().conquest) {
            auto_conquer(g, attacker, c);
            let how = match g.city(c) {
                Some(x) if x.razing => "razing",
                Some(x) if !x.puppet => "annexed",
                _ => "puppet",
            };
            result.insert("result".into(), json!(how));
        }
    }
    result.insert("gold_plundered".into(), json!(gold));
    let text = format!(
        "{} captured {name} from {old_name}! ({gold} gold plundered)",
        player_name(g, attacker)
    );
    let data = EventData {
        city: Some(c),
        old_owner: Some(old),
        new_owner: Some(attacker),
        ..EventData::default()
    };
    g.emit(EngineEvent::CityCaptured, &text, None, Some(at), data, &[]);
    let site = TriggerSite { civ: attacker, city: Some(c), unit: Some(u), tile: None };
    triggers::fire(g, &site, &TriggerEvent::ConqueringCity, true, None);
    // victory.check_domination (conquest.py:194).
    pending(Porting::Pending("1c-08"));
    result
}

/// The decision UnCiv's AI takes over a city it has just taken (`conquest._auto_conquer`,
/// `Battle.automateCityConquer`): a dead city-state's city goes back to it if they are at peace;
/// a small city, or any a city-state takes, not its own, is razed where it may be.
fn auto_conquer(g: &mut Game, pid: PlayerId, c: CityId) {
    let Some((founder, pop)) = g.city(c).map(|x| (x.founder, x.pop)) else { return };
    let founder_cs = g.player(founder).is_some_and(crate::state::players::Player::is_city_state);
    let founder_dead = g.player(founder).is_some_and(|p| !p.alive());
    if founder_cs && founder != pid && !g.at_war(pid, founder) && founder_dead {
        if plan_liberate(g, pid, c).is_ok() {
            liberate(g, pid, c);
        }
        return;
    }
    let cs = g.player(pid).is_some_and(crate::state::players::Player::is_city_state);
    if (pop < 4 || cs)
        && founder != pid
        && can_be_destroyed(g, c, true)
        && let Some(x) = g.city_mut(c, CityTouch::CORE)
    {
        x.puppet = false;
        x.razing = true;
    }
}

// ---- What the conqueror decides (conquest.py:209-288, tools.py:816-830) -----------------------

/// A conquered city's fate, as `city_status` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CityFate {
    Annex,
    Puppet,
    Raze,
    StopRazing,
    Liberate,
}

impl CityFate {
    /// The fate a `status` names.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "annex" => Self::Annex,
            "puppet" => Self::Puppet,
            "raze" => Self::Raze,
            "stop_razing" => Self::StopRazing,
            "liberate" => Self::Liberate,
            _ => return None,
        })
    }
}

/// Whether `pid` may give city `c` this fate (the checks of `conquest.annex`, `puppet`, `raze`
/// and `liberate`, `conquest.py:209-260`).
///
/// # Errors
/// The refusal, with Python's text: annexing what is not a puppet, or a civilization that may
/// not annex; a puppet of a city it founded; razing an original capital, a holy city or a city
/// it founded; liberating a city it founded, one never captured, or the barbarians'.
pub fn plan_fate(g: &Game, pid: PlayerId, c: CityId, fate: CityFate) -> Result<(), ActionError> {
    let city = g.city(c).ok_or_else(|| ActionError::rule("That is not your city."))?;
    if city.owner() != pid {
        return Err(ActionError::rule("That is not your city."));
    }
    match fate {
        CityFate::Annex => {
            if !city.puppet {
                return Err(ActionError::rule(format!("{} is not a puppet.", city.name)));
            }
            // refcheck: may-not-annex-refuses-annexing
            let v = g.view();
            if uq::any(uq::civ(&v, pid, UniqueType::MayNotAnnexCities, &Ctx::civ(pid))) {
                return Err(ActionError::rule(format!(
                    "{} may not annex cities.",
                    player_name(g, pid)
                )));
            }
        }
        CityFate::Puppet => {
            if city.founder == pid {
                return Err(ActionError::rule("You cannot make a puppet of a city you founded."));
            }
        }
        CityFate::Raze => {
            if !can_be_destroyed(g, c, true) {
                return Err(ActionError::rule(format!(
                    "{} cannot be razed (original capitals and holy cities cannot be razed).",
                    city.name
                )));
            }
            if city.founder == pid {
                return Err(ActionError::rule("You can only raze cities you captured."));
            }
        }
        CityFate::StopRazing => {}
        CityFate::Liberate => plan_liberate(g, pid, c)?,
    }
    Ok(())
}

/// Gives city `c` the fate [`plan_fate`] allowed, and says what was done.
pub fn apply_fate(g: &mut Game, pid: PlayerId, c: CityId, fate: CityFate) -> Value {
    let name = city_name(g, c);
    match fate {
        CityFate::Annex => {
            if let Some(x) = g.city_mut(c, CityTouch::CORE | CityTouch::WORK) {
                x.puppet = false;
                x.avoid_growth = false;
                x.focus = CityFocus::Balanced;
            }
            json!({ "annexed": name })
        }
        CityFate::Puppet => {
            if let Some(x) = g.city_mut(c, CityTouch::CORE) {
                x.puppet = true;
                x.queue.clear();
            }
            json!({ "puppet": name })
        }
        CityFate::Raze => {
            let mut turns = 0;
            if let Some(x) = g.city_mut(c, CityTouch::CORE) {
                x.razing = true;
                x.puppet = false;
                turns = x.pop;
            }
            json!({ "razing": name, "turns": turns })
        }
        CityFate::StopRazing => {
            if let Some(x) = g.city_mut(c, CityTouch::CORE) {
                x.razing = false;
            }
            json!({ "stopped_razing": name })
        }
        CityFate::Liberate => liberate(g, pid, c),
    }
}

/// Whether `pid` may give city `c` back to its founder (`conquest.liberate`'s checks,
/// `conquest.py:250-260`).
///
/// # Errors
/// The refusal, with Python's text.
pub fn plan_liberate(g: &Game, pid: PlayerId, c: CityId) -> Result<(), ActionError> {
    let city = g.city(c).ok_or_else(|| ActionError::rule("That is not your city."))?;
    if city.owner() != pid {
        return Err(ActionError::rule("That is not your city."));
    }
    if city.founder == pid || city.previous_owner.is_none() {
        return Err(ActionError::rule(
            "Only cities captured from another civilization can be liberated.",
        ));
    }
    if g.is_barbarian(city.founder) {
        return Err(ActionError::rule("That city cannot be liberated."));
    }
    Ok(())
}

/// Gives a captured city back to the civilization that founded it (`conquest.liberate`,
/// `conquest.py:250-288`, `CityConquestFunctions.liberateCity`): a founder that was eliminated
/// comes back; the city changes hands as a capture does and is no puppet, and is its founder's
/// capital if it is its only city. A major founder makes peace, thinks better of its liberator
/// and opens its borders to it; a city-state makes its liberator its best friend and makes
/// peace. Units of anyone else in the city leave it.
pub fn liberate(g: &mut Game, pid: PlayerId, c: CityId) -> Value {
    let Some((founder, at)) = g.city(c).map(|x| (x.founder, x.tile())) else { return Value::Null };
    if g.player(founder).is_some_and(|p| !p.alive()) {
        let revived = g.revive_player(founder);
        debug_assert!(revived.is_ok(), "a player of the game comes back: {revived:?}");
    }
    conquer_common(g, c, pid, founder);
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.puppet = false;
    }
    if g.player_cities(founder).count() == 1 {
        if let Some(ind) = capital_indicator(g, founder) {
            add_building(g, c, ind, false);
        }
        if let Some(p) = g.player_mut(founder, PlayerTouch::CAPITAL) {
            p.capital = Some(c);
        }
    }
    let pop = g.city(c).map_or(0, |x| x.pop);
    if g.player(founder).is_some_and(crate::state::players::Player::is_major) {
        if g.at_war(pid, founder) {
            let peace = make_peace(g, pid, founder);
            debug_assert!(peace.is_ok(), "two players of the game make peace: {peace:?}");
        }
        add_opinion(g, founder, pid, OpinionKey::LiberatedCity, 10.0 + 10.0 * f64::from(pop));
        let until = g.turn() + g.speed().deal_duration;
        let open = g.update_relation(founder, pid, |r| {
            r.open_borders_until[side(founder, pid)] = until;
        });
        debug_assert!(open.is_ok(), "two players of the game open borders: {open:?}");
    } else {
        // Python's `max(others, default=-60)`: the others' best, which may be below -60.
        let best = g
            .majors(true)
            .map(crate::state::players::Player::id)
            .filter(|&q| q != pid)
            .map(|q| influence(g, founder, q))
            .reduce(f64::max)
            .unwrap_or(-60.0);
        let new = best.max(influence(g, founder, pid)) + 105.0;
        let set = set_influence(g, founder, pid, new.max(60.0));
        debug_assert!(set.is_ok(), "a city-state of the game takes influence: {set:?}");
        if g.at_war(pid, founder) {
            let peace = make_peace(g, pid, founder);
            debug_assert!(peace.is_ok(), "two players of the game make peace: {peace:?}");
        }
    }
    let others: SmallVec<[UnitId; 4]> = g
        .units_at(at)
        .filter(|o| o.owner() != founder)
        .map(crate::state::units::Unit::id)
        .collect();
    for o in others {
        movement::teleport_to_closest(g, o);
    }
    let name = city_name(g, c);
    let text = format!(
        "{} liberated {name}, returning it to {}!",
        player_name(g, pid),
        player_name(g, founder)
    );
    g.emit(EngineEvent::Liberated, &text, None, Some(at), EventData::default(), &[]);
    json!({ "liberated": name, "returned_to": player_name(g, founder) })
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::base::ids::TileIdx;
    use crate::game::core::testing;
    use crate::game::diplomacy::relations::{WarReason, set_war};

    fn at(x: u32, y: u32) -> TileIdx {
        TileIdx(y * u32::from(testing::W) + x)
    }

    fn clean(g: &mut Game) {
        g.settle();
        let v = g.take_violations();
        assert!(v.is_empty(), "{v:?}");
        assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
        assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
    }

    /// Takes city `c` with a warrior of `by` standing beside it, its defences down.
    fn take(g: &mut Game, by: PlayerId, c: CityId, from: TileIdx) {
        let w = testing::unit(g, by, "Warrior", from);
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.health = 1;
        }
        let out = conquer(g, c, w);
        assert!(out.get("captured_city").and_then(Value::as_str).is_some());
    }

    /// Liberating the last city of a civilization that lost it brings the civilization back, as
    /// package 1c-08's end of the round will have eliminated it (`conquest.py:262-265`): alive,
    /// with the city its capital, at peace with its liberator.
    #[test]
    fn liberating_a_civilization_s_last_city_revives_it() {
        let mut g = testing::duel();
        let (rome, greece) = (PlayerId(0), PlayerId(1));
        let roma = crate::game::cities::founding::found_city(&mut g, rome, at(3, 3), Some("Roma"))
            .expect("a city");
        crate::game::cities::founding::found_city(&mut g, greece, at(8, 3), Some("Athens"))
            .expect("a city");
        set_war(&mut g, greece, rome, WarReason::Scenario).expect("war");
        clean(&mut g);
        take(&mut g, greece, roma, at(4, 3));
        clean(&mut g);
        assert_eq!(g.city(roma).map(crate::state::cities::City::owner), Some(greece));
        // Rome is eliminated as the round ends, on another's turn.
        let mut clock = *g.state().clock();
        clock.current = greece;
        g.set_clock(clock);
        g.kill_player(rome).expect("a player with no city is eliminated");
        clean(&mut g);
        assert!(!g.player(rome).is_some_and(crate::state::players::Player::alive));
        plan_liberate(&g, greece, roma).expect("it may be liberated");
        let out = liberate(&mut g, greece, roma);
        clean(&mut g);
        assert_eq!(out["returned_to"], "Rome");
        let p = g.player(rome).expect("Rome");
        assert!(p.alive());
        assert_eq!((p.capital, p.eliminated_turn()), (Some(roma), None));
        let city = g.city(roma).expect("Roma");
        assert_eq!((city.owner(), city.puppet, city.resistance), (rome, false, 0));
        assert!(!g.at_war(rome, greece));
        assert!(city.buildings.iter().any(|b| {
            has_type(g.rules(), &g.rules().buildings()[b].uniques, UniqueType::IndicatesCapital)
        }));
    }

    /// A liberated city-state makes its liberator its best friend: 105 influence above the best
    /// any other major has, and at least 60 (`conquest.py:277-282`).
    #[test]
    fn a_liberated_city_state_counts_its_liberator_its_best_friend() {
        let mut g = testing::duel();
        let (greece, geneva) = (PlayerId(1), PlayerId(2));
        let town =
            crate::game::cities::founding::found_city(&mut g, geneva, at(3, 3), Some("Geneva"))
                .expect("a city");
        set_war(&mut g, greece, geneva, WarReason::Scenario).expect("war");
        take(&mut g, greece, town, at(4, 3));
        clean(&mut g);
        assert_eq!(g.city(town).map(|x| x.puppet), Some(true));
        plan_liberate(&g, greece, town).expect("it may be liberated");
        liberate(&mut g, greece, town);
        clean(&mut g);
        assert_eq!(g.city(town).map(crate::state::cities::City::owner), Some(geneva));
        assert!(!g.at_war(greece, geneva));
        // Rome, the other major, has no influence; Greece had its floor at war.
        let got = crate::game::city_states::influence::raw_influence(&g, geneva, greece);
        assert!((got - 105.0).abs() < 1e-9, "{got}");
    }
}

//! Nuclear weapons (`combat.py:1076-1215`): damage in a radius, cities crippled or destroyed,
//! tiles wrecked and left with fallout, and the diplomatic cost.
//!
//! A detonation is one combat event: it takes the next `combat_seq` and draws everything it
//! rolls (population lost, units' damage, features destroyed, fallout) from `Purpose::Nuke`,
//! tile by tile in the grid's order; each interception on the way is an event of its own.
//! Fallout is a feature (DESIGN.md 4.3), which a nuke adds as Python's did.

use serde_json::{Value, json};

use super::air::try_intercept;
use super::resolve::{can_attack_now, kill_unit, player_name, stream};
use crate::base::ids::{PlayerId, TileIdx, UnitId};
use crate::base::rng::{Purpose, Rng};
use crate::base::sets::PlayerSet;
use crate::game::cities::lifecycle::{add_population, destroy_city};
use crate::game::core::has_type;
use crate::game::derive::rev::{CityTouch, UnitTouch};
use crate::game::diplomacy::relations::{WarReason, add_opinion, set_war};
use crate::game::error::ActionError;
use crate::game::units::{self, health};
use crate::game::{Game, conquest, economy};
use crate::rules::defs::Domain;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::diplo::OpinionKey;
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// A checked detonation: its strength and the tiles it hits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NukePlan {
    pub strength: i32,
    pub hit: Vec<TileIdx>,
}

/// The players with a tile or a unit among `hit`, in id order.
fn touched(g: &Game, hit: &[TileIdx]) -> PlayerSet {
    let mut out = PlayerSet::default();
    for &t in hit {
        if let Some(o) = g.tile(t).and_then(crate::state::map::Tile::owner) {
            out.insert(o);
        }
        for u in g.units_at(t) {
            out.insert(u.owner());
        }
    }
    out
}

/// Checks a nuclear strike on tile `t` (`combat.nuke`'s checks, `combat.py:1079-1109`): nuclear
/// weapons are on, the unit is one, the target is another explored tile in range, the unit can
/// attack now, and no civilization it would hit holds a peace treaty with its owner.
///
/// # Errors
/// The refusal, with Python's text.
pub fn plan_nuke(g: &Game, u: UnitId, t: TileIdx) -> Result<NukePlan, ActionError> {
    if !g.nukes_enabled() {
        return Err(ActionError::rule("Nuclear weapons are disabled in this game."));
    }
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    let strength = uq::unit(&v, u, UniqueType::NuclearWeapon, &ctx).find_map(|h| match h.data() {
        UniqueData::NuclearWeapon(n) => Some(n.strength),
        _ => None,
    });
    let Some(strength) = strength else {
        return Err(ActionError::rule("This unit is not a nuclear weapon."));
    };
    if x.tile() == t {
        return Err(ActionError::rule("A nuke cannot target its own tile."));
    }
    if !g.player(x.owner()).is_some_and(|p| p.explored.contains(t.0)) {
        return Err(ActionError::rule("Target an explored tile."));
    }
    let range = health::attack_range(g, u);
    if i64::from(g.grid().distance(x.tile(), t)) > i64::from(range) {
        return Err(ActionError::rule(format!("Target out of range ({range}).")));
    }
    if let Some(why) = can_attack_now(g, u) {
        return Err(ActionError::rule(why));
    }
    let radius = uq::unit(&v, u, UniqueType::BlastRadius, &ctx)
        .find_map(|h| match h.data() {
            UniqueData::BlastRadius(b) => Some(b.radius),
            _ => None,
        })
        .unwrap_or(2);
    let hit = g.grid().within(t, u32::try_from(radius).unwrap_or(0));
    let pid = x.owner();
    for civ in touched(g, &hit).iter() {
        if civ == pid || g.is_barbarian(civ) {
            continue;
        }
        if g.has_met(pid, civ)
            && !g.at_war(pid, civ)
            && g.relation(pid, civ).is_some_and(|r| r.treaty_until >= g.turn())
        {
            return Err(ActionError::rule(format!(
                "You have a peace treaty with {} and cannot attack them yet.",
                player_name(g, civ)
            )));
        }
    }
    Ok(NukePlan { strength, hit })
}

/// Detonates a nuclear weapon (`combat.nuke`, `combat.py:1110-1141`): war on every civilization
/// met and hit that it is not already at war with; an aircraft meets each hit civilization's
/// interceptors, and one shot down ends it; then every tile hit, at half strength when the
/// weapon lacks its resource; the unit is spent; and every major that knows its owner thinks
/// worse of it.
pub fn nuke(g: &mut Game, u: UnitId, t: TileIdx, plan: NukePlan) -> Value {
    let Some((pid, base)) = g.unit(u).map(|x| (x.owner(), x.base)) else { return Value::Null };
    let what = g.rules().name(base).unwrap_or("").to_owned();
    let mut declared: Vec<PlayerId> = Vec::new();
    for civ in touched(g, &plan.hit).iter() {
        if civ == pid || g.is_barbarian(civ) || !g.has_met(pid, civ) || g.at_war(pid, civ) {
            continue;
        }
        // diplomacy.declare_war(via_nuke=True): only the living can be declared on.
        if !g.player(civ).is_some_and(crate::state::players::Player::alive) {
            continue;
        }
        let war = set_war(g, pid, civ, WarReason::Direct);
        debug_assert!(war.is_ok(), "two players of the game go to war: {war:?}");
        let text = format!("{} declared war on {}!", player_name(g, pid), player_name(g, civ));
        let data = EventData { attacker: Some(pid), defender: Some(civ), ..EventData::default() };
        g.emit(EngineEvent::WarDeclared, &text, None, None, data, &[]);
        declared.push(civ);
    }
    let names = |g: &Game, v: &[PlayerId]| -> Vec<String> {
        v.iter().map(|&p| player_name(g, p)).collect()
    };
    if g.rules().base_units()[base].domain == Domain::Air {
        let mut owners = PlayerSet::default();
        for &h in &plan.hit {
            for o in g.units_at(h) {
                if o.owner() != pid {
                    owners.insert(o.owner());
                }
            }
        }
        for civ in owners.iter() {
            try_intercept(g, u, t, civ, None);
            if g.unit(u).is_some_and(|x| x.hp <= 0) {
                let text = format!("{}'s {what} was shot down.", player_name(g, pid));
                kill_unit(g, u, Some(civ), &text);
                return json!({ "nuke": "intercepted", "declared_war_on": names(g, &declared) });
            }
        }
    }
    let lacking = match g.rules().base_units()[base].required_resource {
        Some(r) if economy::resource_amount(g, pid, r) < 0 => 0.5,
        _ => 1.0,
    };
    let mut rng = stream(g, Purpose::Nuke, u64::from(u.get()), u64::from(t.0));
    for &h in &plan.hit {
        nuke_tile(g, &mut rng, pid, h, plan.strength, h == t, lacking);
    }
    let text = format!("{} detonated a {what} at {}!", player_name(g, pid), g.fmt_xy(t));
    let data = EventData { player: Some(pid), ..EventData::default() };
    g.emit(EngineEvent::Nuke, &text, None, Some(t), data, &[]);
    if g.unit(u).is_some() {
        if units::unit_has(g, u, UniqueType::SelfDestructs, false) {
            units::remove_unit(g, u);
        } else if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
            x.attacks = x.attacks.saturating_add(1);
            x.moves = 0;
        }
    }
    let others: Vec<PlayerId> =
        g.majors(true).map(crate::state::players::Player::id).filter(|&q| q != pid).collect();
    for q in others {
        if g.has_met(q, pid) {
            add_opinion(g, q, pid, OpinionKey::UsedNukes, -50.0);
        }
    }
    g.settle_sight();
    let (x, y) = g.xy(t);
    json!({ "nuke": what, "target": { "x": x, "y": y }, "declared_war_on": names(g, &declared) })
}

/// A nuclear strike on one tile (`combat._nuke_tile`, `combat.py:1144-1200`): a city is
/// destroyed outright when the strike is strong enough and it may be, and otherwise loses health
/// and population; every unit there is damaged (a civilian left at 40 health or less is lost);
/// and a tile with no city loses its features the strike may destroy, or at ground zero or on
/// an even roll is wrecked and left with fallout.
fn nuke_tile(
    g: &mut Game,
    rng: &mut Rng,
    pid: PlayerId,
    t: TileIdx,
    strength: i32,
    ground_zero: bool,
    lacking: f64,
) {
    let mut garrison = 1.0f64;
    if let Some(c) = g.city_at(t).map(crate::state::cities::City::id) {
        let (garrison_mods, pop_mods) = {
            let v = g.view();
            let ctx = Ctx::city(&v, c);
            let f = g.rules().uniques().filters();
            let mut gm = Vec::new();
            for h in uq::city(&v, c, UniqueType::GarrisonDamageFromNukes, &ctx) {
                if let UniqueData::GarrisonDamageFromNukes(x) = h.data()
                    && f.city_matches(x.cities, &v, c, None)
                {
                    gm.extend(core::iter::repeat_n(x.percent, usize::from(h.n)));
                }
            }
            let mut pm = Vec::new();
            for h in uq::city(&v, c, UniqueType::PopulationLossFromNukes, &ctx) {
                if let UniqueData::PopulationLossFromNukes(x) = h.data()
                    && f.city_matches(x.cities, &v, c, None)
                {
                    pm.extend(core::iter::repeat_n(x.percent, usize::from(h.n)));
                }
            }
            (gm, pm)
        };
        for p in garrison_mods {
            garrison *= 1.0 + f64::from(p) / 100.0;
        }
        let pop = g.city(c).map_or(0, |x| i32::from(x.pop));
        if (strength > 2 || (strength > 1 && pop < 5)) && conquest::can_be_destroyed(g, c, true) {
            destroy_city(g, c);
        } else {
            if let Some(x) = g.city_mut(c, CityTouch::CORE) {
                let lost = crate::base::num::trunc_i32(f64::from(x.health) * 0.5 * lacking);
                x.health = (x.health - lost).max(1);
            }
            let mut pop_mod = 1.0f64;
            for p in pop_mods {
                pop_mod *= 1.0 + f64::from(p) / 100.0;
            }
            let draw = |rng: &mut Rng, n: u64| {
                #[allow(clippy::cast_precision_loss, reason = "a draw below 20")]
                let x = rng.below(n) as f64;
                x
            };
            let frac = match strength {
                0 => 0.0,
                1 => (30.0 + draw(rng, 20) + draw(rng, 20)) / 100.0,
                2 => (60.0 + draw(rng, 10) + draw(rng, 10)) / 100.0,
                _ => 1.0,
            };
            let loss = crate::base::num::trunc_i32(f64::from(pop) * pop_mod * frac);
            if loss != 0 {
                add_population(g, c, -loss);
            }
        }
    }
    let here: Vec<UnitId> = g.units_at(t).map(crate::state::units::Unit::id).collect();
    for o in here {
        let Some((owner, base, hp)) = g.unit(o).map(|x| (x.owner(), x.base, i32::from(x.hp)))
        else {
            continue;
        };
        let dmg = if ground_zero || strength >= 2 {
            100
        } else if strength == 1 {
            30 + i32::try_from(rng.below(40) + rng.below(40)).unwrap_or(0)
        } else {
            20 + i32::try_from(rng.below(30)).unwrap_or(0)
        };
        let dmg = crate::base::num::trunc_i32(f64::from(dmg) * garrison * lacking + 1e-6);
        if !g.rules().base_units()[base].military {
            if hp - dmg <= 40 {
                units::remove_unit(g, o);
            }
        } else if hp - dmg <= 0 {
            if let Some(x) = g.unit_mut(o, UnitTouch::CORE) {
                x.hp = 0;
            }
            let what = g.rules().name(base).unwrap_or("");
            let text = format!("A nuclear blast destroyed {}'s {what}.", player_name(g, owner));
            kill_unit(g, o, Some(pid), &text);
        } else if let Some(x) = g.unit_mut(o, UnitTouch::CORE) {
            x.hp = i16::try_from(hp - dmg).unwrap_or(1);
        }
    }
    if g.city_at(t).is_some() {
        return;
    }
    let Some(tile) = g.tile(t) else { return };
    let r = g.rules();
    let destroyable: Vec<(crate::base::ids::FeatureId, i32)> = tile
        .features()
        .iter()
        .filter_map(|f| {
            let terrain = *r.derived().features.get(f)?;
            r.terrains()[terrain].uniques.ids().find_map(|id| match r.uniques().get(id).data {
                UniqueData::DestroyableByNukesChance(x) => Some((f, x.percent)),
                _ => None,
            })
        })
        .collect();
    if destroyable.is_empty() {
        if ground_zero || rng.unit() < 0.5 {
            pillage_and_fallout(g, t);
        }
        return;
    }
    for (f, percent) in destroyable {
        let ch = f64::from(percent) / 100.0;
        if (ch > 0.0 && ground_zero) || rng.unit() < ch {
            if let Some(mut fs) = g.tile(t).map(crate::state::map::Tile::features) {
                fs.remove(f);
                let set = g.set_features(t, fs);
                debug_assert!(set.is_ok(), "a tile of the map takes its features: {set:?}");
            }
            pillage_and_fallout(g, t);
        }
    }
}

/// Wrecks a tile's improvement and route and leaves fallout (`combat._pillage_and_fallout`,
/// `combat.py:1203-1215`): an improvement that cannot be removed stays, one that cannot be
/// pillaged goes, any other is pillaged; no fallout on water, impassable land or fallout.
fn pillage_and_fallout(g: &mut Game, t: TileIdx) {
    let Some(tile) = g.tile(t) else { return };
    let r = g.rules();
    let (route_pillaged, mut imp_pillaged) = (tile.route_pillaged(), tile.improvement_pillaged());
    let mut clear = false;
    if let Some(imp) = tile.improvement().filter(|_| !tile.improvement_pillaged()) {
        let uniques = &r.improvements()[imp].uniques;
        if !has_type(r, uniques, UniqueType::Irremovable) {
            if has_type(r, uniques, UniqueType::Unpillagable) {
                clear = true;
            } else {
                imp_pillaged = true;
            }
        }
    }
    let route = tile.route().is_some();
    if clear {
        let set = g.set_improvement(t, None);
        debug_assert!(set.is_ok(), "a tile of the map takes its improvement: {set:?}");
    }
    if (route && !route_pillaged)
        || imp_pillaged != g.tile(t).is_some_and(|x| x.improvement_pillaged())
    {
        let set = g.set_pillaged(t, route || route_pillaged, imp_pillaged && !clear);
        debug_assert!(set.is_ok(), "a tile of the map takes its pillage: {set:?}");
    }
    if g.is_water(t) || crate::game::path::node::impassable(g, t) {
        return;
    }
    let fallout = r.derived().known.fallout;
    if let Some(mut fs) = g.tile(t).map(crate::state::map::Tile::features)
        && !fs.contains(fallout)
    {
        fs.insert(fallout);
        let set = g.set_features(t, fs);
        debug_assert!(set.is_ok(), "a tile of the map takes fallout: {set:?}");
    }
}

//! Strengths, modifiers and damage (`combat.py:146-420`).
//!
//! A fight's numbers are gathered once into a [`CombatSetup`] ([`setup`]): both sides' strength
//! before modifiers, every modifier with its source, the final strengths and the wounded ratios.
//! The damage at any roll is then closed-form ([`CombatSetup::damage_to_defender`]). Python
//! rebuilt the modifier stacks at every call, about six times for one preview
//! (`combat.py:512-538`), and again for each of the two rolls of a fight.
//!
//! A modifier is keyed by the name of what grants it (`_src`, `combat.py:141-143`): the object a
//! unique came from, or one of the fixed names ("Flanking", "Tile", ...), and kept in the order
//! it was first given, as the preview lists them.

use smallvec::SmallVec;

use super::combatant;
use crate::base::ids::{CityId, PlayerId, TileIdx, UniqueId, UnitId};
use crate::base::num;
use crate::game::Game;
use crate::game::economy;
use crate::game::units::{self, unit_has};
use crate::rules::defs::Domain;
use crate::state::units::Activity;
use crate::unique::world::CombatAction;
use crate::unique::{CombatCtx, Combatant, Ctx, UniqueData, UniqueType, uq};

/// The malus of a unit attacking from the water onto land, or an embarked one attacking land
/// (`combat.LANDING_MALUS`).
pub const LANDING_MALUS: i32 = -50;
/// The malus of a land unit attacking a ship from the shore (`combat.BOARDING_MALUS`).
pub const BOARDING_MALUS: i32 = -50;
/// The malus of a melee attack across a river (`combat.RIVER_MALUS`).
pub const RIVER_MALUS: i32 = -20;
/// The bonus per friendly melee unit next to the defender (`combat.FLANKING`).
pub const FLANKING: f64 = 10.0;
/// The malus of a unit whose strategic resource is short (`combat.MISSING_RESOURCE_MALUS`).
pub const MISSING_RESOURCE_MALUS: i32 = -25;
/// The bonus per turn fortified, up to two (`combat.FORTIFICATION`).
pub const FORTIFICATION: i32 = 20;
/// How much of its damage a unit loses per point of health lost (`combat.WOUNDED_RATIO`).
pub const WOUNDED_RATIO: f64 = 300.0;
/// What a civilian takes from any attack (`combat.DAMAGE_TO_CIVILIAN`).
pub const DAMAGE_TO_CIVILIAN: i32 = 40;

/// Every modifier of one side of a fight, in percent, keyed by the name of what grants it, in the
/// order each was first given (Python's dict).
pub type Mods = SmallVec<[(&'static str, i32); 8]>;

/// Adds to a modifier (`mods[k] = mods.get(k, 0) + v`).
fn add(m: &mut Mods, k: &'static str, v: i32) {
    match m.iter_mut().find(|(n, _)| *n == k) {
        Some(e) => e.1 = e.1.saturating_add(v),
        None => m.push((k, v)),
    }
}

/// Sets a modifier, in its first place if it was given before (`mods[k] = v`).
fn put(m: &mut Mods, k: &'static str, v: i32) {
    match m.iter_mut().find(|(n, _)| *n == k) {
        Some(e) => e.1 = v,
        None => m.push((k, v)),
    }
}

/// The sum of a side's modifiers.
#[must_use]
pub fn total(m: &Mods) -> i32 {
    m.iter().fold(0i32, |a, &(_, v)| a.saturating_add(v))
}

/// A side's modifiers as one multiplier (`combat._final`, `combat.py:361-363`).
#[must_use]
pub fn multiplier(m: &Mods) -> f64 {
    1.0 + f64::from(total(m)) / 100.0
}

/// The name of what granted a unique, which keys its modifier (`combat._src`).
fn src(g: &Game, id: UniqueId) -> &'static str {
    let r = g.rules();
    economy::source_name(r, r.uniques().meta(id).source)
}

/// The context a unique is asked in for this fight, from one side (`combat._ctx`,
/// `combat.py:133-138`): its owner, its city or unit, on its tile, with the tile under attack
/// being the enemy's when it attacks and its own when it defends, unless given.
#[must_use]
pub fn fight_ctx(
    g: &Game,
    ours: Combatant,
    theirs: Option<Combatant>,
    action: CombatAction,
    attacked: Option<TileIdx>,
) -> Ctx {
    let at = attacked.or_else(|| match action {
        CombatAction::Attack => theirs.map(|t| combatant::tile(g, t)),
        CombatAction::Defend => Some(combatant::tile(g, ours)),
    });
    let (city, unit) = match ours {
        Combatant::City(c) => (Some(c), None),
        Combatant::Unit(u) => (None, Some(u)),
    };
    Ctx {
        civ: Some(combatant::owner(g, ours)),
        city,
        unit,
        tile: Some(combatant::tile(g, ours)),
        combat: Some(CombatCtx {
            our: ours,
            their: theirs,
            attacked_tile: at,
            action: Some(action),
        }),
        ignore_conditionals: false,
    }
}

// ---- Strength before modifiers --------------------------------------------------------------

/// A city's combat strength (`combat.city_strength`, `combat.py:149-174`): its base, its
/// population, its terrain, its owner's share of the techs, its garrison, its buildings (times
/// the owner's `[n]% Strength for cities`) and `[n] Strength`, asked in a fight against
/// `theirs` in which the city attacks or defends.
#[must_use]
pub fn city_strength(g: &Game, c: CityId, theirs: Option<Combatant>, action: CombatAction) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let r = g.rules();
    let k = &r.constants().formulas;
    let v = g.view();
    let mut s = k.city_strength_base + f64::from(city.pop) * k.city_strength_per_pop;
    for h in uq::terrains(&v, city.tile(), UniqueType::GrantsCityStrength, &Ctx::IGNORE) {
        if let UniqueData::GrantsCityStrength(x) = h.data() {
            s += f64::from(x.strength);
        }
    }
    let n_techs = r.techs().len();
    #[allow(clippy::cast_precision_loss, reason = "tech counts are far below 2^52")]
    let pct = if n_techs == 0 {
        0.5
    } else {
        g.player(city.owner()).map_or(0, |p| p.tech.known.len()) as f64 / n_techs as f64
    };
    s += num::pow(pct * k.city_strength_from_techs_multiplier, k.city_strength_from_techs_exponent)
        * k.city_strength_from_techs_full_multiplier;
    if let Some(m) = g.military_at(city.tile()) {
        s += f64::from(r.base_units()[m.base].strength)
            * (f64::from(m.hp) / 100.0)
            * k.city_strength_from_garrison;
    }
    let mut bs: f64 = city.buildings.iter().map(|b| r.buildings()[b].city_strength).sum();
    let ctx = Ctx {
        civ: Some(city.owner()),
        city: Some(c),
        tile: Some(city.tile()),
        combat: Some(CombatCtx {
            our: Combatant::City(c),
            their: theirs,
            attacked_tile: None,
            action: Some(action),
        }),
        ..Ctx::default()
    };
    for h in uq::civ(&v, city.owner(), UniqueType::BetterDefensiveBuildings, &ctx) {
        if let UniqueData::BetterDefensiveBuildings(x) = h.data() {
            for _ in 0..h.n {
                bs *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    s += bs;
    for h in uq::city(&v, c, UniqueType::StrengthAmount, &ctx) {
        if let UniqueData::StrengthAmount(x) = h.data() {
            s += f64::from(x.strength) * f64::from(h.n);
        }
    }
    num::round_half_even_i32(s)
}

/// A unit's `[n] Strength` in a fight, its own uniques only (`combat.py:183, 196`).
fn strength_amount(g: &Game, u: UnitId, ctx: &Ctx) -> i32 {
    let v = g.view();
    uq::sum_i32(uq::unit(&v, u, UniqueType::StrengthAmount, ctx), |d| match d {
        UniqueData::StrengthAmount(x) => Some(x.strength),
        _ => None,
    })
}

/// Attack strength before modifiers (`combat.base_attack`, `combat.py:177-184`): a city attacks
/// at three quarters of its strength; a unit at its ranged or melee strength, with `[n]
/// Strength` against a defender.
#[must_use]
pub fn base_attack(g: &Game, a: Combatant, d: Option<Combatant>) -> f64 {
    match a {
        Combatant::City(c) => {
            num::round_half_even(f64::from(city_strength(g, c, d, CombatAction::Attack)) * 0.75)
        }
        Combatant::Unit(u) => {
            let Some(x) = g.unit(u) else { return 0.0 };
            let def = &g.rules().base_units()[x.base];
            let extra = match d {
                Some(_) => strength_amount(g, u, &fight_ctx(g, a, d, CombatAction::Attack, None)),
                None => 0,
            };
            let base = if def.ranged { def.ranged_strength } else { def.strength };
            f64::from(base.saturating_add(extra))
        }
    }
}

/// Defence strength before modifiers (`combat.base_defense`, `combat.py:187-202`): a city's
/// strength, or 1 once its defences are down; an embarked military unit its era's embarked
/// defence; a ranged unit against a ranged attack its ranged strength; otherwise its strength,
/// with `[n] Strength` against an attacker.
#[must_use]
pub fn base_defense(g: &Game, d: Combatant, a: Option<Combatant>) -> f64 {
    match d {
        Combatant::City(c) => {
            if combatant::defeated(g, d) {
                1.0
            } else {
                f64::from(city_strength(g, c, a, CombatAction::Defend))
            }
        }
        Combatant::Unit(u) => {
            let Some(x) = g.unit(u) else { return 0.0 };
            let r = g.rules();
            let def = &r.base_units()[x.base];
            let extra = match a {
                Some(_) => strength_amount(g, u, &fight_ctx(g, d, a, CombatAction::Defend, None)),
                None => 0,
            };
            if def.military && crate::game::movement::is_embarked(g, u) {
                let era = crate::game::derive::civ::era(g, x.owner());
                return f64::from(r.eras()[era].embark_defense);
            }
            if def.ranged && a.is_some_and(|a| combatant::is_ranged(g, a)) {
                return f64::from(def.ranged_strength.saturating_add(extra));
            }
            f64::from(def.strength.saturating_add(extra))
        }
    }
}

// ---- Modifiers ------------------------------------------------------------------------------

/// The best great general's bonus in range of a unit, with the name of the general's type
/// (`combat._great_general_bonus`, `combat.py:205-230`): only the best applies, and it doubles
/// for a unit whose civilization has `Great General provides double combat bonus` when the
/// general is a great person of war.
fn great_general_bonus(
    g: &Game,
    ours: UnitId,
    enemy: Combatant,
    action: CombatAction,
) -> Option<(&'static str, i32)> {
    let x = g.unit(ours)?;
    let (owner, at) = (x.owner(), x.tile());
    let r = g.rules();
    let rules = &r.derived().combat;
    let v = g.view();
    let ctx = Ctx {
        civ: Some(owner),
        tile: Some(at),
        combat: Some(CombatCtx {
            our: Combatant::Unit(ours),
            their: Some(enemy),
            attacked_tile: None,
            action: Some(action),
        }),
        ..Ctx::default()
    };
    let mut best: Option<(crate::base::ids::BaseUnitId, i32)> = None;
    for general in g.player_units(owner) {
        if !rules.aura.may(general.base, &general.promotions) {
            continue;
        }
        for h in uq::unit(&v, general.id(), UniqueType::StrengthBonusInRadius, &ctx) {
            let UniqueData::StrengthBonusInRadius(b) = *h.data() else { continue };
            let radius = u32::try_from(b.radius).unwrap_or(0);
            if g.grid().distance(general.tile(), at) > radius {
                continue;
            }
            if !rules.is_military_filter(b.units) && !units::unit_matches(g, ours, b.units) {
                continue;
            }
            if b.percent > best.map_or(0, |(_, n)| n) {
                best = Some((general.base, b.percent));
            }
        }
    }
    let (general, mut bonus) = best?;
    if unit_has(g, ours, UniqueType::GreatGeneralProvidesDoubleCombatBonus, true)
        && rules.war_generals.contains(general)
    {
        bonus = bonus.saturating_mul(2);
    }
    Some((r.name(general).unwrap_or(""), bonus))
}

/// The modifiers that apply alike to either side (`combat._general_modifiers`,
/// `combat.py:233-278`).
fn general_modifiers(
    g: &Game,
    ours: Combatant,
    enemy: Combatant,
    action: CombatAction,
    from: TileIdx,
) -> Mods {
    let mut mods = Mods::new();
    let ctx = fight_ctx(g, ours, Some(enemy), action, None);
    let v = g.view();
    let r = g.rules();
    let owner = combatant::owner(g, ours);
    match ours {
        Combatant::Unit(u) => {
            let Some(unit) = g.unit(u) else { return mods };
            let at = unit.tile();
            // refcheck: combat-modifiers-in-ruleset-order (a unit's promotions are a set, so
            // their modifiers come in the ruleset's order, not the order it gained them)
            for h in uq::unit_and_civ(&v, u, UniqueType::Strength, &ctx) {
                if let UniqueData::Strength(x) = h.data() {
                    for _ in 0..h.n {
                        add(&mut mods, src(g, h.id), x.percent);
                    }
                }
            }
            let capital = g.player(owner).and_then(|p| p.capital).and_then(|c| g.city(c));
            if let Some(cap) = capital {
                let dist = i32::try_from(g.grid().distance(at, cap.tile())).unwrap_or(i32::MAX);
                for h in uq::unit_and_civ(&v, u, UniqueType::StrengthNearCapital, &ctx) {
                    if let UniqueData::StrengthNearCapital(x) = h.data() {
                        let eff = x.percent.saturating_sub(dist.saturating_mul(3));
                        if eff > 0 {
                            for _ in 0..h.n {
                                add(&mut mods, src(g, h.id), eff);
                            }
                        }
                    }
                }
            }
            // The enemies beside it, and a unit attacking it from beside it from afar.
            let mut adj: SmallVec<[UnitId; 8]> =
                g.grid().neighbors(at).flat_map(|n| g.units_at(n).map(|o| o.id())).collect();
            if let Combatant::Unit(e) = enemy {
                let et = combatant::tile(g, enemy);
                let next_to = |t: TileIdx| g.grid().neighbors(at).any(|n| n == t);
                if !next_to(et) && next_to(from) {
                    adj.push(e);
                }
            }
            let f = r.uniques().filters();
            let mut worst: Option<i32> = None;
            let adjacent = &r.derived().combat.adjacent;
            for o in adj {
                let Some(ou) = g.unit(o) else { continue };
                if !g.at_war(ou.owner(), owner) || !adjacent.may(ou.base, &ou.promotions) {
                    continue;
                }
                let octx = Ctx::unit(&v, o);
                for h in uq::unit(&v, o, UniqueType::StrengthForAdjacentEnemies, &octx) {
                    if let UniqueData::StrengthForAdjacentEnemies(x) = h.data()
                        && units::unit_matches(g, u, x.units)
                        && f.tile_matches(x.tiles, &v, at, Some(owner))
                        && worst.is_none_or(|w| x.percent < w)
                    {
                        worst = Some(x.percent);
                    }
                }
            }
            if let Some(w) = worst {
                put(&mut mods, "Adjacent enemy units", w);
            }
            if !g.is_barbarian(owner)
                && let Some(res) = r.base_units()[unit.base].required_resource
                && economy::resource_amount(g, owner, res) < 0
            {
                put(&mut mods, "Missing resource", MISSING_RESOURCE_MALUS);
            }
            if let Some((name, bonus)) = great_general_bonus(g, u, enemy, action)
                && bonus != 0
            {
                put(&mut mods, name, bonus);
            }
        }
        Combatant::City(c) => {
            for h in uq::city(&v, c, UniqueType::StrengthForCities, &ctx) {
                if let UniqueData::StrengthForCities(x) = h.data() {
                    for _ in 0..h.n {
                        add(&mut mods, src(g, h.id), x.percent);
                    }
                }
            }
        }
    }
    if g.is_barbarian(combatant::owner(g, enemy)) {
        let level = &r.difficulties()[g.state().config().barbarian_difficulty];
        put(&mut mods, "Difficulty", num::trunc_i32(level.barbarian_bonus * 100.0));
    }
    mods
}

/// Whether a tile has a road or railroad `p` may use (`movement.has_connection`,
/// `movement.py:302-309`): a route, or its own forest or jungle when those count as roads.
fn connected(g: &Game, p: PlayerId, t: TileIdx) -> bool {
    let rules = &g.rules().derived().moves;
    if crate::game::path::cost::route_at(g, rules, t).is_some() {
        return true;
    }
    let Some(tile) = g.tile(t) else { return false };
    tile.owner() == Some(p)
        && crate::game::path::cost::top_non_hill(tile, rules.hill)
            .is_some_and(|f| Some(f) == rules.forest || Some(f) == rules.jungle)
        && uq::any(uq::civ(&g.view(), p, UniqueType::ForestsAndJunglesAreRoads, &Ctx::civ(p)))
}

/// Every modifier of an attack (`combat.attack_modifiers`, `combat.py:281-322`): the shared ones,
/// then for a unit landing and boarding, a river crossed, an air sweep and flanking. `sweeping`
/// is an air sweep's attack (Python set the unit's activity to say so).
#[must_use]
pub fn attack_modifiers(
    g: &Game,
    a: Combatant,
    d: Combatant,
    from: TileIdx,
    sweeping: bool,
) -> Mods {
    let mut mods = general_modifiers(g, a, d, CombatAction::Attack, from);
    let Combatant::Unit(u) = a else { return mods };
    let Some(unit) = g.unit(u) else { return mods };
    let owner = unit.owner();
    let dt = combatant::tile(g, d);
    let melee = combatant::is_melee(g, a);
    let land_target = g.is_land(dt);
    let across_coast = unit_has(g, u, UniqueType::AttackAcrossCoast, false);
    if crate::game::movement::is_embarked(g, u) && land_target && !across_coast {
        put(&mut mods, "Landing", LANDING_MALUS);
    }
    if combatant::is_land(g, a)
        && !g.is_water(from)
        && melee
        && g.is_water(dt)
        && !across_coast
        && g.city_at(dt).is_none()
    {
        put(&mut mods, "Boarding", BOARDING_MALUS);
    }
    if !combatant::is_air(g, a)
        && melee
        && g.is_water(from)
        && !g.is_water(dt)
        && !across_coast
        && !matches!(d, Combatant::City(_))
    {
        put(&mut mods, "Landing", LANDING_MALUS);
    }
    if melee && g.grid().distance(from, dt) == 1 {
        let river = match (g.tile(from), g.tile(dt)) {
            (Some(ta), Some(tb)) => {
                crate::game::path::cost::river_between(g.grid(), ta, tb, from, dt)
            }
            _ => false,
        };
        let v = g.view();
        if river
            && !unit_has(g, u, UniqueType::AttackAcrossRiver, false)
            && !(connected(g, owner, from)
                && connected(g, owner, dt)
                && uq::any(uq::civ(
                    &v,
                    owner,
                    UniqueType::RoadsConnectAcrossRivers,
                    &Ctx::civ(owner),
                )))
        {
            put(&mut mods, "Across river", RIVER_MALUS);
        }
    }
    if sweeping || unit.activity == Some(Activity::AirSweep) {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        for h in uq::unit(&v, u, UniqueType::StrengthWhenAirsweep, &ctx) {
            if let UniqueData::StrengthWhenAirsweep(x) = h.data() {
                for _ in 0..h.n {
                    add(&mut mods, src(g, h.id), x.percent);
                }
            }
        }
    }
    if melee {
        let r = g.rules();
        let n = g
            .grid()
            .neighbors(dt)
            .filter(|&nb| {
                g.military_at(nb).is_some_and(|m| {
                    m.id() != u && m.owner() == owner && r.base_units()[m.base].melee
                })
            })
            .count();
        if n > 0 {
            let v = g.view();
            let ctx = fight_ctx(g, a, Some(d), CombatAction::Attack, None);
            let mut fb = FLANKING;
            for h in uq::unit_and_civ(&v, u, UniqueType::FlankAttackBonus, &ctx) {
                if let UniqueData::FlankAttackBonus(x) = h.data() {
                    for _ in 0..h.n {
                        fb *= 1.0 + f64::from(x.percent) / 100.0;
                    }
                }
            }
            #[allow(clippy::cast_precision_loss, reason = "a tile has six neighbours")]
            let flank = num::trunc_i32(fb * n as f64);
            put(&mut mods, "Flanking", flank);
        }
    }
    mods
}

/// The defensive bonus of the terrain a unit stands on (`combat.tile_defense_bonus`,
/// `combat.py:325-341`): its base terrain's, or its features' best if that is not nothing; its
/// natural wonder's; and its unpillaged improvement's `[n]% defense`, asked of the unit there.
#[must_use]
pub fn tile_defense_bonus(g: &Game, t: TileIdx, unit: Option<UnitId>) -> f64 {
    let Some(tile) = g.tile(t) else { return 0.0 };
    let r = g.rules();
    let terrains = r.terrains();
    let mut bonus = terrains[tile.terrain()].defence_bonus;
    let features = tile.features();
    if !features.is_empty() {
        let other = features
            .iter()
            .filter_map(|f| r.derived().features.get(f).copied())
            .map(|x| terrains[x].defence_bonus)
            .fold(f64::NEG_INFINITY, f64::max);
        if other != 0.0 && other.is_finite() {
            bonus = other;
        }
    }
    if let Some(w) = tile.wonder() {
        bonus += terrains[w].defence_bonus;
    }
    if let Some(imp) = tile.improvement().filter(|_| !tile.improvement_pillaged()) {
        let v = g.view();
        let ctx = Ctx { unit, tile: Some(t), ..Ctx::default() }.resolve(&v);
        for h in uq::object(&v, &r.improvements()[imp].uniques, UniqueType::DefensiveBonus, &ctx) {
            if let UniqueData::DefensiveBonus(x) = h.data() {
                bonus += f64::from(x.percent) / 100.0;
            }
        }
    }
    bonus
}

/// Every modifier of a defence (`combat.defense_modifiers`, `combat.py:344-358`): the shared
/// ones, then for a unit not at sea its terrain (a bonus unless it may not have one, a penalty
/// unless it ignores them) and its fortification.
#[must_use]
pub fn defense_modifiers(g: &Game, a: Combatant, d: Combatant, from: TileIdx) -> Mods {
    let mut mods = general_modifiers(g, d, a, CombatAction::Defend, from);
    let Combatant::Unit(u) = d else { return mods };
    let Some(unit) = g.unit(u) else { return mods };
    if crate::game::movement::is_embarked(g, u) {
        return mods;
    }
    let tb = tile_defense_bonus(g, unit.tile(), Some(u));
    // Only the unique that concerns the bonus's sign is asked.
    let counts = if tb > 0.0 {
        !unit_has(g, u, UniqueType::NoDefensiveTerrainBonus, true)
    } else if tb < 0.0 {
        !unit_has(g, u, UniqueType::NoDefensiveTerrainPenalty, true)
    } else {
        false
    };
    if counts {
        put(&mut mods, "Tile", num::round_half_even_i32(tb * 100.0));
    }
    if matches!(unit.activity, Some(Activity::Fortify | Activity::FortifyHeal)) && unit.fortify > 0
    {
        put(&mut mods, "Fortification", FORTIFICATION * i32::from(unit.fortify.min(2)));
    }
    mods
}

// ---- Damage -----------------------------------------------------------------------------------

/// How much of its damage a side deals for its wounds (`combat._wounded_ratio`,
/// `combat.py:376-387`): a city and a unit that ignores its wounds all of it, a unit a third of a
/// point less per point of health lost.
#[must_use]
pub fn wounded_ratio(g: &Game, c: Combatant) -> f64 {
    let Combatant::Unit(u) = c else { return 1.0 };
    let hp = combatant::hp(g, c);
    // An unhurt unit loses nothing either way.
    if hp >= 100 || unit_has(g, u, UniqueType::NoDamagePenaltyWoundedUnits, true) {
        return 1.0;
    }
    1.0 - f64::from(100 - hp) / WOUNDED_RATIO
}

/// A strength ratio as a damage multiplier (`combat._damage_modifier`, `combat.py:390-401`):
/// UnCiv's fourth-power curve, with the roll between 24 and 36.
#[must_use]
pub fn damage_modifier(ratio: f64, to_attacker: bool, rnd: f64) -> f64 {
    let stronger = if ratio >= 1.0 { ratio } else { 1.0 / ratio };
    let mut rm = (num::pow((stronger + 3.0) / 4.0, 4.0) + 1.0) / 2.0;
    if (to_attacker && ratio > 1.0) || (!to_attacker && ratio < 1.0) {
        rm = 1.0 / rm;
    }
    (24.0 + 12.0 * rnd) * rm
}

/// A fight's numbers, gathered once: both sides' modifiers and strengths, which the damage at
/// any roll reads (`combat.py:366-419`).
#[derive(Clone, Debug, PartialEq)]
pub struct CombatSetup {
    pub attacker: Combatant,
    pub defender: Combatant,
    /// Where the attacker attacks from.
    pub from: TileIdx,
    pub attack_modifiers: Mods,
    pub defense_modifiers: Mods,
    /// The final strengths, at least one each (`attacking_strength`, `defending_strength`).
    pub attack: f64,
    pub defense: f64,
    attacker_wounds: f64,
    defender_wounds: f64,
    civilian: bool,
    /// The attacker shoots from afar and takes nothing back.
    ranged: bool,
}

impl CombatSetup {
    /// The damage the defender takes at roll `rnd` (`combat.damage_to_defender`): a civilian a
    /// fixed amount.
    #[must_use]
    pub fn damage_to_defender(&self, rnd: f64) -> i32 {
        if self.civilian {
            return DAMAGE_TO_CIVILIAN;
        }
        let ratio = self.attack / self.defense;
        num::round_half_even_i32(damage_modifier(ratio, false, rnd) * self.attacker_wounds)
    }

    /// The damage the attacker takes back at roll `rnd` (`combat.damage_to_attacker`): none for
    /// a ranged attack that is not an aircraft's, nor from a civilian.
    #[must_use]
    pub fn damage_to_attacker(&self, rnd: f64) -> i32 {
        if self.ranged || self.civilian {
            return 0;
        }
        let ratio = self.attack / self.defense;
        num::round_half_even_i32(damage_modifier(ratio, true, rnd) * self.defender_wounds)
    }
}

/// Gathers a fight's numbers: `a` attacking `d` from `from` (`combat.attacking_strength`,
/// `defending_strength` and the damage functions' shared reads). `sweeping` is an air sweep's.
#[must_use]
pub fn setup(g: &Game, a: Combatant, from: TileIdx, d: Combatant, sweeping: bool) -> CombatSetup {
    let attack_modifiers = attack_modifiers(g, a, d, from, sweeping);
    let defense_modifiers = defense_modifiers(g, a, d, from);
    let attack = (base_attack(g, a, Some(d)) * multiplier(&attack_modifiers)).max(1.0);
    let defense = (base_defense(g, d, Some(a)) * multiplier(&defense_modifiers)).max(1.0);
    CombatSetup {
        attacker: a,
        defender: d,
        from,
        attack_modifiers,
        defense_modifiers,
        attack,
        defense,
        attacker_wounds: wounded_ratio(g, a),
        defender_wounds: wounded_ratio(g, d),
        civilian: combatant::is_civilian(g, d),
        ranged: combatant::is_ranged(g, a) && !combatant::is_air(g, a),
    }
}

/// Whether a unit is of the air domain, by its base unit.
#[must_use]
pub fn is_aircraft(g: &Game, u: UnitId) -> bool {
    g.unit(u).is_some_and(|x| g.rules().base_units()[x.base].domain == Domain::Air)
}

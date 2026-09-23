//! Conditionals (DESIGN.md 5.8): what each one reads, whether it holds, and what a requirement
//! says when it does not.
//!
//! Ports `applies` and the lambdas of `_COND` (`uniques.py:778-1083`), and the refusal texts of
//! `cities._not_met` (`cities.py:1169-1193`). Python looked each conditional's placeholder up in a
//! dict at every evaluation and failed closed on one it did not know (`uniques.py:794`); here each
//! is a [`CondData`] variant compiled at load, so an unknown one never loads, and [`holds`] is one
//! `match`.
//!
//! **What a conditional reads.** The loader gives each conditional its [`CondDeps`] once its filters
//! are compiled (`assign_deps`), and a unique's deps are the union of its conditionals'. A
//! conditional whose answer depends on nothing but the ids in its context (`for [Babylon]
//! Civilizations` reads the civilization's nation, fixed at setup) reads `CONFIG`, so that a
//! unique's deps are empty exactly when it has no conditionals. The classes are defined on
//! [`CondDeps`].
//!
//! **Python's edge cases kept.** With no civilization in context, a conditional about the
//! civilization fails, except those Python wrote as a negation: `before adopting []` and
//! `without []` hold (`uniques.py:937, 822`). `with []% chance`, `if tutorials are enabled` and
//! `on water maps` are as Python had them: a keyed draw, never, never. A missing context and
//! `ignore` make every conditional hold ([`Ctx::IGNORE`]).
//!
//! **Python behaviour fixed** (DESIGN.md 5.12), for the reference checks to list once a group
//! compares them:
//! - building conditionals read a building filter (`if [Wonder] is constructed`), where Python
//!   compared the text with a building's name and its replacement (`uniques.py:853-860`);
//! - `when between [a] and [b] [stat]` scales both bounds by game speed when the unique is
//!   `(modified by game speed)`, as the other two stat comparisons did (`uniques.py:830-834`);
//! - `if no Civilization has adopted []` counts beliefs too (Python's dead key read policies only);
//! - a unit filter after `vs [] units` asks about units only: a city is never a unit.

use super::countable::Countable;
use super::filter::{Combatant, Expr, Filters, TileLeaf, UnitScope};
use super::generated::{CondData, UniqueType};
use super::params::{PolicyOrBelief, PopulationFilter, StatOrResource};
use super::table::{Cond, CondDeps, UFlags, UniqueTable};
use super::world::{CombatAction, CombatCtx, Ctx, EvalWorld};
use crate::base::ids::{
    BuildingId, CityId, Id, NationId, PlayerId, SetRef, TileFilterId, TileIdx, Turn, UniqueId,
    UnitFilterId, UnitId,
};
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::stats::Stat;
use crate::rules::Ruleset;
use crate::rules::defs::{NationKind, ReligionProgress};

// ---- What each conditional reads ---------------------------------------------------------------

/// What a conditional reads, by class (DESIGN.md 5.8), its filters' leaves included.
#[must_use]
pub fn deps_of(data: &CondData, filters: &Filters) -> CondDeps {
    use CondData as C;
    use CondDeps as D;
    let tiles = |f: TileFilterId| filters.tile(f).full.deps();
    let units = |f: UnitFilterId| filters.unit(f).deps();
    let d = match *data {
        C::ConditionalEveryTurns(_)
        | C::ConditionalBeforeTurns(_)
        | C::ConditionalAfterTurns(_) => D::TURN,
        C::ConditionalSpeed(_)
        | C::ConditionalVictoryEnabled(_)
        | C::ConditionalVictoryDisabled(_)
        | C::ConditionalReligionEnabled
        | C::ConditionalReligionDisabled
        | C::ConditionalEspionageEnabled
        | C::ConditionalEspionageDisabled
        | C::ConditionalNuclearWeaponsEnabled
        | C::ConditionalNuclearWeaponsDisabled
        | C::ConditionalTutorialsEnabled
        | C::ConditionalTutorialCompleted(_)
        | C::ConditionalIfStartingInEra(_)
        | C::ConditionalOnWaterMaps
        | C::ConditionalInRegionOfType(_)
        | C::ConditionalInRegionExceptOfType(_) => D::CONFIG,
        C::ConditionalDifficulty(_)
        | C::ConditionalDifficultyOrHigher(_)
        | C::ConditionalDifficultyOrLower(_) => D::SEAT | D::CONFIG,
        // The draw is keyed by the turn and by the tile and unit in context.
        C::ConditionalChance(_) => D::CHANCE | D::TURN | D::TILE | D::UNIT,
        C::ConditionalCivFilter(x) => filters.civ(x.civs).deps(),
        C::ConditionalWar | C::ConditionalNotWar => D::WAR,
        C::ConditionalGoldenAge | C::ConditionalNotGoldenAge => D::GOLDEN_AGE,
        C::ConditionalHappy => D::HAPPINESS_SEEN,
        C::ConditionalDuringEra(_)
        | C::ConditionalBeforeEra(_)
        | C::ConditionalStartingFromEra(_) => D::ERA,
        C::ConditionalTech(_) | C::ConditionalNoTech(_) => D::TECHS,
        C::ConditionalWhileResearching(_) => D::RESEARCH_QUEUE,
        C::ConditionalNoCivAdopted(_) => D::GLOBAL_POLICIES | D::CITY_COUNT,
        C::ConditionalAfterPolicyOrBelief(x) => adopted_deps(x.adopted),
        C::ConditionalBeforePolicyOrBelief(x) => adopted_deps(x.adopted),
        C::ConditionalBeforePantheon
        | C::ConditionalAfterPantheon
        | C::ConditionalBeforeReligion
        | C::ConditionalAfterReligion
        | C::ConditionalBeforeEnhancingReligion
        | C::ConditionalAfterEnhancingReligion
        | C::ConditionalAfterGeneratingGreatProphet => D::RELIGION_STATE,
        C::ConditionalBuildingBuilt(_) | C::ConditionalBuildingNotBuilt(_) => D::CIV_BUILDINGS,
        C::ConditionalBuildingBuiltAll(x) => {
            D::CIV_BUILDINGS | D::CITY_COUNT | filters.city(x.cities).deps()
        }
        C::ConditionalBuildingBuiltAmount(x) => {
            D::CIV_BUILDINGS | D::CITY_COUNT | filters.city(x.cities).deps()
        }
        C::ConditionalBuildingBuiltByAnybody(_) | C::ConditionalBuildingNotBuiltByAnybody(_) => {
            D::GLOBAL_BUILDINGS
        }
        C::ConditionalWithResource(_) | C::ConditionalWithoutResource(_) => D::RESOURCES,
        C::ConditionalWhenAboveAmountStatResource(x) => stat_deps(x.what),
        C::ConditionalWhenBelowAmountStatResource(x) => stat_deps(x.what),
        C::ConditionalWhenBetweenStatResource(x) => stat_deps(x.what),
        C::ConditionalInThisCity
        | C::ConditionalCityWithBuilding(_)
        | C::ConditionalCityWithoutBuilding(_)
        | C::ConditionalPopulationFilter(_)
        | C::ConditionalExactPopulationFilter(_)
        | C::ConditionalBetweenPopulationFilter(_)
        | C::ConditionalBelowPopulationFilter(_) => D::CITY,
        C::ConditionalCityFilter(x) => D::CITY | filters.city(x.cities).deps(),
        // The road and harbour network to the capital: other tiles, other cities, open borders.
        C::ConditionalCityConnected => D::all(),
        C::ConditionalWhenGarrisoned => D::CITY | D::UNIT_SET,
        C::ConditionalOurUnit(x) => D::UNIT | units(x.units),
        C::ConditionalOurUnitOnUnit(x) => D::UNIT | units(x.units),
        C::ConditionalUnitWithPromotion(_)
        | C::ConditionalUnitWithoutPromotion(_)
        | C::ConditionalHasNotUsedOtherActions => D::UNIT,
        C::ConditionalAboveHP(_) | C::ConditionalBelowHP(_) => D::UNIT | D::COMBAT,
        C::ConditionalStackedWithUnit(x) => D::UNIT | D::UNIT_SET | units(x.units),
        C::ConditionalNotStackedWithUnit(x) => D::UNIT | D::UNIT_SET | units(x.units),
        C::ConditionalAdjacentUnit(x) => D::UNIT | D::UNIT_SET | units(x.units),
        C::ConditionalVsCity | C::ConditionalAttacking | C::ConditionalDefending => D::COMBAT,
        C::ConditionalVsUnits(x) => D::COMBAT | units(x.units),
        C::ConditionalVsCombatant(x) => {
            let f = filters.combatant(x.combatants);
            D::COMBAT | f.unit.deps() | f.city.deps()
        }
        C::ConditionalVsLargerCiv => D::COMBAT | D::CITY_COUNT,
        C::ConditionalFightingInTiles(x) => D::COMBAT | D::TILE | tiles(x.tiles),
        C::ConditionalForeignContinent => D::TILE | D::CITY_COUNT,
        C::ConditionalInTiles(x) => D::TILE | tiles(x.tiles),
        C::ConditionalInTilesNot(x) => D::TILE | tiles(x.tiles),
        // Tiles other than the one in context, which no class names.
        C::ConditionalNearTiles(_)
        | C::ConditionalAdjacentTo(_)
        | C::ConditionalNotAdjacentTo(_)
        | C::ConditionalNeighborTiles(_) => D::all(),
        C::ConditionalCountableEqualTo(x) => x.count.deps(filters) | x.to.deps(filters),
        C::ConditionalCountableDifferentThan(x) => x.count.deps(filters) | x.than.deps(filters),
        C::ConditionalCountableMoreThan(x) => x.count.deps(filters) | x.than.deps(filters),
        C::ConditionalCountableLessThan(x) => x.count.deps(filters) | x.than.deps(filters),
        C::ConditionalCountableBetween(x) => {
            x.count.deps(filters) | x.min.deps(filters) | x.max.deps(filters)
        }
    };
    if d.is_empty() { D::CONFIG } else { d }
}

/// What `after adopting []` and `before adopting []` read.
const fn adopted_deps(what: PolicyOrBelief) -> CondDeps {
    match what {
        PolicyOrBelief::Policy(_) => CondDeps::POLICIES,
        PolicyOrBelief::Belief(_) => CondDeps::RELIGION_STATE,
    }
}

/// What a stat or resource comparison reads.
const fn stat_deps(what: StatOrResource) -> CondDeps {
    match what {
        StatOrResource::Resource(_) => CondDeps::RESOURCES,
        StatOrResource::Stat(Stat::Happiness) => CondDeps::HAPPINESS_SEEN,
        StatOrResource::Stat(_) => CondDeps::STOCKS,
    }
}

/// Gives every conditional of `t` its classes, and every unique the union of its conditionals':
/// the loader's last step for uniques, once the filters they read are compiled. Also notes the
/// handle of the city filter `in this city`, which one-time effects read as the city in context
/// (`triggers.py:58-61`).
pub(crate) fn assign_deps(t: &mut UniqueTable) {
    let deps: Vec<CondDeps> = t.conds.iter().map(|(_, c)| deps_of(&c.data, &t.filters)).collect();
    for (c, d) in t.conds.as_mut_slice().iter_mut().zip(&deps) {
        c.deps = *d;
    }
    for u in t.uniques.as_mut_slice() {
        let d = u.conds.ids().fold(CondDeps::empty(), |d, c| d | deps[c.index()]);
        u.set_deps(d);
    }
    let this_city = t.city_filters.iter().find(|&(_, &text)| t.text(text) == "in this city");
    t.this_city = this_city.map(|(id, _)| id);
}

// ---- Whether a unique applies ------------------------------------------------------------------

/// Whether every conditional of the unique `id` holds in `ctx` (`applies`, `uniques.py:778-799`).
/// A unique without conditionals always applies, and so does every unique in a context that
/// ignores them.
pub fn applies<W: EvalWorld>(id: UniqueId, ctx: &Ctx, w: &W) -> bool {
    let t = w.rules().uniques();
    let u = t.get(id);
    if u.conds.is_empty() || ctx.ignore_conditionals {
        return true;
    }
    t.conds(u).iter().all(|c| holds(c, id, ctx, w))
}

/// Whether the conditionals of the unique `id` that are in scope hold, the others counted as
/// holding. A conditional is in scope when it reads a class of `mask` and no context-local class
/// ([`CondDeps::LOCAL`]) outside it. So with [`CondDeps::CIV_LEVEL`] a caller evaluates the
/// civilization-wide conditionals once, and with [`CondDeps::LOCAL`] the rest per city, unit or
/// tile: the two together evaluate each conditional once and agree with [`applies`] (DESIGN.md
/// 5.8, hoisting).
pub fn applies_scoped<W: EvalWorld>(id: UniqueId, ctx: &Ctx, w: &W, mask: CondDeps) -> bool {
    let t = w.rules().uniques();
    let u = t.get(id);
    if u.conds.is_empty() || ctx.ignore_conditionals {
        return true;
    }
    t.conds(u).iter().filter(|c| in_scope(c.deps, mask)).all(|c| holds(c, id, ctx, w))
}

/// Whether a conditional reading `deps` is evaluated under `mask`.
#[must_use]
pub const fn in_scope(deps: CondDeps, mask: CondDeps) -> bool {
    deps.intersects(mask) && deps.intersection(CondDeps::LOCAL).difference(mask).is_empty()
}

/// Whether the conditional `c` of the unique `id` holds in `ctx`: one lambda of `_COND`
/// (`uniques.py:896-1010`). `ctx.ignore_conditionals` is not read here; [`applies`] reads it.
#[allow(clippy::too_many_lines, reason = "one arm per conditional, as _COND had one entry each")]
pub fn holds<W: EvalWorld>(c: &Cond, id: UniqueId, ctx: &Ctx, w: &W) -> bool {
    use CondData as C;
    let r = w.rules();
    let t = r.uniques();
    let f = t.filters();
    let civ = ctx.civ;
    let on_civ = |test: &dyn Fn(PlayerId) -> bool| civ.is_some_and(test);
    let progress =
        |test: fn(ReligionProgress) -> bool| civ.is_some_and(|p| test(w.civ_religion_progress(p)));
    let has = |s: SetRef, city: CityId| has_building(t, s, w, city);
    let rel_city = || ctx.rel_city(w);
    let rel_unit = ctx.rel_unit();
    let rel_tile = ctx.rel_tile();
    let combat = ctx.combat;
    let their = combat.and_then(|c| c.their);
    // Other units than the one in context, matched with nothing in context
    // (`unit_matches(g, o, f)`).
    let other_matches = |o: UnitId, x: UnitFilterId| f.unit_matches(x, w, o, UnitScope::default());
    // The unit a rule means, matched with the context: `other` is not the unit in context, and
    // its owner is seen by the civilization in context (`unit_matches(g, u, f, ctx)`).
    let our_unit = |x: UnitFilterId| {
        let scope = UnitScope { this: ctx.unit, viewer: civ };
        rel_unit.is_some_and(|u| f.unit_matches(x, w, u, scope))
    };
    match c.data {
        C::ConditionalEveryTurns(x) => w.turn().rem_euclid(x.turns.max(1)) == 0,
        C::ConditionalBeforeTurns(x) => w.turn() < x.turn,
        C::ConditionalAfterTurns(x) => w.turn() >= x.turn,
        C::ConditionalSpeed(x) => w.speed() == x.speed,
        C::ConditionalDifficulty(x) => w.difficulty(civ) == x.difficulty,
        // Difficulties are numbered easiest first, as Python's `difficulty_list` was.
        C::ConditionalDifficultyOrHigher(x) => w.difficulty(civ) >= x.difficulty,
        C::ConditionalDifficultyOrLower(x) => w.difficulty(civ) <= x.difficulty,
        C::ConditionalVictoryEnabled(x) => w.victory_enabled(x.victory),
        C::ConditionalVictoryDisabled(x) => !w.victory_enabled(x.victory),
        C::ConditionalReligionEnabled => w.religion_enabled(),
        C::ConditionalReligionDisabled => !w.religion_enabled(),
        C::ConditionalEspionageEnabled => w.espionage_enabled(),
        C::ConditionalEspionageDisabled => !w.espionage_enabled(),
        C::ConditionalNuclearWeaponsEnabled => w.nukes_enabled(),
        C::ConditionalNuclearWeaponsDisabled => !w.nukes_enabled(),
        C::ConditionalChance(x) => {
            let keys = chance_keys(w.turn(), t.meta(id).key, ctx);
            Rng::keyed(w.seed(), Purpose::Chance, &keys).unit() < f64::from(x.percent) / 100.0
        }
        // The engine has no tutorials.
        C::ConditionalTutorialsEnabled | C::ConditionalTutorialCompleted(_) => false,
        C::ConditionalCivFilter(x) => on_civ(&|p| f.civ_matches(x.civs, w, p, Some(p))),
        C::ConditionalWar => on_civ(&|p| w.civ_at_war(p)),
        C::ConditionalNotWar => on_civ(&|p| !w.civ_at_war(p)),
        C::ConditionalGoldenAge => on_civ(&|p| w.civ_golden_age(p)),
        C::ConditionalNotGoldenAge => on_civ(&|p| !w.civ_golden_age(p)),
        C::ConditionalHappy => on_civ(&|p| w.civ_happiness(p) >= 0),
        // Eras are numbered in order, which the loader checks.
        C::ConditionalDuringEra(x) => on_civ(&|p| w.civ_era(p) == x.era),
        C::ConditionalBeforeEra(x) => on_civ(&|p| w.civ_era(p) < x.era),
        C::ConditionalStartingFromEra(x) => on_civ(&|p| w.civ_era(p) >= x.era),
        C::ConditionalIfStartingInEra(x) => w.starting_era() == x.era,
        C::ConditionalTech(x) => on_civ(&|p| w.civ_techs(p).iter().any(|k| t.in_set(x.techs, k))),
        C::ConditionalNoTech(x) => {
            on_civ(&|p| !w.civ_techs(p).iter().any(|k| t.in_set(x.techs, k)))
        }
        C::ConditionalWhileResearching(x) => {
            civ.and_then(|p| w.civ_researching(p)).is_some_and(|k| t.in_set(x.techs, k))
        }
        C::ConditionalNoCivAdopted(x) => {
            !w.civs().any(|p| w.civ_kind(p) == NationKind::Major && adopted(w, p, x.adopted))
        }
        C::ConditionalAfterPolicyOrBelief(x) => on_civ(&|p| adopted(w, p, x.adopted)),
        // Python's negation: with no civilization, nothing has been adopted.
        C::ConditionalBeforePolicyOrBelief(x) => !on_civ(&|p| adopted(w, p, x.adopted)),
        C::ConditionalBeforePantheon => progress(|s| s == ReligionProgress::None),
        C::ConditionalAfterPantheon => progress(|s| s != ReligionProgress::None),
        C::ConditionalBeforeReligion => progress(|s| !s.has_religion()),
        C::ConditionalAfterReligion => progress(ReligionProgress::has_religion),
        C::ConditionalBeforeEnhancingReligion => progress(|s| s != ReligionProgress::Enhanced),
        C::ConditionalAfterEnhancingReligion => progress(|s| s == ReligionProgress::Enhanced),
        C::ConditionalAfterGeneratingGreatProphet => on_civ(&|p| w.civ_prophets_earned(p) > 0),
        C::ConditionalBuildingBuilt(x) => on_civ(&|p| w.civ_cities(p).any(|c| has(x.buildings, c))),
        C::ConditionalBuildingNotBuilt(x) => {
            on_civ(&|p| !w.civ_cities(p).any(|c| has(x.buildings, c)))
        }
        C::ConditionalBuildingBuiltAll(x) => on_civ(&|p| {
            w.civ_cities(p)
                .filter(|&c| f.city_matches(x.cities, w, c, None))
                .all(|c| has(x.buildings, c))
        }),
        C::ConditionalBuildingBuiltAmount(x) => on_civ(&|p| {
            let n = w
                .civ_cities(p)
                .filter(|&c| has(x.buildings, c) && f.city_matches(x.cities, w, c, None))
                .count();
            i64::try_from(n).unwrap_or(i64::MAX) >= i64::from(x.count)
        }),
        C::ConditionalBuildingBuiltByAnybody(x) => w.cities().any(|c| has(x.buildings, c)),
        C::ConditionalBuildingNotBuiltByAnybody(x) => !w.cities().any(|c| has(x.buildings, c)),
        C::ConditionalWithResource(x) => on_civ(&|p| w.civ_resource(p, x.resource) > 0),
        // Python's negation: with no civilization, there is none.
        C::ConditionalWithoutResource(x) => !on_civ(&|p| w.civ_resource(p, x.resource) > 0),
        C::ConditionalWhenAboveAmountStatResource(x) => {
            stat_amount(w, civ, x.what) > f64::from(x.amount) * speed_factor(r, w, id)
        }
        C::ConditionalWhenBelowAmountStatResource(x) => {
            stat_amount(w, civ, x.what) < f64::from(x.amount) * speed_factor(r, w, id)
        }
        C::ConditionalWhenBetweenStatResource(x) => {
            let k = speed_factor(r, w, id);
            let a = stat_amount(w, civ, x.what);
            f64::from(x.min) * k <= a && a <= f64::from(x.max) * k
        }
        C::ConditionalInThisCity => rel_city().is_some(),
        C::ConditionalCityFilter(x) => {
            rel_city().is_some_and(|c| f.city_matches(x.cities, w, c, civ))
        }
        C::ConditionalCityConnected => rel_city().is_some_and(|c| w.city_connected_to_capital(c)),
        C::ConditionalCityWithBuilding(x) => rel_city().is_some_and(|c| has(x.buildings, c)),
        C::ConditionalCityWithoutBuilding(x) => rel_city().is_some_and(|c| !has(x.buildings, c)),
        C::ConditionalPopulationFilter(x) => {
            rel_city().is_some_and(|c| population(w, c, x.population) >= x.count)
        }
        C::ConditionalExactPopulationFilter(x) => {
            rel_city().is_some_and(|c| population(w, c, x.population) == x.count)
        }
        C::ConditionalBetweenPopulationFilter(x) => rel_city().is_some_and(|c| {
            let n = population(w, c, x.population);
            x.min <= n && n <= x.max
        }),
        C::ConditionalBelowPopulationFilter(x) => {
            rel_city().is_some_and(|c| population(w, c, x.population) < x.count)
        }
        C::ConditionalWhenGarrisoned => rel_city().is_some_and(|c| w.city_garrisoned(c)),
        C::ConditionalOurUnit(x) => our_unit(x.units),
        C::ConditionalOurUnitOnUnit(x) => our_unit(x.units),
        C::ConditionalUnitWithPromotion(x) => {
            rel_unit.is_some_and(|u| w.unit_promotions(u).contains(x.promotion))
        }
        C::ConditionalUnitWithoutPromotion(x) => {
            rel_unit.is_some_and(|u| !w.unit_promotions(u).contains(x.promotion))
        }
        C::ConditionalVsCity => matches!(their, Some(Combatant::City(_))),
        C::ConditionalVsUnits(x) => match their {
            Some(Combatant::Unit(u)) => other_matches(u, x.units),
            _ => false,
        },
        C::ConditionalVsCombatant(x) => {
            their.is_some_and(|o| f.combatant_matches(x.combatants, w, o))
        }
        C::ConditionalVsLargerCiv => match (civ, their) {
            (Some(p), Some(o)) => {
                w.civ_cities(p).count() < w.civ_cities(super::world::combatant_owner(w, o)).count()
            }
            _ => false,
        },
        C::ConditionalAttacking => {
            matches!(combat, Some(CombatCtx { action: Some(CombatAction::Attack), .. }))
        }
        C::ConditionalDefending => {
            matches!(combat, Some(CombatCtx { action: Some(CombatAction::Defend), .. }))
        }
        C::ConditionalFightingInTiles(x) => combat
            .and_then(|c| c.attacked_tile)
            .is_some_and(|tile| f.tile_matches(x.tiles, w, tile, civ)),
        C::ConditionalForeignContinent => match (civ, rel_tile) {
            // Python found no capital's landmass without a civilization; nothing is foreign to
            // no one.
            (None, _) | (_, None) => false,
            (Some(p), Some(tile)) => match w.civ_capital(p) {
                None => true,
                Some(cap) => w.tile_landmass(w.city_tile(cap)) != w.tile_landmass(tile),
            },
        },
        C::ConditionalAdjacentUnit(x) => rel_unit.is_some_and(|u| {
            w.grid().neighbors(w.unit_tile(u)).any(|n| {
                w.units_at(n)
                    .any(|o| o != u && Some(w.unit_owner(o)) == civ && other_matches(o, x.units))
            })
        }),
        C::ConditionalAboveHP(x) => health(w, ctx).is_some_and(|h| h > x.hp),
        C::ConditionalBelowHP(x) => health(w, ctx).is_some_and(|h| h < x.hp),
        C::ConditionalHasNotUsedOtherActions => ctx.unit.is_none_or(|u| !w.unit_used_actions(u)),
        C::ConditionalStackedWithUnit(x) => rel_unit.is_some_and(|u| {
            w.units_at(w.unit_tile(u)).any(|o| o != u && other_matches(o, x.units))
        }),
        C::ConditionalNotStackedWithUnit(x) => rel_unit.is_none_or(|u| {
            !w.units_at(w.unit_tile(u)).any(|o| o != u && other_matches(o, x.units))
        }),
        C::ConditionalNeighborTiles(x) => rel_tile.is_some_and(|tile| {
            let n =
                w.grid().neighbors(tile).filter(|&n| f.tile_matches(x.tiles, w, n, civ)).count();
            let n = i64::try_from(n).unwrap_or(i64::MAX);
            i64::from(x.min) <= n && n <= i64::from(x.max)
        }),
        C::ConditionalInTiles(x) => {
            rel_tile.is_some_and(|tile| f.tile_matches(x.tiles, w, tile, civ))
        }
        C::ConditionalInTilesNot(x) => {
            rel_tile.is_some_and(|tile| !f.tile_matches(x.tiles, w, tile, civ))
        }
        C::ConditionalNearTiles(x) => rel_tile.is_some_and(|tile| {
            let radius = u32::try_from(x.radius).unwrap_or(0);
            w.grid().within(tile, radius).into_iter().any(|i| f.tile_matches(x.tiles, w, i, civ))
        }),
        C::ConditionalAdjacentTo(x) => {
            rel_tile.is_some_and(|tile| adjacent_to(w, x.tiles, tile, civ))
        }
        C::ConditionalNotAdjacentTo(x) => {
            rel_tile.is_some_and(|tile| !adjacent_to(w, x.tiles, tile, civ))
        }
        // UnCiv's water maps are archipelago regions, and map generation builds no regions.
        C::ConditionalOnWaterMaps => false,
        // Map generation's own; only map-generation and inert uniques carry them, which nothing
        // evaluates here (`rules::gen_tables::GenCond` does). As map generation reads them: no
        // region is of any type, and every tile is outside every region.
        C::ConditionalInRegionOfType(_) => false,
        C::ConditionalInRegionExceptOfType(_) => true,
        C::ConditionalCountableEqualTo(x) => compare(w, ctx, x.count, x.to, |a, b| a == b),
        C::ConditionalCountableDifferentThan(x) => compare(w, ctx, x.count, x.than, |a, b| a != b),
        C::ConditionalCountableMoreThan(x) => compare(w, ctx, x.count, x.than, |a, b| a > b),
        C::ConditionalCountableLessThan(x) => compare(w, ctx, x.count, x.than, |a, b| a < b),
        C::ConditionalCountableBetween(x) => {
            match (x.count.eval(w, ctx), x.min.eval(w, ctx), x.max.eval(w, ctx)) {
                (Some(n), Some(lo), Some(hi)) => lo <= n && n <= hi,
                _ => false,
            }
        }
    }
}

/// The key of a `with [n]% chance` draw (DESIGN.md 7.2): the turn, the unique's key, and the
/// civilization, tile and unit in context, each `None` distinct from id 0. Python keyed it by the
/// unique's text (`uniques.py:842`), so the same text on two sources rolled together.
#[must_use]
pub fn chance_keys(turn: Turn, key: u64, ctx: &Ctx) -> [u64; 5] {
    [turn.key(), key, ctx.civ.key(), ctx.tile.key(), ctx.rel_unit().key()]
}

/// Whether the civilization has adopted the policy, or its religion has the belief
/// (`_pol_or_belief`, `uniques.py:864-873`).
fn adopted<W: EvalWorld>(w: &W, p: PlayerId, what: PolicyOrBelief) -> bool {
    match what {
        PolicyOrBelief::Policy(x) => w.civ_policies(p).contains(x),
        PolicyOrBelief::Belief(x) => w.civ_beliefs(p).contains(x),
    }
}

/// Whether the city has a building of the filter, which names a building's replacements too
/// (`_city_has`, `uniques.py:859-861`).
fn has_building<W: EvalWorld>(t: &UniqueTable, s: SetRef, w: &W, c: CityId) -> bool {
    w.city_buildings(c).iter().any(|b: BuildingId| t.in_set(s, b))
}

/// The amount of a stat or resource a comparison reads (`_stat_amount`, `uniques.py:811-824`):
/// 0 with no civilization.
fn stat_amount<W: EvalWorld>(w: &W, civ: Option<PlayerId>, what: StatOrResource) -> f64 {
    let Some(p) = civ else { return 0.0 };
    match what {
        StatOrResource::Resource(r) => f64::from(w.civ_resource(p, r)),
        StatOrResource::Stat(Stat::Happiness) => f64::from(w.civ_happiness(p)),
        StatOrResource::Stat(s) => w.civ_stock(p, s),
    }
}

/// The game speed's factor for a unique `(modified by game speed)`, else 1 (`_speed_mod`,
/// `uniques.py:827-831`).
fn speed_factor<W: EvalWorld>(r: &Ruleset, w: &W, id: UniqueId) -> f64 {
    if r.uniques().get(id).flags().contains(UFlags::SPEED) {
        r.speeds()[w.speed()].modifier
    } else {
        1.0
    }
}

/// The citizens of a city a population filter counts (`population_amount`, `uniques.py:720-732`).
fn population<W: EvalWorld>(w: &W, c: CityId, f: PopulationFilter) -> i32 {
    match f {
        PopulationFilter::Population => w.city_population(c),
        PopulationFilter::Specialists => w.city_specialists(c, None),
        PopulationFilter::FollowersOfThisReligion
        | PopulationFilter::FollowersOfTheMajorityReligion => w.city_majority_followers(c),
        PopulationFilter::Unemployed => w.city_unemployed(c),
        PopulationFilter::Specialist(s) => w.city_specialists(c, Some(s)),
    }
}

/// The health of what is in context (`_hp`, `uniques.py:1036-1043`): the unit a rule means, else
/// our side's city in a fight.
fn health<W: EvalWorld>(w: &W, ctx: &Ctx) -> Option<i32> {
    if let Some(u) = ctx.rel_unit() {
        return Some(w.unit_health(u));
    }
    match ctx.combat {
        Some(CombatCtx { our: Combatant::City(c), .. }) => Some(w.city_health(c)),
        _ => None,
    }
}

/// Whether the tile is next to one of the filter (`_adjacent_to`, `uniques.py:1046-1054`): a
/// river runs along the tile itself, and a tile with a river has fresh water next to it.
fn adjacent_to<W: EvalWorld>(w: &W, f: TileFilterId, tile: TileIdx, civ: Option<PlayerId>) -> bool {
    let filters = w.rules().uniques().filters();
    match filters.tile(f).full {
        Expr::Leaf(TileLeaf::River) => return w.tile_river(tile),
        Expr::Leaf(TileLeaf::FreshWater) if w.tile_river(tile) => return true,
        _ => {}
    }
    w.grid().neighbors(tile).any(|n| filters.tile_matches(f, w, n, civ))
}

/// Compares two countables, failing when either has no value (`_cmp_countables`,
/// `uniques.py:876-879`).
fn compare<W: EvalWorld>(
    w: &W,
    ctx: &Ctx,
    a: Countable,
    b: Countable,
    op: fn(i64, i64) -> bool,
) -> bool {
    match (a.eval(w, ctx), b.eval(w, ctx)) {
        (Some(x), Some(y)) => op(x, y),
        _ => false,
    }
}

// ---- What a requirement says -------------------------------------------------------------------

/// Why an object cannot be built, as `cities.rejection_reasons` named it: the kinds `_not_met`
/// gives (`cities.py:1169-1193`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProblemKind {
    /// `if [] is constructed in all [] cities` fails.
    RequiresBuildingInAllCities,
    /// `if [] is constructed in at least [] of [] cities` fails.
    RequiresBuildingInSomeCities,
    /// A conditional of `Can only be built` fails.
    CanOnlyBeBuiltInSpecificCities,
    /// A conditional of `Only available` fails.
    ShouldNotBeDisplayed,
}

impl ProblemKind {
    /// Python's name for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RequiresBuildingInAllCities => "RequiresBuildingInAllCities",
            Self::RequiresBuildingInSomeCities => "RequiresBuildingInSomeCities",
            Self::CanOnlyBeBuiltInSpecificCities => "CanOnlyBeBuiltInSpecificCities",
            Self::ShouldNotBeDisplayed => "ShouldNotBeDisplayed",
        }
    }
}

/// One reason a requirement is not met, as a player reads it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Problem {
    pub kind: ProblemKind,
    pub text: String,
}

impl Cond {
    /// What a requirement says when this conditional fails for a civilization of `nation`
    /// (`_not_met`, `cities.py:1180-1192`). The two building counts name the civilization's own
    /// version of the building, because "requires a Temple in all cities" is actionable; any other
    /// conditional is `Not available (<its text>)`. `Can only be built` says its whole text
    /// instead, which [`super::query::requirement_problems`] supplies.
    #[must_use]
    pub fn describe(&self, rules: &Ruleset, nation: NationId) -> Problem {
        let t = rules.uniques();
        match self.data {
            CondData::ConditionalBuildingBuiltAll(x) => Problem {
                kind: ProblemKind::RequiresBuildingInAllCities,
                text: format!(
                    "Requires a {} in all {} cities",
                    equivalent_building(rules, nation, t.text(t.set(x.buildings).text)),
                    t.city_filter(x.cities)
                ),
            },
            CondData::ConditionalBuildingBuiltAmount(x) => Problem {
                kind: ProblemKind::RequiresBuildingInSomeCities,
                text: format!(
                    "Requires a {} in at least {} cities",
                    equivalent_building(rules, nation, t.text(t.set(x.buildings).text)),
                    x.count
                ),
            },
            _ => Problem {
                kind: ProblemKind::ShouldNotBeDisplayed,
                text: format!("Not available ({})", t.text(self.text)),
            },
        }
    }
}

/// The name of a nation's own version of the building a filter names (`equivalent_building`,
/// `cities.py:1140-1152`): the building it replaces, if the text names a replacement, then the
/// nation's replacement of that, if it has one. A filter that names no building stays as written.
#[must_use]
pub fn equivalent_building<'r>(rules: &'r Ruleset, nation: NationId, text: &'r str) -> &'r str {
    let Some(named) = rules.lookup::<BuildingId>(text) else { return text };
    let base = rules.buildings()[named].replaces.unwrap_or(named);
    let own = rules.derived().nation_uniques[nation]
        .buildings
        .iter()
        .find(|&&(replaced, _)| replaced == base)
        .map_or(base, |&(_, b)| b);
    rules.name(own).unwrap_or(text)
}

/// The type of each conditional, for tables that list them: every type whose role is a
/// conditional, in type order.
pub fn types() -> impl Iterator<Item = UniqueType> {
    UniqueType::ALL.into_iter().filter(|t| t.role() == Some(super::table::Role::Cond))
}

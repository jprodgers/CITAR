//! The job maps of worker automation (DESIGN.md 6.5, 6.11, `JobMap`): for each civilization and
//! builder class, the best job ([`crate::game::automation::best_job`]) on every land tile of its
//! cities but their centres.
//!
//! Python cached a tile's best job per unit type for the rest of a turn, keyed by what the tile
//! and the civilization looked like (`automation._best_job`, `automation.py:313-327`), and
//! recomputed every tile each turn. A map here keeps each tile's job with what it was worked out
//! from, and recomputes a tile only when something its job reads has changed:
//! - **the tile** (`TileSig`): its terrain, features, natural wonder, resource, improvement and
//!   what is pillaged, its river, owner, a repair or build with no build time at the front of its
//!   queue, which neighbours are fresh water or coast, and, when a filter a job reads asks,
//!   whether a city works it and its route. A road, citizens moving, or a turn of work on its
//!   queue, changes no job and recomputes nothing. A tile with no job keeps with it a bound on
//!   what any improvement is worth there, so that when only its improvement changes the bound
//!   moves by the difference in what the two cost, and the tile keeps no job while nothing can
//!   rise above the threshold (`kept_without_job`);
//! - **the civilization** (`CivJobs`): which improvements it has the tech for and which are
//!   obsolete, how long each takes its builders, which resources it sees and which luxuries it
//!   has, the removals it knows, the resources its improvements consume. A change recomputes only
//!   the tiles it can reach: those with the resource, those whose job it was, those where an
//!   improvement it changed could stand and now beat the job (its value on the tile, at its new
//!   build time, above the job's), and, weighing every improvement, those with a feature whose
//!   removal the civilization learned or forgot and those whose standing improvement changed
//!   (an `Irremovable` that no longer holds). The tiles it does not reach raise their bound;
//! - **its territory**: tiles joining are worked out, tiles leaving dropped.
//!
//! A tile's job is worked out as [`best_job`] works it out, with what the civilization settles for
//! every tile (an improvement barred, named by the class, bound to the borders, and the rest of
//! `Plan`) answered once per look at the civilization, which takes a whole map's rebuild from
//! four times the budget of DESIGN.md 10 to under it; the cache oracle compares every tile with
//! `best_job` itself. A ruleset whose relevant uniques have conditionals that read a tile's
//! surroundings, a city or the whole map (or an improvement that must be next to something) asks
//! `best_job` on each tile, keeps no bounds, and recomputes every tile of a map when any tile,
//! owner, city or anything of the civilization a job reads changed: correct, and never the
//! shipped ruleset's case. With the `stats`
//! feature, the maps count the tiles they recompute and those that came out as they were, the
//! redundancy DESIGN.md 10 bounds.

use core::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use smallvec::SmallVec;

use super::civ::{civ_index_full_changed, cond, supply_changed};
use super::rev::Rev;
use crate::base::ids::{
    ImprovementId, ObjectFilterId, PlayerId, ResourceId, TerrainId, TileFilterId, TileIdx, UniqueId,
};
use crate::base::sets::{FeatureSet, ImprovementSet, ResourceSet};
use crate::game::Game;
use crate::game::automation::{best_job, improvement_value, luxury_owned, weighted};
use crate::game::workers::{self, Builder};
use crate::rules::Ruleset;
use crate::rules::defs::{BuilderClass, ImprovementDef, ImprovementKind, Route};
use crate::state::map::Tile;
use crate::unique::filter::TileLeaf;
use crate::unique::{CondDeps, Ctx, UniqueData, UniqueType, applies, uq};

/// The improvement unique types a job reads, with their conditionals asked on the tile.
const READ: [UniqueType; 15] = [
    UniqueType::Unbuildable,
    UniqueType::Unavailable,
    UniqueType::OnlyAvailable,
    UniqueType::ObsoleteWith,
    UniqueType::ConsumesResources,
    UniqueType::CanBuildOutsideBorders,
    UniqueType::CanBuildJustOutsideBorders,
    UniqueType::Irremovable,
    UniqueType::RemovesFeaturesIfBuilt,
    UniqueType::CannotBuildOnTile,
    UniqueType::CanOnlyBeBuiltOnTile,
    UniqueType::MustBeNextTo,
    UniqueType::CanOnlyImproveResource,
    UniqueType::ImprovementBuildableByFreshWater,
    UniqueType::NoFeatureRemovalNeeded,
];

/// What a tile's job reads of the tile and its neighbours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TileSig {
    terrain: TerrainId,
    wonder: Option<TerrainId>,
    features: FeatureSet,
    resource: Option<ResourceId>,
    improvement: Option<ImprovementId>,
    pillaged: bool,
    /// A route, and whether it is pillaged (a repair reads the second).
    route: Option<Route>,
    route_pillaged: bool,
    river: u8,
    owner: Option<PlayerId>,
    /// Whether a city works it, when a filter a job reads asks.
    worked: bool,
    /// A repair, or a build with no build time, at the front of its queue: what a builder may
    /// only go on with.
    front: Option<ImprovementId>,
    /// Which neighbours are a source of fresh water, and which are coast: all a job reads of
    /// them (`Fresh water`, `Coastal`).
    fresh_around: u8,
    coast_around: u8,
}

impl TileSig {
    fn of(g: &Game, t: TileIdx) -> Option<Self> {
        let tile = g.tile(t)?;
        let r = g.rules();
        let caches = &g.dv.jobs;
        let front = g.state().tiles().builds(t).first().map(|s| s.improvement).filter(|&i| {
            let d = &r.improvements()[i];
            d.kind == ImprovementKind::Repair || d.turns_to_build.is_none()
        });
        let worked = caches.reads_worked
            && tile.city().and_then(|c| g.city(c)).is_some_and(|c| c.worked.contains(&t));
        let coast = r.derived().known.map.coast;
        let fresh = &r.derived().fresh_water;
        let (mut fresh_around, mut coast_around) = (0u8, 0u8);
        for (i, n) in g.grid().neighbors(t).enumerate() {
            let Some(x) = g.tile(n) else { continue };
            let bit = 1u8 << i;
            let terrains = core::iter::once(x.terrain())
                .chain(x.wonder())
                .chain(x.features().iter().filter_map(|f| r.derived().features.get(f).copied()));
            if terrains.into_iter().any(|tt| fresh.contains(tt)) {
                fresh_around |= bit;
            }
            if coast.is_some() && Some(x.terrain()) == coast {
                coast_around |= bit;
            }
        }
        Some(Self {
            terrain: tile.terrain(),
            wonder: tile.wonder(),
            features: tile.features(),
            resource: tile.resource(),
            improvement: tile.improvement(),
            pillaged: tile.improvement_pillaged(),
            route: tile.route().filter(|_| caches.reads_route),
            route_pillaged: tile.route().is_some() && tile.route_pillaged(),
            river: tile.river_mask(),
            owner: tile.owner(),
            worked,
            front,
            fresh_around,
            coast_around,
        })
    }
}

/// Whether tile `t`, which had no job, still has none after it changed from what `old` saw to
/// what `new` sees, and if so its new [`Entry::top`]; `None` if it must be worked out again.
///
/// A tile under a great improvement ([`under_great`]) has no job whatever changed, and keeps its
/// bound only if nothing but the improvement did. Otherwise only its improvement may have
/// changed. The bound is on the improvements weighed as though nothing stood in the way (over a
/// great or an irremovable improvement none is taken). Every value a job is weighed by is an
/// improvement's gain on the tile less what the improvement standing there yields and one, so the
/// values of the improvements weighed before move by the difference between the two standing
/// improvements' costs, and none of them can be the new one; the improvement taken away may be
/// built again, at its gain less the new one's cost (at its own build time, which a removal only
/// lengthens). The tile keeps no job while both stay at or below the threshold. With no bound
/// known, only a standing improvement that costs no less than the one before keeps it, the one
/// before neither great nor irremovable (nothing was weighed over it). Never when a filter a job
/// reads asks about improvements.
#[allow(clippy::too_many_arguments, reason = "a tile's entry and both sides of its change")]
fn kept_without_job(
    g: &Game,
    p: PlayerId,
    t: TileIdx,
    key: &CivJobs,
    old: Option<&TileSig>,
    new: Option<&TileSig>,
    top: Option<f64>,
) -> Option<Option<f64>> {
    let (Some(old), Some(new)) = (old, new) else { return None };
    let r = g.rules();
    let same_but_improvement = TileSig { improvement: new.improvement, ..*old } == *new;
    let moves =
        (same_but_improvement && !g.dv.jobs.reads_improvement && !old.pillaged && !new.pillaged)
            .then(|| {
                let cost = |i: Option<ImprovementId>| {
                    i.map_or(0.0, |i| weighted(&r.improvements()[i].stats) + 1.0)
                };
                let returning = old
                    .improvement
                    .filter(|&i| {
                        let d = &r.improvements()[i];
                        d.kind == ImprovementKind::Normal && !d.great && key.only.contains(i)
                    })
                    .map_or(f64::NEG_INFINITY, |i| {
                        let turns = key.turns.get(usize::from(i.0)).copied().unwrap_or(1);
                        improvement_value(g, p, t, i) - f64::from(turns) * 0.15
                    });
                (cost(old.improvement) - cost(new.improvement), returning)
            });
    let moved = moves.zip(top).map(|((shift, returning), top)| (top + shift).max(returning));
    if under_great(g, new) {
        return Some(moved);
    }
    let (shift, returning) = moves?;
    if moved.is_some_and(|m| m > 0.5) || returning > 0.5 {
        return None;
    }
    if top.is_some() {
        return Some(moved);
    }
    let irremovable = old.improvement.is_some_and(|i| {
        let t = r.uniques();
        r.improvements()[i].great
            || r.improvements()[i]
                .uniques
                .ids()
                .any(|id| t.meta(id).ty == Some(UniqueType::Irremovable))
    });
    (shift <= 0.0 && !irremovable).then_some(None)
}

/// Whether a tile holds a great improvement, whole, on a whole route (or none), with no fallout:
/// no job is weighed over it, a repair has nothing to mend and there is no fallout to clear, so
/// it has no job whatever else changes.
fn under_great(g: &Game, sig: &TileSig) -> bool {
    let r = g.rules();
    !sig.pillaged
        && !sig.route_pillaged
        && !sig.features.contains(r.derived().known.fallout)
        && sig.improvement.is_some_and(|i| r.improvements()[i].great)
}

/// Whether improvement `imp` could be built on tile `t` for civilization `p`, all else allowing:
/// it is not there already; the tiles its unconditional `Can only be built on [...] tiles` and
/// `Cannot be built on [...] tiles` name allow it; and one of the tile's terrains takes it, land
/// or water does, fresh water does, or it improves the tile's resource. What [`workers`]'
/// placement rules allow is a subset of this.
fn could_stand(g: &Game, p: PlayerId, t: TileIdx, imp: ImprovementId) -> bool {
    let r = g.rules();
    let Some(tile) = g.tile(t) else { return false };
    if tile.improvement() == Some(imp) {
        return false;
    }
    let d = &r.improvements()[imp];
    let tu = r.uniques();
    let v = g.view();
    let refused = d.uniques.ids().any(|id| {
        let u = tu.get(id);
        if !u.conds.is_empty() {
            return false;
        }
        match u.data {
            UniqueData::CanOnlyBeBuiltOnTile(x) => {
                !tu.filters().tile_matches(x.tiles, &v, t, Some(p))
            }
            UniqueData::CannotBuildOnTile(x) => tu.filters().tile_matches(x.tiles, &v, t, Some(p)),
            _ => false,
        }
    });
    if refused {
        return false;
    }
    let features = &r.derived().features;
    let mut terrains = core::iter::once(tile.terrain())
        .chain(tile.wonder())
        .chain(tile.features().iter().filter_map(|f| features.get(f).copied()));
    let water = g.is_water(t);
    terrains.any(|tt| workers::allowed_on_feature(g, imp, tt))
        || (d.on_land && !water)
        || (d.on_water && water)
        || r.improvements()[imp]
            .uniques
            .ids()
            .any(|id| r.uniques().meta(id).ty == Some(UniqueType::ImprovementBuildableByFreshWater))
        || tile.resource().is_some_and(|res| crate::game::tiles::resource_improved_by(g, res, imp))
}

/// What a civilization's jobs read of it (see the module doc).
#[derive(Clone, Debug, PartialEq)]
struct CivJobs {
    known: ImprovementSet,
    obsolete: ImprovementSet,
    /// Build time of each improvement for the class, by id.
    turns: Vec<i32>,
    visible: ResourceSet,
    lux: ResourceSet,
    removals: ImprovementSet,
    /// How much it has of each resource an improvement consumes, in [`JobCaches::consumed`]'s
    /// order.
    amounts: Vec<i32>,
    /// The latest revision of the civilization-level classes the relevant uniques read.
    epoch: Rev,
    /// The improvements worth looking at on a tile: those it could ever build, and the fallout's
    /// removal.
    only: ImprovementSet,
}

impl CivJobs {
    fn of(g: &Game, p: PlayerId, class: BuilderClass) -> Self {
        let r = g.rules();
        let t = r.uniques();
        let caches = &g.dv.jobs;
        let b = Builder::class(p, class);
        let ctx = Ctx::civ(p);
        let v = g.view();
        let nation = g.player(p).map(|x| x.nation);
        let mut known = ImprovementSet::new();
        let mut obsolete = ImprovementSet::new();
        let mut only = ImprovementSet::new();
        let mut turns = Vec::with_capacity(r.improvements().len());
        for (imp, d) in r.improvements().iter() {
            if g.has_tech(p, d.tech_required) {
                known.insert(imp);
            }
            let old = uq::object(&v, &d.uniques, UniqueType::ObsoleteWith, &ctx).any(
                |h| matches!(*h.data(), UniqueData::ObsoleteWith(x) if g.has_tech(p, Some(x.tech))),
            );
            if old {
                obsolete.insert(imp);
            }
            turns.push(workers::turns_in(g, &b, imp, &ctx));
            let never = d.uniques.ids().any(|id| {
                t.meta(id).ty == Some(UniqueType::Unbuildable) && t.get(id).conds.is_empty()
            });
            if d.kind == ImprovementKind::Normal
                && !d.great
                && known.contains(imp)
                && d.unique_to.is_none_or(|n| Some(n) == nation)
                && !never
            {
                only.insert(imp);
            }
        }
        let fallout = r.derived().known.fallout;
        if let Some(rem) = r.derived().removal_of.get(fallout).copied().flatten() {
            only.insert(rem);
        }
        let mut visible = ResourceSet::new();
        let mut lux = ResourceSet::new();
        for res in r.resources().ids() {
            if workers::resource_visible(g, p, res) {
                visible.insert(res);
            }
            if luxury_owned(g, p, res) {
                lux.insert(res);
            }
        }
        let removals = r
            .derived()
            .feature_removals
            .iter()
            .copied()
            .filter(|&i| g.has_tech(p, r.improvements()[i].tech_required))
            .collect();
        let amounts = caches
            .consumed
            .iter()
            .map(|&res| crate::game::economy::resource_amount(g, p, res))
            .collect();
        let epoch =
            if caches.civ_deps.is_empty() { Rev::START } else { cond(g, caches.civ_deps, &ctx) };
        Self { known, obsolete, turns, visible, lux, removals, amounts, epoch, only }
    }
}

/// One improvement as a civilization's builders of one class see it wherever it stands: what
/// [`workers::unit_can_build`] and [`workers::building_problems`] ask of the civilization rather
/// than of the tile, answered once for the map. Only while no unique a job reads looks at a tile,
/// a city or the map with its conditionals ([`JobCaches::local`]), so that asking them without
/// a tile gives what asking them on each tile would.
#[derive(Clone, Debug)]
struct Weigh {
    imp: ImprovementId,
    /// Barred everywhere: an `Unbuildable` or `Unavailable` that holds, an `Only available` that
    /// does not, `Obsolete with` a known tech, less of a resource than it consumes.
    barred: bool,
    /// One of the class's filters names the improvement itself (not a terrain).
    named: bool,
    /// It has no build time: a builder only goes on with it once it is under way.
    instant: bool,
    outside: bool,
    just_outside: bool,
    only_resource: bool,
    fresh_water: bool,
    cannot_on: SmallVec<[TileFilterId; 1]>,
    only_on: SmallVec<[TileFilterId; 1]>,
}

/// A civilization's builder class as its map weighs tiles with it: [`Weigh`] for each
/// improvement worth looking at, in the ruleset's order, and what else the class and the
/// civilization settle for every tile.
#[derive(Clone, Debug)]
struct Plan {
    weighs: Vec<Weigh>,
    /// The class has a filter at all and the civilization is not the barbarians: it builds.
    builds: bool,
    /// The tile halves of the class's filters (`Can build [Land] improvements on tiles`).
    terrain_filters: SmallVec<[TileFilterId; 2]>,
    /// The improvements that may not be built over, as the civilization's conditionals stand.
    irremovable: ImprovementSet,
}

impl Plan {
    fn of(g: &Game, p: PlayerId, class: BuilderClass, key: &CivJobs) -> Self {
        let r = g.rules();
        let tu = r.uniques();
        let v = g.view();
        let ctx = Ctx::civ(p);
        let filters: &[ObjectFilterId] =
            r.derived().builder_classes.get(usize::from(class.0)).map_or(&[][..], |f| &f[..]);
        let terrain_filters = filters.iter().filter_map(|&o| tu.object(o).tiles).collect();
        let holds = |d: &ImprovementDef, ty| uq::any(uq::object(&v, &d.uniques, ty, &ctx));
        let of_type = |d: &ImprovementDef, ty| {
            d.uniques.ids().filter(move |&id| tu.meta(id).ty == Some(ty)).collect::<Vec<_>>()
        };
        let mut irremovable = ImprovementSet::new();
        for (imp, d) in r.improvements().iter() {
            if holds(d, UniqueType::Irremovable) {
                irremovable.insert(imp);
            }
        }
        let mut weighs = Vec::new();
        for (imp, d) in r.improvements().iter() {
            if d.kind != ImprovementKind::Normal || d.great || !key.only.contains(imp) {
                continue;
            }
            let unbuildable = of_type(d, UniqueType::Unbuildable)
                .into_iter()
                .any(|id| !tu.get(id).conds.is_empty() && applies(id, &ctx, &v));
            let unavailable =
                of_type(d, UniqueType::Unavailable).into_iter().any(|id| applies(id, &ctx, &v));
            let not_yet =
                of_type(d, UniqueType::OnlyAvailable).into_iter().any(|id| !applies(id, &ctx, &v));
            let short = uq::object(&v, &d.uniques, UniqueType::ConsumesResources, &ctx).any(|h| {
                matches!(*h.data(), UniqueData::ConsumesResources(x)
                    if crate::game::economy::resource_amount(g, p, x.resource) < x.amount)
            });
            let tiles_of = |ty| -> SmallVec<[TileFilterId; 1]> {
                uq::object(&v, &d.uniques, ty, &ctx)
                    .filter_map(|h| match *h.data() {
                        UniqueData::CannotBuildOnTile(x) => Some(x.tiles),
                        UniqueData::CanOnlyBeBuiltOnTile(x) => Some(x.tiles),
                        _ => None,
                    })
                    .collect()
            };
            weighs.push(Weigh {
                imp,
                barred: unbuildable
                    || unavailable
                    || not_yet
                    || key.obsolete.contains(imp)
                    || short,
                named: filters
                    .iter()
                    .any(|&o| tu.object(o).improvements.is_some_and(|s| tu.in_set(s, imp))),
                instant: d.turns_to_build.is_none(),
                outside: holds(d, UniqueType::CanBuildOutsideBorders),
                just_outside: holds(d, UniqueType::CanBuildJustOutsideBorders),
                only_resource: holds(d, UniqueType::CanOnlyImproveResource),
                fresh_water: holds(d, UniqueType::ImprovementBuildableByFreshWater),
                cannot_on: tiles_of(UniqueType::CannotBuildOnTile),
                only_on: tiles_of(UniqueType::CanOnlyBeBuiltOnTile),
            });
        }
        Self {
            weighs,
            builds: !filters.is_empty() && !g.is_barbarian(p),
            terrain_filters,
            irremovable,
        }
    }
}

/// [`best_job`] for a builder class, with what the civilization settles for every tile read from
/// `plan`: the same job, as the cache oracle checks, for a sixth of the work (DESIGN.md 10 budgets
/// a whole map at 200 us). The steps are `workers::build_options`' and `best_job`'s, in their
/// order. Besides the job, the highest value weighed ([`Entry::top`]), where the tile's own
/// improvement did not rule out weighing any (a great one does not: the improvements are weighed
/// over it for the bound, and none is taken).
fn planned_job(
    g: &Game,
    p: PlayerId,
    t: TileIdx,
    plan: &Plan,
    key: &CivJobs,
    b: &Builder,
) -> (Option<(ImprovementId, f64)>, Option<f64>) {
    let r = g.rules();
    let Some(tile) = g.tile(t) else { return (None, None) };
    if !plan.builds {
        return (None, Some(f64::NEG_INFINITY));
    }
    let known = &r.derived().known;
    let enemy = workers::is_enemy_territory(g, t, p);
    // A repair comes first, and a builder takes it before anything else (`automation.py:338`).
    if let Some(repair) = known.repair
        && workers::repairable(tile)
        && !enemy
    {
        return (Some((repair, 30.0)), None);
    }
    let front = g.state().tiles().builds(t).first().map(|s| s.improvement);
    let repairing = known.repair.is_some() && front == known.repair;
    let over_great = tile.improvement().is_some_and(|i| r.improvements()[i].great);
    let removal_of = |f| r.derived().removal_of.get(f).copied().flatten();
    let mut best: Option<(ImprovementId, f64)> = None;
    let mut top = f64::NEG_INFINITY;
    // Nothing goes on a city, over an irremovable improvement, or over a great one; over a great
    // one the improvements are still weighed, for the bound the tile keeps.
    let open =
        g.city_at(t).is_none() && tile.improvement().is_none_or(|i| !plan.irremovable.contains(i));
    if open {
        let v = g.view();
        let tf = r.uniques().filters();
        let terrain_named =
            plan.terrain_filters.iter().any(|&f| tf.tile_terrain_matches(f, &v, t, Some(p)));
        let mine = tile.owner() == Some(p);
        let next_to_mine =
            || g.grid().neighbors(t).any(|n| g.tile(n).and_then(Tile::owner) == Some(p));
        for w in &plan.weighs {
            // `unit_can_build`: under way if it has no build time; with a repair under way,
            // anything outside enemy land; else a filter names it or the tile.
            if w.barred || tile.improvement() == Some(w.imp) || (w.instant && front != Some(w.imp))
            {
                continue;
            }
            if if repairing { enemy } else { !(w.named || terrain_named) } {
                continue;
            }
            if !mine && !w.outside && (!w.just_outside || !next_to_mine()) {
                continue;
            }
            if !stands(g, p, t, tile, w, key, None) {
                continue;
            }
            let turns = with_removals(g, t, w.imp, key);
            let value = improvement_value(g, p, t, w.imp) - f64::from(turns) * 0.15;
            top = top.max(value);
            if !over_great && value > 0.5 && best.is_none_or(|(_, bv)| value > bv) {
                best = Some((w.imp, value));
            }
        }
    }
    let top = open.then_some(top);
    // refcheck: fallout-removal-is-a-job
    if best.is_none()
        && tile.features().contains(known.fallout)
        && let Some(rem) = removal_of(known.fallout)
        && key.only.contains(rem)
        && workers::unit_can_build(g, b, rem, t)
        && workers::no_problems(g, b, t, rem)
    {
        return (Some((rem, 8.0)), top);
    }
    (best, top)
}

/// Whether improvement `w` may stand on tile `t` for civilization `p`, the tile's own
/// improvement, a city and an irremovable improvement already ruled out: the rest of
/// `workers::built_here_ok` (`workers.py:60-117`), its uniques read from `w`. `features` stands in
/// for the tile's after the removals a builder would queue first.
fn stands(
    g: &Game,
    p: PlayerId,
    t: TileIdx,
    tile: &Tile,
    w: &Weigh,
    key: &CivJobs,
    features: Option<FeatureSet>,
) -> bool {
    let r = g.rules();
    let imp = w.imp;
    let feature_terrain = |f| r.derived().features.get(f).copied();
    let feats = features.unwrap_or_else(|| tile.features());
    let mut last = feats
        .top()
        .and_then(feature_terrain)
        .unwrap_or_else(|| tile.wonder().unwrap_or_else(|| tile.terrain()));
    if features.is_none()
        && let Some(wonder) = tile.wonder()
    {
        last = wonder;
    }
    // The feature on top cleared, then the one under it looked at in turn.
    // refcheck: improvements-over-removable-features
    if r.terrains()[last].unbuildable && !workers::allowed_on_feature(g, imp, last) {
        let Some(top) = feats.top().filter(|&f| feature_terrain(f) == Some(last)) else {
            return false;
        };
        let removal = r.derived().removal_of.get(top).copied().flatten();
        if removal.is_none_or(|i| !key.removals.contains(i)) {
            return false;
        }
        let mut left = feats;
        left.remove(top);
        return stands(g, p, t, tile, w, key, Some(left));
    }
    let tu = r.uniques();
    let mut restricted = false;
    let mut allowed = false;
    for terrain in core::iter::once(tile.terrain()).chain(feats.iter().filter_map(feature_terrain))
    {
        for id in r.terrains()[terrain].uniques.ids() {
            if let UniqueData::RestrictedBuildableImprovements(x) = tu.get(id).data {
                restricted = true;
                allowed |= tu.in_set(x.improvements, imp);
            }
        }
    }
    if restricted && !allowed {
        return false;
    }
    let v = g.view();
    let matches = |f| tu.filters().tile_matches(f, &v, t, Some(p));
    if w.cannot_on.iter().any(|&f| matches(f)) || w.only_on.iter().any(|&f| !matches(f)) {
        return false;
    }
    let improves_res = tile.resource().is_some_and(|res| {
        workers::resource_visible(g, p, res)
            && crate::game::tiles::resource_improved_by(g, res, imp)
    });
    if w.only_resource && !improves_res {
        return false;
    }
    let d = &r.improvements()[imp];
    workers::allowed_on_feature(g, imp, last)
        || (g.is_land(t) && d.on_land)
        || (g.is_water(t) && d.on_water)
        || (w.fresh_water && workers::fresh_water(g, t))
        || (improves_res && workers::domain_ok(g, t, imp))
}

/// How long civilization `key`'s builders take to build `imp` on tile `t`, with the removals it
/// needs first ([`workers::needed_removals`]), as `workers::build_options` counts them.
fn with_removals(g: &Game, t: TileIdx, imp: ImprovementId, key: &CivJobs) -> i32 {
    let turns_of = |x: ImprovementId| key.turns.get(usize::from(x.0)).copied().unwrap_or(1);
    let removal_of = &g.rules().derived().removal_of;
    workers::needed_removals(g, t, imp)
        .into_iter()
        .filter_map(|f| removal_of.get(f).copied().flatten())
        .fold(turns_of(imp), |turns, rem| turns.saturating_add(turns_of(rem)))
}

/// One tile's entry: what its job was worked out from, and the job.
#[derive(Clone, Copy, Debug)]
struct Entry {
    sig: Option<TileSig>,
    job: Option<(ImprovementId, f64)>,
    /// At least the value of every improvement that could be weighed on the tile as it stands
    /// (as though no great or irremovable improvement stood in the way), job or not, when known
    /// ([`planned_job`] finds it): what lets a tile with no job keep none when only its
    /// improvement changes ([`kept_without_job`]).
    top: Option<f64>,
    /// Whether the job has been worked out at all.
    known: bool,
    dirty: bool,
}

/// One civilization's map for one builder class.
#[derive(Clone, Debug)]
struct JobMap {
    verified: Rev,
    key: Option<CivJobs>,
    /// What the civilization settles for every tile, with `key`; none while a unique a job reads
    /// looks beyond the civilization ([`JobCaches::local`]), when each tile asks [`best_job`].
    plan: Option<Plan>,
    tiles: BTreeMap<TileIdx, Entry>,
}

impl JobMap {
    const fn new() -> Self {
        Self { verified: Rev::NEVER, key: None, plan: None, tiles: BTreeMap::new() }
    }
}

/// How many tiles the maps worked out: afresh (a tile new to a map), again, and again to the
/// same job (feature `stats`).
#[cfg(feature = "stats")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobCounts {
    pub fresh: u64,
    pub recomputed: u64,
    pub unchanged: u64,
}

/// Every job map, and what the ruleset says about what they read: part of `Derived`.
#[derive(Clone, Debug)]
pub(crate) struct JobCaches {
    maps: RefCell<BTreeMap<(PlayerId, BuilderClass), JobMap>>,
    /// The civilization-level classes the relevant uniques read: their conditionals', and their
    /// filters'.
    civ_deps: CondDeps,
    /// The improvements with such a unique: changed whenever those classes move.
    cond_imps: ImprovementSet,
    /// Some relevant unique reads beyond a tile and its neighbours, or beyond the civilization
    /// with its conditionals: every change of the map, of the civilization's cities or of the
    /// civilization recomputes every tile.
    local: bool,
    /// The context-local and map classes the relevant uniques read.
    local_deps: CondDeps,
    /// The resources improvements consume.
    consumed: Vec<ResourceId>,
    /// The improvements that consume each of them.
    consumers: Vec<ImprovementSet>,
    /// The improvements that remove features when built, which the removals known decide.
    removers: ImprovementSet,
    /// Whether a filter a job reads asks whether a city works the tile (`worked`).
    reads_worked: bool,
    /// Whether one asks what route the tile has (`Improvement` or `pillaged` leaves).
    reads_route: bool,
    /// Whether one asks about the tile's improvement (`Improvement`, `improved`, `unimproved`,
    /// `pillaged`).
    reads_improvement: bool,
    #[cfg(feature = "stats")]
    counts: core::cell::Cell<JobCounts>,
}

impl JobCaches {
    /// What `rules`' uniques say about the maps, and no map yet.
    pub(crate) fn new(rules: &Ruleset) -> Self {
        let t = rules.uniques();
        let local_classes = CondDeps::LOCAL | CondDeps::MAP;
        let mut civ_deps = CondDeps::empty();
        let mut cond_imps = ImprovementSet::new();
        let mut local = false;
        let mut local_deps = CondDeps::empty();
        let mut consumed: Vec<ResourceId> = Vec::new();
        let mut consumers: Vec<ImprovementSet> = Vec::new();
        let mut removers = ImprovementSet::new();
        let (mut reads_worked, mut reads_route, mut reads_improvement) = (false, false, false);
        let mut note = |id: UniqueId, civ_deps: &mut CondDeps, local: &mut bool| -> bool {
            let u = t.get(id);
            let conds = u.deps();
            let reads = t.reads(id);
            *civ_deps |= (conds | reads).difference(local_classes);
            local_deps |= (conds | reads).intersection(local_classes);
            if conds.intersects(local_classes)
                || reads.intersects(local_classes.difference(CondDeps::TILE))
            {
                *local = true;
            }
            !conds.is_empty() || !reads.difference(local_classes).is_empty()
        };
        for (imp, d) in rules.improvements().iter() {
            for id in d.uniques.ids() {
                let Some(ty) = t.meta(id).ty else { continue };
                if !READ.contains(&ty) {
                    continue;
                }
                if note(id, &mut civ_deps, &mut local) {
                    cond_imps.insert(imp);
                }
                let tiles = match t.get(id).data {
                    UniqueData::MustBeNextTo(x) => Some(x.tiles),
                    UniqueData::CannotBuildOnTile(x) => Some(x.tiles),
                    UniqueData::CanOnlyBeBuiltOnTile(x) => Some(x.tiles),
                    _ => None,
                };
                if let Some(f) = tiles {
                    for leaf in t.filters().tile(f).full.leaves() {
                        match leaf {
                            TileLeaf::Worked => reads_worked = true,
                            TileLeaf::Improvement(_) | TileLeaf::Pillaged => {
                                reads_route = true;
                                reads_improvement = true;
                            }
                            TileLeaf::Improved | TileLeaf::Unimproved => reads_improvement = true,
                            _ => {}
                        }
                    }
                }
                match t.get(id).data {
                    UniqueData::MustBeNextTo(_) => local = true,
                    UniqueData::RemovesFeaturesIfBuilt => {
                        removers.insert(imp);
                    }
                    UniqueData::ConsumesResources(x) => {
                        let i = match consumed.iter().position(|&r| r == x.resource) {
                            Some(i) => i,
                            None => {
                                consumed.push(x.resource);
                                consumers.push(ImprovementSet::new());
                                consumed.len() - 1
                            }
                        };
                        consumers[i].insert(imp);
                    }
                    _ => {}
                }
            }
        }
        for (id, _) in t.iter() {
            let ty = t.meta(id).ty;
            if ty == Some(UniqueType::SpecificImprovementTime)
                || ty == Some(UniqueType::ImprovementTimeIncrease)
            {
                let _ = note(id, &mut civ_deps, &mut local);
            }
        }
        Self {
            maps: RefCell::new(BTreeMap::new()),
            civ_deps,
            cond_imps,
            local,
            local_deps,
            consumed,
            consumers,
            removers,
            reads_worked,
            reads_route,
            reads_improvement,
            #[cfg(feature = "stats")]
            counts: core::cell::Cell::new(JobCounts::default()),
        }
    }
}

/// The land tiles of civilization `p`'s cities but their centres: where its builders work.
fn candidates(g: &Game, p: PlayerId) -> BTreeSet<TileIdx> {
    let mut out = BTreeSet::new();
    for c in g.player_cities(p) {
        let centre = c.tile();
        for t in crate::game::economy::city_tiles(g, c.id()) {
            if t != centre && g.is_land(t) {
                out.insert(t);
            }
        }
    }
    out
}

/// What changed between two looks at a civilization.
#[derive(Default)]
struct Diff {
    imps: ImprovementSet,
    res: ResourceSet,
    removal_turns: bool,
    /// The features whose removal the civilization learned or forgot: what may stand on a tile
    /// with one changes whatever the improvement.
    unlocked: FeatureSet,
}

impl Diff {
    fn of(g: &Game, old: &CivJobs, new: &CivJobs) -> Self {
        let r = g.rules();
        let caches = &g.dv.jobs;
        let mut d = Self::default();
        for (imp, def) in r.improvements().iter() {
            let i = usize::from(imp.0);
            if old.known.contains(imp) != new.known.contains(imp)
                || old.obsolete.contains(imp) != new.obsolete.contains(imp)
                || old.only.contains(imp) != new.only.contains(imp)
                || old.turns.get(i) != new.turns.get(i)
            {
                d.imps.insert(imp);
                if old.turns.get(i) != new.turns.get(i)
                    && matches!(def.kind, ImprovementKind::RemoveFeature(_))
                {
                    d.removal_turns = true;
                }
            }
        }
        for res in r.resources().ids() {
            if old.visible.contains(res) != new.visible.contains(res)
                || old.lux.contains(res) != new.lux.contains(res)
            {
                d.res.insert(res);
            }
        }
        if old.removals != new.removals {
            for imp in caches.removers.iter() {
                d.imps.insert(imp);
            }
            for (f, rem) in r.derived().removal_of.iter() {
                if let Some(i) = *rem
                    && old.removals.contains(i) != new.removals.contains(i)
                {
                    d.unlocked.insert(f);
                }
            }
        }
        for (i, set) in caches.consumers.iter().enumerate() {
            if old.amounts.get(i) != new.amounts.get(i) {
                for imp in set.iter() {
                    d.imps.insert(imp);
                }
            }
        }
        if old.epoch != new.epoch {
            for imp in caches.cond_imps.iter() {
                d.imps.insert(imp);
            }
        }
        d
    }

    fn is_empty(&self) -> bool {
        self.imps.is_empty()
            && self.res.is_empty()
            && !self.removal_turns
            && self.unlocked.is_empty()
    }

    /// Whether the change can move the job on tile `t`, whose entry is `e`: `None` if it can,
    /// else at least what the improvements it changed are worth on the tile now, for
    /// [`Entry::top`] (`NEG_INFINITY` for none).
    fn reach(&self, g: &Game, p: PlayerId, t: TileIdx, e: &Entry, key: &CivJobs) -> Option<f64> {
        let r = g.rules();
        let tile = g.tile(t)?;
        // Under a great improvement the job cannot move, but the bound can.
        let great = e.sig.as_ref().is_some_and(|s| under_great(g, s));
        if tile.resource().is_some_and(|res| self.res.contains(res)) {
            return great.then_some(f64::INFINITY);
        }
        // A removal that takes another time changes what every improvement it clears the way for
        // takes on a tile with such a feature, and one learned or forgotten what may stand there;
        // a change to the improvement standing on the tile (an `Irremovable` that holds no more)
        // may let any other be built over it.
        let removal = |f| r.derived().removal_of.get(f).copied().flatten().is_some();
        let cleared = (self.removal_turns && tile.features().iter().any(removal))
            || tile.features().iter().any(|f| self.unlocked.contains(f))
            || tile.improvement().is_some_and(|i| self.imps.contains(i));
        let fallout = r.derived().known.fallout;
        let fallout_removal = r.derived().removal_of.get(fallout).copied().flatten();
        let (value, best) = match e.job {
            Some((imp, _)) if self.imps.contains(imp) => return None,
            Some((imp, v)) if r.improvements()[imp].kind == ImprovementKind::Normal => {
                (v, Some(imp))
            }
            // A repair comes first, whatever else a tile offers.
            Some((imp, _)) if r.improvements()[imp].kind == ImprovementKind::Repair => {
                return Some(f64::NEG_INFINITY);
            }
            _ if great => (f64::INFINITY, None),
            _ => (0.5, None),
        };
        if tile.features().contains(fallout)
            && fallout_removal.is_some_and(|x| self.imps.contains(x))
        {
            return None;
        }
        let changed = if cleared { &key.only } else { &self.imps };
        let mut raised = f64::NEG_INFINITY;
        for i in changed.iter() {
            let d = &r.improvements()[i];
            if d.kind != ImprovementKind::Normal || d.great || !key.only.contains(i) {
                continue;
            }
            if !could_stand(g, p, t, i) {
                continue;
            }
            // Its build time with the removals it needs first, as a job is weighed.
            let turns = with_removals(g, t, i, key);
            let ub = improvement_value(g, p, t, i) - f64::from(turns) * 0.15;
            // An equal value takes the job when it comes first in the ruleset, as the first of
            // the best does.
            if ub > value || (ub.total_cmp(&value).is_eq() && best.is_some_and(|b| i < b)) {
                return None;
            }
            raised = raised.max(ub);
        }
        Some(raised)
    }
}

/// Brings civilization `p`'s map for builder class `class` up to date.
fn update(g: &Game, p: PlayerId, class: BuilderClass, m: &mut JobMap) {
    let revs = &g.dv.revs;
    let caches = &g.dv.jobs;
    let now = revs.now();
    let first = m.verified == Rev::NEVER;
    let mut all_dirty = first;
    let mut diff = Diff::default();
    let civ_rev = civ_index_full_changed(g, p)
        .max(supply_changed(g, p))
        .max(revs.civ(p).index)
        .max(revs.config)
        .max(if caches.civ_deps.is_empty() {
            Rev::START
        } else {
            cond(g, caches.civ_deps, &Ctx::civ(p))
        });
    if first || civ_rev > m.verified {
        let key = CivJobs::of(g, p, class);
        match &m.key {
            // What a change of the civilization does to a tile is known only by asking on the
            // tile when a relevant unique reads it with its conditionals (a build time `<in
            // [Grassland] tiles>` `CivJobs` asks with no tile): every tile is worked out again.
            _ if caches.local => all_dirty = true,
            None => all_dirty = true,
            Some(old) if *old != key => diff = Diff::of(g, old, &key),
            Some(_) => {}
        }
        m.plan = (!caches.local).then(|| Plan::of(g, p, class, &key));
        m.key = Some(key);
    }
    if caches.local {
        let mut map_rev = revs
            .tile_log
            .rev()
            .max(revs.owners)
            .max(revs.routes)
            .max(revs.worked)
            .max(revs.cities)
            .max(revs.city_core);
        // A city conditional asked on a tile reads the city whose territory it is: whatever of
        // the civilization's cities it reads, their stocks and religion included.
        if caches.local_deps.contains(CondDeps::CITY) {
            for c in g.player_cities(p) {
                map_rev = map_rev.max(revs.city(c.id()).max());
            }
        }
        if map_rev > m.verified {
            all_dirty = true;
        }
    }
    let territory = first || revs.civ(p).cities > m.verified;
    if territory {
        let now_tiles = candidates(g, p);
        m.tiles.retain(|t, _| now_tiles.contains(t));
        for t in now_tiles {
            m.tiles.entry(t).or_insert(Entry {
                sig: None,
                job: None,
                top: None,
                known: false,
                dirty: true,
            });
        }
    }
    let logged: Option<BTreeSet<TileIdx>> =
        if territory || (caches.reads_worked && revs.worked > m.verified) {
            None
        } else {
            revs.tile_log.since(m.verified).map(|it| {
                let mut s = BTreeSet::new();
                for t in it {
                    s.insert(t);
                    s.extend(g.grid().neighbors(t));
                }
                s
            })
        };
    let check: Vec<TileIdx> = match &logged {
        Some(s) => s.iter().copied().filter(|t| m.tiles.contains_key(t)).collect(),
        None => m.tiles.keys().copied().collect(),
    };
    let Some(key) = m.key.clone() else { return };
    for t in check {
        let sig = TileSig::of(g, t);
        if let Some(e) = m.tiles.get_mut(&t)
            && e.sig != sig
        {
            let kept = if e.known && e.job.is_none() {
                kept_without_job(g, p, t, &key, e.sig.as_ref(), sig.as_ref(), e.top)
            } else {
                None
            };
            match kept {
                Some(top) => e.top = top,
                None => e.dirty = true,
            }
            e.sig = sig;
        }
    }
    if !diff.is_empty() {
        for (&t, e) in &mut m.tiles {
            if e.dirty {
                continue;
            }
            match diff.reach(g, p, t, e, &key) {
                None => e.dirty = true,
                Some(raised) => e.top = e.top.map(|top| top.max(raised)),
            }
        }
    }
    let b = Builder::class(p, class);
    #[cfg(feature = "stats")]
    let mut counts = caches.counts.get();
    for (&t, e) in &mut m.tiles {
        if !(e.dirty || all_dirty) {
            continue;
        }
        if e.sig.is_none() {
            e.sig = TileSig::of(g, t);
        }
        let (job, top) = match &m.plan {
            Some(plan) => planned_job(g, p, t, plan, &key, &b),
            None => (best_job(g, &b, t, Some(&key.only)), None),
        };
        #[cfg(feature = "stats")]
        {
            if !e.known {
                counts.fresh += 1;
            } else {
                counts.recomputed += 1;
                if same(e.job, job) {
                    counts.unchanged += 1;
                }
            }
        }
        e.job = job;
        e.top = top;
        e.known = true;
        e.dirty = false;
    }
    #[cfg(feature = "stats")]
    caches.counts.set(counts);
    m.verified = now;
}

/// Whether two jobs are the same, their values as bits.
fn same(a: Option<(ImprovementId, f64)>, b: Option<(ImprovementId, f64)>) -> bool {
    match (a, b) {
        (Some((i, v)), Some((j, w))) => i == j && v.to_bits() == w.to_bits(),
        (None, None) => true,
        _ => false,
    }
}

/// The best job of civilization `p`'s builder class `class` on tile `t`, from its job map; a tile
/// the map does not cover (not its cities' land) is worked out on the spot.
///
/// # Panics
///
/// Never: the map is borrowed mutably only while it is brought up to date, and computing a job
/// reads no job map.
#[must_use]
pub fn job(g: &Game, p: PlayerId, class: BuilderClass, t: TileIdx) -> Option<(ImprovementId, f64)> {
    let caches = &g.dv.jobs;
    let now = g.dv.revs.now();
    let current = caches.maps.borrow().get(&(p, class)).is_some_and(|m| m.verified == now);
    if !current {
        let mut maps = caches.maps.borrow_mut();
        let m = maps.entry((p, class)).or_insert_with(JobMap::new);
        update(g, p, class, m);
    }
    let found = caches.maps.borrow().get(&(p, class)).and_then(|m| m.tiles.get(&t).map(|e| e.job));
    match found {
        Some(job) => job,
        None => best_job(g, &Builder::class(p, class), t, None),
    }
}

/// How many tiles the maps have worked out, afresh, again, and again to the same job.
#[cfg(feature = "stats")]
#[must_use]
pub fn counts(g: &Game) -> JobCounts {
    g.dv.jobs.counts.get()
}

/// Civilization `p`'s whole map for builder class `class` built cold, for the benchmark of a
/// full rebuild (DESIGN.md 10): how many tiles have a job.
#[cfg(feature = "test-ops")]
#[doc(hidden)]
pub fn rebuild_for_bench(g: &Game, p: PlayerId, class: BuilderClass) -> usize {
    let mut m = JobMap::new();
    update(g, p, class, &mut m);
    m.tiles.values().filter(|e| e.job.is_some()).count()
}

/// Every tile's job on civilization `p`'s map for builder class `class`, brought up to date,
/// in tile order: what the tests compare with the direct port.
#[must_use]
pub fn map_jobs(
    g: &Game,
    p: PlayerId,
    class: BuilderClass,
) -> Vec<(TileIdx, Option<(ImprovementId, f64)>)> {
    let tiles: Vec<TileIdx> = candidates(g, p).into_iter().collect();
    tiles.into_iter().map(|t| (t, job(g, p, class, t))).collect()
}

/// The cache oracle for the job maps: each map computed so far, brought up to date, against a
/// fresh look at every tile it covers, with no improvement passed over.
pub(crate) fn verify(g: &Game) -> Vec<String> {
    let keys: Vec<(PlayerId, BuilderClass)> = g.dv.jobs.maps.borrow().keys().copied().collect();
    let mut out = Vec::new();
    for (p, class) in keys {
        let b = Builder::class(p, class);
        let fresh_tiles = candidates(g, p);
        for &t in &fresh_tiles {
            let got = job(g, p, class, t);
            let want = best_job(g, &b, t, None);
            if !same(got, want) {
                out.push(format!(
                    "player {} builder class {}: the job on tile {} is {got:?}, a fresh look says {want:?}",
                    p.0, class.0, t.0
                ));
            }
        }
        let covered: BTreeSet<TileIdx> =
            g.dv.jobs
                .maps
                .borrow()
                .get(&(p, class))
                .map(|m| m.tiles.keys().copied().collect())
                .unwrap_or_default();
        if covered != fresh_tiles {
            out.push(format!(
                "player {} builder class {}: the map covers other tiles",
                p.0, class.0
            ));
        }
    }
    out
}

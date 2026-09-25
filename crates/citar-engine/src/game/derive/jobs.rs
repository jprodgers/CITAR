//! The job maps of worker automation (DESIGN.md 6.5, 6.11, `JobMap`): for each civilization and
//! builder class, the best job ([`crate::game::automation::best_job`]) on every land tile of its
//! cities but their centres.
//!
//! Python cached a tile's best job per unit type for the rest of a turn, keyed by what the tile
//! and the civilization looked like (`automation._best_job`, `automation.py:313-327`), and
//! recomputed every tile each turn. A map here keeps each tile's job with what it was worked out
//! from, and recomputes a tile only when something its job reads has changed:
//! - **the tile** ([`TileSig`]): its terrain, features, natural wonder, resource, improvement and
//!   what is pillaged, its river, owner, a repair or build with no build time at the front of its
//!   queue, which neighbours are fresh water or coast, and, when a filter a job reads asks,
//!   whether a city works it and its route. A road, citizens moving, or a turn of work on its
//!   queue, changes no job and recomputes nothing;
//! - **the civilization** ([`CivJobs`]): which improvements it has the tech for and which are
//!   obsolete, how long each takes its builders, which resources it sees and which luxuries it
//!   has, the removals it knows, the resources its improvements consume. A change recomputes only
//!   the tiles it can reach: those with the resource, those whose job it was, and those where an
//!   improvement it changed could stand and now beat the job (its value on the tile, at its new
//!   build time, above the job's);
//! - **its territory**: tiles joining are worked out, tiles leaving dropped.
//!
//! A ruleset whose relevant uniques have conditionals that read a tile's surroundings, a city or
//! the whole map (or an improvement that must be next to something) recomputes every tile of a map
//! when any tile, owner or city changed: correct, and never the shipped ruleset's case. With the
//! `stats` feature, the maps count the tiles they recompute and those that came out as they were,
//! the redundancy DESIGN.md 10 bounds.

use core::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use super::civ::{civ_index_full_changed, cond, supply_changed};
use super::rev::Rev;
use crate::base::ids::{ImprovementId, PlayerId, ResourceId, TerrainId, TileIdx, UniqueId};
use crate::base::sets::{FeatureSet, ImprovementSet, ResourceSet};
use crate::game::Game;
use crate::game::automation::{best_job, improvement_value, luxury_owned};
use crate::game::workers::{self, Builder};
use crate::rules::Ruleset;
use crate::rules::defs::{BuilderClass, ImprovementKind, Route};
use crate::unique::filter::TileLeaf;
use crate::unique::{CondDeps, Ctx, UniqueData, UniqueType, uq};

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

/// Whether a tile with no job still has none after its improvement changed from what `old`
/// saw to what `new` sees, and nothing else did: every value a job is weighed by is the
/// improvement's yield gain less what the improvement standing there yields and one, so a
/// standing improvement that yields no less than the one before (or than none) can only lower
/// them; one it replaces cannot be built again, and an irremovable one allows less. Not when the
/// improvement before was a great one, over which nothing was weighed, nor when a filter a job
/// reads asks about improvements.
fn still_no_job(g: &Game, old: Option<&TileSig>, new: Option<&TileSig>) -> bool {
    let (Some(old), Some(new)) = (old, new) else { return false };
    if under_great(g, new) {
        return true;
    }
    if g.dv.jobs.reads_improvement || old.pillaged || new.pillaged {
        return false;
    }
    let same_but_improvement = TileSig { improvement: new.improvement, ..*old } == *new;
    let r = g.rules();
    let penalty = |i: Option<ImprovementId>| {
        i.map_or(0.0, |i| crate::game::automation::weighted(&r.improvements()[i].stats) + 1.0)
    };
    same_but_improvement
        && old.improvement.is_none_or(|i| !r.improvements()[i].great)
        && penalty(new.improvement) >= penalty(old.improvement)
}

/// Whether a tile holds a great improvement, whole, on a whole route (or none): no job is weighed
/// over it, and a repair has nothing to mend, so it has no job whatever else changes.
fn under_great(g: &Game, sig: &TileSig) -> bool {
    !sig.pillaged
        && !sig.route_pillaged
        && sig.improvement.is_some_and(|i| g.rules().improvements()[i].great)
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

/// One tile's entry: what its job was worked out from, and the job.
#[derive(Clone, Copy, Debug)]
struct Entry {
    sig: Option<TileSig>,
    job: Option<(ImprovementId, f64)>,
    /// Whether the job has been worked out at all.
    known: bool,
    dirty: bool,
}

/// One civilization's map for one builder class.
#[derive(Clone, Debug)]
struct JobMap {
    verified: Rev,
    key: Option<CivJobs>,
    tiles: BTreeMap<TileIdx, Entry>,
}

impl JobMap {
    const fn new() -> Self {
        Self { verified: Rev::NEVER, key: None, tiles: BTreeMap::new() }
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
    /// Some relevant unique reads beyond a tile and its neighbours: every change of the map
    /// recomputes every tile.
    local: bool,
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
        let mut consumed: Vec<ResourceId> = Vec::new();
        let mut consumers: Vec<ImprovementSet> = Vec::new();
        let mut removers = ImprovementSet::new();
        let (mut reads_worked, mut reads_route, mut reads_improvement) = (false, false, false);
        let note = |id: UniqueId, civ_deps: &mut CondDeps, local: &mut bool| -> bool {
            let u = t.get(id);
            let conds = u.deps();
            let reads = t.reads(id);
            *civ_deps |= (conds | reads).difference(local_classes);
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
        self.imps.is_empty() && self.res.is_empty() && !self.removal_turns
    }

    /// Whether the change can move the job on tile `t`, whose entry is `e`.
    fn reaches(&self, g: &Game, p: PlayerId, t: TileIdx, e: &Entry, key: &CivJobs) -> bool {
        let r = g.rules();
        let Some(tile) = g.tile(t) else { return true };
        if e.sig.as_ref().is_some_and(|s| under_great(g, s)) {
            return false;
        }
        if tile.resource().is_some_and(|res| self.res.contains(res)) {
            return true;
        }
        // A removal that takes another time changes what every improvement it clears the way for
        // takes on a tile with such a feature.
        let removal = |f| r.derived().removal_of.get(f).copied().flatten().is_some();
        let cleared = self.removal_turns && tile.features().iter().any(removal);
        let fallout = r.derived().known.fallout;
        let fallout_removal = r.derived().removal_of.get(fallout).copied().flatten();
        let (value, best) = match e.job {
            Some((imp, _)) if self.imps.contains(imp) => return true,
            Some((imp, v)) if r.improvements()[imp].kind == ImprovementKind::Normal => {
                (v, Some(imp))
            }
            // A repair comes first, whatever else a tile offers.
            Some((imp, _)) if r.improvements()[imp].kind == ImprovementKind::Repair => {
                return false;
            }
            _ => (0.5, None),
        };
        if tile.features().contains(fallout)
            && fallout_removal.is_some_and(|x| self.imps.contains(x))
        {
            return true;
        }
        let changed = if cleared { &key.only } else { &self.imps };
        changed.iter().any(|i| {
            let d = &r.improvements()[i];
            if d.kind != ImprovementKind::Normal || d.great || !key.only.contains(i) {
                return false;
            }
            if !could_stand(g, p, t, i) {
                return false;
            }
            let turns = key.turns.get(usize::from(i.0)).copied().unwrap_or(1);
            let ub = improvement_value(g, p, t, i) - f64::from(turns) * 0.15;
            // An equal value takes the job when it comes first in the ruleset, as the first of
            // the best does.
            ub > value || (ub.total_cmp(&value).is_eq() && best.is_some_and(|b| i < b))
        })
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
            None => all_dirty = true,
            Some(old) if *old != key => diff = Diff::of(g, old, &key),
            Some(_) => {}
        }
        m.key = Some(key);
    }
    let map_rev =
        revs.tile_log.rev().max(revs.owners).max(revs.worked).max(revs.cities).max(revs.city_core);
    if caches.local && map_rev > m.verified {
        all_dirty = true;
    }
    let territory = first || revs.civ(p).cities > m.verified;
    if territory {
        let now_tiles = candidates(g, p);
        m.tiles.retain(|t, _| now_tiles.contains(t));
        for t in now_tiles {
            m.tiles.entry(t).or_insert(Entry { sig: None, job: None, known: false, dirty: true });
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
    for t in check {
        let sig = TileSig::of(g, t);
        if let Some(e) = m.tiles.get_mut(&t)
            && e.sig != sig
        {
            let kept = e.known && e.job.is_none() && still_no_job(g, e.sig.as_ref(), sig.as_ref());
            e.sig = sig;
            e.dirty |= !kept;
        }
    }
    let Some(key) = m.key.clone() else { return };
    if !diff.is_empty() {
        for (&t, e) in &mut m.tiles {
            if !e.dirty && diff.reaches(g, p, t, e, &key) {
                e.dirty = true;
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
        let job = best_job(g, &b, t, Some(&key.only));
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

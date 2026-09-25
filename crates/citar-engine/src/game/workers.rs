//! Tile improvements (`workers.py`, UnCiv's `ImprovementFunctions`, `TileImprovementFunctions`,
//! `Tile.doWorkerTurn`, `MapUnit.canBuildImprovement`, `TileImprovement.getTurnsToBuild` and
//! `UnitActionsPillage`): what may be built where and by whom, how long it takes, the orders a
//! worker is given and the tile's build queue, the work done at the end of each turn, the
//! improvements great people and work boats make at once, feature removal with the production a
//! chopped forest gives, repairs, and pillage with its loot.
//!
//! Work in progress lives on the tile (`Tiles::builds`), as Python's `tile.build` did: a worker
//! standing on the tile with movement left at the end of its turn advances the first entry
//! (stage E6, `progress_builds`).
//!
//! **Who builds** is a [`Builder`]: a unit, whose own uniques and conditionals say what it may
//! build and how fast (the tools, and the direct port of worker automation), or a civilization's
//! builder class, which knows no unit (`derive::jobs`, the job map automated workers read): the
//! class's `Can build [...] improvements on tiles` filters stand for the unit's, and the
//! civilization's uniques alone for the unit's and its owner's.
//!
//! What differs from Python, on purpose:
//! - a tile's improvement changed by a worker or pillaged is not taken out of the memories of the
//!   civilizations that see it (`workers._after_change`): a memory is written when a tile leaves
//!   sight, and every reader looks at the tile itself while it is in sight, so dropping it changed
//!   nothing anyone saw;
//! - the loot of a pillage and the production of a chop are listed in the order of the stats
//!   (food, production, gold, science, culture, happiness, faith), where Python listed them in the
//!   order the ruleset's texts named them;
//! - an improvement cannot replace another (`replaces`), which no ruleset the engine loads
//!   declares: the check a nation's replacement makes is left out.

use serde_json::{Map, Value, json};
use smallvec::SmallVec;

use super::Game;
use super::cities::construction::add_city_stat;
use super::derive::rev::UnitTouch;
use super::error::{ActionError, ErrCode};
use super::triggers;
use super::units::abilities::{consume_action, usable_action_uniques};
use super::units::{remove_unit, unit_has, unit_uniques};
use crate::base::ids::{
    FeatureId, ImprovementId, NationId, PlayerId, ResourceId, TechId, TerrainId, TileIdx, UniqueId,
    UnitId,
};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::{FeatureSet, ImprovementSet, PlayerSet};
use crate::base::stats::{Stat, StatMask, Stats};
use crate::rules::defs::{BuilderClass, ImprovementKind, Route, TerrainType};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::map::{BuildQueue, BuildStep, Tile};
use crate::unique::table::UFlags;
use crate::unique::trigger::{TriggerEvent, TriggerSite};
use crate::unique::{Ctx, UniqueData, UniqueType, applies, uq};

// ---- Who builds ----------------------------------------------------------------------------------

/// Who builds: a unit, or one of a civilization's builder classes (see the module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Builder {
    pub owner: PlayerId,
    /// The unit, whose uniques and conditionals are read.
    pub unit: Option<UnitId>,
    /// The builder class, read in the unit's stead when there is no unit.
    pub class: Option<BuilderClass>,
}

impl Builder {
    /// Unit `u`, as it is.
    #[must_use]
    pub fn unit(g: &Game, u: UnitId) -> Option<Self> {
        let x = g.unit(u)?;
        Some(Self {
            owner: x.owner(),
            unit: Some(u),
            class: g.rules().base_units()[x.base].builder,
        })
    }

    /// Civilization `p`'s builder class `class`.
    #[must_use]
    pub const fn class(p: PlayerId, class: BuilderClass) -> Self {
        Self { owner: p, unit: None, class: Some(class) }
    }

    /// The context its uniques are asked in on tile `t` (`Ctx(g, civ=owner, unit=u, tile=idx)`).
    #[must_use]
    pub const fn ctx(&self, t: TileIdx) -> Ctx {
        Ctx {
            civ: Some(self.owner),
            city: None,
            unit: self.unit,
            tile: Some(t),
            combat: None,
            ignore_conditionals: false,
        }
    }
}

// ---- Improvements and their uniques --------------------------------------------------------------

/// Whether improvement `imp` has a unique of type `ty` that holds in `ctx` (`workers._has`).
fn has(g: &Game, imp: ImprovementId, ty: UniqueType, ctx: &Ctx) -> bool {
    let v = g.view();
    uq::any(uq::object(&v, &g.rules().improvements()[imp].uniques, ty, ctx))
}

/// Whether improvement `imp` carries a unique of type `ty`, whatever its conditionals
/// (`_umap.has_tag`, or `_has` asked with no context).
fn carries(g: &Game, imp: ImprovementId, ty: UniqueType) -> bool {
    super::core::has_type(g.rules(), &g.rules().improvements()[imp].uniques, ty)
}

/// The uniques of type `ty` of improvement `imp` that hold in `ctx` (`workers._uniques`).
fn holding(g: &Game, imp: ImprovementId, ty: UniqueType, ctx: &Ctx) -> SmallVec<[UniqueId; 2]> {
    let v = g.view();
    uq::object(&v, &g.rules().improvements()[imp].uniques, ty, ctx).map(|h| h.id).collect()
}

/// Every unique of type `ty` of an improvement, whatever its conditionals (`_umap.get(ph)`).
fn all_of(g: &Game, imp: ImprovementId, ty: UniqueType) -> impl Iterator<Item = UniqueId> + '_ {
    let t = g.rules().uniques();
    g.rules().improvements()[imp].uniques.ids().filter(move |&id| t.meta(id).ty == Some(ty))
}

/// The improvement's name.
fn imp_name(g: &Game, imp: ImprovementId) -> &str {
    &g.rules().improvements()[imp].name
}

/// Whether a civilization can see a resource yet (`tiles.resource_visible`, `tiles.py:143-148`).
#[must_use]
pub fn resource_visible(g: &Game, p: PlayerId, r: ResourceId) -> bool {
    g.has_tech(p, g.rules().resources()[r].revealed_by)
}

/// Whether a tile belongs to a civilization `p` is at war with (`tiles.is_enemy_territory`,
/// `tiles.py:168-171`).
#[must_use]
pub fn is_enemy_territory(g: &Game, t: TileIdx, p: PlayerId) -> bool {
    g.tile(t).and_then(Tile::owner).is_some_and(|o| g.at_war(p, o))
}

/// Whether a tile is next to fresh water: a river, a lake or an oasis (`tiles.fresh_water`,
/// `tiles.py:111-119`).
#[must_use]
pub fn fresh_water(g: &Game, t: TileIdx) -> bool {
    use crate::unique::TileFacts;
    g.view().tile_fresh_water(t)
}

/// The terrain a feature is.
fn feature_terrain(g: &Game, f: FeatureId) -> Option<TerrainId> {
    g.rules().derived().features.get(f).copied()
}

/// How highly a route ranks: none, a road, a railroad (`workers.ROAD_RANK`).
const fn route_rank(r: Option<Route>) -> u8 {
    match r {
        None => 0,
        Some(Route::Road) => 1,
        Some(Route::Railroad) => 2,
    }
}

/// Whether an improvement may be built on a terrain (`TileImprovement.canBeBuiltOn`,
/// `workers.py:44-46`): its `terrainsCanBeBuiltOn` names it.
#[must_use]
pub fn can_be_built_on(g: &Game, imp: ImprovementId, terrain: TerrainId) -> bool {
    g.rules().improvements()[imp].terrains_can_be_built_on.contains(terrain)
}

/// Whether an improvement may be built on a terrain or feature (`workers.allowed_on_feature`,
/// `workers.py:49-52`): it may be built on it, or it needs no removal of it.
#[must_use]
pub fn allowed_on_feature(g: &Game, imp: ImprovementId, terrain: TerrainId) -> bool {
    if can_be_built_on(g, imp, terrain) {
        return true;
    }
    let r = g.rules();
    let t = r.uniques();
    let feature = r.terrains()[terrain].feature;
    feature.is_some()
        && all_of(g, imp, UniqueType::NoFeatureRemovalNeeded).any(|id| {
            matches!(t.get(id).data, UniqueData::NoFeatureRemovalNeeded(x) if Some(x.feature) == feature)
        })
}

/// The feature removals a civilization may build: those whose tech it knows
/// (`workers.py:175-176`).
fn known_removals(g: &Game, p: PlayerId) -> ImprovementSet {
    let r = g.rules();
    r.derived()
        .feature_removals
        .iter()
        .copied()
        .filter(|&i| g.has_tech(p, r.improvements()[i].tech_required))
        .collect()
}

// ---- Where an improvement may go (workers.py:60-137) -----------------------------------------------

/// Whether improvement `imp` may be built on tile `t` (`TileImprovementFunctions.
/// canImprovementBeBuiltHere`, `workers._built_here_ok`, `workers.py:60-117`): not what is there
/// already, not on a city; a route over less of one; a removal of what is there; nothing over an
/// irremovable improvement; over an unbuildable feature only what may stand on it, or anything
/// once the civilization knows how to remove the feature; what the tile's terrains restrict it to, the
/// improvement's own requirements of the tile and its neighbours, a resource to improve where it
/// only improves one; then its terrains, land or water, fresh water, or the resource it improves.
/// `features` stands in for the tile's (the recursion after a removal); `known` is the removals
/// the civilization knows, `None` where Python passed none.
#[allow(clippy::too_many_arguments, reason = "Python's signature, one argument a concern")]
fn built_here_ok(
    g: &Game,
    t: TileIdx,
    imp: ImprovementId,
    p: PlayerId,
    ctx: &Ctx,
    features: Option<FeatureSet>,
    known: Option<&ImprovementSet>,
) -> bool {
    let r = g.rules();
    let Some(tile) = g.tile(t) else { return false };
    let feats = features.unwrap_or_else(|| tile.features());
    let mut last = feats
        .top()
        .and_then(|f| feature_terrain(g, f))
        .unwrap_or_else(|| tile.wonder().unwrap_or_else(|| tile.terrain()));
    if features.is_none()
        && let Some(w) = tile.wonder()
    {
        last = w;
    }
    let res_visible = tile.resource().is_some_and(|res| resource_visible(g, p, res));
    if tile.improvement() == Some(imp) || g.city_at(t).is_some() {
        return false;
    }
    let def = &r.improvements()[imp];
    match def.kind {
        ImprovementKind::Cancel => return !g.state().tiles().builds(t).is_empty(),
        ImprovementKind::RemoveRoute(route) => return tile.route() == Some(route),
        ImprovementKind::RemoveFeature(f) => return feats.contains(f),
        ImprovementKind::RemoveImprovement(i) => return tile.improvement() == Some(i),
        ImprovementKind::Route(route) => {
            return !g.is_water(t) && route_rank(Some(route)) > route_rank(tile.route());
        }
        ImprovementKind::Normal | ImprovementKind::Repair => {}
    }
    if tile.improvement().is_some_and(|i| has(g, i, UniqueType::Irremovable, ctx)) {
        return false;
    }
    // An unbuildable feature it may not stand on is no obstacle once the civilization knows how
    // to remove it (UnCiv's `canImprovementBeBuiltHere`): the removal is queued first
    // ([`needed_removal`]). Python allowed it only for an improvement that removes features
    // itself, so a farm was never offered on a forest and never queued the forest's removal.
    // refcheck: improvements-over-removable-features
    if r.terrains()[last].unbuildable && !allowed_on_feature(g, imp, last) {
        let Some(known) = known.filter(|k| !k.is_empty()) else { return false };
        let removal = |f: FeatureId| r.derived().removal_of.get(f).copied().flatten();
        let rem: SmallVec<[FeatureId; 3]> =
            feats.iter().filter(|&f| removal(f).is_some()).collect();
        if rem.is_empty() || rem.iter().any(|&f| removal(f).is_none_or(|i| !known.contains(i))) {
            return false;
        }
        let mut left = feats;
        for f in rem {
            left.remove(f);
        }
        return built_here_ok(g, t, imp, p, ctx, Some(left), Some(known));
    }
    let v = g.view();
    let t_uniques = r.uniques();
    let filters = t_uniques.filters();
    // The tile's terrains' restrictions, whatever their conditionals (`workers.py:91-93`): its
    // base terrain and the features as they stand here.
    let mut restricted = false;
    let mut allowed = false;
    let terrains =
        core::iter::once(tile.terrain()).chain(feats.iter().filter_map(|f| feature_terrain(g, f)));
    for terrain in terrains {
        for id in r.terrains()[terrain].uniques.ids() {
            if let UniqueData::RestrictedBuildableImprovements(x) = t_uniques.get(id).data {
                restricted = true;
                allowed |= t_uniques.in_set(x.improvements, imp);
            }
        }
    }
    if restricted && !allowed {
        return false;
    }
    let matches = |f, at| filters.tile_matches(f, &v, at, Some(p));
    for id in holding(g, imp, UniqueType::CannotBuildOnTile, ctx) {
        if let UniqueData::CannotBuildOnTile(x) = t_uniques.get(id).data
            && matches(x.tiles, t)
        {
            return false;
        }
    }
    for id in holding(g, imp, UniqueType::CanOnlyBeBuiltOnTile, ctx) {
        if let UniqueData::CanOnlyBeBuiltOnTile(x) = t_uniques.get(id).data
            && !matches(x.tiles, t)
        {
            return false;
        }
    }
    for id in holding(g, imp, UniqueType::MustBeNextTo, ctx) {
        if let UniqueData::MustBeNextTo(x) = t_uniques.get(id).data
            && !g.grid().neighbors(t).any(|n| matches(x.tiles, n))
        {
            return false;
        }
    }
    let improves_res = res_visible
        && tile.resource().is_some_and(|res| super::tiles::resource_improved_by(g, res, imp));
    if has(g, imp, UniqueType::CanOnlyImproveResource, ctx) && !improves_res {
        return false;
    }
    if allowed_on_feature(g, imp, last) {
        return true;
    }
    if g.is_land(t) && def.on_land {
        return true;
    }
    if g.is_water(t) && def.on_water {
        return true;
    }
    if has(g, imp, UniqueType::ImprovementBuildableByFreshWater, ctx) && fresh_water(g, t) {
        return true;
    }
    improves_res && domain_ok(g, t, imp)
}

/// Whether an improvement that improves the tile's resource suits the tile's domain
/// (`TileImprovementFunctions.extendedDomainCheck`, `workers._domain_ok`, `workers.py:120-137`):
/// the land or water of its terrains, and of the terrains its features occur on.
pub(crate) fn domain_ok(g: &Game, t: TileIdx, imp: ImprovementId) -> bool {
    let r = g.rules();
    let def = &r.improvements()[imp];
    if def.terrains_can_be_built_on.is_empty() && !def.on_land && !def.on_water {
        return true;
    }
    let (mut land, mut water) = (def.on_land, def.on_water);
    for terrain in def.terrains_can_be_built_on.iter() {
        let td = &r.terrains()[terrain];
        for kind in
            core::iter::once(td.kind).chain(td.occurs_on.iter().map(|&o| r.terrains()[o].kind))
        {
            if kind == TerrainType::Water {
                water = true;
            } else {
                land = true;
            }
        }
    }
    (g.is_land(t) && land) || (g.is_water(t) && water)
}

// ---- Why an improvement cannot be built (workers.py:140-179) -------------------------------------

/// One reason an improvement cannot be built (`ImprovementFunctions.
/// getImprovementBuildingProblems`), with Python's sentence ([`Problem::text`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Another nation's improvement.
    UniqueTo(NationId),
    /// Its tech is unknown.
    NeedsTech(TechId),
    /// `Unbuildable` with no conditionals.
    CannotBeBuilt,
    /// `Unbuildable` under conditionals that hold.
    NotNow,
    /// `Unavailable` holds.
    Unavailable,
    /// An `Only available` does not hold: the texts of them all.
    NotYet(Vec<UniqueId>),
    /// `Obsolete with` a known tech.
    Obsolete,
    /// Not enough of a resource it consumes.
    NeedsResource(i32, crate::base::ids::ResourceId),
    /// Outside the civilization's borders (or not next to them, when it may go just outside).
    Borders { just_outside: bool },
    /// The tile does not take it.
    Tile,
}

impl Problem {
    /// Python's sentence about improvement `imp`.
    #[must_use]
    pub fn text(&self, g: &Game, imp: ImprovementId) -> String {
        let r = g.rules();
        let name = imp_name(g, imp);
        match self {
            Self::UniqueTo(n) => format!("{name} is unique to {}.", r.nations()[*n].name),
            Self::NeedsTech(t) => format!("{name} requires {}.", r.techs()[*t].name),
            Self::CannotBeBuilt => format!("{name} cannot be built."),
            Self::NotNow => format!("{name} cannot be built right now."),
            Self::Unavailable => format!("{name} is unavailable."),
            Self::NotYet(ids) => {
                let t = r.uniques();
                let texts: Vec<&str> = ids.iter().map(|&id| t.text_of(id)).collect();
                format!("{name} is not available yet ({}).", texts.join("; "))
            }
            Self::Obsolete => format!("{name} is obsolete."),
            Self::NeedsResource(n, res) => {
                format!("{name} needs {n} {}.", r.resources()[*res].name)
            }
            Self::Borders { just_outside: true } => {
                format!("{name} must be built inside your borders or next to them.")
            }
            Self::Borders { just_outside: false } => {
                format!("{name} must be built inside your borders.")
            }
            Self::Tile => format!("{name} cannot be built on this tile."),
        }
    }

    /// Whether a great person may not even be offered the improvement: it can never be built,
    /// or is another's (`workers.py:353`).
    #[must_use]
    pub const fn rules_out_offer(&self) -> bool {
        matches!(self, Self::CannotBeBuilt | Self::UniqueTo(_))
    }
}

/// The problems as one sentence, as the tools refuse them (`" ".join(probs)`).
#[must_use]
pub fn problems_text(g: &Game, imp: ImprovementId, probs: &[Problem]) -> String {
    probs.iter().map(|p| p.text(g, imp)).collect::<Vec<_>>().join(" ")
}

/// Why improvement `imp` cannot be built on tile `t` by `b` (`workers.building_problems`,
/// `workers.py:140-179`), in Python's order; empty if it can. With `first`, stops at the first.
#[must_use]
pub fn building_problems(
    g: &Game,
    b: &Builder,
    t: TileIdx,
    imp: ImprovementId,
    ignore_tech: bool,
    first: bool,
) -> Vec<Problem> {
    let r = g.rules();
    let def = &r.improvements()[imp];
    let p = b.owner;
    let ctx = b.ctx(t);
    let tu = r.uniques();
    let v = g.view();
    let mut out: Vec<Problem> = Vec::new();
    macro_rules! push {
        ($e:expr) => {{
            out.push($e);
            if first {
                return out;
            }
        }};
    }
    if let Some(n) = def.unique_to
        && g.player(p).is_none_or(|x| x.nation != n)
    {
        push!(Problem::UniqueTo(n));
    }
    if let Some(tech) = def.tech_required
        && !ignore_tech
        && !g.has_tech(p, Some(tech))
    {
        push!(Problem::NeedsTech(tech));
    }
    let unbuildable: SmallVec<[UniqueId; 2]> = all_of(g, imp, UniqueType::Unbuildable).collect();
    let repair = def.kind == ImprovementKind::Repair;
    if unbuildable.iter().any(|&id| tu.get(id).conds.is_empty()) && !repair {
        push!(Problem::CannotBeBuilt);
    } else if unbuildable.iter().any(|&id| !tu.get(id).conds.is_empty() && applies(id, &ctx, &v)) {
        push!(Problem::NotNow);
    }
    if all_of(g, imp, UniqueType::Unavailable).any(|id| applies(id, &ctx, &v)) {
        push!(Problem::Unavailable);
    }
    let only: SmallVec<[UniqueId; 2]> = all_of(g, imp, UniqueType::OnlyAvailable).collect();
    if only.iter().any(|&id| !applies(id, &ctx, &v)) {
        push!(Problem::NotYet(only.to_vec()));
    }
    if holding(g, imp, UniqueType::ObsoleteWith, &ctx).into_iter().any(
        |id| matches!(tu.get(id).data, UniqueData::ObsoleteWith(x) if g.has_tech(p, Some(x.tech))),
    ) {
        push!(Problem::Obsolete);
    }
    for id in holding(g, imp, UniqueType::ConsumesResources, &ctx) {
        if let UniqueData::ConsumesResources(x) = tu.get(id).data
            && super::economy::resource_amount(g, p, x.resource) < x.amount
        {
            push!(Problem::NeedsResource(x.amount, x.resource));
        }
    }
    let owner = g.tile(t).and_then(Tile::owner);
    if owner != Some(p) && !has(g, imp, UniqueType::CanBuildOutsideBorders, &ctx) {
        let just = has(g, imp, UniqueType::CanBuildJustOutsideBorders, &ctx);
        let next_to = g.grid().neighbors(t).any(|n| g.tile(n).and_then(Tile::owner) == Some(p));
        if !just || !next_to {
            push!(Problem::Borders { just_outside: just });
        }
    }
    let known = known_removals(g, p);
    if !built_here_ok(g, t, imp, p, &ctx, None, Some(&known)) {
        push!(Problem::Tile);
    }
    out
}

/// Whether `b` may build `imp` on `t` for all [`building_problems`] say.
#[must_use]
pub fn no_problems(g: &Game, b: &Builder, t: TileIdx, imp: ImprovementId) -> bool {
    building_problems(g, b, t, imp, false, true).is_empty()
}

// ---- What a builder may build (workers.py:182-231) -----------------------------------------------

/// The front of a tile's build queue.
fn front(g: &Game, t: TileIdx) -> Option<BuildStep> {
    g.state().tiles().builds(t).first().copied()
}

/// Whether `b` has any `Can build [...] improvements on tiles` where it would build on `t`, and
/// whether one of them names `imp` or the tile's terrain.
fn builds(g: &Game, b: &Builder, t: TileIdx, imp: Option<ImprovementId>) -> (bool, bool) {
    let r = g.rules();
    let tu = r.uniques();
    let v = g.view();
    let names = |o: crate::base::ids::ObjectFilterId| {
        let of = tu.object(o);
        imp.is_some_and(|i| of.improvements.is_some_and(|s| tu.in_set(s, i)))
            || of.tiles.is_some_and(|f| tu.filters().tile_terrain_matches(f, &v, t, Some(b.owner)))
    };
    match b.unit {
        Some(u) => {
            let ctx = b.ctx(t);
            let mut any = false;
            let mut named = false;
            for h in uq::unit(&v, u, UniqueType::BuildImprovements, &ctx) {
                if let UniqueData::BuildImprovements(x) = *h.data() {
                    any = true;
                    named |= names(x.object);
                }
            }
            (any, named)
        }
        None => match b.class.and_then(|c| r.derived().builder_classes.get(usize::from(c.0))) {
            Some(filters) => (!filters.is_empty(), filters.iter().any(|&o| names(o))),
            None => (false, false),
        },
    }
}

/// Whether `b` may build `imp` on `t`, as the unit's side of it (`MapUnit.canBuildImprovement`,
/// `workers.unit_can_build`, `workers.py:182-202`): no barbarian; an improvement with no build
/// time only while it is in progress; a repair anywhere but in enemy land; the improvement's
/// availability; one of the builder's filters naming it or the tile's terrain.
#[must_use]
pub fn unit_can_build(g: &Game, b: &Builder, imp: ImprovementId, t: TileIdx) -> bool {
    if g.is_barbarian(b.owner) {
        return false;
    }
    let r = g.rules();
    let def = &r.improvements()[imp];
    let in_progress = front(g, t).map(|s| s.improvement);
    if def.turns_to_build.is_none()
        && def.kind != ImprovementKind::Cancel
        && in_progress != Some(imp)
    {
        return false;
    }
    let repair = r.derived().known.repair;
    if repair.is_some() && (in_progress == repair || Some(imp) == repair) {
        return builds(g, b, t, None).0 && !is_enemy_territory(g, t, b.owner);
    }
    let ctx = b.ctx(t);
    let v = g.view();
    if all_of(g, imp, UniqueType::OnlyAvailable).any(|id| !applies(id, &ctx, &v))
        || all_of(g, imp, UniqueType::Unavailable).any(|id| applies(id, &ctx, &v))
    {
        return false;
    }
    builds(g, b, t, Some(imp)).1
}

/// Whether `b` has any `Can build [...] improvements on tiles` on tile `t`.
#[must_use]
pub fn can_build_anything(g: &Game, b: &Builder, t: TileIdx) -> bool {
    builds(g, b, t, None).0
}

/// How many turns `b` takes to build `imp` (`TileImprovement.getTurnsToBuild`,
/// `workers.turns_to_build`, `workers.py:205-221`): its build time scaled by the speed, divided
/// by the builder's rate increases and multiplied by each of its improvement-specific times,
/// rounded half up, at least 1. A unit's own uniques count with its owner's; a builder class
/// has its civilization's alone.
#[must_use]
pub fn turns_to_build(g: &Game, b: &Builder, imp: ImprovementId, t: TileIdx) -> i32 {
    turns_in(g, b, imp, &b.ctx(t))
}

/// [`turns_to_build`] with its uniques asked in `ctx`: the job map asks them for a civilization
/// with no tile, when no unique of the kind reads one.
#[must_use]
pub fn turns_in(g: &Game, b: &Builder, imp: ImprovementId, ctx: &Ctx) -> i32 {
    let r = g.rules();
    let tu = r.uniques();
    let base = f64::from(r.improvements()[imp].turns_to_build.unwrap_or(0));
    let v = g.view();
    let ctx = *ctx;
    let mut speed: SmallVec<[f64; 2]> = SmallVec::new();
    let mut increase = 0.0;
    let mut any_increase = false;
    let mut visit = |h: uq::Hit<'_>| match *h.data() {
        UniqueData::SpecificImprovementTime(x) if tu.in_set(x.improvements, imp) => {
            for _ in 0..h.n {
                speed.push(f64::from(x.percent));
            }
        }
        UniqueData::ImprovementTimeIncrease(x) if tu.in_set(x.improvements, imp) => {
            any_increase = true;
            increase += f64::from(x.percent) * f64::from(h.n);
        }
        _ => {}
    };
    for ty in [UniqueType::SpecificImprovementTime, UniqueType::ImprovementTimeIncrease] {
        match b.unit {
            Some(u) => uq::unit_and_civ(&v, u, ty, &ctx).for_each(&mut visit),
            None => uq::civ(&v, b.owner, ty, &ctx).for_each(&mut visit),
        }
    }
    let increase = if any_increase { 1.0 + increase / 100.0 } else { 1.0 };
    let mut turns = if increase == 0.0 {
        0.0
    } else {
        g.speed().improvement_build_length_modifier * base / increase
    };
    for pct in speed {
        turns *= 1.0 + pct / 100.0;
    }
    num::floor_i32(turns + 0.5).max(1)
}

/// How long repairing what is pillaged on `t` takes (`workers.repair_turns`, `workers.py:224-231`):
/// what is left of a repair under way; otherwise a repair's time, or what building the pillaged
/// improvement or route anew would take if that is less.
#[must_use]
pub fn repair_turns(g: &Game, b: &Builder, t: TileIdx) -> i32 {
    let r = g.rules();
    let known = &r.derived().known;
    let Some(repair) = known.repair else { return 1 };
    if let Some(s) = front(g, t)
        && s.improvement == repair
    {
        return i32::from(s.turns_left);
    }
    let rt = turns_to_build(g, b, repair, t);
    let Some(tile) = g.tile(t) else { return rt };
    let target = match tile.improvement() {
        Some(i) if tile.improvement_pillaged() => Some(i),
        _ => tile.route().map(|x| match x {
            Route::Road => known.road,
            Route::Railroad => known.railroad,
        }),
    };
    target.map_or(rt, |i| rt.min(turns_to_build(g, b, i, t)))
}

// ---- Options and orders (workers.py:237-341) ------------------------------------------------------

/// What a builder may start on a tile (`workers.build_options`' entries, `workers.py:237-266`).
#[derive(Clone, Debug, PartialEq)]
pub struct BuildOption {
    pub imp: ImprovementId,
    /// Turns, with a removal it needs first.
    pub turns: i32,
    /// The feature to remove first.
    pub first_removes: Option<FeatureId>,
    /// The improvement it would replace.
    pub replaces: Option<ImprovementId>,
    /// A repair.
    pub repair: bool,
}

/// Whether something on `t` is pillaged that a repair would mend.
pub(crate) fn repairable(tile: &Tile) -> bool {
    (tile.improvement_pillaged() && tile.improvement().is_some())
        || (tile.route().is_some() && tile.route_pillaged())
}

/// What `b` may start building on tile `t`, repair first, then every improvement it may build
/// there in the ruleset's order (`workers.build_options`, `workers.py:237-261`), the instant
/// improvements left out ([`water_options`], [`great_options`]). `only` limits the improvements
/// looked at.
#[must_use]
pub fn build_options(
    g: &Game,
    b: &Builder,
    t: TileIdx,
    only: Option<&ImprovementSet>,
) -> Vec<BuildOption> {
    let mut out = Vec::new();
    let r = g.rules();
    if !can_build_anything(g, b, t) || g.is_barbarian(b.owner) {
        return out;
    }
    let Some(tile) = g.tile(t) else { return out };
    let known = &r.derived().known;
    if let Some(repair) = known.repair
        && repairable(tile)
        && !is_enemy_territory(g, t, b.owner)
    {
        out.push(BuildOption {
            imp: repair,
            turns: repair_turns(g, b, t),
            first_removes: None,
            replaces: None,
            repair: true,
        });
    }
    for (imp, def) in r.improvements().iter() {
        if matches!(def.kind, ImprovementKind::Repair | ImprovementKind::Cancel)
            || only.is_some_and(|s| !s.contains(imp))
        {
            continue;
        }
        if !unit_can_build(g, b, imp, t) || !no_problems(g, b, t, imp) {
            continue;
        }
        let mut turns = turns_to_build(g, b, imp, t);
        let first_removes = needed_removal(g, t, imp);
        if let Some(f) = first_removes
            && let Some(rem) = r.derived().removal_of.get(f).copied().flatten()
        {
            turns = turns.saturating_add(turns_to_build(g, b, rem, t));
        }
        let replaces = tile
            .improvement()
            .filter(|_| matches!(def.kind, ImprovementKind::Normal | ImprovementKind::Repair));
        out.push(BuildOption { imp, turns, first_removes, replaces, repair: false });
    }
    out
}

/// The feature to remove before `imp` can be built on `t` (`workers._needed_removal`,
/// `workers.py:269-278`): the highest unbuildable feature it may not stand on, which a removal
/// improvement exists for; none for a route, a removal, or what removes features itself.
#[must_use]
pub fn needed_removal(g: &Game, t: TileIdx, imp: ImprovementId) -> Option<FeatureId> {
    let r = g.rules();
    let def = &r.improvements()[imp];
    if matches!(
        def.kind,
        ImprovementKind::Route(_)
            | ImprovementKind::RemoveFeature(_)
            | ImprovementKind::RemoveRoute(_)
            | ImprovementKind::RemoveImprovement(_)
    ) || carries(g, imp, UniqueType::RemovesFeaturesIfBuilt)
    {
        return None;
    }
    let feats = g.tile(t)?.features();
    let mut ids: SmallVec<[FeatureId; 3]> = feats.iter().collect();
    ids.reverse();
    ids.into_iter().find(|&f| {
        r.derived().removal_of.get(f).copied().flatten().is_some()
            && feature_terrain(g, f)
                .is_some_and(|ft| r.terrains()[ft].unbuildable && !allowed_on_feature(g, imp, ft))
    })
}

/// An improvement a unit makes at once: a great person's (`workers.great_improvement_options`,
/// `workers.py:344-371`) or a work boat's (`workers._water_option`, `workers.py:324-341`).
#[derive(Clone, Debug, PartialEq)]
pub struct InstantOption {
    pub imp: ImprovementId,
    /// The great person's `Can instantly construct [...]` it uses; `None` for a work boat, which
    /// is used up whole.
    pub action: Option<UniqueId>,
    /// Why it cannot be made now (the tile, the tech, the borders).
    pub blocked: Vec<Problem>,
}

/// The improvements unit `u` could make at once where it stands, a work boat's first, then its
/// great improvements (`workers.py:262-265`, in `actions.py:98` the other way round, which
/// [`super::actions`] keeps).
#[must_use]
pub fn water_options(g: &Game, u: UnitId) -> Vec<InstantOption> {
    let Some(b) = Builder::unit(g, u) else { return Vec::new() };
    let Some(t) = g.unit(u).map(crate::state::units::Unit::tile) else { return Vec::new() };
    let Some(res) = g.tile(t).and_then(Tile::resource) else { return Vec::new() };
    if !g.is_water(t) || unit_uniques(g, u, UniqueType::CreateWaterImprovements, false).is_empty() {
        return Vec::new();
    }
    let r = g.rules();
    r.improvements()
        .ids()
        .find(|&imp| super::tiles::resource_improved_by(g, res, imp) && no_problems(g, &b, t, imp))
        .map(|imp| vec![InstantOption { imp, action: None, blocked: Vec::new() }])
        .unwrap_or_default()
}

/// The great improvements unit `u` could make where it stands, by its usable `Can instantly
/// construct [...]` uniques (`workers.great_improvement_options`): each improvement a unique
/// names that is not ruled out, with what blocks it now.
#[must_use]
pub fn great_options(g: &Game, u: UnitId) -> Vec<InstantOption> {
    let Some(b) = Builder::unit(g, u) else { return Vec::new() };
    let Some(t) = g.unit(u).map(crate::state::units::Unit::tile) else { return Vec::new() };
    let r = g.rules();
    let tu = r.uniques();
    let mut out = Vec::new();
    for id in usable_action_uniques(g, u, UniqueType::ConstructImprovementInstantly) {
        let UniqueData::ConstructImprovementInstantly(x) = tu.get(id).data else { continue };
        for imp in r.improvements().ids() {
            if !tu.in_set(x.improvements, imp) {
                continue;
            }
            let blocked = building_problems(g, &b, t, imp, false, false);
            if blocked.iter().any(Problem::rules_out_offer) {
                continue;
            }
            out.push(InstantOption { imp, action: Some(id), blocked });
        }
    }
    out
}

/// Why unit `u` cannot make the instant improvement `o` now (the checks of its `act`,
/// `workers.py:332-335, 357-364`).
///
/// # Errors
/// No movement left, the improvement blocked, or the tile kept for a building's improvement.
pub fn check_instant(g: &Game, u: UnitId, o: &InstantOption) -> Result<(), ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::new(ErrCode::NoSuchUnit, "No such unit."))?;
    if x.moves <= 0 {
        return Err(ActionError::rule("The unit has no movement left."));
    }
    if o.action.is_some() {
        let b = Builder::unit(g, u).ok_or_else(|| ActionError::rule("No such unit."))?;
        let probs = building_problems(g, &b, x.tile(), o.imp, false, false);
        if !probs.is_empty() {
            return Err(ActionError::rule(problems_text(g, o.imp, &probs)));
        }
        if front(g, x.tile()).is_some_and(|s| s.turns_left < 0) {
            return Err(ActionError::rule("This tile is reserved for a building's improvement."));
        }
    }
    Ok(())
}

/// Unit `u` makes the instant improvement `o` [`check_instant`] allowed: a great person spends
/// the use of its action, a work boat is used up (`workers.py:336-338, 365-368`).
pub fn apply_instant(g: &mut Game, u: UnitId, o: &InstantOption) -> Value {
    let Some((owner, t)) = g.unit(u).map(|x| (x.owner(), x.tile())) else { return Value::Null };
    set_improvement(g, t, o.imp, Some(owner), Some(u));
    let name = imp_name(g, o.imp).to_owned();
    match o.action {
        Some(id) => {
            if g.unit(u).is_some() {
                consume_action(g, u, id);
            }
            json!({ "created": name })
        }
        None => {
            remove_unit(g, u);
            json!({ "created": name, "unit_consumed": true })
        }
    }
}

// ---- The build_improvement tool (workers.start_build, workers.py:281-321) -------------------------

/// What `build_improvement` does.
#[derive(Clone, Debug, PartialEq)]
pub enum BuildPlan {
    /// Clear the tile's queue and the unit's order.
    Cancel(UnitId, TileIdx),
    /// Make an improvement at once.
    Instant(UnitId, InstantOption),
    /// Queue it (with a removal first, if one is needed) unless it is queued last already.
    Queue { unit: UnitId, tile: TileIdx, imp: ImprovementId },
}

/// Whether unit `u` may start building what `target` names on its tile (`workers.start_build`'s
/// checks): a repair, a cancellation, an instant improvement, or one it may build now.
///
/// # Errors
/// An unknown improvement, nothing to repair, one the unit cannot build, or its problems.
pub fn plan_build(g: &Game, u: UnitId, target: &str) -> Result<BuildPlan, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::new(ErrCode::NoSuchUnit, "No such unit."))?;
    let t = x.tile();
    let r = g.rules();
    let known = &r.derived().known;
    let lower = target.to_lowercase();
    let imp = if lower == "repair" {
        known.repair.ok_or_else(|| ActionError::rule("Unknown improvement 'repair'."))?
    } else if lower == "cancel" || lower == "cancel improvement order" {
        return Ok(BuildPlan::Cancel(u, t));
    } else {
        r.resolve::<ImprovementId>(target)
            .ok_or_else(|| ActionError::rule(format!("Unknown improvement '{target}'.")))?
    };
    let instants = {
        let mut v = water_options(g, u);
        v.extend(great_options(g, u));
        v
    };
    if let Some(o) = instants.into_iter().find(|o| o.imp == imp) {
        check_instant(g, u, &o)?;
        return Ok(BuildPlan::Instant(u, o));
    }
    let b = Builder::unit(g, u).ok_or_else(|| ActionError::rule("No such unit."))?;
    if Some(imp) == known.repair {
        let tile = g.tile(t).ok_or_else(|| ActionError::rule("No such tile."))?;
        if !can_build_anything(g, &b, t)
            || g.is_barbarian(b.owner)
            || !repairable(tile)
            || is_enemy_territory(g, t, b.owner)
        {
            return Err(ActionError::rule("There is nothing here this unit can repair."));
        }
    } else {
        if !unit_can_build(g, &b, imp, t) {
            let unit = r.base_units()[x.base].name.clone();
            return Err(ActionError::rule(format!("A {unit} cannot build {}.", imp_name(g, imp))));
        }
        let probs = building_problems(g, &b, t, imp, false, false);
        if !probs.is_empty() {
            return Err(ActionError::rule(problems_text(g, imp, &probs)));
        }
    }
    Ok(BuildPlan::Queue { unit: u, tile: t, imp })
}

/// Carries out what [`plan_build`] allowed, and says what the tool returns.
pub fn apply_build(g: &mut Game, plan: BuildPlan) -> Value {
    match plan {
        BuildPlan::Cancel(u, t) => {
            let cleared = g.set_builds(t, BuildQueue::new());
            debug_assert!(cleared.is_ok(), "the unit's tile is on the map: {cleared:?}");
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                x.activity = None;
            }
            json!({ "cancelled": true })
        }
        BuildPlan::Instant(u, o) => apply_instant(g, u, &o),
        BuildPlan::Queue { unit, tile, imp } => {
            let Some(b) = Builder::unit(g, unit) else { return Value::Null };
            let r = g.rules();
            let current = g.state().tiles().builds(tile).last().map(|s| s.improvement);
            if current != Some(imp) {
                let mut queue = BuildQueue::new();
                let repair = Some(imp) == r.derived().known.repair;
                if !repair
                    && let Some(f) = needed_removal(g, tile, imp)
                    && let Some(rem) = r.derived().removal_of.get(f).copied().flatten()
                {
                    queue.push(BuildStep {
                        improvement: rem,
                        turns_left: turns16(turns_to_build(g, &b, rem, tile)),
                    });
                }
                let turns = if repair {
                    repair_turns(g, &b, tile)
                } else {
                    turns_to_build(g, &b, imp, tile)
                };
                queue.push(BuildStep { improvement: imp, turns_left: turns16(turns) });
                let set = g.set_builds(tile, queue);
                debug_assert!(set.is_ok(), "the unit's tile is on the map: {set:?}");
            }
            if let Some(x) = g.unit_mut(unit, UnitTouch::CORE) {
                x.activity = Some(crate::state::units::Activity::Build);
                x.goto = None;
                x.path.clear();
                x.order_wait = 0;
            }
            let steps = g.state().tiles().builds(tile);
            let total: i32 = steps.iter().map(|s| i32::from(s.turns_left)).sum();
            let names: Vec<&str> = steps.iter().map(|s| imp_name(g, s.improvement)).collect();
            json!({ "building": imp_name(g, imp), "turns": total, "queue": names })
        }
    }
}

/// Turns as a build step holds them.
fn turns16(n: i32) -> i16 {
    i16::try_from(n).unwrap_or(i16::MAX)
}

/// `build_improvement`: orders a worker to build on its tile, a removal queued first where one is
/// needed (`tools.build_improvement`, `tools.py:528-537`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuildImprovement {
    pub unit_id: i64,
    pub improvement: String,
}

impl super::action::Rule for BuildImprovement {
    type Plan = BuildPlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<BuildPlan, ActionError> {
        let u = super::units::actions::own_unit(g, pid, self.unit_id)?;
        plan_build(g, u, &self.improvement)
    }

    fn apply(self, g: &mut Game, _: PlayerId, plan: BuildPlan) -> super::action::OutcomeSpec {
        super::action::OutcomeSpec::value(apply_build(g, plan))
    }
}

// ---- Applying improvements (workers.py:377-484) ---------------------------------------------------

/// Builds improvement `imp` on tile `t` (`TileImprovementFunctions.setImprovement`,
/// `workers.set_improvement`, `workers.py:377-418`): a removal takes away what it removes, a route
/// is laid whole, a repair mends what is pillaged, an improvement replaces what was there (and
/// drops the routes still queued); one that removes features takes them. For a civilization, the
/// improvement's one-time uniques apply and `upon building a [...] improvement` fires, with the
/// unit that built it.
pub fn set_improvement(
    g: &mut Game,
    t: TileIdx,
    imp: ImprovementId,
    p: Option<PlayerId>,
    unit: Option<UnitId>,
) {
    let r = g.rules();
    let def = &r.improvements()[imp];
    let Some(tile) = g.tile(t).copied() else { return };
    let refused = |e: crate::state::StateError| {
        debug_assert!(false, "a tile on the map takes an improvement: {e}");
    };
    match def.kind {
        ImprovementKind::RemoveFeature(_)
        | ImprovementKind::RemoveRoute(_)
        | ImprovementKind::RemoveImprovement(_) => activate_removal(g, t, imp, p),
        ImprovementKind::Route(route) => {
            g.set_route(t, Some(route)).unwrap_or_else(refused);
            g.set_pillaged(t, false, tile.improvement_pillaged()).unwrap_or_else(refused);
        }
        ImprovementKind::Repair => {
            g.set_builds(t, BuildQueue::new()).unwrap_or_else(refused);
            if tile.improvement_pillaged() {
                g.set_pillaged(t, tile.route_pillaged(), false).unwrap_or_else(refused);
            } else {
                g.set_pillaged(t, false, false).unwrap_or_else(refused);
            }
        }
        ImprovementKind::Normal | ImprovementKind::Cancel => {
            g.set_pillaged(t, tile.route_pillaged(), false).unwrap_or_else(refused);
            g.set_improvement(t, Some(imp)).unwrap_or_else(refused);
            let queue = g.state().tiles().builds(t);
            if !queue.is_empty() {
                let kept: BuildQueue = queue
                    .iter()
                    .copied()
                    .filter(|s| {
                        !matches!(r.improvements()[s.improvement].kind, ImprovementKind::Route(_))
                    })
                    .collect();
                if kept.len() != queue.len() {
                    g.set_builds(t, kept).unwrap_or_else(refused);
                }
            }
        }
    }
    if carries(g, imp, UniqueType::RemovesFeaturesIfBuilt)
        && let Some(now) = g.tile(t)
    {
        let mut feats = now.features();
        for f in now.features().iter() {
            let removable = r.derived().removal_of.get(f).copied().flatten().is_some();
            if removable && feature_terrain(g, f).is_some_and(|ft| !allowed_on_feature(g, imp, ft))
            {
                feats.remove(f);
            }
        }
        if feats != now.features() {
            g.set_features(t, feats).unwrap_or_else(refused);
        }
    }
    if let Some(p) = p {
        let site = TriggerSite { civ: p, city: None, unit, tile: Some(t) };
        triggers::on_gain(g, &r.improvements()[imp].uniques, &site, None);
        triggers::fire(g, &site, &TriggerEvent::BuildingImprovement(imp), true, None);
    }
}

/// Completes a removal (`workers._activate_removal`, `workers.py:434-455`): an improvement built
/// for the feature removed goes with it; a route, an improvement or a feature is taken away, and
/// a feature that gives production when removed gives it to the nearest city.
fn activate_removal(g: &mut Game, t: TileIdx, imp: ImprovementId, p: Option<PlayerId>) {
    let r = g.rules();
    let Some(tile) = g.tile(t).copied() else { return };
    let refused = |e: crate::state::StateError| {
        debug_assert!(false, "a tile on the map takes a removal: {e}");
    };
    let kind = r.improvements()[imp].kind;
    if let (Some(on), ImprovementKind::RemoveFeature(f)) = (tile.improvement(), kind)
        && tile.features().contains(f)
        && let Some(ft) = feature_terrain(g, f)
    {
        let built_on = &r.improvements()[on].terrains_can_be_built_on;
        if built_on.contains(ft) && !built_on.contains(tile.terrain()) {
            g.set_improvement(t, None).unwrap_or_else(refused);
            g.set_pillaged(t, tile.route_pillaged(), false).unwrap_or_else(refused);
        }
    }
    match kind {
        ImprovementKind::RemoveRoute(_) => {
            g.set_route(t, None).unwrap_or_else(refused);
            let pillaged = g.tile(t).is_some_and(Tile::improvement_pillaged);
            g.set_pillaged(t, false, pillaged).unwrap_or_else(refused);
        }
        ImprovementKind::RemoveImprovement(i) => {
            if g.tile(t).and_then(Tile::improvement) == Some(i) {
                g.set_improvement(t, None).unwrap_or_else(refused);
            }
        }
        ImprovementKind::RemoveFeature(f) => {
            if let (Some(p), Some(ft)) = (p, feature_terrain(g, f))
                && super::core::has_type(
                    r,
                    &r.terrains()[ft].uniques,
                    UniqueType::ProductionBonusWhenRemoved,
                )
            {
                chop_bonus(g, t, ft, p);
            }
            if let Some(now) = g.tile(t)
                && now.features().contains(f)
            {
                let mut feats = now.features();
                feats.remove(f);
                g.set_features(t, feats).unwrap_or_else(refused);
            }
        }
        _ => {}
    }
}

/// The production a civilization's nearest city within five tiles gets from a feature cleared
/// on tile `t` (`workers._chop_bonus`, `workers.py:458-484`): the feature's `Provides [stats] when
/// removed`, scaled by the speed where it says so, by `(6 - distance) / 4` beyond one tile, and
/// by two thirds outside the civilization's own land; announced to it.
fn chop_bonus(g: &mut Game, t: TileIdx, feature: TerrainId, p: PlayerId) {
    let Some((city, dist)) = nearest_city(g, p, t) else { return };
    if dist > 5 {
        return;
    }
    let r = g.rules();
    let tu = r.uniques();
    let mut total = Stats::ZERO;
    let mut named = StatMask::default();
    for id in r.terrains()[feature].uniques.ids() {
        let UniqueData::ProductionBonusWhenRemoved(x) = tu.get(id).data else { continue };
        let mult =
            if tu.get(id).flags().contains(UFlags::SPEED) { g.speed().modifier } else { 1.0 };
        let stats = tu.stats(x.stats);
        for s in Stat::ALL {
            if stats[s] != 0.0 {
                total[s] += stats[s] * mult;
                named.insert(s);
            }
        }
    }
    if named.is_empty() {
        return;
    }
    if dist != 1 {
        for s in named.iter() {
            total[s] = total[s] * f64::from(6 - dist) / 4.0;
        }
    }
    let own =
        g.tile(t).and_then(Tile::city).and_then(|c| g.city(c)).is_some_and(|c| c.owner() == p);
    if !own {
        for s in named.iter() {
            total[s] = total[s] * 2.0 / 3.0;
        }
    }
    for s in named.iter() {
        add_city_stat(g, city, s, f64::from(num::trunc_i32(total[s])));
    }
    let parts: Vec<String> =
        named.iter().map(|s| format!("{} {}", num::trunc_i32(total[s]), s.key())).collect();
    let city_name = g.city(city).map(|c| c.name.to_string()).unwrap_or_default();
    let text = format!(
        "Clearing a {} has created {} for {city_name}.",
        r.terrains()[feature].name,
        parts.join(", ")
    );
    g.emit(
        EngineEvent::Chop,
        &text,
        Some(PlayerSet::single(p)),
        Some(t),
        EventData::default(),
        &[],
    );
}

/// A civilization's city nearest a tile, the first of the nearest in id order, and how far it is.
fn nearest_city(g: &Game, p: PlayerId, t: TileIdx) -> Option<(crate::base::ids::CityId, u32)> {
    let mut best: Option<(crate::base::ids::CityId, u32)> = None;
    for c in g.player_cities(p) {
        let d = g.grid().distance(c.tile(), t);
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((c.id(), d));
        }
    }
    best
}

/// Stage E6, worker builds (`workers.progress_builds`, `UnitTurnManager.endTurn` ->
/// `Tile.doWorkerTurn`, `workers.py:487-514`): each unit of the civilization with movement left
/// that stands on a tile being improved, and may build what is at the front of its queue, does a
/// turn of work; what is finished is built, unless it can no longer be (the borders moved), and
/// announced. A builder whose tile has nothing left to build drops its order.
pub(crate) fn progress_builds(g: &mut Game, p: PlayerId) {
    let ids: Vec<UnitId> = g.player_units(p).map(crate::state::units::Unit::id).collect();
    for u in ids {
        let Some((t, moves, activity)) = g.unit(u).map(|x| (x.tile(), x.moves, x.activity)) else {
            continue;
        };
        if moves <= 0 {
            continue;
        }
        let building = activity == Some(crate::state::units::Activity::Build);
        let Some(step) = front(g, t).filter(|s| s.turns_left >= 0) else {
            if building
                && g.state().tiles().builds(t).is_empty()
                && let Some(x) = g.unit_mut(u, UnitTouch::CORE)
            {
                x.activity = None;
            }
            continue;
        };
        let Some(b) = Builder::unit(g, u) else { continue };
        if !unit_can_build(g, &b, step.improvement, t) {
            continue;
        }
        let left = (step.turns_left - 1).max(0);
        let mut queue: BuildQueue = g.state().tiles().builds(t).iter().copied().collect();
        if left > 0 {
            queue[0].turns_left = left;
            let set = g.set_builds(t, queue);
            debug_assert!(set.is_ok(), "the unit's tile is on the map: {set:?}");
            continue;
        }
        queue.remove(0);
        let set = g.set_builds(t, queue);
        debug_assert!(set.is_ok(), "the unit's tile is on the map: {set:?}");
        let imp = step.improvement;
        let kind = g.rules().improvements()[imp].kind;
        let checked = matches!(kind, ImprovementKind::Normal | ImprovementKind::Cancel);
        if checked && !no_problems(g, &b, t, imp) {
            let text = format!("{} could not be completed.", imp_name(g, imp));
            g.emit(
                EngineEvent::BuildFailed,
                &text,
                Some(PlayerSet::single(p)),
                Some(t),
                EventData::default(),
                &[],
            );
            continue;
        }
        set_improvement(g, t, imp, Some(p), Some(u));
        let who = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
        let text = format!("{who} finished {}.", imp_name(g, imp));
        let data = EventData { improvement: Some(imp), ..EventData::default() };
        g.emit(
            EngineEvent::ImprovementBuilt,
            &text,
            Some(PlayerSet::single(p)),
            Some(t),
            data,
            &[],
        );
        if g.state().tiles().builds(t).is_empty()
            && g.unit(u).is_some_and(|x| x.activity == Some(crate::state::units::Activity::Build))
            && let Some(x) = g.unit_mut(u, UnitTouch::CORE)
        {
            x.activity = None;
        }
    }
}

// ---- Pillaging (workers.py:520-640) ---------------------------------------------------------------

/// What pillaging tile `t` would destroy (`workers.improvement_to_pillage`, `workers.py:520-528`):
/// its improvement, unless it is pillaged, cannot be pillaged or removed, or a city stands there;
/// else its route, unless pillaged.
#[must_use]
pub fn improvement_to_pillage(g: &Game, t: TileIdx) -> Option<ImprovementId> {
    let tile = g.tile(t)?;
    if let Some(i) = tile.improvement()
        && !tile.improvement_pillaged()
        && !carries(g, i, UniqueType::Unpillagable)
        && !carries(g, i, UniqueType::Irremovable)
        && g.city_at(t).is_none()
    {
        return Some(i);
    }
    let known = &g.rules().derived().known;
    match tile.route() {
        Some(route) if !tile.route_pillaged() => Some(match route {
            Route::Road => known.road,
            Route::Railroad => known.railroad,
        }),
        _ => None,
    }
}

/// Why unit `u` cannot pillage where it stands, or `None` (`workers.can_pillage`,
/// `workers.py:531-551`).
#[must_use]
pub fn can_pillage(g: &Game, u: UnitId) -> Option<String> {
    let x = g.unit(u)?;
    let r = g.rules();
    if !r.base_units()[x.base].military {
        return Some("Civilian units cannot pillage.".into());
    }
    if x.carried_by().is_some() {
        return Some("Transported units cannot pillage.".into());
    }
    let t = x.tile();
    if improvement_to_pillage(g, t).is_none() {
        return Some("There is nothing here to pillage.".into());
    }
    let owner = g.tile(t).and_then(Tile::owner);
    if owner == Some(x.owner()) {
        return Some("You cannot pillage your own tiles.".into());
    }
    if unit_has(g, u, UniqueType::CannotPillage, true) {
        return Some("This unit cannot pillage.".into());
    }
    if owner.is_some_and(|o| !g.at_war(x.owner(), o)) {
        return Some("You can only pillage tiles of civilizations you are at war with.".into());
    }
    if x.moves <= 0 {
        return Some("The unit has no movement left.".into());
    }
    None
}

/// Unit `u` pillages the tile it stands on, which [`can_pillage`] allowed (`workers.pillage`,
/// `workers.py:554-583`): the owner is told, the loot taken, the tile pillaged; the unit spends a
/// movement point unless it pillages for free, and heals from an improvement (25, raised by its
/// `[n]% Health from pillaging tiles`), which it destroys where the improvement says so.
pub fn pillage(g: &mut Game, u: UnitId) -> Value {
    let Some((t, base)) = g.unit(u).map(|x| (x.tile(), x.base)) else {
        return Value::Null;
    };
    let Some(imp) = improvement_to_pillage(g, t) else { return Value::Null };
    let Some(tile) = g.tile(t).copied() else { return Value::Null };
    let is_imp = tile.improvement() == Some(imp) && !tile.improvement_pillaged();
    let r = g.rules();
    let name = imp_name(g, imp).to_owned();
    if let Some(o) = tile.owner() {
        let text = format!("An enemy {} has pillaged our {name}.", r.base_units()[base].name);
        g.emit(
            EngineEvent::Pillaged,
            &text,
            Some(PlayerSet::single(o)),
            Some(t),
            EventData::default(),
            &[],
        );
    }
    let loot = loot(g, u, t, imp);
    set_pillaged(g, t);
    if !unit_has(g, u, UniqueType::NoMovementToPillage, true) {
        let sc = r.constants().move_scale;
        if let Some(x) = g.unit_mut(u, UnitTouch::MOVES) {
            x.moves = x.moves.saturating_sub(sc).max(0);
        }
    }
    let mut healed = 0;
    if is_imp {
        let mut amount = 25.0;
        {
            let v = g.view();
            let ctx = Ctx::unit(&v, u);
            for h in uq::unit_and_civ(&v, u, UniqueType::PercentHealthFromPillaging, &ctx) {
                if let UniqueData::PercentHealthFromPillaging(x) = *h.data() {
                    for _ in 0..h.n {
                        amount *= 1.0 + f64::from(x.percent) / 100.0;
                    }
                }
            }
        }
        let before = g.unit(u).map_or(0, |x| i32::from(x.hp));
        super::units::health::heal_by(g, u, num::trunc_i32(amount));
        healed = g.unit(u).map_or(0, |x| i32::from(x.hp)) - before;
        if let Some(now) = g.tile(t).and_then(Tile::improvement)
            && carries(g, now, UniqueType::DestroyedWhenPillaged)
        {
            let refused = |e: crate::state::StateError| {
                debug_assert!(false, "a tile on the map loses its improvement: {e}");
            };
            g.set_improvement(t, None).unwrap_or_else(refused);
            let route = g.tile(t).is_some_and(Tile::route_pillaged);
            g.set_pillaged(t, route, false).unwrap_or_else(refused);
        }
    }
    json!({ "pillaged": name, "loot": loot, "healed": healed })
}

/// Pillages tile `t` (`Tile.setPillaged`, `workers._set_pillaged`, `workers.py:586-602`): a sea
/// improvement is destroyed; on land a build under way stops, and the improvement is pillaged if
/// it can be, else the route.
fn set_pillaged(g: &mut Game, t: TileIdx) {
    let Some(tile) = g.tile(t).copied() else { return };
    let refused = |e: crate::state::StateError| {
        debug_assert!(false, "a tile on the map is pillaged: {e}");
    };
    let can_imp = tile.improvement().is_some_and(|i| {
        !tile.improvement_pillaged()
            && !carries(g, i, UniqueType::Unpillagable)
            && !carries(g, i, UniqueType::Irremovable)
    });
    if g.is_water(t) {
        g.set_improvement(t, None).unwrap_or_else(refused);
        g.set_pillaged(t, tile.route_pillaged(), false).unwrap_or_else(refused);
        return;
    }
    if front(g, t).is_some_and(|s| s.turns_left >= 0) {
        g.set_builds(t, BuildQueue::new()).unwrap_or_else(refused);
    }
    if can_imp {
        g.set_pillaged(t, tile.route_pillaged(), true).unwrap_or_else(refused);
    } else {
        g.set_pillaged(t, true, tile.improvement_pillaged()).unwrap_or_else(refused);
    }
}

/// What pillaging improvement `imp` on tile `t` yields unit `u`'s owner (`workers._loot`,
/// `workers.py:605-640`): its random yields (two draws of up to the amount each, from
/// `Purpose::Pillage` keyed by the turn and the tile) and fixed ones, scaled by the speed where
/// they say so and by the unit's `[n]% Yield from pillaging tiles`; the civilization's stats go to
/// it, the rest to its nearest city; told to it.
fn loot(g: &mut Game, u: UnitId, t: TileIdx, imp: ImprovementId) -> Value {
    let r = g.rules();
    let tu = r.uniques();
    let Some(owner) = g.unit(u).map(crate::state::units::Unit::owner) else { return json!({}) };
    let mut rng = Rng::keyed(g.state().seed(), Purpose::Pillage, &[g.turn().key(), t.0.key()]);
    let mut total = Stats::ZERO;
    let mut named = StatMask::default();
    let speed = |id: UniqueId| {
        if tu.get(id).flags().contains(UFlags::SPEED) { g.speed().modifier } else { 1.0 }
    };
    let def = &r.improvements()[imp];
    for id in def.uniques.ids() {
        if let UniqueData::PillageYieldRandom(x) = tu.get(id).data {
            let stats = tu.stats(x.stats);
            for s in Stat::ALL {
                if stats[s] == 0.0 {
                    continue;
                }
                let most = u64::try_from(num::trunc_i64(stats[s]).max(0)).unwrap_or(0) + 1;
                #[allow(clippy::cast_precision_loss, reason = "two draws below a small amount")]
                let amount = (rng.below(most) + rng.below(most)) as f64;
                total[s] += amount * speed(id);
                named.insert(s);
            }
        }
    }
    for id in def.uniques.ids() {
        if let UniqueData::PillageYieldFixed(x) = tu.get(id).data {
            let stats = tu.stats(x.stats);
            for s in Stat::ALL {
                if stats[s] != 0.0 {
                    total[s] += stats[s] * speed(id);
                    named.insert(s);
                }
            }
        }
    }
    {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        for h in uq::unit_and_civ(&v, u, UniqueType::PercentYieldFromPillaging, &ctx) {
            if let UniqueData::PercentYieldFromPillaging(x) = *h.data() {
                for _ in 0..h.n {
                    for s in named.iter() {
                        total[s] *= 1.0 + f64::from(x.percent) / 100.0;
                    }
                }
            }
        }
    }
    if named.is_empty() {
        return json!({});
    }
    let city = nearest_city(g, owner, t).map(|(c, _)| c);
    let mut out = Map::new();
    let mut parts: Vec<String> = Vec::new();
    for s in named.iter() {
        let n = num::trunc_i32(total[s]);
        if s.is_civ_wide() {
            g.add_stat(owner, s, f64::from(n));
        } else if let Some(c) = city {
            add_city_stat(g, c, s, f64::from(n));
        } else {
            continue;
        }
        out.insert(s.key().into(), json!(n));
        parts.push(format!("{n} {}", s.key()));
    }
    if !parts.is_empty() {
        let text = format!("We have looted {} from a {}.", parts.join(", "), def.name);
        g.emit(
            EngineEvent::Loot,
            &text,
            Some(PlayerSet::single(owner)),
            Some(t),
            EventData::default(),
            &[],
        );
    }
    Value::Object(out)
}

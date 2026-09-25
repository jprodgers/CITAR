//! What a city can build and building it (`cities.py:1091-1360, 1720-1834, 2380-2391`): why an
//! item cannot be built ([`rejection_reasons`], UnCiv's `getRejectionReasons`), the list a city
//! can choose from ([`buildable_items`], which the `Buildable` memo keeps), the production that
//! goes into what it builds each turn, and finishing it: a building added, a unit placed.
//!
//! The production cost of an item (`production_cost`, `cities.py:1091-1128`) is package 1b-06's,
//! in `cities::stats`, which the city's stats read too.
//!
//! What differs from Python:
//! - a new unit is placed on its city's tile or near it by the rules of standing there that a
//!   new unit meets: land on land and water on water or in a coastal city, no foreign city or
//!   territory it may not enter, and no unit of its kind already there. Package 1c-02 ports the
//!   rest of movement (embarking, the ocean), and a unit made on its owner's turn gets its moves
//!   from it too;
//! - `Must be next to [Fresh water]` and `[River]` are read by the tile filter alone, which
//!   already answers both for the city's own tile, as Python's extra tests did.

use core::ops::ControlFlow;

use smallvec::SmallVec;

use super::super::Game;
use super::super::derive::rev::{CityTouch, PlayerTouch, UnitTouch, WorldTouch};
use super::super::{Porting, pending};
use super::founding::{add_building, equivalent_building};
use super::stats::{self as cstats, current_construction, production_cost};
use super::uniques::contains_building;
use crate::base::ids::{BaseUnitId, BuildingId, CityId, PlayerId, TileIdx, UniqueId, UnitId};
use crate::base::num;
use crate::base::sets::{BaseUnitSet, BuildingSet, PlayerSet};
use crate::base::stats::Stat;
use crate::game::core::has_type;
use crate::game::economy;
use crate::rules::Ruleset;
use crate::rules::defs::Domain;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::{Constructible, Perpetual};
use crate::unique::cond::ProblemKind;
use crate::unique::filter::{Expr, UnitLeaf};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// The most items a city's production queue holds (`cities.py:23`).
pub const QUEUE_MAX: usize = 10;

/// An item's name, as Python queued it.
#[must_use]
pub fn item_name(r: &Ruleset, item: Constructible) -> &str {
    match item {
        Constructible::Building(b) => &r.buildings()[b].name,
        Constructible::Unit(u) => &r.base_units()[u].name,
        Constructible::Perpetual(p) => p.name(),
    }
}

/// The unit a civilization trains in place of `u`: its nation's unique unit that replaces it, if
/// it has one (`cities.equivalent_unit`, `cities.py:1132-1135`).
#[must_use]
pub fn equivalent_unit(g: &Game, p: PlayerId, u: BaseUnitId) -> BaseUnitId {
    let Some(nation) = g.player(p).map(|x| x.nation) else { return u };
    let r = g.rules();
    // Most units no unique of the nation replaces: its short list says so without a scan of
    // the ruleset, which the scan's first match (Python's) then settles.
    if !r.derived().nation_uniques[nation].units.iter().any(|&(k, x)| k == u && x != u) {
        return u;
    }
    r.base_units()
        .iter()
        .find(|(_, x)| x.replaces == Some(u) && x.unique_to == Some(nation))
        .map_or(u, |(id, _)| id)
}

/// Whether a unit, or its unit type, carries a unique of type `ty`, conditionals not evaluated
/// (`has_tag` on the unit's map, which held its type's uniques too, `rules.py:115-117`).
#[must_use]
pub fn unit_has_type(r: &Ruleset, u: BaseUnitId, ty: UniqueType) -> bool {
    let d = &r.base_units()[u];
    has_type(r, &d.uniques, ty) || has_type(r, &r.unit_types()[d.unit_type].uniques, ty)
}

/// A unit's uniques of type `ty` that hold in `ctx`, its own then its type's (`matching` on the
/// unit's map); with [`Ctx::IGNORE`], all of them (`get`).
fn unit_hits<'w>(
    v: &'w crate::game::EvalView<'w>,
    r: &Ruleset,
    u: BaseUnitId,
    ty: UniqueType,
    ctx: &Ctx,
) -> impl Iterator<Item = uq::Hit<'w>> + 'w {
    let d = &r.base_units()[u];
    uq::object(v, &d.uniques, ty, ctx).chain(uq::object(
        v,
        &r.unit_types()[d.unit_type].uniques,
        ty,
        ctx,
    ))
}

/// How many of an item a civilization has (`cities.count_constructed`, `cities.py:1153-1166`):
/// the spaceship parts it has added, and a building in its cities (or its equivalent) or a unit
/// on the map, and each in a production queue of a city other than `exclude`.
#[must_use]
pub fn count_constructed(
    g: &Game,
    p: PlayerId,
    item: Constructible,
    exclude: Option<CityId>,
) -> i32 {
    count_made(g, p, item).saturating_add(count_queued(g, p, item, exclude))
}

/// What [`count_constructed`] counts but the queues: the spaceship parts added, the cities with
/// the building (or its equivalent), the units of the kind.
fn count_made(g: &Game, p: PlayerId, item: Constructible) -> i32 {
    let in_space = match item {
        Constructible::Unit(u) => g
            .player(p)
            .and_then(|x| x.major.as_deref())
            .and_then(|m| m.spaceship.get(&u))
            .map_or(0, |&n| i32::from(n)),
        _ => 0,
    };
    let n = match item {
        Constructible::Building(b) => {
            g.player_cities(p).filter(|c| contains_building(g, c.id(), b)).count()
        }
        Constructible::Unit(u) => g.player_units(p).filter(|x| x.base == u).count(),
        Constructible::Perpetual(_) => 0,
    };
    in_space.saturating_add(i32::try_from(n).unwrap_or(i32::MAX))
}

/// The cities of a civilization, other than `exclude`, with an item in their queue: a city that
/// has the building already counted by [`count_made`] instead.
fn count_queued(g: &Game, p: PlayerId, item: Constructible, exclude: Option<CityId>) -> i32 {
    let n = g
        .player_cities(p)
        .filter(|c| Some(c.id()) != exclude && c.queue.contains(&item))
        .filter(|c| match item {
            Constructible::Building(b) => !contains_building(g, c.id(), b),
            _ => true,
        })
        .count();
    i32::try_from(n).unwrap_or(i32::MAX)
}

// ---- Why an item cannot be built (cities.py:1169-1344) --------------------------------------------

/// Why an item cannot be built: Python's names for the kinds of reasons (UnCiv's
/// `RejectionReasonType`), which purchases and progress read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RejectionKind {
    AlreadyBuilt,
    Unbuildable,
    CanOnlyBeBuiltInSpecificCities,
    ShouldNotBeDisplayed,
    RequiresBuildingInAllCities,
    RequiresBuildingInSomeCities,
    PopulationRequirement,
    MustBeOnTile,
    MustNotBeOnTile,
    MustBeNextToTile,
    MustOwnTile,
    Obsoleted,
    MaxNumberBuildable,
    RequiresBuildingInSomeCity,
    UniqueToOtherNation,
    ReplacedByOurUnique,
    RequiresTech,
    WonderBeingBuiltElsewhere,
    CityStateWonder,
    PuppetWonder,
    WonderAlreadyBuilt,
    NationalWonderAlreadyBuilt,
    RequiresBuildingInThisCity,
    ConsumesResources,
    RequiresNearbyResource,
    CannotBeBuilt,
    WaterUnitsInCoastalCities,
    DisabledBySetting,
    NoSettlerForOneCityPlayers,
    NoPlaceToPutUnit,
}

impl RejectionKind {
    /// Python's name for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::AlreadyBuilt => "AlreadyBuilt",
            Self::Unbuildable => "Unbuildable",
            Self::CanOnlyBeBuiltInSpecificCities => "CanOnlyBeBuiltInSpecificCities",
            Self::ShouldNotBeDisplayed => "ShouldNotBeDisplayed",
            Self::RequiresBuildingInAllCities => "RequiresBuildingInAllCities",
            Self::RequiresBuildingInSomeCities => "RequiresBuildingInSomeCities",
            Self::PopulationRequirement => "PopulationRequirement",
            Self::MustBeOnTile => "MustBeOnTile",
            Self::MustNotBeOnTile => "MustNotBeOnTile",
            Self::MustBeNextToTile => "MustBeNextToTile",
            Self::MustOwnTile => "MustOwnTile",
            Self::Obsoleted => "Obsoleted",
            Self::MaxNumberBuildable => "MaxNumberBuildable",
            Self::RequiresBuildingInSomeCity => "RequiresBuildingInSomeCity",
            Self::UniqueToOtherNation => "UniqueToOtherNation",
            Self::ReplacedByOurUnique => "ReplacedByOurUnique",
            Self::RequiresTech => "RequiresTech",
            Self::WonderBeingBuiltElsewhere => "WonderBeingBuiltElsewhere",
            Self::CityStateWonder => "CityStateWonder",
            Self::PuppetWonder => "PuppetWonder",
            Self::WonderAlreadyBuilt => "WonderAlreadyBuilt",
            Self::NationalWonderAlreadyBuilt => "NationalWonderAlreadyBuilt",
            Self::RequiresBuildingInThisCity => "RequiresBuildingInThisCity",
            Self::ConsumesResources => "ConsumesResources",
            Self::RequiresNearbyResource => "RequiresNearbyResource",
            Self::CannotBeBuilt => "CannotBeBuilt",
            Self::WaterUnitsInCoastalCities => "WaterUnitsInCoastalCities",
            Self::DisabledBySetting => "DisabledBySetting",
            Self::NoSettlerForOneCityPlayers => "NoSettlerForOneCityPlayers",
            Self::NoPlaceToPutUnit => "NoPlaceToPutUnit",
        }
    }

    const fn of_problem(k: ProblemKind) -> Self {
        match k {
            ProblemKind::RequiresBuildingInAllCities => Self::RequiresBuildingInAllCities,
            ProblemKind::RequiresBuildingInSomeCities => Self::RequiresBuildingInSomeCities,
            ProblemKind::CanOnlyBeBuiltInSpecificCities => Self::CanOnlyBeBuiltInSpecificCities,
            ProblemKind::ShouldNotBeDisplayed => Self::ShouldNotBeDisplayed,
        }
    }
}

/// One reason an item cannot be built, as a player reads it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rejection {
    pub kind: RejectionKind,
    pub text: String,
}

/// What a caller wants of the reasons.
enum Collect<'a> {
    /// Whether there is one: the first ends the search.
    Any,
    /// Their kinds, in order, with no text written.
    Kinds(&'a mut SmallVec<[RejectionKind; 4]>),
    /// Every reason, with its text.
    All(&'a mut Vec<Rejection>),
}

/// Where reasons go, and how much of the game they read.
struct Reasons<'a> {
    collect: Collect<'a>,
    found: bool,
    /// The reading of the `Buildable` memo: what the owner's other cities are building and the
    /// room in the city's hangar move all turn, so [`buildable_items`] reads them as it lends the
    /// list, and the memo does not. A limit on the item is then counted without the queues, and
    /// [`room`](Self::room) is how many more of it the limits allow.
    memo: bool,
    room: Option<i32>,
}

impl<'a> Reasons<'a> {
    const fn new(collect: Collect<'a>) -> Self {
        Self { collect, found: false, memo: false, room: None }
    }

    /// Adds a reason; `Break` when the caller only asked whether there is one.
    fn add(&mut self, kind: RejectionKind, text: impl FnOnce() -> String) -> ControlFlow<()> {
        self.found = true;
        match &mut self.collect {
            Collect::Any => ControlFlow::Break(()),
            Collect::Kinds(v) => {
                v.push(kind);
                ControlFlow::Continue(())
            }
            Collect::All(v) => {
                v.push(Rejection { kind, text: text() });
                ControlFlow::Continue(())
            }
        }
    }

    /// Whether `Limited to [limit] per Civilization` rules the item out (`cities.py:1244-1246,
    /// 1292-1294`): what the civilization has of it and what its other cities are building,
    /// against the limit. The memo counts what it has alone, and keeps the room left.
    fn over_limit(&mut self, g: &Game, c: CityId, item: Constructible, limit: i32) -> bool {
        let Some(p) = g.city(c).map(crate::state::cities::City::owner) else { return false };
        if !self.memo {
            return count_constructed(g, p, item, Some(c)) >= limit;
        }
        let made = count_made(g, p, item);
        if made >= limit {
            return true;
        }
        let room = limit - made;
        self.room = Some(self.room.map_or(room, |r| r.min(room)));
        false
    }
}

/// Every reason city `c` cannot build `item` now, in Python's order; empty if it can
/// (`cities.rejection_reasons`, `cities.py:1196-1338`).
#[must_use]
pub fn rejection_reasons(g: &Game, c: CityId, item: Constructible) -> Vec<Rejection> {
    let mut all = Vec::new();
    let _flow = reasons(g, c, item, &mut Reasons::new(Collect::All(&mut all)));
    all
}

/// The kinds of [`rejection_reasons`], in the same order, without writing their text: for the
/// rules that read what kind of reason there is and never show it.
#[must_use]
pub fn rejection_kinds(g: &Game, c: CityId, item: Constructible) -> SmallVec<[RejectionKind; 4]> {
    let mut kinds = SmallVec::new();
    let _flow = reasons(g, c, item, &mut Reasons::new(Collect::Kinds(&mut kinds)));
    kinds
}

/// Whether city `c` can build `item` now, and if not the first reason why
/// (`cities.can_build`, `cities.py:1341-1344`).
#[must_use]
pub fn can_build(g: &Game, c: CityId, item: Constructible) -> Option<String> {
    rejection_reasons(g, c, item).into_iter().next().map(|r| r.text)
}

/// Whether city `c` can build `item` now: [`rejection_reasons`] is empty, found without writing
/// a single reason. The quick checks come first, since most items fail on a tech.
#[must_use]
pub fn is_buildable(g: &Game, c: CityId, item: Constructible) -> bool {
    if quick_no(g, c, item) {
        return false;
    }
    let mut out = Reasons::new(Collect::Any);
    let _flow = reasons(g, c, item, &mut out);
    !out.found
}

/// Whether city `c` could build `item` but for what its owner's other cities are building and the
/// room in its hangar: `Some` with the room its limits leave (`None` for no limit) if so.
fn buildable_for_memo(g: &Game, c: CityId, item: Constructible) -> Option<Option<i32>> {
    if quick_no(g, c, item) {
        return None;
    }
    let mut out = Reasons::new(Collect::Any);
    out.memo = true;
    let _flow = reasons(g, c, item, &mut out);
    (!out.found).then_some(out.room)
}

/// Whether one of the cheap reasons rules `item` out in city `c`. Any one reason answers whether
/// there is one, so these come first whatever their place in Python's order; `reasons` looks at
/// the rest.
fn quick_no(g: &Game, c: CityId, item: Constructible) -> bool {
    let Some(city) = g.city(c) else { return true };
    let p = city.owner();
    let r = g.rules();
    match item {
        Constructible::Building(b) => {
            let d = &r.buildings()[b];
            city.buildings.contains(b)
                || !g.has_tech(p, d.required_tech)
                || d.unique_to.is_some_and(|n| g.player(p).is_none_or(|x| x.nation != n))
                || (d.is_wonder && g.state().world().wonders_built.contains_key(&b))
                // Before its requirements, whose `in all [] cities` reads every city's status.
                || (d.is_national_wonder && g.player_cities(p).any(|x| x.buildings.contains(b)))
        }
        Constructible::Unit(u) => {
            let d = &r.base_units()[u];
            !g.has_tech(p, d.required_tech)
                || d.obsolete_tech.is_some_and(|t| g.has_tech(p, Some(t)))
                || d.unique_to.is_some_and(|n| g.player(p).is_none_or(|x| x.nation != n))
                || (unit_has_type(r, u, UniqueType::Unbuildable) && {
                    let v = g.view();
                    uq::any(unit_hits(&v, r, u, UniqueType::Unbuildable, &Ctx::city(&v, c)))
                })
        }
        Constructible::Perpetual(_) => false,
    }
}

/// Adds a reason, and stops when the caller only asked whether there is one.
macro_rules! reject {
    ($out:expr, $kind:expr, $text:expr) => {
        $out.add($kind, || $text)?
    };
}

fn reasons(g: &Game, c: CityId, item: Constructible, out: &mut Reasons<'_>) -> ControlFlow<()> {
    use RejectionKind as K;
    let Some(city) = g.city(c) else { return ControlFlow::Continue(()) };
    let p = city.owner();
    let r = g.rules();
    let t = r.uniques();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let Some(pl) = g.player(p) else { return ControlFlow::Continue(()) };
    match item {
        Constructible::Perpetual(Perpetual::Nothing) => {}
        Constructible::Perpetual(k) => {
            let stat = if k == Perpetual::Gold { Stat::Gold } else { Stat::Science };
            let enabled = uq::civ(&v, p, UniqueType::EnablesStatProduction, &Ctx::civ(p)).any(
                |h| matches!(h.data(), UniqueData::EnablesStatProduction(x) if x.stat == stat),
            );
            if !enabled {
                reject!(
                    out,
                    K::RequiresTech,
                    format!("Converting production to {} needs the right technology.", k.name())
                );
            }
        }
        Constructible::Building(b) => {
            let bd = &r.buildings()[b];
            let name = &bd.name;
            if city.buildings.contains(b) {
                reject!(out, K::AlreadyBuilt, format!("{} already has {name}.", city.name));
            }
            for id in bd.uniques.ids() {
                // Python asked every unique whether it applied, and most say nothing about
                // building; asking only those that may reject keeps what the memo records to
                // what the answer reads.
                let Some(ty) = t.meta(id).ty.filter(|&ty| may_reject(ty)) else { continue };
                let requirement =
                    matches!(ty, UniqueType::OnlyAvailable | UniqueType::CanOnlyBeBuiltWhen);
                if !requirement && !crate::unique::applies(id, &ctx, &v) {
                    continue;
                }
                building_unique(g, c, p, b, id, out)?;
            }
            if let Some(n) = bd.unique_to
                && n != pl.nation
            {
                reject!(
                    out,
                    K::UniqueToOtherNation,
                    format!("{name} is unique to {}.", r.nations()[n].name)
                );
            }
            let eq = equivalent_building(g, p, b);
            if eq != b {
                reject!(
                    out,
                    K::ReplacedByOurUnique,
                    format!("Your civilization builds {} instead.", r.buildings()[eq].name)
                );
            }
            if let Some(tech) = bd.required_tech
                && !g.has_tech(p, Some(tech))
            {
                reject!(out, K::RequiresTech, format!("{name} requires {}.", r.techs()[tech].name));
            }
            if bd.any_wonder {
                let elsewhere = !out.memo
                    && g.player_cities(p)
                        .any(|x| x.id() != c && x.queue.contains(&Constructible::Building(b)));
                if elsewhere {
                    reject!(
                        out,
                        K::WonderBeingBuiltElsewhere,
                        format!("{name} is being built in another of your cities.")
                    );
                }
                if pl.is_city_state() {
                    reject!(out, K::CityStateWonder, "City-states cannot build wonders.".into());
                }
                if city.puppet {
                    reject!(out, K::PuppetWonder, "Puppets cannot build wonders.".into());
                }
            }
            if bd.is_wonder && g.state().world().wonders_built.contains_key(&b) {
                reject!(out, K::WonderAlreadyBuilt, format!("{name} has already been built."));
            }
            if bd.is_national_wonder && g.player_cities(p).any(|x| x.buildings.contains(b)) {
                reject!(out, K::NationalWonderAlreadyBuilt, format!("You already have {name}."));
            }
            if let Some(req) = bd.required_building
                && !contains_building(g, c, req)
            {
                reject!(
                    out,
                    K::RequiresBuildingInThisCity,
                    format!(
                        "{name} requires a {} in this city.",
                        r.buildings()[equivalent_building(g, p, req)].name
                    )
                );
            }
            if let Some(res) = bd.required_resource {
                let have = economy::resource_amount(g, p, res);
                if have < 1 {
                    reject!(
                        out,
                        K::ConsumesResources,
                        format!("{name} needs 1 {} (you have {have}).", r.resources()[res].name)
                    );
                }
            }
            if !bd.required_nearby_improved_resources.is_empty() {
                let near = &bd.required_nearby_improved_resources;
                // The resource and the owner first: they rule out most tiles before the
                // improvement is looked up.
                let ok = economy::city_tiles(g, c).into_iter().any(|i| {
                    let Some(tile) = g.tile(i) else { return false };
                    let Some(res) = tile.resource().filter(|x| near.contains(x)) else {
                        return false;
                    };
                    tile.owner() == Some(p)
                        && crate::game::tiles::unpillaged_improvement(g, i).is_some_and(|imp| {
                            crate::game::tiles::resource_improved_by(g, res, imp)
                                || g.city_at(i).is_some()
                        })
                });
                if !ok {
                    let names: Vec<&str> = near.iter().map(|&x| &*r.resources()[x].name).collect();
                    reject!(
                        out,
                        K::RequiresNearbyResource,
                        format!("{name} needs an improved {} nearby.", names.join(" or "))
                    );
                }
            }
            for h in uq::civ(&v, p, UniqueType::CannotBuildBuildings, &ctx) {
                if let UniqueData::CannotBuildBuildings(x) = h.data()
                    && t.in_set(x.buildings, b)
                {
                    reject!(out, K::CannotBeBuilt, format!("{name} cannot be built."));
                }
            }
        }
        Constructible::Unit(u) => {
            let ud = &r.base_units()[u];
            let name = &ud.name;
            let tile = city.tile();
            if ud.domain == Domain::Water
                && !(g.is_water(tile) || crate::game::tiles::adjacent_to_coast(g, tile))
            {
                reject!(
                    out,
                    K::WaterUnitsInCoastalCities,
                    format!("{name} can only be built in coastal cities.")
                );
            }
            for ty in [UniqueType::OnlyAvailable, UniqueType::CanOnlyBeBuiltWhen] {
                let ids: SmallVec<[UniqueId; 4]> =
                    unit_hits(&v, r, u, ty, &Ctx::IGNORE).map(|h| h.id).collect();
                if out.memo {
                    if ids.iter().any(|&id| requirement_fails_for_memo(g, p, id, &ctx)) {
                        reject!(out, K::ShouldNotBeDisplayed, String::new());
                    }
                    continue;
                }
                for pr in uq::requirement_problems(&v, ids, &ctx, p) {
                    reject!(out, RejectionKind::of_problem(pr.kind), pr.text);
                }
            }
            for _ in unit_hits(&v, r, u, UniqueType::Unavailable, &ctx) {
                reject!(out, K::ShouldNotBeDisplayed, format!("{name} is unavailable."));
            }
            for h in unit_hits(&v, r, u, UniqueType::RequiresPopulation, &Ctx::IGNORE) {
                if let UniqueData::RequiresPopulation(x) = h.data()
                    && x.population > i32::from(city.pop)
                {
                    reject!(
                        out,
                        K::PopulationRequirement,
                        format!("{name} requires the city to have {} population.", x.population)
                    );
                }
            }
            if let Some(tech) = ud.required_tech
                && !g.has_tech(p, Some(tech))
            {
                reject!(out, K::RequiresTech, format!("{name} requires {}.", r.techs()[tech].name));
            }
            if let Some(tech) = ud.obsolete_tech
                && g.has_tech(p, Some(tech))
            {
                reject!(
                    out,
                    K::Obsoleted,
                    format!("{name} is obsolete ({}).", r.techs()[tech].name)
                );
            }
            if let Some(n) = ud.unique_to
                && n != pl.nation
            {
                reject!(
                    out,
                    K::UniqueToOtherNation,
                    format!("{name} is unique to {}.", r.nations()[n].name)
                );
            }
            let eq = equivalent_unit(g, p, u);
            if eq != u {
                reject!(
                    out,
                    K::ReplacedByOurUnique,
                    format!("Your civilization trains {} instead.", r.base_units()[eq].name)
                );
            }
            if !g.nukes_enabled() && unit_has_type(r, u, UniqueType::NuclearWeapon) {
                reject!(out, K::DisabledBySetting, "Nuclear weapons are disabled.".into());
            }
            if uq::any(unit_hits(&v, r, u, UniqueType::Unbuildable, &ctx)) {
                reject!(out, K::Unbuildable, format!("{name} cannot be built."));
            }
            if pl.is_city_state() && unit_has_type(r, u, UniqueType::FoundCity) {
                reject!(
                    out,
                    K::NoSettlerForOneCityPlayers,
                    "City-states cannot build settlers.".into()
                );
            }
            for h in unit_hits(&v, r, u, UniqueType::MaxNumberBuildable, &ctx) {
                if let UniqueData::MaxNumberBuildable(x) = h.data()
                    && out.over_limit(g, c, item, x.limit)
                {
                    reject!(
                        out,
                        K::MaxNumberBuildable,
                        format!("{name} is limited to {}.", x.limit)
                    );
                }
            }
            if !pl.is_barbarian() {
                if let Some(res) = ud.required_resource {
                    let have = economy::resource_amount(g, p, res);
                    if have < 1 {
                        reject!(
                            out,
                            K::ConsumesResources,
                            format!(
                                "{name} needs 1 {} (you have {have} available).",
                                r.resources()[res].name
                            )
                        );
                    }
                }
                for h in unit_hits(&v, r, u, UniqueType::ConsumesResources, &ctx) {
                    if let UniqueData::ConsumesResources(x) = h.data()
                        && economy::resource_amount(g, p, x.resource) < x.amount
                    {
                        reject!(
                            out,
                            K::ConsumesResources,
                            format!(
                                "{name} needs {} {}.",
                                x.amount,
                                r.resources()[x.resource].name
                            )
                        );
                    }
                }
            }
            for h in uq::civ(&v, p, UniqueType::CannotBuildUnits, &ctx) {
                if let UniqueData::CannotBuildUnits(x) = h.data()
                    && t.in_set(x.units, u)
                {
                    reject!(
                        out,
                        K::CannotBeBuilt,
                        format!("{name} cannot be built right now ({}).", t.text_of(h.id))
                    );
                }
            }
            if ud.domain == Domain::Air && !out.memo && !air_capacity_ok(g, c) {
                reject!(out, K::NoPlaceToPutUnit, "No room for more aircraft in this city.".into());
            }
        }
    }
    ControlFlow::Continue(())
}

/// Whether requirement `id` (an `Only available` or a `Can only be built`) is not met in a city
/// of `p`, as the `Buildable` memo reads it (the memo asks only whether there is a reason, so the
/// kind and text do not matter): its civilization-wide conditionals as the civilization's own memo
/// answers them, which moves the list only when an answer changes, and the rest asked here,
/// recording what they read.
fn requirement_fails_for_memo(g: &Game, p: PlayerId, id: UniqueId, ctx: &Ctx) -> bool {
    use super::super::derive::buildable::{civ_requirement_fails, is_local};
    if civ_requirement_fails(g, p, id) {
        return true;
    }
    let t = g.rules().uniques();
    let v = g.view();
    t.conds(t.get(id)).iter().filter(|x| is_local(x)).any(|x| {
        crate::unique::record::note_classes(x.deps);
        !crate::unique::cond::holds(x, id, ctx, &v)
    })
}

/// Whether a building's unique of type `ty` may be a reason it cannot be built: those
/// [`building_unique`] reads.
const fn may_reject(ty: UniqueType) -> bool {
    matches!(
        ty,
        UniqueType::Unbuildable
            | UniqueType::OnlyAvailable
            | UniqueType::CanOnlyBeBuiltWhen
            | UniqueType::Unavailable
            | UniqueType::RequiresPopulation
            | UniqueType::MustBeOn
            | UniqueType::MustNotBeOn
            | UniqueType::MustBeNextTo
            | UniqueType::MustHaveOwnedWithinTiles
            | UniqueType::ObsoleteWith
            | UniqueType::MaxNumberBuildable
            | UniqueType::SpaceshipPart
    )
}

/// One of a building's own uniques that holds (or a requirement, which is asked of its
/// conditionals), as a reason it cannot be built (`cities.py:1215-1253`).
fn building_unique(
    g: &Game,
    c: CityId,
    p: PlayerId,
    b: BuildingId,
    id: UniqueId,
    out: &mut Reasons<'_>,
) -> ControlFlow<()> {
    use RejectionKind as K;
    let r = g.rules();
    let t = r.uniques();
    let filters = t.filters();
    let v = g.view();
    let Some(city) = g.city(c) else { return ControlFlow::Continue(()) };
    let name = &r.buildings()[b].name;
    let at = city.tile();
    match t.meta(id).ty {
        Some(UniqueType::Unbuildable) => {
            reject!(out, K::Unbuildable, format!("{name} cannot be built directly."));
        }
        Some(UniqueType::OnlyAvailable | UniqueType::CanOnlyBeBuiltWhen) => {
            let ctx = Ctx::city(&v, c);
            if out.memo {
                if requirement_fails_for_memo(g, p, id, &ctx) {
                    reject!(out, K::ShouldNotBeDisplayed, String::new());
                }
            } else {
                for pr in uq::requirement_problems(&v, [id], &ctx, p) {
                    reject!(out, RejectionKind::of_problem(pr.kind), pr.text);
                }
            }
        }
        Some(UniqueType::Unavailable) => {
            reject!(out, K::ShouldNotBeDisplayed, format!("{name} is unavailable."));
        }
        _ => match t.get(id).data {
            UniqueData::RequiresPopulation(x) if x.population > i32::from(city.pop) => {
                reject!(
                    out,
                    K::PopulationRequirement,
                    format!("{name} requires {} population.", x.population)
                );
            }
            UniqueData::MustBeOn(x) if !filters.tile_terrain_matches(x.tiles, &v, at, Some(p)) => {
                reject!(
                    out,
                    K::MustBeOnTile,
                    format!("{name} must be built in a city on {}.", t.tile_filter(x.tiles))
                );
            }
            UniqueData::MustNotBeOn(x)
                if filters.tile_terrain_matches(x.tiles, &v, at, Some(p)) =>
            {
                reject!(
                    out,
                    K::MustNotBeOnTile,
                    format!("{name} cannot be built in a city on {}.", t.tile_filter(x.tiles))
                );
            }
            UniqueData::MustBeNextTo(x) => {
                let ok = filters.tile_matches(x.tiles, &v, at, Some(p))
                    || g.grid()
                        .neighbors(at)
                        .any(|n| filters.tile_matches(x.tiles, &v, n, Some(p)));
                if !ok {
                    reject!(
                        out,
                        K::MustBeNextToTile,
                        format!("{name} must be next to {}.", t.tile_filter(x.tiles))
                    );
                }
            }
            UniqueData::MustHaveOwnedWithinTiles(x) => {
                let radius = u32::try_from(x.radius).unwrap_or(0);
                let ok = g.grid().any_within(at, radius, |i| {
                    g.tile(i).and_then(crate::state::map::Tile::owner) == Some(p)
                        && filters.tile_matches(x.tiles, &v, i, Some(p))
                });
                if !ok {
                    reject!(
                        out,
                        K::MustOwnTile,
                        format!(
                            "{name} requires an owned {} within {} tiles.",
                            t.tile_filter(x.tiles),
                            x.radius
                        )
                    );
                }
            }
            UniqueData::ObsoleteWith(x) if g.has_tech(p, Some(x.tech)) => {
                reject!(out, K::Obsoleted, format!("{name} is obsolete."));
            }
            UniqueData::MaxNumberBuildable(x)
                if out.over_limit(g, c, Constructible::Building(b), x.limit) =>
            {
                reject!(out, K::MaxNumberBuildable, format!("{name} is limited to {}.", x.limit));
            }
            UniqueData::SpaceshipPart => {
                let enabled = uq::any(uq::civ(
                    &v,
                    p,
                    UniqueType::EnablesConstructionOfSpaceshipParts,
                    &Ctx::civ(p),
                ));
                if !enabled {
                    reject!(out, K::RequiresBuildingInSomeCity, "Apollo Program not built.".into());
                }
            }
            _ => {}
        },
    }
    ControlFlow::Continue(())
}

/// The base units the `Air` unit filter names: every aircraft (`rules::Derived::aircraft`). A
/// `Can carry [n] extra [Air] units` in a city adds to its hangar (`units.air_capacity_ok` read
/// its filter's text).
fn names_every_aircraft(g: &Game, f: crate::base::ids::UnitFilterId) -> bool {
    let r = g.rules();
    let air = &r.derived().aircraft;
    matches!(r.uniques().filters().unit(f), Expr::Leaf(UnitLeaf::Base(s)) if s == air)
}

/// Whether a city has room for another aircraft (`units.air_capacity_ok`, `units.py:754-760`):
/// the aircraft based there against its hangar.
#[must_use]
pub fn air_capacity_ok(g: &Game, c: CityId) -> bool {
    g.city(c).is_some() && aircraft_based(g, c) < air_room(g, c)
}

/// How many aircraft a city's hangar holds: the ruleset's, and `Can carry [n] extra [Air] units`
/// of the city. What it reads is what the `Buildable` memo validates, so the memo keeps it.
fn air_room(g: &Game, c: CityId) -> i32 {
    let v = g.view();
    let mut cap = g.rules().constants().formulas.city_air_unit_capacity;
    for h in uq::city(&v, c, UniqueType::CarryExtraAirUnits, &Ctx::city(&v, c)) {
        if let UniqueData::CarryExtraAirUnits(x) = h.data()
            && names_every_aircraft(g, x.units)
        {
            cap += x.count * i32::from(h.n);
        }
    }
    cap
}

/// The aircraft based in a city and not carried, which move all turn.
fn aircraft_based(g: &Game, c: CityId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let here = g.air_units_at(city.tile()).filter(|u| u.carried_by().is_none()).count();
    i32::try_from(here).unwrap_or(i32::MAX)
}

// ---- What a city can build (cities.py:1347-1360) -------------------------------------------------

/// What a city can build now, by kind (`cities.buildable_items`, `cities.py:1347-1360`): the
/// units, the buildings, the wonders (national ones too) and the conversions of production. The
/// memo `Buildable` keeps it per city (DESIGN.md 6.5), and [`buildable_items`] lends it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Buildable {
    pub units: BaseUnitSet,
    pub buildings: BuildingSet,
    pub wonders: BuildingSet,
    /// Gold, then Science, as they may be built.
    pub gold: bool,
    pub science: bool,
    /// In the memo, the room in the city's hangar when the list holds an aircraft: the uniques
    /// that make it are read once per computation rather than on every read.
    pub(crate) air_room: Option<i32>,
    /// In the memo, each item a limit applies to, with how many more of it the limit allows
    /// beside those the civilization has; its other cities' queues are counted on each read.
    pub(crate) limited: SmallVec<[(Constructible, i32); 2]>,
}

impl super::super::derive::rev::BitEq for Buildable {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

impl Buildable {
    /// Whether the list holds an item.
    #[must_use]
    pub fn contains(&self, item: Constructible) -> bool {
        match item {
            Constructible::Unit(u) => self.units.contains(u),
            Constructible::Building(b) => self.buildings.contains(b) || self.wonders.contains(b),
            Constructible::Perpetual(Perpetual::Gold) => self.gold,
            Constructible::Perpetual(Perpetual::Science) => self.science,
            Constructible::Perpetual(Perpetual::Nothing) => false,
        }
    }

    /// Takes an item off the list.
    fn remove(&mut self, item: Constructible) {
        match item {
            Constructible::Unit(u) => {
                self.units.remove(u);
            }
            Constructible::Building(b) => {
                self.buildings.remove(b);
                self.wonders.remove(b);
            }
            Constructible::Perpetual(_) => {}
        }
    }
}

/// City `c`'s [`Buildable`] memo, computed: what it could build but for what the other cities of
/// its owner are building and the room aircraft need. Those move all turn (a queue edited, a
/// plane landed), so [`buildable_items`] reads them as it lends the list, and a sibling's queue
/// recomputes no list.
pub(crate) fn compute_buildable(g: &Game, c: CityId) -> Buildable {
    let mut out = Buildable::default();
    let r = g.rules();
    let mut aircraft = false;
    for (u, d) in r.base_units().iter() {
        let item = Constructible::Unit(u);
        let Some(room) = buildable_for_memo(g, c, item) else { continue };
        out.units.insert(u);
        aircraft |= d.domain == Domain::Air;
        if let Some(room) = room {
            out.limited.push((item, room));
        }
    }
    if aircraft {
        out.air_room = Some(air_room(g, c));
    }
    for (b, d) in r.buildings().iter() {
        let item = Constructible::Building(b);
        let Some(room) = buildable_for_memo(g, c, item) else { continue };
        if d.any_wonder {
            out.wonders.insert(b);
        } else {
            out.buildings.insert(b);
        }
        if let Some(room) = room {
            out.limited.push((item, room));
        }
    }
    out.gold = buildable_for_memo(g, c, Constructible::Perpetual(Perpetual::Gold)).is_some();
    out.science = buildable_for_memo(g, c, Constructible::Perpetual(Perpetual::Science)).is_some();
    out
}

/// [`compute_buildable`], for the benchmark of a recomputation (DESIGN.md 10).
#[cfg(feature = "test-ops")]
#[doc(hidden)]
#[must_use]
pub fn compute_buildable_for_bench(g: &Game, c: CityId) -> Buildable {
    compute_buildable(g, c)
}

/// What a city can build now (`cities.buildable_items`): its memo, less the aircraft it has no
/// room for, the wonders another of its owner's cities is building, and the items whose limit
/// the other cities' queues reach.
#[must_use]
pub fn buildable_items(g: &Game, c: CityId) -> Buildable {
    let mut out = super::super::derive::buildable::buildable(g, c).clone();
    let r = g.rules();
    if let Some(room) = out.air_room
        && aircraft_based(g, c) >= room
    {
        let air: SmallVec<[BaseUnitId; 8]> =
            out.units.iter().filter(|&u| r.base_units()[u].domain == Domain::Air).collect();
        for u in air {
            out.units.remove(u);
        }
    }
    if out.wonders.is_empty() && out.limited.is_empty() {
        return out;
    }
    let Some(p) = g.city(c).map(crate::state::cities::City::owner) else { return out };
    // A wonder being built in another of its owner's cities (cities.py:1264-1266).
    for x in g.player_cities(p).filter(|x| x.id() != c) {
        for item in &x.queue {
            if let Constructible::Building(b) = *item {
                out.wonders.remove(b);
            }
        }
    }
    let limited = core::mem::take(&mut out.limited);
    for &(item, room) in &limited {
        if count_queued(g, p, item, Some(c)) >= room {
            out.remove(item);
        }
    }
    out.limited = limited;
    out
}

// ---- Production (cities.py:1621-1632, 1720-1834) --------------------------------------------------

/// The queue less what can no longer be built (`cities.validate_queue`, `cities.py:1621-1631`): a
/// wonder another finished, an obsolete unit, a building whose resource was traded away.
pub fn validate_queue(g: &mut Game, c: CityId) {
    let Some(city) = g.city(c) else { return };
    let kept: SmallVec<[Constructible; 4]> =
        city.queue.iter().copied().filter(|&x| is_buildable(g, c, x)).collect();
    if kept != city.queue
        && let Some(x) = g.city_mut(c, CityTouch::CORE)
    {
        x.queue = kept;
    }
}

/// The production stored for what can no longer be built (`cities._validate_progress`,
/// `cities.py:1740-1770`): a wonder another civilization finished pays it back as gold, an
/// obsolete unit's goes to the unit it upgrades to, and the rest is dropped.
fn validate_progress(g: &mut Game, c: CityId) {
    use RejectionKind as K;
    let Some(city) = g.city(c) else { return };
    let cur = current_construction(city);
    let owner = city.owner();
    let (name, at) = (city.name.clone(), city.tile());
    let items: Vec<(Constructible, f64)> =
        city.progress.iter().filter(|(k, _)| Some(**k) != cur).map(|(k, v)| (*k, *v)).collect();
    let r = g.rules();
    for (item, stored) in items {
        if matches!(item, Constructible::Perpetual(_)) {
            if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
                x.progress.remove(&item);
            }
            continue;
        }
        let rr = rejection_kinds(g, c, item);
        let lost = rr.iter().any(|x| {
            matches!(
                x,
                K::Obsoleted
                    | K::WonderAlreadyBuilt
                    | K::NationalWonderAlreadyBuilt
                    | K::MaxNumberBuildable
            )
        });
        if !lost {
            continue;
        }
        let done = num::trunc_i64(stored);
        match item {
            Constructible::Building(b) => {
                if r.buildings()[b].is_wonder && done != 0 {
                    #[allow(clippy::cast_precision_loss, reason = "production is far below 2^52")]
                    let gold = done as f64;
                    if let Some(x) = g.player_mut(owner, PlayerTouch::STOCKS) {
                        x.econ.gold += gold;
                    }
                    g.emit(
                        EngineEvent::WonderRefund,
                        &format!(
                            "Excess production for {} converted to {done} gold in {name}.",
                            r.buildings()[b].name
                        ),
                        Some(PlayerSet::single(owner)),
                        Some(at),
                        EventData::default(),
                        &[],
                    );
                }
            }
            Constructible::Unit(u) => {
                let obsolete_only = rr.iter().all(|&x| x == K::Obsoleted);
                if let Some(up) = r.base_units()[u].upgrades_to
                    && obsolete_only
                {
                    let up = Constructible::Unit(equivalent_unit(g, owner, up));
                    if is_buildable(g, c, up)
                        && let Some(x) = g.city_mut(c, CityTouch::STOCKS)
                    {
                        #[allow(clippy::cast_precision_loss, reason = "production is small")]
                        let add = done as f64;
                        *x.progress.entry(up).or_insert(0.0) += add;
                    }
                }
            }
            Constructible::Perpetual(_) => {}
        }
        if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
            x.progress.remove(&item);
        }
    }
}

/// At the start of a city's turn, what it builds is finished if the production stored pays for
/// it, and what is left over carries to the next, at most its cost or a turn's production
/// (`cities.construct_if_enough`, `cities.py:1720-1737`).
pub fn construct_if_enough(g: &mut Game, c: CityId) {
    validate_queue(g, c);
    validate_progress(g, c);
    let Some(city) = g.city(c) else { return };
    let Some(item) =
        current_construction(city).filter(|x| !matches!(x, Constructible::Perpetual(_)))
    else {
        return;
    };
    let owner = city.owner();
    let cost = f64::from(production_cost(g, owner, item, Some(c)));
    let done = city.progress.get(&item).copied().unwrap_or(0.0);
    if done < cost {
        return;
    }
    let overflow = done - cost;
    if complete_construction(g, c, item, None) {
        let prod = super::super::derive::stats::city_stats(g, c).production();
        let most = cost.max(num::round_half_even(prod));
        if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
            x.overflow = most.min(overflow);
        }
        // `Cost increases by [n] when built` counts what was finished. Python counted a unit
        // with no room to stand too, so every turn it waited raised its price.
        // refcheck: increasing-cost-counts-what-was-built
        if let Some(x) = g.player_mut(owner, PlayerTouch::OTHER) {
            let n = x.civ.built_increasing.entry(item).or_insert(0);
            *n = n.saturating_add(1);
        }
    } else if let Some(city) = g.city(c) {
        let (name, at) = (city.name.clone(), city.tile());
        g.emit(
            EngineEvent::ProductionBlocked,
            &format!("No room to place a {} near {name}.", item_name(g.rules(), item)),
            Some(PlayerSet::single(owner)),
            Some(at),
            EventData::default(),
            &[],
        );
    }
}

/// At the end of a city's turn its production goes into what it builds, with the overflow
/// (`cities.end_turn_production`, `cities.py:1773-1786`). Something begun is announced if it
/// alerts the world.
///
/// A conversion of production keeps nothing: the city's stats already turned the turn's
/// production into gold or science. Python banked it as overflow too, which the next item took
/// whole, so twenty turns of Gold finished a wonder the turn after; UnCiv's `endTurn` adds
/// nothing for a perpetual construction.
pub fn end_turn_production(g: &mut Game, c: CityId, production: f64) {
    validate_queue(g, c);
    validate_progress(g, c);
    let Some(city) = g.city(c) else { return };
    let Some(item) = current_construction(city) else { return };
    // refcheck: perpetual-production-is-not-banked
    if matches!(item, Constructible::Perpetual(_)) {
        return;
    }
    let prod = num::round_half_even(production);
    if city.progress.get(&item).copied().unwrap_or(0.0) == 0.0 {
        construction_begun(g, c, item);
    }
    if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
        let over = x.overflow;
        *x.progress.entry(item).or_insert(0.0) += prod + over;
        x.overflow = 0.0;
    }
}

/// Something the world notices when it is begun, a wonder with `Triggers a global alert upon
/// build start`, is announced (`cities._construction_begun`, `cities.py:1789-1794`).
fn construction_begun(g: &mut Game, c: CityId, item: Constructible) {
    let r = g.rules();
    let alerts = match item {
        Constructible::Building(b) => {
            has_type(r, &r.buildings()[b].uniques, UniqueType::TriggersAlertOnStart)
        }
        Constructible::Unit(u) => unit_has_type(r, u, UniqueType::TriggersAlertOnStart),
        Constructible::Perpetual(_) => false,
    };
    let Some(city) = g.city(c).filter(|_| alerts) else { return };
    let (owner, at) = (city.owner(), city.tile());
    let who = g.player(owner).map(|x| x.name.clone()).unwrap_or_default();
    let data = EventData { item: Some(item), player: Some(owner), ..EventData::default() };
    g.emit(
        EngineEvent::WonderStarted,
        &format!("{who} has started constructing {}!", item_name(r, item)),
        None,
        Some(at),
        data,
        &[],
    );
}

/// Finishes something in a city: the building added, or the unit placed in or near it, and
/// announced (`cities.complete_construction`, `cities.py:1797-1834`). `bought_with` is the stat
/// it was bought with. `false` when a unit could not be placed, which the caller must not charge
/// for.
pub fn complete_construction(
    g: &mut Game,
    c: CityId,
    item: Constructible,
    bought_with: Option<Stat>,
) -> bool {
    let r = g.rules();
    let Some(city) = g.city(c) else { return false };
    let (owner, at, city_name) = (city.owner(), city.tile(), city.name.clone());
    let who = g.player(owner).map(|x| x.name.clone()).unwrap_or_default();
    match item {
        Constructible::Building(b) => {
            let bd = &r.buildings()[b];
            add_building(g, c, b, true);
            let data = EventData { item: Some(item), player: Some(owner), ..EventData::default() };
            if bd.is_wonder {
                g.edit_world(WorldTouch::WONDERS).wonders_built.insert(b, c);
                g.emit(
                    EngineEvent::WonderBuilt,
                    &format!("{who} has built {} in {city_name}.", bd.name),
                    None,
                    Some(at),
                    data.clone(),
                    &[],
                );
            } else {
                g.emit(
                    EngineEvent::BuildingBuilt,
                    &format!("{city_name} completed {}.", bd.name),
                    Some(PlayerSet::single(owner)),
                    Some(at),
                    data.clone(),
                    &[],
                );
            }
            if has_type(r, &bd.uniques, UniqueType::TriggersAlertOnCompletion) && !bd.is_wonder {
                g.emit(
                    EngineEvent::WonderBuilt,
                    &format!("{who} has completed {}!", bd.name),
                    None,
                    Some(at),
                    data,
                    &[],
                );
            }
        }
        Constructible::Unit(u) => {
            let Some(id) = add_unit_in_city(g, c, u) else { return false };
            if bought_with.is_some()
                && !unit_has_type(r, u, UniqueType::CanMoveImmediatelyOnceBought)
                && let Some(x) = g.unit_mut(id, UnitTouch::MOVES)
            {
                x.moves = 0;
            }
            add_construction_bonuses(g, id, c);
            let data = EventData {
                unit: Some(id),
                item: Some(item),
                player: Some(owner),
                ..EventData::default()
            };
            g.emit(
                EngineEvent::UnitBuilt,
                &format!("{city_name} trained a {}.", r.base_units()[u].name),
                Some(PlayerSet::single(owner)),
                Some(at),
                data,
                &[],
            );
        }
        Constructible::Perpetual(_) => return false,
    }
    if let Some(x) = g.city_mut(c, CityTouch::CORE | CityTouch::STOCKS) {
        x.progress.remove(&item);
        if x.queue.first() == Some(&item) {
            x.queue.remove(0);
        }
    }
    validate_queue(g, c);
    true
}

/// A unit made in a city (`units.add_unit_in_city`, `units.py:113-136`): a ship in a city off the
/// coast goes to the civilization's first coastal city; placed on its city's tile or near it.
fn add_unit_in_city(g: &mut Game, c: CityId, u: BaseUnitId) -> Option<UnitId> {
    let r = g.rules();
    let city = g.city(c)?;
    let owner = city.owner();
    let mut target = c;
    if r.base_units()[u].domain == Domain::Water
        && !(g.is_water(city.tile()) || crate::game::tiles::adjacent_to_coast(g, city.tile()))
    {
        target = g
            .player_cities(owner)
            .find(|x| crate::game::tiles::adjacent_to_coast(g, x.tile()))
            .map(crate::state::cities::City::id)?;
    }
    let at = g.city(target)?.tile();
    let id = place_near(g, owner, u, at)?;
    if let Some(x) = g.unit_mut(id, UnitTouch::CORE) {
        x.origin_city = Some(target);
    }
    // A religious unit takes its city's majority religion (or its founder's), and `upon gaining
    // a [unit]` fires (units.py:127-135).
    pending(Porting::Pending("1b-08"));
    Some(id)
}

/// Whether a new unit of `u` could stand on `t` for `p`: a simplified `movement.can_stand`
/// (`movement.py:260-271`) for a unit that has just been made, which never embarks.
fn stands(g: &Game, p: PlayerId, u: BaseUnitId, t: TileIdx) -> bool {
    let r = g.rules();
    let d = &r.base_units()[u];
    let city = g.city_at(t);
    if d.domain == Domain::Air {
        return city.is_some_and(|x| x.owner() == p && air_capacity_ok(g, x.id()));
    }
    if city.is_some_and(|x| x.owner() != p) {
        return false;
    }
    if !passes(g, p, u, t) {
        return false;
    }
    if d.domain == Domain::Water
        && city.is_some()
        && !(g.is_water(t) || crate::game::tiles::adjacent_to_coast(g, t))
    {
        return false;
    }
    // stack_reason (movement.py:208-236): no foreign unit, and none of its own kind.
    !g.units_at(t).any(|o| {
        let od = &r.base_units()[o.base];
        od.domain != Domain::Air && (o.owner() != p || od.military == d.military)
    })
}

/// Whether a new unit of `u` could pass through `t` for `p`: its terrain for the unit's domain
/// (land on land, water on water, a city for either), and a territory it may enter.
fn passes(g: &Game, p: PlayerId, u: BaseUnitId, t: TileIdx) -> bool {
    let d = &g.rules().base_units()[u];
    let city = g.city_at(t).is_some();
    if !city && crate::game::tiles::is_impassable(g, t) {
        return false;
    }
    let water = g.is_water(t);
    let terrain_ok = match d.domain {
        Domain::Land => !water || city,
        Domain::Water => water || city,
        Domain::Air => true,
    };
    let owner = g.tile(t).and_then(crate::state::map::Tile::owner);
    terrain_ok && owner.is_none_or(|o| o == p || g.can_enter_territory(p, t))
}

/// Where a new unit of `u` would be placed for `p`, on or near `at` (`units.place_unit_near`,
/// `units.py:86-110`): the tile itself, else the nearest ring by ring through tiles it could pass,
/// land before water for a land unit, ten rings at most; `None` if there is no room. Reads only.
#[must_use]
pub fn placement(g: &Game, p: PlayerId, u: BaseUnitId, at: TileIdx) -> Option<TileIdx> {
    // movement.can_stand and can_pass_through in full (embarking, the ocean).
    pending(Porting::Pending("1c-02"));
    if stands(g, p, u, at) {
        return Some(at);
    }
    let land = g.rules().base_units()[u].domain == Domain::Land;
    let mut checked: Vec<TileIdx> = vec![at];
    let mut frontier: Vec<TileIdx> =
        g.grid().neighbors(at).filter(|&n| passes(g, p, u, n)).collect();
    for _ in 0..10 {
        let first = frontier.iter().copied().filter(|&x| !land || g.is_land(x));
        let second = frontier.iter().copied().filter(|&x| land && !g.is_land(x));
        if let Some(spot) = first.chain(second).find(|&x| stands(g, p, u, x)) {
            return Some(spot);
        }
        checked.extend(frontier.iter().copied());
        let mut next: Vec<TileIdx> = Vec::new();
        for &x in &frontier {
            for n in g.grid().neighbors(x) {
                if !checked.contains(&n) && !next.contains(&n) && passes(g, p, u, n) {
                    next.push(n);
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    None
}

/// Places a new unit on or near a tile ([`placement`]).
fn place_near(g: &mut Game, p: PlayerId, u: BaseUnitId, at: TileIdx) -> Option<UnitId> {
    let spot = placement(g, p, u, at)?;
    g.create_unit(p, u, spot, 0).ok()
}

/// Where a unit bought or finished in city `c` would be placed, if anywhere: a ship in a city off
/// the coast in its owner's first coastal city (`units.add_unit_in_city`).
#[must_use]
pub fn unit_placement(g: &Game, c: CityId, u: BaseUnitId) -> Option<TileIdx> {
    let city = g.city(c)?;
    let owner = city.owner();
    let at = if g.rules().base_units()[u].domain == Domain::Water
        && !(g.is_water(city.tile()) || crate::game::tiles::adjacent_to_coast(g, city.tile()))
    {
        g.player_cities(owner).find(|x| crate::game::tiles::adjacent_to_coast(g, x.tile()))?.tile()
    } else {
        city.tile()
    };
    placement(g, owner, u, at)
}

/// The experience and promotions a city gives the units it makes
/// (`units.add_construction_bonuses`, `units.py:139-155`).
fn add_construction_bonuses(g: &mut Game, id: UnitId, c: CityId) {
    let r = g.rules();
    let t = r.uniques();
    let Some(unit) = g.unit(id) else { return };
    let base = unit.base;
    let (xp, promotions) = {
        let v = g.view();
        let ctx = Ctx::city(&v, c);
        let filters = t.filters();
        let mut xp = 0;
        for h in uq::city(&v, c, UniqueType::UnitStartingExperience, &ctx) {
            if let UniqueData::UnitStartingExperience(x) = h.data()
                && t.in_set(x.units, base)
                && filters.city_matches(x.cities, &v, c, None)
            {
                xp += x.xp * i32::from(h.n);
            }
        }
        let mut promotions: SmallVec<[crate::base::ids::PromotionId; 2]> = SmallVec::new();
        for h in uq::city(&v, c, UniqueType::UnitStartingPromotions, &ctx) {
            if let UniqueData::UnitStartingPromotions(x) = h.data()
                && filters.city_matches(x.cities, &v, c, None)
                && t.in_set(x.units, base)
            {
                promotions.push(x.promotion);
            }
        }
        (xp, promotions)
    };
    let skip: SmallVec<[bool; 2]> = promotions
        .iter()
        .map(|&pr| has_type(r, &r.promotions()[pr].uniques, UniqueType::SkipPromotion))
        .collect();
    if let Some(x) = g.unit_mut(id, UnitTouch::CORE) {
        x.xp = xp;
        for (&pr, &skip) in promotions.iter().zip(&skip) {
            if !skip {
                x.promotions.insert(pr);
            }
        }
    }
    if !promotions.is_empty() {
        // What a free promotion does at once (`units.add_promotion`, units.py:240-245).
        pending(Porting::Pending("1b-08"));
    }
}

/// A stat that goes to a city (`cities.add_city_stat`, `cities.py:2380-2391`): production into
/// what it builds (or its overflow), food into its store, anything else to its owner.
pub fn add_city_stat(g: &mut Game, c: CityId, stat: Stat, amount: f64) {
    let Some(city) = g.city(c) else { return };
    let owner = city.owner();
    match stat {
        Stat::Production => {
            let cur =
                current_construction(city).filter(|x| !matches!(x, Constructible::Perpetual(_)));
            if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
                match cur {
                    Some(item) => *x.progress.entry(item).or_insert(0.0) += amount,
                    None => x.overflow += amount,
                }
            }
        }
        Stat::Food => {
            if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
                x.food = (x.food + amount).max(0.0);
            }
        }
        _ => g.add_stat(owner, stat, amount),
    }
}

/// The turns a city takes to finish an item (`cities.turns_to_build`), as the tools report it:
/// `None` for a conversion of production.
#[must_use]
pub fn turns_for(g: &Game, c: CityId, item: Constructible) -> Option<i32> {
    (!matches!(item, Constructible::Perpetual(_))).then(|| cstats::turns_to_build(g, c, item))
}

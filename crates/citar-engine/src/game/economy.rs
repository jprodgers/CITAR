//! A civilization's economy (`economy.py`): which resources it has and where each comes from,
//! the upkeep of its units and routes, how many units it can support, the gold it banks at the
//! end of its turn and the bankruptcy that disbands its units, and its temporary uniques running
//! out.
//!
//! Ports:
//! - the resources, `economy.py:186-358`: what a tile, a city, an allied city-state, a deal and a
//!   unit add or take (`compute_supply`). The supply is a memo (`ResourceSupply`, DESIGN.md
//!   6.5), read through [`supply`]; Python rebuilt it whenever anything was invalidated;
//! - the upkeep of units and routes and the unit supply, `economy.py:505-600`;
//! - gold at the end of a turn and bankruptcy (`process_gold`, `economy.py:731-746`): stage E3;
//! - temporary uniques running out (`_temp_uniques_end_turn`, `turns.py:120-131`): stage E5.
//!
//! What differs from Python, on purpose:
//! - the supply is computed from the unique index without its resource layer, as Python's
//!   `_civ_uniques_nores` read it, and so are the conditionals it evaluates: while it is
//!   computed a civilization's resources read as none (DESIGN.md 6.6). Python recursed without
//!   end on a resource unique whose conditional asked for a resource;
//! - a `[n] units cost no maintenance` that takes the allowance below none leaves it at none,
//!   where Python's slice counted the free units from the other end.

use core::cell::Ref;

use super::derive::civ;
use super::derive::rev::PlayerTouch;
use super::eval::EvalView;
use super::{Game, Porting, pending_or};
use crate::base::ids::{BuildingId, CityId, Id, PlayerId, ResourceId, TileIdx, UnitId};
use crate::base::num::{self, trunc_i32};
use crate::base::sets::{PlayerSet, ResourceSet};
use crate::base::stats::Stats;
use crate::rules::Ruleset;
use crate::rules::defs::{ResourceType, Route};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::diplo::DealItem;
use crate::unique::{Ctx, EvalWorld, FilterFacts, Source, UniqueData, UniqueType, applies, uq};

// ---- The resource supply (economy.py:186-358) --------------------------------------------------

/// Where a resource of a civilization's supply comes from, or goes (the origins of
/// `economy.detailed_resources`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The improved tiles of a city (`"Tiles"`).
    Tiles,
    /// What the buildings of a city need (`"Buildings"`).
    Buildings,
    /// A Mercantile city-state's own luxury, in its capital (`"Mercantile City-State"`).
    MercantileCityState,
    /// `Provides [n] [resource]` on a building of a city: the building.
    Building(BuildingId),
    /// What the city-states allied with it share (`"City-States"`).
    CityStates,
    /// `Provides [n] [resource]` of any other source: that source.
    Unique(Source),
    /// Deals (`"Trade"`).
    Trade,
    /// What its units need (`"Units"`).
    Units,
}

impl Origin {
    /// The origin as Python named it: `"Tiles"`, a building's name, a policy's.
    #[must_use]
    pub fn name(self, r: &Ruleset) -> &str {
        match self {
            Self::Tiles => "Tiles",
            Self::Buildings => "Buildings",
            Self::MercantileCityState => "Mercantile City-State",
            Self::Building(b) => &r.buildings()[b].name,
            Self::CityStates => "City-States",
            Self::Unique(s) => source_name(r, s),
            Self::Trade => "Trade",
            Self::Units => "Units",
        }
    }
}

/// The name of the object a unique came from, as Python's `src_name` gave it
/// (`uniques.parse_list`, `rules.py:82, 94, 160-161`).
#[must_use]
pub fn source_name(r: &Ruleset, s: Source) -> &str {
    match s {
        Source::Nation(id) => &r.nations()[id].name,
        Source::Building(id) => &r.buildings()[id].name,
        Source::Policy(id) => &r.policies()[id].name,
        Source::Tech(id) => &r.techs()[id].name,
        Source::Temporary(_) => "Temporary",
        Source::Era(id) => &r.eras()[id].name,
        Source::CityStateFriend(id) | Source::CityStateAlly(id) | Source::CityStateType(id) => {
            &r.city_state_types()[id].name
        }
        Source::Belief(id) => &r.beliefs()[id].name,
        Source::Resource(id) => &r.resources()[id].name,
        Source::Global => "Global",
        Source::Terrain(id) => &r.terrains()[id].name,
        Source::Improvement(id) => &r.improvements()[id].name,
        Source::UnitType(id) => &r.unit_types()[id].name,
        Source::Unit(id) => &r.base_units()[id].name,
        Source::Promotion(id) => &r.promotions()[id].name,
        Source::Ruins(id) => &r.ruins()[id].name,
    }
}

/// One line of a civilization's supply: an amount of a resource, and where it comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceItem {
    pub resource: ResourceId,
    pub origin: Origin,
    /// Positive for what comes in, negative for what is used or given away.
    pub amount: i32,
}

/// A civilization's resources (`economy.detailed_resources` and `resource_supply`): every line,
/// in Python's order, and the net amount of each resource a line names, in the order they first
/// appear. A resource whose lines cancel out is there with 0, as it was in Python's dict.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceSupply {
    items: Vec<ResourceItem>,
    totals: Vec<(ResourceId, i32)>,
}

impl ResourceSupply {
    fn from_items(items: Vec<ResourceItem>) -> Self {
        let mut totals: Vec<(ResourceId, i32)> = Vec::new();
        for it in &items {
            match totals.iter_mut().find(|(r, _)| *r == it.resource) {
                Some((_, n)) => *n = n.saturating_add(it.amount),
                None => totals.push((it.resource, it.amount)),
            }
        }
        Self { items, totals }
    }

    /// Every line (`detailed_resources`).
    #[must_use]
    pub fn items(&self) -> &[ResourceItem] {
        &self.items
    }

    /// The net amount of each resource a line names (`resource_supply`).
    #[must_use]
    pub fn totals(&self) -> &[(ResourceId, i32)] {
        &self.totals
    }

    /// How much of a resource is available (`resource_amount`): net of what units and
    /// buildings use and deals give away, so it may be below zero.
    #[must_use]
    pub fn amount(&self, r: ResourceId) -> i32 {
        self.totals.iter().find(|(x, _)| *x == r).map_or(0, |&(_, n)| n)
    }

    /// The resources it has some of: whose uniques are its index's resource layer
    /// (`economy.resource_umap`, `economy.py:323-336`).
    #[must_use]
    pub fn positive(&self) -> ResourceSet {
        let mut out = ResourceSet::new();
        for &(r, n) in &self.totals {
            if n > 0 {
                out.insert(r);
            }
        }
        out
    }
}

impl super::derive::rev::BitEq for ResourceSupply {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// A civilization's resources, from its memo (DESIGN.md 6.5); `None` for a player the game does
/// not have.
#[must_use]
pub fn supply(g: &Game, p: PlayerId) -> Option<Ref<'_, ResourceSupply>> {
    civ::supply(g, p)
}

/// How much of a resource a civilization has available (`economy.resource_amount`,
/// `economy.py:356-358`).
#[must_use]
pub fn resource_amount(g: &Game, p: PlayerId, r: ResourceId) -> i32 {
    supply(g, p).map_or(0, |s| s.amount(r))
}

/// What a civilization's supply is, computed (`economy.detailed_resources`,
/// `economy.py:279-311`): its cities' lines, then, for a major, what its allied city-states
/// share, its other `Provides [n] [resource]` uniques, its deals, and what its units need. What
/// the memo `ResourceSupply` holds; read it through [`supply`].
///
/// It reads the unique index without its resource layer, and the conditionals it evaluates see
/// no resources (DESIGN.md 6.6): the uniques of the resources a civilization has depend on this
/// supply.
pub(crate) fn compute_supply(g: &Game, p: PlayerId) -> ResourceSupply {
    let v = EvalView::for_supply(g);
    let r = g.rules;
    let mut items = Vec::new();
    for &c in g.state().cities().of(p) {
        city_resources(&v, c, &mut items);
    }
    let Some(player) = g.player(p) else { return ResourceSupply::default() };
    if player.is_major() {
        let ctx = Ctx::civ(p);
        let mut pct = 1.0;
        for h in uq::civ_no_resources(&v, p, UniqueType::CityStateResources, &ctx) {
            if let UniqueData::CityStateResources(x) = h.data() {
                pct += f64::from(x.percent) * f64::from(h.n) / 100.0;
            }
        }
        for cs in allied_city_states(g, p) {
            for (res, a) in resources_for_ally(&v, cs) {
                items.push(ResourceItem {
                    resource: res,
                    origin: Origin::CityStates,
                    amount: trunc_i32(f64::from(a) * pct),
                });
            }
        }
    }
    let t = r.uniques();
    for h in uq::civ_no_resources(&v, p, UniqueType::ProvidesResources, &Ctx::civ(p)) {
        let source = t.meta(h.id).source;
        if let (UniqueData::ProvidesResources(x), false) =
            (h.data(), matches!(source, Source::Building(_)))
        {
            for _ in 0..h.n {
                items.push(ResourceItem {
                    resource: x.resource,
                    origin: Origin::Unique(source),
                    amount: x.amount,
                });
            }
        }
    }
    for (res, amount) in deal_resource_flows(g, p) {
        items.push(ResourceItem { resource: res, origin: Origin::Trade, amount });
    }
    for u in g.player_units(p) {
        let def = &r.base_units()[u.base];
        if let Some(res) = def.required_resource {
            items.push(ResourceItem { resource: res, origin: Origin::Units, amount: -1 });
        }
        // Python read the unit's own map, its unit type's uniques among them
        // (`rules.py:116-118`), without conditionals.
        let own = uq::object(&v, &def.uniques, UniqueType::ConsumesResources, &Ctx::IGNORE);
        let of_type = uq::object(
            &v,
            &r.unit_types()[def.unit_type].uniques,
            UniqueType::ConsumesResources,
            &Ctx::IGNORE,
        );
        for h in own.chain(of_type) {
            if let UniqueData::ConsumesResources(x) = h.data() {
                items.push(ResourceItem {
                    resource: x.resource,
                    origin: Origin::Units,
                    amount: x.amount.saturating_neg(),
                });
            }
        }
    }
    ResourceSupply::from_items(items)
}

/// The tiles a city owns (`cities.city_tiles`, `cities.py:155-162`): those within its expansion
/// range whose territory it is, nearest first.
#[must_use]
pub fn city_tiles(g: &Game, c: CityId) -> Vec<TileIdx> {
    let Some(city) = g.city(c) else { return Vec::new() };
    let range = u32::try_from(g.rules.constants().formulas.city_expand_range).unwrap_or(0);
    let mut out = g.grid().within(city.tile(), range);
    out.retain(|&t| g.tile(t).and_then(crate::state::map::Tile::city) == Some(c));
    out
}

/// Whether a tile gives its owner `p` its resource (`economy.tile_provides_resource`,
/// `economy.py:196-219`): a resource `p` can see, improved by an unpillaged improvement that
/// improves it (a great improvement improves any strategic resource). On a city's own tile it
/// needs no improvement, only the tech of one that would improve it, or none at all.
#[must_use]
pub fn tile_provides_resource(g: &Game, t: TileIdx, p: PlayerId) -> bool {
    let r = g.rules;
    let Some(tile) = g.tile(t) else { return false };
    let Some(res) = tile.resource() else { return false };
    let rd = &r.resources()[res];
    if !g.has_tech(p, rd.revealed_by) {
        return false;
    }
    if g.state().city_at(t).is_some() {
        let mut imps = rd.improvement.into_iter().chain(rd.improved_by.iter().copied()).peekable();
        if imps.peek().is_none() {
            return true;
        }
        return imps.any(|i| {
            let d = &r.improvements()[i];
            d.turns_to_build != Some(-1) && g.has_tech(p, d.tech_required)
        });
    }
    let Some(imp) = tile.improvement().filter(|_| !tile.improvement_pillaged()) else {
        return false;
    };
    if rd.improvement == Some(imp) || rd.improved_by.contains(&imp) {
        return true;
    }
    rd.kind == ResourceType::Strategic && r.improvements()[imp].great
}

/// The resources a city's owner gets from the tiles the city owns. Of these, those its owner's
/// supply has some of give the city their uniques that hold in one city alone (the Marble
/// decision, DESIGN.md 5.12).
#[must_use]
pub fn provided_resources(g: &Game, c: CityId) -> ResourceSet {
    let mut out = ResourceSet::new();
    let Some(owner) = g.city(c).map(crate::state::cities::City::owner) else { return out };
    for t in city_tiles(g, c) {
        if tile_provides_resource(g, t, owner)
            && let Some(res) = g.tile(t).and_then(crate::state::map::Tile::resource)
        {
            out.insert(res);
        }
    }
    out
}

/// What a city's `[n]% [resources] resource production` uniques multiply each resource by
/// (`economy.resource_modifiers`, `economy.py:222-232`): its own and its religion's, in its
/// context, then its owner's, in the owner's.
fn resource_modifiers(v: &EvalView<'_>, c: CityId) -> Vec<f64> {
    let r = v.rules();
    let t = r.uniques();
    let mut mods = vec![1.0; r.resources().len()];
    let owner = v.city_owner(c);
    let ty = UniqueType::PercentResourceProduction;
    let local = uq::local(v, c, ty, &Ctx::city(v, c));
    let civ_wide = uq::civ_no_resources(v, owner, ty, &Ctx::civ(owner));
    for h in local.chain(civ_wide) {
        let UniqueData::PercentResourceProduction(x) = h.data() else { continue };
        let bonus = f64::from(x.percent) / 100.0;
        for (res, m) in r.resources().ids().zip(mods.iter_mut()) {
            if t.in_set(x.resources, res) {
                for _ in 0..h.n {
                    *m += bonus;
                }
            }
        }
    }
    mods
}

/// What a city adds to its owner's resources, or takes (`economy.city_resources`,
/// `economy.py:235-276`): its tiles' resources, one line per resource; what its buildings need;
/// a Mercantile city-state capital's own luxury; and its buildings' `Provides [n] [resource]`.
fn city_resources(v: &EvalView<'_>, c: CityId, out: &mut Vec<ResourceItem>) {
    let g = v.game();
    let r = g.rules;
    let Some(city) = g.city(c) else { return };
    let owner = city.owner();
    let mods = resource_modifiers(v, c);
    let modded = |res: ResourceId, a: i32| trunc_i32(f64::from(a) * mods[res.index()]);
    let buildings = r.buildings();
    let extra_lux = city.buildings.iter().any(|b| {
        super::core::has_type(
            r,
            &buildings[b].uniques,
            UniqueType::ProvidesExtraLuxuryFromCityResources,
        )
    });
    let mut from_tiles: Vec<(ResourceId, i32)> = Vec::new();
    for t in city_tiles(g, c) {
        let Some(tile) = g.tile(t) else { continue };
        let Some(res) = tile.resource() else { continue };
        if !tile_provides_resource(g, t, owner) {
            continue;
        }
        let kind = r.resources()[res].kind;
        let mut amount =
            if kind == ResourceType::Strategic { i32::from(tile.resource_amount()) } else { 1 };
        if kind == ResourceType::Luxury && extra_lux {
            amount += 1;
        }
        if amount > 0 {
            match from_tiles.iter_mut().find(|(x, _)| *x == res) {
                Some((_, n)) => *n += amount,
                None => from_tiles.push((res, amount)),
            }
        }
    }
    for (res, a) in from_tiles {
        out.push(ResourceItem { resource: res, origin: Origin::Tiles, amount: modded(res, a) });
    }
    for b in city.buildings.iter() {
        if city.free_buildings.contains(b) {
            continue;
        }
        if let Some(res) = buildings[b].required_resource {
            out.push(ResourceItem { resource: res, origin: Origin::Buildings, amount: -1 });
        }
    }
    if let Some(p) = g.player(owner)
        && p.is_city_state()
        && p.capital == Some(c)
        && let Some(res) = p.city_state.as_deref().and_then(|d| d.resource)
    {
        out.push(ResourceItem { resource: res, origin: Origin::MercantileCityState, amount: 1 });
    }
    let ctx = Ctx::city(v, c);
    for b in city.buildings.iter() {
        for h in uq::object(v, &buildings[b].uniques, UniqueType::ProvidesResources, &ctx) {
            if let UniqueData::ProvidesResources(x) = h.data() {
                out.push(ResourceItem {
                    resource: x.resource,
                    origin: Origin::Building(b),
                    amount: modded(x.resource, x.amount),
                });
            }
        }
    }
}

/// The city-states allied with a major, by id (`city_states.allied_city_states`,
/// `city_states.py:155-157`).
#[must_use]
pub fn allied_city_states(g: &Game, major: PlayerId) -> Vec<PlayerId> {
    g.city_states(true)
        .filter(|q| q.city_state.as_deref().and_then(|d| d.ally()) == Some(major))
        .map(crate::state::players::Player::id)
        .collect()
}

/// What an allied city-state shares with its ally (`city_states.resources_for_ally`,
/// `city_states.py:176-184`): the resources its cities produce, summed, in the order they first
/// appear.
fn resources_for_ally(v: &EvalView<'_>, cs: PlayerId) -> Vec<(ResourceId, i32)> {
    let mut lines = Vec::new();
    for &c in v.game().state().cities().of(cs) {
        city_resources(v, c, &mut lines);
    }
    let mut out: Vec<(ResourceId, i32)> = Vec::new();
    for it in lines.into_iter().filter(|it| it.amount > 0) {
        match out.iter_mut().find(|(r, _)| *r == it.resource) {
            Some((_, n)) => *n += it.amount,
            None => out.push((it.resource, it.amount)),
        }
    }
    out
}

/// The resources flowing in and out of a civilization under the deals in force
/// (`diplomacy.deal_resource_flows`, `diplomacy.py:612-625`), in deal order.
fn deal_resource_flows(g: &Game, p: PlayerId) -> Vec<(ResourceId, i32)> {
    let turn = g.turn();
    let mut out = Vec::new();
    for d in g.state().diplo().deals.iter().filter(|d| d.active) {
        for it in &d.ongoing {
            let DealItem::Resource { resource, amount, .. } = it.item else { continue };
            if it.until < turn {
                continue;
            }
            if it.to == p {
                out.push((resource, amount));
            } else if it.from == p {
                out.push((resource, amount.saturating_neg()));
            }
        }
    }
    out
}

/// The players whose lands and cities a civilization's supply reads: itself, and for a major
/// the city-states allied with it. Read on every validation of the supply, so it allocates
/// nothing.
pub(crate) fn supply_owners(g: &Game, p: PlayerId) -> PlayerSet {
    let mut out = PlayerSet::single(p);
    if g.player(p).is_some_and(crate::state::players::Player::is_major) {
        for q in g.city_states(true) {
            if q.city_state.as_deref().and_then(|d| d.ally()) == Some(p) {
                out.insert(q.id());
            }
        }
    }
    out
}

/// The tiles a civilization owns, in map order (`economy.owned_tiles`, `economy.py:186-193`),
/// from their memo; `None` for a player the game does not have.
#[must_use]
pub fn owned_tiles(g: &Game, p: PlayerId) -> Option<Ref<'_, Vec<TileIdx>>> {
    civ::owned_tiles(g, p)
}

// ---- Upkeep and supply (economy.py:505-600) ----------------------------------------------------

/// The gold a civilization pays for its units each turn, after its free allowance
/// (`economy.unit_maintenance`, `economy.py:505-538`, UnCiv's `getUnitMaintenance`).
///
/// Each unit costs 1, times its `[n]% maintenance costs` and its owner's; the dearest are paid
/// for, after the first 3 plus `[n] units cost no maintenance`, and the total grows over the
/// game: with the game's progress `x`, from 0 to 1, it is `(0.5 (1 + x) cost)^(1 + x/3)`. An AI
/// major pays its seat's `aiUnitMaintenanceModifier` of that.
#[must_use]
pub fn unit_maintenance(g: &Game, p: PlayerId) -> i32 {
    let v = g.view();
    let ctx = Ctx::civ(p);
    let mut free = 3i64;
    for h in uq::civ(&v, p, UniqueType::FreeUnits, &ctx) {
        if let UniqueData::FreeUnits(x) = h.data() {
            free += i64::from(x.count) * i64::from(h.n);
        }
    }
    let skip_garrisons = uq::any(uq::civ(&v, p, UniqueType::UnitsInCitiesNoMaintenance, &ctx));
    let civ_wide: Vec<(crate::base::ids::UniqueId, u16, i32)> =
        uq::raw(&v, p, UniqueType::UnitMaintenanceDiscount)
            .filter_map(|h| match h.data() {
                UniqueData::UnitMaintenanceDiscount(x) => Some((h.id, h.n, x.percent)),
                _ => None,
            })
            .collect();
    let mut costs: Vec<f64> = Vec::new();
    for u in g.player_units(p) {
        let id = u.id();
        if skip_garrisons
            && g.state().city_at(u.tile()).is_some()
            && super::units::can_garrison(g, id)
        {
            continue;
        }
        let uctx = Ctx::unit(&v, id);
        let mut m = 1.0;
        let times = |m: &mut f64, pct: i32, n: u16| {
            for _ in 0..n {
                *m *= 1.0 + f64::from(pct) / 100.0;
            }
        };
        for h in uq::unit(&v, id, UniqueType::UnitMaintenanceDiscount, &uctx) {
            if let UniqueData::UnitMaintenanceDiscount(x) = h.data() {
                times(&mut m, x.percent, h.n);
            }
        }
        for &(uid, n, pct) in &civ_wide {
            if applies(uid, &uctx, &v) {
                times(&mut m, pct, n);
            }
        }
        costs.push(m);
    }
    costs.sort_by(|a, b| b.total_cmp(a));
    let free = usize::try_from(free.max(0)).unwrap_or(usize::MAX);
    let to_pay: f64 = costs.iter().skip(free).sum::<f64>().max(0.0);
    let limit = f64::from(g.total_turns().max(1));
    let progress = (f64::from(g.turn()) / limit).min(1.0);
    let mut cost = 0.5 * to_pay * (1.0 + progress);
    cost = if cost > 0.0 { num::pow(cost, 1.0 + progress / 3.0) } else { 0.0 };
    if !g.is_humanlike(p) && g.player(p).is_some_and(crate::state::players::Player::is_major) {
        cost *= g.rules.difficulties()[g.seat_difficulty(Some(p))].ai_unit_maintenance_modifier;
    }
    trunc_i32(cost)
}

/// What a civilization's roads and railroads cost each turn, by stat (`economy.transport_upkeep`,
/// `economy.py:541-567`): the `Costs [n] [stat] per turn` of the unpillaged route on each tile
/// it owns but a city's, but in tiles its `No Maintenance costs for improvements in [tiles]
/// tiles` names, times its `[n]% maintenance on road & railroads`. It walks the tiles the
/// civilization owns ([`owned_tiles`]), not the map.
#[must_use]
pub fn transport_upkeep(g: &Game, p: PlayerId) -> Stats {
    let v = g.view();
    let r = g.rules;
    let filters = r.uniques().filters();
    let ignored: Vec<_> =
        uq::civ(&v, p, UniqueType::NoImprovementMaintenanceInSpecificTiles, &Ctx::civ(p))
            .filter_map(|h| match h.data() {
                UniqueData::NoImprovementMaintenanceInSpecificTiles(x) => Some(x.tiles),
                _ => None,
            })
            .collect();
    let known = &r.derived().known;
    let mut out = Stats::ZERO;
    let Some(owned) = owned_tiles(g, p) else { return out };
    for &t in owned.iter() {
        let Some(tile) = g.tile(t) else { continue };
        let road = match tile.route().filter(|_| !tile.route_pillaged()) {
            None => continue,
            Some(Route::Road) => known.road,
            Some(Route::Railroad) => known.railroad,
        };
        if g.state().city_at(t).is_some()
            || ignored.iter().any(|&f| filters.tile_matches(f, &v, t, Some(p)))
        {
            continue;
        }
        let ctx = Ctx::tile(Some(p), t);
        let uniques = &r.improvements()[road].uniques;
        for ty in [UniqueType::ImprovementMaintenance, UniqueType::ImprovementAllMaintenance] {
            for h in uq::object(&v, uniques, ty, &ctx) {
                let (amount, stat) = match h.data() {
                    UniqueData::ImprovementMaintenance(x) => (x.amount, x.stat),
                    UniqueData::ImprovementAllMaintenance(x) => (x.amount, x.stat),
                    _ => continue,
                };
                out[stat] += f64::from(amount);
            }
        }
    }
    for h in uq::civ(&v, p, UniqueType::RoadMaintenance, &Ctx::civ(p)) {
        if let UniqueData::RoadMaintenance(x) = h.data() {
            for _ in 0..h.n {
                out *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    out
}

/// How many units a civilization supports before its production suffers
/// (`economy.unit_supply`, `economy.py:570-584`): its difficulty's base and per-city supply,
/// with `[n] Unit Supply` and `[n] Unit Supply per city`; the population's share; and
/// `[n] Unit Supply per [k] population [cities]`. An AI major gets its seat's
/// `aiUnitSupplyModifier` more.
#[must_use]
pub fn unit_supply(g: &Game, p: PlayerId) -> i32 {
    let v = g.view();
    let r = g.rules;
    let ctx = Ctx::civ(p);
    let diff = &r.difficulties()[g.difficulty(Some(p))];
    let sum = |ty: UniqueType, f: fn(&UniqueData) -> Option<i32>| -> i64 {
        uq::civ(&v, p, ty, &ctx)
            .map(|h| f(h.data()).map_or(0, |n| i64::from(n) * i64::from(h.n)))
            .sum()
    };
    let base = i64::from(diff.unit_supply_base)
        + sum(UniqueType::BaseUnitSupply, |d| match d {
            UniqueData::BaseUnitSupply(x) => Some(x.supply),
            _ => None,
        });
    let per_city = i64::from(diff.unit_supply_per_city)
        + sum(UniqueType::UnitSupplyPerCity, |d| match d {
            UniqueData::UnitSupplyPerCity(x) => Some(x.supply),
            _ => None,
        });
    let cities: Vec<(CityId, i64)> =
        g.player_cities(p).map(|c| (c.id(), i64::from(c.pop))).collect();
    let n_cities = i64::try_from(cities.len()).unwrap_or(i64::MAX);
    let pop: i64 = cities.iter().map(|&(_, n)| n).sum();
    #[allow(clippy::cast_precision_loss, reason = "a population is far below 2^52")]
    let mut from_pop = pop as f64 * r.constants().formulas.unit_supply_per_population;
    let filters = r.uniques().filters();
    for h in uq::civ(&v, p, UniqueType::UnitSupplyPerPop, &ctx) {
        let UniqueData::UnitSupplyPerPop(x) = h.data() else { continue };
        let per = i64::from(x.per.max(1));
        let counted: i64 = cities
            .iter()
            .filter(|&&(c, _)| filters.city_matches(x.cities, &v, c, None))
            .map(|&(_, n)| n.div_euclid(per))
            .sum();
        #[allow(clippy::cast_precision_loss, reason = "far below 2^52")]
        let add = (i64::from(x.supply) * counted) as f64;
        for _ in 0..h.n {
            from_pop += add;
        }
    }
    let mut supply = base + n_cities * per_city + i64::from(trunc_i32(from_pop));
    if g.player(p).is_some_and(crate::state::players::Player::is_major) && !g.is_humanlike(p) {
        let m = r.difficulties()[g.seat_difficulty(Some(p))].ai_unit_supply_modifier;
        #[allow(clippy::cast_precision_loss, reason = "a unit supply is far below 2^52")]
        let scaled = supply as f64 * (1.0 + m);
        supply = i64::from(trunc_i32(scaled));
    }
    num::saturate_i32(supply)
}

/// How far over its unit supply a civilization is (`economy.unit_supply_deficit`,
/// `economy.py:587-594`).
#[must_use]
pub fn unit_supply_deficit(g: &Game, p: PlayerId) -> i32 {
    let units = i32::try_from(g.state().units().of(p).len()).unwrap_or(i32::MAX);
    (units - unit_supply(g, p)).max(0)
}

/// The production penalty for units over the supply, in percent: 10 a unit, at most 70
/// (`economy.unit_supply_penalty`, `economy.py:597-599`).
#[must_use]
pub fn unit_supply_penalty(g: &Game, p: PlayerId) -> f64 {
    -(f64::from(unit_supply_deficit(g, p)) * 10.0).min(70.0)
}

// ---- The end of a turn (economy.py:731-746, turns.py:120-131) ----------------------------------

/// The gold a civilization's treasury must be at or under before bankruptcy disbands its units
/// (`economy.py:735`).
pub const BANKRUPTCY: f64 = -200.0;

/// Stage E3, gold and bankruptcy (`economy.process_gold`, `economy.py:731-746`, UnCiv's
/// `TurnManager.endTurn`): while the treasury is at [`BANKRUPTCY`] or below and the turn's gold
/// is negative, a military unit is disbanded: one in the civilization's own land before one
/// abroad, and the one with the fewest promotions and least experience first. Then the turn's
/// gold is banked, truncated.
///
/// The turn's gold is the rate stage E2 committed (`last_gold_rate`, DESIGN.md 6.6), which is
/// what Python read from `civ_stats` just before.
pub(crate) fn end_turn_gold(g: &mut Game, p: PlayerId) {
    let Some(rate) = g.player(p).map(|x| x.econ.last_gold_rate) else { return };
    let upkeep = unit_maintenance(g, p);
    let mut gold = rate;
    while gold < 0.0 && g.player(p).is_some_and(|x| x.econ.gold <= BANKRUPTCY) {
        let rules = g.rules;
        let victim = g
            .player_units(p)
            .filter(|u| rules.base_units()[u.base].military)
            .min_by_key(|u| {
                let abroad = g.tile(u.tile()).and_then(crate::state::map::Tile::owner) != Some(p);
                let rank =
                    i64::try_from(u.promotions.len()).unwrap_or(i64::MAX) * 10 + i64::from(u.xp);
                (abroad, rank, u.id())
            })
            .map(|u| (u.id(), u.base, u.tile()));
        let Some((u, base, at)) = victim else { break };
        disband(g, u);
        let name = rules.base_units()[base].name.clone();
        g.emit(
            EngineEvent::Bankrupt,
            &format!("Cannot provide unit upkeep for {name} - unit has been disbanded!"),
            Some(PlayerSet::single(p)),
            Some(at),
            EventData::default(),
            &[],
        );
        gold = gold_after_disbanding(g, p, rate, upkeep);
    }
    if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
        x.econ.gold += gold.trunc();
    }
}

/// The turn's gold again after bankruptcy disbanded units. Python read `civ_stats` afresh; until
/// the civilization's stats are a memo (package 1b-06), the unit upkeep saved is added back to
/// the rate, the only part of it disbanding changes.
fn gold_after_disbanding(g: &Game, p: PlayerId, rate: f64, upkeep: i32) -> f64 {
    pending_or(Porting::Pending("1b-06"), rate + f64::from(upkeep - unit_maintenance(g, p)))
}

/// Disbands a unit (`units.disband`, `units.py:769-776`): what it carries goes with it. Inside
/// its own borders it refunds a twentieth of its purchase price, which needs the purchase costs
/// (package 1b-07).
fn disband(g: &mut Game, u: UnitId) {
    let Some(unit) = g.unit(u) else { return };
    let (owner, at) = (unit.owner(), unit.tile());
    let refund = if g.tile(at).and_then(crate::state::map::Tile::owner) == Some(owner) {
        pending_or(Porting::Pending("1b-07"), 0.0)
    } else {
        0.0
    };
    if refund != 0.0
        && let Some(x) = g.player_mut(owner, PlayerTouch::STOCKS)
    {
        x.econ.gold += refund;
    }
    let cargo: Vec<UnitId> = g.state().units().carried_by(u).collect();
    for x in cargo.into_iter().chain([u]) {
        if let Err(e) = g.despawn_unit(x) {
            debug_assert!(false, "a unit on the map could not be removed: {e}");
        }
    }
}

/// Stage E5 (`turns._temp_uniques_end_turn`, `turns.py:120-131`): every temporary unique has a
/// turn less, and those with none left go. Only a unique going moves the civilization's index.
pub(crate) fn expire_temp_uniques(g: &mut Game, p: PlayerId) {
    let Some(held) = g.player(p).map(|x| x.civ.temp_uniques.clone()) else { return };
    if held.is_empty() {
        return;
    }
    let expiring = held.iter().any(|t| t.turns <= 1);
    let touch = if expiring { PlayerTouch::INDEX } else { PlayerTouch::OTHER };
    if let Some(x) = g.player_mut(p, touch) {
        for t in &mut x.civ.temp_uniques {
            t.turns = t.turns.saturating_sub(1);
        }
        x.civ.temp_uniques.retain(|t| t.turns > 0);
    }
}

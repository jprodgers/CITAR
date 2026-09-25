//! The `Buildable` memo (DESIGN.md 6.5): what each city can build, which Python worked out item by
//! item on every read (`cities.buildable_items`, `cities.py:1347-1360`; 98 ms for 22 cities in a
//! late game).
//!
//! A city's list validates on read against what the rejection rules read of the game
//! (`cities.rejection_reasons`): the city itself (its buildings, size, status and religion, and
//! the tiles of its territory), the tiles and owners around it that `Must be on`, `Must be next
//! to` and `Must have an owned [] within [] tiles` look at, its owner's unique index with the
//! resource layer (techs, era, policies, nation), its units and cities (limits, national
//! wonders), its resources, the world wonders built, religions, the settings, and the classes of
//! the conditionals and filters its last computation evaluated (`unique::record`). Spaceship parts
//! are counted with the units they were, whose removal moves the owner's `roster`.
//!
//! The civilization-wide conditionals of `Only available` and `Can only be built` are asked once
//! per civilization, by a memo of its own ([`CivRequirements`]): `if [Monument] is constructed in
//! all [non-[Puppeted]] cities` reads every city's status, which any city's heal, growth or queue
//! edit moves (`CITY_COUNT`), and the lists validate against that memo's answers instead, which
//! rarely change.
//!
//! What moves all turn is left out of the memo, and `cities::construction::buildable_items` reads
//! it as it lends the list: the room a city has for aircraft (where its owner's units stand), and
//! what the owner's other cities are building (a wonder being built elsewhere, the queued items a
//! limit counts). So a sibling's heal, growth or queue edit recomputes no list.

use core::cell::{Cell, Ref, RefCell};

use super::civ;
use super::rev::{BitEq, Memo, Rev};
use crate::base::collections::LookupMap;
use crate::base::ids::{CityId, PlayerId, UniqueId};
use crate::game::Game;
use crate::game::cities::construction::{self, Buildable};
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::change::Change;
use crate::state::chronicle::Chronicle;
use crate::unique::filter::TileLeaf;
use crate::unique::{Cond, CondDeps, Ctx, UniqueData, UniqueType, cond, record};

/// One city's list, with the classes its last computation recorded.
#[derive(Clone, Debug)]
struct CityMemo {
    list: Memo<Buildable>,
    deps: Cell<CondDeps>,
}

impl Default for CityMemo {
    fn default() -> Self {
        Self { list: Memo::new(), deps: Cell::new(CondDeps::empty()) }
    }
}

/// The requirements of the ruleset's buildings and units (`Only available`, `Can only be built`)
/// whose civilization-wide conditionals do not all hold for a civilization now, by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CivRequirements {
    failing: Vec<UniqueId>,
}

impl BitEq for CivRequirements {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

/// One civilization's requirements, with the classes its last computation recorded.
#[derive(Clone, Debug)]
struct CivMemo {
    reqs: Memo<CivRequirements>,
    deps: Cell<CondDeps>,
}

impl Default for CivMemo {
    fn default() -> Self {
        Self { reqs: Memo::new(), deps: Cell::new(CondDeps::empty()) }
    }
}

/// Every city's list and every civilization's requirements: part of `Derived`.
#[derive(Clone, Debug)]
pub struct BuildableCaches {
    cities: LookupMap<CityId, CityMemo>,
    civs: LookupMap<PlayerId, CivMemo>,
    /// The requirements of the ruleset's buildings and units with a civilization-wide
    /// conditional, in id order.
    requirements: Vec<UniqueId>,
    /// How far from a city the rejection rules read tiles ([`reach`]).
    radius: u32,
    /// Whether a filter they ask of the tiles around reads where cities work (`worked`).
    worked: bool,
    empty: RefCell<Buildable>,
}

impl BuildableCaches {
    /// The caches of `st`, cold.
    #[must_use]
    pub fn new(rules: &Ruleset, st: &State) -> Self {
        let mut cities = LookupMap::with_capacity(st.cities().len());
        for c in st.cities().iter() {
            cities.insert(c.id(), CityMemo::default());
        }
        let mut civs = LookupMap::with_capacity(st.players().len());
        for p in st.players().ids() {
            civs.insert(p, CivMemo::default());
        }
        let (radius, worked) = reach(rules);
        Self {
            cities,
            civs,
            requirements: requirements(rules),
            radius,
            worked,
            empty: RefCell::new(Buildable::default()),
        }
    }

    /// Keeps one list per city of the state as cities come and go.
    pub(crate) fn track(&mut self, ch: &Change) {
        match *ch {
            Change::CityAdded(c) => {
                self.cities.get_or_insert_with(c, CityMemo::default);
            }
            Change::CityRemoved { c, .. } => {
                self.cities.remove(&c);
            }
            _ => {}
        }
    }
}

/// Whether a conditional reads the context it is asked in (a city, a unit, a tile, a fight), and
/// so is asked per city rather than once per civilization.
pub(crate) const fn is_local(c: &Cond) -> bool {
    c.deps.intersects(CondDeps::LOCAL)
}

/// The requirements of the ruleset's buildings, units and unit types with a civilization-wide
/// conditional, in id order.
fn requirements(rules: &Ruleset) -> Vec<UniqueId> {
    let t = rules.uniques();
    let sources = rules
        .buildings()
        .iter()
        .map(|(_, b)| &b.uniques)
        .chain(rules.base_units().iter().map(|(_, u)| &u.uniques))
        .chain(rules.unit_types().iter().map(|(_, u)| &u.uniques));
    let mut out: Vec<UniqueId> = sources
        .flat_map(crate::unique::SourceUniques::ids)
        .filter(|&id| {
            matches!(
                t.meta(id).ty,
                Some(UniqueType::OnlyAvailable | UniqueType::CanOnlyBeBuiltWhen)
            ) && t.conds(t.get(id)).iter().any(|c| !is_local(c))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// How far from a city the rejection rules read the tiles' own facts (terrain, resource,
/// improvement, owner), and whether they read where cities work.
///
/// A ship needs its city by the coast: the neighbours. `Must be on [f]` and `Must not be on [f]`
/// ask the city's tile, `Must be next to [f]` its neighbours, `Must have an owned [f] within [n]
/// tiles` the tiles within `n`; and a filter that reads the tiles beside the one it is asked of
/// (`Fresh water`, `Coastal`) reaches one further. Its other leaves read the tile's owner (within
/// the radius too) or classes its unique records.
fn reach(rules: &Ruleset) -> (u32, bool) {
    let t = rules.uniques();
    let filters = t.filters();
    let mut radius = 1;
    let mut worked = false;
    for (_, u) in t.iter() {
        let (f, base, terrain) = match u.data {
            UniqueData::MustBeOn(x) => (x.tiles, 0, true),
            UniqueData::MustNotBeOn(x) => (x.tiles, 0, true),
            UniqueData::MustBeNextTo(x) => (x.tiles, 1, false),
            UniqueData::MustHaveOwnedWithinTiles(x) => {
                (x.tiles, u32::try_from(x.radius).unwrap_or(0), false)
            }
            _ => continue,
        };
        let tf = filters.tile(f);
        let leaves = if terrain { tf.terrain.leaves() } else { tf.full.leaves() };
        let beside =
            leaves.iter().any(|l| matches!(l, TileLeaf::FreshWater | TileLeaf::NextToCoast));
        worked |= leaves.iter().any(|l| matches!(l, TileLeaf::Worked));
        radius = radius.max(base.saturating_add(u32::from(beside)));
    }
    (radius, worked)
}

/// The requirements `ids` whose civilization-wide conditionals do not all hold for `p`, each
/// conditional asked noting what it reads.
fn compute_requirements(g: &Game, p: PlayerId, ids: &[UniqueId]) -> CivRequirements {
    let t = g.rules().uniques();
    let v = g.view();
    let ctx = Ctx::civ(p);
    let failing = ids
        .iter()
        .copied()
        .filter(|&id| {
            t.conds(t.get(id)).iter().filter(|c| !is_local(c)).any(|c| {
                record::note_classes(c.deps);
                !cond::holds(c, id, &ctx, &v)
            })
        })
        .collect();
    CivRequirements { failing }
}

/// Civilization `p`'s requirements, validated against the classes their conditionals read.
fn civ_requirements(g: &Game, p: PlayerId) -> Option<Ref<'_, CivRequirements>> {
    let caches = &g.dv.buildable;
    let m = caches.civs.get(&p)?;
    let revs = &g.dv.revs;
    Some(m.reqs.get(
        revs.now(),
        || civ::cond(g, m.deps.get(), &Ctx::civ(p)),
        || {
            let (v, d) = record::recorded(|| compute_requirements(g, p, &caches.requirements));
            m.deps.set(d);
            v
        },
    ))
}

/// When civilization `p`'s requirements last changed, validated now.
fn requirements_changed(g: &Game, p: PlayerId) -> Rev {
    drop(civ_requirements(g, p));
    g.dv.buildable.civs.get(&p).map_or(Rev::START, |m| m.reqs.changed())
}

/// Whether a civilization-wide conditional of requirement `id` fails for civilization `p`: its
/// memo's answer, which a list reads rather than recording what the conditionals read.
pub(crate) fn civ_requirement_fails(g: &Game, p: PlayerId, id: UniqueId) -> bool {
    match civ_requirements(g, p) {
        Some(r) => r.failing.binary_search(&id).is_ok(),
        // A player the caches do not know: asked afresh.
        None => !compute_requirements(g, p, &[id]).failing.is_empty(),
    }
}

/// What city `c` can build, but for what moves all turn (`Buildable`).
pub(crate) fn buildable(g: &Game, c: CityId) -> Ref<'_, Buildable> {
    let caches = &g.dv.buildable;
    let Some(m) = caches.cities.get(&c) else { return caches.empty.borrow() };
    let revs = &g.dv.revs;
    let inputs = || {
        let Some(city) = g.city(c) else { return revs.now() };
        let owner = city.owner();
        let o = revs.civ(owner);
        let cr = revs.city(c);
        let mut r = cr
            .core
            .max(cr.buildings)
            .max(cr.religion)
            .max(cr.tiles)
            .max(o.index)
            .max(o.roster)
            .max(o.cities)
            .max(o.buildings)
            .max(civ::civ_index_full_changed(g, owner))
            .max(civ::city_local_full_changed(g, c))
            .max(civ::supply_changed(g, owner))
            .max(requirements_changed(g, owner))
            .max(revs.cities)
            .max(revs.wonders)
            .max(revs.religions)
            .max(revs.config);
        if caches.worked {
            r = r.max(revs.worked);
        }
        for t in g.grid().within(city.tile(), caches.radius) {
            r = r.max(revs.tile(t)).max(revs.tile_owner(t));
        }
        r.max(civ::cond(g, m.deps.get(), &Ctx::city(&g.view(), c)))
    };
    let compute = || {
        let (list, d) = record::recorded(|| construction::compute_buildable(g, c));
        m.deps.set(d);
        list
    };
    m.list.get(revs.now(), inputs, compute)
}

/// Every city's list and every civilization's requirements, validated, against a cold rebuild
/// from the same state: one line for each that disagrees (the cache oracle, DESIGN.md 9.4).
#[must_use]
pub fn verify(g: &Game) -> Vec<String> {
    let cold = Game::assemble(g.rules, g.st.clone(), Chronicle::new(), false);
    let mut out = Vec::new();
    for p in g.st.players().ids() {
        let (warm, fresh) = (civ_requirements(g, p), civ_requirements(&cold, p));
        if warm.as_deref() != fresh.as_deref() {
            out.push(format!(
                "player {}: its building requirements differ from a cold rebuild",
                p.0
            ));
        }
    }
    for city in g.st.cities().iter() {
        let c = city.id();
        if !g.dv.buildable.cities.contains_key(&c) {
            out.push(format!("city {}: no buildable list", c.get()));
            continue;
        }
        if *buildable(g, c) != *buildable(&cold, c) {
            out.push(format!("city {}: its buildable list differs from a cold rebuild", c.get()));
        }
    }
    out
}

#[cfg(all(test, feature = "embedded-ruleset", feature = "stats"))]
mod tests {
    use super::*;
    use crate::base::ids::{BaseUnitId, BuildingId, TileIdx};
    use crate::game::core::testing;
    use crate::game::derive::rev::{CityTouch, PlayerTouch};
    use crate::state::cities::Constructible;

    /// How often each city's list has recomputed, every list read first.
    fn recomputes(g: &Game) -> Vec<(CityId, u64)> {
        let ids: Vec<CityId> = g.st.cities().iter().map(crate::state::cities::City::id).collect();
        ids.into_iter()
            .map(|c| {
                drop(buildable(g, c));
                let m = g.dv.buildable.cities.get(&c).expect("a list");
                (c, m.list.stamp().counts().2)
            })
            .collect()
    }

    /// The cities whose lists recomputed between two readings.
    fn moved(before: &[(CityId, u64)], after: &[(CityId, u64)]) -> Vec<CityId> {
        before.iter().zip(after).filter(|(a, b)| a.1 != b.1).map(|(a, _)| a.0).collect()
    }

    #[test]
    fn a_sibling_that_heals_grows_or_queues_and_citizens_that_move_recompute_no_list() {
        let mut g = testing::duel();
        let rome = PlayerId(0);
        let roma = testing::city(&mut g, rome, TileIdx(22), "Roma");
        let antium = testing::city(&mut g, rome, TileIdx(26), "Antium");
        let _athens = testing::city(&mut g, PlayerId(1), TileIdx(57), "Athens");
        g.settle();
        let warrior = g.rules.lookup::<BaseUnitId>("Warrior").expect("a unit");
        let sibling_changes = |g: &mut Game| {
            if let Some(x) = g.city_mut(antium, CityTouch::CORE) {
                x.health -= 10;
                x.pop += 1;
                x.queue.push(Constructible::Unit(warrior));
            }
        };
        let before = recomputes(&g);
        sibling_changes(&mut g);
        let _touched = g.city_mut(roma, CityTouch::WORK).is_some();
        g.settle();
        assert_eq!(moved(&before, &recomputes(&g)), [antium], "only the city that changed");
        assert!(verify(&g).is_empty(), "{:?}", verify(&g));
        // Rome can build the National Epic but for its Monuments: whether each of its cities has
        // one, and which are puppets, is asked once for Rome rather than by every list.
        let techs: Vec<_> = g.rules.techs().ids().collect();
        if let Some(x) = g.player_mut(rome, PlayerTouch::INDEX) {
            for t in techs {
                x.tech.known.insert(t);
            }
        }
        g.settle();
        let epic = g.rules.lookup::<BuildingId>("National Epic").expect("a building");
        let monument = g.rules.lookup::<BuildingId>("Monument").expect("a building");
        assert!(!buildable(&g, roma).wonders.contains(epic));
        let before = recomputes(&g);
        sibling_changes(&mut g);
        g.settle();
        assert_eq!(moved(&before, &recomputes(&g)), [antium], "only the city that changed");
        assert!(verify(&g).is_empty(), "{:?}", verify(&g));
        // Roma has a Monument and Antium becomes a puppet: every city that counts has one now,
        // and Rome's lists move with the answer.
        if let Some(x) = g.city_mut(roma, CityTouch::BUILDINGS) {
            x.buildings.insert(monument);
        }
        g.settle();
        let before = recomputes(&g);
        if let Some(x) = g.city_mut(antium, CityTouch::CORE) {
            x.puppet = true;
        }
        g.settle();
        assert_eq!(moved(&before, &recomputes(&g)), [roma, antium]);
        assert!(buildable(&g, roma).wonders.contains(epic));
        assert!(verify(&g).is_empty(), "{:?}", verify(&g));
        assert!(g.take_violations().is_empty());
    }
}

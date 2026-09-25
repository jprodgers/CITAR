//! The unique indexes of civilizations, cities, religions and units, and the resource supply
//! between a civilization's index and its resource layer (DESIGN.md 5.12, 6.5, 6.6).
//!
//! Ports the composition of `economy.civ_umaps_no_resources`, `civ_umaps`, `civ_index` and
//! `resource_umap` (`economy.py:77-147, 323-336`), `city_states.bonus_umaps`
//! (`city_states.py:160-173`), `religion.founder_umap` and `follower_umap`
//! (`religion.py:59-82`), `cities.local_umaps` (`cities.py:48-66`) and `units.unit_umap`
//! (`units.py:21-33`). Python rebuilt each whenever any write cleared its cache (`civ_index`
//! 49,582 times in 100 turns); here each is a memo, rebuilt when an input's revision moves, with
//! an early cutoff when the result is equal:
//! - `CivIndex` (per civilization): every unique its nation, buildings, policies, techs,
//!   temporary uniques, era, city-state bonuses, founder beliefs and the global uniques give it,
//!   gathered as [`CivSources`] ([`sources`]); valid while the civilization's `index` revision
//!   stands;
//! - `ResourceSupply` (per civilization): its resources, line by line
//!   (`game::economy::compute_supply`), computed from `CivIndex` alone (DESIGN.md 6.6);
//! - `CivIndexFull` (per civilization): `CivIndex` with the resource layer, the uniques of the
//!   resources the supply has some of ([`Csr::merged`]);
//! - the era (per civilization, `research.player_era`), which era conditionals read once per
//!   unique, and the tiles it owns (`economy.owned_tiles`), which route upkeep walks;
//! - `CityLocal` (per city): its buildings' uniques that hold in it alone. `CityLocalFull` adds
//!   those of the resources its improved tiles give, of the ones its owner's supply has some of
//!   (the Marble decision, DESIGN.md 5.12); the supply's own view reads `CityLocal`, so the
//!   supply never depends on a resource's uniques;
//! - the follower index of a religion and the index of a unit's profile, which are pure
//!   functions of their keys (the follower beliefs; the base unit and its promotions): tables
//!   that only grow and are never iterated, shared out as `Arc`s so that a lookup may add to
//!   the table while other indexes of it are read.
//!
//! [`cond`] is what a memo that evaluates uniques validates against: the revisions the
//! conditionals' classes map to (`Revs::cond`), and for `RESOURCES` the supply memo's own stamp.
//! [`verify`] is the cache oracle for all of them.

use core::cell::{Ref, RefCell};
use core::hash::Hash;
use smallvec::SmallVec;
use std::sync::Arc;

use super::rev::{CopyMemo, Memo, Rev};
use crate::base::collections::LookupMap;
use crate::base::ids::{
    BaseUnitId, BuildingId, CityId, EraId, Id, NationId, PlayerId, ReligionId, TileIdx, UnitId,
};
use crate::base::sets::{BeliefSet, BuildingSet, PlayerVec, PromotionSet, ResourceSet};
use crate::game::Game;
use crate::game::economy::{self, ResourceSupply};
use crate::game::research;
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::change::Change;
use crate::state::chronicle::Chronicle;
use crate::unique::index::{self, CityStateBonus};
use crate::unique::{CivIndex, CivSources, CondDeps, Csr, Ctx, IndexRef, UniqueType};

/// The memos of one civilization.
#[derive(Clone, Debug, Default)]
struct CivMemos {
    /// `CivIndex`: without the resource layer.
    index: Memo<Csr>,
    /// `ResourceSupply`.
    supply: Memo<ResourceSupply>,
    /// `CivIndexFull`: with it.
    full: Memo<Csr>,
    /// Its era; `None` only before the first read.
    era: CopyMemo<Option<EraId>>,
    /// The tiles it owns, in map order.
    owned: Memo<Vec<TileIdx>>,
}

/// The memos of one city.
#[derive(Clone, Debug, Default)]
struct CityMemos {
    /// `CityLocal`: its buildings' uniques that hold in it alone.
    local: Memo<Csr>,
    /// `CityLocalFull`: `CityLocal` and the uniques that hold in it alone of the resources its
    /// tiles give its owner and its owner's supply has some of.
    full: Memo<Csr>,
}

/// A table of indexes that are pure functions of their keys: it only grows, is never iterated,
/// and lends its indexes out shared, so a lookup that adds a key never meets a borrow of another.
#[derive(Clone, Debug)]
struct Shared<K> {
    map: RefCell<LookupMap<K, Arc<Csr>>>,
}

impl<K: Eq + Hash> Default for Shared<K> {
    fn default() -> Self {
        Self { map: RefCell::new(LookupMap::new()) }
    }
}

impl<K: Eq + Hash> Shared<K> {
    /// The index of `key`, built by `build` the first time it is asked for.
    fn get(&self, key: K, build: impl FnOnce() -> Csr) -> Arc<Csr> {
        if let Some(c) = self.map.borrow().get(&key) {
            return Arc::clone(c);
        }
        let c = Arc::new(build());
        self.map.borrow_mut().insert(key, Arc::clone(&c));
        c
    }

    /// How many keys it has seen.
    fn len(&self) -> usize {
        self.map.borrow().len()
    }
}

/// Every cache of this module: part of `Derived`.
#[derive(Clone, Debug)]
pub struct CivCaches {
    civs: PlayerVec<CivMemos>,
    /// `CityLocal` and `CityLocalFull`, for each city of the state: added when a city is, and
    /// dropped with it.
    cities: LookupMap<CityId, CityMemos>,
    profiles: Shared<(BaseUnitId, PromotionSet)>,
    followers: Shared<BeliefSet>,
    /// What the conditionals of the uniques the supply evaluates read, but resources: while the
    /// supply is computed resources read as none (DESIGN.md 6.6).
    supply_deps: CondDeps,
    /// What an index read of a player, city, religion or unit the game does not have lends.
    empty: Csr,
}

impl CivCaches {
    /// The caches of `st`, cold.
    #[must_use]
    pub fn new(rules: &Ruleset, st: &State) -> Self {
        let mut cities = LookupMap::with_capacity(st.cities().len());
        for c in st.cities().iter() {
            cities.insert(c.id(), CityMemos::default());
        }
        Self {
            civs: st.players().ids().map(|_| CivMemos::default()).collect(),
            cities,
            profiles: Shared::default(),
            followers: Shared::default(),
            supply_deps: supply_deps(rules),
            empty: Csr::default(),
        }
    }

    /// Keeps one `CityLocal` memo per city of the state as cities come and go. Called with each
    /// change, under `&mut Game`, so no read of a memo is outstanding.
    pub(crate) fn track(&mut self, ch: &Change) {
        match *ch {
            Change::CityAdded(c) => {
                self.cities.get_or_insert_with(c, CityMemos::default);
            }
            Change::CityRemoved { c, .. } => {
                self.cities.remove(&c);
            }
            _ => {}
        }
    }

    /// How many unit profiles and follower indexes the tables hold (feature `stats` and tests).
    #[must_use]
    pub fn table_sizes(&self) -> (usize, usize) {
        (self.profiles.len(), self.followers.len())
    }
}

/// The union of what the conditionals of the uniques the supply evaluates read: the types
/// `economy::compute_supply` asks for with conditionals (`ConsumesResources` and the extra
/// luxury flag it reads without).
fn supply_deps(rules: &Ruleset) -> CondDeps {
    let types = [
        UniqueType::PercentResourceProduction,
        UniqueType::ProvidesResources,
        UniqueType::CityStateResources,
    ];
    let t = rules.uniques();
    t.iter()
        .filter(|&(id, _)| t.meta(id).ty.is_some_and(|ty| types.contains(&ty)))
        .fold(CondDeps::empty(), |d, (_, u)| d | u.deps())
        .difference(CondDeps::RESOURCES)
}

// ---- Gathering the sources (economy.py:91-129) -------------------------------------------------

/// Everything that gives civilization `p` uniques, but resources (`economy.civ_umaps_no_resources`,
/// `economy.py:91-129`): its nation; the buildings of its cities, counted; its policies, techs and
/// temporary uniques; its era; for a major, each living city-state it has met that counts it a
/// friend or is allied with it (`city_states.bonus_umaps`, `city_states.py:160-173`); and its
/// religion's founder beliefs.
#[must_use]
pub fn sources(g: &Game, p: PlayerId) -> CivSources {
    let Some(pl) = g.player(p) else { return CivSources::new(NationId(0), EraId(0)) };
    let r = g.rules;
    let mut src = CivSources::new(pl.nation, era(g, p));
    let mut counts: Vec<u16> = vec![0; r.buildings().len()];
    for city in g.player_cities(p) {
        for b in city.buildings.iter() {
            if let Some(n) = counts.get_mut(b.index()) {
                *n = n.saturating_add(1);
            }
        }
    }
    src.buildings =
        r.buildings().ids().zip(counts).filter(|&(_, n)| n > 0).collect::<Vec<(BuildingId, u16)>>();
    src.policies = pl.policy.adopted;
    src.techs = pl.tech.known;
    src.temporary = pl.civ.temp_uniques.iter().map(|t| t.unique).collect();
    if pl.is_major() {
        for q in g.city_states(true) {
            let qid = q.id();
            let Some(data) = q.city_state.as_deref() else { continue };
            let Some(cs_type) = data.cs_type else { continue };
            if !g.has_met(p, qid) {
                continue;
            }
            if data.ally() == Some(p) {
                src.city_states.push((cs_type, CityStateBonus::Ally));
            } else if g.is_friend_level(qid, p) {
                src.city_states.push((cs_type, CityStateBonus::Friend));
            }
        }
    }
    if let Some(rel) = pl.religion.founded.and_then(|x| g.state().world().religion(x)) {
        src.founder_beliefs = rel.founder_beliefs.iter().collect();
    }
    src
}

// ---- The memos ---------------------------------------------------------------------------------

/// Civilization `p`'s index without its resource layer (`CivIndex`).
pub(crate) fn civ_index(g: &Game, p: PlayerId) -> IndexRef<'_> {
    let caches = &g.dv.civ;
    let Some(m) = caches.civs.get(p) else { return IndexRef::Plain(&caches.empty) };
    let revs = &g.dv.revs;
    IndexRef::Memo(m.index.get(
        revs.now(),
        || revs.civ(p).index,
        || CivIndex::build(g.rules, &sources(g, p)),
    ))
}

/// Civilization `p`'s index with its resource layer (`CivIndexFull`): `CivIndex` and the
/// uniques of the resources its supply has some of.
pub(crate) fn civ_index_full(g: &Game, p: PlayerId) -> IndexRef<'_> {
    let caches = &g.dv.civ;
    let Some(m) = caches.civs.get(p) else { return IndexRef::Plain(&caches.empty) };
    let inputs = || {
        drop(civ_index(g, p));
        let base = m.index.changed();
        drop(supply(g, p));
        base.max(m.supply.changed())
    };
    let compute = || {
        let base = civ_index(g, p);
        let layer = supply(g, p).map(|s| s.positive()).unwrap_or_default();
        base.merged(&index::resource_layer(g.rules, &layer))
    };
    IndexRef::Memo(m.full.get(g.dv.revs.now(), inputs, compute))
}

/// When civilization `p`'s index with its resource layer last changed, validated now.
pub(crate) fn civ_index_full_changed(g: &Game, p: PlayerId) -> Rev {
    drop(civ_index_full(g, p));
    g.dv.civ.civs.get(p).map_or(Rev::START, |m| m.full.changed())
}

/// Civilization `p`'s resources (`ResourceSupply`); `None` for a player the game does not have.
pub(crate) fn supply(g: &Game, p: PlayerId) -> Option<Ref<'_, ResourceSupply>> {
    let m = g.dv.civ.civs.get(p)?;
    let verified = m.supply.stamp().verified();
    Some(m.supply.get(
        g.dv.revs.now(),
        || supply_inputs(g, p, verified),
        || economy::compute_supply(g, p),
    ))
}

/// When civilization `p`'s supply last changed, validated now.
pub(crate) fn supply_changed(g: &Game, p: PlayerId) -> Rev {
    drop(supply(g, p));
    g.dv.civ.civs.get(p).map_or(Rev::START, |m| m.supply.changed())
}

/// The era civilization `p` is in (`research.player_era`, `research.py:254-275`), from its
/// techs: valid while its `index` revision stands, which every change of its techs moves. The
/// era conditionals read it once per unique they are asked about, so it is kept rather than
/// found in the tech tree each time, as Python kept it per count of techs (`research.py:257`).
pub(crate) fn era(g: &Game, p: PlayerId) -> EraId {
    let Some(m) = g.dv.civ.civs.get(p) else { return EraId(0) };
    let revs = &g.dv.revs;
    m.era
        .get(
            revs.now(),
            || revs.civ(p).index,
            || g.player(p).map(|x| research::player_era(g.rules, &x.tech.known)),
        )
        .unwrap_or(EraId(0))
}

/// The tiles civilization `p` owns, in map order (`economy.owned_tiles`, `economy.py:186-193`):
/// valid while its `cities` revision stands, which a tile changing hands moves for both sides.
/// `None` for a player the game does not have.
pub(crate) fn owned_tiles(g: &Game, p: PlayerId) -> Option<Ref<'_, Vec<TileIdx>>> {
    let m = g.dv.civ.civs.get(p)?;
    let revs = &g.dv.revs;
    Some(m.owned.get(
        revs.now(),
        || revs.civ(p).cities,
        || g.st.tiles().iter().filter(|(_, t)| t.owner() == Some(p)).map(|(i, _)| i).collect(),
    ))
}

/// The latest revision of what civilization `p`'s supply is computed from, for a memo last
/// verified at `verified`: its index's inputs (its techs among them, which reveal resources and
/// allow improvements), the units it has, its cities, their buildings, religions and the tiles
/// they own, for a major its allied city-states' the same, deals and the turn (a deal's end),
/// and what the conditionals of the uniques it evaluates read. A changed tile counts if it
/// belongs to one of them now; one that changed hands moved its old and new owners' `cities`.
/// None of it moves when a unit moves, or when a city grows or works another tile.
fn supply_inputs(g: &Game, p: PlayerId, verified: Rev) -> Rev {
    let revs = &g.dv.revs;
    let st = &g.st;
    let owners = economy::supply_owners(g, p);
    let c = revs.civ(p);
    // `cities` moves when a city-state dies, which no longer lists it among the allies.
    let mut r = c
        .roster
        .max(revs.turn)
        .max(revs.diplo)
        .max(revs.alliances)
        .max(revs.cities)
        .max(revs.religions);
    let deps = g.dv.civ.supply_deps;
    // The civilization-level classes read the same in every city of one owner: only the local
    // ones are asked city by city.
    let local = deps.intersection(CondDeps::LOCAL);
    for q in owners.iter() {
        let x = revs.civ(q);
        r = r.max(x.index).max(x.cities).max(x.buildings).max(x.city_state);
        if !deps.is_empty() {
            r = r.max(revs.cond(st, deps, &Ctx::civ(q)));
        }
        for city in g.player_cities(q) {
            // Its majority religion's follower beliefs are among its own uniques.
            r = r.max(revs.city(city.id()).religion);
            if !local.is_empty() {
                let ctx = Ctx {
                    civ: Some(q),
                    city: Some(city.id()),
                    tile: Some(city.tile()),
                    ..Ctx::default()
                };
                r = r.max(revs.cond(st, local, &ctx));
            }
        }
    }
    let Some(changed) = revs.tile_log.since(verified) else { return revs.now() };
    for t in changed {
        if st
            .tiles()
            .get(t)
            .and_then(crate::state::map::Tile::owner)
            .is_some_and(|o| owners.contains(o))
        {
            r = r.max(revs.tile(t));
        }
    }
    r
}

/// City `c`'s own index (`CityLocal`, `cities.local_umaps` without the religion,
/// `cities.py:48-66`): its buildings' uniques that hold in it alone. What the supply's view
/// reads.
pub(crate) fn city_local(g: &Game, c: CityId) -> IndexRef<'_> {
    let caches = &g.dv.civ;
    let (Some(m), Some(city)) = (caches.cities.get(&c), g.st.cities().get(c)) else {
        return IndexRef::Plain(&caches.empty);
    };
    let revs = &g.dv.revs;
    IndexRef::Memo(m.local.get(
        revs.now(),
        || revs.city(c).buildings,
        || index::city_local(g.rules, &city.buildings, &ResourceSet::new()),
    ))
}

/// City `c`'s own index with its resources (`CityLocalFull`): `CityLocal`, and the uniques that
/// hold in it alone of the resources its improved tiles give its owner, of those its owner's
/// supply has some of (the Marble decision, DESIGN.md 5.12). A resource traded away, or all used
/// up, gives its uniques nowhere, as Python's resource layer held only what the supply had. The
/// turns a city's list shows for a wonder follow from it.
// refcheck: marble-bonus-in-its-own-city
// refcheck: marble-bonus-in-its-own-city-wonder-turns
pub(crate) fn city_local_full(g: &Game, c: CityId) -> IndexRef<'_> {
    let caches = &g.dv.civ;
    let (Some(m), Some(city)) = (caches.cities.get(&c), g.st.cities().get(c)) else {
        return IndexRef::Plain(&caches.empty);
    };
    let revs = &g.dv.revs;
    let owner = city.owner();
    let verified = m.full.stamp().verified();
    // The tiles a city gives resources from: its territory (a tile changing hands moves both
    // owners' `cities`), what is on them and whether its owner may use it (its techs, in its
    // `index`), and whether a city stands on one.
    let inputs = || {
        drop(city_local(g, c));
        let o = revs.civ(owner);
        let r = m
            .local
            .changed()
            .max(supply_changed(g, owner))
            .max(o.index)
            .max(o.cities)
            .max(revs.cities);
        let Some(changed) = revs.tile_log.since(verified) else { return revs.now() };
        changed
            .filter(|&t| g.st.tiles().get(t).and_then(crate::state::map::Tile::city) == Some(c))
            .fold(r, |r, t| r.max(revs.tile(t)))
    };
    let compute = || {
        let had = supply(g, owner).map(|s| s.positive()).unwrap_or_default();
        let given = economy::provided_resources(g, c) & had;
        city_local(g, c).merged(&index::city_local(g.rules, &BuildingSet::new(), &given))
    };
    IndexRef::Memo(m.full.get(revs.now(), inputs, compute))
}

/// When city `c`'s own index with its resources last changed, validated now.
pub(crate) fn city_local_full_changed(g: &Game, c: CityId) -> Rev {
    drop(city_local_full(g, c));
    g.dv.civ.cities.get(&c).map_or(Rev::START, |m| m.full.changed())
}

/// What religion `r` gives the cities that follow it: its follower beliefs' uniques
/// (`religion.follower_umap`, `religion.py:72-82`).
pub(crate) fn follower(g: &Game, r: ReligionId) -> IndexRef<'_> {
    let caches = &g.dv.civ;
    let Some(rel) = g.st.world().religion(r) else { return IndexRef::Plain(&caches.empty) };
    let beliefs = rel.follower_beliefs;
    IndexRef::Shared(
        caches
            .followers
            .get(beliefs, || index::follower(g.rules, &beliefs.iter().collect::<Vec<_>>())),
    )
}

/// Unit `u`'s profile: its base unit's, its unit type's and its promotions' uniques
/// (`units.unit_umap`, `units.py:21-33`).
pub(crate) fn unit_profile(g: &Game, u: UnitId) -> IndexRef<'_> {
    let caches = &g.dv.civ;
    let Some(x) = g.st.units().get(u) else { return IndexRef::Plain(&caches.empty) };
    let key = (x.base, x.promotions);
    IndexRef::Shared(caches.profiles.get(key, || index::unit_profile(g.rules, key.0, &key.1)))
}

/// The latest revision of everything the conditionals of `deps` read in `ctx` (DESIGN.md 6.3):
/// the revisions `Revs::cond` maps them to, for `RESOURCES` when the supply of the civilization
/// in context last changed, and for `CONNECTED` when the trade networks the context names last
/// changed (the memo `Connectivity`). A memo that evaluates uniques validates against this.
#[must_use]
pub fn cond(g: &Game, deps: CondDeps, ctx: &Ctx) -> Rev {
    let memos = CondDeps::RESOURCES | CondDeps::CONNECTED;
    let mut r = g.dv.revs.cond(&g.st, deps.difference(memos), ctx);
    if let Some(p) = ctx.civ
        && deps.contains(CondDeps::RESOURCES)
    {
        r = r.max(supply_changed(g, p));
    }
    if deps.contains(CondDeps::CONNECTED) {
        for p in networks(g, ctx) {
            r = r.max(super::stats::connectivity_changed(g, p));
        }
    }
    r
}

/// Whose trade networks the class `CONNECTED` reads in `ctx`: the civilization in context's, and
/// the owners' of the city in context and of the cities of the fight, whose connection a city
/// filter may ask.
fn networks(g: &Game, ctx: &Ctx) -> SmallVec<[PlayerId; 3]> {
    let mut out: SmallVec<[PlayerId; 3]> = SmallVec::new();
    let mut add = |p: PlayerId| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(p) = ctx.civ {
        add(p);
    }
    let owner = |c| g.st.cities().get(c).map(crate::state::cities::City::owner);
    if let Some(p) = ctx.city.and_then(owner) {
        add(p);
    }
    if let Some(f) = ctx.combat {
        for side in [Some(f.our), f.their].into_iter().flatten() {
            if let crate::unique::filter::Combatant::City(c) = side
                && let Some(p) = owner(c)
            {
                add(p);
            }
        }
    }
    out
}

// ---- The cache oracle (DESIGN.md 9.4) ----------------------------------------------------------

/// Every memo and table of this module, validated, against a cold rebuild from the same state:
/// one line for each that disagrees.
#[must_use]
pub fn verify(g: &Game) -> Vec<String> {
    let cold = Game::assemble(g.rules, g.st.clone(), Chronicle::new(), false);
    let mut out = Vec::new();
    for p in g.st.players().ids() {
        if *civ_index(g, p) != *civ_index(&cold, p) {
            out.push(format!("player {}: the unique index differs from a cold rebuild", p.0));
        }
        if supply(g, p).as_deref() != supply(&cold, p).as_deref() {
            out.push(format!("player {}: the resource supply differs from a cold rebuild", p.0));
        }
        if *civ_index_full(g, p) != *civ_index_full(&cold, p) {
            out.push(format!(
                "player {}: the unique index with resources differs from a cold rebuild",
                p.0
            ));
        }
        if era(g, p) != era(&cold, p) {
            out.push(format!("player {}: the era differs from a cold rebuild", p.0));
        }
        if owned_tiles(g, p).as_deref() != owned_tiles(&cold, p).as_deref() {
            out.push(format!("player {}: the tiles it owns differ from a cold rebuild", p.0));
        }
    }
    for city in g.st.cities().iter() {
        let c = city.id();
        if !g.dv.civ.cities.contains_key(&c) {
            out.push(format!("city {}: no local index memo", c.get()));
            continue;
        }
        if *city_local(g, c) != *city_local(&cold, c) {
            out.push(format!("city {}: the local index differs from a cold rebuild", c.get()));
        }
        if *city_local_full(g, c) != *city_local_full(&cold, c) {
            out.push(format!(
                "city {}: the local index with resources differs from a cold rebuild",
                c.get()
            ));
        }
    }
    for i in 0..g.st.world().religions.len() {
        let r = ReligionId(u8::try_from(i).unwrap_or(u8::MAX));
        if *follower(g, r) != *follower(&cold, r) {
            out.push(format!("religion {i}: the follower index differs from a fresh build"));
        }
    }
    for u in g.st.units().iter() {
        if *unit_profile(g, u.id()) != *unit_profile(&cold, u.id()) {
            out.push(format!("unit {}: its profile differs from a fresh build", u.id().get()));
        }
    }
    out
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;

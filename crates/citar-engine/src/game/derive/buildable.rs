//! The `Buildable` memo (DESIGN.md 6.5): what each city can build, which Python worked out item by
//! item on every read (`cities.buildable_items`, `cities.py:1347-1360`; 98 ms for 22 cities in a
//! late game).
//!
//! A city's list validates on read against what the rejection rules read of the game
//! (`cities.rejection_reasons`): the city itself (its buildings, size, status and religion, the
//! tiles of its territory, and the tiles and owners around it that `Must be next to` and `Must
//! have an owned [] within [] tiles` look at), its owner's unique index with the resource layer
//! (techs, era, policies, nation), its units and cities (limits, queues elsewhere, national
//! wonders), its resources, the world wonders built, religions, the settings, and the classes of
//! the conditionals its last computation evaluated (`unique::record`). Spaceship parts are
//! counted with the units they were, whose removal moves the owner's `roster`.
//!
//! The room a city has for aircraft reads where its owner's units stand, which moves all turn;
//! the list keeps aircraft as if there were room, and `cities::construction::buildable_items`
//! asks the hangar when it lends the list.

use core::cell::{Cell, Ref, RefCell};

use super::civ;
use super::rev::Memo;
use crate::base::collections::LookupMap;
use crate::base::ids::CityId;
use crate::game::Game;
use crate::game::cities::construction::{self, Buildable};
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::change::Change;
use crate::state::chronicle::Chronicle;
use crate::unique::{CondDeps, Ctx, UniqueData, record};

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

/// Every city's list: part of `Derived`.
#[derive(Clone, Debug)]
pub struct BuildableCaches {
    cities: LookupMap<CityId, CityMemo>,
    /// How far from a city the rejection rules look at tiles: its neighbours, and the radius of
    /// the ruleset's widest `Must have an owned [] within [] tiles`.
    radius: u32,
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
        let t = rules.uniques();
        let widest = t
            .iter()
            .filter_map(|(_, u)| match u.data {
                UniqueData::MustHaveOwnedWithinTiles(x) => u32::try_from(x.radius).ok(),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        Self { cities, radius: widest.max(1), empty: RefCell::new(Buildable::default()) }
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

/// What city `c` can build, but for the room aircraft need (`Buildable`).
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
            .max(cr.work)
            .max(o.index)
            .max(o.roster)
            .max(o.cities)
            .max(o.buildings)
            .max(civ::civ_index_full_changed(g, owner))
            .max(civ::city_local_full_changed(g, c))
            .max(civ::supply_changed(g, owner))
            .max(revs.cities)
            .max(revs.wonders)
            .max(revs.religions)
            .max(revs.config);
        // The queues of its owner's other cities: a wonder being built, a limit.
        for &x in g.state().cities().of(owner) {
            r = r.max(revs.city(x).core);
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

/// Every city's list, validated, against a cold rebuild from the same state: one line for each
/// that disagrees (the cache oracle, DESIGN.md 9.4).
#[must_use]
pub fn verify(g: &Game) -> Vec<String> {
    let cold = Game::assemble(g.rules, g.st.clone(), Chronicle::new(), false);
    let mut out = Vec::new();
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

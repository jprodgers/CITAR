//! `Derived`: every cache a game keeps beside its state (DESIGN.md 6.1, 6.5). A pure function of
//! the state: dropped and rebuilt cold it gives the same answers, which the cache oracle
//! ([`Derived::verify`]) checks.
//!
//! Package 1b-01 lands the skeleton: the revisions ([`rev::Revs`]), the grid, the name index
//! events read, the visibility counts (filled by package 1c-01), and what a write means for the
//! caches ([`Derived::on`]). The memos of DESIGN.md 6.5 join it package by package: the unique
//! index memos, resource supply and unit profiles (1b-05), tile yields, city and civilization
//! stats and connectivity (1b-06), the buildable lists (1b-07), and the rest with their systems.
//!
//! Replaces the caches of `game.py:100-145` (`_cache`, `_ycache`, `_static`, `_jobcache`,
//! `_viewcache`, `_names`) and the invalidation of `game.py:565-609`.

pub mod rev;

use core::cell::Ref;

use smallvec::SmallVec;

use self::rev::{Memo, Revs};
use super::events::NameIndex;
use super::pending::SightSource;
use super::vis::Visibility;
use crate::base::hex::HexGrid;
use crate::base::ids::{CityId, PlayerId, TileIdx};
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::change::Change;
use crate::state::map::Tile;
use crate::unique::Csr;

/// What a write means beyond the revisions it moves: which cities must look at their citizens
/// again, and which vision sources must be brought up to date. Read from the state after the
/// write; never a write itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reactions {
    /// Cities to flag for a citizen recheck.
    pub recheck: SmallVec<[CityId; 4]>,
    /// Vision sources to mark dirty.
    pub sight: SmallVec<[SightSource; 2]>,
}

/// Every cache of a game (DESIGN.md 6.1): never saved, rebuilt cold on load.
#[derive(Clone, Debug)]
pub struct Derived {
    pub(crate) revs: Revs,
    grid: HexGrid,
    names: Memo<NameIndex>,
    pub(crate) vis: Visibility,
    /// The index a unique index read hands out until package 1b-05 builds the real ones.
    empty: Csr,
}

impl Derived {
    /// The caches of `st`, cold: nothing computed until it is read.
    ///
    /// # Panics
    ///
    /// Never for a state built by `State::new` or `State::from_parts`, which refuse a map whose
    /// shape is not a valid grid.
    #[must_use]
    pub fn new(st: &State) -> Self {
        let grid = st.map().grid().unwrap_or_else(|e| {
            // State::from_parts checked the shape; reaching this is a bug in state.
            panic!("a state's map is always a valid grid: {e}")
        });
        Self {
            revs: Revs::new(st),
            grid,
            names: Memo::new(),
            vis: Visibility::new(st),
            empty: Csr::default(),
        }
    }

    /// The revisions.
    #[must_use]
    pub const fn revs(&self) -> &Revs {
        &self.revs
    }

    /// The map's grid.
    #[must_use]
    pub const fn grid(&self) -> &HexGrid {
        &self.grid
    }

    /// What each civilization sees.
    #[must_use]
    pub const fn vis(&self) -> &Visibility {
        &self.vis
    }

    /// The name index of `st` (`Game._name_index`, `game.py:814-840`), rebuilt only when a name
    /// or a city's owner changed.
    pub fn names<'a>(&'a self, st: &State) -> Ref<'a, NameIndex> {
        let revs = &self.revs;
        self.names.get(revs.now(), || revs.names.max(revs.cities), || NameIndex::build(st))
    }

    /// The empty index, which the unique index reads hand out until package 1b-05.
    pub(crate) const fn empty_index(&self) -> &Csr {
        &self.empty
    }

    /// What a change means for the caches beyond its revisions: the cities that must recheck
    /// their citizens (DESIGN.md 6.7) and the vision sources to update (DESIGN.md 6.9). It reads
    /// `st`, the state after the write, and never writes.
    ///
    /// A city may work a tile of its owner within its work range that no city stands on, that
    /// the city it belongs to does not work, and that no enemy military unit blocks
    /// (`cities.workable_tiles`, `cities.py:170-193`). So a change to a tile concerns the cities
    /// of the tile's owner, before and after, in range of it: its yield, its owner, a city on it,
    /// or an enemy unit on it. A seat concerns all its player's cities, and war or peace all the
    /// cities of both sides.
    #[must_use]
    pub fn on(&self, st: &State, rules: &Ruleset, ch: &Change) -> Reactions {
        let mut out = Reactions::default();
        let range = u32::try_from(rules.constants().formulas.city_work_range).unwrap_or(0);
        let owner = |t: TileIdx| st.tiles().get(t).and_then(Tile::owner);
        match *ch {
            Change::TileInput(t) => self.flag_near(st, range, t, &[owner(t)], &mut out),
            Change::TileHeight(t) => {
                self.flag_near(st, range, t, &[owner(t)], &mut out);
                out.sight.push(SightSource::Area(t));
            }
            Change::TileOwner { t, old, new } => {
                out.recheck.extend(old.city);
                out.recheck.extend(new.city);
                self.flag_near(st, range, t, &[old.owner, new.owner], &mut out);
                out.sight.push(SightSource::Tile(t));
            }
            Change::UnitPlaced { u, owner: by, from, to } => {
                out.sight.push(SightSource::Unit(u));
                let military = st
                    .units()
                    .get(u)
                    .is_some_and(|x| rules.base_units().get(x.base).is_some_and(|b| b.military));
                if military {
                    for t in [from, Some(to)].into_iter().flatten() {
                        self.flag_blockade(st, range, by, t, &mut out);
                    }
                }
            }
            Change::UnitOwner { u, old, new } => {
                out.sight.push(SightSource::Unit(u));
                if let Some(t) = st.units().get(u).map(crate::state::units::Unit::tile) {
                    for by in [old, new] {
                        self.flag_blockade(st, range, by, t, &mut out);
                    }
                }
            }
            Change::UnitRemoved { u, owner: by, at } => {
                out.sight.push(SightSource::Unit(u));
                self.flag_blockade(st, range, by, at, &mut out);
            }
            Change::CityAdded(c) => {
                out.recheck.push(c);
                if let Some(x) = st.cities().get(c) {
                    self.flag_near(st, range, x.tile(), &[Some(x.owner())], &mut out);
                }
                out.sight.push(SightSource::City(c));
            }
            Change::CityRemoved { c, owner: was, at } => {
                self.flag_near(st, range, at, &[Some(was), owner(at)], &mut out);
                out.sight.push(SightSource::City(c));
            }
            Change::CityOwner { c, old, new } => {
                out.recheck.push(c);
                if let Some(x) = st.cities().get(c) {
                    self.flag_near(st, range, x.tile(), &[Some(old), Some(new)], &mut out);
                }
                out.sight.push(SightSource::City(c));
            }
            Change::CityTiles(c) => {
                out.recheck.push(c);
                if let Some(x) = st.cities().get(c) {
                    self.flag_near(st, range, x.tile(), &[Some(x.owner())], &mut out);
                }
                out.sight.push(SightSource::City(c));
            }
            Change::Diplo { a, b } => {
                // War and peace decide which units blockade whose tiles.
                for p in [a, b] {
                    out.recheck.extend(st.cities().of(p).iter().copied());
                }
            }
            Change::Met { .. } | Change::Turn | Change::Names => {}
            Change::Alliance { cs, .. } => out.sight.push(SightSource::Allies(cs)),
            Change::Spy(p) => out.sight.push(SightSource::Spies(p)),
            Change::Seat(p) => out.recheck.extend(st.cities().of(p).iter().copied()),
            Change::PlayerAlive(p) => out.sight.push(SightSource::Civ(p)),
        }
        out
    }

    /// Flags every city of one of `owners` whose work range reaches tile `t`.
    fn flag_near(
        &self,
        st: &State,
        range: u32,
        t: TileIdx,
        owners: &[Option<PlayerId>],
        out: &mut Reactions,
    ) {
        if owners.iter().all(Option::is_none) || !self.grid.contains(t) {
            return;
        }
        for n in self.grid.within(t, range) {
            if let Some(c) = st.city_at(n)
                && st.cities().get(c).is_some_and(|x| owners.contains(&Some(x.owner())))
                && !out.recheck.contains(&c)
            {
                out.recheck.push(c);
            }
        }
    }

    /// Flags the cities that may work tile `t` if a military unit of `by` there blocks it: the
    /// tile's owner's, when at war with `by` (the blockade, `cities.py:185-190`).
    fn flag_blockade(&self, st: &State, range: u32, by: PlayerId, t: TileIdx, out: &mut Reactions) {
        let Some(owner) = st.tiles().get(t).and_then(Tile::owner) else { return };
        if owner != by && st.diplo().at_war(owner, by) {
            self.flag_near(st, range, t, &[Some(owner)], out);
        }
    }

    /// The cache oracle (DESIGN.md 9.4): every memo, validated, against a cold recompute from
    /// the same state. Returns what disagrees, one line each.
    #[must_use]
    pub fn verify(&self, st: &State) -> Vec<String> {
        let cold = Self::new(st);
        let mut out = Vec::new();
        if self.names(st).entries() != cold.names(st).entries() {
            out.push("the event name index differs from a cold rebuild".to_owned());
        }
        if self.grid != cold.grid {
            out.push("the grid differs from the map's".to_owned());
        }
        out.extend(self.vis.verify(st));
        out
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::game::core::testing;

    #[test]
    fn the_name_index_is_rebuilt_only_when_a_name_changes() {
        let mut g = testing::duel();
        let first = g.dv.names(&g.st).entries().len();
        let verified = g.dv.names.stamp().verified();
        // A change that touches no name leaves the index as it was.
        g.dv.revs.on_change(&g.st, &Change::Turn);
        let _read = g.dv.names(&g.st).entries().len();
        assert_eq!(g.dv.names.stamp().changed(), verified, "no name moved");
        assert!(g.dv.names.stamp().verified() > verified);
        assert_eq!(first, 5, "two civilizations, their leaders and a city-state");
    }

    #[test]
    fn a_tile_change_flags_the_cities_of_its_owner_in_range() {
        use crate::state::TileClaim;
        let mut g = testing::duel();
        let (rome, greece) = (PlayerId(0), PlayerId(1));
        // On the 10-wide map, tile 22 is (2, 2), 25 is (5, 2) and 28 is (8, 2).
        let roma = testing::city(&mut g, rome, TileIdx(22), "Roma");
        let antium = testing::city(&mut g, rome, TileIdx(25), "Antium");
        let athens = testing::city(&mut g, greece, TileIdx(28), "Athens");
        g.settle();
        let t = TileIdx(24);
        g.set_tile_owner(t, TileClaim::city(rome, roma)).expect("a tile");
        let r = g.dv.on(&g.st, g.rules, &Change::TileInput(t));
        let mut flagged = r.recheck.to_vec();
        flagged.sort();
        assert_eq!(flagged, [roma, antium], "Rome's cities in range, not Athens");
        // An enemy's military unit there blocks it for Rome's cities; a friend's does not.
        let warrior = testing::unit(&mut g, greece, "Warrior", t);
        let placed = Change::UnitPlaced { u: warrior, owner: greece, from: None, to: t };
        assert!(g.dv.on(&g.st, g.rules, &placed).recheck.is_empty());
        g.update_relation(rome, greece, |x| x.war = true).expect("a pair");
        let mut flagged = g.dv.on(&g.st, g.rules, &placed).recheck.to_vec();
        flagged.sort();
        assert_eq!(flagged, [roma, antium]);
        assert!(!flagged.contains(&athens));
    }
}

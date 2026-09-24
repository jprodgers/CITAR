//! `Derived`: every cache a game keeps beside its state (DESIGN.md 6.1, 6.5). A pure function of
//! the state: dropped and rebuilt cold it gives the same answers, which the cache oracle
//! ([`Derived::verify`]) checks.
//!
//! Package 1b-01 lands the skeleton: the revisions ([`rev::Revs`]), the grid, the name index
//! events read, the visibility counts (`game::vis`, package 1c-01), and what a write means for the
//! caches ([`Derived::on`]). The memos of DESIGN.md 6.5 join it package by package: the unique
//! index memos, resource supply and unit profiles ([`civ`], 1b-05), tile yields, city and
//! civilization stats, happiness and connectivity ([`stats`], 1b-06), the buildable lists
//! ([`buildable`], 1b-07), and the rest with their systems.
//!
//! Replaces the caches of `game.py:100-145` (`_cache`, `_ycache`, `_static`, `_jobcache`,
//! `_viewcache`, `_names`) and the invalidation of `game.py:565-609`.

pub mod buildable;
pub mod civ;
pub mod rev;
pub mod stats;

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
    /// The unique index memos, the resource supply and the unit profiles.
    pub(crate) civ: civ::CivCaches,
    /// Tile yields, city and civilization stats, happiness and connectivity.
    pub(crate) stats: stats::StatsCaches,
    /// What each city can build.
    pub(crate) buildable: buildable::BuildableCaches,
}

impl Derived {
    /// The caches of `st`, cold: nothing computed until it is read.
    ///
    /// # Panics
    ///
    /// Never for a state built by `State::new` or `State::from_parts`, which refuse a map whose
    /// shape is not a valid grid.
    #[must_use]
    pub fn new(rules: &Ruleset, st: &State) -> Self {
        let grid = st.map().grid().unwrap_or_else(|e| {
            // State::from_parts checked the shape; reaching this is a bug in state.
            panic!("a state's map is always a valid grid: {e}")
        });
        Self {
            revs: Revs::new(st),
            grid,
            names: Memo::new(),
            vis: Visibility::new(rules, st),
            civ: civ::CivCaches::new(rules, st),
            stats: stats::StatsCaches::new(rules, st),
            buildable: buildable::BuildableCaches::new(rules, st),
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

    /// Reads tile `t`'s heights for sight again after its terrains changed, and forgets the
    /// line-of-sight answers that read them.
    pub(crate) fn height_changed(&mut self, rules: &Ruleset, st: &State, t: TileIdx) {
        if let Some(tile) = st.tiles().get(t) {
            self.vis.height_changed(rules, &self.grid, t, tile);
        }
    }

    /// The name index of `st` (`Game._name_index`, `game.py:814-840`), rebuilt only when a name
    /// or a city's owner changed.
    pub fn names<'a>(&'a self, st: &State) -> Ref<'a, NameIndex> {
        let revs = &self.revs;
        self.names.get(revs.now(), || revs.names.max(revs.cities), || NameIndex::build(st))
    }

    /// The unique index memos, the resource supply and the unit profiles.
    #[must_use]
    pub const fn civ(&self) -> &civ::CivCaches {
        &self.civ
    }

    /// What a change means for the caches beyond its revisions: the cities that must recheck
    /// their citizens (DESIGN.md 6.7) and the vision sources to update (DESIGN.md 6.9). It reads
    /// `st`, the state after the write, and never writes.
    ///
    /// A city may work a tile of its owner within its work range that no city stands on, that
    /// the city it belongs to does not work, and that no enemy military unit blocks
    /// (`cities.workable_tiles`, `cities.py:170-193`). So a change to a tile concerns the cities
    /// of the tile's owner, before and after, in range of it: its yield, its owner, a city on it,
    /// or an enemy unit on it. A seat concerns all its player's cities, and war or peace the
    /// cities of each side in range of the other side's military units in its land; a change
    /// to any other term of a relation concerns no city.
    #[must_use]
    pub fn on(&self, st: &State, rules: &Ruleset, ch: &Change) -> Reactions {
        let mut out = Reactions::default();
        let range = u32::try_from(rules.constants().formulas.city_work_range).unwrap_or(0);
        let owner = |t: TileIdx| st.tiles().get(t).and_then(Tile::owner);
        match *ch {
            Change::TileInput(t) => {
                self.flag_around(st, range, t, &[owner(t)], &mut out);
                // What units on it see, when the ruleset's sight uniques read the tile.
                if self.vis.tile_sensitive() {
                    out.sight.push(SightSource::Tile(t));
                }
            }
            Change::TileHeight(t) => {
                self.flag_around(st, range, t, &[owner(t)], &mut out);
                out.sight.push(SightSource::Area(t));
            }
            Change::TileOwner { t, old, new } => {
                out.recheck.extend(old.city);
                out.recheck.extend(new.city);
                self.flag_around(st, range, t, &[old.owner, new.owner], &mut out);
                // The cities that see it through their tiles, and those who see it meeting its
                // new owner.
                for c in [old.city, new.city].into_iter().flatten() {
                    if !out.sight.contains(&SightSource::City(c)) {
                        out.sight.push(SightSource::City(c));
                    }
                }
                out.sight.push(SightSource::Tile(t));
            }
            Change::UnitPlaced { u, owner: by, from, to } => {
                out.sight.push(SightSource::Unit(u));
                let military = st
                    .units()
                    .get(u)
                    .is_some_and(|x| rules.base_units().get(x.base).is_some_and(|b| b.military));
                // A blockade begins or ends as the unit comes or goes, and the cities in range
                // look again at once.
                // refcheck: citizens-follow-a-blockade-at-once
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
                out.sight.push(SightSource::City(c));
                if let Some(x) = st.cities().get(c) {
                    self.flag_near(st, range, x.tile(), &[Some(x.owner())], &mut out);
                    // A unit on its tile is no longer embarked.
                    out.sight.push(SightSource::Tile(x.tile()));
                }
            }
            Change::CityRemoved { c, owner: was, at } => {
                self.flag_near(st, range, at, &[Some(was), owner(at)], &mut out);
                out.sight.push(SightSource::City(c));
                out.sight.push(SightSource::Tile(at));
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
            Change::War { a, b } => {
                // War and peace decide which units blockade whose tiles: each side's military
                // units standing in the other's land.
                self.flag_blockades_of(st, rules, range, a, b, &mut out);
                self.flag_blockades_of(st, rules, range, b, a, &mut out);
            }
            // Two who forget they met meet again if they see each other, as Python's next
            // refresh met them. A meeting makes no other pair meet, so it needs no look.
            Change::Met { a, b } => {
                if !st.diplo().has_met(a, b) {
                    out.sight.push(SightSource::Contact(a));
                    out.sight.push(SightSource::Contact(b));
                }
            }
            Change::Diplo { .. }
            | Change::Talks { .. }
            | Change::Turn
            | Change::Clock
            | Change::Names => {}
            Change::Alliance { cs, .. } => out.sight.push(SightSource::Allies(cs)),
            Change::Spy(p) => out.sight.push(SightSource::Spies(p)),
            Change::Seat(p) => out.recheck.extend(st.cities().of(p).iter().copied()),
            Change::PlayerAlive(p) => out.sight.push(SightSource::Civ(p)),
        }
        out
    }

    /// Flags the cities a change to tile `t` concerns: those of `owners` (its owners before and
    /// after) whose work range reaches it, and those that may work one of its neighbours, since a
    /// tile's yield reads its neighbours (a Moai's culture for each Moai beside it, fresh water,
    /// the coast): the cities of each neighbour's owner whose range reaches that neighbour. For
    /// those it flags every city of the neighbours' owners within one tile more than the range,
    /// which holds them all; the write path still allocates nothing.
    // refcheck: citizens-follow-a-neighbour-at-once
    fn flag_around(
        &self,
        st: &State,
        range: u32,
        t: TileIdx,
        owners: &[Option<PlayerId>],
        out: &mut Reactions,
    ) {
        self.flag_near(st, range, t, owners, out);
        if !self.grid.contains(t) {
            return;
        }
        let mut around: [Option<PlayerId>; 6] = [None; 6];
        for (slot, nb) in around.iter_mut().zip(self.grid.neighbors(t)) {
            *slot = st.tiles().get(nb).and_then(Tile::owner);
        }
        self.flag_near(st, range.saturating_add(1), t, &around, out);
    }

    /// Flags every city of one of `owners` whose work range reaches tile `t`. It walks the
    /// owners' cities rather than the tiles in range: no allocation on the write path, and a
    /// civilization has fewer cities than a work range has tiles, or not many more.
    fn flag_near(
        &self,
        st: &State,
        range: u32,
        t: TileIdx,
        owners: &[Option<PlayerId>],
        out: &mut Reactions,
    ) {
        if !self.grid.contains(t) {
            return;
        }
        for (i, &p) in owners.iter().enumerate() {
            let Some(p) = p else { continue };
            if owners[..i].contains(&Some(p)) {
                continue;
            }
            for &c in st.cities().of(p) {
                if !out.recheck.contains(&c)
                    && st.cities().get(c).is_some_and(|x| self.grid.distance(x.tile(), t) <= range)
                {
                    out.recheck.push(c);
                }
            }
        }
    }

    /// Flags the cities of `owner` that a military unit of `by` standing in `owner`'s land
    /// blocks, or would block at war: what war or peace between them changes.
    fn flag_blockades_of(
        &self,
        st: &State,
        rules: &Ruleset,
        range: u32,
        owner: PlayerId,
        by: PlayerId,
        out: &mut Reactions,
    ) {
        for &u in st.units().of(by) {
            let Some(unit) = st.units().get(u) else { continue };
            let t = unit.tile();
            if st.tiles().get(t).and_then(Tile::owner) == Some(owner)
                && rules.base_units().get(unit.base).is_some_and(|b| b.military)
            {
                self.flag_near(st, range, t, &[Some(owner)], out);
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

    /// The cache oracle (DESIGN.md 9.4) for the caches that read the state alone: validated,
    /// against a cold recompute from the same state. Returns what disagrees, one line each. The
    /// memos that evaluate uniques need the whole game: [`civ::verify`] checks them, and
    /// `vis::verify` what each civilization sees.
    #[must_use]
    pub fn verify(&self, rules: &Ruleset, st: &State) -> Vec<String> {
        let cold = Self::new(rules, st);
        let mut out = Vec::new();
        if self.names(st).entries() != cold.names(st).entries() {
            out.push("the event name index differs from a cold rebuild".to_owned());
        }
        if self.grid != cold.grid {
            out.push("the grid differs from the map's".to_owned());
        }
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

    #[test]
    fn a_tile_change_flags_the_cities_that_may_work_a_neighbour() {
        use crate::state::TileClaim;
        let mut g = testing::duel();
        let rome = PlayerId(0);
        // Roma at (2, 2); (5, 2) is three tiles away, in its range; (6, 2) four and (7, 2) five.
        let roma = testing::city(&mut g, rome, TileIdx(22), "Roma");
        g.set_tile_owner(TileIdx(25), TileClaim::city(rome, roma)).expect("a tile");
        g.settle();
        let next_to_ours = g.dv.on(&g.st, g.rules, &Change::TileInput(TileIdx(26)));
        assert_eq!(next_to_ours.recheck.to_vec(), [roma], "(6, 2) is next to Roma's (5, 2)");
        let farther = g.dv.on(&g.st, g.rules, &Change::TileInput(TileIdx(27)));
        assert!(farther.recheck.is_empty(), "no tile next to (7, 2) is Roma's to work");
    }

    #[test]
    fn war_flags_the_cities_an_enemy_unit_blocks_and_other_terms_none() {
        use crate::state::TileClaim;
        let mut g = testing::duel();
        let (rome, greece) = (PlayerId(0), PlayerId(1));
        let roma = testing::city(&mut g, rome, TileIdx(22), "Roma");
        let athens = testing::city(&mut g, greece, TileIdx(28), "Athens");
        g.settle();
        let (war, peace_terms, talks) = (
            Change::War { a: rome, b: greece },
            Change::Diplo { a: rome, b: greece },
            Change::Talks { a: rome, b: greece },
        );
        for ch in [war, peace_terms, talks] {
            assert!(g.dv.on(&g.st, g.rules, &ch).recheck.is_empty(), "{ch:?}: no unit to block");
        }
        // A Greek warrior in Rome's land, and a Roman worker in Greece's, which blocks nothing.
        let t = TileIdx(23);
        g.set_tile_owner(t, TileClaim::city(rome, roma)).expect("a tile");
        g.set_tile_owner(TileIdx(27), TileClaim::city(greece, athens)).expect("a tile");
        testing::unit(&mut g, greece, "Warrior", t);
        testing::unit(&mut g, rome, "Worker", TileIdx(27));
        assert_eq!(g.dv.on(&g.st, g.rules, &war).recheck.to_vec(), [roma]);
        for ch in [peace_terms, talks] {
            assert!(g.dv.on(&g.st, g.rules, &ch).recheck.is_empty(), "{ch:?}");
        }
        // Through the setter: a research agreement's science flags nothing, and a war flags Roma,
        // blockaded; the shipped ruleset's `[n]% growth <when not at war>` makes every city
        // look again besides.
        g.settle();
        g.update_relation(rome, greece, |r| r.ra_science = [5, 5]).expect("a pair");
        assert!(g.pending.is_empty());
        g.update_relation(rome, greece, |r| r.war = true).expect("a pair");
        let flagged = g.pending.take_recheck();
        assert!(flagged.contains(&roma), "{flagged:?}");
    }
}

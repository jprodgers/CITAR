//! Revisions and self-validating memos (DESIGN.md 6.3).
//!
//! Replaces the invalidation machinery of `game.py:565-609` (`invalidate`, `invalidate_city`,
//! `clear_static`, and the `_cache`, `_ycache` and `_static` dictionaries they cleared). Python
//! threw every cache away on every write; here each write moves the revisions of what it touched,
//! and each memo checks its own inputs when it is read:
//!
//! - a [`Rev`] is a point in the game's sequence of writes. [`Revs`] holds one per input a cache
//!   can read: per tile, per civilization ([`CivRevs`]), per city ([`CityRevs`]), per unit
//!   ([`UnitRevs`]), and a few global ones. Revisions move only under `&mut Game`
//!   (`game::mutate`), never on a read;
//! - a [`Memo`] (or a [`CopyMemo`] for small `Copy` values) holds a value with a [`Stamp`]: when
//!   it was last verified, and when its value last changed. Reading it at the current revision
//!   is one compare. Otherwise its inputs are validated first, recursively, and the memo is
//!   recomputed only if one of them moved since it was verified; a recomputed value equal to the
//!   stored one (floats compared as bits, [`BitEq`]) keeps its old `changed` stamp, so the memos
//!   downstream of it stay valid (early cutoff);
//! - `Revs::cond` maps what a unique's conditionals read ([`CondDeps`]) to the revisions of
//!   those inputs, so a memo that evaluates uniques validates against exactly what they read.
//!   All but `RESOURCES`, which is the supply memo's to answer: a memo validates with
//!   `game::derive::civ::cond`, which maps that class to the memo's stamp and the rest here.
//!
//! The one way to get a borrow error out of a memo is a cycle between memos, which is a bug: the
//! memo being validated holds its value mutably borrowed until it is done, so reading it again
//! from inside its own validation panics on the `RefCell` (or on the busy flag of a
//! [`CopyMemo`]). In release builds the host turns that panic into one poisoned game.

use core::cell::{Cell, Ref, RefCell, RefMut};

use crate::base::collections::LookupMap;
use crate::base::ids::{CityId, Id, PlayerId, TileIdx, UnitId};
use crate::base::sets::PlayerVec;
use crate::base::stats::Stats;
use crate::state::State;
use crate::state::change::Change;
use crate::unique::filter::Combatant;
use crate::unique::{CondDeps, Csr, Ctx, record};

// ---- Revisions --------------------------------------------------------------------------------

/// A point in a game's sequence of writes. Never saved, hashed or used as a random key: two
/// games in the same state may be at different revisions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rev(u64);

impl Rev {
    /// Before anything was verified: a memo starts here, so its first read computes it.
    pub const NEVER: Self = Self(0);

    /// The state as it was built or loaded: every input starts here.
    pub const START: Self = Self(1);

    /// The raw number, for messages and the host's ETag.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The revisions of one civilization's inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivRevs {
    /// What its unique index is built from (DESIGN.md 6.5, `economy.py:91-128`): techs,
    /// policies, era, temporary uniques, its religion's founder beliefs, its seat, the buildings
    /// of its cities, and its city-state bonuses: which city-states it has met, which are alive,
    /// which it is allied with, and with which it stands at the friend level.
    pub index: Rev,
    /// Gold, culture, faith, golden age points, the turns left of a golden age, and the natural
    /// wonders it has found.
    pub stocks: Rev,
    /// Whether it is in a golden age: its turns left crossing 0 ([`PlayerTouch::GOLDEN_AGE`]),
    /// which tile yields and city stats read.
    pub golden_age: Rev,
    /// The research queue, goal and progress.
    pub research: Rev,
    /// Happiness as conditionals see it, committed at fixed stages (DESIGN.md 6.6).
    pub happiness_seen: Rev,
    /// The gold rate citizen ranking reads, written at stage E2 (DESIGN.md 6.6).
    pub gold_rate: Rev,
    /// Its seat: controller, handicap, automatic decisions and difficulty.
    pub seat: Rev,
    /// Which units it has, and where they stand.
    pub units: Rev,
    /// Which units it has and of which base unit each is, and nothing else about them: what its
    /// resource supply reads of its units (their required and consumed resources, DESIGN.md
    /// 6.5). A unit made, lost or given away moves it, and so does an upgrade
    /// ([`UnitTouch::BASE`]); a move, a heal, a promotion or an order does not.
    ///
    /// Unit upkeep reads more than this (whether a unit stands in a city, its promotions, the
    /// conditionals of its unit-level uniques), so a memo of it validates against `units`, the
    /// global `units_core` and `cities`, and the conditionals of the types it evaluates instead.
    pub roster: Rev,
    /// Which cities it has, and which tiles: a tile changing hands moves it for both sides.
    pub cities: Rev,
    /// The buildings in its cities.
    pub buildings: Rev,
    /// How far it has come with religion, its religion's beliefs, and the great prophets it has
    /// earned.
    pub religion: Rev,
    /// A city-state's own data: influence, ally, protectors, quests. No major's index moves
    /// with it, only with a friend level that flips (`Game::set_influence`).
    pub city_state: Rev,
    /// Where its spies are and what they do.
    pub spies: Rev,
}

impl CivRevs {
    const fn at(r: Rev) -> Self {
        Self {
            index: r,
            stocks: r,
            golden_age: r,
            research: r,
            happiness_seen: r,
            gold_rate: r,
            seat: r,
            units: r,
            roster: r,
            cities: r,
            buildings: r,
            religion: r,
            city_state: r,
            spies: r,
        }
    }

    /// The latest of them all.
    #[must_use]
    pub fn max(&self) -> Rev {
        [
            self.index,
            self.stocks,
            self.golden_age,
            self.research,
            self.happiness_seen,
            self.gold_rate,
            self.seat,
            self.units,
            self.roster,
            self.cities,
            self.buildings,
            self.religion,
            self.city_state,
            self.spies,
        ]
        .into_iter()
        .max()
        .unwrap_or(Rev::START)
    }
}

impl Default for CivRevs {
    fn default() -> Self {
        Self::at(Rev::START)
    }
}

/// The revisions of one city's inputs (the city `touch` flags of DESIGN.md 6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CityRevs {
    /// Its buildings, population, status (puppet, razing, resistance, health), queue, owner and
    /// tile.
    pub core: Rev,
    /// Its buildings alone: what its local unique index is built from.
    pub buildings: Rev,
    /// The tiles it owns and works, its specialists and focus.
    pub work: Rev,
    /// Stored food, culture and production.
    pub stocks: Rev,
    /// Religious pressure and followers, and whose holy city it is.
    pub religion: Rev,
    /// Its territory: which tiles are its and what they are and bear (their inputs,
    /// [`Revs::tile`]), which a city's yields read of the tiles it owns but does not work, as a
    /// Citadel's. Not in [`max`](Self::max), which is the city itself.
    pub tiles: Rev,
}

impl CityRevs {
    const fn at(r: Rev) -> Self {
        Self { core: r, buildings: r, work: r, stocks: r, religion: r, tiles: r }
    }

    /// The latest of them all but [`tiles`](Self::tiles).
    #[must_use]
    pub fn max(&self) -> Rev {
        self.core.max(self.buildings).max(self.work).max(self.stocks).max(self.religion)
    }
}

impl Default for CityRevs {
    /// Never written: each field reads as where the revisions started.
    fn default() -> Self {
        Self::at(Rev::NEVER)
    }
}

/// The revisions of one unit's inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitRevs {
    /// Its base unit, owner, promotions, health, status and the actions it has used.
    pub core: Rev,
    /// Its movement points.
    pub moves: Rev,
    /// Where it stands, and whether it is carried.
    pub place: Rev,
}

impl UnitRevs {
    const fn at(r: Rev) -> Self {
        Self { core: r, moves: r, place: r }
    }

    /// The latest of them all.
    #[must_use]
    pub fn max(&self) -> Rev {
        self.core.max(self.moves).max(self.place)
    }
}

impl Default for UnitRevs {
    /// Never written: each field reads as where the revisions started.
    fn default() -> Self {
        Self::at(Rev::NEVER)
    }
}

/// The tiles changed since a revision, for caches that rebuild per tile (movement costs, the
/// route layer). It keeps a bounded log: asked about a revision older than the log reaches, it
/// says so, and the cache rebuilds in full.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileChangeLog {
    /// The latest tile change.
    rev: Rev,
    /// Changes older than this were dropped.
    floor: Rev,
    log: Vec<(Rev, TileIdx)>,
}

impl TileChangeLog {
    /// The most entries kept; past it the older half is dropped.
    pub const CAP: usize = 4096;

    fn new() -> Self {
        Self { rev: Rev::START, floor: Rev::START, log: Vec::new() }
    }

    /// The latest revision at which any tile changed.
    #[must_use]
    pub const fn rev(&self) -> Rev {
        self.rev
    }

    fn push(&mut self, r: Rev, t: TileIdx) {
        self.rev = r;
        if self.log.last() == Some(&(r, t)) {
            return;
        }
        if self.log.len() >= Self::CAP {
            let half = self.log.len() / 2;
            self.floor = self.log[half - 1].0;
            self.log.drain(..half);
        }
        self.log.push((r, t));
    }

    /// The tiles changed after `since`, oldest first and possibly repeated; `None` if the log no
    /// longer reaches back that far.
    #[must_use]
    pub fn since(&self, since: Rev) -> Option<impl Iterator<Item = TileIdx> + '_> {
        if since < self.floor {
            return None;
        }
        let start = self.log.partition_point(|&(r, _)| r <= since);
        Some(self.log[start..].iter().map(|&(_, t)| t))
    }
}

/// Every revision a cache can read (DESIGN.md 6.3). Only `game::mutate` moves them, under
/// `&mut Game`.
#[derive(Clone, Debug)]
pub struct Revs {
    now: Rev,
    /// What a tile yields or costs: terrain, features, resource, improvement, route, river,
    /// pillage, build queue.
    tile: Vec<Rev>,
    /// Who owns a tile, and which city works it.
    tile_owner: Vec<Rev>,
    /// What blocks sight on a tile: its terrain, features and natural wonder.
    tile_height: Vec<Rev>,
    /// Which tiles changed, for the per-tile caches.
    pub tile_log: TileChangeLog,
    /// Any tile's owner.
    pub owners: Rev,
    /// Any route, or its pillage.
    pub routes: Rev,
    /// Which tiles any city works.
    pub worked: Rev,
    /// Which cities exist, who owns each, which is a capital, and who is alive.
    pub cities: Rev,
    /// Any city's buildings, population or status.
    pub city_core: Rev,
    /// Any city's buildings.
    pub city_buildings: Rev,
    /// Any city's religious pressure and followers, and which cities are holy.
    pub city_religion: Rev,
    /// Where any unit stands, who owns it, and which exist.
    pub unit_pos: Rev,
    /// Any unit's promotions, health or status.
    pub units_core: Rev,
    /// The relations the diplomatic conditionals read: war, contact, declared friendships,
    /// pacts, open borders; and deals, which trade resources. Who is alive, since the dead are
    /// at war with no one (`game.py:673-675`).
    pub diplo: Rev,
    /// A city-state's influence with any major, which decides who counts it a friend.
    pub influence: Rev,
    /// Negotiations, opinions, and the bookkeeping of relations (treaty terms, research
    /// agreements, embassies, denouncements), which no rule cache reads.
    pub talks: Rev,
    /// Which city-state is allied with whom.
    pub alliances: Rev,
    /// The policies any civilization has adopted.
    pub policies: Rev,
    /// The founded religions, their beliefs, and who founded which.
    pub religions: Rev,
    /// The world wonders built.
    pub wonders: Rev,
    /// The UN and the barbarian camps.
    pub world: Rev,
    /// A civilization's, leader's or city's name.
    pub names: Rev,
    /// The turn number.
    pub turn: Rev,
    /// The rest of the clock: whose turn it is, whether it began, the phase and the winner.
    /// Nothing derived reads it.
    pub clock: Rev,
    /// The settings.
    pub config: Rev,
    civ: PlayerVec<CivRevs>,
    /// Cities and units by id, sparse: an id may be as large as `store::MAX_ENTITY_ID`, and one
    /// such id in a save must not grow a table to hundreds of megabytes (DESIGN.md 4.8). An id
    /// never written reads as `floor`. A removed entity keeps its entry, so a memo still keyed
    /// by it validates against its removal.
    city: LookupMap<CityId, CityRevs>,
    unit: LookupMap<UnitId, UnitRevs>,
    /// Where every input started: what a city or unit never written since reads as.
    floor: Rev,
}

impl Revs {
    /// The revisions of a state as built or loaded: everything at [`Rev::START`].
    #[must_use]
    pub fn new(st: &State) -> Self {
        Self::starting_at(st, Rev::START)
    }

    /// The revisions of a state whose caches start cold again after revision `after`, as a
    /// settings edit leaves them: everything at the next revision, so that revisions never go
    /// back and every input reads as moved.
    #[must_use]
    pub fn after(st: &State, after: Rev) -> Self {
        Self::starting_at(st, Rev(after.0.saturating_add(1)).max(Rev::START))
    }

    fn starting_at(st: &State, s: Rev) -> Self {
        let tiles = st.tiles().len();
        Self {
            now: s,
            tile: vec![s; tiles],
            tile_owner: vec![s; tiles],
            tile_height: vec![s; tiles],
            tile_log: TileChangeLog::new(),
            owners: s,
            routes: s,
            worked: s,
            cities: s,
            city_core: s,
            city_buildings: s,
            city_religion: s,
            unit_pos: s,
            units_core: s,
            diplo: s,
            influence: s,
            talks: s,
            alliances: s,
            policies: s,
            religions: s,
            wonders: s,
            world: s,
            names: s,
            turn: s,
            clock: s,
            config: s,
            civ: st.players().ids().map(|_| CivRevs::at(s)).collect(),
            city: LookupMap::new(),
            unit: LookupMap::new(),
            floor: s,
        }
    }

    /// The current revision: what a memo verified now is verified at.
    #[must_use]
    #[inline]
    pub const fn now(&self) -> Rev {
        self.now
    }

    /// Moves on to a new revision and returns it: every write takes one.
    pub(crate) fn next(&mut self) -> Rev {
        self.now = Rev(self.now.0.saturating_add(1));
        self.now
    }

    // ---- Reads ------------------------------------------------------------------------------

    /// A tile's yield and cost inputs.
    #[must_use]
    pub fn tile(&self, t: TileIdx) -> Rev {
        self.tile.get(t.0 as usize).copied().unwrap_or(self.floor)
    }

    /// A tile's owner and working city.
    #[must_use]
    pub fn tile_owner(&self, t: TileIdx) -> Rev {
        self.tile_owner.get(t.0 as usize).copied().unwrap_or(self.floor)
    }

    /// What blocks sight on a tile.
    #[must_use]
    pub fn tile_height(&self, t: TileIdx) -> Rev {
        self.tile_height.get(t.0 as usize).copied().unwrap_or(self.floor)
    }

    /// A civilization's revisions; a player the game does not have reads as untouched.
    #[must_use]
    pub fn civ(&self, p: PlayerId) -> CivRevs {
        self.civ.get(p).copied().unwrap_or(CivRevs::at(self.floor))
    }

    /// A city's revisions; a city never written since the game was built reads as untouched.
    #[must_use]
    pub fn city(&self, c: CityId) -> CityRevs {
        let r = self.city.get(&c).copied().unwrap_or_default();
        let f = self.floor;
        CityRevs {
            core: r.core.max(f),
            buildings: r.buildings.max(f),
            work: r.work.max(f),
            stocks: r.stocks.max(f),
            religion: r.religion.max(f),
            tiles: r.tiles.max(f),
        }
    }

    /// A unit's revisions; a unit never written since the game was built reads as untouched.
    #[must_use]
    pub fn unit(&self, u: UnitId) -> UnitRevs {
        let r = self.unit.get(&u).copied().unwrap_or_default();
        let f = self.floor;
        UnitRevs { core: r.core.max(f), moves: r.moves.max(f), place: r.place.max(f) }
    }

    // ---- What a unique's conditionals read ---------------------------------------------------

    /// The latest revision of the inputs the civilization-level classes of `deps` read for
    /// civilization `p` (DESIGN.md 5.8): with the local classes left out, this is what a memo
    /// hoisting the civilization-level half of its uniques validates against. With no
    /// civilization in context the classes about the civilization read nothing that can move,
    /// since the conditionals about it then fail whatever happens.
    ///
    /// Not for `RESOURCES` with a civilization in context: that class is the supply memo's
    /// (`game::derive::civ::cond`), and here it would read as moved on every write, so a memo
    /// validated with it would recompute on every read. Debug builds refuse it.
    #[must_use]
    pub(crate) fn cond_civ(&self, p: Option<PlayerId>, deps: CondDeps) -> Rev {
        debug_assert!(
            !deps.contains(CondDeps::RESOURCES) || p.is_none(),
            "RESOURCES is validated through game::derive::civ::cond"
        );
        let mut r = Rev::START;
        let civ = p.map(|p| self.civ(p));
        for class in deps.difference(CondDeps::LOCAL).iter() {
            let at = match class {
                CondDeps::TURN | CondDeps::CHANCE => self.turn,
                CondDeps::HAPPINESS_SEEN => civ.map_or(Rev::START, |c| c.happiness_seen),
                CondDeps::STOCKS => civ.map_or(Rev::START, |c| c.stocks),
                CondDeps::GOLDEN_AGE => civ.map_or(Rev::START, |c| c.golden_age),
                // The resource supply is a memo (`ResourceSupply`, package 1b-05), which the
                // revisions alone cannot validate: `game::derive::civ::cond` maps this class to
                // the memo's own stamp. Should a release build get here, it reads as moved on
                // every write, which is always correct.
                CondDeps::RESOURCES => civ.map_or(Rev::START, |_| self.now),
                CondDeps::WAR => self.diplo,
                CondDeps::INFLUENCE => self.influence,
                // The trade network is a memo (`Connectivity`, package 1b-06), which the
                // revisions alone cannot validate: `game::derive::civ::cond` maps this class to
                // the memo's own stamp. Read here, it reads as moved on every write, which is
                // always correct: the resource supply, which validates with the revisions and sees
                // the network without the memo, reads it so.
                CondDeps::CONNECTED => self.now,
                CondDeps::ERA | CondDeps::TECHS | CondDeps::POLICIES => {
                    civ.map_or(Rev::START, |c| c.index)
                }
                CondDeps::RESEARCH_QUEUE => civ.map_or(Rev::START, |c| c.research),
                // A city filter over the civilization's cities reads their majority religion.
                CondDeps::RELIGION_STATE => self
                    .religions
                    .max(self.city_religion)
                    .max(civ.map_or(Rev::START, |c| c.religion.max(c.index))),
                CondDeps::CIV_BUILDINGS => {
                    civ.map_or(Rev::START, |c| c.buildings.max(c.cities)).max(self.cities)
                }
                CondDeps::GLOBAL_BUILDINGS => self.city_buildings.max(self.cities),
                // Beliefs count as adopted (`no-civ-adopted-counts-beliefs`): a civilization's
                // are its religion's.
                CondDeps::GLOBAL_POLICIES => self.policies.max(self.religions),
                // What a city filter reads of the cities counted, their religion included.
                CondDeps::CITY_COUNT => self.cities.max(self.city_core).max(self.city_religion),
                CondDeps::UNIT_SET => self.unit_pos.max(self.units_core),
                CondDeps::SEAT => civ.map_or(Rev::START, |c| c.seat),
                CondDeps::CONFIG => self.config,
                CondDeps::MAP => {
                    self.tile_log.rev().max(self.routes).max(self.owners).max(self.worked)
                }
                // Every class is one bit, and the local ones were taken out above; a class added
                // to CondDeps without a line here reads as every input, which is always correct.
                _ => self.now,
            };
            r = r.max(at);
        }
        r
    }

    /// The latest revision of everything the conditionals of `deps` read in `ctx`, the
    /// context-local classes included: the city a rule means (`Ctx::rel_city`: the city in
    /// context, our side's in a fight, or the city whose territory the tile is), the unit, the
    /// tile and the fight. A memo keyed by a tile or a unit that evaluates a city conditional so
    /// validates against its territory city's revisions too.
    ///
    /// Not for `RESOURCES` with a civilization in context ([`cond_civ`](Self::cond_civ)): a memo
    /// validates with `game::derive::civ::cond`, which calls this for the other classes.
    #[must_use]
    pub(crate) fn cond(&self, st: &State, deps: CondDeps, ctx: &Ctx) -> Rev {
        let mut r = self.cond_civ(ctx.civ, deps);
        if deps.contains(CondDeps::CITY) {
            // Not where its citizens work: that is `TILE`'s, which a conditional about the
            // citizens reads too.
            if let Some(c) = rel_city(st, ctx) {
                let x = self.city(c);
                r = r.max(x.core.max(x.buildings).max(x.stocks).max(x.religion)).max(self.cities);
            }
            // Which city's territory the tile is, and whether it is the civilization's.
            for t in [ctx.tile, ctx.rel_tile()].into_iter().flatten() {
                r = r.max(self.tile_owner(t));
            }
        }
        if deps.contains(CondDeps::UNIT) {
            for u in [ctx.unit, ctx.rel_unit()].into_iter().flatten() {
                r = r.max(self.unit(u).max());
            }
        }
        if deps.contains(CondDeps::TILE) {
            for t in [ctx.tile, ctx.rel_tile()].into_iter().flatten() {
                r = r.max(self.tile_facts(st, t));
            }
        }
        if deps.contains(CondDeps::COMBAT)
            && let Some(f) = ctx.combat
        {
            for side in [Some(f.our), f.their].into_iter().flatten() {
                r = r.max(match side {
                    Combatant::Unit(u) => self.unit(u).max(),
                    Combatant::City(c) => self.city(c).max().max(self.cities),
                });
            }
            if let Some(t) = f.attacked_tile {
                r = r.max(self.tile_facts(st, t));
            }
        }
        r
    }

    /// Everything a tile conditional reads of one tile: its inputs, its owner, and where its
    /// territory city's citizens work (which of its tiles, its specialists).
    fn tile_facts(&self, st: &State, t: TileIdx) -> Rev {
        let mut r = self.tile(t).max(self.tile_owner(t));
        if let Some(c) = st.tiles().get(t).and_then(|x| x.city()) {
            r = r.max(self.city(c).work);
        }
        r
    }

    // ---- Writes (game::mutate) ---------------------------------------------------------------

    fn civ_mut(&mut self, p: PlayerId) -> Option<&mut CivRevs> {
        self.civ.get_mut(p)
    }

    fn city_mut(&mut self, c: CityId) -> &mut CityRevs {
        self.city.get_or_insert_with(c, CityRevs::default)
    }

    fn unit_mut(&mut self, u: UnitId) -> &mut UnitRevs {
        self.unit.get_or_insert_with(u, UnitRevs::default)
    }

    /// Moves the unique index of every civilization.
    fn every_index(&mut self, r: Rev) {
        for (_, c) in self.civ.iter_mut() {
            c.index = r;
        }
    }

    /// Moves the unique indexes of `a` and `b` if either is a city-state: its bonuses in the
    /// other's index depend on contact, war and whether it lives (`city_states.py:160-173`).
    fn city_state_bonus(&mut self, st: &State, a: PlayerId, b: PlayerId, r: Rev) {
        let cs = |p| st.player(p).is_some_and(crate::state::players::Player::is_city_state);
        if cs(a) || cs(b) {
            for p in [a, b] {
                if let Some(c) = self.civ_mut(p) {
                    c.index = r;
                }
            }
        }
    }

    fn at_tile(v: &mut [Rev], t: TileIdx, r: Rev) {
        if let Some(x) = v.get_mut(t.0 as usize) {
            *x = r;
        }
    }

    /// Moves the revisions a change touched to a new revision. `st` is the state after the
    /// write.
    pub(crate) fn on_change(&mut self, st: &State, ch: &Change) {
        let r = self.next();
        match *ch {
            Change::TileInput(t) => {
                Self::at_tile(&mut self.tile, t, r);
                self.tile_log.push(r, t);
                self.territory(st, t, r);
            }
            Change::TileHeight(t) => {
                Self::at_tile(&mut self.tile, t, r);
                Self::at_tile(&mut self.tile_height, t, r);
                self.tile_log.push(r, t);
                self.territory(st, t, r);
            }
            Change::TileOwner { t, old, new } => {
                Self::at_tile(&mut self.tile_owner, t, r);
                self.owners = r;
                self.tile_log.push(r, t);
                for c in [old.city, new.city].into_iter().flatten() {
                    let x = self.city_mut(c);
                    x.work = r;
                    x.tiles = r;
                }
                for p in [old.owner, new.owner].into_iter().flatten() {
                    if let Some(c) = self.civ_mut(p) {
                        c.cities = r;
                    }
                }
            }
            Change::UnitPlaced { u, owner, from, .. } => {
                self.unit_mut(u).place = r;
                self.unit_pos = r;
                if let Some(c) = self.civ_mut(owner) {
                    c.units = r;
                    // A unit placed from nowhere is a new one.
                    if from.is_none() {
                        c.roster = r;
                    }
                }
            }
            Change::UnitOwner { u, old, new } => {
                *self.unit_mut(u) = UnitRevs::at(r);
                self.unit_pos = r;
                self.units_core = r;
                for p in [old, new] {
                    if let Some(c) = self.civ_mut(p) {
                        c.units = r;
                        c.roster = r;
                    }
                }
            }
            Change::UnitRemoved { u, owner, .. } => {
                *self.unit_mut(u) = UnitRevs::at(r);
                self.unit_pos = r;
                self.units_core = r;
                if let Some(c) = self.civ_mut(owner) {
                    c.units = r;
                    c.roster = r;
                }
            }
            Change::CityAdded(c) => {
                let owner = st.cities().get(c).map(crate::state::cities::City::owner);
                self.city_event(c, owner.into_iter().collect(), r);
            }
            Change::CityRemoved { c, owner, .. } => self.city_event(c, vec![owner], r),
            Change::CityOwner { c, old, new } => self.city_event(c, vec![old, new], r),
            Change::CityTiles(c) => {
                self.city_mut(c).work = r;
                self.owners = r;
                self.worked = r;
            }
            Change::War { a, b } => {
                self.diplo = r;
                self.city_state_bonus(st, a, b, r);
            }
            Change::Met { a, b } => {
                self.diplo = r;
                self.city_state_bonus(st, a, b, r);
            }
            Change::Diplo { .. } => self.diplo = r,
            Change::Talks { .. } => self.talks = r,
            Change::Alliance { cs, old, new } => {
                self.alliances = r;
                if let Some(c) = self.civ_mut(cs) {
                    c.city_state = r;
                }
                for p in [old, new].into_iter().flatten() {
                    if let Some(c) = self.civ_mut(p) {
                        c.index = r;
                    }
                }
            }
            Change::Spy(p) => {
                if let Some(c) = self.civ_mut(p) {
                    c.spies = r;
                }
            }
            Change::Seat(p) => {
                if let Some(c) = self.civ_mut(p) {
                    c.seat = r;
                    c.index = r;
                }
            }
            Change::PlayerAlive(p) => {
                if let Some(c) = self.civ_mut(p) {
                    *c = CivRevs::at(r);
                }
                self.cities = r;
                self.policies = r;
                self.names = r;
                // The dead are at war with no one (`game.py:673-675`), though their relations
                // keep the flag (`victory.py:381-400`).
                self.diplo = r;
                // A living city-state's bonuses are in the indexes of the majors that met it.
                if st.player(p).is_some_and(crate::state::players::Player::is_city_state) {
                    self.every_index(r);
                }
            }
            Change::Turn => self.turn = r,
            Change::Clock => self.clock = r,
            Change::Names => self.names = r,
        }
    }

    /// Tile `t`'s inputs moved: so did its territory city's [`CityRevs::tiles`].
    fn territory(&mut self, st: &State, t: TileIdx, r: Rev) {
        if let Some(c) = st.tiles().get(t).and_then(crate::state::map::Tile::city) {
            self.city_mut(c).tiles = r;
        }
    }

    /// A city appeared, went, or changed hands: everything about it, which cities exist, and
    /// what its owners' indexes hold (its buildings) moved.
    fn city_event(&mut self, c: CityId, owners: Vec<PlayerId>, r: Rev) {
        *self.city_mut(c) = CityRevs::at(r);
        self.cities = r;
        self.city_core = r;
        self.city_buildings = r;
        self.city_religion = r;
        self.worked = r;
        self.names = r;
        for p in owners {
            if let Some(x) = self.civ_mut(p) {
                x.cities = r;
                x.buildings = r;
                x.index = r;
            }
        }
    }

    /// A touch of a city's fields (DESIGN.md 6.4).
    pub(crate) fn touch_city(&mut self, c: CityId, owner: PlayerId, t: CityTouch) {
        let r = self.next();
        if t.intersects(CityTouch::CORE | CityTouch::BUILDINGS) {
            self.city_mut(c).core = r;
            self.city_core = r;
        }
        if t.contains(CityTouch::BUILDINGS) {
            // Its non-local buildings are in its owner's index (`economy.py:103-108`).
            self.city_mut(c).buildings = r;
            self.city_buildings = r;
            if let Some(x) = self.civ_mut(owner) {
                x.buildings = r;
                x.index = r;
            }
        }
        if t.contains(CityTouch::WORK) {
            self.city_mut(c).work = r;
            self.worked = r;
        }
        if t.contains(CityTouch::STOCKS) {
            self.city_mut(c).stocks = r;
        }
        if t.contains(CityTouch::RELIGION) {
            self.city_mut(c).religion = r;
            self.city_religion = r;
        }
        if t.contains(CityTouch::NAME) {
            self.names = r;
        }
    }

    /// A touch of a player's fields.
    pub(crate) fn touch_player(&mut self, p: PlayerId, t: PlayerTouch) {
        let r = self.next();
        if t.contains(PlayerTouch::CITY_STATE) {
            // Influence moves every turn; a major's index moves only when its friend level
            // flips, which `Game::set_influence` sees.
            self.influence = r;
        }
        if t.contains(PlayerTouch::POLICIES) {
            self.policies = r;
        }
        if t.contains(PlayerTouch::RELIGION) {
            self.religions = r;
        }
        if t.contains(PlayerTouch::NAME) {
            self.names = r;
        }
        if t.contains(PlayerTouch::CAPITAL) {
            self.cities = r;
        }
        let Some(c) = self.civ_mut(p) else { return };
        // Its religion's founder beliefs are in its index (`economy.py:124-128`).
        let fields: [(PlayerTouch, &mut Rev); 10] = [
            (PlayerTouch::INDEX | PlayerTouch::POLICIES | PlayerTouch::RELIGION, &mut c.index),
            (PlayerTouch::STOCKS, &mut c.stocks),
            (PlayerTouch::GOLDEN_AGE, &mut c.golden_age),
            (PlayerTouch::RESEARCH, &mut c.research),
            (PlayerTouch::HAPPINESS_SEEN, &mut c.happiness_seen),
            (PlayerTouch::GOLD_RATE, &mut c.gold_rate),
            (PlayerTouch::CITY_STATE, &mut c.city_state),
            (PlayerTouch::SPIES, &mut c.spies),
            (PlayerTouch::RELIGION, &mut c.religion),
            (PlayerTouch::CAPITAL, &mut c.cities),
        ];
        for (flags, rev) in fields {
            if t.intersects(flags) {
                *rev = r;
            }
        }
    }

    /// A touch of a unit's fields; `owner` is the unit's.
    pub(crate) fn touch_unit(&mut self, u: UnitId, owner: PlayerId, t: UnitTouch) {
        let r = self.next();
        if t.intersects(UnitTouch::CORE | UnitTouch::BASE) {
            self.unit_mut(u).core = r;
            self.units_core = r;
        }
        // Only a new base unit changes what its owner's resource supply reads: every heal,
        // promotion or order of a unit would otherwise recompute the supply.
        if t.contains(UnitTouch::BASE)
            && let Some(c) = self.civ_mut(owner)
        {
            c.roster = r;
        }
        if t.contains(UnitTouch::MOVES) {
            self.unit_mut(u).moves = r;
        }
    }

    /// A touch of the world's fields.
    pub(crate) fn touch_world(&mut self, t: WorldTouch) {
        let r = self.next();
        if t.contains(WorldTouch::RELIGIONS) {
            // Founder beliefs are in their founder's index, and the touch does not say whose
            // religion changed; religions are founded and enhanced a handful of times a game.
            self.religions = r;
            for (_, c) in self.civ.iter_mut() {
                c.index = r;
                c.religion = r;
            }
        }
        if t.contains(WorldTouch::WONDERS) {
            self.wonders = r;
        }
        if t.intersects(WorldTouch::UN | WorldTouch::CAMPS) {
            self.world = r;
        }
    }

    /// A touch of the diplomacy's lists.
    pub(crate) fn touch_diplo(&mut self, t: DiploTouch) {
        let r = self.next();
        if t.contains(DiploTouch::DEALS) {
            self.diplo = r;
        }
        if t.intersects(DiploTouch::NEGOTIATIONS | DiploTouch::OPINIONS) {
            self.talks = r;
        }
    }

    /// A route or its pillage changed: the connections between cities may have.
    pub(crate) fn touch_routes(&mut self) {
        self.routes = self.next();
    }
}

/// The city whose revisions a city conditional in `ctx` reads, as [`Ctx::rel_city`] finds it
/// from the state: the city in context, our side's city in a fight, or the city whose territory
/// the tile in context is, if the civilization in context owns it.
fn rel_city(st: &State, ctx: &Ctx) -> Option<CityId> {
    if ctx.city.is_some() {
        return ctx.city;
    }
    if let Some(crate::unique::CombatCtx { our: Combatant::City(c), .. }) = ctx.combat {
        return Some(c);
    }
    let c = st.tiles().get(ctx.tile?)?.city()?;
    (st.cities().get(c).map(crate::state::cities::City::owner) == ctx.civ).then_some(c)
}

// ---- Touches ----------------------------------------------------------------------------------

bitflags::bitflags! {
    /// What a touch of a city's fields changes (DESIGN.md 6.4). `CORE`, `BUILDINGS` and `WORK`
    /// flag the city for a citizen recheck.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct CityTouch: u8 {
        /// Population, status (puppet, razing, resistance), queue, health: what a city filter
        /// reads of it. Its owner's index does not move.
        const CORE = 1 << 0;
        /// Its buildings: what its local index and its owner's index are built from. Implies
        /// `CORE`.
        const BUILDINGS = 1 << 5;
        /// Worked and locked tiles, specialists, focus.
        const WORK = 1 << 1;
        /// Stored food, culture, production progress.
        const STOCKS = 1 << 2;
        /// Religious pressure and followers, and whose holy city it is (`holy_city_of`).
        const RELIGION = 1 << 3;
        /// Its name, which the event name index reads.
        const NAME = 1 << 4;
    }
}

bitflags::bitflags! {
    /// What a touch of a player's fields changes (DESIGN.md 6.4).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct PlayerTouch: u16 {
        /// What its unique index is built from besides policies: techs, era, temporary uniques.
        const INDEX = 1 << 0;
        /// Its adopted branches and policies: its index, and what every civilization has
        /// adopted.
        const POLICIES = 1 << 11;
        /// Gold, culture, faith, golden age points, the turns left of a golden age while it lasts,
        /// and the natural wonders it has found.
        const STOCKS = 1 << 1;
        /// A golden age beginning or ending: the turns left crossing 0, which tile yields and
        /// city stats read. `Game::set_golden_age_turns` picks it or `STOCKS`.
        const GOLDEN_AGE = 1 << 12;
        /// The research queue, goal and progress.
        const RESEARCH = 1 << 2;
        /// The happiness conditionals see (committed at stages S1 and E1).
        const HAPPINESS_SEEN = 1 << 3;
        /// The gold rate citizen ranking reads (written at stage E2).
        const GOLD_RATE = 1 << 4;
        /// Its name or its leader's, which the event name index reads.
        const NAME = 1 << 5;
        /// A city-state's influence, protectors, quests and the rest of its data. It moves no
        /// major's index: influence that may flip a major's friend level changes through
        /// `Game::set_influence`, which moves that major's index when it does, and the ally
        /// through `Game::set_ally`.
        const CITY_STATE = 1 << 6;
        /// Its spies.
        const SPIES = 1 << 7;
        /// How far it has come with religion (progress, the religion it founded, its pantheon)
        /// and the great prophets it has earned: its index (founder beliefs) too.
        const RELIGION = 1 << 8;
        /// Its capital.
        const CAPITAL = 1 << 9;
        /// Anything else: notes, history, great person points and counts, counters, which no
        /// cache reads. Not the great prophets earned, which religion conditionals read: those
        /// are `RELIGION`.
        const OTHER = 1 << 10;
    }
}

bitflags::bitflags! {
    /// What a touch of a unit's fields changes (DESIGN.md 6.4).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct UnitTouch: u8 {
        /// Promotions, health, experience, status, orders, used actions.
        const CORE = 1 << 0;
        /// Its base unit (an upgrade): what its owner's resource supply reads of it
        /// ([`CivRevs::roster`]). Implies `CORE`.
        const BASE = 1 << 3;
        /// Movement points.
        const MOVES = 1 << 1;
        /// What it sees: marks it as a dirty vision source.
        const SIGHT = 1 << 2;
    }
}

bitflags::bitflags! {
    /// What a touch of the world's fields changes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct WorldTouch: u8 {
        /// The founded religions and their beliefs.
        const RELIGIONS = 1 << 0;
        /// The world wonders built.
        const WONDERS = 1 << 1;
        /// The United Nations.
        const UN = 1 << 2;
        /// The barbarian camps.
        const CAMPS = 1 << 3;
    }
}

bitflags::bitflags! {
    /// What a touch of the diplomacy's lists changes. Relations themselves change through
    /// `Diplomacy::update`, which reports a [`Change`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct DiploTouch: u8 {
        /// Deals, which trade resources and gold.
        const DEALS = 1 << 0;
        /// Negotiations.
        const NEGOTIATIONS = 1 << 1;
        /// Opinions.
        const OPINIONS = 1 << 2;
    }
}

// ---- Equality as bits -------------------------------------------------------------------------

/// Equality for early cutoff: floats compared as bits, so `-0.0` and `0.0` differ and a NaN
/// equals itself. A recomputed memo value equal to the stored one in this sense does not move
/// the memo's `changed` stamp.
pub trait BitEq {
    /// Whether the two are the same, bit for bit.
    fn bit_eq(&self, other: &Self) -> bool;
}

macro_rules! bit_eq_by_eq {
    ($($t:ty),* $(,)?) => {
        $(impl BitEq for $t {
            #[inline]
            fn bit_eq(&self, other: &Self) -> bool {
                self == other
            }
        })*
    };
}

bit_eq_by_eq!(
    bool,
    u8,
    u16,
    u32,
    u64,
    i8,
    i16,
    i32,
    i64,
    usize,
    (),
    PlayerId,
    CityId,
    UnitId,
    TileIdx,
    crate::base::ids::EraId,
    Csr,
    crate::base::sets::BitSet,
    crate::base::sets::PlayerSet,
    Box<str>,
    String,
);

impl BitEq for f64 {
    #[inline]
    fn bit_eq(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl BitEq for f32 {
    #[inline]
    fn bit_eq(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
}

impl BitEq for Stats {
    fn bit_eq(&self, other: &Self) -> bool {
        self.0.iter().zip(other.0.iter()).all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

impl<T: BitEq> BitEq for Option<T> {
    fn bit_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Some(a), Some(b)) => a.bit_eq(b),
            (None, None) => true,
            _ => false,
        }
    }
}

impl<T: BitEq> BitEq for [T] {
    fn bit_eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a.bit_eq(b))
    }
}

impl<T: BitEq> BitEq for Vec<T> {
    fn bit_eq(&self, other: &Self) -> bool {
        self.as_slice().bit_eq(other.as_slice())
    }
}

impl<T: BitEq, const N: usize> BitEq for [T; N] {
    fn bit_eq(&self, other: &Self) -> bool {
        self.as_slice().bit_eq(other.as_slice())
    }
}

impl<A: BitEq, B: BitEq> BitEq for (A, B) {
    fn bit_eq(&self, other: &Self) -> bool {
        self.0.bit_eq(&other.0) && self.1.bit_eq(&other.1)
    }
}

impl<I: Id, T: BitEq> BitEq for crate::base::ids::IdVec<I, T> {
    fn bit_eq(&self, other: &Self) -> bool {
        self.as_slice().bit_eq(other.as_slice())
    }
}

// ---- Memos ------------------------------------------------------------------------------------

/// When a memo was last verified, and when its value last changed.
#[derive(Debug, Default)]
pub struct Stamp {
    verified: Cell<Rev>,
    changed: Cell<Rev>,
    #[cfg(feature = "stats")]
    counts: MemoCounts,
}

impl Clone for Stamp {
    /// The same stamps: a game's clone starts with a copy of its revisions, so what was verified
    /// in the one is verified in the other, and each moves on with its own. The counts start
    /// afresh.
    fn clone(&self) -> Self {
        Self {
            verified: self.verified.clone(),
            changed: self.changed.clone(),
            #[cfg(feature = "stats")]
            counts: MemoCounts::default(),
        }
    }
}

/// How often a memo was read, validated and recomputed (feature `stats`).
#[cfg(feature = "stats")]
#[derive(Debug, Default)]
pub struct MemoCounts {
    /// Reads answered by one compare.
    pub hits: Cell<u64>,
    /// Reads that validated the inputs and found them unchanged.
    pub valid: Cell<u64>,
    /// Reads that recomputed the value.
    pub recomputed: Cell<u64>,
}

impl Stamp {
    /// When the value last changed: what a memo downstream compares with its own verification.
    #[must_use]
    #[inline]
    pub fn changed(&self) -> Rev {
        self.changed.get()
    }

    /// When the value was last verified.
    #[must_use]
    #[inline]
    pub fn verified(&self) -> Rev {
        self.verified.get()
    }

    /// Forgets the verification: the next read validates again.
    pub fn reset(&self) {
        self.verified.set(Rev::NEVER);
    }

    #[cfg(feature = "stats")]
    fn count(c: &Cell<u64>) {
        c.set(c.get().saturating_add(1));
    }

    /// The hit, validation and recompute counts.
    #[cfg(feature = "stats")]
    #[must_use]
    pub fn counts(&self) -> (u64, u64, u64) {
        (self.counts.hits.get(), self.counts.valid.get(), self.counts.recomputed.get())
    }

    /// Whether a read at `now` is answered by the stored value without looking further.
    #[inline]
    fn hit(&self, now: Rev) -> bool {
        let hit = self.verified.get() == now;
        #[cfg(feature = "stats")]
        if hit {
            Self::count(&self.counts.hits);
        }
        hit
    }

    /// Whether inputs last moved at `input` leave the value as it was verified.
    fn still_valid(&self, input: Rev) -> bool {
        let valid = self.verified.get() != Rev::NEVER && input <= self.verified.get();
        #[cfg(feature = "stats")]
        Self::count(if valid { &self.counts.valid } else { &self.counts.recomputed });
        valid
    }

    fn settle(&self, now: Rev, changed: bool) {
        if changed {
            self.changed.set(now);
        }
        self.verified.set(now);
    }
}

/// A value derived from the state, checked against its inputs when it is read (DESIGN.md 6.3).
///
/// [`get`](Self::get) takes the current revision, a function that validates the upstream memos
/// and returns the latest revision among them (their [`Stamp::changed`]) and the state inputs,
/// and a function that computes the value. Neither function may write anything: they run under
/// `&Game`.
#[derive(Debug, Default)]
pub struct Memo<T> {
    stamp: Stamp,
    value: RefCell<T>,
}

impl<T: Clone> Clone for Memo<T> {
    fn clone(&self) -> Self {
        Self { stamp: self.stamp.clone(), value: RefCell::new(self.value.borrow().clone()) }
    }
}

impl<T: BitEq + Default> Memo<T> {
    /// A memo not yet computed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Its stamp.
    #[must_use]
    pub const fn stamp(&self) -> &Stamp {
        &self.stamp
    }

    /// The value as of revision `now`: stored, if the memo was verified at `now` or its inputs
    /// have not moved since it was last verified; otherwise recomputed.
    ///
    /// # Panics
    ///
    /// If `inputs` or `compute` reads this memo again: a cycle between memos is a bug, and the
    /// borrow of the value being validated refuses it.
    pub fn get(
        &self,
        now: Rev,
        inputs: impl FnOnce() -> Rev,
        compute: impl FnOnce() -> T,
    ) -> Ref<'_, T> {
        if self.stamp.hit(now) {
            return self.value.borrow();
        }
        // What the memo reads is its own: a computation downstream that records what it reads
        // validates against this memo's stamp instead (`unique::record`).
        record::isolated(|| {
            // Held for the whole validation: a read of this memo from inside it is a cycle, and
            // it fails here on the RefCell rather than recursing without end.
            let mut slot: RefMut<'_, T> = self.value.borrow_mut();
            let input = inputs();
            if !self.stamp.still_valid(input) {
                let fresh = compute();
                let first =
                    self.stamp.verified() == Rev::NEVER && self.stamp.changed() == Rev::NEVER;
                let differs = first || !fresh.bit_eq(&slot);
                if differs {
                    *slot = fresh;
                }
                self.stamp.settle(now, differs);
            } else {
                self.stamp.settle(now, false);
            }
        });
        self.value.borrow()
    }

    /// When the value last changed, as of its last verification.
    #[must_use]
    pub fn changed(&self) -> Rev {
        self.stamp.changed()
    }

    /// The stored value as it is, verified or not: for the cache oracle, which compares it with
    /// a cold recompute after validating.
    #[must_use]
    pub fn peek(&self) -> Ref<'_, T> {
        self.value.borrow()
    }
}

/// A [`Memo`] for a small `Copy` value, read by copy (DESIGN.md 6.3): stats, totals, flags.
#[derive(Debug, Default)]
pub struct CopyMemo<T: Copy> {
    stamp: Stamp,
    value: Cell<T>,
    busy: Cell<bool>,
}

impl<T: Copy + Default> Clone for CopyMemo<T> {
    fn clone(&self) -> Self {
        Self {
            stamp: self.stamp.clone(),
            value: Cell::new(self.value.get()),
            busy: Cell::new(false),
        }
    }
}

/// Clears a [`CopyMemo`]'s busy flag however its validation ends, a panic included.
struct Busy<'a>(&'a Cell<bool>);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl<T: Copy + BitEq + Default> CopyMemo<T> {
    /// A memo not yet computed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Its stamp.
    #[must_use]
    pub const fn stamp(&self) -> &Stamp {
        &self.stamp
    }

    /// The value as of revision `now`, as [`Memo::get`] gives it.
    ///
    /// # Panics
    ///
    /// If `inputs` or `compute` reads this memo again: a cycle between memos.
    pub fn get(&self, now: Rev, inputs: impl FnOnce() -> Rev, compute: impl FnOnce() -> T) -> T {
        if self.stamp.hit(now) {
            return self.value.get();
        }
        assert!(!self.busy.get(), "a memo was read while it was being validated: a memo cycle");
        self.busy.set(true);
        let _busy = Busy(&self.busy);
        record::isolated(|| {
            let input = inputs();
            if !self.stamp.still_valid(input) {
                let fresh = compute();
                let first =
                    self.stamp.verified() == Rev::NEVER && self.stamp.changed() == Rev::NEVER;
                let differs = first || !fresh.bit_eq(&self.value.get());
                if differs {
                    self.value.set(fresh);
                }
                self.stamp.settle(now, differs);
            } else {
                self.stamp.settle(now, false);
            }
        });
        self.value.get()
    }

    /// When the value last changed, as of its last verification.
    #[must_use]
    pub fn changed(&self) -> Rev {
        self.stamp.changed()
    }

    /// The stored value as it is, verified or not.
    #[must_use]
    pub fn peek(&self) -> T {
        self.value.get()
    }
}

#[cfg(test)]
mod tests;

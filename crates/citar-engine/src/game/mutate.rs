//! Every write to the state (DESIGN.md 6.4).
//!
//! Only this file may call `State`'s mutable accessors (`cargo xtask check`). Rule code writes
//! through `Game` in one of two ways:
//! - **setters**, which wrap the state's own: each state setter returns a `#[must_use]`
//!   [`Change`], and the wrapper passes it to `Game::changed`, which moves the revisions of
//!   what it touched and raises the work the next settle does (citizen rechecks, dirty vision
//!   sources). They are for writes whose consequences need the new state: ownership, placement,
//!   a city or unit appearing or going, a tile changing, a seat, a player's fate, the clock;
//! - **touches** (`Game::city_mut`, `player_mut`, `unit_mut`, `edit_world` and
//!   `edit_diplo`), for field edits on one entity: the touch moves
//!   the revisions its flags name *before* handing out `&mut`. The fields a touch cannot reach
//!   (a unit's owner, tile and carrier; a city's owner and tile; every tile field) move only
//!   through setters.
//!
//! Replaces Python's `invalidate` calls (`game.py:565-609`), which every writer had to remember,
//! and which cleared every cache at once.
//!
//! A dropped `Change` is a write whose caches never hear of it, so it does not compile when it
//! is dropped as a bare statement (with the workspace's `unused_must_use = "deny"`):
//!
//! ```compile_fail
//! #![deny(unused_must_use)]
//! fn tick(st: &mut citar_engine::state::State, clock: citar_engine::state::TurnClock) {
//!     st.set_clock(clock);
//! }
//! ```
//!
//! and compiles once it is passed on:
//!
//! ```
//! #![deny(unused_must_use)]
//! use citar_engine::state::{Change, State, TurnClock};
//! fn tick(st: &mut State, clock: TurnClock) -> Change {
//!     st.set_clock(clock)
//! }
//! ```
//!
//! `let _ = ...` is refused by `clippy::let_underscore_must_use`; `_ = ...`, `let _x = ...` and
//! `drop(...)` get past both lints and are review items.

use smallvec::SmallVec;

use super::Game;
use super::derive::Derived;
use super::derive::rev::{CityTouch, DiploTouch, PlayerTouch, UnitTouch, WorldTouch};
use super::pending::SightSource;
use crate::base::ids::{
    CityId, DifficultyId, ImprovementId, PlayerId, ResourceId, TerrainId, TileIdx, UnitId,
};
use crate::base::sets::FeatureSet;
use crate::rules::defs::Route;
use crate::state::cities::City;
use crate::state::config::GameConfig;
use crate::state::diplo::{Diplomacy, Relation};
use crate::state::map::{BuildQueue, BuildStep};
use crate::state::players::{AutoDecision, AutoOverrides, Controller, Handicap, Player};
use crate::state::units::Unit;
use crate::state::world::World;
use crate::state::{Change, Changes, StateError, TileClaim, TurnClock};
use crate::unique::CondDeps;

#[allow(
    dead_code,
    reason = "every write the rule systems make goes through here; they land from 1b-02 to 1c-10"
)]
impl Game {
    /// Hands one change to the caches (DESIGN.md 6.4): moves the revisions of what it touched,
    /// then asks the derived layer what it means ([`Derived::on`], which only reads) and queues
    /// that work for the next settle.
    pub(crate) fn changed(&mut self, ch: Change) {
        self.dv.revs.on_change(&self.st, &ch);
        self.dv.civ.track(&ch);
        self.dv.stats.track(&ch);
        let react = self.dv.on(&self.st, self.rules, &ch);
        for c in react.recheck {
            self.pending.flag_city(c);
        }
        for s in react.sight {
            self.pending.flag_sight(s);
        }
        self.recheck_civs(&ch);
    }

    /// Flags for a citizen recheck the cities a change concerns through their owners' indexes,
    /// supplies and conditionals, beyond the tiles [`Derived::on`] names (DESIGN.md 6.7): a
    /// city-state's bonuses (contact, war, an ally, its fate), a unit made or lost (the supply),
    /// a resource's tile, a friendship (great person points), a city appearing or changing
    /// hands, and the turn: what its conditionals read, and friendships running out.
    fn recheck_civs(&mut self, ch: &Change) {
        let cs = |g: &Self, p: PlayerId| g.st.player(p).is_some_and(Player::is_city_state);
        match *ch {
            Change::Turn => {
                self.recheck_for(CondDeps::TURN | CondDeps::CHANCE, None);
                if self.dv.stats.deps().friendship {
                    self.flag_friendships_ended();
                }
            }
            Change::PlayerAlive(_) => {
                for c in self.st.cities().ids() {
                    self.pending.flag_city(c);
                }
            }
            Change::Alliance { cs: q, old, new } => {
                for p in [Some(q), old, new].into_iter().flatten() {
                    self.flag_cities_of(p);
                }
            }
            Change::War { a, b } | Change::Met { a, b } => {
                if cs(self, a) || cs(self, b) {
                    self.flag_cities_of(a);
                    self.flag_cities_of(b);
                }
                self.recheck_for(CondDeps::WAR, None);
            }
            Change::Diplo { a, b } => {
                self.flag_cities_of(a);
                self.flag_cities_of(b);
                self.recheck_for(CondDeps::WAR, None);
            }
            Change::UnitPlaced { owner, from, .. } => {
                if from.is_none() {
                    self.flag_cities_of(owner);
                }
                self.recheck_for(CondDeps::UNIT_SET, None);
            }
            Change::UnitRemoved { owner, .. } => {
                self.flag_cities_of(owner);
                self.recheck_for(CondDeps::UNIT_SET, None);
            }
            Change::UnitOwner { old, new, .. } => {
                self.flag_cities_of(old);
                self.flag_cities_of(new);
                self.recheck_for(CondDeps::UNIT_SET, None);
            }
            Change::TileInput(t) | Change::TileHeight(t) => {
                if let Some(tile) = self.st.tiles().get(t)
                    && tile.resource().is_some()
                    && let Some(p) = tile.owner()
                {
                    self.flag_cities_of(p);
                }
                self.recheck_for(CondDeps::MAP, None);
            }
            Change::TileOwner { t, old, new } => {
                if self.st.tiles().get(t).is_some_and(|x| x.resource().is_some()) {
                    for p in [old.owner, new.owner].into_iter().flatten() {
                        self.flag_cities_of(p);
                    }
                }
                self.recheck_for(CondDeps::MAP, None);
            }
            Change::CityAdded(c) => {
                if let Some(p) = self.st.cities().get(c).map(City::owner) {
                    self.flag_cities_of(p);
                }
                self.recheck_for(CondDeps::CITY_COUNT, None);
            }
            Change::CityRemoved { owner, .. } => {
                self.flag_cities_of(owner);
                self.recheck_for(CondDeps::CITY_COUNT, None);
            }
            Change::CityOwner { old, new, .. } => {
                self.flag_cities_of(old);
                self.flag_cities_of(new);
                self.recheck_for(CondDeps::CITY_COUNT, None);
            }
            Change::CityTiles(_)
            | Change::Talks { .. }
            | Change::Spy(_)
            | Change::Seat(_)
            | Change::Clock
            | Change::Names => {}
        }
    }

    /// Hands every change of a write to the caches, in the order they happened.
    pub(crate) fn changed_all(&mut self, chs: Changes) {
        for ch in chs {
            self.changed(ch);
        }
    }

    // ---- Tiles ------------------------------------------------------------------------------

    /// Gives a tile to a player and a city, or to nobody.
    pub(crate) fn set_tile_owner(
        &mut self,
        t: TileIdx,
        claim: TileClaim,
    ) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_owner(t, claim)?;
        self.changed(ch);
        Ok(())
    }

    /// Sets a tile's improvement.
    pub(crate) fn set_improvement(
        &mut self,
        t: TileIdx,
        improvement: Option<ImprovementId>,
    ) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_improvement(t, improvement)?;
        self.changed(ch);
        Ok(())
    }

    /// Sets a tile's route, which may change the connections between cities.
    pub(crate) fn set_route(&mut self, t: TileIdx, route: Option<Route>) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_route(t, route)?;
        self.dv.revs.touch_routes();
        self.changed(ch);
        Ok(())
    }

    /// Sets whether a tile's route and improvement are pillaged.
    pub(crate) fn set_pillaged(
        &mut self,
        t: TileIdx,
        route: bool,
        improvement: bool,
    ) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_pillaged(t, route, improvement)?;
        self.dv.revs.touch_routes();
        self.changed(ch);
        Ok(())
    }

    /// Sets a tile's features.
    pub(crate) fn set_features(&mut self, t: TileIdx, f: FeatureSet) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_features(t, f)?;
        self.changed(ch);
        Ok(())
    }

    /// Sets a tile's base terrain.
    pub(crate) fn set_terrain(&mut self, t: TileIdx, terrain: TerrainId) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_terrain(t, terrain)?;
        self.changed(ch);
        Ok(())
    }

    /// Sets a tile's natural wonder.
    pub(crate) fn set_wonder(
        &mut self,
        t: TileIdx,
        wonder: Option<TerrainId>,
    ) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_wonder(t, wonder)?;
        self.changed(ch);
        Ok(())
    }

    /// Sets a tile's resource and its deposit size.
    pub(crate) fn set_resource(
        &mut self,
        t: TileIdx,
        resource: Option<ResourceId>,
        amount: u8,
    ) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_resource(t, resource, amount)?;
        self.changed(ch);
        Ok(())
    }

    /// Sets a tile's river mask.
    pub(crate) fn set_river(&mut self, t: TileIdx, mask: u8) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_river(t, mask)?;
        self.changed(ch);
        Ok(())
    }

    /// Appends a step to a tile's build queue.
    pub(crate) fn push_build(&mut self, t: TileIdx, step: BuildStep) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().push_build(t, step)?;
        self.changed(ch);
        Ok(())
    }

    /// Takes the front step off a tile's build queue.
    pub(crate) fn pop_build(&mut self, t: TileIdx) -> Result<Option<BuildStep>, StateError> {
        let (step, ch) = self.st.tiles_mut().pop_build(t)?;
        self.changed(ch);
        Ok(step)
    }

    /// Replaces a tile's build queue.
    pub(crate) fn set_builds(&mut self, t: TileIdx, queue: BuildQueue) -> Result<(), StateError> {
        let ch = self.st.tiles_mut().set_builds(t, queue)?;
        self.changed(ch);
        Ok(())
    }

    // ---- Units ------------------------------------------------------------------------------

    /// Puts a new unit on the map.
    pub(crate) fn spawn_unit(&mut self, u: Unit) -> Result<(), StateError> {
        let ch = self.st.units_mut().spawn(u)?;
        self.changed(ch);
        Ok(())
    }

    /// Takes a unit out of the game (`game.py:744-764`); the units it carried stay, no longer
    /// carried.
    pub(crate) fn despawn_unit(&mut self, u: UnitId) -> Result<Unit, StateError> {
        let (unit, chs) = self.st.units_mut().despawn(u)?;
        self.changed_all(chs);
        Ok(unit)
    }

    /// Moves a unit, and what it carries, without movement rules (`game.py:766-785`).
    pub(crate) fn relocate_unit(&mut self, u: UnitId, to: TileIdx) -> Result<(), StateError> {
        let chs = self.st.units_mut().relocate(u, to)?;
        self.changed_all(chs);
        Ok(())
    }

    /// Hands a unit to another player; its orders are the caller's to reset.
    pub(crate) fn set_unit_owner(&mut self, u: UnitId, new: PlayerId) -> Result<(), StateError> {
        let ch = self.st.units_mut().set_owner(u, new)?;
        self.changed(ch);
        Ok(())
    }

    /// Puts a unit on a carrier on its tile.
    pub(crate) fn board_unit(&mut self, u: UnitId, carrier: UnitId) -> Result<(), StateError> {
        let ch = self.st.units_mut().board(u, carrier)?;
        self.changed(ch);
        Ok(())
    }

    /// Takes a unit off its carrier.
    pub(crate) fn unboard_unit(&mut self, u: UnitId) -> Result<(), StateError> {
        let ch = self.st.units_mut().unboard(u)?;
        self.changed(ch);
        Ok(())
    }

    // ---- Cities -----------------------------------------------------------------------------

    /// Adds a new city; the tiles it claims are claimed with
    /// [`set_tile_owner`](Self::set_tile_owner).
    pub(crate) fn add_city(&mut self, city: City) -> Result<(), StateError> {
        let ch = self.st.cities_mut().found(city)?;
        self.changed(ch);
        Ok(())
    }

    /// Takes a city out of the game; its tiles are released with
    /// [`set_tile_owner`](Self::set_tile_owner).
    pub(crate) fn remove_city(&mut self, c: CityId) -> Result<City, StateError> {
        let (city, ch) = self.st.cities_mut().remove(c)?;
        self.changed(ch);
        Ok(city)
    }

    /// Hands a city and its tiles to another player (`conquest.py:60-80`).
    pub(crate) fn transfer_city(&mut self, c: CityId, new: PlayerId) -> Result<(), StateError> {
        let chs = self.st.transfer_city(c, new)?;
        self.changed_all(chs);
        Ok(())
    }

    // ---- Players ----------------------------------------------------------------------------

    /// Eliminates a player that holds no cities (`victory.py:380-392`).
    pub(crate) fn kill_player(&mut self, p: PlayerId) -> Result<(), StateError> {
        let turn = self.st.clock().turn;
        let chs = self.st.kill_player(p, turn)?;
        self.changed_all(chs);
        Ok(())
    }

    /// Brings an eliminated player back (`conquest.py:262-265`).
    pub(crate) fn revive_player(&mut self, p: PlayerId) -> Result<(), StateError> {
        let ch = self.st.revive_player(p)?;
        self.changed(ch);
        Ok(())
    }

    /// Hands a seat to another controller (`state.py:282-296`).
    pub(crate) fn set_seat_controller(
        &mut self,
        p: PlayerId,
        controller: Controller,
        handicap: Option<Handicap>,
        auto: AutoOverrides,
    ) -> Result<(), StateError> {
        let ch = self.st.set_controller(p, controller, handicap, auto)?;
        self.changed(ch);
        Ok(())
    }

    /// Changes one automatic decision of a seat for now.
    pub(crate) fn set_auto_decision(
        &mut self,
        p: PlayerId,
        d: AutoDecision,
        on: bool,
    ) -> Result<(), StateError> {
        let ch = self.st.set_auto_decision(p, d, on)?;
        self.changed(ch);
        Ok(())
    }

    /// Sets a seat's own difficulty, or `None` for the game's.
    pub(crate) fn set_seat_difficulty(
        &mut self,
        p: PlayerId,
        d: Option<DifficultyId>,
    ) -> Result<(), StateError> {
        let ch = self.st.set_seat_difficulty(p, d)?;
        self.changed(ch);
        Ok(())
    }

    /// Sets a city-state's ally.
    pub(crate) fn set_ally(
        &mut self,
        cs: PlayerId,
        ally: Option<PlayerId>,
    ) -> Result<(), StateError> {
        let ch = self.st.set_ally(cs, ally)?;
        self.changed(ch);
        Ok(())
    }

    /// Flags the cities of both sides of each declared friendship that ran out as the turn began:
    /// their great person points lose its bonus (`great_people.city_gpp_bonus`,
    /// `great_people.py:24-31`), which their specialists' ranking reads.
    fn flag_friendships_ended(&mut self) {
        let turn = self.st.clock().turn;
        let ended: SmallVec<[(PlayerId, PlayerId); 4]> = self
            .st
            .diplo()
            .relations()
            .pairs()
            .filter(|(_, _, r)| r.friendship_until.checked_add(1) == Some(turn))
            .map(|(a, b, _)| (a, b))
            .collect();
        for (a, b) in ended {
            self.flag_cities_of(a);
            self.flag_cities_of(b);
        }
    }

    /// Sets the clock: a [`Change::Turn`] only when the turn number moves.
    pub(crate) fn set_clock(&mut self, clock: TurnClock) {
        let ch = self.st.set_clock(clock);
        self.changed(ch);
    }

    /// Sets a major's influence with a city-state. Influence moves every turn, so it moves no
    /// major's unique index, except the one whose friend level it flips: the friend bonuses are
    /// in the index (`city_states.bonus_umaps`, `city_states.py:160-173`). Every write of
    /// influence goes through here; a `CITY_STATE` touch that wrote it would leave that index
    /// stale, which the cache oracle reports.
    pub(crate) fn set_influence(
        &mut self,
        cs: PlayerId,
        major: PlayerId,
        value: f64,
    ) -> Result<(), StateError> {
        let was = self.is_friend_level(cs, major);
        self.recheck_for(CondDeps::INFLUENCE, None);
        let slot = self
            .st
            .players_mut()
            .get_mut(cs)
            .and_then(|p| p.city_state.as_deref_mut())
            .and_then(|d| d.influence.get_mut(major))
            .ok_or(StateError::NoSuchPlayer(cs))?;
        *slot = value;
        self.dv.revs.touch_player(cs, PlayerTouch::CITY_STATE);
        if self.is_friend_level(cs, major) != was {
            self.dv.revs.touch_player(major, PlayerTouch::INDEX);
            self.flag_cities_of(major);
        }
        Ok(())
    }

    // ---- Relations --------------------------------------------------------------------------

    /// Edits the relation of two players.
    pub(crate) fn update_relation(
        &mut self,
        a: PlayerId,
        b: PlayerId,
        f: impl FnOnce(&mut Relation),
    ) -> Result<(), StateError> {
        let chs = self.st.diplo_mut().update(a, b, f)?;
        self.changed_all(chs);
        Ok(())
    }

    /// Records that two players have met, and nothing more: [`Game::make_contact`] is the rule.
    pub(crate) fn set_met(&mut self, a: PlayerId, b: PlayerId) -> Result<(), StateError> {
        let chs = self.st.diplo_mut().meet(a, b)?;
        self.changed_all(chs);
        Ok(())
    }

    // ---- Touches ----------------------------------------------------------------------------

    /// A city's fields, after moving the revisions `t` names (DESIGN.md 6.4). `CORE`,
    /// `BUILDINGS` and `WORK` flag the city for a citizen recheck. `None` if there is no such
    /// city.
    pub(crate) fn city_mut(&mut self, c: CityId, t: CityTouch) -> Option<&mut City> {
        let owner = self.st.cities().get(c)?.owner();
        self.dv.revs.touch_city(c, owner, t);
        if t.intersects(
            CityTouch::CORE | CityTouch::BUILDINGS | CityTouch::WORK | CityTouch::RELIGION,
        ) {
            self.pending.flag_city(c);
        }
        if t.contains(CityTouch::BUILDINGS) {
            // Its buildings are in its owner's index, which every city of its owner reads.
            self.flag_cities_of(owner);
            self.recheck_for(CondDeps::GLOBAL_BUILDINGS | CondDeps::CIV_BUILDINGS, None);
        }
        // Its stored food and culture, which a conditional about the city reads, and no other
        // city's ranking: a tile's city conditionals are its working city's.
        if t.contains(CityTouch::STOCKS) && self.dv.stats.deps().citizens.contains(CondDeps::CITY) {
            self.pending.flag_city(c);
        }
        self.st.cities_mut().get_mut(c)
    }

    /// Writes where a city's citizens are, as the settle's reassignment decided, and marks the
    /// city assigned by this engine (DESIGN.md 6.8). It moves the city's `work` revisions but
    /// flags nothing: the settle flags the cities the change concerns.
    pub(crate) fn set_citizens(&mut self, c: CityId, a: super::cities::citizens::Assignment) {
        let Some(owner) = self.st.cities().get(c).map(City::owner) else { return };
        self.dv.revs.touch_city(c, owner, CityTouch::WORK);
        if let Some(x) = self.st.cities_mut().get_mut(c) {
            x.worked = a.worked;
            x.locked = a.locked;
            x.specialists = a.specialists;
            x.citizens_settled = true;
        }
    }

    /// Flags every city of `p` for a citizen recheck.
    pub(crate) fn flag_cities_of(&mut self, p: PlayerId) {
        for &c in self.st.cities().of(p) {
            self.pending.flag_city(c);
        }
    }

    /// Flags cities for a citizen recheck when a write moved one of `classes` and the
    /// conditionals or filters of the uniques citizens read (tile yields, city stats, great
    /// person points) read one of them: the cities of `civ` for its own classes, every city
    /// otherwise.
    pub(crate) fn recheck_for(&mut self, classes: CondDeps, civ: Option<PlayerId>) {
        if !self.dv.stats.deps().citizens.intersects(classes) {
            return;
        }
        match civ {
            Some(p) => self.flag_cities_of(p),
            None => {
                for c in self.st.cities().ids() {
                    self.pending.flag_city(c);
                }
            }
        }
    }

    /// A player's fields, after moving the revisions `t` names. A seat, and whether the player
    /// is alive, change through setters instead.
    pub(crate) fn player_mut(&mut self, p: PlayerId, t: PlayerTouch) -> Option<&mut Player> {
        self.st.player(p)?;
        self.dv.revs.touch_player(p, t);
        // What its cities' yields read: its index (techs, policies, beliefs), a golden age (a
        // gold more on a tile that has some), its capital. Its gold, culture and faith only
        // through conditionals that read them.
        if t.intersects(
            PlayerTouch::INDEX
                | PlayerTouch::POLICIES
                | PlayerTouch::RELIGION
                | PlayerTouch::GOLDEN_AGE
                | PlayerTouch::CAPITAL,
        ) {
            self.flag_cities_of(p);
        }
        if t.contains(PlayerTouch::STOCKS) {
            self.recheck_for(CondDeps::STOCKS, Some(p));
        }
        if t.contains(PlayerTouch::RESEARCH) {
            self.recheck_for(CondDeps::RESEARCH_QUEUE, Some(p));
        }
        if t.contains(PlayerTouch::POLICIES) {
            self.recheck_for(CondDeps::GLOBAL_POLICIES, None);
        }
        if t.contains(PlayerTouch::RELIGION) {
            self.recheck_for(CondDeps::RELIGION_STATE | CondDeps::GLOBAL_POLICIES, None);
        }
        if t.contains(PlayerTouch::CAPITAL) {
            self.recheck_for(CondDeps::CITY_COUNT, None);
        }
        if t.contains(PlayerTouch::CITY_STATE) {
            self.recheck_for(CondDeps::INFLUENCE, None);
        }
        self.st.players_mut().get_mut(p)
    }

    /// Sets how many turns of a golden age a civilization has left: a `GOLDEN_AGE` touch when
    /// the golden age begins or ends, which tile yields, city stats and citizens read, and a
    /// `STOCKS` touch for a turn counted down within it.
    pub(crate) fn set_golden_age_turns(&mut self, p: PlayerId, turns: i32) {
        let Some(was) = self.st.player(p).map(|x| x.econ.golden_age_turns) else { return };
        let touch = if (was > 0) == (turns > 0) {
            PlayerTouch::STOCKS
        } else {
            PlayerTouch::STOCKS | PlayerTouch::GOLDEN_AGE
        };
        if let Some(x) = self.player_mut(p, touch) {
            x.econ.golden_age_turns = turns;
        }
    }

    /// A unit's fields, after moving the revisions `t` names; `SIGHT` marks it a dirty vision
    /// source. Its owner, tile and carrier change through setters instead.
    pub(crate) fn unit_mut(&mut self, u: UnitId, t: UnitTouch) -> Option<&mut Unit> {
        let owner = self.st.units().get(u)?.owner();
        self.dv.revs.touch_unit(u, owner, t);
        if t.intersects(UnitTouch::CORE | UnitTouch::BASE) {
            self.recheck_for(CondDeps::UNIT_SET, None);
        }
        if t.contains(UnitTouch::BASE) {
            // An upgrade may change what its owner's supply uses.
            self.flag_cities_of(owner);
        }
        if t.contains(UnitTouch::SIGHT) {
            self.pending.flag_sight(SightSource::Unit(u));
        }
        self.st.units_mut().get_mut(u)
    }

    /// The world's fields (religions, wonders, the UN, camps), after moving the revisions `t`
    /// names.
    pub(crate) fn edit_world(&mut self, t: WorldTouch) -> &mut World {
        self.dv.revs.touch_world(t);
        if t.contains(WorldTouch::RELIGIONS) {
            // Founder beliefs are in every civilization's index, follower beliefs in every city.
            for c in self.st.cities().ids() {
                self.pending.flag_city(c);
            }
        }
        if t.contains(WorldTouch::WONDERS) {
            self.recheck_for(CondDeps::GLOBAL_BUILDINGS, None);
        }
        self.st.world_mut()
    }

    /// The diplomacy's lists (deals, negotiations, opinions), after moving the revisions `t`
    /// names. A relation changes through [`update_relation`](Self::update_relation), which
    /// reports what moved.
    pub(crate) fn edit_diplo(&mut self, t: DiploTouch) -> &mut Diplomacy {
        self.dv.revs.touch_diplo(t);
        if t.contains(DiploTouch::DEALS) {
            // Deals trade resources, which are in their parties' indexes.
            for c in self.st.cities().ids() {
                self.pending.flag_city(c);
            }
        }
        self.st.diplo_mut()
    }

    /// Edits the settings. Nearly every cache reads them, so every cache starts cold again: a
    /// settings edit is a scenario's, never a turn's. What follows from them does too: yields
    /// and citizen weights read the difficulty, the speed and the rules switched on, so every
    /// city rechecks its citizens and every player's sight is brought up to date at the next
    /// settle, as a seat change does for one player.
    pub(crate) fn edit_config(&mut self, f: impl FnOnce(&mut GameConfig)) {
        f(self.st.config_mut());
        let now = self.dv.revs.now();
        self.dv = Derived::new(self.rules, &self.st);
        // Revisions never go back: a host's ETag, and anything keyed on a revision, must see
        // every input move on.
        self.dv.revs = super::derive::rev::Revs::after(&self.st, now);
        for c in self.st.cities().ids() {
            self.pending.flag_city(c);
        }
        for p in self.st.players().ids() {
            self.pending.flag_sight(SightSource::Civ(p));
        }
    }
}

/// The doors round the setters and touches, for the tests that corrupt a game on purpose to
/// prove the checks catch it. Nothing moves a revision or raises work through them.
#[cfg(all(test, feature = "embedded-ruleset"))]
impl Game {
    pub(crate) fn corrupt_units(&mut self) -> &mut crate::state::units::Units {
        self.st.units_mut()
    }

    pub(crate) fn corrupt_cities(&mut self) -> &mut crate::state::cities::Cities {
        self.st.cities_mut()
    }

    pub(crate) fn corrupt_players(&mut self) -> &mut crate::base::sets::PlayerVec<Player> {
        self.st.players_mut()
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::game::core::testing;
    use crate::game::derive::rev::Rev;
    use crate::unique::{CondDeps, Ctx};

    fn cid(n: u32) -> CityId {
        CityId::new(n).unwrap_or(CityId::FIRST)
    }

    #[test]
    fn a_setter_moves_the_revisions_of_what_it_touched_and_nothing_else() -> Result<(), StateError>
    {
        let mut g = testing::duel();
        let t = TileIdx(12);
        let before = (g.dv.revs.tile(t), g.dv.revs.tile(TileIdx(13)), g.dv.revs.turn);
        g.set_improvement(t, None)?;
        assert!(g.dv.revs.tile(t) > before.0);
        assert_eq!((g.dv.revs.tile(TileIdx(13)), g.dv.revs.turn), (before.1, before.2));
        assert_eq!(
            g.dv.revs.tile_log.since(Rev::START).map(Iterator::collect::<Vec<_>>),
            Some(vec![t])
        );
        Ok(())
    }

    #[test]
    fn a_touch_flags_the_city_and_only_buildings_move_its_owners_index() -> Result<(), StateError> {
        let mut g = testing::duel();
        g.add_city(City::new(cid(1), "Roma".into(), PlayerId(0), TileIdx(22), 1))?;
        g.pending = super::super::pending::PendingWork::new();
        let before = g.dv.revs.civ(PlayerId(0));
        if let Some(c) = g.city_mut(cid(1), CityTouch::CORE) {
            c.pop = 3;
        }
        assert_eq!(g.dv.revs.civ(PlayerId(0)), before, "growth is not in the index");
        assert_eq!(g.pending.take_recheck(), [cid(1)]);
        let core = g.dv.revs.city(cid(1)).core;
        g.city_mut(cid(1), CityTouch::BUILDINGS);
        let after = g.dv.revs.civ(PlayerId(0));
        assert!(after.index > before.index && after.buildings > before.buildings);
        assert!(g.dv.revs.city(cid(1)).core > core, "buildings imply core");
        assert_eq!(g.pending.take_recheck(), [cid(1)]);
        assert!(g.city_mut(cid(9), CityTouch::CORE).is_none());
        Ok(())
    }

    #[test]
    fn a_settings_edit_starts_every_cache_cold_and_moves_on() {
        let mut g = testing::duel();
        let before = g.rev();
        g.edit_config(|c| c.espionage = false);
        assert!(g.rev() > before);
        assert!(!g.espionage_enabled());
    }

    #[test]
    fn every_conditional_class_maps_to_what_moves_it() -> Result<(), StateError> {
        let mut g = testing::duel();
        let p = PlayerId(0);
        g.add_city(City::new(cid(1), "Roma".into(), p, TileIdx(22), 1))?;
        g.set_tile_owner(TileIdx(22), TileClaim::city(p, cid(1)))?;
        let unit = Unit::new(UnitId::FIRST, crate::base::ids::BaseUnitId(0), p, TileIdx(22), 1);
        g.st.ids_mut().unit = 2;
        g.spawn_unit(unit)?;
        let ctx_city =
            Ctx { civ: Some(p), city: Some(cid(1)), tile: Some(TileIdx(22)), ..Ctx::default() };
        let ctx_tile = Ctx::tile(Some(p), TileIdx(22));
        let ctx_unit = Ctx {
            civ: Some(p),
            unit: Some(UnitId::FIRST),
            tile: Some(TileIdx(22)),
            ..Ctx::default()
        };
        fn next_turn(g: &mut Game) {
            let clock = *g.st.clock();
            g.set_clock(TurnClock { turn: clock.turn + 1, ..clock });
        }
        // Each class, with a write that must move it.
        type Write = Box<dyn Fn(&mut Game)>;
        let cases: Vec<(CondDeps, Ctx, Write)> = vec![
            (CondDeps::TURN, Ctx::civ(p), Box::new(next_turn)),
            (CondDeps::CHANCE, Ctx::civ(p), Box::new(next_turn)),
            (
                CondDeps::HAPPINESS_SEEN,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(0), PlayerTouch::HAPPINESS_SEEN);
                }),
            ),
            (
                CondDeps::STOCKS,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(0), PlayerTouch::STOCKS);
                }),
            ),
            (
                CondDeps::GOLDEN_AGE,
                Ctx::civ(p),
                Box::new(|g| g.set_golden_age_turns(PlayerId(0), 3)),
            ),
            // The connectivity memo's stamp: Rome gets a capital, and a network from it.
            (
                CondDeps::CONNECTED,
                Ctx::civ(p),
                Box::new(|g| {
                    if let Some(x) = g.player_mut(PlayerId(0), PlayerTouch::CAPITAL) {
                        x.capital = Some(cid(1));
                    }
                }),
            ),
            // The supply memo's stamp: a Swordsman needs Iron, a line of Rome's supply.
            (
                CondDeps::RESOURCES,
                Ctx::civ(p),
                Box::new(|g| {
                    testing::unit(g, PlayerId(0), "Swordsman", TileIdx(30));
                }),
            ),
            (
                CondDeps::WAR,
                Ctx::civ(p),
                Box::new(|g| {
                    let _ok = g.update_relation(PlayerId(0), PlayerId(1), |r| r.war = true);
                }),
            ),
            (
                CondDeps::ERA,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(0), PlayerTouch::INDEX);
                }),
            ),
            (
                CondDeps::TECHS,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(0), PlayerTouch::INDEX);
                }),
            ),
            (
                CondDeps::POLICIES,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(0), PlayerTouch::INDEX);
                }),
            ),
            (
                CondDeps::RESEARCH_QUEUE,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(0), PlayerTouch::RESEARCH);
                }),
            ),
            (
                CondDeps::RELIGION_STATE,
                Ctx::civ(p),
                Box::new(|g| {
                    g.edit_world(WorldTouch::RELIGIONS);
                }),
            ),
            (
                CondDeps::RELIGION_STATE,
                Ctx::civ(p),
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::RELIGION);
                }),
            ),
            (
                CondDeps::CIV_BUILDINGS,
                Ctx::civ(p),
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::BUILDINGS);
                }),
            ),
            (
                CondDeps::GLOBAL_BUILDINGS,
                Ctx::civ(p),
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::BUILDINGS);
                }),
            ),
            (
                CondDeps::GLOBAL_POLICIES,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(1), PlayerTouch::POLICIES);
                }),
            ),
            (
                CondDeps::GLOBAL_POLICIES,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(1), PlayerTouch::RELIGION);
                }),
            ),
            (
                CondDeps::CITY_COUNT,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(1), PlayerTouch::CAPITAL);
                }),
            ),
            (
                CondDeps::CITY_COUNT,
                Ctx::civ(p),
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::RELIGION);
                }),
            ),
            (
                CondDeps::CITY_COUNT,
                Ctx::civ(p),
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::CORE);
                }),
            ),
            (
                CondDeps::UNIT_SET,
                Ctx::civ(p),
                Box::new(|g| {
                    let _ok = g.relocate_unit(UnitId::FIRST, TileIdx(23));
                }),
            ),
            (
                CondDeps::SEAT,
                Ctx::civ(p),
                Box::new(|g| {
                    let _ok = g.set_seat_difficulty(PlayerId(0), None);
                }),
            ),
            (CondDeps::CONFIG, Ctx::civ(p), Box::new(|g| g.edit_config(|c| c.religion = true))),
            (
                CondDeps::MAP,
                Ctx::civ(p),
                Box::new(|g| {
                    let _ok = g.set_route(TileIdx(40), None);
                }),
            ),
            (
                CondDeps::CITY,
                ctx_city,
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::STOCKS);
                }),
            ),
            (
                CondDeps::CITY,
                ctx_tile,
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::RELIGION);
                }),
            ),
            (
                CondDeps::UNIT,
                ctx_unit,
                Box::new(|g| {
                    g.unit_mut(UnitId::FIRST, UnitTouch::MOVES);
                }),
            ),
            (
                CondDeps::TILE,
                ctx_tile,
                Box::new(|g| {
                    let _ok = g.set_improvement(TileIdx(22), None);
                }),
            ),
            (
                CondDeps::TILE,
                ctx_tile,
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::WORK);
                }),
            ),
            // Influence, which the friendly civilization and land leaves read, even another
            // major's and below the friend level.
            (
                CondDeps::INFLUENCE,
                Ctx::civ(p),
                Box::new(|g| {
                    let _ok = g.set_influence(PlayerId(2), PlayerId(1), 5.0);
                }),
            ),
            // Last: Greece, at war with Rome since the WAR case, dies, and the dead are at war
            // with no one (`game.py:673-675`).
            (
                CondDeps::WAR,
                Ctx::civ(p),
                Box::new(|g| {
                    let _ok = g.kill_player(PlayerId(1));
                    assert!(!g.is_at_war_any(PlayerId(0)));
                }),
            ),
        ];
        for (i, (class, ctx, write)) in cases.iter().enumerate() {
            let before = super::super::derive::civ::cond(&g, *class, ctx);
            write(&mut g);
            let after = super::super::derive::civ::cond(&g, *class, ctx);
            assert!(after > before, "case {i}: {class:?} did not move");
        }
        // Every class is covered, and the two halves partition them.
        let covered = cases.iter().fold(CondDeps::COMBAT, |d, (c, _, _)| d | *c);
        assert_eq!(covered, CondDeps::all());
        assert_eq!(CondDeps::LOCAL | CondDeps::CIV_LEVEL, CondDeps::all());
        assert!(CondDeps::LOCAL.intersection(CondDeps::CIV_LEVEL).is_empty());
        Ok(())
    }

    #[test]
    fn what_flags_citizens_and_what_does_not() -> Result<(), StateError> {
        let mut g = testing::duel();
        let (rome, greece, geneva) = (PlayerId(0), PlayerId(1), PlayerId(2));
        let roma = testing::city(&mut g, rome, TileIdx(22), "Roma");
        let athens = testing::city(&mut g, greece, TileIdx(57), "Athens");
        g.settle();
        let deps = g.dv.stats.deps().citizens;
        // The shipped ruleset's ranking reads no influence, treasury or turn: none of these
        // flags a city.
        assert!(!deps.intersects(CondDeps::INFLUENCE | CondDeps::STOCKS | CondDeps::TURN));
        g.set_influence(geneva, rome, 10.0)?;
        if let Some(x) = g.player_mut(rome, PlayerTouch::STOCKS) {
            x.econ.gold += 5.0;
        }
        let clock = *g.st.clock();
        g.set_clock(TurnClock { turn: clock.turn + 1, ..clock });
        assert_eq!(g.pending.take_recheck(), []);
        // A city's stored food, which conditionals about the city read ("[in capital]" reads
        // the city class): that city alone.
        assert!(deps.contains(CondDeps::CITY));
        if let Some(x) = g.city_mut(athens, CityTouch::STOCKS) {
            x.food += 1.0;
        }
        assert_eq!(g.pending.take_recheck(), [athens]);
        // A golden age beginning moves the yields of its tiles; a turn of it counted down does not.
        g.set_golden_age_turns(rome, 5);
        assert_eq!(g.pending.take_recheck(), [roma]);
        g.set_golden_age_turns(rome, 4);
        assert_eq!(g.pending.take_recheck(), []);
        // A declared friendship running out, where a unique gives great person points for one.
        g.update_relation(rome, greece, |r| r.friendship_until = clock.turn + 1)?;
        drop(g.pending.take_recheck());
        g.set_clock(TurnClock { turn: clock.turn + 2, ..clock });
        let flagged = g.pending.take_recheck();
        if g.dv.stats.deps().friendship {
            assert_eq!(flagged, [roma, athens]);
        } else {
            assert_eq!(flagged, []);
        }
        Ok(())
    }

    #[test]
    fn classes_that_did_not_move_stay_put() -> Result<(), StateError> {
        let mut g = testing::duel();
        let p = PlayerId(0);
        let ctx = Ctx::civ(p);
        let techs = g.dv.revs.cond(&g.st, CondDeps::TECHS, &ctx);
        let other = g.dv.revs.cond(&g.st, CondDeps::TECHS, &Ctx::civ(PlayerId(1)));
        g.player_mut(PlayerId(1), PlayerTouch::STOCKS);
        g.set_improvement(TileIdx(5), None)?;
        assert_eq!(g.dv.revs.cond(&g.st, CondDeps::TECHS, &ctx), techs);
        assert_eq!(g.dv.revs.cond(&g.st, CondDeps::TECHS, &Ctx::civ(PlayerId(1))), other);
        // No civilization in context: the classes about it cannot move.
        assert_eq!(g.dv.revs.cond(&g.st, CondDeps::STOCKS, &Ctx::default()), Rev::START);
        Ok(())
    }

    #[test]
    fn what_moves_every_turn_leaves_the_indexes_alone() -> Result<(), StateError> {
        let mut g = testing::duel();
        let (rome, greece, geneva) = (PlayerId(0), PlayerId(1), PlayerId(2));
        let roma = testing::city(&mut g, rome, TileIdx(22), "Roma");
        g.update_relation(rome, geneva, |r| r.met = true)?;
        let indexes = CondDeps::TECHS
            | CondDeps::POLICIES
            | CondDeps::ERA
            | CondDeps::GLOBAL_POLICIES
            | CondDeps::CIV_BUILDINGS
            | CondDeps::GLOBAL_BUILDINGS;
        let read = |g: &Game, d: CondDeps, p: PlayerId| g.dv.revs.cond(&g.st, d, &Ctx::civ(p));
        let before = (read(&g, indexes, rome), read(&g, indexes, greece));
        let war = read(&g, CondDeps::WAR, rome);
        let influence = read(&g, CondDeps::INFLUENCE, rome);
        let turn = read(&g, CondDeps::TURN, rome);
        // Growth, a queue edit, a heal: not the buildings.
        if let Some(c) = g.city_mut(roma, CityTouch::CORE) {
            c.pop += 1;
        }
        // Whose turn it is, and a research agreement's science.
        let clock = *g.st.clock();
        g.set_clock(TurnClock { current: greece, turn_started: true, ..clock });
        g.update_relation(rome, greece, |r| r.ra_science[0] += 3)?;
        assert_eq!(read(&g, CondDeps::TURN, rome), turn);
        assert_eq!(read(&g, CondDeps::WAR, rome), war);
        // Influence below the friend level, whoever's.
        g.set_influence(geneva, rome, 10.0)?;
        g.set_influence(geneva, greece, 20.0)?;
        assert_eq!(read(&g, CondDeps::WAR, rome), war, "influence is not war");
        assert!(read(&g, CondDeps::INFLUENCE, rome) > influence);
        assert_eq!((read(&g, indexes, rome), read(&g, indexes, greece)), before);
        // A tech is Rome's alone; a policy is what every civilization has adopted too.
        g.player_mut(rome, PlayerTouch::INDEX);
        assert!(read(&g, CondDeps::TECHS, rome) > before.0);
        assert_eq!(read(&g, indexes, greece), before.1);
        g.player_mut(rome, PlayerTouch::POLICIES);
        assert!(read(&g, CondDeps::GLOBAL_POLICIES, greece) > before.1);
        // Crossing the friend level moves that major's index, and no other's.
        let (techs, greek) = (read(&g, CondDeps::TECHS, rome), read(&g, CondDeps::TECHS, greece));
        g.set_influence(geneva, rome, 35.0)?;
        assert!(g.is_friend_level(geneva, rome));
        assert!(read(&g, CondDeps::TECHS, rome) > techs);
        assert_eq!(read(&g, CondDeps::TECHS, greece), greek);
        // So does war with the city-state, which puts influence at its floor.
        let techs = read(&g, CondDeps::TECHS, rome);
        g.update_relation(rome, geneva, |r| r.war = true)?;
        assert!(!g.is_friend_level(geneva, rome));
        assert!(read(&g, CondDeps::TECHS, rome) > techs);
        assert!(g.set_influence(rome, greece, 1.0).is_err(), "Rome is no city-state");
        Ok(())
    }

    #[test]
    fn a_city_state_met_or_dead_moves_the_indexes_its_bonuses_are_in() -> Result<(), StateError> {
        let mut g = testing::duel();
        let (rome, greece, geneva) = (PlayerId(0), PlayerId(1), PlayerId(2));
        let techs = |g: &Game, p: PlayerId| g.dv.revs.cond(&g.st, CondDeps::TECHS, &Ctx::civ(p));
        let before = techs(&g, greece);
        g.set_met(rome, greece)?;
        assert_eq!(techs(&g, greece), before, "two majors meeting moves no index");
        let before = techs(&g, rome);
        g.set_met(rome, geneva)?;
        assert!(techs(&g, rome) > before);
        let before = (techs(&g, rome), techs(&g, greece));
        g.kill_player(geneva)?;
        assert!(techs(&g, rome) > before.0 && techs(&g, greece) > before.1);
        Ok(())
    }

    #[test]
    fn a_settings_edit_rechecks_every_city_and_all_sight() {
        let mut g = testing::duel();
        let roma = testing::city(&mut g, PlayerId(0), TileIdx(22), "Roma");
        let athens = testing::city(&mut g, PlayerId(1), TileIdx(28), "Athens");
        g.settle();
        g.edit_config(|c| c.religion = !c.religion);
        assert_eq!(g.pending.take_recheck(), [roma, athens]);
        let sight = g.pending.take_sight();
        let civs: Vec<_> = g.st.players().ids().map(SightSource::Civ).collect();
        assert_eq!(sight, civs);
        g.settle();
        assert_eq!(g.take_violations(), []);
    }

    #[test]
    fn revisions_of_a_huge_id_take_one_entry() {
        let mut g = testing::duel();
        let far = UnitId::new(crate::state::store::MAX_ENTITY_ID).unwrap_or(UnitId::FIRST);
        let before = g.dv.revs.unit(far).max();
        g.dv.revs.touch_unit(far, PlayerId(0), UnitTouch::CORE);
        assert!(g.dv.revs.unit(far).max() > before);
        assert_eq!(g.dv.revs.unit(UnitId::FIRST).max(), before, "an untouched id reads the floor");
        let far = CityId::new(crate::state::store::MAX_ENTITY_ID).unwrap_or(CityId::FIRST);
        g.dv.revs.touch_city(far, PlayerId(0), CityTouch::STOCKS);
        assert!(g.dv.revs.city(far).stocks > before);
    }

    #[test]
    fn a_fight_reads_both_sides() -> Result<(), StateError> {
        use crate::unique::filter::Combatant;
        let mut g = testing::duel();
        g.add_city(City::new(cid(1), "Athens".into(), PlayerId(1), TileIdx(30), 1))?;
        g.st.ids_mut().unit = 2;
        g.spawn_unit(Unit::new(
            UnitId::FIRST,
            crate::base::ids::BaseUnitId(0),
            PlayerId(0),
            TileIdx(31),
            1,
        ))?;
        let fight = crate::unique::CombatCtx {
            our: Combatant::Unit(UnitId::FIRST),
            their: Some(Combatant::City(cid(1))),
            attacked_tile: Some(TileIdx(30)),
            action: None,
        };
        let ctx = Ctx {
            civ: Some(PlayerId(0)),
            combat: Some(fight),
            tile: Some(TileIdx(31)),
            unit: Some(UnitId::FIRST),
            ..Ctx::default()
        };
        let before = g.dv.revs.cond(&g.st, CondDeps::COMBAT, &ctx);
        g.city_mut(cid(1), CityTouch::CORE);
        assert!(g.dv.revs.cond(&g.st, CondDeps::COMBAT, &ctx) > before);
        Ok(())
    }
}

//! Every write to the state (DESIGN.md 6.4).
//!
//! Only this file may call `State`'s mutable accessors (`cargo xtask check`). Rule code writes
//! through `Game` in one of two ways:
//! - **setters**, which wrap the state's own: each state setter returns a `#[must_use]`
//!   [`Change`], and the wrapper passes it to [`Game::changed`], which moves the revisions of
//!   what it touched and raises the work the next settle does (citizen rechecks, dirty vision
//!   sources). They are for writes whose consequences need the new state: ownership, placement,
//!   a city or unit appearing or going, a tile changing, a seat, a player's fate, the clock;
//! - **touches** ([`Game::city_mut`], [`Game::player_mut`], [`Game::unit_mut`],
//!   [`Game::edit_world`], [`Game::edit_diplo`]), for field edits on one entity: the touch moves
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
        let react = self.dv.on(&self.st, self.rules, &ch);
        for c in react.recheck {
            self.pending.flag_city(c);
        }
        for s in react.sight {
            self.pending.flag_sight(s);
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

    /// Adds a new city; the tiles it claims are claimed with [`set_tile_owner`](Self::set_tile_owner).
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

    /// Sets the clock.
    pub(crate) fn set_clock(&mut self, clock: TurnClock) {
        let ch = self.st.set_clock(clock);
        self.changed(ch);
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

    /// Records that two players have met, and nothing more: [`Game::meet`] is the rule.
    pub(crate) fn set_met(&mut self, a: PlayerId, b: PlayerId) -> Result<(), StateError> {
        let chs = self.st.diplo_mut().meet(a, b)?;
        self.changed_all(chs);
        Ok(())
    }

    // ---- Touches ----------------------------------------------------------------------------

    /// A city's fields, after moving the revisions `t` names (DESIGN.md 6.4). `CORE` and `WORK`
    /// flag the city for a citizen recheck. `None` if there is no such city.
    pub(crate) fn city_mut(&mut self, c: CityId, t: CityTouch) -> Option<&mut City> {
        let owner = self.st.cities().get(c)?.owner();
        self.dv.revs.touch_city(c, owner, t);
        if t.intersects(CityTouch::CORE | CityTouch::WORK) {
            self.pending.flag_city(c);
        }
        self.st.cities_mut().get_mut(c)
    }

    /// A player's fields, after moving the revisions `t` names. A seat, and whether the player
    /// is alive, change through setters instead.
    pub(crate) fn player_mut(&mut self, p: PlayerId, t: PlayerTouch) -> Option<&mut Player> {
        self.st.player(p)?;
        self.dv.revs.touch_player(p, t);
        self.st.players_mut().get_mut(p)
    }

    /// A unit's fields, after moving the revisions `t` names; `SIGHT` marks it a dirty vision
    /// source. Its owner, tile and carrier change through setters instead.
    pub(crate) fn unit_mut(&mut self, u: UnitId, t: UnitTouch) -> Option<&mut Unit> {
        self.st.units().get(u)?;
        self.dv.revs.touch_unit(u, t);
        if t.contains(UnitTouch::SIGHT) {
            self.pending.flag_sight(SightSource::Unit(u));
        }
        self.st.units_mut().get_mut(u)
    }

    /// The world's fields (religions, wonders, the UN, camps), after moving the revisions `t`
    /// names.
    pub(crate) fn edit_world(&mut self, t: WorldTouch) -> &mut World {
        self.dv.revs.touch_world(t);
        self.st.world_mut()
    }

    /// The diplomacy's lists (deals, negotiations, opinions), after moving the revisions `t`
    /// names. A relation changes through [`update_relation`](Self::update_relation), which
    /// reports what moved.
    pub(crate) fn edit_diplo(&mut self, t: DiploTouch) -> &mut Diplomacy {
        self.dv.revs.touch_diplo(t);
        self.st.diplo_mut()
    }

    /// Edits the settings. Nearly every cache reads them, so every cache starts cold again: a
    /// settings edit is a scenario's, never a turn's.
    pub(crate) fn edit_config(&mut self, f: impl FnOnce(&mut GameConfig)) {
        f(self.st.config_mut());
        let now = self.dv.revs.now();
        self.dv = Derived::new(&self.st);
        // Revisions never go back: a host's ETag, and anything keyed on a revision, must see
        // every input move on.
        self.dv.revs = super::derive::rev::Revs::after(&self.st, now);
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
    fn a_touch_flags_the_city_and_moves_its_owners_index() -> Result<(), StateError> {
        let mut g = testing::duel();
        g.add_city(City::new(cid(1), "Roma".into(), PlayerId(0), TileIdx(22), 1))?;
        g.pending = super::super::pending::PendingWork::new();
        let before = g.dv.revs.civ(PlayerId(0)).index;
        if let Some(c) = g.city_mut(cid(1), CityTouch::CORE) {
            c.pop = 3;
        }
        assert!(g.dv.revs.civ(PlayerId(0)).index > before);
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
        // Each class, with a write that must move it.
        type Write = Box<dyn Fn(&mut Game)>;
        let cases: Vec<(CondDeps, Ctx, Write)> = vec![
            (CondDeps::TURN, Ctx::civ(p), Box::new(|g| g.set_clock(*g.st.clock()))),
            (CondDeps::CHANCE, Ctx::civ(p), Box::new(|g| g.set_clock(*g.st.clock()))),
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
                Box::new(|g| {
                    g.player_mut(PlayerId(0), PlayerTouch::STOCKS);
                }),
            ),
            (
                CondDeps::RESOURCES,
                Ctx::civ(p),
                Box::new(|g| {
                    let _ok = g.set_resource(TileIdx(3), None, 0);
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
                CondDeps::CIV_BUILDINGS,
                Ctx::civ(p),
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::CORE);
                }),
            ),
            (
                CondDeps::GLOBAL_BUILDINGS,
                Ctx::civ(p),
                Box::new(|g| {
                    g.city_mut(CityId::FIRST, CityTouch::CORE);
                }),
            ),
            (
                CondDeps::GLOBAL_POLICIES,
                Ctx::civ(p),
                Box::new(|g| {
                    g.player_mut(PlayerId(1), PlayerTouch::INDEX);
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
        ];
        for (class, ctx, write) in &cases {
            let before = g.dv.revs.cond(&g.st, *class, ctx);
            write(&mut g);
            let after = g.dv.revs.cond(&g.st, *class, ctx);
            assert!(after > before, "{class:?} did not move");
        }
        // Every class is covered, and the two halves partition them.
        let covered = cases.iter().fold(CondDeps::COMBAT, |d, (c, _, _)| d | *c);
        assert_eq!(covered, CondDeps::all());
        assert_eq!(CondDeps::LOCAL | CondDeps::CIV_LEVEL, CondDeps::all());
        assert!(CondDeps::LOCAL.intersection(CondDeps::CIV_LEVEL).is_empty());
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

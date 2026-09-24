//! Layer 2: the persisted game model, and the types that describe a change to it.
//!
//! `State` is everything a save holds and nothing a cache can recompute (DESIGN.md 4, package
//! 1a-08). It is read through `&State`. Writes go through `game::mutate` in one of two ways
//! (DESIGN.md 6.4):
//! - setters that return a `#[must_use]` [`Change`]: those of [`map::Tiles`], [`units::Units`],
//!   [`cities::Cities`] and [`diplo::Diplomacy`], and the operations here that span containers
//!   ([`State::transfer_city`], [`State::kill_player`], [`State::revive_player`]) or change what
//!   the caches key on (seats, alliances, the clock);
//! - the mutable accessors (`tiles_mut` and the rest), which only `game/mutate.rs`, `save/` and
//!   `compat/` may call, and `cargo xtask check` enforces that.
//!
//! Replaces `citar/engine/state.py:13-405` (`GameState` and the classes it holds) and the config
//! shape of `game.py:32-60`. Dropped from `GameState`: `rng_state` (every draw is keyed from the
//! seed, DESIGN.md 7), and `barbarian_state`, `capture_ids` and `first_discovered`, which nothing
//! outside `state.py` read or wrote. `next_id` became the per-kind [`IdCounters`]; `spaceship`
//! moved to the players, `open_borders` into the relations; events, messages, thoughts and stats
//! moved to the chronicle.

pub mod change;
pub mod chronicle;
pub mod cities;
pub mod config;
pub mod diplo;
pub mod map;
pub mod memory;
pub mod players;
pub mod store;
pub mod units;
pub mod world;

pub use change::{Change, Changes, TileClaim};

use crate::base::hex::HexError;
use crate::base::ids::{
    CampId, CityId, DealId, DifficultyId, NegotiationId, PlayerId, TileIdx, Turn, UnitId, VictoryId,
};
use crate::base::sets::{PlayerSet, PlayerVec};

use chronicle::{ChronicleHeads, HostHeads};
use cities::{Cities, CitiesError};
use config::{GameConfig, HostOnly};
use diplo::{Diplomacy, PairError};
use map::{MapInfo, TileError, Tiles};
use players::{AutoDecision, AutoOverrides, Controller, Handicap, Player};
use units::{Units, UnitsError};
use world::World;

/// Whether the game goes on.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Playing,
    Over,
}

/// Where the game is in time (`GameState.turn`, `current`, `turn_started`, `phase`, `winner`,
/// `victory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnClock {
    pub turn: Turn,
    /// Whose turn it is.
    pub current: PlayerId,
    /// Whether the current player's turn has begun.
    pub turn_started: bool,
    pub phase: Phase,
    pub winner: Option<PlayerId>,
    pub victory: Option<VictoryId>,
}

impl Default for TurnClock {
    fn default() -> Self {
        Self {
            turn: 1,
            current: PlayerId(0),
            turn_started: false,
            phase: Phase::Playing,
            winner: None,
            victory: None,
        }
    }
}

/// The next id of each kind of entity, and the combat counter: persisted, and never reused
/// (DESIGN.md 4.2). Python drew units, cities and camps from one `next_id` (`state.py:343`); the
/// converter starts all three at it, so converted ids stay unique across kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdCounters {
    pub unit: u32,
    pub city: u32,
    pub camp: u32,
    pub deal: u32,
    pub negotiation: u32,
    /// Combats fought, which keys each combat's random draws in place of Python's `g.rng`.
    pub combat_seq: u64,
}

impl Default for IdCounters {
    fn default() -> Self {
        Self { unit: 1, city: 1, camp: 1, deal: 1, negotiation: 1, combat_seq: 0 }
    }
}

/// Hands out the counter's value as an id and moves it on; `None` once `u32` is spent.
fn take<I>(counter: &mut u32, make: fn(u32) -> Option<I>) -> Option<I> {
    let id = make(*counter)?;
    *counter = counter.checked_add(1)?;
    Some(id)
}

impl IdCounters {
    /// Every entity counter at `next`, as the converter sets them from Python's `next_id`.
    #[must_use]
    pub const fn starting_at(next: u32) -> Self {
        Self { unit: next, city: next, camp: next, deal: 1, negotiation: 1, combat_seq: 0 }
    }

    /// The next unit id.
    pub fn next_unit(&mut self) -> Option<UnitId> {
        take(&mut self.unit, UnitId::new)
    }

    /// The next city id.
    pub fn next_city(&mut self) -> Option<CityId> {
        take(&mut self.city, CityId::new)
    }

    /// The next camp id.
    pub fn next_camp(&mut self) -> Option<CampId> {
        take(&mut self.camp, CampId::new)
    }

    /// The next deal id.
    pub fn next_deal(&mut self) -> Option<DealId> {
        take(&mut self.deal, DealId::new)
    }

    /// The next negotiation id.
    pub fn next_negotiation(&mut self) -> Option<NegotiationId> {
        take(&mut self.negotiation, NegotiationId::new)
    }

    /// The number of the next combat, counted.
    pub fn next_combat(&mut self) -> u64 {
        let n = self.combat_seq;
        self.combat_seq = n.saturating_add(1);
        n
    }
}

/// Why a state operation, or a state built from parts, was refused. Nothing is changed when an
/// operation is.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Units(#[from] UnitsError),
    #[error(transparent)]
    Cities(#[from] CitiesError),
    #[error(transparent)]
    Tile(#[from] TileError),
    #[error(transparent)]
    Pair(#[from] PairError),
    #[error(transparent)]
    Grid(#[from] HexError),
    /// There is no such player.
    #[error("there is no player {0}")]
    NoSuchPlayer(PlayerId),
    /// The player is not a city-state.
    #[error("player {0} is not a city-state")]
    NotACityState(PlayerId),
    /// A player cannot be eliminated while it holds cities.
    #[error("player {0} still holds cities")]
    HasCities(PlayerId),
    /// The parts do not fit together.
    #[error("{0}")]
    Mismatch(String),
}

/// Everything a `State` is made of, for building one from a save or a conversion and taking one
/// apart. [`State::from_parts`] rebuilds the indexes and checks that the parts fit.
#[derive(Clone, Debug, PartialEq)]
pub struct StateParts {
    pub config: GameConfig,
    pub map: MapInfo,
    pub tiles: Tiles,
    pub players: PlayerVec<Player>,
    pub units: Units,
    pub cities: Cities,
    pub diplo: Diplomacy,
    pub world: World,
    pub clock: TurnClock,
    pub ids: IdCounters,
    pub chronicle: ChronicleHeads,
    pub host: HostOnly<HostHeads>,
}

/// The persisted game (DESIGN.md 4.8).
///
/// Every part is readable through `&State`. Only this module's setters, which report a
/// [`Change`], and the restricted accessors write.
///
/// Its `Serialize` is the canonical form the digest hashes (`save::canon`): the parts in this
/// order, the host's heads as nothing. The JSON save writes its own top level (`save::json`).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct State {
    config: GameConfig,
    map: MapInfo,
    tiles: Tiles,
    players: PlayerVec<Player>,
    units: Units,
    cities: Cities,
    diplo: Diplomacy,
    world: World,
    clock: TurnClock,
    ids: IdCounters,
    chronicle: ChronicleHeads,
    host: HostOnly<HostHeads>,
}

impl State {
    /// A new game's state: these settings, this map and these players, with no units, cities,
    /// relations or history yet, at turn 1.
    ///
    /// Refused if the tiles do not fill the map, the map's shape is not a valid grid, there are
    /// more than 64 players, or a player's id is not its position.
    pub fn new(
        config: GameConfig,
        map: MapInfo,
        tiles: Tiles,
        players: PlayerVec<Player>,
    ) -> Result<Self, StateError> {
        let n = u8::try_from(players.len())
            .ok()
            .filter(|&n| usize::from(n) <= PlayerSet::CAPACITY)
            .ok_or_else(|| {
                StateError::Mismatch(format!("{} players; at most 64 fit", players.len()))
            })?;
        let barbarians =
            players.iter().filter(|(_, p)| p.is_barbarian()).map(|(id, _)| id).collect();
        let size = map.size();
        Self::from_parts(StateParts {
            config,
            map,
            tiles,
            players,
            units: Units::new(size),
            cities: Cities::new(),
            diplo: Diplomacy::new(n, barbarians),
            world: World::default(),
            clock: TurnClock::default(),
            ids: IdCounters::default(),
            chronicle: ChronicleHeads::default(),
            host: HostOnly::default(),
        })
    }

    /// A state from its parts, with the indexes rebuilt and the parts checked against each other,
    /// so that no later read can panic on them:
    /// - the grid is valid, the tiles and continents fill the map, and each major's memory and
    ///   each explored set fit it;
    /// - there are at most 64 players, each at its own id, and the relations cover them;
    /// - every unit, city and tile owner is a player, and every city and unit stands on the map;
    /// - a tile's city exists, and carrier links are sound;
    /// - each id counter is past every unit, city, camp, deal and negotiation id in use, so the
    ///   next spawn cannot collide with one.
    ///
    /// What these checks leave to `save::validate` is referential integrity beyond ownership
    /// (capitals, goto tiles, worked tiles and the like), ranges and floats.
    pub fn from_parts(parts: StateParts) -> Result<Self, StateError> {
        parts.map.grid()?;
        let size = parts.map.size();
        if parts.tiles.len() != size as usize {
            return Err(StateError::Mismatch(format!(
                "{} tiles for a {}x{} map",
                parts.tiles.len(),
                parts.map.width,
                parts.map.height
            )));
        }
        if !parts.map.continents.is_empty() && parts.map.continents.len() != size as usize {
            return Err(StateError::Mismatch(format!(
                "{} continent ids for {size} tiles",
                parts.map.continents.len()
            )));
        }
        let n = parts.players.len();
        if n > PlayerSet::CAPACITY {
            return Err(StateError::Mismatch(format!("{n} players; at most 64 fit")));
        }
        if let Some((pos, p)) = parts.players.iter().find(|(pos, p)| p.id() != *pos) {
            return Err(StateError::Mismatch(format!("player {} is at position {pos}", p.id())));
        }
        if usize::from(parts.diplo.players()) != n {
            return Err(StateError::Mismatch(format!(
                "relations for {} players, {n} players",
                parts.diplo.players()
            )));
        }
        Self::check_refs(&parts, size)?;
        let StateParts {
            config,
            map,
            tiles,
            players,
            units,
            cities,
            diplo,
            world,
            clock,
            ids,
            chronicle,
            host,
        } = parts;
        let mut st = Self {
            config,
            map,
            tiles,
            players,
            units,
            cities,
            diplo,
            world,
            clock,
            ids,
            chronicle,
            host,
        };
        st.rebuild_indexes()?;
        Ok(st)
    }

    /// The checks of [`from_parts`](Self::from_parts) between parts, on a map of `size` tiles
    /// whose shape is already checked.
    fn check_refs(parts: &StateParts, size: u32) -> Result<(), StateError> {
        let bad = |m: String| Err(StateError::Mismatch(m));
        let n = parts.players.len();
        let is_player = |p: PlayerId| usize::from(p.0) < n;
        for (t, tile) in parts.tiles.iter() {
            if let Some(o) = tile.owner()
                && !is_player(o)
            {
                return bad(format!("tile {t} is owned by player {o}, of {n} players"));
            }
            if let Some(c) = tile.city()
                && !parts.cities.contains(c)
            {
                return bad(format!("tile {t} belongs to city {c}, which does not exist"));
            }
        }
        if let Some(u) = parts.units.iter().find(|u| !is_player(u.owner())) {
            return bad(format!(
                "unit {} is owned by player {}, of {n} players",
                u.id(),
                u.owner()
            ));
        }
        for c in parts.cities.iter() {
            if !is_player(c.owner()) {
                return bad(format!(
                    "city {} is owned by player {}, of {n} players",
                    c.id(),
                    c.owner()
                ));
            }
            if c.tile().0 >= size {
                return bad(format!("city {} stands on tile {}, off the map", c.id(), c.tile()));
            }
        }
        for (p, player) in parts.players.iter() {
            if let Some(i) = player.explored.last()
                && i >= size
            {
                return bad(format!("player {p} has explored tile {i}, off the map"));
            }
            if let Some(major) = &player.major
                && major.memory.len() != size as usize
            {
                return bad(format!("player {p} remembers {} tiles of {size}", major.memory.len()));
            }
        }
        let past = |what: &str, next: u32, last: Option<u32>| match last {
            Some(last) if last >= next => {
                bad(format!("the next {what} id is {next}, but {what} {last} exists"))
            }
            _ => Ok(()),
        };
        let ids = &parts.ids;
        past("unit", ids.unit, parts.units.store().last_id().map(UnitId::get))?;
        past("city", ids.city, parts.cities.store().last_id().map(CityId::get))?;
        past("camp", ids.camp, parts.world.camps.keys().next_back().map(|c| c.get()))?;
        past("deal", ids.deal, parts.diplo.deals.iter().map(|d| d.id.get()).max())?;
        past(
            "negotiation",
            ids.negotiation,
            parts.diplo.negotiations.iter().map(|x| x.id.get()).max(),
        )
    }

    /// Takes the state apart.
    #[must_use]
    pub fn into_parts(self) -> StateParts {
        let Self {
            config,
            map,
            tiles,
            players,
            units,
            cities,
            diplo,
            world,
            clock,
            ids,
            chronicle,
            host,
        } = self;
        StateParts {
            config,
            map,
            tiles,
            players,
            units,
            cities,
            diplo,
            world,
            clock,
            ids,
            chronicle,
            host,
        }
    }

    /// Rebuilds everything structural from the persisted data: the occupancy and owner lists of
    /// units and cities, and the war and contact masks. Called on load; never saved.
    pub fn rebuild_indexes(&mut self) -> Result<(), StateError> {
        self.units.rebuild(self.map.size())?;
        self.cities.rebuild();
        self.diplo.rebuild_masks(self.barbarians());
        Ok(())
    }

    /// Checks that every index agrees with the data it indexes: for tests, invariants and
    /// loading.
    pub fn check_indexes(&self) -> Result<(), StateError> {
        self.units.verify()?;
        self.cities.verify()?;
        self.diplo.verify().map_err(StateError::Mismatch)?;
        if self.diplo.barbarians() != self.barbarians() {
            return Err(StateError::Mismatch(
                "the diplomacy's barbarians are not the players'".to_owned(),
            ));
        }
        Ok(())
    }

    // ---- Reads ---------------------------------------------------------------------------------

    /// The settings.
    #[must_use]
    pub const fn config(&self) -> &GameConfig {
        &self.config
    }

    /// The seed every random stream derives from (DESIGN.md 7).
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.config.seed
    }

    /// The map's shape.
    #[must_use]
    pub const fn map(&self) -> &MapInfo {
        &self.map
    }

    /// The tiles.
    #[must_use]
    pub const fn tiles(&self) -> &Tiles {
        &self.tiles
    }

    /// Every player, by id.
    #[must_use]
    pub const fn players(&self) -> &PlayerVec<Player> {
        &self.players
    }

    /// One player.
    #[must_use]
    #[inline]
    pub fn player(&self, p: PlayerId) -> Option<&Player> {
        self.players.get(p)
    }

    /// The barbarian players.
    #[must_use]
    pub fn barbarians(&self) -> PlayerSet {
        self.players.iter().filter(|(_, p)| p.is_barbarian()).map(|(id, _)| id).collect()
    }

    /// The units.
    #[must_use]
    pub const fn units(&self) -> &Units {
        &self.units
    }

    /// The cities.
    #[must_use]
    pub const fn cities(&self) -> &Cities {
        &self.cities
    }

    /// The city standing on tile `t`, if any: the tile names the city that owns it, and that
    /// city stands there.
    #[must_use]
    pub fn city_at(&self, t: TileIdx) -> Option<CityId> {
        let c = self.tiles.get(t)?.city()?;
        (self.cities.get(c)?.tile() == t).then_some(c)
    }

    /// Relations, opinions, deals and negotiations.
    #[must_use]
    pub const fn diplo(&self) -> &Diplomacy {
        &self.diplo
    }

    /// Religions, wonders, the UN and camps.
    #[must_use]
    pub const fn world(&self) -> &World {
        &self.world
    }

    /// Where the game is in time.
    #[must_use]
    pub const fn clock(&self) -> &TurnClock {
        &self.clock
    }

    /// The id counters.
    #[must_use]
    pub const fn ids(&self) -> &IdCounters {
        &self.ids
    }

    /// The heads of the engine's history (digested).
    #[must_use]
    pub const fn chronicle(&self) -> &ChronicleHeads {
        &self.chronicle
    }

    /// The heads of host activity (saved, never digested).
    #[must_use]
    pub const fn host(&self) -> &HostOnly<HostHeads> {
        &self.host
    }

    // ---- Restricted accessors (DESIGN.md 6.4) --------------------------------------------------
    //
    // `&mut` without a revision bump: only game/mutate.rs, save/ and compat/ may call these, which
    // `cargo xtask check` enforces. Everything else writes through Game's setters or a Touch.

    #[allow(dead_code, reason = "called by game::mutate (1b-01), save (1a-09) and compat (1a-10)")]
    pub(crate) fn tiles_mut(&mut self) -> &mut Tiles {
        &mut self.tiles
    }

    #[allow(dead_code, reason = "called by game::mutate (1b-01), save (1a-09) and compat (1a-10)")]
    pub(crate) fn units_mut(&mut self) -> &mut Units {
        &mut self.units
    }

    #[allow(dead_code, reason = "called by game::mutate (1b-01), save (1a-09) and compat (1a-10)")]
    pub(crate) fn cities_mut(&mut self) -> &mut Cities {
        &mut self.cities
    }

    #[allow(dead_code, reason = "called by game::mutate (1b-01), save (1a-09) and compat (1a-10)")]
    pub(crate) fn players_mut(&mut self) -> &mut PlayerVec<Player> {
        &mut self.players
    }

    #[allow(dead_code, reason = "called by game::mutate (1b-01), save (1a-09) and compat (1a-10)")]
    pub(crate) fn diplo_mut(&mut self) -> &mut Diplomacy {
        &mut self.diplo
    }

    #[allow(dead_code, reason = "called by game::mutate (1b-01), save (1a-09) and compat (1a-10)")]
    pub(crate) fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// The settings feed nearly every cache, so they are restricted like the containers.
    #[allow(dead_code, reason = "called by game::mutate (1b-01), save (1a-09) and compat (1a-10)")]
    pub(crate) fn config_mut(&mut self) -> &mut GameConfig {
        &mut self.config
    }

    // Counters and history heads feed no cache, so any engine code that owns the state may move
    // them.

    /// The id counters.
    #[allow(dead_code, reason = "called by the rule systems from 1b-01 on")]
    pub(crate) fn ids_mut(&mut self) -> &mut IdCounters {
        &mut self.ids
    }

    /// The engine's history heads.
    #[allow(dead_code, reason = "called by game::events (1b-01)")]
    pub(crate) fn chronicle_mut(&mut self) -> &mut ChronicleHeads {
        &mut self.chronicle
    }

    /// The host's heads.
    #[allow(dead_code, reason = "called by game::events and save::journal (1a-09, 1b-01)")]
    pub(crate) fn host_mut(&mut self) -> &mut HostHeads {
        &mut self.host.0
    }

    // ---- Writes that span containers or change what caches key on ------------------------------

    fn player_mut_of(&mut self, p: PlayerId) -> Result<&mut Player, StateError> {
        self.players.get_mut(p).ok_or(StateError::NoSuchPlayer(p))
    }

    /// Sets the clock. The turn and whose turn it is feed the conditionals, so it always reports
    /// [`Change::Turn`].
    pub fn set_clock(&mut self, clock: TurnClock) -> Change {
        self.clock = clock;
        Change::Turn
    }

    /// Hands a city, and the tiles it owns, to another player (`conquest.py:60-80`). If it was
    /// the old owner's capital, that owner has none until the rules choose one; everything else a
    /// capture does is the conquest rule's.
    ///
    /// The changes list the city first, then its tiles in index order.
    pub fn transfer_city(&mut self, c: CityId, new: PlayerId) -> Result<Changes, StateError> {
        let old = self.cities.get(c).map(cities::City::owner).ok_or(CitiesError::NoSuchCity(c))?;
        if self.players.get(new).is_none() {
            return Err(StateError::NoSuchPlayer(new));
        }
        let mut out = Changes::new();
        if old == new {
            return Ok(out);
        }
        out.push(self.cities.set_owner(c, new)?);
        let owned: Vec<TileIdx> =
            self.tiles.iter().filter(|(_, t)| t.city() == Some(c)).map(|(i, _)| i).collect();
        for t in owned {
            out.push(self.tiles.set_owner(t, TileClaim::city(new, c))?);
        }
        if let Some(p) = self.players.get_mut(old)
            && p.capital == Some(c)
        {
            p.capital = None;
        }
        Ok(out)
    }

    /// Eliminates a player that holds no cities (`victory.py:380-392`): it is no longer alive,
    /// has no capital, and its units leave the game. Closing its negotiations and deals, and its
    /// spies, is the rules' business.
    ///
    /// The changes list its units' removals in id order, each followed by the units that unit
    /// carried leaving it (as [`Units::despawn`] reports them), then [`Change::PlayerAlive`].
    pub fn kill_player(&mut self, p: PlayerId, turn: Turn) -> Result<Changes, StateError> {
        if self.players.get(p).is_none() {
            return Err(StateError::NoSuchPlayer(p));
        }
        if !self.cities.of(p).is_empty() {
            return Err(StateError::HasCities(p));
        }
        let mut out = Changes::new();
        for u in self.units.of(p).to_vec() {
            let (_, ch) = self.units.despawn(u)?;
            out.append(ch);
        }
        let player = self.player_mut_of(p)?;
        player.set_alive(false, Some(turn));
        player.capital = None;
        out.push(Change::PlayerAlive(p));
        Ok(out)
    }

    /// Brings an eliminated player back, as liberating one of its cities does
    /// (`conquest.py:262-265`).
    pub fn revive_player(&mut self, p: PlayerId) -> Result<Change, StateError> {
        self.player_mut_of(p)?.set_alive(true, None);
        Ok(Change::PlayerAlive(p))
    }

    /// Hands a seat to another controller; `handicap` and `auto` become explicit settings, and
    /// the rest follows the new controller (`state.py:282-296`).
    pub fn set_controller(
        &mut self,
        p: PlayerId,
        controller: Controller,
        handicap: Option<Handicap>,
        auto: AutoOverrides,
    ) -> Result<Change, StateError> {
        self.player_mut_of(p)?.seat_mut().set_controller(controller, handicap, auto);
        Ok(Change::Seat(p))
    }

    /// Changes one automatic decision of a seat for now, not as an override.
    pub fn set_auto_decision(
        &mut self,
        p: PlayerId,
        d: AutoDecision,
        on: bool,
    ) -> Result<Change, StateError> {
        self.player_mut_of(p)?.seat_mut().set_auto(d, on);
        Ok(Change::Seat(p))
    }

    /// Sets a seat's own difficulty, or `None` for the game's.
    pub fn set_seat_difficulty(
        &mut self,
        p: PlayerId,
        d: Option<DifficultyId>,
    ) -> Result<Change, StateError> {
        self.player_mut_of(p)?.seat_mut().set_difficulty(d);
        Ok(Change::Seat(p))
    }

    /// Sets a city-state's ally.
    pub fn set_ally(&mut self, cs: PlayerId, ally: Option<PlayerId>) -> Result<Change, StateError> {
        if let Some(a) = ally
            && self.players.get(a).is_none()
        {
            return Err(StateError::NoSuchPlayer(a));
        }
        let data =
            self.player_mut_of(cs)?.city_state.as_mut().ok_or(StateError::NotACityState(cs))?;
        let old = data.set_ally(ally);
        Ok(Change::Alliance { cs, old, new: ally })
    }
}

#[cfg(test)]
mod tests;

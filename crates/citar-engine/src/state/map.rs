//! The map: its shape ([`MapInfo`]), its tiles ([`Tile`], [`Tiles`]) and their build queues
//! (DESIGN.md 4.3).
//!
//! Replaces `state.py:66-107` (`Tile`), the tile list and `continents` of `GameState`
//! (`state.py:335-337, 364`), and the tile writes scattered through the rules, which here are
//! setters that each return a [`Change`].
//!
//! A tile is 16 bytes of plain data, four to a cache line, so the hot loops (movement costs,
//! yields, worker scoring) read a tile and its neighbours from one or two lines. Its fields are
//! private: they are read through typed accessors and written only through [`Tiles`], whose
//! setters report what moved.

use std::collections::BTreeMap;

use smallvec::SmallVec;

use super::change::{Change, TileClaim};
use crate::base::hex::{Dir, HexError, HexGrid};
use crate::base::ids::{CityId, ImprovementId, PlayerId, ResourceId, TerrainId, TileIdx};
use crate::base::sets::FeatureSet;
use crate::rules::defs::Route;

/// The continent id of a water tile.
pub const WATER: u16 = u16::MAX;

/// The map's shape, from which the hex grid is derived, and each tile's continent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapInfo {
    /// Tiles per row.
    pub width: u16,
    /// Rows.
    pub height: u16,
    /// Whether the map wraps east-west.
    pub wrap_x: bool,
    /// Whether the map wraps north-south (only with an even height, `game.py:181`).
    pub wrap_y: bool,
    /// Each tile's continent, [`WATER`] for water (`state.py:364`, where it was -1).
    pub continents: Vec<u16>,
}

impl MapInfo {
    /// The hex grid of this shape; an error for a size the grid refuses.
    pub fn grid(&self) -> Result<HexGrid, HexError> {
        HexGrid::new(self.width, self.height, self.wrap_x, self.wrap_y)
    }

    /// The number of tiles.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    /// The continent a tile belongs to; `None` for water or a tile off the map.
    #[must_use]
    pub fn continent(&self, t: TileIdx) -> Option<u16> {
        self.continents.get(t.0 as usize).copied().filter(|&c| c != WATER)
    }
}

// ---- RouteBits --------------------------------------------------------------------------------

/// A tile's route and pillage state in one byte: bits 0-1 the route (0 none, 1 road,
/// 2 railroad), bit 2 the route pillaged, bit 3 the improvement pillaged.
///
/// Python kept `route`, `route_pillaged` and `pillaged` as three fields (`state.py:76-78`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RouteBits(u8);

impl RouteBits {
    const ROUTE: u8 = 0b0011;
    const ROUTE_PILLAGED: u8 = 0b0100;
    const IMPROVEMENT_PILLAGED: u8 = 0b1000;

    /// No route, nothing pillaged.
    pub const EMPTY: Self = Self(0);

    /// The byte as stored, with any bit above 3 dropped.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0b1111)
    }

    /// The byte as stored.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The route, if any. The unused level 3 reads as a railroad.
    #[must_use]
    pub const fn route(self) -> Option<Route> {
        match self.0 & Self::ROUTE {
            0 => None,
            1 => Some(Route::Road),
            _ => Some(Route::Railroad),
        }
    }

    /// Whether the route is pillaged.
    #[must_use]
    pub const fn route_pillaged(self) -> bool {
        self.0 & Self::ROUTE_PILLAGED != 0
    }

    /// Whether the improvement is pillaged.
    #[must_use]
    pub const fn improvement_pillaged(self) -> bool {
        self.0 & Self::IMPROVEMENT_PILLAGED != 0
    }

    /// With this route; the pillage bits are kept.
    #[must_use]
    pub const fn with_route(self, route: Option<Route>) -> Self {
        let level = match route {
            None => 0,
            Some(Route::Road) => 1,
            Some(Route::Railroad) => 2,
        };
        Self((self.0 & !Self::ROUTE) | level)
    }

    /// With the route pillaged or not.
    #[must_use]
    pub const fn with_route_pillaged(self, on: bool) -> Self {
        if on { Self(self.0 | Self::ROUTE_PILLAGED) } else { Self(self.0 & !Self::ROUTE_PILLAGED) }
    }

    /// With the improvement pillaged or not.
    #[must_use]
    pub const fn with_improvement_pillaged(self, on: bool) -> Self {
        if on {
            Self(self.0 | Self::IMPROVEMENT_PILLAGED)
        } else {
            Self(self.0 & !Self::IMPROVEMENT_PILLAGED)
        }
    }
}

impl core::fmt::Debug for RouteBits {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RouteBits")
            .field("route", &self.route())
            .field("route_pillaged", &self.route_pillaged())
            .field("improvement_pillaged", &self.improvement_pillaged())
            .finish()
    }
}

// ---- Tile -------------------------------------------------------------------------------------

/// A raw player id meaning "no owner".
const NO_OWNER: u8 = 0xFF;

/// One map tile, in 16 bytes (DESIGN.md 4.3).
///
/// Optional rule ids are stored as id + 1 with 0 for none; the ruleset's sets cap those tables at
/// 64 entries, so the + 1 always fits. The river is a 6-bit mask over the grid's directions.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tile {
    terrain: TerrainId,
    wonder: u8,
    resource: u8,
    resource_amount: u8,
    improvement: u8,
    route: RouteBits,
    owner: u8,
    river: u8,
    features: FeatureSet,
    reserved: u16,
    city: u32,
}

const _: () = assert!(size_of::<Tile>() == 16, "a Tile is 16 bytes (DESIGN.md 4.3)");

/// An optional u8 rule id as stored: id + 1, 0 for none.
fn plus_one(raw: Option<u8>) -> u8 {
    raw.map_or(0, |r| r.saturating_add(1))
}

/// The inverse of [`plus_one`].
fn minus_one(stored: u8) -> Option<u8> {
    stored.checked_sub(1)
}

impl Tile {
    /// A bare tile of this base terrain: no features, resource, improvement, route, river,
    /// owner or city.
    #[must_use]
    pub const fn new(terrain: TerrainId) -> Self {
        Self {
            terrain,
            wonder: 0,
            resource: 0,
            resource_amount: 0,
            improvement: 0,
            route: RouteBits::EMPTY,
            owner: NO_OWNER,
            river: 0,
            features: FeatureSet::EMPTY,
            reserved: 0,
            city: 0,
        }
    }

    /// The base terrain.
    #[must_use]
    #[inline]
    pub const fn terrain(&self) -> TerrainId {
        self.terrain
    }

    /// The natural wonder, if any.
    #[must_use]
    #[inline]
    pub fn wonder(&self) -> Option<TerrainId> {
        minus_one(self.wonder).map(TerrainId)
    }

    /// The resource, if any.
    #[must_use]
    #[inline]
    pub fn resource(&self) -> Option<ResourceId> {
        minus_one(self.resource).map(ResourceId)
    }

    /// The size of a strategic deposit; 0 for other resources.
    #[must_use]
    #[inline]
    pub const fn resource_amount(&self) -> u8 {
        self.resource_amount
    }

    /// The improvement, if any, including the city center, ruins and camps.
    #[must_use]
    #[inline]
    pub fn improvement(&self) -> Option<ImprovementId> {
        minus_one(self.improvement).map(ImprovementId)
    }

    /// The route and pillage bits.
    #[must_use]
    #[inline]
    pub const fn route_bits(&self) -> RouteBits {
        self.route
    }

    /// The route, if any.
    #[must_use]
    #[inline]
    pub const fn route(&self) -> Option<Route> {
        self.route.route()
    }

    /// Whether the route is pillaged.
    #[must_use]
    #[inline]
    pub const fn route_pillaged(&self) -> bool {
        self.route.route_pillaged()
    }

    /// Whether the improvement is pillaged.
    #[must_use]
    #[inline]
    pub const fn improvement_pillaged(&self) -> bool {
        self.route.improvement_pillaged()
    }

    /// The owning player, if any.
    #[must_use]
    #[inline]
    pub const fn owner(&self) -> Option<PlayerId> {
        if self.owner == NO_OWNER { None } else { Some(PlayerId(self.owner)) }
    }

    /// The city that owns (can work) the tile, if any.
    #[must_use]
    #[inline]
    pub const fn city(&self) -> Option<CityId> {
        CityId::new(self.city)
    }

    /// The owner and the city together.
    #[must_use]
    #[inline]
    pub fn claim(&self) -> TileClaim {
        TileClaim { owner: self.owner(), city: self.city() }
    }

    /// The river mask: bit `d` is set when a river runs along the edge in direction `d`.
    #[must_use]
    #[inline]
    pub const fn river_mask(&self) -> u8 {
        self.river
    }

    /// Whether a river runs along the edge in direction `d`.
    #[must_use]
    #[inline]
    pub const fn river(&self, d: Dir) -> bool {
        self.river & (1 << d.index()) != 0
    }

    /// Whether any river runs along the tile.
    #[must_use]
    #[inline]
    pub const fn has_river(&self) -> bool {
        self.river != 0
    }

    /// The terrain features, Hill included.
    #[must_use]
    #[inline]
    pub const fn features(&self) -> FeatureSet {
        self.features
    }

    /// The tile as `CANON_V1` hashes it: the fields in declaration order, little-endian, with the
    /// reserved bytes as zero (DESIGN.md 4.10).
    #[must_use]
    pub const fn canon_bytes(&self) -> [u8; 16] {
        let f = self.features.bits().to_le_bytes();
        let c = self.city.to_le_bytes();
        [
            self.terrain.0,
            self.wonder,
            self.resource,
            self.resource_amount,
            self.improvement,
            self.route.bits(),
            self.owner,
            self.river,
            f[0],
            f[1],
            0,
            0,
            c[0],
            c[1],
            c[2],
            c[3],
        ]
    }

    /// The tile [`canon_bytes`](Self::canon_bytes) wrote, for journal frames and tests. Bits no
    /// tile sets (the reserved bytes, the top of the route byte) are dropped.
    #[must_use]
    pub const fn from_canon_bytes(b: [u8; 16]) -> Self {
        Self {
            terrain: TerrainId(b[0]),
            wonder: b[1],
            resource: b[2],
            resource_amount: b[3],
            improvement: b[4],
            route: RouteBits::from_bits(b[5]),
            owner: b[6],
            river: b[7],
            features: FeatureSet::from_bits(u16::from_le_bytes([b[8], b[9]])),
            reserved: 0,
            city: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        }
    }

    // ---- Builders, for tiles not yet on a map: generation, editors, loading ------------------

    /// With this natural wonder.
    #[must_use]
    pub fn with_wonder(mut self, wonder: Option<TerrainId>) -> Self {
        self.wonder = plus_one(wonder.map(|w| w.0));
        self
    }

    /// With this resource and deposit size.
    #[must_use]
    pub fn with_resource(mut self, resource: Option<ResourceId>, amount: u8) -> Self {
        self.resource = plus_one(resource.map(|r| r.0));
        self.resource_amount = if resource.is_some() { amount } else { 0 };
        self
    }

    /// With this improvement.
    #[must_use]
    pub fn with_improvement(mut self, improvement: Option<ImprovementId>) -> Self {
        self.improvement = plus_one(improvement.map(|i| i.0));
        self
    }

    /// With these route and pillage bits.
    #[must_use]
    pub const fn with_route_bits(mut self, route: RouteBits) -> Self {
        self.route = route;
        self
    }

    /// With this owner and city.
    #[must_use]
    pub fn with_claim(mut self, claim: TileClaim) -> Self {
        self.owner = claim.owner.map_or(NO_OWNER, |p| p.0);
        self.city = claim.city.map_or(0, CityId::get);
        self
    }

    /// With this river mask (bits above the sixth are dropped).
    #[must_use]
    pub const fn with_river(mut self, mask: u8) -> Self {
        self.river = mask & 0b11_1111;
        self
    }

    /// With these features.
    #[must_use]
    pub const fn with_features(mut self, features: FeatureSet) -> Self {
        self.features = features;
        self
    }

    /// With this base terrain.
    #[must_use]
    pub const fn with_terrain(mut self, terrain: TerrainId) -> Self {
        self.terrain = terrain;
        self
    }
}

impl core::fmt::Debug for Tile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Tile")
            .field("terrain", &self.terrain)
            .field("features", &self.features)
            .field("wonder", &self.wonder())
            .field("resource", &self.resource())
            .field("resource_amount", &self.resource_amount)
            .field("improvement", &self.improvement())
            .field("route", &self.route)
            .field("owner", &self.owner())
            .field("city", &self.city())
            .field("river", &self.river)
            .finish()
    }
}

// ---- Build queues -----------------------------------------------------------------------------

/// One entry of a tile's build queue: what a worker is building and how many turns are left
/// (Python's `[improvement, turns left]`, `state.py:82`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BuildStep {
    /// The improvement, route or removal being built.
    pub improvement: ImprovementId,
    /// Turns of work left.
    pub turns_left: i16,
}

/// A tile's build queue, front first.
pub type BuildQueue = SmallVec<[BuildStep; 2]>;

// ---- Tiles ------------------------------------------------------------------------------------

/// Why a tile write was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TileError {
    /// The tile is not on the map.
    #[error("tile {0} is not on the map")]
    OffMap(TileIdx),
}

/// Every tile, and the build queues of the tiles that have one.
///
/// Reads are open. The setters each return the [`Change`] the caches need, and a tile off the
/// map is an error rather than a panic. Only `game::mutate` reaches them, through
/// `State::tiles_mut`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tiles {
    tiles: Vec<Tile>,
    builds: BTreeMap<TileIdx, BuildQueue>,
}

impl Tiles {
    /// These tiles, in index order, with no build queues.
    #[must_use]
    pub const fn new(tiles: Vec<Tile>) -> Self {
        Self { tiles, builds: BTreeMap::new() }
    }

    /// These tiles and build queues. Empty queues and queues of tiles off the map are dropped.
    #[must_use]
    pub fn from_parts(tiles: Vec<Tile>, builds: BTreeMap<TileIdx, BuildQueue>) -> Self {
        let n = tiles.len();
        let builds =
            builds.into_iter().filter(|(t, q)| !q.is_empty() && (t.0 as usize) < n).collect();
        Self { tiles, builds }
    }

    /// The number of tiles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// Whether there are no tiles.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Whether `t` is on the map.
    #[must_use]
    pub fn contains(&self, t: TileIdx) -> bool {
        (t.0 as usize) < self.tiles.len()
    }

    /// The tile at `t`.
    #[must_use]
    #[inline]
    pub fn get(&self, t: TileIdx) -> Option<&Tile> {
        self.tiles.get(t.0 as usize)
    }

    /// Every tile, in index order.
    #[must_use]
    pub fn as_slice(&self) -> &[Tile] {
        &self.tiles
    }

    /// Every tile with its index.
    pub fn iter(&self) -> impl Iterator<Item = (TileIdx, &Tile)> {
        // The map is at most 256 x 256 tiles (hex::MAX_SIDE), so every index fits a u32.
        self.tiles.iter().enumerate().map(|(i, t)| (TileIdx(i as u32), t))
    }

    /// The build queue of `t`, front first; empty if it has none.
    #[must_use]
    pub fn builds(&self, t: TileIdx) -> &[BuildStep] {
        self.builds.get(&t).map_or(&[], |q| q.as_slice())
    }

    /// Every tile with a build queue, in index order.
    pub fn all_builds(&self) -> impl Iterator<Item = (TileIdx, &[BuildStep])> {
        self.builds.iter().map(|(&t, q)| (t, q.as_slice()))
    }

    fn at(&mut self, t: TileIdx) -> Result<&mut Tile, TileError> {
        self.tiles.get_mut(t.0 as usize).ok_or(TileError::OffMap(t))
    }

    /// Gives the tile to a player and a city, or to nobody.
    pub fn set_owner(&mut self, t: TileIdx, claim: TileClaim) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        let old = tile.claim();
        *tile = tile.with_claim(claim);
        Ok(Change::TileOwner { t, old, new: tile.claim() })
    }

    /// Sets the improvement. The pillage bit is left as it is: whether a new improvement starts
    /// whole is the rule's decision (`workers.py:383-448`).
    pub fn set_improvement(
        &mut self,
        t: TileIdx,
        improvement: Option<ImprovementId>,
    ) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        *tile = tile.with_improvement(improvement);
        Ok(Change::TileInput(t))
    }

    /// Sets the route; the pillage bits are kept.
    pub fn set_route(&mut self, t: TileIdx, route: Option<Route>) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        tile.route = tile.route.with_route(route);
        Ok(Change::TileInput(t))
    }

    /// Sets whether the route and the improvement are pillaged.
    pub fn set_pillaged(
        &mut self,
        t: TileIdx,
        route: bool,
        improvement: bool,
    ) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        tile.route = tile.route.with_route_pillaged(route).with_improvement_pillaged(improvement);
        Ok(Change::TileInput(t))
    }

    /// Sets the features. Hills and forests change what can be seen, so this is a height change.
    pub fn set_features(&mut self, t: TileIdx, features: FeatureSet) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        tile.features = features;
        Ok(Change::TileHeight(t))
    }

    /// Sets the base terrain (a mountain blocks sight).
    pub fn set_terrain(&mut self, t: TileIdx, terrain: TerrainId) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        tile.terrain = terrain;
        Ok(Change::TileHeight(t))
    }

    /// Sets the natural wonder.
    pub fn set_wonder(
        &mut self,
        t: TileIdx,
        wonder: Option<TerrainId>,
    ) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        *tile = tile.with_wonder(wonder);
        Ok(Change::TileHeight(t))
    }

    /// Sets the resource and its deposit size (0 unless there is a resource).
    pub fn set_resource(
        &mut self,
        t: TileIdx,
        resource: Option<ResourceId>,
        amount: u8,
    ) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        *tile = tile.with_resource(resource, amount);
        Ok(Change::TileInput(t))
    }

    /// Sets the river mask (bits above the sixth are dropped).
    pub fn set_river(&mut self, t: TileIdx, mask: u8) -> Result<Change, TileError> {
        let tile = self.at(t)?;
        *tile = tile.with_river(mask);
        Ok(Change::TileInput(t))
    }

    /// Appends a step to the tile's build queue.
    pub fn push_build(&mut self, t: TileIdx, step: BuildStep) -> Result<Change, TileError> {
        self.at(t)?;
        self.builds.entry(t).or_default().push(step);
        Ok(Change::TileInput(t))
    }

    /// Takes the front step off the tile's build queue: the step, if there was one, and the
    /// change.
    pub fn pop_build(&mut self, t: TileIdx) -> Result<(Option<BuildStep>, Change), TileError> {
        self.at(t)?;
        let step = match self.builds.get_mut(&t) {
            Some(q) if !q.is_empty() => Some(q.remove(0)),
            _ => None,
        };
        if self.builds.get(&t).is_some_and(|q| q.is_empty()) {
            self.builds.remove(&t);
        }
        Ok((step, Change::TileInput(t)))
    }

    /// Replaces the tile's build queue (an empty one removes it), for edits such as counting a
    /// turn of work down or dropping the road steps of a finished improvement.
    pub fn set_builds(&mut self, t: TileIdx, queue: BuildQueue) -> Result<Change, TileError> {
        self.at(t)?;
        if queue.is_empty() {
            self.builds.remove(&t);
        } else {
            self.builds.insert(t, queue);
        }
        Ok(Change::TileInput(t))
    }
}

impl core::ops::Index<TileIdx> for Tiles {
    type Output = Tile;

    /// Panics for a tile off the map: use it with indices from the grid, and [`Tiles::get`] for
    /// anything from outside.
    #[inline]
    fn index(&self, t: TileIdx) -> &Tile {
        &self.tiles[t.0 as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::ids::FeatureId;

    #[test]
    fn a_tile_round_trips_through_its_canonical_bytes() {
        let mut features = FeatureSet::EMPTY;
        features.insert(FeatureId(0));
        features.insert(FeatureId(9));
        let t = Tile::new(TerrainId(3))
            .with_wonder(Some(TerrainId(20)))
            .with_resource(Some(ResourceId(7)), 4)
            .with_improvement(Some(ImprovementId(12)))
            .with_route_bits(
                RouteBits::EMPTY.with_route(Some(Route::Railroad)).with_route_pillaged(true),
            )
            .with_claim(TileClaim::city(PlayerId(5), CityId::new(70_000).unwrap_or(CityId::FIRST)))
            .with_river(0b10_0001)
            .with_features(features);
        let bytes = t.canon_bytes();
        assert_eq!(bytes[..8], [3, 21, 8, 4, 13, 0b0110, 5, 0b10_0001]);
        assert_eq!(bytes[8..10], (1u16 | 1 << 9).to_le_bytes());
        assert_eq!(bytes[10..12], [0, 0]);
        assert_eq!(bytes[12..], 70_000u32.to_le_bytes());
        assert_eq!(Tile::from_canon_bytes(bytes), t);
        assert_eq!(t.wonder(), Some(TerrainId(20)));
        assert_eq!(t.resource(), Some(ResourceId(7)));
        assert_eq!(t.improvement(), Some(ImprovementId(12)));
        assert_eq!(t.route(), Some(Route::Railroad));
        assert!(t.route_pillaged() && !t.improvement_pillaged());
        assert!(t.river(Dir::E) && t.river(Dir::SE) && !t.river(Dir::W));
        assert_eq!(t.owner(), Some(PlayerId(5)));
        assert_eq!(t.features().top(), Some(FeatureId(9)));
    }

    #[test]
    fn a_bare_tile_has_nothing() {
        let t = Tile::new(TerrainId(0));
        assert_eq!(
            (t.wonder(), t.resource(), t.improvement(), t.route()),
            (None, None, None, None)
        );
        assert_eq!(t.claim(), TileClaim::NONE);
        assert_eq!(t.resource_amount(), 0);
        assert!(!t.has_river());
        assert_eq!(Tile::new(TerrainId(0)).with_resource(None, 9).resource_amount(), 0);
    }

    #[test]
    fn setters_report_what_changed_and_refuse_tiles_off_the_map() -> Result<(), TileError> {
        let mut tiles = Tiles::new(vec![Tile::new(TerrainId(1)); 4]);
        let city = CityId::FIRST;
        let t = TileIdx(2);
        assert_eq!(
            tiles.set_owner(t, TileClaim::city(PlayerId(1), city))?,
            Change::TileOwner { t, old: TileClaim::NONE, new: TileClaim::city(PlayerId(1), city) }
        );
        assert_eq!(tiles.set_features(t, FeatureSet::EMPTY)?, Change::TileHeight(t));
        assert_eq!(tiles.set_route(t, Some(Route::Road))?, Change::TileInput(t));
        assert_eq!(tiles.set_pillaged(t, true, true)?, Change::TileInput(t));
        assert_eq!(tiles[t].route(), Some(Route::Road));
        assert!(tiles[t].route_pillaged() && tiles[t].improvement_pillaged());
        assert_eq!(
            tiles.set_owner(TileIdx(4), TileClaim::NONE),
            Err(TileError::OffMap(TileIdx(4)))
        );
        assert!(tiles.set_river(TileIdx(9), 1).is_err());
        Ok(())
    }

    #[test]
    fn build_queues_push_pop_and_vanish_when_empty() -> Result<(), TileError> {
        let mut tiles = Tiles::new(vec![Tile::new(TerrainId(1)); 4]);
        let t = TileIdx(1);
        let farm = BuildStep { improvement: ImprovementId(3), turns_left: 5 };
        let road = BuildStep { improvement: ImprovementId(0), turns_left: 2 };
        assert_eq!(tiles.push_build(t, farm)?, Change::TileInput(t));
        assert_eq!(tiles.push_build(t, road)?, Change::TileInput(t));
        assert_eq!(tiles.builds(t), &[farm, road]);
        assert_eq!(tiles.pop_build(t)?, (Some(farm), Change::TileInput(t)));
        assert_eq!(tiles.set_builds(t, BuildQueue::new())?, Change::TileInput(t));
        assert_eq!(tiles.builds(t), &[]);
        assert_eq!(tiles.pop_build(t)?, (None, Change::TileInput(t)));
        assert_eq!(tiles.all_builds().count(), 0);
        assert!(tiles.push_build(TileIdx(8), farm).is_err());
        Ok(())
    }
}

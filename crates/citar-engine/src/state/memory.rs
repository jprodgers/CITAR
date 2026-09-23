//! What a major civilization remembers of tiles it can no longer see: the fog of war
//! (DESIGN.md 4.3).
//!
//! Replaces `Player.memory` (`state.py:252`) and its snapshots (`visibility.py:116-125`): a dict
//! from tile to `{"f", "i", "r", "o", "p", "c"}`, which was most of a player's JSON. Here a
//! snapshot is 8 bytes per tile and cities are kept aside. City-states and barbarians keep no
//! memory: Python recorded it for city-states (`visibility.py:141-146`) and never read it.

use std::collections::BTreeMap;

use crate::base::ids::{ImprovementId, PlayerId, TileIdx};
use crate::base::sets::FeatureSet;

use super::map::{RouteBits, Tile};

/// A raw player id meaning "no owner".
const NO_OWNER: u8 = 0xFF;

/// The last sight of one tile, in 8 bytes.
///
/// `flags` bit 0 says the tile has been remembered at all: Python had no entry until the tile
/// first left sight. The improvement is stored as id + 1, 0 for none. The route byte carries the
/// improvement's pillage bit, which Python kept as `"p"`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileMemory {
    features: FeatureSet,
    improvement: u8,
    route: RouteBits,
    owner: u8,
    flags: u8,
    pad: [u8; 2],
}

const _: () = assert!(size_of::<TileMemory>() == 8, "a TileMemory is 8 bytes (DESIGN.md 4.3)");

impl TileMemory {
    const SEEN: u8 = 1;

    /// No memory of the tile.
    pub const NONE: Self = Self {
        features: FeatureSet::EMPTY,
        improvement: 0,
        route: RouteBits::EMPTY,
        owner: NO_OWNER,
        flags: 0,
        pad: [0; 2],
    };

    /// What can be seen of a tile now (`visibility.py:116-122`).
    #[must_use]
    pub fn of(tile: &Tile) -> Self {
        Self {
            features: tile.features(),
            improvement: tile.improvement().map_or(0, |i| i.0.saturating_add(1)),
            route: tile.route_bits(),
            owner: tile.owner().map_or(NO_OWNER, |p| p.0),
            flags: Self::SEEN,
            pad: [0; 2],
        }
    }

    /// Whether there is a memory of the tile at all.
    #[must_use]
    pub const fn is_remembered(&self) -> bool {
        self.flags & Self::SEEN != 0
    }

    /// The features as last seen.
    #[must_use]
    pub const fn features(&self) -> FeatureSet {
        self.features
    }

    /// The improvement as last seen.
    #[must_use]
    pub fn improvement(&self) -> Option<ImprovementId> {
        self.improvement.checked_sub(1).map(ImprovementId)
    }

    /// The route and pillage state as last seen.
    #[must_use]
    pub const fn route(&self) -> RouteBits {
        self.route
    }

    /// The owner as last seen.
    #[must_use]
    pub const fn owner(&self) -> Option<PlayerId> {
        if self.owner == NO_OWNER { None } else { Some(PlayerId(self.owner)) }
    }

    /// The memory as `CANON_V1` hashes it, the padding as zero.
    #[must_use]
    pub const fn canon_bytes(&self) -> [u8; 8] {
        let f = self.features.bits().to_le_bytes();
        [f[0], f[1], self.improvement, self.route.bits(), self.owner, self.flags, 0, 0]
    }

    /// The memory [`canon_bytes`](Self::canon_bytes) wrote.
    #[must_use]
    pub const fn from_canon_bytes(b: [u8; 8]) -> Self {
        Self {
            features: FeatureSet::from_bits(u16::from_le_bytes([b[0], b[1]])),
            improvement: b[2],
            route: RouteBits::from_bits(b[3]),
            owner: b[4],
            flags: b[5],
            pad: [0; 2],
        }
    }
}

impl Default for TileMemory {
    fn default() -> Self {
        Self::NONE
    }
}

impl core::fmt::Debug for TileMemory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if !self.is_remembered() {
            return f.write_str("TileMemory(none)");
        }
        f.debug_struct("TileMemory")
            .field("features", &self.features)
            .field("improvement", &self.improvement())
            .field("route", &self.route)
            .field("owner", &self.owner())
            .finish()
    }
}

/// A city as last seen from afar (Python's `"c": [name, pop, owner]`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CityMemory {
    /// Its name then.
    pub name: Box<str>,
    /// Its population then.
    pub pop: u16,
    /// Its owner then.
    pub owner: PlayerId,
}

/// One major civilization's memory of the map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileMemoryLayer {
    tiles: Vec<TileMemory>,
    cities: BTreeMap<TileIdx, CityMemory>,
}

impl TileMemoryLayer {
    /// An empty memory of a map with `size` tiles.
    #[must_use]
    pub fn new(size: u32) -> Self {
        Self { tiles: vec![TileMemory::NONE; size as usize], cities: BTreeMap::new() }
    }

    /// A memory from its parts, as a save or the converter has them; `None` if a city memory lies
    /// off the map.
    #[must_use]
    pub fn from_parts(
        tiles: Vec<TileMemory>,
        cities: BTreeMap<TileIdx, CityMemory>,
    ) -> Option<Self> {
        let n = tiles.len();
        cities.keys().all(|t| (t.0 as usize) < n).then_some(Self { tiles, cities })
    }

    /// The number of tiles covered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// Whether the layer covers no tiles.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// The memory of `t`; [`TileMemory::NONE`] off the map.
    #[must_use]
    pub fn get(&self, t: TileIdx) -> TileMemory {
        self.tiles.get(t.0 as usize).copied().unwrap_or(TileMemory::NONE)
    }

    /// Every tile's memory, in index order.
    #[must_use]
    pub fn tiles(&self) -> &[TileMemory] {
        &self.tiles
    }

    /// The city remembered on `t`, if any.
    #[must_use]
    pub fn city(&self, t: TileIdx) -> Option<&CityMemory> {
        self.cities.get(&t)
    }

    /// Every remembered city, by tile.
    pub fn cities(&self) -> impl Iterator<Item = (TileIdx, &CityMemory)> {
        self.cities.iter().map(|(&t, c)| (t, c))
    }

    /// Records the last sight of `t`: the tile, and the city on it if there is one. A tile off
    /// the map is ignored.
    pub fn remember(&mut self, t: TileIdx, tile: &Tile, city: Option<CityMemory>) {
        let Some(slot) = self.tiles.get_mut(t.0 as usize) else { return };
        *slot = TileMemory::of(tile);
        match city {
            Some(c) => {
                self.cities.insert(t, c);
            }
            None => {
                self.cities.remove(&t);
            }
        }
    }

    /// The number of tiles remembered.
    #[must_use]
    pub fn remembered(&self) -> usize {
        self.tiles.iter().filter(|m| m.is_remembered()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::ids::{CityId, TerrainId};
    use crate::rules::defs::Route;
    use crate::state::change::TileClaim;

    #[test]
    fn a_memory_keeps_what_was_seen() {
        let tile = Tile::new(TerrainId(2))
            .with_improvement(Some(ImprovementId(4)))
            .with_route_bits(
                RouteBits::EMPTY.with_route(Some(Route::Road)).with_improvement_pillaged(true),
            )
            .with_claim(TileClaim::city(PlayerId(3), CityId::FIRST));
        let m = TileMemory::of(&tile);
        assert!(m.is_remembered());
        assert_eq!(m.improvement(), Some(ImprovementId(4)));
        assert_eq!(m.route().route(), Some(Route::Road));
        assert!(m.route().improvement_pillaged());
        assert_eq!(m.owner(), Some(PlayerId(3)));
        assert_eq!(TileMemory::from_canon_bytes(m.canon_bytes()), m);
        assert!(!TileMemory::NONE.is_remembered());
        assert_eq!(TileMemory::default(), TileMemory::NONE);
    }

    #[test]
    fn a_layer_records_tiles_and_cities() {
        let mut layer = TileMemoryLayer::new(10);
        let tile = Tile::new(TerrainId(1));
        let city = CityMemory { name: "Rome".into(), pop: 3, owner: PlayerId(1) };
        layer.remember(TileIdx(4), &tile, Some(city.clone()));
        layer.remember(TileIdx(99), &tile, None);
        assert_eq!(layer.remembered(), 1);
        assert_eq!(layer.city(TileIdx(4)), Some(&city));
        layer.remember(TileIdx(4), &tile, None);
        assert_eq!(layer.city(TileIdx(4)), None);
        assert!(!layer.get(TileIdx(99)).is_remembered());
        assert!(
            TileMemoryLayer::from_parts(vec![TileMemory::NONE; 2], [(TileIdx(5), city)].into())
                .is_none()
        );
    }
}

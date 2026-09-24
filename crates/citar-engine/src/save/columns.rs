//! The map, its tiles and each major's memory of them as base64 columns, each layer with its
//! own palette (DESIGN.md 4.9); and their canonical forms for the digest (DESIGN.md 4.10).
//!
//! A tile is 16 bytes of ids and bits. The save writes one column per field instead of one
//! object per tile: about 20 times smaller than Python's 14-element rows, and columns compress
//! well. Rule ids in a column are not the ids themselves but positions in the layer's palette,
//! a list of names of the objects the column uses, so a save still reads under a reordered
//! ruleset.
//!
//! The JSON form of the tiles:
//!
//! ```json
//! {"palette": {"terrain": [...], "feature": [...], "resource": [...], "improvement": [...]},
//!  "terrain": "<b64 u8>", "wonder": "<b64 u8>", "resource": "<b64 u8>",
//!  "resource_amount": "<b64 u8>", "improvement": "<b64 u8>", "route": "<b64 u8>",
//!  "owner": "<b64 u8>", "river": "<b64 u8>", "features": "<b64 u16le>", "city": "<b64 u32le>",
//!  "builds": [[tile, [["Farm", 5], ...]], ...]}
//! ```
//!
//! - `terrain` is a position in the terrain palette; `wonder`, `resource` and `improvement` are 0
//!   for none, else a palette position plus 1 (wonders share the terrain palette);
//! - `features` has bit `i` set for the `i`-th feature of its palette;
//! - `route`, `owner`, `river`, `resource_amount` and `city` are the tile's own bytes: 0xFF is no
//!   owner, 0 no city.
//!
//! A memory layer is the same idea over [`TileMemory`]'s fields, with a feature and an
//! improvement palette, and its remembered cities as `[tile, city]` pairs.
//!
//! Canonically, tiles and memories are the concatenated `canon_bytes` of each tile, then the build
//! queues or the remembered cities: the hash gets the tile arrays as large slices.
//!
//! Replaces the tile rows of `state.py:84-85` (`Tile._ORDER`) and the memory dicts of
//! `visibility.py:116-125` as Python saved them.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::{self, Deserializer};
use serde::ser::{self, SerializeStruct, SerializeTuple, Serializer};
use serde::{Deserialize, Serialize};

use crate::base::codec::{b64_decode, b64_encode};
use crate::base::ids::{
    CityId, FeatureId, ImprovementId, PlayerId, ResourceId, TerrainId, TileIdx,
};
use crate::base::sets::FeatureSet;
use crate::state::TileClaim;
use crate::state::map::{BuildQueue, BuildStep, MapInfo, RouteBits, Tile, Tiles};
use crate::state::memory::{CityMemory, TileMemory, TileMemoryLayer};

/// Bytes the canonical writer passes on in one piece: a length, then the bytes.
struct Raw<'a>(&'a [u8]);

impl Serialize for Raw<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(self.0)
    }
}

/// A column of `n` bytes of base64 text; an error names the column.
fn column(text: &str, what: &str, n: usize) -> Result<Vec<u8>, String> {
    // Refused before decoding: a wrong length is wrong whatever the text says.
    if text.len() != n.div_ceil(3) * 4 {
        return Err(format!("the {what} column does not hold {n} values"));
    }
    let bytes = b64_decode(text).map_err(|e| format!("the {what} column: {e}"))?;
    if bytes.len() == n {
        Ok(bytes)
    } else {
        Err(format!("the {what} column holds {} values, not {n}", bytes.len()))
    }
}

/// A column of `n` little-endian values of `W` bytes each.
fn wide_column<const W: usize>(text: &str, what: &str, n: usize) -> Result<Vec<[u8; W]>, String> {
    let bytes = column(text, what, n * W)?;
    Ok(bytes.as_chunks::<W>().0.to_vec())
}

/// The ids a column uses, sorted, and each id's position in that list.
struct Palette<I> {
    ids: Vec<I>,
    pos: BTreeMap<I, u8>,
}

impl<I: Copy + Ord + core::fmt::Debug> Palette<I> {
    fn new(ids: BTreeSet<I>, what: &str) -> Result<Self, String> {
        let ids: Vec<I> = ids.into_iter().collect();
        if ids.len() > 255 {
            return Err(format!("{} kinds of {what} do not fit a palette", ids.len()));
        }
        let pos = ids.iter().enumerate().map(|(i, &id)| (id, i as u8)).collect();
        Ok(Self { ids, pos })
    }

    /// The position of `id`; every id was listed when the palette was made.
    fn at(&self, id: I) -> u8 {
        self.pos.get(&id).copied().unwrap_or(0)
    }

    /// 0 for none, else the position plus 1.
    fn at_opt(&self, id: Option<I>) -> u8 {
        id.map_or(0, |i| self.at(i) + 1)
    }
}

/// The id at a stored palette position, plus 1 when `offset` (0 being none).
fn from_palette<I: Copy>(
    palette: &[I],
    stored: u8,
    offset: bool,
    what: &str,
    tile: usize,
) -> Result<Option<I>, String> {
    let i = if offset {
        match stored.checked_sub(1) {
            None => return Ok(None),
            Some(i) => i,
        }
    } else {
        stored
    };
    palette
        .get(usize::from(i))
        .copied()
        .map(Some)
        .ok_or_else(|| format!("tile {tile}: {what} {stored} is past its palette"))
}

/// A feature set with its bits renumbered from one numbering to another.
fn remap_features(bits: u16, map: &[FeatureId], what: &str, tile: usize) -> Result<u16, String> {
    let mut out = FeatureSet::EMPTY;
    for i in 0..16u8 {
        if bits & (1 << i) != 0 {
            let f = map
                .get(usize::from(i))
                .ok_or_else(|| format!("tile {tile}: {what} bit {i} is past its palette"))?;
            out.insert(*f);
        }
    }
    Ok(out.bits())
}

// ---- MapInfo ----------------------------------------------------------------------------------

/// JSON: the shape, and the continents as a `u16le` column (empty before map generation).
/// `CANON_V1`: the shape, then the continents' bytes.
impl Serialize for MapInfo {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let continents: Vec<u8> = self.continents.iter().flat_map(|c| c.to_le_bytes()).collect();
        if s.is_human_readable() {
            let mut st = s.serialize_struct("MapInfo", 5)?;
            st.serialize_field("width", &self.width)?;
            st.serialize_field("height", &self.height)?;
            st.serialize_field("wrap_x", &self.wrap_x)?;
            st.serialize_field("wrap_y", &self.wrap_y)?;
            st.serialize_field("continents", &b64_encode(&continents))?;
            st.end()
        } else {
            let mut t = s.serialize_tuple(5)?;
            t.serialize_element(&self.width)?;
            t.serialize_element(&self.height)?;
            t.serialize_element(&self.wrap_x)?;
            t.serialize_element(&self.wrap_y)?;
            t.serialize_element(&Raw(&continents))?;
            t.end()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MapDoc {
    width: u16,
    height: u16,
    wrap_x: bool,
    wrap_y: bool,
    continents: String,
}

impl<'de> Deserialize<'de> for MapInfo {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let doc = MapDoc::deserialize(d)?;
        let n = if doc.continents.is_empty() {
            0
        } else {
            usize::from(doc.width) * usize::from(doc.height)
        };
        let continents = wide_column::<2>(&doc.continents, "continent", n)
            .map_err(de::Error::custom)?
            .into_iter()
            .map(u16::from_le_bytes)
            .collect();
        Ok(Self {
            width: doc.width,
            height: doc.height,
            wrap_x: doc.wrap_x,
            wrap_y: doc.wrap_y,
            continents,
        })
    }
}

// ---- Build queues -----------------------------------------------------------------------------

/// `[improvement, turns left]`, Python's own pair (`state.py:82`), in both encodings.
impl Serialize for BuildStep {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (self.improvement, self.turns_left).serialize(s)
    }
}

impl<'de> Deserialize<'de> for BuildStep {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (improvement, turns_left) = <(ImprovementId, i16)>::deserialize(d)?;
        Ok(Self { improvement, turns_left })
    }
}

// ---- Tiles ------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TilePalettes {
    terrain: Vec<TerrainId>,
    feature: Vec<FeatureId>,
    resource: Vec<ResourceId>,
    improvement: Vec<ImprovementId>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TilesDoc {
    palette: TilePalettes,
    terrain: String,
    wonder: String,
    resource: String,
    resource_amount: String,
    improvement: String,
    route: String,
    owner: String,
    river: String,
    features: String,
    city: String,
    builds: Vec<(TileIdx, Vec<BuildStep>)>,
}

impl TilesDoc {
    fn of(tiles: &Tiles) -> Result<Self, String> {
        let all = tiles.as_slice();
        let terrain = Palette::new(
            all.iter().flat_map(|t| [Some(t.terrain()), t.wonder()]).flatten().collect(),
            "terrain",
        )?;
        let mut used = FeatureSet::EMPTY;
        for t in all {
            used = FeatureSet::from_bits(used.bits() | t.features().bits());
        }
        let feature: Vec<FeatureId> = used.iter().collect();
        let resource = Palette::new(all.iter().filter_map(Tile::resource).collect(), "resource")?;
        let improvement =
            Palette::new(all.iter().filter_map(Tile::improvement).collect(), "improvement")?;
        let col = |f: &dyn Fn(&Tile) -> u8| -> String {
            b64_encode(&all.iter().map(f).collect::<Vec<u8>>())
        };
        let features: Vec<u8> = all
            .iter()
            .flat_map(|t| {
                let mut bits = 0u16;
                for (i, f) in feature.iter().enumerate() {
                    if t.features().contains(*f) {
                        bits |= 1 << i;
                    }
                }
                bits.to_le_bytes()
            })
            .collect();
        let city: Vec<u8> =
            all.iter().flat_map(|t| t.city().map_or(0, CityId::get).to_le_bytes()).collect();
        Ok(Self {
            terrain: col(&|t| terrain.at(t.terrain())),
            wonder: col(&|t| terrain.at_opt(t.wonder())),
            resource: col(&|t| resource.at_opt(t.resource())),
            resource_amount: col(&|t| t.resource_amount()),
            improvement: col(&|t| improvement.at_opt(t.improvement())),
            route: col(&|t| t.route_bits().bits()),
            owner: col(&|t| t.owner().map_or(0xFF, |p| p.0)),
            river: col(&|t| t.river_mask()),
            features: b64_encode(&features),
            city: b64_encode(&city),
            builds: tiles.all_builds().map(|(t, q)| (t, q.to_vec())).collect(),
            palette: TilePalettes {
                terrain: terrain.ids,
                feature,
                resource: resource.ids,
                improvement: improvement.ids,
            },
        })
    }

    fn into_tiles(self) -> Result<Tiles, String> {
        let p = &self.palette;
        if p.feature.len() > 16 {
            return Err(format!("{} features do not fit a tile", p.feature.len()));
        }
        let n = b64_decode(&self.terrain).map_err(|e| format!("the terrain column: {e}"))?.len();
        let terrain = column(&self.terrain, "terrain", n)?;
        let wonder = column(&self.wonder, "wonder", n)?;
        let resource = column(&self.resource, "resource", n)?;
        let amount = column(&self.resource_amount, "resource_amount", n)?;
        let improvement = column(&self.improvement, "improvement", n)?;
        let route = column(&self.route, "route", n)?;
        let owner = column(&self.owner, "owner", n)?;
        let river = column(&self.river, "river", n)?;
        let features = wide_column::<2>(&self.features, "features", n)?;
        let city = wide_column::<4>(&self.city, "city", n)?;
        let mut tiles = Vec::with_capacity(n);
        for i in 0..n {
            let base = from_palette(&p.terrain, terrain[i], false, "terrain", i)?
                .ok_or_else(|| format!("tile {i} has no terrain"))?;
            let res = from_palette(&p.resource, resource[i], true, "resource", i)?;
            if res.is_none() && amount[i] != 0 {
                return Err(format!("tile {i} has an amount of no resource"));
            }
            if route[i] > 0b1111 {
                return Err(format!("tile {i}: route byte {} has unknown bits", route[i]));
            }
            if river[i] > 0b11_1111 {
                return Err(format!("tile {i}: river mask {} has unknown bits", river[i]));
            }
            let claim = TileClaim {
                owner: (owner[i] != 0xFF).then_some(PlayerId(owner[i])),
                city: CityId::new(u32::from_le_bytes(city[i])),
            };
            let bits = remap_features(u16::from_le_bytes(features[i]), &p.feature, "feature", i)?;
            tiles.push(
                Tile::new(base)
                    .with_wonder(from_palette(&p.terrain, wonder[i], true, "wonder", i)?)
                    .with_resource(res, amount[i])
                    .with_improvement(from_palette(
                        &p.improvement,
                        improvement[i],
                        true,
                        "improvement",
                        i,
                    )?)
                    .with_route_bits(RouteBits::from_bits(route[i]))
                    .with_claim(claim)
                    .with_river(river[i])
                    .with_features(FeatureSet::from_bits(bits)),
            );
        }
        let mut builds = BTreeMap::new();
        for (t, steps) in self.builds {
            let queue: BuildQueue = steps.into_iter().collect();
            if builds.insert(t, queue).is_some() {
                return Err(format!("tile {t} has two build queues"));
            }
        }
        Tiles::from_parts(tiles, builds).map_err(|e| e.to_string())
    }
}

/// JSON: the columns of the module doc. `CANON_V1`: every tile's `canon_bytes` as one byte
/// string, then the build queues by tile.
impl Serialize for Tiles {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            return TilesDoc::of(self).map_err(ser::Error::custom)?.serialize(s);
        }
        let mut bytes = Vec::with_capacity(self.len() * 16);
        for t in self.as_slice() {
            bytes.extend_from_slice(&t.canon_bytes());
        }
        let builds: BTreeMap<TileIdx, &[BuildStep]> = self.all_builds().collect();
        let mut t = s.serialize_tuple(2)?;
        t.serialize_element(&Raw(&bytes))?;
        t.serialize_element(&builds)?;
        t.end()
    }
}

/// The columns; the map's size is the columns' length, which `State::from_parts` checks against
/// the map.
impl<'de> Deserialize<'de> for Tiles {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        TilesDoc::deserialize(d)?.into_tiles().map_err(de::Error::custom)
    }
}

// ---- Memory layers ----------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryPalettes {
    feature: Vec<FeatureId>,
    improvement: Vec<ImprovementId>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryDoc {
    palette: MemoryPalettes,
    features: String,
    improvement: String,
    route: String,
    owner: String,
    flags: String,
    cities: Vec<(TileIdx, CityMemory)>,
}

impl MemoryDoc {
    fn of(m: &TileMemoryLayer) -> Result<Self, String> {
        let all = m.tiles();
        let mut used = FeatureSet::EMPTY;
        for t in all {
            used = FeatureSet::from_bits(used.bits() | t.features().bits());
        }
        let feature: Vec<FeatureId> = used.iter().collect();
        let improvement =
            Palette::new(all.iter().filter_map(TileMemory::improvement).collect(), "improvement")?;
        let col = |f: &dyn Fn(&[u8; 8]) -> u8| -> String {
            b64_encode(&all.iter().map(|t| f(&t.canon_bytes())).collect::<Vec<u8>>())
        };
        let features: Vec<u8> = all
            .iter()
            .flat_map(|t| {
                let mut bits = 0u16;
                for (i, f) in feature.iter().enumerate() {
                    if t.features().contains(*f) {
                        bits |= 1 << i;
                    }
                }
                bits.to_le_bytes()
            })
            .collect();
        let imp: Vec<u8> = all.iter().map(|t| improvement.at_opt(t.improvement())).collect();
        Ok(Self {
            features: b64_encode(&features),
            improvement: b64_encode(&imp),
            route: col(&|b| b[3]),
            owner: col(&|b| b[4]),
            flags: col(&|b| b[5]),
            cities: m.cities().map(|(t, c)| (t, c.clone())).collect(),
            palette: MemoryPalettes { feature, improvement: improvement.ids },
        })
    }

    fn into_layer(self) -> Result<TileMemoryLayer, String> {
        let p = &self.palette;
        if p.feature.len() > 16 {
            return Err(format!("{} features do not fit a tile", p.feature.len()));
        }
        let n = b64_decode(&self.improvement)
            .map_err(|e| format!("the improvement column: {e}"))?
            .len();
        let features = wide_column::<2>(&self.features, "features", n)?;
        let improvement = column(&self.improvement, "improvement", n)?;
        let route = column(&self.route, "route", n)?;
        let owner = column(&self.owner, "owner", n)?;
        let flags = column(&self.flags, "flags", n)?;
        let mut tiles = Vec::with_capacity(n);
        for i in 0..n {
            if route[i] > 0b1111 {
                return Err(format!("tile {i}: route byte {} has unknown bits", route[i]));
            }
            if flags[i] > 1 {
                return Err(format!("tile {i}: memory flags {} have unknown bits", flags[i]));
            }
            let bits = remap_features(u16::from_le_bytes(features[i]), &p.feature, "feature", i)?;
            let imp = from_palette(&p.improvement, improvement[i], true, "improvement", i)?;
            let f = bits.to_le_bytes();
            let stored = imp.map_or(0, |x| x.0.saturating_add(1));
            tiles.push(TileMemory::from_canon_bytes([
                f[0], f[1], stored, route[i], owner[i], flags[i], 0, 0,
            ]));
        }
        let mut cities = BTreeMap::new();
        for (t, c) in self.cities {
            if cities.insert(t, c).is_some() {
                return Err(format!("tile {t} remembers two cities"));
            }
        }
        TileMemoryLayer::from_parts(tiles, cities)
            .ok_or_else(|| "a remembered city lies off the map".to_owned())
    }
}

/// JSON: the columns of the module doc. `CANON_V1`: every tile's `canon_bytes` as one byte
/// string, then the remembered cities by tile.
impl Serialize for TileMemoryLayer {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            return MemoryDoc::of(self).map_err(ser::Error::custom)?.serialize(s);
        }
        let mut bytes = Vec::with_capacity(self.len() * 8);
        for t in self.tiles() {
            bytes.extend_from_slice(&t.canon_bytes());
        }
        let cities: BTreeMap<TileIdx, &CityMemory> = self.cities().collect();
        let mut t = s.serialize_tuple(2)?;
        t.serialize_element(&Raw(&bytes))?;
        t.serialize_element(&cities)?;
        t.end()
    }
}

impl<'de> Deserialize<'de> for TileMemoryLayer {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        MemoryDoc::deserialize(d)?.into_layer().map_err(de::Error::custom)
    }
}

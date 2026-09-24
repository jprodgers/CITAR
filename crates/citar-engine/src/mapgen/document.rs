//! Editor maps: the map editor's document, read and cleaned (`maps.py:48-60, 124-248`).
//!
//! A document is plain JSON, written by the editor or by hand:
//!
//! ```text
//! {"id": "twin-islands", "width": 40, "height": 26, "wrap_x": false, "wrap_y": false,
//!  "tiles": [[terrain, [features], wonder, river_mask, resource, amount, improvement, route], ...],
//!  "starts": [idx, ...], "cs_starts": [idx, ...]}
//! ```
//!
//! [`read`] ports `maps.validate` with `fix=True` and `maps.tiles_from_rows`: an unknown base
//! terrain, a size out of bounds or a tile count that does not fill the map refuses the document;
//! anything else that is wrong is removed and reported as a warning, as the editor shows them.
//! Rivers are kept on both sides of an edge and never along water, and start positions on water,
//! on impassable terrain or on a natural wonder are dropped.
//!
//! What differs from Python, on purpose (`tests/rules/intended.toml`,
//! `map-documents-read-by-rule`):
//! - which improvements a map may carry is a rule, not Python's list of 19 names
//!   (`MAP_IMPROVEMENTS`, `maps.py:32-35`): an improvement proper (not a route, a removal, a
//!   repair or a cancelled order), neither the city centre nor a great person's. So a Citadel,
//!   the one great improvement Python's list named, is removed, and a new ruleset's improvements
//!   need no list;
//! - on water, a route is removed, and so is an improvement built only on land (one whose
//!   terrains are all land) or one of the ruins and encampments; Python named four (Farm, Mine,
//!   Ancient ruins, Barbarian encampment) and kept a Lumber mill or a Fort on the ocean;
//! - a tile that is not a list, or a start that is not a whole number, is refused or dropped
//!   with a warning, where Python took the characters of a string for a row's fields.

use serde_json::{Map, Value};

use crate::base::hex::{Dir, HexGrid, MAX_SIDE, MIN_SIDE};
use crate::base::ids::{ImprovementId, ResourceId, TerrainId, TileIdx};
use crate::base::py;
use crate::base::sets::FeatureSet;
use crate::rules::Ruleset;
use crate::rules::defs::{ImprovementKind, ResourceType, Route, TerrainType};
use crate::state::map::{RouteBits, Tile};

/// The most warnings a document reports (`maps.py:159-160`).
pub const MAX_WARNINGS: usize = 200;

/// A document the engine cannot read, with the reason the editor shows (`maps.MapError`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MapError(pub String);

/// An editor map, read and cleaned: what a game on it starts from.
#[derive(Clone, Debug, PartialEq)]
pub struct MapDocument {
    pub width: u16,
    pub height: u16,
    pub wrap_x: bool,
    /// Only on an even height: odd rows are offset, so only an even height tiles north-south.
    pub wrap_y: bool,
    /// Every tile, row by row.
    pub tiles: Vec<Tile>,
    /// The major civilizations' start tiles, in seat order, as the document gives them: on land
    /// a unit can stand on, and each once.
    pub starts: Vec<TileIdx>,
    /// The city-states' start tiles, none of them a major's.
    pub cs_starts: Vec<TileIdx>,
    /// What was removed or looks wrong, as `(x,y) what`, at most [`MAX_WARNINGS`].
    pub warnings: Vec<String>,
}

impl MapDocument {
    /// The document's grid.
    pub fn grid(&self) -> Result<HexGrid, MapError> {
        HexGrid::new(self.width, self.height, self.wrap_x, self.wrap_y)
            .map_err(|_| MapError(size_message()))
    }
}

fn size_message() -> String {
    format!("Maps must be between {MIN_SIDE} and {MAX_SIDE} tiles on each side.")
}

/// The width and height of a document, read as Python's `int()` reads them, within the grid's
/// bounds (`maps.py:127-137`).
pub fn dimensions(doc: &Value) -> Result<(u16, u16), MapError> {
    let o = doc.as_object().ok_or_else(|| MapError("A map must be a JSON object.".to_owned()))?;
    let side = |k: &str| o.get(k).and_then(py::int_of);
    let (Some(w), Some(h)) = (side("width"), side("height")) else {
        return Err(MapError("A map needs integer width and height.".to_owned()));
    };
    let fits = |n: i64| (i64::from(MIN_SIDE)..=i64::from(MAX_SIDE)).contains(&n);
    if !fits(w) || !fits(h) {
        return Err(MapError(size_message()));
    }
    // Both are within 8..=256.
    Ok((u16::try_from(w).unwrap_or(MIN_SIDE), u16::try_from(h).unwrap_or(MIN_SIDE)))
}

/// One tile's row, its names not yet looked up (`tiles_from_rows`, `maps.py:52-60`).
struct Row<'a> {
    terrain: &'a Value,
    features: Vec<&'a Value>,
    wonder: &'a Value,
    river: i64,
    resource: &'a Value,
    amount: i64,
    improvement: &'a Value,
    route: &'a Value,
}

const NULL: Value = Value::Null;

impl<'a> Row<'a> {
    /// A row of the compact form; a shorter row is padded with nothing.
    fn of(v: &'a Value) -> Option<Self> {
        let cells = v.as_array()?;
        let cell = |i: usize| cells.get(i).unwrap_or(&NULL);
        let whole = |i: usize| if py::truthy(cell(i)) { py::int_of(cell(i)) } else { Some(0) };
        Some(Self {
            terrain: cell(0),
            features: match cell(1) {
                Value::Array(a) => a.iter().collect(),
                v if !py::truthy(v) => Vec::new(),
                v => vec![v],
            },
            wonder: cell(2),
            river: whole(3)?,
            resource: cell(4),
            amount: whole(5)?,
            improvement: cell(6),
            route: cell(7),
        })
    }
}

/// A name of the document as Python quoted it in a warning: `'Forest'`, or `None`.
fn shown(v: &Value) -> String {
    py::str_of(v)
}

/// Reads and cleans an editor map (`maps.validate` with `fix=True`, `maps.py:124-248`).
pub fn read(rules: &Ruleset, doc: &Value) -> Result<MapDocument, MapError> {
    let (width, height) = dimensions(doc)?;
    let o: &Map<String, Value> =
        doc.as_object().ok_or_else(|| MapError("A map must be a JSON object.".to_owned()))?;
    let rows: &[Value] = match o.get("tiles") {
        Some(Value::Array(a)) => a,
        Some(v) if py::truthy(v) => {
            return Err(MapError("A map's tiles must be a list of rows.".to_owned()));
        }
        _ => &[],
    };
    let size = usize::from(width) * usize::from(height);
    if rows.len() != size {
        return Err(MapError(format!(
            "A {width}x{height} map needs {size} tiles, got {}.",
            rows.len()
        )));
    }
    let wrap_x = o.get("wrap_x").is_some_and(py::truthy);
    let wrap_y = o.get("wrap_y").is_some_and(py::truthy) && height % 2 == 0;
    let grid = HexGrid::new(width, height, wrap_x, wrap_y).map_err(|_| MapError(size_message()))?;
    let mut warnings = Warnings { width, list: Vec::new() };
    let mut tiles = Vec::with_capacity(size);
    for (i, row) in rows.iter().enumerate() {
        let at = u32::try_from(i).unwrap_or(u32::MAX);
        let (x, y) = (at % u32::from(width), at / u32::from(width));
        let row = Row::of(row).ok_or_else(|| {
            MapError(format!("Tile ({x},{y}) is not a row of the map format ([terrain, ...])."))
        })?;
        tiles.push(tile(rules, &row, at, &mut warnings)?);
    }
    mirror_rivers(rules, &grid, &mut tiles);
    let starts = clean_starts(rules, o.get("starts"), &tiles, &mut warnings);
    let cs_starts: Vec<TileIdx> = clean_starts(rules, o.get("cs_starts"), &tiles, &mut warnings)
        .into_iter()
        .filter(|s| !starts.contains(s))
        .collect();
    if o.get("wrap_y").is_some_and(py::truthy) && !wrap_y {
        warnings.push_plain(
            "North-south wrapping needs an even height; this map does not wrap north-south.",
        );
    }
    Ok(MapDocument {
        width,
        height,
        wrap_x,
        wrap_y,
        tiles,
        starts,
        cs_starts,
        warnings: warnings.list,
    })
}

struct Warnings {
    width: u16,
    list: Vec<String>,
}

impl Warnings {
    /// A problem with a tile, as `(x,y) what` (`maps.py:157-160`).
    fn push(&mut self, at: u32, what: &str) {
        if self.list.len() < MAX_WARNINGS {
            let w = u32::from(self.width);
            self.list.push(format!("({},{}) {what}", at % w, at / w));
        }
    }

    /// A problem with the whole map, which Python reported past the cap too.
    fn push_plain(&mut self, what: &str) {
        self.list.push(what.to_owned());
    }
}

/// One tile, cleaned (`maps.py:162-205`).
fn tile(rules: &Ruleset, row: &Row<'_>, at: u32, warn: &mut Warnings) -> Result<Tile, MapError> {
    let terrains = rules.terrains();
    let w = u32::from(warn.width);
    let terrain: TerrainId = row
        .terrain
        .as_str()
        .and_then(|t| rules.lookup::<TerrainId>(t))
        .filter(|&t| matches!(terrains[t].kind, TerrainType::Land | TerrainType::Water))
        .ok_or_else(|| {
            MapError(format!(
                "Tile ({},{}) has unknown base terrain '{}'.",
                at % w,
                at / w,
                shown(row.terrain)
            ))
        })?;
    let water = terrains[terrain].kind == TerrainType::Water;

    // Features: known ones, each on what it may lie on, the base or a feature below it; hills
    // first, which the layer order of a FeatureSet keeps.
    let mut features = FeatureSet::EMPTY;
    let mut accepted: Vec<TerrainId> = Vec::new();
    for f in &row.features {
        let def = f.as_str().and_then(|n| rules.lookup::<TerrainId>(n)).and_then(|id| {
            let d = &terrains[id];
            (d.kind == TerrainType::TerrainFeature).then_some((id, d.feature?))
        });
        let Some((id, fid)) = def else {
            warn.push(at, &format!("unknown feature '{}' removed", shown(f)));
            continue;
        };
        let occurs = &terrains[id].occurs_on;
        if !occurs.is_empty()
            && !occurs.contains(&terrain)
            && !accepted.iter().any(|a| occurs.contains(a))
        {
            warn.push(
                at,
                &format!("{} cannot be on {}; removed", terrains[id].name, terrains[terrain].name),
            );
            continue;
        }
        if features.insert(fid) {
            accepted.push(id);
        }
    }
    // Hills first, the rest as given (`maps.py:176-177`): the top feature, which the resource
    // check reads, is the last of that order.
    let hill = rules.derived().known.hill;
    accepted.sort_by_key(|&id| terrains[id].feature != Some(hill));

    let wonder: Option<TerrainId> = match row.wonder {
        v if !py::truthy(v) => None,
        v => {
            let w = v
                .as_str()
                .and_then(|n| rules.lookup::<TerrainId>(n))
                .filter(|&t| terrains[t].kind == TerrainType::NaturalWonder);
            if w.is_none() {
                warn.push(at, &format!("unknown natural wonder '{}' removed", shown(v)));
            }
            w
        }
    };

    let (resource, amount) = resource(rules, row, terrain, &accepted, at, warn);
    let mut improvement = improvement(rules, row.improvement, at, warn);
    let mut route = match row.route {
        Value::Null => None,
        Value::String(s) if s == "Road" => Some(Route::Road),
        Value::String(s) if s == "Railroad" => Some(Route::Railroad),
        v => {
            warn.push(at, &format!("unknown route '{}' removed", shown(v)));
            None
        }
    };
    let land_only = improvement.is_some_and(|i| land_only(rules, i));
    if water && (route.is_some() || land_only) {
        warn.push(at, "land-only improvement or route on water removed");
        route = None;
        if land_only {
            improvement = None;
        }
    }
    // Six edges; the higher bits mean nothing (`maps.py:206`).
    let river = u8::try_from(row.river & 63).unwrap_or(0);
    Ok(Tile::new(terrain)
        .with_features(features)
        .with_wonder(wonder)
        .with_river(river)
        .with_resource(resource, amount)
        .with_improvement(improvement)
        .with_route_bits(RouteBits::EMPTY.with_route(route)))
}

/// The resource and its deposit size (`maps.py:181-194`): an unknown one is removed; one that does
/// not normally occur on the tile is kept with a warning; a strategic one without a size gets the
/// ruleset's minor deposit, 2 if it gives none; any other has no size.
fn resource(
    rules: &Ruleset,
    row: &Row<'_>,
    terrain: TerrainId,
    features: &[TerrainId],
    at: u32,
    warn: &mut Warnings,
) -> (Option<ResourceId>, u8) {
    let v = row.resource;
    if !py::truthy(v) {
        return (None, 0);
    }
    let Some(id) = v.as_str().and_then(|n| rules.lookup::<ResourceId>(n)) else {
        warn.push(at, &format!("unknown resource '{}' removed", shown(v)));
        return (None, 0);
    };
    let def = &rules.resources()[id];
    let top = features.last().copied().unwrap_or(terrain);
    let found_on = &def.terrains_can_be_found_on;
    if !found_on.contains(&top) && !found_on.contains(&terrain) {
        let name = &rules.terrains()[top].name;
        warn.push(at, &format!("{} does not normally occur on {name} (kept)", def.name));
    }
    let amount = match def.kind {
        ResourceType::Strategic if row.amount == 0 => {
            def.minor_deposit_amount.map_or(2, |d| i64::from(d.default))
        }
        ResourceType::Strategic => row.amount,
        _ => 0,
    };
    (Some(id), u8::try_from(amount.clamp(0, i64::from(u8::MAX))).unwrap_or(u8::MAX))
}

/// The improvement, if a map may carry it (`maps.py:195-197`).
fn improvement(rules: &Ruleset, v: &Value, at: u32, warn: &mut Warnings) -> Option<ImprovementId> {
    if !py::truthy(v) {
        return None;
    }
    let id =
        v.as_str().and_then(|n| rules.lookup::<ImprovementId>(n)).filter(|&i| on_maps(rules, i));
    if id.is_none() {
        warn.push(
            at,
            &format!(
                "improvement '{}' is not allowed on a map (use a scenario); removed",
                shown(v)
            ),
        );
    }
    id
}

/// Whether a map may carry an improvement before the game starts: an improvement proper, not the
/// city centre and not a great person's. Routes, removals, repairs and orders belong to play,
/// and cities and great improvements to a scenario.
// refcheck: map-documents-read-by-rule
#[must_use]
pub fn on_maps(rules: &Ruleset, i: ImprovementId) -> bool {
    let def = &rules.improvements()[i];
    def.kind == ImprovementKind::Normal
        && !def.great
        && Some(i) != rules.derived().known.city_center
}

/// Whether an improvement belongs on land only: the ruins, the encampments, or one whose terrains
/// are all land.
fn land_only(rules: &Ruleset, i: ImprovementId) -> bool {
    let known = &rules.derived().known;
    if [known.ancient_ruins, known.barbarian_camp, known.city_ruins].contains(&Some(i)) {
        return true;
    }
    let on = &rules.improvements()[i].terrains_can_be_built_on;
    let terrains = rules.terrains();
    on.iter().next().is_some() && on.iter().all(|t| terrains[t].kind != TerrainType::Water)
}

/// Rivers on both sides of each edge, and never along water (`maps.py:207-215`), in tile order
/// as Python did them.
fn mirror_rivers(rules: &Ruleset, grid: &HexGrid, tiles: &mut [Tile]) {
    let water = |t: &Tile| rules.terrains()[t.terrain()].kind == TerrainType::Water;
    for i in 0..tiles.len() {
        let at = TileIdx(u32::try_from(i).unwrap_or(u32::MAX));
        for d in Dir::ALL {
            let mask = tiles[i].river_mask();
            if mask & (1 << d.index()) == 0 {
                continue;
            }
            let n = grid.neighbor(at, d).map(|n| n.0 as usize);
            match n {
                Some(n) if !water(&tiles[n]) && !water(&tiles[i]) => {
                    let back = tiles[n].river_mask() | (1 << d.opposite().index());
                    tiles[n] = tiles[n].with_river(back);
                }
                _ => tiles[i] = tiles[i].with_river(mask & !(1 << d.index())),
            }
        }
    }
}

/// A list of start positions, cleaned (`maps.py:217-235`): whole numbers on the map, not on
/// water, impassable terrain or a natural wonder, each once.
fn clean_starts(
    rules: &Ruleset,
    list: Option<&Value>,
    tiles: &[Tile],
    warn: &mut Warnings,
) -> Vec<TileIdx> {
    let mut out: Vec<TileIdx> = Vec::new();
    let Some(Value::Array(list)) = list else { return out };
    for v in list {
        let Some(s) = py::int_of(v).and_then(|s| u32::try_from(s).ok()) else { continue };
        let Some(t) = tiles.get(s as usize) else { continue };
        let def = &rules.terrains()[t.terrain()];
        if def.kind == TerrainType::Water || def.impassable || t.wonder().is_some() {
            warn.push(s, &format!("start position on {} removed", def.name));
            continue;
        }
        let at = TileIdx(s);
        if !out.contains(&at) {
            out.push(at);
        }
    }
    out
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use serde_json::json;

    use super::*;

    fn blank(w: u16, h: u16) -> Value {
        let rows: Vec<Value> = (0..u32::from(w) * u32::from(h))
            .map(|_| json!(["Grassland", [], null, 0, null, 0, null, null]))
            .collect();
        json!({"width": w, "height": h, "tiles": rows})
    }

    #[test]
    fn a_blank_document_reads_whole() {
        let r = Ruleset::shared();
        let doc = read(r, &blank(10, 8)).expect("a blank map");
        assert_eq!((doc.width, doc.height, doc.tiles.len()), (10, 8, 80));
        assert!(doc.warnings.is_empty() && doc.starts.is_empty());
    }

    #[test]
    fn documents_that_cannot_be_read_say_why() {
        let r = Ruleset::shared();
        let err = |v: Value| read(r, &v).expect_err("refused").0;
        assert_eq!(err(json!([1])), "A map must be a JSON object.");
        assert_eq!(
            err(json!({"width": "x", "height": 8})),
            "A map needs integer width and height."
        );
        assert_eq!(err(json!({"width": 4, "height": 8})), size_message());
        assert_eq!(
            err(json!({"width": 8, "height": 8, "tiles": []})),
            "A 8x8 map needs 64 tiles, got 0."
        );
        let mut doc = blank(8, 8);
        doc["tiles"][9][0] = json!("Nowhere");
        assert_eq!(err(doc), "Tile (1,1) has unknown base terrain 'Nowhere'.");
    }

    /// The warnings are Python's for the same document (`maps.validate`), but for the Citadel.
    #[test]
    fn what_is_wrong_on_a_tile_is_removed_and_reported() {
        let r = Ruleset::shared();
        let mut doc = blank(8, 8);
        doc["tiles"][0] = json!([
            "Grassland",
            ["Hill", "Forest", "Nothing", "Oasis"],
            "Plains",
            0,
            "Unobtainium",
            0,
            "Road",
            "Canal"
        ]);
        doc["tiles"][1] = json!(["Ocean", [], null, 0, "Iron", 0, "Farm", "Road"]);
        doc["tiles"][2] = json!(["Plains", [], null, 0, "Iron", 0, "Mine", "Railroad"]);
        doc["tiles"][3] = json!(["Grassland", [], null, 0, "Wheat", 5, "Citadel", null]);
        // Deer in a forest on a hill: the forest is the top feature, however the row orders them.
        doc["tiles"][4] = json!(["Grassland", ["Forest", "Hill"], null, 0, "Deer", 0, null, null]);
        let d = read(r, &doc).expect("fixed, not refused");
        let t0 = &d.tiles[0];
        let hill = r.derived().known.hill;
        assert!(t0.features().contains(hill));
        assert_eq!(t0.features().iter().count(), 2, "Hill and Forest are kept");
        assert_eq!(
            (t0.wonder(), t0.resource(), t0.improvement(), t0.route()),
            (None, None, None, None)
        );
        let t1 = &d.tiles[1];
        assert_eq!((t1.improvement(), t1.route()), (None, None), "a farm and a road on the ocean");
        let iron = r.lookup::<ResourceId>("Iron");
        assert_eq!((d.tiles[2].resource(), d.tiles[2].resource_amount()), (iron, 2));
        assert_eq!(d.tiles[2].route(), Some(Route::Railroad));
        assert_eq!(d.tiles[3].resource_amount(), 0, "a bonus resource has no deposit size");
        assert_eq!(d.tiles[3].improvement(), None, "a great improvement belongs to a scenario");
        assert_eq!(d.tiles[4].features().iter().count(), 2);
        assert_eq!(d.tiles[4].resource(), r.lookup::<ResourceId>("Deer"));
        assert_eq!(
            d.warnings,
            [
                "(0,0) unknown feature 'Nothing' removed",
                "(0,0) Oasis cannot be on Grassland; removed",
                "(0,0) unknown natural wonder 'Plains' removed",
                "(0,0) unknown resource 'Unobtainium' removed",
                "(0,0) improvement 'Road' is not allowed on a map (use a scenario); removed",
                "(0,0) unknown route 'Canal' removed",
                "(1,0) Iron does not normally occur on Ocean (kept)",
                "(1,0) land-only improvement or route on water removed",
                "(3,0) Wheat does not normally occur on Grassland (kept)",
                // Python's list kept the Citadel (refcheck: map-documents-read-by-rule).
                "(3,0) improvement 'Citadel' is not allowed on a map (use a scenario); removed",
            ]
        );
    }

    #[test]
    fn rivers_lie_on_both_sides_of_an_edge_and_never_along_water() {
        let r = Ruleset::shared();
        let mut doc = blank(8, 8);
        // (1,1) has a river on its east edge; (0,0) one on its west, off the map.
        doc["tiles"][9][3] = json!(1);
        doc["tiles"][0][3] = json!(1 << 3);
        doc["tiles"][20] = json!(["Coast", [], null, 1, null, 0, null, null]);
        let d = read(r, &doc).expect("a map");
        assert_eq!(d.tiles[9].river_mask(), 1);
        assert_eq!(d.tiles[10].river_mask(), 1 << 3, "the east neighbour's west edge");
        assert_eq!(d.tiles[0].river_mask(), 0, "no neighbour on that side");
        assert_eq!(d.tiles[20].river_mask(), 0, "no river along water");
    }

    #[test]
    fn starts_on_water_twice_or_off_the_map_are_dropped() {
        let r = Ruleset::shared();
        let mut doc = blank(8, 8);
        doc["tiles"][5] = json!(["Ocean", [], null, 0, null, 0, null, null]);
        doc["starts"] = json!([3, 5, 3, 99, "x", 12]);
        doc["cs_starts"] = json!([12, 20]);
        let d = read(r, &doc).expect("a map");
        assert_eq!(d.starts, [TileIdx(3), TileIdx(12)]);
        assert_eq!(d.cs_starts, [TileIdx(20)], "a major's start is no city-state's");
        assert_eq!(d.warnings, ["(5,0) start position on Ocean removed"]);
    }

    #[test]
    fn north_south_wrapping_needs_an_even_height() {
        let r = Ruleset::shared();
        let mut doc = blank(8, 9);
        doc["wrap_y"] = json!(true);
        let d = read(r, &doc).expect("a map");
        assert!(!d.wrap_y);
        assert_eq!(d.warnings.len(), 1);
    }
}

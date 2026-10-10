//! A whole map from a seed (`generate_map`, `mapgen.py:1662-1721`).
//!
//! The steps run in Python's order: the ice, the land's shape, climate, mountains and hills,
//! lakes and coasts, vegetation and rare features, the polar ice, landmasses, rivers and the
//! conversions near them, then the starts. A map whose starts do not fit is made again, up to
//! twelve times. Then come the city-states' sites, the natural wonders, the resources (with the
//! starts made playable and the lobby's shares settled) and the ruins.
//!
//! Each phase draws from its own stream, `Rng::keyed(seed, Purpose::Map*, [attempt, step])`,
//! where Python drew everything from one `random.Random`: tuning one phase never shifts another
//! phase's draws (DESIGN.md 7.2). A resource step that only some maps need, such as the variety
//! top-ups, has a step of its own, so a map that needs none comes out as before.

use super::landmass::{ice_band, land_mask, land_scores};
use super::map::{GenMap, Kit};
use super::options::{MapOptions, MapType};
use super::rivers::rivers;
use super::starts::{choose_cs_starts, choose_starts, start_scores};
use super::wonders::natural_wonders;
use super::{MapError, resources, ruins, terrain};
use crate::base::hex::HexGrid;
use crate::base::ids::{NationId, TileIdx};
use crate::base::rng::{Purpose, Rng};
use crate::rules::Ruleset;
use crate::rules::defs::ResourceType;
use crate::state::map::Tile;

/// How many times a map is made again when its starts do not fit (`mapgen.py:1680`).
pub const ATTEMPTS: u8 = 12;

/// What to generate.
#[derive(Clone, Debug)]
pub struct GenSpec<'a> {
    pub width: u16,
    /// North-south wrapping needs an even height; an odd one does not wrap.
    pub height: u16,
    pub map_type: MapType,
    pub options: MapOptions,
    /// The major civilizations, each with a start.
    pub players: usize,
    /// The city-states the map should have sites for; a crowded map may have fewer.
    pub city_states: usize,
    /// Each major's nation, for its start bias; empty or `None` for no bias.
    pub nations: &'a [Option<NationId>],
    /// Whether to spread ancient ruins.
    pub ruins: bool,
}

/// A generated map.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedMap {
    pub width: u16,
    pub height: u16,
    pub wrap_x: bool,
    pub wrap_y: bool,
    /// Every tile, row by row.
    pub tiles: Vec<Tile>,
    /// The majors' starts, in seat order.
    pub starts: Vec<TileIdx>,
    /// The city-states' sites.
    pub cs_starts: Vec<TileIdx>,
    /// Each tile's landmass, numbered from the largest; `state::map::WATER` for water.
    pub continents: Vec<u16>,
    /// The attempt whose starts fitted, from 0.
    pub attempt: u8,
}

impl GeneratedMap {
    /// The map's grid.
    pub fn grid(&self) -> Result<HexGrid, MapError> {
        HexGrid::new(self.width, self.height, self.wrap_x, self.wrap_y).map_err(|_| size_error())
    }
}

fn size_error() -> MapError {
    MapError(format!(
        "Maps must be between {} and {} tiles on each side.",
        crate::base::hex::MIN_SIDE,
        crate::base::hex::MAX_SIDE
    ))
}

/// The streams of one attempt.
#[derive(Clone, Copy)]
pub(crate) struct Streams {
    seed: u64,
    attempt: u8,
    /// A phase whose stream is keyed apart, for the test that tuning one phase leaves the
    /// others alone.
    swapped: Option<Purpose>,
}

impl Streams {
    pub(crate) fn rng(&self, p: Purpose, step: u64) -> Rng {
        let attempt = u64::from(self.attempt);
        if self.swapped == Some(p) {
            Rng::keyed(self.seed, p, &[attempt, step, u64::MAX])
        } else {
            Rng::keyed(self.seed, p, &[attempt, step])
        }
    }
}

/// Generates a map from `seed` (`generate_map`).
///
/// # Errors
/// A size the grid refuses, a ruleset with no base terrain of land or of water, or starts that
/// did not fit in twelve attempts: too many civilizations for the land.
pub fn generate(rules: &Ruleset, seed: u64, spec: &GenSpec<'_>) -> Result<GeneratedMap, MapError> {
    generate_with(rules, seed, spec, None, &mut |_, _| {})
}

/// [`generate`], with one phase's stream swapped for another, and `observe` shown the tiles after
/// each phase.
pub(crate) fn generate_with(
    rules: &Ruleset,
    seed: u64,
    spec: &GenSpec<'_>,
    swapped: Option<Purpose>,
    observe: &mut dyn FnMut(Purpose, &GenMap<'_>),
) -> Result<GeneratedMap, MapError> {
    let opts = &spec.options;
    let (wrap_x, wrap_y) = opts.wraps();
    let grid = HexGrid::new(spec.width, spec.height, wrap_x, wrap_y).map_err(|_| size_error())?;
    let kit = Kit::new(rules).ok_or_else(|| {
        MapError("Map generation needs a base terrain of land and one of water.".to_owned())
    })?;
    let radius = rules.constants().map_size_predefined(spec.width, spec.height).radius;
    let mut nations: Vec<Option<NationId>> = spec.nations.to_vec();
    nations.resize(spec.players, None);

    let mut found = None;
    for attempt in 0..ATTEMPTS {
        let s = Streams { seed, attempt, swapped };
        let ice = ice_band(&mut s.rng(Purpose::MapIce, 0), &grid, opts.ice_sides());
        let (scores, fraction) =
            land_scores(&mut s.rng(Purpose::MapLand, 0), &grid, spec.map_type, spec.players, &ice);
        let land = land_mask(&grid, &scores, fraction);
        let tiles = land.iter().map(|&l| Tile::new(if l { kit.land } else { kit.ocean })).collect();
        let mut m = GenMap::new(&kit, opts, grid.clone(), tiles, ice);
        observe(Purpose::MapIce, &m);
        terrain::humidity_and_temperature(&mut m, &mut s.rng(Purpose::MapClimate, 0));
        observe(Purpose::MapClimate, &m);
        terrain::mountains_and_hills(&mut m, &mut s.rng(Purpose::MapRelief, 0));
        observe(Purpose::MapRelief, &m);
        terrain::lakes_and_coasts(&mut m, &mut s.rng(Purpose::MapLakes, 0));
        observe(Purpose::MapLakes, &m);
        terrain::vegetation(&mut m, &mut s.rng(Purpose::MapVegetation, 0));
        terrain::rare_features(&mut m, &mut s.rng(Purpose::MapVegetation, 1));
        observe(Purpose::MapVegetation, &m);
        terrain::ice(&mut m);
        m.assign_continents();
        rivers(&mut m, &mut s.rng(Purpose::MapRivers, 0));
        observe(Purpose::MapRivers, &m);
        terrain::convert_terrains(&mut m);
        let scores = start_scores(&m);
        let starts =
            choose_starts(&m, &mut s.rng(Purpose::MapStarts, 0), &scores, spec.players, &nations);
        if let Some(starts) = starts {
            found = Some((m, s, scores, starts));
            break;
        }
    }
    let Some((mut m, s, scores, starts)) = found else {
        return Err(MapError(format!(
            "Could not generate a map with valid start positions: {} civilizations do not fit \
             on this {}x{} map.",
            spec.players, spec.width, spec.height
        )));
    };
    observe(Purpose::MapStarts, &m);
    let cs = choose_cs_starts(&m, &scores, spec.city_states, &starts);
    let avoid: Vec<TileIdx> = starts.iter().chain(&cs).copied().collect();
    natural_wonders(&mut m, &mut s.rng(Purpose::MapWonders, 0), radius, &avoid);
    observe(Purpose::MapWonders, &m);

    let step = |k: u64| s.rng(Purpose::MapResources, k);
    resources::strategic(&mut m, &mut step(0));
    resources::strategic_variety(&mut m, &mut step(1), &starts, &cs);
    resources::luxuries(&mut m, &mut step(2), &starts, &cs);
    resources::luxury_variety_top_up(&mut m, &mut step(3), &starts, &cs);
    resources::bonus(&mut m, &mut step(4));
    let mut rng = step(5);
    for &t in &avoid {
        resources::normalize_start(&mut m, &mut rng, t);
    }
    let mut rng = step(6);
    for kind in [ResourceType::Strategic, ResourceType::Luxury] {
        resources::rebalance(&mut m, &mut rng, kind, &starts);
    }
    // A start's tile is cleared and shares move tiles about: put back any type that lost its
    // last deposit.
    resources::strategic_variety(&mut m, &mut step(7), &starts, &cs);
    resources::luxury_variety_top_up(&mut m, &mut step(8), &starts, &cs);
    observe(Purpose::MapResources, &m);
    if spec.ruins {
        ruins::ruins(&mut m, &mut s.rng(Purpose::MapRuins, 0), &starts, &cs);
    }
    m.assign_continents();
    observe(Purpose::MapRuins, &m);
    Ok(GeneratedMap {
        width: spec.width,
        height: spec.height,
        wrap_x: m.grid.wrap_x(),
        wrap_y: m.grid.wrap_y(),
        tiles: m.tiles,
        starts,
        cs_starts: cs,
        continents: m.continent,
        attempt: s.attempt,
    })
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::base::ids::TerrainId;
    use crate::mapgen::document::MapDocument;

    fn spec(players: usize) -> GenSpec<'static> {
        GenSpec {
            width: 44,
            height: 28,
            map_type: MapType::Continents,
            options: MapOptions::default(),
            players,
            city_states: 4,
            nations: &[],
            ruins: true,
        }
    }

    #[test]
    fn a_seed_makes_the_same_map_every_time() {
        let r = Ruleset::shared();
        let a = generate(r, 9, &spec(2)).expect("a map");
        let b = generate(r, 9, &spec(2)).expect("a map");
        assert_eq!(a, b);
        assert_eq!(a.starts.len(), 2);
        assert!(a.cs_starts.len() <= 4);
        let c = generate(r, 10, &spec(2)).expect("a map");
        assert_ne!(a.tiles, c.tiles);
    }

    #[test]
    fn too_many_civilizations_for_the_land_are_refused() {
        let r = Ruleset::shared();
        let mut s = spec(60);
        s.width = 16;
        s.height = 16;
        let e = generate(r, 1, &s).expect_err("no room");
        assert!(e.0.contains("Could not generate a map"), "{}", e.0);
    }

    #[test]
    fn starts_and_ruins_fill_a_map_that_lacks_them() {
        let r = Ruleset::shared();
        let map = generate(r, 5, &GenSpec { ruins: false, ..spec(2) }).expect("a map");
        let grid = map.grid().expect("grid");
        let ruins = r.derived().known.ancient_ruins;
        assert!(map.tiles.iter().all(|t| t.improvement().is_none()), "no ruins asked for");
        let mut doc = MapDocument {
            width: map.width,
            height: map.height,
            wrap_x: map.wrap_x,
            wrap_y: map.wrap_y,
            tiles: map.tiles.clone(),
            starts: Vec::new(),
            cs_starts: Vec::new(),
            warnings: Vec::new(),
        };
        // Keep one start, and ask for three.
        let filled =
            super::super::fill_starts_on(r, &doc, &map.starts[..1], 3, 7, &[], 0).expect("starts");
        assert_eq!(filled.len(), 3);
        assert_eq!(filled[0], map.starts[0]);
        for (i, &a) in filled.iter().enumerate() {
            for &b in &filled[i + 1..] {
                assert!(grid.distance(a, b) >= 2, "{a:?} and {b:?}");
            }
        }
        let mut rng = Rng::keyed(5, Purpose::MapPrepare, &[]);
        super::super::ruins_on(r, &mut doc, &mut rng, &filled, &[]).expect("ruins");
        let n = doc.tiles.iter().filter(|t| t.improvement().is_some()).count();
        assert!(n > 0);
        assert!(doc.tiles.iter().all(|t| t.improvement().is_none_or(|i| Some(i) == ruins)));
    }

    /// The helpers `maps.prepare` reuses refuse a document whose tiles do not fill its size, or
    /// a start off the map, instead of reading past the tiles, and leave the document alone.
    #[test]
    fn starts_and_ruins_refuse_a_document_that_does_not_add_up() {
        let r = Ruleset::shared();
        let map = generate(r, 5, &GenSpec { ruins: false, ..spec(2) }).expect("a map");
        let mut doc = MapDocument {
            width: map.width,
            height: map.height,
            wrap_x: false,
            wrap_y: false,
            tiles: map.tiles[..map.tiles.len() - 1].to_vec(),
            starts: Vec::new(),
            cs_starts: Vec::new(),
            warnings: Vec::new(),
        };
        let mut rng = Rng::keyed(5, Purpose::MapPrepare, &[]);
        let e = super::super::fill_starts_on(r, &doc, &[], 2, 7, &[], 0).expect_err("short");
        assert_eq!(e.0, "The map has 1231 tiles; a 44x28 map has 1232.");
        let before = doc.clone();
        let e = super::super::ruins_on(r, &mut doc, &mut rng, &[], &[]).expect_err("short");
        assert_eq!(e.0, "The map has 1231 tiles; a 44x28 map has 1232.");
        assert_eq!(doc, before);
        doc.tiles.clone_from(&map.tiles);
        doc.tiles.push(map.tiles[0]);
        assert!(super::super::fill_starts_on(r, &doc, &[], 2, 7, &[], 0).is_err(), "too long");
        doc.tiles.clone_from(&map.tiles);
        let off = [TileIdx(44 * 28)];
        let e = super::super::fill_starts_on(r, &doc, &off, 2, 7, &[], 0).expect_err("off");
        assert_eq!(e.0, "Tile 1232 is not on the 44x28 map.");
        let e = super::super::fill_starts_on(r, &doc, &[], 2, 7, &off, 3).expect_err("off");
        assert_eq!(e.0, "Tile 1232 is not on the 44x28 map.");
        let e = super::super::ruins_on(r, &mut doc, &mut rng, &[], &off).expect_err("off");
        assert_eq!(e.0, "Tile 1232 is not on the 44x28 map.");
        assert_eq!(doc.tiles, map.tiles, "a refused document keeps its tiles");
    }

    /// Gate 4 of package 1b-04: swapping the vegetation's stream changes the vegetation and
    /// nothing drawn before it, and the rivers, which read nothing vegetation writes, come out
    /// the same; the other phases' streams draw exactly as before.
    #[test]
    fn tuning_one_phase_leaves_the_other_phases_draws_alone() {
        let r = Ruleset::shared();
        let s = spec(2);
        type Snapshots = Vec<(Purpose, Vec<(TerrainId, u16, u8)>)>;
        let run = |swapped: Option<Purpose>| -> (Snapshots, GeneratedMap) {
            let mut snaps: Snapshots = Vec::new();
            let map = generate_with(r, 77, &s, swapped, &mut |p, m| {
                let tiles = m
                    .tiles
                    .iter()
                    .map(|t| (t.terrain(), t.features().bits(), t.river_mask()))
                    .collect();
                snaps.push((p, tiles));
            })
            .expect("a map");
            (snaps, map)
        };
        let (plain, a) = run(None);
        let (swapped, b) = run(Some(Purpose::MapVegetation));
        assert_eq!((a.attempt, b.attempt), (0, 0), "both fit at the first attempt");
        let at = |snaps: &Snapshots, p: Purpose| {
            snaps.iter().find(|(q, _)| *q == p).map(|(_, t)| t.clone()).expect("observed")
        };
        for p in [Purpose::MapIce, Purpose::MapClimate, Purpose::MapRelief, Purpose::MapLakes] {
            assert_eq!(at(&plain, p), at(&swapped, p), "{p:?} comes before the vegetation");
        }
        assert_ne!(at(&plain, Purpose::MapVegetation), at(&swapped, Purpose::MapVegetation));
        let rivers = |snaps: &Snapshots| -> Vec<u8> {
            at(snaps, Purpose::MapRivers).into_iter().map(|(_, _, r)| r).collect()
        };
        assert_eq!(rivers(&plain), rivers(&swapped), "rivers read nothing vegetation writes");
        assert!(rivers(&plain).iter().any(|&m| m != 0), "the map has rivers");
        // Every other phase's stream draws the same numbers either way.
        let plain = Streams { seed: 77, attempt: 0, swapped: None };
        let tuned = Streams { swapped: Some(Purpose::MapVegetation), ..plain };
        for p in Purpose::ALL.iter().copied().filter(|p| format!("{p:?}").starts_with("Map")) {
            for step in 0..9 {
                let (mut x, mut y) = (plain.rng(p, step), tuned.rng(p, step));
                let same = (0..16).all(|_| x.next_u64() == y.next_u64());
                assert_eq!(same, p != Purpose::MapVegetation, "{p:?} step {step}");
            }
        }
    }
}

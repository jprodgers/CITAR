//! Map generation (package 1b-04):
//! - the properties every generated map keeps, over 200 seeds of each of the five map types on
//!   a duel and a small map, every edge mode among them (gate 1): the rivers reach water and
//!   never cross, the ice stays in its polar band, every strategic resource is on the map, the
//!   luxuries are as varied as the map's size asks, and the starts are about as good as one
//!   another; with the starts, sites and tiles well formed;
//! - `Game::new` on a generated map, and `api::maps::generate_map`;
//! - the kitchen sink's `Must be on [n] largest landmasses`;
//! - the resources, as Python's `tests/test_mapgen.py` checked them: the luxury variety by
//!   map size, every type on the big maps, sparse and switched-off kinds; and the lobby's caps
//!   and shares;
//! - a feature a ruleset keeps from generating, everywhere or on some tiles.
//!
//! That tuning one phase leaves the other phases' draws alone (gate 4) is a unit test beside the
//! generator (`mapgen::generate`), which can swap one phase's stream.

use std::sync::OnceLock;

use citar_engine::api::maps;
use citar_engine::base::ids::{ResourceId, TerrainId, TileIdx};
use citar_engine::mapgen::options::resource_options;
use citar_engine::mapgen::{self, GenSpec, GeneratedMap, MAP_TYPES, MapOptions, MapType, metrics};
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::{ResourceType, TerrainType};
use citar_engine::state::Phase;
use citar_engine::state::config::MapEdges;
use citar_engine::state::map::WATER;
use citar_engine::unique::UniqueType;
use citar_testkit::rulesets;
use citar_testkit::script::new_game;
use serde_json::json;

/// The lowest a start's score may be, as a share of the best start's on the same map. The
/// generator places the starts as Python's did, which does not balance them further: over the
/// gate's 2,000 maps the worst share is 0.455 (small fractal), the lowest percentile 0.52 to
/// 0.72 by type and size, and the median 0.76 to 0.9 (`report_start_quality`).
const START_QUALITY: f64 = 0.4;

fn spec(r: &Ruleset, size: &str, map_type: MapType, edges: MapEdges) -> GenSpec<'static> {
    let c = r.constants();
    let lobby = &c.map_sizes[c.map_size_id(size).expect("a lobby size")];
    GenSpec {
        width: lobby.width,
        height: lobby.height,
        map_type,
        options: MapOptions { edges, ..MapOptions::default() },
        players: usize::from(lobby.players),
        city_states: usize::from(lobby.city_states),
        nations: &[],
        ruins: true,
    }
}

/// Everything wrong with one generated map.
fn problems(r: &Ruleset, s: &GenSpec<'_>, map: &GeneratedMap) -> Vec<String> {
    let mut out = Vec::new();
    let grid = map.grid().expect("a grid");
    let terrains = r.terrains();
    let features = &r.derived().features;

    // Starts and sites: as many as asked (sites at most), on passable land, each once, apart.
    if map.starts.len() != s.players {
        out.push(format!("{} starts for {} players", map.starts.len(), s.players));
    }
    if map.cs_starts.len() > s.city_states {
        out.push(format!("{} sites for {} city-states", map.cs_starts.len(), s.city_states));
    }
    let all: Vec<TileIdx> = map.starts.iter().chain(&map.cs_starts).copied().collect();
    for (i, &a) in all.iter().enumerate() {
        let t = &map.tiles[a.0 as usize];
        let d = &terrains[t.terrain()];
        if d.kind != TerrainType::Land || d.impassable || t.wonder().is_some() {
            out.push(format!("a start on {} at {a:?}", d.name));
        }
        if t.resource().is_some() || t.improvement().is_some() {
            out.push(format!("a start with a resource or an improvement at {a:?}"));
        }
        for &b in &all[i + 1..] {
            if grid.distance(a, b) < 4 {
                out.push(format!("starts {a:?} and {b:?} are {} apart", grid.distance(a, b)));
            }
        }
    }
    for (i, &a) in map.starts.iter().enumerate() {
        for &b in &map.starts[i + 1..] {
            if grid.distance(a, b) < 5 {
                out.push(format!("majors {a:?} and {b:?} are {} apart", grid.distance(a, b)));
            }
        }
    }

    // Tiles: each feature lies on its base or on a feature below it, wonders stand alone, a
    // strategic deposit has a size and nothing else does.
    for (i, t) in map.tiles.iter().enumerate() {
        let base = t.terrain();
        let mut below = vec![base];
        for f in t.features().iter() {
            let ft: TerrainId = features[f];
            let on = &terrains[ft].occurs_on;
            if !on.is_empty() && !below.iter().any(|b| on.contains(b)) {
                out.push(format!("tile {i}: {} on {}", terrains[ft].name, terrains[base].name));
            }
            below.push(ft);
        }
        if t.wonder().is_some() && (!t.features().is_empty() || t.resource().is_some()) {
            out.push(format!("tile {i}: a wonder with features or a resource"));
        }
        if let Some(res) = t.resource() {
            let strategic = r.resources()[res].kind == ResourceType::Strategic;
            if strategic != (t.resource_amount() > 0) {
                out.push(format!(
                    "tile {i}: {} with {}",
                    r.resources()[res].name,
                    t.resource_amount()
                ));
            }
        }
    }
    let continents = mapgen::continents::assign(r, &grid, &map.tiles);
    if continents != map.continents {
        out.push("the continents are not the tiles' landmasses".to_owned());
    }
    if !map.continents.contains(&0) && map.continents.iter().any(|&c| c != WATER) {
        out.push("no landmass 0".to_owned());
    }

    // The properties of gate 1.
    let rivers = metrics::rivers(r, map).expect("a grid");
    if !rivers.sound() {
        out.push(format!("rivers: {rivers:?}"));
    }
    let ice = metrics::ice_outside_band(r, map, s.options.edges);
    if ice > 0 {
        out.push(format!("{ice} ice tiles outside the polar band"));
    }
    let missing = metrics::missing_strategic(r, map);
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|&x| &*r.resources()[x].name).collect();
        out.push(format!("no {}", names.join(", ")));
    }
    let (present, wanted) = metrics::luxury_variety(r, map, &s.options);
    if present < wanted {
        out.push(format!("{present} luxury types, {wanted} wanted"));
    }
    let quality = metrics::start_quality(r, map);
    let best = quality.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let worst = quality.iter().copied().fold(f64::INFINITY, f64::min);
    if !quality.is_empty() && worst < best * START_QUALITY {
        out.push(format!("start quality from {worst:.1} to {best:.1}"));
    }
    out
}

/// Gate 1 for one map type and size: 200 seeds, the edge modes in turn.
fn properties(size: &str, map_type: MapType) {
    let r = Ruleset::shared();
    let mut failures = Vec::new();
    for seed in 0..200u64 {
        let edges = MapEdges::ALL[usize::try_from(seed % 5).unwrap_or(0)];
        let s = spec(r, size, map_type, edges);
        match mapgen::generate(r, seed, &s) {
            Ok(map) => {
                for p in problems(r, &s, &map) {
                    failures.push(format!("seed {seed} ({}): {p}", edges.name()));
                }
            }
            Err(e) => failures.push(format!("seed {seed} ({}): {}", edges.name(), e.0)),
        }
    }
    assert!(failures.is_empty(), "{size} {}:\n{}", map_type.key(), failures.join("\n"));
}

#[test]
fn duel_continents_keep_their_properties() {
    properties("duel", MapType::Continents);
}

#[test]
fn duel_pangaea_keeps_its_properties() {
    properties("duel", MapType::Pangaea);
}

#[test]
fn duel_archipelago_keeps_its_properties() {
    properties("duel", MapType::Archipelago);
}

#[test]
fn duel_inland_sea_keeps_its_properties() {
    properties("duel", MapType::InlandSea);
}

#[test]
fn duel_fractal_keeps_its_properties() {
    properties("duel", MapType::Fractal);
}

#[test]
fn small_continents_keep_their_properties() {
    properties("small", MapType::Continents);
}

#[test]
fn small_pangaea_keeps_its_properties() {
    properties("small", MapType::Pangaea);
}

#[test]
fn small_archipelago_keeps_its_properties() {
    properties("small", MapType::Archipelago);
}

#[test]
fn small_inland_sea_keeps_its_properties() {
    properties("small", MapType::InlandSea);
}

#[test]
fn small_fractal_keeps_its_properties() {
    properties("small", MapType::Fractal);
}

/// A game on a generated map: every seat on its own start, a site for each city-state, the map
/// the settings asked for, and turns that pass with every check clean.
#[test]
fn a_game_sets_up_on_a_generated_map() {
    let r = Ruleset::shared();
    let cfg = json!({
        "seed": 5,
        "map_size": "duel",
        "map_type": "pangaea",
        "map_edges": "wrap_x",
        "players": [{"nation": "Egypt"}, {"nation": "Rome"}],
        "city_states": 2,
        "turn_limit": 6,
    });
    let mut g = new_game(r, cfg.as_object().expect("settings")).expect("a game");
    let info = g.state().map();
    assert_eq!((info.width, info.height, info.wrap_x, info.wrap_y), (44, 28, true, false));
    let starts: Vec<TileIdx> =
        g.state().players().iter().filter_map(|(_, p)| p.start_tile).collect();
    assert_eq!(starts.len(), 4, "two majors and two city-states, each with a start");
    for &s in &starts {
        let t = g.state().tiles().get(s).expect("a tile");
        assert_eq!(r.terrains()[t.terrain()].kind, TerrainType::Land);
        assert!(info.continent(s).is_some());
    }
    // The same settings make the same map: it comes from the game's seed.
    let again = new_game(r, cfg.as_object().expect("settings")).expect("a game");
    assert_eq!(g.state().tiles().as_slice(), again.state().tiles().as_slice());
    while g.phase() == Phase::Playing {
        g.end_turn(g.current()).expect("a turn ends");
    }
    assert!(g.turn() >= 6);
    // More civilizations than the land takes is refused with a sentence.
    let crowded = json!({"seed": 1, "width": 16, "height": 16, "players": vec![json!({}); 20]});
    let e = new_game(r, crowded.as_object().expect("settings")).expect_err("no room");
    assert!(e.contains("Could not generate a map"), "{e}");
}

/// `api::maps::generate_map`: the editor's document of a generated map, which a game can then
/// be set up on.
#[test]
fn a_generated_document_makes_a_game() {
    let r = Ruleset::shared();
    let doc =
        maps::generate_map(r, 3, &json!({"map_size": "duel", "players": 2})).expect("a document");
    assert_eq!(doc["starts"].as_array().map(Vec::len), Some(2));
    let cfg = json!({"seed": 3, "map": doc, "players": [{}, {}]});
    let g = new_game(r, cfg.as_object().expect("settings")).expect("a game on the document");
    assert_eq!(g.state().map().width, 44);
}

/// The kitchen sink's natural wonder must be on the largest landmass (`Must be on [1] largest
/// landmasses`): wherever the generator puts it, it is on landmass 0.
#[test]
fn a_wonder_that_must_be_on_the_largest_landmass_is() {
    let r = rulesets::kitchen_sink();
    let spire = r.lookup::<TerrainId>("Kitchen Sink Spire").expect("the spire");
    let mut placed = 0;
    for seed in 0..40 {
        let s =
            GenSpec { city_states: 0, ..spec(r, "small", MapType::Continents, MapEdges::IceCaps) };
        let map = mapgen::generate(r, seed, &s).expect("a map");
        for (i, t) in map.tiles.iter().enumerate() {
            if t.wonder() == Some(spire) {
                placed += 1;
                assert_eq!(
                    map.continents[i], 0,
                    "seed {seed}: the spire on landmass {}",
                    map.continents[i]
                );
            }
        }
    }
    assert!(placed > 0, "the spire was never placed");
}

/// Prints the start-quality ratios (worst over best start) of the gate's maps: the lowest and
/// the percentiles, per size and type. Run with `--ignored --nocapture`.
#[test]
#[ignore = "a report, not a check"]
#[allow(clippy::disallowed_macros, reason = "a report prints what it measured")]
fn report_start_quality() {
    let r = Ruleset::shared();
    for size in ["duel", "small"] {
        for t in mapgen::MAP_TYPES {
            let mut ratios = Vec::new();
            for seed in 0..200u64 {
                let edges = MapEdges::ALL[usize::try_from(seed % 5).unwrap_or(0)];
                let s = spec(r, size, t, edges);
                let map = mapgen::generate(r, seed, &s).expect("a map");
                let q = metrics::start_quality(r, &map);
                let best = q.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let worst = q.iter().copied().fold(f64::INFINITY, f64::min);
                ratios.push(worst / best);
            }
            ratios.sort_by(f64::total_cmp);
            println!(
                "{size:6} {:12} min {:.3} p1 {:.3} p5 {:.3} median {:.3}",
                t.key(),
                ratios[0],
                ratios[2],
                ratios[10],
                ratios[100]
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Resources: Python's `tests/test_mapgen.py` (`ResourceVarietyTest`), and the lobby's rules.
// ---------------------------------------------------------------------------------------------

/// A map of a lobby size with continents and the default edges, for `players` and
/// `city_states`, and the lobby's resource settings `resources` (Python's `gen`).
fn resource_map(
    r: &Ruleset,
    size: &str,
    seed: u64,
    players: usize,
    city_states: usize,
    resources: &serde_json::Value,
) -> (GenSpec<'static>, GeneratedMap) {
    let mut s = spec(r, size, MapType::Continents, MapEdges::default());
    s.players = players;
    s.city_states = city_states;
    s.options.resources = resource_options(r, Some(resources));
    let map = mapgen::generate(r, seed, &s)
        .unwrap_or_else(|e| panic!("{size} seed {seed}: no map ({})", e.0));
    (s, map)
}

/// The luxury types a map may hold: those that generate naturally, less those only a
/// city-state makes (`ResourceVarietyTest.lux`).
fn luxury_types(r: &Ruleset) -> Vec<ResourceId> {
    let t = r.uniques();
    r.resources()
        .iter()
        .filter(|(id, x)| {
            x.kind == ResourceType::Luxury
                && !r.gen_tables().resources[*id].never
                && !x.uniques.ids().any(|u| t.meta(u).ty == Some(UniqueType::CityStateOnlyResource))
        })
        .map(|(id, _)| id)
        .collect()
}

/// The names of `types` the map lacks.
fn lacking(r: &Ruleset, map: &GeneratedMap, types: &[ResourceId]) -> Vec<String> {
    let counts = metrics::resource_counts(r, map);
    types.iter().filter(|&&x| counts[x] == 0).map(|&x| r.resources()[x].name.to_string()).collect()
}

/// How many tiles hold a resource of `kind`.
fn tiles_of(r: &Ruleset, map: &GeneratedMap, kind: ResourceType) -> u32 {
    let counts = metrics::resource_counts(r, map);
    r.resources().iter().filter(|(_, x)| x.kind == kind).map(|(id, _)| counts[id]).sum()
}

fn resource(r: &Ruleset, name: &str) -> ResourceId {
    r.lookup::<ResourceId>(name).unwrap_or_else(|| panic!("{name} is in the ruleset"))
}

/// The share of luxury types a map keeps (`test_variety_curve`): half on a duel or a small
/// map, three quarters on a standard one, 90% on a large one and all from huge up, rising
/// between.
#[test]
fn the_luxury_variety_rises_with_the_lobby_sizes() {
    let r = Ruleset::shared();
    let c = r.constants();
    let v = |k: &str| {
        let s = &c.map_sizes[c.map_size_id(k).expect("a lobby size")];
        mapgen::luxury_variety(u32::from(s.width) * u32::from(s.height))
    };
    for (size, want) in [
        ("duel", 0.5),
        ("small", 0.5),
        ("standard", 0.75),
        ("large", 0.9),
        ("huge", 1.0),
        ("gargantuan", 1.0),
    ] {
        assert!((v(size) - want).abs() < 1e-9, "{size}: {} for {want}", v(size));
    }
    assert!((mapgen::luxury_variety(100) - 0.5).abs() < 1e-9);
    let between = mapgen::luxury_variety(4500);
    assert!(v("standard") < between && between < v("large"), "{between}");
    // The count is rounded up, and never more than there are.
    assert_eq!(mapgen::luxury_types_wanted(100, 5), 3);
    assert_eq!(mapgen::luxury_types_wanted(76 * 48, 20), 15);
    assert_eq!(mapgen::luxury_types_wanted(160 * 100, 20), 20);
    assert_eq!(mapgen::luxury_types_wanted(160 * 100, 0), 0);
}

/// Huge and gargantuan maps hold every luxury and strategic type, even with the fewest
/// players, who leave the most luxury types to chance (`test_big_maps_have_every_type`).
#[test]
fn big_maps_have_every_luxury_and_strategic_type() {
    let r = Ruleset::shared();
    let lux = luxury_types(r);
    for (size, seed) in [("huge", 1), ("huge", 7), ("gargantuan", 3)] {
        let (s, map) = resource_map(r, size, seed, 2, 2, &json!({}));
        assert_eq!(lacking(r, &map, &lux), Vec::<String>::new(), "{size} seed {seed}");
        assert_eq!(metrics::missing_strategic(r, &map), vec![], "{size} seed {seed}");
        let variety = metrics::luxury_variety(r, &map, &s.options);
        assert_eq!(variety, (lux.len(), lux.len()), "{size} seed {seed}");
    }
}

/// A standard map holds at least 70% of the luxury types, a duel or a small one half, and
/// each of them every strategic type (`test_standard_and_small_maps`).
#[test]
fn standard_and_small_maps_keep_their_share_of_the_types() {
    let r = Ruleset::shared();
    let lux = luxury_types(r);
    let n = lux.len();
    for seed in [1, 2, 5] {
        for (size, least) in [("standard", (7 * n).div_ceil(10)), ("duel", n / 2), ("small", n / 2)]
        {
            let (_, map) = resource_map(r, size, seed, 2, 0, &json!({}));
            let present = n - lacking(r, &map, &lux).len();
            assert!(present >= least, "{size} seed {seed}: {present} of {n} luxury types");
            assert_eq!(metrics::missing_strategic(r, &map), vec![], "{size} seed {seed}");
        }
    }
}

/// Sparse strategic resources still leave one of each type, a luxury turned off is never
/// placed, and a kind whose density is 0 is not placed at all
/// (`test_sparse_settings_still_have_every_strategic`).
#[test]
fn sparse_settings_still_have_every_strategic_type() {
    let r = Ruleset::shared();
    let silk = resource(r, "Silk");
    let sparse =
        json!({"strategic": {"density": 0.05}, "luxury": {"each": {"Silk": {"mode": "off"}}}});
    for seed in [4, 9, 13] {
        let (_, map) = resource_map(r, "duel", seed, 2, 4, &sparse);
        assert_eq!(metrics::missing_strategic(r, &map), vec![], "seed {seed}");
        assert_eq!(metrics::resource_counts(r, &map)[silk], 0, "seed {seed}: Silk is off");
        assert!(tiles_of(r, &map, ResourceType::Luxury) > 0);
    }
    let kinds = [
        ("strategic", ResourceType::Strategic),
        ("luxury", ResourceType::Luxury),
        ("bonus", ResourceType::Bonus),
    ];
    for (key, kind) in kinds {
        let (_, map) = resource_map(r, "duel", 4, 2, 4, &json!({ key: {"density": 0} }));
        assert_eq!(tiles_of(r, &map, kind), 0, "{key} turned off");
        for (_, other) in kinds.iter().filter(|(_, k)| *k != kind) {
            assert!(tiles_of(r, &map, *other) > 0, "{key} off leaves {other:?}");
        }
    }
    let (_, map) = resource_map(r, "duel", 4, 2, 4, &json!({"density": 0}));
    assert!(map.tiles.iter().all(|t| t.resource().is_none()), "every kind turned off");
}

/// A capped resource never has more tiles than its cap, whatever step would place it.
#[test]
fn a_capped_resource_stays_under_its_cap() {
    let r = Ruleset::shared();
    let (uranium, iron, wine) = (resource(r, "Uranium"), resource(r, "Iron"), resource(r, "Wine"));
    let caps = json!({
        "strategic": {"each": {"Uranium": {"mode": "cap", "value": 1},
                               "Iron": {"mode": "cap", "value": 2}}},
        "luxury": {"each": {"Wine": {"mode": "cap", "value": 1}}},
    });
    for (size, seed) in [("duel", 1), ("duel", 2), ("small", 3), ("standard", 4)] {
        let (_, map) = resource_map(r, size, seed, 2, 2, &caps);
        let counts = metrics::resource_counts(r, &map);
        assert!(counts[uranium] <= 1, "{size} seed {seed}: {} Uranium", counts[uranium]);
        assert!(counts[iron] <= 2, "{size} seed {seed}: {} Iron", counts[iron]);
        assert!(counts[wine] <= 1, "{size} seed {seed}: {} Wine", counts[wine]);
        assert!(counts[uranium] + counts[iron] > 0, "a capped type is still placed");
    }
}

/// A resource with a share is that share of every resource of its kind, to within a tile; the
/// kind's total is what the density made, the same as without the share, and every strategic
/// type and the luxury variety stay on the map (`mapgen-shares-keep-every-type`: Python's
/// shares took a type's last deposit, and the top-ups that put it back diluted the share).
#[test]
fn a_share_sets_a_resource_s_part_of_its_kind() {
    let r = Ruleset::shared();
    let (iron, wine) = (resource(r, "Iron"), resource(r, "Wine"));
    let shares = json!({
        "strategic": {"each": {"Iron": {"mode": "share", "value": 60}}},
        "luxury": {"each": {"Wine": {"mode": "share", "value": 25}}},
    });
    for (size, seed) in [("standard", 1), ("standard", 5), ("small", 3), ("duel", 1)] {
        let (_, plain) = resource_map(r, size, seed, 2, 2, &json!({}));
        let (s, map) = resource_map(r, size, seed, 2, 2, &shares);
        let counts = metrics::resource_counts(r, &map);
        for (res, kind, share) in
            [(iron, ResourceType::Strategic, 0.6), (wine, ResourceType::Luxury, 0.25)]
        {
            let total = tiles_of(r, &map, kind);
            assert_eq!(total, tiles_of(r, &plain, kind), "{size} seed {seed}: {kind:?} tiles");
            let have = f64::from(counts[res]);
            assert!(
                (have - share * f64::from(total)).abs() <= 1.0,
                "{size} seed {seed}: {have} {} of {total}",
                r.resources()[res].name
            );
        }
        assert_eq!(metrics::missing_strategic(r, &map), vec![], "{size} seed {seed}");
        let (present, wanted) = metrics::luxury_variety(r, &map, &s.options);
        assert!(present >= wanted, "{size} seed {seed}: {present} luxury types, {wanted} wanted");
    }
}

// ---------------------------------------------------------------------------------------------
// Features a ruleset keeps from generating.
// ---------------------------------------------------------------------------------------------

/// The shipped ruleset with Forest marked `Doesn't generate naturally <in [Hill] tiles>` and
/// Jungle `Doesn't generate naturally`.
fn features_held_back() -> &'static Ruleset {
    static RULES: OnceLock<&'static Ruleset> = OnceLock::new();
    RULES.get_or_init(|| {
        let files = rulesets::overlay(&[]).expect("the files");
        let (_, terrains) =
            files.iter().find(|(n, _)| n == "ruleset/terrains.json").expect("the terrains");
        let terrains: serde_json::Value = serde_json::from_slice(terrains).expect("JSON");
        let with = |name: &str, extra: &str| {
            let mut uniques = terrains[name]["uniques"].as_array().cloned().unwrap_or_default();
            uniques.push(json!(extra));
            uniques
        };
        let patch = json!({
            "Forest": {"uniques": with("Forest", "Doesn't generate naturally <in [Hill] tiles>")},
            "Jungle": {"uniques": with("Jungle", "Doesn't generate naturally")},
        })
        .to_string();
        let files = rulesets::overlay(&[("ruleset/terrains.json", &patch)]).expect("the patch");
        Ruleset::leak(&rulesets::files_of(&files)).unwrap_or_else(|e| panic!("no ruleset:\n{e}"))
    })
}

/// A feature marked `Doesn't generate naturally <in [Hill] tiles>` still grows where the
/// condition does not hold, and never where it does; one marked unconditionally never grows
/// (`mapgen-features-that-never-generate`).
#[test]
fn a_feature_held_back_on_some_tiles_grows_on_the_others() {
    let count = |r: &Ruleset| {
        let feature = |name: &str| {
            let t = r.lookup::<TerrainId>(name).expect("a terrain");
            r.terrains()[t].feature.expect("a feature")
        };
        let (forest, jungle, hill) = (feature("Forest"), feature("Jungle"), r.derived().known.hill);
        let (mut forests, mut on_hills, mut jungles) = (0, 0, 0);
        for seed in 0..12u64 {
            let t = MAP_TYPES[usize::try_from(seed % 5).unwrap_or(0)];
            let map = mapgen::generate(r, seed, &spec(r, "small", t, MapEdges::default()))
                .expect("a map");
            for tile in &map.tiles {
                let f = tile.features();
                forests += usize::from(f.contains(forest));
                on_hills += usize::from(f.contains(forest) && f.contains(hill));
                jungles += usize::from(f.contains(jungle));
            }
        }
        (forests, on_hills, jungles)
    };
    let (forests, on_hills, jungles) = count(Ruleset::shared());
    assert!(forests > 0 && on_hills > 0 && jungles > 0, "{forests} {on_hills} {jungles}");
    let (forests, on_hills, jungles) = count(features_held_back());
    assert!(forests > 0, "forests still grow off the hills");
    assert_eq!((on_hills, jungles), (0, 0));
}

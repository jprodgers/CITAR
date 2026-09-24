//! Map generation (package 1b-04):
//! - the properties every generated map keeps, over 200 seeds of each of the five map types on
//!   a duel and a small map, every edge mode among them (gate 1): the rivers reach water and
//!   never cross, the ice stays in its polar band, every strategic resource is on the map, the
//!   luxuries are as varied as the map's size asks, and the starts are about as good as one
//!   another; with the starts, sites and tiles well formed;
//! - `Game::new` on a generated map, and `api::maps::generate_map`;
//! - the kitchen sink's `Must be on [n] largest landmasses`.
//!
//! That tuning one phase leaves the other phases' draws alone (gate 4) is a unit test beside the
//! generator (`mapgen::generate`), which can swap one phase's stream.

use citar_engine::api::maps;
use citar_engine::base::ids::{TerrainId, TileIdx};
use citar_engine::mapgen::{self, GenSpec, GeneratedMap, MapOptions, MapType, metrics};
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::{ResourceType, TerrainType};
use citar_engine::state::Phase;
use citar_engine::state::config::MapEdges;
use citar_engine::state::map::WATER;
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

//! The golden set of package 1b-04 (DESIGN.md 9.6):
//! - **`maps.json`**: ten generated maps, duel to huge, every map type and edge mode among them,
//!   each a blake3 of its tiles (`Tile::canon_bytes`, row by row), its landmasses and its
//!   starts, with a few counts beside it so that a diff says what moved. A hash that moves is a
//!   change to map generation or to the embedded ruleset; one that differs between targets is a
//!   determinism bug in it (its maths, its sorts, its streams).
//!
//! Written by `golden bless` when map generation changes on purpose.

use citar_engine::base::ids::NationId;
use citar_engine::mapgen::{self, GenSpec, GeneratedMap, MapOptions, MapType, metrics};
use citar_engine::rules::Ruleset;
use citar_engine::state::config::MapEdges;
use citar_engine::state::map::WATER;
use serde_json::{Value, json};

use super::{SetReport, capped, diff_rows, digest_of, read_committed, render_rows};

/// The maps: (lobby size, type, edges, seed).
const MAPS: [(&str, MapType, MapEdges, u64); 10] = [
    ("duel", MapType::Continents, MapEdges::IceCaps, 1),
    ("duel", MapType::Archipelago, MapEdges::WrapX, 2),
    ("small", MapType::Pangaea, MapEdges::Boxed, 3),
    ("small", MapType::Fractal, MapEdges::WrapY, 4),
    ("standard", MapType::InlandSea, MapEdges::IceCaps, 5),
    ("standard", MapType::Continents, MapEdges::WrapBoth, 6),
    ("large", MapType::Archipelago, MapEdges::IceCaps, 7),
    ("large", MapType::Fractal, MapEdges::WrapX, 8),
    ("huge", MapType::Pangaea, MapEdges::IceCaps, 9),
    ("huge", MapType::InlandSea, MapEdges::WrapX, 10),
];

/// blake3 of a map: every tile's canonical bytes, then the landmasses and the starts, little
/// endian.
#[must_use]
pub fn map_hash(map: &GeneratedMap) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"CITAR-MAP");
    h.update(&map.width.to_le_bytes());
    h.update(&map.height.to_le_bytes());
    h.update(&[u8::from(map.wrap_x), u8::from(map.wrap_y)]);
    for t in &map.tiles {
        h.update(&t.canon_bytes());
    }
    for c in &map.continents {
        h.update(&c.to_le_bytes());
    }
    for list in [&map.starts, &map.cs_starts] {
        h.update(&u32::try_from(list.len()).unwrap_or(u32::MAX).to_le_bytes());
        for t in list {
            h.update(&t.0.to_le_bytes());
        }
    }
    h.finalize().to_hex().to_string()
}

/// One row per map, and what went wrong making one.
fn answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let c = r.constants();
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    for (size, map_type, edges, seed) in MAPS {
        let name = format!("map-{size}-{}", map_type.key());
        let Some(lobby) = c.map_size_id(size).map(|id| &c.map_sizes[id]) else {
            problems.push(format!("{name}: no lobby size {size}"));
            continue;
        };
        let players = usize::from(lobby.players);
        // The first majors, so that start biases count.
        let nations: Vec<Option<NationId>> =
            r.derived().major_nations.iter().take(players).copied().map(Some).collect();
        let spec = GenSpec {
            width: lobby.width,
            height: lobby.height,
            map_type,
            options: MapOptions { edges, ..MapOptions::default() },
            players,
            city_states: usize::from(lobby.city_states),
            nations: &nations,
            ruins: true,
        };
        match mapgen::generate(r, seed, &spec) {
            Ok(map) => {
                let land = map.continents.iter().filter(|&&x| x != WATER).count();
                let resources = map.tiles.iter().filter(|t| t.resource().is_some()).count();
                let rivers = metrics::rivers(r, &map).map_or(0, |x| x.edges);
                rows.push(json!([
                    name,
                    edges.name(),
                    seed,
                    [map.width, map.height],
                    map_hash(&map),
                    map.attempt,
                    [map.starts.len(), map.cs_starts.len()],
                    land,
                    rivers,
                    resources,
                ]));
            }
            Err(e) => problems.push(format!("{name}: {}", e.0)),
        }
    }
    let v = json!({
        "format": 1,
        "maps": "[name, edges, seed, [width, height], blake3(CITAR-MAP, size, wraps, canon tiles, landmasses, starts), attempt, [starts, sites], land tiles, river edges, resources]",
        "ruleset_id": r.id().to_hex(),
        "rows": rows,
    });
    (v, problems)
}

fn render(v: &Value) -> String {
    render_rows(v, &["rows"])
}

/// The file `golden bless` writes.
#[must_use]
pub fn blessed() -> Vec<(&'static str, String)> {
    vec![("maps.json", render(&answers().0))]
}

/// The `maps` set, checked against `maps.json`.
#[must_use]
pub fn check_maps() -> SetReport {
    let (got, mut problems) = answers();
    match read_committed("maps.json") {
        Err(e) => problems.push(e),
        Ok(want) => {
            if want.get("ruleset_id") != got.get("ruleset_id") {
                problems.push("maps.json: the ruleset id differs from this build's".to_owned());
            }
            problems.extend(diff_rows("maps.json", "rows", want.get("rows"), &got["rows"]));
        }
    }
    SetReport {
        name: "maps",
        computed: digest_of(&got),
        problems: capped(problems),
        waiting: Vec::new(),
    }
}

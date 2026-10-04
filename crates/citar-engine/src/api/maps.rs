//! Maps for the host and the map editor (DESIGN.md 8.1): the editor's document checked and
//! cleaned ([`validate_map`], `maps.validate`), made blank ([`blank_map`]) or from the generator
//! ([`generate_map`], `maps.generated_map`), taken from a game in progress
//! ([`Game::export_map`], `maps.map_from_game`), and summed up for lists ([`map_summary`])
//! (`maps.py:30-257`). The files stay in Python: the engine reads and writes no disk.
//!
//! The engine draws nothing: `generate_map` takes the seed, which the facade draws when the
//! lobby leaves it empty, as Python's `generated_map` did itself (`maps.py:92`). The settings
//! are the lobby's, read as `Game::config_from_json` reads them, so an unknown map size, type or
//! edge mode is refused the same way (`config-refuses-unknown-names`).
//!
//! A document is read as a new game reads it (`mapgen::document`), so what the editor is told
//! is wrong with a map is what a game on it would drop: which improvements a map may carry is a
//! rule rather than Python's list (`map-documents-read-by-rule`). What else differs, on purpose:
//! a map whose name makes no id is `map`, where Python named it after the clock; and a map's
//! land share counts the tiles whose terrain the ruleset calls water, where Python named Ocean,
//! Coast and Lakes (`map-summary-reads-water-by-rule`).

use serde_json::{Map, Value, json};

use crate::base::hex::{MAX_SIDE, MIN_SIDE};
use crate::base::ids::{TerrainId, TileIdx};
use crate::base::{num, py};
use crate::game::Game;
use crate::game::error::EngineError;
use crate::game::setup;
use crate::mapgen::document::{self, tile_row};
use crate::mapgen::options::{option_number, resource_options};
use crate::mapgen::{self, GenSpec, MapOptions, MapType};
use crate::rules::Ruleset;
use crate::state::config::MapSource;

/// A map from the generator, as the editor's document, for editing or for a game
/// (`maps.generated_map`).
///
/// `settings` are the lobby's: `map_size` (small by default) or `width` and `height`,
/// `map_type`, `map_edges`, `river_density`, `resources`, `players` and `city_states` (the
/// size's by default), `ruins` (on by default) and a `name`.
///
/// # Errors
/// [`EngineError::Config`] for settings that name nothing the ruleset or the lobby has, or a
/// count that is not one; [`EngineError::Map`] for a size out of bounds, or more civilizations
/// than the land takes.
pub fn generate_map(rules: &Ruleset, seed: u64, settings: &Value) -> Result<Value, EngineError> {
    let empty = Map::new();
    let o = match settings {
        Value::Object(o) => o,
        Value::Null => &empty,
        other => {
            return Err(EngineError::Config(format!(
                "The map settings must be a JSON object, not {}.",
                py::repr(other)
            )));
        }
    };
    let get = |k: &str| o.get(k).filter(|x| !x.is_null());
    let MapSource::Generated { size, map_type, edges, dims } = setup::generated_map(rules, &get)?
    else {
        return Err(EngineError::Config("The map settings name no generated map.".to_owned()));
    };
    let lobby = &rules.map_sizes()[size];
    let (width, height) = dims.unwrap_or((lobby.width, lobby.height));
    let count = |k: &str, default: u8| -> Result<usize, EngineError> {
        match get(k) {
            None => Ok(usize::from(default)),
            Some(v) => {
                py::int_of(v).and_then(|n| u8::try_from(n).ok()).map(usize::from).ok_or_else(|| {
                    EngineError::Config(format!(
                        "{k} must be a whole number from 0 to 255, not {}.",
                        py::repr(v)
                    ))
                })
            }
        }
    };
    let players = count("players", lobby.players)?;
    let city_states = count("city_states", lobby.city_states)?;
    let ruins = get("ruins").is_none_or(py::truthy);
    let options = MapOptions {
        edges,
        rivers: option_number(get("river_density"), 1.0, 0.0, 5.0),
        resources: resource_options(rules, get("resources")),
    };
    let shape = MapType::from_key(&rules.constants().map_types[map_type].key);
    let spec = GenSpec {
        width,
        height,
        map_type: shape,
        options,
        players,
        city_states,
        nations: &[],
        ruins,
    };
    let map = mapgen::generate(rules, seed, &spec).map_err(|e| EngineError::Map(e.0))?;
    let name = get("name").map(py::str_of).filter(|n| !n.is_empty());
    let title = || {
        let words: Vec<String> = shape
            .key()
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
            })
            .collect();
        format!("{} {width}x{} (seed {seed})", words.join(" "), map.height)
    };
    let description = format!(
        "Generated: {}, {players} players, {city_states} city-states, seed {seed}; {}.",
        shape.key(),
        spec.options.describe(rules)
    );
    Ok(json!({
        "format": "citar-map",
        "version": 1,
        "id": name.as_deref().map_or_else(String::new, slug),
        "name": name.clone().unwrap_or_else(title),
        "description": description,
        "width": map.width,
        "height": map.height,
        "wrap_x": map.wrap_x,
        "wrap_y": map.wrap_y,
        "tiles": map.tiles.iter().map(|t| tile_row(rules, t)).collect::<Vec<_>>(),
        "starts": map.starts.iter().map(|t| t.0).collect::<Vec<_>>(),
        "cs_starts": map.cs_starts.iter().map(|t| t.0).collect::<Vec<_>>(),
    }))
}

/// The most characters a map's name keeps (`maps.py:239`).
const NAME_CHARS: usize = 80;
/// The most characters its description keeps.
const DESCRIPTION_CHARS: usize = 2000;
/// The keys a document keeps as they are, which the host writes (`maps.py:244-246`).
const KEPT: [&str; 4] = ["created", "modified", "author", "recommended"];

/// The first `n` characters of `s` (Python's `s[:n]`).
fn head_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// A document's text field as Python's `str()` read it, `""` for none.
fn text_of(v: Option<&Value>) -> String {
    match v {
        None => String::new(),
        Some(v) if !py::truthy(v) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => py::str_of(v),
    }
}

/// An editor map checked against the ruleset and cleaned, with what was wrong
/// (`maps.validate` with `fix=True`, `maps.py:136-247`): the document as it would be saved, with
/// what could not stand removed, and the problems, `(x,y) what` each, at most
/// [`document::MAX_WARNINGS`] of the tiles'. Its id is its own or its name's slug; its name at most
/// 80 characters, `Untitled map` for none, and its description at most 2,000; `created`,
/// `modified`, `author` and `recommended` are kept as they are.
///
/// # Errors
/// [`EngineError::Map`] for a document that cannot be a map: not an object, no whole width and
/// height, a side out of bounds, tiles that do not fill it, a tile that is not a row or whose
/// base terrain is unknown.
pub fn validate_map(rules: &Ruleset, doc: &Value) -> Result<(Value, Vec<String>), EngineError> {
    let read = document::read(rules, doc).map_err(|e| EngineError::Map(e.0))?;
    let o = doc.as_object();
    let get = |k: &str| o.and_then(|o| o.get(k));
    let id = match get("id") {
        Some(v) if py::truthy(v) => v.clone(),
        _ => json!(slug(&text_of(get("name")))),
    };
    let name = match text_of(get("name")) {
        n if n.is_empty() => "Untitled map".to_owned(),
        n => head_chars(&n, NAME_CHARS).to_owned(),
    };
    let mut clean = json!({
        "format": "citar-map",
        "version": 1,
        "id": id,
        "name": name,
        "description": head_chars(&text_of(get("description")), DESCRIPTION_CHARS),
        "width": read.width,
        "height": read.height,
        "wrap_x": read.wrap_x,
        "wrap_y": read.wrap_y,
        "tiles": read.tiles.iter().map(|t| tile_row(rules, t)).collect::<Vec<_>>(),
        "starts": read.starts.iter().map(|t| t.0).collect::<Vec<_>>(),
        "cs_starts": read.cs_starts.iter().map(|t| t.0).collect::<Vec<_>>(),
    });
    if let Some(m) = clean.as_object_mut() {
        for k in KEPT {
            if let Some(v) = get(k) {
                m.insert(k.to_owned(), v.clone());
            }
        }
    }
    Ok((clean, read.warnings))
}

/// An empty map of one base terrain, for the editor to start from (`maps.blank_map` and the
/// facade's `blank_map`, `maps.py:71-77`, `engine_api.py:213-219`): its id is its name's slug,
/// or empty for a map with no name, which is `Untitled map`.
///
/// # Errors
/// [`EngineError::Map`] for a terrain that is no base terrain of the ruleset, or a side out of
/// bounds.
pub fn blank_map(
    rules: &Ruleset,
    width: i64,
    height: i64,
    terrain: &str,
    name: &str,
) -> Result<Value, EngineError> {
    use crate::rules::defs::TerrainType;
    let base = rules
        .lookup::<TerrainId>(terrain)
        .filter(|&t| matches!(rules.terrains()[t].kind, TerrainType::Land | TerrainType::Water));
    if base.is_none() {
        return Err(EngineError::Map(format!(
            "Unknown base terrain '{}'.",
            crate::base::text::echo(terrain)
        )));
    }
    let fits = |n: i64| (i64::from(MIN_SIDE)..=i64::from(MAX_SIDE)).contains(&n);
    if !fits(width) || !fits(height) {
        return Err(EngineError::Map(format!(
            "Maps must be between {MIN_SIDE} and {MAX_SIDE} tiles on each side."
        )));
    }
    // Both sides are within 8..=256.
    let size = usize::try_from(width * height).unwrap_or(0);
    let row = json!([terrain, [], null, 0, null, 0, null, null]);
    Ok(json!({
        "format": "citar-map",
        "version": 1,
        "id": if name.is_empty() { String::new() } else { slug(name) },
        "name": if name.is_empty() { "Untitled map" } else { name },
        "description": "",
        "width": width,
        "height": height,
        "tiles": vec![row; size],
        "starts": [],
        "cs_starts": [],
    }))
}

/// A map's headline facts, for lists (`maps.summary`, `maps.py:250-257`): its id, name and
/// description, size and wraps, how many starts it has, the share of its tiles that are land (to
/// three places), and when it was last saved.
///
/// # Errors
/// [`EngineError::Map`] for a document with no width, height or tiles, which Python's lists
/// skipped.
// refcheck: map-summary-reads-water-by-rule
pub fn map_summary(rules: &Ruleset, doc: &Value) -> Result<Value, EngineError> {
    use crate::rules::defs::TerrainType;
    let o = doc.as_object();
    let get = |k: &str| o.and_then(|o| o.get(k));
    let missing = |k: &str| EngineError::Map(format!("The map has no {k}."));
    let width = get("width").ok_or_else(|| missing("width"))?;
    let height = get("height").ok_or_else(|| missing("height"))?;
    let tiles = get("tiles").and_then(Value::as_array).ok_or_else(|| missing("tiles"))?;
    let water = |row: &Value| {
        let terrain = row.get(0).and_then(Value::as_str).and_then(|n| rules.lookup::<TerrainId>(n));
        terrain.is_some_and(|t| rules.terrains()[t].kind == TerrainType::Water)
    };
    let land = tiles.iter().filter(|row| !water(row)).count();
    #[allow(clippy::cast_precision_loss, reason = "a count of tiles, far below 2^53")]
    let share = land as f64 / tiles.len().max(1) as f64;
    let count = |k: &str| get(k).and_then(Value::as_array).map_or(0, Vec::len);
    Ok(json!({
        "id": get("id").cloned().unwrap_or(Value::Null),
        "name": get("name").cloned().unwrap_or(Value::Null),
        "description": get("description").cloned().unwrap_or_else(|| json!("")),
        "width": width,
        "height": height,
        "wrap_x": get("wrap_x").is_some_and(py::truthy),
        "wrap_y": get("wrap_y").is_some_and(py::truthy),
        "starts": count("starts"),
        "cs_starts": count("cs_starts"),
        "land_share": num::round_ndigits(share, 3),
        "modified": get("modified").cloned().unwrap_or(Value::Null),
    }))
}

impl Game {
    /// The game's terrain as a map to play again (`maps.map_from_game`, `maps.py:103-124`): the
    /// tiles as they are now, without cities, borders, units or the improvements a map may not
    /// carry, and a start where each civilization and city-state has its capital, or where it
    /// began if it has none. Its name is `Map from game` unless one is given.
    #[must_use]
    pub fn export_map(&self, name: &str) -> Value {
        let r = self.rules();
        let mut starts = Vec::new();
        let mut cs_starts = Vec::new();
        for (_, p) in self.state().players().iter() {
            if p.is_barbarian() {
                continue;
            }
            let capital =
                p.capital.and_then(|c| self.city(c)).map(crate::state::cities::City::tile);
            let Some(TileIdx(spot)) = capital.or(p.start_tile) else { continue };
            if p.is_major() { starts.push(spot) } else { cs_starts.push(spot) }
        }
        let grid = self.grid();
        json!({
            "format": "citar-map",
            "version": 1,
            "id": if name.is_empty() { String::new() } else { slug(name) },
            "name": if name.is_empty() { "Map from game" } else { name },
            "description": format!("Terrain of game turn {}.", self.turn()),
            "width": grid.width(),
            "height": grid.height(),
            "wrap_x": grid.wrap_x(),
            "wrap_y": grid.wrap_y(),
            "tiles": self.state().tiles().iter().map(|(_, t)| tile_row(r, t)).collect::<Vec<_>>(),
            "starts": starts,
            "cs_starts": cs_starts,
        })
    }
}

/// A file-safe id from a name (`maps.slug`, `maps.py:43-46`): lower-case letters and digits, the
/// rest runs of dashes, at most 60 characters; `map` for a name with none. Python named that
/// case after the clock, which the engine has not got.
#[must_use]
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
    }
    // Only ASCII is kept, so 60 bytes are 60 characters.
    out.truncate(60);
    if out.is_empty() { "map".to_owned() } else { out }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::mapgen::document;

    #[test]
    fn a_generated_document_reads_back_whole() {
        let r = Ruleset::shared();
        let settings = json!({"map_size": "duel", "map_type": "inland_sea", "map_edges": "wrap_x",
            "players": 2, "city_states": 3, "resources": {"luxury": {"density": 0.5}}});
        let doc = generate_map(r, 42, &settings).expect("a map");
        assert_eq!(doc["name"], "Inland Sea 44x28 (seed 42)");
        assert_eq!(doc["id"], "");
        assert_eq!(
            doc["description"],
            "Generated: inland_sea, 2 players, 3 city-states, seed 42; edges wrap x, luxury x0.5."
        );
        let read = document::read(r, &doc).expect("a document the editor reads");
        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert!(read.wrap_x && !read.wrap_y);
        assert_eq!(read.starts.len(), 2);
        let again = generate_map(r, 42, &settings).expect("a map");
        assert_eq!(doc, again, "a seed makes one map");
        assert_eq!(read.tiles.len(), 44 * 28);
        // The tiles come back as generated.
        let rows = doc["tiles"].as_array().expect("rows");
        for (row, t) in rows.iter().zip(&read.tiles) {
            assert_eq!(row, &tile_row(r, t));
        }
    }

    #[test]
    fn map_settings_are_refused_as_the_lobby_refuses_them() {
        let r = Ruleset::shared();
        let e = generate_map(r, 1, &json!({"map_type": "donut"})).expect_err("no such type");
        assert!(e.to_string().starts_with("Unknown map_type 'donut'. Known: continents"), "{e}");
        let e = generate_map(r, 1, &json!({"players": "many"})).expect_err("not a count");
        assert_eq!(e.to_string(), "players must be a whole number from 0 to 255, not 'many'.");
        let e = generate_map(r, 1, &json!({"width": 4})).expect_err("too narrow");
        assert!(matches!(e, EngineError::Map(_)));
        // A height is checked as given, then made even on a map that wraps north-south: the
        // longest a u16 holds is refused, not overflowed, and so is one past the longest side.
        for (height, edges) in [(65_535, "wrap_y"), (257, "wrap_both"), (7, "wrap_y")] {
            let e = generate_map(r, 1, &json!({"height": height, "map_edges": edges}))
                .expect_err("out of bounds");
            assert_eq!(e.to_string(), "Maps must be between 8 and 256 tiles on each side.");
        }
        let cfg = json!({"seed": 1, "height": 65_535, "map_edges": "wrap_y"});
        let e = crate::game::Game::config_from_json(r, cfg.to_string().as_bytes())
            .expect_err("too tall");
        assert!(matches!(e, EngineError::Map(_)), "{e}");
        let odd = json!({"map_size": "duel", "height": 29, "map_edges": "wrap_y", "players": 2});
        let tall = generate_map(r, 1, &odd).expect("an odd height made even");
        assert_eq!(tall["height"], 30);
        let named = generate_map(r, 1, &json!({"map_size": "duel", "name": "Twin Islands!"}))
            .expect("a map");
        assert_eq!(
            (named["id"].as_str(), named["name"].as_str()),
            (Some("twin-islands"), Some("Twin Islands!"))
        );
    }

    #[test]
    fn slugs_are_python_s() {
        assert_eq!(slug("Twin  islands (v2)"), "twin-islands-v2");
        assert_eq!(slug("--"), "map");
        assert_eq!(slug(&"a".repeat(70)).len(), 60);
    }
}

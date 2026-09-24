//! Maps for the host (DESIGN.md 8.1): a generated map as the editor's document
//! ([`generate_map`], `maps.generated_map`, `maps.py:80-100`).
//!
//! The engine draws nothing: `generate_map` takes the seed, which the facade draws when the
//! lobby leaves it empty, as Python's `generated_map` did itself (`maps.py:92`). The settings
//! are the lobby's, read as `Game::config_from_json` reads them, so an unknown map size, type or
//! edge mode is refused the same way (`config-refuses-unknown-names`).

use serde_json::{Map, Value, json};

use crate::base::py;
use crate::game::error::EngineError;
use crate::game::setup;
use crate::mapgen::document::tile_row;
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

//! New-game setup, completed (package 1c-09, DESIGN.md 6.14), gate 4: a new game keeps every
//! invariant and agrees with a cold rebuild of its caches on every map size and type; a setup
//! that would hold more than 64 players is refused; and an editor map's missing starts,
//! city-state sites and ruins are filled in as `maps.prepare` filled them, or the map refused
//! when there is no room for every civilization.

use citar_engine::base::ids::{ImprovementId, PlayerId};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{DebugOptions, EngineError, Game};
use citar_engine::rules::Ruleset;
use citar_testkit::golden::newgame;
use citar_testkit::script::map_doc;
use serde_json::{Value, json};

/// A new game from settings, or why it was refused.
fn try_new(settings: Value) -> Result<Game, EngineError> {
    let r = Ruleset::shared();
    let setup = config_from_value(r, settings)?;
    Game::new(r, &setup).map(|(g, _)| g)
}

/// Every check clean on a new game, and what a new game has: a start for every seat, a unit of
/// each civilization and city-state on its start or beside it.
fn sound(name: &str, g: &mut Game) {
    g.set_debug_options(DebugOptions::ALL);
    assert!(g.take_violations().is_empty(), "{name}");
    let broken = g.check_invariants();
    assert!(broken.is_empty(), "{name}: {broken:?}");
    let stale = g.verify_caches();
    assert!(stale.is_empty(), "{name}: {stale:?}");
    let grid = g.grid();
    for (id, p) in g.state().players().iter().filter(|(_, p)| !p.is_barbarian()) {
        let start = p.start_tile.unwrap_or_else(|| panic!("{name}: {id:?} has a start"));
        let near = g
            .state()
            .units()
            .of(id)
            .iter()
            .filter_map(|&u| g.unit(u))
            .any(|u| grid.distance(u.tile(), start) <= 3);
        assert!(near, "{name}: {id:?} has its starting units by its start");
    }
    assert_eq!((g.turn(), g.current()), (1, PlayerId(0)));
    assert!(g.state().clock().turn_started, "{name}: the first turn has begun");
}

/// The five map types on one lobby size, each a new game with the lobby's defaults.
fn every_type(size: &str, seed: u64) {
    for (i, ty) in
        ["continents", "pangaea", "archipelago", "inland_sea", "fractal"].iter().enumerate()
    {
        let edges = ["ice_caps", "wrap_x", "boxed", "wrap_y", "wrap_both"][i];
        let name = format!("{size} {ty} {edges}");
        let settings = newgame::generated_settings(size, ty, edges, seed + i as u64);
        let mut g = try_new(settings).unwrap_or_else(|e| panic!("{name}: {e}"));
        let lobby = g.rules().constants();
        let players = lobby.map_size_id(size).map(|id| lobby.map_sizes[id].players);
        assert_eq!(g.majors(true).count(), usize::from(players.unwrap_or(0)), "{name}");
        sound(&name, &mut g);
    }
}

#[test]
fn a_new_duel_game_is_sound_on_every_map_type() {
    every_type("duel", 100);
}

#[test]
fn a_new_small_game_is_sound_on_every_map_type() {
    every_type("small", 200);
}

#[test]
fn a_new_standard_game_is_sound_on_every_map_type() {
    every_type("standard", 300);
}

#[test]
fn a_new_large_game_is_sound_on_every_map_type() {
    every_type("large", 400);
}

#[test]
fn a_new_huge_game_is_sound_on_every_map_type() {
    every_type("huge", 500);
}

#[test]
fn a_new_gargantuan_game_is_sound_on_every_map_type() {
    every_type("gargantuan", 600);
}

#[test]
fn a_game_of_more_than_64_players_is_refused() {
    // refcheck: games-hold-at-most-64-players
    let seats = vec![json!({}); 24];
    let refused = try_new(json!({
        "seed": 3, "map_size": "gargantuan", "players": seats, "city_states": 47,
    }));
    let Err(EngineError::Config(m)) = refused else {
        panic!("24 civilizations, 40 city-states and the barbarians are refused, not {refused:?}")
    };
    assert!(m.starts_with("A game holds at most 64 players"), "{m}");
    // The map had sites for 40 of the 47 city-states asked for: they and the barbarians make 65.
    assert!(m.contains("these settings make 65"), "{m}");
    // Without the barbarians and with fewer city-states it fits.
    let fits = try_new(json!({
        "seed": 3, "map_size": "gargantuan", "players": seats, "city_states": 40,
        "barbarians": "off",
    }))
    .expect("64 players fit");
    assert_eq!(fits.state().players().len(), 64);
    // More seats than the lobby allows are refused before anything is made.
    let many = try_new(json!({"seed": 3, "players": vec![json!({}); 25]}));
    assert!(matches!(many, Err(EngineError::Config(m)) if m == "Games support 1 to 24 players."));
}

/// A document of `w` by `h` ocean tiles, with grassland at `land` and the starts `starts`.
fn ocean(w: usize, h: usize, land: &[usize], starts: &[usize]) -> Value {
    let tiles: Vec<Value> = (0..w * h)
        .map(|i| {
            let t = if land.contains(&i) { "Grassland" } else { "Ocean" };
            json!([t, [], null, 0, null, 0, null, null])
        })
        .collect();
    json!({"id": "sea", "width": w, "height": h, "tiles": tiles, "starts": starts, "cs_starts": []})
}

#[test]
fn a_map_with_no_room_for_every_civilization_is_refused() {
    // Islands of a tile each, one a start: no landmass is big enough (25 tiles) for a start to be
    // chosen, so the map holds the one civilization it gives a start and no more.
    let doc = ocean(10, 10, &[22, 55, 77], &[22]);
    let one = try_new(json!({"seed": 1, "map": doc, "players": [{}], "city_states": 0}))
        .expect("one fits");
    assert_eq!(one.player(PlayerId(0)).and_then(|p| p.start_tile).map(|t| t.0), Some(22));
    let two = try_new(json!({"seed": 1, "map": doc, "players": [{}, {}]}));
    let Err(EngineError::Map(m)) = two else { panic!("refused, not {two:?}") };
    assert_eq!(m, "This map has room for only 1 civilizations (asked for 2).");
    // City-states with no site are left out of the game.
    let alone =
        try_new(json!({"seed": 1, "map": doc, "players": [{}], "city_states": 2})).expect("a game");
    assert_eq!(alone.state().players().iter().filter(|(_, p)| p.is_city_state()).count(), 0);
}

#[test]
fn a_map_that_has_ruins_keeps_its_own_and_gets_no_more() {
    let r = Ruleset::shared();
    let ruins: ImprovementId = r.derived().known.ancient_ruins.expect("the ruleset has ruins");
    let count =
        |g: &Game| g.state().tiles().iter().filter(|(_, t)| t.improvement() == Some(ruins)).count();
    let (doc, _) = map_doc("arena").expect("the arena");
    let game = |doc: &Value, on: bool| {
        try_new(json!({"seed": 4, "map": doc, "players": [{}, {}], "city_states": 0,
                       "barbarians": "off", "ruins": on}))
        .expect("a game")
    };
    assert_eq!(count(&game(&doc, true)), 7, "spread on a map with none");
    assert_eq!(count(&game(&doc, false)), 0, "none unless the settings want them");
    let mut marked = doc.clone();
    marked["tiles"][30][6] = json!("Ancient ruins");
    assert_eq!(count(&game(&marked, true)), 1, "a map with its own gets no more");
    // The spread is the game's own draw: another seed spreads as many elsewhere.
    let other = try_new(json!({"seed": 5, "map": doc, "players": [{}, {}], "city_states": 0,
                               "barbarians": "off", "ruins": true}))
    .expect("a game");
    let at = |g: &Game| -> Vec<u32> {
        g.state()
            .tiles()
            .iter()
            .filter(|(_, t)| t.improvement() == Some(ruins))
            .map(|(i, _)| i.0)
            .collect()
    };
    assert_eq!(at(&other).len(), 7);
    assert_ne!(at(&other), at(&game(&doc, true)));
}

#[test]
fn the_arena_games_of_the_golden_set_fill_in_what_the_map_lacks() {
    let games = newgame::arena_games().expect("the arena");
    let mut counts = Vec::new();
    for (name, _, settings) in games {
        let mut g = newgame::new_game(&settings).expect("an arena game");
        let css = g.state().players().iter().filter(|(_, p)| p.is_city_state()).count();
        counts.push((g.majors(true).count(), css));
        sound(&name, &mut g);
    }
    // Six civilizations leave room for the map's one city-state site alone; two for all three.
    assert_eq!(counts, [(6, 1), (2, 3)]);
}

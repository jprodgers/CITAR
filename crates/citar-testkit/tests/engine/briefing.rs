//! The briefing and the ASCII map a model reads (package 1d-03), where the rule scripts cannot
//! reach: what a host may pass that no tool lets a model pass, and what a player knows that
//! Python's briefing did not ask.
//!
//! - `ascii_map` is a public host function: a centre far off the map, or a radius far out of
//!   range, draws the viewer's own window rather than overflowing or drawing nothing, and a
//!   centre past a wrapping edge is read across it.
//! - A saved city's food store is any finite number: a vast one still briefs.
//! - The ruins and camps a unit's options say are nearby are those its civilization knows of,
//!   as its map shows them (`briefing-nearby-reads-what-it-knows`).
//!
//! Gate 1 is the refcheck group `briefing`, gate 2 the scripts `briefing_*`.

use citar_engine::api::{briefing, testops};
use citar_engine::base::ids::{PlayerId, TileIdx};
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::TerrainType;
use citar_testkit::script::{map_doc, new_game};
use serde_json::{Value, json};

const P0: PlayerId = PlayerId(0);

/// Two benchmark civilizations on `map` with no units, no city-state and no camp, and the first
/// one's capital at (5,5).
fn game(map: &str) -> Game {
    let (doc, _) = map_doc(map).expect("the map");
    let cfg = json!({
        "seed": 1,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 0, "barbarians": "normal", "ruins": false, "map": doc,
    });
    let mut g = new_game(Ruleset::shared(), cfg.as_object().expect("an object"))
        .unwrap_or_else(|e| panic!("{e}"));
    test_ops(&mut g, json!([{"op": "clear_units", "player": "all"}, {"op": "clear_camps"}]));
    ops(&mut g, json!([{"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"}]));
    g
}

fn ops(g: &mut Game, list: Value) -> Vec<Value> {
    g.apply_ops(&list).unwrap_or_else(|e| panic!("{e}")).0
}

fn test_ops(g: &mut Game, list: Value) -> Vec<Value> {
    testops::apply(g, &list).unwrap_or_else(|e| panic!("{e}")).0
}

#[test]
fn a_map_centred_off_the_map_is_the_viewer_s_own() {
    let g = game("arena");
    let own = briefing::ascii_map(&g, P0, None, 5);
    assert!(own.starts_with("Map around (5,5), x 0-15, y 0-10:\n"), "{own}");
    assert!(own.lines().any(|l| l.starts_with("y=5 ") && l.contains('@')), "{own}");
    for far in
        [(i32::MIN, i32::MAX), (i32::MAX, i32::MIN), (1000, -1000), (24, 3), (3, 16), (-1, 0)]
    {
        assert_eq!(briefing::ascii_map(&g, P0, Some(far), 5), own, "centred on {far:?}");
    }
    // The radius is held between 2 and 20, whatever it is.
    assert_eq!(briefing::ascii_map(&g, P0, None, i64::MIN), briefing::ascii_map(&g, P0, None, 2));
    assert_eq!(briefing::ascii_map(&g, P0, None, i64::MAX), briefing::ascii_map(&g, P0, None, 20));
    // A player the game lacks gets nothing, wherever it looks.
    assert_eq!(briefing::ascii_map(&g, PlayerId(9), Some((i32::MIN, 0)), 5), "");
}

#[test]
fn a_centre_past_a_wrapping_edge_is_read_across_it() {
    let g = game("arena_wrap");
    let seam = briefing::ascii_map(&g, P0, Some((23, 15)), 3);
    assert!(
        seam.starts_with(
            "Map around (23,15), x 17-5, y 12-2 (the map wraps: this window runs across its edge):\n"
        ),
        "{seam}"
    );
    for past in [(-1, -1), (47, 31), (-25, 15)] {
        assert_eq!(briefing::ascii_map(&g, P0, Some(past), 3), seam, "centred on {past:?}");
    }
    // As far as a host can go, the window is still drawn, across the map's own seams.
    let far = briefing::ascii_map(&g, P0, Some((i32::MIN, i32::MAX)), 20);
    assert!(far.starts_with("Map around ("), "{far}");
    assert!(far.lines().filter(|l| l.starts_with("y=")).count() > 10, "{far}");
}

#[test]
fn a_city_with_a_vast_food_store_is_briefed() {
    let mut g = game("arena");
    ops(&mut g, json!([{"op": "set_city", "x": 5, "y": 5, "food": 1e19}]));
    let text = g.briefing(P0);
    let line =
        text.lines().find(|l| l.contains("] Roma (5,5)")).unwrap_or_else(|| panic!("{text}"));
    // Python's -(-need // per) with need about -1e19: past what the growth can mean, but a
    // number, not a panic.
    assert!(line.contains("(grows in -"), "{line}");
}

/// The line of a unit's options in the briefing, if it has one.
fn options_of(g: &Game, unit: &str) -> String {
    let text = g.briefing(P0);
    let head = format!("  [#{unit}] Warrior (");
    let at = text.find("OPTIONS FOR UNITS NEEDING ORDERS:").unwrap_or_else(|| panic!("{text}"));
    text[at..]
        .lines()
        .find(|l| l.starts_with(&head))
        .unwrap_or_else(|| panic!("no options for #{unit}: {text}"))
        .to_owned()
}

#[test]
fn a_unit_is_told_of_the_camps_its_civilization_knows_of() {
    let mut g = game("arena");
    let made = ops(
        &mut g,
        json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 7, "y": 5},
            {"op": "reveal", "player": 0},
        ]),
    );
    let warrior = made[0]["unit_ids"][0].to_string();
    // A tile the reveal explored, which the civilization has never seen: flat land 5 or 6 tiles
    // from the warrior, empty.
    let from = g.grid().idx(7, 5).expect("on the map");
    let r = g.rules();
    let open = |t: TileIdx| {
        let tile = g.tile(t).expect("a tile");
        let def = &r.terrains()[tile.terrain()];
        def.kind == TerrainType::Land
            && !def.impassable
            && tile.wonder().is_none()
            && tile.improvement().is_none()
            && g.city_at(t).is_none()
            && g.units_at(t).next().is_none()
    };
    let fog: TileIdx = g
        .grid()
        .within(from, 6)
        .into_iter()
        .find(|&t| g.grid().distance(from, t) >= 5 && open(t) && !g.derived().vis().sees(P0, t))
        .expect("a tile in the fog");
    // Where the warrior can stand to see it.
    let beside =
        g.grid().within(fog, 1).into_iter().find(|&t| t != fog && open(t)).expect("a tile by it");
    let (x, y) = g.xy(fog);
    let (bx, by) = g.xy(beside);
    let camp = format!("barbarian camp ({x},{y})");

    // Raised in the fog, it is no camp the civilization knows of: its map does not show it, and
    // neither do the warrior's options, where Python's named it.
    test_ops(&mut g, json!([{"op": "create_camp", "x": x, "y": y}]));
    assert!(!options_of(&g, &warrior).contains(&camp), "{}", options_of(&g, &warrior));
    assert!(!briefing::ascii_map(&g, P0, Some((x, y)), 2).contains('X'));

    // Once seen, it is; and it still is from out of sight, as the map remembers it.
    test_ops(
        &mut g,
        json!([{"op": "set_unit", "unit": made[0]["unit_ids"][0], "x": bx, "y": by},
               {"op": "refresh_visibility"}]),
    );
    assert!(options_of(&g, &warrior).contains(&camp), "{}", options_of(&g, &warrior));
    test_ops(
        &mut g,
        json!([{"op": "set_unit", "unit": made[0]["unit_ids"][0], "x": 7, "y": 5},
               {"op": "refresh_visibility"}]),
    );
    assert!(!g.derived().vis().sees(P0, fog), "out of sight again");
    assert!(options_of(&g, &warrior).contains(&camp), "{}", options_of(&g, &warrior));

    // Cleared where it cannot see, the camp is still there for it until it looks again.
    test_ops(&mut g, json!([{"op": "clear_camps"}]));
    assert!(options_of(&g, &warrior).contains(&camp), "{}", options_of(&g, &warrior));
}

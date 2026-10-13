//! The minimap's block of a seat's own view (design/phase3-features.md P3.7.2 and P3.7.5,
//! package 3-17):
//!
//! - the stage thresholds, on the shipped ruleset (the Renaissance and the Information era by
//!   name), on one whose eras are all renamed (by position: ⌈n/3⌉ and the second-to-last) and on
//!   one whose names sit at other eras (the names win);
//! - the home box on a map that does not wrap, one that wraps east-west and one that wraps both
//!   ways, each checked against the view's own tiles: the explored tiles of the home continent,
//!   covered the short way round;
//! - a tile explored on another continent left out;
//! - a seat with no capital (its first city, else its settler) and with no city and no settler;
//! - who has a block: a major's own view, never the god view nor a city-state's;
//! - its cost: at most 5% of the seat view's on the late fixtures (P3.9.7).

use citar_engine::api::views::{Minimap, Stage, TileBox};
use citar_engine::api::{maps::blank_map, testops};
use citar_engine::base::ids::{PlayerId, TileIdx};
use citar_engine::game::{DebugOptions, Game};
use citar_engine::rules::{Ruleset, embedded};
use citar_testkit::fixtures;
use citar_testkit::games;
use citar_testkit::rulesets::files_of;
use citar_testkit::script::new_game;
use serde_json::{Value, json};

const P0: PlayerId = PlayerId(0);

/// A map of `w` by `h`, land where `land(x, y)` (grassland) and ocean elsewhere, with the two
/// majors' starts.
fn map_doc(
    w: u16,
    h: u16,
    edges: (bool, bool),
    land: impl Fn(i32, i32) -> bool,
    starts: [(i32, i32); 2],
) -> Value {
    let mut doc = blank_map(Ruleset::shared(), i64::from(w), i64::from(h), "Ocean", "minimap")
        .expect("a blank map");
    for y in 0..i32::from(h) {
        for x in 0..i32::from(w) {
            if land(x, y) {
                let i = usize::try_from(y * i32::from(w) + x).expect("on the map");
                doc["tiles"][i] = json!(["Grassland", [], null, 0, null, 0, null, null]);
            }
        }
    }
    doc["wrap_x"] = json!(edges.0);
    doc["wrap_y"] = json!(edges.1);
    doc["starts"] = json!(starts.map(|(x, y)| y * i32::from(w) + x));
    doc["cs_starts"] = json!([]);
    doc
}

/// Two benchmark civilizations on `doc`, without barbarians or ruins.
fn game_on(rules: &'static Ruleset, doc: Value) -> Game {
    let cfg = json!({
        "seed": 7,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 0, "barbarians": "off", "ruins": false, "map": doc,
    });
    new_game(rules, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"))
}

fn ops(g: &mut Game, list: Value) {
    g.apply_ops(&list).unwrap_or_else(|e| panic!("{e}"));
}

fn test_ops(g: &mut Game, list: Value) {
    testops::apply(g, &list).unwrap_or_else(|e| panic!("{e}"));
}

/// The seat's view as the browser gets it, read back.
fn view(g: &Game, p: Option<PlayerId>) -> Value {
    serde_json::from_slice(&g.view_json(p, 50)).expect("the view is JSON")
}

/// The shortest interval covering the marked positions, found the long way: every arc tried,
/// the shortest kept, of those one that does not cross the seam, else the first to start. The
/// engine finds it by the longest gap ([`citar_engine::api::views::minimap::covering`]).
fn shortest_arc(on: &[bool], wrap: bool) -> Option<(usize, usize)> {
    let n = on.len();
    let marked: Vec<usize> = (0..n).filter(|&i| on[i]).collect();
    if !wrap {
        return Some((*marked.first()?, *marked.last()?));
    }
    let mut best: Option<(usize, bool, usize)> = None;
    for start in 0..n {
        for len in 1..=n {
            if marked.iter().all(|&i| (i + n - start) % n < len) {
                let key = (len, start + len > n, start);
                if best.is_none_or(|b| key < b) {
                    best = Some(key);
                }
                break;
            }
        }
    }
    best.map(|(len, _, start)| (start, (start + len - 1) % n))
}

/// The box the home continent's explored tiles make, as the view's own tiles show them: each
/// tile the view lists is one the seat has explored.
fn expected_home(g: &Game, v: &Value, continent: u16) -> Option<TileBox> {
    let (w, h) = (v["width"].as_u64().expect("a width"), v["height"].as_u64().expect("a height"));
    let mut cols = vec![false; usize::try_from(w).expect("small")];
    let mut rows = vec![false; usize::try_from(h).expect("small")];
    for row in v["tiles"].as_array().expect("tiles") {
        let i = u32::try_from(row[0].as_u64().expect("an index")).expect("small");
        if g.continent(TileIdx(i)) == Some(continent) {
            let (x, y) = g.xy(TileIdx(i));
            cols[usize::try_from(x).expect("on the map")] = true;
            rows[usize::try_from(y).expect("on the map")] = true;
        }
    }
    let (x0, x1) = shortest_arc(&cols, v["wrap_x"] == json!(true))?;
    let (y0, y1) = shortest_arc(&rows, v["wrap_y"] == json!(true))?;
    let at = |i: usize| i32::try_from(i).expect("small");
    Some(TileBox { x0: at(x0), y0: at(y0), x1: at(x1), y1: at(y1) })
}

/// The seat's block, read from its view, with its home box checked against the view's tiles.
fn block(g: &Game, p: PlayerId, home_continent: Option<u16>) -> Minimap {
    let v = view(g, Some(p));
    let m = g.minimap(p).expect("a major has a block");
    assert_eq!(v["minimap"], serde_json::to_value(m).expect("JSON"), "the view writes the block");
    let want = home_continent.and_then(|c| expected_home(g, &v, c));
    assert_eq!(m.home, want, "the home box is the explored home continent's");
    m
}

fn tile_at(g: &Game, x: i32, y: i32) -> TileIdx {
    g.grid().idx(x, y).expect("on the map")
}

// ---- The stages ---------------------------------------------------------------------------

/// The shipped ruleset's files with every era name replaced as `rename` says, in every file
/// (the eras' own, the techs' and policies' eras, the uniques that name one), loaded once.
fn renamed_eras(rename: &[(&str, &str)]) -> &'static Ruleset {
    let files: Vec<(String, Vec<u8>)> = embedded()
        .files
        .iter()
        .map(|&(name, bytes)| {
            let mut text = String::from_utf8(bytes.to_vec()).expect("UTF-8");
            // through placeholders, so that names may be swapped
            for (i, (from, _)) in rename.iter().enumerate() {
                text = text.replace(from, &format!("\u{1}{i}\u{1}"));
            }
            for (i, (_, to)) in rename.iter().enumerate() {
                text = text.replace(&format!("\u{1}{i}\u{1}"), to);
            }
            (name.to_owned(), text.into_bytes())
        })
        .collect();
    Ruleset::leak(&files_of(&files)).unwrap_or_else(|e| panic!("the renamed eras load:\n{e}"))
}

/// A small one-continent game on `rules`, its first civilization granted each era in turn: the
/// stage its view's block shows at each.
fn stages(rules: &'static Ruleset) -> Vec<Stage> {
    let doc = map_doc(16, 10, (false, false), |x, _| (2..=13).contains(&x), [(4, 4), (11, 5)]);
    let mut g = game_on(rules, doc);
    let names: Vec<String> = rules.eras().as_slice().iter().map(|e| e.name.to_string()).collect();
    let mut out = vec![block(&g, P0, g.continent(tile_at(&g, 4, 4))).stage];
    for name in &names[1..] {
        ops(&mut g, json!([{"op": "grant_era", "player": 0, "era": name}]));
        let era = citar_engine::game::query::era(&g, P0);
        assert_eq!(rules.name(era), Some(name.as_str()), "the civilization is in {name}");
        out.push(block(&g, P0, g.continent(tile_at(&g, 4, 4))).stage);
    }
    out
}

const E: Stage = Stage::Early;
const R: Stage = Stage::Renaissance;
const I: Stage = Stage::Information;

#[test]
fn the_stage_opens_out_at_the_renaissance_and_the_information_era() {
    // the shipped nine: the Renaissance era is the fourth, the Information era the eighth
    assert_eq!(stages(Ruleset::shared()), [E, E, E, R, R, R, R, I, I]);
}

#[test]
fn a_ruleset_without_the_names_counts_positions() {
    let eras = [
        ("Ancient era", "Age of Stone"),
        ("Classical era", "Age of Bronze"),
        ("Medieval era", "Age of Iron"),
        ("Renaissance era", "Age of Sail"),
        ("Industrial era", "Age of Steam"),
        ("Modern era", "Age of Oil"),
        ("Atomic era", "Age of the Atom"),
        ("Information era", "Age of Data"),
        ("Future era", "Age to Come"),
    ];
    let rules = renamed_eras(&eras);
    let k = rules.derived().known.minimap;
    assert_eq!(
        (k.renaissance, k.information),
        (rules.eras().ids().nth(3), rules.eras().ids().nth(7))
    );
    // nine eras: the one at ⌈9/3⌉ = 3 and the second-to-last, 7, as the shipped names sit
    assert_eq!(stages(rules), [E, E, E, R, R, R, R, I, I]);
}

#[test]
fn the_names_win_wherever_they_sit() {
    // the Renaissance's name on the third era and the Information era's on the seventh
    let swapped = [
        ("Medieval era", "Renaissance era"),
        ("Renaissance era", "Medieval era"),
        ("Atomic era", "Information era"),
        ("Information era", "Atomic era"),
    ];
    assert_eq!(stages(renamed_eras(&swapped)), [E, E, R, R, R, R, I, I, I]);
}

// ---- The home box -------------------------------------------------------------------------

/// Land on two continents: `a` and `b` say which columns are whose, rows 1 to `h - 2` (all of
/// them on a map that wraps north-south, where `rows` says which).
struct Shape {
    w: u16,
    h: u16,
    edges: (bool, bool),
    a: fn(i32) -> bool,
    b: fn(i32) -> bool,
    rows: fn(i32) -> bool,
}

impl Shape {
    fn game(&self, starts: [(i32, i32); 2]) -> Game {
        let (a, b, rows) = (self.a, self.b, self.rows);
        game_on(
            Ruleset::shared(),
            map_doc(self.w, self.h, self.edges, |x, y| rows(y) && (a(x) || b(x)), starts),
        )
    }
}

/// The checks every shape gets: the start's home box, then with a capital, then with a unit on
/// the other continent (its tiles explored, and left out), then with everything revealed (the
/// whole home continent).
fn home_box_checks(
    shape: &Shape,
    start: (i32, i32),
    other: (i32, i32),
    whole: TileBox,
) -> Vec<TileBox> {
    let mut g = shape.game([start, (other.0, other.1)]);
    let home = g.continent(tile_at(&g, start.0, start.1)).expect("land");
    let away = g.continent(tile_at(&g, other.0, other.1)).expect("land");
    assert_ne!(home, away, "two continents");
    let mut boxes = Vec::new();
    // the start: no city yet, so the settler's continent
    let m = block(&g, P0, Some(home));
    assert_eq!((m.stage, m.capital), (Stage::Early, None));
    boxes.extend(m.home);
    // a capital
    ops(
        &mut g,
        json!([{"op": "found_city", "player": 0, "x": start.0, "y": start.1, "capital": true}]),
    );
    let m = block(&g, P0, Some(home));
    assert_eq!(m.capital, Some([start.0, start.1]));
    boxes.extend(m.home);
    // a unit of the seat's on the other continent: what it sees there is explored, and left out
    ops(
        &mut g,
        json!([{"op": "add_unit", "player": 0, "unit": "Scout", "x": other.0, "y": other.1}]),
    );
    test_ops(&mut g, json!([{"op": "refresh_visibility"}]));
    let v = view(&g, Some(P0));
    let there = v["tiles"]
        .as_array()
        .expect("tiles")
        .iter()
        .filter(|row| {
            g.continent(TileIdx(u32::try_from(row[0].as_u64().unwrap_or(0)).unwrap_or(0)))
                == Some(away)
        })
        .count();
    assert!(there > 0, "the scout explored the other continent");
    let m = block(&g, P0, Some(home));
    assert_eq!(m.home, boxes.last().copied(), "the other continent's tiles change nothing");
    // everything explored: the whole home continent
    ops(&mut g, json!([{"op": "reveal", "player": 0}]));
    let m = block(&g, P0, Some(home));
    assert_eq!(m.home, Some(whole));
    boxes.extend(m.home);
    boxes
}

#[test]
fn the_home_box_on_a_map_that_does_not_wrap() {
    let shape = Shape {
        w: 30,
        h: 14,
        edges: (false, false),
        a: |x| (2..=12).contains(&x),
        b: |x| (17..=27).contains(&x),
        rows: |y| (1..=12).contains(&y),
    };
    let boxes = home_box_checks(&shape, (6, 6), (21, 7), TileBox { x0: 2, y0: 1, x1: 12, y1: 12 });
    assert!(boxes.iter().all(|b| b.x0 <= b.x1 && b.y0 <= b.y1), "{boxes:?}");
}

#[test]
fn the_home_box_crosses_an_east_west_seam() {
    // the home continent runs from column 25 over the seam to column 4
    let shape = Shape {
        w: 30,
        h: 14,
        edges: (true, false),
        a: |x| x >= 25 || x <= 4,
        b: |x| (11..=19).contains(&x),
        rows: |y| (1..=12).contains(&y),
    };
    let boxes = home_box_checks(&shape, (0, 6), (15, 7), TileBox { x0: 25, y0: 1, x1: 4, y1: 12 });
    assert!(boxes.iter().all(|b| b.x0 > b.x1), "every box crosses the seam: {boxes:?}");
}

#[test]
fn the_home_box_crosses_both_seams() {
    // the home continent runs over both seams, from (25, 11) to (4, 2)
    let shape = Shape {
        w: 30,
        h: 14,
        edges: (true, true),
        a: |x| x >= 25 || x <= 4,
        b: |x| (11..=19).contains(&x),
        rows: |y| y >= 11 || y <= 2 || (5..=8).contains(&y),
    };
    let mut g = shape.game([(0, 0), (15, 7)]);
    let home = g.continent(tile_at(&g, 0, 0)).expect("land");
    let m = block(&g, P0, Some(home));
    let first = m.home.expect("a home");
    assert!(first.x0 > first.x1 && first.y0 > first.y1, "{first:?}");
    ops(&mut g, json!([{"op": "reveal", "player": 0}]));
    // rows 5 to 8 are land of their own in the columns of the home continent, but not joined to
    // it: another continent, as the engine counts them
    let m = block(&g, P0, Some(home));
    assert_eq!(m.home, Some(TileBox { x0: 25, y0: 11, x1: 4, y1: 2 }));
}

/// The game as a save reloads it, with the seat's capital forgotten: a state Python wrote could
/// hold cities and no capital, which play itself never leaves (a lost capital moves at once).
fn without_capital(g: &Game) -> Game {
    let bytes = g.snapshot().to_json().expect("the state");
    let mut state: Value = serde_json::from_slice(&bytes).expect("JSON");
    state["players"][0]["capital"] = Value::Null;
    let bytes = serde_json::to_vec(&state).expect("JSON");
    Game::load(Ruleset::shared(), &bytes, &mut std::iter::empty()).expect("it loads").0
}

#[test]
fn without_a_capital_the_first_city_then_a_settler_anchors_the_home() {
    let shape = Shape {
        w: 30,
        h: 14,
        edges: (false, false),
        a: |x| (2..=12).contains(&x),
        b: |x| (17..=27).contains(&x),
        rows: |y| (1..=12).contains(&y),
    };
    let mut g = shape.game([(6, 6), (21, 7)]);
    let (a, b) = (g.continent(tile_at(&g, 6, 6)), g.continent(tile_at(&g, 21, 7)));
    // the settler at the start
    let m = block(&g, P0, a);
    assert_eq!((m.capital, m.home.is_some()), (None, true));
    // only a scout, on the other continent: no settler and no city, so no home
    test_ops(&mut g, json!([{"op": "clear_units", "player": 0}]));
    ops(&mut g, json!([{"op": "add_unit", "player": 0, "unit": "Scout", "x": 20, "y": 3}]));
    let m = block(&g, P0, None);
    assert_eq!((m.capital, m.home), (None, None));
    // a settler there makes it home
    ops(&mut g, json!([{"op": "add_unit", "player": 0, "unit": "Settler", "x": 22, "y": 4}]));
    test_ops(&mut g, json!([{"op": "refresh_visibility"}]));
    assert!(block(&g, P0, b).home.is_some());
    // a city on the first continent is the capital, and wins over the settler
    ops(&mut g, json!([{"op": "found_city", "player": 0, "x": 8, "y": 9}]));
    assert_eq!(block(&g, P0, a).capital, Some([8, 9]));
    // a second city on the other continent; with the capital forgotten, the first city anchors
    // the home, not the second nor the settler
    ops(&mut g, json!([{"op": "found_city", "player": 0, "x": 25, "y": 10}]));
    let lost = without_capital(&g);
    let m = block(&lost, P0, a);
    assert_eq!(m.capital, None, "cities, and no capital");
    assert!(m.home.is_some());
    // with nothing left, nothing
    ops(
        &mut g,
        json!([{"op": "remove_city", "x": 8, "y": 9}, {"op": "remove_city", "x": 25, "y": 10}]),
    );
    test_ops(&mut g, json!([{"op": "clear_units", "player": 0}]));
    let m = block(&g, P0, None);
    assert_eq!((m.stage, m.capital, m.home), (Stage::Early, None, None));
    assert_eq!(
        view(&g, Some(P0))["minimap"],
        json!({"stage": "early", "capital": null, "home": null})
    );
}

#[test]
fn only_a_major_s_own_view_has_a_block() {
    let (doc, _) = citar_testkit::script::map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": 3,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 1, "barbarians": "off", "ruins": false, "map": doc,
    });
    let g = new_game(Ruleset::shared(), cfg.as_object().expect("an object"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(view(&g, None).get("minimap").is_none(), "the god view has none");
    let cs = g
        .state()
        .players()
        .iter()
        .find(|(_, p)| p.is_city_state())
        .map(|(id, _)| id)
        .expect("a city-state");
    assert!(view(&g, Some(cs)).get("minimap").is_none(), "a city-state's view has none");
    assert!(g.minimap(cs).is_none() && g.minimap(PlayerId(40)).is_none());
    for p in [PlayerId(0), PlayerId(1)] {
        assert!(view(&g, Some(p))["minimap"].is_object(), "{p:?}'s own view has one");
    }
    // a view's keys are its own: a host may not write a key called minimap
    assert!(citar_engine::api::views::ClientView::FIELDS.contains(&"minimap"));
}

// ---- Its cost (P3.9.7) --------------------------------------------------------------------

#[test]
fn the_block_costs_at_most_5_percent_of_the_seat_view() {
    #[allow(clippy::disallowed_types, reason = "the gate is a timing; the game never sees it")]
    fn median_of_alternating(g: &Game, p: PlayerId) -> (f64, f64) {
        const REPEAT: u32 = 20;
        let (mut block, mut view) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            let t = std::time::Instant::now();
            for _ in 0..REPEAT {
                std::hint::black_box(g.minimap(std::hint::black_box(p)));
            }
            block.push(t.elapsed().as_secs_f64() / f64::from(REPEAT));
            let t = std::time::Instant::now();
            for _ in 0..REPEAT {
                std::hint::black_box(g.view_json(std::hint::black_box(Some(p)), 150));
            }
            view.push(t.elapsed().as_secs_f64() / f64::from(REPEAT));
        }
        block.sort_by(f64::total_cmp);
        view.sort_by(f64::total_cmp);
        (block[2], view[2])
    }
    // CI's shared runners stall at times: the budget is the design's on the laptop, three times it there
    #[allow(clippy::disallowed_methods, reason = "a test's budget, not a game's input")]
    let budget = if std::env::var_os("CI").is_some() { 0.15 } else { 0.05 };
    let late: Vec<_> = fixtures::committed()
        .expect("the fixtures")
        .into_iter()
        .filter(|f| f.path.to_string_lossy().replace('\\', "/").contains("fixtures-late"))
        .collect();
    assert_eq!(late.len(), 3, "the three late fixtures");
    for f in &late {
        let g = games::from_fixture(f, f.name.as_bytes(), DebugOptions::OFF)
            .expect("the fixture loads");
        let p = g.majors(true).map(|x| x.id()).next().expect("a living major");
        let (block, view) = median_of_alternating(&g, p);
        #[allow(clippy::disallowed_macros, reason = "the timing is reported, not only asserted")]
        {
            println!(
                "{}: the minimap block {:.1} µs, the seat view {:.1} µs, {:.2}% ({} tiles, {} explored)",
                f.name,
                block * 1e6,
                view * 1e6,
                block / view * 100.0,
                g.state().tiles().iter().count(),
                g.player(p).map_or(0, |x| x.explored.len())
            );
        }
        assert!(
            block <= budget * view,
            "{}: the block took {block:e} s, the view {view:e} s",
            f.name
        );
    }
}

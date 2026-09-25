//! The golden set of package 1c-09 (DESIGN.md 6.14, 9.6):
//! - **`newgame.json`**: ten new games on generated maps, duel to huge, every map type and edge
//!   mode among them, and two on the rule scripts' arena, which has fewer starts and city-state
//!   sites than they ask for and no ruins, so `maps.prepare` fills them. Each row is
//!   the state's digest right after `Game::new`, with a few counts beside it so that a diff says
//!   what moved. A digest that moves is a change to setup (or to what it calls: map generation,
//!   the starting techs, units, camps, sight); one that differs between targets is a determinism
//!   bug in it. A new game must also keep every invariant and agree with a cold rebuild of its
//!   caches, which the check asks too.
//!
//! Written by `golden bless` when setup changes on purpose.

use citar_engine::game::{DebugOptions, Game};
use citar_engine::rules::Ruleset;
use serde_json::{Map, Value, json};

use super::{SetReport, capped, diff_rows, digest_of, read_committed, render_rows};
use crate::script;

/// The generated games: (lobby size, map type, edges, seed).
const GAMES: [(&str, &str, &str, u64); 10] = [
    ("duel", "continents", "ice_caps", 1),
    ("duel", "archipelago", "wrap_x", 2),
    ("small", "pangaea", "boxed", 3),
    ("small", "fractal", "wrap_y", 4),
    ("standard", "inland_sea", "ice_caps", 5),
    ("standard", "continents", "wrap_both", 6),
    ("large", "archipelago", "ice_caps", 7),
    ("large", "fractal", "wrap_x", 8),
    ("huge", "pangaea", "ice_caps", 9),
    ("huge", "inland_sea", "wrap_x", 10),
];

/// The settings of a generated game: the lobby's size, type and edges, its players with the
/// first a person's seat and the rest the bot's (AI seats get free techs), and every other
/// setting at its default: city-states, barbarians and ruins as the lobby has them.
#[must_use]
pub fn generated_settings(size: &str, map_type: &str, edges: &str, seed: u64) -> Value {
    let r = Ruleset::shared();
    let c = r.constants();
    let players = c.map_size_id(size).map_or(2, |id| usize::from(c.map_sizes[id].players));
    let seats: Vec<Value> = (0..players)
        .map(|i| if i == 0 { json!({"controller": "human"}) } else { json!({"controller": "bot"}) })
        .collect();
    json!({
        "seed": seed,
        "map_size": size,
        "map_type": map_type,
        "map_edges": edges,
        "players": seats,
    })
}

/// A new game from settings, as JSON.
///
/// # Errors
/// If the settings are refused.
pub fn new_game(settings: &Value) -> Result<Game, String> {
    let cfg: Map<String, Value> = settings.as_object().cloned().unwrap_or_default();
    script::new_game(Ruleset::shared(), &cfg)
}

/// One row: the digest and the counts.
fn row(name: &str, seed: u64, g: &Game) -> Result<Value, String> {
    let st = g.state();
    let digest = g.digest().map_err(|e| format!("{name}: does not digest: {e}"))?;
    let kinds = |f: fn(&citar_engine::state::players::Player) -> bool| {
        st.players().iter().filter(|(_, p)| f(p)).count()
    };
    let explored: usize = st.players().iter().map(|(_, p)| p.explored.len()).sum();
    Ok(json!([
        name,
        seed,
        [g.grid().width(), g.grid().height()],
        digest.to_hex(),
        [
            kinds(citar_engine::state::players::Player::is_major),
            kinds(citar_engine::state::players::Player::is_city_state),
            kinds(citar_engine::state::players::Player::is_barbarian)
        ],
        st.units().len(),
        st.world().camps.len(),
        explored,
        g.chronicle().events().len(),
    ]))
}

/// What a new game must keep: every invariant, and caches equal to a cold rebuild.
fn soundness(name: &str, g: &mut Game) -> Vec<String> {
    g.set_debug_options(DebugOptions::ALL);
    let mut out: Vec<String> =
        g.check_invariants().iter().map(|v| format!("{name}: breaks {v:?}")).collect();
    out.extend(g.verify_caches().into_iter().map(|e| format!("{name}: caches: {e}")));
    out
}

/// The games on the rule scripts' arena, which gives five starts, one city-state site and no
/// ruins: `(name, seed, settings)`. Six civilizations take its starts and a sixth chosen, which
/// leaves room for only the one city-state site of the three asked for; two leave room for all
/// three, two of them chosen. Both want ruins and the barbarians.
///
/// # Errors
/// If the arena cannot be read.
pub fn arena_games() -> Result<Vec<(String, u64, Value)>, String> {
    let (doc, _) = script::map_doc("arena")?;
    let game = |seed: u64, seats: Value| json!({"seed": seed, "players": seats, "city_states": 3, "ruins": true, "map": doc});
    Ok(vec![
        (
            "newgame-arena-six-seats".to_owned(),
            11,
            game(
                11,
                json!([{}, {"controller": "bot"}, {}, {"controller": "bot"}, {}, {"controller": "bot"}]),
            ),
        ),
        (
            "newgame-arena-city-state-sites".to_owned(),
            12,
            game(12, json!([{}, {"controller": "bot"}])),
        ),
    ])
}

/// One row per game, and what went wrong setting one up.
fn answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    let mut games: Vec<(String, u64, Result<Value, String>)> = GAMES
        .iter()
        .map(|&(size, ty, edges, seed)| {
            (format!("newgame-{size}-{ty}"), seed, Ok(generated_settings(size, ty, edges, seed)))
        })
        .collect();
    match arena_games() {
        Ok(more) => games.extend(more.into_iter().map(|(name, seed, v)| (name, seed, Ok(v)))),
        Err(e) => problems.push(format!("the arena: {e}")),
    }
    for (name, seed, settings) in games {
        let made = settings.and_then(|s| new_game(&s));
        match made {
            Ok(mut g) => match row(&name, seed, &g) {
                Ok(v) => {
                    rows.push(v);
                    problems.extend(soundness(&name, &mut g));
                }
                Err(e) => problems.push(e),
            },
            Err(e) => problems.push(format!("{name}: does not set up: {e}")),
        }
    }
    let v = json!({
        "format": 1,
        "games": "[name, seed, [width, height], digest after Game::new, [majors, city-states, barbarians], units, camps, explored tiles (summed over players), events]",
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
    vec![("newgame.json", render(&answers().0))]
}

/// The `newgame` set, checked against `newgame.json`.
#[must_use]
pub fn check_newgame() -> SetReport {
    let (got, mut problems) = answers();
    match read_committed("newgame.json") {
        Err(e) => problems.push(e),
        Ok(want) => {
            if want.get("ruleset_id") != got.get("ruleset_id") {
                problems.push("newgame.json: the ruleset id differs from this build's".to_owned());
            }
            problems.extend(diff_rows("newgame.json", "rows", want.get("rows"), &got["rows"]));
        }
    }
    SetReport {
        name: "newgame",
        computed: digest_of(&got),
        problems: capped(problems),
        waiting: Vec::new(),
    }
}

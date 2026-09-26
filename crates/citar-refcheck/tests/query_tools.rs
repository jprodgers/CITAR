//! The query tools and the facade's other views against the Python engine's answers
//! (package 1d-02): `refcheck/query_tools.json.gz`, recorded by `scripts/refcheck/query_tools.py`
//! on the committed fixtures.
//!
//! The refcheck group `views` compares what the browser receives; the query tools read more of
//! the views than that (a unit's and a city's detail, tiles, the tech tree, policies, religion,
//! great people, espionage, city-states, victory), and the facade adds a spectator's view,
//! `empire_summary`, `standings` and `path_preview`. Each recorded call runs through
//! `Game::execute_query` on the fixture loaded as refcheck loads it, and each answer is compared
//! with refcheck's comparator; a difference must be one the engine makes on purpose, named by an
//! id of `refcheck/intended.toml` or `tests/rules/intended.toml` in [`EXPLAINED`].
//!
//! Set `CITAR_QUERY_TOOLS` to a recording of a corpus (`query_tools.py --corpus <dir> --out
//! <file>`) and `CITAR_REFCHECK_CORPUS` to that corpus to check it instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use citar_engine::base::ids::{PlayerId, UnitId};
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_refcheck::Group;
use citar_refcheck::compare::{self, CompareSpec, Options, Pattern};
use serde_json::{Value, json};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Differences the engine makes on purpose, by the intended id that explains them and the path
/// of the difference under the tool's name (or `god_view`, `empire_summary`, `standings`,
/// `path_preview`), and whether the committed recording shows it (an explanation it shows must
/// be used there, or it is stale; the others are the corpus's).
const EXPLAINED: &[(&str, &str, bool)] = &[
    // Python matched the quests on a key its list never had, so it listed none.
    ("city-state-view-lists-its-quests", "get_city_states.ok[*].quests", false),
    ("city-state-view-lists-its-quests", "get_city_states.ok[*].quests[*]", false),
    // Python read each kind's points under its pool's name, so it showed none.
    ("great-people-view-shows-the-points", "get_great_people.ok.progress[*].points", true),
    // A civilian brought to 0 health, which Rust loads at 1.
    ("civilians-at-zero-health", "get_unit.ok.hp", false),
    ("civilians-at-zero-health", "get_units.ok[*].hp", false),
    ("civilians-at-zero-health", "get_tile.ok.units[*].hp", false),
    ("civilians-at-zero-health", "get_unit.ok.attack_targets[*].defender_hp", false),
    ("civilians-at-zero-health", "god_view.units[*].hp", false),
    // Marble's bonus toward wonders, in its own city only: production, and the turns to build.
    ("marble-bonus-in-its-own-city", "get_city.ok.yields.production", true),
    ("marble-bonus-in-its-own-city", "get_city.ok.yield_breakdown.*.production", true),
    ("marble-bonus-in-its-own-city", "get_city.ok.queue[*].turns", true),
    ("marble-bonus-in-its-own-city", "get_city.ok.can_build.wonders[*].turns", true),
    ("marble-bonus-in-its-own-city", "get_cities.ok[*].yields.production", true),
    ("marble-bonus-in-its-own-city", "get_cities.ok[*].queue[*].turns", true),
    ("marble-bonus-in-its-own-city", "god_view.cities[*].yields.production", true),
    ("marble-bonus-in-its-own-city", "god_view.cities[*].queue[*].turns", true),
    // A farm, mine or plantation offered on a forest or jungle, the feature removed first.
    ("improvements-over-removable-features", "get_unit.ok.build_options[*]", true),
    // A city's food total a hair from a tenth's half or from zero, rounded as it is.
    ("city-view-rounds-its-own-sums", "get_city.ok.yields.food", false),
    ("city-view-rounds-its-own-sums", "get_city.ok.turns_to_grow", false),
    ("city-view-rounds-its-own-sums", "get_city.ok.starving", false),
    ("city-view-rounds-its-own-sums", "get_cities.ok[*].yields.food", false),
    ("city-view-rounds-its-own-sums", "get_cities.ok[*].turns_to_grow", false),
    ("city-view-rounds-its-own-sums", "get_cities.ok[*].starving", false),
    ("city-view-rounds-its-own-sums", "god_view.cities[*].yields.food", false),
    ("city-view-rounds-its-own-sums", "god_view.cities[*].turns_to_grow", false),
    ("city-view-rounds-its-own-sums", "god_view.cities[*].starving", false),
    // The civilizations at war, by player id rather than in the order they were met.
    ("empire-summary-wars-by-id", "empire_summary.at_war_with[*]", false),
];

/// Lists compared as sets, and lists keyed by an id.
fn spec() -> CompareSpec {
    CompareSpec::new(Group::Views)
        .multiset("get_unit.ok.promotions")
        .multiset("get_units.ok[*].promotions")
        .multiset("get_tile.ok.units[*].promotions")
        .multiset("get_tile.ok.features")
        .multiset("get_city.ok.buildings")
        .multiset("get_city.ok.worked_tiles")
        .multiset("get_city.ok.locked_tiles")
        .multiset("get_cities.ok[*].buildings")
        .multiset("get_cities.ok[*].worked_tiles")
        .multiset("get_cities.ok[*].locked_tiles")
        .multiset("get_empire.ok.policies")
        .multiset("get_empire.ok.happiness.luxury_types")
        .multiset("get_policies.ok.adopted")
        .keyed("get_unit.ok.build_options", "id")
        .multiset("get_religion.ok.your_beliefs")
        .multiset("get_religion.ok.world_religions[*].beliefs")
        .multiset("get_diplomacy.ok.messages[*].to")
        .multiset("get_diplomacy.ok.players[*].trade_options.*.*")
        .multiset("get_diplomacy.ok.players[*].trade_options.agreements_possible_now")
        .multiset("empire_summary.policies")
        .multiset("empire_summary.happiness.luxury_types")
        .keyed("god_view.units", "id")
        .multiset("god_view.units[*].promotions")
        .keyed("god_view.cities", "id")
        .multiset("god_view.cities[*].buildings")
        .multiset("god_view.cities[*].worked_tiles")
        .multiset("god_view.cities[*].locked_tiles")
        .multiset("god_view.tiles[*][2]")
        .multiset("god_view.empires.*.policies")
        .multiset("god_view.empires.*.happiness.luxury_types")
        .keyed("god_view.players", "id")
        .keyed("god_view.negotiations", "id")
        .multiset("god_view.messages[*].to")
}

/// Every intended id of both lists.
fn intended_ids() -> Vec<String> {
    let mut ids = Vec::new();
    for file in ["refcheck/intended.toml", "tests/rules/intended.toml"] {
        let text = std::fs::read_to_string(root().join(file)).expect("an intended list");
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("id = \"") {
                ids.push(rest.trim_end_matches('"').to_owned());
            }
        }
    }
    ids
}

/// The recording, and where its states' fixtures are.
fn recording() -> (Value, Vec<PathBuf>) {
    let (file, dirs) = match std::env::var_os("CITAR_QUERY_TOOLS") {
        Some(f) => {
            let corpus = std::env::var_os("CITAR_REFCHECK_CORPUS")
                .expect("CITAR_QUERY_TOOLS needs CITAR_REFCHECK_CORPUS, the corpus it records");
            (PathBuf::from(f), vec![PathBuf::from(corpus)])
        }
        None => (
            root().join("refcheck/query_tools.json.gz"),
            vec![root().join("refcheck/fixtures-mini"), root().join("refcheck/fixtures-late")],
        ),
    };
    let bytes = gunzip(&file);
    (serde_json::from_slice(&bytes).expect("the recording is JSON"), dirs)
}

fn gunzip(path: &Path) -> Vec<u8> {
    use std::io::Read;
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(f).read_to_end(&mut out).expect("gzip");
    out
}

/// A state's game, loaded as refcheck loads it.
fn load(dirs: &[PathBuf], name: &str) -> Game {
    let (case, turn) = name.split_once('/').expect("<case>/t<turn>");
    let path = dirs
        .iter()
        .map(|d| d.join(case).join(format!("{turn}.json.gz")))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no fixture for {name}"));
    let doc: Value = serde_json::from_slice(&gunzip(&path)).expect("a fixture");
    let state = serde_json::to_vec(&doc["state"]).expect("a state");
    Game::from_python(Ruleset::shared(), &state).expect("the state loads").0
}

fn pid(v: &Value) -> PlayerId {
    PlayerId(v.as_u64().and_then(|n| u8::try_from(n).ok()).expect("a player id"))
}

/// Python's answer and Rust's to every recorded question of one state, each under its tool's
/// name.
fn answers(g: &Game, rec: &Value) -> Vec<(String, Value, Value)> {
    let mut out = Vec::new();
    for call in rec["calls"].as_array().expect("calls") {
        let tool = call["tool"].as_str().expect("a tool");
        let rust = match g.execute_query(pid(&call["pid"]), tool, &call["args"]) {
            Ok(v) => json!({ "ok": v }),
            Err(e) => json!({ "error": e.message }),
        };
        let python = match (call.get("ok"), call.get("error")) {
            (Some(v), _) => json!({ "ok": v }),
            (_, Some(e)) => json!({ "error": e }),
            _ => panic!("a call with no answer"),
        };
        let label = format!("{tool} {} {}", call["pid"], call["args"]);
        out.push((label, json!({ tool: python }), json!({ tool: rust })));
    }
    let mut god = serde_json::to_value(g.client_view(None, 150)).expect("a view");
    if let Some(m) = god.as_object_mut() {
        m.shift_remove("events");
        if let Some(Value::Array(tiles)) = m.get_mut("tiles") {
            *tiles = tiles.iter().step_by(25).cloned().collect();
        }
    }
    out.push(("god_view".into(), json!({"god_view": rec["god_view"]}), json!({ "god_view": god })));
    let facade = &rec["facade"];
    for (p, python) in facade["empire_summary"].as_object().expect("summaries") {
        let rust = g.empire_summary(PlayerId(p.parse().expect("an id"))).map(|s| s.to_json());
        out.push((
            format!("empire_summary {p}"),
            json!({ "empire_summary": python }),
            json!({ "empire_summary": rust }),
        ));
    }
    let standings: BTreeMap<String, Value> = g
        .standings()
        .iter()
        .map(|s| (s.player.0.to_string(), serde_json::to_value(s).expect("a standing")))
        .collect();
    out.push((
        "standings".into(),
        json!({ "standings": facade["standings"] }),
        json!({ "standings": standings }),
    ));
    for pp in facade["path_preview"].as_array().expect("paths") {
        let unit = UnitId::new(u32::try_from(pp["unit"].as_u64().expect("a unit")).expect("an id"))
            .expect("a unit id");
        let x = i32::try_from(pp["x"].as_i64().expect("x")).expect("x");
        let y = i32::try_from(pp["y"].as_i64().expect("y")).expect("y");
        let rust =
            serde_json::to_value(g.path_preview(pid(&pp["pid"]), unit, x, y)).expect("a path");
        out.push((
            format!("path_preview {pp}"),
            json!({ "path_preview": pp["answer"] }),
            json!({ "path_preview": rust }),
        ));
    }
    out
}

#[test]
fn the_query_tools_answer_as_python_s_did() {
    let (rec, dirs) = recording();
    let known = intended_ids();
    let explained: Vec<(&str, Pattern, bool)> = EXPLAINED
        .iter()
        .map(|&(id, p, committed)| {
            assert!(known.iter().any(|k| k == id), "{id} is in neither intended list");
            (id, Pattern::parse(p).expect("a pattern"), committed)
        })
        .collect();
    let spec = spec();
    let mut used = vec![false; explained.len()];
    let mut unexplained = Vec::new();
    let mut compared = 0usize;
    for (name, state) in rec.as_object().expect("states by name") {
        let g = load(&dirs, name);
        let before = (g.digest().expect("a digest"), g.rev());
        for (label, python, rust) in answers(&g, state) {
            compared += 1;
            for d in compare::compare(&spec, &python, &rust, &Options::default()) {
                match explained.iter().position(|(_, p, _)| p.matches(&d.path)) {
                    Some(i) => used[i] = true,
                    None => unexplained.push(format!(
                        "{name} {label}: {} {:?} python {} rust {}",
                        d.path,
                        d.kind,
                        d.python.map_or_else(|| "-".into(), |v| trim(&v.to_string())),
                        d.rust.map_or_else(|| "-".into(), |v| trim(&v.to_string())),
                    )),
                }
            }
        }
        assert_eq!((g.digest().expect("a digest"), g.rev()), before, "{name}: a query changed it");
    }
    assert!(compared > 1000, "only {compared} answers compared");
    let shown: Vec<&String> = unexplained.iter().take(40).collect();
    assert!(
        unexplained.is_empty(),
        "{} unexplained differences, the first:\n{}\nby place:\n{}",
        unexplained.len(),
        shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"),
        by_place(&unexplained)
    );
    if std::env::var_os("CITAR_QUERY_TOOLS").is_none() {
        let stale: Vec<String> = explained
            .iter()
            .zip(&used)
            .filter(|((_, _, committed), u)| *committed && !**u)
            .map(|((id, p, _), _)| format!("{id} at {p}"))
            .collect();
        assert!(stale.is_empty(), "explanations that explain nothing: {stale:?}");
    }
}

fn trim(s: &str) -> String {
    if s.chars().count() > 160 {
        format!("{}...", s.chars().take(160).collect::<String>())
    } else {
        s.to_owned()
    }
}

/// How many differences there are at each place, list positions and ids blurred, most first.
fn by_place(lines: &[String]) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for l in lines {
        let place = l.split(": ").nth(1).unwrap_or("").split(' ').next().unwrap_or("");
        let mut blurred = String::new();
        let mut in_brackets = false;
        for ch in place.chars() {
            match ch {
                '[' => {
                    in_brackets = true;
                    blurred.push_str("[*]");
                }
                ']' => in_brackets = false,
                c if !in_brackets => blurred.push(c),
                _ => {}
            }
        }
        *counts.entry(blurred).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.iter().map(|(p, n)| format!("{n:6} {p}")).collect::<Vec<_>>().join("\n")
}

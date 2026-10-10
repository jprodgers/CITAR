//! The query tools and the facade's other views against the Python engine's answers
//! (packages 1d-02 and 1d-03): `refcheck/query_tools.json.gz`, recorded by
//! `scripts/refcheck/query_tools.py` on the committed fixtures.
//!
//! The refcheck group `views` compares what the browser receives; the query tools read more of
//! the views than that (a unit's and a city's detail, tiles, the tech tree, policies, religion,
//! great people, espionage, city-states, victory, the ASCII map and the rules), and the facade
//! adds a spectator's view, `empire_summary`, `standings`, `path_preview`, the game's terrain as
//! a map with its summary and what the editor says of it, and the scenario editor's overview and
//! seats. Each recorded call runs through
//! `Game::execute_query` on the fixture loaded as refcheck loads it, and each answer is compared
//! with refcheck's comparator; a difference must be one the engine makes on purpose, named by an
//! id of `refcheck/intended.toml` or `tests/rules/intended.toml` in [`EXPLAINED`]. A second pass
//! tells an int from a float, which the comparator takes as equal: each number must be of the
//! kind Python's was, but for [`KINDS_EXPLAINED`].
//!
//! Set `CITAR_QUERY_TOOLS` to a recording of a corpus (`query_tools.py --corpus <dir> --out
//! <file>`) and `CITAR_REFCHECK_CORPUS` to that corpus to check it instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use citar_engine::api::{maps, scenario};
use citar_engine::base::ids::{ImprovementId, PlayerId, UnitId};
use citar_engine::game::Game;
use citar_engine::mapgen::document;
use citar_engine::rules::Ruleset;
use citar_refcheck::Group;
use citar_refcheck::compare::{self, CompareSpec, Diff, DiffKind, Options, Pattern};
use serde_json::{Value, json};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// A difference the engine makes on purpose: the intended id that explains it, the path of the
/// difference under the tool's name (or `god_view`, `empire_summary`, `standings`,
/// `path_preview`), whether the committed recording shows it (an explanation it shows must be
/// used there, or it is stale; the others are the corpus's), and, where the id explains only
/// some of the differences at that path, which.
struct Why {
    id: &'static str,
    at: &'static str,
    committed: bool,
    only: Option<fn(&Diff) -> bool>,
}

const fn why(id: &'static str, at: &'static str, committed: bool) -> Why {
    Why { id, at, committed, only: None }
}

/// A build option Rust offers where Python offered none, one that removes the tile's feature
/// first: an option Rust lost, or one it adds for another reason, stays unexplained.
fn offered_with_a_removal(d: &Diff) -> bool {
    d.kind == DiffKind::Extra && d.rust.as_ref().is_some_and(|v| v.get("first_removes").is_some())
}

/// A name Rust gives as `unknown` where Python gave it: in place (a text), or in a set of names
/// (Python's name missing, `unknown` extra).
fn named_unknown(d: &Diff) -> bool {
    let unknown = Some(&json!("unknown"));
    match d.kind {
        DiffKind::Text | DiffKind::Extra => d.rust.as_ref() == unknown,
        DiffKind::Missing => d.python.as_ref().is_some_and(Value::is_string),
        _ => false,
    }
}

/// Whether a text differs from Python's, line for line, only where `same` allows.
fn lines_differ_only(d: &Diff, same: fn(&str, &str) -> bool) -> bool {
    let (Some(Value::String(py)), Some(Value::String(rs))) = (&d.python, &d.rust) else {
        return false;
    };
    let (pl, rl): (Vec<&str>, Vec<&str>) = (py.lines().collect(), rs.lines().collect());
    pl.len() == rl.len() && pl.iter().zip(&rl).all(|(p, r)| p == r || same(p, r))
}

/// A unit at 0 health in Python's line, at 1 in Rust's.
fn zero_health(p: &str, r: &str) -> bool {
    p.strip_suffix(" hp 0").is_some_and(|head| r.strip_suffix(" hp 1") == Some(head))
}

/// A map's text whose only differences are units at 0 health in Python's, at 1 in Rust's.
fn zero_health_in_text(d: &Diff) -> bool {
    lines_differ_only(d, zero_health)
}

/// A map's text whose differences are those, and owners the reader has not met, whom Rust calls
/// an unknown civilization or city-state where Python named them.
fn unknown_owners_in_text(d: &Diff) -> bool {
    lines_differ_only(d, |p, r| {
        zero_health(p, r)
            || ["Unknown Civilization", "Unknown City-State"].iter().any(|u| {
                r.split_once(&format!("(owned by {u})")).is_some_and(|(head, tail)| {
                    p.starts_with(&format!("{head}(owned by ")) && p.ends_with(tail)
                })
            })
    })
}

/// A great improvement Python's list let a map carry, which Rust's rule leaves to a scenario: a
/// Farm or a Mine Rust lost stays unexplained.
fn great_improvement_dropped(d: &Diff) -> bool {
    let r = Ruleset::shared();
    let great = d
        .python
        .as_ref()
        .and_then(Value::as_str)
        .and_then(|name| r.lookup::<ImprovementId>(name))
        .is_some_and(|i| r.improvements()[i].great && !document::on_maps(r, i));
    great && d.rust.as_ref().is_some_and(Value::is_null)
}

/// An influence Rust lists at zero, where Python's city-state kept none for that civilization
/// (a zero, or a zero marked as a float by the kinds' pass).
fn zero_influence_listed(d: &Diff) -> bool {
    let zero = match &d.rust {
        Some(Value::String(s)) => s == "\u{1}0.0",
        Some(v) => v.as_f64() == Some(0.0),
        None => false,
    };
    d.kind == DiffKind::Extra && zero
}

/// The differences in value the engine makes on purpose.
const EXPLAINED: &[Why] = &[
    // Python matched the quests on a key its list never had, so it listed none.
    why("city-state-view-lists-its-quests", "get_city_states.ok[*].quests", false),
    why("city-state-view-lists-its-quests", "get_city_states.ok[*].quests[*]", false),
    // Python read each kind's points under its pool's name, so it showed none.
    why("great-people-view-shows-the-points", "get_great_people.ok.progress[*].points", true),
    // A civilian brought to 0 health, which Rust loads at 1.
    why("civilians-at-zero-health", "get_unit.ok.hp", false),
    why("civilians-at-zero-health", "get_units.ok[*].hp", false),
    why("civilians-at-zero-health", "get_tile.ok.units[*].hp", false),
    why("civilians-at-zero-health", "get_unit.ok.attack_targets[*].defender_hp", false),
    why("civilians-at-zero-health", "god_view.units[*].hp", false),
    Why {
        id: "civilians-at-zero-health",
        at: "get_map.ok",
        committed: true,
        only: Some(zero_health_in_text),
    },
    // Marble's bonus toward wonders, in its own city only: production, and the turns to build.
    why("marble-bonus-in-its-own-city", "get_city.ok.yields.production", true),
    why("marble-bonus-in-its-own-city", "get_city.ok.yield_breakdown.*.production", true),
    why("marble-bonus-in-its-own-city", "get_city.ok.queue[*].turns", true),
    why("marble-bonus-in-its-own-city", "get_city.ok.can_build.wonders[*].turns", true),
    why("marble-bonus-in-its-own-city", "get_cities.ok[*].yields.production", true),
    why("marble-bonus-in-its-own-city", "get_cities.ok[*].queue[*].turns", true),
    why("marble-bonus-in-its-own-city", "god_view.cities[*].yields.production", true),
    why("marble-bonus-in-its-own-city", "god_view.cities[*].queue[*].turns", true),
    // A farm, mine or plantation offered on a forest or jungle, the feature removed first.
    Why {
        id: "improvements-over-removable-features",
        at: "get_unit.ok.build_options[*]",
        committed: true,
        only: Some(offered_with_a_removal),
    },
    // A city's food total a hair from a tenth's half or from zero, rounded as it is.
    why("city-view-rounds-its-own-sums", "get_city.ok.yields.food", false),
    why("city-view-rounds-its-own-sums", "get_city.ok.turns_to_grow", false),
    why("city-view-rounds-its-own-sums", "get_city.ok.starving", false),
    why("city-view-rounds-its-own-sums", "get_cities.ok[*].yields.food", false),
    why("city-view-rounds-its-own-sums", "get_cities.ok[*].turns_to_grow", false),
    why("city-view-rounds-its-own-sums", "get_cities.ok[*].starving", false),
    why("city-view-rounds-its-own-sums", "god_view.cities[*].yields.food", false),
    why("city-view-rounds-its-own-sums", "god_view.cities[*].turns_to_grow", false),
    why("city-view-rounds-its-own-sums", "god_view.cities[*].starving", false),
    // The civilizations at war, by player id rather than in the order they were met.
    why("empire-summary-wars-by-id", "empire_summary.at_war_with[*]", false),
    // A city-state's ally, and a message's recipients, the caller has not met: unknown.
    Why {
        id: "views-hide-unmet-allies-and-recipients",
        at: "get_city_states.ok[*].ally",
        committed: false,
        only: Some(named_unknown),
    },
    Why {
        id: "views-hide-unmet-allies-and-recipients",
        at: "get_diplomacy.ok.messages[*].to[*]",
        committed: false,
        only: Some(named_unknown),
    },
    // The ASCII map names no owner the reader has not met.
    Why {
        id: "briefing-names-only-known-players",
        at: "get_map.ok",
        committed: false,
        only: Some(unknown_owners_in_text),
    },
    // A Citadel on a game's map, which a map may not carry.
    Why {
        id: "map-documents-read-by-rule",
        at: "maps.export.tiles[*][6]",
        committed: false,
        only: Some(great_improvement_dropped),
    },
    // The scenario editor's overview: each city-state's influence with every civilization.
    Why {
        id: "scenario-overview-lists-every-influence",
        at: "scenario.overview.players[*].influence.*",
        committed: true,
        only: Some(zero_influence_listed),
    },
];

/// The numbers Rust writes of another kind than Python's on purpose (a float where Python's was
/// an int), which [`the_query_tools_write_numbers_of_python_s_kind`] finds.
const KINDS_EXPLAINED: &[Why] = &[
    // Python's food stored was an int after some resets and a float otherwise.
    why("city-food-stored-is-a-float", "get_city.ok.food_stored", false),
    why("city-food-stored-is-a-float", "get_cities.ok[*].food_stored", false),
    why("city-food-stored-is-a-float", "god_view.cities[*].food_stored", true),
];

/// A list of explanations, with which of them explained something.
struct Explanations {
    rows: Vec<(&'static Why, Pattern)>,
    used: Vec<bool>,
}

impl Explanations {
    fn new(list: &'static [Why], known: &[String]) -> Self {
        let rows: Vec<(&Why, Pattern)> = list
            .iter()
            .map(|w| {
                assert!(known.iter().any(|k| k == w.id), "{} is in neither intended list", w.id);
                (w, Pattern::parse(w.at).expect("a pattern"))
            })
            .collect();
        let used = vec![false; rows.len()];
        Self { rows, used }
    }

    /// Whether an explanation explains `d`; the first that does is used.
    fn explain(&mut self, d: &Diff) -> bool {
        let found = self
            .rows
            .iter()
            .position(|(w, p)| p.matches(&d.path) && w.only.is_none_or(|only| only(d)));
        found.inspect(|&i| self.used[i] = true).is_some()
    }

    /// Fails on an explanation the committed recording shows that explained nothing, unless the
    /// recording is a corpus's.
    fn assert_none_stale(&self) {
        if std::env::var_os("CITAR_QUERY_TOOLS").is_some() {
            return;
        }
        let stale: Vec<String> = self
            .rows
            .iter()
            .zip(&self.used)
            .filter(|((w, _), u)| w.committed && !**u)
            .map(|((w, _), _)| format!("{} at {}", w.id, w.at))
            .collect();
        assert!(stale.is_empty(), "explanations that explain nothing: {stale:?}");
    }
}

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
        .multiset("scenario.overview.players[*].met")
        .multiset("scenario.overview.players[*].policies")
        .multiset("scenario.overview.cities[*].buildings")
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
    out.push(("maps".into(), json!({ "maps": rec["maps"] }), json!({ "maps": map_answers(g) })));
    out.push((
        "scenario".into(),
        json!({ "scenario": rec["scenario"] }),
        json!({ "scenario": scenario_answers(g, &rec["scenario"]) }),
    ));
    out
}

/// The game's terrain as a map, every 25th tile of it, its summary, what the editor says of it,
/// and whether it reads back as it was written (its id aside, which the editor makes from its
/// name).
fn map_answers(g: &Game) -> Value {
    let r = Ruleset::shared();
    let export = g.export_map("");
    let (clean, warnings) = maps::validate_map(r, &export).expect("the game's map reads");
    let mut same = export.clone();
    same["id"] = clean["id"].clone();
    let mut sampled = export.clone();
    if let Some(Value::Array(tiles)) = sampled.get_mut("tiles") {
        *tiles = tiles.iter().step_by(25).cloned().collect();
    }
    json!({
        "export": sampled,
        "summary": maps::map_summary(r, &export).expect("a summary"),
        "warnings": warnings,
        "round_trip": clean == same,
    })
}

/// The scenario editor's overview, the default seats, and the seat lists recorded, checked.
fn scenario_answers(g: &Game, rec: &Value) -> Value {
    let normalized: Vec<Value> = rec["normalize_seats"]
        .as_array()
        .expect("seat lists")
        .iter()
        .map(|n| match scenario::normalize_seats(g, Some(&n["seats"])) {
            Ok(v) => json!({"seats": n["seats"], "ok": v}),
            Err(e) => json!({"seats": n["seats"], "error": e.message}),
        })
        .collect();
    json!({
        "overview": scenario::overview(g),
        "default_seats": scenario::default_seats(g),
        "normalize_seats": normalized,
    })
}

#[test]
fn the_query_tools_answer_as_python_s_did() {
    let (rec, dirs) = recording();
    let mut explained = Explanations::new(EXPLAINED, &intended_ids());
    let spec = spec();
    let mut unexplained = Vec::new();
    let mut compared = 0usize;
    for (name, state) in rec.as_object().expect("states by name") {
        let g = load(&dirs, name);
        let before = (g.digest().expect("a digest"), g.rev());
        for (label, python, rust) in answers(&g, state) {
            compared += 1;
            for d in compare::compare(&spec, &python, &rust, &Options::default()) {
                if !explained.explain(&d) {
                    unexplained.push(line(name, &label, d));
                }
            }
        }
        assert_eq!((g.digest().expect("a digest"), g.rev()), before, "{name}: a query changed it");
    }
    assert!(compared > 1000, "only {compared} answers compared");
    assert!(
        unexplained.is_empty(),
        "{} unexplained differences, the first:\n{}\nby place:\n{}",
        unexplained.len(),
        unexplained.iter().take(40).map(String::as_str).collect::<Vec<_>>().join("\n"),
        by_place(&unexplained)
    );
    explained.assert_none_stale();
}

/// Python's `json.dumps` wrote an int as `4` and a float as `4.0`, and a model reads the text,
/// while refcheck's comparator takes the two as equal. Each whole float is made a marked text
/// here, so that compared again the two kinds differ: every number is of the kind Python's was,
/// but for [`KINDS_EXPLAINED`] (and where its value differs on purpose, [`EXPLAINED`]).
#[test]
fn the_query_tools_write_numbers_of_python_s_kind() {
    let (rec, dirs) = recording();
    let known = intended_ids();
    let mut values = Explanations::new(EXPLAINED, &known);
    let mut kinds = Explanations::new(KINDS_EXPLAINED, &known);
    let spec = spec();
    let mut wrong = Vec::new();
    for (name, state) in rec.as_object().expect("states by name") {
        let g = load(&dirs, name);
        for (label, python, rust) in answers(&g, state) {
            let (py, rs) = (kinds_marked(&python), kinds_marked(&rust));
            for d in compare::compare(&spec, &py, &rs, &Options::default()) {
                let of_kind = marked(d.python.as_ref()) || marked(d.rust.as_ref());
                if of_kind && !values.explain(&d) && !kinds.explain(&d) {
                    wrong.push(line(name, &label, d));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} numbers of another kind than Python's, the first:\n{}\nby place:\n{}",
        wrong.len(),
        wrong.iter().take(40).map(String::as_str).collect::<Vec<_>>().join("\n"),
        by_place(&wrong)
    );
    kinds.assert_none_stale();
}

/// A whole float as a marked text (`"\u{1}4.0"`), everything else as it is.
fn kinds_marked(v: &Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => match n.as_f64() {
            Some(x) if x.is_finite() && x.fract() == 0.0 => Value::String(format!("\u{1}{x}.0")),
            _ => v.clone(),
        },
        Value::Array(a) => Value::Array(a.iter().map(kinds_marked).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (k.clone(), kinds_marked(x))).collect())
        }
        _ => v.clone(),
    }
}

/// Whether a value holds a marked whole float.
fn marked(v: Option<&Value>) -> bool {
    match v {
        Some(Value::String(s)) => s.starts_with('\u{1}'),
        Some(Value::Array(a)) => a.iter().any(|x| marked(Some(x))),
        Some(Value::Object(m)) => m.values().any(|x| marked(Some(x))),
        _ => false,
    }
}

/// One difference as a line of the report, with the lines that differ of a long text.
fn line(name: &str, label: &str, d: Diff) -> String {
    let changed: Vec<&str> = d
        .detail
        .as_deref()
        .unwrap_or("")
        .lines()
        .filter(|l| (l.starts_with('-') || l.starts_with('+')) && !l.starts_with("---"))
        .filter(|l| !l.starts_with("+++"))
        .take(6)
        .collect();
    let detail = if changed.is_empty() {
        String::new()
    } else {
        format!("\n      {}", changed.join("\n      "))
    };
    format!(
        "{name} {label}: {} {:?} python {} rust {}{detail}",
        d.path,
        d.kind,
        d.python.map_or_else(|| "-".into(), |v| trim(&v.to_string())),
        d.rust.map_or_else(|| "-".into(), |v| trim(&v.to_string())),
    )
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

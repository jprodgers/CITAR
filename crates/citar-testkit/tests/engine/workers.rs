//! Workers, unit actions and automation (package 1c-04):
//! - worker job choices on the recorded fixtures, logged against the Python engine's
//!   (`data/worker_jobs.json`, from `scripts/refcheck/worker_jobs.py`) and against the direct
//!   port (each tile asked for the unit itself rather than the job map): every difference is an
//!   intended one (gate 4);
//! - a soak slice of `RandomAgent`s founding, building, automating, exploring and taking their
//!   units' actions, every check (the job maps' and danger maps' oracles among them) clean at every
//!   settle, and with `stats` the job maps' redundant recomputes under 5% (gate 3);
//! - the kitchen sink's extras of these systems: `[n]% construction time for [improvements]`
//!   (the Walker), `Pillaging this improvement yields [stats]`, `Destroyed when pillaged` and
//!   `Obsolete with [tech]` (the Outpost), `[n]% Health from pillaging tiles` and `[n]% Yield from
//!   pillaging tiles` (the Raider), `upon building a [Farm] improvement` (the nation), `Can
//!   generate a large amount of culture` and `Can speed up the construction of a wonder` (the
//!   Sage);
//! - an inquisitor removing heresy.

use std::collections::BTreeSet;

use citar_engine::api::{ActionError, testops, tools};
use citar_engine::base::ids::{BaseUnitId, CityId, ImprovementId, PlayerId, TileIdx, UnitId};
use citar_engine::game::automation::{best_job, worker_jobs, worker_jobs_with};
use citar_engine::game::derive::jobs;
use citar_engine::game::workers::{self, Builder, Problem};
use citar_engine::game::{
    Action, DebugOptions, DriveOptions, Drivers, Game, Stop, religion, units,
};
use citar_engine::rules::Ruleset;
use citar_testkit::agents::RandomAgent;
use citar_testkit::fixtures::{self, Fixture};
use citar_testkit::rulesets::kitchen_sink;
use citar_testkit::script::{map_doc, new_game};
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);
const YOU: PlayerId = PlayerId(1);

/// A bare game on the arena: the seats given, no unit, no city-state, no barbarian.
fn arena(r: &'static Ruleset, seats: &[Value], extra: &Value) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let mut cfg = json!({
        "seed": 1,
        "players": seats,
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
        "map": doc,
    });
    for (k, v) in extra.as_object().into_iter().flatten() {
        cfg[k] = v.clone();
    }
    let mut g = new_game(r, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("cleared");
    g
}

/// The kitchen sink's own nation against a benchmark civilization.
fn sink_game() -> Game {
    arena(
        kitchen_sink(),
        &[json!({"nation": "Kitchen Sink"}), json!({"nation": "BenchmarkCiv"})],
        &json!({}),
    )
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// A tool call as a host makes one.
fn tool(g: &mut Game, p: PlayerId, name: &str, args: &Value) -> Result<Value, ActionError> {
    let mut fields = tools::normalize(name, args)?;
    fields.insert("tool".into(), json!(name));
    let action: Action = serde_json::from_value(Value::Object(fields)).expect("the arguments fit");
    g.act(p, action).map(|(out, _)| out)
}

/// Scenario or test operations, which must succeed.
fn ops(g: &mut Game, list: &Value) -> Vec<Value> {
    g.apply_ops(list).map(|(out, _)| out).unwrap_or_else(|e| panic!("{list}: {e}"))
}

fn test_ops(g: &mut Game, list: &Value) -> Vec<Value> {
    testops::apply(g, list).map(|(out, _)| out).unwrap_or_else(|e| panic!("{list}: {e}"))
}

fn first_unit(out: &Value) -> UnitId {
    out["unit_ids"][0]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(UnitId::new)
        .unwrap_or_else(|| panic!("no unit in {out}"))
}

fn founded(out: &Value) -> CityId {
    out["city_id"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(CityId::new)
        .unwrap_or_else(|| panic!("no city in {out}"))
}

fn tile(g: &Game, x: i32, y: i32) -> TileIdx {
    g.grid().idx(x, y).expect("on the map")
}

fn imp(g: &Game, name: &str) -> ImprovementId {
    g.rules().lookup::<ImprovementId>(name).expect("an improvement")
}

// ---- Gate 4: worker jobs on the fixtures ----------------------------------------------------------

/// What Python recorded (`scripts/refcheck/worker_jobs.py`).
const WORKER_JOBS: &str = include_str!("../../data/worker_jobs.json");

fn load(f: &Fixture) -> Game {
    let bytes = fixtures::read_state(f).unwrap_or_else(|e| panic!("{e}"));
    let (g, _) = Game::from_python(Ruleset::shared(), &bytes)
        .unwrap_or_else(|e| panic!("{} does not load: {e}", f.name));
    g
}

/// Why the job on a tile may differ from Python's, if an intended difference explains it: an
/// improvement over a feature its civilization can now clear first
/// (`improvements-over-removable-features`), or fallout to clear (`fallout-removal-is-a-job`).
fn why_differs(g: &Game, b: &Builder, t: TileIdx) -> Option<&'static str> {
    let fallout = g.rules().derived().known.fallout;
    let removal = g.rules().derived().removal_of.get(fallout).copied().flatten();
    let opts = workers::build_options(g, b, t, None);
    match best_job(g, b, t, None) {
        Some((i, _)) if Some(i) == removal => Some("fallout-removal-is-a-job"),
        Some((i, _)) if opts.iter().any(|o| o.imp == i && o.first_removes.is_some()) => {
            Some("improvements-over-removable-features")
        }
        _ => None,
    }
}

#[test]
fn worker_jobs_on_the_fixtures_are_pythons_but_for_the_intended_differences() {
    let recorded: Value = serde_json::from_str(WORKER_JOBS).expect("worker_jobs.json is JSON");
    let states = fixtures::committed().unwrap_or_else(|e| panic!("{e}"));
    let (workers_seen, tiles_seen) = compare_jobs(&states, &recorded, "the committed fixtures");
    assert!(workers_seen > 50 && tiles_seen > 500, "the fixtures hold workers");
}

/// The environment variable naming Python's recording of the corpus
/// (`scripts/refcheck/worker_jobs.py --corpus <folder> --out <file>`), which the corpus
/// comparison reads beside `CITAR_REFCHECK_CORPUS`.
const CORPUS_JOBS_ENV: &str = "CITAR_WORKER_JOBS_CORPUS";

#[test]
fn worker_jobs_on_the_corpus_are_pythons_but_for_the_intended_differences() {
    let Some(states) = fixtures::corpus().unwrap_or_else(|e| panic!("{e}")) else { return };
    #[allow(clippy::disallowed_methods, reason = "a test's switch, not a game's input")]
    let Some(path) = std::env::var_os(CORPUS_JOBS_ENV) else { return };
    #[allow(clippy::disallowed_methods, reason = "the recording is a file")]
    let text = std::fs::read_to_string(&path).expect("the corpus recording");
    let recorded: Value = serde_json::from_str(&text).expect("JSON");
    compare_jobs(&states, &recorded, "the corpus");
}

/// Compares the job maps and the direct port with what Python recorded on `states`, prints the
/// intended differences, and fails on any other. How many workers and tiles it compared.
#[allow(clippy::disallowed_macros, reason = "the intended differences, for --no-capture")]
fn compare_jobs(states: &[Fixture], recorded: &Value, what: &str) -> (usize, usize) {
    let recorded = recorded.as_object().expect("an object");
    let mut log: Vec<String> = Vec::new();
    let mut unexplained: Vec<String> = Vec::new();
    let (mut workers_seen, mut tiles_seen) = (0, 0);
    for f in states {
        let Some(rec) = recorded.get(&f.name) else { panic!("{} was not recorded", f.name) };
        let g = load(f);
        let r = g.rules();
        let name_of = |i: ImprovementId| r.name(i).unwrap_or("").to_owned();
        // Each tile's job on the civilization's map, against Python's for the first worker of the
        // type, and against the direct port for that worker.
        for row in rec["tiles"].as_array().into_iter().flatten() {
            let owner = PlayerId(u8::try_from(row[0].as_u64().unwrap_or(0)).unwrap_or(0));
            let base: BaseUnitId = r.lookup(row[1].as_str().unwrap_or("")).expect("a unit type");
            let class = r.base_units()[base].builder.expect("a builder class");
            let worker = g
                .player_units(owner)
                .find(|u| u.base == base)
                .map(|u| u.id())
                .expect("a worker of the type");
            let direct = Builder::unit(&g, worker).expect("the worker");
            for t in row[2].as_array().into_iter().flatten() {
                tiles_seen += 1;
                let at = TileIdx(u32::try_from(t[0].as_u64().unwrap_or(0)).unwrap_or(0));
                let want = t[1].as_str().map(|n| (n.to_owned(), t[2].as_f64().unwrap_or(0.0)));
                let got = jobs::job(&g, owner, class, at).map(|(i, v)| (name_of(i), v));
                let port = best_job(&g, &direct, at, None).map(|(i, v)| (name_of(i), v));
                let same = |a: &Option<(String, f64)>, b: &Option<(String, f64)>| match (a, b) {
                    (Some((i, v)), Some((j, w))) => i == j && (v - w).abs() < 1e-6,
                    (None, None) => true,
                    _ => false,
                };
                if !same(&got, &port) {
                    unexplained.push(format!(
                        "{} tile {}: the job map says {got:?}, the direct port {port:?}",
                        f.name, at.0
                    ));
                }
                if !same(&got, &want) {
                    let line = format!(
                        "{} player {} tile {}: Rust {got:?}, Python {want:?}",
                        f.name, owner.0, at.0
                    );
                    match why_differs(&g, &direct, at) {
                        Some(id) => log.push(format!("{line} ({id})")),
                        None => unexplained.push(line),
                    }
                }
            }
        }
        // Each worker's choice, with no tile claimed.
        let claimed = BTreeSet::new();
        for w in rec["workers"].as_array().into_iter().flatten() {
            workers_seen += 1;
            let u = UnitId::new(u32::try_from(w[0].as_u64().unwrap_or(0)).unwrap_or(0))
                .expect("a unit id");
            let want = w[1].as_array().map(|j| {
                (
                    u32::try_from(j[0].as_u64().unwrap_or(0)).unwrap_or(0),
                    j[1].as_str().unwrap_or("").to_owned(),
                )
            });
            let got = worker_jobs(&g, u, &claimed).map(|(t, i)| (t.0, name_of(i)));
            let b = Builder::unit(&g, u).expect("the worker");
            let port = worker_jobs_with(&g, u, &claimed, |t| best_job(&g, &b, t, None))
                .map(|(t, i)| (t.0, name_of(i)));
            if got != port {
                unexplained.push(format!(
                    "{} unit {}: the job map chooses {got:?}, the direct port {port:?}",
                    f.name,
                    u.get()
                ));
            }
            if got != want {
                let line =
                    format!("{} unit {}: Rust chooses {got:?}, Python {want:?}", f.name, u.get());
                // A different choice follows from a tile whose job differs for an intended reason.
                let why = [got.as_ref(), want.as_ref()]
                    .into_iter()
                    .flatten()
                    .find_map(|(t, _)| why_differs(&g, &b, TileIdx(*t)));
                match why {
                    Some(id) => log.push(format!("{line} ({id})")),
                    None => unexplained.push(line),
                }
            }
        }
    }
    println!(
        "worker jobs: {workers_seen} workers and {tiles_seen} tiles on {what}; {} intended \
         differences",
        log.len()
    );
    for line in &log {
        println!("  {line}");
    }
    assert!(
        unexplained.is_empty(),
        "{} unexplained:\n{}",
        unexplained.len(),
        unexplained.join("\n")
    );
    (workers_seen, tiles_seen)
}

// ---- Gate 3 and the agents -------------------------------------------------------------------------

#[test]
#[allow(clippy::disallowed_macros, reason = "the run's counts, for --no-capture")]
fn random_agents_found_build_automate_and_act_for_eighty_turns_cleanly() {
    let r = Ruleset::shared();
    let seats: Vec<Value> = (0..3).map(|_| json!({"nation": "BenchmarkCiv"})).collect();
    let mut g = arena(r, &seats, &json!({"turn_limit": 80, "city_states": 1, "speed": "Quick"}));
    let starts = [(0u8, (5, 5)), (1, (18, 10)), (2, (18, 4))];
    let mut list = vec![
        json!({"op": "grant_tech", "player": "all", "techs": ["Mining", "Calendar", "The Wheel", "Bronze Working", "Animal Husbandry"]}),
    ];
    for &(p, (x, y)) in &starts {
        list.push(json!({"op": "found_city", "player": p, "x": x, "y": y, "claim_radius": 2}));
        list.push(
            json!({"op": "add_unit", "player": p, "unit": "Worker", "x": x, "y": y, "count": 3}),
        );
        for unit in ["Settler", "Warrior", "Scout"] {
            list.push(json!({"op": "add_unit", "player": p, "unit": unit, "x": x, "y": y}));
        }
        for unit in ["Great Prophet", "Great Scientist", "Great Artist", "Great Engineer"] {
            list.push(json!({"op": "add_unit", "player": p, "unit": unit, "x": x, "y": y}));
        }
        list.push(json!({"op": "set_player", "player": p, "gold": 200, "faith": 60}));
    }
    ops(&mut g, &Value::Array(list));
    // Every worker automated, each civilization in its turn.
    for &(p, _) in &starts {
        let pid = PlayerId(p);
        test_ops(&mut g, &json!([{"op": "force_turn", "player": p}]));
        let workers: Vec<UnitId> = g
            .player_units(pid)
            .filter(|u| g.rules().name(u.base) == Some("Worker"))
            .map(|u| u.id())
            .collect();
        for u in workers {
            tool(&mut g, pid, "unit_order", &json!({"unit_id": u.get(), "order": "automate"}))
                .expect("automated");
        }
    }
    let (mut a, mut b, mut c) = (RandomAgent::new(), RandomAgent::new(), RandomAgent::new());
    let mut d = Drivers::none(g.state().players().len())
        .with(PlayerId(0), &mut a)
        .with(PlayerId(1), &mut b)
        .with(PlayerId(2), &mut c);
    let (stop, batch) = g.drive(&mut d, DriveOptions::default()).expect("a live game");
    assert_eq!(stop, Stop::GameOver);
    clean(&mut g);
    let kinds = |k: &str| batch.events().iter().filter(|e| e.kind.name() == k).count();
    let mut all: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for e in batch.events() {
        *all.entry(e.kind.name()).or_default() += 1;
    }
    println!("events: {all:?}");
    let cities = g.state().cities().len();
    assert!(cities > 3, "the settlers founded cities: {cities}");
    assert!(kinds("improvement_built") > 3, "improvements: {}", kinds("improvement_built"));
    assert!(kinds("golden_age") >= 1, "a great artist's golden age: {all:?}");
    #[cfg(feature = "stats")]
    {
        let n = jobs::counts(&g);
        println!("job maps: {n:?}");
        assert!(n.recomputed > 0, "the maps recomputed tiles: {n:?}");
        #[allow(clippy::cast_precision_loss, reason = "counts of tiles")]
        let redundant = n.unchanged as f64 / n.recomputed as f64;
        assert!(redundant < 0.05, "{:.1}% of recomputes changed nothing: {n:?}", redundant * 100.0);
    }
}

// ---- The kitchen sink ------------------------------------------------------------------------------

#[test]
fn the_kitchen_sink_walker_builds_farms_in_half_the_time() {
    let mut g = sink_game();
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Kitchen Sink Walker", "x": 6, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Worker", "x": 6, "y": 5},
        ]),
    );
    let (walker, worker) = (first_unit(&out[1]), first_unit(&out[2]));
    let (t, farm, mine) = (tile(&g, 6, 5), imp(&g, "Farm"), imp(&g, "Mine"));
    let w = Builder::unit(&g, walker).expect("the walker");
    let k = Builder::unit(&g, worker).expect("the worker");
    let (walks, works) =
        (workers::turns_to_build(&g, &w, farm, t), workers::turns_to_build(&g, &k, farm, t));
    // `[-50]% construction time for [Farm] improvements`, rounded half up: 7 turns become 4.
    assert_eq!((walks, works), (4, 7));
    assert_eq!(workers::turns_to_build(&g, &w, mine, t), workers::turns_to_build(&g, &k, mine, t));
    clean(&mut g);
}

#[test]
fn the_kitchen_sink_raider_pillages_the_outpost_for_double_loot_and_destroys_it() {
    let mut g = sink_game();
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 1, "x": 18, "y": 10},
            {"op": "set_tile", "x": 17, "y": 10, "improvement": "Kitchen Sink Outpost"},
            {"op": "add_unit", "player": 0, "unit": "Kitchen Sink Raider", "x": 17, "y": 10, "hp": 50},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "set_relation", "a": 0, "b": 1, "state": "war"},
        ]),
    );
    let raider = first_unit(&out[2]);
    let gold = g.player(ME).map(|p| p.econ.gold).unwrap_or_default();
    let done =
        tool(&mut g, ME, "unit_order", &json!({"unit_id": raider.get(), "order": "pillage"}))
            .expect("it pillages");
    // `Pillaging this improvement yields [+20 Gold]`, doubled by `[+100]% Yield from pillaging
    // tiles`; 25 health raised half by `[+50]% Health from pillaging tiles`.
    assert_eq!(
        done,
        json!({"pillaged": "Kitchen Sink Outpost", "loot": {"gold": 40}, "healed": 37})
    );
    assert_eq!(g.player(ME).map(|p| p.econ.gold - gold), Some(40.0));
    assert_eq!(g.unit(raider).map(|u| u.hp), Some(87));
    // `Destroyed when pillaged`: nothing is left to repair.
    let t = g.tile(tile(&g, 17, 10)).copied().expect("a tile");
    assert_eq!((t.improvement(), t.improvement_pillaged()), (None, false));
    clean(&mut g);
}

#[test]
fn the_kitchen_sink_outpost_goes_obsolete_with_refrigeration() {
    let mut g = sink_game();
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "grant_tech", "player": 0, "tech": "Construction"},
            {"op": "add_unit", "player": 0, "unit": "Worker", "x": 6, "y": 5},
        ]),
    );
    let worker = first_unit(&out[2]);
    let (t, outpost) = (tile(&g, 6, 5), imp(&g, "Kitchen Sink Outpost"));
    let b = Builder::unit(&g, worker).expect("the worker");
    assert!(workers::building_problems(&g, &b, t, outpost, false, false).is_empty());
    ops(&mut g, &json!([{"op": "grant_tech", "player": 0, "tech": "Refrigeration"}]));
    let probs = workers::building_problems(&g, &b, t, outpost, false, false);
    assert_eq!(probs, [Problem::Obsolete]);
    let e = tool(
        &mut g,
        ME,
        "build_improvement",
        &json!({"unit_id": worker.get(), "improvement": "Kitchen Sink Outpost"}),
    )
    .expect_err("obsolete");
    assert_eq!(e.message, "Kitchen Sink Outpost is obsolete.");
    clean(&mut g);
}

#[test]
fn a_farm_built_pays_the_kitchen_sink_twenty_gold() {
    let mut g = sink_game();
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Worker", "x": 6, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Worker", "x": 4, "y": 5},
        ]),
    );
    let (worker, other) = (first_unit(&out[1]), first_unit(&out[2]));
    let order = tool(
        &mut g,
        ME,
        "build_improvement",
        &json!({"unit_id": worker.get(), "improvement": "Farm"}),
    )
    .expect("a farm");
    let gold = g.player(ME).map(|p| p.econ.gold).unwrap_or_default();
    let turns = order["turns"].as_i64().expect("turns");
    test_ops(&mut g, &json!([{"op": "progress_builds", "player": 0, "turns": turns}]));
    assert_eq!(g.tile(tile(&g, 6, 5)).and_then(|t| t.improvement()), Some(imp(&g, "Farm")));
    // `Gain [20] [Gold] <upon building a [Farm] improvement>`, once.
    assert_eq!(g.player(ME).map(|p| p.econ.gold - gold), Some(20.0));
    // A mine is not a farm: nothing more.
    ops(
        &mut g,
        &json!([{"op": "grant_tech", "player": 0, "tech": "Mining"}, {"op": "set_tile", "x": 4, "y": 5, "features": ["Hill"]}]),
    );
    let order = tool(
        &mut g,
        ME,
        "build_improvement",
        &json!({"unit_id": other.get(), "improvement": "Mine"}),
    )
    .expect("a mine");
    let gold = g.player(ME).map(|p| p.econ.gold).unwrap_or_default();
    let turns = order["turns"].as_i64().expect("turns");
    test_ops(&mut g, &json!([{"op": "progress_builds", "player": 0, "turns": turns}]));
    assert_eq!(g.tile(tile(&g, 4, 5)).and_then(|t| t.improvement()), Some(imp(&g, "Mine")));
    assert_eq!(g.player(ME).map(|p| p.econ.gold - gold), Some(0.0));
    clean(&mut g);
}

#[test]
fn the_kitchen_sink_sage_writes_a_treatise_and_hurries_only_a_wonder() {
    let mut g = sink_game();
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "grant_tech", "player": 0, "tech": "Calendar"},
            {"op": "add_unit", "player": 0, "unit": "Kitchen Sink Sage", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Kitchen Sink Sage", "x": 5, "y": 5},
        ]),
    );
    let c = founded(&out[0]);
    let (writer, builder) = (first_unit(&out[2]), first_unit(&out[3]));
    let ids = |g: &Game, u: UnitId| -> Vec<String> {
        citar_engine::game::actions::unit_actions(g, u).into_iter().map(|a| a.id).collect()
    };
    assert_eq!(ids(&g, writer), ["hurry_construction", "political_treatise"]);
    let culture = g.player(ME).map(|p| p.econ.culture).unwrap_or_default();
    let done = tool(
        &mut g,
        ME,
        "unit_action",
        &json!({"unit_id": writer.get(), "action": "political_treatise"}),
    )
    .expect("a treatise");
    let added = done["culture_added"].as_f64().expect("culture");
    assert_eq!(g.player(ME).map(|p| p.econ.culture - culture), Some(added));
    assert!(g.unit(writer).is_none(), "the sage is spent");
    // `Can speed up the construction of a wonder`: a monument is no wonder.
    tool(&mut g, ME, "set_production", &json!({"city_id": c.get(), "item": "Monument"}))
        .expect("a monument");
    let e = tool(
        &mut g,
        ME,
        "unit_action",
        &json!({"unit_id": builder.get(), "action": "hurry_construction"}),
    )
    .expect_err("not a wonder");
    assert_eq!(e.message, "This unit cannot hurry construction.");
    tool(&mut g, ME, "set_production", &json!({"city_id": c.get(), "item": "Stonehenge"}))
        .expect("a wonder");
    let done = tool(
        &mut g,
        ME,
        "unit_action",
        &json!({"unit_id": builder.get(), "action": "hurry_construction"}),
    )
    .expect("it hurries the wonder");
    assert_eq!(done["item"], "Stonehenge");
    assert!(done["production_added"].as_i64().is_some_and(|n| n > 0), "{done}");
    assert!(g.unit(builder).is_none());
    clean(&mut g);
}

#[test]
fn an_inquisitor_clears_the_other_religions_out_of_its_city() {
    let r = Ruleset::shared();
    let seats: Vec<Value> = (0..2).map(|_| json!({"nation": "BenchmarkCiv"})).collect();
    let mut g = arena(r, &seats, &json!({}));
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Antium"},
            {"op": "set_player", "player": "all", "faith": 25},
            {"op": "add_unit", "player": 0, "unit": "Great Prophet", "x": 5, "y": 5},
            {"op": "add_unit", "player": 1, "unit": "Great Prophet", "x": 18, "y": 10},
        ]),
    );
    let (roma, antium) = (founded(&out[0]), founded(&out[1]));
    let (p0, p1) = (first_unit(&out[3]), first_unit(&out[4]));
    tool(&mut g, ME, "found_pantheon", &json!({"belief": "God of the Sea"})).expect("a pantheon");
    tool(&mut g, ME, "unit_action", &json!({"unit_id": p0.get(), "action": "found_religion", "name": "Mine", "beliefs": ["Ceremonial Burial", "Feed the World"]}))
        .expect("a religion");
    test_ops(&mut g, &json!([{"op": "force_turn", "player": 1}]));
    tool(&mut g, YOU, "found_pantheon", &json!({"belief": "God of War"})).expect("a pantheon");
    tool(&mut g, YOU, "unit_action", &json!({"unit_id": p1.get(), "action": "found_religion", "name": "Yours", "beliefs": ["Church Property", "Asceticism"]}))
        .expect("a religion");
    test_ops(&mut g, &json!([{"op": "force_turn", "player": 0}]));
    let mine = g.player(ME).and_then(|p| p.religion.founded).expect("a religion");
    let yours = g.player(YOU).and_then(|p| p.religion.founded).expect("a religion");
    religion::add_pressure(&mut g, roma, Some(yours), 300);
    let inquisitor = r.lookup::<BaseUnitId>("Inquisitor").expect("a unit");
    let u = units::add_unit_in_city(&mut g, roma, inquisitor).expect("made in Roma");
    assert_eq!(g.unit(u).and_then(|x| x.religion), Some(mine), "Roma's religion");
    let e =
        tool(&mut g, ME, "unit_action", &json!({"unit_id": u.get(), "action": "spread_religion"}))
            .expect_err("an inquisitor does not spread");
    assert!(e.message.starts_with("Inquisitor has no action 'spread_religion'."), "{}", e.message);
    let done =
        tool(&mut g, ME, "unit_action", &json!({"unit_id": u.get(), "action": "remove_heresy"}))
            .expect("heresy removed");
    assert_eq!(done, json!({"removed_heresy": "Roma"}));
    let pressures = g.city(roma).map(|c| c.pressures.clone()).unwrap_or_default();
    assert!(pressures.iter().all(|&(k, _)| k.is_none() || k == Some(mine)), "{pressures:?}");
    assert!(g.unit(u).is_none(), "used once, then used up");
    assert!(g.city(antium).is_some());
    clean(&mut g);
}

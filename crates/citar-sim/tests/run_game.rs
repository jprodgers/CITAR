//! `run_game` against Python's (package 2-04, gates 2 and 3): the result's shape is the one
//! `scripts/bots/run_game_keys.py` recorded from `engine_api.run_game`, the same spec gives the
//! same result, the hooks hear every round and every event once, and a driver that panics is a
//! crash record, or the caller's error with `raise_errors`, while the process lives on.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::game::{DriverOutcome, EventBatch, Game, SeatDriver};
use citar_engine::rules::Ruleset;
use citar_engine::state::chronicle::Event;
use citar_engine::state::players::DriverMemory;
use citar_sim::{RoundInfo, RunResult, RunSpec, Runner, Seats, SimError, panics, run_game};
use serde_json::{Map, Value, json};

/// The file `scripts/bots/run_game_keys.py` wrote.
fn recorded() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/run_game_keys.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("JSON")
}

/// The shape of a JSON value, as `run_game_keys.py` writes it: `null`, `bool`, `number` (an int
/// or a float) or `string`; `{"list": [...]}` with the distinct shapes of the elements;
/// `{"by_id": [...]}` for an object keyed by ids, with the distinct shapes of its values;
/// `{"object": {...}}` for any other, its keys sorted.
fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(a) => json!({"list": distinct(a.iter().map(shape))}),
        Value::Object(o) => {
            let ids = !o.is_empty() && o.keys().all(|k| k.bytes().all(|b| b.is_ascii_digit()));
            if ids {
                json!({"by_id": distinct(o.values().map(shape))})
            } else {
                let sorted: BTreeMap<&String, Value> =
                    o.iter().map(|(k, x)| (k, shape(x))).collect();
                let m: Map<String, Value> =
                    sorted.into_iter().map(|(k, x)| (k.clone(), x)).collect();
                json!({"object": m})
            }
        }
    }
}

/// Shapes without repeats, sorted by their canonical text.
fn distinct(shapes: impl Iterator<Item = Value>) -> Vec<Value> {
    let by_text: BTreeMap<String, Value> = shapes.map(|s| (canonical(&s), s)).collect();
    by_text.into_values().collect()
}

/// Compact JSON with every object's keys sorted, as Python's `sort_keys=True` writes it.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let sorted: BTreeMap<&String, &Value> = o.iter().collect();
            let parts: Vec<String> = sorted
                .into_iter()
                .map(|(k, x)| format!("{}:{}", Value::String(k.clone()), canonical(x)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

fn bot(version: VersionId) -> Box<dyn SeatDriver> {
    let tuning = Arc::new(Tuning::new(version, Overrides::default()));
    Box::new(Bot::new(Arc::new(BotSpec::new(version, tuning, None, Some(0.4)))))
}

/// `basic-1` in every major's seat, as the recording put `basic` there.
fn basic_seats(g: &Game) -> Seats {
    g.majors(false).map(|p| (p.id(), bot(VersionId::Basic1))).collect()
}

/// What the hooks of one run heard.
#[derive(Debug, Default, PartialEq)]
struct Heard {
    rounds: Vec<RoundInfo>,
    events: Vec<Event>,
}

/// Plays `config` to its end with `basic-1` bots, through `run_game`.
fn play(config: &Value) -> (RunResult, Heard) {
    let spec = RunSpec { config: config.clone(), raise_errors: true, ..RunSpec::default() };
    let rules = Ruleset::shared();
    // The majors are the first players of a new game, one per seat of the configuration.
    let seats = config["players"].as_array().map_or(0, Vec::len);
    let drivers = (0..seats)
        .map(|p| (PlayerId(u8::try_from(p).expect("a seat")), bot(VersionId::Basic1)))
        .collect();
    let mut heard = Heard::default();
    let mut events = Vec::new();
    let r = run_game(rules, spec, drivers, &mut |i| heard.rounds.push(i.clone()), &mut |b| {
        events.extend(b.events().iter().cloned());
    })
    .expect("plays to its end");
    heard.events = events;
    (r, heard)
}

/// Gate 2: the result's keys and value types are exactly those Python's `run_game` gave on the
/// same duel (numbers as one class).
#[test]
fn the_result_has_the_keys_and_types_python_recorded() {
    let file = recorded();
    let (r, _) = play(&file["config"]);
    let got = shape(&serde_json::to_value(&r).expect("serialises"));
    let want = &file["shape"];
    assert_eq!(
        canonical(&got),
        canonical(want),
        "run_game's shape differs from run_game_keys.json:\nRust:   {}\nPython: {}",
        serde_json::to_string_pretty(&got).unwrap_or_default(),
        serde_json::to_string_pretty(want).unwrap_or_default()
    );
    // The duel ended on its limit, as Python's did.
    assert_eq!((r.phase, r.turns, r.turn_limit), ("over", 15, 15));
    assert_eq!(r.victory.as_deref(), Some("Time"));
}

/// Gate 2: the same spec twice gives the same result, the same rounds and the same events.
#[test]
fn the_same_spec_twice_gives_identical_results() {
    let config = recorded()["config"].clone();
    let (a, heard_a) = play(&config);
    let (b, heard_b) = play(&config);
    assert_eq!(a, b);
    assert_eq!(heard_a, heard_b);
    assert_eq!(
        serde_json::to_string(&a).expect("serialises"),
        serde_json::to_string(&b).expect("serialises")
    );
}

/// The round hook hears the first turn, each turn as it begins and the turn the game ended on;
/// the event hook every event after the game's creation, once and in order.
#[test]
fn the_hooks_hear_every_round_and_every_event_once() {
    let config = recorded()["config"].clone();
    let (r, heard) = play(&config);
    let turns: Vec<i32> = heard.rounds.iter().map(|i| i.turn).collect();
    assert_eq!(turns, (1..=16).collect::<Vec<_>>());
    assert!(heard.rounds[..15].iter().all(|i| i.phase == "playing"));
    assert_eq!(heard.rounds[15].phase, "over", "the game ended on turn 16");
    assert_eq!(heard.rounds[0].last_stats, None);
    assert_eq!(heard.rounds[15].last_stats.as_ref(), r.stats.as_array().and_then(|s| s.last()));
    assert!(heard.rounds.iter().all(|i| i.turn_limit == 15));

    // Against the game's own history, with the runner stepped by hand.
    let rules = Ruleset::shared();
    let spec = RunSpec { config, raise_errors: true, ..RunSpec::default() };
    let mut runner = Runner::new_with(rules, spec, basic_seats).expect("a game");
    let created = runner.game().chronicle().events().len();
    let mut stepped: Vec<Event> = Vec::new();
    while !runner.is_over() {
        stepped.extend(runner.step().expect("steps").events.into_events());
    }
    let history = &runner.game().chronicle().events()[created..];
    assert_eq!(stepped.as_slice(), history, "each event once, in order");
    assert_eq!(stepped, heard.events, "run_game delivers what the steps return");
    let ids: Vec<u32> = stepped.iter().map(|e| e.id.get()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "ascending ids");
}

/// A driver that panics when its seat's turn `on` comes: a bug on demand.
struct PanicsOn {
    on: i32,
}

impl SeatDriver for PanicsOn {
    fn play_turn(&mut self, g: &mut Game, _: PlayerId, _: &mut DriverMemory) -> DriverOutcome {
        assert!(g.turn() < self.on, "the test driver panics on turn {}", self.on);
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        _: &mut Game,
        _: PlayerId,
        _: NegotiationId,
        _: &mut DriverMemory,
    ) -> DriverOutcome {
        DriverOutcome::Done
    }
}

fn panicking_duel(raise_errors: bool) -> (RunSpec, Seats) {
    let mut spec = RunSpec {
        config: json!({"seed": 9, "map_size": "duel", "map_type": "continents", "turn_limit": 10,
                       "players": [{"controller": "bot"}, {"controller": "bot"}]}),
        raise_errors,
        ..RunSpec::default()
    };
    spec.labels.insert(PlayerId(1), "careless".to_owned());
    let drivers = vec![
        (PlayerId(0), bot(VersionId::Idle)),
        (PlayerId(1), Box::new(PanicsOn { on: 4 }) as Box<dyn SeatDriver>),
    ];
    (spec, drivers)
}

/// Gate 3: a panicking driver gives a crash record naming its seat's label and where it
/// happened, the game is poisoned and over, and the process plays on; with `raise_errors` the
/// caller gets the crash.
#[test]
fn a_panicking_driver_is_a_crash_record_or_the_callers_error_and_the_process_lives() {
    panics::install();
    let rules = Ruleset::shared();

    let (spec, drivers) = panicking_duel(false);
    let mut runner = Runner::new(rules, spec.clone(), drivers).expect("a game");
    while !runner.is_over() {
        runner.step().expect("a crash is recorded, not raised");
    }
    let poisoned = runner.game().poisoned().map(str::to_owned);
    assert!(poisoned.as_deref().is_some_and(|m| m.contains("panics on turn 4")), "{poisoned:?}");
    let crash = &runner.crashes()[0];
    assert_eq!(crash.to_string(), "T4 P1 careless: panic: the test driver panics on turn 4");
    assert!(crash.trace.starts_with("at ") && crash.trace.contains("run_game.rs"), "{crash:?}");
    let r = runner.result();
    assert_eq!(r.errors.len(), 1);
    assert!(
        r.errors[0].starts_with("T4 P1 careless: panic: the test driver panics on turn 4\nat ")
    );
    assert_eq!(r.phase, "playing", "stopped by the crash, as Python's error limit left it");

    // run_game records it the same way and returns the result.
    let (spec, drivers) = panicking_duel(false);
    let r = run_game(rules, spec, drivers, &mut |_| {}, &mut |_: &EventBatch| {}).expect("ends");
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.turn, 4);

    // With raise_errors, the caller gets it.
    let (spec, drivers) = panicking_duel(true);
    match run_game(rules, spec, drivers, &mut |_| {}, &mut |_: &EventBatch| {}) {
        Err(SimError::Crashed(c)) => {
            assert_eq!(c.label.as_deref(), Some("careless"));
            assert_eq!(c.player, Some(PlayerId(1)));
        }
        other => panic!("{other:?}"),
    }

    // The process lives on: another game plays to its end.
    let (r, _) = play(&recorded()["config"]);
    assert!(r.errors.is_empty() && r.phase == "over");
}

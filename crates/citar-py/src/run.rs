//! `run_game`: a whole headless game on citar-sim's runner (DESIGN.md P2.4.1, P2.6.1), the
//! facade's `engine_api.run_game`, which the lab, `citar sim` and the balance runner call.
//!
//! The runner steps the game one driven seat at a time. Each step runs with the GIL released;
//! between steps the binding takes the GIL back to hand the step's events to `on_event` and,
//! when the turn changed, the round to `on_turn`, as `headless.play` called them. A drive that
//! panics is the runner's crash record, `T<turn> P<player> <label>: panic: ...` with where it
//! happened, in the result's `errors`; with `raise_errors` it is raised as `EngineCrash`
//! instead. A Rust bot does not raise, so there is no `max_errors`: a crash ends the game.

use std::collections::BTreeMap;

use citar_bot::Bot as Driver;
use citar_engine::api::views::to_py_json;
use citar_engine::base::ids::PlayerId;
use citar_engine::base::py as pyish;
use citar_engine::game::SeatDriver;
use citar_engine::rules::Ruleset;
use citar_sim::{RunSpec, Runner, Seats, SimError, TRACEBACK_LIMIT};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use serde_json::Value;

use crate::Bytes;
use crate::bot::seat_bots;
use crate::calls::{self, detached};
use crate::errors::{Failure, caught, parse};

impl From<SimError> for Failure {
    fn from(e: SimError) -> Self {
        match e {
            SimError::Config(e) => e.into(),
            SimError::Refused(a) => a.into(),
            SimError::Crashed(c) => Self::Crash(c.record()),
            other => Self::Runtime(other.to_string()),
        }
    }
}

/// What a step hands back to the GIL: its events as JSON, and the round's info when the turn
/// changed (or the game ended on a new turn).
struct Delivery {
    events: Option<Vec<u8>>,
    round: Option<Vec<u8>>,
}

/// The run's labels, `{pid: text}` with the ids as JSON's text keys.
fn labels_of(v: Option<&Value>) -> Result<BTreeMap<PlayerId, String>, Failure> {
    let mut out = BTreeMap::new();
    let Some(Value::Object(m)) = v else {
        return match v {
            None | Some(Value::Null) => Ok(out),
            Some(_) => {
                Err(Failure::Value("labels must be an object of {player id: text}.".to_owned()))
            }
        };
    };
    for (k, text) in m {
        let p = k
            .parse::<u8>()
            .map_err(|_| Failure::Value(format!("labels: {k:?} is not a player id.")))?;
        out.insert(PlayerId(p), pyish::str_of(text));
    }
    Ok(out)
}

/// The seat and turn of the spec's `test_panic`, whose bot then panics on that turn or later:
/// the crash record's way through `run_game`, for the bindings' tests. Builds without the test
/// operations refuse it.
fn test_panic(v: Option<&Value>) -> Result<Option<(PlayerId, i32)>, Failure> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(None) };
    if !cfg!(feature = "test-ops") {
        return Err(Failure::Value(
            "test_panic needs a build with the test operations.".to_owned(),
        ));
    }
    let n = |k: &str| v.get(k).and_then(Value::as_i64);
    let (Some(p), Some(t)) = (n("player").and_then(|p| u8::try_from(p).ok()), n("turn")) else {
        return Err(Failure::Value("test_panic is {\"player\": id, \"turn\": n}.".to_owned()));
    };
    Ok(Some((PlayerId(p), i32::try_from(t).unwrap_or(i32::MAX))))
}

/// The driver of seat `p`: its bot, which panics from the turn `test_panic` names if it names
/// the seat.
#[cfg(feature = "test-ops")]
fn seat_driver(bot: Driver, p: PlayerId, panic_at: Option<(PlayerId, i32)>) -> Box<dyn SeatDriver> {
    match panic_at {
        Some((who, turn)) if who == p => Box::new(PanicAt { bot, turn }),
        _ => Box::new(bot),
    }
}

/// The driver of a seat: its bot (a build without the test operations refuses `test_panic`).
#[cfg(not(feature = "test-ops"))]
fn seat_driver(bot: Driver, _: PlayerId, _: Option<(PlayerId, i32)>) -> Box<dyn SeatDriver> {
    Box::new(bot)
}

/// A seat's bot that panics once the game reaches a turn (`test_panic`).
#[cfg(feature = "test-ops")]
struct PanicAt {
    bot: Driver,
    turn: i32,
}

#[cfg(feature = "test-ops")]
impl SeatDriver for PanicAt {
    fn play_turn(
        &mut self,
        g: &mut citar_engine::game::Game,
        pid: PlayerId,
        mem: &mut citar_engine::state::players::DriverMemory,
    ) -> citar_engine::game::DriverOutcome {
        assert!(
            g.turn() < self.turn,
            "the test bot panics on turn {}, as it was asked to",
            g.turn()
        );
        self.bot.play_turn(g, pid, mem)
    }

    fn respond(
        &mut self,
        g: &mut citar_engine::game::Game,
        pid: PlayerId,
        nid: citar_engine::base::ids::NegotiationId,
        mem: &mut citar_engine::state::players::DriverMemory,
    ) -> citar_engine::game::DriverOutcome {
        self.bot.respond(g, pid, nid, mem)
    }
}

/// Calls `hook` with each element of the JSON list `rows`, decoded by `loads` (Python's
/// `json.loads`).
fn deliver_each(loads: &Bound<'_, PyAny>, hook: &Bound<'_, PyAny>, rows: &[u8]) -> PyResult<()> {
    let list = loads.call1((PyBytes::new(loads.py(), rows),))?;
    for item in list.try_iter()? {
        hook.call1((item?,))?;
    }
    Ok(())
}

/// Calls `hook` with the JSON object `row`, decoded by `loads` (Python's `json.loads`).
fn deliver_one(loads: &Bound<'_, PyAny>, hook: &Bound<'_, PyAny>, row: &[u8]) -> PyResult<()> {
    hook.call1((loads.call1((PyBytes::new(loads.py(), row),))?,))?;
    Ok(())
}

/// Plays a whole headless game and returns how it went as JSON bytes (`engine_api.run_game`):
/// `{turn, turns, phase, winner, victory, turn_limit, stats, players, errors}`, the majors'
/// rows with `difficulty`, `techs`, `future_techs`, `policies`, `religion`, `great_people`,
/// `cities`, `spaceship` and `score`.
///
/// `spec_json`: `config` (the game's settings, as for `Game.new`), `labels` (`{pid: text}`,
/// added to a crash line after the player), `raise_errors` (raise the first crash as
/// `EngineCrash` instead of recording it), `traceback_limit` (frames kept per crash, 5);
/// `max_errors` is read and ignored. `bots`: `{pid: Bot}`; a major with none passes its turns.
/// `on_turn(info)` hears the first turn, each turn as it begins and the turn the game ended on
/// if it ended on a new one, with `{turn, phase, turn_limit, last_stats}`; `on_event(event)`
/// hears every event after the game's creation, each step's before that step's `on_turn`.
#[pyfunction]
#[pyo3(signature = (spec_json, bots, on_turn = None, on_event = None))]
pub fn run_game(
    py: Python<'_>,
    spec_json: &[u8],
    bots: &Bound<'_, PyDict>,
    on_turn: Option<Bound<'_, PyAny>>,
    on_event: Option<Bound<'_, PyAny>>,
) -> PyResult<Bytes> {
    let spec = parse(spec_json, "The run's spec")?;
    let config = spec
        .get("config")
        .filter(|c| !c.is_null())
        .cloned()
        .ok_or_else(|| Failure::Value("The run's spec needs the game's config.".to_owned()))?;
    let traceback_limit = match spec.get("traceback_limit") {
        None | Some(Value::Null) => TRACEBACK_LIMIT,
        Some(n) => n
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| Failure::Value(format!("traceback_limit must be a count, not {n}.")))?,
    };
    let run = RunSpec {
        config,
        labels: labels_of(spec.get("labels"))?,
        raise_errors: spec.get("raise_errors").is_some_and(pyish::truthy),
        traceback_limit,
        ..RunSpec::default()
    };
    let panic_at = test_panic(spec.get("test_panic"))?;
    let mut seats: Seats = Vec::new();
    for (pid, s) in seat_bots(bots)? {
        let p = u8::try_from(pid)
            .map_err(|_| Failure::Value(format!("bots: {pid} is not a player id.")))?;
        seats.push((PlayerId(p), seat_driver(Driver::new(s), PlayerId(p), panic_at)));
    }
    let wanted: Vec<PlayerId> = seats.iter().map(|(p, _)| *p).collect();
    let mut runner = detached(py, || {
        caught(|| Runner::new(Ruleset::shared(), run, seats).map_err(Failure::from))
            .map_err(Failure::Crash)?
    })?;
    if let Some(p) =
        wanted.iter().find(|&&p| !runner.game().player(p).is_some_and(|x| x.is_major()))
    {
        return Err(Failure::Value(format!(
            "bots: player {} is no major civilization of the game.",
            p.0
        ))
        .into());
    }
    let loads = py.import("json")?.getattr("loads")?;
    if let Some(hook) = &on_turn {
        let first = to_py_json(&runner.round_info());
        // The hooks run Python from inside this call: counted, so an exiting interpreter waits
        // for them (crate::calls).
        let _held = calls::hold(py);
        deliver_one(&loads, hook, &first)?;
    }
    let mut shown = runner.game().turn();
    let (want_events, want_rounds) = (on_event.is_some(), on_turn.is_some());
    while !runner.is_over() {
        // The runner catches a drive's panic as a crash record; this catches the rest.
        let delivery = detached(py, || {
            caught(|| {
                let step = runner.step()?;
                let g = runner.game();
                let events = (want_events && !step.events.is_empty()).then(|| {
                    let rows: Vec<Value> =
                        step.events.events().iter().map(|e| g.event_json(e, None)).collect();
                    to_py_json(&rows)
                });
                let round = (g.turn() != shown).then(|| {
                    shown = g.turn();
                    want_rounds.then(|| to_py_json(&runner.round_info()))
                });
                Ok::<_, SimError>(Delivery { events, round: round.flatten() })
            })
            .map_err(Failure::Crash)?
            .map_err(Failure::from)
        })?;
        if delivery.events.is_some() || delivery.round.is_some() {
            let _held = calls::hold(py);
            if let (Some(hook), Some(rows)) = (&on_event, &delivery.events) {
                deliver_each(&loads, hook, rows)?;
            }
            if let (Some(hook), Some(row)) = (&on_turn, &delivery.round) {
                deliver_one(&loads, hook, row)?;
            }
        }
    }
    // Moved in: a game is `Send`, never `Sync`.
    let result = detached(py, move || caught(|| Bytes(to_py_json(&runner.result()))));
    Ok(result.map_err(Failure::Crash)?)
}

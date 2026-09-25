//! Test operations: what a rule script does to a game that no player or editor may (DESIGN.md
//! 9.3). Feature `test-ops`; `citar/engine/testops.py` is the Python side, and
//! `tests/rules/README.md` documents each.
//!
//! They replace the internals Python's tests poked (`g.remove_unit`, `p.auto[...] = ...`,
//! `g.s.turn = ...`), so that a script means the same on both engines. Each is
//! `{"op": name, ...parameters}` and returns what it did; [`apply`] runs a list of them all or
//! nothing, as [`Game::apply_ops`] runs scenario operations.
//!
//! Package 1b-02 ports those that need no later system: `clear_units`, `set_turn`, `unmeet`,
//! `set_controller`, `set_auto`, `refresh_visibility` and `reload`; package 1b-03 the turn
//! operations `end_turn`, `end_round` and `force_turn`; package 1b-07 `complete_construction`. The others are listed with the package
//! that ports what they need, and are refused as not ported until then.

use serde_json::{Map, Value, json};

use super::scenario::{pid, players};
use crate::base::ids::{CityId, PlayerId, UnitId};
use crate::base::py;
use crate::game::cities::construction;
use crate::game::error::{ActionError, ErrCode};
use crate::game::events::EventBatch;
use crate::game::pending::SightSource;
use crate::game::{Game, Porting};
use crate::save::journal::JournalCursor;
use crate::state::cities::Constructible;
use crate::state::players::{AutoDecision, Controller, SeatOverrides};
use crate::state::{Phase, TurnClock};

/// A test operation's parameters.
pub type Params = Map<String, Value>;

type Run = fn(&mut Game, &Params) -> Result<Value, ActionError>;

/// One test operation.
#[derive(Clone, Copy, Debug)]
pub struct TestOp {
    /// Its name: `clear_units`.
    pub name: &'static str,
    /// Its parameters, as the script language documents them.
    pub params: &'static str,
    /// Whether what it needs is ported yet.
    pub porting: Porting,
    run: Run,
}

/// Every test operation, sorted by name.
pub static TEST_OPS: &[TestOp] = &[
    TestOp {
        name: "add_spy",
        params: "player: a new spy in the hideout",
        porting: Porting::Pending("1c-05"),
        run: add_spy,
    },
    TestOp {
        name: "attack_as",
        params: "unit, x, y: the unit attacks the tile, whoever's turn it is",
        porting: Porting::Pending("1c-03"),
        run: attack_as,
    },
    TestOp {
        name: "automate",
        params: "player: the player's automated units act now",
        porting: Porting::Pending("1c-04"),
        run: automate,
    },
    TestOp {
        name: "barbarian_act",
        params: "the barbarians take a turn now",
        porting: Porting::Pending("1c-06"),
        run: barbarian_act,
    },
    TestOp {
        name: "capture_civilian",
        params: "unit, x, y: the unit takes the civilian on the tile",
        porting: Porting::Pending("1c-03"),
        run: capture_civilian,
    },
    TestOp {
        name: "clear_units",
        params: "player (id, list of ids, or 'all' for every player, the barbarians included): \
                 every unit of those players is removed",
        porting: Porting::Ported,
        run: clear_units,
    },
    TestOp {
        name: "close_negotiation",
        params: "negotiation, status, note; optional by: closes it from outside",
        porting: Porting::Pending("1c-05"),
        run: close_negotiation,
    },
    TestOp {
        name: "complete_construction",
        params: "city: what it is building completes now",
        porting: Porting::Ported,
        run: complete_construction,
    },
    TestOp {
        name: "end_round",
        params: "every remaining turn of the round ends, and the round with them",
        porting: Porting::Ported,
        run: end_round,
    },
    TestOp {
        name: "end_turn",
        params: "optional player (the current one by default): that player's turn ends, and play \
                 moves on to the next major civilization's",
        porting: Porting::Ported,
        run: end_turn,
    },
    TestOp {
        name: "force_turn",
        params: "player: it is that player's turn now, started",
        porting: Porting::Ported,
        run: force_turn,
    },
    TestOp {
        name: "open_negotiation_as",
        params: "player, to, message; optional give, receive: opens a negotiation out of turn",
        porting: Porting::Pending("1c-05"),
        run: open_negotiation_as,
    },
    TestOp {
        name: "progress_builds",
        params: "turns: workers' builds advance by that many turns",
        porting: Porting::Pending("1c-04"),
        run: progress_builds,
    },
    TestOp {
        name: "ready_unit",
        params: "unit: full moves and no orders",
        porting: Porting::Pending("1c-02"),
        run: ready_unit,
    },
    TestOp {
        name: "refresh_visibility",
        params: "what every civilization sees is brought up to date",
        porting: Porting::Ported,
        run: refresh_visibility,
    },
    TestOp {
        name: "reload",
        params: "the game is saved and the save loaded, as a host does",
        porting: Porting::Ported,
        run: reload,
    },
    TestOp {
        name: "sack_city",
        params: "city: the barbarians sack it",
        porting: Porting::Pending("1c-06"),
        run: sack_city,
    },
    TestOp {
        name: "set_auto",
        params: "player, decision (un_vote, conquest or free_picks), on (bool): the engine takes \
                 the decision for the civilization, or not, until its controller changes",
        porting: Porting::Ported,
        run: set_auto,
    },
    TestOp {
        name: "set_controller",
        params: "player, controller; optional handicap, auto: hands the seat to another driver",
        porting: Porting::Ported,
        run: set_controller,
    },
    TestOp {
        name: "set_turn",
        params: "turn (1 or more): the game's turn number",
        porting: Porting::Ported,
        run: set_turn,
    },
    TestOp {
        name: "set_unit",
        params: "unit; any of hp, moves, xp, x, y, promotions",
        porting: Porting::Pending("1c-02"),
        run: set_unit,
    },
    TestOp {
        name: "unmeet",
        params: "a, b: the two no longer know each other",
        porting: Porting::Ported,
        run: unmeet,
    },
];

/// The test operation called `name`, exactly.
#[must_use]
pub fn op(name: &str) -> Option<&'static TestOp> {
    TEST_OPS.binary_search_by(|o| o.name.cmp(name)).ok().map(|i| &TEST_OPS[i])
}

/// Every test operation with its parameters, sorted by name.
#[must_use]
pub fn help() -> Value {
    Value::Array(TEST_OPS.iter().map(|o| json!({"op": o.name, "params": o.params})).collect())
}

/// The test operations not ported yet, with the package each waits for.
pub fn pending() -> impl Iterator<Item = (&'static str, &'static str)> {
    TEST_OPS.iter().filter_map(|o| match o.porting {
        Porting::Pending(pkg) => Some((o.name, pkg)),
        Porting::Ported => None,
    })
}

/// Runs test operations in order, all or nothing, then settles; returns what each did and the
/// events they appended. The error names the operation that failed, as `apply_ops`' does.
pub fn apply(g: &mut Game, ops: &Value) -> Result<(Vec<Value>, EventBatch), ActionError> {
    g.ensure_live()?;
    let before = g.clone();
    g.begin_call();
    match run_all(g, ops) {
        Ok(out) => {
            g.settle();
            Ok((out, g.take_batch()))
        }
        Err(e) => {
            *g = before;
            Err(e)
        }
    }
}

fn run_all(g: &mut Game, ops: &Value) -> Result<Vec<Value>, ActionError> {
    let list: &[Value] = match ops {
        Value::Array(a) => a,
        v if !py::truthy(v) => &[],
        _ => return Err(bad("The test operations must be a list of objects.")),
    };
    let mut out = Vec::with_capacity(list.len());
    for (i, o) in list.iter().enumerate() {
        let n = i + 1;
        let spec = o.get("op").and_then(Value::as_str).and_then(op);
        let (Value::Object(params), Some(spec)) = (o, spec) else {
            let what = py::repr(match o {
                Value::Object(m) => m.get("op").unwrap_or(&Value::Null),
                other => other,
            });
            let known: Vec<&str> = TEST_OPS.iter().map(|o| o.name).collect();
            return Err(bad(format!(
                "Test operation {n}: unknown op {what}. Known: {}.",
                known.join(", ")
            )));
        };
        let done = (spec.run)(g, params).map_err(|e| {
            ActionError::new(e.code, format!("Test operation {n} ({}): {}", spec.name, e.message))
        })?;
        out.push(done);
    }
    Ok(out)
}

fn bad(message: impl Into<String>) -> ActionError {
    ActionError::new(ErrCode::BadParam, message)
}

// ---- The ported ones --------------------------------------------------------------------------

/// Removes every unit of the players named, in id order.
fn clear_units(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let targets: Vec<PlayerId> = match o.get("player") {
        Some(Value::String(s)) if s == "all" => g.state().players().ids().collect(),
        v => players(g, v, false)?,
    };
    let mut removed: Vec<UnitId> = Vec::new();
    for p in targets {
        removed.extend(g.state().units().of(p).iter().copied());
    }
    removed.sort();
    for &u in &removed {
        if g.unit(u).is_some() {
            g.despawn_unit(u).map_err(|e| ActionError::rule(format!("The game refused ({e}).")))?;
        }
    }
    Ok(json!({"removed": removed.iter().map(|u| u.get()).collect::<Vec<_>>()}))
}

/// Sets the turn number.
fn set_turn(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let turn = o
        .get("turn")
        .and_then(py::int_of)
        .and_then(|n| i32::try_from(n).ok())
        .filter(|&n| n >= 1)
        .ok_or_else(|| bad("turn must be a whole number, 1 or more."))?;
    let clock = TurnClock { turn, ..*g.state().clock() };
    g.set_clock(clock);
    Ok(json!({"turn": turn}))
}

/// Two players forget they have met.
fn unmeet(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let a = pid(g, o.get("a"), false)?;
    let b = pid(g, o.get("b"), false)?;
    if a == b {
        return Err(bad("A player cannot forget itself."));
    }
    g.update_relation(a, b, |r| r.met = false)
        .map_err(|e| ActionError::rule(format!("The game refused ({e}).")))?;
    Ok(json!({}))
}

/// Hands a seat to another driver, with the host's own checks of the handicap and the automatic
/// decisions (`engine_api.set_controller`).
fn set_controller(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = pid(g, o.get("player"), false)?;
    let raw = o.get("controller").unwrap_or(&Value::Null);
    let controller = raw
        .as_str()
        .and_then(Controller::from_name)
        .ok_or_else(|| bad(format!("Unknown controller {}.", py::repr(raw))))?;
    let over = SeatOverrides::parse(o.get("handicap"), o.get("auto")).map_err(|e| bad(e.0))?;
    g.set_seat_controller(p, controller, over.handicap, over.auto)
        .map_err(|e| ActionError::rule(format!("The game refused ({e}).")))?;
    Ok(json!({}))
}

/// Changes one automatic decision of a seat for now, as play does: a new controller re-derives
/// it.
fn set_auto(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = pid(g, o.get("player"), false)?;
    let raw = o.get("decision").unwrap_or(&Value::Null);
    let d = raw
        .as_str()
        .and_then(AutoDecision::from_name)
        .ok_or_else(|| bad(format!("Unknown decision {}.", py::repr(raw))))?;
    let on = match o.get("on") {
        Some(Value::Bool(b)) => *b,
        _ => return Err(bad("on must be true or false.")),
    };
    g.set_auto_decision(p, d, on)
        .map_err(|e| ActionError::rule(format!("The game refused ({e}).")))?;
    Ok(json!({}))
}

/// Where the game is in time, as the turn operations report it.
fn clock(g: &Game) -> Value {
    json!({"turn": g.turn(), "current": g.current().0})
}

/// Ends a player's turn, the current one's unless another is named, as the host's `end_turn`
/// does (`Game.end_turn`): play moves on to the next major civilization.
fn end_turn(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = match o.get("player") {
        None | Some(Value::Null) => g.current(),
        v => pid(g, v, false)?,
    };
    g.end_turn_now(p)?;
    Ok(clock(g))
}

/// Ends every turn left in the round, and the round.
fn end_round(g: &mut Game, _: &Params) -> Result<Value, ActionError> {
    let start = g.turn();
    // Each call moves play on at least one player, and a round has at most 64.
    while g.phase() == Phase::Playing && g.turn() == start {
        g.end_turn_now(g.current())?;
    }
    Ok(clock(g))
}

/// Makes it a player's turn now and starts it (`EngineGame.force_turn`): refused, as the host's
/// is, for a player who has been eliminated and in a game that is over.
fn force_turn(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = pid(g, o.get("player"), false)?;
    g.force_turn_now(p)?;
    Ok(clock(g))
}

/// Finishes what a city builds now, whatever production it has stored, as its turn would
/// (`cities.complete_construction`).
fn complete_construction(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let c = py::int_of(o.get("city").unwrap_or(&Value::Null))
        .and_then(|n| u32::try_from(n).ok())
        .and_then(CityId::new)
        .filter(|&c| g.city(c).is_some())
        .ok_or_else(|| ActionError::new(ErrCode::NoSuchCity, "No such city."))?;
    let Some(city) = g.city(c) else { return Err(bad("No such city.")) };
    let Some(item) =
        city.queue.first().copied().filter(|x| !matches!(x, Constructible::Perpetual(_)))
    else {
        return Err(ActionError::rule(format!(
            "{} is building nothing that completes.",
            city.name
        )));
    };
    if !construction::complete_construction(g, c, item, None) {
        return Err(ActionError::rule("No room to place the unit."));
    }
    Ok(json!({"completed": construction::item_name(g.rules(), item)}))
}

/// Brings what every civilization sees up to date, as Python's `visibility.refresh(force=True)`
/// did: every player's sight is looked at again in the settle that follows.
fn refresh_visibility(g: &mut Game, _: &Params) -> Result<Value, ActionError> {
    let all: Vec<PlayerId> = g.state().players().ids().collect();
    for p in all {
        g.pending.flag_sight(SightSource::Civ(p));
    }
    Ok(json!({}))
}

/// Saves the game with its whole history, as one journal chunk, and loads it again: what a
/// host does between sessions.
fn reload(g: &mut Game, _: &Params) -> Result<Value, ActionError> {
    let failed = |what: &str, e: &dyn core::fmt::Display| {
        ActionError::rule(format!("The game did not {what} ({e})."))
    };
    // The whole history in one chunk: a copy whose journal starts afresh.
    let mut copy = g.clone();
    copy.journal = JournalCursor::default();
    copy.st.host_mut().journal_seq = 0;
    let chunk = copy.take_journal_chunk().map_err(|e| failed("save its history", &e))?;
    let state = copy.snapshot().to_json().map_err(|e| failed("save", &e))?;
    let chunks: Vec<Vec<u8>> = chunk.into_iter().map(|c| c.json).collect();
    let mut it = chunks.iter().map(Vec::as_slice);
    let (mut loaded, _report) =
        Game::load(g.rules(), &state, &mut it).map_err(|e| failed("load", &e))?;
    loaded.set_debug_options(g.debug_options());
    *g = loaded;
    Ok(json!({}))
}

// ---- Those whose systems are not ported yet ----------------------------------------------------

/// The refusal of a test operation whose system is not ported yet: `path` names the system, and
/// `cargo xtask check` counts the calls (DESIGN.md 3.4, rule 4).
fn not_ported(path: &str) -> ActionError {
    ActionError::new(
        ErrCode::NotPorted,
        format!("This test operation is not ported to the new engine yet ({path})."),
    )
}

fn add_spy(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::espionage"))
}

fn attack_as(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::combat"))
}

fn automate(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::automation"))
}

fn barbarian_act(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::barbarians"))
}

fn capture_civilian(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::combat"))
}

fn close_negotiation(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::diplomacy::negotiation"))
}

fn open_negotiation_as(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::diplomacy::negotiation"))
}

fn progress_builds(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::workers"))
}

fn ready_unit(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::units"))
}

fn sack_city(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::barbarians"))
}

fn set_unit(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::units"))
}

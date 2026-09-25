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
//! operations `end_turn`, `end_round` and `force_turn`; package 1b-07 `complete_construction`;
//! package 1b-08 `found_religion`, `enhance_religion` and `enter_ruins`, which stand in for the
//! unit actions and the moves of packages 1c-04 and 1c-02; package 1c-02 `set_unit` and
//! `ready_unit`; package 1c-03 `attack_as` and `capture_civilian`; package 1c-04 `automate` and
//! `progress_builds`; package 1c-05 `add_spy`, `close_negotiation` and `open_negotiation_as`;
//! package 1c-06 `add_barbarian`, `add_quest`, `barbarian_act`, `clear_camps` (which the bare
//! prelude of the rule scripts runs), `create_camp` and `sack_city`; package 1c-09 `debug` and
//! `set_difficulty`, the host's commands of those names, and `drive`, the host's drive with a
//! test driver at the seats named. Every test operation is ported.

use serde_json::{Map, Value, json};

use super::game::DebugAction;
use super::scenario::{given, pid, players, resolve, tile, whole};
use crate::base::ids::{CityId, DifficultyId, NegotiationId, PlayerId, PromotionId, UnitId};
use crate::base::py;
use crate::base::sets::PromotionSet;
use crate::game::cities::construction;
use crate::game::derive::rev::UnitTouch;
use crate::game::diplomacy::actions::RespondNegotiation;
use crate::game::error::{ActionError, ErrCode};
use crate::game::events::EventBatch;
use crate::game::pending::SightSource;
use crate::game::{Action, DriveOptions, DriverOutcome, Drivers, Game, Porting, SeatDriver, Stop};
use crate::save::journal::JournalCursor;
use crate::state::cities::Constructible;
use crate::state::players::{AutoDecision, Controller, DriverMemory, SeatOverrides};
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
        name: "add_barbarian",
        params: "unit, x, y (or at); optional hp: a barbarian unit on the tile, whatever the \
                 scenario operations allow; its id",
        porting: Porting::Ported,
        run: add_barbarian,
    },
    TestOp {
        name: "add_quest",
        params: "city_state, player, quest (its name); optional scope (individual or global, the \
                 row's by default), target (a player id; a resource, wonder, great person or \
                 natural wonder by name; a contest's starting score; the investment percent; for \
                 Spread Religion the player whose religion it is), x and y (or at: the camp to \
                 clear): the city-state gives the major the quest now, whether or not it fits; its \
                 text",
        porting: Porting::Ported,
        run: add_quest,
    },
    TestOp {
        name: "add_spy",
        params: "player: a new spy in the hideout",
        porting: Porting::Ported,
        run: add_spy,
    },
    TestOp {
        name: "attack_as",
        params: "unit, x, y: the unit attacks the tile, whoever's turn it is",
        porting: Porting::Ported,
        run: attack_as,
    },
    TestOp {
        name: "automate",
        params: "player: the player's units carry out their standing orders now (moves, \
                 exploring, automated workers, sleepers waking), as at the start of its turn",
        porting: Porting::Ported,
        run: automate,
    },
    TestOp {
        name: "barbarian_act",
        params: "optional unit: the barbarians take a turn now (their units start their turn and \
                 act, then their camps); with a unit, only that barbarian acts, with the moves it has",
        porting: Porting::Ported,
        run: barbarian_act,
    },
    TestOp {
        name: "capture_civilian",
        params: "unit (or player, the barbarians included), x, y: the unit, or the player, takes the \
                 civilian on the tile",
        porting: Porting::Ported,
        run: capture_civilian,
    },
    TestOp {
        name: "clear_camps",
        params: "every barbarian camp is removed, with its improvement",
        porting: Porting::Ported,
        run: clear_camps,
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
        porting: Porting::Ported,
        run: close_negotiation,
    },
    TestOp {
        name: "complete_construction",
        params: "city: what it is building completes now",
        porting: Porting::Ported,
        run: complete_construction,
    },
    TestOp {
        name: "create_camp",
        params: "x, y (or at): a barbarian camp on the tile; its id",
        porting: Porting::Ported,
        run: create_camp,
    },
    TestOp {
        name: "debug",
        params: "action (meet_all, reveal or gold): the host's developer shortcut",
        porting: Porting::Ported,
        run: debug,
    },
    TestOp {
        name: "drive",
        params: "drivers (the players a test driver plays: it does nothing with its turns); \
                 optional answer (what the driver answers a negotiation waiting on it: reject by \
                 default, accept, reply, or none to leave it), defer (drivers that leave what waits \
                 on them to the host, as a hybrid seat's bot leaves it to its model), seat_limit: \
                 the host drives the game until it has something to do; why it stopped",
        porting: Porting::Ported,
        run: drive,
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
        name: "enhance_religion",
        params: "unit, beliefs: the great prophet enhances its owner's religion where it stands, \
                 and is spent",
        porting: Porting::Ported,
        run: enhance_religion,
    },
    TestOp {
        name: "enter_ruins",
        params: "unit: the unit explores the ancient ruins it stands on",
        porting: Porting::Ported,
        run: enter_ruins,
    },
    TestOp {
        name: "force_turn",
        params: "player: it is that player's turn now, started",
        porting: Porting::Ported,
        run: force_turn,
    },
    TestOp {
        name: "found_religion",
        params: "unit, name, beliefs: the great prophet founds a religion where it stands, and is \
                 spent",
        porting: Porting::Ported,
        run: found_religion,
    },
    TestOp {
        name: "open_negotiation_as",
        params: "player, to, message; optional give, receive: opens a negotiation out of turn",
        porting: Porting::Ported,
        run: open_negotiation_as,
    },
    TestOp {
        name: "progress_builds",
        params: "player; optional turns (1 by default): the player's workers do that many turns \
                 of work, as at the end of its turns",
        porting: Porting::Ported,
        run: progress_builds,
    },
    TestOp {
        name: "ready_unit",
        params: "unit: full moves and no orders",
        porting: Porting::Ported,
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
        params: "city: the barbarians sack it; what they took",
        porting: Porting::Ported,
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
        name: "set_difficulty",
        params: "player, difficulty (a level's name): the seat's own difficulty, as the host sets \
                 it; ok is false, and nothing changes, for a name that is no level",
        porting: Porting::Ported,
        run: set_difficulty,
    },
    TestOp {
        name: "set_turn",
        params: "turn (1 or more): the game's turn number",
        porting: Porting::Ported,
        run: set_turn,
    },
    TestOp {
        name: "set_unit",
        params: "unit; any of hp, moves, xp, x and y, promotions (the list it then has), carrier \
                 (a unit on its tile that carries it, or null)",
        porting: Porting::Ported,
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

/// The unit a test operation names.
fn unit_of(g: &Game, o: &Params) -> Result<UnitId, ActionError> {
    o.get("unit")
        .and_then(py::int_of)
        .and_then(|n| u32::try_from(n).ok())
        .and_then(UnitId::new)
        .filter(|&u| g.unit(u).is_some())
        .ok_or_else(|| bad("No such unit."))
}

/// A list of names, as a script gives them.
fn names_of(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(py::str_of).collect(),
        _ => Vec::new(),
    }
}

/// A great prophet founds or enhances a religion where it stands and is spent, as the unit
/// actions `found_religion` and `enhance_religion` do (`game::actions`), which check besides
/// whether the unit may act now.
fn prophet_acts(
    g: &mut Game,
    u: UnitId,
    enhance: bool,
    name: &str,
    beliefs: &[String],
) -> Result<Value, ActionError> {
    use crate::game::great_people::consume_unit;
    use crate::game::religion::found;
    use crate::unique::UniqueType;
    let Some((p, at, base)) = g.unit(u).map(|x| (x.owner(), x.tile(), x.base)) else {
        return Err(ActionError::rule("No such unit."));
    };
    let ty = if enhance { UniqueType::MayEnhanceReligion } else { UniqueType::MayFoundReligion };
    if !construction::unit_has_type(g.rules(), base, ty) {
        return Err(ActionError::rule(if enhance {
            "This unit cannot enhance a religion."
        } else {
            "This unit cannot found a religion."
        }));
    }
    let spend = move |g: &mut Game| consume_unit(g, u);
    if enhance {
        let chosen = found::plan_enhance(g, p, at, beliefs)?;
        found::apply_enhance(g, p, &chosen, spend);
        Ok(found::enhance_result(g, p))
    } else {
        let plan = found::plan_religion(g, p, at, name, beliefs, None)?;
        found::apply_religion(g, p, &plan, spend);
        Ok(found::religion_result(g, p, &plan))
    }
}

/// A great prophet founds a religion where it stands (`religion.found_religion`), as its unit
/// action does.
fn found_religion(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let u = unit_of(g, o)?;
    let name = o.get("name").map(py::str_of).unwrap_or_default();
    prophet_acts(g, u, false, &name, &names_of(o.get("beliefs")))
}

/// A great prophet enhances its owner's religion where it stands (`religion.enhance_religion`).
fn enhance_religion(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let u = unit_of(g, o)?;
    prophet_acts(g, u, true, "", &names_of(o.get("beliefs")))
}

/// A unit explores the ancient ruins it stands on (`ruins.enter`), as moving onto them will
/// (package 1c-02).
fn enter_ruins(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let u = unit_of(g, o)?;
    let at = g.unit(u).map(crate::state::units::Unit::tile).ok_or_else(|| bad("No such unit."))?;
    let ruins = g.rules().derived().known.ancient_ruins;
    if ruins.is_none() || g.tile(at).and_then(crate::state::map::Tile::improvement) != ruins {
        return Err(ActionError::rule("There are no ancient ruins here."));
    }
    Ok(json!({"found": crate::game::ruins::enter(g, u, at)}))
}

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

/// A developer shortcut of the host's (`EngineGame.debug`): meet_all, reveal or gold.
fn debug(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let raw = o.get("action").unwrap_or(&Value::Null);
    let action = raw
        .as_str()
        .and_then(DebugAction::from_name)
        .ok_or_else(|| bad("Unknown debug action."))?;
    g.debug_now(action);
    Ok(json!({}))
}

/// The driver of the `drive` operation: it does nothing with its turns, and answers a
/// negotiation that waits on it with `answer`, or leaves it (`None`), or leaves it to the host
/// (`defer`, as a hybrid seat's bot leaves it to the seat's model).
struct TestDriver {
    answer: Option<&'static str>,
    defer: bool,
}

impl SeatDriver for TestDriver {
    fn play_turn(&mut self, _: &mut Game, _: PlayerId, _: &mut DriverMemory) -> DriverOutcome {
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        g: &mut Game,
        pid: PlayerId,
        nid: NegotiationId,
        _: &mut DriverMemory,
    ) -> DriverOutcome {
        if self.defer {
            return DriverOutcome::Deferred;
        }
        if let Some(action) = self.answer {
            let a = RespondNegotiation {
                negotiation_id: i64::from(nid.get()),
                action: json!(action),
                message: Some(json!("(test driver)")),
                give: None,
                receive: None,
            };
            // A refused answer leaves the negotiation as it was, which is an answer too.
            let _refused = g.act(pid, Action::RespondNegotiation(a));
        }
        DriverOutcome::Done
    }
}

/// The host drives the game (`Game::drive`), a test driver at each seat named (one that defers
/// what waits on it to the host for those under `defer`, as `citar/engine/testops.py` mirrors
/// it), until it stops:
/// `stop` (`external`, `hybrid_diplomat`, `awaiting_reply`, `seat_limit`, `game_over`), the
/// `player` it names (or null), the `negotiations` it waits on, and where the game is in time.
fn drive(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let named = |key: &str| -> Result<Vec<PlayerId>, ActionError> {
        match o.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            v => players(g, v, true),
        }
    };
    let seats = named("drivers")?;
    let deferring = named("defer")?;
    // As Python reads it: absent is reject, and anything but those four names is refused.
    let answer = match o.get("answer") {
        None => Some("reject"),
        Some(v) => match v.as_str() {
            Some("reject") => Some("reject"),
            Some("accept") => Some("accept"),
            Some("reply") => Some("reply"),
            Some("none") => None,
            _ => {
                return Err(bad(format!(
                    "answer must be reject, accept, reply or none, not '{}'.",
                    py::str_of(v)
                )));
            }
        },
    };
    let limit = match o.get("seat_limit") {
        None | Some(Value::Null) => 0,
        Some(v) => py::int_of(v)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| bad("seat_limit must be a whole number, 0 or more."))?,
    };
    let mut agents: Vec<(PlayerId, TestDriver)> = seats
        .into_iter()
        .map(|p| (p, TestDriver { answer, defer: deferring.contains(&p) }))
        .collect();
    let mut d = Drivers::none(g.state().players().len());
    for (p, a) in &mut agents {
        d = d.with(*p, a);
    }
    let (stop, _) = g.drive(&mut d, DriveOptions::default().with_seat_limit(limit))?;
    let (name, player, nids): (&str, Option<PlayerId>, Vec<u32>) = match stop {
        Stop::External(p) => ("external", Some(p), Vec::new()),
        Stop::HybridDiplomat(p) => ("hybrid_diplomat", Some(p), Vec::new()),
        Stop::AwaitingReply { pid, nids } => {
            ("awaiting_reply", Some(pid), nids.iter().map(|n| n.get()).collect())
        }
        Stop::SeatLimit => ("seat_limit", None, Vec::new()),
        _ => ("game_over", None, Vec::new()),
    };
    Ok(json!({
        "stop": name,
        "player": player.map(|p| p.0),
        "negotiations": nids,
        "turn": g.turn(),
        "current": g.current().0,
    }))
}

/// Gives a seat its own difficulty, named loosely, as the host's `set_difficulty` does
/// (`EngineGame.set_difficulty`): `ok` is false, and nothing changes, for a name that is no
/// level.
fn set_difficulty(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = pid(g, o.get("player"), false)?;
    let name = o.get("difficulty").map(py::str_of).unwrap_or_default();
    let Some(level) = g.rules().resolve::<DifficultyId>(&name) else {
        return Ok(json!({"ok": false}));
    };
    g.set_seat_difficulty(p, Some(level))
        .map_err(|e| ActionError::rule(format!("The game refused ({e}).")))?;
    Ok(json!({"ok": true}))
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

/// The unit an operation names by `unit`.
fn unit_param(g: &Game, o: &Params) -> Result<UnitId, ActionError> {
    let n: u32 = whole(o.get("unit").unwrap_or(&Value::Null), "unit")?;
    UnitId::new(n)
        .filter(|&u| g.unit(u).is_some())
        .ok_or_else(|| ActionError::new(ErrCode::NoSuchUnit, "No such unit."))
}

/// Sets a unit's fields, as a test poked them: health (1 to 100), movement in move-scale units,
/// experience, its tile (moved without movement rules, what it carries with it), its promotions
/// (the list it then has), and the unit carrying it on its tile, or none.
fn set_unit(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let u = unit_param(g, o)?;
    let hp = match given(o, "hp") {
        Some(v) => Some(i16::try_from(whole::<i64>(v, "hp")?.clamp(1, 100)).unwrap_or(100)),
        None => None,
    };
    let moves = match given(o, "moves") {
        Some(v) => Some(whole::<i32>(v, "moves")?.max(0)),
        None => None,
    };
    let xp = match given(o, "xp") {
        Some(v) => Some(whole::<i32>(v, "xp")?.max(0)),
        None => None,
    };
    let at = if o.contains_key("x") || o.contains_key("y") { Some(tile(g, o)?) } else { None };
    let promotions = match given(o, "promotions") {
        None => None,
        Some(Value::Array(names)) => {
            let mut set = PromotionSet::new();
            for n in names {
                set.insert(resolve::<PromotionId>(g, Some(n))?);
            }
            Some(set)
        }
        Some(_) => return Err(bad("promotions must be a list of promotion names.")),
    };
    let refused =
        |e: &dyn core::fmt::Display| ActionError::rule(format!("The game refused ({e})."));
    if let Some(t) = at {
        g.relocate_unit(u, t).map_err(|e| refused(&e))?;
    }
    if o.contains_key("carrier") {
        match given(o, "carrier") {
            None => g.unboard_unit(u).map_err(|e| refused(&e))?,
            Some(v) => {
                let n: u32 = whole(v, "carrier")?;
                let c = UnitId::new(n)
                    .filter(|&c| g.unit(c).is_some())
                    .ok_or_else(|| ActionError::new(ErrCode::NoSuchUnit, "No such carrier."))?;
                g.board_unit(u, c).map_err(|e| refused(&e))?;
            }
        }
    }
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
        if let Some(h) = hp {
            x.hp = h;
        }
        if let Some(m) = moves {
            x.moves = m;
        }
        if let Some(v) = xp {
            x.xp = v;
        }
        if let Some(p) = promotions {
            x.promotions = p;
        }
    }
    Ok(json!({}))
}

/// Readies a unit to act: its full movement, and no orders, attacks or action this turn.
fn ready_unit(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let u = unit_param(g, o)?;
    let full = crate::game::movement::max_moves(g, u);
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
        x.moves = full;
        x.activity = None;
        x.goto = None;
        x.path.clear();
        x.order_wait = 0;
        x.attacks = 0;
        x.acted = false;
    }
    Ok(json!({"moves": full}))
}

/// A unit attacks a tile as the `attack` tool would have it, whoever's turn it is: a nuclear
/// weapon detonates, an aircraft strikes, anything else attacks. What the attack reports.
fn attack_as(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let u = unit_param(g, o)?;
    let t = tile(g, o)?;
    // What the attacker's owner sees, after the operations before it in the list.
    g.settle_sight();
    let plan = crate::game::combat::actions::plan_attack(g, u, t)?;
    Ok(crate::game::combat::actions::apply_attack(g, plan))
}

/// A unit, or a player (the barbarians among them), takes the civilian on a tile
/// (`units.capture_civilian`), as the tests poked it; the captured unit's new id, or null when it
/// was destroyed instead.
fn capture_civilian(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let captor = match (given(o, "unit"), given(o, "player")) {
        (None, Some(v)) => {
            let n: u8 = whole(v, "player")?;
            Some(PlayerId(n)).filter(|&p| g.player(p).is_some())
        }
        _ => g.unit(unit_param(g, o)?).map(crate::state::units::Unit::owner),
    }
    .ok_or_else(|| ActionError::new(ErrCode::InvalidPlayer, "No such player."))?;
    let t = tile(g, o)?;
    let victim = g
        .civilian_at(t)
        .map(crate::state::units::Unit::id)
        .ok_or_else(|| ActionError::rule("There is no civilian there."))?;
    let taken = crate::game::units::capture::capture_civilian_by(g, captor, victim);
    Ok(json!({"unit": taken.map(UnitId::get)}))
}

/// A player's units carry out their standing orders now (`automation.run_unit_orders`), as
/// stage S8 of its turn has them.
fn automate(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = pid(g, o.get("player"), false)?;
    crate::game::automation::run_unit_orders(g, p);
    Ok(json!({}))
}

/// A player's workers do some turns of work (`workers.progress_builds`), as stage E6 of each of
/// its turns has them; their movement is left as it is.
fn progress_builds(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = pid(g, o.get("player"), false)?;
    let turns: u16 = match given(o, "turns") {
        Some(v) => whole(v, "turns")?,
        None => 1,
    };
    for _ in 0..turns {
        crate::game::workers::progress_builds(g, p);
    }
    Ok(json!({}))
}

// ---- Diplomacy and espionage (package 1c-05) -----------------------------------------------------

/// Gives a major civilization a new spy in its hideout (`espionage.add_spy`), whether or not
/// espionage is on; the spy's name.
fn add_spy(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = pid(g, o.get("player"), false)?;
    if !g.player(p).is_some_and(crate::state::players::Player::is_major) {
        return Err(bad("Only a major civilization has spies."));
    }
    let i = crate::game::espionage::add_spy(g, p).ok_or_else(|| bad("No spy was added."))?;
    let name = crate::game::espionage::spies(g, p).get(i).map(|s| s.name.to_string());
    Ok(json!({"spy": name}))
}

/// Closes a negotiation from outside it, as a host's timeout does
/// (`diplomacy.close_negotiation`); the negotiation as `inspect` gives it.
fn close_negotiation(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    use crate::game::diplomacy::negotiation;
    let nid = o.get("negotiation").and_then(py::int_of).unwrap_or(-1);
    let status = o.get("status").map(py::str_of).unwrap_or_default();
    let note = o.get("note").filter(|v| py::truthy(v)).map(py::str_of).unwrap_or_default();
    let by = match given(o, "by") {
        None => None,
        v => Some(pid(g, v, false)?),
    };
    let status = negotiation::close_status(&status)?;
    let id = negotiation::get(g, nid)?.id;
    negotiation::plan_close(g, id, status)?;
    negotiation::close(g, id, status, &note, by);
    let n = g.state().diplo().negotiation(id).ok_or_else(|| bad("No such negotiation."))?;
    Ok(negotiation::negotiation_json(g, n))
}

/// Opens a negotiation for a player whether or not it is its turn (`open_negotiation_as`); what
/// the tool reports.
fn open_negotiation_as(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    use crate::game::diplomacy::negotiation;
    let p = pid(g, o.get("player"), true)?;
    let to = o.get("to").and_then(py::int_of).ok_or_else(|| bad("'to' must be a player id."))?;
    let message = o.get("message").cloned().unwrap_or(Value::Null);
    let plan = negotiation::plan_open(g, p, to, &message, o.get("give"), o.get("receive"))?;
    Ok(negotiation::open(g, p, plan))
}

// ---- Barbarians (package 1c-06) ------------------------------------------------------------------

/// The barbarians take a turn now, as stage S0 has them (`barbarians.take_turn` after each of
/// their units' `units.start_turn`); with `unit`, only that barbarian acts, with the moves it has
/// (`barbarians._automate`, as the tests poked it).
fn barbarian_act(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let bid = g
        .barbarian_id()
        .ok_or_else(|| ActionError::new(ErrCode::InvalidPlayer, "The game has no barbarians."))?;
    g.settle_sight();
    if given(o, "unit").is_some() {
        let u = unit_param(g, o)?;
        if g.unit(u).map(crate::state::units::Unit::owner) != Some(bid) {
            return Err(ActionError::rule("That is not a barbarian unit."));
        }
        crate::game::barbarians::act_for_test(g, u);
    } else {
        crate::game::units::turn::start_units(g, bid);
        crate::game::barbarians::take_turn(g);
    }
    g.settle_sight();
    Ok(json!({}))
}

/// The city a test operation names by `city`.
fn city_param(g: &Game, o: &Params) -> Result<CityId, ActionError> {
    py::int_of(o.get("city").unwrap_or(&Value::Null))
        .and_then(|n| u32::try_from(n).ok())
        .and_then(CityId::new)
        .filter(|&c| g.city(c).is_some())
        .ok_or_else(|| ActionError::new(ErrCode::NoSuchCity, "No such city."))
}

/// Every barbarian camp is removed, with its improvement, as a bare game has none; the tiles.
fn clear_camps(g: &mut Game, _: &Params) -> Result<Value, ActionError> {
    let tiles = crate::game::barbarians::clear_camps(g);
    Ok(
        json!({"removed": tiles.iter().map(|&t| { let (x, y) = g.xy(t); json!([x, y]) }).collect::<Vec<_>>()}),
    )
}

/// A barbarian unit on a tile, which `add_unit` refuses to make (`g.create_unit` for the
/// barbarians, as the tests poked it); `unit_id`.
fn add_barbarian(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let bid = g
        .barbarian_id()
        .ok_or_else(|| ActionError::new(ErrCode::InvalidPlayer, "The game has no barbarians."))?;
    let base: crate::base::ids::BaseUnitId = resolve(g, o.get("unit"))?;
    let at = tile(g, o)?;
    let hp = match given(o, "hp") {
        None => None,
        Some(v) => Some(i16::try_from(whole::<i64>(v, "hp")?.clamp(1, 100)).unwrap_or(100)),
    };
    let u = g
        .create_unit(bid, base, at, 0)
        .map_err(|e| ActionError::rule(format!("The game refused the edit ({e}).")))?;
    if let Some(h) = hp
        && let Some(x) = g.unit_mut(u, UnitTouch::CORE)
    {
        x.hp = h;
    }
    Ok(json!({"unit_id": u.get()}))
}

/// A barbarian camp on a tile (`barbarians.create_camp`); its id.
fn create_camp(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let t = tile(g, o)?;
    let id = crate::game::barbarians::create_camp(g, t)
        .ok_or_else(|| ActionError::rule("The ruleset has no barbarian camp."))?;
    Ok(json!(id.get()))
}

// ---- City-states (package 1c-06) ----------------------------------------------------------------

/// A city-state gives a major a quest now, as its turn would (`city_states._assign`), whether or
/// not the quest fits the game: its row's scope unless `scope` says otherwise, and the target its
/// row takes, from `target` or, for a camp, from `x` and `y`. A contest starts from 0 and an
/// investment at the row's percent unless `target` says otherwise. `quest`, its text.
fn add_quest(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    use crate::rules::defs::{QuestScope, QuestTargetKind};
    use crate::state::players::QuestTarget;
    let cs = pid(g, o.get("city_state"), false)?;
    if !g.is_city_state(cs) {
        return Err(bad(format!("Player {} is not a city-state.", cs.0)));
    }
    let major = pid(g, o.get("player"), true)?;
    let raw = o.get("quest").unwrap_or(&Value::Null);
    let name = raw.as_str().unwrap_or_default();
    let (k, def) = g
        .rules()
        .quests()
        .iter()
        .find(|(_, d)| &*d.name == name)
        .ok_or_else(|| bad(format!("Unknown quest {}.", py::repr(raw))))?;
    let scope = match given(o, "scope").map(py::str_of).as_deref() {
        None => def.scope,
        Some("individual") => QuestScope::Individual,
        Some("global") => QuestScope::Global,
        Some(other) => {
            return Err(bad(format!("scope must be individual or global, not '{other}'.")));
        }
    };
    let t = given(o, "target");
    let target = match def.target {
        QuestTargetKind::None => QuestTarget::None,
        QuestTargetKind::Tile => QuestTarget::Tile(tile(g, o)?),
        QuestTargetKind::Resource => QuestTarget::Resource(resolve(g, t)?),
        QuestTargetKind::Building => QuestTarget::Building(resolve(g, t)?),
        QuestTargetKind::UnitType => QuestTarget::UnitType(resolve(g, t)?),
        QuestTargetKind::NaturalWonder => QuestTarget::NaturalWonder(resolve(g, t)?),
        QuestTargetKind::Player => QuestTarget::Player(pid(g, t, false)?),
        QuestTargetKind::Religion => {
            let founder = pid(g, t, false)?;
            let founded = g.player(founder).and_then(|p| p.religion.founded);
            QuestTarget::Religion(founded.ok_or_else(|| {
                ActionError::rule(format!("Player {} has founded no religion.", founder.0))
            })?)
        }
        QuestTargetKind::Baseline => {
            QuestTarget::Baseline(t.map_or(Ok(0), |v| whole(v, "target"))?)
        }
        QuestTargetKind::Percent => {
            let row = def.params.first().copied().unwrap_or(50.0);
            let row = i16::try_from(crate::base::num::trunc_i64(row)).unwrap_or(50);
            QuestTarget::Percent(t.map_or(Ok(row), |v| whole(v, "target"))?)
        }
    };
    let q = crate::game::city_states::quests::assign(g, cs, k, major, target, scope);
    Ok(json!({"quest": crate::game::city_states::quests::quest_text(g, &q)}))
}

/// The barbarians sack a city (`barbarians.sack_city`); what they took, as an attack reports it.
fn sack_city(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let c = city_param(g, o)?;
    Ok(crate::game::barbarians::sack_city(g, c).to_json())
}

#[cfg(test)]
mod tests {
    use super::TEST_OPS;

    /// The operations are sorted, which `op` searches by, and each is described in one line as
    /// Python's `testops.py` describes it, with no run of spaces a broken literal would leave.
    #[test]
    fn the_operations_are_sorted_and_described_in_one_line() {
        for w in TEST_OPS.windows(2) {
            assert!(w[0].name < w[1].name, "{} before {}", w[0].name, w[1].name);
        }
        for o in TEST_OPS {
            assert!(
                !o.params.contains("  ") && !o.params.contains('\n'),
                "{}: {:?}",
                o.name,
                o.params
            );
        }
    }
}

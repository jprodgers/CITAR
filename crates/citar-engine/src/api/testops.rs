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
//! `ready_unit`. The others are listed with the package that ports what they need, and are
//! refused as not ported until then.

use serde_json::{Map, Value, json};

use super::scenario::{given, pid, players, resolve, tile, whole};
use crate::base::ids::{CityId, PlayerId, PromotionId, UnitId};
use crate::base::py;
use crate::base::sets::PromotionSet;
use crate::game::cities::construction;
use crate::game::derive::rev::UnitTouch;
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

/// A great prophet founds or enhances a religion where it stands and is spent, as the unit action
/// will do (package 1c-04), which checks besides whether the unit may act now.
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
/// action will (package 1c-04).
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

fn sack_city(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::barbarians"))
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

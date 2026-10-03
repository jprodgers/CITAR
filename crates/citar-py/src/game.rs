//! `Game`: one game behind a lock, with the small copy of where it stands that the cheap reads
//! serve (DESIGN.md P2.6.1-P2.6.2). The facade's `EngineGame` (`citar/engine_api.py`) wraps it.
//!
//! - **Heavy calls** (anything that may take more than about 100 µs: commands, views, the
//!   briefing, the replay, loads, saves) run with the GIL released ([`detached`]) and take the
//!   game's lock inside, so no thread waits for a game while holding the GIL.
//! - **Cheap reads** (`turn`, `current`, `phase`, `winner`, `victory`, `turn_limit`,
//!   `revision`, `poisoned`, `is_alive`, `negotiation_head`, `open_negotiation_heads`) read
//!   [`Heads`] with the GIL held and never wait for the game: every call that changes the game
//!   publishes a fresh copy before it lets go of the game's lock. The lock order is game, then
//!   heads; the getters take only the heads, each for a copy.
//! - **Panics** are caught inside the game's lock: the game is poisoned, the lock is not, and
//!   the caller gets `EngineCrash`. A poisoned game refuses every command with `EngineCrash`
//!   and still answers reads, views, the replay and saves (DESIGN.md 8.5).
//! - Every command returns the events it appended as JSON bytes, each in Python's dict shape,
//!   which the facade fans out to its subscribers after the call.

use std::sync::{Mutex, MutexGuard, PoisonError};

use citar_bot::Bot as Driver;
use citar_engine::api::game::DebugAction;
use citar_engine::api::host::{Heads, NegotiationHead, phase_name};
use citar_engine::api::views::client::ClientView;
use citar_engine::api::views::replay::deal_json;
use citar_engine::api::views::{ReplayFormat, to_py_json};
use citar_engine::base::ids::{DealId, EventId, NegotiationId, PlayerId, UnitId};
use citar_engine::base::sets::PlayerSet;
use citar_engine::game::diplomacy::{deals, negotiation};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{
    ActionError as EngineAction, DriveOptions, DriverOutcome, Drivers, ErrCode, EventBatch,
    Game as EngineGame, SeatDriver,
};
use citar_engine::rules::Ruleset;
use citar_engine::state::chronicle::EventData;
use citar_engine::state::diplo::{DealItem, NegStatus, Terms};
use citar_engine::state::players::{Controller, SeatOverrides};
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::{PyDict, PyList};
use serde_json::{Map, Value, json};

use crate::Bytes;
use crate::bot::Bot;
use crate::calls::detached;
use crate::errors::{Failure, caught, parse, parse_opt};

/// One game (DESIGN.md P2.6.1). Frozen, so it must be `Sync`: the engine's `Game` is `Send` and
/// not `Sync`, and the lock around it makes it shareable.
#[pyclass(frozen, module = "citar._engine", name = "Game")]
pub struct Game {
    game: Mutex<EngineGame>,
    heads: Mutex<Heads>,
}

/// A player id as the engine takes one, for a game that has it.
fn player_of(g: &EngineGame, pid: i64) -> Result<PlayerId, Failure> {
    u8::try_from(pid)
        .ok()
        .map(PlayerId)
        .filter(|&p| g.player(p).is_some())
        .ok_or_else(|| Failure::Value(format!("No player {pid}.")))
}

/// A major civilization of the game: the briefing and the empire's summary are a major's.
fn major_of(g: &EngineGame, pid: i64) -> Result<PlayerId, Failure> {
    let p = player_of(g, pid)?;
    if g.player(p).is_some_and(|x| x.is_major()) {
        Ok(p)
    } else {
        Err(Failure::Value(format!("Player {pid} is not a major civilization.")))
    }
}

/// A player id for a command whose own refusal names an unknown player (`execute`): a number
/// no game has is refused as the tools refuse one.
fn caller_of(pid: i64) -> Result<PlayerId, Failure> {
    u8::try_from(pid)
        .map(PlayerId)
        .map_err(|_| EngineAction::new(ErrCode::InvalidPlayer, "Invalid player.").into())
}

/// A negotiation of the game, refused with the engine's sentence for an unknown id.
fn negotiation_of(g: &EngineGame, nid: i64) -> Result<NegotiationId, Failure> {
    Ok(negotiation::get(g, nid)?.id)
}

/// A batch of events as JSON: each as it happened, in Python's dict.
fn events_json(g: &EngineGame, batch: &EventBatch) -> Bytes {
    let rows: Vec<Value> = batch.events().iter().map(|e| g.event_json(e, None)).collect();
    Bytes(to_py_json(&rows))
}

/// A negotiation head as the facade's dict.
fn head_dict<'py>(py: Python<'py>, h: &NegotiationHead) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("id", h.id)?;
    d.set_item("initiator", h.initiator)?;
    d.set_item("responder", h.responder)?;
    d.set_item("status", h.status)?;
    d.set_item("awaiting", h.awaiting)?;
    d.set_item("entries", h.entries)?;
    Ok(d)
}

/// Deal items as the engine stores them (from a negotiation's record), strictly.
fn items_of(g: &EngineGame, v: &Value) -> Result<Vec<DealItem>, Failure> {
    let list = match v {
        Value::Null => return Ok(Vec::new()),
        Value::Array(a) => a,
        _ => return Err(Failure::Value("Deal items must be a list of objects.".to_owned())),
    };
    list.iter()
        .map(|i| DealItem::from_json(i, g.rules()).map_err(|e| Failure::Value(e.0)))
        .collect()
}

/// The event data a host may attach to an event of its own: the players it concerns, and the
/// unit, city, deal or negotiation it is about, by id.
fn host_data(v: &Value) -> Result<EventData, Failure> {
    let mut d = EventData::default();
    let Some(m) = v.as_object() else {
        return match v {
            Value::Null => Ok(d),
            _ => Err(Failure::Value("The event's data must be an object.".to_owned())),
        };
    };
    for (k, x) in m {
        let n = x.as_u64().ok_or_else(|| {
            Failure::Value(format!("The event's {k} must be an id, a whole number, not {x}."))
        })?;
        let player = || u8::try_from(n).ok().map(PlayerId);
        let id32 = || u32::try_from(n).ok();
        let bad = || Failure::Value(format!("The event's {k} is no id: {n}."));
        match k.as_str() {
            "player" => d.player = Some(player().ok_or_else(bad)?),
            "a" => d.a = Some(player().ok_or_else(bad)?),
            "b" => d.b = Some(player().ok_or_else(bad)?),
            "owner" => d.owner = Some(player().ok_or_else(bad)?),
            "sender" => d.sender = Some(player().ok_or_else(bad)?),
            "unit" => d.unit = Some(id32().and_then(UnitId::new).ok_or_else(bad)?),
            "city" => {
                d.city =
                    Some(id32().and_then(citar_engine::base::ids::CityId::new).ok_or_else(bad)?);
            }
            "deal" => d.deal = Some(id32().and_then(DealId::new).ok_or_else(bad)?),
            "negotiation" => {
                d.negotiation = Some(id32().and_then(NegotiationId::new).ok_or_else(bad)?);
            }
            _ => {
                return Err(Failure::Value(format!(
                    "An event of the host's carries player, a, b, owner, sender, unit, city, deal \
                     or negotiation, not {k}."
                )));
            }
        }
    }
    Ok(d)
}

impl Game {
    /// Wraps a game the engine made, publishing its heads.
    pub fn wrap(g: EngineGame) -> Self {
        let heads = g.heads();
        Self { game: Mutex::new(g), heads: Mutex::new(heads) }
    }

    fn lock(&self) -> MutexGuard<'_, EngineGame> {
        // Never poisoned: every call catches its panics inside the lock.
        self.game.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn heads(&self) -> MutexGuard<'_, Heads> {
        // Held only for a copy, by code that cannot panic.
        self.heads.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Publishes where `g` stands, still under its lock (DESIGN.md P2.6.2).
    fn publish(&self, g: &EngineGame) {
        let fresh = g.heads();
        *self.heads() = fresh;
    }

    /// Reads the game with the GIL released, under its lock. A panic poisons the game and is an
    /// `EngineCrash`.
    fn read<T, F>(&self, py: Python<'_>, f: F) -> Result<T, Failure>
    where
        F: FnOnce(&EngineGame) -> Result<T, Failure> + Send,
        T: Send,
    {
        detached(py, || {
            let mut g = self.lock();
            match caught(|| f(&g)) {
                Ok(done) => done,
                Err(crash) => {
                    g.poison(&crash);
                    self.publish(&g);
                    Err(Failure::Crash(crash))
                }
            }
        })
    }

    /// Changes the game with the GIL released, under its lock, then publishes its heads. A
    /// poisoned game refuses with `EngineCrash`; a panic poisons the game and is one.
    fn write<T, F>(&self, py: Python<'_>, f: F) -> Result<T, Failure>
    where
        F: FnOnce(&mut EngineGame) -> Result<T, Failure> + Send,
        T: Send,
    {
        detached(py, || {
            let mut g = self.lock();
            if let Some(why) = g.poisoned() {
                return Err(Failure::poisoned(why));
            }
            let done = match caught(|| f(&mut g)) {
                Ok(done) => done,
                Err(crash) => {
                    g.poison(&crash);
                    Err(Failure::Crash(crash))
                }
            };
            self.publish(&g);
            done
        })
    }
}

/// `{pid: {tool: [taken, refused]}}` for the bots of a drive, by seat.
fn action_counts(drivers: &[(PlayerId, Driver)]) -> Bytes {
    let m: Map<String, Value> =
        drivers.iter().map(|(p, b)| (p.0.to_string(), b.refusals().to_json())).collect();
    Bytes(to_py_json(&m))
}

#[pymethods]
impl Game {
    // ---- Making, loading and saving ----------------------------------------------------------

    /// A new game from the lobby's settings as JSON bytes (`EngineGame.new`), which must carry a
    /// seed and, for an editor map, the map's document inline. `ValueError` for settings that do
    /// not make a game, `MapError` for a map that does not.
    #[staticmethod]
    fn new(py: Python<'_>, config_json: &[u8]) -> PyResult<Self> {
        let made = detached(py, || {
            caught(|| {
                let rules = Ruleset::shared();
                let config = parse(config_json, "The settings")?;
                let setup = config_from_value(rules, config)?;
                let (game, _created) = EngineGame::new(rules, &setup)?;
                Ok::<_, Failure>(game)
            })
            .map_err(Failure::Crash)?
        })?;
        Ok(Self::wrap(made))
    }

    /// A game from a save: its state JSON and the journal chunks that rebuild its history
    /// (`EngineGame.from_save`, `from_state`). Returns the game and what loading found, as JSON
    /// bytes: `rules_changed` (the save's ruleset id and engine, when another ruleset made it),
    /// `chronicle_incomplete` and `engine`. `LoadError` for a save that does not load.
    #[staticmethod]
    #[pyo3(signature = (state_json, chunks = Vec::new()))]
    fn load(
        py: Python<'_>,
        state_json: &[u8],
        chunks: Vec<PyBackedBytes>,
    ) -> PyResult<(Self, Bytes)> {
        let (game, report) = detached(py, || {
            caught(|| {
                let mut it = chunks.iter().map(|c| &**c);
                let (game, report) = EngineGame::load(Ruleset::shared(), state_json, &mut it)?;
                let report = json!({
                    "rules_changed": report.rules_changed.map(|(id, engine)| json!([id.to_hex(), engine])),
                    "chronicle_incomplete": report.chronicle_incomplete,
                    "engine": report.engine,
                });
                Ok::<_, Failure>((game, Bytes(to_py_json(&report))))
            })
            .map_err(Failure::Crash)?
        })?;
        Ok((Self::wrap(game), report))
    }

    /// The game as one save until package 2-11's container (`EngineGame.to_save`): the state
    /// JSON, and the whole history as one journal chunk (`None` before there is any), which
    /// `Game.load` reads back to the same game.
    fn save(&self, py: Python<'_>) -> PyResult<(Bytes, Option<Bytes>)> {
        Ok(self.read(py, |g| {
            let s = g.save_whole().map_err(|e| Failure::Runtime(e.to_string()))?;
            Ok((Bytes(s.state), s.history.map(Bytes)))
        })?)
    }

    /// The state alone as JSON (`EngineGame.state_dict`): a scenario's or an undo's starting
    /// point, with no history.
    fn state_json(&self, py: Python<'_>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            g.snapshot().to_json().map(Bytes).map_err(|e| Failure::Runtime(e.to_string()))
        })?)
    }

    /// The state's digest, 64 hex digits (DESIGN.md 4.10): equal games have equal digests.
    fn digest(&self, py: Python<'_>) -> PyResult<String> {
        Ok(self.read(py, |g| {
            g.digest().map(|d| d.to_hex()).map_err(|e| Failure::Runtime(e.to_string()))
        })?)
    }

    // ---- The heads: read without waiting for the game -------------------------------------

    /// The current turn.
    #[getter]
    fn turn(&self) -> i32 {
        self.heads().turn
    }

    /// Whose turn it is, as a player id.
    #[getter]
    fn current(&self) -> u8 {
        self.heads().current.0
    }

    /// `playing` or `over`.
    #[getter]
    fn phase(&self) -> &'static str {
        phase_name(self.heads().phase)
    }

    /// The winner's id, once there is one.
    #[getter]
    fn winner(&self) -> Option<u8> {
        self.heads().winner.map(|p| p.0)
    }

    /// How the game was won, once it has been.
    #[getter]
    fn victory(&self) -> Option<&'static str> {
        self.heads().victory
    }

    /// The turn the game ends on.
    #[getter]
    fn turn_limit(&self) -> i32 {
        self.heads().turn_limit
    }

    /// The game's revision: it moves on every change, never on a read.
    #[getter]
    fn revision(&self) -> u64 {
        self.heads().revision
    }

    /// Why the game stopped, once a panic has poisoned it.
    #[getter]
    fn poisoned(&self) -> Option<String> {
        self.heads().poisoned.as_deref().map(str::to_owned)
    }

    /// Whether a civilization is still in the game. `ValueError` for a player the game lacks.
    fn is_alive(&self, pid: i64) -> PyResult<bool> {
        let alive = u8::try_from(pid).ok().and_then(|p| self.heads().is_alive(PlayerId(p)));
        Ok(alive.ok_or_else(|| Failure::Value(format!("No player {pid}.")))?)
    }

    /// Where one negotiation stands: `{id, initiator, responder, status, awaiting, entries}`.
    /// An open one comes from the heads; one no longer open is read under the game's lock.
    /// `ActionError` for an unknown id.
    fn negotiation_head<'py>(&self, py: Python<'py>, nid: i64) -> PyResult<Bound<'py, PyDict>> {
        let open = u32::try_from(nid).ok().and_then(|n| self.heads().open_head(n).copied());
        let head = match open {
            Some(h) => h,
            None => self.read(py, |g| {
                let id = negotiation_of(g, nid)?;
                g.negotiation_head(id)
                    .ok_or_else(|| Failure::Value(format!("No negotiation {nid}.")))
            })?,
        };
        head_dict(py, &head)
    }

    /// `negotiation_head` of each open negotiation, oldest first; only those `pid` is a party to,
    /// if given.
    #[pyo3(signature = (pid = None))]
    fn open_negotiation_heads<'py>(
        &self,
        py: Python<'py>,
        pid: Option<i64>,
    ) -> PyResult<Bound<'py, PyList>> {
        let who = match pid {
            None => None,
            Some(n) => match u8::try_from(n) {
                Ok(p) => Some(PlayerId(p)),
                Err(_) => return Ok(PyList::empty(py)),
            },
        };
        let heads: Vec<NegotiationHead> = self.heads().open_heads(who).copied().collect();
        let list = PyList::empty(py);
        for h in &heads {
            list.append(head_dict(py, h)?)?;
        }
        Ok(list)
    }

    // ---- Reads -----------------------------------------------------------------------------

    /// The game's settings as Python's `Game.new` normalised them (`EngineGame.config`).
    fn config(&self, py: Python<'_>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| Ok(Bytes(to_py_json(&g.lobby_config()))))?)
    }

    /// The game at a glance (`EngineGame.summary`).
    fn summary(&self, py: Python<'_>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| Ok(Bytes(to_py_json(&g.summary_json()))))?)
    }

    /// One civilization's identity and settings (`EngineGame.player`).
    fn player(&self, py: Python<'_>, pid: i64) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let p = player_of(g, pid)?;
            Ok(Bytes(to_py_json(&g.player_row(p))))
        })?)
    }

    /// A civilization's name.
    fn player_name(&self, py: Python<'_>, pid: i64) -> PyResult<String> {
        Ok(self.read(py, |g| {
            let p = player_of(g, pid)?;
            Ok(g.player(p).map(|x| x.name.to_string()).unwrap_or_default())
        })?)
    }

    /// The major civilizations, as `player` gives each (`EngineGame.majors`).
    #[pyo3(signature = (alive_only = true))]
    fn majors(&self, py: Python<'_>, alive_only: bool) -> PyResult<Bytes> {
        Ok(self.read(py, |g| Ok(Bytes(to_py_json(&g.majors_json(alive_only)))))?)
    }

    /// How a civilization is doing: `{score, cities, units, population, techs, gold, era}`
    /// (`EngineGame.standing`).
    fn standing(&self, py: Python<'_>, pid: i64) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let p = player_of(g, pid)?;
            Ok(Bytes(to_py_json(&g.standing(p))))
        })?)
    }

    /// Every major civilization's standing, the eliminated ones included, keyed by id as text
    /// (`EngineGame.standings`; JSON keys are strings).
    fn standings(&self, py: Python<'_>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let m: Map<String, Value> = g
                .standings()
                .into_iter()
                .map(|s| (s.player.0.to_string(), serde_json::to_value(&s).unwrap_or(Value::Null)))
                .collect();
            Ok(Bytes(to_py_json(&m)))
        })?)
    }

    /// The rounds' statistics rows, oldest first; the last `last` of them if given
    /// (`EngineGame.stats`).
    #[pyo3(signature = (last = None))]
    fn stats(&self, py: Python<'_>, last: Option<usize>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| Ok(Bytes(to_py_json(&g.stats_rows(last)))))?)
    }

    /// Every event as it happened, oldest first; the last `last` if given (`EngineGame.events`).
    #[pyo3(signature = (last = None))]
    fn events(&self, py: Python<'_>, last: Option<usize>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| Ok(Bytes(to_py_json(&g.event_rows(last)))))?)
    }

    /// Event `event_id` as player `pid` may see it (everything for `None`): unmet civilizations
    /// anonymised, their places dropped (`EngineGame.event_view`). `ValueError` for an event the
    /// game does not have.
    #[pyo3(signature = (event_id, pid = None))]
    fn event_view(&self, py: Python<'_>, event_id: i64, pid: Option<i64>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let viewer = pid.map(|p| player_of(g, p)).transpose()?;
            let ev = u32::try_from(event_id)
                .ok()
                .and_then(EventId::new)
                .and_then(|id| g.event_by_id(id))
                .ok_or_else(|| Failure::Value(format!("No event {event_id}.")))?;
            Ok(Bytes(to_py_json(&g.event_json(ev, viewer))))
        })?)
    }

    /// Recorded thoughts from index `since` on, oldest first; only `pid`'s if given
    /// (`EngineGame.thoughts`).
    #[pyo3(signature = (pid = None, since = 0))]
    fn thoughts(&self, py: Python<'_>, pid: Option<i64>, since: usize) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let who = pid.map(|p| player_of(g, p)).transpose()?;
            Ok(Bytes(to_py_json(&g.thought_rows(who, since))))
        })?)
    }

    /// How many thoughts have been recorded, a mark for `thoughts(since=...)`.
    fn thought_count(&self, py: Python<'_>) -> PyResult<usize> {
        Ok(self.read(py, |g| Ok(g.chronicle().thoughts().len()))?)
    }

    /// Records a seat's reasoning, an action or a system note for spectators and the replay
    /// (`EngineGame.add_thought`).
    #[pyo3(signature = (pid, text, kind = None))]
    fn add_thought(
        &self,
        py: Python<'_>,
        pid: i64,
        text: &str,
        kind: Option<&str>,
    ) -> PyResult<()> {
        Ok(self.write(py, |g| {
            let p = player_of(g, pid)?;
            g.add_thought(p, text, kind);
            Ok(())
        })?)
    }

    /// Records an event of the host's own (an agent's error, a pause) and returns it as a batch
    /// (`EngineGame.emit`). `players` `None` makes it public; `data_json` names the players and
    /// objects it concerns by id (`{"player": 2}`).
    #[pyo3(signature = (kind, text, players = None, data_json = None))]
    fn emit(
        &self,
        py: Python<'_>,
        kind: &str,
        text: &str,
        players: Option<Vec<i64>>,
        data_json: Option<&[u8]>,
    ) -> PyResult<Bytes> {
        Ok(self.write(py, |g| {
            let audience = match players {
                None => None,
                Some(list) => Some(
                    list.into_iter()
                        .map(|p| player_of(g, p))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .collect::<PlayerSet>(),
                ),
            };
            let data = host_data(&parse_opt(data_json, "The event's data")?)?;
            let batch = g.emit_host(kind, text, audience, data);
            Ok(events_json(g, &batch))
        })?)
    }

    /// What the game's checks have found since the last call, as text (DESIGN.md 9.4): nothing
    /// in a build without them (a release wheel).
    fn take_violations(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        Ok(detached(py, || {
            let mut g = self.lock();
            g.take_violations().iter().map(ToString::to_string).collect()
        }))
    }

    // ---- Tools and views -------------------------------------------------------------------

    /// Runs a player tool with JSON arguments (`EngineGame.execute`): the result and the events
    /// it appended (none for a query). `ActionError` for a refusal.
    #[pyo3(signature = (pid, tool, args_json = None))]
    fn execute(
        &self,
        py: Python<'_>,
        pid: i64,
        tool: &str,
        args_json: Option<&[u8]>,
    ) -> PyResult<(Bytes, Bytes)> {
        Ok(self.write(py, |g| {
            let p = caller_of(pid)?;
            let args = match parse_opt(args_json, "The tool's arguments")? {
                Value::Null => Value::Object(Map::new()),
                other => other,
            };
            let done = g.execute(p, tool, &args)?;
            Ok((Bytes(to_py_json(&done.result)), events_json(g, &done.events)))
        })?)
    }

    /// The client view as JSON bytes, as player `pid` sees the game (everything for `None`),
    /// with the last `event_limit` of its events (`EngineGame.view`). `extra_json`'s keys are
    /// added to the view as they are (the server's `seat`, `session`, `version`,
    /// `spectator`); a key of the view's own is refused with `ValueError`.
    #[pyo3(signature = (pid = None, event_limit = 150, extra_json = None))]
    fn view_json(
        &self,
        py: Python<'_>,
        pid: Option<i64>,
        event_limit: u32,
        extra_json: Option<&[u8]>,
    ) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let viewer = pid.map(|p| player_of(g, p)).transpose()?;
            let extra = match parse_opt(extra_json, "The view's extra keys")? {
                Value::Null => Map::new(),
                Value::Object(m) => m,
                _ => {
                    return Err(Failure::Value(
                        "The view's extra keys must be an object.".to_owned(),
                    ));
                }
            };
            if let Some(k) = extra.keys().find(|k| ClientView::FIELDS.contains(&k.as_str())) {
                return Err(Failure::Value(format!("{k} is the view's own key.")));
            }
            let mut view = g.client_view(viewer, i64::from(event_limit));
            view.rest.extend(extra);
            Ok(Bytes(to_py_json(&view)))
        })?)
    }

    /// A language model's start-of-turn briefing (`EngineGame.briefing`).
    fn briefing(&self, py: Python<'_>, pid: i64) -> PyResult<String> {
        Ok(self.read(py, |g| Ok(g.briefing(major_of(g, pid)?)))?)
    }

    /// What is still unhandled this turn, as a short note (`EngineGame.turn_progress`).
    fn turn_progress(&self, py: Python<'_>, pid: i64) -> PyResult<String> {
        Ok(self.read(py, |g| Ok(g.turn_progress(major_of(g, pid)?)))?)
    }

    /// The empire at a glance, with its city count, whom it is at war with and its notebook
    /// (`EngineGame.empire_summary`).
    fn empire_summary(&self, py: Python<'_>, pid: i64) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let p = major_of(g, pid)?;
            Ok(Bytes(to_py_json(&g.empire_summary(p).map(|s| s.to_json()))))
        })?)
    }

    /// Why the `end_turn` tool would refuse `pid` because of an open negotiation, or `None`.
    fn end_turn_refusal(&self, py: Python<'_>, pid: i64) -> PyResult<Option<String>> {
        Ok(self.read(py, |g| Ok(g.end_turn_refusal(player_of(g, pid)?)))?)
    }

    // ---- Negotiations ----------------------------------------------------------------------

    /// One negotiation as the game keeps it (`EngineGame.negotiation`). `ActionError` for an
    /// unknown id.
    fn negotiation(&self, py: Python<'_>, nid: i64) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let id = negotiation_of(g, nid)?;
            Ok(Bytes(to_py_json(&g.negotiation_record(id))))
        })?)
    }

    /// The open negotiations, oldest first; only `pid`'s if given
    /// (`EngineGame.open_negotiations`).
    #[pyo3(signature = (pid = None))]
    fn open_negotiations(&self, py: Python<'_>, pid: Option<i64>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let who = pid.map(|p| player_of(g, p)).transpose()?;
            Ok(Bytes(to_py_json(&g.negotiation_records(who, true))))
        })?)
    }

    /// Every negotiation, settled ones included, oldest first; only `pid`'s if given
    /// (`EngineGame.negotiations`).
    #[pyo3(signature = (pid = None))]
    fn negotiations(&self, py: Python<'_>, pid: Option<i64>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let who = pid.map(|p| player_of(g, p)).transpose()?;
            Ok(Bytes(to_py_json(&g.negotiation_records(who, false))))
        })?)
    }

    /// A negotiation as one side sees it, in its own terms (`EngineGame.negotiation_view`).
    fn negotiation_view(&self, py: Python<'_>, nid: i64, pid: i64) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let id = negotiation_of(g, nid)?;
            let p = player_of(g, pid)?;
            Ok(Bytes(to_py_json(&g.negotiation_view(id, p)?)))
        })?)
    }

    /// Closes an open negotiation from outside it (a timeout, a forced close) with a note both
    /// sides are told: the negotiation as it now stands, and the events
    /// (`EngineGame.close_negotiation`).
    #[pyo3(signature = (nid, status, note, by = None))]
    fn close_negotiation(
        &self,
        py: Python<'_>,
        nid: i64,
        status: &str,
        note: &str,
        by: Option<i64>,
    ) -> PyResult<(Bytes, Bytes)> {
        Ok(self.write(py, |g| {
            let id = negotiation_of(g, nid)?;
            let st = NegStatus::from_name(status).ok_or_else(|| {
                EngineAction::new(ErrCode::BadParam, format!("Unknown status '{status}'."))
            })?;
            let by = by.map(|p| player_of(g, p)).transpose()?;
            let (_, batch) = g.close_negotiation(id, st, note, by)?;
            Ok((Bytes(to_py_json(&g.negotiation_record(id))), events_json(g, &batch)))
        })?)
    }

    /// How many messages a negotiation may hold in this game.
    fn max_chat_messages(&self, py: Python<'_>) -> PyResult<u32> {
        Ok(self.read(py, |g| Ok(g.max_chat_messages()))?)
    }

    /// One concluded deal, or `None` (`EngineGame.deal`).
    fn deal(&self, py: Python<'_>, deal_id: i64) -> PyResult<Option<Bytes>> {
        Ok(self.read(py, |g| {
            let deal = u32::try_from(deal_id).ok().and_then(DealId::new).and_then(|d| g.deal(d));
            Ok(deal.map(|d| Bytes(to_py_json(&deal_json(g, d)))))
        })?)
    }

    /// Deal items, as the game stores them, as a sentence (`EngineGame.describe_items`).
    fn describe_items(&self, py: Python<'_>, items_json: &[u8]) -> PyResult<String> {
        Ok(self.read(py, |g| {
            let items = items_of(g, &parse(items_json, "The items")?)?;
            Ok(g.describe_items(&items))
        })?)
    }

    /// Checks that `giver` can give `items` to `receiver` under `proposal` (the terms as the game
    /// stores them). `ActionError` names the first it cannot (`EngineGame.validate_items`).
    fn validate_items(
        &self,
        py: Python<'_>,
        giver: i64,
        receiver: i64,
        items_json: &[u8],
        proposal_json: &[u8],
    ) -> PyResult<()> {
        Ok(self.read(py, |g| {
            let (a, b) = (player_of(g, giver)?, player_of(g, receiver)?);
            let items = items_of(g, &parse(items_json, "The items")?)?;
            let terms = Terms::from_json(&parse(proposal_json, "The proposal")?, g.rules())
                .map_err(|e| Failure::Value(e.0))?;
            Ok(g.validate_items(a, b, &items, &terms)?)
        })?)
    }

    /// Opens a negotiation for `pid` whether or not it is its turn (a probe's scripted
    /// counterparty), `give` and `receive` as a caller writes deal items: what the tool returns,
    /// and the events (`EngineGame.open_negotiation_as`).
    #[pyo3(signature = (pid, to, message, give_json = None, receive_json = None))]
    fn open_negotiation_as(
        &self,
        py: Python<'_>,
        pid: i64,
        to: i64,
        message: &str,
        give_json: Option<&[u8]>,
        receive_json: Option<&[u8]>,
    ) -> PyResult<(Bytes, Bytes)> {
        Ok(self.write(py, |g| {
            let (p, q) = (caller_of(pid)?, caller_of(to)?);
            let give = parse_opt(give_json, "The items given")?;
            let receive = parse_opt(receive_json, "The items received")?;
            let give = deals::normalize_items(g, p, Some(&give))?;
            let receive = deals::normalize_items(g, q, Some(&receive))?;
            let (done, batch) = g.open_negotiation_as(p, q, message, &give, &receive)?;
            Ok((Bytes(to_py_json(&done)), events_json(g, &batch)))
        })?)
    }

    // ---- Seats -----------------------------------------------------------------------------

    /// Hands a civilization to another turn driver; its handicap and automatic decisions follow
    /// it, except those set explicitly, now or before (`EngineGame.set_controller`). The events.
    /// `ValueError` for a setting that is not one.
    #[pyo3(signature = (pid, controller, handicap = None, auto_json = None))]
    fn set_controller(
        &self,
        py: Python<'_>,
        pid: i64,
        controller: &str,
        handicap: Option<&str>,
        auto_json: Option<&[u8]>,
    ) -> PyResult<Bytes> {
        Ok(self.write(py, |g| {
            let p = player_of(g, pid)?;
            let c = Controller::from_name(controller)
                .ok_or_else(|| Failure::Value(format!("Unknown controller '{controller}'.")))?;
            let auto = parse_opt(auto_json, "The automatic decisions")?;
            let handicap = handicap.map(|h| json!(h));
            let over = SeatOverrides::parse(handicap.as_ref(), Some(&auto))
                .map_err(|e| Failure::Value(e.0))?;
            let batch = g.set_controller(p, c, over.handicap, over.auto)?;
            Ok(events_json(g, &batch))
        })?)
    }

    /// Gives one seat its own difficulty level; false, with nothing changed, for a name that is
    /// no level (`EngineGame.set_difficulty`).
    fn set_difficulty(&self, py: Python<'_>, pid: i64, name: &str) -> PyResult<bool> {
        Ok(self.write(py, |g| {
            let p = player_of(g, pid)?;
            Ok(g.set_difficulty(p, name))
        })?)
    }

    // ---- Scenario, map and debug commands --------------------------------------------------

    /// Applies scenario operations in order, all or nothing (`EngineGame.apply_ops`): what each
    /// said, and the events. `ActionError` names the operation that failed.
    fn apply_ops(&self, py: Python<'_>, ops_json: &[u8]) -> PyResult<(Bytes, Bytes)> {
        Ok(self.write(py, |g| {
            let ops = parse(ops_json, "The operations")?;
            let (done, batch) = g.apply_ops(&ops)?;
            Ok((Bytes(to_py_json(&done)), events_json(g, &batch)))
        })?)
    }

    /// The scenario editor's summary: civilizations, relations, cities.
    fn scenario_overview(&self, py: Python<'_>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| Ok(Bytes(to_py_json(&g.scenario_overview()))))?)
    }

    /// Seat types for a scenario, from how its civilizations were being played.
    fn default_seats(&self, py: Python<'_>) -> PyResult<Bytes> {
        Ok(self
            .read(py, |g| Ok(Bytes(to_py_json(&citar_engine::api::scenario::default_seats(g)))))?)
    }

    /// A scenario's seat list checked against its civilizations. `ActionError` for one that
    /// does not fit.
    #[pyo3(signature = (seats_json = None))]
    fn normalize_seats(&self, py: Python<'_>, seats_json: Option<&[u8]>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let seats = parse_opt(seats_json, "The seats")?;
            let given = (!seats.is_null()).then_some(&seats);
            Ok(Bytes(to_py_json(&citar_engine::api::scenario::normalize_seats(g, given)?)))
        })?)
    }

    /// The game's terrain as a map to play again (`EngineGame.export_map`).
    #[pyo3(signature = (name = ""))]
    fn export_map(&self, py: Python<'_>, name: &str) -> PyResult<Bytes> {
        Ok(self.read(py, |g| Ok(Bytes(to_py_json(&g.export_map(name)))))?)
    }

    /// The route a move order would take for one of `pid`'s units: `{"path": [[x, y], ...],
    /// "turns"}`, or `{"path": null}` (`EngineGame.path_preview`).
    fn path_preview(
        &self,
        py: Python<'_>,
        pid: i64,
        unit_id: i64,
        x: i32,
        y: i32,
    ) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let none = json!({"path": null});
            let (Ok(p), Some(u)) =
                (u8::try_from(pid), u32::try_from(unit_id).ok().and_then(UnitId::new))
            else {
                return Ok(Bytes(to_py_json(&none)));
            };
            Ok(Bytes(to_py_json(&g.path_preview(PlayerId(p), u, x, y))))
        })?)
    }

    /// Whether two civilizations know each other.
    fn has_met(&self, py: Python<'_>, a: i64, b: i64) -> PyResult<bool> {
        Ok(self.read(py, |g| Ok(g.has_met(player_of(g, a)?, player_of(g, b)?)))?)
    }

    /// Makes two civilizations meet, with everything a first contact brings. The events.
    fn meet(&self, py: Python<'_>, a: i64, b: i64) -> PyResult<Bytes> {
        Ok(self.write(py, |g| {
            let (a, b) = (player_of(g, a)?, player_of(g, b)?);
            let batch = g.meet(a, b)?;
            Ok(events_json(g, &batch))
        })?)
    }

    /// Makes it `pid`'s turn now and starts it (a probe's single-turn case). The events.
    fn force_turn(&self, py: Python<'_>, pid: i64) -> PyResult<Bytes> {
        Ok(self.write(py, |g| {
            let batch = g.force_turn(player_of(g, pid)?)?;
            Ok(events_json(g, &batch))
        })?)
    }

    /// Ends `pid`'s turn as the host does, playing on to the next major civilization's. The
    /// events. `ActionError` when it is not `pid`'s turn or the game is over.
    fn end_turn(&self, py: Python<'_>, pid: i64) -> PyResult<Bytes> {
        Ok(self.write(py, |g| {
            let batch = g.end_turn(player_of(g, pid)?)?;
            Ok(events_json(g, &batch))
        })?)
    }

    /// A developer shortcut: `meet_all`, `reveal` or `gold`. The events. `ValueError` for
    /// anything else.
    fn debug(&self, py: Python<'_>, action: &str) -> PyResult<Bytes> {
        Ok(self.write(py, |g| {
            let a = DebugAction::from_name(action)
                .ok_or_else(|| Failure::Value("Unknown debug action.".to_owned()))?;
            let batch = g.debug(a)?;
            Ok(events_json(g, &batch))
        })?)
    }

    // ---- The replay ------------------------------------------------------------------------

    /// Everything the recap needs, as JSON bytes, the frames `full` (Python's shape) or `delta`
    /// (as stored) (`EngineGame.replay_data`).
    #[pyo3(signature = (format = "full"))]
    fn replay_data(&self, py: Python<'_>, format: &str) -> PyResult<Bytes> {
        let f = match format {
            "full" => ReplayFormat::Full,
            "delta" => ReplayFormat::Delta,
            other => return Err(Failure::Value(format!("Unknown replay format '{other}'.")).into()),
        };
        Ok(self.read(py, |g| Ok(Bytes(g.replay_data(f))))?)
    }

    /// The replay as the server serves it (`/replay`): `extra_json`'s `id` and `name` first, then
    /// `replay_data`'s keys, each player with its `seat` from `extra_json`'s `seats` (by player
    /// id, `null` for none).
    #[pyo3(signature = (extra_json = None))]
    fn replay_json(&self, py: Python<'_>, extra_json: Option<&[u8]>) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let mut extra = match parse_opt(extra_json, "The replay's extra keys")? {
                Value::Null => Map::new(),
                Value::Object(m) => m,
                _ => {
                    return Err(Failure::Value(
                        "The replay's extra keys must be an object.".to_owned(),
                    ));
                }
            };
            let seats = extra.shift_remove("seats").unwrap_or(Value::Null);
            let mut data = g.replay_json(ReplayFormat::Full);
            if let Some(players) = data.get_mut("players").and_then(Value::as_array_mut) {
                for (i, p) in players.iter_mut().enumerate() {
                    if let Some(m) = p.as_object_mut() {
                        m.insert("seat".into(), seats.get(i).cloned().unwrap_or(Value::Null));
                    }
                }
            }
            let mut out = Map::new();
            for k in ["id", "name"] {
                out.insert(k.into(), extra.shift_remove(k).unwrap_or(Value::Null));
            }
            if let Value::Object(m) = data {
                out.extend(m);
            }
            out.extend(extra);
            Ok(Bytes(to_py_json(&out)))
        })?)
    }

    // ---- Bots ------------------------------------------------------------------------------

    /// Plays the seats `bots` drives, turn after turn, until the host has something to do or
    /// the game is over (`Game::drive`); `seat_limit` driven turns at most, 0 for no limit.
    /// Returns the stop (`{"stop", "player", "negotiations"}`), the events, and each bot's
    /// actions as `{pid: {tool: [taken, refused]}}`. Each handle's spec is taken once, at the
    /// start: a `set_diplomacy` during the drive applies to the next one.
    #[pyo3(signature = (bots, seat_limit = 0))]
    fn drive(
        &self,
        py: Python<'_>,
        bots: &Bound<'_, PyDict>,
        seat_limit: u32,
    ) -> PyResult<(Bytes, Bytes, Bytes)> {
        let specs = crate::bot::seat_bots(bots)?;
        Ok(self.write(py, |g| {
            let mut drivers: Vec<(PlayerId, Driver)> = Vec::with_capacity(specs.len());
            for (pid, spec) in specs {
                drivers.push((player_of(g, pid)?, Driver::new(spec)));
            }
            let mut d = Drivers::none(g.state().players().len());
            for (p, b) in &mut drivers {
                d = d.with(*p, b as &mut dyn SeatDriver);
            }
            let (stop, batch) =
                g.drive(&mut d, DriveOptions::default().with_seat_limit(seat_limit))?;
            drop(d);
            let nids: Vec<u32> = stop.negotiations().iter().map(|n| n.get()).collect();
            let stop = json!({
                "stop": stop.name(),
                "player": stop.player().map(|p| p.0),
                "negotiations": nids,
            });
            Ok((Bytes(to_py_json(&stop)), events_json(g, &batch), action_counts(&drivers)))
        })?)
    }

    /// Puts negotiation `nid`, which waits on `pid`, to `bot` (`Game::answer`, the session's
    /// responder for a bot seat): `done` or `deferred` (left to the seat's model), the events,
    /// and the bot's actions as `{tool: [taken, refused]}`. `ActionError` when the negotiation
    /// does not wait on `pid`, which a responder that lost a race ignores.
    fn answer(
        &self,
        py: Python<'_>,
        pid: i64,
        nid: i64,
        bot: &Bound<'_, Bot>,
    ) -> PyResult<(&'static str, Bytes, Bytes)> {
        let spec = bot.get().snapshot();
        Ok(self.write(py, |g| {
            let p = caller_of(pid)?;
            let id = negotiation_of(g, nid)?;
            let mut driver = Driver::new(spec);
            let (outcome, batch) = g.answer(p, id, &mut driver)?;
            let outcome = if outcome == DriverOutcome::Deferred { "deferred" } else { "done" };
            Ok((outcome, events_json(g, &batch), Bytes(to_py_json(&driver.refusals().to_json()))))
        })?)
    }

    /// What `bot` makes of `pid`'s diplomatic situation, about negotiation `nid` if given, for a
    /// language model to weigh (`EngineGame.bot_advice`): plain data that changes nothing.
    #[pyo3(signature = (pid, bot, nid = None))]
    fn bot_advice(
        &self,
        py: Python<'_>,
        pid: i64,
        bot: &Bound<'_, Bot>,
        nid: Option<i64>,
    ) -> PyResult<Bytes> {
        let spec = bot.get().snapshot();
        Ok(self.read(py, |g| {
            let p = player_of(g, pid)?;
            let id = nid.map(|n| negotiation_of(g, n)).transpose()?;
            Ok(Bytes(to_py_json(&citar_bot::advice(g, p, &spec, id))))
        })?)
    }

    // ---- Tests -----------------------------------------------------------------------------

    /// What a rule script reads of the game (`api::inspect`): `query_json` is `{"what": ...}`.
    /// With the `test-ops` feature only.
    #[cfg(feature = "test-ops")]
    fn inspect(&self, py: Python<'_>, query_json: &[u8]) -> PyResult<Bytes> {
        Ok(self.read(py, |g| {
            let q = parse(query_json, "The query")?;
            Ok(Bytes(to_py_json(&citar_engine::api::inspect::inspect(g, &q)?)))
        })?)
    }

    /// Applies test operations in order, all or nothing (`api::testops`): what each did, and the
    /// events. `ActionError` names the operation that failed. With the `test-ops` feature only.
    #[cfg(feature = "test-ops")]
    fn test_ops(&self, py: Python<'_>, ops_json: &[u8]) -> PyResult<(Bytes, Bytes)> {
        Ok(self.write(py, |g| {
            let ops = parse(ops_json, "The test operations")?;
            let (done, batch) = citar_engine::api::testops::apply(g, &ops)?;
            Ok((Bytes(to_py_json(&done)), events_json(g, &batch)))
        })?)
    }

    /// Turns every check of DESIGN.md 9.4 on (the invariants and the cache oracle at every
    /// settle) or back to the build's default; with the `test-ops` feature only.
    #[cfg(feature = "test-ops")]
    fn set_checks(&self, py: Python<'_>, on: bool) -> PyResult<()> {
        use citar_engine::game::DebugOptions;
        Ok(self.write(py, |g| {
            g.set_debug_options(if on { DebugOptions::ALL } else { DebugOptions::default() });
            Ok(())
        })?)
    }

    fn __repr__(&self) -> String {
        let h = self.heads();
        format!("<citar._engine.Game turn {} {}>", h.turn, phase_name(h.phase))
    }
}

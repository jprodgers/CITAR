//! What a host reads of a game in the facade's shapes, and the small copy of where a game stands
//! that it reads without waiting for it (DESIGN.md P2.6.1-P2.6.2; package 2-06a).
//!
//! Design §3.2 planned an `api/summary.rs` that was never built; these are its reads, as engine
//! functions with engine tests, so the bindings stay a thin layer of calls. Each replaces a
//! method of `EngineGame` (`citar/engine_api.py`) and keeps its keys:
//! - [`Heads`] and [`NegotiationHead`]: `turn`, `current`, `phase`, `winner`, `victory`,
//!   `turn_limit`, `is_alive`, `negotiation_head` and `open_negotiation_heads`
//!   (`engine_api.py:83-86, 413-441, 475-477, 553-564`), which the bindings publish after every
//!   call that changes the game and serve with the GIL held;
//! - [`Game::summary_json`], [`Game::player_row`] and [`Game::majors_json`]: `summary`,
//!   `player` and `majors` (`engine_api.py:448-481`);
//! - [`Game::lobby_config`]: `config`, the settings as `Game.new` normalised them
//!   (`game.py:32-60, 145-196`);
//! - [`Game::negotiation_record`] and [`Game::negotiation_records`]: `negotiation`,
//!   `negotiations` and `open_negotiations` (`engine_api.py:540-569`), in the stored form;
//! - [`Game::event_by_id`], [`Game::event_rows`], [`Game::stats_rows`] and
//!   [`Game::thought_rows`]: `event_view`'s event, `events`, `stats` and `thoughts`
//!   (`engine_api.py:499-503, 631-655`), as Python's rows;
//! - [`Game::save_whole`]: `to_save` and `state_dict` until the v2 saves of package 2-11, the
//!   state and its whole history as one journal chunk.

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::views::client::stats_row_json;
use super::views::{players::kind_name, stored_negotiation, thought_json};
use crate::base::ids::{EventId, NegotiationId, PlayerId, Turn};
use crate::game::Game;
use crate::game::victory::won_by;
use crate::save::journal::{self, JournalCursor};
use crate::save::{SaveError, to_json};
use crate::state::Phase;
use crate::state::chronicle::Event;
use crate::state::config::{AiBaseValues, MapSource, ResourceKindOptions, ResourceRule};
use crate::state::diplo::{NegStatus, Negotiation};
use crate::state::players::AutoDecision;

/// A phase as Python named it: `playing`, `over`.
#[must_use]
pub const fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Playing => "playing",
        Phase::Over => "over",
    }
}

/// Where a negotiation stands, without the history and proposals that make a copy of it cost
/// (`engine_api._head`): cheap enough to poll in a wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct NegotiationHead {
    pub id: u32,
    pub initiator: u8,
    pub responder: u8,
    /// `open`, `accepted`, ...
    pub status: &'static str,
    /// Whose move it is while it is open.
    pub awaiting: Option<u8>,
    /// How many history entries it has: a new one means somebody spoke.
    pub entries: u32,
}

impl NegotiationHead {
    /// Where `n` stands.
    #[must_use]
    pub fn of(n: &Negotiation) -> Self {
        Self {
            id: n.id.get(),
            initiator: n.initiator.0,
            responder: n.responder.0,
            status: n.status.name(),
            awaiting: n.awaiting.map(|p| p.0),
            entries: u32::try_from(n.history.len()).unwrap_or(u32::MAX),
        }
    }

    /// Whether `pid` is a party to it; every negotiation is when `pid` is `None`.
    #[must_use]
    pub fn involves(&self, pid: Option<PlayerId>) -> bool {
        pid.is_none_or(|p| self.initiator == p.0 || self.responder == p.0)
    }
}

/// The small copy of where a game stands that a host reads without waiting for the game
/// (DESIGN.md P2.6.2). The bindings take one at the end of every call that changes the game,
/// still under its lock, and serve the facade's cheap reads from it, so a server's lobby, queue
/// and pool never wait for a bot's turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heads {
    pub turn: Turn,
    /// Whose turn it is.
    pub current: PlayerId,
    pub phase: Phase,
    pub winner: Option<PlayerId>,
    /// How the game was won, by the victory's name (`Neutral` for a winner with none).
    pub victory: Option<&'static str>,
    /// The turn the game ends on.
    pub turn_limit: Turn,
    /// The game's revision ([`Game::rev`]): it moves on every write.
    pub revision: u64,
    /// Whether each player, by id, is still in the game.
    pub alive: Vec<bool>,
    /// The open negotiations, oldest first.
    pub open: Vec<NegotiationHead>,
    /// Why the game was stopped, if a panic poisoned it.
    pub poisoned: Option<Box<str>>,
}

impl Heads {
    /// Whether player `pid` is in the game and alive; `None` for a player the game lacks.
    #[must_use]
    pub fn is_alive(&self, pid: PlayerId) -> Option<bool> {
        self.alive.get(usize::from(pid.0)).copied()
    }

    /// The open negotiations `pid` is a party to (all of them for `None`), oldest first.
    pub fn open_heads(&self, pid: Option<PlayerId>) -> impl Iterator<Item = &NegotiationHead> {
        self.open.iter().filter(move |h| h.involves(pid))
    }

    /// The open negotiation `nid`, if it is open.
    #[must_use]
    pub fn open_head(&self, nid: u32) -> Option<&NegotiationHead> {
        self.open.iter().find(|h| h.id == nid)
    }
}

/// A game's state and its whole history, as one save until the v2 container of package 2-11
/// (DESIGN.md P2.7.1): what [`Game::load`] reads back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WholeSave {
    /// The state: format v1 JSON.
    pub state: Vec<u8>,
    /// The whole history as one journal chunk, numbered 0; `None` for a game with none yet.
    pub history: Option<Vec<u8>>,
}

/// Python's defaults for the two settings the server reads from a game's configuration
/// (`DEFAULT_CONFIG`, `game.py:56-57`): the engine keeps the lobby's own values with the host's
/// keys, and falls back to these when it gave none.
fn server_defaults() -> [(&'static str, Value); 2] {
    [("on_disconnect", json!("pause")), ("reconnect_seconds", json!(180))]
}

impl Game {
    /// Where the game stands, for the host's cheap reads (DESIGN.md P2.6.2).
    #[must_use]
    pub fn heads(&self) -> Heads {
        let c = self.state().clock();
        Heads {
            turn: c.turn,
            current: c.current,
            phase: c.phase,
            winner: c.winner,
            victory: won_by(self).map(|w| w.name(self.rules())),
            turn_limit: self.total_turns(),
            revision: self.rev(),
            alive: self.state().players().iter().map(|(_, p)| p.alive()).collect(),
            open: self
                .negotiations()
                .iter()
                .filter(|n| n.status == NegStatus::Open)
                .map(NegotiationHead::of)
                .collect(),
            poisoned: self.poisoned().map(Into::into),
        }
    }

    /// Where negotiation `nid` stands, open or not (`EngineGame.negotiation_head`); `None` for an
    /// id the game lacks.
    #[must_use]
    pub fn negotiation_head(&self, nid: NegotiationId) -> Option<NegotiationHead> {
        self.negotiation(nid).map(NegotiationHead::of)
    }

    /// One negotiation as the state keeps it (`EngineGame.negotiation`): its id, parties, turn,
    /// status, whose move it is, the proposal and who made it, the history and `exchanges`, and
    /// the deal it concluded; `None` for an id the game lacks.
    #[must_use]
    pub fn negotiation_record(&self, nid: NegotiationId) -> Option<Value> {
        self.negotiation(nid).map(|n| stored_negotiation(self, n))
    }

    /// The negotiations `pid` is a party to (every one for `None`), oldest first, as
    /// [`negotiation_record`](Self::negotiation_record) gives each: only the open ones with
    /// `open_only` (`EngineGame.open_negotiations`), else settled ones too
    /// (`EngineGame.negotiations`).
    #[must_use]
    pub fn negotiation_records(&self, pid: Option<PlayerId>, open_only: bool) -> Vec<Value> {
        self.negotiations()
            .iter()
            .filter(|n| !open_only || n.status == NegStatus::Open)
            .filter(|n| pid.is_none_or(|p| n.initiator == p || n.responder == p))
            .map(|n| stored_negotiation(self, n))
            .collect()
    }

    /// One civilization's public identity and settings (`EngineGame.player`,
    /// `engine_api.py:448-456`): `id`, `kind`, `name`, `color`, `leader`, `nation`, `alive`,
    /// `eliminated_turn`, `controller`, `handicap`, `auto`, `overrides` (the handicap and
    /// automatic decisions its seat set explicitly), `difficulty` (a major's falls back to the
    /// game's) and `founded_city`; `None` for a player the game lacks.
    #[must_use]
    pub fn player_row(&self, pid: PlayerId) -> Option<Value> {
        let p = self.player(pid)?;
        let r = self.rules();
        let seat = p.seat();
        let auto = seat.auto();
        let over = seat.overrides();
        let mut overrides = Map::new();
        if let Some(h) = over.handicap {
            overrides.insert("handicap".into(), json!(h.name()));
        }
        if !over.auto.is_empty() {
            let set: Map<String, Value> = AutoDecision::ALL
                .into_iter()
                .filter_map(|d| over.auto.get(d).map(|on| (d.name().to_owned(), json!(on))))
                .collect();
            overrides.insert("auto".into(), Value::Object(set));
        }
        let difficulty =
            seat.difficulty().or_else(|| p.is_major().then(|| self.state().config().difficulty));
        Some(json!({
            "id": pid.0,
            "kind": kind_name(p.kind),
            "name": &*p.name,
            "color": p.color.to_hex(),
            "leader": &*p.leader,
            "nation": r.name(p.nation),
            "alive": p.alive(),
            "eliminated_turn": p.eliminated_turn(),
            "controller": seat.controller().name(),
            "handicap": seat.handicap().name(),
            "auto": {"un_vote": auto.un_vote, "conquest": auto.conquest, "free_picks": auto.free_picks},
            "overrides": overrides,
            "difficulty": difficulty.and_then(|d| r.name(d)),
            "founded_city": p.founded_city,
        }))
    }

    /// The major civilizations, the eliminated ones too unless `alive_only`, as
    /// [`player_row`](Self::player_row) gives each (`EngineGame.majors`).
    #[must_use]
    pub fn majors_json(&self, alive_only: bool) -> Vec<Value> {
        let ids: Vec<PlayerId> =
            self.majors(alive_only).map(crate::state::players::Player::id).collect();
        ids.into_iter().filter_map(|p| self.player_row(p)).collect()
    }

    /// The game at a glance (`EngineGame.summary`, `engine_api.py:458-463`): `turn`, `current`,
    /// `phase`, `winner`, `victory`, `turn_limit` and every player as
    /// [`player_row`](Self::player_row) gives it.
    #[must_use]
    pub fn summary_json(&self) -> Value {
        let h = self.heads();
        let players: Vec<Value> =
            self.state().players().ids().filter_map(|p| self.player_row(p)).collect();
        json!({
            "turn": h.turn,
            "current": h.current.0,
            "phase": phase_name(h.phase),
            "winner": h.winner.map(|p| p.0),
            "victory": h.victory,
            "turn_limit": h.turn_limit,
            "players": players,
        })
    }

    /// The game's settings as Python's `Game.new` left its config dict (`EngineGame.config`;
    /// `DEFAULT_CONFIG` normalised, `game.py:32-60, 145-196`): every setting of the lobby by the
    /// name the lobby uses, rule objects by name, the configured turn limit and city-state count,
    /// the map's size, edges and wraps, `resources` as the lobby's object (`null` for the
    /// normal placement), the seats as the lobby sent them (`players`), and every other key the
    /// lobby sent, verbatim. An editor map adds its id (`map`) and its `map_type` is `custom`,
    /// with no `map_edges`.
    ///
    /// The server's own two settings, `on_disconnect` and `reconnect_seconds`, are the lobby's,
    /// or Python's defaults when it gave none, as `DEFAULT_CONFIG` filled them in.
    #[must_use]
    pub fn lobby_config(&self) -> Value {
        let r = self.rules();
        let st = self.state();
        let c = st.config();
        let k = r.constants();
        let map = st.map();
        let (size, map_type, edges, editor) = match &c.map {
            MapSource::Generated { size, map_type, edges, .. } => {
                (*size, &*k.map_types[*map_type].key, Some(edges.name()), None)
            }
            MapSource::Editor { id, size } => (*size, "custom", None, Some(&**id)),
        };
        let victories: Map<String, Value> = r
            .victories()
            .iter()
            .map(|(id, v)| (v.name.to_string(), json!(c.victory_enabled(id))))
            .collect();
        let mut m = Map::new();
        let mut put = |key: &str, v: Value| {
            m.insert(key.to_owned(), v);
        };
        put("map_size", json!(&*k.map_sizes[size].key));
        put("map_type", json!(map_type));
        put("width", json!(map.width));
        put("height", json!(map.height));
        put("seed", json!(c.seed));
        put("speed", json!(r.name(c.speed)));
        put("difficulty", json!(r.name(c.difficulty)));
        put("barbarian_difficulty", json!(r.name(c.barbarian_difficulty)));
        put(
            "ai_base_values",
            json!(match c.ai_base_values {
                AiBaseValues::Unciv => "unciv",
                AiBaseValues::Monotonic => "monotonic",
            }),
        );
        put("starting_era", json!(r.name(c.starting_era)));
        put("barbarians", json!(&*k.barbarian_levels[c.barbarians].key));
        put("barbarian_aggression", json!(c.barbarian_aggression));
        put("turn_limit", json!(c.turn_limit));
        put("victories", Value::Object(victories));
        put("city_states", json!(c.city_states));
        put("religion", json!(c.religion));
        put("espionage", json!(c.espionage));
        put("nuclear_weapons", json!(c.nuclear_weapons));
        put("tech_trading", json!(c.tech_trading));
        put("ruins", json!(c.ruins));
        put("map_edges", json!(edges));
        put("river_density", json!(c.river_density));
        put("resources", resources_json(self));
        let host = &c.host;
        for (key, default) in server_defaults() {
            put(key, host.get(key).cloned().unwrap_or(default));
        }
        put("players", host.get("players").cloned().unwrap_or_else(|| json!([])));
        if let Some(id) = editor {
            put("map", json!(id));
        }
        if let Some(n) = c.diplomacy.max_chat_messages {
            put("diplomacy", json!({"max_chat_messages": n}));
        }
        for (key, v) in host.iter() {
            if !m.contains_key(key) {
                m.insert(key.clone(), v.clone());
            }
        }
        m.insert("wrap_x".into(), json!(map.wrap_x));
        m.insert("wrap_y".into(), json!(map.wrap_y));
        Value::Object(m)
    }

    /// The event numbered `id`, if the game's history has it: what a host scrubs for a viewer
    /// (`EngineGame.event_view`).
    #[must_use]
    pub fn event_by_id(&self, id: EventId) -> Option<&Event> {
        let all = self.chronicle().events();
        all.binary_search_by_key(&id, |e| e.id).ok().map(|i| &all[i])
    }

    /// Every event as it happened, unscrubbed, oldest first, in Python's dict
    /// (`EngineGame.events`): the last `last` of them, or all for `None` or 0.
    #[must_use]
    pub fn event_rows(&self, last: Option<usize>) -> Vec<Value> {
        let all = self.chronicle().events();
        let n = last.filter(|&n| n > 0).unwrap_or(all.len());
        all[all.len().saturating_sub(n)..].iter().map(|e| self.event_json(e, None)).collect()
    }

    /// The rounds' statistics rows as Python's dicts (`EngineGame.stats`): the last `last` of
    /// them, or all for `None` or 0.
    #[must_use]
    pub fn stats_rows(&self, last: Option<usize>) -> Vec<Value> {
        self.stats(last).iter().map(stats_row_json).collect()
    }

    /// The recorded thoughts from position `since` on, only `pid`'s if given, as Python's dicts
    /// (`EngineGame.thoughts`).
    #[must_use]
    pub fn thought_rows(&self, pid: Option<PlayerId>, since: usize) -> Vec<Value> {
        self.thoughts(pid, since).into_iter().map(thought_json).collect()
    }

    /// The game as one save until package 2-11's container (`EngineGame.to_save` on Rust,
    /// DESIGN.md P2.7.1): the state, and its whole history as one journal chunk, which
    /// [`Game::load`] reads back to the same game. The game's own journal cursor does not move,
    /// so a host that also takes chunks ([`Game::take_journal_chunk`]) misses none.
    ///
    /// # Errors
    /// A state that does not write, which the engine's checks rule out.
    pub fn save_whole(&self) -> Result<WholeSave, SaveError> {
        let mut st = self.state().clone();
        let mut cursor = JournalCursor::default();
        let host = &mut st.heads_mut().1;
        host.journal_seq = 0;
        let chunk = journal::take_chunk(self.rules(), self.chronicle(), &mut cursor, host)?;
        let state = to_json(self.rules(), &st)?;
        Ok(WholeSave { state, history: chunk.map(|c| c.json) })
    }
}

/// The lobby's `resources` object for a game's resource placement, `null` for the normal one
/// (`MapOptions.__init__`, `mapgen.py:77-93`, read the other way).
fn resources_json(g: &Game) -> Value {
    let r = g.rules();
    let res = &g.state().config().resources;
    if *res == crate::state::config::ResourceOptions::default() {
        return Value::Null;
    }
    let kind = |o: &ResourceKindOptions| {
        let each: Map<String, Value> = o
            .each
            .iter()
            .filter_map(|&(id, rule)| {
                let v = match rule {
                    ResourceRule::Off => json!({"mode": "off"}),
                    ResourceRule::Cap(n) => json!({"mode": "cap", "value": n}),
                    ResourceRule::Share(n) => json!({"mode": "share", "value": n}),
                };
                r.name(id).map(|name| (name.to_owned(), v))
            })
            .collect();
        json!({"density": o.density, "each": each})
    };
    json!({
        "density": res.density,
        "strategic": kind(&res.strategic),
        "luxury": kind(&res.luxury),
        "bonus": kind(&res.bonus),
    })
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;

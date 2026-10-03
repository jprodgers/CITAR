//! What a finished run reports (`headless.result`, `citar/bots/headless.py:65-84`): how the game
//! ended, its statistics a row per round, and each civilization's standing, in the shape of
//! `engine_api.run_game`'s dict, so the lab, `citar sim` and the balance runner read either
//! backend's result alike. `scripts/bots/run_game_keys.py` records that shape from Python, and
//! `tests/run_game.rs` holds this one to it.

use citar_engine::api::views::client::stats_row_json;
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::game::Game;
use citar_engine::game::victory::{score, won_by};
use citar_engine::state::Phase;
use citar_engine::state::players::{Player, PlayerKind};
use serde::Serialize;
use serde_json::{Map, Value};

/// A major civilization's standing at the end (`headless.result`'s extra keys), in Python's
/// order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MajorRow {
    /// The seat's difficulty: its own, else the game's (Python's `Player.difficulty`, which
    /// `Game.new` filled in the same way, `game.py:247`).
    pub difficulty: String,
    /// Technologies known.
    pub techs: u32,
    pub future_techs: i32,
    /// Policies adopted, branches and finishers included (`len(p.policies)`).
    pub policies: u32,
    /// How far its religion got: `none`, `pantheon`, `founding`, `religion`, `enhancing` or
    /// `enhanced` (`Player.religion_state`).
    pub religion: &'static str,
    /// Great people earned.
    pub great_people: i32,
    pub cities: u32,
    /// The spaceship parts added, by name; `null` before the first (Python had no entry until
    /// then).
    pub spaceship: Value,
    /// 0 once eliminated, whatever it had built.
    pub score: i32,
}

/// One civilization at the end of a run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlayerRow {
    pub id: u8,
    /// `major`, `city_state` or `barbarian`.
    pub kind: &'static str,
    pub name: String,
    pub alive: bool,
    /// For a major civilization.
    #[serde(flatten)]
    pub major: Option<MajorRow>,
}

/// How a game went: `engine_api.run_game`'s dict, its keys in Python's order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunResult {
    /// The turn the game stopped on.
    pub turn: Turn,
    /// Turns played: the turn before it.
    pub turns: Turn,
    /// `playing` or `over`: a game a crash stopped is still `playing`, as a Python run stopped
    /// by its error limit was.
    pub phase: &'static str,
    pub winner: Option<u8>,
    /// The victory's name (`Time`, `Scientific`, ..., `Neutral` for a win no victory type
    /// stands for).
    pub victory: Option<String>,
    /// The game's last turn: the configuration's, else the speed's.
    pub turn_limit: Turn,
    /// A row per round, as `EngineGame.stats` gives them.
    pub stats: Value,
    /// Every civilization, by id.
    pub players: Vec<PlayerRow>,
    /// A line per crash, with where it happened when the panic hook saw it.
    pub errors: Vec<String>,
}

impl RunResult {
    /// The result of `g` as it stands, with `errors`.
    #[must_use]
    pub fn of(g: &Game, errors: Vec<String>) -> Self {
        let st = g.state();
        let clock = st.clock();
        let rules = g.rules();
        Self {
            turn: clock.turn,
            turns: clock.turn - 1,
            phase: match clock.phase {
                Phase::Playing => "playing",
                Phase::Over => "over",
            },
            winner: clock.winner.map(|p| p.0),
            victory: won_by(g).map(|w| w.name(rules).to_owned()),
            turn_limit: st.config().turn_limit,
            stats: Value::Array(g.stats(None).iter().map(stats_row_json).collect()),
            players: st.players().iter().map(|(id, p)| player_row(g, id, p)).collect(),
            errors,
        }
    }
}

/// `kind` as Python wrote it.
#[must_use]
pub const fn kind_name(kind: PlayerKind) -> &'static str {
    match kind {
        PlayerKind::Major => "major",
        PlayerKind::CityState => "city_state",
        PlayerKind::Barbarian => "barbarian",
    }
}

fn player_row(g: &Game, id: PlayerId, p: &Player) -> PlayerRow {
    PlayerRow {
        id: id.0,
        kind: kind_name(p.kind),
        name: p.name.to_string(),
        alive: p.alive(),
        major: p.is_major().then(|| major_row(g, id, p)),
    }
}

fn major_row(g: &Game, id: PlayerId, p: &Player) -> MajorRow {
    let rules = g.rules();
    let spaceship = p.major.as_ref().map(|m| &m.spaceship).filter(|s| !s.is_empty()).map_or(
        Value::Null,
        |parts| {
            Value::Object(
                parts
                    .iter()
                    .map(|(&u, &n)| (rules.name(u).unwrap_or("?").to_owned(), Value::from(n)))
                    .collect::<Map<String, Value>>(),
            )
        },
    );
    MajorRow {
        difficulty: rules.name(g.seat_difficulty(Some(id))).unwrap_or("?").to_owned(),
        techs: count(p.tech.known.len()),
        future_techs: p.tech.future_techs,
        policies: count(p.policy.adopted.len()),
        religion: p.religion.progress.name(),
        great_people: p.gp.earned,
        cities: count(g.player_cities(id).count()),
        spaceship,
        score: if p.alive() { score(g, id).total } else { 0 },
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

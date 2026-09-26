//! The strict converter from Python's `GameState.to_dict()` (DESIGN.md 4.12, package 1a-10).
//!
//! [`state_from_python`] reads a state the Python engine wrote (`citar/engine/state.py:315-405`,
//! the `state` of a save or a refcheck fixture) into a [`State`] and the [`Chronicle`] of its
//! history. It is strict: an unresolved name, an unknown key, an unknown event type or a NaN fails
//! the conversion with its JSON path, and there is no lenient mode.
//!
//! The mapping (DESIGN.md 4.4-4.7):
//! - tiles are 14-tuples in `Tile._ORDER` (`state.py:84-85`); `fallout` is OR-ed into the
//!   features, as nukes already appended Fallout there (`combat.py:1203-1215`);
//! - units, cities and camps are keyed by their ids as text, and keep their ids; the three id
//!   counters start at Python's shared `next_id`, and deals and negotiations count on from
//!   their lists (`diplomacy.py:540, 781`);
//! - the 28 keys of `Player.flags` become typed fields; the explorers' keys move to their units,
//!   `Player.free_buildings` to the cities, and `GameState.spaceship` to the majors;
//! - a quest's `data1` becomes its `QuestTarget`, and religions, keyed by name in Python, become
//!   `ReligionId`s in founding order;
//! - `relations["a,b"]` and `open_borders["a>b"]` fold into one relation per pair, with contact
//!   from the players' `met` lists; opinions move to their holders, a scenario's `"a>b"` key
//!   included (`scenario.py:429`);
//! - events, messages, thoughts and stats rows move to the chronicle, their name references from
//!   code points to UTF-8 byte offsets, and the engine heads are hashed over them;
//! - the fields Python lacks start as DESIGN.md 4.12 says: `citizens_settled` false,
//!   `last_gold_rate` 0 (unless Python had one), `happiness_seen` 0, `combat_seq` 0, no driver
//!   memory.
//!
//! What is dropped is counted in the [`ConvertReport`], field by field ([`Dropped`]): the dead
//! fields of DESIGN.md 4.4-4.6, the barbarians' explored tiles and the non-majors' memories, which
//! nothing read, the explorer state of units that are gone, free buildings of cities their holder
//! no longer has, and the order of lists that are sets. Nothing else is: the converted state is
//! checked by `State::from_parts` and `save::validate` before it is returned, so it saves and
//! loads.

mod config;
mod diplo;
mod entities;
mod history;
mod names;
mod players;
mod read;
mod world;

use core::fmt;

use serde_json::Value;

use crate::base::sets::{PlayerSet, PlayerVec};
use crate::rules::Ruleset;
use crate::save::validate;
use crate::state::chronicle::Chronicle;
use crate::state::cities::Cities;
use crate::state::config::HostOnly;
use crate::state::{State, StateParts};

use read::{Obj, Path, Res};

/// Why a Python state could not be converted: where, as a JSON path, and what.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {message}")]
pub struct ConvertError {
    /// The place in the state: `players[3].flags.pairs["5"].wary`, `$` for the whole of it.
    pub path: String,
    /// What is wrong there.
    pub message: String,
}

/// A field, or part of one, the conversion dropped, and why (DESIGN.md 4.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dropped {
    /// Python's Mersenne Twister state: every draw is keyed from the seed (DESIGN.md 7).
    RngState,
    /// `GameState.barbarian_state`, which nothing outside `state.py` read or wrote.
    BarbarianState,
    /// `GameState.capture_ids`, which nothing outside `state.py` read or wrote.
    CaptureIds,
    /// `GameState.first_discovered`, which nothing outside `state.py` read or wrote.
    FirstDiscovered,
    /// `flags["last_stats"]`, written at `turns.py:90` and never read.
    LastStats,
    /// `flags["unreachable_explore"]`, which the explorer only ever cleared.
    UnreachableExplore,
    /// An explorer's target or recent tiles, kept in `flags` for a unit that is gone.
    ExplorerGone,
    /// `Player.cs_unit_timer`, never read.
    CsUnitTimer,
    /// `Player.tribute_turn`, never read.
    TributeTurn,
    /// `Player.ruins_rewards`, never read.
    RuinsRewards,
    /// `Player.spy_eras`, never read (`flags["eras_spy_earned"]` is the one read).
    SpyEras,
    /// `Player.faith_buys`, never read.
    FaithBuys,
    /// The float `Player.gp_threshold`, never read (the pools' thresholds are in `flags`).
    GpThreshold,
    /// The barbarians' explored tiles: they keep none (DESIGN.md 4.3).
    BarbarianExplored,
    /// A city-state's or the barbarians' memory of tiles, which Python recorded and never read
    /// (`visibility.py:141-146`): only majors remember (DESIGN.md 4.3).
    MinorMemory,
    /// `Player.free_buildings` of a city the player no longer holds, which Python no longer
    /// read (`cities.py:285-293`).
    FreeBuildingsElsewhere,
    /// `Unit.build`, never read.
    UnitBuild,
    /// `Unit.due_heal`, never read.
    UnitDueHeal,
    /// `City.spaceship_parts`, never read.
    CitySpaceshipParts,
    /// A negotiation's `exchanges` where it was not its history's length: only the archived bots
    /// read it (`diplomacy.py:728`).
    Exchanges,
    /// A quest's `data2`, which Python never filled.
    QuestData2,
    /// The order of a list that is a set: techs, policies, buildings, promotions, features,
    /// worked tiles, met civilizations and the like (DESIGN.md 4.5). Counted per list whose
    /// order was not the ruleset's or ascending.
    ListOrder,
}

impl Dropped {
    /// Every kind, in report order.
    pub const ALL: [Self; 22] = [
        Self::RngState,
        Self::BarbarianState,
        Self::CaptureIds,
        Self::FirstDiscovered,
        Self::LastStats,
        Self::UnreachableExplore,
        Self::ExplorerGone,
        Self::CsUnitTimer,
        Self::TributeTurn,
        Self::RuinsRewards,
        Self::SpyEras,
        Self::FaithBuys,
        Self::GpThreshold,
        Self::BarbarianExplored,
        Self::MinorMemory,
        Self::FreeBuildingsElsewhere,
        Self::UnitBuild,
        Self::UnitDueHeal,
        Self::CitySpaceshipParts,
        Self::Exchanges,
        Self::QuestData2,
        Self::ListOrder,
    ];

    /// The Python field, in the path grammar.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::RngState => "rng_state",
            Self::BarbarianState => "barbarian_state",
            Self::CaptureIds => "capture_ids",
            Self::FirstDiscovered => "first_discovered",
            Self::LastStats => "players[*].flags.last_stats",
            Self::UnreachableExplore => "players[*].flags.unreachable_explore",
            Self::ExplorerGone => "players[*].flags.explore_targets|explore_hist",
            Self::CsUnitTimer => "players[*].cs_unit_timer",
            Self::TributeTurn => "players[*].tribute_turn",
            Self::RuinsRewards => "players[*].ruins_rewards",
            Self::SpyEras => "players[*].spy_eras",
            Self::FaithBuys => "players[*].faith_buys",
            Self::GpThreshold => "players[*].gp_threshold",
            Self::BarbarianExplored => "players[*].explored",
            Self::MinorMemory => "players[*].memory",
            Self::FreeBuildingsElsewhere => "players[*].free_buildings",
            Self::UnitBuild => "units.*.build",
            Self::UnitDueHeal => "units.*.due_heal",
            Self::CitySpaceshipParts => "cities.*.spaceship_parts",
            Self::Exchanges => "negotiations[*].exchanges",
            Self::QuestData2 => "players[*].quests[*].data2",
            Self::ListOrder => "(the order of lists that are sets)",
        }
    }

    /// Why it was dropped.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::RngState => "every draw is keyed from the seed",
            Self::BarbarianState | Self::CaptureIds | Self::FirstDiscovered => {
                "nothing outside state.py read or wrote it"
            }
            Self::LastStats => "written, never read",
            Self::UnreachableExplore => "only ever cleared",
            Self::ExplorerGone => "the unit it was kept for is gone",
            Self::CsUnitTimer
            | Self::TributeTurn
            | Self::RuinsRewards
            | Self::SpyEras
            | Self::FaithBuys
            | Self::GpThreshold
            | Self::UnitBuild
            | Self::UnitDueHeal
            | Self::CitySpaceshipParts => "never read",
            Self::BarbarianExplored => "the barbarians keep no explored tiles",
            Self::MinorMemory => "only majors remember tiles; Python never read the others'",
            Self::FreeBuildingsElsewhere => "a city the player no longer holds",
            Self::Exchanges => "only the archived bots read it",
            Self::QuestData2 => "Python never filled it",
            Self::ListOrder => "sets have no order: views sort them",
        }
    }
}

/// What the conversion dropped, counted by [`Dropped`]. A field counts once per record that held
/// something there (a unit's `build` that was not `None`, a player's non-empty `faith_buys`),
/// not once per record that has the key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConvertReport {
    counts: [u32; Dropped::ALL.len()],
}

impl ConvertReport {
    fn note(&mut self, d: Dropped) {
        self.add(d, 1);
    }

    fn add(&mut self, d: Dropped, n: u32) {
        let slot = &mut self.counts[d as usize];
        *slot = slot.saturating_add(n);
    }

    /// How many were dropped of one kind.
    #[must_use]
    pub const fn count(&self, d: Dropped) -> u32 {
        self.counts[d as usize]
    }

    /// Every kind something was dropped of, with its count, in report order.
    pub fn dropped(&self) -> impl Iterator<Item = (Dropped, u32)> + '_ {
        Dropped::ALL.into_iter().map(|d| (d, self.count(d))).filter(|&(_, n)| n > 0)
    }

    /// Whether nothing was dropped.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counts.iter().all(|&n| n == 0)
    }

    /// Adds another report's counts, for a summary over many states.
    pub fn merge(&mut self, other: &Self) {
        for d in Dropped::ALL {
            self.add(d, other.count(d));
        }
    }
}

impl fmt::Display for ConvertReport {
    /// One line per kind dropped: `players[*].flags.last_stats: 7 (written, never read)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (d, n) in self.dropped() {
            writeln!(f, "{}: {n} ({})", d.field(), d.reason())?;
        }
        Ok(())
    }
}

/// A converted Python state: the state, the history, and what was dropped.
#[derive(Clone, Debug)]
pub struct Converted {
    pub state: State,
    pub chronicle: Chronicle,
    pub report: ConvertReport,
}

/// What every part of the conversion reads against: the ruleset, the game's players, map and
/// religions, and the report of what was dropped.
struct Cx<'a> {
    r: &'static Ruleset,
    /// The number of players.
    n: usize,
    /// The players' names, by id, which the UN's tallies key by (`victory.py:216`).
    player_names: Vec<&'a str>,
    /// The number of tiles, and of tiles per row.
    size: u32,
    width: u16,
    /// The founded religions' and pantheons' names, in founding order: a `ReligionId` is a
    /// position here.
    religions: Vec<&'a str>,
    report: ConvertReport,
}

/// The fields of `GameState.to_dict()` (`state.py:370-384`).
const TOP: [&str; 32] = [
    "config",
    "width",
    "height",
    "tiles",
    "players",
    "units",
    "cities",
    "turn",
    "current",
    "next_id",
    "rng_state",
    "phase",
    "winner",
    "victory",
    "relations",
    "open_borders",
    "deals",
    "negotiations",
    "messages",
    "thoughts",
    "events",
    "camps",
    "stats",
    "turn_started",
    "barbarian_state",
    "capture_ids",
    "religions",
    "wonders_built",
    "un",
    "spaceship",
    "continents",
    "first_discovered",
];

/// Converts the JSON of Python's `GameState.to_dict()` into a state, its history and a report of
/// what was dropped, strictly: see the module docs.
pub fn state_from_python(json: &[u8], rules: &'static Ruleset) -> Result<Converted, ConvertError> {
    let doc: Value = serde_json::from_slice(json).map_err(|e| {
        let offset = byte_offset(json, e.line(), e.column());
        ConvertError { path: read::path_at(json, offset), message: format!("not JSON: {e}") }
    })?;
    convert(&doc, rules)
}

/// The byte a serde_json error at `line` and `column` (both from 1) points at.
fn byte_offset(json: &[u8], line: usize, column: usize) -> usize {
    let mut start = 0;
    for _ in 1..line {
        match json.get(start..).and_then(|rest| rest.iter().position(|&b| b == b'\n')) {
            Some(nl) => start += nl + 1,
            None => break,
        }
    }
    start + column.saturating_sub(1)
}

/// The conversion of a parsed state.
fn convert(doc: &Value, rules: &'static Ruleset) -> Result<Converted, ConvertError> {
    let top = Obj::new(doc, Path::ROOT)?;
    for key in TOP {
        // Every key must be there: this is what to_dict writes, whole.
        top.req(key)?;
    }
    top.finish()?;

    let players_json = read::list(top.req("players")?, &top.at("players"))?;
    if players_json.len() > PlayerSet::CAPACITY {
        return Err(top
            .at("players")
            .err(format!("{} players; at most 64 fit", players_json.len())));
    }
    let (map, config) = config::map_and_config(&top, rules)?;
    let mut cx = Cx {
        r: rules,
        n: players_json.len(),
        player_names: players_json
            .iter()
            .map(|p| p.get("name").and_then(Value::as_str).unwrap_or_default())
            .collect(),
        size: map.size(),
        width: map.width,
        religions: Vec::new(),
        report: ConvertReport::default(),
    };
    for (drop, key) in [
        (Dropped::BarbarianState, "barbarian_state"),
        (Dropped::CaptureIds, "capture_ids"),
        (Dropped::FirstDiscovered, "first_discovered"),
    ] {
        dead(&mut cx, &top, key, drop)?;
    }
    if !read::is_none(top.get("rng_state")) {
        cx.report.note(Dropped::RngState);
    }

    let religions = world::religions(&mut cx, &top)?;
    let tiles = config::tiles(&mut cx, &top)?;
    let mut units = entities::units(&mut cx, &top)?;
    let mut cities = entities::cities(&mut cx, &top)?;
    let players = players::players(&mut cx, &top, &mut units, &mut cities)?;
    let diplo = diplo::diplomacy(&mut cx, &top, &players)?;
    let players: Vec<_> = players.into_iter().map(|p| p.player).collect();
    let world = world::world(&mut cx, &top, religions)?;
    let clock = world::clock(&cx, &top)?;
    let ids = world::ids(&top, &diplo)?;
    let (chronicle, heads, host) = history::history(&mut cx, &top)?;

    let units = entities::build_units(units, map.size(), top.path())?;
    let cities = Cities::from_cities(cities).map_err(|e| top.at("cities").err(e.to_string()))?;
    let state = State::from_parts(StateParts {
        config,
        map,
        tiles,
        players: PlayerVec::from_vec(players),
        units,
        cities,
        diplo,
        world,
        clock,
        ids,
        chronicle: heads,
        host: HostOnly(host),
    })
    .map_err(|e| Path::ROOT.err(e.to_string()))?;
    validate(&state, rules).map_err(|errs| {
        let first = errs.first().map_or_else(String::new, ToString::to_string);
        Path::ROOT.err(format!("the converted state does not validate: {first}"))
    })?;
    Ok(Converted { state, chronicle, report: cx.report })
}

/// Checks a record that must be empty, or counts it as dropped: a dead field that held nothing
/// drops nothing.
fn dead(cx: &mut Cx<'_>, o: &Obj<'_>, key: &str, drop: Dropped) -> Res<()> {
    let Some(v) = o.get(key) else { return Ok(()) };
    let empty = match v {
        Value::Null => true,
        Value::Object(m) => m.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    };
    if !empty {
        cx.report.note(drop);
    }
    Ok(())
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;

//! New-game setup (DESIGN.md 6.14): the lobby's settings normalised into a [`NewGame`]
//! ([`Game::config_from_json`], `game.py:151-196`), and [`Game::new`], which sets a game up from
//! one as a table of stages ([`SETUP`], `game.py:198-318`).
//!
//! Each stage is ported by the package that owns its system; one whose system is not ported is
//! `Pending` and an explicit no-op, and `inspect` lists it. Package 1b-03 ports the settings, the
//! nations, the players, the relations and the first turn, games on an editor map that gives
//! its start positions, and the starting techs, gold and culture that rule scripts rely on.
//! Package 1b-04 ports games on a generated map (`mapgen::generate`, `game.py:218-221`).
//!
//! What differs from Python, on purpose (`tests/rules/intended.toml`):
//! - the engine draws nothing and reads no file: the settings must carry a seed, and an editor
//!   map comes inline, which the host resolves from its id (DESIGN.md 8.1). Python drew a seed
//!   and read `saves/maps/<id>.json` (`game.py:156-157, 165-170`);
//! - a setting that names nothing the ruleset or the lobby has (a speed, a difficulty, an era, a
//!   map size, type or edges, a barbarian level, a victory, the AI base values, a controller, a
//!   nation) is refused, naming what is valid, where Python fell back to a default or kept a
//!   value no rule read; a seat's nation that is a city-state is refused where Python drew
//!   another at random. Resource rules still skip unknown resources, as Python's did on purpose
//!   (`mapgen.py:62-63`) (`config-refuses-unknown-names`);
//! - the nations are shuffled on their own stream, `Purpose::NationShuffle`, where Python drew
//!   them from the stream the map was then drawn from, so tuning map generation never
//!   reshuffles the nations (DESIGN.md 6.14).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::derive::rev::PlayerTouch;
use super::error::{ActionError, EngineError, ErrCode};
use super::events::EventBatch;
use super::{Game, Porting, pending, research};
use crate::base::hex::HexGrid;
use crate::base::ids::{
    BarbarianLevelId, DifficultyId, EraId, MapSizeId, MapTypeId, NationId, PlayerId, SpeedId,
    TechId, TileIdx, VictoryId,
};
use crate::base::py;
use crate::base::rng::{Purpose, Rng};
use crate::base::sets::PlayerVec;
use crate::mapgen::continents;
use crate::mapgen::document::{self, MapDocument};
use crate::mapgen::options::{self, MapOptions, MapType};
use crate::mapgen::{self, GenSpec};
use crate::rules::constants::PLAYER_COLORS;
use crate::rules::defs::NationKind;
use crate::rules::{Named, Ruleset};
use crate::state::State;
use crate::state::chronicle::{Chronicle, EngineEvent, EventData};
use crate::state::config::{
    AiBaseValues, DiplomacyConfig, GameConfig, HostOnly, MapDoc, MapEdges, MapSource, NewGame,
};
use crate::state::map::{MapInfo, Tiles};
use crate::state::players::{Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides};
use crate::unique::world::{Ctx, EvalWorld};
use crate::unique::{UniqueData, UniqueType};

/// The colours of city-states by type (`state.py:16-17`); any other type is grey.
const CITY_STATE_COLORS: [(&str, &str); 5] = [
    ("Cultured", "#a58cff"),
    ("Maritime", "#4fd68a"),
    ("Mercantile", "#f2d43d"),
    ("Militaristic", "#e85f5f"),
    ("Religious", "#f5f5f5"),
];

/// The barbarians' colour (`state.py:18`).
const BARBARIAN_COLOR: &str = "#2b2b2b";

/// The settings the engine reads; every other key is the host's, kept verbatim.
const ENGINE_KEYS: [&str; 27] = [
    "seed",
    "map",
    "map_size",
    "map_type",
    "width",
    "height",
    "wrap_x",
    "wrap_y",
    "map_edges",
    "speed",
    "difficulty",
    "barbarian_difficulty",
    "ai_base_values",
    "starting_era",
    "barbarians",
    "barbarian_aggression",
    "turn_limit",
    "victories",
    "city_states",
    "religion",
    "espionage",
    "nuclear_weapons",
    "tech_trading",
    "ruins",
    "river_density",
    "resources",
    "diplomacy",
];

// ---- The settings (game.py:151-196) -----------------------------------------------------------

impl Game {
    /// Normalises a lobby's settings, as JSON, into a [`NewGame`] (`game.py:151-196`): the
    /// defaults of `DEFAULT_CONFIG` for what is missing or null, names resolved loosely, and the
    /// seats checked. The seed is required, and an editor map must come inline (DESIGN.md 8.1).
    pub fn config_from_json(rules: &'static Ruleset, json: &[u8]) -> Result<NewGame, EngineError> {
        let v: Value = serde_json::from_slice(json)
            .map_err(|e| config(format!("The settings are not valid JSON ({e}).")))?;
        config_from_value(rules, v)
    }
}

/// [`Game::config_from_json`] from JSON already parsed. It takes the settings whole, so that the
/// map document and the seats move into the [`NewGame`] rather than being copied: an editor
/// map's document is its biggest part by far.
pub fn config_from_value(rules: &'static Ruleset, v: Value) -> Result<NewGame, EngineError> {
    let Value::Object(mut o) = v else {
        return Err(config("The settings must be a JSON object."));
    };
    // Both are the engine's (`ENGINE_KEYS`, and the seats go back into the host's keys).
    let map_setting = o.shift_remove("map").filter(py::truthy);
    let seats_setting = o.shift_remove("players").filter(|s| !s.is_null());
    let get = |k: &str| o.get(k).filter(|x| !x.is_null());
    let c = rules.constants();

    let seed = match get("seed") {
        None => {
            return Err(config(
                "The settings need a seed: a whole number every random draw of the game derives \
                 from. The host draws one when the lobby leaves it empty.",
            ));
        }
        Some(s) => s.as_u64().ok_or_else(|| {
            config(format!(
                "seed must be a whole number from 0 to {}, not {}.",
                u64::MAX,
                py::repr(s)
            ))
        })?,
    };
    let speed: SpeedId = named(rules, get("speed"), "speed")?.unwrap_or(c.default_speed);
    let difficulty: DifficultyId =
        named(rules, get("difficulty"), "difficulty")?.unwrap_or(c.default_difficulty);
    let barbarian_difficulty: DifficultyId =
        named(rules, get("barbarian_difficulty"), "barbarian_difficulty")?.unwrap_or(difficulty);
    // The first era, which is the one numbered 0 (Python's "Ancient era", game.py:42).
    let starting_era: EraId =
        named(rules, get("starting_era"), "starting_era")?.unwrap_or(EraId(0));
    let barbarians = barbarian_level(rules, get("barbarians"))?;
    let turn_limit = match get("turn_limit").filter(|t| py::truthy(t)) {
        None => rules.speeds()[speed].max_turns(),
        Some(t) => py::int_of(t)
            .and_then(|n| i32::try_from(n).ok())
            .filter(|&n| n >= 1)
            .ok_or_else(|| {
                config(format!(
                    "turn_limit must be a whole number, 1 or more, not {}.",
                    py::repr(t)
                ))
            })?,
    };

    // The map: an editor document inline, or the generator's settings.
    let (map, doc) = match map_setting {
        Some(Value::String(id)) => {
            return Err(config(format!(
                "The map must come inline, as the editor's document: the host resolves a map id \
                 to its document (got the id '{id}')."
            )));
        }
        Some(body @ Value::Object(_)) => {
            let (w, h) = document::dimensions(&body).map_err(|e| EngineError::Map(e.0))?;
            let id: Box<str> = ["id", "name"]
                .iter()
                .filter_map(|k| body.get(*k).filter(|x| py::truthy(x)))
                .map(py::str_of)
                .next()
                .unwrap_or_else(|| "custom".to_owned())
                .into();
            let size = nearest_size(rules, w, h)?;
            (MapSource::Editor { id: id.clone(), size }, Some(MapDoc { id, body }))
        }
        Some(other) => {
            return Err(config(format!(
                "map must be the editor's map document, an object, not {}.",
                py::repr(&other)
            )));
        }
        None => (generated_map(rules, &get)?, None),
    };
    let size = match &map {
        MapSource::Generated { size, .. } | MapSource::Editor { size, .. } => *size,
    };

    // The seats (game.py:191-196), checked before anything is made.
    let body = doc.as_ref().map(|d| &d.body);
    let seats = match seats_setting {
        None => Vec::new(),
        Some(Value::Array(a)) => a,
        Some(other) => {
            return Err(config(format!(
                "players must be a list of seats, such as [{{\"controller\": \"bot\"}}], not {}.",
                py::repr(&other)
            )));
        }
    };
    let seats = if seats.is_empty() {
        vec![Value::Object(Map::new()); default_seat_count(rules, size, body)]
    } else {
        seats
    };
    seat_specs(rules, &seats)?;

    let mut out =
        GameConfig::new(seed, map, speed, difficulty, starting_era, barbarians, turn_limit);
    out.barbarian_difficulty = barbarian_difficulty;
    out.ai_base_values = match get("ai_base_values").and_then(Value::as_str) {
        None | Some("unciv") => AiBaseValues::Unciv,
        Some("monotonic") => AiBaseValues::Monotonic,
        Some(other) => {
            return Err(config(format!(
                "Unknown ai_base_values '{other}'. Known: unciv, monotonic."
            )));
        }
    };
    if let Some(a) = get("barbarian_aggression") {
        // int(float(x)), clamped (game.py:162-166).
        let n = py::float_of(a)
            .filter(|x| x.is_finite())
            .ok_or_else(|| config("barbarian_aggression must be a number from 0 to 100."))?;
        out.barbarian_aggression = Some(clamp_u8(n.trunc(), 100.0));
    }
    out.disabled_victories = victories(rules, get("victories"))?;
    out.city_states = match get("city_states") {
        None => default_city_states(rules, size, body),
        Some(n) => py::int_of(n).and_then(|n| u8::try_from(n).ok()).ok_or_else(|| {
            config(format!(
                "city_states must be a whole number from 0 to 255, not {}.",
                py::repr(n)
            ))
        })?,
    };
    for (key, slot) in [
        ("religion", &mut out.religion),
        ("espionage", &mut out.espionage),
        ("nuclear_weapons", &mut out.nuclear_weapons),
        ("tech_trading", &mut out.tech_trading),
        ("ruins", &mut out.ruins),
    ] {
        if let Some(x) = get(key) {
            *slot = py::truthy(x);
        }
    }
    out.river_density = options::option_number(get("river_density"), 1.0, 0.0, 5.0);
    out.resources = options::resource_options(rules, get("resources"));
    out.diplomacy = diplomacy(get("diplomacy"))?;
    let mut host = BTreeMap::new();
    for (k, x) in o {
        // Python dropped null settings (`game.py:150`).
        if !x.is_null() && !ENGINE_KEYS.contains(&k.as_str()) {
            host.insert(k, x);
        }
    }
    host.insert("players".to_owned(), Value::Array(seats));
    out.host = HostOnly(host);
    let new = match doc {
        Some(d) => NewGame::editor(out, d),
        None => NewGame::generated(out),
    };
    // The settings name the document by its id, so the two agree.
    new.ok_or_else(|| config("The settings and the map document do not agree."))
}

fn config(message: impl Into<String>) -> EngineError {
    EngineError::Config(message.into())
}

/// A rule object a setting names loosely; `None` for a setting that is absent or empty, as Python
/// fell back to the default for those (`rules.resolve(...) or default`).
fn named<I: Named>(
    rules: &Ruleset,
    v: Option<&Value>,
    key: &str,
) -> Result<Option<I>, EngineError> {
    match v {
        None => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        // refcheck: config-refuses-unknown-names
        Some(Value::String(s)) => rules.resolve::<I>(s).map(Some).ok_or_else(|| {
            // A short table is listed; a long one (the nations) would drown the message.
            let known: Vec<&str> = rules.names(I::KIND).collect();
            if known.len() <= 12 {
                config(format!("Unknown {key} '{s}'. Known: {}.", known.join(", ")))
            } else {
                config(format!("Unknown {key} '{s}'."))
            }
        }),
        Some(other) => Err(config(format!("{key} must be a name, not {}.", py::repr(other)))),
    }
}

/// The barbarian level (`game.py:236`): `normal` by default; a falsy setting is the level that
/// switches them off.
fn barbarian_level(rules: &Ruleset, v: Option<&Value>) -> Result<BarbarianLevelId, EngineError> {
    let c = rules.constants();
    let off = || c.barbarian_levels.iter().find(|(_, l)| l.level.is_none()).map(|(id, _)| id);
    let known = || c.barbarian_levels.iter().map(|(_, l)| &*l.key).collect::<Vec<_>>().join(", ");
    match v {
        None => c
            .barbarian_level_id("normal")
            .or_else(|| c.barbarian_levels.ids().next())
            .ok_or_else(|| config("The ruleset has no barbarian levels.")),
        Some(Value::String(s)) if !s.is_empty() => c
            .barbarian_level_id(s)
            .ok_or_else(|| config(format!("Unknown barbarians '{s}'. Known: {}.", known()))),
        Some(x) if !py::truthy(x) => {
            off().ok_or_else(|| config("The ruleset has no level that switches barbarians off."))
        }
        Some(x) => {
            Err(config(format!("barbarians must be one of {}, not {}.", known(), py::repr(x))))
        }
    }
}

/// The lobby size whose area is nearest a map's, the first of two as near (`game.py:177-179`).
fn nearest_size(rules: &Ruleset, w: u16, h: u16) -> Result<MapSizeId, EngineError> {
    let area = u32::from(w) * u32::from(h);
    rules
        .map_sizes()
        .iter()
        .min_by_key(|(_, m)| (u32::from(m.width) * u32::from(m.height)).abs_diff(area))
        .map(|(id, _)| id)
        .ok_or_else(|| config("The ruleset has no map sizes."))
}

/// A generated map's settings (`game.py:180-190`): the lobby size (small by default), its type,
/// its edges, and a width and height set apart from the size's.
pub(crate) fn generated_map<'a>(
    rules: &Ruleset,
    get: &impl Fn(&str) -> Option<&'a Value>,
) -> Result<MapSource, EngineError> {
    let c = rules.constants();
    let key = |k: &str, default: &str| -> Result<String, EngineError> {
        match get(k) {
            None => Ok(default.to_owned()),
            Some(Value::String(s)) if s.is_empty() => Ok(default.to_owned()),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(other) => Err(config(format!("{k} must be a name, not {}.", py::repr(other)))),
        }
    };
    let size_key = key("map_size", "small")?;
    let size = c
        .map_size_id(&size_key)
        .or_else(|| (size_key == "small").then(|| c.map_sizes.ids().next()).flatten())
        .ok_or_else(|| {
            let known: Vec<&str> = c.map_sizes.iter().map(|(_, m)| &*m.key).collect();
            config(format!("Unknown map_size '{size_key}'. Known: {}.", known.join(", ")))
        })?;
    let type_key = key("map_type", "continents")?;
    let map_type: MapTypeId = c
        .map_type_id(&type_key)
        .or_else(|| (type_key == "continents").then(|| c.map_types.ids().next()).flatten())
        .ok_or_else(|| {
            let known: Vec<&str> = c.map_types.iter().map(|(_, m)| &*m.key).collect();
            config(format!("Unknown map_type '{type_key}'. Known: {}.", known.join(", ")))
        })?;
    let edges_key = key("map_edges", MapEdges::default().name())?;
    let edges = MapEdges::from_name(&edges_key).ok_or_else(|| {
        let known: Vec<&str> = MapEdges::ALL.iter().map(|e| e.name()).collect();
        config(format!("Unknown map_edges '{edges_key}'. Known: {}.", known.join(", ")))
    })?;
    let lobby = &c.map_sizes[size];
    let side = |k: &str, default: u16| -> Result<u16, EngineError> {
        match get(k).filter(|x| py::truthy(x)) {
            None => Ok(default),
            Some(x) => py::int_of(x)
                .and_then(|n| u16::try_from(n).ok())
                .ok_or_else(|| config(format!("{k} must be a whole number, not {}.", py::repr(x)))),
        }
    };
    let width = side("width", lobby.width)?;
    let mut height = side("height", lobby.height)?;
    let (wrap_x, wrap_y) = edges.wraps();
    let (lo, hi) = (crate::base::hex::MIN_SIDE, crate::base::hex::MAX_SIDE);
    let size_error =
        || EngineError::Map(format!("Maps must be between {lo} and {hi} tiles on each side."));
    // The sides as given are checked first, as `maps.generated_map` did (maps.py:88-91): a
    // height beyond the grid is refused before it is made even, so it cannot overflow.
    if !(lo..=hi).contains(&width) || !(lo..=hi).contains(&height) {
        return Err(size_error());
    }
    // Odd rows are offset: only an even height tiles north-south (game.py:188-189). The longest
    // side is even, so an odd height within it stays within it.
    const { assert!(crate::base::hex::MAX_SIDE.is_multiple_of(2)) };
    if wrap_y && !height.is_multiple_of(2) {
        height += 1;
    }
    HexGrid::new(width, height, wrap_x, wrap_y).map_err(|_| size_error())?;
    let dims = ((width, height) != (lobby.width, lobby.height)).then_some((width, height));
    Ok(MapSource::Generated { size, map_type, edges, dims })
}

/// How many seats a game has when the lobby names none (`game.py:190`): an editor map's start
/// positions, or the lobby size's players.
fn default_seat_count(rules: &Ruleset, size: MapSizeId, doc: Option<&Value>) -> usize {
    let starts = doc.and_then(|d| d.get("starts")).and_then(Value::as_array).map_or(0, Vec::len);
    if starts > 0 { starts } else { usize::from(rules.map_sizes()[size].players) }
}

/// How many city-states a game has when the lobby does not say (`game.py:192-194`).
fn default_city_states(rules: &Ruleset, size: MapSizeId, doc: Option<&Value>) -> u8 {
    match doc {
        Some(d) => {
            let n = d.get("cs_starts").and_then(Value::as_array).map_or(0, Vec::len);
            u8::try_from(n).unwrap_or(u8::MAX)
        }
        None => rules.map_sizes()[size].city_states,
    }
}

/// Python's victory names by what the lobby may call them (`_victory_name`, `game.py:1040-1044`).
const VICTORY_ALIASES: [(&str, &str); 9] = [
    ("science", "Scientific"),
    ("scientific", "Scientific"),
    ("culture", "Cultural"),
    ("cultural", "Cultural"),
    ("domination", "Domination"),
    ("diplomatic", "Diplomatic"),
    ("diplomacy", "Diplomatic"),
    ("score", "Time"),
    ("time", "Time"),
];

/// The victories switched off, sorted (`game.py:152-154`).
fn victories(rules: &Ruleset, v: Option<&Value>) -> Result<Vec<VictoryId>, EngineError> {
    let m = match v {
        None => return Ok(Vec::new()),
        Some(x) if !py::truthy(x) => return Ok(Vec::new()),
        Some(Value::Object(m)) => m,
        Some(other) => {
            return Err(config(format!(
                "victories must be an object of true and false, such as {{\"Time\": false}}, not \
                 {}.",
                py::repr(other)
            )));
        }
    };
    let mut off = Vec::new();
    for (k, on) in m {
        let lower = k.to_lowercase();
        let name = VICTORY_ALIASES.iter().find(|(a, _)| *a == lower).map_or(k.as_str(), |(_, n)| n);
        let id: VictoryId = named(rules, Some(&Value::String(name.to_owned())), "victory")?
            .ok_or_else(|| config("A victory needs a name."))?;
        if !py::truthy(on) {
            off.push(id);
        }
    }
    off.sort();
    off.dedup();
    Ok(off)
}

/// A whole number clamped to `0..=hi`, as a byte.
fn clamp_u8(x: f64, hi: f64) -> u8 {
    // In range after the clamp, and whole.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=255"
    )]
    let b = x.clamp(0.0, hi) as u8;
    b
}

/// The game's own diplomacy settings (`diplomacy.max_chat_messages`, `diplomacy.py:712-717`): a
/// chat limit of at least 2, or the ruleset's.
fn diplomacy(v: Option<&Value>) -> Result<DiplomacyConfig, EngineError> {
    let Some(limit) = v.and_then(Value::as_object).and_then(|o| o.get("max_chat_messages")) else {
        return Ok(DiplomacyConfig::default());
    };
    if !py::truthy(limit) {
        return Ok(DiplomacyConfig::default());
    }
    let n = py::int_of(limit).ok_or_else(|| {
        config(format!(
            "diplomacy.max_chat_messages must be a whole number, not {}.",
            py::repr(limit)
        ))
    })?;
    let n = u16::try_from(n.clamp(2, i64::from(u16::MAX))).unwrap_or(u16::MAX);
    Ok(DiplomacyConfig { max_chat_messages: Some(n) })
}

// ---- The seats (game.py:196-243) ---------------------------------------------------------------

/// One seat of the lobby, checked.
#[derive(Clone, Debug)]
struct SeatSpec {
    name: Option<Box<str>>,
    leader: Option<Box<str>>,
    color: Option<String>,
    /// `None` for a nation drawn at random.
    nation: Option<NationId>,
    controller: Controller,
    overrides: SeatOverrides,
    /// The seat's own difficulty, or `None` for the game's.
    difficulty: Option<DifficultyId>,
}

/// The seats, checked in Python's order: their number, then each one's handicap and automatic
/// decisions (`game.py:191-195`), then what else each names.
fn seat_specs(rules: &Ruleset, seats: &[Value]) -> Result<Vec<SeatSpec>, EngineError> {
    let max = rules.constants().max_players;
    if seats.is_empty() || seats.len() > usize::from(max) {
        return Err(config(format!("Games support 1 to {max} players.")));
    }
    let empty = Map::new();
    let objects: Vec<&Map<String, Value>> = seats
        .iter()
        .enumerate()
        .map(|(i, s)| match s {
            Value::Object(m) => Ok(m),
            Value::Null => Ok(&empty),
            other => Err(config(format!(
                "Seat {} must be an object of settings, such as {{\"controller\": \"bot\"}}, not {}.",
                i + 1,
                py::repr(other)
            ))),
        })
        .collect::<Result<_, _>>()?;
    let overrides = objects
        .iter()
        .map(|s| SeatOverrides::parse(s.get("handicap"), s.get("auto")).map_err(|e| config(e.0)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(objects.len());
    for (s, overrides) in objects.into_iter().zip(overrides) {
        let text = |k: &str| -> Result<Option<Box<str>>, EngineError> {
            match s.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(t)) if t.is_empty() => Ok(None),
                Some(Value::String(t)) => Ok(Some(t.as_str().into())),
                Some(other) => {
                    Err(config(format!("A seat's {k} must be text, not {}.", py::repr(other))))
                }
            }
        };
        let controller = match text("controller")? {
            None => Controller::Human,
            Some(c) => Controller::from_name(&c).ok_or_else(|| {
                let known: Vec<&str> = Controller::ALL.iter().map(|c| c.name()).collect();
                config(format!("Unknown controller '{c}'. Known: {}.", known.join(", ")))
            })?,
        };
        let nation = match s.get("nation") {
            Some(Value::String(n)) if matches!(n.as_str(), "random" | "Random") => None,
            n => {
                let nation: Option<NationId> = named(rules, n, "nation")?;
                if let Some(x) = nation
                    && rules.nations()[x].kind != NationKind::Major
                {
                    return Err(config(format!(
                        "{} is not a civilization a seat can play: choose a major civilization \
                         or 'random'.",
                        rules.nations()[x].name
                    )));
                }
                nation
            }
        };
        out.push(SeatSpec {
            name: text("name")?,
            leader: text("leader")?,
            color: s.get("color").and_then(Value::as_str).map(str::to_owned),
            nation,
            controller,
            overrides,
            difficulty: named(rules, s.get("difficulty"), "difficulty")?,
        });
    }
    Ok(out)
}

/// Whether two colours are too close to tell apart on the map: a "redmean" weighted distance
/// under 60 (`colors_clash`, `game.py:70-77`).
#[must_use]
pub fn colors_clash(a: Rgb, b: Rgb) -> bool {
    let [x, y] = [a.0, b.0].map(|c| c.map(f64::from));
    let rm = (x[0] + y[0]) / 2.0;
    let (dr, dg, db) = (x[0] - y[0], x[1] - y[1], x[2] - y[2]);
    let d2 = (2.0 + rm / 256.0) * dr * dr + 4.0 * dg * dg + (2.0 + (255.0 - rm) / 256.0) * db * db;
    d2.sqrt() < 60.0
}

/// One colour per civilization, first come first served (`unique_colors`, `game.py:80-97`): a
/// colour asked for is granted in seat order unless an earlier civilization has it or one too
/// close; the rest get the first palette colours nobody has taken.
#[must_use]
pub fn unique_colors(wanted: &[Option<&str>]) -> Vec<Rgb> {
    let mut out: Vec<Option<Rgb>> = Vec::with_capacity(wanted.len());
    for w in wanted {
        let c = w.and_then(Rgb::from_hex);
        let ok = c.filter(|&c| !out.iter().flatten().any(|&o| colors_clash(c, o)));
        out.push(ok);
    }
    let palette: Vec<Rgb> = PLAYER_COLORS.iter().filter_map(|h| Rgb::from_hex(h)).collect();
    for i in 0..out.len() {
        if out[i].is_some() {
            continue;
        }
        let taken: Vec<Rgb> = out.iter().flatten().copied().collect();
        let free = palette.iter().copied().find(|&p| !taken.iter().any(|&o| colors_clash(p, o)));
        out[i] = Some(free.unwrap_or(palette[i % palette.len()]));
    }
    out.into_iter().map(Option::unwrap_or_default).collect()
}

// ---- The stages (game.py:198-318) -------------------------------------------------------------

/// What a setup stage does.
#[derive(Clone, Copy, Debug)]
pub enum SetupStep {
    /// Before the game exists: works on the draft the state is made from.
    Draft(fn(&mut Draft<'_>) -> Result<(), EngineError>),
    /// On the new game.
    Game(fn(&mut Game, &Draft<'_>) -> Result<(), EngineError>),
    /// A system not ported yet: nothing happens.
    Pending,
}

/// One stage of setup (DESIGN.md 6.14).
#[derive(Clone, Copy, Debug)]
pub struct SetupStage {
    pub name: &'static str,
    pub step: SetupStep,
    /// Whether its system is ported, or the package that will port it.
    pub porting: Porting,
}

impl SetupStage {
    const fn draft(name: &'static str, f: fn(&mut Draft<'_>) -> Result<(), EngineError>) -> Self {
        Self { name, step: SetupStep::Draft(f), porting: Porting::Ported }
    }

    const fn game(
        name: &'static str,
        f: fn(&mut Game, &Draft<'_>) -> Result<(), EngineError>,
    ) -> Self {
        Self { name, step: SetupStep::Game(f), porting: Porting::Ported }
    }

    const fn later(name: &'static str, porting: Porting) -> Self {
        Self { name, step: SetupStep::Pending, porting }
    }
}

/// The stages of a new game, in order (DESIGN.md 6.14): those that make the state, then those
/// that play on the new game.
pub static SETUP: [SetupStage; 15] = [
    SetupStage::draft("config", read_seats),
    SetupStage::draft("nations", choose_nations),
    SetupStage::draft("map: an editor document", read_map),
    SetupStage::draft("map: a generated map", generate_map),
    SetupStage::later("map: starts and ruins a document lacks", Porting::Pending("1c-09")),
    SetupStage::draft("players", make_players),
    SetupStage::game("starting techs, gold and culture", starting_techs),
    SetupStage::later("city-state init", Porting::Pending("1c-06")),
    SetupStage::game("starting units", starting_units),
    SetupStage::later("starting triggers", Porting::Pending("1b-08")),
    SetupStage::game("relations", relations),
    SetupStage::later("camps", Porting::Pending("1c-06")),
    SetupStage::later("happiness", Porting::Pending("1b-06")),
    SetupStage::game("visibility", visibility),
    SetupStage::game("begin", begin),
];

/// Every setup stage still waiting for its package: `(stage, package)`.
pub fn waiting() -> impl Iterator<Item = (&'static SetupStage, &'static str)> {
    SETUP.iter().filter_map(|s| match s.porting {
        Porting::Pending(pkg) => Some((s, pkg)),
        Porting::Ported => None,
    })
}

/// What a new game is made from, as the stages before it exists work it out. The map's tiles and
/// continents and the players move into the new state, so the stages on the game see the rest.
#[derive(Debug)]
pub struct Draft<'a> {
    rules: &'static Ruleset,
    setup: &'a NewGame,
    seats: Vec<SeatSpec>,
    nations: Vec<NationId>,
    /// The city-states' nations drawn: as many as asked for, if the ruleset has them.
    cs_nations: Vec<NationId>,
    map: Option<MapDocument>,
    continents: Vec<u16>,
    starts: Vec<TileIdx>,
    cs_starts: Vec<TileIdx>,
    players: Vec<Player>,
}

impl<'a> Draft<'a> {
    fn new(rules: &'static Ruleset, setup: &'a NewGame) -> Self {
        Self {
            rules,
            setup,
            seats: Vec::new(),
            nations: Vec::new(),
            cs_nations: Vec::new(),
            map: None,
            continents: Vec::new(),
            starts: Vec::new(),
            cs_starts: Vec::new(),
            players: Vec::new(),
        }
    }

    fn config(&self) -> &GameConfig {
        self.setup.config()
    }
}

/// The stages that make the state ran without making a map: a bug in the table.
fn no_map() -> EngineError {
    config("The map stage of setup made no map.")
}

/// The refusal of a setup that needs a system not ported yet (DESIGN.md 3.4, rule 4).
fn not_ported(path: &str, what: &str) -> EngineError {
    ActionError::new(
        ErrCode::NotPorted,
        format!("{what} is not ported to the new engine yet ({path})."),
    )
    .into()
}

/// config: the seats of the settings, checked again, since a host may build a [`NewGame`]
/// itself; a game whose settings name none gets the default number (`game.py:190-191`).
fn read_seats(d: &mut Draft<'_>) -> Result<(), EngineError> {
    let cfg = d.config();
    let size = match &cfg.map {
        MapSource::Generated { size, .. } | MapSource::Editor { size, .. } => *size,
    };
    let listed = cfg.host.get("players").and_then(Value::as_array).filter(|a| !a.is_empty());
    let seats = match listed {
        Some(a) => a.clone(),
        None => {
            let body = d.setup.map_doc().map(|m| &m.body);
            vec![Value::Null; default_seat_count(d.rules, size, body)]
        }
    };
    d.seats = seat_specs(d.rules, &seats)?;
    Ok(())
}

/// nations (`game.py:201-217`): the ones the seats chose, then the rest drawn from the major
/// civilizations nobody chose, the benchmark civilization when none is left; and the city-states'.
fn choose_nations(d: &mut Draft<'_>) -> Result<(), EngineError> {
    let r = d.rules;
    let seed = d.config().seed;
    let benchmark = r.nations().iter().find(|(_, n)| n.benchmark).map(|(id, _)| id);
    let chosen: Vec<Option<NationId>> = d.seats.iter().map(|s| s.nation).collect();
    let mut pool: Vec<NationId> = r
        .derived()
        .major_nations
        .iter()
        .copied()
        .filter(|&x| !r.nations()[x].benchmark && !chosen.contains(&Some(x)))
        .collect();
    Rng::keyed(seed, Purpose::NationShuffle, &[0]).shuffle(&mut pool);
    d.nations = chosen
        .into_iter()
        .map(|c| c.or_else(|| pool.pop()).or(benchmark))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| config("The ruleset has too few civilizations for this many seats."))?;
    let mut cs_pool: Vec<NationId> = r.derived().city_state_nations.clone();
    Rng::keyed(seed, Purpose::NationShuffle, &[1]).shuffle(&mut cs_pool);
    cs_pool.truncate(usize::from(d.config().city_states));
    d.cs_nations = cs_pool;
    Ok(())
}

/// map: the editor's document read and cleaned (`maps.prepare`, `maps.py:323-346`), its
/// continents, and the start positions it gives. Settings without a document generate the map
/// in the next stage.
fn read_map(d: &mut Draft<'_>) -> Result<(), EngineError> {
    let Some(doc) = d.setup.map_doc() else { return Ok(()) };
    let map = document::read(d.rules, &doc.body).map_err(|e| EngineError::Map(e.0))?;
    let grid = map.grid().map_err(|e| EngineError::Map(e.0))?;
    d.continents = continents::assign(d.rules, &grid, &map.tiles);
    let n = d.seats.len();
    if map.starts.len() < n {
        // maps._fill_starts chooses the rest (package 1c-09).
        return Err(not_ported(
            "mapgen::prepare",
            &format!(
                "This map gives {} start positions for {n} civilizations; choosing the rest",
                map.starts.len()
            ),
        ));
    }
    let starts = map.starts[..n].to_vec();
    let cs_starts: Vec<TileIdx> = map
        .cs_starts
        .iter()
        .copied()
        .filter(|&s| starts.iter().all(|&x| grid.distance(s, x) >= 3))
        .take(d.cs_nations.len())
        .collect();
    if cs_starts.len() < d.cs_nations.len() {
        return Err(not_ported(
            "mapgen::prepare",
            &format!(
                "This map gives {} city-state start positions for {} city-states; choosing the \
                 rest",
                cs_starts.len(),
                d.cs_nations.len()
            ),
        ));
    }
    // Ruins for a map that has none, when the settings want them (maps.py:344-345).
    pending(Porting::Pending("1c-09"));
    d.starts = starts;
    d.cs_starts = cs_starts;
    d.map = Some(map);
    Ok(())
}

/// map: a generated map, for settings without an editor document (`mapgen.generate_map`,
/// `game.py:218-221`): the lobby size's width and height or those the settings give, the map
/// type, the edges, the rivers and the resources the lobby set; a start for each seat, placed
/// by its nation's start bias; a site for each city-state it has room for; and ruins if the
/// settings want them. The map draws from the `Map*` streams of the game's seed.
fn generate_map(d: &mut Draft<'_>) -> Result<(), EngineError> {
    let setup = d.setup;
    if setup.map_doc().is_some() {
        return Ok(());
    }
    let r = d.rules;
    let cfg = setup.config();
    let MapSource::Generated { size, map_type, edges, dims } = cfg.map else {
        return Err(no_map());
    };
    let lobby = &r.map_sizes()[size];
    let (width, height) = dims.unwrap_or((lobby.width, lobby.height));
    let nations: Vec<Option<NationId>> = d.nations.iter().copied().map(Some).collect();
    let spec = GenSpec {
        width,
        height,
        map_type: MapType::from_key(&r.constants().map_types[map_type].key),
        options: MapOptions { edges, rivers: cfg.river_density, resources: cfg.resources.clone() },
        players: d.seats.len(),
        city_states: d.cs_nations.len(),
        nations: &nations,
        ruins: cfg.ruins,
    };
    let map = mapgen::generate(r, cfg.seed, &spec).map_err(|e| EngineError::Map(e.0))?;
    d.continents = map.continents;
    d.starts.clone_from(&map.starts);
    d.cs_starts.clone_from(&map.cs_starts);
    d.map = Some(MapDocument {
        width: map.width,
        height: map.height,
        wrap_x: map.wrap_x,
        wrap_y: map.wrap_y,
        tiles: map.tiles,
        starts: map.starts,
        cs_starts: map.cs_starts,
        warnings: Vec::new(),
    });
    Ok(())
}

/// players (`game.py:222-243`): the majors in seat order, the city-states that found a start,
/// and the barbarians if the settings have them.
fn make_players(d: &mut Draft<'_>) -> Result<(), EngineError> {
    let r = d.rules;
    let cfg = d.setup.config();
    let map = d.map.as_ref().ok_or_else(no_map)?;
    let cells = u32::from(map.width) * u32::from(map.height);
    let id = |i: usize| {
        u8::try_from(i).map(PlayerId).map_err(|_| config("A game holds at most 64 players."))
    };
    let wanted: Vec<Option<&str>> = d.seats.iter().map(|s| s.color.as_deref()).collect();
    let colors = unique_colors(&wanted);
    let mut players = Vec::new();
    for (i, s) in d.seats.iter().enumerate() {
        let nation = d.nations[i];
        let def = &r.nations()[nation];
        let name: Box<str> = match &s.name {
            Some(n) => n.clone(),
            None if def.benchmark => format!("Civilization {}", i + 1).into(),
            None => def.name.clone(),
        };
        let seat =
            Seat::new(s.controller, s.overrides, Some(s.difficulty.unwrap_or(cfg.difficulty)));
        let mut p = Player::new(id(i)?, PlayerKind::Major, name, nation, colors[i], seat, cells);
        p.leader = s.leader.clone().or_else(|| def.leader_name.clone()).unwrap_or_default();
        p.start_tile = Some(d.starts[i]);
        players.push(p);
    }
    for (j, &nation) in d.cs_nations.iter().take(d.cs_starts.len()).enumerate() {
        let def = &r.nations()[nation];
        let kind = def.city_state_type.map(|t| &*r.city_state_types()[t].name);
        let hex =
            CITY_STATE_COLORS.iter().find(|(t, _)| Some(*t) == kind).map_or("#bbbbbb", |(_, c)| c);
        let seat = Seat::new(Controller::Minor, SeatOverrides::default(), None);
        let color = Rgb::from_hex(hex).unwrap_or_default();
        let mut p = Player::new(
            id(players.len())?,
            PlayerKind::CityState,
            def.name.clone(),
            nation,
            color,
            seat,
            cells,
        );
        if let Some(data) = p.city_state.as_deref_mut() {
            data.cs_type = def.city_state_type;
        }
        p.start_tile = Some(d.cs_starts[j]);
        players.push(p);
    }
    let barbarians_on = r.constants().barbarian_levels[cfg.barbarians].level.is_some();
    if barbarians_on {
        let nation = r
            .nations()
            .iter()
            .find(|(_, n)| n.kind == NationKind::Barbarian)
            .map(|(id, _)| id)
            .ok_or_else(|| config("The ruleset has no barbarians, which these settings want."))?;
        let seat = Seat::new(Controller::Barbarian, SeatOverrides::default(), None);
        let color = Rgb::from_hex(BARBARIAN_COLOR).unwrap_or_default();
        let name = r.nations()[nation].name.clone();
        players.push(Player::new(
            id(players.len())?,
            PlayerKind::Barbarian,
            name,
            nation,
            color,
            seat,
            cells,
        ));
    }
    d.players = players;
    Ok(())
}

/// starting techs, gold and culture (`game.py:249-262`): every tech of an earlier era or marked
/// `Starting tech`, an AI seat's free techs, `Starts with [tech]` of the civilization's uniques,
/// and the starting era's gold and culture scaled by the speed.
fn starting_techs(g: &mut Game, _: &Draft<'_>) -> Result<(), EngineError> {
    let r = g.rules;
    let era = g.st.config().starting_era;
    let civs: Vec<PlayerId> =
        g.st.players().iter().filter(|(_, p)| !p.is_barbarian()).map(|(id, _)| id).collect();
    let (gold, culture) = {
        let e = &r.eras()[era];
        let sp = g.speed();
        // int() of the scaled amounts truncates toward zero.
        (
            (f64::from(e.starting_gold) * sp.gold_cost_modifier).trunc(),
            (f64::from(e.starting_culture) * sp.culture_cost_modifier).trunc(),
        )
    };
    for p in civs {
        let mut grant: Vec<TechId> = r
            .techs()
            .iter()
            .filter(|(_, t)| {
                t.era < era || super::core::has_type(r, &t.uniques, UniqueType::StartingTech)
            })
            .map(|(id, _)| id)
            .collect();
        if g.player(p).is_some_and(Player::is_major) && !g.is_humanlike(p) {
            let level = g.seat_difficulty(Some(p));
            grant.extend(r.difficulties()[level].ai_free_techs.iter().copied());
        }
        research::add_tech_silently(g, p, &grant);
        let more = starts_with(g, p);
        research::add_tech_silently(g, p, &more);
        if let Some(pl) = g.player_mut(p, PlayerTouch::STOCKS) {
            pl.econ.gold += gold;
            pl.econ.culture += culture;
        }
    }
    Ok(())
}

/// starting units (`game.py:278-289`): each civilization's and city-state's starting units
/// (`units::starting_units`), each on the first tile within three of its start it may stand on,
/// ring by ring; a unit with no such tile is left out.
fn starting_units(g: &mut Game, _: &Draft<'_>) -> Result<(), EngineError> {
    let era = g.st.config().starting_era;
    let starts: Vec<(PlayerId, crate::base::ids::TileIdx)> =
        g.st.players()
            .iter()
            .filter(|(_, p)| !p.is_barbarian())
            .filter_map(|(id, p)| p.start_tile.map(|t| (id, t)))
            .collect();
    for (p, start) in starts {
        for base in super::units::starting_units(g, p, era) {
            if let Some(spot) = super::units::find_spawn_tile(g, start, base, p) {
                g.create_unit(p, base, spot, 0)?;
            }
        }
    }
    Ok(())
}

/// The techs `Starts with [tech]` grants the civilization now (`civ_uniques(StartsWithTech)`,
/// `game.py:259-260`): its uniques at the start of a game are its nation's, its techs', its era's
/// and the global ones (`economy.civ_umaps`, `economy.py:95-138`), each if its conditionals hold.
fn starts_with(g: &Game, p: PlayerId) -> Vec<TechId> {
    let r = g.rules;
    let v = g.view();
    let Some(pl) = g.player(p) else { return Vec::new() };
    let ctx = Ctx { civ: Some(p), ..Ctx::default() }.resolve(&v);
    let mut sources = vec![&r.nations()[pl.nation].uniques];
    sources.extend(pl.tech.known.iter().map(|t| &r.techs()[t].uniques));
    sources.push(&r.eras()[v.civ_era(p)].uniques);
    sources.push(r.global_uniques());
    let mut out = Vec::new();
    for src in sources {
        for id in src.ids() {
            if let UniqueData::StartsWithTech(x) = r.uniques().get(id).data
                && crate::unique::cond::applies(id, &ctx, &v)
            {
                out.push(x.tech);
            }
        }
    }
    out
}

/// relations (`game.py:291-298`): Python made a fresh relation for every pair of civilizations;
/// the state holds one for every pair from the start (`Diplomacy::new`), at peace and unmet, with
/// the barbarians at war with everyone. Nothing is left to do.
fn relations(g: &mut Game, _: &Draft<'_>) -> Result<(), EngineError> {
    debug_assert!(g.st.players().ids().all(|a| {
        g.st.players()
            .ids()
            .all(|b| a == b || g.is_barbarian(a) || g.is_barbarian(b) || !g.at_war(a, b))
    }));
    Ok(())
}

/// visibility (`game.py:312`): every source registered, and what it shows explored, met and
/// discovered, before the world is told it begins (DESIGN.md 6.9).
fn visibility(g: &mut Game, _: &Draft<'_>) -> Result<(), EngineError> {
    g.settle_sight();
    Ok(())
}

/// begin (`game.py:314-316`): the first player's turn begins, and the world is told.
fn begin(g: &mut Game, d: &Draft<'_>) -> Result<(), EngineError> {
    g.begin_turn();
    let text = format!(
        "The world begins. {} civilizations and {} city-states stir.",
        d.seats.len(),
        d.cs_nations.len()
    );
    g.emit(EngineEvent::GameStart, &text, None, None, EventData::default(), &[]);
    Ok(())
}

impl Game {
    /// Sets a new game up from its settings and, for an editor map, the document
    /// (`Game.new`, `game.py:145-318`), stage by stage ([`SETUP`]); returns it at the start of
    /// the first player's turn, with the events that start emitted.
    pub fn new(
        rules: &'static Ruleset,
        setup: &NewGame,
    ) -> Result<(Self, EventBatch), EngineError> {
        let mut d = Draft::new(rules, setup);
        for s in &SETUP {
            if let (SetupStep::Draft(f), Porting::Ported) = (s.step, s.porting) {
                f(&mut d)?;
            }
        }
        let map = d.map.as_mut().ok_or_else(no_map)?;
        let info = MapInfo {
            width: map.width,
            height: map.height,
            wrap_x: map.wrap_x,
            wrap_y: map.wrap_y,
            continents: std::mem::take(&mut d.continents),
        };
        let tiles = Tiles::new(std::mem::take(&mut map.tiles));
        let players: PlayerVec<Player> = std::mem::take(&mut d.players).into_iter().collect();
        let st = State::new(setup.config().clone(), info, tiles, players)?;
        let mut g = Self::from_state(rules, st, Chronicle::new())?;
        g.begin_call();
        for s in &SETUP {
            if let (SetupStep::Game(f), Porting::Ported) = (s.step, s.porting) {
                f(&mut g, &d)?;
            }
        }
        g.settle();
        let batch = g.take_batch();
        Ok((g, batch))
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn colours_asked_for_are_granted_unless_one_too_close_is_taken() {
        let red = Rgb::from_hex("#aa0000").expect("red");
        let near = Rgb::from_hex("#ab0101").expect("nearly red");
        assert!(colors_clash(red, near));
        assert!(!colors_clash(red, Rgb::from_hex("#0000aa").expect("blue")));
        let got = unique_colors(&[Some("#aa0000"), Some("#ab0101"), None, Some("bad")]);
        assert_eq!(got[0], red);
        // The second clashes with the first, so it and the rest get the first free palette
        // colours: blue, then green.
        assert_eq!(got[1].to_hex(), "#0000aa");
        assert_eq!(got[2].to_hex(), "#00aa00");
        assert_eq!(got[3].to_hex(), "#aaaa00");
    }

    #[test]
    fn the_stages_that_make_the_state_come_first() {
        let first_game =
            SETUP.iter().position(|s| matches!(s.step, SetupStep::Game(_))).expect("a game stage");
        assert!(SETUP[first_game..].iter().all(|s| !matches!(s.step, SetupStep::Draft(_))));
        for s in &SETUP {
            assert_eq!(
                matches!(s.step, SetupStep::Pending),
                matches!(s.porting, Porting::Pending(_)),
                "{}: a pending stage does nothing, and only a pending one",
                s.name
            );
        }
    }

    #[test]
    fn settings_default_as_python_defaulted_them() {
        let r = Ruleset::shared();
        let new = config_from_value(r, json!({"seed": 3})).expect("a generated game");
        let c = new.config();
        assert_eq!(c.seed, 3);
        assert_eq!(c.speed, r.constants().default_speed);
        assert_eq!(c.barbarian_difficulty, c.difficulty);
        assert_eq!(c.turn_limit, r.speeds()[c.speed].max_turns());
        assert!(matches!(c.map, MapSource::Generated { dims: None, .. }));
        let seats = c.host.get("players").and_then(Value::as_array).map(Vec::len);
        assert_eq!(seats, Some(4), "a small map's players");
        assert_eq!(c.city_states, 8);
    }
}

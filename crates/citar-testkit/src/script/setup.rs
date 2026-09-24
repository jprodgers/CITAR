//! A new game from a rule script's settings, the way Python's `Game.new` sets one up on an editor
//! map with its start positions given (`game.py:145-318`, `maps.py:323-346`), for as much of it
//! as a bare game needs: the settings, the seats and their checks, nations, the map, the players,
//! their starting techs, gold and culture, and the relations.
//!
//! It stands in for `Game::new` until package 1b-03 ports the setup stages, and is then replaced
//! by it. What a bare script never sees is left out: starting units (the prelude clears them),
//! barbarian camps and ruins (off), city-state setup and the first turn's start. Where Python
//! drew at random (unnamed nations, the city-states' nations, missing start positions), this
//! takes the first in the ruleset's order, or refuses: scripts name their nations, and the
//! arena gives every start.

use citar_engine::base::hex::HexGrid;
use citar_engine::base::ids::{
    DifficultyId, EraId, ImprovementId, NationId, PlayerId, ResourceId, SpeedId, TechId, TerrainId,
    TileIdx,
};
use citar_engine::base::py;
use citar_engine::base::sets::{FeatureSet, PlayerVec};
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_engine::rules::constants::PLAYER_COLORS;
use citar_engine::rules::defs::{NationKind, Route};
use citar_engine::state::State;
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::config::{GameConfig, MapSource};
use citar_engine::state::map::{MapInfo, RouteBits, Tile, Tiles, WATER};
use citar_engine::state::players::{Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides};
use citar_engine::unique::{SourceUniques, UniqueData, UniqueType};
use serde_json::{Map, Value};

/// The settings this builder reads; any other is refused rather than ignored, until `Game::new`
/// (package 1b-03) reads them all.
const KNOWN: [&str; 16] = [
    "seed",
    "map",
    "players",
    "speed",
    "difficulty",
    "barbarian_difficulty",
    "starting_era",
    "barbarians",
    "turn_limit",
    "city_states",
    "religion",
    "espionage",
    "nuclear_weapons",
    "tech_trading",
    "ruins",
    "river_density",
];

/// The colours of city-states by type (`state.py:16-17`).
const CITY_STATE_COLORS: [(&str, &str); 5] = [
    ("Cultured", "#a58cff"),
    ("Maritime", "#4fd68a"),
    ("Mercantile", "#f2d43d"),
    ("Militaristic", "#e85f5f"),
    ("Religious", "#f5f5f5"),
];

/// A new game from the settings, with the editor map inline under `map`; the text of a refusal
/// is Python's where Python refused the same settings.
pub fn new_game(rules: &'static Ruleset, cfg: &Map<String, Value>) -> Result<Game, String> {
    if let Some(k) = cfg.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return Err(format!(
            "the setting {k:?} is not read by the Rust runner's setup yet (package 1b-03 ports \
             Game::new)"
        ));
    }
    let seed = cfg
        .get("seed")
        .and_then(Value::as_u64)
        .ok_or("the settings need a seed, a whole number")?;
    let doc =
        cfg.get("map").and_then(Value::as_object).ok_or("the settings need a map document")?;
    let c = rules.constants();
    let speed: SpeedId = named(rules, cfg.get("speed"))?.unwrap_or(c.default_speed);
    let difficulty: DifficultyId =
        named(rules, cfg.get("difficulty"))?.unwrap_or(c.default_difficulty);
    let barbarian_difficulty: DifficultyId =
        named(rules, cfg.get("barbarian_difficulty"))?.unwrap_or(difficulty);
    let era: EraId = named(rules, cfg.get("starting_era"))?.unwrap_or(EraId(0));
    let level_key = cfg.get("barbarians").and_then(Value::as_str).unwrap_or("normal");
    let barbarians = c.barbarian_level_id(level_key).ok_or("unknown barbarians setting")?;
    let barbarians_on = c.barbarian_levels[barbarians].level.is_some();
    let turn_limit = match cfg.get("turn_limit") {
        Some(v) if py::truthy(v) => {
            i32::try_from(py::int_of(v).ok_or("turn_limit must be a whole number")?)
                .map_err(|_| "turn_limit out of range")?
        }
        _ => rules.speeds()[speed].max_turns(),
    };

    // The map (maps.prepare, game.py:171-190).
    let width = doc.get("width").and_then(Value::as_u64).and_then(|w| u16::try_from(w).ok());
    let height = doc.get("height").and_then(Value::as_u64).and_then(|h| u16::try_from(h).ok());
    let (Some(width), Some(height)) = (width, height) else {
        return Err("A map needs integer width and height.".to_owned());
    };
    let wrap_x = doc.get("wrap_x").is_some_and(py::truthy);
    let wrap_y = doc.get("wrap_y").is_some_and(py::truthy) && height % 2 == 0;
    let grid = HexGrid::new(width, height, wrap_x, wrap_y).map_err(|e| e.to_string())?;
    let tiles = read_tiles(rules, doc, &grid)?;
    let area = u32::from(width) * u32::from(height);
    let size = c
        .map_sizes
        .iter()
        .min_by_key(|(_, m)| (u32::from(m.width) * u32::from(m.height)).abs_diff(area))
        .map(|(id, _)| id)
        .ok_or("the ruleset has no map sizes")?;
    let id = doc.get("id").or_else(|| doc.get("name")).and_then(Value::as_str).unwrap_or("custom");
    let source = MapSource::Editor { id: id.into(), size };

    // The seats (game.py:191-199): checked before anything else is made.
    let empty = Vec::new();
    let seats: &Vec<Value> = cfg.get("players").and_then(Value::as_array).unwrap_or(&empty);
    let starts = start_list(doc, "starts", &grid)?;
    let seats: Vec<Map<String, Value>> = if seats.is_empty() {
        vec![Map::new(); starts.len().max(1)]
    } else {
        seats.iter().map(|s| s.as_object().cloned().unwrap_or_default()).collect()
    };
    let n = seats.len();
    if n < 1 || n > usize::from(c.max_players) {
        return Err(format!("Games support 1 to {} players", c.max_players));
    }
    let overrides = seats
        .iter()
        .map(|s| SeatOverrides::parse(s.get("handicap"), s.get("auto")).map_err(|e| e.0))
        .collect::<Result<Vec<_>, _>>()?;
    let n_cs = match cfg.get("city_states") {
        Some(v) if !v.is_null() => {
            usize::try_from(py::int_of(v).ok_or("city_states must be a whole number")?)
                .map_err(|_| "city_states must not be negative")?
        }
        _ => start_list(doc, "cs_starts", &grid)?.len(),
    };

    // Nations (game.py:201-217), without the shuffle: the first free ones.
    let d = rules.derived();
    let mut chosen: Vec<Option<NationId>> = Vec::with_capacity(n);
    for s in &seats {
        let wanted = s
            .get("nation")
            .and_then(Value::as_str)
            .filter(|t| !matches!(*t, "" | "random" | "Random"));
        let nation = wanted
            .and_then(|t| rules.resolve::<NationId>(t))
            .filter(|&x| rules.nations()[x].kind == NationKind::Major);
        chosen.push(nation);
    }
    let benchmark = rules.nations().iter().find(|(_, x)| x.benchmark).map(|(id, _)| id);
    let mut pool: Vec<NationId> = d
        .major_nations
        .iter()
        .copied()
        .filter(|x| !rules.nations()[*x].benchmark && !chosen.contains(&Some(*x)))
        .collect();
    pool.reverse();
    let nations: Vec<NationId> = chosen
        .into_iter()
        .map(|x| x.or_else(|| pool.pop()).or(benchmark).ok_or("no nation left for a seat"))
        .collect::<Result<_, _>>()?;
    let cs_nations: Vec<NationId> = d.city_state_nations.iter().copied().take(n_cs).collect();

    // Start positions (maps.prepare, maps.py:329-339): the map's own, which must be enough.
    if starts.len() < n {
        return Err(format!(
            "This map has room for only {} civilizations (asked for {n}).",
            starts.len()
        ));
    }
    let starts = &starts[..n];
    let cs_starts: Vec<TileIdx> = start_list(doc, "cs_starts", &grid)?
        .into_iter()
        .filter(|&s| starts.iter().all(|&x| grid.distance(s, x) >= 3))
        .take(n_cs)
        .collect();
    if cs_starts.len() < cs_nations.len() {
        return Err(format!(
            "This map has room for only {} city-states (asked for {}).",
            cs_starts.len(),
            cs_nations.len()
        ));
    }

    // The settings (game.py:151-165).
    let mut config = GameConfig::new(seed, source, speed, difficulty, era, barbarians, turn_limit);
    config.barbarian_difficulty = barbarian_difficulty;
    config.city_states = u8::try_from(n_cs).map_err(|_| "too many city-states")?;
    for (key, slot) in [
        ("religion", &mut config.religion),
        ("espionage", &mut config.espionage),
        ("nuclear_weapons", &mut config.nuclear_weapons),
        ("tech_trading", &mut config.tech_trading),
        ("ruins", &mut config.ruins),
    ] {
        if let Some(v) = cfg.get(key).filter(|v| !v.is_null()) {
            *slot = py::truthy(v);
        }
    }
    if let Some(v) = cfg.get("river_density").filter(|v| !v.is_null()) {
        config.river_density = py::float_of(v).ok_or("river_density must be a number")?;
    }

    // The players (game.py:222-243).
    let cells = u32::from(width) * u32::from(height);
    let mut players: Vec<Player> = Vec::new();
    for (i, s) in seats.iter().enumerate() {
        let nation = nations[i];
        let def = &rules.nations()[nation];
        let id = PlayerId(u8::try_from(i).map_err(|_| "too many players")?);
        let text = |k: &str| s.get(k).and_then(Value::as_str).filter(|t| !t.is_empty());
        let name = match text("name") {
            Some(t) => t.to_owned(),
            None if def.benchmark => format!("Civilization {}", i + 1),
            None => def.name.to_string(),
        };
        let controller = match text("controller") {
            Some(t) => {
                Controller::from_name(t).ok_or_else(|| format!("unknown controller {t:?}"))?
            }
            None => Controller::Human,
        };
        let seat_difficulty: DifficultyId =
            named(rules, s.get("difficulty"))?.unwrap_or(difficulty);
        let seat = Seat::new(controller, overrides[i], Some(seat_difficulty));
        let color = text("color")
            .and_then(Rgb::from_hex)
            .or_else(|| Rgb::from_hex(PLAYER_COLORS[i % PLAYER_COLORS.len()]))
            .unwrap_or_default();
        let mut p = Player::new(id, PlayerKind::Major, name.into(), nation, color, seat, cells);
        p.leader =
            text("leader").map_or_else(|| def.leader_name.clone().unwrap_or_default(), Into::into);
        p.start_tile = Some(starts[i]);
        players.push(p);
    }
    for (j, &nation) in cs_nations.iter().enumerate() {
        let def = &rules.nations()[nation];
        let id = PlayerId(u8::try_from(players.len()).map_err(|_| "too many players")?);
        let type_name = def.city_state_type.map(|t| &*rules.city_state_types()[t].name);
        let hex = CITY_STATE_COLORS
            .iter()
            .find(|(t, _)| Some(*t) == type_name)
            .map_or("#bbbbbb", |(_, c)| c);
        let seat = Seat::new(Controller::Minor, SeatOverrides::default(), None);
        let color = Rgb::from_hex(hex).unwrap_or_default();
        let mut p =
            Player::new(id, PlayerKind::CityState, def.name.clone(), nation, color, seat, cells);
        if let Some(data) = p.city_state.as_deref_mut() {
            data.cs_type = def.city_state_type;
        }
        p.start_tile = Some(cs_starts[j]);
        players.push(p);
    }
    if barbarians_on {
        let id = PlayerId(u8::try_from(players.len()).map_err(|_| "too many players")?);
        let nation =
            rules.lookup::<NationId>("Barbarians").ok_or("the ruleset has no Barbarians")?;
        let seat = Seat::new(Controller::Barbarian, SeatOverrides::default(), None);
        let color = Rgb::from_hex("#2b2b2b").unwrap_or_default();
        players.push(Player::new(
            id,
            PlayerKind::Barbarian,
            "Barbarians".into(),
            nation,
            color,
            seat,
            cells,
        ));
    }

    // Starting techs, gold and culture (game.py:249-262).
    let era_def = &rules.eras()[era];
    let speed_def = &rules.speeds()[speed];
    for p in &mut players {
        if p.is_barbarian() {
            continue;
        }
        for (t, def) in rules.techs().iter() {
            if has(rules, &def.uniques, UniqueType::StartingTech) || def.era < era {
                p.tech.known.insert(t);
            }
        }
        if p.is_major() && !p.seat().is_humanlike() {
            let level = p.seat().difficulty().unwrap_or(difficulty);
            for &t in rules.difficulties()[level].ai_free_techs.iter() {
                p.tech.known.insert(t);
            }
        }
        let known: Vec<TechId> = p.tech.known.iter().collect();
        let mut sources = vec![&rules.nations()[p.nation].uniques, rules.global_uniques()];
        sources.extend(known.iter().map(|&t| &rules.techs()[t].uniques));
        for src in sources {
            for id in src.ids() {
                if let UniqueData::StartsWithTech(x) = rules.uniques().get(id).data {
                    p.tech.known.insert(x.tech);
                }
            }
        }
        // int() of the scaled amounts truncates toward zero.
        p.econ.gold += (f64::from(era_def.starting_gold) * speed_def.gold_cost_modifier).trunc();
        p.econ.culture +=
            (f64::from(era_def.starting_culture) * speed_def.culture_cost_modifier).trunc();
    }

    let map =
        MapInfo { width, height, wrap_x, wrap_y, continents: continents(rules, &tiles, &grid) };
    let players: PlayerVec<Player> = players.into_iter().collect();
    let st = State::new(config, map, Tiles::new(tiles), players).map_err(|e| e.to_string())?;
    Game::from_state(rules, st, Chronicle::new()).map_err(|e| e.to_string())
}

/// A ruleset object named loosely by a setting; `None` when the setting is absent or empty, and
/// an error when it names nothing.
fn named<I: citar_engine::rules::Named>(
    rules: &Ruleset,
    v: Option<&Value>,
) -> Result<Option<I>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => rules
            .resolve::<I>(s)
            .map(Some)
            .ok_or_else(|| format!("unknown {} {s:?}", I::KIND.as_str())),
        Some(other) => Err(format!("{} must be a name, not {other}", I::KIND.as_str())),
    }
}

/// Whether one of an object's uniques is of type `ty`, whatever its conditionals.
fn has(rules: &Ruleset, u: &SourceUniques, ty: UniqueType) -> bool {
    u.ids().any(|id| rules.uniques().meta(id).ty == Some(ty))
}

/// A list of start tiles from the document: indices on the map.
fn start_list(doc: &Map<String, Value>, key: &str, grid: &HexGrid) -> Result<Vec<TileIdx>, String> {
    let Some(list) = doc.get(key).and_then(Value::as_array) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for v in list {
        let t = v
            .as_u64()
            .and_then(|i| u32::try_from(i).ok())
            .map(TileIdx)
            .filter(|&t| grid.contains(t))
            .ok_or_else(|| format!("{key}: {v} is not a tile of the map"))?;
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// The tiles of the document's rows: terrain, features, wonder, river, resource, amount,
/// improvement, route (`maps.py:48-60`), named exactly.
fn read_tiles(
    rules: &Ruleset,
    doc: &Map<String, Value>,
    grid: &HexGrid,
) -> Result<Vec<Tile>, String> {
    let rows = doc.get("tiles").and_then(Value::as_array).ok_or("a map needs its tiles")?;
    if rows.len() != grid.size() as usize {
        return Err(format!(
            "A {}x{} map needs {} tiles, got {}.",
            grid.width(),
            grid.height(),
            grid.size(),
            rows.len()
        ));
    }
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let cell = |k: usize| row.get(k).filter(|v| !v.is_null());
        let text = |k: usize| cell(k).and_then(Value::as_str);
        let err = |what: &str| format!("tile {i}: {what}");
        let terrain: TerrainId =
            text(0).and_then(|t| rules.lookup(t)).ok_or_else(|| err("unknown terrain"))?;
        let mut features = FeatureSet::EMPTY;
        for f in cell(1).and_then(Value::as_array).into_iter().flatten() {
            let id: TerrainId =
                f.as_str().and_then(|t| rules.lookup(t)).ok_or_else(|| err("unknown feature"))?;
            let feature = rules.terrains()[id].feature.ok_or_else(|| err("not a feature"))?;
            features.insert(feature);
        }
        let wonder: Option<TerrainId> = match text(2) {
            Some(t) => Some(rules.lookup(t).ok_or_else(|| err("unknown wonder"))?),
            None => None,
        };
        let river = cell(3).and_then(Value::as_u64).unwrap_or(0) & 63;
        let resource: Option<ResourceId> = match text(4) {
            Some(t) => Some(rules.lookup(t).ok_or_else(|| err("unknown resource"))?),
            None => None,
        };
        let amount =
            cell(5).and_then(Value::as_u64).and_then(|a| u8::try_from(a).ok()).unwrap_or(0);
        let improvement: Option<ImprovementId> = match text(6) {
            Some(t) => Some(rules.lookup(t).ok_or_else(|| err("unknown improvement"))?),
            None => None,
        };
        let route = match text(7) {
            None => None,
            Some("Road") => Some(Route::Road),
            Some("Railroad") => Some(Route::Railroad),
            Some(_) => return Err(err("unknown route")),
        };
        out.push(
            Tile::new(terrain)
                .with_features(features)
                .with_wonder(wonder)
                .with_river(u8::try_from(river).unwrap_or(0))
                .with_resource(resource, amount)
                .with_improvement(improvement)
                .with_route_bits(RouteBits::EMPTY.with_route(route)),
        );
    }
    Ok(out)
}

/// The landmasses, numbered from the largest, water apart (`mapgen._assign_continents`,
/// `mapgen.py:743-751`).
fn continents(rules: &Ruleset, tiles: &[Tile], grid: &HexGrid) -> Vec<u16> {
    use citar_engine::rules::defs::TerrainType;
    let land = |t: TileIdx| {
        tiles
            .get(t.0 as usize)
            .is_some_and(|x| rules.terrains()[x.terrain()].kind != TerrainType::Water)
    };
    let mut comp = vec![usize::MAX; tiles.len()];
    let mut comps: Vec<Vec<TileIdx>> = Vec::new();
    for start in grid.tiles() {
        if !land(start) || comp[start.0 as usize] != usize::MAX {
            continue;
        }
        let k = comps.len();
        let mut members = vec![start];
        comp[start.0 as usize] = k;
        let mut i = 0;
        while i < members.len() {
            for n in grid.neighbors(members[i]) {
                if land(n) && comp[n.0 as usize] == usize::MAX {
                    comp[n.0 as usize] = k;
                    members.push(n);
                }
            }
            i += 1;
        }
        comps.push(members);
    }
    let mut order: Vec<usize> = (0..comps.len()).collect();
    order.sort_by_key(|&k| core::cmp::Reverse(comps[k].len()));
    let mut out = vec![WATER; tiles.len()];
    for (id, &k) in order.iter().enumerate() {
        for t in &comps[k] {
            out[t.0 as usize] = u16::try_from(id).unwrap_or(WATER - 1);
        }
    }
    out
}

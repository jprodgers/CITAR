//! The settings, the map's shape and the tiles.
//!
//! Reads the config dict `Game.new` normalised (`game.py:32-60, 151-205`), `GameState.width`,
//! `height` and `continents`, and the tiles as 14-tuples in `Tile._ORDER` (`state.py:66-107`).

use std::collections::BTreeMap;

use serde_json::Value;

use super::read::{self, Obj, Path, Res, flag, int, list, text};
use super::{Cx, Dropped};
use crate::base::ids::{
    BarbarianLevelId, CityId, DifficultyId, EraId, FeatureId, ImprovementId, ResourceId, SpeedId,
    TerrainId, TileIdx, VictoryId,
};
use crate::base::sets::FeatureSet;
use crate::rules::Ruleset;
use crate::rules::defs::{ResourceType, Route, TerrainType};
use crate::state::TileClaim;
use crate::state::config::{
    AiBaseValues, DiplomacyConfig, GameConfig, HostOnly, MapEdges, MapSource, ResourceKindOptions,
    ResourceOptions, ResourceRule,
};
use crate::state::map::{BuildQueue, BuildStep, MapInfo, RouteBits, Tile, Tiles, WATER};

/// The settings keys the engine reads; the rest are kept verbatim for the host.
const HOST_KEYS: [&str; 3] = ["on_disconnect", "reconnect_seconds", "players"];

/// The map's shape and the settings.
pub(super) fn map_and_config(top: &Obj<'_>, r: &'static Ruleset) -> Res<(MapInfo, GameConfig)> {
    let width: u16 = top.int_req("width")?;
    let height: u16 = top.int_req("height")?;
    let at = top.at("config");
    let cfg = Obj::new(top.req("config")?, at)?;
    for (key, want) in [("width", width), ("height", height)] {
        if let Some(v) = cfg.get(key).filter(|v| !v.is_null()) {
            let got: u16 = int(v, &cfg.at(key))?;
            if got != want {
                return Err(cfg.at(key).err(format!("{got}, but the state's {key} is {want}")));
            }
        }
    }
    let map = MapInfo {
        width,
        height,
        wrap_x: cfg.flag("wrap_x", false)?,
        wrap_y: cfg.flag("wrap_y", false)?,
        continents: continents(top, u32::from(width) * u32::from(height))?,
    };
    map.grid().map_err(|e| top.path().err(e.to_string()))?;
    let config = settings(&cfg, r, &map)?;
    Ok((map, config))
}

/// Each tile's continent, -1 (water) as [`WATER`]; empty if Python had none.
fn continents(top: &Obj<'_>, size: u32) -> Res<Vec<u16>> {
    let out = top.each("continents", |v, p| {
        let c: i64 = int(v, p)?;
        match c {
            -1 => Ok(WATER),
            0..=65_534 => Ok(u16::try_from(c).unwrap_or(WATER)),
            _ => Err(p.err(format!("continent {c} is out of range"))),
        }
    })?;
    if !out.is_empty() && out.len() != size as usize {
        return Err(top
            .at("continents")
            .err(format!("{} continent ids for {size} tiles", out.len())));
    }
    Ok(out)
}

/// The typed settings.
fn settings(cfg: &Obj<'_>, r: &'static Ruleset, map: &MapInfo) -> Res<GameConfig> {
    let name_of = |key: &str| -> Res<Option<&str>> { cfg.opt_text(key) };
    let lookup = |key: &str| -> Res<&str> {
        name_of(key)?.ok_or_else(|| cfg.at(key).err("the setting is missing"))
    };
    let seed: u64 = cfg.int_req("seed")?;
    let size_key = lookup("map_size")?;
    let size = r
        .constants()
        .map_size_id(size_key)
        .ok_or_else(|| cfg.at("map_size").err(format!("no map size {size_key:?}")))?;
    let type_key = lookup("map_type")?;
    let map_source = match cfg.opt_text("map")? {
        Some(id) => {
            if type_key != "custom" {
                return Err(cfg.at("map").err(format!("an editor map with map type {type_key:?}")));
            }
            // Python cleared the edges of an editor map, whose wraps are its own (game.py:185).
            if !read::is_none(cfg.get("map_edges")) {
                return Err(cfg.at("map_edges").err("an editor map has no edges setting"));
            }
            MapSource::Editor { id: id.into(), size }
        }
        None => {
            let map_type = r
                .constants()
                .map_type_id(type_key)
                .ok_or_else(|| cfg.at("map_type").err(format!("no map type {type_key:?}")))?;
            let edges = match cfg.opt_text("map_edges")? {
                None => MapEdges::default(),
                Some(e) => MapEdges::from_name(e)
                    .ok_or_else(|| cfg.at("map_edges").err(format!("no map edges {e:?}")))?,
            };
            let lobby = r.map_sizes().get(size).map(|s| (s.width, s.height));
            let dims = (lobby != Some((map.width, map.height))).then_some((map.width, map.height));
            MapSource::Generated { size, map_type, edges, dims }
        }
    };
    let named = |key: &str| -> Res<&str> { lookup(key) };
    let speed: SpeedId = id(r, named("speed")?, &cfg.at("speed"))?;
    let difficulty: DifficultyId = id(r, named("difficulty")?, &cfg.at("difficulty"))?;
    let barbarian_difficulty: DifficultyId = match name_of("barbarian_difficulty")? {
        None => difficulty,
        Some(n) => id(r, n, &cfg.at("barbarian_difficulty"))?,
    };
    let starting_era: EraId = id(r, named("starting_era")?, &cfg.at("starting_era"))?;
    let level = named("barbarians")?;
    let barbarians: BarbarianLevelId = r
        .constants()
        .barbarian_level_id(level)
        .ok_or_else(|| cfg.at("barbarians").err(format!("no barbarian setting {level:?}")))?;
    let turn_limit = match cfg.opt_int("turn_limit")? {
        Some(t) => t,
        None => r.speeds().get(speed).map_or(500, |s| s.max_turns()),
    };
    let mut out =
        GameConfig::new(seed, map_source, speed, difficulty, starting_era, barbarians, turn_limit);
    out.barbarian_difficulty = barbarian_difficulty;
    out.ai_base_values = match cfg.text("ai_base_values", "unciv")? {
        "unciv" => AiBaseValues::Unciv,
        "monotonic" => AiBaseValues::Monotonic,
        other => return Err(cfg.at("ai_base_values").err(format!("no AI base values {other:?}"))),
    };
    out.barbarian_aggression = cfg.opt_int::<u8>("barbarian_aggression")?;
    if out.barbarian_aggression.is_some_and(|a| a > 100) {
        return Err(cfg.at("barbarian_aggression").err("not a percentage"));
    }
    out.disabled_victories = victories(cfg, r)?;
    let default_cs = r.map_sizes().get(size).map_or(0, |s| s.city_states);
    out.city_states = cfg.opt_int("city_states")?.unwrap_or(default_cs);
    out.religion = cfg.flag("religion", true)?;
    out.espionage = cfg.flag("espionage", true)?;
    out.nuclear_weapons = cfg.flag("nuclear_weapons", true)?;
    out.tech_trading = cfg.flag("tech_trading", true)?;
    out.ruins = cfg.flag("ruins", true)?;
    out.river_density = cfg.real("river_density", 1.0)?;
    if let Some(v) = cfg.get("resources").filter(|v| !v.is_null()) {
        out.resources = resources(v, &cfg.at("resources"), r)?;
    }
    if let Some(v) = cfg.get("diplomacy").filter(|v| !v.is_null()) {
        out.diplomacy = diplomacy(v, &cfg.at("diplomacy"))?;
    }
    let mut host = BTreeMap::new();
    for key in HOST_KEYS {
        if let Some(v) = cfg.get(key) {
            host.insert(key.to_owned(), v.clone());
        }
    }
    for (k, v) in cfg.rest() {
        host.insert(k.to_owned(), v.clone());
    }
    out.host = HostOnly(host);
    Ok(out)
}

/// A rule object of a table tools resolve in, from a setting.
fn id<I: super::names::PyName>(r: &'static Ruleset, name: &str, p: &Path<'_>) -> Res<I> {
    I::resolve_in(r, name)
        .or_else(|| I::loose(r, name))
        .ok_or_else(|| p.err(format!("the ruleset has no {} {name:?}", I::WHAT)))
}

/// The victories switched off, sorted.
fn victories(cfg: &Obj<'_>, r: &'static Ruleset) -> Res<Vec<VictoryId>> {
    let mut off: Vec<VictoryId> = cfg
        .entries("victories", |k, v, p| {
            let on = flag(v, p)?;
            let v: VictoryId = id(r, k, p)?;
            Ok((!on).then_some(v))
        })?
        .into_iter()
        .flatten()
        .collect();
    off.sort();
    off.dedup();
    Ok(off)
}

/// The lobby's resource options (`mapgen.MapOptions`), with Python's clamps.
fn resources(v: &Value, p: &Path<'_>, r: &'static Ruleset) -> Res<ResourceOptions> {
    let o = Obj::new(v, *p)?;
    let clamp = |x: f64| x.clamp(0.0, 5.0);
    let mut out = ResourceOptions { density: clamp(o.real("density", 1.0)?), ..Default::default() };
    for (key, kind) in [
        ("strategic", ResourceType::Strategic),
        ("luxury", ResourceType::Luxury),
        ("bonus", ResourceType::Bonus),
    ] {
        let Some(sub) = o.get(key).filter(|v| !v.is_null()) else { continue };
        let at = o.at(key);
        let s = Obj::new(sub, at)?;
        let mut opts =
            ResourceKindOptions { density: clamp(s.real("density", 1.0)?), each: Vec::new() };
        let rules = s.entries("each", |name, rule, rp| {
            let res: ResourceId = id(r, name, rp)?;
            if r.resources().get(res).map(|d| d.kind) != Some(kind) {
                return Err(rp.err(format!("{name} is not a {key} resource")));
            }
            let ro = Obj::new(rule, *rp)?;
            let mode = ro.text_req("mode")?;
            let value = ro.real("value", 0.0)?.clamp(0.0, 10_000.0);
            ro.finish()?;
            Ok(match mode {
                "normal" => None,
                "off" => Some((res, ResourceRule::Off)),
                "cap" => Some((res, ResourceRule::Cap(value))),
                "share" => Some((res, ResourceRule::Share(value))),
                other => return Err(rp.err(format!("no resource mode {other:?}"))),
            })
        })?;
        opts.each = rules.into_iter().flatten().collect();
        opts.each.sort_by_key(|&(id, _)| id);
        s.finish()?;
        match kind {
            ResourceType::Strategic => out.strategic = opts,
            ResourceType::Luxury => out.luxury = opts,
            ResourceType::Bonus => out.bonus = opts,
        }
    }
    o.finish()?;
    Ok(out)
}

/// The game's own diplomacy settings (`diplomacy.py:712-717`).
fn diplomacy(v: &Value, p: &Path<'_>) -> Res<DiplomacyConfig> {
    let o = Obj::new(v, *p)?;
    let max = o.opt_int::<u16>("max_chat_messages")?.filter(|&m| m != 0).map(|m| m.max(2));
    o.finish()?;
    Ok(DiplomacyConfig { max_chat_messages: max })
}

// ---- Tiles ------------------------------------------------------------------------------------

/// The fields of a tile, in `Tile._ORDER` (`state.py:84-85`).
const TILE_ORDER: [&str; 14] = [
    "terrain",
    "features",
    "wonder",
    "river",
    "resource",
    "resource_amount",
    "improvement",
    "pillaged",
    "route",
    "route_pillaged",
    "owner",
    "city",
    "fallout",
    "build",
];

/// Every tile and its build queue.
pub(super) fn tiles(cx: &mut Cx<'_>, top: &Obj<'_>) -> Res<Tiles> {
    let at = top.at("tiles");
    let rows = list(top.req("tiles")?, &at)?;
    if rows.len() != cx.size as usize {
        return Err(at.err(format!("{} tiles for a map of {}", rows.len(), cx.size)));
    }
    let mut out = Vec::with_capacity(rows.len());
    let mut builds = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let p = at.index(i);
        let (tile, queue) = tile(cx, row, &p)?;
        out.push(tile);
        if let Some(q) = queue {
            builds.insert(TileIdx(u32::try_from(i).unwrap_or(u32::MAX)), q);
        }
    }
    Tiles::from_parts(out, builds).map_err(|e| at.err(e.to_string()))
}

/// One tile, and its build queue if it has a non-empty one.
fn tile(cx: &mut Cx<'_>, row: &Value, p: &Path<'_>) -> Res<(Tile, Option<BuildQueue>)> {
    let cells = list(row, p)?;
    if cells.len() != TILE_ORDER.len() {
        return Err(p.err(format!("a tile of {} fields; Tile._ORDER has 14", cells.len())));
    }
    let r = cx.r;
    let field = |i: usize| (&cells[i], p.field(TILE_ORDER[i]));
    let (v, fp) = field(0);
    let terrain: TerrainId = cx.named(v, &fp)?;
    if !matches!(
        r.terrains().get(terrain).map(|t| t.kind),
        Some(TerrainType::Land | TerrainType::Water)
    ) {
        return Err(fp.err(format!("{} is not a base terrain", read::shown(v))));
    }
    let (v, fp) = field(1);
    let mut features = features(cx, list(v, &fp)?, &fp)?;
    let (v, fp) = field(2);
    let wonder: Option<TerrainId> = cx.opt_named(Some(v), &fp)?;
    if let Some(w) = wonder
        && r.terrains().get(w).map(|t| t.kind) != Some(TerrainType::NaturalWonder)
    {
        return Err(fp.err(format!("{} is not a natural wonder", read::shown(v))));
    }
    let (v, fp) = field(3);
    let river: u8 = int(v, &fp)?;
    if river > 0b11_1111 {
        return Err(fp.err(format!("river mask {river} has bits past the six edges")));
    }
    let (v, fp) = field(4);
    let resource: Option<ResourceId> = cx.opt_named(Some(v), &fp)?;
    let (v, fp) = field(5);
    let amount: u8 = int(v, &fp)?;
    let (v, fp) = field(6);
    let improvement: Option<ImprovementId> = cx.opt_named(Some(v), &fp)?;
    let (v, fp) = field(7);
    let pillaged = flag(v, &fp)?;
    let (v, fp) = field(8);
    let route = match v {
        Value::Null => None,
        v => match text(v, &fp)? {
            "Road" => Some(Route::Road),
            "Railroad" => Some(Route::Railroad),
            other => return Err(fp.err(format!("no route {other:?}"))),
        },
    };
    let (v, fp) = field(9);
    let route_pillaged = flag(v, &fp)?;
    let (v, fp) = field(10);
    let owner = cx.opt_player(Some(v), &fp)?;
    let (v, fp) = field(11);
    let city: Option<CityId> = cx.opt_city(Some(v), &fp)?;
    let (v, fp) = field(12);
    if flag(v, &fp)? {
        features.insert(r.derived().known.fallout);
    }
    let (v, fp) = field(13);
    let queue = match v {
        Value::Null => None,
        v => {
            let steps = list(v, &fp)?;
            let mut q = BuildQueue::new();
            for (i, s) in steps.iter().enumerate() {
                let sp = fp.index(i);
                let pair = list(s, &sp)?;
                let [name, turns] = pair else {
                    return Err(sp.err("a build step is [improvement, turns left]"));
                };
                q.push(BuildStep {
                    improvement: cx.named(name, &sp.index(0))?,
                    turns_left: int(turns, &sp.index(1))?,
                });
            }
            // Python kept an emptied queue as []; the tiles keep none.
            (!q.is_empty()).then_some(q)
        }
    };
    let bits = RouteBits::EMPTY
        .with_route(route)
        .with_route_pillaged(route_pillaged)
        .with_improvement_pillaged(pillaged);
    let tile = Tile::new(terrain)
        .with_wonder(wonder)
        .with_resource(resource, amount)
        .with_improvement(improvement)
        .with_route_bits(bits)
        .with_claim(TileClaim { owner, city })
        .with_river(river)
        .with_features(features);
    if resource.is_none() && amount != 0 {
        return Err(p.field("resource_amount").err("a deposit size without a resource"));
    }
    Ok((tile, queue))
}

/// A tile's features from Python's list; its order is dropped, and counted if it was not layer
/// order.
pub(super) fn features(cx: &mut Cx<'_>, names: &[Value], p: &Path<'_>) -> Res<FeatureSet> {
    let mut set = FeatureSet::EMPTY;
    let mut last: Option<FeatureId> = None;
    let mut sorted = true;
    for (i, v) in names.iter().enumerate() {
        let at = p.index(i);
        let f: FeatureId = cx.named(v, &at)?;
        if usize::from(f.0) >= FeatureSet::CAPACITY {
            return Err(at.err("a feature past the set's capacity"));
        }
        if !set.insert(f) {
            return Err(at.err(format!("{} is listed twice", read::shown(v))));
        }
        sorted &= last.is_none_or(|l| l < f);
        last = Some(f);
    }
    if !sorted {
        cx.report.note(Dropped::ListOrder);
    }
    Ok(set)
}

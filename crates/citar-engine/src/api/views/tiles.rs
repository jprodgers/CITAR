//! A tile as a viewer knows it: as it is while in sight, as last seen in the fog, nothing before
//! it is explored (`views._tile_state` and `tile_info`, `views.py:255-302`).

use serde_json::{Map, Value, json};

use super::units::unit_info;
use crate::base::ids::{ImprovementId, PlayerId, ResourceId, TerrainId, TileIdx};
use crate::base::num;
use crate::base::sets::FeatureSet;
use crate::base::stats::Stat;
use crate::game::Game;
use crate::game::derive::stats as memo;
use crate::game::vis::sight::unit_visible_to;
use crate::rules::defs::Route;
use crate::state::map::Tile;

/// The river edges' names, by bit (`views.py:278`).
const RIVER_EDGES: [&str; 6] = ["E", "NE", "NW", "W", "SW", "SE"];

/// What a viewer knows of the parts of a tile that change: as they are while it sees the tile,
/// as it last saw them in the fog (Python's memory kept no route pillage), or, for a tile it has
/// explored but never watched leave its sight, the features and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Known {
    pub features: FeatureSet,
    pub improvement: Option<ImprovementId>,
    pub route: Option<Route>,
    pub owner: Option<PlayerId>,
    pub pillaged: bool,
    pub route_pillaged: bool,
}

impl Known {
    /// Tile `t` as `viewer` knows it; `sees` is whether it sees the tile now (`_tile_state`).
    #[must_use]
    pub fn of(g: &Game, t: TileIdx, tile: &Tile, viewer: Option<PlayerId>, sees: bool) -> Self {
        let now = Self {
            features: tile.features(),
            improvement: tile.improvement(),
            route: tile.route(),
            owner: tile.owner(),
            pillaged: tile.improvement_pillaged(),
            route_pillaged: tile.route_pillaged(),
        };
        let Some(v) = viewer.filter(|_| !sees) else { return now };
        let mem = g.player(v).and_then(|p| p.major.as_deref()).map(|m| m.memory.get(t));
        match mem.filter(crate::state::memory::TileMemory::is_remembered) {
            Some(m) => Self {
                features: m.features(),
                improvement: m.improvement(),
                route: m.route().route(),
                owner: m.owner(),
                pillaged: m.route().improvement_pillaged(),
                route_pillaged: false,
            },
            None => Self {
                features: tile.features(),
                improvement: None,
                route: None,
                owner: None,
                pillaged: false,
                route_pillaged: false,
            },
        }
    }
}

/// A route's name (`"Road"`, `"Railroad"`).
#[must_use]
pub const fn route_name(r: Route) -> &'static str {
    match r {
        Route::Road => "Road",
        Route::Railroad => "Railroad",
    }
}

/// The names of a set of features, lowest layer first, as terrains.
pub(crate) fn feature_names(g: &Game, f: FeatureSet) -> impl Iterator<Item = &str> + '_ {
    let r = g.rules();
    f.iter().filter_map(move |x| r.derived().features.get(x).and_then(|&t| r.name(t)))
}

/// Whether `viewer` sees a tile now; a spectator sees them all.
#[must_use]
pub fn sees(g: &Game, viewer: Option<PlayerId>, t: TileIdx) -> bool {
    viewer.is_none_or(|v| g.derived().vis().sees(v, t))
}

/// Whether `viewer` has explored a tile; a spectator has explored them all (`views.knows_tile`).
#[must_use]
pub fn knows(g: &Game, viewer: Option<PlayerId>, t: TileIdx) -> bool {
    viewer.is_none_or(|v| g.player(v).is_some_and(|p| p.explored.contains(t.0)))
}

/// Whether `viewer` can see a resource yet: it needs the revealing tech, which a spectator never
/// has (`tiles.resource_visible` with no player).
#[must_use]
pub fn resource_seen(g: &Game, viewer: Option<PlayerId>, res: ResourceId) -> bool {
    let by = g.rules().resources()[res].revealed_by;
    by.is_none() || viewer.is_some_and(|v| g.has_tech(v, by))
}

/// A tile's base terrain, its natural wonder and its features, as terrains
/// (`tiles.all_terrains`).
fn all_terrains(g: &Game, tile: &Tile) -> Vec<TerrainId> {
    let r = g.rules();
    let mut out = vec![tile.terrain()];
    out.extend(tile.wonder());
    out.extend(tile.features().iter().filter_map(|f| r.derived().features.get(f).copied()));
    out
}

/// A tile as `viewer` sees it (`views.tile_info`): nothing but its place until it is explored;
/// then its terrain, its features, improvement, route and owner as the viewer knows them, the
/// resource it can see, its yields, movement cost and defence, and while in sight the city and
/// the units on it.
#[must_use]
pub fn tile_info(g: &Game, t: TileIdx, viewer: Option<PlayerId>) -> Value {
    let (x, y) = g.xy(t);
    let Some(tile) = g.tile(t).filter(|_| knows(g, viewer, t)) else {
        return json!({"x": x, "y": y, "explored": false});
    };
    let r = g.rules();
    let visible = sees(g, viewer, t);
    let k = Known::of(g, t, tile, viewer, visible);
    let mut m = Map::new();
    m.insert("x".into(), json!(x));
    m.insert("y".into(), json!(y));
    m.insert("explored".into(), json!(true));
    m.insert("visible".into(), json!(visible));
    m.insert("terrain".into(), json!(r.name(tile.terrain())));
    m.insert("features".into(), json!(feature_names(g, k.features).collect::<Vec<_>>()));
    if let Some(w) = tile.wonder() {
        m.insert("natural_wonder".into(), json!(r.name(w)));
    }
    let river = tile.river_mask();
    if river != 0 {
        let edges: Vec<&str> = RIVER_EDGES
            .iter()
            .enumerate()
            .filter(|&(i, _)| river & (1 << i) != 0)
            .map(|(_, &n)| n)
            .collect();
        m.insert("river_edges".into(), json!(edges));
    }
    if let Some(imp) = k.improvement {
        let name = r.name(imp).unwrap_or("");
        let text = if k.pillaged { format!("{name} (pillaged)") } else { name.to_owned() };
        m.insert("improvement".into(), json!(text));
    }
    if let Some(route) = k.route {
        let name = route_name(route);
        let text = if k.route_pillaged { format!("{name} (pillaged)") } else { name.to_owned() };
        m.insert("route".into(), json!(text));
    }
    if let Some(o) = k.owner {
        m.insert("owner".into(), json!(o.0));
    }
    if let Some(res) = tile.resource().filter(|&res| resource_seen(g, viewer, res)) {
        m.insert("resource".into(), json!(r.name(res)));
        if tile.resource_amount() > 0 {
            m.insert("resource_amount".into(), json!(tile.resource_amount()));
        }
    }
    let yields = memo::tile_yield(g, t, viewer, None);
    m.insert("yields".into(), super::rounded(&yields, &Stat::ALL, 1));
    let terrains = all_terrains(g, tile);
    let cost = terrains.iter().map(|&x| r.terrains()[x].movement_cost).max().unwrap_or(1);
    let defence = num::py_sum(terrains.iter().map(|&x| r.terrains()[x].defence_bonus));
    m.insert("movement_cost".into(), json!(cost));
    m.insert("defense_bonus_percent".into(), json!(num::round_half_even_i64(defence * 100.0)));
    let sighted = visible || viewer.is_none();
    if sighted && let Some(c) = g.city_at(t) {
        m.insert(
            "city".into(),
            json!({"id": c.id().get(), "name": &*c.name, "owner": c.owner().0}),
        );
    }
    if sighted {
        let units: Vec<Value> = g
            .units_at(t)
            .filter(|u| viewer.is_none_or(|v| unit_visible_to(g, v, u.id())))
            .map(|u| unit_info(g, u.id(), viewer, false))
            .collect();
        if !units.is_empty() {
            m.insert("units".into(), Value::Array(units));
        }
    }
    let steps = g.state().tiles().builds(t);
    if !steps.is_empty() && (viewer.is_none() || k.owner == viewer) {
        let wip: Vec<Value> = steps
            .iter()
            .filter(|s| s.turns_left >= 0)
            .map(|s| json!({"improvement": r.name(s.improvement), "turns_left": s.turns_left}))
            .collect();
        m.insert("work_in_progress".into(), Value::Array(wip));
    }
    Value::Object(m)
}

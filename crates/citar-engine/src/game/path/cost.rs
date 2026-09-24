//! What one step costs (`movement.enter_cost`, `movement.py:330-377`), with the routes and rivers
//! it reads (`movement.py:277-327`).
//!
//! Costs are in move-scale units (`game.json` `move_scale`, 60 to a movement point): a road
//! costs a half or a third, a railroad a tenth, and [`ALL`](super::ALL) is "whatever is left".
//! The order of the rules is Python's, which decides what wins: embarking, then a zone of
//! control, then a unit that pays one point everywhere, then railroads, roads (not across a
//! river without the right unique), a unit that ignores terrain, a river crossing, and last the
//! terrain with its double-movement uniques, the rough-terrain penalty and the hill rule.

use super::ALL;
use super::class::{DoubleOn, Mover};
use super::node::governing;
use crate::base::ids::TileIdx;
use crate::rules::defs::{Domain, Route, TerrainType};
use crate::state::map::Tile;
use crate::unique::Ctx;

/// What a step needs to know of one tile, for one mover: the half of `enter_cost` that reads the
/// tile alone. A search works it out once per tile it looks at; [`Mover::cost`] for the two tiles
/// of one step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    /// Land, or a city: what embarking reads.
    pub land: bool,
    /// Its route is a railroad (`route_at`).
    pub rail: bool,
    /// It has a road or railroad the mover may use (`has_connection`).
    pub conn: bool,
    /// Its river edges.
    pub river: u8,
    /// What entering it costs on top, when its owner is at war with the mover.
    pub extra: i32,
    /// Entering it over its terrain: the cost with `extra` included where Python added it, or
    /// [`ALL`] (`movement.py:362-377`).
    pub terrain: i32,
}

impl Mover<'_> {
    /// What the step from `a` to its neighbour `b` costs, zones of control included
    /// (`movement.enter_cost`).
    #[must_use]
    pub fn edge_cost(&self, a: TileIdx, b: TileIdx) -> i32 {
        self.cost(a, b, true)
    }

    /// What the step from `a` to its neighbour `b` costs, with or without zones of control
    /// (`movement.enter_cost(..., zoc)`).
    #[must_use]
    pub fn cost(&self, a: TileIdx, b: TileIdx, zoc: bool) -> i32 {
        let Some(d) = self.g.grid().neighbor_table(a).iter().position(|&n| n == b.0) else {
            return ALL;
        };
        match (self.facts(a), self.facts(b)) {
            (Some(fa), Some(fb)) => self.cost_from(a, d, &fa, &fb, zoc),
            _ => ALL,
        }
    }

    /// The step from `a` in direction `d` to the tile whose facts are `fb` (`movement.enter_cost`,
    /// in Python's order).
    #[inline]
    pub(crate) fn cost_from(&self, a: TileIdx, d: usize, fa: &Facts, fb: &Facts, zoc: bool) -> i32 {
        let sc = self.rules.scale;
        if self.def.domain == Domain::Land && fa.land != fb.land && !self.prof.on_water {
            return if fb.land { self.prof.disembark } else { self.prof.embark }.unwrap_or(ALL);
        }
        if zoc && !self.barbarian && !self.prof.ignores_zoc && self.zoc_toward(a, d) {
            return ALL;
        }
        if self.prof.all_1 {
            return sc;
        }
        if fa.rail && fb.rail {
            return sc / 10 + fb.extra;
        }
        // A river along the edge between them: both have rivers, and `a`'s lies toward `b`
        // (`movement.river_between`, `movement.py:277-285`).
        let crossing = fa.river & (1 << d) != 0 && fb.river != 0;
        if fa.conn && fb.conn && (!crossing || self.civ.rivers_ok) {
            let road = if self.civ.road_speed { sc / 3 } else { sc / 2 };
            return road + fb.extra;
        }
        if self.prof.ignores_terrain {
            return sc + fb.extra;
        }
        if crossing {
            return ALL;
        }
        fb.terrain
    }

    /// What a step needs to know of tile `t`; `None` off the map.
    pub(crate) fn facts(&self, t: TileIdx) -> Option<Facts> {
        let tile = self.g.tile(t)?;
        Some(self.facts_on(t, tile, self.g.city_at(t)))
    }

    /// Whether the mover may route through tile `t` on its way elsewhere
    /// ([`passable`](Self::passable)), and the facts of `t` its steps read: both at one look
    /// at the tile.
    pub(crate) fn look(&self, t: TileIdx) -> Option<(bool, Facts)> {
        let tile = self.g.tile(t)?;
        let city = self.g.city_at(t);
        let pass = self.passable_on(t, tile, city.map(crate::state::cities::City::owner), None);
        Some((pass, self.facts_on(t, tile, city)))
    }

    /// The facts of tile `t`, given what is on it.
    fn facts_on(
        &self,
        t: TileIdx,
        tile: &Tile,
        city: Option<&crate::state::cities::City>,
    ) -> Facts {
        let g = self.g;
        let own = if tile.route_pillaged() { None } else { tile.route() };
        let route = match city {
            Some(c) if g.has_tech(c.owner(), self.rules.rail_tech) => Some(Route::Railroad),
            Some(c) if g.has_tech(c.owner(), self.rules.road_tech) => own.or(Some(Route::Road)),
            _ => own,
        };
        let conn = route.is_some()
            || (self.civ.forest_roads
                && tile.owner() == Some(self.pid)
                && top_non_hill(tile, self.rules.hill)
                    .is_some_and(|f| Some(f) == self.rules.forest || Some(f) == self.rules.jungle));
        let extra = match tile.owner() {
            Some(o) if self.at_war(o) => self.extra(o),
            _ => 0,
        };
        let water = g.rules().terrains()[tile.terrain()].kind == TerrainType::Water;
        Facts {
            land: !water || city.is_some(),
            rail: route == Some(Route::Railroad),
            conn,
            river: tile.river_mask(),
            extra,
            terrain: self.terrain_cost(t, tile, city.is_some(), extra),
        }
    }

    /// Entering tile `b` over its terrain (`movement.py:362-377`), `extra` included where Python
    /// added it.
    fn terrain_cost(&self, b: TileIdx, tb: &Tile, city: bool, extra: i32) -> i32 {
        let g = self.g;
        let r = g.rules();
        let sc = self.rules.scale;
        let terrain_cost =
            if city { sc } else { r.terrains()[governing(g, tb)].movement_cost.saturating_mul(sc) };
        let half = terrain_cost / 2 + extra;
        let features = tb.features();
        let doubles = &self.prof.doubles;
        if doubles.iter().any(|d| {
            matches!(d.on, DoubleOn::Feature(f) if features.contains(f)) && self.double_holds(d, b)
        }) {
            return half;
        }
        if self.prof.rough_penalty && rough(g, tb) {
            return ALL;
        }
        if features.contains(self.rules.hill) && self.civ.hill_ignore {
            return sc + extra;
        }
        if doubles.iter().any(|d| {
            matches!(d.on, DoubleOn::Base(t) if t == tb.terrain()) && self.double_holds(d, b)
        }) {
            return half;
        }
        let f = r.uniques().filters();
        if doubles.iter().any(|d| match d.on {
            DoubleOn::Filter(id) => {
                self.double_holds(d, b) && f.tile_matches(id, &g.view(), b, Some(self.pid))
            }
            _ => false,
        }) {
            return half;
        }
        terrain_cost + extra
    }

    /// Whether a double-movement unique's conditionals hold on the tile entered, asked of the
    /// unit there (`movement.py:363`).
    fn double_holds(&self, d: &super::class::Double, b: TileIdx) -> bool {
        if !d.conditional {
            return true;
        }
        let v = self.g.view();
        let ctx = Ctx { civ: Some(self.pid), unit: self.unit, tile: Some(b), ..Ctx::default() };
        crate::unique::applies(d.id, &ctx.resolve(&v), &v)
    }

    /// The route a tile gives (`movement.route_at`, `movement.py:288-299`): its route if not
    /// pillaged; a city centre is a railroad once its owner knows the railroad's tech, and a road
    /// once it knows the road's.
    #[must_use]
    pub fn route_at(&self, t: TileIdx) -> Option<Route> {
        route_at(self.g, self.rules, t)
    }

    /// Whether a tile has a road or railroad this civilization may use (`movement.has_connection`,
    /// `movement.py:302-309`): a route, or its own forest or jungle when those count as roads.
    #[must_use]
    pub fn connected(&self, t: TileIdx) -> bool {
        if self.route_at(t).is_some() {
            return true;
        }
        let Some(tile) = self.g.tile(t) else { return false };
        self.civ.forest_roads
            && tile.owner() == Some(self.pid)
            && top_non_hill(tile, self.rules.hill)
                .is_some_and(|f| Some(f) == self.rules.forest || Some(f) == self.rules.jungle)
    }

    /// Whether an enemy exerts a zone of control on the step from `a` to `b`: an enemy on a tile
    /// next to both (`movement.zoc_between`, `movement.py:312-327`).
    #[must_use]
    pub fn zoc_between(&self, a: TileIdx, b: TileIdx) -> bool {
        self.g
            .grid()
            .neighbor_table(a)
            .iter()
            .position(|&n| n == b.0)
            .is_some_and(|d| self.zoc_toward(a, d))
    }

    /// Whether an enemy exerts a zone of control on the step from `a` in direction `d`: the two
    /// tiles next to both ends are `a`'s neighbours on either side of that direction.
    #[inline]
    pub(crate) fn zoc_toward(&self, a: TileIdx, d: usize) -> bool {
        let zoc = self.zoc();
        if zoc.none {
            return false;
        }
        let zoc = &zoc.tiles;
        let nb = self.g.grid().neighbor_table(a);
        [nb[(d + 5) % 6], nb[(d + 1) % 6]]
            .into_iter()
            .any(|n| n != crate::base::hex::NO_TILE && zoc.contains(n))
    }
}

/// The route a tile gives, as [`Mover::route_at`].
pub(crate) fn route_at(
    g: &crate::game::Game,
    rules: &super::class::MoveRules,
    t: TileIdx,
) -> Option<Route> {
    let tile = g.tile(t)?;
    let own = if tile.route_pillaged() { None } else { tile.route() };
    if let Some(c) = g.city_at(t) {
        if g.has_tech(c.owner(), rules.rail_tech) {
            return Some(Route::Railroad);
        }
        if g.has_tech(c.owner(), rules.road_tech) {
            return own.or(Some(Route::Road));
        }
    }
    own
}

/// Whether a river runs along the edge between two neighbours (`movement.river_between`,
/// `movement.py:277-285`): both tiles have rivers, and `a`'s lies on the edge toward `b`.
#[must_use]
pub fn river_between(
    grid: &crate::base::hex::HexGrid,
    ta: &Tile,
    tb: &Tile,
    a: TileIdx,
    b: TileIdx,
) -> bool {
    if ta.river_mask() == 0 || tb.river_mask() == 0 {
        return false;
    }
    match grid.neighbor_table(a).iter().position(|&n| n == b.0) {
        Some(d) => ta.river_mask() & (1 << d) != 0,
        None => false,
    }
}

/// Whether any terrain on a tile is rough (`tiles.is_rough`, `tiles.py:70-74`).
#[must_use]
pub fn rough(g: &crate::game::Game, t: &Tile) -> bool {
    let r = g.rules();
    let terrains = r.terrains();
    terrains[t.terrain()].rough
        || t.wonder().is_some_and(|w| terrains[w].rough)
        || t.features()
            .iter()
            .any(|f| r.derived().features.get(f).is_some_and(|&x| terrains[x].rough))
}

/// A tile's top feature that is not a hill (`Tile.feature`, `state.py:100-106`).
#[must_use]
pub fn top_non_hill(
    t: &Tile,
    hill: crate::base::ids::FeatureId,
) -> Option<crate::base::ids::FeatureId> {
    let mut f = t.features();
    f.remove(hill);
    f.top()
}

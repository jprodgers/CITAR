//! A city's borders (`cities.py:927-1085`, UnCiv's `CityExpansionManager`): the culture its next
//! tile costs, which tile it claims when its borders grow, and buying a tile with gold, which the
//! tool `buy_tile` runs (`tools.py:760-770`).
//!
//! Python chose among tiles that rank the same by the first in `within`'s order, which sorts by
//! distance and then by the cube offsets it enumerated (`hexmap.py:138-157`); the engine's
//! `within` goes ring by ring (DESIGN.md 4.3), so the choice takes Python's order explicitly
//! ([`within_order`]).
//!
//! What waits for other packages, marked where it happens: a barbarian camp on a claimed tile is
//! destroyed (1c-06), and foreign units that may not stay in the new territory are moved out
//! (1c-02).

use serde_json::json;

use super::super::action::{OutcomeSpec, Rule};
use super::super::derive::rev::{CityTouch, PlayerTouch};
use super::super::error::ActionError;
use super::super::{Game, Porting, pending};
use super::citizens::{own_city, tile_at};
use super::stats::work_range;
use crate::base::hex::Cube;
use crate::base::ids::{CityId, PlayerId, ResourceId, TileIdx};
use crate::base::num;
use crate::game::economy;
use crate::rules::defs::ResourceType;
use crate::state::TileClaim;
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// The tiles a city owns beyond its first ring, which the costs of more grow with
/// (`cities.tiles_claimed`, `cities.py:927-930`).
#[must_use]
pub fn tiles_claimed(g: &Game, c: CityId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let centre = city.tile();
    let n = economy::city_tiles(g, c)
        .into_iter()
        .filter(|&t| t != centre && g.grid().distance(t, centre) > 1)
        .count();
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// The culture a city's next tile costs (`cities.culture_to_next_tile`, `cities.py:933-942`):
/// `6 * (claimed + 1.4813)^1.3`, times the speed, half again for a city-state, times `[n]%
/// Culture cost of natural border growth [cities]`; rounded.
#[must_use]
pub fn culture_to_next_tile(g: &Game, c: CityId) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let mut cost = 6.0 * num::pow(f64::from(tiles_claimed(g, c).max(0)) + 1.4813, 1.3);
    cost *= g.speed().culture_cost_modifier;
    if g.is_city_state(city.owner()) {
        cost *= 1.5;
    }
    let t = g.rules().uniques();
    let v = g.view();
    for h in uq::city(&v, c, UniqueType::BorderGrowthPercentage, &Ctx::city(&v, c)) {
        if let UniqueData::BorderGrowthPercentage(x) = *h.data()
            && t.filters().city_matches(x.cities, &v, c, None)
        {
            for _ in 0..h.n {
                cost *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    num::round_half_even_i32(cost)
}

/// Whether a civilization can see a resource yet: it needs the tech that reveals it
/// (`tiles.resource_visible`, `tiles.py:143-148`).
fn resource_visible(g: &Game, p: PlayerId, r: ResourceId) -> bool {
    g.has_tech(p, g.rules().resources()[r].revealed_by)
}

/// Python's order of the tiles within a distance of `centre` (`hexmap.within`,
/// `hexmap.py:138-157`): by distance, then by the cube offset `(dq, dr)` it enumerated, the
/// nearest copy of a tile on a map that wraps.
#[must_use]
pub fn within_order(g: &Game, centre: TileIdx, t: TileIdx) -> (u32, i32, i32) {
    let grid = g.grid();
    let (x, y) = grid.unwrapped_xy(centre, t);
    let at = Cube::from_offset(x, y);
    let c = grid.cube(centre);
    (grid.distance(centre, t), at.q - c.q, at.r - c.r)
}

/// How a tile ranks for a city's borders to grow into, lower being better
/// (`cities.rank_tile_for_expansion`, `cities.py:945-995`): nearer, with a resource or natural
/// wonder, what it yields, a resource or wonder beside it; worse contested.
#[must_use]
pub fn rank_tile_for_expansion(g: &Game, c: CityId, t: TileIdx) -> i64 {
    let Some(city) = g.city(c) else { return i64::MAX };
    let Some(tile) = g.tile(t) else { return i64::MAX };
    let r = g.rules();
    let wr = work_range(g);
    let d = g.grid().distance(t, city.tile());
    let p = city.owner();
    let mut score = i64::from(d) * 100;
    match tile.resource().filter(|&x| resource_visible(g, p, x)) {
        Some(res) => {
            if r.resources()[res].kind != ResourceType::Bonus {
                score -= 105;
            } else if d <= wr {
                score -= 104;
            }
        }
        None => {
            if g.is_water(t) {
                score += 3;
            }
            if d > wr {
                score += 100;
            }
        }
    }
    if tile.wonder().is_some() {
        score -= 105;
    }
    let yields = super::super::derive::stats::tile_yield(g, t, Some(p), Some(c));
    score -= num::trunc_i64(yields.iter().map(|(_, x)| x).sum());
    let Some(pl) = g.player(p) else { return score };
    let (mut adj_nw, mut contested) = (false, false);
    for n in g.grid().neighbors(t) {
        let Some(nt) = g.tile(n) else { continue };
        if !pl.explored.contains(n.0) || nt.owner() == Some(p) {
            continue;
        }
        if nt.owner().is_some() {
            contested = true;
            continue;
        }
        let nd = g.grid().distance(city.tile(), n);
        if let Some(res) = nt.resource()
            && resource_visible(g, p, res)
            && (nd <= wr || r.resources()[res].kind != ResourceType::Bonus)
        {
            score -= 1;
        }
        if nt.wonder().is_some() {
            if !adj_nw {
                score -= 1;
            }
            adj_nw = true;
        }
    }
    if contested {
        score += 10;
    }
    score
}

/// The tiles a city's borders can grow into: unowned, within its expansion range, beside one of
/// its own (`cities.choosable_tiles`, `cities.py:998-1001`).
#[must_use]
pub fn choosable_tiles(g: &Game, c: CityId) -> Vec<TileIdx> {
    let Some(city) = g.city(c) else { return Vec::new() };
    let range = u32::try_from(g.rules().constants().formulas.city_expand_range).unwrap_or(0);
    g.grid()
        .within(city.tile(), range)
        .into_iter()
        .filter(|&t| g.tile(t).is_some_and(|x| x.owner().is_none()))
        .filter(|&t| g.grid().neighbors(t).any(|n| g.tile(n).and_then(|x| x.city()) == Some(c)))
        .collect()
}

/// A city takes a tile (`cities.take_ownership`, `cities.py:1004-1028`): another city working it
/// lets it go, and the tile is the city's.
pub fn take_ownership(g: &mut Game, c: CityId, t: TileIdx) {
    let Some(owner) = g.city(c).map(crate::state::cities::City::owner) else { return };
    let Some(tile) = g.tile(t).copied() else { return };
    if let Some(other) = tile.city().filter(|&x| x != c)
        && g.city(other).is_some_and(|x| x.worked.contains(&t) || x.locked.contains(&t))
        && let Some(x) = g.city_mut(other, CityTouch::WORK)
    {
        x.worked.retain(|&w| w != t);
        x.locked.retain(|&w| w != t);
    }
    let camp = g.rules().derived().known.barbarian_camp;
    if camp.is_some() && tile.improvement() == camp {
        // barbarians.remove_camp (cities.py:1019-1021).
        pending(Porting::Pending("1c-06"));
    }
    if let Err(e) = g.set_tile_owner(t, TileClaim::city(owner, c)) {
        debug_assert!(false, "a tile of the map could not be claimed: {e}");
    }
    if g.units_at(t).any(|u| u.owner() != owner && !g.can_enter_territory(u.owner(), t)) {
        // movement.teleport_to_closest for the units that may not stay (cities.py:1025-1028).
        pending(Porting::Pending("1c-02"));
    }
}

/// A city's borders grow into the best tile it can claim, if any (`cities.expand_borders`,
/// `cities.py:1031-1038`); the tile claimed.
pub fn expand_borders(g: &mut Game, c: CityId) -> Option<TileIdx> {
    let centre = g.city(c)?.tile();
    let best = choosable_tiles(g, c)
        .into_iter()
        .min_by_key(|&t| (rank_tile_for_expansion(g, c, t), within_order(g, centre, t)))?;
    take_ownership(g, c, best);
    Some(best)
}

/// What a tile costs a city to buy (`cities.buy_tile_cost`, `cities.py:1041-1049`): 50 for each
/// step beyond the first and 5 for each tile the city has claimed, times the speed and `[n]%
/// Gold cost of acquiring tiles [cities]`; rounded.
#[must_use]
pub fn buy_tile_cost(g: &Game, c: CityId, t: TileIdx) -> i32 {
    let Some(city) = g.city(c) else { return 0 };
    let d = f64::from(g.grid().distance(t, city.tile()));
    let mut cost = 50.0 * (d - 1.0) + f64::from(tiles_claimed(g, c)) * 5.0;
    cost *= g.speed().gold_cost_modifier;
    let tt = g.rules().uniques();
    let v = g.view();
    for h in uq::city(&v, c, UniqueType::TileCostPercentage, &Ctx::city(&v, c)) {
        if let UniqueData::TileCostPercentage(x) = *h.data()
            && tt.filters().city_matches(x.cities, &v, c, None)
        {
            for _ in 0..h.n {
                cost *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    num::round_half_even_i32(cost)
}

/// Why a city cannot buy a tile, if it cannot (`cities.can_buy_tile`, `cities.py:1052-1069`).
#[must_use]
pub fn can_buy_tile(g: &Game, c: CityId, t: TileIdx) -> Option<String> {
    let Some(city) = g.city(c) else { return Some("No such city.".into()) };
    if city.puppet || city.razing {
        return Some("Puppets and cities being razed cannot buy tiles.".into());
    }
    if g.tile(t).is_none_or(|x| x.owner().is_some()) {
        return Some("That tile is already owned.".into());
    }
    if city.resistance > 0 {
        return Some("The city is in resistance.".into());
    }
    let wr = work_range(g);
    if g.grid().distance(t, city.tile()) > wr {
        return Some(format!("Tiles can only be bought within {wr} tiles of the city."));
    }
    if !g.grid().neighbors(t).any(|n| g.tile(n).and_then(|x| x.city()) == Some(c)) {
        return Some("You can only buy tiles adjacent to the city's borders.".into());
    }
    None
}

/// `buy_tile`: a city buys an unowned tile beside its borders with gold (`tools.buy_tile`,
/// `cities.buy_tile`, `cities.py:1072-1085`); its citizens are placed again when the call
/// settles.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BuyTile {
    pub city_id: i64,
    pub x: i64,
    pub y: i64,
}

impl Rule for BuyTile {
    type Plan = (CityId, TileIdx, i32);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let t = tile_at(g, self.x, self.y)?;
        if let Some(reason) = can_buy_tile(g, c, t) {
            return Err(ActionError::rule(reason));
        }
        let cost = buy_tile_cost(g, c, t);
        let gold = g.player(pid).map_or(0.0, |x| x.econ.gold);
        if gold < f64::from(cost) {
            return Err(ActionError::rule(format!(
                "Buying that tile costs {cost} gold; you have {}.",
                num::trunc_i64(gold)
            )));
        }
        Ok((c, t, cost))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (c, t, cost): Self::Plan) -> OutcomeSpec {
        if let Some(x) = g.player_mut(pid, PlayerTouch::STOCKS) {
            x.econ.gold -= f64::from(cost);
        }
        take_ownership(g, c, t);
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.tiles_bought = x.tiles_bought.saturating_add(1);
        }
        let (x, y) = g.xy(t);
        OutcomeSpec::value(json!({"bought": {"x": x, "y": y}, "gold_spent": cost}))
    }
}

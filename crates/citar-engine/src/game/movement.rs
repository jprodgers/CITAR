//! Moving units (`movement.py:99-702`): movement allowance, steps and what entering a tile
//! does, standing move orders, and units sent home. What a step costs and the searches are
//! [`super::path`]'s; this module is the rules that write.
//!
//! **A move** (`move_toward`) follows the best path, or the route an order was given, step by
//! step while the unit has moves. It stops, with Python's reason, where the next tile holds one of
//! its own units it may not stop on, or a foreign unit it cannot capture; where a step is refused;
//! where the unit is lost; and where an enemy military unit comes into view. Each step runs
//! through `Game::step_seeing`, which settles sight on both sides of it, so the enemy check sees
//! exactly the tiles the step revealed; Python compared the enemy units it saw before and after,
//! which differs only when a step starts a war, and no step does.
//!
//! **A standing order** is kept when the unit ran out of moves or waits for the way to clear, for
//! at most [`ORDER_PATIENCE`] turns without getting further; it follows the route it was given
//! and never plans again. Package 1c-04 carries orders out at the start of each turn.

use serde_json::{Value, json};

use super::derive::rev::UnitTouch;
use super::error::{ActionError, ErrCode};
use super::path::{Blocked, Mover, PathKey, stack_reason};
use super::units::{self, promotions::add_promotion, remove_unit};
use super::{Game, Porting, pending};
use crate::base::ids::{PlayerId, TileIdx, UnitId};
use crate::rules::defs::Domain;
use crate::state::map::Tile;
use crate::state::units::Activity;
use crate::unique::{FilterFacts, UniqueData};

pub use super::units::health::{max_movement, max_moves};

/// Turns a standing move order waits for its next tile to clear before it is given up
/// (`movement.ORDER_PATIENCE`).
pub const ORDER_PATIENCE: u8 = 3;

/// Whether a land unit is at sea (`movement.is_embarked`, `movement.py:99-107`).
#[must_use]
pub fn is_embarked(g: &Game, u: UnitId) -> bool {
    g.view().unit_embarked(u)
}

/// Whether a unit of `base` for `p` could end its move on `t` (`movement.can_stand`); with a
/// unit, that unit where it stands.
#[must_use]
pub fn can_stand(
    g: &Game,
    p: PlayerId,
    base: crate::base::ids::BaseUnitId,
    t: TileIdx,
    u: Option<UnitId>,
) -> bool {
    let m = match u {
        Some(u) => Mover::unit(g, u),
        None => Mover::of_type(g, p, base),
    };
    m.is_some_and(|m| m.can_stand(t))
}

/// What the step from `a` to its neighbour `b` costs unit `u` (`movement.enter_cost`).
#[must_use]
pub fn enter_cost(g: &Game, u: UnitId, a: TileIdx, b: TileIdx) -> i32 {
    Mover::unit(g, u).map_or(super::path::ALL, |m| m.edge_cost(a, b))
}

/// The best path for `u` to `target` within `max_turns` turns (`movement.find_path`); a path
/// found at this revision already is not searched again.
#[must_use]
pub fn find_path(g: &Game, u: UnitId, target: TileIdx, max_turns: u32) -> Option<Vec<TileIdx>> {
    let x = g.unit(u)?;
    let key = PathKey { unit: u, from: x.tile(), moves: x.moves, target, max_turns };
    let now = g.derived().revs().now().get();
    let cache = g.derived().path_cache();
    if let Some(found) = cache.try_borrow().ok().and_then(|c| c.get(now, &key)) {
        return found;
    }
    let found = Mover::unit(g, u)?.find_path(target, max_turns);
    if let Ok(mut c) = cache.try_borrow_mut() {
        c.put(now, key, found.clone());
    }
    found
}

/// The turns a path takes `u` (`movement.path_turns`).
#[must_use]
pub fn path_turns(g: &Game, u: UnitId, path: &[TileIdx]) -> u32 {
    Mover::unit(g, u).map_or(1, |m| m.path_turns(path))
}

/// What `u` can reach this turn, with the movement it would have left there
/// (`movement.reachable_this_turn`).
#[must_use]
pub fn reachable_this_turn(g: &Game, u: UnitId) -> Vec<(TileIdx, i32)> {
    Mover::unit(g, u).map_or_else(Vec::new, |m| m.reachable())
}

/// Moves a unit one tile (`movement.step`, `movement.py:515-549`): it needs moves, a neighbour it
/// may pass into that is no foreign city and holds no foreign unit but a civilian it captures; it
/// pays the step, captures, moves, stops fortifying or sleeping, has acted, and enters the tile.
/// One look at the unit's movement ([`Mover`]) checks the step and prices it.
pub fn step(g: &mut Game, u: UnitId, nb: TileIdx) -> Result<(), Blocked> {
    let checked = {
        let m = Mover::unit(g, u).ok_or(Blocked::NoMoves)?;
        priced_step(&m, nb)?
    };
    take_step(g, u, nb, checked)
}

/// A step checked and priced ([`priced_step`]): the civilian it captures, and what it costs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Checked {
    capture: Option<UnitId>,
    cost: i32,
}

/// Checks the step of `m`'s unit onto `nb` ([`check_step`]) and prices it, with that one mover.
pub(crate) fn priced_step(m: &Mover<'_>, nb: TileIdx) -> Result<Checked, Blocked> {
    let capture = check_step(m, nb)?;
    let from = m.unit.and_then(|u| m.game().unit(u)).ok_or(Blocked::NoMoves)?.tile();
    Ok(Checked { capture, cost: m.edge_cost(from, nb) })
}

/// Takes a step [`priced_step`] allowed, before anything was written.
pub(crate) fn take_step(g: &mut Game, u: UnitId, nb: TileIdx, c: Checked) -> Result<(), Blocked> {
    if let Some(x) = g.unit_mut(u, UnitTouch::MOVES) {
        x.moves = if c.cost >= x.moves { 0 } else { x.moves - c.cost };
    }
    if let Some(v) = c.capture {
        units::capture::capture_civilian(g, u, v);
    }
    if g.relocate_unit(u, nb).is_err() {
        return Err(Blocked::NotAdjacent);
    }
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.fortify = 0;
        if matches!(
            x.activity,
            Some(Activity::Fortify | Activity::FortifyHeal | Activity::Sleep | Activity::SleepHeal)
        ) {
            x.activity = None;
        }
        x.acted = true;
    }
    on_enter_tile(g, u, nb);
    Ok(())
}

/// Whether unit `u` may step onto its neighbour `nb` now, and the civilian it would capture there
/// (the checks of `movement.step`, `movement.py:519-538`): it has moves, the tile is next to it
/// and passable, no foreign city, and holds no foreign unit but a civilian of an enemy's with no
/// military unit beside it, which a military unit captures.
pub fn step_check(g: &Game, u: UnitId, nb: TileIdx) -> Result<Option<UnitId>, Blocked> {
    check_step(&Mover::unit(g, u).ok_or(Blocked::NoMoves)?, nb)
}

/// [`step_check`] with the unit's movement already looked at: `m` is its mover.
pub(crate) fn check_step(m: &Mover<'_>, nb: TileIdx) -> Result<Option<UnitId>, Blocked> {
    let g = m.game();
    let x = m.unit.and_then(|u| g.unit(u)).ok_or(Blocked::NoMoves)?;
    if x.moves <= 0 {
        return Err(Blocked::NoMoves);
    }
    if !g.grid().neighbors(x.tile()).any(|n| n == nb) {
        return Err(Blocked::NotAdjacent);
    }
    if let Some(why) = m.pass_reason(nb) {
        return Err(why);
    }
    if g.city_at(nb).is_some_and(|c| c.owner() != m.pid) {
        return Err(Blocked::ForeignCity);
    }
    let r = g.rules();
    let mut capture = None;
    for other in g.units_at(nb) {
        let od = &r.base_units()[other.base];
        if other.owner() == m.pid || od.domain == Domain::Air {
            continue;
        }
        if !od.military && m.military() && m.at_war(other.owner()) && g.military_at(nb).is_none() {
            capture = Some(other.id());
            continue;
        }
        return Err(Blocked::ForeignUnit);
    }
    Ok(capture)
}

/// What entering a tile does (`movement.on_enter_tile`, `movement.py:552-562`): ancient ruins
/// for a major's unit, a barbarian camp cleared by a military unit, and the promotions natural
/// wonders around it grant.
pub fn on_enter_tile(g: &mut Game, u: UnitId, t: TileIdx) {
    let Some((owner, base)) = g.unit(u).map(|x| (x.owner(), x.base)) else { return };
    let known = &g.rules().derived().known;
    let imp = g.tile(t).and_then(Tile::improvement);
    let major = g.player(owner).is_some_and(crate::state::players::Player::is_major);
    if imp.is_some() && imp == known.ancient_ruins && major {
        // ruins.enter (ruins.py).
        pending(Porting::Pending("1b-08"));
    } else if imp.is_some()
        && imp == known.barbarian_camp
        && g.rules().base_units()[base].military
        && !g.is_barbarian(owner)
    {
        // barbarians.clear_camp (barbarians.py).
        pending(Porting::Pending("1c-06"));
    }
    if g.unit(u).is_some() {
        terrain_promotions(g, u);
    }
}

/// The promotions natural wonders grant the units next to them (`movement.terrain_promotions`,
/// `movement.py:565-576`): each terrain within one tile's `Grants [promotion] ... to adjacent
/// [units] units for the rest of the game`, whatever its conditionals, free.
pub fn terrain_promotions(g: &mut Game, u: UnitId) {
    let Some(tile) = g.unit(u).map(crate::state::units::Unit::tile) else { return };
    let r = g.rules();
    let t = r.uniques();
    let mut grants = Vec::new();
    for n in g.grid().within(tile, 1) {
        let Some(nt) = g.tile(n) else { continue };
        let terrains = core::iter::once(nt.terrain())
            .chain(nt.wonder())
            .chain(nt.features().iter().filter_map(|f| r.derived().features.get(f).copied()));
        for terrain in terrains {
            for id in r.terrains()[terrain].uniques.ids() {
                if let UniqueData::TerrainGrantsPromotion(x) = t.get(id).data {
                    grants.push((x.promotion, x.units));
                }
            }
        }
    }
    for (promotion, units) in grants {
        let has = g.unit(u).is_none_or(|x| x.promotions.contains(promotion));
        if !has && units::unit_matches(g, u, units) {
            add_promotion(g, u, promotion, true);
        }
    }
}

/// Why a move stopped short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    OutOfMoves,
    BlockedFriendly,
    BlockedForeign,
    /// A step was refused.
    Step(Blocked),
    UnitLost,
    EnemySpotted,
    /// A standing order whose unit is no longer on its route.
    OffRoute,
}

impl Stop {
    /// Whether the unit waits with its order: out of moves, or its next tile taken.
    #[must_use]
    pub const fn waits(self) -> bool {
        matches!(self, Self::OutOfMoves | Self::BlockedFriendly | Self::BlockedForeign)
    }

    /// What Python said.
    #[must_use]
    pub fn text(self, g: &Game) -> String {
        match self {
            Self::OutOfMoves => "out of moves".into(),
            Self::BlockedFriendly => "blocked by a friendly unit".into(),
            Self::BlockedForeign => "blocked by a foreign unit".into(),
            Self::Step(b) => b.text(g),
            Self::UnitLost => "unit lost".into(),
            Self::EnemySpotted => "enemy spotted".into(),
            Self::OffRoute => "no longer on its planned route".into(),
        }
    }
}

/// What a move did (`movement.move_toward`'s result).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveResult {
    pub from: TileIdx,
    /// Where it ended, `None` if it was lost.
    pub to: Option<TileIdx>,
    pub arrived: bool,
    /// Movement left, in move-scale units.
    pub moves_left: i32,
    pub stopped: Option<Stop>,
    /// The order was given up.
    pub gave_up: bool,
    /// The turns without progress an order waited before it was given up.
    pub stalled: Option<u8>,
    pub order_kept: bool,
    pub turns_remaining: u32,
}

impl MoveResult {
    /// The tool's result, as Python's dict.
    #[must_use]
    pub fn to_json(&self, g: &Game) -> Value {
        let xy = |t: TileIdx| {
            let (x, y) = g.xy(t);
            json!({"x": x, "y": y})
        };
        let sc = f64::from(g.rules().constants().move_scale);
        let stopped = if self.arrived {
            Value::Null
        } else {
            self.stopped.map_or(Value::Null, |s| {
                let mut text = s.text(g);
                if let Some(n) = self.stalled {
                    text.push_str(&format!(", no progress for {n} turns"));
                }
                Value::String(text)
            })
        };
        let moves_left = if self.to.is_some() {
            json!(crate::base::num::round_ndigits(f64::from(self.moves_left) / sc, 2))
        } else {
            json!(0)
        };
        json!({
            "from": xy(self.from),
            "to": self.to.map_or(Value::Null, xy),
            "arrived": self.arrived,
            "moves_left": moves_left,
            "stopped": stopped,
            "order_kept": self.order_kept,
            "gave_up": self.gave_up,
            "turns_remaining": self.turns_remaining,
        })
    }
}

/// Moves a unit toward `target` as far as its moves go this turn, keeping the destination as a
/// standing order where it must wait (`movement.move_toward`, `movement.py:583-671`). A new
/// order plans its route; `continuing` follows the route the order was given. Refused with
/// Python's message when no path exists.
pub fn move_toward(
    g: &mut Game,
    u: UnitId,
    target: TileIdx,
    set_goto: bool,
    continuing: bool,
) -> Result<MoveResult, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::new(ErrCode::NoSuchUnit, "No such unit."))?;
    let start = x.tile();
    let planned = x.path.clone();
    let path: Vec<TileIdx> = if continuing && !planned.is_empty() {
        let on_route = planned.iter().position(|&t| t == start);
        match (planned.last() == Some(&target), on_route) {
            (true, Some(i)) => planned[i..].to_vec(),
            _ => {
                clear_order(g, u, true);
                if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                    x.activity = None;
                }
                let moves_left = g.unit(u).map_or(0, |x| x.moves);
                return Ok(MoveResult {
                    from: start,
                    to: Some(start),
                    arrived: false,
                    moves_left,
                    stopped: Some(Stop::OffRoute),
                    gave_up: true,
                    stalled: None,
                    order_kept: false,
                    turns_remaining: 0,
                });
            }
        }
    } else {
        find_path(g, u, target, 40).ok_or_else(|| {
            ActionError::new(
                ErrCode::NoPath,
                format!("No path from {} to {}.", g.fmt_xy(start), g.fmt_xy(target)),
            )
        })?
    };
    Ok(follow(g, u, target, path, set_goto, continuing))
}

/// Moves a unit along `path` toward `target`, as [`move_toward`] does once it has its path: a new
/// order (`continuing` false) starts its wait afresh.
pub fn follow(
    g: &mut Game,
    u: UnitId,
    target: TileIdx,
    path: Vec<TileIdx>,
    set_goto: bool,
    continuing: bool,
) -> MoveResult {
    let Some((owner, start, base)) = g.unit(u).map(|x| (x.owner(), x.tile(), x.base)) else {
        return MoveResult {
            from: target,
            to: None,
            arrived: false,
            moves_left: 0,
            stopped: Some(Stop::UnitLost),
            gave_up: false,
            stalled: None,
            order_kept: false,
            turns_remaining: 0,
        };
    };
    if !continuing && let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.order_wait = 0;
    }
    let r = g.rules();
    let military = r.base_units()[base].military;
    let mut stop = None;
    for &nb in path.iter().skip(1) {
        // One look at the unit's movement for everything this step asks of it.
        let checked = {
            let (Some(m), Some((here, moves))) =
                (Mover::unit(g, u), g.unit(u).map(|x| (x.tile(), x.moves)))
            else {
                stop = Some(Stop::UnitLost);
                break;
            };
            if moves <= 0 {
                stop = Some(Stop::OutOfMoves);
                break;
            }
            let others: Vec<PlayerId> = g
                .units_at(nb)
                .filter(|o| o.owner() != owner && r.base_units()[o.base].domain != Domain::Air)
                .map(crate::state::units::Unit::owner)
                .collect();
            if others.is_empty()
                && stack_reason(g, owner, base, nb, Some(u)).is_some()
                && (nb == target || m.edge_cost(here, nb) >= moves)
            {
                stop = Some(Stop::BlockedFriendly);
                break;
            }
            let capturable = !others.is_empty()
                && military
                && g.at_war(owner, others[0])
                && !g
                    .units_at(nb)
                    .filter(|o| o.owner() != owner && r.base_units()[o.base].domain != Domain::Air)
                    .any(|o| r.base_units()[o.base].military);
            if !others.is_empty() && !capturable {
                stop = Some(Stop::BlockedForeign);
                break;
            }
            match priced_step(&m, nb) {
                Ok(c) => c,
                Err(why) => {
                    stop = Some(Stop::Step(why));
                    break;
                }
            }
        };
        let (stepped, seen) = g.step_seeing(owner, |g| take_step(g, u, nb, checked));
        if let Err(why) = stepped {
            stop = Some(Stop::Step(why));
            break;
        }
        if g.unit(u).is_none() {
            stop = Some(Stop::UnitLost);
            break;
        }
        if super::vis::enemy_spotted(g, owner, &seen) && nb != target {
            stop = Some(Stop::EnemySpotted);
            break;
        }
    }
    let now = g.unit(u).map(|x| x.tile());
    let alive = now.is_some();
    let arrived = now == Some(target);
    let mut stalled = None;
    if alive {
        if arrived {
            clear_order(g, u, true);
        } else if set_goto && stop.is_some_and(Stop::waits) {
            let moved = now != Some(start);
            let mut wait = g.unit(u).map_or(0, |x| x.order_wait);
            if moved {
                wait = 0;
            } else if stop != Some(Stop::OutOfMoves) {
                wait = wait.saturating_add(1);
            }
            if wait >= ORDER_PATIENCE {
                stalled = Some(wait);
                clear_order(g, u, true);
            } else if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                x.order_wait = wait;
                x.activity = Some(Activity::Goto);
                x.goto = Some(target);
                if !continuing || x.path.is_empty() {
                    x.path.clone_from(&path);
                }
            }
        } else if stop != Some(Stop::OutOfMoves) {
            clear_order(g, u, true);
        }
    }
    let (moves_left, goto) = g.unit(u).map_or((0, None), |x| (x.moves, x.goto));
    let order_kept = alive && goto == Some(target);
    let turns_remaining =
        if !arrived && alive && goto.is_some() { remaining_turns(g, u, &path, target) } else { 0 };
    MoveResult {
        from: start,
        to: now,
        arrived,
        moves_left,
        stopped: if arrived { None } else { stop },
        gave_up: stalled.is_some(),
        stalled,
        order_kept,
        turns_remaining,
    }
}

/// Ends a unit's move order: its destination, route and wait, and a `goto` activity with
/// `activity`.
fn clear_order(g: &mut Game, u: UnitId, activity: bool) {
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        if activity && x.activity == Some(Activity::Goto) {
            x.activity = None;
        }
        x.goto = None;
        x.path.clear();
        x.order_wait = 0;
    }
}

/// The turns left on the planned path from where the unit stopped (`movement._remaining_turns`,
/// `movement.py:674-680`), or on a new path if it left it.
fn remaining_turns(g: &Game, u: UnitId, path: &[TileIdx], target: TileIdx) -> u32 {
    let Some(at) = g.unit(u).map(crate::state::units::Unit::tile) else { return 0 };
    if let Some(i) = path.iter().position(|&t| t == at) {
        let rest = &path[i..];
        if rest.last() == Some(&target) {
            return path_turns(g, u, rest);
        }
    }
    let fresh = find_path(g, u, target, 40).unwrap_or_else(|| vec![at]);
    path_turns(g, u, &fresh)
}

/// Sends a unit that may not stay where it is to the nearest tile it may be on, within seven
/// rings, or out of the game (`movement.teleport_to_closest`, `movement.py:693-702`,
/// `UnitMovement.teleportToClosestMoveableTile`). The tiles of a ring are tried in the grid's
/// ring order.
pub fn teleport_to_closest(g: &mut Game, u: UnitId) {
    let spot = {
        let Some(m) = Mover::unit(g, u) else { return };
        let Some(at) = g.unit(u).map(crate::state::units::Unit::tile) else { return };
        (1..8).find_map(|r| {
            g.grid().ring(at, r).into_iter().find(|&t| {
                let owner = g.tile(t).and_then(Tile::owner);
                (owner.is_none_or(|o| o == m.pid || g.can_enter_territory(m.pid, t)))
                    && m.can_stand(t)
            })
        })
    };
    match spot {
        Some(t) => {
            let _moved = g.relocate_unit(u, t).is_ok();
        }
        None => remove_unit(g, u),
    }
}

/// Sends home every unit of `side` standing in `other`'s land, when a peace ends their war
/// (`diplomacy.make_peace`, `diplomacy.py:248-250`).
pub fn send_home(g: &mut Game, side: PlayerId, other: PlayerId) {
    let ids: Vec<UnitId> = g.state().units().of(side).to_vec();
    for u in ids {
        let Some(t) = g.unit(u).map(crate::state::units::Unit::tile) else { continue };
        if g.tile(t).and_then(Tile::owner) == Some(other) {
            teleport_to_closest(g, u);
        }
    }
}

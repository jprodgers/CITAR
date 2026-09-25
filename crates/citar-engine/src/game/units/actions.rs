//! The unit tools (`tools.py:412-630`): `move_unit`, `unit_order`, `upgrade_unit` and
//! `promote_unit`, as typed actions (DESIGN.md 8.3).
//!
//! Each checks on `&Game` everything Python refused, with Python's messages, before anything is
//! written; Python refused some after it had started writing (a move with no path had already
//! cleared the unit's orders, an upgrade it could not place had removed and remade the unit), so
//! a refusal here leaves the game as it was. The orders whose systems later packages port are
//! refused as not ported: an aircraft's move (a rebase, package 1c-03), `explore`, `automate`
//! and `pillage` (package 1c-04).

use serde_json::{Value, json};

use super::promotions::{add_promotion, plan_promotion, promotion_names, unit_label};
use super::upgrades::{UpgradePlan, apply_upgrade, disband, plan_upgrade};
use super::{unit_has, unit_uniques};
use crate::base::ids::{PlayerId, PromotionId, TileIdx, UnitId};
use crate::game::Game;
use crate::game::action::{OutcomeSpec, Rule};
use crate::game::derive::rev::UnitTouch;
use crate::game::error::{ActionError, ErrCode};
use crate::game::movement;
use crate::game::path::{Blocked, Mover, stack_reason};
use crate::rules::defs::Domain;
use crate::state::units::{Activity, Unit};
use crate::unique::UniqueType;

/// The refusal of an order whose system is not ported yet (DESIGN.md 3.4, rule 4).
fn not_ported(path: &str) -> ActionError {
    ActionError::new(
        ErrCode::NotPorted,
        format!("This order is not ported to the new engine yet ({path})."),
    )
}

/// One of the caller's own units (`tools._own_unit`, `tools.py:154-166`); the refusal lists the
/// units they do have.
pub fn own_unit(g: &Game, pid: PlayerId, unit_id: i64) -> Result<UnitId, ActionError> {
    let found = u32::try_from(unit_id)
        .ok()
        .and_then(UnitId::new)
        .filter(|&u| g.unit(u).is_some_and(|x| x.owner() == pid));
    found.ok_or_else(|| {
        let r = g.rules();
        let ids: Vec<String> = g
            .player_units(pid)
            .map(|x| format!("#{} {}", x.id().get(), r.name(x.base).unwrap_or("")))
            .collect();
        let list = if ids.is_empty() { "none".to_owned() } else { ids.join(", ") };
        ActionError::new(
            ErrCode::NoSuchUnit,
            format!(
                "You have no unit with id {unit_id} (units are used up by some actions and lost \
                 when killed). Your units now: {list}."
            ),
        )
    })
}

/// A tile by its coordinates (`tools._idx`, `tools.py:143-151`); the refusal names the map's
/// size.
pub fn tile_at(g: &Game, x: i64, y: i64) -> Result<TileIdx, ActionError> {
    let found =
        i32::try_from(x).ok().zip(i32::try_from(y).ok()).and_then(|(x, y)| g.grid().idx(x, y));
    found.ok_or_else(|| {
        let grid = g.grid();
        ActionError::new(
            ErrCode::OffMap,
            format!("({x},{y}) is off the map (map is {}x{}).", grid.width(), grid.height()),
        )
    })
}

// ---- move_unit -------------------------------------------------------------------------------------

/// `move_unit`: moves a unit toward a tile along the best path, as far as this turn allows, and
/// keeps going on later turns (`tools.move_unit`, `tools.py:412-448`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MoveUnit {
    pub unit_id: i64,
    pub x: i64,
    pub y: i64,
}

/// A checked move: the unit, where to, and the path it will follow.
pub struct MovePlan {
    unit: UnitId,
    target: TileIdx,
    path: Vec<TileIdx>,
}

impl Rule for MoveUnit {
    type Plan = MovePlan;

    // refcheck: unit-refusals-change-nothing (Python woke the unit before it looked for a path)
    fn check(&self, g: &Game, pid: PlayerId) -> Result<MovePlan, ActionError> {
        let u = own_unit(g, pid, self.unit_id)?;
        let target = tile_at(g, self.x, self.y)?;
        let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
        if g.rules().base_units()[x.base].domain == Domain::Air {
            // combat.rebase (combat.py:1044-1075).
            return Err(not_ported("game::combat::air"));
        }
        let name = unit_label(g, u);
        if target == x.tile() {
            return Err(ActionError::rule(format!(
                "{name} is already at ({},{}).",
                self.x, self.y
            )));
        }
        if x.moves <= 0 {
            let why = match x.activity {
                Some(Activity::Explore) => " (its explore order already moved it this turn)",
                Some(Activity::Goto) => " (its standing move order already moved it this turn)",
                Some(Activity::Automate) => " (its automated orders already used them)",
                _ => "",
            };
            return Err(ActionError::rule(format!(
                "{name} has no moves left this turn{why}. Nothing happened. Moves refresh next turn."
            )));
        }
        let path = movement::find_path(g, u, target, 40).ok_or_else(|| {
            ActionError::new(
                ErrCode::NoPath,
                format!("No path from {} to {}.", g.fmt_xy(x.tile()), g.fmt_xy(target)),
            )
        })?;
        // A move that goes nowhere is an error unless the unit waits with its order: its first
        // step refused (`tools.py:442-446`).
        if let Some(why) = first_step_refused(g, u, &path, target) {
            return Err(ActionError::rule(format!(
                "{name} could not move toward ({},{}): {}.",
                self.x,
                self.y,
                why.text(g)
            )));
        }
        Ok(MovePlan { unit: u, target, path })
    }

    fn apply(self, g: &mut Game, _: PlayerId, plan: MovePlan) -> OutcomeSpec {
        let u = plan.unit;
        let name = unit_label(g, u);
        if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
            x.activity = None;
        }
        let res = movement::follow(g, u, plan.target, plan.path, true, false);
        let mut out = res.to_json(g);
        if res.to == Some(res.from) && !res.arrived && res.order_kept {
            let stopped = res.stopped.map(|s| s.text(g)).unwrap_or_default();
            if let Some(m) = out.as_object_mut() {
                m.insert(
                    "note".into(),
                    Value::String(format!(
                        "{name} could not move this turn ({stopped}); it will keep trying next turn."
                    )),
                );
            }
        }
        OutcomeSpec::value(out)
    }
}

/// Why the first step of a new move would be refused, if it would (what `move_toward` meets
/// first, `movement.py:613-632`): `None` when it steps, or waits with its order because the tile
/// holds a unit.
fn first_step_refused(g: &Game, u: UnitId, path: &[TileIdx], target: TileIdx) -> Option<Blocked> {
    let &nb = path.get(1)?;
    let x = g.unit(u)?;
    let m = Mover::unit(g, u)?;
    let owner = x.owner();
    let r = g.rules();
    let foreign: Vec<&Unit> = g
        .units_at(nb)
        .filter(|o| o.owner() != owner && r.base_units()[o.base].domain != Domain::Air)
        .collect();
    if foreign.is_empty() && stack_reason(g, owner, x.base, nb, Some(u)).is_some() {
        let cost = m.edge_cost(x.tile(), nb);
        if nb == target || cost >= x.moves {
            return None;
        }
    }
    let military = r.base_units()[x.base].military;
    let capturable = !foreign.is_empty()
        && military
        && g.at_war(owner, foreign[0].owner())
        && !foreign.iter().any(|o| r.base_units()[o.base].military);
    if !foreign.is_empty() && !capturable {
        return None;
    }
    movement::check_step(&m, nb).err()
}

// ---- unit_order ------------------------------------------------------------------------------------

/// `unit_order`: a standing order, or clearing one (`tools.unit_order`, `tools.py:540-602`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnitOrder {
    pub unit_id: i64,
    pub order: String,
}

/// A checked order.
pub enum OrderPlan {
    Fortify(UnitId),
    Rest(UnitId, Activity),
    Clear(UnitId),
    Skip(UnitId),
    SetUp(UnitId),
    AlreadySetUp,
    Disband(UnitId),
}

impl Rule for UnitOrder {
    type Plan = OrderPlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<OrderPlan, ActionError> {
        let u = own_unit(g, pid, self.unit_id)?;
        let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
        let def = &g.rules().base_units()[x.base];
        match self.order.trim().to_lowercase().as_str() {
            "fortify" => {
                if !def.military {
                    return Err(ActionError::rule("Civilians cannot fortify; use sleep."));
                }
                Ok(OrderPlan::Fortify(u))
            }
            "sleep" => Ok(OrderPlan::Rest(u, Activity::Sleep)),
            "heal" => Ok(OrderPlan::Rest(u, Activity::Heal)),
            "wake" | "cancel" => Ok(OrderPlan::Clear(u)),
            "skip" => Ok(OrderPlan::Skip(u)),
            "explore" => {
                if !def.military && def.domain == Domain::Land {
                    return Err(ActionError::rule("Civilians cannot explore."));
                }
                // automation.explore (automation.py).
                Err(not_ported("game::automation"))
            }
            "automate" => {
                if unit_uniques(g, u, UniqueType::BuildImprovements, false).is_empty() {
                    return Err(ActionError::rule(
                        "Only units that build improvements (Workers) can be automated.",
                    ));
                }
                // automation.automate_worker (automation.py).
                Err(not_ported("game::automation"))
            }
            // workers.pillage (workers.py).
            "pillage" => Err(not_ported("game::workers")),
            "setup" => {
                if !unit_has(g, u, UniqueType::MustSetUp, false) {
                    return Err(ActionError::rule("This unit does not need to set up."));
                }
                if x.set_up {
                    return Ok(OrderPlan::AlreadySetUp);
                }
                if x.moves <= 0 {
                    return Err(ActionError::rule("No movement left."));
                }
                Ok(OrderPlan::SetUp(u))
            }
            "disband" => Ok(OrderPlan::Disband(u)),
            _ => Err(ActionError::rule(
                "Unknown order. Use fortify, sleep, wake, skip, heal, explore, automate, pillage, \
                 setup, disband, cancel.",
            )),
        }
    }

    fn apply(self, g: &mut Game, _: PlayerId, plan: OrderPlan) -> OutcomeSpec {
        let ok = json!({"ok": true});
        let sc = g.rules().constants().move_scale;
        // An order in place of a move order ends the move order: its route and wait too.
        let set = |g: &mut Game, u: UnitId, a: Option<Activity>| {
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                x.activity = a;
                x.goto = None;
                x.path.clear();
                x.order_wait = 0;
            }
        };
        OutcomeSpec::value(match plan {
            OrderPlan::Fortify(u) => {
                set(g, u, Some(Activity::Fortify));
                json!({
                    "ok": true,
                    "note": "Fortification builds up by 20% per turn (max 40%) while the unit stays put."
                })
            }
            OrderPlan::Rest(u, a) => {
                set(g, u, Some(a));
                ok
            }
            OrderPlan::Clear(u) => {
                set(g, u, None);
                ok
            }
            OrderPlan::Skip(u) => {
                if let Some(x) = g.unit_mut(u, UnitTouch::MOVES) {
                    x.moves = 0;
                }
                ok
            }
            OrderPlan::AlreadySetUp => json!({"ok": true, "note": "Already set up."}),
            OrderPlan::SetUp(u) => {
                if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
                    x.moves = x.moves.saturating_sub(sc).max(0);
                    x.set_up = true;
                }
                ok
            }
            OrderPlan::Disband(u) => disband(g, u),
        })
    }
}

// ---- upgrade_unit ----------------------------------------------------------------------------------

/// `upgrade_unit`: upgrades an obsolete unit for gold, in its owner's land (`tools.upgrade_unit`,
/// `tools.py:605-610`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpgradeUnit {
    pub unit_id: i64,
}

impl Rule for UpgradeUnit {
    type Plan = UpgradePlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<UpgradePlan, ActionError> {
        plan_upgrade(g, own_unit(g, pid, self.unit_id)?)
    }

    fn apply(self, g: &mut Game, _: PlayerId, plan: UpgradePlan) -> OutcomeSpec {
        OutcomeSpec::value(apply_upgrade(g, plan))
    }
}

// ---- promote_unit ----------------------------------------------------------------------------------

/// `promote_unit`: spends experience on a promotion (`tools.promote_unit`, `tools.py:613-630`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromoteUnit {
    pub unit_id: i64,
    pub promotion: String,
}

impl Rule for PromoteUnit {
    type Plan = (UnitId, PromotionId);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<(UnitId, PromotionId), ActionError> {
        let u = own_unit(g, pid, self.unit_id)?;
        Ok((u, plan_promotion(g, u, &self.promotion)?))
    }

    fn apply(self, g: &mut Game, _: PlayerId, (u, pr): (UnitId, PromotionId)) -> OutcomeSpec {
        add_promotion(g, u, pr, false);
        OutcomeSpec::render(move |g| json!({"promotions": promotion_names(g, u)}))
    }
}

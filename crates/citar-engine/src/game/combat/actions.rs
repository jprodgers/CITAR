//! The combat and conquest tools (`tools.py:456-491, 772-781, 803-830`): `attack`, `air_sweep`,
//! `city_attack`, `city_status` and `return_civilian`, as typed actions (DESIGN.md 8.3).
//!
//! Each checks on `&Game` everything Python refused, with Python's messages, before anything is
//! written; the result is known once the action has applied, so it is reported as it was then.

use serde_json::Value;

use super::air::{air_strike, air_sweep, plan_air_strike, plan_air_sweep};
use super::city::{city_bombard, plan_bombard};
use super::nuke::{NukePlan, nuke, plan_nuke};
use super::resolve::{attack, validate_attack};
use crate::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use crate::game::Game;
use crate::game::action::{OutcomeSpec, Rule};
use crate::game::conquest::{CityFate, apply_fate, plan_fate};
use crate::game::error::ActionError;
use crate::game::lookup::own_city;
use crate::game::lookup::{own_unit, tile_at};
use crate::game::units::capture::{plan_return_civilian, return_civilian};
use crate::game::units::type_has;
use crate::rules::defs::Domain;
use crate::unique::{Combatant, UniqueType};

/// `attack`: a unit attacks a tile (`tools.attack_tool`, `tools.py:456-476`): a nuclear weapon
/// detonates there, an aircraft strikes it, anything else attacks as a melee or ranged unit.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Attack {
    pub unit_id: i64,
    pub x: i64,
    pub y: i64,
}

/// A checked attack, by the kind of unit that makes it.
pub enum AttackPlan {
    Nuke(UnitId, TileIdx, NukePlan),
    Air(UnitId, Combatant),
    Ground(UnitId, Combatant),
}

/// Checks `unit`'s attack on `t` as the tool dispatches it, for the tool and the test operation
/// `attack_as`.
///
/// # Errors
/// The attack's refusal.
pub fn plan_attack(g: &Game, u: UnitId, t: TileIdx) -> Result<AttackPlan, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    if type_has(g, x.base, UniqueType::NuclearWeapon) {
        return Ok(AttackPlan::Nuke(u, t, plan_nuke(g, u, t)?));
    }
    if g.rules().base_units()[x.base].domain == Domain::Air {
        return Ok(AttackPlan::Air(u, plan_air_strike(g, u, t)?));
    }
    Ok(AttackPlan::Ground(u, validate_attack(g, u, t)?))
}

/// Carries out a checked attack, and says what happened.
pub fn apply_attack(g: &mut Game, plan: AttackPlan) -> Value {
    match plan {
        AttackPlan::Nuke(u, t, p) => nuke(g, u, t, p),
        AttackPlan::Air(u, d) => air_strike(g, u, d),
        AttackPlan::Ground(u, d) => attack(g, u, d),
    }
}

impl Rule for Attack {
    type Plan = AttackPlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<AttackPlan, ActionError> {
        let u = own_unit(g, pid, self.unit_id)?;
        let t = tile_at(g, self.x, self.y)?;
        plan_attack(g, u, t)
    }

    fn apply(self, g: &mut Game, _: PlayerId, plan: AttackPlan) -> OutcomeSpec {
        OutcomeSpec::value(apply_attack(g, plan))
    }
}

/// `air_sweep`: a fighter sweeps a tile of interceptors before bombers go in
/// (`tools.air_sweep`, `tools.py:479-490`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AirSweep {
    pub unit_id: i64,
    pub x: i64,
    pub y: i64,
}

impl Rule for AirSweep {
    type Plan = (UnitId, TileIdx);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<(UnitId, TileIdx), ActionError> {
        let u = own_unit(g, pid, self.unit_id)?;
        let t = tile_at(g, self.x, self.y)?;
        plan_air_sweep(g, u, t)?;
        Ok((u, t))
    }

    fn apply(self, g: &mut Game, _: PlayerId, (u, t): (UnitId, TileIdx)) -> OutcomeSpec {
        OutcomeSpec::value(air_sweep(g, u, t))
    }
}

/// `city_attack`: a city bombards an enemy in range, once a turn (`tools.city_attack`,
/// `tools.py:772-781`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CityAttack {
    pub city_id: i64,
    pub x: i64,
    pub y: i64,
}

impl Rule for CityAttack {
    type Plan = (CityId, Combatant);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<(CityId, Combatant), ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let t = tile_at(g, self.x, self.y)?;
        Ok((c, plan_bombard(g, c, t)?))
    }

    fn apply(self, g: &mut Game, _: PlayerId, (c, d): (CityId, Combatant)) -> OutcomeSpec {
        OutcomeSpec::value(city_bombard(g, c, d))
    }
}

/// `city_status`: what to do with a conquered city: annex it, keep it a puppet, raze it, stop
/// razing it, or liberate it (`tools.city_status`, `tools.py:816-830`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CityStatus {
    pub city_id: i64,
    /// Python looked it up in a dict, so anything but one of the five names is refused alike.
    pub status: Value,
}

impl Rule for CityStatus {
    type Plan = (CityId, CityFate);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<(CityId, CityFate), ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let fate = CityFate::from_name(self.status.as_str().unwrap_or("")).ok_or_else(|| {
            ActionError::rule("status must be annex, puppet, raze, stop_razing or liberate.")
        })?;
        plan_fate(g, pid, c, fate)?;
        Ok((c, fate))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (c, fate): (CityId, CityFate)) -> OutcomeSpec {
        OutcomeSpec::value(apply_fate(g, pid, c, fate))
    }
}

/// `return_civilian`: gives a civilian taken back from the barbarians to the civilization it was
/// taken from, or keeps it (`tools.return_civilian`, `tools.py:803-813`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReturnCivilian {
    pub unit_id: i64,
    /// Read as Python's `bool(keep)` read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<Value>,
}

impl Rule for ReturnCivilian {
    type Plan = (UnitId, PlayerId);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<(UnitId, PlayerId), ActionError> {
        let u = own_unit(g, pid, self.unit_id)?;
        Ok((u, plan_return_civilian(g, u)?))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (u, orig): (UnitId, PlayerId)) -> OutcomeSpec {
        let keep = self.keep.as_ref().is_some_and(crate::base::py::truthy);
        OutcomeSpec::value(return_civilian(g, pid, u, orig, keep))
    }
}

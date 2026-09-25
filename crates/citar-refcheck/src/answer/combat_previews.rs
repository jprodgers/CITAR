//! The `combat_previews` group (DESIGN.md 9.2, package 1c-03): a sample of the fights units and
//! cities could start, with each fight's strengths, modifiers and damage at the rolls 0, 0.5 and 1,
//! the engine's own preview for ground and sea units, and for aircraft whether they can attack
//! now and who would intercept them (`scripts/refcheck/queries.py::combat_previews`).
//!
//! Python sampled the fights and wrote the attacker, where it attacks from and the target next
//! to each answer, so the Rust side asks the same questions of the same pairs: the defender is
//! whatever defends the target now, the numbers come from one `combat::setup`, and the
//! interception candidates are every unit of the defender's (the defender itself aside) that
//! could intercept over the target, in id order. `attackers` counts every unit and city that has
//! something to attack, as the sampling saw them.

use citar_engine::base::ids::{CityId, TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::combat::{air, city, combatant, resolve, strength};
use citar_engine::game::units::{health, type_has};
use citar_engine::rules::defs::Domain;
use citar_engine::unique::{Combatant, UniqueType};
use serde_json::{Map, Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `combat_previews` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct CombatPreviews;

impl AnswerModule for CombatPreviews {
    fn group(&self) -> Group {
        Group::CombatPreviews
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let fights: Vec<Value> = expected
            .get("fights")
            .and_then(Value::as_array)
            .map(|v| v.iter().map(|e| fight(g, e)).collect())
            .unwrap_or_default();
        Ok(json!({ "attackers": attackers(g), "fights": fights }))
    }
}

/// How many units and cities have something to attack (`queries.combat_previews`' count): a
/// military unit that is no nuclear weapon, at a tile within its range that holds something, that
/// its owner sees (the barbarians see everything) and that it may attack; a city that is not the
/// barbarians', with a target to bombard.
fn attackers(g: &Game) -> usize {
    let r = g.rules();
    let mut n = 0;
    for u in g.state().units().iter() {
        let def = &r.base_units()[u.base];
        if !def.military || type_has(g, u.base, UniqueType::NuclearWeapon) {
            continue;
        }
        let a = Combatant::Unit(u.id());
        let owner = u.owner();
        let range = u32::try_from(health::attack_range(g, u.id())).unwrap_or(0);
        let any = g.grid().within(u.tile(), range).into_iter().any(|t| {
            t != u.tile()
                && combatant::combatant_at(g, t).is_some()
                && (g.is_barbarian(owner) || g.derived().vis().sees(owner, t))
                && resolve::contains_attackable_enemy(g, t, a).is_none()
        });
        n += usize::from(any);
    }
    for c in g.state().cities().iter() {
        if !g.is_barbarian(c.owner()) && !city::bombard_targets(g, c.id()).is_empty() {
            n += 1;
        }
    }
    n
}

/// A number the recording wrote.
fn num(e: &Value, key: &str) -> Option<u32> {
    e.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok())
}

/// A side's modifiers, keyed by source.
fn mods(m: &strength::Mods) -> Value {
    Value::Object(m.iter().map(|&(k, v)| (k.to_owned(), json!(v))).collect::<Map<_, _>>())
}

/// The rolls every fight's damage is asked at (`queries.ROLLS`).
const ROLLS: [f64; 3] = [0.0, 0.5, 1.0];

/// One sampled fight, answered from the game.
fn fight(g: &Game, e: &Value) -> Value {
    let recorded = e.get("attacker").cloned().unwrap_or(Value::Null);
    let Some(target) = num(e, "target").map(TileIdx) else { return Value::Null };
    let mut out = Map::new();
    let a = if let Some(c) = num(&recorded, "city").and_then(CityId::new) {
        let Some(x) = g.city(c) else { return Value::Null };
        out.insert(
            "attacker".into(),
            json!({ "city": c.get(), "owner": x.owner().0, "from": x.tile().0 }),
        );
        Combatant::City(c)
    } else if let Some(u) = num(&recorded, "unit").and_then(UnitId::new) {
        let Some(x) = g.unit(u) else { return Value::Null };
        let what = g.rules().name(x.base).unwrap_or("");
        out.insert(
            "attacker".into(),
            json!({ "unit": u.get(), "type": what, "owner": x.owner().0, "from": x.tile().0 }),
        );
        Combatant::Unit(u)
    } else {
        return Value::Null;
    };
    out.insert("target".into(), json!(target.0));
    let Some(d) = combatant::combatant_at(g, target) else { return Value::Object(out) };
    out.insert(
        "defender".into(),
        match d {
            Combatant::City(c) => json!({ "city": c.get() }),
            Combatant::Unit(u) => json!({ "unit": u.get() }),
        },
    );
    let from = combatant::tile(g, a);
    let s = strength::setup(g, a, from, d, false);
    out.insert("attacker_strength".into(), json!(s.attack));
    out.insert("defender_strength".into(), json!(s.defense));
    out.insert("attack_modifiers".into(), mods(&s.attack_modifiers));
    out.insert("defense_modifiers".into(), mods(&s.defense_modifiers));
    out.insert("damage_to_defender".into(), json!(ROLLS.map(|r| s.damage_to_defender(r))));
    out.insert("damage_to_attacker".into(), json!(ROLLS.map(|r| s.damage_to_attacker(r))));
    if let Combatant::Unit(u) = a {
        let air = g.unit(u).is_some_and(|x| g.rules().base_units()[x.base].domain == Domain::Air);
        if air {
            out.insert("can_attack_now".into(), json!(resolve::can_attack_now(g, u)));
            out.insert("interception".into(), interception(g, u, d, target));
        } else {
            let preview = resolve::preview(g, u, target)
                .unwrap_or_else(|err| json!({ "error": err.message }));
            out.insert("preview".into(), preview);
        }
    }
    Value::Object(out)
}

/// What an air strike on `target` meets before its fight (`queries._interception`): nothing for
/// an aircraft that cannot be intercepted; otherwise each unit of the defender's that could
/// intercept there, in id order, with its chance, its damage factor and its damage at the rolls.
fn interception(g: &Game, aircraft: UnitId, d: Combatant, target: TileIdx) -> Value {
    if air::cannot_be_intercepted(g, aircraft) {
        return json!({ "immune": true });
    }
    let owner = combatant::owner(g, d);
    let candidates: Vec<Value> = g
        .player_units(owner)
        .map(|x| x.id())
        .filter(|&u| Combatant::Unit(u) != d && air::can_intercept(g, u, target))
        .map(|u| {
            let ic = Combatant::Unit(u);
            let at = combatant::tile(g, ic);
            let s = strength::setup(g, ic, at, Combatant::Unit(aircraft), false);
            let what = g.unit(u).and_then(|x| g.rules().name(x.base)).unwrap_or("");
            json!({
                "unit": u.get(),
                "type": what,
                "chance": air::intercept_chance(g, u),
                "factor": air::interception_factor(g, u, aircraft),
                "damage": ROLLS.map(|r| s.damage_to_defender(r)),
            })
        })
        .collect();
    json!({ "immune": false, "candidates": candidates })
}

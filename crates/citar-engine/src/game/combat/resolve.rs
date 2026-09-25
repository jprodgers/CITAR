//! Targeting and fighting (`combat.py:422-888`): whether a unit may attack a tile, the preview,
//! and a fight from its first blow to what the winner does next.
//!
//! **One stream per fight.** Python drew a fight's rolls from the game's saved Mersenne Twister
//! (`g.rng`), so a fight's outcome depended on every draw before it. Here each combat event takes
//! the next `combat_seq` and draws from its own stream, keyed by the turn, that number and both
//! sides (DESIGN.md 7.2): the two rolls, the blows, a withdrawal's tile and a capture's chance,
//! in that order.
//!
//! **Deliberate differences** (`tests/rules/intended.toml`):
//! - a civilian a ranged attack brings to no health dies (Python left it on the map at 0, and
//!   reported it captured), and a fight reports a capture only when a melee unit took the
//!   civilian (`civilians-under-fire`);
//! - `upon being defeated` fires for the unit that dies, which Python never fired (DESIGN.md
//!   5.9); what fires applies to its civilization once it is gone ([`kill_unit`]).

use serde_json::{Map, Value, json};
use smallvec::SmallVec;

use super::combatant::{self, combatant_at};
use super::strength::{self, CombatSetup};
use crate::base::ids::{PlayerId, TileIdx, UnitId};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::stats::Stat;
use crate::game::derive::rev::{CityTouch, PlayerTouch, UnitTouch};
use crate::game::error::ActionError;
use crate::game::units::{self, health, promotions, unit_has};
use crate::game::{Game, Porting, movement, pending, triggers, vis};
use crate::rules::defs::Domain;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::units::Activity;
use crate::unique::params::CostOrStrength;
use crate::unique::trigger::{TriggerEvent, TriggerSite};
use crate::unique::world::CombatAction;
use crate::unique::{CombatCtx, Combatant, Ctx, UniqueData, UniqueType, UnitFacts, uq};

/// A combat event's own stream of draws (DESIGN.md 7.2): the event's number, taken now, keys it
/// with the turn and both sides.
pub(crate) fn stream(g: &mut Game, purpose: Purpose, a: u64, b: u64) -> Rng {
    let seq = g.next_combat_seq();
    Rng::keyed(g.state().seed(), purpose, &[g.turn().key(), seq, a, b])
}

/// A unit's type and id as messages name it: `Warrior #12`.
fn label(g: &Game, u: UnitId) -> String {
    let what = g.unit(u).and_then(|x| g.rules().name(x.base)).unwrap_or("");
    format!("{what} #{}", u.get())
}

/// A player's name, for messages.
pub(crate) fn player_name(g: &Game, p: PlayerId) -> String {
    g.player(p).map(|x| x.name.to_string()).unwrap_or_default()
}

// ---- Targeting (combat.py:425-509) ------------------------------------------------------------

/// Why a unit cannot attack now, or `None` if it can (`combat.can_attack_now`,
/// `combat.py:425-435`): a civilian never can, nor a unit with no movement left or no attack left
/// this turn.
#[must_use]
pub fn can_attack_now(g: &Game, u: UnitId) -> Option<String> {
    let x = g.unit(u)?;
    if !g.rules().base_units()[x.base].military {
        return Some("Civilian units cannot attack.".to_owned());
    }
    if x.moves <= 0 {
        return Some(format!("{} has no movement left this turn.", label(g, u)));
    }
    if i32::from(x.attacks) >= health::max_attacks(g, u) {
        return Some(format!("{} has already attacked this turn.", label(g, u)));
    }
    None
}

/// Whether a civilization's land units may embark yet (`movement.civ_can_embark`): never the
/// barbarians'.
fn civ_can_embark(g: &Game, p: PlayerId) -> bool {
    !g.is_barbarian(p)
        && uq::any(uq::civ(&g.view(), p, UniqueType::LandUnitEmbarkation, &Ctx::civ(p)))
}

/// Why the enemy on tile `t` cannot be attacked by `att`, or `None` if it can
/// (`combat.contains_attackable_enemy`, `combat.py:438-469`): an embarked unit only makes melee
/// attacks onto land unless it may attack at sea; there must be something there, not one's own,
/// of someone at war with the attacker; a land unit strikes water only once its civilization may
/// embark; and the attacker's own `Cannot attack`, `Can only attack [...] units` and `Can only
/// attack [...] tiles`.
#[must_use]
pub fn contains_attackable_enemy(g: &Game, t: TileIdx, att: Combatant) -> Option<String> {
    let owner = combatant::owner(g, att);
    if let Combatant::Unit(u) = att
        && movement::is_embarked(g, u)
        && !unit_has(g, u, UniqueType::AttackOnSea, false)
        && (g.is_water(t) || combatant::is_ranged(g, att))
    {
        return Some("Embarked units can only make melee attacks onto land.".to_owned());
    }
    let Some(tc) = combatant_at(g, t) else {
        return Some("There is nothing to attack there.".to_owned());
    };
    let their = combatant::owner(g, tc);
    if their == owner {
        return Some("That is your own.".to_owned());
    }
    if !g.at_war(owner, their) {
        return Some(format!(
            "You are not at war with {}. Declare war first.",
            player_name(g, their)
        ));
    }
    if let Combatant::Unit(_) = att
        && combatant::is_land(g, att)
        && combatant::is_melee(g, att)
        && g.is_water(t)
        && !civ_can_embark(g, owner)
    {
        return Some("Land units cannot attack water tiles yet.".to_owned());
    }
    let Combatant::Unit(u) = att else { return None };
    let v = g.view();
    let ctx = Ctx {
        civ: Some(owner),
        unit: Some(u),
        tile: Some(t),
        combat: Some(CombatCtx {
            our: att,
            their: Some(tc),
            attacked_tile: Some(t),
            action: Some(CombatAction::Attack),
        }),
        ..Ctx::default()
    };
    let what = g.unit(u).and_then(|x| g.rules().name(x.base)).unwrap_or("");
    if unit_has(g, u, UniqueType::CannotAttack, false)
        && uq::any(uq::unit(&v, u, UniqueType::CannotAttack, &ctx))
    {
        return Some(format!("{what} cannot attack."));
    }
    let t_ = g.rules().uniques();
    let f = t_.filters();
    let mut only_units: SmallVec<[_; 2]> = SmallVec::new();
    for h in uq::unit(&v, u, UniqueType::CanOnlyAttackUnits, &ctx) {
        if let UniqueData::CanOnlyAttackUnits(x) = h.data() {
            only_units.extend(core::iter::repeat_n(x.combatants, usize::from(h.n)));
        }
    }
    if !only_units.is_empty() && !only_units.iter().any(|&x| f.combatant_matches(x, &v, tc)) {
        let names: Vec<&str> = only_units.iter().map(|&x| t_.combatant_filter(x)).collect();
        return Some(format!("{what} can only attack {} targets.", names.join(", ")));
    }
    let mut only_tiles: SmallVec<[_; 2]> = SmallVec::new();
    for h in uq::unit(&v, u, UniqueType::CanOnlyAttackTiles, &ctx) {
        if let UniqueData::CanOnlyAttackTiles(x) = h.data() {
            only_tiles.extend(core::iter::repeat_n(x.tiles, usize::from(h.n)));
        }
    }
    if !only_tiles.is_empty() && !only_tiles.iter().any(|&x| f.tile_matches(x, &v, t, Some(owner)))
    {
        let names: Vec<&str> = only_tiles.iter().map(|&x| t_.tile_filter(x)).collect();
        return Some(format!("{what} can only attack {} tiles.", names.join(", ")));
    }
    None
}

/// Checks a ground or sea unit's attack on tile `t` completely and returns what would defend it
/// (`combat.validate_attack`, `combat.py:472-509`), shared by the preview and the attack, so a
/// preview never says what an attack would refuse.
///
/// # Errors
/// The refusal, with Python's text: the unit cannot attack now, it is an aircraft, the tile is
/// out of sight or holds nothing it may attack, out of range or line of sight, a unit that must
/// set up has no movement to, a melee unit is not beside the target, or a city with no defences
/// left meets a unit that cannot take it.
pub fn validate_attack(g: &Game, u: UnitId, t: TileIdx) -> Result<Combatant, ActionError> {
    if let Some(why) = can_attack_now(g, u) {
        return Err(ActionError::rule(why));
    }
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    let r = g.rules();
    let def = &r.base_units()[x.base];
    if def.domain == Domain::Air {
        return Err(ActionError::rule("Aircraft attack with air_strike."));
    }
    if !g.is_barbarian(x.owner()) && !g.derived().vis().sees(x.owner(), t) {
        return Err(ActionError::rule("You cannot see that tile."));
    }
    let a = Combatant::Unit(u);
    if let Some(why) = contains_attackable_enemy(g, t, a) {
        return Err(ActionError::rule(why));
    }
    let dist = g.grid().distance(x.tile(), t);
    if def.ranged {
        let range = health::attack_range(g, u);
        if i64::from(dist) > i64::from(range) {
            return Err(ActionError::rule(format!(
                "Target is {dist} tiles away; range is {range}."
            )));
        }
        if dist > 1
            && !unit_has(g, u, UniqueType::IndirectFire, true)
            && !vis::has_los(g, x.tile(), t)
        {
            return Err(ActionError::rule(
                "No line of sight to the target (hills, forest, jungle or mountains in the way).",
            ));
        }
        if unit_has(g, u, UniqueType::MustSetUp, false)
            && !x.set_up
            && x.moves < r.constants().move_scale + 1
        {
            return Err(ActionError::rule(format!(
                "{} must set up before attacking (needs 1 movement to set up and some left to \
                 fire).",
                r.name(x.base).unwrap_or("")
            )));
        }
    } else if dist != 1 {
        return Err(ActionError::rule(
            "Melee units can only attack adjacent tiles (move next to the target first).",
        ));
    }
    let d =
        combatant_at(g, t).ok_or_else(|| ActionError::rule("There is nothing to attack there."))?;
    if let Combatant::City(c) = d
        && combatant::defeated(g, d)
        && !combatant::is_melee(g, a)
    {
        let name = g.city(c).map(|c| c.name.to_string()).unwrap_or_default();
        return Err(ActionError::rule(format!(
            "{name} has no defenses left: attack with a melee unit to capture it."
        )));
    }
    Ok(d)
}

/// A modifier as the preview lists it: `Flanking +20%`.
fn mod_lines(m: &strength::Mods) -> Value {
    Value::Array(m.iter().map(|&(k, v)| Value::String(format!("{k} {v:+}%"))).collect())
}

/// A fight predicted without starting it (`combat.preview`, `combat.py:512-538`): the fight's
/// numbers gathered once, and the damage each side takes at the lowest and highest roll.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub setup: CombatSetup,
    pub ranged: bool,
    /// `[lowest roll, highest roll]`.
    pub damage_to_defender: [i32; 2],
    pub damage_to_attacker: [i32; 2],
    pub defender_hp: i32,
    /// A city with no defences left, which a melee attack takes.
    pub city_down: bool,
}

/// Predicts unit `u`'s attack on tile `t` (`combat.preview`): [`validate_attack`]'s checks, then
/// one [`strength::setup`] and the damage at both ends of the roll, so the numbers bracket what
/// can happen. Python rebuilt the modifier stacks about six times (`combat.py:512-538`).
///
/// # Errors
/// [`validate_attack`]'s refusals.
pub fn preview_of(g: &Game, u: UnitId, t: TileIdx) -> Result<Preview, ActionError> {
    let d = validate_attack(g, u, t)?;
    let a = Combatant::Unit(u);
    let setup = strength::setup(g, a, combatant::tile(g, a), d, false);
    Ok(Preview {
        ranged: combatant::is_ranged(g, a),
        damage_to_defender: [setup.damage_to_defender(0.0), setup.damage_to_defender(1.0)],
        damage_to_attacker: [setup.damage_to_attacker(0.0), setup.damage_to_attacker(1.0)],
        defender_hp: combatant::hp(g, d),
        city_down: matches!(d, Combatant::City(_)) && combatant::defeated(g, d),
        setup,
    })
}

/// The preview as the tool and `inspect` report it (`combat.preview`, `combat.py:525-538`): the
/// final strengths to a decimal, both sides' modifiers, whether it is ranged, the damage each
/// side takes at the lowest and highest roll, and the defender with its hit points.
///
/// # Errors
/// [`validate_attack`]'s refusals.
pub fn preview(g: &Game, u: UnitId, t: TileIdx) -> Result<Value, ActionError> {
    let p = preview_of(g, u, t)?;
    let s = &p.setup;
    let mut out = Map::new();
    out.insert("attacker_strength".into(), json!(num::round_ndigits(s.attack, 1)));
    out.insert("defender_strength".into(), json!(num::round_ndigits(s.defense, 1)));
    out.insert("attacker_modifiers".into(), mod_lines(&s.attack_modifiers));
    out.insert("defender_modifiers".into(), mod_lines(&s.defense_modifiers));
    out.insert("ranged".into(), json!(p.ranged));
    out.insert("damage_to_defender".into(), json!(p.damage_to_defender));
    out.insert("damage_to_attacker".into(), json!(p.damage_to_attacker));
    out.insert("defender".into(), json!(combatant::name(g, s.defender)));
    out.insert("defender_hp".into(), json!(p.defender_hp));
    if let Combatant::City(_) = s.defender {
        out.insert("target".into(), json!("city"));
        if p.city_down {
            out.insert(
                "note".into(),
                json!("City defenses are down: a melee attack will capture it."),
            );
        }
    } else {
        out.insert("target".into(), json!("unit"));
    }
    Ok(Value::Object(out))
}

// ---- A fight (combat.py:544-871) -------------------------------------------------------------

/// What the fight needs to know of a side once it may be gone: whether it was a civilian, where
/// it stood, and the facts its filters read.
#[derive(Clone, Copy, Debug)]
struct Victim {
    side: Combatant,
    civilian: bool,
    tile: TileIdx,
    facts: Option<UnitFacts>,
}

impl Victim {
    fn of(g: &Game, d: Combatant) -> Self {
        Self {
            side: d,
            civilian: combatant::is_civilian(g, d),
            tile: combatant::tile(g, d),
            facts: match d {
                Combatant::Unit(u) => Some(UnitFacts::of(&g.view(), u)),
                Combatant::City(_) => None,
            },
        }
    }
}

/// Damage to one side, one point or a whole blow (`Combatant.take_damage`, `combat.py:102-112`):
/// a unit down to 0 at least, a city to 1 at least, with the turn it was hit, which city healing
/// reads. Returns the new hit points.
fn wound(g: &mut Game, c: Combatant, to: i32) {
    match c {
        Combatant::Unit(u) => {
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
                x.hp = i16::try_from(to.clamp(0, 100)).unwrap_or(0);
            }
        }
        Combatant::City(x) => {
            let turn = g.turn();
            if let Some(city) = g.city_mut(x, CityTouch::CORE) {
                city.health = to.max(1);
                city.damaged_turn = turn;
            }
        }
    }
}

/// The blows of a fight (`combat._take_damage`, `combat.py:553-582`): the damage both ways at two
/// rolls; then a melee attack on a civilian captures it, a ranged attack (not an aircraft's)
/// deals its damage, and anything else trades blows one point at a time, each to the side the
/// roll picks in proportion to what it has left to take, until one side is done for. Returns
/// the damage dealt to the defender and to the attacker, and whether a civilian was captured.
pub(crate) fn blows(
    g: &mut Game,
    rng: &mut Rng,
    a: Combatant,
    d: Combatant,
    from: TileIdx,
    sweeping: bool,
) -> (i32, i32) {
    let s: CombatSetup = strength::setup(g, a, from, d, sweeping);
    let r1 = rng.unit();
    let r2 = rng.unit();
    let mut pd = s.damage_to_defender(r2).max(0);
    let mut pa = s.damage_to_attacker(r1).max(0);
    let (ah, dh) = (combatant::hp(g, a), combatant::hp(g, d));
    let (mut a_hp, mut d_hp) = (ah, dh);
    let (mut a_hit, mut d_hit) = (false, false);
    let mut captured = false;
    let city_floor = |c: Combatant| i32::from(matches!(c, Combatant::City(_)));
    if let (Combatant::Unit(au), Combatant::Unit(du)) = (a, d)
        && combatant::is_civilian(g, d)
        && combatant::is_melee(g, a)
    {
        units::capture::capture_civilian(g, au, du);
        captured = true;
    } else if combatant::is_ranged(g, a) && !combatant::is_air(g, a) {
        d_hp = (d_hp - pd).max(city_floor(d));
        d_hit = true;
    } else {
        while pd + pa > 0 {
            let n = u64::try_from(pd + pa).unwrap_or(1);
            if rng.below(n) < u64::try_from(pd).unwrap_or(0) {
                pd -= 1;
                d_hp = (d_hp - 1).max(city_floor(d));
                d_hit = true;
                if d_hp <= city_floor(d) {
                    break;
                }
            } else {
                pa -= 1;
                a_hp = (a_hp - 1).max(city_floor(a));
                a_hit = true;
                if a_hp <= city_floor(a) {
                    break;
                }
            }
        }
    }
    if d_hit {
        wound(g, d, d_hp);
    }
    if a_hit {
        wound(g, a, a_hp);
    }
    let dealt_d = if captured { 0 } else { dh - d_hp };
    plunder_from_damage(g, a, d, dealt_d);
    (dealt_d, ah - a_hp)
}

/// Gold and the like taken by a unit that plunders in proportion to the damage it deals
/// (`combat._plunder_from_damage`, `combat.py:585-598`).
fn plunder_from_damage(g: &mut Game, a: Combatant, d: Combatant, dmg: i32) {
    let Combatant::Unit(u) = a else { return };
    if dmg <= 0 {
        return;
    }
    let owner = combatant::owner(g, a);
    let mut plunders: SmallVec<[(i32, crate::base::ids::CombatantFilterId, Stat); 2]> =
        SmallVec::new();
    {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        for h in uq::unit_and_civ(&v, u, UniqueType::DamageUnitsPlunder, &ctx) {
            if let UniqueData::DamageUnitsPlunder(x) = *h.data() {
                plunders.extend(core::iter::repeat_n(
                    (x.percent, x.combatants, x.into),
                    usize::from(h.n),
                ));
            }
        }
    }
    let f = g.rules().uniques().filters();
    for (percent, who, into) in plunders {
        if !f.combatant_matches(who, &g.view(), d) {
            continue;
        }
        let amount = num::trunc_i32(f64::from(percent) / 100.0 * f64::from(dmg));
        if amount == 0 {
            continue;
        }
        g.add_stat(owner, into, f64::from(amount));
        let what = combatant::name(g, a);
        let text = format!(
            "{}'s {what} plundered {amount} {} from {}.",
            player_name(g, owner),
            into.name(),
            combatant::name(g, d)
        );
        let at = combatant::tile(g, d);
        let audience = core::iter::once(owner).collect();
        g.emit(EngineEvent::Plunder, &text, Some(audience), Some(at), EventData::default(), &[]);
    }
}

/// Takes a defeated unit out and tells both sides (`combat._kill_unit`, `combat.py:601-610`):
/// `upon being defeated` fires for it, and `upon losing a [unit]` for its owner after.
///
/// What `upon being defeated` fires is found while the unit stands, since its own uniques are
/// among those that fire, and applied once it is gone, at its civilization alone: an effect on
/// the unit itself (an upgrade, a heal, its own destruction) would act on the dead, and a free
/// upgrade would put a new unit on the tile at no health.
pub(crate) fn kill_unit(g: &mut Game, victim: UnitId, killer: Option<PlayerId>, text: &str) {
    let Some((owner, at, base)) = g.unit(victim).map(|x| (x.owner(), x.tile(), x.base)) else {
        return;
    };
    let facts = UnitFacts::of(&g.view(), victim);
    let site = TriggerSite { civ: owner, city: None, unit: Some(victim), tile: None };
    let defeat = triggers::find(g, &site, &TriggerEvent::Defeat, true);
    units::remove_unit(g, victim);
    let gone = TriggerSite { unit: None, ..site };
    for id in defeat {
        triggers::apply(g, id, &gone, None);
    }
    let audience = core::iter::once(owner).chain(killer).collect();
    let data =
        EventData { unit_type: Some(base), owner: Some(owner), killer, ..EventData::default() };
    g.emit(EngineEvent::UnitKilled, text, Some(audience), Some(at), data, &[]);
    triggers::fire(g, &TriggerSite::civ(owner), &TriggerEvent::LosingUnit(facts), false, None);
    // victory.check_elimination (combat.py:609-610).
    pending(Porting::Pending("1c-08"));
}

/// Gold, faith or culture earned for a kill, where a unique grants it (`combat._earn_from_killing`,
/// `combat.py:613-643`): the killer's `Earn [n]% of killed [units] unit's [cost or strength]`,
/// and those of the first city within four tiles of the killer that has any of `... when killed
/// within 4 tiles of a city following this religion`.
fn earn_from_killing(g: &mut Game, killer: Combatant, dead: &Victim) {
    let Some(facts) = dead.facts else { return };
    let r = g.rules();
    let def = &r.base_units()[facts.base];
    let (strength, cost) = (def.strength.max(def.ranged_strength), def.cost);
    let owner = combatant::owner(g, killer);
    let at = combatant::tile(g, killer);
    let plunders: Vec<(i32, crate::base::ids::UnitFilterId, CostOrStrength, Stat)> = {
        let v = g.view();
        let ctx = Ctx {
            civ: Some(owner),
            combat: Some(CombatCtx {
                our: killer,
                their: Some(dead.side),
                attacked_tile: None,
                action: None,
            }),
            ..Ctx::default()
        }
        .resolve(&v);
        let mut out = Vec::new();
        let mut push = |h: uq::Hit<'_>| {
            let e = match *h.data() {
                UniqueData::KillUnitPlunder(x) => (x.percent, x.units, x.of, x.into),
                UniqueData::KillUnitPlunderNearCity(x) => (x.percent, x.units, x.of, x.into),
                _ => return,
            };
            out.extend(core::iter::repeat_n(e, usize::from(h.n)));
        };
        match killer {
            Combatant::Unit(u) => {
                uq::unit_and_civ(&v, u, UniqueType::KillUnitPlunder, &ctx).for_each(&mut push);
            }
            Combatant::City(_) => {
                uq::civ(&v, owner, UniqueType::KillUnitPlunder, &ctx).for_each(&mut push);
            }
        }
        for c in g.state().cities().iter() {
            if g.grid().distance(c.tile(), at) > 4 {
                continue;
            }
            let mut near =
                uq::city(&v, c.id(), UniqueType::KillUnitPlunderNearCity, &ctx).peekable();
            if near.peek().is_some() {
                near.for_each(&mut push);
                break;
            }
        }
        out
    };
    let f = r.uniques().filters();
    for (percent, units, of, into) in plunders {
        if !f.unit_facts_match(units, &g.view(), &facts, None) {
            continue;
        }
        let base = match of {
            CostOrStrength::Cost => cost,
            CostOrStrength::Strength => strength,
        };
        let amount = num::trunc_i32(f64::from(base) * f64::from(percent) / 100.0);
        if amount != 0 {
            g.add_stat(owner, into, f64::from(amount));
        }
    }
    // city_states.barbarian_killed_near and on_military_unit_killed (combat.py:639-643).
    pending(Porting::Pending("1c-06"));
}

/// Healing for a unit that has just killed (`combat._heal_after_kill`, `combat.py:646-652`).
fn heal_after_kill(g: &mut Game, c: Combatant) {
    let Combatant::Unit(u) = c else { return };
    if g.unit(u).is_none() {
        return;
    }
    let heals: SmallVec<[i32; 2]> = {
        let v = g.view();
        let ctx = Ctx::unit(&v, u);
        uq::unit_and_civ(&v, u, UniqueType::HealsAfterKilling, &ctx)
            .flat_map(|h| match *h.data() {
                UniqueData::HealsAfterKilling(x) => core::iter::repeat_n(x.hp, usize::from(h.n)),
                _ => core::iter::repeat_n(0, 0),
            })
            .collect()
    };
    for hp in heals {
        health::heal_by(g, u, hp);
    }
}

/// Experience for a side that lived through the fight, less against the barbarians
/// (`combat._add_xp`, `combat.py:655-663`).
pub(crate) fn add_xp(g: &mut Game, c: Combatant, amount: i32, other_owner: PlayerId) {
    let Combatant::Unit(u) = c else { return };
    if g.unit(u).is_none() {
        return;
    }
    let vs_barbarian = g.is_barbarian(other_owner);
    promotions::add_xp(g, u, amount, vs_barbarian);
}

/// A defender that withdraws before a melee attack instead of fighting (`combat._withdraw`,
/// `combat.py:666-699`): to a tile it could stand on, land for a land unit, with no city but its
/// own; one not next to the attacker if there is one, else one that is; drawn from the fight's
/// stream.
fn withdraw(g: &mut Game, rng: &mut Rng, a: Combatant, d: Combatant) -> bool {
    let (Combatant::Unit(au), Combatant::Unit(du)) = (a, d) else { return false };
    if !combatant::is_melee(g, a) {
        return false;
    }
    let owner = combatant::owner(g, d);
    let frm = combatant::tile(g, d);
    let atk = combatant::tile(g, a);
    let may = {
        let v = g.view();
        let ctx = Ctx {
            civ: Some(owner),
            unit: Some(du),
            tile: Some(frm),
            combat: Some(CombatCtx { our: d, their: Some(a), attacked_tile: None, action: None }),
            ..Ctx::default()
        };
        uq::any(uq::unit(&v, du, UniqueType::WithdrawsBeforeMeleeCombat, &ctx))
    };
    if !may || movement::is_embarked(g, du) {
        return false;
    }
    let land = combatant::is_land(g, d);
    let Some(base) = g.unit(du).map(|x| x.base) else { return false };
    let ok = |t: TileIdx| {
        movement::can_stand(g, owner, base, t, Some(du))
            && !(land && !g.is_land(t))
            && g.city_at(t).is_none_or(|c| c.owner() == owner)
    };
    let next_to_attacker = |t: TileIdx| g.grid().neighbors(atk).any(|n| n == t);
    let first: Vec<TileIdx> =
        g.grid().neighbors(frm).filter(|&t| t != atk && !next_to_attacker(t) && ok(t)).collect();
    let second: Vec<TileIdx> =
        g.grid().neighbors(frm).filter(|&t| next_to_attacker(t) && ok(t)).collect();
    let to = if first.is_empty() { rng.pick(&second) } else { rng.pick(&first) };
    let Some(&to) = to else { return false };
    if g.relocate_unit(du, to).is_err() {
        return false;
    }
    let text = format!("{} withdrew from a {}.", combatant::name(g, d), combatant::name(g, a));
    let audience = [owner, combatant::owner(g, Combatant::Unit(au))].into_iter().collect();
    g.emit(EngineEvent::Combat, &text, Some(audience), Some(to), EventData::default(), &[]);
    true
}

/// Spends the attacker's attack and movement (`combat._reduce_attacker_moves`,
/// `combat.py:702-722`): a city has attacked this turn; a unit has one attack fewer, and either
/// a move's worth of movement fewer (when it may move after attacking or attack again, unless it
/// is an aircraft or a melee unit that won) or none left; and it stops fortifying, sleeping or
/// following an order.
fn reduce_attacker_moves(g: &mut Game, a: Combatant, dead: &Victim) {
    let u = match a {
        Combatant::City(c) => {
            if let Some(x) = g.city_mut(c, CityTouch::CORE) {
                x.attacked = true;
            }
            return;
        }
        Combatant::Unit(u) => u,
    };
    let Some(x) = g.unit(u) else { return };
    if dead.civilian && x.tile() == dead.tile {
        return;
    }
    let attacks = x.attacks.saturating_add(1);
    let keeps = unit_has(g, u, UniqueType::CanMoveAfterAttacking, false)
        || health::max_attacks(g, u) > i32::from(attacks);
    let spend = keeps
        && !combatant::is_air(g, a)
        && !(combatant::is_melee(g, a) && combatant::defeated(g, dead.side));
    let sc = g.rules().constants().move_scale;
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
        x.attacks = attacks;
        x.acted = true;
        if !keeps {
            x.moves = 0;
        } else if spend {
            x.moves = (x.moves - sc).max(0);
        }
        if matches!(
            x.activity,
            Some(
                Activity::Fortify
                    | Activity::FortifyHeal
                    | Activity::Sleep
                    | Activity::SleepHeal
                    | Activity::Goto
                    | Activity::Explore
            )
        ) {
            x.activity = None;
        }
    }
}

/// Captures a defeated military unit instead of destroying it, where a unique lets the winner
/// (`combat._try_capture_military`, `combat.py:725-755`): `May capture killed [units] units`
/// with a chance from the strengths, drawn from the fight's stream, or a melee unit's `Earn [n]
/// Gold and capture [units] units` (whose gold comes whether or not the unit finds room). The
/// captured unit appears beside where it fell with half its health and no movement.
fn try_capture_military(g: &mut Game, rng: &mut Rng, a: Combatant, d: Combatant) -> bool {
    let (Combatant::Unit(au), Combatant::Unit(du)) = (a, d) else { return false };
    if !combatant::defeated(g, d) || combatant::is_civilian(g, d) {
        return false;
    }
    let Some((d_owner, d_tile, d_base)) = g.unit(du).map(|x| (x.owner(), x.tile(), x.base)) else {
        return false;
    };
    let a_owner = combatant::owner(g, a);
    let (uncapturable, prize, gains) = {
        let v = g.view();
        let ctx = Ctx {
            civ: Some(d_owner),
            unit: Some(du),
            combat: Some(CombatCtx {
                our: d,
                their: Some(a),
                attacked_tile: Some(d_tile),
                action: None,
            }),
            ..Ctx::default()
        }
        .resolve(&v);
        let uncapturable = uq::any(uq::unit(&v, du, UniqueType::Uncapturable, &ctx));
        let f = g.rules().uniques().filters();
        let actx = Ctx::unit(&v, au);
        let prize = uq::unit(&v, au, UniqueType::KillUnitCapture, &actx).any(|h| match h.data() {
            UniqueData::KillUnitCapture(x) => {
                f.unit_matches(x.units, &v, du, crate::unique::UnitScope::default())
            }
            _ => false,
        });
        let gctx = Ctx {
            civ: Some(a_owner),
            combat: Some(CombatCtx { our: a, their: Some(d), attacked_tile: None, action: None }),
            ..Ctx::default()
        }
        .resolve(&v);
        let mut gains: SmallVec<[i32; 2]> = SmallVec::new();
        if combatant::is_melee(g, a) {
            for h in uq::unit_and_civ(&v, au, UniqueType::GainFromDefeatingUnit, &gctx) {
                if let UniqueData::GainFromDefeatingUnit(x) = h.data()
                    && units::unit_matches(g, du, x.units)
                {
                    gains.extend(core::iter::repeat_n(x.gold, usize::from(h.n)));
                }
            }
        }
        (uncapturable, prize, gains)
    };
    if uncapturable {
        return false;
    }
    let mut captured = false;
    if prize {
        let ratio =
            strength::base_attack(g, a, Some(d)) / strength::base_defense(g, d, Some(a)).max(1.0);
        let chance = (0.1 + ratio * 0.4).min(0.8);
        if rng.unit() <= chance {
            captured = true;
        }
    }
    for gold in &gains {
        if let Some(p) = g.player_mut(a_owner, PlayerTouch::STOCKS) {
            p.econ.gold += f64::from(*gold);
        }
        captured = true;
    }
    if !captured {
        return false;
    }
    let Some(nu) = units::place_unit_near(g, a_owner, d_base, d_tile) else { return false };
    if let Some(x) = g.unit_mut(nu, UnitTouch::CORE | UnitTouch::MOVES) {
        x.moves = 0;
        x.hp = 50;
    }
    let what = g.rules().name(d_base).unwrap_or("");
    let text = format!("{} captured an enemy {what}!", player_name(g, a_owner));
    let audience = [a_owner, d_owner].into_iter().collect();
    g.emit(
        EngineEvent::UnitCaptured,
        &text,
        Some(audience),
        Some(d_tile),
        EventData::default(),
        &[],
    );
    true
}

/// A ground or sea unit attacks tile `t` whose defender [`validate_attack`] found
/// (`combat.attack`, `combat.py:758-766`): a unit that must set up does so first, for a move's
/// worth of its movement.
pub fn attack(g: &mut Game, u: UnitId, d: Combatant) -> Value {
    if unit_has(g, u, UniqueType::MustSetUp, false) && g.unit(u).is_some_and(|x| !x.set_up) {
        let sc = g.rules().constants().move_scale;
        if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
            x.set_up = true;
            x.moves = (x.moves - sc).max(0);
        }
    }
    resolve(g, Combatant::Unit(u), d)
}

/// One fight between two sides, and everything that follows it (`combat.resolve`,
/// `combat.py:769-871`): an aircraft meets interceptors first; a defender may withdraw; then the
/// blows, a capture, the dead, the notice, a city taken, what a kill earns and heals, a unit that
/// destroys itself, a melee winner moving into the emptied tile, the attack spent and the
/// experience earned. The result says what happened.
pub fn resolve(g: &mut Game, a: Combatant, d: Combatant) -> Value {
    let from = combatant::tile(g, a);
    let at = combatant::tile(g, d);
    let (a_owner, d_owner) = (combatant::owner(g, a), combatant::owner(g, d));
    // What the attacker is, which its experience and its victim's read once it may be gone.
    let (a_air, a_ranged) = (combatant::is_air(g, a), combatant::is_ranged(g, a));
    let mut res = Map::new();
    res.insert("attacker".into(), json!(combatant::name(g, a)));
    res.insert("defender".into(), json!(combatant::name(g, d)));
    res.insert("ranged".into(), json!(a_ranged));
    if let Combatant::Unit(au) = a
        && a_air
    {
        let dmg = super::air::try_intercept(g, au, at, d_owner, Some(d));
        if dmg != 0 {
            res.insert("intercepted".into(), json!(dmg));
        }
        if g.unit(au).is_some_and(|x| x.hp <= 0) {
            let text =
                format!("{}'s {} was shot down.", player_name(g, a_owner), combatant::name(g, a));
            kill_unit(g, au, Some(d_owner), &text);
            res.insert("attacker_killed".into(), json!(true));
            return Value::Object(res);
        }
    }
    let mut rng = stream(g, Purpose::Combat, combatant::key(a), combatant::key(d));
    let before = Victim::of(g, d);
    if withdraw(g, &mut rng, a, d) {
        reduce_attacker_moves(g, a, &before);
        res.insert("withdrew".into(), json!(true));
        g.settle_sight();
        return Value::Object(res);
    }
    let already_down = matches!(d, Combatant::City(_)) && combatant::defeated(g, d);
    let (dd, da) = blows(g, &mut rng, a, d, from, false);
    res.insert("damage_to_defender".into(), json!(dd));
    res.insert("damage_to_attacker".into(), json!(da));
    match d {
        Combatant::City(c) => {
            res.insert("city_hp".into(), json!(g.city(c).map_or(0, |x| x.health)));
        }
        Combatant::Unit(du) => {
            if let Some(x) = g.unit(du) {
                res.insert("defender_hp".into(), json!(x.hp));
            }
        }
    }
    let captured_military = try_capture_military(g, &mut rng, a, d);
    let (a_name, d_name) = (player_name(g, a_owner), player_name(g, d_owner));
    // What the dead are, as they fell: their facts after the blows, for what a kill earns and the
    // triggers that read them.
    let defender = if matches!(d, Combatant::Unit(du) if g.unit(du).is_some()) {
        Victim::of(g, d)
    } else {
        before
    };
    let attacker = Victim::of(g, a);
    let victim_type = before.facts.and_then(|f| g.rules().name(f.base)).unwrap_or("");
    // refcheck: civilians-under-fire (a civilian brought to no health dies; a capture is
    // reported only when a melee unit took it)
    let taken = before.civilian && matches!(d, Combatant::Unit(du) if g.unit(du).is_none());
    let defender_dead = matches!(d, Combatant::Unit(du) if g.unit(du).is_some_and(|x| x.hp <= 0));
    let attacker_dead = matches!(a, Combatant::Unit(au) if g.unit(au).is_some_and(|x| x.hp <= 0));
    if taken {
        res.insert("captured".into(), json!(victim_type));
    }
    if let Combatant::Unit(du) = d
        && defender_dead
    {
        res.insert("defender_killed".into(), json!(true));
        let text = format!(
            "{a_name}'s {} destroyed {d_name}'s {victim_type} at {}.",
            combatant::name(g, a),
            g.fmt_xy(at)
        );
        kill_unit(g, du, Some(a_owner), &text);
    }
    if let Combatant::Unit(au) = a
        && attacker_dead
    {
        res.insert("attacker_killed".into(), json!(true));
        let text = format!(
            "{a_name}'s {} died attacking {d_name}'s {}.",
            combatant::name(g, a),
            combatant::name(g, d)
        );
        kill_unit(g, au, Some(d_owner), &text);
    }
    if !defender_dead && !attacker_dead && !before.civilian {
        let took = if da != 0 { format!(", took -{da})") } else { ")".to_owned() };
        let text = format!(
            "{a_name}'s {} attacked {d_name}'s {} at {} (-{dd} HP{took}",
            combatant::name(g, a),
            combatant::name(g, d),
            g.fmt_xy(at)
        );
        let audience = [a_owner, d_owner].into_iter().collect();
        g.emit(EngineEvent::Combat, &text, Some(audience), Some(at), EventData::default(), &[]);
    }
    let city_taken = super::city::handle_city_defeated(g, a, d);
    let captured_city = city_taken.as_ref().is_some_and(|m| m.contains_key("captured_city"));
    if let Some(m) = city_taken {
        res.extend(m);
    }
    if defender_dead {
        earn_from_killing(g, a, &defender);
        heal_after_kill(g, a);
        if let (Combatant::Unit(au), Some(facts)) = (a, defender.facts)
            && g.unit(au).is_some()
        {
            let site = TriggerSite { civ: a_owner, city: None, unit: Some(au), tile: None };
            triggers::fire(g, &site, &TriggerEvent::DefeatingUnit(facts), true, None);
        }
    } else if attacker_dead && matches!(d, Combatant::Unit(_)) {
        earn_from_killing(g, d, &attacker);
        heal_after_kill(g, d);
    }
    if let Combatant::Unit(au) = a
        && g.unit(au).is_some()
    {
        if unit_has(g, au, UniqueType::SelfDestructs, false) {
            units::remove_unit(g, au);
        } else if g.unit(au).is_some_and(|x| x.activity == Some(Activity::Goto))
            && let Some(x) = g.unit_mut(au, UnitTouch::CORE)
        {
            x.activity = None;
            x.goto = None;
        }
    }
    if let Combatant::Unit(au) = a
        && !captured_military
        && !captured_city
        && defender_dead
        && combatant::is_melee(g, a)
        && let Some((base, moves)) = g.unit(au).map(|x| (x.base, x.moves))
        && g.city_at(at).is_none()
        && moves > 0
        && movement::can_stand(g, a_owner, base, at, Some(au))
    {
        if let Some(c) = g.civilian_at(at).filter(|c| c.owner() != a_owner).map(|c| c.id()) {
            units::capture::capture_civilian(g, au, c);
        }
        if g.relocate_unit(au, at).is_ok() {
            movement::on_enter_tile(g, au, at);
            res.insert("advanced".into(), json!(true));
        }
    }
    reduce_attacker_moves(g, a, &before);
    if !already_down && !before.civilian {
        let (mine, theirs) = if a_air {
            (4, 2)
        } else if a_ranged {
            (if matches!(d, Combatant::City(_)) { 3 } else { 2 }, 2)
        } else {
            (5, 4)
        };
        add_xp(g, a, mine, d_owner);
        add_xp(g, d, theirs, a_owner);
    }
    let camp = g.rules().derived().known.barbarian_camp;
    if g.is_barbarian(d_owner)
        && camp.is_some()
        && g.tile(at).and_then(crate::state::map::Tile::improvement) == camp
    {
        // barbarians.camp_attacked (combat.py:867-869).
        pending(Porting::Pending("1c-06"));
    }
    g.settle_sight();
    Value::Object(res)
}

//! Capturing a civilian (`units.capture_civilian`, `units.py:608-640`, `Battle.captureCivilianUnit`):
//! a military unit that moves onto an enemy's civilian at war takes it. A settler becomes a
//! worker; a great person, a religious unit or an uncapturable one is destroyed instead. The
//! captor gets a new unit, with no movement left. A civilian taken back from the barbarians may
//! be returned to the civilization they took it from, which its captor then decides
//! ([`return_civilian`], `units.py:643-673`).

use serde_json::{Value, json};

use super::{equivalent_unit, remove_unit, type_has};
use crate::base::ids::{PlayerId, UnitId};
use crate::game::Game;
use crate::game::derive::rev::UnitTouch;
use crate::game::error::ActionError;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::UniqueType;

/// The captor's unit takes the civilian `victim`; the new unit, if it was not destroyed.
pub fn capture_civilian(g: &mut Game, captor: UnitId, victim: UnitId) -> Option<UnitId> {
    let captor_owner = g.unit(captor)?.owner();
    let v = g.unit(victim)?.clone();
    let r = g.rules();
    let vd = &r.base_units()[v.base];
    let old = v.owner();
    let at = v.tile();
    let name = |g: &Game, p| g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let (captor_name, old_name) = (name(g, captor_owner), name(g, old));
    let what = vd.name.to_string();
    let audience = [old, captor_owner].into_iter().collect();
    if type_has(g, v.base, UniqueType::Uncapturable)
        || vd.great_person
        || type_has(g, v.base, UniqueType::ReligiousUnit)
    {
        remove_unit(g, victim);
        let text = format!("{captor_name} destroyed {old_name}'s {what}.");
        g.emit(EngineEvent::UnitKilled, &text, Some(audience), Some(at), EventData::default(), &[]);
        return None;
    }
    let settler = type_has(g, v.base, UniqueType::FoundCity);
    let new_type = if settler { r.derived().known.worker.unwrap_or(v.base) } else { v.base };
    remove_unit(g, victim);
    let base = if g.is_barbarian(captor_owner) || new_type == v.base {
        new_type
    } else {
        equivalent_unit(g, captor_owner, new_type)
    };
    let nu = g.create_unit(captor_owner, base, at, 0).ok()?;
    if let Some(x) = g.unit_mut(nu, UnitTouch::CORE | UnitTouch::MOVES) {
        x.moves = 0;
        x.original_owner = v.original_owner;
    }
    let text = format!("{captor_name} captured {old_name}'s {what}!");
    g.emit(EngineEvent::UnitCaptured, &text, Some(audience), Some(at), EventData::default(), &[]);
    let orig = v.original_owner;
    let offer = orig.filter(|&o| {
        g.is_barbarian(old)
            && !g.is_barbarian(captor_owner)
            && o != captor_owner
            && g.player(o).is_some_and(|p| p.alive() && !p.is_barbarian())
    });
    if let Some(o) = offer {
        if let Some(x) = g.unit_mut(nu, UnitTouch::CORE) {
            x.return_offer = Some(o);
        }
        let kind = g.rules().name(base).unwrap_or("");
        let text = format!(
            "You recaptured a {kind} that barbarians took from {}. Return it to them, or keep it?",
            name(g, o)
        );
        let data = EventData { unit: Some(nu), player: Some(o), ..EventData::default() };
        let to = core::iter::once(captor_owner).collect();
        g.emit(EngineEvent::CivilianRecaptured, &text, Some(to), Some(at), data, &[]);
    }
    Some(nu)
}

/// Whether `pid` may answer the offer to give back unit `u` (`units.return_civilian`'s check,
/// `units.py:649-651`): a civilian it took back from the barbarians that belonged to another.
///
/// # Errors
/// The refusal, with Python's text.
pub fn plan_return_civilian(g: &Game, u: UnitId) -> Result<PlayerId, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::rule("No such unit."))?;
    x.return_offer.ok_or_else(|| {
        let what = g.rules().name(x.base).unwrap_or("");
        ActionError::rule(format!(
            "{what} #{} is not a recaptured civilian that can be returned.",
            u.get()
        ))
    })
}

/// Gives a recaptured civilian back to the civilization it was taken from, or keeps it
/// (`units.return_civilian`, `units.py:643-673`, `Battle.captureCivilianUnit`'s question): kept,
/// or kept because its first owner is gone; otherwise it goes to the first owner's nearest city,
/// with no movement left, and earns its goodwill: 45 influence with a city-state, a better
/// opinion with a major.
pub fn return_civilian(
    g: &mut Game,
    pid: PlayerId,
    u: UnitId,
    orig: PlayerId,
    keep: bool,
) -> Value {
    let Some((base, at)) = g.unit(u).map(|x| (x.base, x.tile())) else { return Value::Null };
    let what = g.rules().name(base).unwrap_or("").to_owned();
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.return_offer = None;
    }
    if keep {
        return json!({ "kept": what, "unit_id": u.get() });
    }
    let (alive, city_state, name) = g.player(orig).map_or((false, false, String::new()), |p| {
        (p.alive(), p.is_city_state(), p.name.to_string())
    });
    if !alive {
        return json!({
            "kept": what,
            "unit_id": u.get(),
            "note": format!("{name} no longer exists; the {what} stays with you."),
        });
    }
    let near = g
        .player_cities(orig)
        .map(|c| (g.grid().distance(c.tile(), at), c.id(), c.tile()))
        .min()
        .map_or(at, |(_, _, t)| t);
    remove_unit(g, u);
    let placed = super::place_unit_near(g, orig, base, near)
        .or_else(|| super::place_unit_near(g, orig, base, at));
    if let Some(nu) = placed
        && let Some(x) = g.unit_mut(nu, UnitTouch::MOVES)
    {
        x.moves = 0;
    }
    if city_state {
        let added = crate::game::city_states::influence::add_influence(g, orig, pid, 45.0);
        debug_assert!(added.is_ok(), "a city-state of the game takes influence: {added:?}");
    } else {
        crate::game::diplomacy::relations::add_opinion(
            g,
            orig,
            pid,
            crate::state::diplo::OpinionKey::ReturnedCivilian,
            20.0,
        );
    }
    let who = g.player(pid).map(|p| p.name.to_string()).unwrap_or_default();
    let text = format!("{who} returned a captured {what} to {name}.");
    let audience = [pid, orig].into_iter().collect();
    let data = EventData { player: Some(orig), ..EventData::default() };
    g.emit(EngineEvent::CivilianReturned, &text, Some(audience), Some(at), data, &[]);
    json!({ "returned": what, "to": name })
}

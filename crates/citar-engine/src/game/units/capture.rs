//! Capturing a civilian (`units.capture_civilian`, `units.py:608-640`, `Battle.captureCivilianUnit`):
//! a military unit that moves onto an enemy's civilian at war takes it. A settler becomes a
//! worker; a great person, a religious unit or an uncapturable one is destroyed instead. The
//! captor gets a new unit, with no movement left. A civilian taken back from the barbarians may
//! be returned to the civilization they took it from.

use super::{equivalent_unit, remove_unit, type_has};
use crate::base::ids::UnitId;
use crate::game::Game;
use crate::game::derive::rev::UnitTouch;
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

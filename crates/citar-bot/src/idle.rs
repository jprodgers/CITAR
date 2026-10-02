//! The idle bot (`citar/bots/idle.py`): founds its capital, then does nothing, and rejects every
//! negotiation. A control for what the real bot is worth, and a punching bag in conquest tests.

use citar_engine::base::ids::{NegotiationId, UnitId};
use citar_engine::game::Action;
use citar_engine::game::actions::FoundCity;
use citar_engine::game::diplomacy::actions::RespondNegotiation;
use serde_json::json;

use crate::driver::Turn;

/// Its turn: while it has no city, each unit in id order tries to found one (`IdleBot.play_turn`).
pub(crate) fn play_turn(t: &mut Turn<'_>) {
    let pid = t.pid();
    let units: Vec<UnitId> = t.game().player_units(pid).map(|u| u.id()).collect();
    for u in units {
        if t.game().player_cities(pid).next().is_some() {
            break;
        }
        t.act(Action::FoundCity(FoundCity { unit_id: i64::from(u.get()), name: None }));
    }
}

/// Its answer to negotiation `nid`: no (`IdleBot.respond`).
pub(crate) fn respond(t: &mut Turn<'_>, nid: NegotiationId) {
    t.act(Action::RespondNegotiation(RespondNegotiation {
        negotiation_id: i64::from(nid.get()),
        action: json!("reject"),
        message: Some(json!("We are not interested.")),
        give: None,
        receive: None,
    }));
}

//! `basic-1`: the port of `citar/bots/basic.py`, the scripted opponent of 0.1.5 (DESIGN.md
//! P2.3).
//!
//! [`play_turn`] is `BasicBot.play_turn` (basic.py:740-757): the phases of a turn in Python's
//! order. Each is a no-op until the package that ports it (P2.3.10): 2-01b the context,
//! research, empire, cities, gold and the economy's units; 2-03 units and fighting; 2-05
//! diplomacy. What Python's turn did around them is the drive's now:
//! - `handle_negotiations` (745): `Game::drive` puts every negotiation that waits on a driven
//!   seat to its driver's [`respond`] before the seat plays, and again as answers come back;
//! - `_settle_chats` and `end_turn` (754-757): the drive ends the turn, and a chat the bot
//!   opened that still waits on a seat nobody drives stops it for the host (P2.3.8).
//!
//! Until package 2-01b ports the settlers, the units phase founds the capital as the idle bot
//! does, so that games with `basic-1` seats (the runner's, the bindings', the benchmarks') have
//! cities to play with.

use citar_engine::base::ids::NegotiationId;
use citar_engine::game::Action;
use citar_engine::game::diplomacy::actions::RespondNegotiation;
use serde_json::json;

use crate::BotSpec;
use crate::driver::Turn;
use crate::memory::Memory;
use crate::params::{Params, Resolved};

/// What one turn of `basic-1` plays with besides the game: its spec, its parameters and their
/// names resolved for the game's ruleset, and the seat's memory, which the driver decodes before
/// the turn and keeps after it.
#[expect(dead_code, reason = "the phases of 2-01b, 2-03 and 2-05 read it; until then none does")]
pub(crate) struct Seat<'a> {
    pub spec: &'a BotSpec,
    pub params: &'a Params,
    pub resolved: &'a Resolved,
    pub memory: &'a mut Memory,
}

/// The facts a turn's decisions share (`BasicBot.context`, basic.py:777-835), built twice a
/// turn as Python built them: at the start, and after the units have moved. Package 2-01b gives
/// it its fields (`basic1/context.rs`).
pub(crate) struct Context {}

/// One turn of the seat (`BasicBot.play_turn`, basic.py:740-757).
pub(crate) fn play_turn(t: &mut Turn<'_>, s: &mut Seat<'_>) {
    let ctx = context(t, s);
    choose_research(t, s, &ctx);
    empire_choices(t, s, &ctx);
    city_bombard(t, s);
    manage_units(t, s, &ctx);
    city_bombard(t, s);
    let ctx = context(t, s);
    manage_cities(t, s, &ctx);
    manage_gold(t, s, &ctx);
    consider_diplomacy(t, s, &ctx);
}

/// `context` (basic.py:777-835): package 2-01b.
fn context(t: &Turn<'_>, s: &Seat<'_>) -> Context {
    let _ = (t, s);
    Context {}
}

/// `choose_research` (basic.py:876-998): package 2-01b.
fn choose_research(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let _ = (t, s, ctx);
}

/// `empire_choices`: policies, free great people, the pantheon (basic.py:1003-1041), package
/// 2-01b; spies (1043-1064), package 2-05.
fn empire_choices(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let _ = (t, s, ctx);
}

/// `city_bombard` (basic.py:1577-1587): package 2-01b.
fn city_bombard(t: &mut Turn<'_>, s: &mut Seat<'_>) {
    let _ = (t, s);
}

/// `manage_units` (basic.py:1771-2094): its order and dispatch, settlers, workers, work boats
/// and scouts in package 2-01b; promotions, garrisons, escorts, special, air and naval units and
/// fighting in 2-03.
///
/// Until 2-01b, it founds the capital as the idle bot does: while the seat has no city, each
/// unit in id order tries to found one.
fn manage_units(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let _ = (s, ctx);
    crate::idle::found_capital(t);
}

/// `manage_cities` (basic.py:1152-1202): focus, growth, production through the advisor;
/// package 2-01b.
fn manage_cities(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let _ = (t, s, ctx);
}

/// `manage_gold` (basic.py:1591-1643, 1739-1766), with faith (1696-1737): package 2-01b;
/// city-state gifts (1645-1694), package 2-05.
fn manage_gold(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let _ = (t, s, ctx);
}

/// `consider_diplomacy` (basic.py:2394-2556): package 2-05.
fn consider_diplomacy(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let _ = (t, s, ctx);
}

/// Its answer to negotiation `nid`, which waits on the seat and which the bot owns (the driver
/// has left the model's to the host): `BasicBot.respond` (basic.py:2651-2708), which package
/// 2-05 ports. Until then it says no with Python's own lines: to talk with nothing on the
/// table, "We have nothing further to discuss."; to a proposal, "That does not interest us."
pub(crate) fn respond(t: &mut Turn<'_>, s: &mut Seat<'_>, nid: NegotiationId) {
    let _ = s;
    let talk = t.game().negotiation(nid).is_some_and(|n| n.proposal.is_none());
    let line =
        if talk { "We have nothing further to discuss." } else { "That does not interest us." };
    t.act(Action::RespondNegotiation(RespondNegotiation {
        negotiation_id: i64::from(nid.get()),
        action: json!("reject"),
        message: Some(json!(line)),
        give: None,
        receive: None,
    }));
}

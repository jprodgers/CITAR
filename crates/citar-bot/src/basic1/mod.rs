//! `basic-1`: the port of `citar/bots/basic.py`, the scripted opponent of 0.1.5 (DESIGN.md
//! P2.3).
//!
//! [`play_turn`] is `BasicBot.play_turn` (basic.py:740-757): the phases of a turn in Python's
//! order. Package 2-01b ports the economy:
//! - [`context`]: the facts a turn's decisions share (777-871);
//! - [`research`]: research and the tech values, in both modes (876-998);
//! - [`empire`]: policies, a free great person, the pantheon (1003-1041);
//! - [`cities`]: production through the engine's advisor, focus, growth, city bombardment
//!   (1152-1202, 1577-1587);
//! - [`gold`]: purchases, deficits, upgrades and the spare units (1591-1643, 1739-1766);
//! - [`faith`]: beliefs and what faith buys (1696-1737);
//! - [`units`]: `manage_units`' order and dispatch (1771-1805), with [`settlers`] (1835-1857,
//!   1879-1888), [`workers`] and work boats (1927-1949) and scouts (2054-2063).
//!
//! Package 2-03 ports units and fighting (the dispatch's other branches, promotions, escorts);
//! 2-05 diplomacy, spies, city-state gifts, [`respond`] and the advice. What Python's turn did
//! around the phases is the drive's now:
//! - `handle_negotiations` (745): `Game::drive` puts every negotiation that waits on a driven
//!   seat to its driver's [`respond`] before the seat plays, and again as answers come back;
//! - `_settle_chats` and `end_turn` (754-757): the drive ends the turn, and a chat the bot
//!   opened that still waits on a seat nobody drives stops it for the host (P2.3.8).

pub(crate) mod cities;
pub(crate) mod context;
pub(crate) mod empire;
pub(crate) mod faith;
pub(crate) mod gold;
pub(crate) mod research;
pub(crate) mod settlers;
pub(crate) mod units;
pub(crate) mod workers;

use citar_engine::base::ids::{NegotiationId, PlayerId, TileIdx, UnitId};
use citar_engine::game::Action;
use citar_engine::game::Game;
use citar_engine::game::advisor::{Advisor, AdvisorParams, BotFacts};
use citar_engine::game::diplomacy::actions::RespondNegotiation;
use serde_json::json;

use self::context::Context;
use crate::BotSpec;
use crate::driver::Turn;
use crate::memory::Memory;
use crate::params::{Params, Resolved};

/// What one turn of `basic-1` plays with besides the game: its spec, its parameters, their names
/// resolved for the game's ruleset and the advisor's share of them with the seat's aggression,
/// and the seat's memory, which the driver decodes before the turn and keeps after it.
pub(crate) struct Seat<'a> {
    pub spec: &'a BotSpec,
    pub params: &'a Params,
    pub resolved: &'a Resolved,
    pub memory: &'a mut Memory,
    /// The production advisor's parameters (`Tuning::advisor` with the seat's aggression).
    pub advisor: AdvisorParams,
}

impl<'a> Seat<'a> {
    /// The seat of `spec` with `memory`, its names resolved by `resolved`.
    pub(crate) fn new(spec: &'a BotSpec, resolved: &'a Resolved, memory: &'a mut Memory) -> Self {
        let params = spec.tuning.params();
        let advisor = spec.tuning.advisor(spec.aggression);
        Self { spec, params, resolved, memory, advisor }
    }
}

/// The production advisor for the seat's turn as the game is now, told what the bot remembers
/// (DESIGN.md P2.3.7): a war being prepared, its garrisons, a settler waiting for an escort, the
/// turns work boats were queued, and the sites given up on within `site_blacklist_turns`.
pub(crate) fn advisor(g: &Game, pid: PlayerId, s: &Seat<'_>) -> Advisor {
    let m = &*s.memory;
    let turn = i64::from(g.turn());
    let blocked: Vec<TileIdx> = m
        .bad_sites
        .iter()
        .filter(|&(_, &at)| turn - i64::from(at) < i64::from(s.params.site_blacklist_turns))
        .map(|(&x, _)| x)
        .collect();
    let garrisons: Vec<UnitId> = m.garrisons.values().copied().collect();
    let facts = BotFacts {
        preparing_war: m.war_prep.is_some(),
        garrisons: &garrisons,
        need_escort: m.need_escort,
        boat_turns: &m.boat_turns,
        blocked_sites: &blocked,
    };
    Advisor::with_facts(g, pid, &s.advisor, &facts)
}

/// One turn of the seat (`BasicBot.play_turn`, basic.py:740-757).
pub(crate) fn play_turn(t: &mut Turn<'_>, s: &mut Seat<'_>) {
    let ctx = context(t, s);
    research::choose_research(t, s, &ctx);
    empire::empire_choices(t, s, &ctx);
    cities::city_bombard(t);
    units::manage_units(t, s, &ctx);
    cities::city_bombard(t);
    let ctx = context(t, s);
    cities::manage_cities(t, s, &ctx);
    gold::manage_gold(t, s, &ctx);
    consider_diplomacy(t, s, &ctx);
}

/// `context` (basic.py:777-835), as the game is now.
fn context(t: &Turn<'_>, s: &Seat<'_>) -> Context {
    context::build(t.game(), t.pid(), s)
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

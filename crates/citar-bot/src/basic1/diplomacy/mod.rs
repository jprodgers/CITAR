//! Diplomacy (`consider_diplomacy`, basic.py:2394-2476): peace offered in a long or losing war,
//! embassies, friendship and research agreements every `diplo_every` turns, a war prepared and
//! declared ([`war`]), and luxury trades ([`trade`]). With them, what the bot gives city-states
//! ([`city_states`]), where its spies go ([`spies`]), what it makes of a deal ([`evaluate`]) and
//! how it answers one ([`respond`]).
//!
//! Each kind of decision is skipped when the seat's language model owns its category
//! (`Owners`, DESIGN.md P2.3.8): peace offers (`peace`), embassies, friendship and research
//! agreements (`agreements`), preparing and declaring a war (`war`; a war being prepared is
//! forgotten when the model takes war over), luxury trades and the gold counter (`trades`),
//! city-state gifts (`city_states`) and spies (`espionage`). The bot still fights every war it
//! is in, whoever declared it (`units::military`).
//!
//! The draws are the keyed streams of DESIGN.md P2.3.5 (`Peace`, `Friendship`, `WarPrep`, each
//! keyed by the other civilization), so whether one category is the model's moves no draw of
//! another, and the order in which the bot asks moves none.
//!
//! What differs from Python:
//! - The civilizations met are visited in player-id order, where Python kept the order they
//!   were met (the engine keeps every list of players in id order,
//!   `met-lists-in-player-id-order`): the first civilization a luxury is offered to, or a war
//!   prepared on, may differ when several qualify.
//! - `diplo_every` and `lux_trade_every` below 1 read as 1, where Python's `%` by 0 raised.
//! - Units the turn disbanded since the context was built are no longer counted near a siege
//!   or in the field: the context holds ids, read back from the game.

pub(crate) mod city_states;
pub(crate) mod evaluate;
pub(crate) mod respond;
pub(crate) mod spies;
pub(crate) mod trade;
pub(crate) mod war;

use citar_engine::base::ids::PlayerId;
use citar_engine::base::num;
use citar_engine::base::rng::KeyPart;
use citar_engine::game::cities::stats::max_health;
use citar_engine::game::diplomacy::actions::OpenNegotiation;
use citar_engine::game::diplomacy::category::Category;
use citar_engine::game::diplomacy::deals::ra_cost;
use citar_engine::game::diplomacy::relations::{civ_has, has_embassy, is_friends, opinion};
use citar_engine::game::{Action, Game};
use citar_engine::state::diplo::Relation;
use citar_engine::unique::UniqueType;
use serde_json::{Value, json};

use super::Seat;
use super::context::Context;
use super::units::war_plan::military_power;
use crate::driver::Turn;
use crate::stream::Stream;

/// The living major civilizations `pid` has met, in player-id order (`p.met`, filtered as each
/// of Python's loops filtered it: `kind == "major"` and alive).
pub(crate) fn met_majors(g: &Game, pid: PlayerId) -> Vec<PlayerId> {
    // refcheck: met-lists-in-player-id-order
    g.majors(true).map(|p| p.id()).filter(|&q| q != pid && g.has_met(pid, q)).collect()
}

/// A deal item with no fields, as a caller writes it: `{"type": "embassy"}`.
pub(crate) fn plain(kind: &str) -> Value {
    json!({ "type": kind })
}

/// Opens a negotiation with `to` (the `open_negotiation` tool): whether the rules took it.
pub(crate) fn open(
    t: &mut Turn<'_>,
    to: PlayerId,
    message: &str,
    give: Vec<Value>,
    receive: Vec<Value>,
) -> bool {
    t.act(Action::OpenNegotiation(OpenNegotiation {
        to: i64::from(to.0),
        message: json!(message),
        give: Some(Value::Array(give)),
        receive: Some(Value::Array(receive)),
    }))
    .is_some()
}

/// `consider_diplomacy` (basic.py:2394-2476): for each civilization met, peace in a war or
/// agreements and a war to prepare in peace; then the war being prepared, and the luxury trades.
pub(crate) fn consider_diplomacy(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let pid = t.pid();
    let owners = &s.spec.owners;
    let mine = military_power(t.game(), pid);
    if owners.llm(Category::War) {
        // A war being prepared is the model's call now.
        s.memory.war_prep = None;
    }
    for q in met_majors(t.game(), pid) {
        let g = t.game();
        let Some(rel) = g.relation(pid, q).cloned() else { continue };
        let theirs = military_power(g, q);
        if rel.war {
            offer_peace(t, s, ctx, q, &rel, mine, theirs);
            continue;
        }
        agreements(t, s, q, &rel, mine, theirs);
        war::consider_war(t.game(), pid, s, ctx, q, &rel, mine, theirs);
    }
    war::prepare(t, s, ctx, mine);
    let k = s.params.lux_trade_every.max(1);
    if !s.spec.owners.llm(Category::Trades)
        && t.game().turn().rem_euclid(k) == i32::from(pid.0).rem_euclid(k)
    {
        trade::trade_luxuries(t, s);
    }
}

/// Peace in a war with `q` (basic.py:2415-2430): offered once `peace_min_turns` have passed in
/// a war it is losing (weaker than `losing_ratio` of them, or one of its cities theirs) or that
/// has gone on `war_long_turns`, unless a siege of their city is going well; drawn from the
/// `Peace` stream.
fn offer_peace(
    t: &mut Turn<'_>,
    s: &Seat<'_>,
    ctx: &Context,
    q: PlayerId,
    rel: &Relation,
    mine: f64,
    theirs: f64,
) {
    let g = t.game();
    let pid = t.pid();
    let p = s.params;
    let turn = g.turn();
    let lost = g.state().cities().iter().any(|c| c.founder == pid && c.owner() == q);
    let mut long_war = i64::from(turn) - i64::from(rel.since) >= i64::from(p.war_long_turns);
    let losing = mine < theirs * p.losing_ratio || lost;
    let besieged = s.memory.war_plan.as_ref().and_then(|w| g.city_at(w.city));
    if let Some(tc) = besieged
        && tc.owner() == q
        && !lost
        && mine >= theirs * p.losing_ratio
    {
        let grid = g.grid();
        let near = ctx
            .military
            .iter()
            .filter_map(|&u| g.unit(u))
            .filter(|u| {
                i64::from(grid.distance(u.tile(), tc.tile())) <= i64::from(p.siege_progress_radius)
            })
            .count();
        if i64::try_from(near).unwrap_or(i64::MAX) >= i64::from(p.siege_progress_units)
            && f64::from(tc.health) < f64::from(max_health(g, tc.id())) * p.siege_progress_health
        {
            // The siege is progressing: keep going.
            long_war = false;
        }
    }
    if !s.spec.owners.llm(Category::Peace)
        && i64::from(turn) - i64::from(rel.since) >= i64::from(p.peace_min_turns)
        && (losing || long_war)
        && Stream::Peace.rng(g, pid, &[q.key()]).unit() < p.peace_offer_chance
    {
        open(
            t,
            q,
            "This war profits no one. Let us make peace.",
            vec![plain("peace_treaty")],
            Vec::new(),
        );
    }
}

/// Agreements with `q` in peace, every `diplo_every` turns by the pair (basic.py:2431-2444):
/// embassies first where both may have them, then friendship with one it does not dislike and
/// is not much stronger than (drawn from the `Friendship` stream), then a research agreement
/// with a friend it can pay for.
fn agreements(t: &mut Turn<'_>, s: &Seat<'_>, q: PlayerId, rel: &Relation, mine: f64, theirs: f64) {
    let g = t.game();
    let pid = t.pid();
    let p = s.params;
    let every = p.diplo_every.max(1);
    let turn = g.turn();
    if s.spec.owners.llm(Category::Agreements)
        || turn.rem_euclid(every) != (i32::from(pid.0) + i32::from(q.0)).rem_euclid(every)
    {
        return;
    }
    let friends = is_friends(g, pid, q);
    if !has_embassy(g, pid, q)
        && civ_has(g, pid, UniqueType::EnablesEmbassies)
        && civ_has(g, q, UniqueType::EnablesEmbassies)
    {
        open(t, q, "Let us exchange embassies.", vec![plain("embassy")], vec![plain("embassy")]);
    } else if !friends
        && opinion(g, pid, q) >= 0.0
        && mine < theirs * p.friend_max_ratio
        && Stream::Friendship.rng(g, pid, &[q.key()]).unit()
            < p.friend_chance - p.friend_chance_aggr * s.spec.aggression
    {
        open(
            t,
            q,
            "Let us declare our friendship.",
            vec![plain("declaration_of_friendship")],
            Vec::new(),
        );
    } else if friends
        && rel.ra_until < turn
        && gold(g, pid) > f64::from(ra_cost(g, pid, q)) + f64::from(p.ra_gold_margin)
        && civ_has(g, pid, UniqueType::EnablesResearchAgreements)
    {
        open(
            t,
            q,
            "A research agreement would benefit us both.",
            vec![plain("research_agreement")],
            Vec::new(),
        );
    }
}

/// A civilization's treasury.
pub(crate) fn gold(g: &Game, pid: PlayerId) -> f64 {
    g.player(pid).map_or(0.0, |x| x.econ.gold)
}

/// Python's `sum` of the context's threats (Neumaier, as `sum` adds floats).
pub(crate) fn total_threat(ctx: &Context) -> f64 {
    num::py_sum(ctx.threat.iter().copied())
}

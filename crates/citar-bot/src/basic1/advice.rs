//! What the bot makes of a seat's diplomatic situation, as plain data for a language model to
//! weigh (`advice`, basic.py:2713-2771; DESIGN.md P2.3.8): the worth of the proposal on the
//! table of a negotiation, the wars it fights or prepares with its power against theirs and
//! whether its army has gathered, the luxuries it could trade away, and up to five things it
//! would ask for (luxuries others have to spare, peace in a war it is losing).
//!
//! It reads the game and the seat's memory and writes nothing: neither the game, nor the bot's
//! plans. The facade's `bot_advice` asks it, as Phase 3's hybrid advisor will.

use citar_engine::api::views::empire::luxury_resources;
use citar_engine::base::ids::{NegotiationId, PlayerId, ResourceId};
use citar_engine::base::num;
use citar_engine::game::Game;
use citar_engine::game::advisor::is_army;
use citar_engine::rules::defs::Domain;

use super::diplomacy::evaluate::{Mind, evaluate};
use super::diplomacy::war::army_needed;
use super::units::war_plan::military_power;
use super::units::within;
use crate::{Advice, Want, WarReadiness};

/// How many wants the advice lists at most.
const WANTS: usize = 5;

/// The advice for seat `pid` of a bot of `m`, about negotiation `nid` if one is given, visiting
/// the civilizations it has met in `met`'s order.
pub(crate) fn advice(
    g: &Game,
    m: Mind<'_>,
    pid: PlayerId,
    nid: Option<NegotiationId>,
    met: &[PlayerId],
) -> Advice {
    let p = m.params;
    let mine = military_power(g, pid);
    let mut out = Advice::default();
    if let Some(n) = nid.and_then(|id| g.negotiation(id))
        && let Some(terms) = &n.proposal
        && (pid == n.initiator || pid == n.responder)
    {
        let other = if pid == n.initiator { n.responder } else { n.initiator };
        let v = evaluate(g, m, pid, other, terms.gives(pid), terms.gives(other));
        out.deal_value = Some(num::round_ndigits(v, 1));
    }
    let prep = m.memory.war_prep.as_ref();
    let plan = m.memory.war_plan.as_ref();
    let r = g.rules();
    // The field army: land military units, scouts and garrisons aside.
    let field: Vec<_> = g
        .player_units(pid)
        .filter(|u| {
            is_army(g, u.base)
                && r.base_units()[u.base].domain == Domain::Land
                && !m.memory.garrisons.values().any(|&x| x == u.id())
        })
        .map(|u| u.tile())
        .collect();
    let mut targets: Vec<PlayerId> = met.iter().copied().filter(|&q| g.at_war(pid, q)).collect();
    if let Some(w) = prep
        && !targets.contains(&w.player)
        && g.player(w.player).is_some_and(|x| x.alive())
    {
        targets.push(w.player);
    }
    for q in targets {
        let theirs = military_power(g, q);
        let planned = plan.filter(|w| g.city_at(w.city).is_some_and(|c| c.owner() == q));
        let gathered = if let Some(w) = planned {
            w.advance
        } else if let Some(rally) = prep.filter(|w| w.player == q).and_then(|w| w.rally) {
            let need = army_needed(m.params, g.player_cities(pid).count());
            let ready = field
                .iter()
                .filter(|&&at| within(g.grid().distance(at, rally), p.prep_gather_radius))
                .count();
            i64::try_from(ready).unwrap_or(i64::MAX) >= need
        } else {
            false
        };
        out.war_readiness.push(WarReadiness {
            player: q,
            name: g.player(q).map(|x| x.name.to_string()).unwrap_or_default(),
            at_war: g.at_war(pid, q),
            preparing: prep.is_some_and(|w| w.player == q),
            power_ratio: num::round_ndigits(mine / theirs, 2),
            army_gathered: gathered,
        });
    }
    let lux = luxury_resources(g, pid);
    let net = |r: ResourceId| lux.iter().find(|(x, _)| *x == r).map_or(0, |(_, l)| l.net);
    let name = |r: ResourceId| r_name(g, r);
    let mut spare: Vec<String> =
        lux.iter().filter(|(_, d)| d.net >= p.lux_spare_at).map(|&(r, _)| name(r)).collect();
    spare.sort();
    out.spare_luxuries = spare;
    for &q in met {
        if g.at_war(pid, q) {
            continue;
        }
        let mut theirs: Vec<String> = luxury_resources(g, q)
            .into_iter()
            .filter(|&(r, d)| d.net >= p.lux_spare_at && net(r) <= 0)
            .map(|(r, _)| name(r))
            .collect();
        theirs.sort();
        for resource in theirs {
            let known = out
                .wants
                .iter()
                .any(|w| matches!(w, Want::Resource { resource: x, .. } if *x == resource));
            if !known {
                out.wants.push(Want::Resource { resource, from: q });
            }
        }
    }
    for w in &out.war_readiness {
        // The ratio as the advice shows it, rounded, as Python compared it.
        if w.at_war && w.power_ratio < p.losing_ratio {
            out.wants.push(Want::PeaceTreaty { with: w.player });
        }
    }
    out.wants.truncate(WANTS);
    out
}

/// A resource's name.
fn r_name(g: &Game, r: ResourceId) -> String {
    g.rules().name(r).unwrap_or_default().to_owned()
}

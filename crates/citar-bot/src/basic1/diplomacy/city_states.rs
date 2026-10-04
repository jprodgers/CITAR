//! Gifts of gold to city-states (`_court_city_states` and `_gift_city_state`,
//! basic.py:1645-1694), at the end of the gold phase (`manage_gold`, 1640), with city-states the
//! bot's (`Owners`).
//!
//! In the classic mode, with gold over `cs_gift_gold` (more by era) and no war, `cs_gift_amount`
//! goes to the city-state it has met where it stands best: a friend above
//! `cs_gift_focus_influence` first, then the most influence. In the typed mode it gifts where
//! the gift buys the most: where it crosses the friend or ally threshold or defends an alliance
//! (`cs_v_*`), weighted by what the city-state's type gives and what the civilization needs
//! (Mercantile when unhappy, `cs_w_*`), never to one it is allied with safely; a larger gift
//! when it is rich.
//!
//! The city-state types are compared by the ids `Resolved` gives their names, once per ruleset.

use citar_engine::base::ids::PlayerId;
use citar_engine::game::Action;
use citar_engine::game::Game;
use citar_engine::game::city_states::actions::influence_from_gold;
use citar_engine::game::city_states::influence::{data, influence};
use citar_engine::game::city_states::{ALLY_INFLUENCE, CityStateAction, FRIEND_INFLUENCE};
use serde_json::json;

use super::gold;
use crate::basic1::Seat;
use crate::basic1::context::Context;
use crate::driver::Turn;
use crate::params::{CityStateKind, CsGiftMode};

/// The city-state gifts of a turn (`_court_city_states`, basic.py:1645-1657).
pub(crate) fn court_city_states(t: &mut Turn<'_>, s: &Seat<'_>, ctx: &Context) {
    let choice = match s.params.cs_gift_mode {
        CsGiftMode::Typed => typed(t.game(), t.pid(), s, ctx),
        CsGiftMode::Classic => classic(t.game(), t.pid(), s, ctx),
    };
    if let Some((cs, amount)) = choice {
        t.act(Action::CityStateAction(CityStateAction {
            player_id: i64::from(cs.0),
            action: json!("gift_gold"),
            amount: Some(i64::from(amount)),
            unit_id: None,
        }));
    }
}

/// The era as a count, for the parameters' "per era".
fn era(ctx: &Context) -> f64 {
    f64::from(u32::try_from(ctx.era).unwrap_or(u32::MAX))
}

/// The living city-states `pid` has met and is at peace with, in player-id order.
fn courted(g: &Game, pid: PlayerId) -> impl Iterator<Item = PlayerId> + '_ {
    g.city_states(true).map(|q| q.id()).filter(move |&q| g.has_met(pid, q) && !g.at_war(pid, q))
}

/// The classic gift (basic.py:1651-1657): whom, and how much.
fn classic(g: &Game, pid: PlayerId, s: &Seat<'_>, ctx: &Context) -> Option<(PlayerId, i32)> {
    let p = s.params;
    let floor = f64::from(p.cs_gift_gold) + f64::from(p.cs_gift_gold_per_era) * era(ctx);
    if gold(g, pid) <= floor || !ctx.wars.is_empty() {
        return None;
    }
    // A friend past `cs_gift_focus_influence` first, then the most influence; the first of
    // equals, as Python's `max` kept it.
    let mut best: Option<(PlayerId, (bool, f64))> = None;
    for q in courted(g, pid) {
        let inf = influence(g, q, pid);
        let key = (inf >= f64::from(p.cs_gift_focus_influence), inf);
        let better = best.is_none_or(|(_, (bf, bi))| {
            key.0 > bf || (key.0 == bf && key.1.total_cmp(&bi).is_gt())
        });
        if better {
            best = Some((q, key));
        }
    }
    best.map(|(q, _)| (q, p.cs_gift_amount))
}

/// The typed gift (`_gift_city_state`, basic.py:1659-1694): whom, and how much.
fn typed(g: &Game, pid: PlayerId, s: &Seat<'_>, ctx: &Context) -> Option<(PlayerId, i32)> {
    let p = s.params;
    let treasury = gold(g, pid);
    let reserve = f64::from(p.cs_typed_reserve) + f64::from(p.cs_typed_reserve_per_era) * era(ctx);
    if treasury < reserve + f64::from(p.cs_typed_min_spare) || !ctx.wars.is_empty() {
        return None;
    }
    let cities = i64::try_from(ctx.cities.len()).unwrap_or(i64::MAX);
    let need_hap = i64::from(ctx.hap) < cities + i64::from(p.cs_typed_hap_margin);
    let weight = |kind: Option<CityStateKind>| match kind {
        Some(CityStateKind::Mercantile) if need_hap => p.cs_w_mercantile_unhappy,
        Some(CityStateKind::Mercantile) => p.cs_w_mercantile,
        Some(CityStateKind::Maritime) => p.cs_w_maritime,
        Some(CityStateKind::Cultured) => p.cs_w_cultured,
        Some(CityStateKind::Religious) => p.cs_w_religious,
        Some(CityStateKind::Militaristic) => p.cs_w_militaristic,
        None => 1.0,
    };
    let amount = if treasury - reserve < f64::from(p.cs_typed_big_above) {
        p.cs_typed_amount
    } else {
        p.cs_typed_big_amount
    };
    let rivals_of: Vec<PlayerId> = g.majors(true).map(|o| o.id()).filter(|&o| o != pid).collect();
    let mut best: Option<PlayerId> = None;
    let mut best_v = 0.0;
    for q in courted(g, pid) {
        let inf = influence(g, q, pid);
        let after = inf + f64::from(influence_from_gold(g, q, pid, amount));
        // The best standing of any other civilization (Python's `max(..., default=0)`).
        let rivals = if rivals_of.is_empty() {
            0.0
        } else {
            rivals_of.iter().map(|&o| influence(g, q, o)).fold(f64::NEG_INFINITY, f64::max)
        };
        let d = data(g, q);
        let ally = d.and_then(|d| d.ally());
        let mut v = 0.0;
        if inf < FRIEND_INFLUENCE && FRIEND_INFLUENCE <= after {
            v += p.cs_v_friend;
        }
        if after >= ALLY_INFLUENCE && ally != Some(pid) && after > rivals {
            v += p.cs_v_ally;
        }
        if ally == Some(pid) && rivals > inf - f64::from(p.cs_typed_defend_margin) {
            // Defend the alliance.
            v += p.cs_v_defend;
        }
        if ally == Some(pid)
            && inf - rivals > f64::from(p.cs_typed_safe_lead)
            && inf > ALLY_INFLUENCE + f64::from(p.cs_typed_safe_above_ally)
        {
            // Safely allied: no need to pay more.
            v = 0.0;
        }
        v *= weight(d.and_then(|d| d.cs_type).and_then(|t| s.resolved.city_state_kind(t)));
        if v > best_v {
            best = Some(q);
            best_v = v;
        }
    }
    best.map(|q| (q, amount))
}

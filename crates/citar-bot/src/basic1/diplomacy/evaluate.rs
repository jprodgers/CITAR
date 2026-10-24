//! What a deal is worth to the bot (`evaluate` and `_war_item_value`, basic.py:2558-2643), in
//! gold, from its own side: positive is worth accepting. The same function values both sides,
//! so the bot's own offers are ones it would accept, which is what stops it proposing what
//! nobody would take.
//!
//! The items are the engine's typed `DealItem`s, as a negotiation's `Terms` hold them; the
//! seat's memory says whether a war is planned or being prepared, which moves what peace and
//! a war on someone are worth.
//!
//! Kept from Python on purpose: a peace treaty, which a proposal lists on both sides, is valued
//! on each, where friendship, pacts and research agreements are counted once (on the side the
//! bot receives). Python's comment says mutual items count once, so this was likely a slip; but
//! every profile's peace weights (`deal_peace`, `deal_peace_when_strong`,
//! `deal_peace_while_winning`) were tuned with it, and refcheck's `bot_value` and
//! `bot_advice_plain_data` record it (a peace for 50 gold in an even war is worth 190, two
//! `deal_peace` of 120 less the gold). Halving it would retune the bot, not fix a rule.

use citar_engine::api::views::empire::{luxury_resources, strategic_resources};
use citar_engine::base::ids::{PlayerId, ResourceId};
use citar_engine::game::Game;
use citar_engine::game::diplomacy::deals::ra_cost;
use citar_engine::game::diplomacy::relations::{has_pact, is_friends, opinion};
use citar_engine::game::query;
use citar_engine::game::research::tech_cost;
use citar_engine::rules::defs::ResourceType;
use citar_engine::state::diplo::DealItem;

use super::gold;
use crate::BotSpec;
use crate::basic1::units::war_plan::military_power;
use crate::memory::Memory;
use crate::params::Params;

/// What the bot values a deal with: its spec (the aggression), its parameters and its memory.
#[derive(Clone, Copy)]
pub(crate) struct Mind<'a> {
    pub spec: &'a BotSpec,
    pub params: &'a Params,
    pub memory: &'a Memory,
}

/// `evaluate` (basic.py:2558-2617): the worth to `pid` of a deal with `other` in which it gives
/// `give` and receives `receive`.
pub(crate) fn evaluate(
    g: &Game,
    m: Mind<'_>,
    pid: PlayerId,
    other: PlayerId,
    give: &[DealItem],
    receive: &[DealItem],
) -> f64 {
    let p = m.params;
    let (mine, theirs) = (military_power(g, pid), military_power(g, other));
    let mut lines = Lines::default();
    let mut v = 0.0;
    for (items, sign) in [(receive, 1i32), (give, -1i32)] {
        let s = f64::from(sign);
        for &it in items {
            match it {
                DealItem::Gold { amount } => v += s * f64::from(amount),
                DealItem::GoldPerTurn { amount, turns } => {
                    v += s * f64::from(amount) * f64::from(turns) * p.deal_gpt;
                }
                DealItem::Resource { resource, amount, .. } => {
                    v += s
                        * resource_worth(g, p, pid, &mut lines, resource, sign)
                        * f64::from(amount);
                }
                DealItem::OpenBorders { .. } => v += s * f64::from(p.deal_open_borders),
                DealItem::Embassy => v += s * f64::from(p.deal_embassy),
                DealItem::PeaceTreaty => {
                    // Not signed, and counted on each side it is listed on (the module's note).
                    let plan = m.memory.war_plan.as_ref();
                    let attacking =
                        plan.is_some_and(|w| g.city_at(w.city).is_some_and(|c| c.owner() == other));
                    let campaign = plan.is_some_and(|w| {
                        i64::from(g.turn()) - i64::from(w.since)
                            < i64::from(p.deal_peace_campaign_turns)
                    });
                    if attacking && campaign && mine > theirs * p.deal_peace_campaign_ratio {
                        v -= f64::from(p.deal_peace_while_winning);
                    } else if mine <= theirs * p.deal_peace_strong_ratio {
                        v += f64::from(p.deal_peace);
                    } else {
                        v -= f64::from(p.deal_peace_when_strong);
                    }
                }
                DealItem::DeclarationOfFriendship | DealItem::DefensivePact => {
                    // Mutual items appear on both sides: counted once, on ours.
                    if sign > 0 {
                        v += if opinion(g, pid, other) >= 0.0 && m.memory.war_prep.is_none() {
                            f64::from(p.deal_friendship)
                        } else {
                            -f64::from(p.deal_friendship_refuse)
                        };
                    }
                }
                DealItem::ResearchAgreement => {
                    if sign > 0 {
                        let cost = f64::from(ra_cost(g, pid, other)) + f64::from(p.deal_ra_margin);
                        v += if gold(g, pid) > cost {
                            f64::from(p.deal_ra)
                        } else {
                            -f64::from(p.deal_ra_refuse)
                        };
                    }
                }
                DealItem::DeclareWar { target } => {
                    v += s * war_item_value(g, m, pid, target, sign < 0, mine);
                }
                DealItem::ShareMap => {
                    v += s * f64::from(if sign > 0 { p.deal_map_get } else { p.deal_map_give });
                }
                DealItem::City { city_id } => {
                    let pop = g.city(city_id).map_or(1.0, |c| f64::from(c.pop));
                    v += s * (f64::from(p.deal_city) + f64::from(p.deal_city_per_pop) * pop);
                }
                DealItem::Tech { tech } => {
                    let cost = s * f64::from(tech_cost(g, pid, tech));
                    v += cost * if sign > 0 { p.deal_tech_get } else { p.deal_tech_give };
                }
            }
        }
    }
    v
}

/// The resource lines a valuation reads, worked out once and only if a resource is valued.
#[derive(Default)]
struct Lines {
    luxuries: Option<Vec<(ResourceId, i32)>>,
    strategic: Option<Vec<(ResourceId, i32)>>,
}

/// One of `resource` to `pid`, unsigned (basic.py:2576-2586): a luxury it lacks (receiving) or
/// would lack without this one (giving) is `deal_lux_needed`, any other `deal_lux_spare`; a
/// strategic resource is worth more by era, more again when it has none available. A bonus
/// resource, which no deal can carry, is worth nothing.
fn resource_worth(
    g: &Game,
    p: &Params,
    pid: PlayerId,
    lines: &mut Lines,
    resource: ResourceId,
    sign: i32,
) -> f64 {
    let kind = g.rules().resources().get(resource).map(|d| d.kind);
    let have = |list: &[(ResourceId, i32)]| {
        list.iter().find(|&&(r, _)| r == resource).map_or(0, |&(_, n)| n)
    };
    match kind {
        Some(ResourceType::Luxury) => {
            let list = lines.luxuries.get_or_insert_with(|| {
                luxury_resources(g, pid).into_iter().map(|(r, l)| (r, l.net)).collect()
            });
            let have = have(list);
            let needed = (sign > 0 && have <= 0) || (sign < 0 && have <= 1);
            f64::from(if needed { p.deal_lux_needed } else { p.deal_lux_spare })
        }
        Some(ResourceType::Strategic) => {
            let list = lines.strategic.get_or_insert_with(|| {
                strategic_resources(g, pid).into_iter().map(|(r, s)| (r, s.available)).collect()
            });
            // Strategic resources matter more as units need them: scaled by the era.
            let era = 1.0 + f64::from(query::era(g, pid).0);
            if have(list) <= 0 {
                f64::from(p.deal_strategic_needed)
                    + f64::from(p.deal_strategic_needed_per_era) * era
            } else {
                f64::from(p.deal_strategic_spare_per_era) * era
            }
        }
        Some(ResourceType::Bonus) | None => 0.0,
    }
}

/// `_war_item_value` (basic.py:2619-2643): a "declare war on `target`" item. Received (they
/// declare), it helps against someone the seat fights or plans to fight (`deal_ally_war`), else
/// little (`deal_other_war`). Given (the seat declares), it costs the risk of fighting the
/// target, more for a city-state's protectors and the betrayal of a friend or ally, less for a
/// war it meant to fight anyway, scaled down by aggression; nothing for a war it is in already.
fn war_item_value(
    g: &Game,
    m: Mind<'_>,
    pid: PlayerId,
    target: PlayerId,
    giving: bool,
    mine: f64,
) -> f64 {
    let p = m.params;
    let prepared = m
        .memory
        .war_prep
        .as_ref()
        .and_then(|w| w.target)
        .is_some_and(|t| g.city_at(t).is_some_and(|c| c.owner() == target));
    let planned = m
        .memory
        .war_plan
        .as_ref()
        .is_some_and(|w| g.city_at(w.city).is_some_and(|c| c.owner() == target));
    let ours = g.at_war(pid, target) || prepared || planned;
    if !giving {
        return f64::from(if ours { p.deal_ally_war } else { p.deal_other_war });
    }
    if g.at_war(pid, target) {
        return 0.0;
    }
    let theirs = military_power(g, target);
    let mut cost = f64::from(p.deal_war_cost)
        + f64::from(p.deal_war_risk) * (theirs / mine.max(1.0) - p.deal_war_risk_from).max(0.0);
    if g.is_city_state(target) {
        // Other city-states and the target's protectors take offence.
        cost += f64::from(p.deal_war_city_state);
    }
    if is_friends(g, pid, target) || has_pact(g, pid, target) {
        cost += f64::from(p.deal_war_betrayal);
    }
    if ours {
        // It meant to fight them anyway.
        cost *= p.deal_war_planned_share;
    }
    cost * (p.deal_war_mult - p.deal_war_mult_aggr * m.spec.aggression)
}

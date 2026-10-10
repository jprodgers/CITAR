//! Luxury trades (`trade_luxuries` and `_buy_luxury`, basic.py:2505-2556): a luxury the seat has
//! to spare offered to a civilization that lacks it, for a luxury of theirs it lacks or for
//! gold every turn; and, while its happiness is short, gold every turn offered for a luxury it
//! lacks. A new luxury type is happiness for every city, the cheapest there is, and each side of
//! such a trade gains one.
//!
//! What the bot would offer is worked out first ([`plan`], which only reads), then offered: a
//! sale once, to the first civilization that would take one, whatever it answers; a purchase
//! to each civilization in turn until one negotiation opens. The reference checks ask the plan
//! (`crate::decisions`), as Python's recorder refused every offer.

use citar_engine::api::views::empire::{Luxury, luxury_resources};
use citar_engine::base::ids::{PlayerId, ResourceId};
use citar_engine::base::stats::Stat;
use citar_engine::game::{Game, query};
use serde_json::{Value, json};

use super::{gold, open};
use crate::basic1::Seat;
use crate::driver::Turn;

/// A negotiation the bot would open: with whom, its message, and what it gives and asks, as a
/// caller writes deal items.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Offer {
    pub to: PlayerId,
    pub message: &'static str,
    pub give: Vec<Value>,
    pub receive: Vec<Value>,
}

/// What [`plan`] finds to offer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Trade {
    /// A luxury to spare, offered once to the first civilization that lacks it.
    Sell(Offer),
    /// Gold every turn for a luxury, offered to each civilization that has one to spare in turn
    /// until one negotiation opens.
    Buy(Vec<Offer>),
    /// Nothing.
    Nothing,
}

impl Trade {
    /// Every offer it would make were each refused, in order: what Python's recorder saw.
    pub(crate) fn offers(&self) -> Vec<Offer> {
        match self {
            Self::Sell(o) => vec![o.clone()],
            Self::Buy(list) => list.clone(),
            Self::Nothing => Vec::new(),
        }
    }
}

/// A luxury's net amount in a civilization's lines (0 for one it has no line of).
fn net(lines: &[(ResourceId, Luxury)], r: ResourceId) -> i32 {
    lines.iter().find(|(x, _)| *x == r).map_or(0, |(_, l)| l.net)
}

/// A resource item, as `trade_luxuries` wrote one: one of `r`, for the deal's turns.
fn resource(g: &Game, r: ResourceId) -> Value {
    json!({"type": "resource", "resource": g.rules().name(r).unwrap_or_default(), "amount": 1})
}

/// Gold every turn for the speed's deal duration.
fn gold_per_turn(g: &Game, amount: i32) -> Value {
    json!({"type": "gold_per_turn", "amount": amount, "turns": g.speed().deal_duration})
}

/// The first of `theirs`' luxuries they have to spare and `mine` lacks, in the ruleset's order.
fn wanted(
    theirs: &[(ResourceId, Luxury)],
    mine: &[(ResourceId, Luxury)],
    spare_at: i32,
) -> Option<ResourceId> {
    theirs.iter().find(|&&(r, d)| d.net >= spare_at && net(mine, r) <= 0).map(|&(r, _)| r)
}

/// The trade `pid` would offer, visiting the civilizations in `met` in order
/// (`trade_luxuries`, basic.py:2505-2532): with a luxury to spare (`lux_spare_at`), the first
/// civilization at peace with it that lacks one of them is offered the first such, for the
/// first luxury of theirs it lacks, else for `lux_sell_gpt` gold a turn if they hold
/// `lux_buyer_min_gold`; failing that, and with `lux_buy`, [`buy`].
pub(crate) fn plan(g: &Game, pid: PlayerId, s: &Seat<'_>, met: &[PlayerId]) -> Trade {
    let p = s.params;
    let mine = luxury_resources(g, pid);
    let surplus: Vec<ResourceId> =
        mine.iter().filter(|(_, d)| d.net >= p.lux_spare_at).map(|&(r, _)| r).collect();
    if !surplus.is_empty() {
        for &q in met {
            if !trading_partner(g, pid, q) {
                continue;
            }
            let theirs = luxury_resources(g, q);
            let Some(give) = surplus.iter().copied().find(|&r| net(&theirs, r) <= 0) else {
                continue;
            };
            let receive = match wanted(&theirs, &mine, p.lux_spare_at) {
                Some(want) => resource(g, want),
                None if gold(g, q) < f64::from(p.lux_buyer_min_gold) => continue,
                None => gold_per_turn(g, p.lux_sell_gpt),
            };
            return Trade::Sell(Offer {
                to: q,
                message: "We have luxuries to spare. Shall we trade?",
                give: vec![resource(g, give)],
                receive: vec![receive],
            });
        }
    }
    if p.lux_buy { buy(g, pid, s, met, &mine) } else { Trade::Nothing }
}

/// A living major at peace with `pid`.
fn trading_partner(g: &Game, pid: PlayerId, q: PlayerId) -> bool {
    g.player(q).is_some_and(|x| x.alive() && x.is_major()) && !g.at_war(pid, q)
}

/// `_buy_luxury` (basic.py:2534-2556): while the seat's happiness is under its cities plus
/// `lux_buy_hap_margin` and it makes `lux_buy_gpt` gold a turn, that much gold a turn for the
/// first luxury each civilization at peace with it has to spare and it lacks.
fn buy(
    g: &Game,
    pid: PlayerId,
    s: &Seat<'_>,
    met: &[PlayerId],
    mine: &[(ResourceId, Luxury)],
) -> Trade {
    let p = s.params;
    let cities = i64::try_from(g.player_cities(pid).count()).unwrap_or(i64::MAX);
    if i64::from(query::happiness(g, pid).total) >= cities + i64::from(p.lux_buy_hap_margin) {
        return Trade::Nothing;
    }
    if query::civ_stats(g, pid).total[Stat::Gold] < f64::from(p.lux_buy_gpt) {
        return Trade::Nothing;
    }
    let mut offers = Vec::new();
    for &q in met {
        if !trading_partner(g, pid, q) {
            continue;
        }
        let theirs = luxury_resources(g, q);
        let Some(want) = wanted(&theirs, mine, p.lux_spare_at) else { continue };
        offers.push(Offer {
            to: q,
            message: "We would pay for a luxury you have to spare.",
            give: vec![gold_per_turn(g, p.lux_buy_gpt)],
            receive: vec![resource(g, want)],
        });
    }
    if offers.is_empty() { Trade::Nothing } else { Trade::Buy(offers) }
}

/// `trade_luxuries` (basic.py:2505-2556): the [`plan`]'s offers made, a sale once and a
/// purchase until one opens.
pub(crate) fn trade_luxuries(t: &mut Turn<'_>, s: &Seat<'_>) {
    let pid = t.pid();
    let met = super::met_majors(t.game(), pid);
    match plan(t.game(), pid, s, &met) {
        Trade::Sell(o) => {
            open(t, o.to, o.message, o.give, o.receive);
        }
        Trade::Buy(offers) => {
            for o in offers {
                if open(t, o.to, o.message, o.give, o.receive) {
                    break;
                }
            }
        }
        Trade::Nothing => {}
    }
}

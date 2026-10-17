//! The bot's answer to a negotiation that waits on it (`respond`, basic.py:2651-2708): accept,
//! reject or counter, always with a line of text.
//!
//! It reads only the items, never the words, and it never loops: talk with nothing on the table
//! gets one invitation to make a proposal, then a rejection; its own proposal answered with
//! words alone stands once, then it rejects; a deal it values at nothing or more it accepts; one
//! short by less than `counter_max_gap` it counters, at most `counter_rounds` times, asking for
//! the shortfall plus `counter_margin` in gold, added to the gold already on the table (so the
//! deal keeps one gold line) and never more than the other side holds less what is on the table
//! already, since the rules check only the counterer's side and an ask they could not pay would
//! fail only when accepted. Asking for gold is a trade: with trades the model's, a short deal is
//! rejected instead. A counter or acceptance the rules refuse becomes a rejection, so the
//! negotiation never stays open waiting on the bot.
//!
//! The driver puts only the negotiations the bot owns to it (`Owners::owns`); those the seat's
//! model owns are deferred to the host (DESIGN.md P2.3.8).

use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::base::num;
use citar_engine::game::Action;
use citar_engine::game::diplomacy::actions::RespondNegotiation;
use citar_engine::game::diplomacy::category::Category;
use citar_engine::rules::Ruleset;
use citar_engine::state::diplo::{DealItem, NegAction, NegStatus};
use serde_json::{Value, json};

use super::evaluate::{Mind, evaluate};
use super::gold;
use crate::basic1::Seat;
use crate::driver::Turn;

/// Sends one response in `nid`, with `items` (what the bot gives, what it asks) for a counter:
/// whether the rules took it.
fn answer(
    t: &mut Turn<'_>,
    nid: NegotiationId,
    action: &str,
    message: &str,
    items: Option<(Value, Value)>,
) -> bool {
    let (give, receive) = items.map_or((None, None), |(g, r)| (Some(g), Some(r)));
    t.act(Action::RespondNegotiation(RespondNegotiation {
        negotiation_id: i64::from(nid.get()),
        action: json!(action),
        message: Some(json!(message)),
        give,
        receive,
    }))
    .is_some()
}

/// Items as a caller writes them, or `None` if one names what the ruleset lacks.
fn written(rules: &Ruleset, items: &[DealItem]) -> Option<Value> {
    items.iter().map(|it| it.to_json(rules)).collect::<Option<Vec<_>>>().map(Value::Array)
}

/// `respond` (basic.py:2651-2708): the bot's answer to negotiation `nid`, if it waits on the
/// seat.
pub(crate) fn respond(t: &mut Turn<'_>, s: &Seat<'_>, nid: NegotiationId) {
    let pid = t.pid();
    let p = s.params;
    let Some(n) = t.game().negotiation(nid).cloned() else { return };
    if n.status != NegStatus::Open || n.awaiting != Some(pid) {
        return;
    }
    let other: PlayerId = if pid == n.initiator { n.responder } else { n.initiator };
    let mine = |action: NegAction| {
        n.history.iter().filter(|h| h.by == Some(pid) && h.action == action).count()
    };
    let Some(proposal) = &n.proposal else {
        if mine(NegAction::Reply) > 0 {
            answer(t, nid, "reject", "We have nothing further to discuss.", None);
        } else {
            answer(t, nid, "reply", "Words are wind. Make a concrete proposal.", None);
        }
        return;
    };
    if n.proposal_by == Some(pid) {
        // Its proposal stands and they answered with words alone: it says so once, then closes.
        let made =
            n.history.iter().rposition(|h| h.by == Some(pid) && h.proposal.is_some()).unwrap_or(0);
        let replied =
            n.history[made..].iter().any(|h| h.by == Some(pid) && h.action == NegAction::Reply);
        if replied {
            answer(t, nid, "reject", "Then we have no deal.", None);
        } else {
            answer(t, nid, "reply", "Our offer stands.", None);
        }
        return;
    }
    let g = t.game();
    let give = proposal.gives(pid);
    let receive = proposal.gives(other);
    let mind = Mind { spec: s.spec, params: p, memory: s.memory };
    let value = evaluate(g, mind, pid, other, give, receive);
    let countered = mine(NegAction::Counter);
    if value >= 0.0 {
        if !answer(t, nid, "accept", "Agreed.", None) {
            answer(t, nid, "reject", "We cannot fulfil those terms.", None);
        }
        return;
    }
    // Ask for more of the gold already on the table rather than a second gold line, and never
    // for more than they hold in all.
    let mut asked: Vec<DealItem> = receive.to_vec();
    let lump = asked.iter().position(|it| matches!(it, DealItem::Gold { .. }));
    let on_table = lump.map_or(0, |i| match asked[i] {
        DealItem::Gold { amount } => i64::from(amount),
        _ => 0,
    });
    let ask = (num::trunc_i64(-value) + i64::from(p.counter_margin))
        .min(num::trunc_i64(gold(g, other)) - on_table);
    #[allow(clippy::cast_precision_loss, reason = "an amount of gold, far below 2^53")]
    let covers = ask as f64 >= -value;
    let rounds = i64::try_from(countered).unwrap_or(i64::MAX) < i64::from(p.counter_rounds);
    if rounds
        && value > -f64::from(p.counter_max_gap)
        && covers
        && !s.spec.owners.llm(Category::Trades)
    {
        let gold_item =
            |amount: i64| i32::try_from(amount).ok().map(|a| DealItem::Gold { amount: a });
        let grown = match lump {
            Some(i) => gold_item(on_table + ask).map(|it| asked[i] = it),
            None => gold_item(ask).map(|it| asked.push(it)),
        };
        let items =
            grown.and_then(|()| Some((written(g.rules(), give)?, written(g.rules(), &asked)?)));
        let message = format!("Add {ask} gold and we have a deal.");
        if !items.is_some_and(|items| answer(t, nid, "counter", &message, Some(items))) {
            // A counter the rules refuse must still end the bot's move, or the negotiation
            // stays open waiting on it.
            answer(t, nid, "reject", "That does not interest us.", None);
        }
    } else {
        let line = if countered == 0 {
            "That does not interest us."
        } else {
            "That is our last word, then. No deal."
        };
        answer(t, nid, "reject", line, None);
    }
}

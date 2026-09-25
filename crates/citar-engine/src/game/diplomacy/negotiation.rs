//! Messages and negotiations (`diplomacy.py:303-337, 694-963`), in the chat model of Phase 0: a
//! negotiation is a chat between two major civilizations with a deal on the table.
//!
//! Every entry carries a message and a number (`seq`); the history keeps four actions (`open`,
//! `reply`, `counter`, `accept`, `reject`, and `close` for what the game closes from outside),
//! with `decline`, `withdraw` and `end` read as `reject`. Only the side whose move it is may
//! accept, counter or reply; either side may reject at any time, which is how the opener
//! withdraws. A chat that would pass its message cap closes instead of taking the message, so an
//! open chat never holds more than the cap (invariant NEG-1). Neither side may end its turn while
//! a negotiation it is in is open ([`end_turn_refusal`], which the `end_turn` action reads);
//! `Game::end_turn` keeps Python's safety net, expiring the negotiations the player opened
//! (`expire_for`, stage E0). A war cancels the negotiations of its two sides.
//!
//! Each step is split as the action pipeline runs it: a `plan_*` that only reads and refuses,
//! and an apply that cannot fail. The actions read a proposal as a caller writes it, in JSON;
//! [`plan_open_terms`] and [`plan_respond_terms`] take typed items, for bots and the host.
//!
//! What differs, on purpose (tests/rules/intended.toml): a message to "all" goes to the
//! civilizations met in player-id order, where Python kept the order they were met
//! (`met-lists-in-player-id-order`).

use serde_json::{Value, json};

use super::deals::{
    describe_items, execute_deal, make_proposal, plan_deal, proposal_of, validate_items,
};
use super::relations::name;
use crate::base::ids::{MessageId, NegotiationId, PlayerId};
use crate::base::py;
use crate::base::sets::PlayerSet;
use crate::base::text::truncate_chars;
use crate::game::Game;
use crate::game::derive::rev::DiploTouch;
use crate::game::error::{ActionError, ErrCode};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::diplo::{DealItem, NegAction, NegEntry, NegStatus, Negotiation, Terms};
use crate::state::players::Player;

/// The longest message kept, in code points (`diplomacy.py:308, 780`).
pub const MESSAGE_LIMIT: usize = 4000;

/// The most of a message an event quotes (`diplomacy.py:787, 835`).
const QUOTE_LIMIT: usize = 500;

/// The longest note kept on a closed negotiation (`diplomacy.py:889`).
const NOTE_LIMIT: usize = 500;

fn refuse(message: impl Into<String>) -> ActionError {
    ActionError::new(ErrCode::Negotiation, message)
}

/// The two players as an audience.
fn pair(a: PlayerId, b: PlayerId) -> PlayerSet {
    [a, b].into_iter().collect()
}

// ---- Messages (diplomacy.py:306-337) ----------------------------------------------------------

/// Records a message from `sender` to `recipients`, its text cut to [`MESSAGE_LIMIT`]
/// (`add_message`, `diplomacy.py:306-310`).
pub fn add_message(
    g: &mut Game,
    sender: PlayerId,
    recipients: &[PlayerId],
    text: &str,
) -> Option<MessageId> {
    let to: PlayerSet = recipients.iter().copied().collect();
    g.record_message(sender, to, truncate_chars(text, MESSAGE_LIMIT))
}

/// A message checked: to whom, in the order named, and what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePlan {
    pub recipients: Vec<PlayerId>,
    pub text: String,
}

/// Checks a message from `pid` (`send_message`, `diplomacy.py:313-333`): some text, to one
/// civilization or a list of them by id, or to `"all"` it has met (or when `to` is null), each a
/// major civilization it has met.
///
/// # Errors
/// No text, a recipient that is no player id or no major met, or nobody met for `"all"`.
pub fn plan_send_message(
    g: &Game,
    pid: PlayerId,
    to: &Value,
    text: &Value,
) -> Result<MessagePlan, ActionError> {
    if !py::truthy(text) || py::strip(&py::str_of(text)).is_empty() {
        return Err(ActionError::rule("Message text is empty."));
    }
    let recipients: Vec<PlayerId> = match to {
        Value::Null => met_majors(g, pid),
        Value::String(s) if s == "all" => met_majors(g, pid),
        _ => {
            let ids: Vec<&Value> = match to {
                Value::Array(list) => list.iter().collect(),
                v => vec![v],
            };
            let mut out = Vec::with_capacity(ids.len());
            for q in ids {
                let Some(n) = py::int_of(q) else {
                    return Err(ActionError::rule(format!(
                        "Invalid recipient '{}'. Use player ids (numbers) or 'all'.",
                        py::str_of(q)
                    )));
                };
                let p = u8::try_from(n)
                    .ok()
                    .map(PlayerId)
                    .filter(|&p| p != pid && g.player(p).is_some_and(Player::is_major));
                let Some(p) = p else {
                    return Err(ActionError::rule(format!(
                        "Invalid recipient {n} (messages go to major civilizations)."
                    )));
                };
                if !g.has_met(pid, p) {
                    return Err(ActionError::rule(format!("You have not met {} yet.", name(g, p))));
                }
                out.push(p);
            }
            return Ok(MessagePlan { recipients: out, text: py::str_of(text) });
        }
    };
    if recipients.is_empty() {
        return Err(ActionError::rule("You have not met any other civilization yet."));
    }
    Ok(MessagePlan { recipients, text: py::str_of(text) })
}

/// The living major civilizations `pid` has met, in player-id order.
fn met_majors(g: &Game, pid: PlayerId) -> Vec<PlayerId> {
    // refcheck: met-lists-in-player-id-order
    g.majors(true).map(Player::id).filter(|&q| q != pid && g.has_met(pid, q)).collect()
}

/// Sends a message [`plan_send_message`] checked, and says to whom (`diplomacy.py:334-337`).
/// Nothing said is binding.
pub fn send_message(g: &mut Game, pid: PlayerId, plan: &MessagePlan) -> Value {
    let id = add_message(g, pid, &plan.recipients, &plan.text);
    let names: Vec<String> = plan.recipients.iter().map(|&q| name(g, q)).collect();
    let names = names.join(", ");
    let text =
        format!("{} \u{2192} {names}: {}", name(g, pid), truncate_chars(&plan.text, MESSAGE_LIMIT));
    let mut audience = PlayerSet::single(pid);
    for &q in &plan.recipients {
        audience.insert(q);
    }
    let data = EventData { message: id, sender: Some(pid), ..EventData::default() };
    g.emit(EngineEvent::Message, &text, Some(audience), None, data, &[]);
    json!({"sent_to": names, "message_id": id.map(MessageId::get)})
}

// ---- Negotiations (diplomacy.py:694-963) -------------------------------------------------------

/// How many messages a negotiation may hold before it closes: the game's own setting, at least
/// 2, else the ruleset's (`max_chat_messages`, `diplomacy.py:712-717`).
#[must_use]
pub fn max_chat_messages(g: &Game) -> u32 {
    match g.state().config().diplomacy.max_chat_messages {
        Some(n) if n > 0 => u32::from(n.max(2)),
        _ => g.rules().constants().diplomacy.max_chat_messages,
    }
}

/// The negotiation with id `nid` (`get_negotiation`, `diplomacy.py:704-709`).
///
/// # Errors
/// No negotiation has that id.
pub fn get(g: &Game, nid: i64) -> Result<&Negotiation, ActionError> {
    u32::try_from(nid)
        .ok()
        .and_then(NegotiationId::new)
        .and_then(|id| g.state().diplo().negotiation(id))
        .ok_or_else(|| refuse(format!("No negotiation with id {nid}.")))
}

/// The side of a negotiation `pid` is not: the initiator for anyone but the initiator.
fn other_side(n: &Negotiation, pid: PlayerId) -> PlayerId {
    if pid == n.initiator { n.responder } else { n.initiator }
}

/// Edits a negotiation, which exists.
fn edit(g: &mut Game, nid: NegotiationId, f: impl FnOnce(&mut Negotiation)) {
    if let Some(n) = g.edit_diplo(DiploTouch::NEGOTIATIONS).negotiation_mut(nid) {
        f(n);
    }
}

/// Appends an entry to a negotiation's history, numbered after the last (`_add_entry`,
/// `diplomacy.py:720-729`).
fn add_entry(
    g: &mut Game,
    nid: NegotiationId,
    by: Option<PlayerId>,
    action: NegAction,
    message: &str,
    proposal: Option<Terms>,
    note: Option<&str>,
) {
    let turn = g.turn();
    edit(g, nid, |n| {
        let seq = u16::try_from(n.history.len() + 1).unwrap_or(u16::MAX);
        n.history.push(NegEntry {
            seq,
            by,
            action,
            message: message.into(),
            proposal,
            turn,
            note: note.filter(|s| !s.is_empty()).map(Into::into),
        });
    });
}

/// ` Proposal: Rome gives 60 gold; Greece gives nothing.`, with its lead word.
fn proposal_text(g: &Game, lead: &str, t: &Terms, speaker: PlayerId, other: PlayerId) -> String {
    format!(
        " {lead}: {} gives {}; {} gives {}.",
        name(g, speaker),
        describe_items(g, t.gives(speaker)),
        name(g, other),
        describe_items(g, t.gives(other))
    )
}

/// A negotiation checked, for [`open`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPlan {
    pub to: PlayerId,
    pub text: String,
    pub proposal: Option<Terms>,
}

/// Checks a negotiation `pid` would open with `to` (`open_negotiation`, `diplomacy.py:758-779`):
/// a living major it has met, a message, no negotiation of the two still open, no more than the
/// ruleset's number opened with them this turn, and a proposal whose items `pid` can give, read
/// as a caller writes them. That it is `pid`'s turn is the action's guard; a host's
/// `open_negotiation_as` lends the turn, as Python's did.
///
/// # Errors
/// Why it may not.
pub fn plan_open(
    g: &Game,
    pid: PlayerId,
    to: i64,
    message: &Value,
    give: Option<&Value>,
    receive: Option<&Value>,
) -> Result<OpenPlan, ActionError> {
    let (to, text) = check_open(g, pid, to, message)?;
    let proposal = make_proposal(g, pid, to, give, receive)?;
    finish_open(g, pid, to, text, proposal)
}

/// [`plan_open`] with typed items: `give` what `pid` would give, `receive` what it would get,
/// each checked as reading it written would ([`proposal_of`]).
///
/// # Errors
/// Why it may not.
pub fn plan_open_terms(
    g: &Game,
    pid: PlayerId,
    to: PlayerId,
    message: &str,
    give: &[DealItem],
    receive: &[DealItem],
) -> Result<OpenPlan, ActionError> {
    let (to, text) = check_open(g, pid, i64::from(to.0), &Value::from(message))?;
    let proposal = proposal_of(g, pid, to, give, receive)?;
    finish_open(g, pid, to, text, proposal)
}

/// What opening a negotiation checks before its proposal: the partner, the message, no chat
/// of the two still open and the ruleset's number per turn. The partner, and the message as it
/// is kept.
fn check_open(
    g: &Game,
    pid: PlayerId,
    to: i64,
    message: &Value,
) -> Result<(PlayerId, String), ActionError> {
    let partner = u8::try_from(to)
        .ok()
        .map(PlayerId)
        .filter(|&t| t != pid && g.player(t).is_some_and(|p| p.is_major() && p.alive()));
    let Some(to) = partner else {
        return Err(refuse(
            "Invalid negotiation partner (negotiations are between major civilizations; use the \
             city-state tools for city-states).",
        ));
    };
    if !g.has_met(pid, to) {
        return Err(refuse(format!("You have not met {}.", name(g, to))));
    }
    let text = py::strip(&py::str_of(message)).to_owned();
    if !py::truthy(message) || text.is_empty() {
        return Err(refuse(
            "Open a negotiation with a message: every entry in a negotiation carries one.",
        ));
    }
    let negs = &g.state().diplo().negotiations;
    let between = |n: &Negotiation| {
        (n.initiator == pid && n.responder == to) || (n.initiator == to && n.responder == pid)
    };
    if let Some(n) = negs.iter().find(|n| n.status == NegStatus::Open && between(n)) {
        return Err(refuse(format!(
            "There is already an open negotiation with {} (id {}).",
            name(g, to),
            n.id.get()
        )));
    }
    let turn = g.turn();
    let per_turn =
        negs.iter().filter(|n| n.turn == turn && n.initiator == pid && n.responder == to).count();
    let most = g.rules().constants().diplomacy.negotiations_per_pair_per_turn;
    if u32::try_from(per_turn).unwrap_or(u32::MAX) >= most {
        return Err(refuse(format!(
            "You have already opened {per_turn} negotiations with {} this turn.",
            name(g, to)
        )));
    }
    Ok((to, truncate_chars(&text, MESSAGE_LIMIT).to_owned()))
}

/// A negotiation's proposal checked: `pid` can give what it would give.
fn finish_open(
    g: &Game,
    pid: PlayerId,
    to: PlayerId,
    text: String,
    proposal: Option<Terms>,
) -> Result<OpenPlan, ActionError> {
    if let Some(t) = &proposal {
        validate_items(g, pid, to, t.gives(pid), t)?;
    }
    Ok(OpenPlan { to, text, proposal })
}

/// Opens a negotiation [`plan_open`] checked, which awaits the other side, and tells both
/// (`diplomacy.py:780-792`).
pub fn open(g: &mut Game, pid: PlayerId, plan: OpenPlan) -> Value {
    let OpenPlan { to, text, proposal } = plan;
    let Some(id) = g.next_negotiation_id() else {
        return json!({"status": "refused"});
    };
    let turn = g.turn();
    g.edit_diplo(DiploTouch::NEGOTIATIONS).negotiations.push(Negotiation {
        id,
        initiator: pid,
        responder: to,
        turn,
        status: NegStatus::Open,
        awaiting: Some(to),
        proposal: proposal.clone(),
        proposal_by: proposal.as_ref().map(|_| pid),
        history: Vec::new(),
        deal: None,
    });
    add_entry(g, id, Some(pid), NegAction::Open, &text, proposal.clone(), None);
    add_message(g, pid, &[to], &text);
    let mut out = format!(
        "{} opened negotiations with {}: \"{}\"",
        name(g, pid),
        name(g, to),
        truncate_chars(&text, QUOTE_LIMIT)
    );
    if let Some(t) = &proposal {
        out.push_str(&proposal_text(g, "Proposal", t, pid, to));
    }
    let data = EventData { negotiation: Some(id), awaiting: Some(to), ..EventData::default() };
    g.emit(EngineEvent::Negotiation, &out, Some(pair(pid, to)), None, data, &[]);
    json!({"negotiation_id": id.get(), "status": "open", "awaiting": name(g, to)})
}

/// A response, as `respond_negotiation` names them (`RESPONSE_ACTIONS`, `diplomacy.py:46`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Response {
    Accept,
    Counter,
    Reject,
    Reply,
}

impl Response {
    /// Its name: `counter`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Counter => "counter",
            Self::Reject => "reject",
            Self::Reply => "reply",
        }
    }

    /// The response an action names, in any case, with Python's aliases for `reject`
    /// (`ACTION_ALIASES`, `diplomacy.py:47`).
    #[must_use]
    pub fn read(action: &str) -> Option<Self> {
        match action.to_lowercase().as_str() {
            "accept" => Some(Self::Accept),
            "counter" => Some(Self::Counter),
            "reject" | "decline" | "withdraw" | "end" => Some(Self::Reject),
            "reply" => Some(Self::Reply),
            _ => None,
        }
    }

    const fn action(self) -> NegAction {
        match self {
            Self::Accept => NegAction::Accept,
            Self::Counter => NegAction::Counter,
            Self::Reject => NegAction::Reject,
            Self::Reply => NegAction::Reply,
        }
    }
}

/// A response checked, for [`respond`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RespondPlan {
    /// The proposal on the table becomes a deal.
    Accept { nid: NegotiationId, other: PlayerId, text: String, terms: Terms },
    /// The negotiation ends.
    Reject { nid: NegotiationId, other: PlayerId, text: String },
    /// A reply, or a counter-offer with its proposal, and the other side's move.
    Deliver {
        nid: NegotiationId,
        other: PlayerId,
        text: String,
        response: Response,
        proposal: Option<Terms>,
    },
    /// The message would pass the cap: the negotiation closes instead, and it is not delivered.
    Cap { nid: NegotiationId, response: Response, cap: u32 },
}

/// Checks `pid`'s response in negotiation `nid` (`respond_negotiation`, `diplomacy.py:803-859`):
/// an open negotiation it is part of, a response it may make now, with a message; an accepted
/// proposal that is not its own, whose deal both sides can carry out; a counter-offer with at
/// least one item it can give.
///
/// # Errors
/// Why it may not.
pub fn plan_respond(
    g: &Game,
    pid: PlayerId,
    nid: i64,
    action: &Value,
    message: Option<&Value>,
    give: Option<&Value>,
    receive: Option<&Value>,
) -> Result<RespondPlan, ActionError> {
    let n = get(g, nid)?;
    check_party(n, pid)?;
    let act =
        if py::truthy(action) { py::strip(&py::str_of(action)).to_owned() } else { String::new() };
    let Some(response) = Response::read(&act) else {
        return Err(refuse(
            "action must be one of: accept, counter, reject, reply (decline, withdraw and end \
             also mean reject).",
        ));
    };
    let raw = message.filter(|m| py::truthy(m)).map(py::str_of).unwrap_or_default();
    check_response(g, pid, n, response, &raw, |other| {
        // Python's `give or []`: what is not given, or is empty, is no items.
        let empty = Value::Array(Vec::new());
        let or_empty = |v: Option<&Value>| match v {
            Some(v) if py::truthy(v) => v.clone(),
            _ => empty.clone(),
        };
        let (give, receive) = (or_empty(give), or_empty(receive));
        make_proposal(g, pid, other, Some(&give), Some(&receive))
    })
}

/// [`plan_respond`] with the response and a counter-offer's items typed: `give` what `pid` would
/// give and `receive` what it would get, read only for a counter-offer and checked as reading
/// them written would ([`proposal_of`]).
///
/// # Errors
/// Why it may not.
pub fn plan_respond_terms(
    g: &Game,
    pid: PlayerId,
    nid: NegotiationId,
    response: Response,
    message: &str,
    give: &[DealItem],
    receive: &[DealItem],
) -> Result<RespondPlan, ActionError> {
    let n = get(g, i64::from(nid.get()))?;
    check_party(n, pid)?;
    check_response(g, pid, n, response, message, |other| proposal_of(g, pid, other, give, receive))
}

/// A response may be made only in an open negotiation, by one of its parties.
fn check_party(n: &Negotiation, pid: PlayerId) -> Result<(), ActionError> {
    let id = n.id.get();
    if n.status != NegStatus::Open {
        return Err(refuse(format!("Negotiation #{id} is {}.", n.status.name())));
    }
    if pid != n.initiator && pid != n.responder {
        return Err(refuse(format!("You are not part of negotiation #{id}.")));
    }
    Ok(())
}

/// The rest of [`plan_respond`], once the response is read: whose move it is, the message, and
/// what the response needs. `counter` builds a counter-offer's proposal toward the other side.
fn check_response(
    g: &Game,
    pid: PlayerId,
    n: &Negotiation,
    response: Response,
    raw: &str,
    counter: impl FnOnce(PlayerId) -> Result<Option<Terms>, ActionError>,
) -> Result<RespondPlan, ActionError> {
    let id = n.id.get();
    let other = other_side(n, pid);
    if n.awaiting != Some(pid) && response != Response::Reject {
        let whose = n.awaiting.map(|p| name(g, p)).unwrap_or_default();
        return Err(refuse(format!(
            "It is {whose}'s move in negotiation #{id}; wait for their reply, or withdraw it with \
             action 'reject'."
        )));
    }
    let text = truncate_chars(py::strip(raw), MESSAGE_LIMIT).to_owned();
    if text.is_empty() {
        return Err(refuse(format!(
            "Every response in a negotiation carries a message, and your {} in negotiation \
             #{id} has none: add message='...' (a short line will do).",
            response.name()
        )));
    }
    match response {
        Response::Accept => {
            let Some(terms) = n.proposal.clone() else {
                return Err(refuse(format!(
                    "There is no proposal on the table in negotiation #{id} to accept. Use \
                     'reply' or 'counter'."
                )));
            };
            if n.proposal_by == Some(pid) {
                return Err(refuse(
                    "You cannot accept your own proposal; wait for the other side.",
                ));
            }
            plan_deal(g, n.initiator, n.responder, &terms)?;
            Ok(RespondPlan::Accept { nid: n.id, other, text, terms })
        }
        Response::Reject => Ok(RespondPlan::Reject { nid: n.id, other, text }),
        Response::Counter | Response::Reply => {
            let mut proposal = None;
            if response == Response::Counter {
                let Some(t) = counter(other)? else {
                    return Err(refuse(
                        "A counter-offer needs at least one item; use reply to send only a \
                         message.",
                    ));
                };
                validate_items(g, pid, other, t.gives(pid), &t)?;
                proposal = Some(t);
            }
            let cap = max_chat_messages(g);
            if u32::try_from(n.history.len()).unwrap_or(u32::MAX) >= cap {
                return Ok(RespondPlan::Cap { nid: n.id, response, cap });
            }
            Ok(RespondPlan::Deliver { nid: n.id, other, text, response, proposal })
        }
    }
}

/// Carries out a response [`plan_respond`] checked, and tells both sides
/// (`diplomacy.py:822-871`).
pub fn respond(g: &mut Game, pid: PlayerId, plan: RespondPlan) -> Value {
    let me = name(g, pid);
    match plan {
        RespondPlan::Accept { nid, other, text, terms } => {
            let parties = g.state().diplo().negotiation(nid).map(|n| (n.initiator, n.responder));
            let Some((a, b)) = parties else { return json!({"status": "refused"}) };
            let deal = execute_deal(g, a, b, &terms);
            let summary = |g: &Game| {
                deal.and_then(|d| g.state().diplo().deal(d)).map(|d| d.summary.to_string())
            };
            let now = g.state().diplo().negotiation(nid).map(|n| n.status);
            if let Some(status) = now.filter(|&s| s != NegStatus::Open) {
                // What the deal set off closed the chat (a war between its parties, which
                // `validate_items` refuses): the chat keeps the close the game gave it, and no
                // entry passes it.
                edit(g, nid, |n| n.deal = deal);
                return json!({"status": status.name(), "deal": summary(g)});
            }
            edit(g, nid, |n| {
                n.status = NegStatus::Accepted;
                n.deal = deal;
                n.awaiting = None;
            });
            add_entry(g, nid, Some(pid), NegAction::Accept, &text, None, None);
            add_message(g, pid, &[other], &text);
            let out = format!("{me} accepted the deal. \"{}\"", truncate_chars(&text, QUOTE_LIMIT));
            let data = EventData {
                negotiation: Some(nid),
                status: Some(NegStatus::Accepted),
                ..EventData::default()
            };
            g.emit(EngineEvent::Negotiation, &out, Some(pair(pid, other)), None, data, &[]);
            json!({"status": "accepted", "deal": summary(g)})
        }
        RespondPlan::Reject { nid, other, text } => {
            edit(g, nid, |n| {
                n.status = NegStatus::Rejected;
                n.awaiting = None;
            });
            add_entry(g, nid, Some(pid), NegAction::Reject, &text, None, None);
            add_message(g, pid, &[other], &text);
            let out =
                format!("{me} ended the negotiation. \"{}\"", truncate_chars(&text, QUOTE_LIMIT));
            let data = EventData {
                negotiation: Some(nid),
                status: Some(NegStatus::Rejected),
                ..EventData::default()
            };
            g.emit(EngineEvent::Negotiation, &out, Some(pair(pid, other)), None, data, &[]);
            json!({"status": "rejected"})
        }
        RespondPlan::Cap { nid, response, cap } => {
            // The safety cap: a chat that has run this long without a deal is not converging.
            // The message that would pass the cap closes it instead of joining it.
            let note = format!("The negotiation reached its limit of {cap} messages and closed.");
            close(g, nid, NegStatus::Expired, &note, None);
            json!({
                "status": "expired",
                "awaiting": null,
                "note": format!(
                    "Negotiation #{} already held {cap} messages, its limit, so it has closed and \
                     your {} was not delivered.",
                    nid.get(),
                    response.name()
                ),
            })
        }
        RespondPlan::Deliver { nid, other, text, response, proposal } => {
            if let Some(t) = &proposal {
                let t = t.clone();
                edit(g, nid, |n| {
                    n.proposal = Some(t);
                    n.proposal_by = Some(pid);
                });
            }
            add_entry(g, nid, Some(pid), response.action(), &text, proposal.clone(), None);
            edit(g, nid, |n| n.awaiting = Some(other));
            add_message(g, pid, &[other], &text);
            let verb = if response == Response::Counter { "countered" } else { "replied" };
            let mut out = format!("{me} {verb}: \"{}\"", truncate_chars(&text, QUOTE_LIMIT));
            if let Some(t) = &proposal {
                out.push_str(&proposal_text(g, "New proposal", t, pid, other));
            }
            let data =
                EventData { negotiation: Some(nid), awaiting: Some(other), ..EventData::default() };
            g.emit(EngineEvent::Negotiation, &out, Some(pair(pid, other)), None, data, &[]);
            json!({"status": "open", "awaiting": name(g, other)})
        }
    }
}

/// The statuses a negotiation may be closed with from outside (`CLOSED_STATUSES`,
/// `diplomacy.py:701`).
pub const CLOSED_STATUSES: [NegStatus; 3] =
    [NegStatus::Rejected, NegStatus::Expired, NegStatus::Cancelled];

/// The refusal of a status a negotiation cannot be closed with from outside.
fn not_a_close(name: &str) -> ActionError {
    refuse(format!("A negotiation closes as rejected, expired, cancelled, not '{name}'."))
}

/// The status named `name`, if a negotiation may be closed with it from outside
/// (`close_negotiation`, `diplomacy.py:882-883`).
///
/// # Errors
/// A name that is no status, or not a closed one.
pub fn close_status(name: &str) -> Result<NegStatus, ActionError> {
    NegStatus::from_name(name)
        .filter(|s| CLOSED_STATUSES.contains(s))
        .ok_or_else(|| not_a_close(name))
}

/// Checks that negotiation `nid` may be closed from outside with `status` (`close_negotiation`,
/// `diplomacy.py:882-886`).
///
/// # Errors
/// A status that is not a closed one, no such negotiation, or one not open.
pub fn plan_close(g: &Game, nid: NegotiationId, status: NegStatus) -> Result<(), ActionError> {
    if !CLOSED_STATUSES.contains(&status) {
        return Err(not_a_close(status.name()));
    }
    let n = get(g, i64::from(nid.get()))?;
    if n.status != NegStatus::Open {
        return Err(refuse(format!("Negotiation #{} is already {}.", n.id.get(), n.status.name())));
    }
    Ok(())
}

/// Closes an open negotiation from outside the conversation (`close_negotiation`,
/// `diplomacy.py:887-895`): a timeout, a war, a forced close. The history gets an entry with the
/// note, cut to 500 code points, and both sides are told. `by` is the player it is closed for,
/// if any.
pub fn close(
    g: &mut Game,
    nid: NegotiationId,
    status: NegStatus,
    note: &str,
    by: Option<PlayerId>,
) {
    let Some((a, b)) = g.state().diplo().negotiation(nid).map(|n| (n.initiator, n.responder))
    else {
        return;
    };
    edit(g, nid, |n| {
        n.status = status;
        n.awaiting = None;
    });
    let note = truncate_chars(note, NOTE_LIMIT);
    add_entry(g, nid, by, NegAction::Close, "", None, Some(note));
    let verb = match status {
        NegStatus::Rejected => "was declined",
        NegStatus::Expired => "expired",
        _ => "was cancelled",
    };
    let text = format!(
        "Negotiation #{} between {} and {} {verb}. {note}",
        nid.get(),
        name(g, a),
        name(g, b)
    );
    let data = EventData { negotiation: Some(nid), status: Some(status), ..EventData::default() };
    g.emit(EngineEvent::Negotiation, py::strip(&text), Some(pair(a, b)), None, data, &[]);
}

/// The open negotiations matching `f`, in order.
fn open_where(g: &Game, f: impl Fn(&Negotiation) -> bool) -> Vec<NegotiationId> {
    g.state()
        .diplo()
        .negotiations
        .iter()
        .filter(|n| n.status == NegStatus::Open && f(n))
        .map(|n| n.id)
        .collect()
}

/// A war between `a` and `b` cancels their open negotiations (`diplomacy.py:212-214`).
pub(crate) fn cancel_between(g: &mut Game, a: PlayerId, b: PlayerId) {
    let ids = open_where(g, |n| {
        (n.initiator == a && n.responder == b) || (n.initiator == b && n.responder == a)
    });
    if ids.is_empty() {
        return;
    }
    let note = format!("{} declared war on {}.", name(g, a), name(g, b));
    for id in ids {
        close(g, id, NegStatus::Cancelled, &note, None);
    }
}

/// Closes the negotiations `pid` opened that are still open at the end of its turn (stage E0,
/// `expire_negotiations`, `diplomacy.py:898-906`): a safety net for hosts that end turns
/// directly, since the `end_turn` action refuses while one is open.
pub(crate) fn expire_for(g: &mut Game, pid: PlayerId) {
    let ids = open_where(g, |n| n.initiator == pid);
    if ids.is_empty() {
        return;
    }
    let note = format!("It was still open at the end of {}'s turn.", name(g, pid));
    for id in ids {
        close(g, id, NegStatus::Expired, &note, None);
    }
}

/// Stage E0: a major's negotiations expire.
pub(crate) fn expire_stage(g: &mut Game, pid: PlayerId) {
    expire_for(g, pid);
}

/// Why an open negotiation stops `pid` ending its turn, or `None` (`end_turn_refusal`,
/// `diplomacy.py:909-929`). One waiting on `pid` must be answered first, and is named first,
/// since that one it can act on; one waiting on the other side must get its reply, or be
/// withdrawn.
#[must_use]
pub fn end_turn_refusal(g: &Game, pid: PlayerId) -> Option<String> {
    let mut waiting = None;
    for n in &g.state().diplo().negotiations {
        if n.status != NegStatus::Open || (pid != n.initiator && pid != n.responder) {
            continue;
        }
        let who = name(g, other_side(n, pid));
        let id = n.id.get();
        if n.awaiting == Some(pid) {
            return Some(format!(
                "Answer {who} in negotiation #{id} first: respond_negotiation(negotiation_id={id}, \
                 action='accept', 'counter', 'reply' or 'reject', message=...). You cannot end \
                 your turn while a negotiation waits on you."
            ));
        }
        if waiting.is_none() {
            waiting = Some(format!(
                "You are waiting for {who} to answer negotiation #{id}. End your turn after they \
                 reply, or withdraw it with respond_negotiation(negotiation_id={id}, \
                 action='reject', message=...)."
            ));
        }
    }
    waiting
}

/// A proposal's items as Python's dicts.
fn items_json(g: &Game, t: &Terms, p: PlayerId) -> Value {
    Value::Array(t.gives(p).iter().filter_map(|i| i.to_json(g.rules())).collect())
}

/// A negotiation as it is kept: `id`, `initiator`, `responder`, `turn`, `status`, `awaiting`,
/// `proposal` (Python's dict by player id), `proposal_by`, `history` (`seq`, `by`, `action`,
/// `message`, `proposal`, `turn`, `note`) and `deal_id`.
#[must_use]
pub fn negotiation_json(g: &Game, n: &Negotiation) -> Value {
    let terms = |t: &Option<Terms>| t.as_ref().and_then(|t| t.to_json(g.rules()));
    let history: Vec<Value> = n
        .history
        .iter()
        .map(|h| {
            json!({
                "seq": h.seq, "by": h.by.map(|p| p.0), "action": h.action.name(),
                "message": &*h.message, "proposal": terms(&h.proposal), "turn": h.turn,
                "note": h.note.as_deref(),
            })
        })
        .collect();
    json!({
        "id": n.id.get(), "initiator": n.initiator.0, "responder": n.responder.0, "turn": n.turn,
        "status": n.status.name(), "awaiting": n.awaiting.map(|p| p.0),
        "proposal": terms(&n.proposal), "proposal_by": n.proposal_by.map(|p| p.0),
        "history": history, "deal_id": n.deal.map(crate::base::ids::DealId::get),
    })
}

/// A negotiation as `pid` sees it, the proposal in its own terms (`negotiation_view`,
/// `diplomacy.py:932-963`): what it would give and receive, since the other formulation is a
/// reliable way to accept the opposite of what was meant. The entries the game added to close
/// the chat are not messages, so an open chat never shows more than the cap.
#[must_use]
pub fn negotiation_view(g: &Game, n: &Negotiation, pid: PlayerId) -> Value {
    let other = other_side(n, pid);
    let persp = |t: &Option<Terms>| match t {
        None => Value::Null,
        Some(t) => json!({
            "you_give": items_json(g, t, pid),
            "you_receive": items_json(g, t, other),
            "summary": format!(
                "You give {}; you receive {}.",
                describe_items(g, t.gives(pid)),
                describe_items(g, t.gives(other))
            ),
        }),
    };
    let history: Vec<Value> = n
        .history
        .iter()
        .map(|h| {
            let mut e = json!({
                "seq": h.seq, "by": h.by.map(|p| name(g, p)), "you": h.by == Some(pid),
                "action": h.action.name(), "message": &*h.message, "proposal": persp(&h.proposal),
            });
            if let (Some(note), Some(m)) = (&h.note, e.as_object_mut()) {
                m.insert("note".to_owned(), Value::from(&**note));
            }
            e
        })
        .collect();
    let messages = n.history.iter().filter(|h| h.action != NegAction::Close).count();
    json!({
        "id": n.id.get(), "with": other.0, "with_name": name(g, other), "status": n.status.name(),
        "you_initiated": n.initiator == pid, "your_move": n.awaiting == Some(pid), "turn": n.turn,
        "messages": messages, "max_messages": max_chat_messages(g),
        "current_proposal": persp(&n.proposal),
        "proposal_by_you": n.proposal.as_ref().map(|_| n.proposal_by == Some(pid)),
        "history": history,
    })
}

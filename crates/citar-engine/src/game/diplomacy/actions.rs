//! The diplomacy tools as typed actions (DESIGN.md 8.3): `send_message`, `open_negotiation`,
//! `respond_negotiation`, `declare_war`, `denounce` (`tools.py:931-995`) and `end_turn`
//! (`tools.py:1092-1109`), whose check holds the chat rule: no one ends a turn while a
//! negotiation it is in is open (phase0-spec A1.5). `Game::end_turn`, which hosts call directly,
//! keeps Python's safety net of expiring the negotiations the player opened.
//!
//! Each checks on `&Game` everything Python refused, with Python's messages, before anything is
//! written. Arguments whose schema takes more than one type (`to`) or that Python read with `str`
//! (`message`, `action`, deal items) are kept as JSON and read as Python read them.

use serde_json::{Value, json};

use super::negotiation::{
    MessagePlan, OpenPlan, RespondPlan, end_turn_refusal, open, plan_open, plan_respond,
    plan_send_message, respond, send_message,
};
use super::relations::{declare_war, denounce, plan_declare_war, plan_denounce};
use crate::base::ids::PlayerId;
use crate::base::py;
use crate::game::Game;
use crate::game::action::{OutcomeSpec, Rule};
use crate::game::error::{ActionError, ErrCode};
use crate::state::Phase;

/// `send_message`: free text to a civilization met, a list of them, or `"all"` met; nothing
/// said is binding (`tools.send_message`, `diplomacy.send_message`). Any time.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SendMessage {
    /// A player id, a list of them, or `"all"`.
    pub to: Value,
    pub text: Value,
}

impl Rule for SendMessage {
    type Plan = MessagePlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<MessagePlan, ActionError> {
        plan_send_message(g, pid, &self.to, &self.text)
    }

    fn apply(self, g: &mut Game, pid: PlayerId, plan: MessagePlan) -> OutcomeSpec {
        OutcomeSpec::value(send_message(g, pid, &plan))
    }
}

/// `open_negotiation`: a message and an optional proposal to a civilization met, on the
/// player's turn (`tools.open_negotiation`, `diplomacy.open_negotiation`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpenNegotiation {
    pub to: i64,
    pub message: Value,
    /// What the opener gives: deal items, as the caller writes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub give: Option<Value>,
    /// What the opener would receive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receive: Option<Value>,
}

impl Rule for OpenNegotiation {
    type Plan = OpenPlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<OpenPlan, ActionError> {
        plan_open(g, pid, self.to, &self.message, self.give.as_ref(), self.receive.as_ref())
    }

    fn apply(self, g: &mut Game, pid: PlayerId, plan: OpenPlan) -> OutcomeSpec {
        OutcomeSpec::value(open(g, pid, plan))
    }
}

/// `respond_negotiation`: accept, counter, reply or reject in a negotiation, with a message
/// every time (`tools.respond_negotiation`, `diplomacy.respond_negotiation`). Any time: a
/// negotiation opened on another's turn waits on the caller.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RespondNegotiation {
    pub negotiation_id: i64,
    pub action: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub give: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receive: Option<Value>,
}

impl Rule for RespondNegotiation {
    type Plan = RespondPlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<RespondPlan, ActionError> {
        plan_respond(
            g,
            pid,
            self.negotiation_id,
            &self.action,
            self.message.as_ref(),
            self.give.as_ref(),
            self.receive.as_ref(),
        )
    }

    fn apply(self, g: &mut Game, pid: PlayerId, plan: RespondPlan) -> OutcomeSpec {
        OutcomeSpec::value(respond(g, pid, plan))
    }
}

/// `declare_war`: war on a civilization or city-state met, with a message if the declarer has
/// one (`tools.declare_war`, `diplomacy.declare_war`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeclareWar {
    pub player_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
}

impl Rule for DeclareWar {
    type Plan = PlayerId;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<PlayerId, ActionError> {
        plan_declare_war(g, pid, self.player_id)
    }

    fn apply(self, g: &mut Game, pid: PlayerId, target: PlayerId) -> OutcomeSpec {
        let message = self.message.as_ref().filter(|m| py::truthy(m)).map(py::str_of);
        OutcomeSpec::value(declare_war(g, pid, target, message.as_deref()))
    }
}

/// `denounce`: publicly denounce a civilization met (`tools.denounce`, `diplomacy.denounce`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Denounce {
    pub player_id: i64,
}

impl Rule for Denounce {
    type Plan = PlayerId;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<PlayerId, ActionError> {
        plan_denounce(g, pid, self.player_id)
    }

    fn apply(self, g: &mut Game, pid: PlayerId, target: PlayerId) -> OutcomeSpec {
        OutcomeSpec::value(denounce(g, pid, target))
    }
}

/// `end_turn`: the player's turn ends, and play moves on to the next major civilization's
/// (`tools.end_turn`, `tools.py:1092-1109`). Refused while a negotiation the player is in is
/// open, whichever side it waits on, and while a seat's driver plays, whose turn `drive` ends.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndTurn {}

impl Rule for EndTurn {
    type Plan = ();

    fn check(&self, g: &Game, pid: PlayerId) -> Result<(), ActionError> {
        g.ensure_not_driving()?;
        match end_turn_refusal(g, pid) {
            Some(why) => Err(ActionError::new(ErrCode::Negotiation, why)),
            None => Ok(()),
        }
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (): ()) -> OutcomeSpec {
        let ended = g.end_turn_now(pid);
        debug_assert!(ended.is_ok(), "the guard let the player end its turn: {ended:?}");
        OutcomeSpec::render(|g| {
            let next = g.player(g.current()).map(|p| p.name.to_string());
            let phase = match g.phase() {
                Phase::Playing => "playing",
                Phase::Over => "over",
            };
            json!({"ended": true, "next_player": next, "turn": g.turn(), "phase": phase})
        })
    }
}

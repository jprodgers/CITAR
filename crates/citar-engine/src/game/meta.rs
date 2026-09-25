//! The tools a seat keeps for itself (`tools.py:900-925, 1057-1087`): `set_civ_name`, a name of
//! its own for its civilization and leader; `write_notes`, its private notebook, which the
//! briefing shows it; and `log_thought`, its reasoning for spectators and the replay. Each may be
//! used at any time.
//!
//! A thought is host activity: kept in the chronicle, counted in the host heads and never
//! digested, as `Game::add_thought` keeps the host's own. Python also handed it to the session's
//! listeners as a pseudo-event; a host reads it with `Game::thoughts` instead (DESIGN.md 8.1).

use serde_json::{Value, json};

use super::Game;
use super::action::{OutcomeSpec, Rule};
use super::cities::founding::clean_name;
use super::derive::rev::PlayerTouch;
use super::error::ActionError;
use super::events::Mention;
use crate::base::ids::PlayerId;
use crate::base::py;
use crate::base::text::truncate_chars;
use crate::state::chronicle::{EngineEvent, EventData};

/// The longest name a civilization or its leader may have, in characters (`tools.py:911`).
pub const NAME_LIMIT: usize = 48;

/// The most of its notebook a seat keeps, in characters: the end, since notes grow by appending
/// and the recent half is the useful one (`tools.py:1066-1073`).
pub const NOTES_LIMIT: usize = 8000;

/// The longest thought kept, in characters (`tools.py:1084`).
pub const THOUGHT_LIMIT: usize = 4000;

/// A player's text as Python's tools read it: `str(x or "")`.
fn text(v: &Value) -> String {
    if py::truthy(v) { py::str_of(v) } else { String::new() }
}

/// `set_civ_name`: names the civilization, and its leader if one is given (`tools.set_civ_name`,
/// `tools.py:900-925`). Cosmetic: the civilization's bonuses do not change. Names stay unique,
/// whatever their case, or every message would be ambiguous; a name that changes nothing is
/// refused, as renaming a city to itself is.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetCivName {
    pub name: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<Value>,
}

impl Rule for SetCivName {
    /// The new name, and the new leader's name if one was given.
    type Plan = (String, Option<String>);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let name = clean_name(&text(&self.name), NAME_LIMIT);
        if name.is_empty() {
            return Err(ActionError::rule("Name cannot be empty (plain text only)."));
        }
        let lower = name.to_lowercase();
        let taken =
            g.state().players().iter().any(|(id, p)| id != pid && p.name.to_lowercase() == lower);
        if taken {
            return Err(ActionError::rule("Another civilization already uses that name."));
        }
        let leader = self
            .leader
            .as_ref()
            .filter(|v| py::truthy(v))
            .map(|v| clean_name(&text(v), NAME_LIMIT));
        let p = g.player(pid).ok_or_else(|| ActionError::rule("Invalid player."))?;
        if *p.name == *name && leader.as_ref().is_none_or(|l| **l == *p.leader) {
            return Err(ActionError::rule(format!("Your civilization is already named {name}.")));
        }
        Ok((name, leader))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (name, leader): Self::Plan) -> OutcomeSpec {
        let Some(old) = g.player(pid).map(|p| p.name.to_string()) else {
            return OutcomeSpec::value(Value::Null);
        };
        if let Some(p) = g.player_mut(pid, PlayerTouch::NAME) {
            p.name = name.as_str().into();
            if let Some(l) = leader {
                p.leader = l.into();
            }
        }
        let now = g.player(pid).map(|p| p.leader.to_string()).unwrap_or_default();
        if old != name {
            let led = if now.is_empty() { String::new() } else { format!(", led by {now}") };
            let data = EventData { player: Some(pid), ..EventData::default() };
            g.emit(
                EngineEvent::CivRenamed,
                &format!("{old} is now known as {name}{led}."),
                None,
                None,
                data,
                &[Mention::civ(&old, pid)],
            );
        }
        OutcomeSpec::value(json!({"name": name, "leader": now}))
    }
}

/// `write_notes`: writes to the seat's private notebook, replacing it or, with mode `append`,
/// adding a line to it (`tools.write_notes`, `tools.py:1057-1073`). It keeps the last
/// [`NOTES_LIMIT`] characters. The notebook is the only memory a model has that is not rebuilt
/// from the game each turn, and the briefing shows it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WriteNotes {
    pub text: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Value>,
}

impl Rule for WriteNotes {
    /// The notebook as it will be.
    type Plan = String;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let major = g.player(pid).and_then(|p| p.major.as_deref());
        let old = major.map_or("", |m| &*m.notes);
        let new = py::str_of(&self.text);
        let append = self.mode.as_ref().is_some_and(|m| m.as_str() == Some("append"));
        let notes = if append { py::strip(&format!("{old}\n{new}")).to_owned() } else { new };
        let n = notes.chars().count();
        Ok(notes.chars().skip(n.saturating_sub(NOTES_LIMIT)).collect())
    }

    fn apply(self, g: &mut Game, pid: PlayerId, notes: Self::Plan) -> OutcomeSpec {
        let length = notes.chars().count();
        if let Some(m) = g.player_mut(pid, PlayerTouch::OTHER).and_then(|p| p.major.as_deref_mut())
        {
            m.notes = notes.into();
        }
        OutcomeSpec::value(json!({"notes_length": length}))
    }
}

/// `log_thought`: records the seat's reasoning for this turn, for spectators and the replay and
/// never for other players (`tools.log_thought`, `tools.py:1076-1087`), its first
/// [`THOUGHT_LIMIT`] characters.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LogThought {
    pub text: Value,
}

impl Rule for LogThought {
    type Plan = ();

    fn check(&self, _: &Game, _: PlayerId) -> Result<Self::Plan, ActionError> {
        Ok(())
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (): Self::Plan) -> OutcomeSpec {
        let t = py::str_of(&self.text);
        g.add_thought(pid, truncate_chars(&t, THOUGHT_LIMIT), None);
        OutcomeSpec::value(json!({"ok": true}))
    }
}

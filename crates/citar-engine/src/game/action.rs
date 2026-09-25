//! Typed actions, and the one pipeline every action runs through (DESIGN.md 8.3).
//!
//! Replaces the checks of `tools.execute` (`tools.py:84-130`) that come before a tool's own:
//! the player is a living major civilization, the game goes on, and it is the player's turn
//! unless the tool may be used at any time. An action then runs in four steps:
//! 1. **check**, on `&Game`: nothing can be written, so a refusal returns before anything
//!    changed. The digest is as it was, no event is emitted, and nothing settles (property P2);
//! 2. **apply**, infallible: what the check planned, written through `game::mutate`;
//! 3. **settle**: citizens, sight and what they reveal catch up (DESIGN.md 6.7);
//! 4. **render**: the result is read from the settled game, so a tool that reassigns citizens or
//!    reveals tiles reports what the caller will see (`tools.py:680-757`).
//!
//! [`Action`] is a serde enum tagged by tool name. Each system package adds the variants of its
//! tools, with their argument specs, as it ports their rules (DESIGN.md 3.4, rule 2); package
//! 1b-01 lands the pipeline. The JSON registry of tools comes in package 1d-01.

use super::Game;
use super::cities::borders::BuyTile;
use super::cities::citizens::{SetCityFocus, SetSpecialists, WorkTile};
use super::cities::purchase::Buy;
use super::cities::queue::{ChangeQueue, RenameCity, SetAutoProduction, SetProduction};
use super::error::{ActionError, ErrCode};
use super::events::EventBatch;
use super::great_people::ChooseGreatPerson;
use super::policies::AdoptPolicy;
use super::religion::found::FoundPantheon;
use super::research::{ChooseFreeTech, DequeueResearch, SetResearch};
use crate::base::ids::PlayerId;
use crate::save::journal::Record;
use crate::state::Phase;
use crate::state::chronicle::ActionRecord;

/// What an action reports: the tool's result, as the model reads it.
pub type Outcome = serde_json::Value;

/// How to report an applied action, read from the game once it has settled.
pub struct OutcomeSpec(Box<dyn FnOnce(&Game) -> Outcome>);

impl OutcomeSpec {
    /// A result known when the action applied.
    #[must_use]
    pub fn value(v: Outcome) -> Self {
        Self(Box::new(move |_| v))
    }

    /// A result read from the settled game: the worked tiles after the citizens moved, the
    /// tiles a move revealed.
    #[must_use]
    pub fn render(f: impl FnOnce(&Game) -> Outcome + 'static) -> Self {
        Self(Box::new(f))
    }

    fn finish(self, g: &Game) -> Outcome {
        (self.0)(g)
    }
}

impl core::fmt::Debug for OutcomeSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("OutcomeSpec")
    }
}

/// One tool's rule: a check that only reads, and an apply that cannot fail.
pub trait Rule {
    /// What the check found, for the apply to carry out.
    type Plan;

    /// Whether the rule allows it now. Reads only.
    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError>;

    /// Carries out what the check planned, and says how to report it.
    fn apply(self, g: &mut Game, pid: PlayerId, plan: Self::Plan) -> OutcomeSpec;
}

/// Checks, then applies: the part of the pipeline each action shares.
fn run<R: Rule>(g: &mut Game, pid: PlayerId, r: R) -> Result<OutcomeSpec, ActionError> {
    let plan = r.check(g, pid)?;
    Ok(r.apply(g, pid, plan))
}

/// An action a player takes, by tool name and argument names (DESIGN.md 8.3).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum Action {
    /// The pipeline's own test action.
    #[cfg(test)]
    Probe(tests::Probe),
    /// `adopt_policy` (package 1b-07).
    AdoptPolicy(AdoptPolicy),
    /// `buy` (package 1b-07).
    Buy(Buy),
    /// `buy_tile` (package 1b-07).
    BuyTile(BuyTile),
    /// `change_queue` (package 1b-07).
    ChangeQueue(ChangeQueue),
    /// `choose_free_tech` (package 1b-07).
    ChooseFreeTech(ChooseFreeTech),
    /// `choose_great_person` (package 1b-08).
    ChooseGreatPerson(ChooseGreatPerson),
    /// `dequeue_research` (package 1b-07).
    DequeueResearch(DequeueResearch),
    /// `found_pantheon` (package 1b-08).
    FoundPantheon(FoundPantheon),
    /// `rename_city` (package 1b-07).
    RenameCity(RenameCity),
    /// `set_auto_production` (package 1b-07).
    SetAutoProduction(SetAutoProduction),
    /// `set_city_focus` (package 1b-06).
    SetCityFocus(SetCityFocus),
    /// `set_production` (package 1b-07).
    SetProduction(SetProduction),
    /// `set_research` (package 1b-07).
    SetResearch(SetResearch),
    /// `set_specialists` (package 1b-06).
    SetSpecialists(SetSpecialists),
    /// `work_tile` (package 1b-06).
    WorkTile(WorkTile),
}

impl Action {
    /// The tool's name.
    #[must_use]
    pub const fn tool(&self) -> &'static str {
        match *self {
            #[cfg(test)]
            Self::Probe(_) => "probe",
            Self::AdoptPolicy(_) => "adopt_policy",
            Self::Buy(_) => "buy",
            Self::BuyTile(_) => "buy_tile",
            Self::ChangeQueue(_) => "change_queue",
            Self::ChooseFreeTech(_) => "choose_free_tech",
            Self::ChooseGreatPerson(_) => "choose_great_person",
            Self::DequeueResearch(_) => "dequeue_research",
            Self::FoundPantheon(_) => "found_pantheon",
            Self::RenameCity(_) => "rename_city",
            Self::SetAutoProduction(_) => "set_auto_production",
            Self::SetProduction(_) => "set_production",
            Self::SetResearch(_) => "set_research",
            Self::SetCityFocus(_) => "set_city_focus",
            Self::SetSpecialists(_) => "set_specialists",
            Self::WorkTile(_) => "work_tile",
        }
    }

    /// Whether the tool may be used outside the player's turn (`Tool.any_time`).
    #[must_use]
    pub const fn any_time(&self) -> bool {
        match *self {
            #[cfg(test)]
            Self::Probe(ref p) => p.any_time,
            // `tools.rename_city` is `any_time` (tools.py:782).
            Self::RenameCity(_) => true,
            Self::AdoptPolicy(_)
            | Self::Buy(_)
            | Self::BuyTile(_)
            | Self::ChangeQueue(_)
            | Self::ChooseFreeTech(_)
            | Self::ChooseGreatPerson(_)
            | Self::DequeueResearch(_)
            | Self::FoundPantheon(_)
            | Self::SetAutoProduction(_)
            | Self::SetProduction(_)
            | Self::SetResearch(_)
            | Self::SetCityFocus(_)
            | Self::SetSpecialists(_)
            | Self::WorkTile(_) => false,
        }
    }

    fn run(self, g: &mut Game, pid: PlayerId) -> Result<OutcomeSpec, ActionError> {
        match self {
            #[cfg(test)]
            Self::Probe(p) => run(g, pid, p),
            Self::AdoptPolicy(x) => run(g, pid, x),
            Self::Buy(x) => run(g, pid, x),
            Self::BuyTile(x) => run(g, pid, x),
            Self::ChangeQueue(x) => run(g, pid, x),
            Self::ChooseFreeTech(x) => run(g, pid, x),
            Self::ChooseGreatPerson(x) => run(g, pid, x),
            Self::DequeueResearch(x) => run(g, pid, x),
            Self::FoundPantheon(x) => run(g, pid, x),
            Self::RenameCity(x) => run(g, pid, x),
            Self::SetAutoProduction(x) => run(g, pid, x),
            Self::SetProduction(x) => run(g, pid, x),
            Self::SetResearch(x) => run(g, pid, x),
            Self::SetCityFocus(x) => run(g, pid, x),
            Self::SetSpecialists(x) => run(g, pid, x),
            Self::WorkTile(x) => run(g, pid, x),
        }
    }
}

impl Game {
    /// Takes an action for a player (DESIGN.md 8.3): the guard and the rule's check, which only
    /// read; then the apply, a settle, and the result read from the settled game, with the
    /// events the call appended.
    ///
    /// An action that succeeds is logged (`tools.py:129-130`), here and not in the JSON layer,
    /// since bots and drivers call `act` directly and Python logged theirs too.
    pub fn act(&mut self, pid: PlayerId, a: Action) -> Result<(Outcome, EventBatch), ActionError> {
        self.ensure_live()?;
        self.begin_call();
        self.guard(pid, a.any_time())?;
        let record = self.action_record(pid, &a);
        let spec = a.run(self, pid)?;
        self.settle();
        Record::of(&mut self.st, &mut self.chron).action(record);
        let out = spec.finish(self);
        Ok((out, self.take_batch()))
    }

    /// The log entry of an action (`tools.py:130`): the turn it was taken on, and its arguments
    /// as the tool took them, the action's fields without the tag that names the tool. Python
    /// logged the turn after the call, which for an `end_turn` that closed the round was the
    /// next one; the wall-clock time is the host's to add.
    fn action_record(&self, pid: PlayerId, a: &Action) -> ActionRecord {
        // An action's fields are plain data, so it always serialises; an empty object stands
        // in if one ever does not.
        let mut args = serde_json::to_value(a).unwrap_or_default();
        if let Some(fields) = args.as_object_mut() {
            fields.shift_remove("tool");
        }
        let args = if args.is_object() { args.to_string() } else { "{}".to_owned() };
        ActionRecord { turn: self.turn(), player: pid, tool: a.tool().into(), args: args.into() }
    }

    /// Whether `pid` may act now (`tools.py:105-112`): a major civilization of this game, alive,
    /// in a game that goes on, on its turn unless the tool may be used at any time.
    pub fn guard(&self, pid: PlayerId, any_time: bool) -> Result<(), ActionError> {
        self.ensure_live()?;
        let Some(p) = self.player(pid).filter(|p| p.is_major()) else {
            return Err(ActionError::new(ErrCode::InvalidPlayer, "Invalid player."));
        };
        if !p.alive() {
            return Err(ActionError::new(
                ErrCode::Eliminated,
                "Your civilization has been eliminated.",
            ));
        }
        if self.phase() != Phase::Playing {
            return Err(ActionError::new(ErrCode::GameOver, "The game is over."));
        }
        if !any_time && self.current() != pid {
            let whose = self.player(self.current()).map_or("another player", |p| &p.name);
            return Err(ActionError::new(
                ErrCode::NotYourTurn,
                format!("It is not your turn (it is {whose}'s turn)."),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

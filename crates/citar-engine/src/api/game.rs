//! Host methods on [`Game`] (DESIGN.md 8.1): the commands a host calls besides the tools, each
//! of which settles and returns the events it appended.
//!
//! Package 1b-02 lands the scenario and seat commands: `apply_ops` (`scenario.apply_ops`,
//! `scenario.py:471-487`), `meet` (`engine_api.meet`), `set_controller` and `set_difficulty`
//! (`engine_api.py:648-667`). Package 1b-03 adds `end_turn` (`Game.end_turn`,
//! `game.py:1012-1037`) and `force_turn` (`engine_api.force_turn`); `Game::new`,
//! `Game::config_from_json` and `Game::drive` are in `game::setup` and `game::turn::drive`. The
//! rest land with the packages that port what they do.

use serde_json::Value;

use super::scenario;
use crate::base::ids::PlayerId;
use crate::game::Game;
use crate::game::error::{ActionError, EngineError, ErrCode};
use crate::game::events::EventBatch;
use crate::state::players::{AutoOverrides, Controller, Handicap};

impl Game {
    /// Applies scenario operations in order, all or nothing (DESIGN.md 8.1), and returns what
    /// each said. Python applied them until the first failure and left the earlier ones applied,
    /// without the visibility refresh at the end (`scenario.py:471-487`); here the game is
    /// cloned first, and put back when one fails, so an editor never sees a game half edited
    /// and unsettled. The error names the operation: "Operation 3 (set_tile): ...".
    pub fn apply_ops(&mut self, ops: &Value) -> Result<(Vec<Value>, EventBatch), ActionError> {
        self.ensure_live()?;
        // refcheck: atomic-apply-ops
        let before = self.clone();
        self.begin_call();
        match scenario::apply(self, ops) {
            Ok(results) => {
                self.settle();
                Ok((results, self.take_batch()))
            }
            Err(e) => {
                *self = before;
                Err(e)
            }
        }
    }

    /// Ends `pid`'s turn and plays on to the next major civilization's (`Game.end_turn`,
    /// `game.py:1012-1037`): the city-states and the barbarians play their turns inside the
    /// call, a round ends after the last player, and the next major civilization's turn begins.
    /// Refused when the game is over or it is not `pid`'s turn. The chat rule that may refuse a
    /// player's `end_turn` tool is the action's, not this (phase0-spec A1.5).
    pub fn end_turn(&mut self, pid: PlayerId) -> Result<EventBatch, ActionError> {
        self.ensure_live()?;
        self.begin_call();
        self.end_turn_now(pid)?;
        self.settle();
        Ok(self.take_batch())
    }

    /// Makes it `pid`'s turn now and starts it, a probe's single-turn case
    /// (`EngineGame.force_turn`, `engine_api.py:754-762`); nothing if it is already `pid`'s turn.
    /// Refused for a player the game does not have, a player who has been eliminated, and a game
    /// that is over: Python made a dead player's turn current and moved the turn of a finished
    /// game (`force-turn-only-for-the-living`).
    pub fn force_turn(&mut self, pid: PlayerId) -> Result<EventBatch, ActionError> {
        self.ensure_live()?;
        self.begin_call();
        self.force_turn_now(pid)?;
        self.settle();
        Ok(self.take_batch())
    }

    /// Makes two players meet, with everything a first contact brings (`engine_api.meet`,
    /// `game.py:694-701`); nothing for a player and itself, the barbarians, or two who have met.
    pub fn meet(&mut self, a: PlayerId, b: PlayerId) -> Result<EventBatch, ActionError> {
        self.ensure_live()?;
        self.begin_call();
        self.make_contact(a, b);
        self.settle();
        Ok(self.take_batch())
    }

    /// Hands a civilization to another turn driver (`engine_api.set_controller`,
    /// `state.py:282-296`): its handicap and automatic decisions follow the controller, except
    /// those set explicitly, now (`handicap`, `auto`) or before.
    pub fn set_controller(
        &mut self,
        pid: PlayerId,
        controller: Controller,
        handicap: Option<Handicap>,
        auto: AutoOverrides,
    ) -> Result<EventBatch, EngineError> {
        self.ensure_live()?;
        if self.player(pid).is_none() {
            return Err(
                ActionError::new(ErrCode::InvalidPlayer, format!("No player {}.", pid.0)).into()
            );
        }
        self.begin_call();
        self.set_seat_controller(pid, controller, handicap, auto)?;
        self.settle();
        Ok(self.take_batch())
    }

    /// Gives one seat its own difficulty level, named loosely (`engine_api.set_difficulty`);
    /// false, with nothing changed, for a name that is no level or a game that takes no more
    /// commands.
    pub fn set_difficulty(&mut self, pid: PlayerId, name: &str) -> bool {
        let Some(level) = self.rules().resolve(name) else { return false };
        if self.ensure_live().is_err() || self.player(pid).is_none() {
            return false;
        }
        self.begin_call();
        if self.set_seat_difficulty(pid, Some(level)).is_err() {
            return false;
        }
        self.settle();
        true
    }
}

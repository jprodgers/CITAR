//! Whose turn it is, and how it passes (`game.py:1004-1037`, `turns.py:20-118, 190-202`).
//!
//! - `end_turn_now` ends a player's turn and hands the game on: city-states and the barbarians
//!   play their turns inside the call, and it stops at the next living major civilization,
//!   whose turn it begins (`Game.end_turn`; the host's [`Game::end_turn`]). A round ends when
//!   the last player has moved.
//! - `begin_turn` starts the current player's turn if it has not started (`begin_turn`).
//! - `force_turn_now` makes it one player's turn at once (`EngineGame.force_turn`; the host's
//!   [`Game::force_turn`]).
//! - A game may chain a digest of every round (DESIGN.md 4.10): [`Game::set_chain`].
//!
//! The steps of a turn are the stage tables of [`super::stages`]. `save_rng` is gone,
//! since every draw is keyed (DESIGN.md 7). What differs from Python, on purpose
//! (`tests/rules/intended.toml`):
//! - with no major civilization alive, Python's loop never ended: it played round after round
//!   inside one call. Here a call plays at most one more round and returns
//!   (`end-turn-stops-without-majors`);
//! - a player who ends a turn that is not theirs is told whose it is, as the tools' refusal
//!   says, where Python's `Game.end_turn` said only "It is not your turn."
//!   (`end-turn-names-whose-turn-it-is`).

use super::stages::{self, PLAYER_END, PLAYER_START, ROUND_END};
use crate::base::digest::Digest;
use crate::base::ids::PlayerId;
use crate::game::Game;
use crate::game::error::{ActionError, ErrCode};
use crate::save::chain::{DigestChain, Digester};
use crate::state::{Phase, TurnClock};

/// A game's chain of round digests and the buffer it takes them with (DESIGN.md 4.10). Never
/// saved: the chain covers states, and the host keeps its head beside a save.
#[derive(Clone, Debug)]
pub struct RoundChain {
    chain: DigestChain,
    digester: Digester,
    last: Option<Digest>,
}

impl Game {
    /// Starts the current player's turn, unless it has started or the game is over
    /// (`game.py:1004-1010`).
    pub(crate) fn begin_turn(&mut self) {
        let c = *self.st.clock();
        if c.turn_started || c.phase != Phase::Playing {
            return;
        }
        self.set_clock(TurnClock { turn_started: true, ..c });
        stages::run_player(self, &PLAYER_START, c.current);
    }

    /// Ends `pid`'s turn and hands the game on (`game.py:1012-1037`): city-states and the
    /// barbarians play their turns here, a round ends after the last player, and the call stops
    /// at the next living major civilization, having begun its turn. Refused in a game that is
    /// over, or when it is not `pid`'s turn.
    ///
    /// Settles at the settle points of the turns it plays, not after: a public call settles once
    /// more at its end.
    pub(crate) fn end_turn_now(&mut self, pid: PlayerId) -> Result<(), ActionError> {
        self.ensure_live()?;
        if self.phase() != Phase::Playing {
            return Err(ActionError::new(ErrCode::GameOver, "The game is over."));
        }
        if pid != self.current() {
            // refcheck: end-turn-names-whose-turn-it-is
            let whose = self.player(self.current()).map_or("another player", |p| &p.name);
            return Err(ActionError::new(
                ErrCode::NotYourTurn,
                format!("It is not your turn (it is {whose}'s turn)."),
            ));
        }
        stages::run_player(self, &PLAYER_END, pid);
        let n = self.st.players().len();
        let mut rounds = 0;
        while self.phase() == Phase::Playing {
            let mut next = usize::from(self.current().0) + 1;
            if next >= n {
                // Only a game with no living major civilization comes round twice: stop there.
                // refcheck: end-turn-stops-without-majors
                if rounds == 1 {
                    break;
                }
                rounds += 1;
                self.end_round();
                if self.phase() != Phase::Playing {
                    break;
                }
                next = 0;
            }
            let p = PlayerId(u8::try_from(next).unwrap_or(u8::MAX));
            let c = *self.st.clock();
            self.set_clock(TurnClock { current: p, turn_started: false, ..c });
            let Some(player) = self.player(p) else { break };
            if !player.alive() {
                continue;
            }
            let automatic = !player.is_major();
            self.begin_turn();
            if automatic {
                stages::run_player(self, &PLAYER_END, p);
                continue;
            }
            break;
        }
        Ok(())
    }

    /// Ends the round (`turns.end_round`, `turns.py:190-202`): eliminations, diplomacy, the
    /// statistics and the frame, the next turn, the vote, victory and the turn limit, a settle,
    /// and the round's digest for a game that chains.
    pub(crate) fn end_round(&mut self) {
        stages::run_round(self, &ROUND_END);
    }

    /// Makes it `pid`'s turn now and starts it, as a probe's single-turn case does
    /// (`EngineGame.force_turn`, `engine_api.py:754-762`); nothing if it is already.
    pub(crate) fn force_turn_now(&mut self, pid: PlayerId) {
        let c = *self.st.clock();
        if c.current == pid || self.player(pid).is_none() {
            return;
        }
        self.set_clock(TurnClock { current: pid, turn_started: false, ..c });
        self.begin_turn();
    }

    /// Chains a digest of every round from here on (DESIGN.md 4.10), taken at the end of each
    /// round, after its last settle and before the next turn begins; `None` stops. Opt-in: jobs
    /// and tests chain, lobby games do not. A host carries a chain across a save and load with
    /// [`DigestChain::resume`].
    pub fn set_chain(&mut self, chain: Option<DigestChain>) {
        self.chain = chain
            .map(|chain| Box::new(RoundChain { chain, digester: Digester::new(), last: None }));
    }

    /// The chain of round digests, if the game keeps one.
    #[must_use]
    pub fn chain(&self) -> Option<&DigestChain> {
        self.chain.as_ref().map(|c| &c.chain)
    }

    /// The digest of the last round the chain took, if any.
    #[must_use]
    pub fn last_round_digest(&self) -> Option<Digest> {
        self.chain.as_ref().and_then(|c| c.last)
    }

    /// Folds the digest of the round just ended into the chain, if the game keeps one. The round
    /// is the turn before the current one.
    pub(crate) fn chain_round(&mut self) {
        let Some(mut rc) = self.chain.take() else { return };
        match rc.digester.digest(self.rules, &self.st) {
            Ok(d) => {
                rc.chain.push(self.turn().saturating_sub(1), &d);
                rc.last = Some(d);
            }
            // A float that is not finite: invariant PLAYER-1 reports it at the settle before.
            Err(e) => debug_assert!(false, "the round's digest failed: {e}"),
        }
        self.chain = Some(rc);
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use crate::base::ids::PlayerId;
    use crate::game::core::testing;

    #[test]
    fn with_no_major_civilization_alive_a_call_plays_at_most_one_more_round() {
        // refcheck: end-turn-stops-without-majors
        let mut g = testing::duel();
        g.begin_turn();
        for p in [PlayerId(0), PlayerId(1)] {
            g.kill_player(p).expect("a player");
        }
        g.end_turn_now(PlayerId(0)).expect("the turn of the current player");
        assert_eq!(g.turn(), 2, "one round, and no more");
        assert!(g.player(g.current()).is_some_and(|p| !p.is_major()));
    }

    #[test]
    fn a_forced_turn_begins_at_once_and_only_for_another_player() {
        let mut g = testing::duel();
        g.begin_turn();
        let before = g.rev();
        g.force_turn_now(PlayerId(0));
        assert_eq!(g.rev(), before, "it is the player's turn already");
        g.force_turn_now(PlayerId(1));
        assert_eq!(g.current(), PlayerId(1));
        assert!(g.state().clock().turn_started);
        let last = g.chronicle().events().last().map(|e| e.text.to_string());
        assert_eq!(last.as_deref(), Some("Turn 1 (4000 BC): Greece's turn."));
    }
}

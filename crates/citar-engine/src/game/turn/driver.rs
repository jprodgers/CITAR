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
//!   inside one call. Here a call ends the round and returns before anyone's turn of the next
//!   round begins (`end-turn-stops-without-majors`);
//! - a player who ends a turn that is not theirs is told whose it is, as the tools' refusal
//!   says, where Python's `Game.end_turn` said only "It is not your turn."
//!   (`end-turn-names-whose-turn-it-is`);
//! - a forced turn is refused for a dead player and in a game that is over, where Python's
//!   `force_turn` made it the dead player's turn, or moved the turn of a finished game
//!   (`force-turn-only-for-the-living`).
//!
//! While a seat's driver plays inside `drive`, ending or forcing a turn is refused: `drive` ends
//! the turn itself once the driver has returned (DESIGN.md 6.12).

use super::stages::{self, PLAYER_END, PLAYER_START, ROUND_END};
use crate::base::digest::Digest;
use crate::base::ids::{PlayerId, Turn};
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
    /// The turn number of the round being ended, set as its end begins.
    round: Turn,
    /// The last round taken, and its digest.
    last: Option<(Turn, Digest)>,
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
        // A turn begun anew, as a forced one is, is its driver's to play again.
        self.clear_drive_mark();
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
        self.ensure_not_driving()?;
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
        // A turn ends only once it has begun: the first turn of a round a call stopped before.
        self.begin_turn();
        self.clear_drive_mark();
        stages::run_player(self, &PLAYER_END, pid);
        let n = self.st.players().len();
        while self.phase() == Phase::Playing {
            let mut next = usize::from(self.current().0) + 1;
            if next >= n {
                self.end_round();
                if self.phase() != Phase::Playing {
                    break;
                }
                next = 0;
                if self.majors(true).next().is_none() {
                    // Nobody would ever stop the loop: return at the round's start, before anyone
                    // plays, where the next call picks it up.
                    // refcheck: end-turn-stops-without-majors
                    let c = *self.st.clock();
                    self.set_clock(TurnClock { current: PlayerId(0), turn_started: false, ..c });
                    break;
                }
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
    /// statistics and the frame, the next turn, the vote, victory and the turn limit, then the
    /// round's close: a settle, and the round's digest for a game that chains. A round that ends
    /// the game at its eliminations skips to its close.
    pub(crate) fn end_round(&mut self) {
        let round = self.turn();
        if let Some(rc) = self.chain.as_mut() {
            rc.round = round;
        }
        stages::run_round(self, &ROUND_END);
    }

    /// Makes it `pid`'s turn now and starts it, as a probe's single-turn case does
    /// (`EngineGame.force_turn`, `engine_api.py:754-762`); nothing if it is already. Refused for
    /// a player the game does not have, a dead one, a game that is over, and while a seat's
    /// driver plays.
    pub(crate) fn force_turn_now(&mut self, pid: PlayerId) -> Result<(), ActionError> {
        self.ensure_live()?;
        self.ensure_not_driving()?;
        let Some(p) = self.player(pid) else {
            return Err(ActionError::new(ErrCode::InvalidPlayer, format!("No player {}.", pid.0)));
        };
        // refcheck: force-turn-only-for-the-living
        if !p.alive() {
            return Err(ActionError::new(
                ErrCode::Eliminated,
                format!("{} has been eliminated and plays no turns.", p.name),
            ));
        }
        let c = *self.st.clock();
        if c.phase != Phase::Playing {
            return Err(ActionError::new(ErrCode::GameOver, "The game is over."));
        }
        if c.current != pid {
            self.set_clock(TurnClock { current: pid, turn_started: false, ..c });
            self.begin_turn();
        }
        Ok(())
    }

    /// Chains a digest of every round from here on (DESIGN.md 4.10), taken at the end of each
    /// round, after its last settle and before the next turn begins; `None` stops. Opt-in: jobs
    /// and tests chain, lobby games do not. A host carries a chain across a save and load with
    /// [`DigestChain::resume`].
    pub fn set_chain(&mut self, chain: Option<DigestChain>) {
        self.chain = chain.map(|chain| {
            Box::new(RoundChain { chain, digester: Digester::new(), round: 0, last: None })
        });
    }

    /// The chain of round digests, if the game keeps one.
    #[must_use]
    pub fn chain(&self) -> Option<&DigestChain> {
        self.chain.as_ref().map(|c| &c.chain)
    }

    /// The digest of the last round the chain took, if any.
    #[must_use]
    pub fn last_round_digest(&self) -> Option<Digest> {
        self.last_round().map(|(_, d)| d)
    }

    /// The last round the chain took, by its turn number, and its digest.
    #[must_use]
    pub fn last_round(&self) -> Option<(Turn, Digest)> {
        self.chain.as_ref().and_then(|c| c.last)
    }

    /// Folds the digest of the round being ended into the chain, if the game keeps one, under
    /// the round's turn number (set as the round's end began, before the turn moves on).
    pub(crate) fn chain_round(&mut self) {
        let Some(mut rc) = self.chain.take() else { return };
        match rc.digester.digest(self.rules, &self.st) {
            Ok(d) => {
                rc.chain.push(rc.round, &d);
                rc.last = Some((rc.round, d));
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
    use crate::game::Game;
    use crate::game::core::testing;
    use crate::game::error::ErrCode;
    use crate::save::chain::DigestChain;
    use crate::state::Phase;

    #[test]
    fn with_no_major_civilization_alive_a_call_ends_the_round_and_stops_at_the_next() {
        // refcheck: end-turn-stops-without-majors
        let mut g = testing::duel();
        g.begin_turn();
        for p in [PlayerId(0), PlayerId(1)] {
            g.kill_player(p).expect("a player");
        }
        let at = |g: &Game| {
            let c = g.state().clock();
            (c.turn, c.current, c.turn_started)
        };
        g.end_turn_now(PlayerId(0)).expect("the turn of the current player");
        // The round ended, and nobody's turn of the next one has begun, let alone ended: the
        // players after the first do not end a turn twice in one round.
        assert_eq!(at(&g), (2, PlayerId(0), false));
        g.end_turn_now(PlayerId(0)).expect("the next round, from its start");
        assert_eq!(at(&g), (3, PlayerId(0), false), "one round per call");
    }

    #[test]
    fn a_forced_turn_begins_at_once_and_only_for_another_player() {
        let mut g = testing::duel();
        g.begin_turn();
        let before = g.rev();
        g.force_turn_now(PlayerId(0)).expect("a living player");
        assert_eq!(g.rev(), before, "it is the player's turn already");
        g.force_turn_now(PlayerId(1)).expect("a living player");
        assert_eq!(g.current(), PlayerId(1));
        assert!(g.state().clock().turn_started);
        let last = g.chronicle().events().last().map(|e| e.text.to_string());
        assert_eq!(last.as_deref(), Some("Turn 1 (4000 BC): Greece's turn."));
    }

    #[test]
    fn a_forced_turn_is_refused_for_the_dead_and_changes_nothing()
    -> Result<(), crate::base::digest::CanonError> {
        // refcheck: force-turn-only-for-the-living
        let mut g = testing::duel();
        g.set_debug_options(crate::game::DebugOptions::ALL);
        g.begin_turn();
        g.kill_player(PlayerId(1)).expect("a player");
        g.settle();
        let before = (g.digest()?, g.rev(), g.current());
        let e = g.force_turn(PlayerId(1)).expect_err("a dead player");
        assert_eq!(e.code, ErrCode::Eliminated);
        assert_eq!(e.message, "Greece has been eliminated and plays no turns.");
        let e = g.force_turn(PlayerId(9)).expect_err("no such player");
        assert_eq!((e.code, e.message.as_str()), (ErrCode::InvalidPlayer, "No player 9."));
        assert_eq!((g.digest()?, g.rev(), g.current()), before);
        assert_eq!(g.take_violations(), [], "no dead player's turn, so TURN-1 holds");
        Ok(())
    }

    #[test]
    fn a_round_that_ends_the_game_is_still_closed_and_chained_as_itself()
    -> Result<(), crate::base::digest::CanonError> {
        let mut g = testing::duel();
        testing::unit(&mut g, PlayerId(0), "Warrior", crate::base::ids::TileIdx(40));
        let theirs = testing::unit(&mut g, PlayerId(1), "Warrior", crate::base::ids::TileIdx(60));
        g.begin_turn();
        g.set_chain(Some(DigestChain::new(b"test")));
        g.end_round();
        assert_eq!(g.turn(), 2);
        assert_eq!(g.last_round().map(|(t, _)| t), Some(1));
        // A round whose eliminations end the game stops before the next turn, and is still
        // settled and chained, under its own turn number: the second civilization, left with
        // nothing, is eliminated, and the first, the last one standing, wins.
        crate::game::units::remove_unit(&mut g, theirs);
        g.end_round();
        assert_eq!(g.phase(), Phase::Over);
        assert_eq!(g.state().clock().winner, Some(PlayerId(0)));
        assert_eq!(g.turn(), 2, "the turn does not move on in a game that is over");
        assert_eq!(g.chain().map(|c| c.rounds()), Some(2));
        assert_eq!(g.last_round(), Some((2, g.digest()?)));
        Ok(())
    }
}

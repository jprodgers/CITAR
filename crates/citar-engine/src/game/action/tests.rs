//! Gate 5 of package 1b-01: a refused action leaves the game as it was and does not settle.

use serde_json::json;

use super::{OutcomeSpec, Rule};
use crate::base::ids::PlayerId;
use crate::game::Game;
use crate::game::derive::rev::PlayerTouch;
use crate::game::error::ActionError;

/// A test action: adds gold, unless told to refuse.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Probe {
    pub gold: i32,
    pub refuse: bool,
    pub any_time: bool,
}

impl Rule for Probe {
    type Plan = f64;

    fn check(&self, _: &Game, _: PlayerId) -> Result<f64, ActionError> {
        if self.refuse {
            return Err(ActionError::rule("The probe refuses."));
        }
        Ok(f64::from(self.gold))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, plan: f64) -> OutcomeSpec {
        if let Some(p) = g.player_mut(pid, PlayerTouch::STOCKS) {
            p.econ.gold += plan;
        }
        // Raise work for the settle, so the rendered result shows it ran first.
        let cities: Vec<_> = g.st.cities().of(pid).to_vec();
        for c in cities {
            g.city_mut(c, crate::game::derive::rev::CityTouch::WORK);
        }
        OutcomeSpec::render(
            move |g| json!({"gold": g.player(pid).map(|p| p.econ.gold), "settled": g.pending.is_empty()}),
        )
    }
}

#[cfg(feature = "embedded-ruleset")]
mod gates {
    use super::Probe;
    use crate::base::ids::{PlayerId, TileIdx};
    use crate::game::core::testing;
    use crate::game::derive::rev::CityTouch;
    use crate::game::error::ErrCode;
    use crate::game::{Action, Game};
    use crate::state::{Phase, TurnClock};

    const ROME: PlayerId = PlayerId(0);

    fn game() -> Game {
        let mut g = testing::duel();
        testing::city(&mut g, ROME, TileIdx(22), "Roma");
        g.settle();
        g
    }

    fn probe(gold: i32, refuse: bool) -> Action {
        Action::Probe(Probe { gold, refuse, any_time: false })
    }

    /// Everything a refusal must leave as it was: the digest, the revision, the events, the
    /// action log and the pending work (a refusal never settles).
    fn snapshot(g: &Game) -> (crate::base::digest::Digest, u64, usize, usize, bool) {
        (
            g.digest().expect("a digest"),
            g.rev(),
            g.chronicle().events().len(),
            g.chronicle().actions().len(),
            g.pending.is_empty(),
        )
    }

    #[test]
    fn a_refused_action_changes_nothing_and_does_not_settle() {
        let mut g = game();
        // Work is pending, as it would be before a settle point: a settle would clear it.
        let c = g.st.cities().ids()[0];
        g.city_mut(c, CityTouch::WORK);
        let before = snapshot(&g);
        assert!(!before.4);
        let refusals = [
            (ROME, probe(5, true), ErrCode::Rule),
            (PlayerId(1), probe(5, false), ErrCode::NotYourTurn),
            (PlayerId(2), probe(5, false), ErrCode::InvalidPlayer),
            (PlayerId(9), probe(5, false), ErrCode::InvalidPlayer),
        ];
        for (pid, a, code) in refusals {
            let err = g.act(pid, a).expect_err("refused");
            assert_eq!(err.code, code, "{err}");
            assert!(err.message.ends_with('.') || err.message.ends_with(')'), "{err}");
            assert_eq!(snapshot(&g), before, "{err}");
        }
        assert_eq!(
            g.act(PlayerId(1), probe(5, false)).expect_err("refused").message,
            "It is not your turn (it is Rome's turn)."
        );
    }

    #[test]
    fn eliminated_and_game_over_are_refused_before_the_rule() {
        let mut g = game();
        g.kill_player(PlayerId(1)).expect("Greece holds no cities");
        g.settle();
        let err = g.act(PlayerId(1), probe(1, false)).expect_err("eliminated");
        assert_eq!(err.code, ErrCode::Eliminated);
        let clock = TurnClock { phase: Phase::Over, winner: Some(ROME), ..*g.st.clock() };
        g.set_clock(clock);
        g.settle();
        let before = snapshot(&g);
        let err = g.act(ROME, probe(1, false)).expect_err("over");
        assert_eq!((err.code, err.message.as_str()), (ErrCode::GameOver, "The game is over."));
        assert_eq!(snapshot(&g), before);
    }

    #[test]
    fn an_action_applies_settles_then_renders() {
        let mut g = game();
        let gold = g.player(ROME).map(|p| p.econ.gold).unwrap_or_default();
        let (out, batch) = g.act(ROME, probe(7, false)).expect("accepted");
        assert_eq!(out["gold"], serde_json::json!(gold + 7.0));
        assert_eq!(out["settled"], serde_json::json!(true), "rendered after the settle");
        assert!(batch.is_empty() && g.pending.is_empty());
        assert_eq!(g.take_violations(), []);
        // Any time: another player's turn is no refusal.
        let any = Action::Probe(Probe { gold: 1, refuse: false, any_time: true });
        assert!(g.act(PlayerId(1), any).is_ok());
    }

    #[test]
    fn an_action_that_succeeds_is_logged_and_a_refusal_is_not() {
        let mut g = game();
        let (hosted, digest) = (g.st.host().actions, g.digest().expect("a digest"));
        g.act(ROME, probe(0, false)).expect("accepted");
        let _refused = g.act(ROME, probe(4, true)).expect_err("refused");
        let log = g.chronicle().actions();
        assert_eq!(log.len(), 1);
        let rec = &log[0];
        assert_eq!((rec.turn, rec.player, &*rec.tool), (g.turn(), ROME, "probe"));
        let args: serde_json::Value = serde_json::from_str(&rec.args).expect("JSON");
        assert_eq!(args, serde_json::json!({"gold": 0, "refuse": false, "any_time": false}));
        // Host activity: counted in the host heads, never in the digest.
        assert_eq!(g.st.host().actions, hosted + 1);
        assert_eq!(g.digest().expect("a digest"), digest);
    }

    #[test]
    fn a_poisoned_game_refuses_everything() {
        let mut g = game();
        g.poison("a test");
        let before = snapshot(&g);
        let err = g.act(ROME, probe(1, false)).expect_err("poisoned");
        assert_eq!(err.code, ErrCode::Poisoned);
        assert_eq!(snapshot(&g), before);
        assert_eq!(g.poisoned(), Some("a test"));
    }

    #[test]
    fn actions_are_tagged_by_tool_name() {
        let a = probe(3, false);
        let v = serde_json::to_value(&a).expect("serialises");
        assert_eq!(
            v,
            serde_json::json!({"tool": "probe", "gold": 3, "refuse": false, "any_time": false})
        );
        assert_eq!(serde_json::from_value::<Action>(v).expect("reads back"), a);
        assert_eq!(a.tool(), "probe");
    }
}

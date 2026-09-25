//! Settle: the consequential writes, done to a fixed point (DESIGN.md 6.7).
//!
//! Python did these as it went: a unit step refreshed vision, which met civilizations, which
//! emitted events (`visibility.py:128-168`), and a changed tile reassigned citizens from inside
//! the yield code (`game.py:588-601`, `cities.py:748`). Here writes only flag what they made
//! stale (`game::pending`), and a settle catches up, in a fixed order:
//! 1. sight: dirty vision sources are brought up to date, and what the transitions reveal is
//!    queued as effects (`game::vis`, package 1c-01); the effects are applied in their order,
//!    and the two alternate until neither has anything left;
//! 2. citizens: flagged cities reassign their citizens in id order, pass after pass, until no
//!    city is flagged or [`SETTLE_PASSES`] passes are spent, which is invariant violation
//!    SETTLE-1 (`game::cities::citizens`, package 1b-06);
//! 3. the checks [`DebugOptions`](crate::game::DebugOptions) asks for.
//!
//! Settle runs at the end of every successful mutating call and at the settle points of a turn,
//! never after a refusal, a query, a view, a snapshot or a save. Within one settle nothing
//! citizens read can move: happiness and the gold rate are committed only at fixed stages, and
//! sight never depends on citizens. So the passes converge, and what a settle does is a pure
//! function of the calls that succeeded.

use crate::game::Game;
use crate::game::invariants::{self, Code, Violation};
use crate::game::pending::{Effect, EffectQueue};

/// How many citizen passes one settle may take (DESIGN.md 6.7).
pub const SETTLE_PASSES: u32 = 8;

#[cfg(feature = "test-ops")]
impl Game {
    /// Settles the game, as the end of a public call does: for tests that call the rules
    /// directly, between their steps.
    pub fn settle_for_test(&mut self) {
        self.settle();
    }
}

impl Game {
    /// Settles the game: sight and its effects, then citizens, then the checks (DESIGN.md 6.7).
    /// Pending work is empty afterwards.
    pub(crate) fn settle(&mut self) {
        self.settle_sight();
        let mut passes = 0;
        while self.pending.any_recheck() && passes < SETTLE_PASSES {
            self.reassign_flagged();
            passes += 1;
        }
        if self.pending.any_recheck() {
            let left = self.pending.take_recheck();
            if self.debug.invariants {
                self.report(Violation::new(
                    Code::Settle1,
                    format!(
                        "citizens did not settle within {SETTLE_PASSES} passes; still flagged: {}",
                        left.iter().map(|c| c.get().to_string()).collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
        self.dv.vis.clear_newly_seen();
        self.run_checks();
    }

    /// Sight and its effects to a fixed point, the first part of a settle (`visibility.refresh`,
    /// `visibility.py:128-168`): the dirty vision sources are brought up to date, which queues
    /// meetings and discoveries; they are applied in their order, which may make more sources
    /// dirty; and so on until nothing is left. Also what setup's visibility stage runs.
    pub(crate) fn settle_sight(&mut self) {
        let limit = EffectQueue::limit(self.st.tiles().len(), self.st.players().len());
        let mut applied = 0u64;
        loop {
            self.sync_sight();
            if self.fx.is_empty() {
                break;
            }
            while let Some(e) = self.fx.pop() {
                applied += 1;
                if applied > limit {
                    self.runaway(format!(
                        "the effect queue ran past {limit} effects; the last was {e:?}"
                    ));
                    return;
                }
                self.apply_effect(e);
            }
        }
    }

    /// A settle on its own, for the benchmark of one with nothing pending (DESIGN.md 10): the
    /// engine settles only inside its own calls.
    #[cfg(feature = "test-ops")]
    #[doc(hidden)]
    pub fn settle_for_bench(&mut self) {
        self.settle();
    }

    /// A write that moves the game's revision and nothing any cache reads (a touch of player
    /// `p`'s `OTHER` fields), for the benchmark of a first read after an unrelated change
    /// (DESIGN.md 10).
    #[cfg(feature = "test-ops")]
    #[doc(hidden)]
    pub fn unrelated_change_for_bench(&mut self, p: crate::base::ids::PlayerId) {
        let _touched = self.player_mut(p, crate::game::derive::rev::PlayerTouch::OTHER).is_some();
    }

    /// A write that moves city `c`'s `core` revision, as a heal, a growth or a queue edit does,
    /// and changes nothing, for the benchmark of the lists its sibling cities read after it
    /// (DESIGN.md 10).
    #[cfg(feature = "test-ops")]
    #[doc(hidden)]
    pub fn city_change_for_bench(&mut self, c: crate::base::ids::CityId) {
        let _touched = self.city_mut(c, crate::game::derive::rev::CityTouch::CORE).is_some();
    }

    /// Moves unit `u` to tile `t` without movement rules and settles, for the benchmark of the
    /// memos read after a move (DESIGN.md 6.5: a move recomputes none that did not read it).
    ///
    /// # Errors
    /// If there is no such unit or tile.
    #[cfg(feature = "test-ops")]
    #[doc(hidden)]
    pub fn move_unit_for_bench(
        &mut self,
        u: crate::base::ids::UnitId,
        t: crate::base::ids::TileIdx,
    ) -> Result<(), crate::state::StateError> {
        self.relocate_unit(u, t)?;
        self.settle();
        Ok(())
    }

    /// Stops a runaway effect queue: a bug, reported as SETTLE-1 where checks run.
    fn runaway(&mut self, why: String) {
        while self.fx.pop().is_some() {}
        self.pending.clear_sight();
        debug_assert!(false, "{why}");
        if self.debug.invariants {
            self.report(Violation::new(Code::Settle1, why));
        }
    }

    /// Applies one effect, which may raise more work.
    fn apply_effect(&mut self, e: Effect) {
        match e {
            Effect::Meet { a, b } => self.make_contact(a, b),
            Effect::Wonder { civ, tile } => self.discover_wonder(civ, tile),
        }
    }

    /// One pass over the flagged cities, in id order: each reassigns its citizens, which may
    /// flag a sibling city whose tiles it took or released (DESIGN.md 6.7). A sibling with a
    /// higher id is reassigned in the same pass, one with a lower id in the next.
    fn reassign_flagged(&mut self) {
        #[cfg(test)]
        if self.pending.stubborn {
            return;
        }
        let mut from = 0;
        while let Some(c) = self.pending.take_recheck_from(from) {
            from = c.get().saturating_add(1);
            self.reassign(c);
        }
    }

    /// The checks the debug options ask for (DESIGN.md 9.4): they only read.
    fn run_checks(&mut self) {
        if self.debug.invariants {
            for v in invariants::check(self) {
                self.report(v);
            }
        }
        if self.debug.verify_caches {
            for why in self.verify_caches() {
                self.report(Violation::new(Code::Cache1, why));
            }
        }
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use crate::base::ids::{PlayerId, TileIdx};
    use crate::game::core::testing;
    use crate::game::derive::rev::CityTouch;
    use crate::game::invariants::Code;
    use crate::game::pending::Effect;

    #[test]
    fn a_settle_empties_pending_work_and_applies_effects_in_order() {
        let mut g = testing::duel();
        let c = testing::city(&mut g, PlayerId(0), TileIdx(22), "Roma");
        g.city_mut(c, CityTouch::WORK);
        g.fx.push(Effect::meet(PlayerId(1), PlayerId(0)));
        g.fx.push(Effect::meet(PlayerId(0), PlayerId(2)));
        assert!(!g.pending.is_empty());
        g.settle();
        assert!(g.pending.is_empty() && g.fx.is_empty());
        assert!(g.has_met(PlayerId(0), PlayerId(1)) && g.has_met(PlayerId(0), PlayerId(2)));
        let texts: Vec<_> = g.chronicle().events().iter().map(|e| &*e.text).collect();
        assert_eq!(
            texts,
            ["Rome and Greece have made contact.", "Rome and Geneva have made contact."]
        );
        assert_eq!(g.take_violations(), []);
    }

    #[test]
    fn citizens_that_never_settle_are_settle_1_and_left_as_they_are() {
        let mut g = testing::duel();
        testing::city(&mut g, PlayerId(0), TileIdx(22), "Roma");
        g.pending.stubborn = true;
        g.settle();
        let codes: Vec<Code> = g.take_violations().iter().map(|v| v.code).collect();
        assert_eq!(codes, [Code::Settle1]);
        assert!(g.pending.is_empty(), "the flags are cleared");
    }

    #[test]
    fn a_settle_with_nothing_pending_changes_nothing() -> Result<(), crate::base::digest::CanonError>
    {
        let mut g = testing::duel();
        let before = (g.digest()?, g.rev(), g.chronicle().events().len());
        g.settle();
        assert_eq!((g.digest()?, g.rev(), g.chronicle().events().len()), before);
        Ok(())
    }
}

//! Settle: the consequential writes, done to a fixed point (DESIGN.md 6.7).
//!
//! Python did these as it went: a unit step refreshed vision, which met civilizations, which
//! emitted events (`visibility.py:128-168`), and a changed tile reassigned citizens from inside
//! the yield code (`game.py:588-601`, `cities.py:748`). Here writes only flag what they made
//! stale (`game::pending`), and a settle catches up, in a fixed order:
//! 1. sight: dirty vision sources are brought up to date, and what the transitions reveal is
//!    queued as effects (package 1c-01); the effects are applied in their order, and the two
//!    alternate until neither has anything left;
//! 2. citizens: flagged cities reassign their citizens in id order, pass after pass, until no
//!    city is flagged or [`SETTLE_PASSES`] passes are spent, which is invariant violation
//!    SETTLE-1 (package 1b-06 ports the assignment);
//! 3. the checks [`DebugOptions`](crate::game::DebugOptions) asks for.
//!
//! Settle runs at the end of every successful mutating call and at the settle points of a turn,
//! never after a refusal, a query, a view, a snapshot or a save. Within one settle nothing
//! citizens read can move: happiness and the gold rate are committed only at fixed stages, and
//! sight never depends on citizens. So the passes converge, and what a settle does is a pure
//! function of the calls that succeeded.

use crate::game::invariants::{self, Code, Violation};
use crate::game::pending::{Effect, EffectQueue};
use crate::game::{Game, Porting, pending};

/// How many citizen passes one settle may take (DESIGN.md 6.7).
pub const SETTLE_PASSES: u32 = 8;

impl Game {
    /// Settles the game: sight and its effects, then citizens, then the checks (DESIGN.md 6.7).
    /// Pending work is empty afterwards.
    pub(crate) fn settle(&mut self) {
        let mut applied = 0u32;
        while self.pending.any_sight() || !self.fx.is_empty() {
            self.sync_sight();
            while let Some(e) = self.fx.pop() {
                applied += 1;
                if applied > EffectQueue::LIMIT {
                    self.runaway(format!(
                        "the effect queue ran past {} effects; the last was {e:?}",
                        EffectQueue::LIMIT
                    ));
                    break;
                }
                self.apply_effect(e);
            }
            if applied > EffectQueue::LIMIT {
                break;
            }
        }
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
        self.run_checks();
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

    /// Brings the dirty vision sources up to date and queues what their transitions reveal:
    /// explored tiles, memory, first contact, natural wonders (DESIGN.md 6.9).
    fn sync_sight(&mut self) {
        // visibility.py:96-168: footprints, counts, transitions and their effects.
        pending(Porting::Pending("1c-01"));
        self.pending.clear_sight();
    }

    /// Applies one effect, which may raise more work.
    fn apply_effect(&mut self, e: Effect) {
        match e {
            Effect::Meet { a, b } => self.meet(a, b),
        }
    }

    /// One pass over the flagged cities, in id order: each reassigns its citizens, which may
    /// flag a sibling city whose tiles it took or released (DESIGN.md 6.7).
    fn reassign_flagged(&mut self) {
        // cities.assign_citizens (cities.py:748-926) and the citizen oracle's settled flag.
        pending(Porting::Pending("1b-06"));
        #[cfg(test)]
        if self.pending.stubborn {
            return;
        }
        self.pending.clear_recheck();
    }

    /// The checks the debug options ask for (DESIGN.md 9.4): they only read.
    fn run_checks(&mut self) {
        if self.debug.invariants {
            for v in invariants::check(self) {
                self.report(v);
            }
        }
        if self.debug.verify_caches {
            for why in self.dv.verify(&self.st) {
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

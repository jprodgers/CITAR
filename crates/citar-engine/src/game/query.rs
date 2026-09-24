//! Reads shared by refcheck's answer modules, the views and the tools (DESIGN.md 9.2).
//!
//! Every function here takes `&Game`: a query validates the memos it touches and never changes
//! the state, the revisions a write moves, or the digest (property P8). Refcheck asserts the
//! digest before and after each group; the views and the tool queries (packages 1d-01 and
//! 1d-02) build their answers from these.
//!
//! Package 1b-01 lands the skeleton: the unique queries in a game's view. Each system package
//! adds the reads its refcheck group compares (DESIGN.md 3.4, rule 1).

use crate::base::ids::{PlayerId, UniqueId};
use crate::unique::{Ctx, UniqueType, uq};

use super::Game;

/// The civilization's uniques of type `ty` that hold in `ctx`, with their copies, in index
/// order (`civ_uniques`, `economy.py:132-148`).
#[must_use]
pub fn civ_uniques(g: &Game, p: PlayerId, ty: UniqueType, ctx: &Ctx) -> Vec<(UniqueId, u16)> {
    let v = g.view();
    uq::civ(&v, p, ty, ctx).map(|h| (h.id, h.n)).collect()
}

/// Whether a unique's conditionals hold in `ctx` (`Unique.applies`, `uniques.py:778-796`).
#[must_use]
pub fn applies(g: &Game, id: UniqueId, ctx: &Ctx) -> bool {
    crate::unique::applies(id, ctx, &g.view())
}

/// The context of a question about civilization `p` in this game.
#[must_use]
pub const fn civ_ctx(p: PlayerId) -> Ctx {
    Ctx::civ(p)
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::game::core::testing;

    #[test]
    fn a_query_changes_nothing() -> Result<(), crate::base::digest::CanonError> {
        let g = testing::duel();
        let before = (g.digest()?, g.rev());
        let _found = civ_uniques(&g, PlayerId(0), UniqueType::Stats, &civ_ctx(PlayerId(0)));
        assert!(applies(&g, UniqueId(0), &Ctx::IGNORE));
        assert_eq!((g.digest()?, g.rev()), before);
        Ok(())
    }
}

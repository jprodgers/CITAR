//! Reads shared by refcheck's answer modules, the views and the tools (DESIGN.md 9.2).
//!
//! Every function here takes `&Game`: a query validates the memos it touches and never changes
//! the state, the revisions a write moves, or the digest (property P8). Refcheck asserts the
//! digest before and after each group; the views and the tool queries (packages 1d-01 and
//! 1d-02) build their answers from these.
//!
//! Package 1b-01 lands the skeleton: the unique queries in a game's view. Each system package
//! adds the reads its refcheck group compares (DESIGN.md 3.4, rule 1): package 1b-05 the
//! resource supply, the unique index by placeholder, unit upkeep and unit supply of the `civs`
//! group.

use std::collections::BTreeMap;

use crate::base::ids::{EraId, PlayerId, ResourceId, UniqueId};
use crate::unique::{Ctx, UniqueType, index, uq};

use super::Game;
use super::derive::civ;
use super::economy::{self, ResourceItem};

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

/// A civilization's net amount of each resource it has a line of, in the order they first
/// appear (`economy.resource_supply`, `economy.py:339-353`).
#[must_use]
pub fn resource_supply(g: &Game, p: PlayerId) -> Vec<(ResourceId, i32)> {
    economy::supply(g, p).map(|s| s.totals().to_vec()).unwrap_or_default()
}

/// A civilization's resources line by line (`economy.detailed_resources`,
/// `economy.py:279-311`).
#[must_use]
pub fn detailed_resources(g: &Game, p: PlayerId) -> Vec<ResourceItem> {
    economy::supply(g, p).map(|s| s.items().to_vec()).unwrap_or_default()
}

/// How many uniques a civilization's sources give it, by placeholder, as Python's `civ_index`
/// counted them (`economy.py:135-147`): the entries of its index with its resource layer, each
/// as many times as its copies, and the uniques of the same sources the index leaves out by
/// design (`unique::index::unindexed`). A unique with no type counts under its text, as Python
/// took a tag's text for its placeholder.
#[must_use]
pub fn unique_index_counts(g: &Game, p: PlayerId) -> BTreeMap<String, u32> {
    let r = g.rules();
    let t = r.uniques();
    let mut src = civ::sources(g, p);
    src.resources = economy::supply(g, p).map(|s| s.positive()).unwrap_or_default();
    let placeholder = |id: UniqueId| match t.meta(id).ty {
        Some(ty) => ty.placeholder().to_owned(),
        None => t.text_of(id).to_owned(),
    };
    let mut out = BTreeMap::new();
    for e in civ::civ_index_full(g, p).entries() {
        *out.entry(placeholder(e.id)).or_insert(0) += u32::from(e.n);
    }
    for (id, n) in index::unindexed(r, &src) {
        *out.entry(placeholder(id)).or_insert(0) += u32::from(n);
    }
    out
}

/// The era a civilization is in (`research.player_era`, `research.py:254-275`).
#[must_use]
pub fn era(g: &Game, p: PlayerId) -> EraId {
    civ::era(g, p)
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

//! The cache oracle (DESIGN.md 9.4): every cache a game keeps, validated, against a cold
//! recompute from the same state. Replaces nothing in Python, which kept its caches unchecked
//! (`game.py:565-609` cleared them by hand).
//!
//! [`Game::verify_caches`] runs each memo family's own check and says, one line each, where a
//! cache disagrees. Between them they cover every memo of DESIGN.md 6.5, a rebuild of what each
//! civilization sees and whom it has met, and the citizen oracle (DESIGN.md 6.8):
//!
//! | Memo (DESIGN.md 6.5) | Checked by |
//! |---|---|
//! | `CivIndex`, `ResourceSupply`, `CivIndexFull`, `CityLocal`, `FollowerIndex` (with the city's full local index, the unit profiles, the era and the owned tiles) | `derive::civ::verify`, against the one cold game `cold` builds from a clone of the state |
//! | `CityMods`, `TileYield` (the owners' and every other viewer's), `CityHappiness`, `CityStats`, `Happiness`, `CivStats`, `Connectivity` (with unit upkeep and the supply deficit) | `derive::stats::verify`, against the same cold game |
//! | `SightMods`, `UnitSight`, `LosCache`, and the visibility counts and sources | `vis::verify`: sight rebuilt from the state, each unit's sight against the index, every line of sight kept against a fresh walk; then the met sets and the natural wonders found against Python's rule over what each civilization now sees |
//! | `Buildable` (with the civilization-wide requirements) | `derive::buildable::verify`, against the same cold game |
//! | `JobMap` | `derive::jobs::verify` |
//! | `DangerMap` | `derive::danger::verify` |
//! | `CityNeighbours` (the grid of cities) and each city's religious spread | `derive::religion::verify`, against the same cold game |
//! | `MoveCosts`, `Zoc`, and the unit movement profiles | `path::memo::verify` |
//! | `RouteLayer`, and the cheapest step off the routes | here: against a cold look at the map |
//! | the paths found at this revision | here: each against a fresh search |
//! | the event name index, and the grid | `Derived::verify` |
//! | the citizens of every city the engine has assigned | `cities::citizens::verify` |
//!
//! Compiled into test builds, debug builds and release builds with the `checks` feature, as the
//! invariants are (DESIGN.md 9.4); a shipped release build has neither. Whether it runs at every
//! settle is `DebugOptions::verify_caches`; testkit turns it on in scripts, properties and chaos.

use crate::game::Game;
use crate::game::path::Mover;

impl Game {
    /// Where the caches disagree with a cold recompute (the cache oracle, DESIGN.md 9.4): one
    /// line for each disagreement, none when every cache is right. It only reads: a game plays
    /// the same whether it runs or not.
    #[must_use]
    pub fn verify_caches(&self) -> Vec<String> {
        // One cold game for every family that compares with one: its memos, computed lazily as the
        // families ask, serve them all, so the state is cloned and each cold memo built once.
        let cold = cold(self);
        let mut out = self.dv.verify(self.rules, &self.st);
        out.extend(super::civ::verify(self, &cold));
        out.extend(crate::game::vis::verify(self));
        out.extend(super::stats::verify(self, &cold));
        out.extend(super::buildable::verify(self, &cold));
        out.extend(super::religion::verify(self, &cold));
        out.extend(crate::game::cities::citizens::verify(self));
        out.extend(crate::game::path::memo::verify(self));
        out.extend(super::danger::verify(self));
        out.extend(super::jobs::verify(self));
        if self.dv.terrain_floor(self) != crate::game::path::terrain_floor(self) {
            out.push(
                "the cheapest step off the routes differs from a cold look at the map".to_owned(),
            );
        }
        if *self.dv.route_net(self) != crate::game::path::route_net(self) {
            out.push("where routes run differs from a cold look at the map".to_owned());
        }
        out.extend(paths(self));
        out
    }
}

/// A game over a copy of `g`'s state with every cache cold and no history: what the memo
/// families compare their validated values with, built once for each [`Game::verify_caches`]
/// (and by the families' own tests).
pub(crate) fn cold(g: &Game) -> Game {
    Game::assemble(g.rules, g.st.clone(), crate::state::chronicle::Chronicle::new(), false)
}

/// The paths kept at this revision against a fresh search from where each unit stands: an
/// answer is kept only for the unit's tile and moves as they were asked, and any write moves the
/// revision, so each must be what a search gives now.
fn paths(g: &Game) -> Vec<String> {
    let now = g.dv.revs.now().get();
    let kept: Vec<_> = match g.dv.path_cache().try_borrow() {
        Ok(c) => c.at(now).to_vec(),
        // Only a search in progress holds it, and none is while the oracle reads.
        Err(_) => return vec!["the path cache is borrowed while the oracle reads".to_owned()],
    };
    let mut out = Vec::new();
    for (key, path) in kept {
        let asked = g.unit(key.unit).map(|u| (u.tile(), u.moves));
        if asked != Some((key.from, key.moves)) {
            out.push(format!(
                "a path kept for unit {} from tile {} with {} moves, where it is not",
                key.unit.get(),
                key.from,
                key.moves
            ));
            continue;
        }
        let fresh = Mover::unit(g, key.unit).and_then(|m| m.find_path(key.target, key.max_turns));
        if fresh != path {
            out.push(format!(
                "the path kept for unit {} to tile {} within {} turns is {path:?}, a fresh search \
                 finds {fresh:?}",
                key.unit.get(),
                key.target,
                key.max_turns
            ));
        }
    }
    out
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use crate::base::ids::{PlayerId, TileIdx};
    use crate::game::core::testing;
    use crate::game::path::{PathCache, PathKey};

    #[test]
    fn a_path_kept_is_what_a_fresh_search_finds_and_a_wrong_one_is_reported() {
        let mut g = testing::duel();
        let u = testing::unit(&mut g, PlayerId(0), "Warrior", TileIdx(22));
        g.settle();
        let target = TileIdx(25);
        let found = crate::game::movement::find_path(&g, u, target, 5);
        assert!(found.is_some(), "a warrior walks three tiles");
        assert_eq!(g.verify_caches(), Vec::<String>::new(), "a path found is a path kept");
        // A wrong answer kept at this revision, as a bug in the cache would keep it.
        let now = g.dv.revs.now().get();
        let x = g.unit(u).expect("the warrior");
        let key = PathKey { unit: u, from: x.tile(), moves: x.moves, target, max_turns: 5 };
        let mut cache = PathCache::default();
        cache.put(now, key, Some(vec![TileIdx(25)]));
        *g.dv.path_cache().borrow_mut() = cache;
        let found = g.verify_caches();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("a fresh search"), "{found:?}");
    }
}

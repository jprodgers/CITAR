//! The engine's own checks as testkit asks them (DESIGN.md 9.4): the invariants a game breaks and
//! where its caches disagree with a cold rebuild.
//!
//! The engine compiles both only into test and debug builds and into release builds with its
//! `checks` feature. Testkit turns that feature on through its own default `checks` feature, so
//! the golden binary, chaos and the tests have them in every profile. citar-bench builds testkit
//! without it, and turns the checks off in each game it times (`citar_bench::unchecked`), since a
//! command that selects testkit too unifies the feature back on. A build without them answers
//! with one line saying so, never with an empty list that would pass for a clean game.

use citar_engine::game::Game;

/// What a build without the engine's checks answers.
#[cfg(not(any(debug_assertions, feature = "checks")))]
const UNCHECKED: &str =
    "this build has no engine checks: build citar-testkit with its `checks` feature";

/// Every invariant of DESIGN.md 9.4 the game breaks now, one line each.
#[must_use]
pub fn invariants(g: &Game) -> Vec<String> {
    #[cfg(any(debug_assertions, feature = "checks"))]
    {
        g.check_invariants().into_iter().map(|v| v.to_string()).collect()
    }
    #[cfg(not(any(debug_assertions, feature = "checks")))]
    {
        let _ = g;
        vec![UNCHECKED.to_owned()]
    }
}

/// Where the game's caches disagree with a cold rebuild (the cache oracle), one line each.
#[must_use]
pub fn caches(g: &Game) -> Vec<String> {
    #[cfg(any(debug_assertions, feature = "checks"))]
    {
        g.verify_caches()
    }
    #[cfg(not(any(debug_assertions, feature = "checks")))]
    {
        let _ = g;
        vec![UNCHECKED.to_owned()]
    }
}

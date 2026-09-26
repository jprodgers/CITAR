//! Bugs seeded on purpose, so that the stability checks can show they find them (package 1e-01,
//! gate 1; DESIGN.md 9.5). Replaces nothing in Python.
//!
//! Each [`SeededBug`] is a bug of a kind the properties are there to catch, planted at one real
//! site of the engine. Only a test build can plant one ([`seed`], feature `test-ops`, on the
//! calling thread only); elsewhere [`has`] is a constant `false` and every site reads as the
//! engine without it. The sites:
//! - [`SeededBug::MutatesBeforeRefusing`]: `Game::act` takes a gold piece from a player whose
//!   `buy` its rule refused, as a tool that writes before it finds it must refuse would
//!   (property P2);
//! - [`SeededBug::StaleVisibility`]: `Derived::on` forgets to bring a unit's sight up to date
//!   when the unit is placed, made or moved (property P4, through the cache oracle);
//! - [`SeededBug::WrongTouch`]: renaming a city touches its `CORE` rather than its `NAME`, so
//!   the event name index keeps the old name (property P4);
//! - [`SeededBug::QueryWrites`]: every query tool, after its answer, writes through the
//!   interior mutability of the asker's happiness memo, which the end of the asker's turn then
//!   commits (property P8);
//! - [`SeededBug::Panics`]: a purchase the rule carried out panics before it settles, which a
//!   chaos replay file must reproduce (gate 3).

#[cfg(feature = "test-ops")]
use crate::base::ids::PlayerId;
#[cfg(feature = "test-ops")]
use crate::game::Game;

/// A bug a test build can plant in the engine (see the module's documentation for the sites).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeededBug {
    /// A refused `buy` still costs its player a gold piece.
    MutatesBeforeRefusing,
    /// A unit placed, made or moved leaves its sight where it was.
    StaleVisibility,
    /// Renaming a city touches its `CORE`, not its `NAME`.
    WrongTouch,
    /// A query tool raises the asker's happiness memo by one, stamps unmoved.
    QueryWrites,
    /// A purchase carried out panics.
    Panics,
}

impl SeededBug {
    /// Every seeded bug, in declaration order.
    pub const ALL: [Self; 5] = [
        Self::MutatesBeforeRefusing,
        Self::StaleVisibility,
        Self::WrongTouch,
        Self::QueryWrites,
        Self::Panics,
    ];

    /// Its name, as a replay file and the chaos command line write it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MutatesBeforeRefusing => "mutates_before_refusing",
            Self::StaleVisibility => "stale_visibility",
            Self::WrongTouch => "wrong_touch",
            Self::QueryWrites => "query_writes",
            Self::Panics => "panics",
        }
    }

    /// The bug called `name`, if there is one.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }
}

#[cfg(feature = "test-ops")]
std::thread_local! {
    /// The bug planted on this thread, if any.
    static BUG: core::cell::Cell<Option<SeededBug>> = const { core::cell::Cell::new(None) };
}

/// Whether `bug` is planted on this thread: never outside a test build.
#[inline]
#[must_use]
pub fn has(bug: SeededBug) -> bool {
    #[cfg(feature = "test-ops")]
    {
        BUG.with(|b| b.get() == Some(bug))
    }
    #[cfg(not(feature = "test-ops"))]
    {
        let _ = bug;
        false
    }
}

/// Plants `bug` in every game this thread plays until the guard is dropped, which puts back
/// what was planted before, by a panic too.
#[cfg(feature = "test-ops")]
#[must_use = "the bug is removed again when the guard is dropped"]
pub fn seed(bug: Option<SeededBug>) -> Seeded {
    Seeded(BUG.with(|b| b.replace(bug)))
}

/// A planted bug, removed when dropped ([`seed`]).
#[cfg(feature = "test-ops")]
#[derive(Debug)]
pub struct Seeded(Option<SeededBug>);

#[cfg(feature = "test-ops")]
impl Drop for Seeded {
    fn drop(&mut self) {
        let before = self.0.take();
        BUG.with(|b| b.set(before));
    }
}

/// What `Game::act` watches for its two seeded bugs: whether the action is a purchase.
#[cfg(feature = "test-ops")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Watch {
    buys: bool,
}

#[cfg(feature = "test-ops")]
impl Watch {
    /// Watches action `a`.
    pub(crate) const fn of(a: &crate::game::Action) -> Self {
        Self { buys: matches!(a, crate::game::Action::Buy(_)) }
    }

    /// After `pid`'s action was checked and, if `ok`, applied: the refused purchase that still
    /// costs a gold piece, or the one carried out that panics.
    ///
    /// # Panics
    /// With [`SeededBug::Panics`] planted, after a purchase was carried out.
    pub(crate) fn after(self, g: &mut Game, pid: PlayerId, ok: bool) {
        if !self.buys {
            return;
        }
        if !ok
            && has(SeededBug::MutatesBeforeRefusing)
            && let Some(p) = g.player_mut(pid, crate::game::derive::rev::PlayerTouch::STOCKS)
        {
            p.econ.gold -= 1.0;
        }
        assert!(
            !(ok && has(SeededBug::Panics)),
            "seeded bug: a purchase panics once it is paid for"
        );
    }
}

/// After a query tool answered `p`: with [`SeededBug::QueryWrites`] planted, `p`'s happiness
/// memo is validated and then raised by one through its `RefCell`, its stamps unmoved, so that
/// the next reads take the raised value until an input moves.
#[cfg(feature = "test-ops")]
pub(crate) fn after_query(g: &Game, p: PlayerId) {
    if has(SeededBug::QueryWrites) {
        crate::game::derive::stats::raise_happiness_for_seeded_bug(g, p);
    }
}

#[cfg(all(test, feature = "test-ops", feature = "embedded-ruleset"))]
mod tests {
    use super::{SeededBug, has, seed};
    use crate::base::ids::{PlayerId, TileIdx};
    use crate::game::core::testing;

    #[test]
    fn a_bug_is_planted_on_this_thread_until_its_guard_drops() {
        assert!(SeededBug::ALL.iter().all(|&b| !has(b)), "none by default");
        {
            let _outer = seed(Some(SeededBug::WrongTouch));
            assert!(has(SeededBug::WrongTouch) && !has(SeededBug::Panics));
            {
                let _inner = seed(None);
                assert!(!has(SeededBug::WrongTouch));
            }
            assert!(has(SeededBug::WrongTouch), "the inner guard puts the outer bug back");
            #[allow(clippy::disallowed_methods, reason = "the bug is the calling thread's alone")]
            let elsewhere = std::thread::spawn(|| has(SeededBug::WrongTouch)).join();
            assert_eq!(elsewhere.ok(), Some(false), "another thread's games have none");
        }
        assert!(!has(SeededBug::WrongTouch));
        for b in SeededBug::ALL {
            assert_eq!(SeededBug::named(b.name()), Some(b));
        }
    }

    #[test]
    fn a_wrong_touch_leaves_the_name_index_stale_for_the_oracle() {
        let mut g = testing::duel();
        let roma = testing::city(&mut g, PlayerId(0), TileIdx(22), "Roma");
        g.settle();
        assert_eq!(g.verify_caches(), Vec::<String>::new());
        let _names = g.dv.names(&g.st).entries().len();
        let _bug = seed(Some(SeededBug::WrongTouch));
        crate::game::cities::founding::write_name(&mut g, roma, "Nova Roma");
        g.settle();
        let found = g.verify_caches();
        assert!(found.iter().any(|l| l.contains("name index")), "{found:?}");
    }

    #[test]
    fn a_unit_placed_without_its_sight_leaves_visibility_stale_for_the_oracle() {
        let mut g = testing::duel();
        let u = testing::unit(&mut g, PlayerId(0), "Warrior", TileIdx(22));
        g.settle();
        assert_eq!(g.verify_caches(), Vec::<String>::new());
        let _bug = seed(Some(SeededBug::StaleVisibility));
        g.relocate_unit(u, TileIdx(27)).expect("a move");
        g.settle();
        assert!(!g.verify_caches().is_empty(), "the warrior's sight stayed behind");
    }
}

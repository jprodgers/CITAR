//! What each civilization sees (DESIGN.md 6.9).
//!
//! Package 1b-01 lands the part other systems read: per civilization, the tiles it sees now
//! ([`Visibility::sees`]), which event audiences widen by (`game.py:872-875`) and which invariant
//! VIS-1 holds to the explored tiles. Package 1c-01 ports `visibility.py`: the sight sources and
//! their footprints, the counts, the transitions and their effects (explored tiles, memory,
//! first contact, natural wonders). Until then nobody sees anything, and settle's `sync_sight`
//! only drops the dirty sources.

use crate::base::ids::{PlayerId, TileIdx};
use crate::base::sets::{BitSet, PlayerVec};
use crate::game::{Porting, pending};
use crate::state::State;

/// Each civilization's visible tiles: derived, never saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Visibility {
    visible: PlayerVec<BitSet>,
}

impl Visibility {
    /// The visibility of `st`, cold.
    #[must_use]
    pub fn new(st: &State) -> Self {
        // visibility.py:96-168: sources, footprints and counts.
        pending(Porting::Pending("1c-01"));
        Self { visible: st.players().ids().map(|_| BitSet::new()).collect() }
    }

    /// Whether `p` sees tile `t` now.
    #[must_use]
    #[inline]
    pub fn sees(&self, p: PlayerId, t: TileIdx) -> bool {
        self.visible.get(p).is_some_and(|v| v.contains(t.0))
    }

    /// The tiles `p` sees now.
    #[must_use]
    pub fn visible(&self, p: PlayerId) -> Option<&BitSet> {
        self.visible.get(p)
    }

    /// Where these counts disagree with a rebuild from `st`, one line each (the cache oracle).
    #[must_use]
    pub fn verify(&self, st: &State) -> Vec<String> {
        let cold = Self::new(st);
        if *self == cold {
            Vec::new()
        } else {
            vec!["the visible tiles differ from a cold rebuild".to_owned()]
        }
    }

    /// Lets `p` see `t`, as a test arranges what a civilization sees before the sight sources
    /// exist (package 1c-01).
    #[cfg(any(test, feature = "test-ops"))]
    pub fn reveal_for_test(&mut self, p: PlayerId, t: TileIdx) {
        if let Some(v) = self.visible.get_mut(p) {
            v.insert(t.0);
        }
    }
}

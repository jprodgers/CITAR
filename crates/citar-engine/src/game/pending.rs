//! Work a write raises and the next settle does, and the effect queue (DESIGN.md 6.4, 6.7).
//!
//! Python did this work on the spot and re-entrantly: a unit step refreshed every civilization's
//! vision, which met civilizations, which emitted events (`visibility.py:128-168`), and a changed
//! tile reassigned citizens from inside the yield code (`game.py:588-601`). Here a write only
//! flags what it made stale, and settle catches up in a fixed order, to a fixed point. Nothing
//! here is ever saved: it is empty at every settle point, where saves are taken.

use std::collections::BTreeSet;

use crate::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use crate::base::sets::BitSet;

/// A vision source whose footprint may have changed, or a place where sight or contact must be
/// looked at again (DESIGN.md 6.9). Package 1c-01 brings them up to date in `sync_sight`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SightSource {
    /// A unit moved, appeared, left, changed hands or boarded.
    Unit(UnitId),
    /// A city appeared, went, changed hands or changed its tiles.
    City(CityId),
    /// A city-state's ally changed: who sees its cities.
    Allies(PlayerId),
    /// A player's spies moved or changed what they do.
    Spies(PlayerId),
    /// Every source of a player, as when it is eliminated or revived.
    Civ(PlayerId),
    /// What blocks sight changed on a tile, for every unit that sees across it.
    Area(TileIdx),
    /// A tile changed hands, which may bring its new owner into contact with those who see it.
    Tile(TileIdx),
}

/// What the next settle must do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingWork {
    /// Cities to reassign citizens in, by raw id.
    recheck: BitSet,
    /// Vision sources to bring up to date, in a fixed order.
    sight: BTreeSet<SightSource>,
    /// Makes citizen reassignment leave its flags, so a test can make settle run out of passes.
    #[cfg(test)]
    pub(crate) stubborn: bool,
}

impl PendingWork {
    /// Nothing to do.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether there is nothing to do: true at every settle point (invariant PEND-1).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recheck.is_empty() && self.sight.is_empty()
    }

    /// Flags a city for a citizen recheck.
    pub fn flag_city(&mut self, c: CityId) {
        self.recheck.insert(c.get());
    }

    /// Marks a vision source dirty.
    pub fn flag_sight(&mut self, s: SightSource) {
        self.sight.insert(s);
    }

    /// Whether any city is flagged.
    #[must_use]
    pub fn any_recheck(&self) -> bool {
        !self.recheck.is_empty()
    }

    /// The flagged cities, in id order, unflagging them.
    pub fn take_recheck(&mut self) -> Vec<CityId> {
        let out = self.recheck.iter().filter_map(CityId::new).collect();
        self.recheck.clear();
        out
    }

    /// Whether any vision source is dirty.
    #[must_use]
    pub fn any_sight(&self) -> bool {
        !self.sight.is_empty()
    }

    /// The dirty vision sources, in order, cleaning them.
    pub fn take_sight(&mut self) -> Vec<SightSource> {
        core::mem::take(&mut self.sight).into_iter().collect()
    }

    /// Unflags every city.
    pub fn clear_recheck(&mut self) {
        self.recheck.clear();
    }

    /// Cleans every vision source.
    pub fn clear_sight(&mut self) {
        self.sight.clear();
    }
}

/// A follow-up a derived reaction asks for, which writes the state when it is applied
/// (DESIGN.md 6.4). The order of the variants, then of their fields, is the order the queue
/// drains in: `(kind, civ, other, tile)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effect {
    /// Two players meet (`Game.meet`, `game.py:694-701`); the lower id first.
    Meet {
        /// One side.
        a: PlayerId,
        /// The other.
        b: PlayerId,
    },
}

impl Effect {
    /// A meeting of `a` and `b`, the same whichever way round they are given.
    #[must_use]
    pub fn meet(a: PlayerId, b: PlayerId) -> Self {
        Self::Meet { a: a.min(b), b: a.max(b) }
    }
}

/// The effects waiting to be applied, drained in [`Effect`] order (DESIGN.md 6.4). An effect
/// queued twice before it is applied is applied once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectQueue {
    queue: BTreeSet<Effect>,
}

impl EffectQueue {
    /// How many effects one drain may apply before it is a runaway, which is a bug: every
    /// effect applied moves the game toward a state with fewer effects to come (a meeting
    /// happens once), so a real game needs a few per player pair at most.
    pub const LIMIT: u32 = 1 << 16;

    /// Queues an effect.
    pub fn push(&mut self, e: Effect) {
        self.queue.insert(e);
    }

    /// The first effect in drain order, taken off the queue.
    pub fn pop(&mut self) -> Option<Effect> {
        self.queue.pop_first()
    }

    /// Whether nothing waits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_drain_in_kind_then_player_order_once_each() {
        let mut q = EffectQueue::default();
        q.push(Effect::meet(PlayerId(3), PlayerId(1)));
        q.push(Effect::meet(PlayerId(0), PlayerId(2)));
        q.push(Effect::meet(PlayerId(1), PlayerId(3)));
        assert_eq!(q.pop(), Some(Effect::Meet { a: PlayerId(0), b: PlayerId(2) }));
        assert_eq!(q.pop(), Some(Effect::Meet { a: PlayerId(1), b: PlayerId(3) }));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn pending_work_drains_in_id_order() {
        let mut p = PendingWork::new();
        assert!(p.is_empty());
        for n in [7, 2, 7] {
            p.flag_city(CityId::new(n).unwrap_or(CityId::FIRST));
        }
        p.flag_sight(SightSource::Tile(TileIdx(4)));
        p.flag_sight(SightSource::Unit(UnitId::FIRST));
        assert!(!p.is_empty() && p.any_recheck() && p.any_sight());
        assert_eq!(p.take_recheck().iter().map(|c| c.get()).collect::<Vec<_>>(), [2, 7]);
        assert_eq!(
            p.take_sight(),
            [SightSource::Unit(UnitId::FIRST), SightSource::Tile(TileIdx(4))]
        );
        assert!(p.is_empty());
    }
}

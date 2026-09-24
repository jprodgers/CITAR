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
    /// What blocks sight changed on a tile, for every unit that sees across it. The heights and
    /// the line-of-sight cache were updated when it changed; the units near it and a natural
    /// wonder on it are looked at at the sync.
    Area(TileIdx),
    /// A tile changed hands, which may bring its new owner into contact with those who see it,
    /// or (when the ruleset's sight uniques read tiles) what a unit on it sees changed.
    Tile(TileIdx),
    /// Everything a player sees, and everything of its that others see, must be checked for
    /// first contact again: two players forgot they had met.
    Contact(PlayerId),
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
    /// Two players meet (`Game.meet`, `game.py:694-701`). Sight queues it viewer first, the
    /// civilization that saw something of the other's, as Python's refresh met them viewer by
    /// viewer in id order (`visibility.py:152-165`): the queue's `(a, b)` order is that order,
    /// and the announcement names `a` first.
    Meet {
        /// The side that saw the other, or the lower id (`Effect::meet`).
        a: PlayerId,
        /// The other.
        b: PlayerId,
    },
    /// A major discovers the natural wonder it sees on a tile
    /// (`visibility._discover_natural_wonders`, `visibility.py:171-198`).
    Wonder {
        /// The major.
        civ: PlayerId,
        /// The wonder's tile.
        tile: TileIdx,
    },
}

impl Effect {
    /// A meeting of `a` and `b` with no viewer, the same whichever way round they are given:
    /// the lower id first.
    #[must_use]
    pub fn meet(a: PlayerId, b: PlayerId) -> Self {
        Self::Meet { a: a.min(b), b: a.max(b) }
    }
}

/// The effects waiting to be applied, drained in [`Effect`] order (DESIGN.md 6.4). An effect
/// queued twice before it is applied is applied once.
///
/// Effects are idempotent per key: applying one whose work is done does nothing (two players
/// meet once; a tile is explored once, and its memory snapshot is taken once per sighting). So
/// every effect applied moves the game toward a state with fewer effects to come, and one settle
/// applies at most a few per tile per civilization and per player pair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectQueue {
    queue: BTreeSet<Effect>,
}

impl EffectQueue {
    /// The kinds of effect keyed by a tile and a civilization (explored bit, memory snapshot,
    /// natural wonder, contact across a border), with room for the ones systems add.
    const PER_TILE: u64 = 8;

    /// The kinds keyed by two players (a meeting), with room likewise.
    const PER_PAIR: u64 = 8;

    /// The fewest effects a runaway takes, whatever the map.
    const FLOOR: u64 = 1 << 16;

    /// How many effects one settle may apply on a map of `tiles` tiles with `players` players
    /// before it is a runaway, which is a bug: every tile's effects for every civilization and
    /// every pair's, twice over. A whole map revealed to a civilization is one tile effect of
    /// each kind per tile, far below it.
    #[must_use]
    pub fn limit(tiles: usize, players: usize) -> u64 {
        let wide = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        let (t, p) = (wide(tiles), wide(players));
        let per_settle = Self::PER_TILE
            .saturating_mul(t)
            .saturating_mul(p)
            .saturating_add(Self::PER_PAIR.saturating_mul(p).saturating_mul(p));
        per_settle.saturating_mul(2).max(Self::FLOOR)
    }

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
    fn the_runaway_limit_grows_with_the_map_and_the_players() {
        // The largest map, every tile revealed to every civilization, is no runaway.
        let (tiles, players) = (256 * 256, 24);
        let reveal = u64::try_from(tiles * players).unwrap_or(u64::MAX);
        assert!(EffectQueue::limit(tiles, players) > 4 * reveal);
        assert_eq!(EffectQueue::limit(0, 0), 1 << 16, "a floor for tiny games");
        assert!(EffectQueue::limit(80, 3) >= 1 << 16);
        assert_eq!(EffectQueue::limit(usize::MAX, usize::MAX), u64::MAX, "saturates");
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

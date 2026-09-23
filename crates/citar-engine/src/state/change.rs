//! What a write to [`State`](super::State) changed, for the caches to catch up on (DESIGN.md 6.4).
//!
//! Every setter whose consequences need the new state (ownership, placement, a city appearing or
//! disappearing, a tile changing, a seat or a player's fate) returns a `#[must_use]` [`Change`].
//! `game::mutate` hands it to `Game::changed`, which bumps the revisions, asks the derived layer
//! for the effects, and queues them. A `Change` dropped on the floor is a write the memos never
//! hear about, so `unused_must_use` and `clippy::let_underscore_must_use` are denied across the
//! workspace.
//!
//! Python had no such record: every writer called `g.invalidate()` (`game.py:565-609`), which
//! cleared every cache at once.

use smallvec::SmallVec;

use crate::base::ids::{CityId, PlayerId, TileIdx, UnitId};

/// Who holds a tile: the player that owns it, and the city that may work it.
///
/// They move together when a city claims or releases a tile, but not always: a scenario may give
/// a tile an owner and no city (`scenario.py:378`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TileClaim {
    /// The owning player.
    pub owner: Option<PlayerId>,
    /// The city that owns (can work) the tile (`state.py:78-80`).
    pub city: Option<CityId>,
}

impl TileClaim {
    /// Nobody's tile.
    pub const NONE: Self = Self { owner: None, city: None };

    /// A tile claimed by `city` of `owner`.
    #[must_use]
    pub const fn city(owner: PlayerId, city: CityId) -> Self {
        Self { owner: Some(owner), city: Some(city) }
    }
}

/// One write, as the derived layer needs to hear of it.
///
/// Each variant names what moved and, where the state after the write no longer says it, what it
/// moved from: a removed unit's owner and tile, a city's old owner.
#[must_use = "a Change carries the revision bumps and effects of a write: pass it to Game::changed"]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    /// Something a tile yields or costs changed: its improvement, route, resource, river or
    /// pillage state, or its build queue.
    TileInput(TileIdx),
    /// Something that also blocks or lifts sight changed: its base terrain, its features (hills,
    /// forest) or its natural wonder. It implies [`TileInput`](Self::TileInput).
    TileHeight(TileIdx),
    /// The tile changed hands, or changed the city that works it.
    TileOwner {
        /// The tile.
        t: TileIdx,
        /// Who held it before.
        old: TileClaim,
        /// Who holds it now.
        new: TileClaim,
    },
    /// A unit appeared (`from` is `None`), moved, or boarded or left a carrier on the same tile.
    UnitPlaced {
        /// The unit.
        u: UnitId,
        /// Its owner.
        owner: PlayerId,
        /// Where it was, if it was on the map.
        from: Option<TileIdx>,
        /// Where it is now.
        to: TileIdx,
    },
    /// A unit changed hands.
    UnitOwner {
        /// The unit.
        u: UnitId,
        /// Its owner before.
        old: PlayerId,
        /// Its owner now.
        new: PlayerId,
    },
    /// A unit left the game.
    UnitRemoved {
        /// The unit, no longer in the store.
        u: UnitId,
        /// Its last owner.
        owner: PlayerId,
        /// Its last tile.
        at: TileIdx,
    },
    /// A city was founded, or put into the store by a scenario.
    CityAdded(CityId),
    /// A city left the game.
    CityRemoved {
        /// The city, no longer in the store.
        c: CityId,
        /// Its last owner.
        owner: PlayerId,
        /// Its tile.
        at: TileIdx,
    },
    /// The set of tiles a city owns changed as a whole (a border rebuild, a scenario).
    CityTiles(CityId),
    /// A city changed hands.
    CityOwner {
        /// The city.
        c: CityId,
        /// Its owner before.
        old: PlayerId,
        /// Its owner now.
        new: PlayerId,
    },
    /// A relation between two players changed: war, a treaty, an embassy, open borders.
    Diplo {
        /// One side.
        a: PlayerId,
        /// The other side.
        b: PlayerId,
    },
    /// Two players met.
    Met {
        /// One side.
        a: PlayerId,
        /// The other side.
        b: PlayerId,
    },
    /// A city-state's ally changed.
    Alliance {
        /// The city-state.
        cs: PlayerId,
        /// Its ally before.
        old: Option<PlayerId>,
        /// Its ally now.
        new: Option<PlayerId>,
    },
    /// Where a player's spies stand, or what they do, changed: they see the city they are in.
    Spy(PlayerId),
    /// A seat changed: its controller, handicap, automatic decisions or difficulty.
    Seat(PlayerId),
    /// A player was eliminated or came back.
    PlayerAlive(PlayerId),
    /// The turn, or whose turn it is, changed.
    Turn,
    /// A civilization, leader or city name changed, which the event name index reads.
    Names,
}

/// The changes of one write that moved several things, in the order they happened: a unit and
/// the units it carries, a city and its tiles.
#[must_use = "Changes carry the revision bumps and effects of a write: pass each to Game::changed"]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes(SmallVec<[Change; 2]>);

impl Changes {
    /// No changes.
    pub fn new() -> Self {
        Self(SmallVec::new())
    }

    /// Adds one.
    pub fn push(&mut self, c: Change) {
        self.0.push(c);
    }

    /// Adds all of `more`, after these.
    pub fn append(&mut self, more: Changes) {
        self.0.extend(more.0);
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The changes, in order.
    pub fn as_slice(&self) -> &[Change] {
        &self.0
    }
}

impl From<Change> for Changes {
    fn from(c: Change) -> Self {
        let mut out = Self::new();
        out.push(c);
        out
    }
}

impl IntoIterator for Changes {
    type Item = Change;
    type IntoIter = smallvec::IntoIter<[Change; 2]>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_stays_small() {
        // It travels through the effect queue by value.
        assert!(size_of::<Change>() <= 24, "{} bytes", size_of::<Change>());
    }

    #[test]
    fn changes_keep_their_order() {
        let mut cs = Changes::from(Change::Turn);
        cs.push(Change::Names);
        let mut more = Changes::new();
        more.push(Change::Seat(PlayerId(2)));
        cs.append(more);
        assert_eq!(cs.len(), 3);
        assert_eq!(cs.as_slice(), &[Change::Turn, Change::Names, Change::Seat(PlayerId(2))]);
        assert_eq!(cs.into_iter().next_back(), Some(Change::Seat(PlayerId(2))));
    }
}

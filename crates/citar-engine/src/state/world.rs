//! What belongs to the world rather than to a player: founded religions, built world wonders,
//! the United Nations and barbarian camps (DESIGN.md 4.6).
//!
//! Replaces `GameState.religions`, `wonders_built`, `un` and `camps` (`state.py:355-362`):
//! - religions are kept in founding order and named by [`ReligionId`], their index; Python keyed
//!   them by name and stored an `enhanced` flag it never set (enhancement is read from the
//!   beliefs);
//! - the UN state has explicit defaults instead of being created on first read
//!   (`victory.py:97-104`), and its tally is keyed by player id rather than by civilization name,
//!   which broke after a rename;
//! - camps are keyed by [`CampId`].

use std::collections::BTreeMap;

use crate::base::ids::{
    BeliefId, BuildingId, CampId, CityId, PlayerId, ReligionId, RulesReligionId, TileIdx, Turn,
};
use crate::base::sets::{BeliefSet, PlayerSet};

/// What a founded religion is called: a pantheon by its belief, a religion by its row of
/// `religions.json` (`religion.py:509, 652`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReligionName {
    Pantheon(BeliefId),
    Religion(RulesReligionId),
}

/// A founded pantheon or religion (`religion.py:473-477`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Religion {
    pub name: ReligionName,
    /// The name it is shown under, which its founder may have chosen.
    pub display: Box<str>,
    pub founder: PlayerId,
    /// Founder and enhancer beliefs.
    pub founder_beliefs: BeliefSet,
    /// Pantheon and follower beliefs.
    pub follower_beliefs: BeliefSet,
    /// Whether an inquisitor has blocked its holy city's pressure (`religion.py:765-773`).
    pub blocked_holy: bool,
}

/// One result of a world leader vote (`victory.py:216-217`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnResult {
    pub turn: Turn,
    /// Votes per candidate, most first.
    pub tally: Vec<(PlayerId, u16)>,
    pub votes_needed: u16,
    pub winner: Option<PlayerId>,
}

/// The United Nations (`victory.py:97-104`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Un {
    /// The turn of the next vote, once one is scheduled.
    pub next_vote: Option<Turn>,
    /// This vote's ballots so far: each voter's candidate, `None` for an abstention.
    pub votes: BTreeMap<PlayerId, Option<PlayerId>>,
    /// The last vote's result.
    pub results: Option<UnResult>,
    /// Everyone who has ever won a vote.
    pub won: PlayerSet,
    /// The turn the last vote was counted.
    pub processed_turn: Option<Turn>,
}

/// A barbarian encampment (`barbarians.py:190`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Camp {
    pub tile: TileIdx,
    /// Turns to its next spawn, or, once destroyed, until it is forgotten.
    pub countdown: i16,
    /// Units it has spawned, from -1.
    pub spawned: i16,
    pub destroyed: bool,
}

impl Camp {
    /// A new camp on `tile`.
    #[must_use]
    pub const fn new(tile: TileIdx) -> Self {
        Self { tile, countdown: 0, spawned: -1, destroyed: false }
    }
}

/// The world's shared state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct World {
    /// Founded religions and pantheons; a [`ReligionId`] is an index here.
    pub religions: Vec<Religion>,
    /// Which city built each world wonder.
    pub wonders_built: BTreeMap<BuildingId, CityId>,
    pub un: Un,
    pub camps: BTreeMap<CampId, Camp>,
}

impl World {
    /// A founded religion.
    #[must_use]
    pub fn religion(&self, r: ReligionId) -> Option<&Religion> {
        self.religions.get(usize::from(r.0))
    }

    /// The religion or pantheon with this name, if founded.
    #[must_use]
    pub fn religion_named(&self, name: ReligionName) -> Option<ReligionId> {
        self.religions
            .iter()
            .position(|r| r.name == name)
            .and_then(|i| u8::try_from(i).ok())
            .map(ReligionId)
    }

    /// The camp on a tile that is still standing (`barbarians.py:112-114`).
    #[must_use]
    pub fn camp_at(&self, t: TileIdx) -> Option<CampId> {
        self.camps.iter().find(|(_, c)| c.tile == t && !c.destroyed).map(|(&id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn religions_are_found_by_name_and_index() {
        let mut w = World::default();
        w.religions.push(Religion {
            name: ReligionName::Pantheon(BeliefId(4)),
            display: "Sun God".into(),
            founder: PlayerId(0),
            founder_beliefs: BeliefSet::new(),
            follower_beliefs: [BeliefId(4)].into_iter().collect(),
            blocked_holy: false,
        });
        assert_eq!(w.religion_named(ReligionName::Pantheon(BeliefId(4))), Some(ReligionId(0)));
        assert_eq!(w.religion_named(ReligionName::Religion(RulesReligionId(0))), None);
        assert_eq!(w.religion(ReligionId(0)).map(|r| r.founder), Some(PlayerId(0)));
        assert!(w.religion(ReligionId(1)).is_none());
    }

    #[test]
    fn a_destroyed_camp_is_not_at_its_tile() {
        let mut w = World::default();
        let id = CampId::FIRST;
        w.camps.insert(id, Camp::new(TileIdx(7)));
        assert_eq!(w.camp_at(TileIdx(7)), Some(id));
        if let Some(c) = w.camps.get_mut(&id) {
            c.destroyed = true;
        }
        assert_eq!(w.camp_at(TileIdx(7)), None);
    }
}

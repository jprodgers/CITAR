//! What `basic-1` remembers between turns, kept in the seat's `DriverMemory` (DESIGN.md
//! P2.3.4): the dicts of `BasicBot.__init__` (basic.py:684-694) that outlive a turn, typed, per
//! seat, and saved with the game, where Python lost them on every save.
//!
//! Python keyed most of them by player because one bot object could play several seats; a
//! seat's memory needs no key. Not kept: `_sites_cache` and `_bv_cache`, caches that are gone
//! (P2.3.9, fixes 5 and 6), and `_space_res`, a fact of the ruleset (`Resolved`'s docs).
//!
//! The encoding is `serde_json` of [`Memory`] (fields in declaration order, map keys in order),
//! so the same memory always gives the same bytes, which matters because the engine digests
//! them. A memory of another kind or version (a seat that changed bot version mid-game, an
//! older schema), or one that does not decode, starts fresh.
//!
//! [`Memory::prune`] runs at the start of each turn, so that memory stays far under
//! `DriverMemory::MAX_LEN` however long the game: what names a unit or a city the seat no longer
//! has goes, and so do records past the turns they count for.

use std::collections::BTreeMap;

use citar_engine::base::ids::{CityId, PlayerId, TileIdx, Turn, UnitId};
use citar_engine::game::Game;
use citar_engine::state::players::{DriverMemory, DriverTooLarge};
use serde::{Deserialize, Serialize};

/// The `DriverMemory` kind of `basic-1`'s memory.
pub const MEMORY_KIND: u16 = 1;

/// The version of its format.
pub const MEMORY_VERSION: u16 = 1;

/// A war being prepared (`_war_prep[pid]`, basic.py:2452-2475).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarPrep {
    /// Whom.
    pub player: PlayerId,
    /// Since when.
    pub since: Turn,
    /// The city tile the war will be fought for, once chosen.
    pub target: Option<TileIdx>,
    /// Where the army gathers, once chosen.
    pub rally: Option<TileIdx>,
}

/// A war being fought for a city (`_war_plan[pid]`, basic.py:2294-2338).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarPlan {
    /// The target city's tile.
    pub city: TileIdx,
    /// Since when.
    pub since: Turn,
    /// Whether the army advances on the city, rather than gathering at the rally point.
    pub advance: bool,
    /// The turn the plan was last brought up to date.
    pub checked: Option<Turn>,
    /// Where the army gathers.
    pub rally: Option<TileIdx>,
    /// Whether the siege has worn the city down enough for melee units to close in.
    pub siege_ready: bool,
}

/// What `basic-1` remembers between turns for its seat.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub war_prep: Option<WarPrep>,
    pub war_plan: Option<WarPlan>,
    /// Settler -> its escort (`_escorts`).
    pub escorts: BTreeMap<UnitId, UnitId>,
    /// City -> the unit that garrisons it (`_garrisons`).
    pub garrisons: BTreeMap<CityId, UnitId>,
    /// Site -> times a settler turned back from it (`_retreats`).
    pub retreats: BTreeMap<TileIdx, u8>,
    /// Site -> the turn a settler failed to reach it (`_bad_sites`).
    pub bad_sites: BTreeMap<TileIdx, Turn>,
    /// The city tile where a settler waits for an escort (`_need_escort`).
    pub need_escort: Option<TileIdx>,
    /// City -> the turn a work boat was last queued there (`_boat_turn`).
    pub boat_turns: BTreeMap<CityId, Turn>,
}

impl Memory {
    /// The memory a seat kept: a fresh one for a kind or version this bot does not know, or
    /// bytes that do not decode.
    #[must_use]
    pub fn decode(m: &DriverMemory) -> Self {
        if m.kind() != MEMORY_KIND || m.version() != MEMORY_VERSION {
            return Self::default();
        }
        serde_json::from_slice(m.bytes()).unwrap_or_default()
    }

    /// The memory as the seat keeps it.
    ///
    /// # Errors
    /// Over `DriverMemory::MAX_LEN` bytes, which pruning keeps it far below.
    pub fn encode(&self) -> Result<DriverMemory, DriverTooLarge> {
        // Plain data with integer keys: serialising cannot fail, and an empty memory stands in if
        // it ever did.
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        DriverMemory::new(MEMORY_KIND, MEMORY_VERSION, bytes)
    }

    /// Drops what no longer counts, at the start of `pid`'s turn in `g` (DESIGN.md P2.3.4):
    /// - escorts whose settler or escort is no longer one of the seat's units (Python's
    ///   `_follow` dropped them as it met them);
    /// - garrisons of cities the seat no longer has, or held by units it no longer has (Python
    ///   rebuilt the dict each turn, basic.py:1774);
    /// - boat turns of cities it no longer has, and those more than `boat_retry_turns` old, after
    ///   which a city may queue a boat as if it never had (basic.py:1455);
    /// - blacklisted sites `blacklist_turns` old or more, which the bot no longer avoids
    ///   (basic.py:1090);
    /// - retreat counts of 0, which read as no entry (basic.py:1867-1871 resets a count to 0).
    ///
    /// War plans name players and tiles, which the war code weighs each turn: it forgets a plan
    /// whose city is no longer an enemy's as the units' turn starts (`units::war_plan`), and a
    /// prepared war is diplomacy's (package 2-05). A settler's waiting tile is the advisor's to
    /// read. So they stay.
    pub fn prune(&mut self, g: &Game, pid: PlayerId, blacklist_turns: i32, boat_retry_turns: i32) {
        let turn = g.turn();
        let ours_u = |u: UnitId| g.unit(u).is_some_and(|x| x.owner() == pid);
        let ours_c = |c: CityId| g.city(c).is_some_and(|x| x.owner() == pid);
        self.escorts.retain(|&settler, &mut escort| ours_u(settler) && ours_u(escort));
        self.garrisons.retain(|&city, &mut unit| ours_c(city) && ours_u(unit));
        self.boat_turns.retain(|&city, &mut t| {
            ours_c(city) && i64::from(turn) - i64::from(t) <= i64::from(boat_retry_turns)
        });
        self.bad_sites
            .retain(|_, &mut t| i64::from(turn) - i64::from(t) < i64::from(blacklist_turns));
        self.retreats.retain(|_, &mut n| n > 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_round_trips_and_a_foreign_one_starts_fresh() {
        let mut m = Memory {
            war_prep: Some(WarPrep {
                player: PlayerId(2),
                since: 40,
                target: None,
                rally: Some(TileIdx(7)),
            }),
            need_escort: Some(TileIdx(3)),
            ..Memory::default()
        };
        m.retreats.insert(TileIdx(9), 2);
        let unit = UnitId::new(5).expect("an id");
        let city = CityId::new(1).expect("an id");
        m.garrisons.insert(city, unit);
        let enc = m.encode().expect("small");
        assert_eq!((enc.kind(), enc.version()), (MEMORY_KIND, MEMORY_VERSION));
        assert_eq!(Memory::decode(&enc), m);
        assert_eq!(m.encode().expect("small"), enc, "the same memory, the same bytes");
        let other = DriverMemory::new(9, 1, enc.bytes().to_vec()).expect("small");
        assert_eq!(Memory::decode(&other), Memory::default());
        let newer = DriverMemory::new(MEMORY_KIND, 2, enc.bytes().to_vec()).expect("small");
        assert_eq!(Memory::decode(&newer), Memory::default());
        let junk = DriverMemory::new(MEMORY_KIND, MEMORY_VERSION, b"{".to_vec()).expect("small");
        assert_eq!(Memory::decode(&junk), Memory::default());
    }
}

//! Units, and the two indexes that find them: who stands on a tile, and who owns what
//! (DESIGN.md 4.4).
//!
//! Replaces `state.py:110-144` (`Unit`) and the unit bookkeeping of `Game`: the occupancy index
//! `_occ` (`game.py:716-785`: `_rebuild_occupancy`, `create_unit`, `remove_unit`, `place_unit`)
//! and `change_owner` (`game.py:796-804`, whose order resets are the conquest rule's business).
//!
//! A unit's owner, tile and carrier are private: only [`Units`] moves them, and each of its
//! operations returns the [`Change`] the caches need. The indexes are structural, rebuilt on load
//! and never saved.

use core::fmt;

use smallvec::SmallVec;

use super::change::{Change, Changes};
use super::store::{Entity, EntityStore, StoreError};
use crate::base::ids::{
    AbilityKey, BaseUnitId, CampId, CityId, Id, PlayerId, ReligionId, TileIdx, Turn, UnitId,
};
use crate::base::sets::{PlayerVec, PromotionSet};

/// A standing order (Python's `Unit.activity`, `state.py:122`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Fortify,
    /// Fortified until healed.
    FortifyHeal,
    Sleep,
    /// Asleep until healed.
    SleepHeal,
    Heal,
    Build,
    Goto,
    Explore,
    Automate,
    AirSweep,
}

impl Activity {
    /// Every activity.
    pub const ALL: [Self; 10] = [
        Self::Fortify,
        Self::FortifyHeal,
        Self::Sleep,
        Self::SleepHeal,
        Self::Heal,
        Self::Build,
        Self::Goto,
        Self::Explore,
        Self::Automate,
        Self::AirSweep,
    ];

    /// The name Python saved and the views show: `fortify_heal`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fortify => "fortify",
            Self::FortifyHeal => "fortify_heal",
            Self::Sleep => "sleep",
            Self::SleepHeal => "sleep_heal",
            Self::Heal => "heal",
            Self::Build => "build",
            Self::Goto => "goto",
            Self::Explore => "explore",
            Self::Automate => "automate",
            Self::AirSweep => "air_sweep",
        }
    }

    /// The activity called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// An explorer's own memory: the target it is heading for and where it stood on its last turns,
/// so it can tell when fog has it going back and forth (`automation.py:236-285`). Python kept
/// both in `Player.flags` keyed by unit id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExploreMemory {
    /// The tile it is exploring toward.
    pub target: Option<TileIdx>,
    /// Its tile at the end of its last few explore orders, oldest first, at most four.
    pub recent: SmallVec<[TileIdx; 4]>,
}

/// One unit (`state.py:110-144`).
///
/// Dropped from Python: `build` and `due_heal`, which nothing read, and `status`, whose one
/// value is [`set_up`](Self::set_up).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unit {
    id: UnitId,
    /// Its row of `units.json`.
    pub base: BaseUnitId,
    owner: PlayerId,
    tile: TileIdx,
    carried_by: Option<UnitId>,
    pub hp: i16,
    /// In move-scale units (`game.json` `move_scale` to a movement point).
    pub moves: i32,
    pub xp: i32,
    pub promotions: PromotionSet,
    /// Promotions bought with XP, which sets the next XP threshold.
    pub promotion_count: u8,
    /// Free promotion picks.
    pub pending_promotions: u8,
    /// Turns fortified: 0, 1, then 2 and more.
    pub fortify: u8,
    pub activity: Option<Activity>,
    pub goto: Option<TileIdx>,
    /// The route fixed when the move order was given, followed and never re-planned.
    pub path: Vec<TileIdx>,
    /// Turns a move order has been held up without progress.
    pub order_wait: u8,
    /// Attacks made this turn.
    pub attacks: u8,
    /// Interceptions made this turn.
    pub interceptions: u8,
    pub acted: bool,
    /// Set up to fire (siege units): Python's `"Set Up"` status.
    pub set_up: bool,
    pub name: Option<Box<str>>,
    /// A barbarian's home camp.
    pub camp: Option<CampId>,
    pub created_turn: Turn,
    /// The religion a religious unit carries.
    pub religion: Option<ReligionId>,
    pub religious_strength: i16,
    pub religious_strength_lost: i16,
    /// Times each limited ability has been used, sorted by key. Python keyed a dict by
    /// `"placeholder|params"` text (`units.py:417, 472`).
    pub abilities_used: SmallVec<[(AbilityKey, u8); 2]>,
    /// The city that trained it.
    pub origin_city: Option<CityId>,
    pub original_owner: Option<PlayerId>,
    /// A civilian recaptured from barbarians: the civilization it may be given back to.
    pub return_offer: Option<PlayerId>,
    pub explore: ExploreMemory,
}

impl Unit {
    /// A fresh unit at full health, with no moves, orders or history, not yet in any store.
    #[must_use]
    pub fn new(
        id: UnitId,
        base: BaseUnitId,
        owner: PlayerId,
        tile: TileIdx,
        created_turn: Turn,
    ) -> Self {
        Self {
            id,
            base,
            owner,
            tile,
            carried_by: None,
            hp: 100,
            moves: 0,
            xp: 0,
            promotions: PromotionSet::new(),
            promotion_count: 0,
            pending_promotions: 0,
            fortify: 0,
            activity: None,
            goto: None,
            path: Vec::new(),
            order_wait: 0,
            attacks: 0,
            interceptions: 0,
            acted: false,
            set_up: false,
            name: None,
            camp: None,
            created_turn,
            religion: None,
            religious_strength: 0,
            religious_strength_lost: 0,
            abilities_used: SmallVec::new(),
            origin_city: None,
            original_owner: None,
            return_offer: None,
            explore: ExploreMemory::default(),
        }
    }

    /// The same unit, carried by `carrier`: for units read from a save or converted from Python,
    /// before they are in a store. [`Units::from_units`] checks the link; a unit already in a
    /// store boards through [`Units::board`], which reports the change.
    #[must_use]
    pub fn with_carrier(mut self, carrier: Option<UnitId>) -> Self {
        self.carried_by = carrier;
        self
    }

    /// Its id.
    #[must_use]
    #[inline]
    pub const fn id(&self) -> UnitId {
        self.id
    }

    /// Its owner.
    #[must_use]
    #[inline]
    pub const fn owner(&self) -> PlayerId {
        self.owner
    }

    /// Its tile.
    #[must_use]
    #[inline]
    pub const fn tile(&self) -> TileIdx {
        self.tile
    }

    /// The unit carrying it (an aircraft on a carrier), if any.
    #[must_use]
    #[inline]
    pub const fn carried_by(&self) -> Option<UnitId> {
        self.carried_by
    }

    /// How often it has used an ability.
    #[must_use]
    pub fn ability_uses(&self, key: AbilityKey) -> u8 {
        self.abilities_used.iter().find(|(k, _)| *k == key).map_or(0, |&(_, n)| n)
    }

    /// Counts one more use of an ability, keeping the list sorted by key.
    pub fn use_ability(&mut self, key: AbilityKey) {
        match self.abilities_used.binary_search_by_key(&key, |&(k, _)| k) {
            Ok(i) => self.abilities_used[i].1 = self.abilities_used[i].1.saturating_add(1),
            Err(i) => self.abilities_used.insert(i, (key, 1)),
        }
    }
}

impl Entity for Unit {
    type Id = UnitId;

    fn id(&self) -> UnitId {
        self.id
    }
}

/// Why a unit operation was refused. Nothing is changed when one is.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UnitsError {
    /// There is no such unit.
    #[error("there is no unit {0}")]
    NoSuchUnit(UnitId),
    /// The tile is not on the map.
    #[error("tile {0} is not on the map")]
    OffMap(TileIdx),
    /// The unit's id was used before.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A new unit must start uncarried; it boards afterwards.
    #[error("unit {0} cannot be spawned already carried")]
    SpawnedCarried(UnitId),
    /// A unit cannot carry itself.
    #[error("unit {0} cannot carry itself")]
    SelfCarry(UnitId),
    /// The carrier is on another tile.
    #[error("unit {unit} is on tile {at}, its carrier {carrier} on another")]
    NotTogether {
        /// The unit boarding.
        unit: UnitId,
        /// Where it is.
        at: TileIdx,
        /// The carrier.
        carrier: UnitId,
    },
    /// Carriers do not nest: a carried unit cannot carry, and a carrier cannot be carried.
    #[error("unit {0} cannot both carry and be carried")]
    Nested(UnitId),
    /// The indexes disagree with the units (a check, never a refusal).
    #[error("unit indexes are inconsistent: {0}")]
    Corrupt(String),
}

/// The raw id meaning "no unit" in the occupancy lists: ids start at 1.
const NIL: u32 = 0;

/// The unit store with its occupancy lists and owner lists (DESIGN.md 4.4).
///
/// - `occ_head[t]` is the lowest id of the units on tile `t`, and `occ_next[u]` the next id on
///   the same tile, so each tile's units form a list in ascending id order;
/// - `by_owner[p]` holds a player's unit ids, ascending.
#[derive(Clone, Default)]
pub struct Units {
    store: EntityStore<UnitId, Unit>,
    occ_head: Vec<u32>,
    occ_next: Vec<u32>,
    by_owner: PlayerVec<Vec<UnitId>>,
}

impl Units {
    /// No units, on a map of `tiles` tiles.
    #[must_use]
    pub fn new(tiles: u32) -> Self {
        Self {
            store: EntityStore::new(),
            occ_head: vec![NIL; tiles as usize],
            occ_next: Vec::new(),
            by_owner: PlayerVec::new(),
        }
    }

    /// These units, in ascending id order, on a map of `tiles` tiles, with their indexes built.
    ///
    /// Refused if the ids are out of order, a unit stands off the map, or a carrier link is broken
    /// (a missing or distant carrier, or carriers carrying carriers).
    pub fn from_units(
        units: impl IntoIterator<Item = Unit>,
        tiles: u32,
    ) -> Result<Self, UnitsError> {
        let store = EntityStore::try_from_values(units)?;
        let mut out = Self { store, ..Self::new(tiles) };
        out.rebuild(tiles)?;
        Ok(out)
    }

    /// Rebuilds the occupancy and owner lists from the units, on a map of `tiles` tiles, and
    /// checks the carrier links. A refusal leaves the lists empty.
    pub fn rebuild(&mut self, tiles: u32) -> Result<(), UnitsError> {
        self.occ_head.clear();
        self.occ_head.resize(tiles as usize, NIL);
        self.occ_next.clear();
        self.by_owner.clear();
        let ids = self.store.ids();
        // Every tile is checked before any list is built.
        for u in self.store.values() {
            if u.tile.0 as usize >= self.occ_head.len() {
                return Err(UnitsError::OffMap(u.tile));
            }
        }
        // Descending, so that pushing each unit onto the front of its tile's list leaves every
        // list ascending.
        for &id in ids.iter().rev() {
            let tile = self.store.get(id).map(|u| u.tile);
            if let Some(tile) = tile {
                self.occ_push_front(tile, id);
            }
        }
        for u in self.store.values() {
            self.by_owner.ensure(u.owner).push(u.id);
        }
        for u in self.store.values() {
            if let Some(c) = u.carried_by {
                self.check_carrier(u, c)?;
            }
        }
        Ok(())
    }

    fn check_carrier(&self, u: &Unit, c: UnitId) -> Result<(), UnitsError> {
        let carrier = self.store.get(c).ok_or(UnitsError::NoSuchUnit(c))?;
        if c == u.id {
            return Err(UnitsError::SelfCarry(u.id));
        }
        if carrier.tile != u.tile {
            return Err(UnitsError::NotTogether { unit: u.id, at: u.tile, carrier: c });
        }
        if carrier.carried_by.is_some() {
            return Err(UnitsError::Nested(c));
        }
        Ok(())
    }

    // ---- Reads ---------------------------------------------------------------------------------

    /// The number of units.
    #[must_use]
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether there are no units.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// The unit with this id.
    #[must_use]
    #[inline]
    pub fn get(&self, u: UnitId) -> Option<&Unit> {
        self.store.get(u)
    }

    /// The unit with this id, to edit its plain fields. Owner, tile and carrier stay private.
    pub fn get_mut(&mut self, u: UnitId) -> Option<&mut Unit> {
        self.store.get_mut(u)
    }

    /// Two different units at once, such as an attacker and a defender.
    pub fn get2_mut(&mut self, a: UnitId, b: UnitId) -> Option<(&mut Unit, &mut Unit)> {
        self.store.get2_mut(a, b)
    }

    /// Whether the unit exists.
    #[must_use]
    pub fn contains(&self, u: UnitId) -> bool {
        self.store.contains(u)
    }

    /// Every unit, ascending by id.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Unit> {
        self.store.values()
    }

    /// Every unit id, ascending: a snapshot, for loops that add or remove units.
    #[must_use]
    pub fn ids(&self) -> Vec<UnitId> {
        self.store.ids()
    }

    /// The store itself.
    #[must_use]
    pub fn store(&self) -> &EntityStore<UnitId, Unit> {
        &self.store
    }

    /// The ids of the units on tile `t`, ascending.
    pub fn at(&self, t: TileIdx) -> impl Iterator<Item = UnitId> + '_ {
        let mut raw = self.occ_head.get(t.0 as usize).copied().unwrap_or(NIL);
        core::iter::from_fn(move || {
            let id = UnitId::new(raw)?;
            raw = self.occ_next.get(raw as usize).copied().unwrap_or(NIL);
            Some(id)
        })
    }

    /// The units on tile `t`, ascending by id.
    pub fn units_at(&self, t: TileIdx) -> impl Iterator<Item = &Unit> + '_ {
        self.at(t).filter_map(|u| self.store.get(u))
    }

    /// A player's unit ids, ascending.
    #[must_use]
    pub fn of(&self, p: PlayerId) -> &[UnitId] {
        self.by_owner.get(p).map_or(&[], Vec::as_slice)
    }

    /// The units a unit carries, ascending by id.
    pub fn carried_by(&self, carrier: UnitId) -> impl Iterator<Item = UnitId> + '_ {
        let tile = self.store.get(carrier).map(|c| c.tile);
        tile.into_iter()
            .flat_map(|t| self.at(t))
            .filter(move |&u| self.store.get(u).is_some_and(|x| x.carried_by == Some(carrier)))
    }

    // ---- Occupancy lists ------------------------------------------------------------------------

    /// The link after `id`, growing the links to it. Only ids the store holds get here, and the
    /// store refuses any above `store::MAX_ENTITY_ID`, so a corrupt id cannot grow this far.
    fn next_slot(&mut self, id: UnitId) -> &mut u32 {
        let i = id.index();
        if i >= self.occ_next.len() {
            self.occ_next.resize(i + 1, NIL);
        }
        &mut self.occ_next[i]
    }

    fn occ_push_front(&mut self, t: TileIdx, id: UnitId) {
        let head = self.occ_head[t.0 as usize];
        *self.next_slot(id) = head;
        self.occ_head[t.0 as usize] = id.get();
    }

    /// Links `id` into tile `t`'s list at its place in id order.
    fn occ_insert(&mut self, t: TileIdx, id: UnitId) {
        let raw = id.get();
        let head = self.occ_head[t.0 as usize];
        if head == NIL || head > raw {
            self.occ_push_front(t, id);
            return;
        }
        let mut prev = head;
        loop {
            let next = self.occ_next.get(prev as usize).copied().unwrap_or(NIL);
            if next == NIL || next > raw {
                *self.next_slot(id) = next;
                if let Some(p) = self.occ_next.get_mut(prev as usize) {
                    *p = raw;
                }
                return;
            }
            prev = next;
        }
    }

    /// Unlinks `id` from tile `t`'s list.
    fn occ_remove(&mut self, t: TileIdx, id: UnitId) {
        let raw = id.get();
        let after = self.occ_next.get(raw as usize).copied().unwrap_or(NIL);
        let Some(head) = self.occ_head.get_mut(t.0 as usize) else { return };
        if *head == raw {
            *head = after;
        } else {
            let mut prev = *head;
            while prev != NIL {
                let next = self.occ_next.get(prev as usize).copied().unwrap_or(NIL);
                if next == raw {
                    if let Some(p) = self.occ_next.get_mut(prev as usize) {
                        *p = after;
                    }
                    break;
                }
                prev = next;
            }
        }
        if let Some(n) = self.occ_next.get_mut(raw as usize) {
            *n = NIL;
        }
    }

    fn owner_insert(&mut self, p: PlayerId, id: UnitId) {
        let list = self.by_owner.ensure(p);
        if let Err(i) = list.binary_search(&id) {
            list.insert(i, id);
        }
    }

    fn owner_remove(&mut self, p: PlayerId, id: UnitId) {
        if let Some(list) = self.by_owner.get_mut(p)
            && let Ok(i) = list.binary_search(&id)
        {
            list.remove(i);
        }
    }

    // ---- Writes ---------------------------------------------------------------------------------

    /// Puts a new unit on the map. Its id must be new and its tile on the map; it starts
    /// uncarried.
    pub fn spawn(&mut self, unit: Unit) -> Result<Change, UnitsError> {
        if unit.tile.0 as usize >= self.occ_head.len() {
            return Err(UnitsError::OffMap(unit.tile));
        }
        if unit.carried_by.is_some() {
            return Err(UnitsError::SpawnedCarried(unit.id));
        }
        let (id, owner, tile) = (unit.id, unit.owner, unit.tile);
        self.store.insert(unit)?;
        self.occ_insert(tile, id);
        self.owner_insert(owner, id);
        Ok(Change::UnitPlaced { u: id, owner, from: None, to: tile })
    }

    /// Takes a unit off the map and out of the game. The units it carried stay where they are,
    /// no longer carried (`game.py:751-764`).
    ///
    /// The changes list the unit's removal, then each unit it carried leaving it, in id order, as
    /// [`unboard`](Self::unboard) reports one: whether a unit is carried matters to healing,
    /// city air capacity and carrier capacity.
    pub fn despawn(&mut self, u: UnitId) -> Result<(Unit, Changes), UnitsError> {
        let (owner, tile) =
            self.get(u).map(|x| (x.owner, x.tile)).ok_or(UnitsError::NoSuchUnit(u))?;
        let carried: SmallVec<[UnitId; 4]> = self.carried_by(u).collect();
        self.occ_remove(tile, u);
        self.owner_remove(owner, u);
        let unit = self.store.remove(u).ok_or(UnitsError::NoSuchUnit(u))?;
        let mut out = Changes::new();
        out.push(Change::UnitRemoved { u, owner, at: tile });
        for c in carried {
            if let Some(x) = self.store.get_mut(c) {
                x.carried_by = None;
                out.push(Change::UnitPlaced { u: c, owner: x.owner, from: Some(tile), to: tile });
            }
        }
        Ok((unit, out))
    }

    /// Moves a unit to `to` without movement rules, taking the units it carries along
    /// (`game.py:766-785`). A carried unit moved away from its carrier leaves it.
    ///
    /// The changes list the unit first, then its cargo in id order.
    pub fn relocate(&mut self, u: UnitId, to: TileIdx) -> Result<Changes, UnitsError> {
        if to.0 as usize >= self.occ_head.len() {
            return Err(UnitsError::OffMap(to));
        }
        let (owner, from, carrier) = self
            .get(u)
            .map(|x| (x.owner, x.tile, x.carried_by))
            .ok_or(UnitsError::NoSuchUnit(u))?;
        let cargo: SmallVec<[UnitId; 4]> = self.carried_by(u).collect();
        let mut out = Changes::new();
        self.move_one(u, from, to);
        if from != to
            && carrier.is_some()
            && let Some(x) = self.store.get_mut(u)
        {
            x.carried_by = None;
        }
        out.push(Change::UnitPlaced { u, owner, from: Some(from), to });
        for c in cargo {
            let Some(cowner) = self.get(c).map(|x| x.owner) else { continue };
            self.move_one(c, from, to);
            out.push(Change::UnitPlaced { u: c, owner: cowner, from: Some(from), to });
        }
        Ok(out)
    }

    fn move_one(&mut self, u: UnitId, from: TileIdx, to: TileIdx) {
        if from == to {
            return;
        }
        self.occ_remove(from, u);
        self.occ_insert(to, u);
        if let Some(x) = self.store.get_mut(u) {
            x.tile = to;
        }
    }

    /// Hands a unit to another player. Its orders are the rule's to reset (`game.py:796-804`);
    /// the units it carries keep their owners.
    pub fn set_owner(&mut self, u: UnitId, new: PlayerId) -> Result<Change, UnitsError> {
        let old = self.get(u).map(|x| x.owner).ok_or(UnitsError::NoSuchUnit(u))?;
        if old != new {
            self.owner_remove(old, u);
            self.owner_insert(new, u);
            if let Some(x) = self.store.get_mut(u) {
                x.owner = new;
            }
        }
        Ok(Change::UnitOwner { u, old, new })
    }

    /// Puts a unit on a carrier on its tile, as an aircraft rebasing to a carrier does
    /// (`combat.py:1063-1070`). Carriers do not nest.
    pub fn board(&mut self, u: UnitId, carrier: UnitId) -> Result<Change, UnitsError> {
        if u == carrier {
            return Err(UnitsError::SelfCarry(u));
        }
        let x = self.get(u).ok_or(UnitsError::NoSuchUnit(u))?;
        let c = self.get(carrier).ok_or(UnitsError::NoSuchUnit(carrier))?;
        if x.tile != c.tile {
            return Err(UnitsError::NotTogether { unit: u, at: x.tile, carrier });
        }
        if c.carried_by.is_some() {
            return Err(UnitsError::Nested(carrier));
        }
        if self.carried_by(u).next().is_some() {
            return Err(UnitsError::Nested(u));
        }
        let (owner, tile) = (x.owner, x.tile);
        if let Some(x) = self.store.get_mut(u) {
            x.carried_by = Some(carrier);
        }
        Ok(Change::UnitPlaced { u, owner, from: Some(tile), to: tile })
    }

    /// Takes a unit off its carrier, on the same tile.
    pub fn unboard(&mut self, u: UnitId) -> Result<Change, UnitsError> {
        let x = self.store.get_mut(u).ok_or(UnitsError::NoSuchUnit(u))?;
        x.carried_by = None;
        Ok(Change::UnitPlaced { u, owner: x.owner, from: Some(x.tile), to: x.tile })
    }

    // ---- Checks ---------------------------------------------------------------------------------

    /// Checks that the indexes agree with the units, and the carrier links hold: each unit is
    /// listed once, on its own tile, in id order; each owner list holds exactly its units; each
    /// carried unit shares its carrier's tile, and no carrier is carried.
    pub fn verify(&self) -> Result<(), UnitsError> {
        let bad = |m: String| Err(UnitsError::Corrupt(m));
        let mut seen = 0usize;
        for t in 0..self.occ_head.len() {
            let tile = TileIdx(u32::try_from(t).unwrap_or(u32::MAX));
            let mut last = 0u32;
            for id in self.at(tile) {
                seen += 1;
                if seen > self.len() {
                    return bad(format!("tile {tile} lists more units than exist (a cycle?)"));
                }
                if id.get() <= last {
                    return bad(format!("tile {tile} lists unit {id} out of order"));
                }
                last = id.get();
                match self.get(id) {
                    Some(u) if u.tile == tile => {}
                    Some(u) => {
                        return bad(format!("tile {tile} lists unit {id}, which is on {}", u.tile));
                    }
                    None => return bad(format!("tile {tile} lists missing unit {id}")),
                }
            }
        }
        if seen != self.len() {
            return bad(format!("the tiles list {seen} units, the store holds {}", self.len()));
        }
        let mut owned = 0usize;
        for (p, list) in self.by_owner.iter() {
            owned += list.len();
            if !list.is_sorted() || list.windows(2).any(|w| w[0] == w[1]) {
                return bad(format!("player {p}'s unit list is not strictly ascending"));
            }
            for &id in list {
                if self.get(id).map(|u| u.owner) != Some(p) {
                    return bad(format!("player {p}'s list holds unit {id}, which is not theirs"));
                }
            }
        }
        if owned != self.len() {
            return bad(format!("the owner lists hold {owned} units, the store {}", self.len()));
        }
        for u in self.iter() {
            if let Some(c) = u.carried_by {
                self.check_carrier(u, c)?;
            }
        }
        Ok(())
    }
}

impl PartialEq for Units {
    /// Equal when they hold the same units; the indexes follow from those.
    fn eq(&self, other: &Self) -> bool {
        self.store == other.store && self.occ_head.len() == other.occ_head.len()
    }
}

impl fmt::Debug for Units {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Units").field("store", &self.store).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(n: u32) -> UnitId {
        UnitId::new(n).unwrap_or(UnitId::FIRST)
    }

    fn unit(n: u32, owner: u8, tile: u32) -> Unit {
        Unit::new(uid(n), BaseUnitId(0), PlayerId(owner), TileIdx(tile), 1)
    }

    #[test]
    fn occupancy_lists_stay_in_id_order() -> Result<(), UnitsError> {
        let mut us = Units::new(10);
        for (n, t) in [(5, 3), (7, 4), (9, 3)] {
            let placed =
                Change::UnitPlaced { u: uid(n), owner: PlayerId(0), from: None, to: TileIdx(t) };
            assert_eq!(us.spawn(unit(n, 0, t))?, placed);
        }
        assert!(us.spawn(unit(2, 0, 3)).is_err(), "2 is below ids the store has held");
        assert!(us.spawn(unit(12, 0, 10)).is_err(), "tile 10 is off the map");
        assert_eq!(us.at(TileIdx(3)).collect::<Vec<_>>(), [uid(5), uid(9)]);
        let moved = us.relocate(uid(7), TileIdx(3))?;
        assert_eq!(moved.len(), 1);
        assert_eq!(us.at(TileIdx(3)).collect::<Vec<_>>(), [uid(5), uid(7), uid(9)]);
        assert_eq!(us.at(TileIdx(4)).count(), 0);
        us.verify()
    }

    #[test]
    fn cargo_moves_with_its_carrier_and_is_dropped_when_it_goes() -> Result<(), UnitsError> {
        let mut us = Units::from_units([unit(1, 0, 2), unit(2, 0, 2), unit(3, 0, 2)], 10)?;
        assert!(us.board(uid(2), uid(1)).is_ok());
        assert!(us.board(uid(3), uid(1)).is_ok());
        assert_eq!(us.board(uid(1), uid(2)), Err(UnitsError::Nested(uid(2))));
        assert_eq!(us.carried_by(uid(1)).collect::<Vec<_>>(), [uid(2), uid(3)]);
        let changes = us.relocate(uid(1), TileIdx(6))?;
        assert_eq!(changes.len(), 3);
        assert!(us.iter().all(|u| u.tile() == TileIdx(6)));
        us.verify()?;
        let _changed = us.set_owner(uid(3), PlayerId(1))?;
        let (gone, ch) = us.despawn(uid(1))?;
        assert_eq!(gone.id(), uid(1));
        let at = TileIdx(6);
        assert_eq!(
            ch.as_slice(),
            &[
                Change::UnitRemoved { u: uid(1), owner: PlayerId(0), at },
                Change::UnitPlaced { u: uid(2), owner: PlayerId(0), from: Some(at), to: at },
                Change::UnitPlaced { u: uid(3), owner: PlayerId(1), from: Some(at), to: at },
            ],
            "the removal, then each unit it carried leaving it"
        );
        assert!(us.iter().all(|u| u.carried_by().is_none()));
        us.verify()
    }

    #[test]
    fn owner_lists_follow_ownership() -> Result<(), UnitsError> {
        let mut us = Units::from_units([unit(1, 0, 0), unit(2, 1, 0), unit(3, 0, 1)], 4)?;
        assert_eq!(us.of(PlayerId(0)), &[uid(1), uid(3)]);
        let handed = Change::UnitOwner { u: uid(3), old: PlayerId(0), new: PlayerId(1) };
        assert_eq!(us.set_owner(uid(3), PlayerId(1))?, handed);
        assert_eq!(us.of(PlayerId(1)), &[uid(2), uid(3)]);
        assert_eq!(us.of(PlayerId(7)), &[] as &[UnitId]);
        us.verify()
    }

    #[test]
    fn broken_carrier_links_are_refused_on_load() {
        let mut a = unit(1, 0, 1);
        a.carried_by = Some(uid(2));
        let b = unit(2, 0, 3);
        assert!(matches!(Units::from_units([a, b], 5), Err(UnitsError::NotTogether { .. })));
        assert!(matches!(Units::from_units([unit(1, 0, 9)], 5), Err(UnitsError::OffMap(_))));
    }

    #[test]
    fn abilities_are_counted_in_key_order() {
        let mut u = unit(1, 0, 0);
        u.use_ability(AbilityKey(4));
        u.use_ability(AbilityKey(1));
        u.use_ability(AbilityKey(4));
        assert_eq!(u.abilities_used.as_slice(), &[(AbilityKey(1), 1), (AbilityKey(4), 2)]);
        assert_eq!(u.ability_uses(AbilityKey(4)), 2);
        assert_eq!(u.ability_uses(AbilityKey(9)), 0);
    }

    #[test]
    fn activities_by_name() {
        for a in Activity::ALL {
            assert_eq!(Activity::from_name(a.name()), Some(a));
        }
        assert_eq!(Activity::from_name("Fortify"), None);
    }
}

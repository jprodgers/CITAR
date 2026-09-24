//! Cities, and the list of each player's cities (DESIGN.md 4.4).
//!
//! Replaces `state.py:147-190` (`City`) and the city bookkeeping of `Game`. A city's owner and
//! tile are private: only [`Cities`] (and `State::transfer_city`, which also moves the tiles)
//! changes them, and each change returns the [`Change`] the caches need. Which city stands on a
//! tile is read from the tile itself (`State::city_at`), so there is no separate index.
//!
//! Changed from Python:
//! - `free_buildings` moved here from `Player.free_buildings[str(city id)]`;
//! - buildings are a set, and the religious pressures are seeded when the city is created, not on
//!   first read (`religion.py:105-109`), so a read never writes;
//! - `spaceship_parts`, which nothing read, is dropped;
//! - [`citizens_settled`](City::citizens_settled) is new (DESIGN.md 6.8).

use core::fmt;
use std::collections::BTreeMap;

use smallvec::SmallVec;

use super::change::Change;
use super::store::{Entity, EntityStore, StoreError};
use crate::base::ids::{
    BaseUnitId, BuildingId, CityId, PlayerId, ReligionId, ResourceId, TileIdx, Turn,
};
use crate::base::sets::{BuildingSet, MAX_SPECIALISTS, PlayerVec};

/// A production item that is not a thing: turning production into gold or science, or into
/// nothing (`cities.py:24`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Perpetual {
    Gold,
    Science,
    Nothing,
}

impl Perpetual {
    /// Every perpetual item.
    pub const ALL: [Self; 3] = [Self::Gold, Self::Science, Self::Nothing];

    /// The name Python queued: `Gold`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gold => "Gold",
            Self::Science => "Science",
            Self::Nothing => "Nothing",
        }
    }

    /// The item called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// Something a city can build (Python's queue entries, `{"kind", "id"}`, `state.py:159`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Constructible {
    Building(BuildingId),
    Unit(BaseUnitId),
    Perpetual(Perpetual),
}

/// What a city's citizens favour (`cities.py:25-31`).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CityFocus {
    #[default]
    Balanced,
    /// The player places the citizens.
    Manual,
    Food,
    Production,
    Gold,
    Science,
    Culture,
    Faith,
    Happiness,
    GoldGrowth,
    ProductionGrowth,
}

impl CityFocus {
    /// Every focus, in Python's order.
    pub const ALL: [Self; 11] = [
        Self::Balanced,
        Self::Manual,
        Self::Food,
        Self::Production,
        Self::Gold,
        Self::Science,
        Self::Culture,
        Self::Faith,
        Self::Happiness,
        Self::GoldGrowth,
        Self::ProductionGrowth,
    ];

    /// The name Python saved and tools take: `gold_growth`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Manual => "manual",
            Self::Food => "food",
            Self::Production => "production",
            Self::Gold => "gold",
            Self::Science => "science",
            Self::Culture => "culture",
            Self::Faith => "faith",
            Self::Happiness => "happiness",
            Self::GoldGrowth => "gold_growth",
            Self::ProductionGrowth => "production_growth",
        }
    }

    /// The focus called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// The pressure a new city starts with: all of it toward no religion (`religion.py:105-109`).
pub const NO_RELIGION_PRESSURE: i32 = 100;

/// One city (`state.py:147-190`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct City {
    id: CityId,
    owner: PlayerId,
    tile: TileIdx,
    pub name: Box<str>,
    /// The civilization that founded it.
    pub founder: PlayerId,
    pub previous_owner: Option<PlayerId>,
    pub founded_turn: Turn,
    pub turn_acquired: Turn,
    /// Whether it was its founder's first capital.
    pub original_capital: bool,
    pub pop: u16,
    pub food: f64,
    /// Culture stored toward the next border tile.
    pub culture: f64,
    pub tiles_claimed: u16,
    pub tiles_bought: u16,
    pub buildings: BuildingSet,
    /// The buildings it got free, which cost no maintenance.
    pub free_buildings: BuildingSet,
    pub queue: SmallVec<[Constructible; 4]>,
    /// Production invested in each item.
    #[serde(with = "crate::base::codec::pairs")]
    pub progress: BTreeMap<Constructible, f64>,
    pub overflow: f64,
    pub bought_this_turn: SmallVec<[Constructible; 2]>,
    pub auto_production: bool,
    /// The tiles its citizens work, sorted.
    pub worked: Vec<TileIdx>,
    /// The tiles the player locked, sorted.
    pub locked: Vec<TileIdx>,
    /// Specialists by `SpecialistId`.
    #[serde(with = "specialist_counts")]
    pub specialists: [u8; MAX_SPECIALISTS],
    pub manual_specialists: bool,
    pub focus: CityFocus,
    pub avoid_growth: bool,
    /// Whether this engine has assigned its citizens at least once; a city converted from a
    /// Python save keeps Python's worked tiles until then (DESIGN.md 6.8).
    pub citizens_settled: bool,
    pub health: i32,
    pub damaged_turn: Turn,
    pub attacked: bool,
    /// The last turn barbarians sacked it.
    pub sacked_turn: Turn,
    pub puppet: bool,
    /// Turns of resistance left.
    pub resistance: i16,
    pub razing: bool,
    /// Religious pressure by religion, `None` being no religion, sorted.
    pub pressures: SmallVec<[(Option<ReligionId>, i32); 4]>,
    pub religions_adopted: SmallVec<[ReligionId; 2]>,
    pub holy_city_of: Option<ReligionId>,
    /// We Love The King Day turns left.
    pub wltkd: i16,
    pub demanded_resource: Option<ResourceId>,
    pub demand_countdown: i16,
}

/// A city's specialists: in JSON the non-zero counts by specialist, which a reordered ruleset
/// cannot misread; in `CANON_V1` the fixed array.
mod specialist_counts {
    use std::collections::BTreeMap;

    use serde::de::{self, Deserializer};
    use serde::ser::Serializer;
    use serde::{Deserialize, Serialize};

    use crate::base::ids::{Id, SpecialistId};
    use crate::base::sets::MAX_SPECIALISTS;

    pub fn serialize<S: Serializer>(
        counts: &[u8; MAX_SPECIALISTS],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        if !s.is_human_readable() {
            return counts.serialize(s);
        }
        let mut map = BTreeMap::new();
        for (i, &n) in counts.iter().enumerate() {
            if let Some(id) = SpecialistId::from_index(i).filter(|_| n != 0) {
                map.insert(id, n);
            }
        }
        map.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; MAX_SPECIALISTS], D::Error> {
        if !d.is_human_readable() {
            return <[u8; MAX_SPECIALISTS]>::deserialize(d);
        }
        let mut out = [0u8; MAX_SPECIALISTS];
        for (id, n) in BTreeMap::<SpecialistId, u8>::deserialize(d)? {
            let slot = out.get_mut(id.index()).ok_or_else(|| {
                de::Error::custom(format!("specialist {} is past the {MAX_SPECIALISTS} kept", id.0))
            })?;
            *slot = n;
        }
        Ok(out)
    }
}

impl City {
    /// A new city of population 1, as founding leaves it before the founding rules run, with its
    /// religious pressure seeded.
    #[must_use]
    pub fn new(
        id: CityId,
        name: Box<str>,
        owner: PlayerId,
        tile: TileIdx,
        founded_turn: Turn,
    ) -> Self {
        let mut pressures = SmallVec::new();
        pressures.push((None, NO_RELIGION_PRESSURE));
        Self {
            id,
            owner,
            tile,
            name,
            founder: owner,
            previous_owner: None,
            founded_turn,
            turn_acquired: founded_turn,
            original_capital: false,
            pop: 1,
            food: 0.0,
            culture: 0.0,
            tiles_claimed: 0,
            tiles_bought: 0,
            buildings: BuildingSet::new(),
            free_buildings: BuildingSet::new(),
            queue: SmallVec::new(),
            progress: BTreeMap::new(),
            overflow: 0.0,
            bought_this_turn: SmallVec::new(),
            auto_production: false,
            worked: Vec::new(),
            locked: Vec::new(),
            specialists: [0; MAX_SPECIALISTS],
            manual_specialists: false,
            focus: CityFocus::Balanced,
            avoid_growth: false,
            citizens_settled: false,
            health: 200,
            damaged_turn: -1,
            attacked: false,
            sacked_turn: -1000,
            puppet: false,
            resistance: 0,
            razing: false,
            pressures,
            religions_adopted: SmallVec::new(),
            holy_city_of: None,
            wltkd: 0,
            demanded_resource: None,
            demand_countdown: 0,
        }
    }

    /// Its id.
    #[must_use]
    #[inline]
    pub const fn id(&self) -> CityId {
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

    /// The pressure toward a religion (`None`: toward none).
    #[must_use]
    pub fn pressure(&self, r: Option<ReligionId>) -> i32 {
        self.pressures.iter().find(|(k, _)| *k == r).map_or(0, |&(_, v)| v)
    }

    /// Sets the pressure toward a religion, keeping the list sorted; 0 removes the entry.
    pub fn set_pressure(&mut self, r: Option<ReligionId>, v: i32) {
        match self.pressures.binary_search_by_key(&r, |&(k, _)| k) {
            Ok(i) if v == 0 => {
                self.pressures.remove(i);
            }
            Ok(i) => self.pressures[i].1 = v,
            Err(_) if v == 0 => {}
            Err(i) => self.pressures.insert(i, (r, v)),
        }
    }
}

impl Entity for City {
    type Id = CityId;

    fn id(&self) -> CityId {
        self.id
    }
}

/// Why a city operation was refused. Nothing is changed when one is.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CitiesError {
    /// There is no such city.
    #[error("there is no city {0}")]
    NoSuchCity(CityId),
    /// The city's id was used before.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The owner lists disagree with the cities (a check, never a refusal).
    #[error("city indexes are inconsistent: {0}")]
    Corrupt(String),
}

/// The city store and each player's city list, ascending by id.
#[derive(Clone, Default)]
pub struct Cities {
    store: EntityStore<CityId, City>,
    by_owner: PlayerVec<Vec<CityId>>,
}

impl Cities {
    /// No cities.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// These cities, in ascending id order, with the owner lists built.
    pub fn from_cities(cities: impl IntoIterator<Item = City>) -> Result<Self, CitiesError> {
        let mut out =
            Self { store: EntityStore::try_from_values(cities)?, by_owner: PlayerVec::new() };
        out.rebuild();
        Ok(out)
    }

    /// Rebuilds the owner lists from the cities.
    pub fn rebuild(&mut self) {
        self.by_owner.clear();
        for c in self.store.values() {
            self.by_owner.ensure(c.owner).push(c.id);
        }
    }

    /// The number of cities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// The city with this id.
    #[must_use]
    #[inline]
    pub fn get(&self, c: CityId) -> Option<&City> {
        self.store.get(c)
    }

    /// The city with this id, to edit its plain fields. Owner and tile stay private.
    pub fn get_mut(&mut self, c: CityId) -> Option<&mut City> {
        self.store.get_mut(c)
    }

    /// Whether the city exists.
    #[must_use]
    pub fn contains(&self, c: CityId) -> bool {
        self.store.contains(c)
    }

    /// Every city, ascending by id.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &City> {
        self.store.values()
    }

    /// Every city id, ascending: a snapshot, for loops that add or remove cities.
    #[must_use]
    pub fn ids(&self) -> Vec<CityId> {
        self.store.ids()
    }

    /// The store itself.
    #[must_use]
    pub fn store(&self) -> &EntityStore<CityId, City> {
        &self.store
    }

    /// A player's city ids, ascending.
    #[must_use]
    pub fn of(&self, p: PlayerId) -> &[CityId] {
        self.by_owner.get(p).map_or(&[], Vec::as_slice)
    }

    fn owner_insert(&mut self, p: PlayerId, c: CityId) {
        let list = self.by_owner.ensure(p);
        if let Err(i) = list.binary_search(&c) {
            list.insert(i, c);
        }
    }

    fn owner_remove(&mut self, p: PlayerId, c: CityId) {
        if let Some(list) = self.by_owner.get_mut(p)
            && let Ok(i) = list.binary_search(&c)
        {
            list.remove(i);
        }
    }

    /// Adds a new city. The tiles it claims are claimed through `Tiles::set_owner`.
    pub fn found(&mut self, city: City) -> Result<Change, CitiesError> {
        let (id, owner) = (city.id, city.owner);
        self.store.insert(city)?;
        self.owner_insert(owner, id);
        Ok(Change::CityAdded(id))
    }

    /// Hands a city to another player. Its tiles move with `State::transfer_city`, which calls
    /// this.
    pub fn set_owner(&mut self, c: CityId, new: PlayerId) -> Result<Change, CitiesError> {
        let old = self.get(c).map(|x| x.owner).ok_or(CitiesError::NoSuchCity(c))?;
        if old != new {
            self.owner_remove(old, c);
            self.owner_insert(new, c);
            if let Some(x) = self.store.get_mut(c) {
                x.owner = new;
            }
        }
        Ok(Change::CityOwner { c, old, new })
    }

    /// Takes a city out of the game. Its tiles are released through `Tiles::set_owner`.
    pub fn remove(&mut self, c: CityId) -> Result<(City, Change), CitiesError> {
        let owner = self.get(c).map(|x| x.owner).ok_or(CitiesError::NoSuchCity(c))?;
        self.owner_remove(owner, c);
        let city = self.store.remove(c).ok_or(CitiesError::NoSuchCity(c))?;
        let at = city.tile;
        Ok((city, Change::CityRemoved { c, owner, at }))
    }

    /// Checks that each owner list holds exactly that player's cities, ascending.
    pub fn verify(&self) -> Result<(), CitiesError> {
        let mut owned = 0usize;
        for (p, list) in self.by_owner.iter() {
            owned += list.len();
            if list.windows(2).any(|w| w[0] >= w[1]) {
                return Err(CitiesError::Corrupt(format!(
                    "player {p}'s city list is not strictly ascending"
                )));
            }
            if let Some(&c) = list.iter().find(|&&c| self.get(c).map(City::owner) != Some(p)) {
                return Err(CitiesError::Corrupt(format!(
                    "player {p}'s list holds city {c}, which is not theirs"
                )));
            }
        }
        if owned != self.len() {
            return Err(CitiesError::Corrupt(format!(
                "the owner lists hold {owned} cities, the store {}",
                self.len()
            )));
        }
        Ok(())
    }
}

impl PartialEq for Cities {
    /// Equal when they hold the same cities; the owner lists follow from those.
    fn eq(&self, other: &Self) -> bool {
        self.store == other.store
    }
}

impl fmt::Debug for Cities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cities").field("store", &self.store).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(n: u32) -> CityId {
        CityId::new(n).unwrap_or(CityId::FIRST)
    }

    fn city(n: u32, owner: u8) -> City {
        City::new(cid(n), format!("City {n}").into(), PlayerId(owner), TileIdx(n), 1)
    }

    #[test]
    fn a_new_city_is_seeded_toward_no_religion() {
        let c = city(1, 0);
        assert_eq!(c.pressure(None), NO_RELIGION_PRESSURE);
        assert_eq!((c.pop, c.health, c.sacked_turn, c.damaged_turn), (1, 200, -1000, -1));
        assert_eq!(c.founder, PlayerId(0));
    }

    #[test]
    fn pressures_stay_sorted_and_drop_zeros() {
        let mut c = city(1, 0);
        c.set_pressure(Some(ReligionId(3)), 50);
        c.set_pressure(Some(ReligionId(1)), 20);
        assert_eq!(
            c.pressures.as_slice(),
            &[(None, 100), (Some(ReligionId(1)), 20), (Some(ReligionId(3)), 50)]
        );
        c.set_pressure(None, 0);
        c.set_pressure(Some(ReligionId(9)), 0);
        assert_eq!(c.pressures.len(), 2);
        assert_eq!(c.pressure(Some(ReligionId(3))), 50);
    }

    #[test]
    fn owner_lists_follow_founding_capture_and_removal() -> Result<(), CitiesError> {
        let mut cs = Cities::new();
        assert_eq!(cs.found(city(1, 0))?, Change::CityAdded(cid(1)));
        assert_eq!(cs.found(city(4, 1))?, Change::CityAdded(cid(4)));
        assert!(cs.found(city(2, 1)).is_err(), "ids are never reused or taken out of order");
        assert_eq!(
            cs.set_owner(cid(1), PlayerId(1))?,
            Change::CityOwner { c: cid(1), old: PlayerId(0), new: PlayerId(1) }
        );
        assert_eq!(cs.of(PlayerId(1)), &[cid(1), cid(4)]);
        let (gone, ch) = cs.remove(cid(4))?;
        assert_eq!(ch, Change::CityRemoved { c: cid(4), owner: PlayerId(1), at: gone.tile() });
        assert_eq!(cs.of(PlayerId(1)), &[cid(1)]);
        assert!(cs.remove(cid(4)).is_err());
        cs.verify()
    }

    #[test]
    fn focuses_and_perpetual_items_by_name() {
        for f in CityFocus::ALL {
            assert_eq!(CityFocus::from_name(f.name()), Some(f));
        }
        for p in Perpetual::ALL {
            assert_eq!(Perpetual::from_name(p.name()), Some(p));
        }
    }
}

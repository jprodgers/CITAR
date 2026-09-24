//! Rule objects, players, tiles and religions as Python named them.
//!
//! Python saved every rule object by its display name. A name resolves exactly first, then
//! through the ruleset's loose resolver (lower case, letters and digits only) for the tables
//! tools resolve in (DESIGN.md 4.12); a name that resolves neither way fails the conversion with
//! its place.

use serde_json::Value;

use super::read::{Path, Res, int, key_int, shown, text};
use super::{Cx, Dropped};
use crate::base::ids::{
    AbilityKey, BaseUnitId, BeliefId, BuildingId, CityId, CityStateTypeId, DifficultyId, EraId,
    FeatureId, ImprovementId, NationId, PlayerId, PolicyId, PromotionId, QuestKindId, ReligionId,
    ResourceId, RuinId, RulesReligionId, SpecialistId, SpeedId, TechId, TerrainId, TextId, TileIdx,
    UnitTypeId, VictoryId,
};
use crate::base::sets::{IdSet, PlayerSet};
use crate::rules::Ruleset;
use crate::save::ctx::RuleName;
use crate::state::cities::{Constructible, Perpetual};

/// A rule id Python named, and how loosely the name may be read.
pub(super) trait PyName: RuleName {
    /// The id a name that is not exact names, if its table resolves loosely.
    fn loose(_r: &'static Ruleset, _name: &str) -> Option<Self> {
        None
    }
}

macro_rules! loose {
    ($($id:ty),* $(,)?) => {$(
        impl PyName for $id {
            fn loose(r: &'static Ruleset, name: &str) -> Option<Self> {
                r.resolve(name)
            }
        }
    )*};
}

loose!(
    TechId,
    BaseUnitId,
    BuildingId,
    PromotionId,
    TerrainId,
    ResourceId,
    ImprovementId,
    BeliefId,
    PolicyId,
    NationId,
    EraId,
    SpecialistId,
    SpeedId,
    DifficultyId,
    UnitTypeId,
    VictoryId,
);

impl PyName for FeatureId {
    fn loose(r: &'static Ruleset, name: &str) -> Option<Self> {
        let t = r.resolve::<TerrainId>(name)?;
        r.derived().features.iter().find(|&(_, &x)| x == t).map(|(f, _)| f)
    }
}

impl PyName for CityStateTypeId {}
impl PyName for RulesReligionId {}
impl PyName for QuestKindId {}
impl PyName for RuinId {}
impl PyName for TextId {}
impl PyName for AbilityKey {}

impl Cx<'_> {
    /// The rule object `name` names.
    pub(super) fn id_of<I: PyName>(&self, name: &str, p: &Path<'_>) -> Res<I> {
        I::resolve_in(self.r, name)
            .or_else(|| I::loose(self.r, name))
            .ok_or_else(|| p.err(format!("the ruleset has no {} {name:?}", I::WHAT)))
    }

    /// The rule object the text `v` names.
    pub(super) fn named<I: PyName>(&self, v: &Value, p: &Path<'_>) -> Res<I> {
        self.id_of(text(v, p)?, p)
    }

    /// The rule object `v` names, or `None` for Python's `None`.
    pub(super) fn opt_named<I: PyName>(&self, v: Option<&Value>, p: &Path<'_>) -> Res<Option<I>> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(v) => self.named(v, p).map(Some),
        }
    }

    /// A set of rule objects from a list of names. The list's order is dropped, and counted if
    /// it was not the ruleset's; a name listed twice is an error, since Python counted the list.
    pub(super) fn id_set<I: PyName, const W: usize>(
        &mut self,
        list: &[Value],
        p: &Path<'_>,
    ) -> Res<IdSet<I, W>> {
        let mut set = IdSet::new();
        let mut last: Option<I> = None;
        let mut sorted = true;
        for (i, v) in list.iter().enumerate() {
            let at = p.index(i);
            let id: I = self.named(v, &at)?;
            if id.index() >= IdSet::<I, W>::CAPACITY {
                return Err(at.err(format!("{} {} does not fit its set", I::WHAT, id.index())));
            }
            if !set.insert(id) {
                return Err(at.err(format!("{} is listed twice", shown(v))));
            }
            sorted &= last.is_none_or(|l| l < id);
            last = Some(id);
        }
        if !sorted {
            self.report.note(Dropped::ListOrder);
        }
        Ok(set)
    }

    /// What a city builds, by name: a building, a unit or a perpetual item, as Python looked
    /// them up in that order.
    pub(super) fn constructible(&self, name: &str, p: &Path<'_>) -> Res<Constructible> {
        let r = self.r;
        if let Some(b) = r.lookup::<BuildingId>(name) {
            return Ok(Constructible::Building(b));
        }
        if let Some(u) = r.lookup::<BaseUnitId>(name) {
            return Ok(Constructible::Unit(u));
        }
        if let Some(x) = Perpetual::from_name(name) {
            return Ok(Constructible::Perpetual(x));
        }
        if let Some(b) = r.resolve::<BuildingId>(name) {
            return Ok(Constructible::Building(b));
        }
        if let Some(u) = r.resolve::<BaseUnitId>(name) {
            return Ok(Constructible::Unit(u));
        }
        Err(p.err(format!("the ruleset has no building or unit {name:?}")))
    }

    /// A player id: one of the game's players.
    pub(super) fn player(&self, v: &Value, p: &Path<'_>) -> Res<PlayerId> {
        let id: u8 = int(v, p)?;
        self.player_id(id, p)
    }

    /// A player id written as a dict key: `"3"`.
    pub(super) fn player_key(&self, k: &str, p: &Path<'_>) -> Res<PlayerId> {
        let id: u8 = key_int(k, p)?;
        self.player_id(id, p)
    }

    fn player_id(&self, id: u8, p: &Path<'_>) -> Res<PlayerId> {
        if usize::from(id) < self.n {
            Ok(PlayerId(id))
        } else {
            Err(p.err(format!("player {id} is not one of the {} players", self.n)))
        }
    }

    /// A player id, or `None` for Python's `None`.
    pub(super) fn opt_player(&self, v: Option<&Value>, p: &Path<'_>) -> Res<Option<PlayerId>> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(v) => self.player(v, p).map(Some),
        }
    }

    /// A set of players from a list of ids. Its order is dropped, and counted if it was not
    /// ascending.
    pub(super) fn player_set(&mut self, list: &[Value], p: &Path<'_>) -> Res<PlayerSet> {
        let mut set = PlayerSet::EMPTY;
        let mut sorted = true;
        let mut last: Option<PlayerId> = None;
        for (i, v) in list.iter().enumerate() {
            let at = p.index(i);
            let id = self.player(v, &at)?;
            if !set.insert(id) {
                return Err(at.err(format!("player {id} is listed twice")));
            }
            sorted &= last.is_none_or(|l| l < id);
            last = Some(id);
        }
        if !sorted {
            self.report.note(Dropped::ListOrder);
        }
        Ok(set)
    }

    /// A tile index on the map.
    pub(super) fn tile(&self, v: &Value, p: &Path<'_>) -> Res<TileIdx> {
        let t: u32 = int(v, p)?;
        self.tile_idx(t, p)
    }

    /// A tile index written as a dict key.
    pub(super) fn tile_key(&self, k: &str, p: &Path<'_>) -> Res<TileIdx> {
        let t: u32 = key_int(k, p)?;
        self.tile_idx(t, p)
    }

    fn tile_idx(&self, t: u32, p: &Path<'_>) -> Res<TileIdx> {
        if t < self.size {
            Ok(TileIdx(t))
        } else {
            Err(p.err(format!("tile {t} is off the map of {} tiles", self.size)))
        }
    }

    /// A tile index, or `None` for Python's `None`.
    pub(super) fn opt_tile(&self, v: Option<&Value>, p: &Path<'_>) -> Res<Option<TileIdx>> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(v) => self.tile(v, p).map(Some),
        }
    }

    /// A city id. Whether the city exists is checked where it matters (`State::from_parts`,
    /// `save::validate`): some fields name cities razed since.
    pub(super) fn city(&self, v: &Value, p: &Path<'_>) -> Res<CityId> {
        let id: u32 = int(v, p)?;
        CityId::new(id).ok_or_else(|| p.err("0 is not a city id"))
    }

    /// A city id, or `None` for Python's `None`.
    pub(super) fn opt_city(&self, v: Option<&Value>, p: &Path<'_>) -> Res<Option<CityId>> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(v) => self.city(v, p).map(Some),
        }
    }

    /// A founded religion or pantheon, by the name Python keyed it under.
    pub(super) fn religion(&self, v: &Value, p: &Path<'_>) -> Res<ReligionId> {
        self.religion_named(text(v, p)?, p)
    }

    /// A founded religion or pantheon called `name`.
    pub(super) fn religion_named(&self, name: &str, p: &Path<'_>) -> Res<ReligionId> {
        self.religions
            .iter()
            .position(|r| **r == *name)
            .and_then(|i| u8::try_from(i).ok())
            .map(ReligionId)
            .ok_or_else(|| p.err(format!("no religion {name:?} was founded")))
    }

    /// A founded religion, or `None` for Python's `None`.
    pub(super) fn opt_religion(&self, v: Option<&Value>, p: &Path<'_>) -> Res<Option<ReligionId>> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(v) => self.religion(v, p).map(Some),
        }
    }
}

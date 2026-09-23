//! Layer 1: the ruleset, loaded once and shared by every game as `&'static Ruleset`.
//!
//! Typed tables, derived tables, name resolution, constants and the client JSON (DESIGN.md 5,
//! package 1a-03). It shares layer 1 with [`crate::unique`]: tables hold compiled uniques, and
//! the unique compiler resolves names against the tables.
//!
//! Replaces `citar/engine/rules.py:37-342` and reads the data in `citar/data/**`.
//!
//! - [`source`]: the files as bytes ([`RulesetFiles`], [`embedded`]) and the [`RulesetId`];
//! - `raw`: the files as written, read strictly;
//! - [`defs`]: the typed tables, every name resolved to an id;
//! - [`derived`]: the tables computed at load;
//! - [`constants`]: `game.json`, typed;
//! - [`names`]: loose lookup by name or id, for tools;
//! - `client`: the ruleset as the browser reads it;
//! - [`errors`]: what can be wrong with a ruleset.

pub mod constants;
pub mod defs;
pub mod derived;
pub mod errors;
pub mod names;
pub mod source;

mod client;
mod load;
mod raw;

use std::sync::{Mutex, OnceLock, PoisonError};

use crate::base::ids::{
    BaseUnitId, BeliefId, BuildingId, CityStateTypeId, DifficultyId, EraId, FracId, IdVec,
    ImprovementId, NationId, PersonalityId, PolicyId, PromotionId, QuestKindId, ResourceId, RuinId,
    RulesReligionId, SpecialistId, SpeedId, TechId, TerrainId, UnitTypeId, VictoryId,
};

pub use self::constants::{Constants, RULES_VERSION};
use self::defs::{
    BaseUnitDef, BeliefDef, BuildingDef, CityStateTypeDef, DifficultyDef, EraDef, ImprovementDef,
    NationDef, PersonalityDef, PolicyDef, PromotionDef, QuestDef, ResourceDef, RuinDef,
    SpecialistDef, SpeedDef, TechColumn, TechDef, TerrainDef, Uniques, UnitTypeDef, VictoryDef,
};
pub use self::derived::Derived;
pub use self::errors::{RulesetError, RulesetErrorKind, RulesetErrors};
use self::names::NameIndex;
pub use self::names::NameKind;
#[cfg(feature = "embedded-ruleset")]
pub use self::source::embedded;
pub use self::source::{BUILD_ID, RulesetFiles, RulesetId};

/// The whole ruleset: every table typed, every reference an id, and the derived tables.
///
/// Read-only once loaded. Games share one through `&'static Ruleset` ([`Ruleset::leak`],
/// [`Ruleset::shared`]), which is why it is `Sync`: it has no interior mutability except the
/// client JSON, built once behind a `OnceLock`.
pub struct Ruleset {
    id: RulesetId,
    pub techs: IdVec<TechId, TechDef>,
    pub tech_columns: Vec<TechColumn>,
    /// In order of their number, which is their id.
    pub eras: IdVec<EraId, EraDef>,
    pub base_units: IdVec<BaseUnitId, BaseUnitDef>,
    pub unit_types: IdVec<UnitTypeId, UnitTypeDef>,
    pub buildings: IdVec<BuildingId, BuildingDef>,
    pub promotions: IdVec<PromotionId, PromotionDef>,
    pub terrains: IdVec<TerrainId, TerrainDef>,
    pub resources: IdVec<ResourceId, ResourceDef>,
    pub improvements: IdVec<ImprovementId, ImprovementDef>,
    pub beliefs: IdVec<BeliefId, BeliefDef>,
    /// The names of the religions players can found.
    pub religions: IdVec<RulesReligionId, Box<str>>,
    pub specialists: IdVec<SpecialistId, SpecialistDef>,
    pub city_state_types: IdVec<CityStateTypeId, CityStateTypeDef>,
    /// Easiest first.
    pub difficulties: IdVec<DifficultyId, DifficultyDef>,
    pub speeds: IdVec<SpeedId, SpeedDef>,
    pub victories: IdVec<VictoryId, VictoryDef>,
    pub quests: IdVec<QuestKindId, QuestDef>,
    pub ruins: IdVec<RuinId, RuinDef>,
    pub personalities: IdVec<PersonalityId, PersonalityDef>,
    /// Branches first, then the policies, each in file order.
    pub policies: IdVec<PolicyId, PolicyDef>,
    /// How many of `policies` are branches.
    pub policy_branch_count: u16,
    /// `ruleset/nations.json` with `custom/nations.json` merged in.
    pub nations: IdVec<NationId, NationDef>,
    /// Uniques every civilization has.
    pub global_uniques: Uniques,
    pub constants: Constants,
    /// The fractional unique parameters, interned by the unique compiler (package 1a-05).
    pub fracs: IdVec<FracId, f64>,
    pub derived: Derived,
    names: [NameIndex; 16],
    client: client::ClientSource,
    client_json: OnceLock<String>,
}

/// How much a ruleset holds, by kind: Python's `ruleset_counts` (`engine_api.py:134-139`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub techs: u32,
    pub units: u32,
    pub buildings: u32,
    pub nations: u32,
    /// Policies, not counting the branches.
    pub policies: u32,
}

impl Ruleset {
    /// Loads a ruleset from its files, checking everything: every file present and strict
    /// JSON, every field known, every reference resolved, every table within its set.
    ///
    /// # Errors
    /// Every problem found, each naming its file and object.
    pub fn load(files: &RulesetFiles<'_>) -> Result<Ruleset, RulesetErrors> {
        load::load(files)
    }

    /// Loads a ruleset and leaks it, so games can hold it as `&'static`. Loading the same
    /// ruleset again (by [`RulesetId`]) returns the one already leaked, so a host that loads the
    /// same files repeatedly leaks one copy.
    ///
    /// # Errors
    /// As [`load`](Self::load).
    pub fn leak(files: &RulesetFiles<'_>) -> Result<&'static Ruleset, RulesetErrors> {
        static LEAKED: Mutex<Vec<&'static Ruleset>> = Mutex::new(Vec::new());
        let rules = Self::load(files)?;
        // A panic elsewhere while the lock was held left the list intact, so poisoning is safe
        // to ignore.
        let mut leaked = LEAKED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(&known) = leaked.iter().find(|r| r.id == rules.id) {
            return Ok(known);
        }
        let rules: &'static Ruleset = Box::leak(Box::new(rules));
        leaked.push(rules);
        Ok(rules)
    }

    /// The embedded ruleset, loaded on first use.
    ///
    /// # Panics
    /// If the embedded ruleset does not load, which the tests rule out for every build.
    #[cfg(feature = "embedded-ruleset")]
    #[must_use]
    pub fn shared() -> &'static Ruleset {
        static SHARED: OnceLock<&'static Ruleset> = OnceLock::new();
        SHARED.get_or_init(|| match Self::leak(&embedded()) {
            Ok(rules) => rules,
            Err(e) => panic!("the embedded ruleset does not load: {e}"),
        })
    }

    /// What identifies this ruleset.
    #[must_use]
    pub fn id(&self) -> RulesetId {
        self.id
    }

    /// The version saves record: the format version and the start of the [`RulesetId`],
    /// `2-0123456789ab` (`engine_api.rules_version`).
    #[must_use]
    pub fn version(&self) -> String {
        format!("{RULES_VERSION}-{}", &self.id.to_hex()[..12])
    }

    /// How much the ruleset holds (`engine_api.ruleset_counts`).
    #[must_use]
    pub fn counts(&self) -> Counts {
        let n = |len: usize| u32::try_from(len).unwrap_or(u32::MAX);
        Counts {
            techs: n(self.techs.len()),
            units: n(self.base_units.len()),
            buildings: n(self.buildings.len()),
            nations: n(self.nations.len()),
            policies: n(self.policies.len() - usize::from(self.policy_branch_count)),
        }
    }

    /// The most major civilizations a game may seat (`engine_api.max_players`).
    #[must_use]
    pub fn max_players(&self) -> u8 {
        self.constants.max_players
    }

    /// The lobby's map sizes (`engine_api.map_sizes`).
    #[must_use]
    pub fn map_sizes(&self) -> &[constants::MapSize] {
        &self.constants.map_sizes
    }

    /// The speeds' names, in file order (`engine_api.speeds`).
    pub fn speeds(&self) -> impl ExactSizeIterator<Item = &str> {
        self.speeds.as_slice().iter().map(|s| &*s.name)
    }

    /// The difficulties' names, easiest first (`engine_api.difficulties`).
    pub fn difficulties(&self) -> impl ExactSizeIterator<Item = &str> {
        self.difficulties.as_slice().iter().map(|d| &*d.name)
    }

    /// The position in `kind`'s table of the object `text` names: exactly, or else by its
    /// normalised name or id (`rules.py:263-270`). Positions are ids: `TechId(pos)` for techs,
    /// and branches then policies for [`NameKind::Policy`].
    #[must_use]
    pub fn resolve(&self, kind: NameKind, text: &str) -> Option<usize> {
        self.index(kind).resolve(text)
    }

    /// The name of the object `text` names in `kind`'s table, as the ruleset spells it:
    /// `"quick"` gives `"Quick"` for speeds (`engine_api.resolve_name`).
    #[must_use]
    pub fn resolve_name(&self, kind: NameKind, text: &str) -> Option<&str> {
        self.name(kind, self.resolve(kind, text)?)
    }

    /// The position of the object called exactly `name` in `kind`'s table.
    #[must_use]
    pub fn lookup(&self, kind: NameKind, name: &str) -> Option<usize> {
        self.index(kind).lookup(name)
    }

    /// The name of the object at `pos` in `kind`'s table.
    #[must_use]
    pub fn name(&self, kind: NameKind, pos: usize) -> Option<&str> {
        self.index(kind).name(pos)
    }

    /// Every name in `kind`'s table, in table order.
    pub fn names(&self, kind: NameKind) -> impl ExactSizeIterator<Item = &str> {
        self.index(kind).names()
    }

    fn index(&self, kind: NameKind) -> &NameIndex {
        &self.names[kind as usize]
    }

    /// The ruleset as the browser and AI agents read it, as JSON text: Python's `to_client`
    /// (`rules.py:303-331`), with the objects as the files wrote them, in file order. Built on
    /// first use.
    #[must_use]
    pub fn client_json(&self) -> &str {
        self.client_json.get_or_init(|| client::build(self))
    }
}

/// Short: the tables run to thousands of lines.
impl core::fmt::Debug for Ruleset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ruleset").field("id", &self.id).field("counts", &self.counts()).finish()
    }
}

// Games on several threads share one ruleset.
const _: fn() = || {
    fn sync<T: Send + Sync>() {}
    sync::<Ruleset>();
};

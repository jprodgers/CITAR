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
//! - [`names`]: loose lookup by name or id, for tools, typed by the id it gives ([`Named`]);
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
pub use self::names::{NameKind, Named};
#[cfg(feature = "embedded-ruleset")]
pub use self::source::embedded;
pub use self::source::{BUILD_ID, RulesetFiles, RulesetId};

/// The whole ruleset: every table typed, every reference an id, and the derived tables.
///
/// Read-only once loaded: the tables are reached through accessors, so a ruleset cannot be
/// changed after its [`RulesetId`], name indexes and derived tables were computed from it. A
/// variant is made by loading edited files. Games share one through `&'static Ruleset`
/// ([`Ruleset::leak`], [`Ruleset::shared`]), which is why it is `Sync`: it has no interior
/// mutability except the client JSON, built once behind a `OnceLock`.
///
/// Inside the crate the fields are open, for the loader and the unique compiler (package 1a-05)
/// to fill in.
pub struct Ruleset {
    id: RulesetId,
    pub(crate) techs: IdVec<TechId, TechDef>,
    pub(crate) tech_columns: Vec<TechColumn>,
    pub(crate) eras: IdVec<EraId, EraDef>,
    pub(crate) base_units: IdVec<BaseUnitId, BaseUnitDef>,
    pub(crate) unit_types: IdVec<UnitTypeId, UnitTypeDef>,
    pub(crate) buildings: IdVec<BuildingId, BuildingDef>,
    pub(crate) promotions: IdVec<PromotionId, PromotionDef>,
    pub(crate) terrains: IdVec<TerrainId, TerrainDef>,
    pub(crate) resources: IdVec<ResourceId, ResourceDef>,
    pub(crate) improvements: IdVec<ImprovementId, ImprovementDef>,
    pub(crate) beliefs: IdVec<BeliefId, BeliefDef>,
    pub(crate) religions: IdVec<RulesReligionId, Box<str>>,
    pub(crate) specialists: IdVec<SpecialistId, SpecialistDef>,
    pub(crate) city_state_types: IdVec<CityStateTypeId, CityStateTypeDef>,
    pub(crate) difficulties: IdVec<DifficultyId, DifficultyDef>,
    pub(crate) speeds: IdVec<SpeedId, SpeedDef>,
    pub(crate) victories: IdVec<VictoryId, VictoryDef>,
    pub(crate) quests: IdVec<QuestKindId, QuestDef>,
    pub(crate) ruins: IdVec<RuinId, RuinDef>,
    pub(crate) personalities: IdVec<PersonalityId, PersonalityDef>,
    pub(crate) policies: IdVec<PolicyId, PolicyDef>,
    pub(crate) policy_branch_count: u16,
    pub(crate) nations: IdVec<NationId, NationDef>,
    pub(crate) global_uniques: Uniques,
    pub(crate) constants: Constants,
    pub(crate) fracs: IdVec<FracId, f64>,
    pub(crate) derived: Derived,
    names: [NameIndex; 16],
    client: client::ClientSource,
    client_json: OnceLock<String>,
}

/// The read-only tables.
impl Ruleset {
    /// The technologies, in file order.
    #[must_use]
    #[inline]
    pub fn techs(&self) -> &IdVec<TechId, TechDef> {
        &self.techs
    }

    /// The columns of the tech tree, in file order.
    #[must_use]
    #[inline]
    pub fn tech_columns(&self) -> &[TechColumn] {
        &self.tech_columns
    }

    /// The eras, in order of their number, which is their id.
    #[must_use]
    #[inline]
    pub fn eras(&self) -> &IdVec<EraId, EraDef> {
        &self.eras
    }

    /// The units of `units.json`.
    #[must_use]
    #[inline]
    pub fn base_units(&self) -> &IdVec<BaseUnitId, BaseUnitDef> {
        &self.base_units
    }

    /// The unit types: Melee, Mounted, ...
    #[must_use]
    #[inline]
    pub fn unit_types(&self) -> &IdVec<UnitTypeId, UnitTypeDef> {
        &self.unit_types
    }

    /// The buildings and wonders.
    #[must_use]
    #[inline]
    pub fn buildings(&self) -> &IdVec<BuildingId, BuildingDef> {
        &self.buildings
    }

    /// The promotions.
    #[must_use]
    #[inline]
    pub fn promotions(&self) -> &IdVec<PromotionId, PromotionDef> {
        &self.promotions
    }

    /// The terrains: base terrains, features and natural wonders.
    #[must_use]
    #[inline]
    pub fn terrains(&self) -> &IdVec<TerrainId, TerrainDef> {
        &self.terrains
    }

    /// The resources.
    #[must_use]
    #[inline]
    pub fn resources(&self) -> &IdVec<ResourceId, ResourceDef> {
        &self.resources
    }

    /// The improvements, including the routes and the remove, repair and cancel orders.
    #[must_use]
    #[inline]
    pub fn improvements(&self) -> &IdVec<ImprovementId, ImprovementDef> {
        &self.improvements
    }

    /// The beliefs.
    #[must_use]
    #[inline]
    pub fn beliefs(&self) -> &IdVec<BeliefId, BeliefDef> {
        &self.beliefs
    }

    /// The names of the religions players can found.
    #[must_use]
    #[inline]
    pub fn religions(&self) -> &IdVec<RulesReligionId, Box<str>> {
        &self.religions
    }

    /// The specialists.
    #[must_use]
    #[inline]
    pub fn specialists(&self) -> &IdVec<SpecialistId, SpecialistDef> {
        &self.specialists
    }

    /// The city-state types: Cultured, Maritime, ...
    #[must_use]
    #[inline]
    pub fn city_state_types(&self) -> &IdVec<CityStateTypeId, CityStateTypeDef> {
        &self.city_state_types
    }

    /// The difficulties, easiest first.
    #[must_use]
    #[inline]
    pub fn difficulties(&self) -> &IdVec<DifficultyId, DifficultyDef> {
        &self.difficulties
    }

    /// The game speeds.
    #[must_use]
    #[inline]
    pub fn speeds(&self) -> &IdVec<SpeedId, SpeedDef> {
        &self.speeds
    }

    /// The victories.
    #[must_use]
    #[inline]
    pub fn victories(&self) -> &IdVec<VictoryId, VictoryDef> {
        &self.victories
    }

    /// The city-state quests.
    #[must_use]
    #[inline]
    pub fn quests(&self) -> &IdVec<QuestKindId, QuestDef> {
        &self.quests
    }

    /// The rewards of the ancient ruins.
    #[must_use]
    #[inline]
    pub fn ruins(&self) -> &IdVec<RuinId, RuinDef> {
        &self.ruins
    }

    /// The AI leader personalities.
    #[must_use]
    #[inline]
    pub fn personalities(&self) -> &IdVec<PersonalityId, PersonalityDef> {
        &self.personalities
    }

    /// The policy branches, then the policies, each in file order.
    #[must_use]
    #[inline]
    pub fn policies(&self) -> &IdVec<PolicyId, PolicyDef> {
        &self.policies
    }

    /// How many of [`policies`](Self::policies) are branches.
    #[must_use]
    #[inline]
    pub fn policy_branch_count(&self) -> u16 {
        self.policy_branch_count
    }

    /// `ruleset/nations.json` with `custom/nations.json` merged in.
    #[must_use]
    #[inline]
    pub fn nations(&self) -> &IdVec<NationId, NationDef> {
        &self.nations
    }

    /// The uniques every civilization has.
    #[must_use]
    #[inline]
    pub fn global_uniques(&self) -> &Uniques {
        &self.global_uniques
    }

    /// `game.json`, typed.
    #[must_use]
    #[inline]
    pub fn constants(&self) -> &Constants {
        &self.constants
    }

    /// The fractional unique parameters, interned by the unique compiler (package 1a-05).
    #[must_use]
    #[inline]
    pub fn fracs(&self) -> &IdVec<FracId, f64> {
        &self.fracs
    }

    /// The tables derived at load.
    #[must_use]
    #[inline]
    pub fn derived(&self) -> &Derived {
        &self.derived
    }
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
    pub fn speed_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.speeds.as_slice().iter().map(|s| &*s.name)
    }

    /// The difficulties' names, easiest first (`engine_api.difficulties`).
    pub fn difficulty_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.difficulties.as_slice().iter().map(|d| &*d.name)
    }

    /// The object `text` names: exactly, or else by its normalised name or id
    /// (`rules.py:263-270`). The id type says which table: `r.resolve::<TechId>("bronze_working")`.
    #[must_use]
    pub fn resolve<I: Named>(&self, text: &str) -> Option<I> {
        self.index(I::KIND).resolve(text).and_then(I::from_index)
    }

    /// The object called exactly `name`.
    #[must_use]
    pub fn lookup<I: Named>(&self, name: &str) -> Option<I> {
        self.index(I::KIND).lookup(name).and_then(I::from_index)
    }

    /// The name of the object `id`, or `None` for an id this ruleset does not have.
    #[must_use]
    pub fn name<I: Named>(&self, id: I) -> Option<&str> {
        self.index(I::KIND).name(id.index())
    }

    /// The name of the object `text` names in `kind`'s table, as the ruleset spells it:
    /// `"quick"` gives `"Quick"` for speeds (`engine_api.resolve_name`). The facade's
    /// string-keyed form of [`resolve`](Self::resolve).
    #[must_use]
    pub fn resolve_name(&self, kind: NameKind, text: &str) -> Option<&str> {
        let index = self.index(kind);
        index.name(index.resolve(text)?)
    }

    /// Every name in `kind`'s table, in table order, which is id order.
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

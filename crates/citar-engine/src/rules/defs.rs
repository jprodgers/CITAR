//! The typed tables (DESIGN.md 5.3): one struct per ruleset object, with every reference to
//! another object resolved to its id, and each table an `IdVec` in file order.
//!
//! Python kept every object as the dict it read and looked names up at each use
//! (`rules.py:49-81`), adding a few private fields at load (`rules.py:100-167`). Here the names
//! are resolved once, when the ruleset loads, and the fields Python added (`_era`, `_domain`,
//! `_ranged`, `_rough`, `_stat_related`, ...) are ordinary fields filled in by `derived`.
//!
//! Each object's uniques are compiled when the ruleset loads (`unique::compile`): its `uniques`
//! field says which of the ruleset's compiled uniques are its own, split by what the engine does
//! with them ([`SourceUniques`]). The filters outside uniques are compiled with the rest
//! (`unique::filter`): an improvement's `terrainsCanBeBuiltOn` is a set of terrains, and a start
//! bias a tile filter; victory milestones are [`Milestone`]s.

use serde::Deserialize;

pub use super::gen_tables::{Milestone, MilestoneDef};
use crate::base::ids::{
    BaseUnitId, BuildingId, CityStateTypeId, DifficultyId, EraId, FeatureId, ImprovementId,
    NationId, PersonalityId, PolicyId, PromotionId, ResourceId, RulesReligionId, SpecialistId,
    TechId, TerrainId, TileFilterId, UnitTypeId,
};
use crate::base::sets::TerrainSet;
use crate::base::stats::{StatMask, Stats};
pub use crate::unique::SourceUniques;

// ---- Small vocabularies -----------------------------------------------------------------------

/// Where a unit moves: a unit type's `movementType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum Domain {
    Land,
    Water,
    Air,
}

/// A terrain's `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum TerrainType {
    /// A base terrain of land: Grassland, Plains, Mountain, ...
    Land,
    /// A base terrain of water: Ocean, Coast, Lakes.
    Water,
    /// A feature on top of a base terrain: Hill, Forest, Fallout, ...
    TerrainFeature,
    /// A natural wonder, which replaces a tile's features.
    NaturalWonder,
}

/// A resource's `resourceType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum ResourceType {
    Bonus,
    Luxury,
    Strategic,
}

/// A belief's `type`: which slot of a religion it fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum BeliefType {
    Pantheon,
    Founder,
    Follower,
    Enhancer,
}

/// A nation's `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NationKind {
    /// A civilization a player or a bot can lead.
    Major,
    /// A city-state.
    CityState,
    /// The barbarians.
    Barbarian,
}

/// A victory the AI can aim for: a nation's `preferredVictoryType` and the keys of a policy
/// branch's `priorities`. `Neutral` aims for none in particular.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum VictoryFocus {
    Neutral,
    Cultural,
    Diplomatic,
    Domination,
    Scientific,
}

/// Whether a city-state quest is given to one civilization or to all of them as a contest.
/// Python's default for a quest without `type` is `Individual` (`city_states.py:1037`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Deserialize)]
pub enum QuestScope {
    #[default]
    Individual,
    Global,
}

/// What a city-state quest asks for. Python told each quest's behaviour, and what its `data1`
/// held, by comparing the quest's name (`city_states.py:815-910, 1095-1191`); here the loader
/// resolves the name once, and a quest it does not know is an error rather than a quest that is
/// never given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuestKind {
    /// Connect a city to the city-state's capital by road.
    Route,
    /// Clear a barbarian camp near the city-state.
    ClearBarbarianCamp,
    /// Connect a luxury or strategic resource neither has.
    ConnectResource,
    /// Build a wonder.
    ConstructWonder,
    /// Acquire a kind of great person.
    AcquireGreatPerson,
    /// Conquer a nearby city-state.
    ConquerCityState,
    /// Bully a nearby city-state.
    BullyCityState,
    /// Find a civilization's city.
    FindPlayer,
    /// Find a natural wonder.
    FindNaturalWonder,
    /// Give gold, after the city-state was bullied.
    GiveGold,
    /// Pledge to protect the city-state, after it was bullied.
    PledgeToProtect,
    /// Denounce the civilization that bullied the city-state.
    DenounceCivilization,
    /// Make one's religion the majority in the city-state's capital.
    SpreadReligion,
    /// A contest: gain the most culture.
    ContestCulture,
    /// A contest: gain the most faith.
    ContestFaith,
    /// A contest: research the most technologies.
    ContestTechnologies,
    /// Gifts of gold are worth more for a while.
    Invest,
}

impl QuestKind {
    /// Every kind, in the order Python tested them (`city_states.py:823-908`).
    pub const ALL: [Self; 17] = [
        Self::Route,
        Self::ClearBarbarianCamp,
        Self::ConnectResource,
        Self::ConstructWonder,
        Self::AcquireGreatPerson,
        Self::ConquerCityState,
        Self::BullyCityState,
        Self::FindPlayer,
        Self::FindNaturalWonder,
        Self::GiveGold,
        Self::PledgeToProtect,
        Self::DenounceCivilization,
        Self::SpreadReligion,
        Self::ContestCulture,
        Self::ContestFaith,
        Self::ContestTechnologies,
        Self::Invest,
    ];

    /// The quest's name in `quests.json`: `Clear Barbarian Camp`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Route => "Route",
            Self::ClearBarbarianCamp => "Clear Barbarian Camp",
            Self::ConnectResource => "Connect Resource",
            Self::ConstructWonder => "Construct Wonder",
            Self::AcquireGreatPerson => "Acquire Great Person",
            Self::ConquerCityState => "Conquer City State",
            Self::BullyCityState => "Bully City State",
            Self::FindPlayer => "Find Player",
            Self::FindNaturalWonder => "Find Natural Wonder",
            Self::GiveGold => "Give Gold",
            Self::PledgeToProtect => "Pledge to Protect",
            Self::DenounceCivilization => "Denounce Civilization",
            Self::SpreadReligion => "Spread Religion",
            Self::ContestCulture => "Contest Culture",
            Self::ContestFaith => "Contest Faith",
            Self::ContestTechnologies => "Contest Technologies",
            Self::Invest => "Invest",
        }
    }

    /// The kind whose quest is called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// What a quest of this kind is about, which Python stored in `data1`
    /// (`city_states.py:823-908`).
    #[must_use]
    pub const fn target(self) -> QuestTargetKind {
        match self {
            Self::Route => QuestTargetKind::None,
            Self::ClearBarbarianCamp => QuestTargetKind::Tile,
            Self::ConnectResource => QuestTargetKind::Resource,
            Self::ConstructWonder => QuestTargetKind::Building,
            Self::AcquireGreatPerson => QuestTargetKind::UnitType,
            Self::ConquerCityState
            | Self::BullyCityState
            | Self::FindPlayer
            | Self::GiveGold
            | Self::PledgeToProtect
            | Self::DenounceCivilization => QuestTargetKind::Player,
            Self::FindNaturalWonder => QuestTargetKind::NaturalWonder,
            Self::SpreadReligion => QuestTargetKind::Religion,
            Self::ContestCulture | Self::ContestFaith | Self::ContestTechnologies => {
                QuestTargetKind::Baseline
            }
            Self::Invest => QuestTargetKind::Percent,
        }
    }
}

/// Which kind of `QuestTarget` a quest holds (DESIGN.md 4.5): Python's `data1`, typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuestTargetKind {
    /// Nothing: the route quest.
    None,
    /// A tile: the barbarian camp to clear.
    Tile,
    /// A resource to connect.
    Resource,
    /// A wonder to build.
    Building,
    /// A great person to acquire, as its base unit (Python's unit `type`).
    UnitType,
    /// A player: the city-state to conquer or bully, the civilization to find, or the bully to
    /// pay off, protect against or denounce.
    Player,
    /// A natural wonder to find.
    NaturalWonder,
    /// The religion to spread.
    Religion,
    /// A contest's starting score: culture, faith or technologies when the quest was given.
    Baseline,
    /// The investment bonus in percent, from the quest's `params` (`city_states.py:907-908`).
    Percent,
}

/// A city-state's personality (`city_states.py:17`), one of the two things a quest's weights are
/// keyed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CityStatePersonality {
    Friendly,
    Neutral,
    Hostile,
    Irrational,
}

impl CityStatePersonality {
    /// Every personality, in Python's order.
    pub const ALL: [Self; 4] = [Self::Friendly, Self::Neutral, Self::Hostile, Self::Irrational];

    /// The name the ruleset and the saves write: `Friendly`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Friendly => "Friendly",
            Self::Neutral => "Neutral",
            Self::Hostile => "Hostile",
            Self::Irrational => "Irrational",
        }
    }

    /// The personality called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// A route level (`workers.py:25-26`, `ROAD_RANK`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Route {
    Road = 1,
    Railroad = 2,
}

/// What building an improvement does, where Python told by its name (`workers.py:22-26`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImprovementKind {
    /// An improvement proper: a farm, a mine, a great improvement, ...
    Normal,
    /// A road or a railroad, which is a route rather than an improvement.
    Route(Route),
    /// `Remove Forest` and the like: clears one feature.
    RemoveFeature(FeatureId),
    /// `Remove Road`, `Remove Railroad`.
    RemoveRoute(Route),
    /// `Remove <improvement>`: clears an improvement.
    RemoveImprovement(ImprovementId),
    /// `Cancel improvement order`.
    Cancel,
    /// `Repair`: repairs a pillaged improvement.
    Repair,
}

/// One entry of a nation's start bias, as the map generator reads it (`mapgen.py:1000-1012`).
/// Its filter is one map generation reads, from the terrain alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartBias {
    /// `Coast`: a coastal start.
    Coast,
    /// A tile filter the start should have nearby.
    Prefer(TileFilterId),
    /// `Avoid [filter]`: a tile filter the start should not have nearby.
    Avoid(TileFilterId),
}

/// A start bias entry as written, before its filter is compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartBiasText<'a> {
    Coast,
    Prefer(&'a str),
    Avoid(&'a str),
}

impl StartBias {
    /// Reads one entry as `_bias_score_raw` did.
    pub(crate) fn read(text: &str) -> StartBiasText<'_> {
        if text == "Coast" {
            StartBiasText::Coast
        } else if let Some(f) = text.strip_prefix("Avoid [").and_then(|t| t.strip_suffix(']')) {
            StartBiasText::Avoid(f)
        } else {
            StartBiasText::Prefer(text)
        }
    }
}

/// A unit a difficulty gives at the start: a unit by name, or `Era Starting Unit`, the starting
/// era's military unit (`units.py:166-171`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartingUnit {
    Unit(BaseUnitId),
    EraStartingUnit,
}

/// The name a difficulty writes for [`StartingUnit::EraStartingUnit`].
pub const ERA_STARTING_UNIT: &str = "Era Starting Unit";

/// How much of a strategic resource a deposit holds, by the map's resource setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deposit {
    pub sparse: i32,
    pub default: i32,
    pub abundant: i32,
}

/// One stretch of a speed's calendar: `years_per_turn` years a turn up to turn `until_turn`.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SpeedTurns {
    pub years_per_turn: f64,
    pub until_turn: i32,
}

/// The units that can build the same improvements share a builder class: those whose
/// `Can build [...] improvements on tiles` uniques name the same filters. An index into
/// `Derived::builder_classes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuilderClass(pub u8);

// ---- The tables -------------------------------------------------------------------------------

/// A technology.
#[derive(Clone, Debug, PartialEq)]
pub struct TechDef {
    pub name: Box<str>,
    /// The snake_case id tools accept: `bronze_working`.
    pub key: Option<Box<str>>,
    pub era: EraId,
    /// Its column in the tech tree.
    pub column: u16,
    pub cost: i32,
    pub prerequisites: Box<[TechId]>,
    pub uniques: SourceUniques,
}

/// A column of the tech tree, with the costs UnCiv attaches to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TechColumn {
    pub column: u16,
    pub era: EraId,
    pub tech_cost: i32,
    pub building_cost: i32,
    pub wonder_cost: i32,
}

/// An era. Its id is its `number`: the loader requires eras in order, numbered from 0.
#[derive(Clone, Debug, PartialEq)]
pub struct EraDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub research_agreement_cost: i32,
    /// Python's defaults when absent: 1 settler, 0 workers, 1 military unit, a Warrior, no gold
    /// and no culture (`units.py:165-166`, `game.py:273`).
    pub starting_settler_count: i32,
    pub starting_worker_count: i32,
    pub starting_military_unit_count: i32,
    pub starting_military_unit: BaseUnitId,
    pub starting_gold: i32,
    pub starting_culture: i32,
    pub settler_population: i32,
    /// Buildings a city founded in this era starts with.
    pub settler_buildings: Box<[BuildingId]>,
    /// Wonders that can no longer be built by a game starting in this era.
    pub starting_obsolete_wonders: Box<[BuildingId]>,
    pub base_unit_buy_cost: i32,
    pub embark_defense: i32,
    pub start_percent: i32,
    pub uniques: SourceUniques,
}

/// A building or wonder.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildingDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    /// Production cost; -1 when the file gives none, as Python defaulted it (`rules.py:148`).
    pub cost: i32,
    pub stats: Stats,
    pub percent_stat_bonus: Stats,
    pub is_wonder: bool,
    pub is_national_wonder: bool,
    pub city_strength: f64,
    pub city_health: i32,
    pub hurry_cost_modifier: Option<i32>,
    pub maintenance: i32,
    pub required_tech: Option<TechId>,
    pub required_building: Option<BuildingId>,
    pub required_resource: Option<ResourceId>,
    pub required_nearby_improved_resources: Box<[ResourceId]>,
    pub replaces: Option<BuildingId>,
    pub unique_to: Option<NationId>,
    /// Great person points per turn, by the great person unit.
    pub great_person_points: Box<[(BaseUnitId, i32)]>,
    pub specialist_slots: Box<[(SpecialistId, i32)]>,
    pub uniques: SourceUniques,
    /// A wonder or a national wonder (`rules.py:146`).
    pub any_wonder: bool,
    /// The stats the building raises, for the AI's valuation (`rules.py:169-180`).
    pub stat_related: StatMask,
}

/// A unit of `units.json` (UnCiv's base unit).
#[derive(Clone, Debug, PartialEq)]
pub struct BaseUnitDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub unit_type: UnitTypeId,
    pub movement: i32,
    /// Production cost; 0 when the file gives none (`rules.py:122`).
    pub cost: i32,
    pub hurry_cost_modifier: Option<i32>,
    /// 0 when the file gives none (`rules.py:119`).
    pub strength: i32,
    /// 0 when the file gives none (`rules.py:120`).
    pub ranged_strength: i32,
    /// 2 when the file gives none (`rules.py:121`).
    pub range: i32,
    pub intercept_range: i32,
    pub religious_strength: i32,
    pub required_tech: Option<TechId>,
    pub obsolete_tech: Option<TechId>,
    pub upgrades_to: Option<BaseUnitId>,
    pub unique_to: Option<NationId>,
    pub replaces: Option<BaseUnitId>,
    pub required_resource: Option<ResourceId>,
    /// Promotions the unit starts with.
    pub promotions: Box<[PromotionId]>,
    pub uniques: SourceUniques,
    /// Its unit type's domain (`rules.py:123`).
    pub domain: Domain,
    /// Has ranged strength (`rules.py:124`).
    pub ranged: bool,
    /// Has melee strength and no ranged strength (`rules.py:125`).
    pub melee: bool,
    /// Ranged or melee (`rules.py:126`).
    pub military: bool,
    /// The era of its required tech, or the first era (`rules.py:128`).
    pub era: EraId,
    /// Carries `Great Person - [...]` (`rules.py:129`).
    pub great_person: bool,
    /// Which builder class it belongs to, if it can build improvements.
    pub builder: Option<BuilderClass>,
}

/// A unit type of `unit_types.json`: Melee, Mounted, ...
#[derive(Clone, Debug, PartialEq)]
pub struct UnitTypeDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub domain: Domain,
    pub uniques: SourceUniques,
}

/// A promotion.
#[derive(Clone, Debug, PartialEq)]
pub struct PromotionDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    /// The unit types that may take it.
    pub unit_types: Box<[UnitTypeId]>,
    pub prerequisites: Box<[PromotionId]>,
    pub uniques: SourceUniques,
}

/// A terrain: a base terrain, a feature or a natural wonder.
#[derive(Clone, Debug, PartialEq)]
pub struct TerrainDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub kind: TerrainType,
    pub stats: Stats,
    /// 1 when the file gives none (`rules.py:137`).
    pub movement_cost: i32,
    pub impassable: bool,
    pub defence_bonus: f64,
    pub override_stats: bool,
    pub unbuildable: bool,
    /// The terrains a feature can lie on.
    pub occurs_on: Box<[TerrainId]>,
    /// The base terrain a natural wonder turns its tile into.
    pub turns_into: Option<TerrainId>,
    /// The map generator's weight; Python's default is 10 (`mapgen.py:1114`).
    pub weight: Option<i32>,
    pub uniques: SourceUniques,
    /// Carries `Rough terrain` (`rules.py:136`).
    pub rough: bool,
    /// Its place in the feature layers, if it is a feature.
    pub feature: Option<FeatureId>,
}

/// A resource.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub kind: ResourceType,
    pub stats: Stats,
    pub terrains_can_be_found_on: Box<[TerrainId]>,
    /// The improvement that improves it.
    pub improvement: Option<ImprovementId>,
    /// Other improvements that improve it too.
    pub improved_by: Box<[ImprovementId]>,
    /// What improving it adds.
    pub improvement_stats: Stats,
    pub revealed_by: Option<TechId>,
    pub major_deposit_amount: Option<Deposit>,
    pub minor_deposit_amount: Option<Deposit>,
    pub uniques: SourceUniques,
}

/// A tile improvement, including the pseudo-improvements that build routes, remove features,
/// repair and cancel.
#[derive(Clone, Debug, PartialEq)]
pub struct ImprovementDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub stats: Stats,
    /// The terrains it can be built on: its `terrainsCanBeBuiltOn` filters, each read as
    /// `terrain_matches` reads one (`workers.py:44-46`).
    pub terrains_can_be_built_on: TerrainSet,
    pub turns_to_build: Option<i32>,
    pub tech_required: Option<TechId>,
    pub unique_to: Option<NationId>,
    pub uniques: SourceUniques,
    /// What building it does (`workers.py:22-26`).
    pub kind: ImprovementKind,
    /// Carries `Great Improvement` (`rules.py:143`).
    pub great: bool,
}

/// A belief.
#[derive(Clone, Debug, PartialEq)]
pub struct BeliefDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub kind: BeliefType,
    pub uniques: SourceUniques,
}

/// A specialist.
#[derive(Clone, Debug, PartialEq)]
pub struct SpecialistDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    /// What one specialist yields (`rules.py:166-167`).
    pub stats: Stats,
    /// Great person points per turn, by the great person unit.
    pub great_person_points: Box<[(BaseUnitId, i32)]>,
}

/// A city-state type: Cultured, Maritime, ...
#[derive(Clone, Debug, PartialEq)]
pub struct CityStateTypeDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    /// What a city-state of this type gives its friends.
    pub friend: SourceUniques,
    /// What it gives its ally.
    pub ally: SourceUniques,
    pub uniques: SourceUniques,
}

/// A difficulty level. Its id orders them, easiest first, as Python's `difficulty_list` did.
#[derive(Clone, Debug, PartialEq)]
pub struct DifficultyDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub base_happiness: i32,
    pub extra_happiness_per_luxury: i32,
    pub research_cost_modifier: f64,
    pub unit_cost_modifier: f64,
    pub unit_supply_base: i32,
    pub unit_supply_per_city: i32,
    pub building_cost_modifier: f64,
    pub policy_cost_modifier: f64,
    pub unhappiness_modifier: f64,
    pub barbarian_bonus: f64,
    pub barbarian_spawn_delay: i32,
    pub player_bonus_starting_units: Box<[StartingUnit]>,
    /// The difficulty whose base values AI players use (`economy.py:39-43`).
    pub ai_difficulty_level: DifficultyId,
    pub ai_city_growth_modifier: f64,
    pub ai_unit_cost_modifier: f64,
    pub ai_building_cost_modifier: f64,
    pub ai_wonder_cost_modifier: f64,
    pub ai_building_maintenance_modifier: f64,
    pub ai_unit_maintenance_modifier: f64,
    pub ai_unit_supply_modifier: f64,
    pub ai_free_techs: Box<[TechId]>,
    pub ai_major_civ_bonus_starting_units: Box<[StartingUnit]>,
    pub ai_city_state_bonus_starting_units: Box<[StartingUnit]>,
    pub ai_unhappiness_modifier: f64,
    pub ais_exchange_techs: bool,
    pub turn_barbarians_can_enter_player_tiles: i32,
    pub clear_barbarian_camp_reward: i32,
}

/// A game speed.
#[derive(Clone, Debug, PartialEq)]
pub struct SpeedDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub modifier: f64,
    pub production_cost_modifier: f64,
    pub gold_cost_modifier: f64,
    pub science_cost_modifier: f64,
    pub culture_cost_modifier: f64,
    pub faith_cost_modifier: f64,
    pub improvement_build_length_modifier: f64,
    pub barbarian_modifier: f64,
    pub gold_gift_modifier: f64,
    pub city_state_tribute_scaling_interval: f64,
    pub golden_age_length_modifier: f64,
    pub religious_pressure_adjacent_city: i32,
    pub peace_deal_duration: i32,
    pub deal_duration: i32,
    pub start_year: i32,
    /// The calendar: never empty, and `until_turn` rises strictly.
    pub turns: Box<[SpeedTurns]>,
}

impl SpeedDef {
    /// The last turn of a game at this speed: the end of its calendar (`rules.py:228`).
    #[must_use]
    pub fn max_turns(&self) -> i32 {
        self.turns.last().map_or(0, |t| t.until_turn)
    }
}

/// A victory.
#[derive(Clone, Debug, PartialEq)]
pub struct VictoryDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    /// What winning it takes, in order.
    pub milestones: Box<[MilestoneDef]>,
    /// The spaceship parts a scientific victory needs, one entry per part.
    pub required_spaceship_parts: Box<[BaseUnitId]>,
    pub hidden_in_victory_screen: bool,
}

/// A kind of city-state quest.
#[derive(Clone, Debug, PartialEq)]
pub struct QuestDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    /// What the quest asks for, from its name.
    pub kind: QuestKind,
    /// What its target is: `kind.target()`.
    pub target: QuestTargetKind,
    pub scope: QuestScope,
    /// Python's default is 40 (`city_states.py:1059`).
    pub influence: Option<i32>,
    /// Python's default is 0 (`city_states.py:1059`).
    pub duration: Option<i32>,
    /// Python's default is 1 (`city_states.py:1020`).
    pub minimum_civs: Option<i32>,
    /// Weight factors by the city-state's type (`city_states.py:951-961`).
    pub weight_by_type: Box<[(CityStateTypeId, f64)]>,
    /// Weight factors by the city-state's personality.
    pub weight_by_personality: Box<[(CityStatePersonality, f64)]>,
    pub params: Box<[f64]>,
}

/// A reward of the ancient ruins.
#[derive(Clone, Debug, PartialEq)]
pub struct RuinDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub excluded_difficulties: Box<[DifficultyId]>,
    pub uniques: SourceUniques,
}

/// An AI leader personality: weights the bot gives each concern, 0 to 10.
#[derive(Clone, Debug, PartialEq)]
pub struct PersonalityDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub production: i32,
    pub food: i32,
    pub gold: i32,
    pub science: i32,
    pub culture: i32,
    pub happiness: i32,
    pub faith: i32,
    pub military: i32,
    pub aggressive: i32,
    pub declare_war: i32,
    pub commerce: i32,
    pub diplomacy: i32,
    pub loyal: i32,
    pub expansion: i32,
    pub denounce_willingness: i32,
    /// How much it favours each policy branch.
    pub priorities: Box<[(PolicyId, i32)]>,
}

/// A policy branch or a policy: one id space, branches first.
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub uniques: SourceUniques,
    pub kind: PolicyKind,
}

/// Whether a [`PolicyDef`] is a branch or a policy of one.
#[derive(Clone, Debug, PartialEq)]
pub enum PolicyKind {
    /// A branch, which is adopted like a policy and opens its members.
    Branch {
        era: EraId,
        /// How much each victory focus favours the branch.
        priorities: Box<[(VictoryFocus, i32)]>,
        members: Box<[PolicyId]>,
    },
    /// A policy of a branch.
    Member {
        branch: PolicyId,
        /// Policies or branches that must be adopted first.
        requires: Box<[PolicyId]>,
        /// Adopted for free once the rest of the branch is.
        finisher: bool,
    },
}

impl PolicyDef {
    /// Whether this is a branch.
    #[must_use]
    pub fn is_branch(&self) -> bool {
        matches!(self.kind, PolicyKind::Branch { .. })
    }
}

/// A nation: a civilization, a city-state or the barbarians.
#[derive(Clone, Debug, PartialEq)]
pub struct NationDef {
    pub name: Box<str>,
    pub key: Option<Box<str>>,
    pub kind: NationKind,
    pub leader_name: Option<Box<str>>,
    pub adjective: Option<Box<str>>,
    pub start_bias: Box<[StartBias]>,
    pub preferred_victory_type: Option<VictoryFocus>,
    pub personality: Option<PersonalityId>,
    pub favored_religion: Option<RulesReligionId>,
    pub city_state_type: Option<CityStateTypeId>,
    /// City names, in the order cities take them.
    pub cities: Box<[Box<str>]>,
    /// CITAR's benchmark civilization, which has no unique ability.
    pub benchmark: bool,
    pub uniques: SourceUniques,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_biases_read_as_the_map_generator_read_them() {
        assert_eq!(StartBias::read("Coast"), StartBiasText::Coast);
        assert_eq!(StartBias::read("Avoid [Tundra]"), StartBiasText::Avoid("Tundra"));
        assert_eq!(StartBias::read("Jungle"), StartBiasText::Prefer("Jungle"));
        assert_eq!(StartBias::read("Avoid [Tundra"), StartBiasText::Prefer("Avoid [Tundra"));
    }

    #[test]
    fn quest_kinds_by_name() {
        for k in QuestKind::ALL {
            assert_eq!(QuestKind::from_name(k.name()), Some(k));
        }
        assert_eq!(QuestKind::from_name("route"), None, "exact names only");
        assert_eq!(QuestKind::ClearBarbarianCamp.target(), QuestTargetKind::Tile);
        assert_eq!(QuestKind::GiveGold.target(), QuestTargetKind::Player);
        assert_eq!(QuestKind::ContestFaith.target(), QuestTargetKind::Baseline);
        assert_eq!(QuestKind::Invest.target(), QuestTargetKind::Percent);
    }

    #[test]
    fn personalities_by_name() {
        assert_eq!(CityStatePersonality::from_name("Hostile"), Some(CityStatePersonality::Hostile));
        assert_eq!(CityStatePersonality::from_name("hostile"), None);
    }
}

//! The loader: files to a checked, typed [`Ruleset`] (DESIGN.md 5.3), replacing `Rules.__init__`,
//! `_prepare`, `_index` and `_validate` (`rules.py:44-260`).
//!
//! It runs in stages, each collecting every problem it finds and the loader stopping after any
//! stage that found one, since the next would only report its consequences:
//! 1. the files: all present, none unknown, each strict JSON (`source`);
//! 2. the rows: each fits its struct, no unknown field (`raw`);
//! 3. the sizes: every table fits its id type and its set;
//! 4. the references: every name resolves, in the table it must;
//! 5. the rules the data must follow: names match keys, eras numbered in order, speed calendars
//!    in order, branches and their policies in agreement, and the `game.json` values later code
//!    divides by, loops over or seats players with in range;
//! 6. the derived tables, which may need objects the engine names (Hill, Road).
//!
//! Python checked only the references of stage 4 for techs, units and buildings
//! (`rules.py:238-260`).

use super::constants::{BarbarianLevel, Constants, MapSize, MapType, RawGame};
use super::defs::{
    BaseUnitDef, BeliefDef, BuildingDef, CityStatePersonality, CityStateTypeDef, DifficultyDef,
    Domain, ERA_STARTING_UNIT, EraDef, ImprovementDef, ImprovementKind, NationDef, PersonalityDef,
    PolicyDef, PolicyKind, PromotionDef, QuestDef, QuestKind, ResourceDef, RuinDef, SpecialistDef,
    SpeedDef, StartBias, StartingUnit, TechColumn, TechDef, TerrainDef, Uniques, UnitTypeDef,
    VictoryDef,
};
use super::derived::{self, Derived};
use super::errors::{Problems, RulesetErrorKind, RulesetErrors};
use super::names::{NameIndex, NameKind};
use super::raw::{self, RawRuleset, Table};
use super::source::{self, GAME, RulesetFiles};
use super::{Ruleset, client};
use crate::base::hex::{MAX_SIDE, MIN_SIDE};
use crate::base::ids::{BaseUnitId, DifficultyId, EraId, Id, IdVec, PolicyId, SpeedId, UnitTypeId};
use crate::base::sets::{
    BaseUnitSet, BeliefSet, BuildingSet, EraSet, ImprovementSet, PlayerSet, PolicySet,
    PromotionSet, ResourceSet, TechSet, TerrainSet,
};
use crate::base::stats::StatMask;

pub(super) fn load(files: &RulesetFiles<'_>) -> Result<Ruleset, RulesetErrors> {
    let mut p = Problems::default();
    let docs = source::parse_files(files, &mut p);
    p.check()?;
    let id = source::ruleset_id(&docs);
    let raw = raw::read(&docs, &mut p);
    p.check()?;
    check_sizes(&raw, &mut p);
    p.check()?;
    let rules = link(&raw, &mut p);
    p.check()?;
    let Some(mut rules) = rules else { return Err(gave_up(&mut p, "the tables")) };
    check_rules(&raw, &rules, &mut p);
    p.check()?;
    let derived = derived::derive(&mut rules, &mut p);
    p.check()?;
    let Some(derived) = derived else { return Err(gave_up(&mut p, "the derived tables")) };
    rules.id = id;
    rules.derived = derived;
    rules.names = name_indexes(&raw);
    rules.client = client::ClientSource { docs: docs.into_vec(), nations: raw.nations_json };
    Ok(rules)
}

/// The report for a stage that gave up without saying why, which no stage does: every `None`
/// comes with a problem pushed. Kept so that a slip there is an error rather than a panic.
fn gave_up(p: &mut Problems, what: &str) -> RulesetErrors {
    p.push(RulesetErrorKind::Invalid, "", "", format!("{what} could not be built"));
    p.check().err().unwrap_or_else(|| RulesetErrors(Vec::new()))
}

// ---- Stage 3: sizes ---------------------------------------------------------------------------

/// Every table within its id type and its set. The message names what to raise.
fn check_sizes(raw: &RawRuleset, p: &mut Problems) {
    const U8: usize = 1 << 8;
    const U16: usize = 1 << 16;
    // (the file's table, its size, the most its holder takes, the holder and how to widen it)
    let sizes = [
        ("techs", raw.techs.len(), TechSet::CAPACITY, "a TechSet: raise sets::TECH_WORDS"),
        (
            "policies",
            raw.policy_branches.len() + raw.policies.len(),
            PolicySet::CAPACITY,
            "a PolicySet: raise sets::POLICY_WORDS",
        ),
        (
            "buildings",
            raw.buildings.len(),
            BuildingSet::CAPACITY,
            "a BuildingSet: raise sets::BUILDING_WORDS",
        ),
        (
            "units",
            raw.units.len(),
            BaseUnitSet::CAPACITY,
            "a BaseUnitSet: raise sets::BASE_UNIT_WORDS",
        ),
        (
            "promotions",
            raw.promotions.len(),
            PromotionSet::CAPACITY,
            "a PromotionSet: raise sets::PROMOTION_WORDS",
        ),
        (
            "beliefs",
            raw.beliefs.len(),
            BeliefSet::CAPACITY,
            "a BeliefSet: raise sets::BELIEF_WORDS",
        ),
        (
            "terrains",
            raw.terrains.len(),
            TerrainSet::CAPACITY,
            "a TerrainSet: raise sets::TERRAIN_WORDS",
        ),
        (
            "resources",
            raw.resources.len(),
            ResourceSet::CAPACITY,
            "a ResourceSet: raise sets::RESOURCE_WORDS",
        ),
        (
            "improvements",
            raw.improvements.len(),
            ImprovementSet::CAPACITY,
            "an ImprovementSet: raise sets::IMPROVEMENT_WORDS",
        ),
        ("eras", raw.eras.len(), EraSet::CAPACITY, "an EraSet: raise sets::ERA_WORDS"),
        ("unit_types", raw.unit_types.len(), U16, "a UnitTypeId (u16)"),
        ("nations", raw.nations.len(), U16, "a NationId (u16)"),
        ("personalities", raw.personalities.len(), U16, "a PersonalityId (u16)"),
        ("quests", raw.quests.len(), U16, "a QuestKindId (u16)"),
        ("ruins", raw.ruins.len(), U16, "a RuinId (u16)"),
        ("speeds", raw.speeds.len(), U8, "a SpeedId (u8)"),
        ("difficulties", raw.difficulties.len(), U8, "a DifficultyId (u8)"),
        ("victories", raw.victories.len(), U8, "a VictoryId (u8)"),
        ("specialists", raw.specialists.len(), U8, "a SpecialistId (u8)"),
        ("city_state_types", raw.city_state_types.len(), U8, "a CityStateTypeId (u8)"),
        ("religions", raw.religions.len(), U8, "a RulesReligionId (u8)"),
    ];
    for (table, len, cap, holder) in sizes {
        if len > cap {
            p.push(
                RulesetErrorKind::Capacity,
                &format!("ruleset/{table}.json"),
                "",
                format!("{len} {table}, more than {cap}, which is all {holder} holds"),
            );
        }
    }
}

// ---- Stage 4: references ----------------------------------------------------------------------

/// The tables a name can refer to.
#[derive(Clone, Copy, Debug)]
enum Tab {
    Techs,
    Eras,
    Units,
    UnitTypes,
    Buildings,
    Promotions,
    Terrains,
    Resources,
    Improvements,
    Specialists,
    CityStateTypes,
    Difficulties,
    Nations,
    Personalities,
    /// Branches and policies, one id space, branches first.
    Policies,
    Branches,
    /// The policies alone, in the same id space: what a branch may list as its members.
    Members,
    Religions,
    Speeds,
}

impl Tab {
    fn file(self) -> &'static str {
        match self {
            Self::Techs => "ruleset/techs.json",
            Self::Eras => "ruleset/eras.json",
            Self::Units => "ruleset/units.json",
            Self::UnitTypes => "ruleset/unit_types.json",
            Self::Buildings => "ruleset/buildings.json",
            Self::Promotions => "ruleset/promotions.json",
            Self::Terrains => "ruleset/terrains.json",
            Self::Resources => "ruleset/resources.json",
            Self::Improvements => "ruleset/improvements.json",
            Self::Specialists => "ruleset/specialists.json",
            Self::CityStateTypes => "ruleset/city_state_types.json",
            Self::Difficulties => "ruleset/difficulties.json",
            Self::Nations => "ruleset/nations.json",
            Self::Personalities => "ruleset/personalities.json",
            Self::Policies => "ruleset/policies.json",
            Self::Branches => "ruleset/policies.json (branches)",
            Self::Members => "ruleset/policies.json (policies)",
            Self::Religions => "ruleset/religions.json",
            Self::Speeds => "ruleset/speeds.json",
        }
    }

    fn position(self, raw: &RawRuleset, name: &str) -> Option<usize> {
        match self {
            Self::Techs => raw.techs.get_index_of(name),
            Self::Eras => raw.eras.get_index_of(name),
            Self::Units => raw.units.get_index_of(name),
            Self::UnitTypes => raw.unit_types.get_index_of(name),
            Self::Buildings => raw.buildings.get_index_of(name),
            Self::Promotions => raw.promotions.get_index_of(name),
            Self::Terrains => raw.terrains.get_index_of(name),
            Self::Resources => raw.resources.get_index_of(name),
            Self::Improvements => raw.improvements.get_index_of(name),
            Self::Specialists => raw.specialists.get_index_of(name),
            Self::CityStateTypes => raw.city_state_types.get_index_of(name),
            Self::Difficulties => raw.difficulties.get_index_of(name),
            Self::Nations => raw.nations.get_index_of(name),
            Self::Personalities => raw.personalities.get_index_of(name),
            Self::Policies => raw
                .policy_branches
                .get_index_of(name)
                .or_else(|| raw.policies.get_index_of(name).map(|i| i + raw.policy_branches.len())),
            Self::Branches => raw.policy_branches.get_index_of(name),
            Self::Members => raw.policies.get_index_of(name).map(|i| i + raw.policy_branches.len()),
            Self::Religions => raw.religions.iter().position(|r| r == name),
            Self::Speeds => raw.speeds.get_index_of(name),
        }
    }
}

/// Resolves names while reporting the ones that do not resolve.
struct Linker<'a> {
    raw: &'a RawRuleset,
    p: &'a mut Problems,
    file: &'static str,
    object: &'a str,
}

impl<'a> Linker<'a> {
    fn at(&mut self, file: &'static str, object: &'a str) {
        self.file = file;
        self.object = object;
    }

    /// The id of `name` in `tab`, or a report naming `field`.
    fn one<I: Id>(&mut self, field: &str, tab: Tab, name: &str) -> Option<I> {
        let found = tab.position(self.raw, name).and_then(I::from_index);
        if found.is_none() {
            self.p.push(
                RulesetErrorKind::UnknownReference,
                self.file,
                self.object,
                format!("{field} {name:?} is not in {}", tab.file()),
            );
        }
        found
    }

    fn opt<I: Id>(&mut self, field: &str, tab: Tab, name: Option<&String>) -> Option<I> {
        name.and_then(|n| self.one(field, tab, n))
    }

    fn all<I: Id>(&mut self, field: &str, tab: Tab, names: &[String]) -> Box<[I]> {
        names.iter().filter_map(|n| self.one(field, tab, n)).collect()
    }

    /// Names mapped to amounts, keyed by the objects they name.
    fn keyed<I: Id, V: Copy>(&mut self, field: &str, tab: Tab, map: &Table<V>) -> Box<[(I, V)]> {
        map.iter().filter_map(|(n, &v)| self.one(field, tab, n).map(|id| (id, v))).collect()
    }

    fn starting_units(&mut self, field: &str, names: &[String]) -> Box<[StartingUnit]> {
        names
            .iter()
            .filter_map(|n| {
                if n == ERA_STARTING_UNIT {
                    Some(StartingUnit::EraStartingUnit)
                } else {
                    self.one(field, Tab::Units, n).map(StartingUnit::Unit)
                }
            })
            .collect()
    }
}

fn text(s: &str) -> Box<str> {
    s.into()
}

fn key(id: Option<&String>) -> Option<Box<str>> {
    id.map(|s| text(s))
}

fn uniques(list: &[String]) -> Uniques {
    list.iter().map(|s| text(s)).collect()
}

fn texts(list: &[String]) -> Box<[Box<str>]> {
    list.iter().map(|s| text(s)).collect()
}

/// Every table typed, every name resolved. Derived fields are left at neutral values for
/// `derived` to fill in. `None` only without a `game.json`, which the raw stage reported.
#[allow(clippy::too_many_lines, reason = "one short block per table reads best in one place")]
fn link(raw: &RawRuleset, p: &mut Problems) -> Option<Ruleset> {
    let Some(game) = &raw.game else {
        p.push(RulesetErrorKind::MissingFile, GAME, "", "the file is missing");
        return None;
    };
    let mut l = Linker { raw, p, file: "", object: "" };

    let mut techs = IdVec::with_capacity(raw.techs.len());
    for (name, t) in &raw.techs {
        l.at("ruleset/techs.json", name);
        push(
            &mut techs,
            TechDef {
                name: text(name),
                key: key(t.id.as_ref()),
                era: l.one("era", Tab::Eras, &t.era).unwrap_or(EraId(0)),
                column: t.column,
                cost: t.cost,
                prerequisites: l.all("prerequisite", Tab::Techs, &t.prerequisites),
                uniques: uniques(&t.uniques),
            },
        );
    }
    let mut tech_columns = Vec::with_capacity(raw.tech_columns.len());
    for c in &raw.tech_columns {
        l.at("ruleset/techs.json", "columns");
        tech_columns.push(TechColumn {
            column: c.column_number,
            era: l.one("era", Tab::Eras, &c.era).unwrap_or(EraId(0)),
            tech_cost: c.tech_cost,
            building_cost: c.building_cost,
            wonder_cost: c.wonder_cost,
        });
    }

    let mut eras = IdVec::with_capacity(raw.eras.len());
    for (name, e) in &raw.eras {
        l.at("ruleset/eras.json", name);
        let unit = e.starting_military_unit.as_deref().unwrap_or("Warrior");
        push(
            &mut eras,
            EraDef {
                name: text(name),
                key: key(e.id.as_ref()),
                research_agreement_cost: e.research_agreement_cost,
                starting_settler_count: e.starting_settler_count.unwrap_or(1),
                starting_worker_count: e.starting_worker_count.unwrap_or(0),
                starting_military_unit_count: e.starting_military_unit_count.unwrap_or(1),
                starting_military_unit: l
                    .one("startingMilitaryUnit", Tab::Units, unit)
                    .unwrap_or(BaseUnitId(0)),
                starting_gold: e.starting_gold.unwrap_or(0),
                starting_culture: e.starting_culture.unwrap_or(0),
                settler_population: e.settler_population,
                settler_buildings: l.all("settlerBuildings", Tab::Buildings, &e.settler_buildings),
                starting_obsolete_wonders: l.all(
                    "startingObsoleteWonders",
                    Tab::Buildings,
                    &e.starting_obsolete_wonders,
                ),
                base_unit_buy_cost: e.base_unit_buy_cost,
                embark_defense: e.embark_defense,
                start_percent: e.start_percent,
                uniques: uniques(&e.uniques),
            },
        );
    }

    let mut buildings = IdVec::with_capacity(raw.buildings.len());
    for (name, b) in &raw.buildings {
        l.at("ruleset/buildings.json", name);
        push(
            &mut buildings,
            BuildingDef {
                name: text(name),
                key: key(b.id.as_ref()),
                cost: b.cost.unwrap_or(-1),
                stats: b.stats(),
                percent_stat_bonus: b.percent_stat_bonus.stats(),
                is_wonder: b.is_wonder,
                is_national_wonder: b.is_national_wonder,
                city_strength: b.city_strength,
                city_health: b.city_health,
                hurry_cost_modifier: b.hurry_cost_modifier,
                maintenance: b.maintenance,
                required_tech: l.opt("requiredTech", Tab::Techs, b.required_tech.as_ref()),
                required_building: l.opt(
                    "requiredBuilding",
                    Tab::Buildings,
                    b.required_building.as_ref(),
                ),
                required_resource: l.opt(
                    "requiredResource",
                    Tab::Resources,
                    b.required_resource.as_ref(),
                ),
                required_nearby_improved_resources: l.all(
                    "requiredNearbyImprovedResources",
                    Tab::Resources,
                    &b.required_nearby_improved_resources,
                ),
                replaces: l.opt("replaces", Tab::Buildings, b.replaces.as_ref()),
                unique_to: l.opt("uniqueTo", Tab::Nations, b.unique_to.as_ref()),
                great_person_points: l.keyed(
                    "greatPersonPoints",
                    Tab::Units,
                    &b.great_person_points,
                ),
                specialist_slots: l.keyed("specialistSlots", Tab::Specialists, &b.specialist_slots),
                uniques: uniques(&b.uniques),
                any_wonder: false,
                stat_related: StatMask::EMPTY,
            },
        );
    }

    let mut unit_types = IdVec::with_capacity(raw.unit_types.len());
    for (name, t) in &raw.unit_types {
        push(
            &mut unit_types,
            UnitTypeDef {
                name: text(name),
                key: key(t.id.as_ref()),
                domain: t.movement_type,
                uniques: uniques(&t.uniques),
            },
        );
    }

    let mut base_units = IdVec::with_capacity(raw.units.len());
    for (name, u) in &raw.units {
        l.at("ruleset/units.json", name);
        push(
            &mut base_units,
            BaseUnitDef {
                name: text(name),
                key: key(u.id.as_ref()),
                unit_type: l.one("unitType", Tab::UnitTypes, &u.unit_type).unwrap_or(UnitTypeId(0)),
                movement: u.movement,
                cost: u.cost.unwrap_or(0),
                hurry_cost_modifier: u.hurry_cost_modifier,
                strength: u.strength.unwrap_or(0),
                ranged_strength: u.ranged_strength.unwrap_or(0),
                range: u.range.unwrap_or(2),
                intercept_range: u.intercept_range,
                religious_strength: u.religious_strength,
                required_tech: l.opt("requiredTech", Tab::Techs, u.required_tech.as_ref()),
                obsolete_tech: l.opt("obsoleteTech", Tab::Techs, u.obsolete_tech.as_ref()),
                upgrades_to: l.opt("upgradesTo", Tab::Units, u.upgrades_to.as_ref()),
                unique_to: l.opt("uniqueTo", Tab::Nations, u.unique_to.as_ref()),
                replaces: l.opt("replaces", Tab::Units, u.replaces.as_ref()),
                required_resource: l.opt(
                    "requiredResource",
                    Tab::Resources,
                    u.required_resource.as_ref(),
                ),
                promotions: l.all("promotions", Tab::Promotions, &u.promotions),
                uniques: uniques(&u.uniques),
                domain: Domain::Land,
                ranged: false,
                melee: false,
                military: false,
                era: EraId(0),
                great_person: false,
                builder: None,
            },
        );
    }

    let mut promotions = IdVec::with_capacity(raw.promotions.len());
    for (name, pr) in &raw.promotions {
        l.at("ruleset/promotions.json", name);
        push(
            &mut promotions,
            PromotionDef {
                name: text(name),
                key: key(pr.id.as_ref()),
                unit_types: l.all("unitTypes", Tab::UnitTypes, &pr.unit_types),
                prerequisites: l.all("prerequisites", Tab::Promotions, &pr.prerequisites),
                uniques: uniques(&pr.uniques),
            },
        );
    }

    let mut terrains = IdVec::with_capacity(raw.terrains.len());
    for (name, t) in &raw.terrains {
        l.at("ruleset/terrains.json", name);
        push(
            &mut terrains,
            TerrainDef {
                name: text(name),
                key: key(t.id.as_ref()),
                kind: t.kind,
                stats: t.stats(),
                movement_cost: t.movement_cost.unwrap_or(1),
                impassable: t.impassable,
                defence_bonus: t.defence_bonus,
                override_stats: t.override_stats,
                unbuildable: t.unbuildable,
                occurs_on: l.all("occursOn", Tab::Terrains, &t.occurs_on),
                turns_into: l.opt("turnsInto", Tab::Terrains, t.turns_into.as_ref()),
                weight: t.weight,
                uniques: uniques(&t.uniques),
                rough: false,
                feature: None,
            },
        );
    }

    let mut resources = IdVec::with_capacity(raw.resources.len());
    for (name, r) in &raw.resources {
        l.at("ruleset/resources.json", name);
        push(
            &mut resources,
            ResourceDef {
                name: text(name),
                key: key(r.id.as_ref()),
                kind: r.resource_type,
                stats: r.stats(),
                terrains_can_be_found_on: l.all(
                    "terrainsCanBeFoundOn",
                    Tab::Terrains,
                    &r.terrains_can_be_found_on,
                ),
                improvement: l.opt("improvement", Tab::Improvements, r.improvement.as_ref()),
                improved_by: l.all("improvedBy", Tab::Improvements, &r.improved_by),
                improvement_stats: r.improvement_stats.stats(),
                revealed_by: l.opt("revealedBy", Tab::Techs, r.revealed_by.as_ref()),
                major_deposit_amount: r.major_deposit_amount,
                minor_deposit_amount: r.minor_deposit_amount,
                uniques: uniques(&r.uniques),
            },
        );
    }

    let mut improvements = IdVec::with_capacity(raw.improvements.len());
    for (name, i) in &raw.improvements {
        l.at("ruleset/improvements.json", name);
        push(
            &mut improvements,
            ImprovementDef {
                name: text(name),
                key: key(i.id.as_ref()),
                stats: i.stats(),
                terrains_can_be_built_on: texts(&i.terrains_can_be_built_on),
                turns_to_build: i.turns_to_build,
                tech_required: l.opt("techRequired", Tab::Techs, i.tech_required.as_ref()),
                unique_to: l.opt("uniqueTo", Tab::Nations, i.unique_to.as_ref()),
                uniques: uniques(&i.uniques),
                kind: ImprovementKind::Normal,
                great: false,
            },
        );
    }

    let mut beliefs = IdVec::with_capacity(raw.beliefs.len());
    for (name, b) in &raw.beliefs {
        push(
            &mut beliefs,
            BeliefDef {
                name: text(name),
                key: key(b.id.as_ref()),
                kind: b.kind,
                uniques: uniques(&b.uniques),
            },
        );
    }

    let religions: IdVec<_, Box<str>> = raw.religions.iter().map(|r| text(r)).collect();

    let mut specialists = IdVec::with_capacity(raw.specialists.len());
    for (name, s) in &raw.specialists {
        l.at("ruleset/specialists.json", name);
        push(
            &mut specialists,
            SpecialistDef {
                name: text(name),
                key: key(s.id.as_ref()),
                stats: s.stats(),
                great_person_points: l.keyed(
                    "greatPersonPoints",
                    Tab::Units,
                    &s.great_person_points,
                ),
            },
        );
    }

    let mut city_state_types = IdVec::with_capacity(raw.city_state_types.len());
    for (name, c) in &raw.city_state_types {
        push(
            &mut city_state_types,
            CityStateTypeDef {
                name: text(name),
                key: key(c.id.as_ref()),
                friend: uniques(&c.friend_bonus_uniques),
                ally: uniques(&c.ally_bonus_uniques),
                uniques: uniques(&c.uniques),
            },
        );
    }

    let mut difficulties = IdVec::with_capacity(raw.difficulties.len());
    for (name, d) in &raw.difficulties {
        l.at("ruleset/difficulties.json", name);
        push(
            &mut difficulties,
            DifficultyDef {
                name: text(name),
                key: key(d.id.as_ref()),
                base_happiness: d.base_happiness,
                extra_happiness_per_luxury: d.extra_happiness_per_luxury,
                research_cost_modifier: d.research_cost_modifier,
                unit_cost_modifier: d.unit_cost_modifier,
                unit_supply_base: d.unit_supply_base,
                unit_supply_per_city: d.unit_supply_per_city,
                building_cost_modifier: d.building_cost_modifier,
                policy_cost_modifier: d.policy_cost_modifier,
                unhappiness_modifier: d.unhappiness_modifier,
                barbarian_bonus: d.barbarian_bonus,
                barbarian_spawn_delay: d.barbarian_spawn_delay,
                player_bonus_starting_units: l
                    .starting_units("playerBonusStartingUnits", &d.player_bonus_starting_units),
                ai_difficulty_level: l
                    .one("aiDifficultyLevel", Tab::Difficulties, &d.ai_difficulty_level)
                    .unwrap_or(DifficultyId(0)),
                ai_city_growth_modifier: d.ai_city_growth_modifier,
                ai_unit_cost_modifier: d.ai_unit_cost_modifier,
                ai_building_cost_modifier: d.ai_building_cost_modifier,
                ai_wonder_cost_modifier: d.ai_wonder_cost_modifier,
                ai_building_maintenance_modifier: d.ai_building_maintenance_modifier,
                ai_unit_maintenance_modifier: d.ai_unit_maintenance_modifier,
                ai_unit_supply_modifier: d.ai_unit_supply_modifier,
                ai_free_techs: l.all("aiFreeTechs", Tab::Techs, &d.ai_free_techs),
                ai_major_civ_bonus_starting_units: l.starting_units(
                    "aiMajorCivBonusStartingUnits",
                    &d.ai_major_civ_bonus_starting_units,
                ),
                ai_city_state_bonus_starting_units: l.starting_units(
                    "aiCityStateBonusStartingUnits",
                    &d.ai_city_state_bonus_starting_units,
                ),
                ai_unhappiness_modifier: d.ai_unhappiness_modifier,
                ais_exchange_techs: d.ais_exchange_techs,
                turn_barbarians_can_enter_player_tiles: d.turn_barbarians_can_enter_player_tiles,
                clear_barbarian_camp_reward: d.clear_barbarian_camp_reward,
            },
        );
    }

    let mut speeds = IdVec::with_capacity(raw.speeds.len());
    for (name, s) in &raw.speeds {
        push(
            &mut speeds,
            SpeedDef {
                name: text(name),
                key: key(s.id.as_ref()),
                modifier: s.modifier,
                production_cost_modifier: s.production_cost_modifier,
                gold_cost_modifier: s.gold_cost_modifier,
                science_cost_modifier: s.science_cost_modifier,
                culture_cost_modifier: s.culture_cost_modifier,
                faith_cost_modifier: s.faith_cost_modifier,
                improvement_build_length_modifier: s.improvement_build_length_modifier,
                barbarian_modifier: s.barbarian_modifier,
                gold_gift_modifier: s.gold_gift_modifier,
                city_state_tribute_scaling_interval: s.city_state_tribute_scaling_interval,
                golden_age_length_modifier: s.golden_age_length_modifier,
                religious_pressure_adjacent_city: s.religious_pressure_adjacent_city,
                peace_deal_duration: s.peace_deal_duration,
                deal_duration: s.deal_duration,
                start_year: s.start_year,
                turns: s.turns.clone().into(),
            },
        );
    }

    let mut victories = IdVec::with_capacity(raw.victories.len());
    for (name, v) in &raw.victories {
        l.at("ruleset/victories.json", name);
        push(
            &mut victories,
            VictoryDef {
                name: text(name),
                key: key(v.id.as_ref()),
                milestones: texts(&v.milestones),
                required_spaceship_parts: l.all(
                    "requiredSpaceshipParts",
                    Tab::Units,
                    &v.required_spaceship_parts,
                ),
                hidden_in_victory_screen: v.hidden_in_victory_screen,
            },
        );
    }

    let mut quests = IdVec::with_capacity(raw.quests.len());
    for (name, q) in &raw.quests {
        l.at("ruleset/quests.json", name);
        // A weight is keyed by a personality or by a city-state type (`city_states.py:956-960`).
        let mut by_type = Vec::new();
        let mut by_personality = Vec::new();
        for (k, &w) in &q.weight_for_city_state_type {
            if let Some(pers) = CityStatePersonality::from_name(k) {
                by_personality.push((pers, w));
            } else if let Some(t) = l.one("weightForCityStateType", Tab::CityStateTypes, k) {
                by_type.push((t, w));
            }
        }
        // Python gave a quest it had no code for silently never (`city_states.py:909`).
        let kind = QuestKind::from_name(name).unwrap_or_else(|| {
            let known: Vec<&str> = QuestKind::ALL.iter().map(|k| k.name()).collect();
            l.p.push(
                RulesetErrorKind::Invalid,
                "ruleset/quests.json",
                name,
                format!("the engine has no quest of this name; it knows {}", known.join(", ")),
            );
            QuestKind::Route
        });
        push(
            &mut quests,
            QuestDef {
                name: text(name),
                key: key(q.id.as_ref()),
                kind,
                target: kind.target(),
                scope: q.scope.unwrap_or_default(),
                influence: q.influence,
                duration: q.duration,
                minimum_civs: q.minimum_civs,
                weight_by_type: by_type.into(),
                weight_by_personality: by_personality.into(),
                params: q.params.clone().into(),
            },
        );
    }

    let mut ruins = IdVec::with_capacity(raw.ruins.len());
    for (name, r) in &raw.ruins {
        l.at("ruleset/ruins.json", name);
        push(
            &mut ruins,
            RuinDef {
                name: text(name),
                key: key(r.id.as_ref()),
                excluded_difficulties: l.all(
                    "excludedDifficulties",
                    Tab::Difficulties,
                    &r.excluded_difficulties,
                ),
                uniques: uniques(&r.uniques),
            },
        );
    }

    let mut personalities = IdVec::with_capacity(raw.personalities.len());
    for (name, x) in &raw.personalities {
        l.at("ruleset/personalities.json", name);
        push(
            &mut personalities,
            PersonalityDef {
                name: text(name),
                key: key(x.id.as_ref()),
                production: x.production,
                food: x.food,
                gold: x.gold,
                science: x.science,
                culture: x.culture,
                happiness: x.happiness,
                faith: x.faith,
                military: x.military,
                aggressive: x.aggressive,
                declare_war: x.declare_war,
                commerce: x.commerce,
                diplomacy: x.diplomacy,
                loyal: x.loyal,
                expansion: x.expansion,
                denounce_willingness: x.denounce_willingness,
                priorities: l.keyed("priorities", Tab::Branches, &x.priorities),
            },
        );
    }

    let mut policies = IdVec::with_capacity(raw.policy_branches.len() + raw.policies.len());
    for (name, b) in &raw.policy_branches {
        l.at("ruleset/policies.json", name);
        push(
            &mut policies,
            PolicyDef {
                name: text(name),
                key: key(b.id.as_ref()),
                uniques: uniques(&b.uniques),
                kind: PolicyKind::Branch {
                    era: l.one("era", Tab::Eras, &b.era).unwrap_or(EraId(0)),
                    priorities: b.priorities.iter().map(|(&k, &v)| (k, v)).collect(),
                    members: l.all("members", Tab::Members, &b.members),
                },
            },
        );
    }
    for (name, x) in &raw.policies {
        l.at("ruleset/policies.json", name);
        push(
            &mut policies,
            PolicyDef {
                name: text(name),
                key: key(x.id.as_ref()),
                uniques: uniques(&x.uniques),
                kind: PolicyKind::Member {
                    branch: l.one("branch", Tab::Branches, &x.branch).unwrap_or(PolicyId(0)),
                    requires: l.all("requires", Tab::Policies, &x.requires),
                    finisher: x.is_finisher,
                },
            },
        );
    }

    let mut nations = IdVec::with_capacity(raw.nations.len());
    for (name, n) in &raw.nations {
        l.at(raw.nation_file(name), name);
        push(
            &mut nations,
            NationDef {
                name: text(name),
                key: key(n.id.as_ref()),
                kind: n.kind,
                leader_name: n.leader_name.as_deref().map(text),
                adjective: n.adjective.as_deref().map(text),
                start_bias: n.start_bias.iter().map(|b| StartBias::parse(b)).collect(),
                preferred_victory_type: n.preferred_victory_type,
                personality: l.opt("personality", Tab::Personalities, n.personality.as_ref()),
                favored_religion: l.opt(
                    "favoredReligion",
                    Tab::Religions,
                    n.favored_religion.as_ref(),
                ),
                city_state_type: l.opt(
                    "cityStateType",
                    Tab::CityStateTypes,
                    n.city_state_type.as_ref(),
                ),
                cities: texts(&n.cities),
                benchmark: n.benchmark,
                uniques: uniques(&n.uniques),
            },
        );
    }

    let constants = link_game(&mut l, game);

    Some(Ruleset {
        id: super::RulesetId([0; 32]),
        techs,
        tech_columns,
        eras,
        base_units,
        unit_types,
        buildings,
        promotions,
        terrains,
        resources,
        improvements,
        beliefs,
        religions,
        specialists,
        city_state_types,
        difficulties,
        speeds,
        victories,
        quests,
        ruins,
        personalities,
        policies,
        // check_sizes held branches and policies to 128.
        policy_branch_count: u16::try_from(raw.policy_branches.len()).unwrap_or(u16::MAX),
        nations,
        global_uniques: uniques(&raw.global_uniques),
        constants,
        fracs: IdVec::new(),
        derived: Derived::empty(),
        names: Default::default(),
        client: client::ClientSource::default(),
        client_json: std::sync::OnceLock::new(),
    })
}

/// Appends to a table whose size `check_sizes` already held to its id type, so there is room.
fn push<I: Id, T>(table: &mut IdVec<I, T>, def: T) {
    let pushed = table.push(def).is_ok();
    debug_assert!(pushed, "check_sizes let a table outgrow its id type");
}

/// `game.json`, with its speed and difficulty names resolved.
fn link_game(l: &mut Linker<'_>, g: &RawGame) -> Constants {
    l.at(GAME, "");
    Constants {
        move_scale: g.move_scale,
        map_sizes: g
            .map_sizes
            .iter()
            .map(|(k, m)| MapSize {
                key: text(k),
                name: text(&m.name),
                width: m.width,
                height: m.height,
                players: m.players,
                city_states: m.city_states,
            })
            .collect(),
        map_size_predefined: g.map_size_predefined.clone(),
        map_types: g
            .map_types
            .iter()
            .map(|(k, t)| MapType { key: text(k), name: text(&t.name) })
            .collect(),
        default_speed: l.one("default_speed", Tab::Speeds, &g.default_speed).unwrap_or(SpeedId(0)),
        benchmark_speed: l
            .one("benchmark_speed", Tab::Speeds, &g.benchmark_speed)
            .unwrap_or(SpeedId(0)),
        default_difficulty: l
            .one("default_difficulty", Tab::Difficulties, &g.default_difficulty)
            .unwrap_or(DifficultyId(0)),
        max_players: g.max_players,
        formulas: g.constants.clone(),
        barbarian_levels: g
            .barbarians
            .levels
            .iter()
            .map(|(k, v)| BarbarianLevel { key: text(k), level: v.clone() })
            .collect(),
        diplomacy: g.diplomacy.clone(),
    }
}

// ---- Stage 5: the rules the data must follow --------------------------------------------------

fn check_rules(raw: &RawRuleset, r: &Ruleset, p: &mut Problems) {
    // An object's name is its key: uniques and saves name objects by name, tables key them.
    fn names_match<T>(
        p: &mut Problems,
        file_of: impl Fn(&str) -> &'static str,
        table: &Table<T>,
        name: impl Fn(&T) -> &str,
    ) {
        for (k, v) in table {
            if name(v) != k {
                p.push(
                    RulesetErrorKind::Name,
                    file_of(k),
                    k,
                    format!("its name {:?} differs from its key", name(v)),
                );
            }
        }
    }
    let file = |f: &'static str| move |_: &str| f;
    names_match(p, file("ruleset/techs.json"), &raw.techs, |x| &x.name);
    names_match(p, file("ruleset/eras.json"), &raw.eras, |x| &x.name);
    names_match(p, file("ruleset/buildings.json"), &raw.buildings, |x| &x.name);
    names_match(p, file("ruleset/units.json"), &raw.units, |x| &x.name);
    names_match(p, file("ruleset/unit_types.json"), &raw.unit_types, |x| &x.name);
    names_match(p, file("ruleset/promotions.json"), &raw.promotions, |x| &x.name);
    names_match(p, file("ruleset/terrains.json"), &raw.terrains, |x| &x.name);
    names_match(p, file("ruleset/resources.json"), &raw.resources, |x| &x.name);
    names_match(p, file("ruleset/improvements.json"), &raw.improvements, |x| &x.name);
    names_match(p, file("ruleset/beliefs.json"), &raw.beliefs, |x| &x.name);
    names_match(p, file("ruleset/specialists.json"), &raw.specialists, |x| &x.name);
    names_match(p, file("ruleset/city_state_types.json"), &raw.city_state_types, |x| &x.name);
    names_match(p, file("ruleset/difficulties.json"), &raw.difficulties, |x| &x.name);
    names_match(p, file("ruleset/speeds.json"), &raw.speeds, |x| &x.name);
    names_match(p, file("ruleset/victories.json"), &raw.victories, |x| &x.name);
    names_match(p, file("ruleset/quests.json"), &raw.quests, |x| &x.name);
    names_match(p, file("ruleset/ruins.json"), &raw.ruins, |x| &x.name);
    names_match(p, file("ruleset/personalities.json"), &raw.personalities, |x| &x.name);
    names_match(p, file("ruleset/policies.json"), &raw.policy_branches, |x| &x.name);
    names_match(p, file("ruleset/policies.json"), &raw.policies, |x| &x.name);
    names_match(p, |k| raw.nation_file(k), &raw.nations, |x| &x.name);

    // Branches and policies share an id space and a name space.
    for name in raw.policies.keys() {
        if raw.policy_branches.contains_key(name) {
            p.push(
                RulesetErrorKind::Name,
                "ruleset/policies.json",
                name,
                "a policy has the name of a branch",
            );
        }
    }
    for (i, name) in raw.religions.iter().enumerate() {
        if raw.religions[..i].contains(name) {
            p.push(RulesetErrorKind::Name, "ruleset/religions.json", name, "named twice");
        }
    }
    check_branches(r, p);

    // Rules read an era's number as its index (`rules.py:108, 111`), so the id is the number.
    for (i, (name, e)) in raw.eras.iter().enumerate() {
        if usize::try_from(e.number).ok() != Some(i) {
            p.push(
                RulesetErrorKind::Invalid,
                "ruleset/eras.json",
                name,
                format!(
                    "its number is {}, but it is era {i} in file order; list the eras by number \
                     from 0",
                    e.number
                ),
            );
        }
    }

    for s in r.speeds.as_slice() {
        let bad = if s.turns.is_empty() {
            Some("the calendar `turns` is empty".to_owned())
        } else {
            s.turns
                .iter()
                .zip(s.turns.iter().skip(1))
                .find(|(a, b)| b.until_turn <= a.until_turn)
                .map(|(a, b)| {
                    format!(
                        "untilTurn {} follows {}; the calendar must rise",
                        b.until_turn, a.until_turn
                    )
                })
        };
        let bad = bad.or_else(|| {
            s.turns.iter().find(|t| t.until_turn < 1 || t.years_per_turn <= 0.0).map(|t| {
                format!(
                    "a calendar entry of {} years until turn {} is not positive",
                    t.years_per_turn, t.until_turn
                )
            })
        });
        if let Some(text) = bad {
            p.push(RulesetErrorKind::Invalid, "ruleset/speeds.json", &s.name, text);
        }
    }

    if raw.game.is_some() {
        check_game(&r.constants, p);
    }
}

/// A branch and its policies agree: a branch's members are exactly its policies that are not
/// its finisher. Adoption reads a policy's branch, and completion the branch's members
/// (`policies.py:32, 136-138`), so where they disagreed a branch could never complete or a
/// policy would count towards another branch.
fn check_branches(r: &Ruleset, p: &mut Problems) {
    let file = "ruleset/policies.json";
    for (id, def) in r.policies.iter() {
        match &def.kind {
            PolicyKind::Branch { members, .. } => {
                for (i, &m) in members.iter().enumerate() {
                    let member = &r.policies[m];
                    let why = match member.kind {
                        _ if members[..i].contains(&m) => Some("is listed twice"),
                        PolicyKind::Member { finisher: true, .. } => {
                            Some("is the branch's finisher, which is adopted once the members are")
                        }
                        PolicyKind::Member { branch, .. } if branch != id => {
                            Some("belongs to another branch")
                        }
                        _ => None,
                    };
                    if let Some(why) = why {
                        p.push(
                            RulesetErrorKind::Invalid,
                            file,
                            &def.name,
                            format!("its member {:?} {why}", member.name),
                        );
                    }
                }
            }
            PolicyKind::Member { branch, finisher: false, .. } => {
                let listed = match &r.policies[*branch].kind {
                    PolicyKind::Branch { members, .. } => members.contains(&id),
                    // Never: `branch` was resolved among the branches alone.
                    PolicyKind::Member { .. } => true,
                };
                if !listed {
                    p.push(
                        RulesetErrorKind::Invalid,
                        file,
                        &def.name,
                        format!(
                            "its branch {:?} does not list it among its members",
                            r.policies[*branch].name
                        ),
                    );
                }
            }
            PolicyKind::Member { finisher: true, .. } => {}
        }
    }
}

/// The constants later code divides by, loops over, counts seats with or searches in order.
fn check_game(k: &Constants, p: &mut Problems) {
    let mut invalid = |object: &str, text: String| {
        p.push(RulesetErrorKind::Invalid, GAME, object, text);
    };
    if k.map_size_predefined.is_empty() {
        invalid("map_size_predefined", "the list is empty".to_owned());
    }
    // `map_size_predefined()` keeps the last size that fits, which is the largest only in order.
    for pair in k.map_size_predefined.windows(2) {
        if pair[1].radius <= pair[0].radius {
            invalid(
                "map_size_predefined",
                format!(
                    "{} (radius {}) follows {} (radius {}); list the sizes smallest first",
                    pair[1].name, pair[1].radius, pair[0].name, pair[0].radius
                ),
            );
        }
    }
    if k.max_players == 0 {
        invalid("max_players", "a game needs a player".to_owned());
    }
    // Moves are counted in parts of a tile (`automation.py:68`, `movement.py:356`).
    if k.move_scale < 1 {
        invalid("move_scale", format!("{} parts to a tile; it must be at least 1", k.move_scale));
    }
    let f = &k.formulas;
    // An upgrade's cost is rounded down to a multiple of it (`units.py:515`).
    if f.unit_upgrade_cost.round_to < 1 {
        invalid(
            "constants",
            format!(
                "unit_upgrade_cost.round_to is {}; it must be at least 1",
                f.unit_upgrade_cost.round_to
            ),
        );
    }
    // Counts, distances and numbers of turns.
    let counts = [
        ("max_xp_from_barbarians", f.max_xp_from_barbarians),
        ("minimal_city_distance", f.minimal_city_distance),
        ("minimal_city_distance_other_continents", f.minimal_city_distance_other_continents),
        ("base_city_bombard_range", f.base_city_bombard_range),
        ("city_work_range", f.city_work_range),
        ("city_expand_range", f.city_expand_range),
        ("city_air_unit_capacity", f.city_air_unit_capacity),
        ("religion_limit_base", f.religion_limit_base),
        ("pantheon_base", f.pantheon_base),
        ("pantheon_growth", f.pantheon_growth),
        ("minimum_war_duration", f.minimum_war_duration),
        ("base_turns_until_revolt", f.base_turns_until_revolt),
        ("city_state_election_turns", f.city_state_election_turns),
        ("max_gold_trade_offer", f.max_gold_trade_offer),
        ("max_spy_rank", f.max_spy_rank),
    ];
    for (name, value) in counts {
        if value < 0 {
            invalid(
                "constants",
                format!(
                    "{name} is {value}; a count, a distance or a number of turns is never negative"
                ),
            );
        }
    }
    for m in &k.map_sizes {
        if !(MIN_SIDE..=MAX_SIDE).contains(&m.width) || !(MIN_SIDE..=MAX_SIDE).contains(&m.height) {
            invalid(
                &m.key,
                format!(
                    "{}x{} is outside the map sides of {MIN_SIDE} to {MAX_SIDE}",
                    m.width, m.height
                ),
            );
        }
    }
    // Majors, city-states and the barbarians each take a seat.
    let seats = |majors: u8, city_states: u8| usize::from(majors) + usize::from(city_states) + 1;
    let too_many = |what: &str, seats: usize| {
        format!("{what} {seats} seats, more than a PlayerSet holds ({})", PlayerSet::CAPACITY)
    };
    for m in &k.map_sizes {
        let n = seats(m.players, m.city_states);
        if n > PlayerSet::CAPACITY {
            p.push(RulesetErrorKind::Capacity, GAME, &m.key, too_many("its players take", n));
        }
    }
    let n = seats(k.max_players, 0);
    if n > PlayerSet::CAPACITY {
        p.push(
            RulesetErrorKind::Capacity,
            GAME,
            "max_players",
            too_many("the most major civilizations and the barbarians take", n),
        );
    }
}

// ---- The name indexes -------------------------------------------------------------------------

fn index_of<T>(table: &Table<T>, id: impl Fn(&T) -> Option<&str>) -> NameIndex {
    NameIndex::new(table.iter().map(|(k, v)| (k.as_str(), id(v))))
}

fn name_indexes(raw: &RawRuleset) -> [NameIndex; 16] {
    NameKind::ALL.map(|kind| match kind {
        NameKind::Tech => index_of(&raw.techs, |x| x.id.as_deref()),
        NameKind::Unit => index_of(&raw.units, |x| x.id.as_deref()),
        NameKind::Building => index_of(&raw.buildings, |x| x.id.as_deref()),
        NameKind::Promotion => index_of(&raw.promotions, |x| x.id.as_deref()),
        NameKind::Terrain => index_of(&raw.terrains, |x| x.id.as_deref()),
        NameKind::Resource => index_of(&raw.resources, |x| x.id.as_deref()),
        NameKind::Improvement => index_of(&raw.improvements, |x| x.id.as_deref()),
        NameKind::Belief => index_of(&raw.beliefs, |x| x.id.as_deref()),
        NameKind::Policy => NameIndex::new(
            raw.policy_branches
                .iter()
                .map(|(k, v)| (k.as_str(), v.id.as_deref()))
                .chain(raw.policies.iter().map(|(k, v)| (k.as_str(), v.id.as_deref()))),
        ),
        NameKind::Nation => index_of(&raw.nations, |x| x.id.as_deref()),
        NameKind::Era => index_of(&raw.eras, |x| x.id.as_deref()),
        NameKind::Specialist => index_of(&raw.specialists, |x| x.id.as_deref()),
        NameKind::Speed => index_of(&raw.speeds, |x| x.id.as_deref()),
        NameKind::Difficulty => index_of(&raw.difficulties, |x| x.id.as_deref()),
        NameKind::UnitType => index_of(&raw.unit_types, |x| x.id.as_deref()),
        NameKind::Victory => index_of(&raw.victories, |x| x.id.as_deref()),
    })
}

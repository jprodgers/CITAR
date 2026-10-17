//! The raw layer: the ruleset files as written, before any name is resolved (DESIGN.md 5.3).
//!
//! Each row of each table is read into a serde struct that denies unknown fields, so a misspelt
//! field (`requiredTeck`) is a load error instead of a value Python silently ignored. Tables keep
//! the files' order, which becomes id order. Keys that start with `_` are comments and are
//! skipped, as Python skipped them in `custom/nations.json` (`rules.py:78-81`), and the custom
//! nations are merged into the nations as Python merged them: a new key is appended, a known key
//! replaced in place, and a missing `id` made from the key (`rules.py:76-81`).
//!
//! Fields Python gave a default at load (`rules.py:119-122, 132-133, 137, 148`) are optional
//! here and get the same default in `defs`.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::constants::RawGame;
use super::defs::{
    BeliefType, Deposit, Domain, NationKind, QuestScope, ResourceType, SpeedTurns, TerrainType,
    VictoryFocus,
};
use super::errors::{Problems, RulesetErrorKind};
use super::source::{CUSTOM_NATIONS, Docs, GAME};
use crate::base::collections::DetMap;
use crate::base::stats::{Stat, Stats};

/// A table by name, in file order.
pub(crate) type Table<T> = DetMap<String, T>;

/// Every file, read.
pub(crate) struct RawRuleset {
    pub tech_columns: Vec<RawTechColumn>,
    pub techs: Table<RawTech>,
    pub eras: Table<RawEra>,
    pub buildings: Table<RawBuilding>,
    pub units: Table<RawUnit>,
    pub unit_types: Table<RawUnitType>,
    pub promotions: Table<RawPromotion>,
    pub terrains: Table<RawTerrain>,
    pub resources: Table<RawResource>,
    pub improvements: Table<RawImprovement>,
    pub beliefs: Table<RawBelief>,
    pub religions: Vec<String>,
    pub specialists: Table<RawSpecialist>,
    pub city_state_types: Table<RawCityStateType>,
    pub difficulties: Table<RawDifficulty>,
    pub speeds: Table<RawSpeed>,
    pub victories: Table<RawVictory>,
    pub quests: Table<RawQuest>,
    pub ruins: Table<RawRuin>,
    pub personalities: Table<RawPersonality>,
    pub policy_branches: Table<RawPolicyBranch>,
    pub policies: Table<RawPolicy>,
    pub nations: Table<RawNation>,
    /// The nations with the custom ones merged in, as JSON, for the client.
    pub nations_json: Map<String, Value>,
    /// The nations `custom/nations.json` added or replaced.
    pub custom_nations: Vec<String>,
    pub global_uniques: Vec<String>,
    pub game: Option<RawGame>,
}

/// Reads every parsed file into its raw structs, reporting every row that does not fit.
pub(crate) fn read(docs: &Docs, p: &mut Problems) -> RawRuleset {
    let mut r = Reader { docs, p };
    let techs_file: Option<RawTechsFile> = r.whole("ruleset/techs.json");
    let (tech_columns, techs) = match techs_file {
        Some(f) => (f.columns, r.rows("ruleset/techs.json", "techs", &f.techs)),
        None => (Vec::new(), Table::default()),
    };
    let policies_file: Option<RawPoliciesFile> = r.whole("ruleset/policies.json");
    let (policy_branches, policies) = match policies_file {
        Some(f) => (
            r.rows("ruleset/policies.json", "branches", &f.branches),
            r.rows("ruleset/policies.json", "policies", &f.policies),
        ),
        None => (Table::default(), Table::default()),
    };
    let (nations_json, custom_keys) = merged_nations(docs, r.p);
    let nations = r.nations(&nations_json, &custom_keys);
    let global: Option<RawGlobalUniques> = r.whole("ruleset/global_uniques.json");
    RawRuleset {
        tech_columns,
        techs,
        eras: r.table("ruleset/eras.json"),
        buildings: r.table("ruleset/buildings.json"),
        units: r.table("ruleset/units.json"),
        unit_types: r.table("ruleset/unit_types.json"),
        promotions: r.table("ruleset/promotions.json"),
        terrains: r.table("ruleset/terrains.json"),
        resources: r.table("ruleset/resources.json"),
        improvements: r.table("ruleset/improvements.json"),
        beliefs: r.table("ruleset/beliefs.json"),
        religions: r.whole("ruleset/religions.json").unwrap_or_default(),
        specialists: r.table("ruleset/specialists.json"),
        city_state_types: r.table("ruleset/city_state_types.json"),
        difficulties: r.table("ruleset/difficulties.json"),
        speeds: r.table("ruleset/speeds.json"),
        victories: r.table("ruleset/victories.json"),
        quests: r.table("ruleset/quests.json"),
        ruins: r.table("ruleset/ruins.json"),
        personalities: r.table("ruleset/personalities.json"),
        policy_branches,
        policies,
        nations,
        nations_json,
        custom_nations: custom_keys,
        global_uniques: global.map(|g| g.uniques).unwrap_or_default(),
        game: r.whole(GAME),
    }
}

struct Reader<'a> {
    docs: &'a Docs,
    p: &'a mut Problems,
}

impl Reader<'_> {
    /// A whole file as one struct.
    fn whole<T: DeserializeOwned>(&mut self, file: &str) -> Option<T> {
        let v = self.docs.get(file)?;
        match T::deserialize(v) {
            Ok(t) => Some(t),
            Err(e) => {
                self.p.push(RulesetErrorKind::Schema, file, "", e.to_string());
                None
            }
        }
    }

    /// A file that is one table.
    fn table<T: DeserializeOwned>(&mut self, file: &str) -> Table<T> {
        match self.docs.get(file) {
            Some(v) => self.rows(file, "", v),
            None => Table::default(),
        }
    }

    /// The rows of a table: an object of objects by name. `part` names the table inside a file
    /// that holds more than one.
    fn rows<T: DeserializeOwned>(&mut self, file: &str, part: &str, v: &Value) -> Table<T> {
        let Some(obj) = v.as_object() else {
            let what = if part.is_empty() { "the file" } else { part };
            self.p.push(
                RulesetErrorKind::Schema,
                file,
                part,
                format!("{what} should be an object of rows by name"),
            );
            return Table::default();
        };
        self.rows_in(obj, |_| file)
    }

    /// The rows of an object of rows, each reported against the file `file_of` its key names.
    fn rows_in<'f, T: DeserializeOwned>(
        &mut self,
        obj: &Map<String, Value>,
        file_of: impl Fn(&str) -> &'f str,
    ) -> Table<T> {
        let mut out = Table::default();
        for (key, row) in obj {
            if key.starts_with('_') {
                continue;
            }
            match T::deserialize(row) {
                Ok(t) => {
                    out.insert(key.clone(), t);
                }
                Err(e) => self.p.push(RulesetErrorKind::Schema, file_of(key), key, e.to_string()),
            }
        }
        out
    }

    /// The merged nations, each problem reported against the file its row came from.
    fn nations(&mut self, merged: &Map<String, Value>, custom: &[String]) -> Table<RawNation> {
        self.rows_in(merged, |key| nation_file(custom, key))
    }
}

impl RawRuleset {
    /// The file the nation `key` came from, for reports about it.
    pub fn nation_file(&self, key: &str) -> &'static str {
        nation_file(&self.custom_nations, key)
    }
}

/// The file the nation `key` came from, given the keys `custom/nations.json` added or replaced.
fn nation_file(custom: &[String], key: &str) -> &'static str {
    if custom.iter().any(|c| c == key) { CUSTOM_NATIONS } else { "ruleset/nations.json" }
}

/// `ruleset/nations.json` with `custom/nations.json` merged in, as Python merged it
/// (`rules.py:75-81`), and the keys that came from the custom file.
fn merged_nations(docs: &Docs, p: &mut Problems) -> (Map<String, Value>, Vec<String>) {
    let file = "ruleset/nations.json";
    let mut out: Map<String, Value> = match docs.get(file).map(Value::as_object) {
        Some(Some(m)) => m
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        Some(None) => {
            p.push(
                RulesetErrorKind::Schema,
                file,
                "",
                "the file should be an object of rows by name",
            );
            Map::new()
        }
        None => Map::new(),
    };
    let mut custom_keys = Vec::new();
    match docs.get(CUSTOM_NATIONS).map(Value::as_object) {
        Some(Some(custom)) => {
            for (k, v) in custom {
                if k.starts_with('_') {
                    continue;
                }
                let mut v = v.clone();
                if let Some(obj) = v.as_object_mut()
                    && !obj.contains_key("id")
                {
                    obj.insert("id".to_owned(), Value::String(snake_id(k)));
                }
                // A known key keeps its place, as a Python dict assignment does.
                out.insert(k.clone(), v);
                custom_keys.push(k.clone());
            }
        }
        Some(None) => p.push(
            RulesetErrorKind::Schema,
            CUSTOM_NATIONS,
            "",
            "the file should be an object of rows by name",
        ),
        None => {}
    }
    (out, custom_keys)
}

/// `re.sub(r"[^0-9a-z]+", "_", k.lower()).strip("_")` (`rules.py:80`): the id Python gave a
/// custom nation that had none.
fn snake_id(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut gap = false;
    for c in key.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            gap = false;
        } else if !gap {
            out.push('_');
            gap = true;
        }
    }
    out.trim_matches('_').to_owned()
}

// ---- Files that are not one table -------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTechsFile {
    columns: Vec<RawTechColumn>,
    techs: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPoliciesFile {
    branches: Value,
    policies: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGlobalUniques {
    uniques: Vec<String>,
}

// ---- Rows -------------------------------------------------------------------------------------

/// The seven yields, as a table row writes them: absent means zero.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawYields {
    #[serde(default)]
    pub food: f64,
    #[serde(default)]
    pub production: f64,
    #[serde(default)]
    pub gold: f64,
    #[serde(default)]
    pub science: f64,
    #[serde(default)]
    pub culture: f64,
    #[serde(default)]
    pub happiness: f64,
    #[serde(default)]
    pub faith: f64,
}

impl RawYields {
    pub fn stats(&self) -> Stats {
        let mut s = Stats::ZERO;
        s[Stat::Food] = self.food;
        s[Stat::Production] = self.production;
        s[Stat::Gold] = self.gold;
        s[Stat::Science] = self.science;
        s[Stat::Culture] = self.culture;
        s[Stat::Happiness] = self.happiness;
        s[Stat::Faith] = self.faith;
        s
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawTechColumn {
    pub column_number: u16,
    pub era: String,
    pub tech_cost: i32,
    pub building_cost: i32,
    pub wonder_cost: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawTech {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub era: String,
    pub column: u16,
    pub cost: i32,
    #[serde(default)]
    pub prerequisites: Vec<String>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawEra {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub number: i32,
    pub research_agreement_cost: i32,
    #[serde(default)]
    pub starting_settler_count: Option<i32>,
    #[serde(default)]
    pub starting_worker_count: Option<i32>,
    #[serde(default)]
    pub starting_military_unit_count: Option<i32>,
    #[serde(default)]
    pub starting_military_unit: Option<String>,
    #[serde(default)]
    pub starting_gold: Option<i32>,
    #[serde(default)]
    pub starting_culture: Option<i32>,
    pub settler_population: i32,
    #[serde(default)]
    pub settler_buildings: Vec<String>,
    #[serde(default)]
    pub starting_obsolete_wonders: Vec<String>,
    pub base_unit_buy_cost: i32,
    pub embark_defense: i32,
    pub start_percent: i32,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawBuilding {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub cost: Option<i32>,
    #[serde(default)]
    pub food: f64,
    #[serde(default)]
    pub production: f64,
    #[serde(default)]
    pub gold: f64,
    #[serde(default)]
    pub science: f64,
    #[serde(default)]
    pub culture: f64,
    #[serde(default)]
    pub happiness: f64,
    #[serde(default)]
    pub faith: f64,
    #[serde(default)]
    pub is_wonder: bool,
    #[serde(default)]
    pub is_national_wonder: bool,
    #[serde(default)]
    pub city_strength: f64,
    #[serde(default)]
    pub city_health: i32,
    #[serde(default)]
    pub hurry_cost_modifier: Option<i32>,
    #[serde(default)]
    pub maintenance: i32,
    #[serde(default)]
    pub required_tech: Option<String>,
    #[serde(default)]
    pub required_building: Option<String>,
    #[serde(default)]
    pub required_resource: Option<String>,
    #[serde(default)]
    pub required_nearby_improved_resources: Vec<String>,
    #[serde(default)]
    pub replaces: Option<String>,
    #[serde(default)]
    pub unique_to: Option<String>,
    #[serde(default)]
    pub great_person_points: DetMap<String, i32>,
    #[serde(default)]
    pub percent_stat_bonus: RawYields,
    #[serde(default)]
    pub specialist_slots: DetMap<String, i32>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

impl RawBuilding {
    pub fn stats(&self) -> Stats {
        RawYields {
            food: self.food,
            production: self.production,
            gold: self.gold,
            science: self.science,
            culture: self.culture,
            happiness: self.happiness,
            faith: self.faith,
        }
        .stats()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawUnit {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub unit_type: String,
    pub movement: i32,
    #[serde(default)]
    pub cost: Option<i32>,
    #[serde(default)]
    pub hurry_cost_modifier: Option<i32>,
    #[serde(default)]
    pub strength: Option<i32>,
    #[serde(default)]
    pub ranged_strength: Option<i32>,
    #[serde(default)]
    pub range: Option<i32>,
    #[serde(default)]
    pub intercept_range: i32,
    #[serde(default)]
    pub religious_strength: i32,
    #[serde(default)]
    pub required_tech: Option<String>,
    #[serde(default)]
    pub obsolete_tech: Option<String>,
    #[serde(default)]
    pub upgrades_to: Option<String>,
    #[serde(default)]
    pub unique_to: Option<String>,
    #[serde(default)]
    pub replaces: Option<String>,
    #[serde(default)]
    pub required_resource: Option<String>,
    #[serde(default)]
    pub promotions: Vec<String>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawUnitType {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub movement_type: Domain,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawPromotion {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub unit_types: Vec<String>,
    #[serde(default)]
    pub prerequisites: Vec<String>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawTerrain {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: TerrainType,
    #[serde(default)]
    pub food: f64,
    #[serde(default)]
    pub production: f64,
    #[serde(default)]
    pub gold: f64,
    #[serde(default)]
    pub science: f64,
    #[serde(default)]
    pub culture: f64,
    #[serde(default)]
    pub happiness: f64,
    #[serde(default)]
    pub faith: f64,
    #[serde(default)]
    pub movement_cost: Option<i32>,
    #[serde(default)]
    pub impassable: bool,
    #[serde(default)]
    pub defence_bonus: f64,
    #[serde(default)]
    pub override_stats: bool,
    #[serde(default)]
    pub unbuildable: bool,
    #[serde(default)]
    pub occurs_on: Vec<String>,
    #[serde(default)]
    pub turns_into: Option<String>,
    #[serde(default)]
    pub weight: Option<i32>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

impl RawTerrain {
    pub fn stats(&self) -> Stats {
        RawYields {
            food: self.food,
            production: self.production,
            gold: self.gold,
            science: self.science,
            culture: self.culture,
            happiness: self.happiness,
            faith: self.faith,
        }
        .stats()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawResource {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub resource_type: ResourceType,
    #[serde(default)]
    pub food: f64,
    #[serde(default)]
    pub production: f64,
    #[serde(default)]
    pub gold: f64,
    #[serde(default)]
    pub science: f64,
    #[serde(default)]
    pub culture: f64,
    #[serde(default)]
    pub happiness: f64,
    #[serde(default)]
    pub faith: f64,
    #[serde(default)]
    pub terrains_can_be_found_on: Vec<String>,
    #[serde(default)]
    pub improvement: Option<String>,
    #[serde(default)]
    pub improved_by: Vec<String>,
    #[serde(default)]
    pub improvement_stats: RawYields,
    #[serde(default)]
    pub revealed_by: Option<String>,
    #[serde(default)]
    pub major_deposit_amount: Option<Deposit>,
    #[serde(default)]
    pub minor_deposit_amount: Option<Deposit>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

impl RawResource {
    pub fn stats(&self) -> Stats {
        RawYields {
            food: self.food,
            production: self.production,
            gold: self.gold,
            science: self.science,
            culture: self.culture,
            happiness: self.happiness,
            faith: self.faith,
        }
        .stats()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawImprovement {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub food: f64,
    #[serde(default)]
    pub production: f64,
    #[serde(default)]
    pub gold: f64,
    #[serde(default)]
    pub science: f64,
    #[serde(default)]
    pub culture: f64,
    #[serde(default)]
    pub happiness: f64,
    #[serde(default)]
    pub faith: f64,
    #[serde(default)]
    pub terrains_can_be_built_on: Vec<String>,
    #[serde(default)]
    pub turns_to_build: Option<i32>,
    #[serde(default)]
    pub tech_required: Option<String>,
    #[serde(default)]
    pub unique_to: Option<String>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

impl RawImprovement {
    pub fn stats(&self) -> Stats {
        RawYields {
            food: self.food,
            production: self.production,
            gold: self.gold,
            science: self.science,
            culture: self.culture,
            happiness: self.happiness,
            faith: self.faith,
        }
        .stats()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawBelief {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: BeliefType,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawSpecialist {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub food: f64,
    #[serde(default)]
    pub production: f64,
    #[serde(default)]
    pub gold: f64,
    #[serde(default)]
    pub science: f64,
    #[serde(default)]
    pub culture: f64,
    #[serde(default)]
    pub happiness: f64,
    #[serde(default)]
    pub faith: f64,
    #[serde(default)]
    pub great_person_points: DetMap<String, i32>,
}

impl RawSpecialist {
    pub fn stats(&self) -> Stats {
        RawYields {
            food: self.food,
            production: self.production,
            gold: self.gold,
            science: self.science,
            culture: self.culture,
            happiness: self.happiness,
            faith: self.faith,
        }
        .stats()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawCityStateType {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub friend_bonus_uniques: Vec<String>,
    #[serde(default)]
    pub ally_bonus_uniques: Vec<String>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawDifficulty {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
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
    #[serde(default)]
    pub barbarian_spawn_delay: i32,
    #[serde(default)]
    pub player_bonus_starting_units: Vec<String>,
    pub ai_difficulty_level: String,
    pub ai_city_growth_modifier: f64,
    pub ai_unit_cost_modifier: f64,
    pub ai_building_cost_modifier: f64,
    pub ai_wonder_cost_modifier: f64,
    pub ai_building_maintenance_modifier: f64,
    pub ai_unit_maintenance_modifier: f64,
    pub ai_unit_supply_modifier: f64,
    #[serde(default)]
    pub ai_free_techs: Vec<String>,
    #[serde(default)]
    pub ai_major_civ_bonus_starting_units: Vec<String>,
    #[serde(default)]
    pub ai_city_state_bonus_starting_units: Vec<String>,
    pub ai_unhappiness_modifier: f64,
    #[serde(default)]
    pub ais_exchange_techs: bool,
    #[serde(default)]
    pub turn_barbarians_can_enter_player_tiles: i32,
    #[serde(default = "default_camp_reward")]
    pub clear_barbarian_camp_reward: i32,
}

/// Python's default for a difficulty without `clearBarbarianCampReward` (`barbarians.py:391`).
fn default_camp_reward() -> i32 {
    25
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawSpeed {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
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
    pub turns: Vec<SpeedTurns>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawVictory {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub milestones: Vec<String>,
    #[serde(default)]
    pub required_spaceship_parts: Vec<String>,
    #[serde(default)]
    pub hidden_in_victory_screen: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawQuest {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, rename = "type")]
    pub scope: Option<QuestScope>,
    #[serde(default)]
    pub influence: Option<i32>,
    #[serde(default)]
    pub duration: Option<i32>,
    #[serde(default)]
    pub minimum_civs: Option<i32>,
    #[serde(default)]
    pub weight_for_city_state_type: DetMap<String, f64>,
    #[serde(default)]
    pub params: Vec<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawRuin {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub excluded_difficulties: Vec<String>,
    #[serde(default)]
    pub uniques: Vec<String>,
    /// How many times it is put among the rewards drawn from (UnCiv's `RuinReward.weight`);
    /// 1 when left out.
    #[serde(default)]
    pub weight: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawPersonality {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
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
    pub priorities: DetMap<String, i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawPolicyBranch {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub era: String,
    #[serde(default)]
    pub priorities: DetMap<VictoryFocus, i32>,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawPolicy {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub branch: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default, rename = "is_finisher")]
    pub is_finisher: bool,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawNation {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    pub kind: NationKind,
    #[serde(default)]
    pub leader_name: Option<String>,
    #[serde(default)]
    pub adjective: Option<String>,
    #[serde(default)]
    pub start_bias: Vec<String>,
    #[serde(default)]
    pub preferred_victory_type: Option<VictoryFocus>,
    #[serde(default)]
    pub personality: Option<String>,
    #[serde(default)]
    pub favored_religion: Option<String>,
    #[serde(default)]
    pub city_state_type: Option<String>,
    #[serde(default)]
    pub cities: Vec<String>,
    #[serde(default)]
    pub benchmark: bool,
    #[serde(default)]
    pub uniques: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_nation_ids_are_python_snake_case() {
        assert_eq!(snake_id("BenchmarkCiv"), "benchmarkciv");
        assert_eq!(snake_id("The Huns"), "the_huns");
        assert_eq!(snake_id("  Côte d'Ivoire!  "), "c_te_d_ivoire");
        assert_eq!(snake_id("\u{212a}orea"), "korea", "the Kelvin sign lower-cases to k");
    }

    #[test]
    fn an_unknown_field_is_an_error_naming_it() {
        let v: Value = serde_json::json!({"name": "Farm", "turnsToBuild": 7, "turnsToBiuld": 7});
        let e = RawImprovement::deserialize(&v).expect_err("unknown field");
        assert!(e.to_string().contains("unknown field `turnsToBiuld`"), "{e}");
    }
}

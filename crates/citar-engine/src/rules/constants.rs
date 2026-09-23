//! CITAR's constants: `citar/data/game.json`, typed (DESIGN.md 5.3), and the few constants the
//! client JSON carries from Python code rather than from the data.
//!
//! Python read `game.json` as a dict and its formula constants as `R.k["name"]`
//! (`rules.py:83-85`), so a misspelt key was a `KeyError` in the middle of a game. Here the file
//! is read into typed structs with every field required and no unknown field allowed, so the
//! same mistake fails when the ruleset loads.

use serde::Deserialize;

use crate::base::ids::{DifficultyId, SpeedId};

/// The ruleset format version the client JSON carries (`rules.py:19`, `RULES_VERSION`).
pub const RULES_VERSION: u32 = 2;

/// The colours of the major civilizations' seats, in seat order (`state.py:13-15`). The client
/// JSON carries them as `player_colors`.
pub const PLAYER_COLORS: [&str; 24] = [
    "#aa0000", "#0000aa", "#00aa00", "#aaaa00", "#50006e", "#c85a00", "#ff00e6", "#299bcc",
    "#00998a", "#cc298b", "#b2ff00", "#9900ff", "#00ffff", "#99741f", "#004c99", "#ff0000",
    "#00ff33", "#ff0066", "#0066ff", "#99003d", "#9b29cc", "#00ff80", "#0099ff", "#ffff00",
];

/// The 32 formula constants of `game.json`'s `constants`, which mirror UnCiv's `ModConstants`
/// and hard-coded rules. Integers where the rule counts, floats where it scales.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Formulas {
    pub max_xp_from_barbarians: i32,
    pub city_strength_base: f64,
    pub city_strength_per_pop: f64,
    pub city_strength_from_techs_multiplier: f64,
    pub city_strength_from_techs_exponent: f64,
    pub city_strength_from_techs_full_multiplier: f64,
    pub city_strength_from_garrison: f64,
    pub unit_supply_per_population: f64,
    pub minimal_city_distance: i32,
    pub minimal_city_distance_other_continents: i32,
    pub base_city_bombard_range: i32,
    pub city_work_range: i32,
    pub city_expand_range: i32,
    pub city_air_unit_capacity: i32,
    pub unit_upgrade_cost: UnitUpgradeCost,
    pub ancient_ruin_count_multiplier: f64,
    pub religion_limit_base: i32,
    pub religion_limit_multiplier: f64,
    pub pantheon_base: i32,
    pub pantheon_growth: i32,
    pub minimum_war_duration: i32,
    pub base_turns_until_revolt: i32,
    pub city_state_election_turns: i32,
    pub max_gold_trade_offer: i32,
    pub tribute_global_modifier: i32,
    pub tribute_local_modifier: i32,
    pub max_spy_rank: i32,
    pub spy_rank_skill_percent_bonus: i32,
    pub spy_rank_steal_percent_bonus: i32,
    pub spy_tech_steal_cost_modifier: f64,
    pub score_from_population: i32,
    pub score_from_wonders: i32,
}

/// The terms of the unit upgrade cost (`units.py:507-515`): `base` plus `per_production` per
/// point of production the upgrade adds, scaled by `1 + era_multiplier * era`, raised to
/// `exponent`, and truncated to a multiple of `round_to`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitUpgradeCost {
    pub base: f64,
    pub per_production: f64,
    pub era_multiplier: f64,
    pub exponent: f64,
    pub round_to: i32,
}

/// One map size of the lobby, such as `standard`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapSize {
    /// The key the lobby and the saves use: `standard`.
    pub key: Box<str>,
    /// The name shown: `Standard`.
    pub name: Box<str>,
    pub width: u16,
    pub height: u16,
    /// Major civilizations by default.
    pub players: u8,
    /// City-states by default.
    pub city_states: u8,
}

/// One of UnCiv's predefined map sizes, which scale research and policy costs by map area
/// (`rules.py:292-301`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapSizePredefined {
    pub name: Box<str>,
    /// The hexagonal radius of UnCiv's map of this size.
    pub radius: i32,
    pub tech_cost_multiplier: f64,
    pub tech_cost_per_city: f64,
    pub policy_cost_per_city: f64,
}

/// A map type of the lobby, such as `continents`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapType {
    /// The key: `continents`.
    pub key: Box<str>,
    /// The name shown: `Continents`.
    pub name: Box<str>,
}

/// A barbarian setting of the lobby, such as `raging`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarbarianLevel {
    /// The key: `off`, `normal`, `raging`.
    pub key: Box<str>,
    /// The setting, or `None` for barbarians switched off.
    pub level: Option<BarbarianSetting>,
}

/// What a barbarian level that is not off sets.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarbarianSetting {
    /// The name shown: `Raging`.
    pub name: Box<str>,
    /// UnCiv's raging barbarians.
    #[serde(default)]
    pub raging: bool,
    /// How aggressive the barbarians are, 0 to 100; Python's default is 50 (`rules.py:330`).
    pub aggression: Option<i32>,
}

impl BarbarianSetting {
    /// The aggression, or Python's default of 50 when the file gives none (`rules.py:330`).
    #[must_use]
    pub fn aggression(&self) -> i32 {
        self.aggression.unwrap_or(50)
    }
}

/// Limits on diplomacy chats.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diplomacy {
    /// The longest a negotiation's chat may grow.
    pub max_chat_messages: u32,
    /// How many negotiations one pair of civilizations may open in one turn.
    pub negotiations_per_pair_per_turn: u32,
}

/// Everything in `game.json`, typed and with its names resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Constants {
    /// Movement points per tile of movement: moves are counted in sixtieths.
    pub move_scale: i32,
    /// The lobby's map sizes, in file order.
    pub map_sizes: Vec<MapSize>,
    /// UnCiv's predefined sizes, smallest first; never empty.
    pub map_size_predefined: Vec<MapSizePredefined>,
    /// The lobby's map types, in file order.
    pub map_types: Vec<MapType>,
    pub default_speed: SpeedId,
    /// The speed benchmark games use.
    pub benchmark_speed: SpeedId,
    pub default_difficulty: DifficultyId,
    /// The most major civilizations a game may seat.
    pub max_players: u8,
    /// The formula constants (`R.k` in Python).
    pub formulas: Formulas,
    /// The lobby's barbarian settings, in file order.
    pub barbarian_levels: Vec<BarbarianLevel>,
    pub diplomacy: Diplomacy,
}

impl Constants {
    /// UnCiv's `getPredefinedOrNextSmaller` for a rectangular map: the largest predefined size
    /// whose radius is at most the radius of a hexagonal map of the same area, or the smallest
    /// (`rules.py:292-301`).
    #[must_use]
    pub fn map_size_predefined(&self, width: u16, height: u16) -> &MapSizePredefined {
        let area = f64::from(u32::from(width) * u32::from(height));
        // The inverse of a hexagonal map's area, 1 + 3r(r + 1). sqrt is correctly rounded on
        // every target, so it needs no libm wrapper.
        let radius = ((12.0 * area - 3.0).sqrt() - 3.0) / 6.0;
        let mut best = &self.map_size_predefined[0];
        for p in &self.map_size_predefined {
            if f64::from(p.radius) <= radius {
                best = p;
            }
        }
        best
    }

    /// The map size with this key.
    #[must_use]
    pub fn map_size(&self, key: &str) -> Option<&MapSize> {
        self.map_sizes.iter().find(|m| &*m.key == key)
    }
}

// ---- The raw file -----------------------------------------------------------------------------

/// `game.json` as written, before its names are resolved.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGame {
    /// The file's comment.
    #[serde(rename = "_doc", default)]
    pub _doc: Option<String>,
    pub move_scale: i32,
    pub map_sizes: indexmap::IndexMap<String, RawMapSize>,
    pub map_size_predefined: Vec<MapSizePredefined>,
    pub map_types: indexmap::IndexMap<String, RawMapType>,
    pub default_speed: String,
    pub benchmark_speed: String,
    pub default_difficulty: String,
    pub max_players: u8,
    pub constants: Formulas,
    pub barbarians: RawBarbarians,
    pub diplomacy: Diplomacy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawMapSize {
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub players: u8,
    pub city_states: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawMapType {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawBarbarians {
    pub levels: indexmap::IndexMap<String, Option<BarbarianSetting>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn predefined(radii: &[i32]) -> Vec<MapSizePredefined> {
        radii
            .iter()
            .map(|&radius| MapSizePredefined {
                name: format!("r{radius}").into(),
                radius,
                tech_cost_multiplier: 1.0,
                tech_cost_per_city: 0.05,
                policy_cost_per_city: 0.1,
            })
            .collect()
    }

    #[test]
    fn predefined_sizes_follow_unciv() {
        let k = Constants {
            move_scale: 60,
            map_sizes: vec![],
            map_size_predefined: predefined(&[10, 15, 20, 30, 40]),
            map_types: vec![],
            default_speed: SpeedId(0),
            benchmark_speed: SpeedId(0),
            default_difficulty: DifficultyId(0),
            max_players: 24,
            formulas: serde_json::from_str::<RawGame>(include_str!(
                "../../../../citar/data/game.json"
            ))
            .expect("the shipped game.json")
            .constants,
            barbarian_levels: vec![],
            diplomacy: Diplomacy { max_chat_messages: 30, negotiations_per_pair_per_turn: 2 },
        };
        // Python: radius of 44x28 is (sqrt(12*1232-3)-3)/6 = 19.76..., so Small (15).
        assert_eq!(k.map_size_predefined(44, 28).radius, 15);
        assert_eq!(k.map_size_predefined(60, 38).radius, 20);
        assert_eq!(k.map_size_predefined(160, 100).radius, 40);
        assert_eq!(k.map_size_predefined(8, 8).radius, 10, "below every radius: the smallest");
        assert_eq!(k.map_size_predefined(0, 0).radius, 10, "no area at all: the smallest");
    }
}

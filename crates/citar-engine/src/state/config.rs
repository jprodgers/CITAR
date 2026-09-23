//! A game's settings, typed (DESIGN.md 4.8).
//!
//! Replaces the config dict of `game.py:32-60` (`DEFAULT_CONFIG`) as `Game.new` left it after
//! normalising (`game.py:151-196`). Rule objects are ids, the seed is required (the host draws
//! one when the lobby leaves it empty), and an editor map travels inline as a document instead of
//! an id the engine would read from disk. Seats are not here: they live on the players.
//!
//! Keys the engine does not use (the server's `on_disconnect` and `reconnect_seconds`, the
//! lobby's player list, anything newer) are kept verbatim in [`GameConfig::host`], saved and
//! never digested.

use core::ops::{Deref, DerefMut};
use std::collections::BTreeMap;

use serde_json::Value;

use crate::base::ids::{
    BarbarianLevelId, DifficultyId, EraId, MapSizeId, MapTypeId, ResourceId, SpeedId, Turn,
    VictoryId,
};

/// A value the host keeps with the game that is saved but never digested: host activity must
/// not move the digest (DESIGN.md 4.7, 4.10).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostOnly<T>(pub T);

impl<T> Deref for HostOnly<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for HostOnly<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// What happens at the map's edges (`mapgen.py:26-34`, `EDGE_MODES`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapEdges {
    /// Polar ice north and south, open ocean east and west.
    #[default]
    IceCaps,
    /// A cylinder: east-west wraps, ice at the poles.
    WrapX,
    /// North-south wraps, open ocean east and west.
    WrapY,
    /// A torus: no edges and no ice.
    WrapBoth,
    /// Ice on all four sides.
    Boxed,
}

impl MapEdges {
    /// Every mode, in Python's order.
    pub const ALL: [Self; 5] =
        [Self::IceCaps, Self::WrapX, Self::WrapY, Self::WrapBoth, Self::Boxed];

    /// The key the lobby uses: `wrap_both`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::IceCaps => "ice_caps",
            Self::WrapX => "wrap_x",
            Self::WrapY => "wrap_y",
            Self::WrapBoth => "wrap_both",
            Self::Boxed => "boxed",
        }
    }

    /// The mode called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.name() == name)
    }

    /// Whether the map wraps east-west and north-south (`mapgen.edge_wraps`).
    #[must_use]
    pub const fn wraps(self) -> (bool, bool) {
        match self {
            Self::IceCaps | Self::Boxed => (false, false),
            Self::WrapX => (true, false),
            Self::WrapY => (false, true),
            Self::WrapBoth => (true, true),
        }
    }
}

/// Which difficulty base values the AI gets (`game.py:40`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AiBaseValues {
    /// UnCiv's.
    #[default]
    Unciv,
    /// Easier AIs use Prince base values (`economy.difficulty`).
    Monotonic,
}

/// A lobby rule for one resource (`mapgen.py:35, 84-93`); a resource without one is placed
/// normally.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResourceRule {
    /// Not placed at all.
    Off,
    /// At most this many deposits.
    Cap(f64),
    /// This share, in percent, of the kind's deposits.
    Share(f64),
}

/// The lobby's density and rules for one kind of resource.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceKindOptions {
    /// Scales how many of this kind are placed, 1 being normal.
    pub density: f64,
    /// Rules for single resources, sorted by resource.
    pub each: Vec<(ResourceId, ResourceRule)>,
}

impl Default for ResourceKindOptions {
    fn default() -> Self {
        Self { density: 1.0, each: Vec::new() }
    }
}

/// How the map generator places resources (`mapgen.py:52-93`, the lobby's `resources`).
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceOptions {
    /// Scales every kind, 1 being normal.
    pub density: f64,
    pub strategic: ResourceKindOptions,
    pub luxury: ResourceKindOptions,
    pub bonus: ResourceKindOptions,
}

impl Default for ResourceOptions {
    fn default() -> Self {
        Self {
            density: 1.0,
            strategic: ResourceKindOptions::default(),
            luxury: ResourceKindOptions::default(),
            bonus: ResourceKindOptions::default(),
        }
    }
}

/// An editor map, sent inline by the host, which resolves a map id to its document; Python read
/// it from disk inside `Game.new` (`game.py:165-170`). `api::maps` validates the document when
/// the game is set up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapDoc {
    /// The map's id or name, for display.
    pub id: Box<str>,
    /// The editor document (`maps.py`'s format).
    pub body: Value,
}

/// Where the map comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum MapSource {
    /// The generator (`mapgen.generate_map`).
    Generated {
        size: MapSizeId,
        map_type: MapTypeId,
        edges: MapEdges,
        /// Width and height set explicitly, instead of the size's.
        dims: Option<(u16, u16)>,
    },
    /// An editor map.
    Document(Box<MapDoc>),
}

/// The game's own diplomacy settings (`diplomacy.py:686-691`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DiplomacyConfig {
    /// How many messages a negotiation may hold before it closes; `None` for the ruleset's.
    pub max_chat_messages: Option<u16>,
}

/// A game's settings (`DEFAULT_CONFIG`, `game.py:32-60`, normalised).
#[derive(Clone, Debug, PartialEq)]
pub struct GameConfig {
    /// Every random stream derives from it (DESIGN.md 7).
    pub seed: u64,
    pub map: MapSource,
    pub speed: SpeedId,
    /// The game's difficulty; a seat may have its own.
    pub difficulty: DifficultyId,
    pub barbarian_difficulty: DifficultyId,
    pub ai_base_values: AiBaseValues,
    pub starting_era: EraId,
    pub barbarians: BarbarianLevelId,
    /// 0-100; `None` for the barbarian level's own.
    pub barbarian_aggression: Option<u8>,
    pub turn_limit: Turn,
    /// Victories switched off, sorted; every other one counts.
    pub disabled_victories: Vec<VictoryId>,
    pub city_states: u8,
    pub religion: bool,
    pub espionage: bool,
    pub nuclear_weapons: bool,
    pub tech_trading: bool,
    pub ruins: bool,
    /// Scales how many rivers are traced: 0 none, 1 normal.
    pub river_density: f64,
    pub resources: ResourceOptions,
    pub diplomacy: DiplomacyConfig,
    /// Keys the engine does not use, kept verbatim.
    pub host: HostOnly<BTreeMap<String, Value>>,
}

impl GameConfig {
    /// Settings with these essentials and Python's defaults for everything else: every victory,
    /// religion, espionage, nuclear weapons, tech trading and ruins on, normal rivers and
    /// resources, the game difficulty for the barbarians, no city-states yet.
    #[must_use]
    pub fn new(
        seed: u64,
        map: MapSource,
        speed: SpeedId,
        difficulty: DifficultyId,
        starting_era: EraId,
        barbarians: BarbarianLevelId,
        turn_limit: Turn,
    ) -> Self {
        Self {
            seed,
            map,
            speed,
            difficulty,
            barbarian_difficulty: difficulty,
            ai_base_values: AiBaseValues::Unciv,
            starting_era,
            barbarians,
            barbarian_aggression: None,
            turn_limit,
            disabled_victories: Vec::new(),
            city_states: 0,
            religion: true,
            espionage: true,
            nuclear_weapons: true,
            tech_trading: true,
            ruins: true,
            river_density: 1.0,
            resources: ResourceOptions::default(),
            diplomacy: DiplomacyConfig::default(),
            host: HostOnly::default(),
        }
    }

    /// Whether a victory counts in this game.
    #[must_use]
    pub fn victory_enabled(&self, v: VictoryId) -> bool {
        self.disabled_victories.binary_search(&v).is_err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_by_name_and_their_wraps() {
        for e in MapEdges::ALL {
            assert_eq!(MapEdges::from_name(e.name()), Some(e));
        }
        assert_eq!(MapEdges::WrapX.wraps(), (true, false));
        assert_eq!(MapEdges::Boxed.wraps(), (false, false));
        assert_eq!(MapEdges::default(), MapEdges::IceCaps);
    }

    #[test]
    fn victories_count_unless_switched_off() {
        let map = MapSource::Generated {
            size: MapSizeId(1),
            map_type: MapTypeId(0),
            edges: MapEdges::IceCaps,
            dims: None,
        };
        let mut cfg = GameConfig::new(
            7,
            map,
            SpeedId(0),
            DifficultyId(3),
            EraId(0),
            BarbarianLevelId(1),
            500,
        );
        assert!(cfg.victory_enabled(VictoryId(2)));
        cfg.disabled_victories = vec![VictoryId(2), VictoryId(4)];
        assert!(!cfg.victory_enabled(VictoryId(2)) && cfg.victory_enabled(VictoryId(3)));
        assert_eq!(cfg.barbarian_difficulty, DifficultyId(3));
        cfg.host.insert("on_disconnect".to_owned(), Value::from("pause"));
        assert_eq!(cfg.host.len(), 1);
    }
}

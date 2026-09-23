//! What the unique evaluator asks of a world (DESIGN.md 5.7 and 5.11).
//!
//! Filters and conditionals read the game through traits of plain facts, so they can be tested
//! against mock worlds before any `Game` exists, and so map generation, which has a map but no
//! game, can evaluate its filters too:
//! - [`TileFacts`]: what a tile's terrain says. Map generation implements it over the map it is
//!   building (DESIGN.md 5.7: map-generation filters use only terrain-level leaves);
//! - [`FilterFacts`]: everything else a dynamic filter reads about civilizations, tiles, units and
//!   cities (`uniques.py:398-701`).
//!
//! Package 1a-07 adds `EvalWorld`, the conditionals' facts, on top of [`FilterFacts`], with `Ctx`.
//! Each method answers one question Python's filters asked of `Game`; the Python lines are named
//! where the answer is not simply a field.

use crate::base::ids::{
    BaseUnitId, CityId, ImprovementId, NationId, PlayerId, ReligionId, ResourceId, TileIdx, UnitId,
};
use crate::base::sets::{BuildingSet, PromotionSet, TerrainSet};
use crate::rules::defs::NationKind;

/// What a tile's terrain says: all a map-generation filter may read.
pub trait TileFacts {
    /// Every terrain on the tile: its base terrain, its features and its natural wonder
    /// (`tiles.py:38-44`, `mapgen.py:404-410`).
    fn tile_terrains(&self, t: TileIdx) -> TerrainSet;

    /// Whether a river runs along the tile.
    fn tile_river(&self, t: TileIdx) -> bool;

    /// Whether the tile has fresh water: a river, or a source of fresh water (a lake, an oasis)
    /// on it or next to it (`tiles.py:111-125`).
    fn tile_fresh_water(&self, t: TileIdx) -> bool;

    /// Whether a neighbour of the tile is coast (`tiles.py:128-135`).
    fn tile_next_to_coast(&self, t: TileIdx) -> bool;
}

/// What a dynamic filter reads besides the terrain.
pub trait FilterFacts: TileFacts {
    // ---- Civilizations -----------------------------------------------------------------------

    /// The civilization's nation.
    fn civ_nation(&self, p: PlayerId) -> NationId;

    /// Whether the civilization is a major one, a city-state or the barbarians.
    fn civ_kind(&self, p: PlayerId) -> NationKind;

    /// Whether a human holds the seat: the handicap Python compared with `"human"`
    /// (`uniques.py:566-569`).
    fn civ_is_human(&self, p: PlayerId) -> bool;

    /// The religion the civilization founded, if any.
    fn civ_religion(&self, p: PlayerId) -> Option<ReligionId>;

    /// Whether `a` and `b` are at war.
    fn at_war(&self, a: PlayerId, b: PlayerId) -> bool;

    /// Whether `a` has met `b`.
    fn has_met(&self, a: PlayerId, b: PlayerId) -> bool;

    /// Whether `a` counts `b` a friend (`Game.is_friend`).
    fn is_friend(&self, a: PlayerId, b: PlayerId) -> bool;

    /// Whether `a` has open borders with `b` (`Game.has_open_borders`).
    fn has_open_borders(&self, a: PlayerId, b: PlayerId) -> bool;

    // ---- Tiles --------------------------------------------------------------------------------

    /// The tile's owner.
    fn tile_owner(&self, t: TileIdx) -> Option<PlayerId>;

    /// Whether the tile is friendly territory to `p`: its own, or a friend's it may enter
    /// (`tiles.py:151-165`).
    fn tile_friendly_to(&self, t: TileIdx, p: PlayerId) -> bool;

    /// The tile's resource, visible or not.
    fn tile_resource(&self, t: TileIdx) -> Option<ResourceId>;

    /// Whether `p` can see resource `r`: it needs no tech, or `p` has the tech that reveals it
    /// (`tiles.py:143-148`).
    fn resource_visible(&self, p: PlayerId, r: ResourceId) -> bool;

    /// The tile's improvement, unless it is pillaged.
    fn tile_improvement(&self, t: TileIdx) -> Option<ImprovementId>;

    /// The tile's route, unless it is pillaged.
    fn tile_route(&self, t: TileIdx) -> Option<ImprovementId>;

    /// Whether the tile's improvement or its route is pillaged.
    fn tile_pillaged(&self, t: TileIdx) -> bool;

    /// Whether a city works the tile.
    fn tile_worked(&self, t: TileIdx) -> bool;

    // ---- Units --------------------------------------------------------------------------------

    fn unit_owner(&self, u: UnitId) -> PlayerId;

    /// The unit's row in `units.json`.
    fn unit_base(&self, u: UnitId) -> BaseUnitId;

    fn unit_promotions(&self, u: UnitId) -> PromotionSet;

    /// Whether the unit has lost health (`uniques.py:546`).
    fn unit_wounded(&self, u: UnitId) -> bool;

    fn unit_embarked(&self, u: UnitId) -> bool;

    /// Whether the unit has set up to attack (the `Set Up` status, `combat.py:763-764`).
    fn unit_set_up(&self, u: UnitId) -> bool;

    // ---- Cities -------------------------------------------------------------------------------

    fn city_owner(&self, c: CityId) -> PlayerId;

    /// The civilization that founded the city.
    fn city_founder(&self, c: CityId) -> PlayerId;

    fn city_buildings(&self, c: CityId) -> BuildingSet;

    fn city_is_capital(&self, c: CityId) -> bool;

    /// Whether the city's tile is next to the coast (`Game.is_coastal`).
    fn city_coastal(&self, c: CityId) -> bool;

    /// Whether the city suffers the unhappiness of an annexed city (`cities.has_annex_unhappiness`).
    fn city_annex_unhappiness(&self, c: CityId) -> bool;

    fn city_puppet(&self, c: CityId) -> bool;

    /// Whether a road or harbour network links the city to its owner's capital
    /// (`cities.connected_to_capital`).
    fn city_connected_to_capital(&self, c: CityId) -> bool;

    /// Whether a military unit stands in the city (`cities.is_garrisoned`).
    fn city_garrisoned(&self, c: CityId) -> bool;

    /// Whether the city still resists its conqueror.
    fn city_resisting(&self, c: CityId) -> bool;

    fn city_razing(&self, c: CityId) -> bool;

    /// Whether the city is some religion's holy city.
    fn city_holy(&self, c: CityId) -> bool;

    /// The religion most of the city follows (`religion.majority_religion`).
    fn city_majority_religion(&self, c: CityId) -> Option<ReligionId>;

    // ---- Religions ----------------------------------------------------------------------------

    /// Whether the religion is a major religion rather than a pantheon (`religion.is_major`).
    fn religion_is_major(&self, r: ReligionId) -> bool;

    /// Whether the religion has been enhanced.
    fn religion_is_enhanced(&self, r: ReligionId) -> bool;
}

//! What the unique evaluator asks of a world (DESIGN.md 5.7, 5.8 and 5.11), and the context it
//! asks it in.
//!
//! Filters and conditionals read the game through traits of plain facts, so they can be tested
//! against mock worlds before any `Game` exists, and so map generation, which has a map but no
//! game, can evaluate its filters too:
//! - [`TileFacts`]: what a tile's terrain says. Map generation implements it over the map it is
//!   building (DESIGN.md 5.7: map-generation filters use only terrain-level leaves);
//! - [`FilterFacts`]: everything else a dynamic filter reads about civilizations, tiles, units and
//!   cities (`uniques.py:398-701`);
//! - [`EvalWorld`]: what the conditionals, the countables and the queries read besides
//!   (`uniques.py:778-1083`), with the unique indexes ([`IndexRef`]). `game::eval::EvalView` is
//!   its one production implementation, and validates each index it hands out on read (DESIGN.md
//!   6.3), so that no caller has to know which memos an evaluation touches.
//!
//! [`Ctx`] ports `Ctx` (`uniques.py:215-293`): what a unique is asked about. It is `Copy`, holds
//! ids only, and derives its civilization and tile from what it is given, as Python's constructor
//! did ([`Ctx::resolve`]). Its constructors (`civ`, `city`, `unit`, `fight`, `tile`) resolve; a
//! context written field by field must call `resolve` itself, which debug builds check.
//!
//! Each method answers one question Python asked of `Game`; the Python lines are named where the
//! answer is not simply a field. The traits are generic and monomorphised, deliberately not
//! object-safe.

use core::cell::Ref;
use core::ops::Deref;

use super::filter::Combatant;
use super::index::Csr;
use crate::base::hex::HexGrid;
use crate::base::ids::{
    BaseUnitId, CityId, DifficultyId, EraId, ImprovementId, NationId, PlayerId, ReligionId,
    ResourceId, SpecialistId, SpeedId, TechId, TileIdx, Turn, UnitId, VictoryId,
};
use crate::base::sets::{BeliefSet, BuildingSet, PolicySet, PromotionSet, TechSet, TerrainSet};
use crate::base::stats::Stat;
use crate::rules::Ruleset;
use crate::rules::defs::{NationKind, ReligionProgress};

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

    /// Whether `a` counts `b` a friend (`Game.is_friend`, `game.py:677-686`): a friendship
    /// declared until a turn not yet past, or, with a city-state, its influence at the friend
    /// level or above.
    fn is_friend(&self, a: PlayerId, b: PlayerId) -> bool;

    /// Whether `a` lets `b`'s units through its territory (`Game.has_open_borders`,
    /// `game.py:705-708`): an agreement until a turn not yet past.
    fn has_open_borders(&self, a: PlayerId, b: PlayerId) -> bool;

    // ---- Tiles --------------------------------------------------------------------------------

    /// The tile's owner.
    fn tile_owner(&self, t: TileIdx) -> Option<PlayerId>;

    /// Whether the tile is friendly territory to `p` (`tiles.py:151-165`): its own, or the
    /// territory of a civilization `p` has met that is either a city-state whose influence with
    /// `p` is at the friend level or above (or any city-state, when `p` has the unique
    /// `City-State territory always counts as friendly territory`), or one that gives `p` open
    /// borders until a turn not yet past. Its answer changes with the tile's owner, met, open
    /// borders, the turn, influence and `p`'s uniques.
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

/// Which of a civilization's unique indexes to read (DESIGN.md 6.6): without the uniques of the
/// resources it has, which is what the resource supply itself is computed from, or with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndexLayer {
    /// `civ_umaps_no_resources` (`economy.py:91-129`): `CivIndex`.
    NoResources,
    /// `civ_umaps` (`economy.py:77-88`): `CivIndexFull`, with the resource layer.
    Full,
}

/// A unique index a world hands out: borrowed from a table, or from a memo that validated itself
/// before it lent it (DESIGN.md 6.3). Either way it reads as a [`Csr`].
#[derive(Debug)]
pub enum IndexRef<'a> {
    /// An index held plainly, as a mock world or a one-off table holds it.
    Plain(&'a Csr),
    /// An index held in a memo.
    Memo(Ref<'a, Csr>),
}

impl Deref for IndexRef<'_> {
    type Target = Csr;

    fn deref(&self) -> &Csr {
        match self {
            Self::Plain(c) => c,
            Self::Memo(r) => r,
        }
    }
}

impl<'a> From<&'a Csr> for IndexRef<'a> {
    fn from(c: &'a Csr) -> Self {
        Self::Plain(c)
    }
}

impl<'a> From<Ref<'a, Csr>> for IndexRef<'a> {
    fn from(r: Ref<'a, Csr>) -> Self {
        Self::Memo(r)
    }
}

/// What the conditionals, countables, queries and triggers read of a game besides what a filter
/// reads (DESIGN.md 5.11): plain facts of the game, its civilizations, cities, tiles and units,
/// and the unique indexes. Every fact is about the game as it stands; none changes it.
///
/// The class of [`super::CondDeps`] each group of facts falls under is named on the group, so an
/// implementation knows which revisions a fact must move with.
pub trait EvalWorld: FilterFacts {
    // ---- The game and its settings ------------------------------------------------------------

    /// The ruleset the game plays by, which compiled the uniques being evaluated.
    fn rules(&self) -> &Ruleset;

    /// The game's seed, for the keyed draws of `with [n]% chance` (DESIGN.md 7.1).
    fn seed(&self) -> u64;

    /// The map's grid.
    fn grid(&self) -> &HexGrid;

    /// The turn (`TURN`).
    fn turn(&self) -> Turn;

    /// The game speed (`CONFIG`).
    fn speed(&self) -> SpeedId;

    /// The era the game started in (`CONFIG`): Python's `config["starting_era"]`, the ancient
    /// era by default (`uniques.py:872`).
    fn starting_era(&self) -> EraId;

    /// The difficulty a seat plays on (`SEAT`), as `economy.difficulty` gave it: a humanlike
    /// seat's own, an AI seat's `aiDifficultyLevel`; with no seat, the game's (`CONFIG`).
    fn difficulty(&self, p: Option<PlayerId>) -> DifficultyId;

    /// Whether a victory is enabled for the game (`CONFIG`, `game.py:360-362`).
    fn victory_enabled(&self, v: VictoryId) -> bool;

    /// Whether religion is in play (`CONFIG`, `game.py:343-349`).
    fn religion_enabled(&self) -> bool;

    /// Whether espionage is in play (`CONFIG`).
    fn espionage_enabled(&self) -> bool;

    /// Whether nuclear weapons may be built (`CONFIG`).
    fn nukes_enabled(&self) -> bool;

    /// Every living civilization, in seat order (`CITY_COUNT`).
    fn civs(&self) -> impl Iterator<Item = PlayerId> + '_;

    /// Every city of the game, in id order (`GLOBAL_BUILDINGS` reads their buildings).
    fn cities(&self) -> impl Iterator<Item = CityId> + '_;

    // ---- Civilizations ------------------------------------------------------------------------

    /// Whether the civilization is at war with anyone but the barbarians (`WAR`,
    /// `Game.is_at_war_any`, `game.py:673-675`).
    fn civ_at_war(&self, p: PlayerId) -> bool;

    /// Whether the civilization is in a golden age (`GOLDEN_AGE`).
    fn civ_golden_age(&self, p: PlayerId) -> bool;

    /// The civilization's happiness as conditionals see it (`HAPPINESS_SEEN`): the value
    /// committed at fixed stages of the turn, not the live one (DESIGN.md 6.6).
    fn civ_happiness(&self, p: PlayerId) -> i32;

    /// The civilization's stock of a stat (`STOCKS`), as `Game.stat_reserve` read it
    /// (`game.py:630-634`): gold, culture and faith; golden age points for happiness; none of
    /// science, food or production.
    fn civ_stock(&self, p: PlayerId, s: Stat) -> f64;

    /// How much of a resource the civilization has available (`RESOURCES`,
    /// `economy.resource_amount`): the staged supply (DESIGN.md 6.6), which is computed from the
    /// index without the resource layer, so a resource's uniques never feed its own supply.
    fn civ_resource(&self, p: PlayerId, r: ResourceId) -> i32;

    /// The civilization's era (`ERA`, `research.player_era`).
    fn civ_era(&self, p: PlayerId) -> EraId;

    /// The civilization's techs (`TECHS`).
    fn civ_techs(&self, p: PlayerId) -> TechSet;

    /// The tech the civilization researches now: the head of its queue (`RESEARCH_QUEUE`).
    fn civ_researching(&self, p: PlayerId) -> Option<TechId>;

    /// The civilization's adopted branches and policies (`POLICIES`).
    fn civ_policies(&self, p: PlayerId) -> PolicySet;

    /// The number of branches the civilization has completed (`POLICIES`,
    /// `policies.completed_branches`).
    fn civ_completed_branches(&self, p: PlayerId) -> i32;

    /// The beliefs of the civilization's own religion (`RELIGION_STATE`, `religion.civ_beliefs`).
    fn civ_beliefs(&self, p: PlayerId) -> BeliefSet;

    /// How far the civilization has come with religion (`RELIGION_STATE`).
    fn civ_religion_progress(&self, p: PlayerId) -> ReligionProgress;

    /// How many great prophets the civilization has earned (`RELIGION_STATE`).
    fn civ_prophets_earned(&self, p: PlayerId) -> i32;

    /// The civilization's capital, if it has one (`CITY_COUNT`).
    fn civ_capital(&self, p: PlayerId) -> Option<CityId>;

    /// The civilization's cities, in id order (`CITY_COUNT`).
    fn civ_cities(&self, p: PlayerId) -> impl Iterator<Item = CityId> + '_;

    /// The civilization's units, in id order (`UNIT_SET`).
    fn civ_units(&self, p: PlayerId) -> impl Iterator<Item = UnitId> + '_;

    // ---- Cities (the city in context is `CITY`) ----------------------------------------------

    /// The city's tile.
    fn city_tile(&self, c: CityId) -> TileIdx;

    /// The city's health, for `when above [n] HP` in a city's fight (`combat.py`
    /// `Combatant.hp`).
    fn city_health(&self, c: CityId) -> i32;

    /// The food the city has stored: the countable `Food` in a city (`uniques.py:757-758`).
    fn city_food(&self, c: CityId) -> f64;

    /// The city's population.
    fn city_population(&self, c: CityId) -> i32;

    /// The city's specialists of one kind, or of every kind for `None`.
    fn city_specialists(&self, c: CityId, s: Option<SpecialistId>) -> i32;

    /// The citizens working no tile and no specialist slot (`cities.free_population`).
    fn city_unemployed(&self, c: CityId) -> i32;

    /// How many of the city's citizens follow its majority religion
    /// (`religion.followers_of_majority`).
    fn city_majority_followers(&self, c: CityId) -> i32;

    // ---- Tiles (the tile in context is `TILE`) ------------------------------------------------

    /// The city whose territory the tile is (`Tile.city`).
    fn tile_city(&self, t: TileIdx) -> Option<CityId>;

    /// The landmass the tile is on (`Game.continent`); `None` for water, or a tile no landmass
    /// was numbered for.
    fn tile_landmass(&self, t: TileIdx) -> Option<u16>;

    /// The units on the tile, in id order (`UNIT_SET`).
    fn units_at(&self, t: TileIdx) -> impl Iterator<Item = UnitId> + '_;

    // ---- Units (the unit in context is `UNIT`) ------------------------------------------------

    /// The unit's tile.
    fn unit_tile(&self, u: UnitId) -> TileIdx;

    /// The unit's health, 100 when unhurt.
    fn unit_health(&self, u: UnitId) -> i32;

    /// Whether the unit has used a limited action this turn (`Unit.abilities_used` is not
    /// empty).
    fn unit_used_actions(&self, u: UnitId) -> bool;

    // ---- The unique indexes (DESIGN.md 5.12) --------------------------------------------------

    /// The civilization's index of standing and triggered uniques.
    fn civ_index(&self, p: PlayerId, layer: IndexLayer) -> IndexRef<'_>;

    /// The uniques that hold only in the city: its buildings' local ones, and the Marble
    /// decision's local resource uniques.
    fn city_local(&self, c: CityId) -> IndexRef<'_>;

    /// The uniques a religion gives the cities that follow it: its follower beliefs'.
    fn follower(&self, r: ReligionId) -> IndexRef<'_>;

    /// The uniques of the unit's profile: its base unit's, its unit type's and its promotions'
    /// (`units.unit_umap`, `units.py:21-33`).
    fn unit_index(&self, u: UnitId) -> IndexRef<'_>;
}

/// Who attacks in a fight (`Ctx.action`, `"attack"` or `"defend"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CombatAction {
    Attack,
    Defend,
}

/// The fight a unique is asked about (`Ctx.our`, `their`, `attacked_tile` and `action`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CombatCtx {
    /// The side whose unique it is.
    pub our: Combatant,
    /// The other side.
    pub their: Option<Combatant>,
    /// The tile under attack.
    pub attacked_tile: Option<TileIdx>,
    /// Whether our side attacks or defends; `None` for a question about a fight that is neither.
    pub action: Option<CombatAction>,
}

/// What a unique is asked about (`Ctx`, `uniques.py:215-293`): a civilization, and optionally a
/// city, a unit, a tile and a fight. `ignore_conditionals` makes every conditional hold, as
/// Python's `ignore` and a missing context did (`uniques.py:789-790`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ctx {
    pub civ: Option<PlayerId>,
    pub city: Option<CityId>,
    pub unit: Option<UnitId>,
    pub tile: Option<TileIdx>,
    pub combat: Option<CombatCtx>,
    pub ignore_conditionals: bool,
}

impl Ctx {
    /// A context in which every conditional holds: Python's `applies(u, None)` and `ignore`.
    pub const IGNORE: Self = Self {
        civ: None,
        city: None,
        unit: None,
        tile: None,
        combat: None,
        ignore_conditionals: true,
    };

    /// A civilization-wide question (`civ_ctx`, `uniques.py:286-288`).
    #[must_use]
    pub const fn civ(p: PlayerId) -> Self {
        Self {
            civ: Some(p),
            city: None,
            unit: None,
            tile: None,
            combat: None,
            ignore_conditionals: false,
        }
    }

    /// A question about a city, asked by its owner on its tile (`cities.city_ctx`).
    #[must_use]
    pub fn city<W: EvalWorld>(w: &W, c: CityId) -> Self {
        Self { city: Some(c), ..Self::default() }.resolve(w)
    }

    /// A question about a unit, asked by its owner on its tile (`units.unit_ctx`).
    #[must_use]
    pub fn unit<W: EvalWorld>(w: &W, u: UnitId) -> Self {
        Self { unit: Some(u), ..Self::default() }.resolve(w)
    }

    /// A question about a fight, asked by our side's owner on our side's tile, as the combat
    /// modules built it (`combat.py:137, 460, 676, 732`).
    #[must_use]
    pub fn fight<W: EvalWorld>(w: &W, combat: CombatCtx) -> Self {
        Self { combat: Some(combat), ..Self::default() }.resolve(w)
    }

    /// A question about a tile, asked by `civ`.
    #[must_use]
    pub const fn tile(civ: Option<PlayerId>, t: TileIdx) -> Self {
        Self {
            civ,
            city: None,
            unit: None,
            tile: Some(t),
            combat: None,
            ignore_conditionals: false,
        }
    }

    /// The context with its civilization and tile derived from what it holds, as Python's
    /// constructor derived them (`uniques.py:233-248`): the civilization of the city, else of the
    /// unit, else of our side of the fight; the tile of the unit, else of the city, else of our
    /// side. Fields already set are kept.
    #[must_use]
    pub fn resolve<W: EvalWorld>(mut self, w: &W) -> Self {
        let our = self.combat.map(|c| c.our);
        if self.civ.is_none() {
            self.civ = match (self.city, self.unit, our) {
                (Some(c), _, _) => Some(w.city_owner(c)),
                (None, Some(u), _) => Some(w.unit_owner(u)),
                (None, None, Some(o)) => Some(combatant_owner(w, o)),
                (None, None, None) => None,
            };
        }
        if self.tile.is_none() {
            self.tile = match (self.unit, self.city, our) {
                (Some(u), _, _) => Some(w.unit_tile(u)),
                (None, Some(c), _) => Some(w.city_tile(c)),
                (None, None, Some(o)) => Some(combatant_tile(w, o)),
                (None, None, None) => None,
            };
        }
        self
    }

    /// Whether the civilization and the tile are derived already: [`resolve`](Self::resolve)
    /// would change nothing. A context built field by field that forgot to resolve has a city,
    /// unit or fight but no civilization, and every conditional about the civilization would
    /// fail in it without a word; [`super::cond::applies`] checks this in debug builds.
    #[must_use]
    pub fn is_resolved<W: EvalWorld>(&self, w: &W) -> bool {
        self.resolve(w) == *self
    }

    /// The unit a rule means (`rel_unit`): our side's, in a fight with a unit on our side,
    /// otherwise the one in context.
    #[must_use]
    pub fn rel_unit(&self) -> Option<UnitId> {
        match self.combat {
            Some(CombatCtx { our: Combatant::Unit(u), .. }) => Some(u),
            _ => self.unit,
        }
    }

    /// The tile a rule means (`rel_tile`): the one under attack, in a fight, otherwise the one in
    /// context.
    #[must_use]
    pub fn rel_tile(&self) -> Option<TileIdx> {
        self.combat.and_then(|c| c.attacked_tile).or(self.tile)
    }

    /// The city a rule means (`rel_city`): the one in context, else our side's in a fight, else
    /// the city whose territory the tile in context is, if the civilization in context owns it.
    /// So in a tile's or a unit's context `in [Capital] cities` asks about the city whose
    /// territory it is, which is why the city conditionals read `TILE` besides `CITY`.
    #[must_use]
    pub fn rel_city<W: EvalWorld>(&self, w: &W) -> Option<CityId> {
        if self.city.is_some() {
            return self.city;
        }
        if let Some(CombatCtx { our: Combatant::City(c), .. }) = self.combat {
            return Some(c);
        }
        let c = w.tile_city(self.tile?)?;
        (Some(w.city_owner(c)) == self.civ).then_some(c)
    }
}

/// A combatant's owner.
pub fn combatant_owner<W: EvalWorld>(w: &W, c: Combatant) -> PlayerId {
    match c {
        Combatant::Unit(u) => w.unit_owner(u),
        Combatant::City(c) => w.city_owner(c),
    }
}

/// A combatant's tile.
pub fn combatant_tile<W: EvalWorld>(w: &W, c: Combatant) -> TileIdx {
    match c {
        Combatant::Unit(u) => w.unit_tile(u),
        Combatant::City(c) => w.city_tile(c),
    }
}

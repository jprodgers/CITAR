//! Evaluation (package 1a-07, DESIGN.md 5.8-5.12) against its contract:
//! - gate 1: every conditional holds where it should and fails where it should in a mock world,
//!   one case per `CondData` variant (the match has no wildcard, so a new variant does not compile
//!   here until it has its case), and a table of what each conditional reads;
//! - gate 2: Python's edge cases: no civilization in context, and a context that ignores
//!   conditionals;
//! - gate 3: a chance draw is keyed by the unique's key, and a missing civilization, tile or unit
//!   is not id 0;
//! - gate 4 is in `tests/props.rs`: a civilization's index is the same whatever order its sources
//!   come in;
//! - gate 5: every `OneTimeEffect` kind decodes, from the one-time uniques of the kitchen sink;
//! - gate 6: `Cond::describe` and `uq::requirement_problems` say what Python's `_not_met` said
//!   (`data/not_met.json`, recorded by `scripts/refcheck/not_met_dump.py`).
//!
//! Besides: the countables, the indexes, the queries and `fire`, against the same mock world.
//!
//! The ruleset is the kitchen sink with one more nation, `Eval Test`, that carries `[+1 Gold]`
//! under each conditional once ([`CONDS`]), so every conditional's case reads a compiled one.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use citar_engine::base::hex::HexGrid;
use citar_engine::base::ids::{
    BaseUnitId, BeliefId, BuildingId, CityId, CityStateTypeId, DifficultyId, EraId, ImprovementId,
    NationId, PlayerId, PolicyId, PromotionId, ReligionId, ResourceId, SpecialistId, SpeedId,
    TechId, TerrainId, TileIdx, UniqueId, UnitId, VictoryId,
};
use citar_engine::base::rng::{KeyPart, Purpose, Rng};
use citar_engine::base::sets::{
    BeliefSet, BuildingSet, PolicySet, PromotionSet, ResourceSet, TechSet, TerrainSet,
};
use citar_engine::base::stats::Stat;
use citar_engine::rules::defs::{NationKind, ReligionProgress};
use citar_engine::rules::{Named, Ruleset};
use citar_engine::unique::cond::{
    self, Problem, ProblemKind, applies, applies_scoped, chance_keys, deps_of, holds, in_scope,
};
use citar_engine::unique::countable::Countable;
use citar_engine::unique::filter::{CityLeaf, Combatant, Leaf, UnitFacts, UnitScope};
use citar_engine::unique::index::{
    self, CityStateBonus, CivIndex, CivSources, Csr, placeholder_counts,
};
use citar_engine::unique::params::{PolicyOrBelief, PromotionOrStatus, StatOrResource};
use citar_engine::unique::trigger::{
    CityScope, OneTimeEffect, TriggerEvent, TriggerKind, TriggerSite, UnitEffect, fire,
};
use citar_engine::unique::world::{CombatAction, CombatCtx};
use citar_engine::unique::{
    CondData, CondDeps, Ctx, EvalWorld, FilterFacts, IndexLayer, IndexRef, Role, Source, TileFacts,
    UFlags, UniqueData, UniqueType, uq,
};
use citar_testkit::rulesets::{KITCHEN_SINK, files_of, kitchen_sink, overlay};
use serde_json::{Value, json};

use super::rules::shipped;

const NOT_MET: &str = include_str!("../../data/not_met.json");

/// The conditionals of the `Eval Test` nation, each under `[+1 Gold]`: every supported
/// conditional but the map-generation ones, which only map-generation and inert uniques carry
/// (the shipped ruleset's start biases have both).
const CONDS: &[&str] = &[
    "every [3] turns",
    "before turn number [10]",
    "after turn number [10]",
    "on [Quick] game speed",
    "on [Prince] difficulty",
    "on [Prince] difficulty or higher",
    "on [Prince] difficulty or lower",
    "when [Scientific] Victory is enabled",
    "when [Scientific] Victory is disabled",
    "when religion is enabled",
    "when religion is disabled",
    "when espionage is enabled",
    "when espionage is disabled",
    "when nuclear weapons are enabled",
    "when nuclear weapons are disabled",
    "with [100]% chance",
    "with [0]% chance",
    "if tutorials are enabled",
    "if tutorial [Eval] is completed",
    "for [Major] Civilizations",
    "for [Human player] Civilizations",
    "for [Hostile] Civilizations",
    "when at war",
    "when not at war",
    "during a Golden Age",
    "when not in a Golden Age",
    "while the empire is happy",
    "during the [Medieval era]",
    "before the [Medieval era]",
    "starting from the [Medieval era]",
    "if starting in the [Ancient era]",
    "after discovering [Writing]",
    "before discovering [Writing]",
    "while researching [Writing]",
    "if no Civilization has adopted [Aristocracy]",
    "after adopting [Aristocracy]",
    "before adopting [Aristocracy]",
    "if no Civilization has adopted [Ancestor Worship]",
    "after adopting [Ancestor Worship]",
    "before adopting [Ancestor Worship]",
    "before founding a Pantheon",
    "after founding a Pantheon",
    "before founding a religion",
    "after founding a religion",
    "before enhancing a religion",
    "after enhancing a religion",
    "after generating a Great Prophet",
    "if [Temple] is constructed",
    "if [Temple] is not constructed",
    "if [Temple] is constructed in all [non-[Puppeted]] cities",
    "if [Temple] is constructed in at least [2] of [All] cities",
    "if [Temple] is constructed by anybody",
    "if [Temple] is not constructed by anybody",
    "with [Iron]",
    "without [Iron]",
    "when above [100] [Gold]",
    "when above [2] [Iron]",
    "when above [5] [Happiness]",
    "when below [100] [Gold]",
    "when between [10] and [20] [Happiness]",
    "in this city",
    "in [Capital] cities",
    "in cities connected to the capital",
    "in cities with a [Temple]",
    "in cities without a [Temple]",
    "in cities with at least [3] [Population]",
    "in cities with [2] [Specialists]",
    "in cities with between [1] and [5] [Population]",
    "in cities with less than [3] [Unemployed]",
    "with a garrison",
    "for [Military] units",
    "when [Wounded]",
    "for units with [Drill I]",
    "for units without [Drill I]",
    "for units with [Set Up]",
    "for units without [Set Up]",
    "vs cities",
    "vs [Mounted] units",
    "vs [City]",
    "when fighting units from a Civilization with more Cities than you",
    "when attacking",
    "when defending",
    "when fighting in [Hill] tiles",
    "on foreign continents",
    "when adjacent to a [Great General] unit",
    "when above [50] HP",
    "when below [50] HP",
    "if it hasn't used other actions yet",
    "when stacked with a [Great General] unit",
    "when not stacked with a [Great General] unit",
    "with [1] to [2] neighboring [Hill] tiles",
    "in [Hill] tiles",
    "in tiles without [Hill]",
    "within [2] tiles of a [Mountain]",
    "in tiles adjacent to [River] tiles",
    "in tiles adjacent to [Hill] tiles",
    "in tiles not adjacent to [Hill] tiles",
    "on water maps",
    "when number of [Cities] is equal to [2]",
    "when number of [Cities] is different than [2]",
    "when number of [[Military] Units] is more than [1]",
    "when number of [Cities] is less than [3]",
    "when number of [Cities] is between [1] and [3]",
];

/// The three stat comparisons scaled by game speed, each under `[+4 Gold]`.
const SCALED: [&str; 3] = [
    "[+4 Gold] <when above [100] [Gold]> <(modified by game speed)>",
    "[+4 Gold] <when below [100] [Gold]> <(modified by game speed)>",
    "[+4 Gold] <when between [10] and [20] [Gold]> <(modified by game speed)>",
];

/// Countables with filters, each compiled in `[+3 Gold] <when number of [..] is more than [0]>`.
const COUNTED: &[&str] = &[
    "[Temple] Buildings",
    "[Puppeted] Cities",
    "Remaining [Major] Civilizations",
    "[Military] Units",
];

/// The kitchen sink with the `Eval Test` nation, and two uniques that differ only by their key.
fn rules() -> &'static Ruleset {
    static RULES: OnceLock<&'static Ruleset> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut uniques: Vec<String> = CONDS.iter().map(|c| format!("[+1 Gold] <{c}>")).collect();
        // The same text twice: two uniques whose draws must not go together (gate 3).
        uniques.push("[+2 Gold] <with [50]% chance>".into());
        uniques.push("[+2 Gold] <with [50]% chance>".into());
        uniques.extend(
            COUNTED.iter().map(|c| format!("[+3 Gold] <when number of [{c}] is more than [0]>")),
        );
        uniques.extend(SCALED.iter().map(|&u| u.to_owned()));
        let patch = json!({ "Eval Test": {
            "name": "Eval Test", "kind": "major", "leaderName": "Eval Leader", "adjective": "Eval",
            "preferredVictoryType": "Scientific", "cities": ["Evalton", "Testburg"],
            "uniques": uniques,
        }})
        .to_string();
        let mut patches = KITCHEN_SINK.to_vec();
        patches.push(("ruleset/nations.json", &patch));
        let files = overlay(&patches).expect("the patches apply");
        Ruleset::leak(&files_of(&files)).unwrap_or_else(|e| panic!("does not load:\n{e}"))
    })
}

fn id<I: Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("{name} is in the ruleset"))
}

/// The city-state type called `name`.
fn city_state_type(r: &Ruleset, name: &str) -> CityStateTypeId {
    r.city_state_types().iter().find(|(_, d)| &*d.name == name).map(|(i, _)| i).expect("a type")
}

/// The countable `[text]`, as the `Eval Test` compiled it.
fn counted(r: &Ruleset, text: &str) -> Countable {
    let want = format!("[+3 Gold] <when number of [{text}] is more than [0]>");
    let t = r.uniques();
    let n: NationId = id(r, "Eval Test");
    let u = r.nations()[n].uniques.ids().find(|&u| t.text_of(u) == want).expect("counted");
    match t.conds(t.get(u))[0].data {
        CondData::ConditionalCountableMoreThan(x) => x.count,
        other => panic!("{other:?} is not a count"),
    }
}

/// The `Eval Test` unique with the conditional `cond`.
fn eval_unique(r: &Ruleset, cond: &str) -> UniqueId {
    eval_text(r, &format!("[+1 Gold] <{cond}>"))
}

/// The `Eval Test` unique written `text`.
fn eval_text(r: &Ruleset, text: &str) -> UniqueId {
    let n: NationId = id(r, "Eval Test");
    r.nations()[n]
        .uniques
        .ids()
        .find(|&u| r.uniques().text_of(u) == text)
        .unwrap_or_else(|| panic!("{text} is on Eval Test"))
}

// ---- The mock world -------------------------------------------------------------------------

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn cid(n: u32) -> CityId {
    CityId::new(n).expect("a city id")
}

fn uid(n: u32) -> UnitId {
    UnitId::new(n).expect("a unit id")
}

#[derive(Clone, Debug)]
struct Civ {
    nation: NationId,
    kind: NationKind,
    human: bool,
    alive: bool,
    difficulty: DifficultyId,
    religion: Option<ReligionId>,
    golden_age: bool,
    happiness: i32,
    stocks: [f64; Stat::COUNT],
    resources: BTreeMap<ResourceId, i32>,
    era: EraId,
    techs: TechSet,
    researching: Option<TechId>,
    policies: PolicySet,
    branches: i32,
    beliefs: BeliefSet,
    progress: ReligionProgress,
    prophets: i32,
    capital: Option<CityId>,
    temporary: Vec<UniqueId>,
    city_states: Vec<(CityStateTypeId, CityStateBonus)>,
    founder_beliefs: Vec<BeliefId>,
}

#[derive(Clone, Debug, Default)]
struct Tile {
    terrains: TerrainSet,
    river: bool,
    owner: Option<PlayerId>,
    city: Option<CityId>,
    landmass: Option<u16>,
    resource: Option<ResourceId>,
    improvement: Option<ImprovementId>,
}

#[derive(Clone, Debug)]
struct City {
    owner: PlayerId,
    founder: PlayerId,
    tile: TileIdx,
    buildings: BuildingSet,
    local_resources: ResourceSet,
    puppet: bool,
    connected: bool,
    garrisoned: bool,
    religion: Option<ReligionId>,
    health: i32,
    food: f64,
    population: i32,
    specialists: BTreeMap<SpecialistId, i32>,
    unemployed: i32,
    followers: i32,
}

#[derive(Clone, Debug)]
struct Unit {
    owner: PlayerId,
    base: BaseUnitId,
    promotions: PromotionSet,
    tile: TileIdx,
    health: i32,
    used: bool,
    set_up: bool,
}

/// A world of plain facts, answered from tables, with its unique indexes built by the engine's
/// own index functions ([`World::reindex`]).
#[derive(Clone)]
struct World {
    rules: &'static Ruleset,
    grid: HexGrid,
    seed: u64,
    turn: i32,
    speed: SpeedId,
    starting_era: EraId,
    game_difficulty: DifficultyId,
    victories_off: BTreeSet<VictoryId>,
    religion: bool,
    espionage: bool,
    nukes: bool,
    civs: Vec<Civ>,
    war: BTreeSet<(PlayerId, PlayerId)>,
    met: BTreeSet<(PlayerId, PlayerId)>,
    tiles: Vec<Tile>,
    cities: BTreeMap<CityId, City>,
    units: BTreeMap<UnitId, Unit>,
    /// Per religion: its follower beliefs.
    religions: Vec<Vec<BeliefId>>,
    civ_full: Vec<Csr>,
    civ_bare: Vec<Csr>,
    local: BTreeMap<CityId, Csr>,
    follower: Vec<Csr>,
    unit_index: BTreeMap<UnitId, Csr>,
}

fn pair(a: PlayerId, b: PlayerId) -> (PlayerId, PlayerId) {
    if a <= b { (a, b) } else { (b, a) }
}

impl World {
    /// Four civilizations: the Eval Test (human), the Kitchen Sink (a bot), a city-state and the
    /// barbarians, on an 8 by 8 map of grassland. The Eval Test has cities 1 (its capital, at
    /// (2, 2)) and 2 (at (5, 2)), a Warrior (unit 1) in city 1 and a Great General (unit 2) at
    /// (3, 3); the Kitchen Sink has city 3, its capital at (5, 5), and a Horseman (unit 3) in it.
    fn new() -> Self {
        let r = rules();
        let grid = HexGrid::new(8, 8, false, false).expect("a grid");
        let grass: TerrainId = id(r, "Grassland");
        let tile =
            Tile { terrains: [grass].into_iter().collect(), landmass: Some(0), ..Tile::default() };
        let civ = |nation: &str, kind| Civ {
            nation: id(r, nation),
            kind,
            human: false,
            alive: true,
            difficulty: id(r, "Prince"),
            religion: None,
            golden_age: false,
            happiness: 5,
            stocks: [0.0; Stat::COUNT],
            resources: BTreeMap::new(),
            era: id(r, "Ancient era"),
            techs: TechSet::new(),
            researching: None,
            policies: PolicySet::new(),
            branches: 0,
            beliefs: BeliefSet::new(),
            progress: ReligionProgress::None,
            prophets: 0,
            capital: None,
            temporary: Vec::new(),
            city_states: Vec::new(),
            founder_beliefs: Vec::new(),
        };
        let mut w = Self {
            rules: r,
            seed: 42,
            turn: 12,
            speed: id(r, "Standard"),
            starting_era: id(r, "Ancient era"),
            game_difficulty: id(r, "Prince"),
            victories_off: BTreeSet::new(),
            religion: true,
            espionage: true,
            nukes: true,
            civs: vec![
                Civ { human: true, capital: Some(cid(1)), ..civ("Eval Test", NationKind::Major) },
                Civ { capital: Some(cid(3)), ..civ("Kitchen Sink", NationKind::Major) },
                civ("Almaty", NationKind::CityState),
                civ("Barbarians", NationKind::Barbarian),
            ],
            war: BTreeSet::new(),
            met: [pair(P0, P1), pair(P0, P2)].into_iter().collect(),
            tiles: vec![tile; grid.size() as usize],
            grid,
            cities: BTreeMap::new(),
            units: BTreeMap::new(),
            religions: Vec::new(),
            civ_full: Vec::new(),
            civ_bare: Vec::new(),
            local: BTreeMap::new(),
            follower: Vec::new(),
            unit_index: BTreeMap::new(),
        };
        for (c, owner, x, y) in [(1, P0, 2, 2), (2, P0, 5, 2), (3, P1, 5, 5)] {
            let t = w.grid.idx(x, y).expect("on the map");
            w.found(cid(c), owner, t);
        }
        let warrior = w.unit_of(P0, "Warrior", w.city(1).tile);
        w.units.insert(uid(1), warrior);
        let general = w.unit_of(P0, "Great General", w.grid.idx(3, 3).expect("on the map"));
        w.units.insert(uid(2), general);
        let horseman = w.unit_of(P1, "Horseman", w.city(3).tile);
        w.units.insert(uid(3), horseman);
        w.reindex();
        w
    }

    fn found(&mut self, c: CityId, owner: PlayerId, t: TileIdx) {
        self.cities.insert(
            c,
            City {
                owner,
                founder: owner,
                tile: t,
                buildings: BuildingSet::new(),
                local_resources: ResourceSet::new(),
                puppet: false,
                connected: false,
                garrisoned: false,
                religion: None,
                health: 200,
                food: 0.0,
                population: 1,
                specialists: BTreeMap::new(),
                unemployed: 0,
                followers: 0,
            },
        );
        let tile = &mut self.tiles[t.0 as usize];
        tile.owner = Some(owner);
        tile.city = Some(c);
    }

    fn unit_of(&self, owner: PlayerId, base: &str, tile: TileIdx) -> Unit {
        Unit {
            owner,
            base: id(self.rules, base),
            promotions: PromotionSet::new(),
            tile,
            health: 100,
            used: false,
            set_up: false,
        }
    }

    fn civ(&mut self, p: PlayerId) -> &mut Civ {
        &mut self.civs[usize::from(p.0)]
    }

    fn city(&self, c: u32) -> &City {
        &self.cities[&cid(c)]
    }

    fn city_mut(&mut self, c: u32) -> &mut City {
        self.cities.get_mut(&cid(c)).expect("a city")
    }

    fn unit_mut(&mut self, u: u32) -> &mut Unit {
        self.units.get_mut(&uid(u)).expect("a unit")
    }

    fn tile_mut(&mut self, t: TileIdx) -> &mut Tile {
        &mut self.tiles[t.0 as usize]
    }

    fn add_terrain(&mut self, t: TileIdx, name: &str) {
        let terrain: TerrainId = id(self.rules, name);
        self.tile_mut(t).terrains.insert(terrain);
    }

    fn give_building(&mut self, c: u32, name: &str) {
        let b: BuildingId = id(self.rules, name);
        self.city_mut(c).buildings.insert(b);
    }

    /// What each civilization's sources are, as `game::derive::civ` will gather them.
    fn sources(&self, p: PlayerId, resources: bool) -> CivSources {
        let c = &self.civs[usize::from(p.0)];
        let mut buildings: BTreeMap<BuildingId, u16> = BTreeMap::new();
        for city in self.cities.values().filter(|x| x.owner == p) {
            for b in city.buildings.iter() {
                *buildings.entry(b).or_insert(0) += 1;
            }
        }
        CivSources {
            nation: c.nation,
            buildings: buildings.into_iter().collect(),
            policies: c.policies,
            techs: c.techs,
            temporary: c.temporary.clone(),
            era: c.era,
            city_states: c.city_states.clone(),
            founder_beliefs: c.founder_beliefs.clone(),
            resources: if resources {
                c.resources.iter().filter(|&(_, &n)| n > 0).map(|(&r, _)| r).collect()
            } else {
                ResourceSet::new()
            },
        }
    }

    /// Builds every index from the world's state.
    fn reindex(&mut self) {
        let r = self.rules;
        let players = (0..self.civs.len()).map(|i| PlayerId(u8::try_from(i).expect("few")));
        self.civ_full =
            players.clone().map(|p| CivIndex::build(r, &self.sources(p, true))).collect();
        self.civ_bare = players.map(|p| CivIndex::build(r, &self.sources(p, false))).collect();
        self.local = self
            .cities
            .iter()
            .map(|(&c, x)| (c, index::city_local(r, &x.buildings, &x.local_resources)))
            .collect();
        self.follower = self.religions.iter().map(|b| index::follower(r, b)).collect();
        self.unit_index = self
            .units
            .iter()
            .map(|(&u, x)| (u, index::unit_profile(r, x.base, &x.promotions)))
            .collect();
    }

    fn tile(&self, t: TileIdx) -> &Tile {
        &self.tiles[t.0 as usize]
    }

    fn c(&self, p: PlayerId) -> &Civ {
        &self.civs[usize::from(p.0)]
    }
}

impl TileFacts for World {
    fn tile_terrains(&self, t: TileIdx) -> TerrainSet {
        self.tile(t).terrains
    }
    fn tile_river(&self, t: TileIdx) -> bool {
        self.tile(t).river
    }
    fn tile_fresh_water(&self, t: TileIdx) -> bool {
        self.tile(t).river
    }
    fn tile_next_to_coast(&self, _: TileIdx) -> bool {
        false
    }
}

impl FilterFacts for World {
    fn civ_nation(&self, p: PlayerId) -> NationId {
        self.c(p).nation
    }
    fn civ_kind(&self, p: PlayerId) -> NationKind {
        self.c(p).kind
    }
    fn civ_is_human(&self, p: PlayerId) -> bool {
        self.c(p).human
    }
    fn civ_religion(&self, p: PlayerId) -> Option<ReligionId> {
        self.c(p).religion
    }
    fn at_war(&self, a: PlayerId, b: PlayerId) -> bool {
        self.war.contains(&pair(a, b))
    }
    fn has_met(&self, a: PlayerId, b: PlayerId) -> bool {
        self.met.contains(&pair(a, b))
    }
    fn is_friend(&self, _: PlayerId, _: PlayerId) -> bool {
        false
    }
    fn has_open_borders(&self, _: PlayerId, _: PlayerId) -> bool {
        false
    }
    fn tile_owner(&self, t: TileIdx) -> Option<PlayerId> {
        self.tile(t).owner
    }
    fn tile_friendly_to(&self, t: TileIdx, p: PlayerId) -> bool {
        self.tile(t).owner == Some(p)
    }
    fn tile_resource(&self, t: TileIdx) -> Option<ResourceId> {
        self.tile(t).resource
    }
    fn resource_visible(&self, _: PlayerId, _: ResourceId) -> bool {
        true
    }
    fn tile_improvement(&self, t: TileIdx) -> Option<ImprovementId> {
        self.tile(t).improvement
    }
    fn tile_route(&self, _: TileIdx) -> Option<ImprovementId> {
        None
    }
    fn tile_pillaged(&self, _: TileIdx) -> bool {
        false
    }
    fn tile_worked(&self, _: TileIdx) -> bool {
        false
    }
    fn unit_owner(&self, u: UnitId) -> PlayerId {
        self.units[&u].owner
    }
    fn unit_base(&self, u: UnitId) -> BaseUnitId {
        self.units[&u].base
    }
    fn unit_promotions(&self, u: UnitId) -> PromotionSet {
        self.units[&u].promotions
    }
    fn unit_wounded(&self, u: UnitId) -> bool {
        self.units[&u].health < 100
    }
    fn unit_embarked(&self, _: UnitId) -> bool {
        false
    }
    fn unit_set_up(&self, u: UnitId) -> bool {
        self.units[&u].set_up
    }
    fn city_owner(&self, c: CityId) -> PlayerId {
        self.cities[&c].owner
    }
    fn city_founder(&self, c: CityId) -> PlayerId {
        self.cities[&c].founder
    }
    fn city_buildings(&self, c: CityId) -> BuildingSet {
        self.cities[&c].buildings
    }
    fn city_is_capital(&self, c: CityId) -> bool {
        self.c(self.cities[&c].owner).capital == Some(c)
    }
    fn city_coastal(&self, _: CityId) -> bool {
        false
    }
    fn city_annex_unhappiness(&self, c: CityId) -> bool {
        let x = &self.cities[&c];
        x.founder != x.owner && !x.puppet
    }
    fn city_puppet(&self, c: CityId) -> bool {
        self.cities[&c].puppet
    }
    fn city_connected_to_capital(&self, c: CityId) -> bool {
        self.cities[&c].connected
    }
    fn city_garrisoned(&self, c: CityId) -> bool {
        self.cities[&c].garrisoned
    }
    fn city_resisting(&self, _: CityId) -> bool {
        false
    }
    fn city_razing(&self, _: CityId) -> bool {
        false
    }
    fn city_holy(&self, _: CityId) -> bool {
        false
    }
    fn city_majority_religion(&self, c: CityId) -> Option<ReligionId> {
        self.cities[&c].religion
    }
    fn religion_is_major(&self, _: ReligionId) -> bool {
        true
    }
    fn religion_is_enhanced(&self, _: ReligionId) -> bool {
        false
    }
}

impl EvalWorld for World {
    fn rules(&self) -> &Ruleset {
        self.rules
    }
    fn seed(&self) -> u64 {
        self.seed
    }
    fn grid(&self) -> &HexGrid {
        &self.grid
    }
    fn turn(&self) -> i32 {
        self.turn
    }
    fn speed(&self) -> SpeedId {
        self.speed
    }
    fn starting_era(&self) -> EraId {
        self.starting_era
    }
    fn difficulty(&self, p: Option<PlayerId>) -> DifficultyId {
        p.map_or(self.game_difficulty, |p| self.c(p).difficulty)
    }
    fn victory_enabled(&self, v: VictoryId) -> bool {
        !self.victories_off.contains(&v)
    }
    fn religion_enabled(&self) -> bool {
        self.religion
    }
    fn espionage_enabled(&self) -> bool {
        self.espionage
    }
    fn nukes_enabled(&self) -> bool {
        self.nukes
    }
    fn civs(&self) -> impl Iterator<Item = PlayerId> + '_ {
        self.civs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.alive)
            .map(|(i, _)| PlayerId(u8::try_from(i).expect("few")))
    }
    fn cities(&self) -> impl Iterator<Item = CityId> + '_ {
        self.cities.keys().copied()
    }
    fn civ_at_war(&self, p: PlayerId) -> bool {
        self.civs()
            .any(|q| q != p && self.civ_kind(q) != NationKind::Barbarian && self.at_war(p, q))
    }
    fn civ_golden_age(&self, p: PlayerId) -> bool {
        self.c(p).golden_age
    }
    fn civ_happiness(&self, p: PlayerId) -> i32 {
        self.c(p).happiness
    }
    fn civ_stock(&self, p: PlayerId, s: Stat) -> f64 {
        self.c(p).stocks[s.index()]
    }
    fn civ_resource(&self, p: PlayerId, r: ResourceId) -> i32 {
        self.c(p).resources.get(&r).copied().unwrap_or(0)
    }
    fn civ_era(&self, p: PlayerId) -> EraId {
        self.c(p).era
    }
    fn civ_techs(&self, p: PlayerId) -> TechSet {
        self.c(p).techs
    }
    fn civ_researching(&self, p: PlayerId) -> Option<TechId> {
        self.c(p).researching
    }
    fn civ_policies(&self, p: PlayerId) -> PolicySet {
        self.c(p).policies
    }
    fn civ_completed_branches(&self, p: PlayerId) -> i32 {
        self.c(p).branches
    }
    fn civ_beliefs(&self, p: PlayerId) -> BeliefSet {
        self.c(p).beliefs
    }
    fn civ_religion_progress(&self, p: PlayerId) -> ReligionProgress {
        self.c(p).progress
    }
    fn civ_prophets_earned(&self, p: PlayerId) -> i32 {
        self.c(p).prophets
    }
    fn civ_capital(&self, p: PlayerId) -> Option<CityId> {
        self.c(p).capital
    }
    fn civ_cities(&self, p: PlayerId) -> impl Iterator<Item = CityId> + '_ {
        self.cities.iter().filter(move |(_, c)| c.owner == p).map(|(&c, _)| c)
    }
    fn civ_units(&self, p: PlayerId) -> impl Iterator<Item = UnitId> + '_ {
        self.units.iter().filter(move |(_, u)| u.owner == p).map(|(&u, _)| u)
    }
    fn city_tile(&self, c: CityId) -> TileIdx {
        self.cities[&c].tile
    }
    fn city_health(&self, c: CityId) -> i32 {
        self.cities[&c].health
    }
    fn city_food(&self, c: CityId) -> f64 {
        self.cities[&c].food
    }
    fn city_population(&self, c: CityId) -> i32 {
        self.cities[&c].population
    }
    fn city_specialists(&self, c: CityId, s: Option<SpecialistId>) -> i32 {
        let x = &self.cities[&c];
        match s {
            Some(s) => x.specialists.get(&s).copied().unwrap_or(0),
            None => x.specialists.values().sum(),
        }
    }
    fn city_unemployed(&self, c: CityId) -> i32 {
        self.cities[&c].unemployed
    }
    fn city_majority_followers(&self, c: CityId) -> i32 {
        self.cities[&c].followers
    }
    fn tile_city(&self, t: TileIdx) -> Option<CityId> {
        self.tile(t).city
    }
    fn tile_landmass(&self, t: TileIdx) -> Option<u16> {
        self.tile(t).landmass
    }
    fn units_at(&self, t: TileIdx) -> impl Iterator<Item = UnitId> + '_ {
        self.units.iter().filter(move |(_, u)| u.tile == t).map(|(&u, _)| u)
    }
    fn unit_tile(&self, u: UnitId) -> TileIdx {
        self.units[&u].tile
    }
    fn unit_health(&self, u: UnitId) -> i32 {
        self.units[&u].health
    }
    fn unit_used_actions(&self, u: UnitId) -> bool {
        self.units[&u].used
    }
    fn civ_index(&self, p: PlayerId, layer: IndexLayer) -> IndexRef<'_> {
        let all = match layer {
            IndexLayer::Full => &self.civ_full,
            IndexLayer::NoResources => &self.civ_bare,
        };
        IndexRef::Plain(&all[usize::from(p.0)])
    }
    fn city_local(&self, c: CityId) -> IndexRef<'_> {
        IndexRef::Plain(&self.local[&c])
    }
    fn follower(&self, r: ReligionId) -> IndexRef<'_> {
        IndexRef::Plain(&self.follower[usize::from(r.0)])
    }
    fn unit_index(&self, u: UnitId) -> IndexRef<'_> {
        IndexRef::Plain(&self.unit_index[&u])
    }
}

// ---- Gate 1: every conditional, true and false ------------------------------------------------

/// A fight of the Eval Test's Warrior (unit 1) against `their`.
fn fight(w: &World, their: Combatant, action: CombatAction, at: TileIdx) -> Ctx {
    Ctx::fight(
        w,
        CombatCtx {
            our: Combatant::Unit(uid(1)),
            their: Some(their),
            attacked_tile: Some(at),
            action: Some(action),
        },
    )
}

/// Sets the civilization's amount of a stat or resource, as a comparison reads it.
fn set_amount(w: &mut World, p: PlayerId, what: StatOrResource, v: f64) {
    #[allow(clippy::cast_possible_truncation, reason = "small whole test amounts")]
    let whole = v as i32;
    match what {
        StatOrResource::Resource(res) => {
            w.civ(p).resources.insert(res, whole);
        }
        StatOrResource::Stat(Stat::Happiness) => w.civ(p).happiness = whole,
        StatOrResource::Stat(s) => w.civ(p).stocks[s.index()] = v,
    }
}

/// The step past a bound that a comparison of `what` tells apart: half a unit of a stock, one of
/// a count.
fn step(what: StatOrResource) -> f64 {
    match what {
        StatOrResource::Stat(Stat::Happiness) | StatOrResource::Resource(_) => 1.0,
        StatOrResource::Stat(_) => 0.5,
    }
}

/// Gives `p` the policy or the belief.
fn adopt(w: &mut World, p: PlayerId, what: PolicyOrBelief) {
    match what {
        PolicyOrBelief::Policy(x) => {
            w.civ(p).policies.insert(x);
        }
        PolicyOrBelief::Belief(x) => {
            w.civ(p).beliefs.insert(x);
        }
    }
}

/// `a` for the true case, `b` for the false one.
fn pick<T>(yes: bool, a: T, b: T) -> T {
    if yes { a } else { b }
}

/// Sets `w` so that `c` holds (`yes`) or fails (`!yes`), and gives the context to ask it in;
/// `None` when it can never have that answer. One arm per variant, and no wildcard.
#[allow(clippy::too_many_lines, reason = "one arm per conditional")]
fn case(w: &mut World, c: &CondData, yes: bool) -> Option<Ctx> {
    use CondData as C;
    let r = w.rules;
    let civ = Ctx::civ(P0);
    let city1 = Ctx::city(w, cid(1));
    let unit1 = Ctx::unit(w, uid(1));
    let at = w.city(1).tile;
    let tile = Ctx::tile(Some(P0), at);
    let far = w.grid.idx(7, 7).expect("on the map");
    let neighbours: Vec<TileIdx> = w.grid.neighbors(at).collect();
    Some(match *c {
        C::ConditionalEveryTurns(_) => {
            w.turn = pick(yes, 12, 13);
            civ
        }
        C::ConditionalBeforeTurns(_) => {
            w.turn = pick(yes, 5, 10);
            civ
        }
        C::ConditionalAfterTurns(_) => {
            w.turn = pick(yes, 10, 9);
            civ
        }
        C::ConditionalSpeed(_) => {
            w.speed = id(r, pick(yes, "Quick", "Standard"));
            civ
        }
        C::ConditionalDifficulty(_) => {
            w.civ(P0).difficulty = id(r, pick(yes, "Prince", "King"));
            civ
        }
        C::ConditionalDifficultyOrHigher(_) => {
            w.civ(P0).difficulty = id(r, pick(yes, "King", "Chieftain"));
            civ
        }
        C::ConditionalDifficultyOrLower(_) => {
            w.civ(P0).difficulty = id(r, pick(yes, "Chieftain", "King"));
            civ
        }
        C::ConditionalVictoryEnabled(x) => {
            if !yes {
                w.victories_off.insert(x.victory);
            }
            civ
        }
        C::ConditionalVictoryDisabled(x) => {
            if yes {
                w.victories_off.insert(x.victory);
            }
            civ
        }
        C::ConditionalReligionEnabled => {
            w.religion = yes;
            civ
        }
        C::ConditionalReligionDisabled => {
            w.religion = !yes;
            civ
        }
        C::ConditionalEspionageEnabled => {
            w.espionage = yes;
            civ
        }
        C::ConditionalEspionageDisabled => {
            w.espionage = !yes;
            civ
        }
        C::ConditionalNuclearWeaponsEnabled => {
            w.nukes = yes;
            civ
        }
        C::ConditionalNuclearWeaponsDisabled => {
            w.nukes = !yes;
            civ
        }
        // Certain draws: 100% always, 0% never.
        C::ConditionalChance(x) => {
            if (x.percent == 100) != yes {
                return None;
            }
            unit1
        }
        C::ConditionalTutorialsEnabled | C::ConditionalTutorialCompleted(_) => {
            if yes {
                return None;
            }
            civ
        }
        C::ConditionalCivFilter(x) => match r.uniques().civ_filter(x.civs) {
            "Major" => Ctx::civ(pick(yes, P0, P2)),
            "Human player" => Ctx::civ(pick(yes, P0, P1)),
            // Seen by the civilization itself, which is never hostile to itself.
            "Hostile" => {
                if yes {
                    return None;
                }
                w.war.insert(pair(P0, P1));
                civ
            }
            other => panic!("no case for [{other}] Civilizations"),
        },
        C::ConditionalWar => {
            // War with the barbarians does not count (`game.py:673-675`).
            w.war.insert(pair(P0, pick(yes, P1, P3)));
            civ
        }
        C::ConditionalNotWar => {
            w.war.insert(pair(P0, pick(yes, P3, P1)));
            civ
        }
        C::ConditionalGoldenAge => {
            w.civ(P0).golden_age = yes;
            civ
        }
        C::ConditionalNotGoldenAge => {
            w.civ(P0).golden_age = !yes;
            civ
        }
        C::ConditionalHappy => {
            w.civ(P0).happiness = pick(yes, 0, -1);
            civ
        }
        C::ConditionalDuringEra(_) => {
            w.civ(P0).era = id(r, pick(yes, "Medieval era", "Ancient era"));
            civ
        }
        C::ConditionalBeforeEra(_) => {
            w.civ(P0).era = id(r, pick(yes, "Classical era", "Medieval era"));
            civ
        }
        C::ConditionalStartingFromEra(_) => {
            w.civ(P0).era = id(r, pick(yes, "Renaissance era", "Classical era"));
            civ
        }
        C::ConditionalIfStartingInEra(_) => {
            w.starting_era = id(r, pick(yes, "Ancient era", "Classical era"));
            civ
        }
        C::ConditionalTech(_) => {
            let t: TechId = id(r, pick(yes, "Writing", "Pottery"));
            w.civ(P0).techs.insert(t);
            civ
        }
        C::ConditionalNoTech(_) => {
            let t: TechId = id(r, pick(yes, "Pottery", "Writing"));
            w.civ(P0).techs.insert(t);
            civ
        }
        C::ConditionalWhileResearching(_) => {
            w.civ(P0).researching = Some(id(r, pick(yes, "Writing", "Pottery")));
            civ
        }
        // A policy or a belief alike (Python's dead key read policies only).
        C::ConditionalNoCivAdopted(x) => {
            // A city-state's adoption does not count; another major's does.
            adopt(w, pick(yes, P2, P1), x.adopted);
            civ
        }
        C::ConditionalAfterPolicyOrBelief(x) => {
            adopt(w, pick(yes, P0, P1), x.adopted);
            civ
        }
        C::ConditionalBeforePolicyOrBelief(x) => {
            adopt(w, pick(yes, P1, P0), x.adopted);
            civ
        }
        C::ConditionalBeforePantheon => {
            w.civ(P0).progress = pick(yes, ReligionProgress::None, ReligionProgress::Pantheon);
            civ
        }
        C::ConditionalAfterPantheon => {
            w.civ(P0).progress = pick(yes, ReligionProgress::Founding, ReligionProgress::None);
            civ
        }
        C::ConditionalBeforeReligion => {
            w.civ(P0).progress = pick(yes, ReligionProgress::Founding, ReligionProgress::Religion);
            civ
        }
        C::ConditionalAfterReligion => {
            w.civ(P0).progress = pick(yes, ReligionProgress::Enhancing, ReligionProgress::Founding);
            civ
        }
        C::ConditionalBeforeEnhancingReligion => {
            w.civ(P0).progress = pick(yes, ReligionProgress::Enhancing, ReligionProgress::Enhanced);
            civ
        }
        C::ConditionalAfterEnhancingReligion => {
            w.civ(P0).progress = pick(yes, ReligionProgress::Enhanced, ReligionProgress::Enhancing);
            civ
        }
        C::ConditionalAfterGeneratingGreatProphet => {
            w.civ(P0).prophets = pick(yes, 1, 0);
            civ
        }
        C::ConditionalBuildingBuilt(_) => {
            w.give_building(pick(yes, 2, 3), "Temple");
            civ
        }
        C::ConditionalBuildingNotBuilt(_) => {
            w.give_building(pick(yes, 3, 2), "Temple");
            civ
        }
        C::ConditionalBuildingBuiltAll(_) => {
            // Only the cities the filter selects count: city 2 is a puppet in the true case.
            w.give_building(1, "Temple");
            w.city_mut(2).puppet = yes;
            civ
        }
        C::ConditionalBuildingBuiltAmount(_) => {
            w.give_building(1, "Temple");
            w.give_building(pick(yes, 2, 3), "Temple");
            civ
        }
        C::ConditionalBuildingBuiltByAnybody(_) => {
            if yes {
                w.give_building(3, "Temple");
            }
            civ
        }
        C::ConditionalBuildingNotBuiltByAnybody(_) => {
            if !yes {
                w.give_building(3, "Temple");
            }
            civ
        }
        C::ConditionalWithResource(x) => {
            w.civ(P0).resources.insert(x.resource, pick(yes, 1, 0));
            civ
        }
        C::ConditionalWithoutResource(x) => {
            w.civ(P0).resources.insert(x.resource, pick(yes, 0, 2));
            civ
        }
        // Just past the bound, and on it: the comparisons are strict.
        C::ConditionalWhenAboveAmountStatResource(x) => {
            let at = f64::from(x.amount);
            set_amount(w, P0, x.what, pick(yes, at + step(x.what), at));
            civ
        }
        C::ConditionalWhenBelowAmountStatResource(x) => {
            let at = f64::from(x.amount);
            set_amount(w, P0, x.what, pick(yes, at - step(x.what), at));
            civ
        }
        // On the upper bound, and just past it: the bounds are inclusive.
        C::ConditionalWhenBetweenStatResource(x) => {
            let at = f64::from(x.max);
            set_amount(w, P0, x.what, pick(yes, at, at + step(x.what)));
            civ
        }
        C::ConditionalInThisCity => pick(yes, city1, civ),
        C::ConditionalCityFilter(_) => pick(yes, city1, Ctx::city(w, cid(2))),
        C::ConditionalCityConnected => {
            w.city_mut(1).connected = yes;
            city1
        }
        C::ConditionalCityWithBuilding(_) => {
            if yes {
                w.give_building(1, "Temple");
            }
            city1
        }
        C::ConditionalCityWithoutBuilding(_) => {
            if !yes {
                w.give_building(1, "Temple");
            }
            city1
        }
        C::ConditionalPopulationFilter(_) => {
            w.city_mut(1).population = pick(yes, 3, 2);
            city1
        }
        C::ConditionalExactPopulationFilter(_) => {
            let s: SpecialistId = r.specialists().ids().next().expect("a specialist");
            w.city_mut(1).specialists.insert(s, pick(yes, 2, 1));
            city1
        }
        C::ConditionalBetweenPopulationFilter(_) => {
            w.city_mut(1).population = pick(yes, 5, 6);
            city1
        }
        C::ConditionalBelowPopulationFilter(_) => {
            w.city_mut(1).unemployed = pick(yes, 2, 3);
            city1
        }
        C::ConditionalWhenGarrisoned => {
            w.city_mut(1).garrisoned = yes;
            city1
        }
        C::ConditionalOurUnit(_) => pick(yes, unit1, Ctx::unit(w, uid(2))),
        C::ConditionalOurUnitOnUnit(_) => {
            w.unit_mut(1).health = pick(yes, 50, 100);
            unit1
        }
        C::ConditionalUnitWithPromotion(x) => {
            carry(w, x.promotion, yes);
            unit1
        }
        C::ConditionalUnitWithoutPromotion(x) => {
            carry(w, x.promotion, !yes);
            unit1
        }
        C::ConditionalVsCity => fight(
            w,
            pick(yes, Combatant::City(cid(3)), Combatant::Unit(uid(3))),
            CombatAction::Attack,
            at,
        ),
        C::ConditionalVsUnits(_) => fight(
            w,
            pick(yes, Combatant::Unit(uid(3)), Combatant::City(cid(3))),
            CombatAction::Attack,
            at,
        ),
        C::ConditionalVsCombatant(_) => fight(
            w,
            pick(yes, Combatant::City(cid(3)), Combatant::Unit(uid(3))),
            CombatAction::Attack,
            at,
        ),
        C::ConditionalVsLargerCiv => {
            if yes {
                w.found(cid(4), P1, w.grid.idx(6, 6).expect("on the map"));
                w.found(cid(5), P1, w.grid.idx(7, 6).expect("on the map"));
            }
            fight(w, Combatant::Unit(uid(3)), CombatAction::Attack, at)
        }
        C::ConditionalAttacking => fight(
            w,
            Combatant::Unit(uid(3)),
            pick(yes, CombatAction::Attack, CombatAction::Defend),
            at,
        ),
        C::ConditionalDefending => fight(
            w,
            Combatant::Unit(uid(3)),
            pick(yes, CombatAction::Defend, CombatAction::Attack),
            at,
        ),
        C::ConditionalFightingInTiles(_) => {
            let target = w.city(3).tile;
            if yes {
                w.add_terrain(target, "Hill");
            } else {
                // The unit's own tile is not the one under attack.
                w.add_terrain(at, "Hill");
            }
            fight(w, Combatant::City(cid(3)), CombatAction::Attack, target)
        }
        C::ConditionalForeignContinent => {
            w.tile_mut(far).landmass = Some(pick(yes, 1, 0));
            Ctx::tile(Some(P0), far)
        }
        C::ConditionalAdjacentUnit(_) => {
            w.unit_mut(2).tile = pick(yes, neighbours[0], far);
            unit1
        }
        C::ConditionalAboveHP(_) => {
            w.unit_mut(1).health = pick(yes, 60, 50);
            unit1
        }
        C::ConditionalBelowHP(_) => {
            w.unit_mut(1).health = pick(yes, 40, 50);
            unit1
        }
        C::ConditionalHasNotUsedOtherActions => {
            w.unit_mut(1).used = !yes;
            unit1
        }
        C::ConditionalStackedWithUnit(_) => {
            w.unit_mut(2).tile = pick(yes, at, far);
            unit1
        }
        C::ConditionalNotStackedWithUnit(_) => {
            w.unit_mut(2).tile = pick(yes, far, at);
            unit1
        }
        C::ConditionalNeighborTiles(_) => {
            let hills = pick(yes, 1, 3);
            for &n in &neighbours[..hills] {
                w.add_terrain(n, "Hill");
            }
            tile
        }
        C::ConditionalInTiles(_) => {
            if yes {
                w.add_terrain(at, "Hill");
            }
            tile
        }
        C::ConditionalInTilesNot(_) => {
            if !yes {
                w.add_terrain(at, "Hill");
            }
            tile
        }
        C::ConditionalNearTiles(_) => {
            let two = w.grid.ring(at, pick(yes, 2, 3))[0];
            w.add_terrain(two, "Mountain");
            tile
        }
        C::ConditionalAdjacentTo(x) => {
            if r.uniques().tile_filter(x.tiles) == "River" {
                // The tile's own river, as `_adjacent_to` read it.
                w.tile_mut(at).river = yes;
                w.tile_mut(neighbours[0]).river = !yes;
            } else if yes {
                w.add_terrain(neighbours[0], "Hill");
            } else {
                w.add_terrain(at, "Hill");
            }
            tile
        }
        C::ConditionalNotAdjacentTo(_) => {
            if !yes {
                w.add_terrain(neighbours[0], "Hill");
            }
            tile
        }
        C::ConditionalOnWaterMaps | C::ConditionalInRegionOfType(_) => {
            if yes {
                return None;
            }
            tile
        }
        C::ConditionalInRegionExceptOfType(_) => {
            if !yes {
                return None;
            }
            tile
        }
        C::ConditionalCountableEqualTo(_) => {
            if !yes {
                w.city_mut(2).owner = P1;
            }
            civ
        }
        C::ConditionalCountableDifferentThan(_) => {
            if yes {
                w.city_mut(2).owner = P1;
            }
            civ
        }
        C::ConditionalCountableMoreThan(_) => {
            if yes {
                let extra = w.unit_of(P0, "Warrior", far);
                w.units.insert(uid(9), extra);
            }
            civ
        }
        C::ConditionalCountableLessThan(_) => {
            if !yes {
                w.city_mut(3).owner = P0;
            }
            civ
        }
        C::ConditionalCountableBetween(_) => {
            if !yes {
                w.city_mut(1).owner = P1;
                w.city_mut(2).owner = P1;
            }
            civ
        }
    })
}

/// Gives the Warrior (unit 1) the promotion or the status, or not.
fn carry(w: &mut World, what: PromotionOrStatus, on: bool) {
    let unit = w.unit_mut(1);
    match what {
        PromotionOrStatus::Promotion(p) if on => {
            unit.promotions.insert(p);
        }
        PromotionOrStatus::Promotion(_) => {}
        PromotionOrStatus::SetUp => unit.set_up = on,
    }
}

/// Every conditional of the ruleset once, with a unique that carries it: the Eval Test's, and the
/// map-generation ones from the shipped ruleset's uniques.
fn every_cond() -> Vec<(UniqueId, usize)> {
    let r = rules();
    let t = r.uniques();
    let mut out: Vec<(UniqueId, usize)> = CONDS.iter().map(|c| (eval_unique(r, c), 0)).collect();
    let region = |ty: UniqueType| {
        t.iter()
            .find_map(|(u, x)| t.conds(x).iter().position(|c| c.data.ty() == ty).map(|i| (u, i)))
            .expect("a map-generation unique with the conditional")
    };
    out.push(region(UniqueType::ConditionalInRegionOfType));
    out.push(region(UniqueType::ConditionalInRegionExceptOfType));
    out
}

#[test]
fn every_conditional_holds_and_fails_where_it_should() {
    let r = rules();
    let t = r.uniques();
    let mut covered = BTreeSet::new();
    let mut problems = Vec::new();
    for (u, i) in every_cond() {
        let c = t.conds(t.get(u))[i];
        covered.insert(c.data.ty());
        for yes in [true, false] {
            let mut w = World::new();
            let Some(ctx) = case(&mut w, &c.data, yes) else { continue };
            w.reindex();
            if holds(&c, u, &ctx, &w) != yes {
                problems.push(format!(
                    "<{}> should {}",
                    t.text(c.text),
                    if yes { "hold" } else { "fail" }
                ));
            }
            // A unique of one conditional applies exactly when it holds.
            if t.conds(t.get(u)).len() == 1 {
                assert_eq!(applies(u, &ctx, &w), yes, "{}", t.text_of(u));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    let every: BTreeSet<UniqueType> = cond::types().collect();
    assert_eq!(covered, every, "every conditional type has a case");
    assert_eq!(every.len(), 94);
}

#[test]
fn what_each_conditional_reads() {
    use CondDeps as D;
    let r = rules();
    let t = r.uniques();
    let city = D::CITY.union(D::TILE);
    let around = D::TILE.union(D::MAP);
    let table: &[(&str, CondDeps)] = &[
        ("every [3] turns", D::TURN),
        ("before turn number [10]", D::TURN),
        ("after turn number [10]", D::TURN),
        ("on [Quick] game speed", D::CONFIG),
        ("on [Prince] difficulty", D::SEAT.union(D::CONFIG)),
        ("on [Prince] difficulty or higher", D::SEAT.union(D::CONFIG)),
        ("on [Prince] difficulty or lower", D::SEAT.union(D::CONFIG)),
        ("when [Scientific] Victory is enabled", D::CONFIG),
        ("when [Scientific] Victory is disabled", D::CONFIG),
        ("when religion is enabled", D::CONFIG),
        ("when religion is disabled", D::CONFIG),
        ("when espionage is enabled", D::CONFIG),
        ("when espionage is disabled", D::CONFIG),
        ("when nuclear weapons are enabled", D::CONFIG),
        ("when nuclear weapons are disabled", D::CONFIG),
        ("with [100]% chance", D::CHANCE.union(D::TURN).union(D::TILE).union(D::UNIT)),
        ("with [0]% chance", D::CHANCE.union(D::TURN).union(D::TILE).union(D::UNIT)),
        ("if tutorials are enabled", D::CONFIG),
        ("if tutorial [Eval] is completed", D::CONFIG),
        // A civilization's kind is fixed at setup; what nothing changes reads CONFIG.
        ("for [Major] Civilizations", D::CONFIG),
        ("for [Human player] Civilizations", D::SEAT),
        ("for [Hostile] Civilizations", D::WAR),
        ("when at war", D::WAR),
        ("when not at war", D::WAR),
        ("during a Golden Age", D::GOLDEN_AGE),
        ("when not in a Golden Age", D::GOLDEN_AGE),
        ("while the empire is happy", D::HAPPINESS_SEEN),
        ("during the [Medieval era]", D::ERA),
        ("before the [Medieval era]", D::ERA),
        ("starting from the [Medieval era]", D::ERA),
        ("if starting in the [Ancient era]", D::CONFIG),
        ("after discovering [Writing]", D::TECHS),
        ("before discovering [Writing]", D::TECHS),
        ("while researching [Writing]", D::RESEARCH_QUEUE),
        ("if no Civilization has adopted [Aristocracy]", D::GLOBAL_POLICIES.union(D::CITY_COUNT)),
        ("after adopting [Aristocracy]", D::POLICIES),
        ("before adopting [Aristocracy]", D::POLICIES),
        (
            "if no Civilization has adopted [Ancestor Worship]",
            D::GLOBAL_POLICIES.union(D::CITY_COUNT),
        ),
        ("after adopting [Ancestor Worship]", D::RELIGION_STATE),
        ("before adopting [Ancestor Worship]", D::RELIGION_STATE),
        ("before founding a Pantheon", D::RELIGION_STATE),
        ("after founding a Pantheon", D::RELIGION_STATE),
        ("before founding a religion", D::RELIGION_STATE),
        ("after founding a religion", D::RELIGION_STATE),
        ("before enhancing a religion", D::RELIGION_STATE),
        ("after enhancing a religion", D::RELIGION_STATE),
        ("after generating a Great Prophet", D::RELIGION_STATE),
        ("if [Temple] is constructed", D::CIV_BUILDINGS),
        ("if [Temple] is not constructed", D::CIV_BUILDINGS),
        (
            "if [Temple] is constructed in all [non-[Puppeted]] cities",
            D::CIV_BUILDINGS.union(D::CITY_COUNT),
        ),
        (
            "if [Temple] is constructed in at least [2] of [All] cities",
            D::CIV_BUILDINGS.union(D::CITY_COUNT),
        ),
        ("if [Temple] is constructed by anybody", D::GLOBAL_BUILDINGS),
        ("if [Temple] is not constructed by anybody", D::GLOBAL_BUILDINGS),
        ("with [Iron]", D::RESOURCES),
        ("without [Iron]", D::RESOURCES),
        ("when above [100] [Gold]", D::STOCKS),
        ("when above [2] [Iron]", D::RESOURCES),
        ("when above [5] [Happiness]", D::HAPPINESS_SEEN),
        ("when below [100] [Gold]", D::STOCKS),
        ("when between [10] and [20] [Happiness]", D::HAPPINESS_SEEN),
        // The city a rule means: in a tile's or a unit's context, the territory's city.
        ("in this city", city),
        // Whether a city is the capital is the city's own fact.
        ("in [Capital] cities", city),
        // The trade network, which its memo keeps.
        ("in cities connected to the capital", city.union(D::CONNECTED)),
        ("in cities with a [Temple]", city),
        ("in cities without a [Temple]", city),
        ("in cities with at least [3] [Population]", city),
        ("in cities with [2] [Specialists]", city),
        ("in cities with between [1] and [5] [Population]", city),
        ("in cities with less than [3] [Unemployed]", city),
        ("with a garrison", city.union(D::UNIT_SET)),
        ("for [Military] units", D::UNIT),
        ("when [Wounded]", D::UNIT),
        ("for units with [Drill I]", D::UNIT),
        ("for units without [Drill I]", D::UNIT),
        ("for units with [Set Up]", D::UNIT),
        ("for units without [Set Up]", D::UNIT),
        ("vs cities", D::COMBAT),
        ("vs [Mounted] units", D::COMBAT),
        ("vs [City]", D::COMBAT),
        (
            "when fighting units from a Civilization with more Cities than you",
            D::COMBAT.union(D::CITY_COUNT),
        ),
        ("when attacking", D::COMBAT),
        ("when defending", D::COMBAT),
        ("when fighting in [Hill] tiles", D::COMBAT.union(D::TILE)),
        ("on foreign continents", D::TILE.union(D::CITY_COUNT)),
        ("when adjacent to a [Great General] unit", D::UNIT.union(D::UNIT_SET)),
        ("when above [50] HP", D::UNIT.union(D::COMBAT)),
        ("when below [50] HP", D::UNIT.union(D::COMBAT)),
        ("if it hasn't used other actions yet", D::UNIT),
        ("when stacked with a [Great General] unit", D::UNIT.union(D::UNIT_SET)),
        ("when not stacked with a [Great General] unit", D::UNIT.union(D::UNIT_SET)),
        // The tiles around: where the tile is, and the map's tiles, not every class.
        ("with [1] to [2] neighboring [Hill] tiles", around),
        ("in [Hill] tiles", D::TILE),
        ("in tiles without [Hill]", D::TILE),
        ("within [2] tiles of a [Mountain]", around),
        ("in tiles adjacent to [River] tiles", around),
        ("in tiles adjacent to [Hill] tiles", around),
        ("in tiles not adjacent to [Hill] tiles", around),
        ("on water maps", D::CONFIG),
        ("when number of [Cities] is equal to [2]", D::CITY_COUNT),
        ("when number of [Cities] is different than [2]", D::CITY_COUNT),
        ("when number of [[Military] Units] is more than [1]", D::UNIT_SET),
        ("when number of [Cities] is less than [3]", D::CITY_COUNT),
        ("when number of [Cities] is between [1] and [3]", D::CITY_COUNT),
    ];
    assert_eq!(table.len(), CONDS.len(), "the table covers every Eval Test conditional");
    for &(text, want) in table {
        let u = eval_unique(r, text);
        let c = t.conds(t.get(u))[0];
        assert_eq!(c.deps, want, "<{text}>");
        assert_eq!(deps_of(&c.data, t.filters()), want, "<{text}>, recomputed");
        assert_eq!(t.get(u).deps(), want, "a unique reads what its conditional reads");
    }
    // A unique's deps are the union of its conditionals', and empty exactly when it has none,
    // over every unique of the kitchen sink.
    for (u, x) in t.iter() {
        let union = t.conds(x).iter().fold(CondDeps::empty(), |d, c| d | c.deps);
        assert_eq!(x.deps(), union, "{}", t.text_of(u));
        assert_eq!(x.deps().is_empty(), x.conds.is_empty(), "{}", t.text_of(u));
    }
    // The shipped tile-neighbourhood uniques (the Celts' Faith, Polynesia's Moai) read neither the
    // turn, nor chance, nor the units.
    let busy = D::TURN | D::CHANCE | D::UNIT_SET | D::STOCKS;
    for (_, c) in t.all_conds().iter() {
        let ty = c.data.ty();
        if matches!(
            ty,
            UniqueType::ConditionalNeighborTiles
                | UniqueType::ConditionalNearTiles
                | UniqueType::ConditionalAdjacentTo
                | UniqueType::ConditionalNotAdjacentTo
        ) && !c.deps.contains(D::all())
        {
            assert!(!c.deps.intersects(busy), "<{}> reads {:?}", t.text(c.text), c.deps);
        }
    }
    let celts = shipped()
        .uniques()
        .all_conds()
        .iter()
        .find(|(_, c)| c.data.ty() == UniqueType::ConditionalNeighborTiles)
        .map(|(_, c)| c.deps)
        .expect("the Celts' neighbouring forests");
    assert_eq!(celts, around, "`{{unimproved}} {{Forest}}` reads the tiles alone");
    // The city leaves that read beyond the city.
    assert_eq!(CityLeaf::Capital.deps(), D::CITY);
    assert_eq!(CityLeaf::Garrisoned.deps(), D::UNIT_SET);
    assert_eq!(CityLeaf::ConnectedToCapital.deps(), D::CONNECTED);
    assert_eq!(CityLeaf::Puppeted.deps(), D::empty());
}

#[test]
fn scoped_evaluation_splits_every_unique_in_two() {
    let r = rules();
    let t = r.uniques();
    // Every conditional is in exactly one of the two scopes.
    for (_, c) in t.all_conds().iter() {
        let civ = in_scope(c.deps, CondDeps::CIV_LEVEL);
        let local = in_scope(c.deps, CondDeps::LOCAL);
        assert!(civ != local, "<{}> reads {:?}", t.text(c.text), c.deps);
        assert_eq!(local, c.deps.intersects(CondDeps::LOCAL));
    }
    // And evaluating the two halves agrees with evaluating the whole, in a few contexts.
    let mut w = World::new();
    w.give_building(1, "Temple");
    w.civ(P0).techs.insert(id(r, "Writing"));
    w.unit_mut(1).health = 40;
    let ctxs = [Ctx::civ(P0), Ctx::city(&w, cid(1)), Ctx::unit(&w, uid(1)), Ctx::default()];
    for (u, x) in t.iter() {
        if x.conds.is_empty() {
            continue;
        }
        for ctx in &ctxs {
            let whole = applies(u, ctx, &w);
            let halves = applies_scoped(u, ctx, &w, CondDeps::CIV_LEVEL)
                && applies_scoped(u, ctx, &w, CondDeps::LOCAL);
            assert_eq!(whole, halves, "{} in {ctx:?}", t.text_of(u));
        }
    }
}

// ---- Gate 2: Python's edge cases ----------------------------------------------------------------

#[test]
fn with_no_civilization_the_civilization_conditionals_fail_as_in_python() {
    let r = rules();
    let t = r.uniques();
    let w = World::new();
    // What holds with nothing in context, in the unchanged world: the game's settings, the
    // negations Python wrote as negations, and what asks about nobody in particular.
    let hold: BTreeSet<&str> = [
        "every [3] turns",
        "after turn number [10]",
        "on [Prince] difficulty",
        "on [Prince] difficulty or higher",
        "on [Prince] difficulty or lower",
        "when [Scientific] Victory is enabled",
        "when religion is enabled",
        "when espionage is enabled",
        "when nuclear weapons are enabled",
        "with [100]% chance",
        "if starting in the [Ancient era]",
        "if no Civilization has adopted [Aristocracy]",
        "before adopting [Aristocracy]",
        "if no Civilization has adopted [Ancestor Worship]",
        "before adopting [Ancestor Worship]",
        "if [Temple] is not constructed by anybody",
        "without [Iron]",
        "when below [100] [Gold]",
        "if it hasn't used other actions yet",
        "when not stacked with a [Great General] unit",
    ]
    .into_iter()
    .collect();
    let none = Ctx::default();
    let mut problems = Vec::new();
    for text in CONDS {
        let u = eval_unique(r, text);
        let got = applies(u, &none, &w);
        if got != hold.contains(text) {
            problems.push(format!("<{text}> gives {got} with nothing in context"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // A context that ignores conditionals, as Python's `applies(u, None)` and `ignore` did:
    // everything applies, even what never holds.
    for (u, _) in t.iter() {
        assert!(applies(u, &Ctx::IGNORE, &w), "{}", t.text_of(u));
    }
    assert!(applies(eval_unique(r, "if tutorials are enabled"), &Ctx::IGNORE, &w));
    assert!(!applies(eval_unique(r, "if tutorials are enabled"), &Ctx::civ(P0), &w));
}

// ---- Gate 3: the chance key ---------------------------------------------------------------------

#[test]
fn a_chance_is_keyed_by_the_unique_and_none_is_not_zero() {
    let r = rules();
    let t = r.uniques();
    let n: NationId = id(r, "Eval Test");
    let twins: Vec<UniqueId> = r.nations()[n]
        .uniques
        .ids()
        .filter(|&u| t.text_of(u) == "[+2 Gold] <with [50]% chance>")
        .collect();
    assert_eq!(twins.len(), 2);
    let (a, b) = (twins[0], twins[1]);
    assert_ne!(t.meta(a).key, t.meta(b).key, "the same text twice has two keys");
    let ctx = Ctx::civ(P0);
    assert_eq!(chance_keys(7, t.meta(a).key, &ctx)[1], t.meta(a).key, "the key is meta.key");
    // The draws: as keyed, and not together.
    let mut w = World::new();
    let mut differ = 0;
    let mut held = 0;
    for turn in 0..200 {
        w.turn = turn;
        let draw = |u: UniqueId| {
            Rng::keyed(w.seed, Purpose::Chance, &chance_keys(turn, t.meta(u).key, &ctx)).unit()
                < 0.5
        };
        assert_eq!(applies(a, &ctx, &w), draw(a));
        differ += usize::from(applies(a, &ctx, &w) != applies(b, &ctx, &w));
        held += usize::from(applies(a, &ctx, &w));
    }
    assert!(differ > 50, "the twins roll apart: {differ} of 200 turns differ");
    assert!((60..140).contains(&held), "about half of 200 draws hold: {held}");
    // A missing civilization, tile or unit is its own key part, never id 0.
    let key = t.meta(a).key;
    let zero = Ctx { civ: Some(PlayerId(0)), tile: Some(TileIdx(0)), ..Ctx::default() };
    let none = Ctx::default();
    let (kz, kn) = (chance_keys(3, key, &zero), chance_keys(3, key, &none));
    assert_eq!(kn[2], None::<PlayerId>.key());
    assert_ne!(kz[2], kn[2]);
    assert_ne!(kz[3], kn[3]);
    assert_eq!(kn[4], None::<UnitId>.key());
    let first = |k: &[u64; 5]| Rng::keyed(w.seed, Purpose::Chance, k).next_u64();
    assert_ne!(first(&kz), first(&kn));
    // The unit is the one a rule means: our side's, in a fight.
    let fighting = fight(&w, Combatant::Unit(uid(3)), CombatAction::Attack, TileIdx(0));
    assert_eq!(chance_keys(3, key, &fighting)[4], uid(1).key());
}

// ---- Countables ---------------------------------------------------------------------------------

#[test]
fn countables_count_as_python_counted() {
    let r = rules();
    let mut w = World::new();
    w.civ(P0).stocks[Stat::Gold.index()] = 57.9;
    w.civ(P0).stocks[Stat::Faith.index()] = -3.5;
    w.civ(P0).branches = 2;
    w.city_mut(1).food = 12.7;
    w.give_building(1, "Temple");
    w.give_building(2, "Temple");
    w.give_building(2, "Monument");
    w.give_building(3, "Temple");
    w.city_mut(2).puppet = true;
    w.civ(P3).alive = false;
    let civ = Ctx::civ(P0);
    let city = Ctx::city(&w, cid(1));
    assert_eq!(Countable::Int(4).eval(&w, &civ), Some(4));
    assert_eq!(Countable::Turns.eval(&w, &Ctx::default()), Some(12));
    assert_eq!(Countable::Cities.eval(&w, &civ), Some(2));
    assert_eq!(Countable::Cities.eval(&w, &Ctx::default()), None, "no civilization");
    assert_eq!(Countable::Units.eval(&w, &civ), Some(2));
    assert_eq!(Countable::CompletedBranches.eval(&w, &civ), Some(2));
    assert_eq!(Countable::Stat(Stat::Gold).eval(&w, &civ), Some(57), "int() truncates");
    assert_eq!(Countable::Stat(Stat::Faith).eval(&w, &civ), Some(-3), "toward zero");
    assert_eq!(Countable::Stat(Stat::Food).eval(&w, &city), Some(12), "a city's stored food");
    assert_eq!(Countable::Stat(Stat::Production).eval(&w, &city), Some(0));
    assert_eq!(Countable::Stat(Stat::Food).eval(&w, &civ), Some(0), "no city: the stock, none");
    assert_eq!(Countable::Stat(Stat::Gold).eval(&w, &Ctx::default()), None);
    // With filters: the civilization's own things that match, and the living civilizations.
    assert_eq!(counted(r, "[Temple] Buildings").eval(&w, &civ), Some(2), "not the other's");
    assert_eq!(counted(r, "[Puppeted] Cities").eval(&w, &civ), Some(1));
    assert_eq!(counted(r, "Remaining [Major] Civilizations").eval(&w, &civ), Some(2));
    assert_eq!(
        counted(r, "Remaining [Major] Civilizations").eval(&w, &Ctx::default()),
        Some(2),
        "counted with no civilization too"
    );
    assert_eq!(counted(r, "[Military] Units").eval(&w, &civ), Some(1), "not the general");
    assert_eq!(counted(r, "[Military] Units").eval(&w, &Ctx::default()), None);
    w.civ(P1).alive = false;
    assert_eq!(counted(r, "Remaining [Major] Civilizations").eval(&w, &civ), Some(1));
    // What they read.
    let f = r.uniques().filters();
    assert_eq!(Countable::Int(1).deps(f), CondDeps::empty());
    assert_eq!(counted(r, "[Temple] Buildings").deps(f), CondDeps::CIV_BUILDINGS);
    assert_eq!(counted(r, "[Puppeted] Cities").deps(f), CondDeps::CITY_COUNT);
    assert_eq!(counted(r, "[Military] Units").deps(f), CondDeps::UNIT_SET);
    assert_eq!(Countable::Stat(Stat::Food).deps(f), CondDeps::CITY);
}

// ---- The indexes ------------------------------------------------------------------------------

#[test]
fn a_civilizations_index_holds_its_sources_standing_and_triggered_uniques() {
    let r = rules();
    let t = r.uniques();
    let mut w = World::new();
    // The Kitchen Sink with its Works in two cities: one entry, two copies; the Works' local
    // unique goes to each city's index instead.
    w.found(cid(4), P1, TileIdx(0));
    w.give_building(3, "Kitchen Sink Works");
    w.give_building(4, "Kitchen Sink Works");
    w.reindex();
    let full = &w.civ_full[1];
    let cost = full.get(UniqueType::CostIncreasesWhenBuilt);
    assert_eq!(cost.len(), 1);
    assert_eq!(cost[0].n, 2, "two Works, one entry");
    assert!(full.get(UniqueType::FoodConsumptionBySpecialists).is_empty(), "a local unique");
    assert_eq!(w.local[&cid(3)].get(UniqueType::FoodConsumptionBySpecialists).len(), 1);
    assert!(w.local[&cid(1)].is_empty(), "the Eval Test's city has no buildings");
    // The nation's triggered uniques sit at their triggers' types.
    let start = full.get(UniqueType::TriggerUponTurnStart);
    assert_eq!(start.len(), 1);
    assert_eq!(t.text_of(start[0].id), "Gain [5] [Gold] <upon turn start>");
    assert!(
        full.get(UniqueType::OneTimeGainStat).is_empty(),
        "a triggered unique is not at its own type"
    );
    // Entries are sorted by (type, id), each once.
    let slot = |e: &index::Entry| {
        let m = t.meta(e.id);
        m.trigger.map_or(m.ty, |tr| Some(tr.ty())).expect("a type")
    };
    for pair in full.entries().windows(2) {
        assert!((slot(&pair[0]) as usize, pair[0].id) < (slot(&pair[1]) as usize, pair[1].id));
    }
    // The global uniques are in every civilization's index, the nation's in its own only.
    let global = r.global_uniques().civ.first().copied().expect("global uniques");
    let has = |c: &Csr, u: UniqueId| c.entries().iter().any(|e| e.id == u);
    assert!(has(&w.civ_full[0], global) && has(&w.civ_full[1], global));
    assert!(!has(&w.civ_full[0], start[0].id));
    // Techs, policies, era, founder beliefs, temporary variants and city-state bonuses.
    let mut src = w.sources(P0, false);
    let base = CivIndex::build(r, &src);
    let variant = t
        .iter()
        .find(|(_, u)| u.flags().contains(UFlags::TEMPORARY))
        .map(|(id, _)| id)
        .expect("a temporary variant");
    src.temporary = vec![variant, variant];
    let cs = city_state_type(r, "Kitchen Sink");
    src.city_states = vec![(cs, CityStateBonus::Friend)];
    let founder = r
        .beliefs()
        .iter()
        .find(|(_, b)| {
            !b.uniques.civ.is_empty() && b.uniques.civ.iter().all(|&u| t.meta(u).ty.is_some())
        })
        .map(|(id, _)| id)
        .expect("a belief with uniques");
    src.founder_beliefs = vec![founder];
    let grown = CivIndex::build(r, &src);
    let entry = |c: &Csr, u: UniqueId| c.entries().iter().find(|e| e.id == u).copied();
    assert_eq!(entry(&grown, variant).map(|e| e.n), Some(2), "granted twice");
    assert_eq!(entry(&base, variant), None);
    let friend = r.city_state_types()[cs].friend.civ[0];
    let ally = r.city_state_types()[cs].ally.civ[0];
    assert!(has(&grown, friend) && !has(&grown, ally));
    assert!(r.beliefs()[founder].uniques.civ.iter().all(|&u| has(&grown, u)));
    // The resource layer: Marble's local unique goes to a city, not the civilization.
    let marble: ResourceId = id(r, "Marble");
    let iron: ResourceId = id(r, "Iron");
    src.resources = [marble, iron].into_iter().collect();
    let with_resources = CivIndex::build(r, &src);
    assert!(with_resources.get(UniqueType::PercentProductionWonders).is_empty());
    let local =
        index::city_local(r, &BuildingSet::new(), &[marble].into_iter().collect::<ResourceSet>());
    assert_eq!(local.get(UniqueType::PercentProductionWonders).len(), 1, "the Marble decision");
    // Counts by placeholder, a triggered unique under its own.
    let counts: BTreeMap<&str, u32> = placeholder_counts(r, full).into_iter().collect();
    assert_eq!(counts.get("Cost increases by [] when built"), Some(&2));
    assert!(counts.contains_key("Gain [] []"));
}

#[test]
fn a_units_profile_holds_its_row_its_type_and_its_promotions() {
    let r = rules();
    let t = r.uniques();
    let raider: BaseUnitId = id(r, "Kitchen Sink Raider");
    let drill: PromotionId = id(r, "Drill I");
    let bare = index::unit_profile(r, raider, &PromotionSet::new());
    assert!(bare.has(UniqueType::AttackOnSea));
    assert_eq!(bare.get(UniqueType::TriggerUponDefeatingUnit).len(), 1);
    let unit_type = r.base_units()[raider].unit_type;
    for &u in r.unit_types()[unit_type].uniques.civ.iter() {
        if t.meta(u).ty.is_some() {
            assert!(bare.entries().iter().any(|e| e.id == u), "the type's {}", t.text_of(u));
        }
    }
    let promoted = index::unit_profile(r, raider, &[drill].into_iter().collect());
    for &u in r.promotions()[drill].uniques.civ.iter() {
        assert!(promoted.entries().iter().any(|e| e.id == u), "{}", t.text_of(u));
    }
    let followers: Vec<BeliefId> = r.beliefs().ids().take(3).collect();
    let mut shuffled = followers.clone();
    shuffled.reverse();
    assert_eq!(index::follower(r, &followers), index::follower(r, &shuffled));
}

// ---- The queries ------------------------------------------------------------------------------

#[test]
fn queries_find_what_holds_with_its_copies() {
    let r = rules();
    let t = r.uniques();
    let mut w = World::new();
    w.found(cid(4), P1, TileIdx(0));
    w.give_building(3, "Kitchen Sink Works");
    w.give_building(4, "Kitchen Sink Works");
    w.reindex();
    let ctx = Ctx::civ(P1);
    let cost = |d: &UniqueData| match d {
        UniqueData::CostIncreasesWhenBuilt(x) => Some(x.cost),
        _ => None,
    };
    assert_eq!(uq::sum_i32(uq::civ(&w, P1, UniqueType::CostIncreasesWhenBuilt, &ctx), cost), 60);
    assert!(uq::any(uq::civ(&w, P1, UniqueType::NotDestroyedWhenCityCaptured, &ctx)));
    assert!(!uq::any(uq::civ(&w, P0, UniqueType::NotDestroyedWhenCityCaptured, &Ctx::civ(P0))));
    // The Eval Test's [+1 Gold] uniques: those whose conditional holds in the unchanged world.
    let hits: Vec<UniqueId> =
        uq::civ(&w, P0, UniqueType::Stats, &Ctx::civ(P0)).map(|h| h.id).collect();
    let n: NationId = id(r, "Eval Test");
    let want: Vec<UniqueId> = r.nations()[n]
        .uniques
        .ids()
        .filter(|&u| t.meta(u).ty == Some(UniqueType::Stats) && applies(u, &Ctx::civ(P0), &w))
        .collect();
    let eval_hits: Vec<UniqueId> =
        hits.iter().copied().filter(|&u| matches!(t.meta(u).source, Source::Nation(_))).collect();
    assert_eq!(eval_hits, want, "in id order, the ones that hold");
    assert!(uq::raw(&w, P0, UniqueType::Stats).count() > hits.len(), "raw ignores conditionals");
    // A city reads its local uniques, then its owner's.
    let city = Ctx::city(&w, cid(3));
    let local: Vec<UniqueId> =
        uq::city(&w, cid(3), UniqueType::FoodConsumptionBySpecialists, &city)
            .map(|h| h.id)
            .collect();
    assert_eq!(local.len(), 1);
    assert!(uq::civ(&w, P1, UniqueType::FoodConsumptionBySpecialists, &ctx).next().is_none());
    // A unit's profile, then its owner's.
    let raider = w.unit_of(P1, "Kitchen Sink Raider", TileIdx(0));
    w.units.insert(uid(7), raider);
    w.reindex();
    let u7 = Ctx::unit(&w, uid(7));
    assert!(uq::any(uq::unit(&w, uid(7), UniqueType::AttackOnSea, &u7)));
    assert!(uq::any(uq::unit_and_civ(&w, uid(7), UniqueType::NotDestroyedWhenCityCaptured, &u7)));
    assert!(!uq::any(uq::unit(&w, uid(7), UniqueType::NotDestroyedWhenCityCaptured, &u7)));
    // One object's, and a tile's terrains'.
    let works: BuildingId = id(r, "Kitchen Sink Works");
    let obj = uq::object(&w, &r.buildings()[works].uniques, UniqueType::ObsoleteWith, &ctx);
    assert_eq!(obj.count(), 1);
    let spire: TerrainId = id(r, "Kitchen Sink Spire");
    w.tile_mut(TileIdx(5)).terrains.insert(spire);
    let ty = t.meta(r.terrains()[spire].uniques.ids().next().expect("a unique")).ty.expect("typed");
    assert!(uq::any(uq::terrains(&w, TileIdx(5), ty, &Ctx::tile(Some(P0), TileIdx(5)))));
}

// ---- Triggers ----------------------------------------------------------------------------------

#[test]
fn triggers_fire_when_their_event_passes_their_filter() {
    let r = rules();
    let t = r.uniques();
    let mut w = World::new();
    let texts = |ids: &[UniqueId]| ids.iter().map(|&u| t.text_of(u).to_owned()).collect::<Vec<_>>();
    let site = TriggerSite::civ(P1);
    let fired = fire(&w, &site, &TriggerEvent::TurnStart, true);
    assert_eq!(texts(&fired), ["Gain [5] [Gold] <upon turn start>"]);
    assert!(fire(&w, &TriggerSite::civ(P0), &TriggerEvent::TurnStart, true).is_empty());
    // Filters on the other civilization, seen by the one it happens to.
    let war = |on| fire(&w, &site, &TriggerEvent::DeclaringWar { on }, true);
    assert_eq!(
        texts(&war(P2)),
        ["Gain [25] [Culture] <upon declaring war on [City-State] Civilizations>"]
    );
    assert!(war(P0).is_empty(), "a major is not a city-state");
    let entering = fire(&w, &site, &TriggerEvent::EnteringWar { with: P0 }, true);
    assert_eq!(
        texts(&entering),
        ["Free [Warrior] appears <upon entering a war with [Major] Civilizations>"]
    );
    // Filters on units and improvements. A unit that is gone is matched by its facts, taken
    // before it went: the site fires after the removal, as Python's did.
    let facts = |u| UnitFacts::of(&w, uid(u));
    let (horseman, general) = (facts(3), facts(2));
    let mut gone = w.clone();
    gone.units.remove(&uid(3));
    gone.units.remove(&uid(2));
    let lost = |u| fire(&gone, &site, &TriggerEvent::LosingUnit(u), true);
    assert_eq!(lost(horseman).len(), 1, "a Horseman is military");
    assert!(lost(general).is_empty(), "a Great General is not");
    // The facts answer every unit filter of the ruleset as the unit did.
    let f = t.filters();
    for u in [1, 2, 3] {
        let x = UnitFacts::of(&w, uid(u));
        for (filter, _) in f.units().iter() {
            for viewer in [None, Some(P0), Some(P1)] {
                let live = f.unit_matches(filter, &w, uid(u), UnitScope { this: None, viewer });
                let copy = f.unit_facts_match(filter, &w, &x, viewer);
                assert_eq!(copy, live, "{}", t.unit_filter(filter));
            }
        }
    }
    let gained = |b: &str| fire(&w, &site, &TriggerEvent::GainingUnit(id(r, b)), true);
    assert_eq!(gained("Great Prophet").len(), 1);
    assert!(gained("Warrior").is_empty(), "the filter holds wherever a unit is gained");
    let farm: ImprovementId = id(r, "Farm");
    assert_eq!(fire(&w, &site, &TriggerEvent::BuildingImprovement(farm), true).len(), 1);
    let mine: ImprovementId = id(r, "Mine");
    assert!(fire(&w, &site, &TriggerEvent::BuildingImprovement(mine), true).is_empty());
    // A policy.
    let aristocracy: PolicyId = id(r, "Aristocracy");
    let adopted =
        fire(&w, &site, &TriggerEvent::Adopting(PolicyOrBelief::Policy(aristocracy)), true);
    assert_eq!(texts(&adopted), ["Free Great Person <upon adopting [Aristocracy]>"]);
    // A unit's profile fires with the unit, unless left out.
    let raider = w.unit_of(P1, "Kitchen Sink Raider", TileIdx(0));
    w.units.insert(uid(7), raider);
    w.reindex();
    let at_unit = TriggerSite { unit: Some(uid(7)), ..site };
    let promoted = fire(&w, &at_unit, &TriggerEvent::Promotion, true);
    assert_eq!(texts(&promoted), ["[This Unit] loses [1] movement <upon being promoted>"]);
    assert!(fire(&w, &at_unit, &TriggerEvent::Promotion, false).is_empty());
    let warrior = UnitFacts::of(&w, uid(1));
    let defeated = fire(&w, &at_unit, &TriggerEvent::DefeatingUnit(warrior), true);
    assert_eq!(defeated.len(), 1, "a Warrior is military");
    // The site's context: a building's copies fire once each.
    w.give_building(3, "Kitchen Sink Works");
    assert_eq!(at_unit.ctx(&w).tile, Some(TileIdx(0)), "the unit's tile");
    // Every kind is a trigger type, and each trigger type is one kind.
    let kinds: BTreeSet<UniqueType> = TriggerKind::ALL.iter().map(|k| k.ty()).collect();
    let triggers: BTreeSet<UniqueType> =
        UniqueType::ALL.into_iter().filter(|u| u.role() == Some(Role::Trigger)).collect();
    assert_eq!(kinds, triggers);
    // A timed triggered unique fires as its grant, whatever its effect's own conditionals
    // (`<when attacking>`) say now.
    let timed = fire(&w, &site, &TriggerEvent::BeingDeclaredWarUpon { by: P0 }, true);
    assert_eq!(timed.len(), 1);
    assert!(matches!(
        OneTimeEffect::decode(r, timed[0]),
        Some(OneTimeEffect::Timed { turns: 10, .. })
    ));
}

// ---- Gate 5: every one-time effect decodes ------------------------------------------------------

#[test]
fn every_one_time_effect_kind_decodes() {
    let r = kitchen_sink();
    let t = r.uniques();
    let mut kinds = BTreeSet::new();
    let mut types = BTreeSet::new();
    for (u, x) in t.iter() {
        let m = t.meta(u);
        let gain = m.ty.and_then(|ty| ty.info().support).is_some_and(|s| s.gain);
        let once = m.role == Role::OneTime || gain || m.timed.is_some();
        let decoded = OneTimeEffect::decode(r, u);
        assert_eq!(decoded.is_some(), once, "{}", t.text_of(u));
        if let Some(e) = decoded {
            kinds.insert(e.kind_name());
            if let Some(ty) = m.ty {
                types.insert(ty);
            }
            if m.timed.is_some() {
                assert!(matches!(e, OneTimeEffect::Timed { .. }));
            }
            // Resolved as the text says.
            if let (
                UniqueData::OneTimeGainStat(g),
                OneTimeEffect::GainStat { stat, min, max, speed },
            ) = (x.data, e)
            {
                assert_eq!((stat, min, max), (g.stat, g.amount, g.amount));
                assert_eq!(speed, x.flags().contains(UFlags::SPEED));
            }
            if let OneTimeEffect::Unit(UnitEffect::Movement(n)) = e {
                let lose = m.ty == Some(UniqueType::OneTimeUnitLoseMovement);
                assert_eq!(n < 0, lose, "{}", t.text_of(u));
            }
        }
    }
    let one_time: BTreeSet<UniqueType> =
        UniqueType::ALL.into_iter().filter(|u| u.role() == Some(Role::OneTime)).collect();
    assert_eq!(one_time.len(), 39);
    let missing: Vec<&str> = one_time.difference(&types).map(|u| u.name()).collect();
    assert!(missing.is_empty(), "one-time types that decode nowhere: {missing:?}");
    assert_eq!(kinds.len(), OneTimeEffect::KINDS, "{kinds:?}");
    // `[in this city]` is the city in context, though as a filter it is every city.
    let scopes: BTreeSet<bool> = t
        .iter()
        .filter_map(|(u, _)| match OneTimeEffect::decode(r, u) {
            Some(
                OneTimeEffect::GainPopulation { cities, .. }
                | OneTimeEffect::TakeOverTilesInCity { cities, .. }
                | OneTimeEffect::FreeBuilding { cities, .. },
            ) => Some(cities == CityScope::ThisCity),
            _ => None,
        })
        .collect();
    assert_eq!(scopes, [false, true].into_iter().collect(), "this city, and cities matching");
}

// ---- Gate 6: what a requirement says ------------------------------------------------------------

/// The uniques a rejection reads of an object (`rejection_reasons`): a building's, or a unit's
/// with its unit type's.
fn object_uniques(r: &Ruleset, kind: &str, name: &str) -> Vec<UniqueId> {
    match kind {
        "Building" => r.buildings()[id::<BuildingId>(r, name)].uniques.ids().collect(),
        "Unit" => {
            let u: BaseUnitId = id(r, name);
            let ty = r.base_units()[u].unit_type;
            r.base_units()[u].uniques.ids().chain(r.unit_types()[ty].uniques.ids()).collect()
        }
        other => panic!("{other} is no kind of object"),
    }
}

/// What each failing conditional of the unique says, as `requirement_problems` puts it.
fn said(r: &Ruleset, nation: NationId, u: UniqueId, failing: &[usize]) -> Vec<(String, String)> {
    let t = r.uniques();
    let built_variant = t.meta(u).ty == Some(UniqueType::CanOnlyBeBuiltWhen);
    let conds = t.conds(t.get(u));
    failing
        .iter()
        .map(|&i| {
            let p = conds[i].describe(r, nation);
            let p = if built_variant && p.kind == ProblemKind::ShouldNotBeDisplayed {
                Problem {
                    kind: ProblemKind::CanOnlyBeBuiltInSpecificCities,
                    text: t.text_of(u).into(),
                }
            } else {
                p
            };
            (p.kind.name().to_owned(), p.text)
        })
        .collect()
}

fn pairs(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .expect("a list")
        .iter()
        .map(|p| (p[0].as_str().expect("a kind").into(), p[1].as_str().expect("a text").into()))
        .collect()
}

#[test]
fn describe_says_what_python_said() {
    let r = shipped();
    let t = r.uniques();
    let data: Value = serde_json::from_str(NOT_MET).expect("not_met.json is JSON");
    let find = |kind: &str, object: &str, text: &str| {
        object_uniques(r, kind, object)
            .into_iter()
            .find(|&u| t.text_of(u) == text)
            .unwrap_or_else(|| panic!("{object} has {text}"))
    };
    let mut checked = 0;
    let mut problems = Vec::new();
    for row in data["forced"].as_array().expect("forced rows") {
        let nation: NationId = id(r, row[0].as_str().expect("a nation"));
        let u = find(
            row[1].as_str().expect("a kind"),
            row[2].as_str().expect("an object"),
            row[3].as_str().expect("a text"),
        );
        let all: Vec<usize> = (0..t.conds(t.get(u)).len()).collect();
        let (got, want) = (said(r, nation, u, &all), pairs(&row[4]));
        if got != want {
            problems.push(format!("{row}: Rust says {got:?}"));
        }
        checked += 1;
    }
    for row in data["fixtures"].as_array().expect("fixture rows") {
        let nation: NationId = id(r, row[2].as_str().expect("a nation"));
        let u = find(
            row[3].as_str().expect("a kind"),
            row[4].as_str().expect("an object"),
            row[5].as_str().expect("a text"),
        );
        let failing: Vec<usize> = row[6]
            .as_array()
            .expect("indices")
            .iter()
            .map(|i| usize::try_from(i.as_u64().expect("an index")).expect("small"))
            .collect();
        let (got, want) = (said(r, nation, u, &failing), pairs(&row[7]));
        if got != want {
            problems.push(format!("{row}: Rust says {got:?}"));
        }
        checked += 1;
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(checked > 70, "{checked} rows");
}

#[test]
fn requirement_problems_name_what_fails_in_a_world() {
    let r = rules();
    let mut w = World::new();
    let circus: BuildingId = id(r, "Circus Maximus");
    let courthouse: BuildingId = id(r, "Courthouse");
    let ctx = Ctx::city(&w, cid(1));
    let got = uq::requirement_problems(&w, r.buildings()[circus].uniques.ids(), &ctx, P0);
    assert_eq!(
        got,
        [Problem {
            kind: ProblemKind::RequiresBuildingInAllCities,
            text: "Requires a Colosseum in all non-[Puppeted] cities".into()
        }]
    );
    w.give_building(1, "Colosseum");
    w.give_building(2, "Colosseum");
    assert!(uq::requirement_problems(&w, r.buildings()[circus].uniques.ids(), &ctx, P0).is_empty());
    let got = uq::requirement_problems(&w, r.buildings()[courthouse].uniques.ids(), &ctx, P0);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].kind, ProblemKind::CanOnlyBeBuiltInSpecificCities);
    assert_eq!(got[0].text, "Can only be built <in [Annexed] cities>");
    // Any other conditional is `Not available (...)`.
    w.religion = false;
    let temple: BuildingId = id(r, "Temple");
    let got = uq::requirement_problems(&w, r.buildings()[temple].uniques.ids(), &ctx, P0);
    assert_eq!(
        got,
        [Problem {
            kind: ProblemKind::ShouldNotBeDisplayed,
            text: "Not available (when religion is enabled)".into()
        }]
    );
    // The two counts name the civilization's own building.
    let russia: NationId = id(r, "Russia");
    let heroic: BuildingId = id(r, "Heroic Epic");
    let c = r.buildings()[heroic]
        .uniques
        .ids()
        .flat_map(|u| r.uniques().conds(r.uniques().get(u)).iter().copied())
        .next()
        .expect("a conditional");
    assert_eq!(c.describe(r, russia).text, "Requires a Krepost in all non-[Puppeted] cities");
    assert_eq!(cond::equivalent_building(r, russia, "Krepost"), "Krepost");
    assert_eq!(cond::equivalent_building(r, russia, "Wonder"), "Wonder", "a filter stays");
}

// ---- Context --------------------------------------------------------------------------------

#[test]
fn a_context_derives_what_python_derived() {
    let w = World::new();
    let city = Ctx::city(&w, cid(3));
    assert_eq!((city.civ, city.tile), (Some(P1), Some(w.city(3).tile)));
    let unit = Ctx::unit(&w, uid(2));
    assert_eq!((unit.civ, unit.tile), (Some(P0), Some(w.units[&uid(2)].tile)));
    // A tile of the civilization's city means that city; another's, none.
    let t1 = w.city(1).tile;
    assert_eq!(Ctx::tile(Some(P0), t1).rel_city(&w), Some(cid(1)));
    assert_eq!(Ctx::tile(Some(P1), t1).rel_city(&w), None);
    // In a fight: our side's unit and the tile under attack.
    let f = fight(&w, Combatant::City(cid(3)), CombatAction::Attack, TileIdx(5));
    assert_eq!(f.civ, Some(P0));
    assert_eq!(f.rel_unit(), Some(uid(1)));
    assert_eq!(f.rel_tile(), Some(TileIdx(5)));
    let explicit = Ctx { civ: Some(P2), ..Ctx::city(&w, cid(1)) };
    assert_eq!(explicit.resolve(&w).civ, Some(P2), "a civilization given is kept");
    // Every constructor resolves.
    let built =
        [Ctx::civ(P0), Ctx::city(&w, cid(1)), Ctx::unit(&w, uid(1)), f, Ctx::tile(None, t1)];
    for ctx in built {
        assert!(ctx.is_resolved(&w), "{ctx:?}");
    }
    assert!(!Ctx { unit: Some(uid(1)), ..Ctx::default() }.is_resolved(&w));
}

/// A context written field by field that forgot to resolve would fail every civilization
/// conditional without a word; debug builds refuse it.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "is not resolved")]
fn an_unresolved_context_is_refused_in_debug_builds() {
    let w = World::new();
    let u = eval_unique(rules(), "when not in a Golden Age");
    let _ = applies(u, &Ctx { unit: Some(uid(1)), ..Ctx::default() }, &w);
}

// ---- More forms of the conditionals -------------------------------------------------------------

/// In a tile's or a unit's context, a city conditional asks about the city whose territory the
/// tile is, when the civilization in context owns it (`rel_city`, `uniques.py:262-273`).
#[test]
fn a_city_conditional_means_the_territorys_city_in_a_tile_or_unit_context() {
    let r = rules();
    let t = r.uniques();
    let mut w = World::new();
    let capital = eval_unique(r, "in [Capital] cities");
    let temple = eval_unique(r, "in cities with a [Temple]");
    let no_temple = eval_unique(r, "in cities without a [Temple]");
    let big = eval_unique(r, "in cities with at least [3] [Population]");
    // A tile of city 1's territory (the capital's), a tile of city 2's, and one of no city.
    let near1 = w.grid.neighbors(w.city(1).tile).next().expect("a neighbour");
    let near2 = w.grid.neighbors(w.city(2).tile).next().expect("a neighbour");
    let wild = w.grid.idx(0, 7).expect("on the map");
    for (tile, c) in [(near1, 1), (near2, 2)] {
        let x = w.tile_mut(tile);
        x.owner = Some(P0);
        x.city = Some(cid(c));
    }
    w.unit_mut(2).tile = near1;
    w.reindex();
    let ask = |w: &World, u: UniqueId, ctx: &Ctx| {
        let c = t.conds(t.get(u))[0];
        (holds(&c, u, ctx, w), applies(u, ctx, w))
    };
    let on_tile = Ctx::tile(Some(P0), near1);
    let on_unit = Ctx::unit(&w, uid(2));
    assert_eq!(on_unit.tile, Some(near1));
    for ctx in [on_tile, on_unit] {
        assert_eq!(ctx.rel_city(&w), Some(cid(1)));
        assert_eq!(ask(&w, capital, &ctx), (true, true), "the capital's territory: {ctx:?}");
        assert_eq!(ask(&w, temple, &ctx), (false, false));
        assert_eq!(ask(&w, no_temple, &ctx), (true, true));
    }
    // The territory's city changes: its buildings and its citizens are what the tile reads.
    w.give_building(1, "Temple");
    w.city_mut(1).population = 3;
    for ctx in [on_tile, Ctx::unit(&w, uid(2))] {
        assert_eq!(ask(&w, temple, &ctx), (true, true));
        assert_eq!(ask(&w, no_temple, &ctx), (false, false));
        assert_eq!(ask(&w, big, &ctx), (true, true));
    }
    // Another city's territory, a city of someone else's, and no city's.
    assert_eq!(ask(&w, capital, &Ctx::tile(Some(P0), near2)), (false, false));
    assert_eq!(ask(&w, no_temple, &Ctx::tile(Some(P0), near2)), (true, true));
    for ctx in [Ctx::tile(Some(P1), near1), Ctx::tile(Some(P0), wild), Ctx::tile(None, near1)] {
        assert_eq!(ctx.rel_city(&w), None, "{ctx:?}");
        for u in [capital, temple, no_temple, big] {
            assert_eq!(ask(&w, u, &ctx), (false, false), "{} in {ctx:?}", t.text_of(u));
        }
    }
    // So what they read names the tile: a memo keyed by the tile moves with its territory.
    for u in [capital, temple, no_temple, big] {
        assert!(t.get(u).deps().contains(CondDeps::CITY | CondDeps::TILE), "{}", t.text_of(u));
    }
}

/// `when above [n] HP` in a city's fight reads the city's health (`_hp`, `uniques.py:1036-1043`),
/// and the city conditionals read our side's city.
#[test]
fn a_citys_fight_reads_the_citys_health() {
    let r = rules();
    let mut w = World::new();
    let above = eval_unique(r, "when above [50] HP");
    let below = eval_unique(r, "when below [50] HP");
    let capital = eval_unique(r, "in [Capital] cities");
    let defending = |w: &World, c: u32| {
        Ctx::fight(
            w,
            CombatCtx {
                our: Combatant::City(cid(c)),
                their: Some(Combatant::Unit(uid(3))),
                attacked_tile: Some(w.city(c).tile),
                action: Some(CombatAction::Defend),
            },
        )
    };
    let ctx = defending(&w, 1);
    assert_eq!((ctx.civ, ctx.tile, ctx.rel_unit()), (Some(P0), Some(w.city(1).tile), None));
    assert!(applies(above, &ctx, &w) && !applies(below, &ctx, &w), "200 HP");
    assert!(applies(capital, &ctx, &w));
    assert!(!applies(capital, &defending(&w, 2), &w), "city 2 is not the capital");
    w.city_mut(1).health = 40;
    assert!(!applies(above, &ctx, &w) && applies(below, &ctx, &w), "40 HP");
    w.city_mut(1).health = 50;
    assert!(!applies(above, &ctx, &w) && !applies(below, &ctx, &w), "50 HP is neither");
}

/// `(modified by game speed)` scales the bounds of all three stat comparisons, `when between`'s
/// two as well (refcheck: between-stat-scales-by-speed): on Marathon (3x), 100 Gold is 300 and
/// 10 to 20 is 30 to 60.
#[test]
fn the_stat_comparisons_scale_by_game_speed() {
    let r = rules();
    let mut w = World::new();
    let [above, below, between] = SCALED.map(|u| eval_text(r, u));
    let plain = eval_unique(r, "when above [100] [Gold]");
    let ctx = Ctx::civ(P0);
    let with_gold = |w: &mut World, g: f64| {
        w.civ(P0).stocks[Stat::Gold.index()] = g;
    };
    let answers = |w: &World| [above, below, between, plain].map(|u| applies(u, &ctx, w));
    // Standard speed: as written.
    with_gold(&mut w, 150.0);
    assert_eq!(answers(&w), [true, false, false, true]);
    with_gold(&mut w, 15.0);
    assert_eq!(answers(&w), [false, true, true, false]);
    // Marathon: scaled, the unscaled unique unchanged.
    w.speed = id(r, "Marathon");
    assert!((r.speeds()[w.speed].modifier - 3.0).abs() < 1e-9);
    with_gold(&mut w, 150.0);
    assert_eq!(answers(&w), [false, true, false, true]);
    with_gold(&mut w, 300.5);
    assert_eq!(answers(&w), [true, false, false, true]);
    with_gold(&mut w, 15.0);
    assert_eq!(answers(&w), [false, true, false, false], "15 is below 30");
    for (g, inside) in [(30.0, true), (45.0, true), (60.0, true), (29.5, false), (60.5, false)] {
        with_gold(&mut w, g);
        assert_eq!(applies(between, &ctx, &w), inside, "{g} Gold between 30 and 60");
    }
    // Quick (0.67): 10 to 20 is 6.7 to 13.4.
    w.speed = id(r, "Quick");
    with_gold(&mut w, 7.0);
    assert!(applies(between, &ctx, &w));
    with_gold(&mut w, 15.0);
    assert!(!applies(between, &ctx, &w));
}

#[test]
fn eval_ids_and_types_line_up() {
    // The ids this file names exist in the ruleset it loads.
    let r = rules();
    let _: BeliefId = r.beliefs().ids().next().expect("a belief");
    let _: CityStateTypeId = city_state_type(r, "Kitchen Sink");
    assert!(TriggerKind::ALL.windows(2).all(|p| p[0] < p[1]));
    assert_eq!(TriggerKind::COUNT, 25);
}

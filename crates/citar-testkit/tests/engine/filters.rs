//! Filters and the tables for map generation, the AI and victory (package 1a-06, DESIGN.md 5.7
//! and 5.10) against their contract:
//! - gate 1: for every filter text the ruleset writes, in every static domain, the Rust set equals
//!   the Python truth table over every object (`data/filters.json`, recorded by
//!   `scripts/refcheck/filters.py`), and every static filter the loaded ruleset holds equals its row;
//! - gate 2: every dynamic leaf, and compiled filters of the ruleset, answer right against a mock
//!   world;
//! - gate 3: constant folding keeps the meaning of random trees, on abstract leaves in
//!   `tests/props.rs` and here on the engine's own: every merge a leaf allows is exact, and random
//!   trees answer the same folded as not, on random mock worlds;
//! - gate 4: a region conditional on an effect is refused;
//! - gate 5: every map-generation unique lands in a table (the tables' snapshot is the golden
//!   `gen.json`, checked in `tests/determinism.rs`);
//! - a term that matches nothing (in the form its reader reads it), a filter nested too deep, an
//!   object filter that would select a kind its terms do not name, a map-generation filter that
//!   asks more than the terrain and an unknown milestone are refused, naming the file and object.

use std::collections::{BTreeMap, BTreeSet};

use citar_engine::base::ids::{
    BaseUnitId, BuildingId, CityId, Id, ImprovementId, NationId, PlayerId, PolicyId, PromotionId,
    ReligionId, ResourceId, TerrainId, TileIdx, UnitId,
};
use citar_engine::base::sets::{BitSet, BuildingSet, PromotionSet, ResourceSet, TerrainSet};
use citar_engine::rules::defs::{Milestone, NationKind, StartBias};
use citar_engine::rules::gen_tables::Near;
use citar_engine::rules::{Named, Ruleset, RulesetErrorKind};
use citar_engine::unique::filter::statics::members;
use citar_engine::unique::filter::{
    self, CityLeaf, CivLeaf, Combatant, Expr, TileLeaf, UnitFacts, UnitLeaf, UnitScope,
};
use citar_engine::unique::{
    CondDeps, FilterFacts, Role, Source, StaticDomain, TileFacts, UniqueType,
};
use proptest::prelude::*;
use serde_json::{Value, json};

use super::rules::{load_edited, shipped};

const FILTERS: &str = include_str!("../../data/filters.json");

// ---- Gate 1: static filters against Python's truth tables ---------------------------------------

/// The names of a domain's objects, in id order.
fn domain_names(r: &Ruleset, d: StaticDomain) -> Vec<String> {
    fn of<I: Named>(r: &Ruleset, n: usize) -> Vec<String> {
        (0..n).filter_map(I::from_index).map(|i| r.name(i).unwrap_or("?").to_owned()).collect()
    }
    let n = d.size(r);
    match d {
        StaticDomain::BaseUnit => of::<BaseUnitId>(r, n),
        StaticDomain::Building => of::<BuildingId>(r, n),
        StaticDomain::Terrain => of::<TerrainId>(r, n),
        StaticDomain::Improvement => of::<ImprovementId>(r, n),
        StaticDomain::Resource => of::<ResourceId>(r, n),
        StaticDomain::Tech => of::<citar_engine::base::ids::TechId>(r, n),
        StaticDomain::Era => of::<citar_engine::base::ids::EraId>(r, n),
        StaticDomain::Policy => of::<PolicyId>(r, n),
        StaticDomain::Promotion => of::<PromotionId>(r, n),
        StaticDomain::Nation => of::<NationId>(r, n),
    }
}

fn domain_named(name: &str) -> StaticDomain {
    StaticDomain::ALL.into_iter().find(|d| d.name() == name).expect("a domain")
}

#[test]
fn static_filters_equal_python_truth_tables() {
    let r = shipped();
    let data: Value = serde_json::from_str(FILTERS).expect("filters.json is JSON");
    for d in StaticDomain::ALL {
        let want: Vec<&str> = data["objects"][d.name()]
            .as_array()
            .expect("objects")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(domain_names(r, d), want, "{} objects in id order", d.name());
    }
    let rows = data["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2050, "205 texts in 10 domains");
    let mut problems = Vec::new();
    let mut table: BTreeMap<(String, String), BitSet> = BTreeMap::new();
    for row in rows {
        let d = domain_named(row[0].as_str().expect("a domain"));
        let text = row[1].as_str().expect("a text");
        let names = domain_names(r, d);
        let want: BitSet = row[2]
            .as_array()
            .expect("members")
            .iter()
            .map(|m| {
                let m = m.as_str().expect("a name");
                u32::try_from(names.iter().position(|n| n == m).expect("a member")).expect("u32")
            })
            .collect();
        let got = members(r, d, text).expect("not too deep");
        if got != want {
            let show = |s: &BitSet| s.iter().map(|i| names[i as usize].clone()).collect::<Vec<_>>();
            problems.push(format!(
                "{} [{text}]: Python {:?}, Rust {:?}",
                d.name(),
                show(&want),
                show(&got)
            ));
        }
        table.insert((d.name().to_owned(), text.to_owned()), want);
    }
    // Every static filter the loaded ruleset holds is one of the rows, and equals it.
    let t = r.uniques();
    let mut checked = 0;
    for (_, s) in t.sets().iter() {
        if s.fixed {
            continue;
        }
        let key = (s.domain.name().to_owned(), t.text(s.text).to_owned());
        match table.get(&key) {
            Some(want) if *want == s.members => checked += 1,
            Some(_) => problems.push(format!("the loaded {key:?} differs from its row")),
            None => problems.push(format!("{key:?} is not among the recorded texts")),
        }
    }
    assert!(problems.is_empty(), "{} differences:\n{}", problems.len(), problems.join("\n"));
    assert!(checked > 60, "{checked} static filters checked");
}

#[test]
fn static_filters_read_as_the_ruleset_means_them() {
    let r = shipped();
    let set = |d, text| {
        let names = domain_names(r, d);
        members(r, d, text)
            .expect("parses")
            .iter()
            .map(|i| names[i as usize].clone())
            .collect::<Vec<String>>()
    };
    let has = |d, text, name: &str| set(d, text).iter().any(|n| n == name);
    // Units: roles, domains, types, eras through their tech, and the `[x] units` reading.
    assert!(has(StaticDomain::BaseUnit, "{Military} {Land}", "Warrior"));
    assert!(!has(StaticDomain::BaseUnit, "{Military} {Land}", "Trireme"));
    assert!(has(StaticDomain::BaseUnit, "Military units", "Warrior"));
    assert!(has(StaticDomain::BaseUnit, "Aircraft", "Fighter"), "the type's tag");
    assert!(!has(StaticDomain::BaseUnit, "Aircraft", "Guided Missile"));
    assert!(has(StaticDomain::BaseUnit, "{pre-[Industrial era]} {Military} {Land}", "Musketman"));
    assert!(!has(StaticDomain::BaseUnit, "{pre-[Industrial era]} {Military} {Land}", "Rifleman"));
    assert!(!has(StaticDomain::BaseUnit, "non-[Air]", "Bomber"));
    // Buildings: wonders, stats, eras through their tech.
    assert!(has(StaticDomain::Building, "Science", "Library"));
    assert!(has(StaticDomain::Building, "Wonder", "The Great Library"));
    assert!(!has(StaticDomain::Building, "Wonder", "Library"));
    // Eras.
    assert_eq!(set(StaticDomain::Era, "pre-[Classical era]"), ["Ancient era"]);
    // Terrains and improvements.
    assert!(has(StaticDomain::Terrain, "Rough terrain", "Hill"));
    assert!(has(StaticDomain::Improvement, "All Road", "Railroad"));
    assert!(
        members(r, StaticDomain::Terrain, &format!("{}x{}", "non-[".repeat(20), "]".repeat(20)))
            .is_err()
    );
}

// ---- Gate 2: dynamic leaves against a mock world -----------------------------------------------

#[derive(Clone, Debug, Default)]
struct Tile {
    terrains: TerrainSet,
    river: bool,
    fresh: bool,
    coast: bool,
    owner: Option<PlayerId>,
    friendly_to: BTreeSet<u8>,
    resource: Option<ResourceId>,
    improvement: Option<ImprovementId>,
    route: Option<ImprovementId>,
    pillaged: bool,
    worked: bool,
}

#[derive(Clone, Debug)]
struct Civ {
    nation: NationId,
    kind: NationKind,
    human: bool,
    religion: Option<ReligionId>,
    sees: ResourceSet,
}

#[derive(Clone, Debug)]
struct Unit {
    owner: PlayerId,
    base: BaseUnitId,
    promotions: PromotionSet,
    wounded: bool,
    embarked: bool,
    set_up: bool,
}

#[derive(Clone, Debug)]
struct City {
    owner: PlayerId,
    founder: PlayerId,
    buildings: BuildingSet,
    capital: bool,
    coastal: bool,
    annex: bool,
    puppet: bool,
    connected: bool,
    garrisoned: bool,
    resisting: bool,
    razing: bool,
    holy: bool,
    religion: Option<ReligionId>,
}

/// A world of plain facts: each question answered from a table.
#[derive(Debug, Default)]
struct Mock {
    tiles: Vec<Tile>,
    civs: Vec<Civ>,
    war: BTreeSet<(u8, u8)>,
    met: BTreeSet<(u8, u8)>,
    friends: BTreeSet<(u8, u8)>,
    open: BTreeSet<(u8, u8)>,
    units: BTreeMap<u32, Unit>,
    cities: BTreeMap<u32, City>,
    /// Per religion: (major, enhanced).
    religions: Vec<(bool, bool)>,
}

fn pair(a: PlayerId, b: PlayerId) -> (u8, u8) {
    (a.0, b.0)
}

impl TileFacts for Mock {
    fn tile_terrains(&self, t: TileIdx) -> TerrainSet {
        self.tiles[t.0 as usize].terrains
    }
    fn tile_river(&self, t: TileIdx) -> bool {
        self.tiles[t.0 as usize].river
    }
    fn tile_fresh_water(&self, t: TileIdx) -> bool {
        self.tiles[t.0 as usize].fresh
    }
    fn tile_next_to_coast(&self, t: TileIdx) -> bool {
        self.tiles[t.0 as usize].coast
    }
}

impl FilterFacts for Mock {
    fn civ_nation(&self, p: PlayerId) -> NationId {
        self.civs[usize::from(p.0)].nation
    }
    fn civ_kind(&self, p: PlayerId) -> NationKind {
        self.civs[usize::from(p.0)].kind
    }
    fn civ_is_human(&self, p: PlayerId) -> bool {
        self.civs[usize::from(p.0)].human
    }
    fn civ_religion(&self, p: PlayerId) -> Option<ReligionId> {
        self.civs[usize::from(p.0)].religion
    }
    fn at_war(&self, a: PlayerId, b: PlayerId) -> bool {
        self.war.contains(&pair(a, b)) || self.war.contains(&pair(b, a))
    }
    fn has_met(&self, a: PlayerId, b: PlayerId) -> bool {
        self.met.contains(&pair(a, b))
    }
    fn is_friend(&self, a: PlayerId, b: PlayerId) -> bool {
        self.friends.contains(&pair(a, b))
    }
    fn has_open_borders(&self, a: PlayerId, b: PlayerId) -> bool {
        self.open.contains(&pair(a, b))
    }
    fn tile_owner(&self, t: TileIdx) -> Option<PlayerId> {
        self.tiles[t.0 as usize].owner
    }
    fn tile_friendly_to(&self, t: TileIdx, p: PlayerId) -> bool {
        self.tiles[t.0 as usize].friendly_to.contains(&p.0)
    }
    fn tile_resource(&self, t: TileIdx) -> Option<ResourceId> {
        self.tiles[t.0 as usize].resource
    }
    fn resource_visible(&self, p: PlayerId, r: ResourceId) -> bool {
        self.civs[usize::from(p.0)].sees.contains(r)
    }
    fn tile_improvement(&self, t: TileIdx) -> Option<ImprovementId> {
        let x = &self.tiles[t.0 as usize];
        if x.pillaged { None } else { x.improvement }
    }
    fn tile_route(&self, t: TileIdx) -> Option<ImprovementId> {
        self.tiles[t.0 as usize].route
    }
    fn tile_pillaged(&self, t: TileIdx) -> bool {
        self.tiles[t.0 as usize].pillaged
    }
    fn tile_worked(&self, t: TileIdx) -> bool {
        self.tiles[t.0 as usize].worked
    }
    fn unit_owner(&self, u: UnitId) -> PlayerId {
        self.units[&u.get()].owner
    }
    fn unit_base(&self, u: UnitId) -> BaseUnitId {
        self.units[&u.get()].base
    }
    fn unit_promotions(&self, u: UnitId) -> PromotionSet {
        self.units[&u.get()].promotions
    }
    fn unit_wounded(&self, u: UnitId) -> bool {
        self.units[&u.get()].wounded
    }
    fn unit_embarked(&self, u: UnitId) -> bool {
        self.units[&u.get()].embarked
    }
    fn unit_set_up(&self, u: UnitId) -> bool {
        self.units[&u.get()].set_up
    }
    fn city_owner(&self, c: CityId) -> PlayerId {
        self.cities[&c.get()].owner
    }
    fn city_founder(&self, c: CityId) -> PlayerId {
        self.cities[&c.get()].founder
    }
    fn city_buildings(&self, c: CityId) -> BuildingSet {
        self.cities[&c.get()].buildings
    }
    fn city_is_capital(&self, c: CityId) -> bool {
        self.cities[&c.get()].capital
    }
    fn city_coastal(&self, c: CityId) -> bool {
        self.cities[&c.get()].coastal
    }
    fn city_annex_unhappiness(&self, c: CityId) -> bool {
        self.cities[&c.get()].annex
    }
    fn city_puppet(&self, c: CityId) -> bool {
        self.cities[&c.get()].puppet
    }
    fn city_connected_to_capital(&self, c: CityId) -> bool {
        self.cities[&c.get()].connected
    }
    fn city_garrisoned(&self, c: CityId) -> bool {
        self.cities[&c.get()].garrisoned
    }
    fn city_resisting(&self, c: CityId) -> bool {
        self.cities[&c.get()].resisting
    }
    fn city_razing(&self, c: CityId) -> bool {
        self.cities[&c.get()].razing
    }
    fn city_holy(&self, c: CityId) -> bool {
        self.cities[&c.get()].holy
    }
    fn city_majority_religion(&self, c: CityId) -> Option<ReligionId> {
        self.cities[&c.get()].religion
    }
    fn religion_is_major(&self, r: ReligionId) -> bool {
        self.religions[usize::from(r.0)].0
    }
    fn religion_is_enhanced(&self, r: ReligionId) -> bool {
        self.religions[usize::from(r.0)].1
    }
}

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn uid(n: u32) -> UnitId {
    UnitId::new(n).expect("an id")
}

fn cid(n: u32) -> CityId {
    CityId::new(n).expect("an id")
}

fn id<I: Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("{name}"))
}

/// Four civilizations: two majors (0 human, 1 AI), a city-state (2) and the barbarians (3).
/// 0 is at war with 1, has met 2, counts 2 a friend and has open borders from 2.
fn world(r: &Ruleset) -> Mock {
    let nation = |kind: NationKind| {
        r.nations().iter().find(|(_, n)| n.kind == kind).map(|(id, _)| id).expect("a nation")
    };
    let civ = |kind, human| Civ {
        nation: nation(kind),
        kind,
        human,
        religion: None,
        sees: ResourceSet::new(),
    };
    let mut m = Mock {
        civs: vec![
            civ(NationKind::Major, true),
            civ(NationKind::Major, false),
            civ(NationKind::CityState, false),
            civ(NationKind::Barbarian, false),
        ],
        religions: vec![(true, false), (false, false), (true, true)],
        ..Mock::default()
    };
    m.civs[1].nation = r.derived().major_nations[1];
    m.civs[0].religion = Some(ReligionId(2));
    m.war.insert((0, 1));
    m.met.insert((0, 2));
    m.friends.insert((0, 2));
    m.open.insert((2, 0));
    m
}

#[test]
fn civilization_leaves() {
    let r = shipped();
    let m = world(r);
    let eval = |l: CivLeaf, p, v| l.eval(&m, p, v);
    assert!(eval(CivLeaf::Human, P0, None) && !eval(CivLeaf::Human, P1, None));
    assert!(eval(CivLeaf::Ai, P1, None) && !eval(CivLeaf::Ai, P0, None));
    assert!(eval(CivLeaf::Kind(NationKind::CityState), P2, None));
    assert!(eval(CivLeaf::Kind(NationKind::Barbarian), P3, None));
    assert!(!eval(CivLeaf::Kind(NationKind::Major), P3, None));
    // Seen by player 0.
    assert!(eval(CivLeaf::Hostile, P1, Some(P0)) && !eval(CivLeaf::Hostile, P2, Some(P0)));
    assert!(eval(CivLeaf::Known, P2, Some(P0)) && eval(CivLeaf::Known, P0, Some(P0)));
    assert!(!eval(CivLeaf::Known, P3, Some(P0)));
    assert!(eval(CivLeaf::Friendly, P2, Some(P0)) && !eval(CivLeaf::Friendly, P1, Some(P0)));
    assert!(eval(CivLeaf::OpenBorders, P2, Some(P0)) && !eval(CivLeaf::OpenBorders, P1, Some(P0)));
    // Without a viewer, the tests against one fail, as Python's did.
    for l in [CivLeaf::Hostile, CivLeaf::Known, CivLeaf::Friendly, CivLeaf::OpenBorders] {
        assert!(!eval(l, P2, None), "{l:?}");
    }
    let n = m.civs[1].nation;
    let only: citar_engine::base::sets::NationSet = [n].into_iter().collect();
    assert!(eval(CivLeaf::Nation(only), P1, None) && !eval(CivLeaf::Nation(only), P0, None));
    // The seat is what the human and AI tests read (DESIGN.md 5.7).
    let human = filter::civ_filter(r, "Human player").expect("compiles");
    assert_eq!(human, Expr::Leaf(CivLeaf::Human));
    assert!(human.deps().contains(CondDeps::SEAT));
    assert!(filter::civ_filter(r, "AI player").expect("compiles").deps().contains(CondDeps::SEAT));
    assert_eq!(filter::civ_filter(r, "Major").expect("compiles").deps(), CondDeps::empty());
    let cs = filter::civ_filter(r, "City-State").expect("compiles");
    assert!(cs.eval(&mut |l| l.eval(&m, P2, None)) && !cs.eval(&mut |l| l.eval(&m, P0, None)));
    assert!(filter::civ_filter(r, "Nobody at all").is_err(), "a term that matches nothing");
    // The tests against the viewer read the diplomatic state, and open borders the turn they
    // end. Friendship also reads a city-state's influence, another civilization's state that no
    // class names (1a-07 decided): every class.
    let deps = |text| filter::civ_filter(r, text).expect("compiles").deps();
    assert_eq!(deps("Hostile"), CondDeps::WAR);
    assert_eq!(deps("Known"), CondDeps::WAR);
    assert_eq!(deps("Open Borders"), CondDeps::WAR | CondDeps::TURN);
    assert_eq!(deps("Friendly"), CondDeps::all());
}

fn unit_world(r: &Ruleset) -> Mock {
    let mut m = world(r);
    let unit = |owner, base: &str| Unit {
        owner,
        base: id(r, base),
        promotions: PromotionSet::new(),
        wounded: false,
        embarked: false,
        set_up: false,
    };
    m.units.insert(1, unit(P0, "Warrior"));
    m.units.insert(2, Unit { wounded: true, embarked: true, ..unit(P1, "Trireme") });
    m.units.insert(3, unit(P3, "Fighter"));
    m.units.insert(
        4,
        Unit {
            promotions: [id::<PromotionId>(r, "Drill I")].into_iter().collect(),
            set_up: true,
            ..unit(P2, "Catapult")
        },
    );
    m
}

#[test]
fn unit_leaves() {
    let r = shipped();
    let m = unit_world(r);
    let scope = UnitScope { this: Some(uid(1)), viewer: Some(P0) };
    let eval = |l: UnitLeaf, u| l.eval(&m, uid(u), scope);
    assert!(!eval(UnitLeaf::Other, 1) && eval(UnitLeaf::Other, 2));
    assert!(UnitLeaf::Other.eval(&m, uid(1), UnitScope::default()), "no unit in context");
    assert!(eval(UnitLeaf::Wounded, 2) && !eval(UnitLeaf::Wounded, 1));
    assert!(eval(UnitLeaf::Embarked, 2) && !eval(UnitLeaf::Embarked, 1));
    assert!(eval(UnitLeaf::SetUp, 4) && !eval(UnitLeaf::SetUp, 1));
    let warriors = [id::<BaseUnitId>(r, "Warrior")].into_iter().collect();
    assert!(eval(UnitLeaf::Base(warriors), 1) && !eval(UnitLeaf::Base(warriors), 2));
    let drill = [id::<PromotionId>(r, "Drill I")].into_iter().collect();
    assert!(eval(UnitLeaf::Promotion(drill), 4) && !eval(UnitLeaf::Promotion(drill), 1));
    // The owner seen by the viewer in context.
    assert!(
        eval(UnitLeaf::Owner(CivLeaf::Hostile), 2) && !eval(UnitLeaf::Owner(CivLeaf::Hostile), 4)
    );
    assert!(eval(UnitLeaf::Owner(CivLeaf::Kind(NationKind::Barbarian)), 3));
}

/// Compiles a unit filter of the ruleset's grammar and asks it of unit `u`.
fn unit_is(r: &Ruleset, m: &Mock, text: &str, u: u32, scope: UnitScope) -> bool {
    filter::unit_filter(r, text).expect(text).eval(&mut |l| l.eval(m, uid(u), scope))
}

#[test]
fn unit_filters_compile_and_answer() {
    let r = shipped();
    let m = unit_world(r);
    let s = UnitScope { this: Some(uid(1)), viewer: Some(P0) };
    let is = |text, u| unit_is(r, &m, text, u, s);
    assert!(
        is("{Military} {Land}", 1) && !is("{Military} {Land}", 2) && !is("{Military} {Land}", 3)
    );
    assert!(is("{Military} {Water}", 2));
    assert!(is("non-[Air]", 1) && !is("non-[Air]", 3));
    assert!(is("Aircraft", 3) && !is("Aircraft", 1), "the Fighter's type is tagged Aircraft");
    assert!(is("Wounded", 2) && is("wounded units", 2) && !is("Wounded", 1));
    assert!(is("Barbarian", 3) && is("Barbarians", 3) && !is("Barbarian", 1));
    assert!(!is("{Barbarian} {Water}", 3) && is("City-State", 4));
    assert!(is("Embarked", 2) && is("Non-City", 1) && is("All", 1));
    assert!(is("Drill I", 4), "a promotion by name");
    assert!(is("other", 2) && !is("other", 1));
    assert!(is("Set Up", 4) && !is("Set Up", 1));
    // Folded: the static parts are one set test, and the constants are gone.
    assert!(matches!(
        filter::unit_filter(r, "{Military} {Land}"),
        Ok(Expr::Leaf(UnitLeaf::Base(_)))
    ));
    assert_eq!(filter::unit_filter(r, "Non-City"), Ok(Expr::Const(true)));
    assert_eq!(filter::unit_filter(r, "All"), Ok(Expr::Const(true)));
    let err = filter::unit_filter(r, "{Military} {Flying Carpet}").expect_err("refused");
    assert!(err.contains("\"Flying Carpet\" matches no unit"), "{err}");
}

fn city_world(r: &Ruleset) -> Mock {
    let mut m = world(r);
    let city = |owner| City {
        owner,
        founder: owner,
        buildings: BuildingSet::new(),
        capital: false,
        coastal: false,
        annex: false,
        puppet: false,
        connected: false,
        garrisoned: false,
        resisting: false,
        razing: false,
        holy: false,
        religion: None,
    };
    m.cities.insert(
        1,
        City {
            capital: true,
            coastal: true,
            connected: true,
            garrisoned: true,
            holy: true,
            religion: Some(ReligionId(2)),
            buildings: [id::<BuildingId>(r, "The Great Library")].into_iter().collect(),
            ..city(P0)
        },
    );
    // Taken from player 1: annexed, resisting and unhappy about it.
    m.cities.insert(
        2,
        City {
            founder: P1,
            annex: true,
            resisting: true,
            religion: Some(ReligionId(1)),
            ..city(P0)
        },
    );
    m.cities.insert(3, City { founder: P0, puppet: true, razing: true, annex: true, ..city(P1) });
    m.cities.insert(4, city(P2));
    m
}

#[test]
fn city_leaves() {
    let r = shipped();
    let m = city_world(r);
    let eval = |l: CityLeaf, c, v| l.eval(&m, cid(c), v);
    let own = None;
    for (l, yes, no) in [
        (CityLeaf::Coastal, 1, 2),
        (CityLeaf::Capital, 1, 2),
        (CityLeaf::ConnectedToCapital, 1, 2),
        (CityLeaf::Garrisoned, 1, 2),
        (CityLeaf::Holy, 1, 2),
        (CityLeaf::Resisting, 2, 1),
        (CityLeaf::Razing, 3, 1),
        (CityLeaf::Puppeted, 3, 2),
        (CityLeaf::Annexed, 2, 3),
        (CityLeaf::NonOccupied, 3, 2),
        (CityLeaf::MajorReligion, 1, 2),
        (CityLeaf::EnhancedReligion, 1, 2),
        (CityLeaf::FollowsViewersReligion, 1, 2),
    ] {
        assert!(eval(l, yes, own), "{l:?} of city {yes}");
        assert!(!eval(l, no, own), "{l:?} of city {no}");
    }
    // The viewer is the owner unless named.
    assert!(eval(CityLeaf::Yours, 1, own) && !eval(CityLeaf::Yours, 3, Some(P0)));
    assert!(eval(CityLeaf::Foreign, 3, Some(P0)) && !eval(CityLeaf::Foreign, 3, own));
    assert!(eval(CityLeaf::Enemy, 3, Some(P0)) && !eval(CityLeaf::Enemy, 4, Some(P0)));
    assert!(eval(CityLeaf::NonEnemyForeign, 4, Some(P0)));
    assert!(
        !eval(CityLeaf::NonEnemyForeign, 3, Some(P0)) && !eval(CityLeaf::NonEnemyForeign, 1, own)
    );
    let wonders = [id::<BuildingId>(r, "The Great Library")].into_iter().collect();
    assert!(eval(CityLeaf::Has(wonders), 1, own) && !eval(CityLeaf::Has(wonders), 2, own));
    let cs = CityLeaf::Owner(CivLeaf::Kind(NationKind::CityState));
    assert!(eval(cs, 4, own) && !eval(cs, 1, own));
    assert!(eval(CityLeaf::Owner(CivLeaf::Hostile), 3, Some(P0)));
}

#[test]
fn city_filters_compile_and_answer() {
    let r = shipped();
    let m = city_world(r);
    let is = |text: &str, c, v| {
        filter::city_filter(r, text).expect(text).eval(&mut |l| l.eval(&m, cid(c), v))
    };
    assert!(is("in capital", 1, None) && is("Capital", 1, None) && !is("in capital", 2, None));
    assert!(is("non-[Puppeted]", 1, None) && !is("non-[Puppeted]", 3, None));
    assert!(is("in all cities with a world wonder", 1, None));
    assert!(is("in City-State cities", 4, None) && !is("in City-State cities", 1, None));
    assert!(is("in this city", 3, None) && is("in all cities", 3, None));
    assert!(is("in cities following this religion", 2, None), "scoped by the follower index");
    assert!(is("in foreign cities", 3, Some(P0)) && is("in non-enemy foreign cities", 4, Some(P0)));
    assert_eq!(filter::city_filter(r, "in all cities"), Ok(Expr::Const(true)));
    assert!(filter::city_filter(r, "in cities on the moon").is_err());
}

fn tile_world(r: &Ruleset) -> Mock {
    let mut m = world(r);
    let terrains = |names: &[&str]| names.iter().map(|n| id::<TerrainId>(r, n)).collect();
    let iron = id::<ResourceId>(r, "Iron");
    let fish = id::<ResourceId>(r, "Fish");
    m.civs[0].sees = [fish].into_iter().collect();
    m.tiles = vec![
        // 0: player 0's hill forest by the coast, with iron, a city centre and a road.
        Tile {
            terrains: terrains(&["Plains", "Hill", "Forest"]),
            coast: true,
            owner: Some(P0),
            friendly_to: [0].into_iter().collect(),
            resource: Some(iron),
            improvement: r.derived().known.city_center,
            route: Some(r.derived().known.road),
            worked: true,
            ..Tile::default()
        },
        // 1: unowned coast with fish.
        Tile { terrains: terrains(&["Coast"]), resource: Some(fish), ..Tile::default() },
        // 2: player 1's desert on a river, with a pillaged farm.
        Tile {
            terrains: terrains(&["Desert"]),
            river: true,
            fresh: true,
            owner: Some(P1),
            improvement: Some(id(r, "Farm")),
            pillaged: true,
            ..Tile::default()
        },
        // 3: the city-state's grassland, friendly to player 0.
        Tile {
            terrains: terrains(&["Grassland"]),
            owner: Some(P2),
            friendly_to: [0, 2].into_iter().collect(),
            ..Tile::default()
        },
        // 4: a mountain.
        Tile { terrains: terrains(&["Mountain"]), ..Tile::default() },
    ];
    m
}

#[test]
fn tile_leaves() {
    let r = shipped();
    let m = tile_world(r);
    let t = |i: u32| TileIdx(i);
    let eval = |l: TileLeaf, i, v| l.eval(&m, t(i), v);
    let hill: TerrainSet = [id::<TerrainId>(r, "Hill")].into_iter().collect();
    assert!(eval(TileLeaf::Terrains(hill), 0, None) && !eval(TileLeaf::Terrains(hill), 3, None));
    assert!(eval(TileLeaf::River, 2, None) && !eval(TileLeaf::River, 0, None));
    assert!(eval(TileLeaf::FreshWater, 2, None) && !eval(TileLeaf::FreshWater, 3, None));
    assert!(eval(TileLeaf::NextToCoast, 0, None) && !eval(TileLeaf::NextToCoast, 3, None));
    assert!(eval(TileLeaf::Unowned, 1, None) && !eval(TileLeaf::Unowned, 0, None));
    assert!(eval(TileLeaf::Yours, 0, Some(P0)) && !eval(TileLeaf::Yours, 0, None));
    assert!(!eval(TileLeaf::Yours, 1, Some(P0)));
    assert!(
        eval(TileLeaf::FriendlyLand, 3, Some(P0)) && !eval(TileLeaf::FriendlyLand, 2, Some(P0))
    );
    assert!(eval(TileLeaf::ForeignLand, 2, Some(P0)) && !eval(TileLeaf::ForeignLand, 3, Some(P0)));
    assert!(!eval(TileLeaf::ForeignLand, 2, None), "no viewer, no foreign land");
    assert!(eval(TileLeaf::EnemyLand, 2, Some(P0)) && !eval(TileLeaf::EnemyLand, 3, Some(P0)));
    assert!(!eval(TileLeaf::EnemyLand, 1, Some(P0)), "unowned is no enemy's");
    assert!(eval(TileLeaf::Owner(CivLeaf::Kind(NationKind::CityState)), 3, None));
    assert!(!eval(TileLeaf::Owner(CivLeaf::Kind(NationKind::CityState)), 1, None));
    // Resources, as the viewer sees them: player 0 sees fish but not iron.
    assert!(eval(TileLeaf::AnyResource, 1, Some(P0)) && !eval(TileLeaf::AnyResource, 0, Some(P0)));
    assert!(!eval(TileLeaf::AnyResource, 1, None), "`resource` needs a viewer");
    let iron: ResourceSet = [id::<ResourceId>(r, "Iron")].into_iter().collect();
    assert!(eval(TileLeaf::Resource(iron), 0, None), "no viewer sees everything");
    assert!(!eval(TileLeaf::Resource(iron), 0, Some(P0)));
    // Improvements, the pillaged ones not counting.
    assert!(eval(TileLeaf::Improved, 0, None) && !eval(TileLeaf::Improved, 2, None));
    assert!(eval(TileLeaf::Unimproved, 2, None) && eval(TileLeaf::Unimproved, 3, None));
    assert!(eval(TileLeaf::Pillaged, 2, None) && !eval(TileLeaf::Pillaged, 0, None));
    assert!(eval(TileLeaf::Worked, 0, None) && !eval(TileLeaf::Worked, 1, None));
    let road = [r.derived().known.road].into_iter().collect();
    let farm = [id::<ImprovementId>(r, "Farm")].into_iter().collect();
    assert!(eval(TileLeaf::Improvement(road), 0, None), "a route counts");
    assert!(!eval(TileLeaf::Improvement(farm), 2, None), "a pillaged farm does not");
    // The terrain-level leaves answer from TileFacts alone; the others do not.
    assert_eq!(TileLeaf::River.eval_terrain(&m, t(2)), Some(true));
    assert_eq!(TileLeaf::Worked.eval_terrain(&m, t(0)), None);
    // Friendly and foreign land read met, open borders, a city-state's influence and the
    // viewer's uniques (`FilterFacts::tile_friendly_to`): every class, as 1a-07 decided.
    let deps = |text| filter::tile_filter(r, text).expect("compiles").full.deps();
    assert_eq!(deps("Friendly Land"), CondDeps::all());
    assert_eq!(deps("Foreign Land"), CondDeps::all());
    assert_eq!(deps("Enemy Land"), CondDeps::WAR);
    assert_eq!(deps("Iron"), CondDeps::TECHS, "resource visibility");
}

#[test]
fn tile_filters_compile_and_answer() {
    let r = shipped();
    let m = tile_world(r);
    let is = |text: &str, i, v| {
        filter::tile_filter(r, text).expect(text).full.eval(&mut |l| l.eval(&m, TileIdx(i), v))
    };
    let terrain_is = |text: &str, i, v| {
        filter::tile_filter(r, text).expect(text).terrain.eval(&mut |l| l.eval(&m, TileIdx(i), v))
    };
    assert!(is("{your} {Coastal} {City center}", 0, Some(P0)));
    assert!(!is("{your} {Coastal} {City center}", 0, Some(P1)));
    assert!(!terrain_is("{your} {Coastal} {City center}", 0, Some(P0)), "no improvements");
    assert!(
        is("Hill", 0, None) && is("{Forest} {Hill}", 0, None) && !is("{Forest} {Hill}", 3, None)
    );
    assert!(is("Water", 1, None) && is("Land", 0, None) && !is("Water", 0, None));
    assert!(is("Coastal", 0, None) && !is("Coastal", 1, None), "Coastal is land by the coast");
    assert!(is("Water resource", 1, Some(P0)) && !is("Water resource", 0, Some(P0)));
    assert!(is("Fresh water", 2, None) && is("non-fresh water", 0, None));
    assert!(!is("non-fresh water", 2, None));
    assert!(is("Elevated", 4, None) && is("Elevated", 0, None) && !is("Elevated", 3, None));
    assert!(is("Featureless", 3, None) && !is("Featureless", 0, None));
    assert!(is("Open terrain", 3, None) && !is("Open terrain", 0, None));
    assert!(is("Rough", 0, None) && !is("Rough", 3, None), "map generation's word for it");
    assert!(is("Foreign Land", 2, Some(P0)) && is("Friendly", 3, Some(P0)));
    assert!(is("Enemy Land", 2, Some(P0)));
    assert!(is("Iron", 0, None) && !is("Iron", 0, Some(P0)), "the resource, as the viewer sees it");
    assert!(is("Strategic resource", 0, None) && is("Bonus resource", 1, Some(P0)));
    assert!(is("City center", 0, None) && !is("{unimproved} {Forest}", 0, None));
    assert!(is("unimproved", 2, None) && is("pillaged", 2, None) && is("worked", 0, None));
    assert!(is("Road", 0, None), "the route");
    assert!(is("All", 4, None));
    // Terrain-level filters read through TileFacts alone, as map generation reads them.
    let hill = filter::tile_filter(r, "{Plains} {Hill}").expect("compiles");
    assert!(hill.terrain_level);
    assert!(!filter::tile_filter(r, "Farm").expect("compiles").terrain_level);
    assert!(!filter::tile_filter(r, "Iron").expect("compiles").terrain_level);
    assert!(filter::tile_filter(r, "Elevated").expect("compiles").terrain_level);
    let err = filter::tile_filter(r, "{Hill} {Swamp of Doom}").expect_err("refused");
    assert!(err.contains("\"Swamp of Doom\" matches no tile"), "{err}");
}

#[test]
fn combatant_filters_ask_units_as_units_and_cities_as_cities() {
    let r = shipped();
    let mut m = unit_world(r);
    m.cities = city_world(r).cities;
    let f = filter::combatant_filter(r, "City").expect("compiles");
    let is = |f: &filter::CombatantFilter, c| match c {
        Combatant::Unit(u) => f.unit.eval(&mut |l| l.eval(&m, u, UnitScope::default())),
        Combatant::City(x) => f.city.eval(&mut |l| l.eval(&m, x, None)),
    };
    assert!(is(&f, Combatant::City(cid(1))) && !is(&f, Combatant::Unit(uid(1))));
    let cs = filter::combatant_filter(r, "City-States").expect("compiles");
    assert!(is(&cs, Combatant::City(cid(4))) && is(&cs, Combatant::Unit(uid(4))));
    assert!(!is(&cs, Combatant::City(cid(1))) && !is(&cs, Combatant::Unit(uid(1))));
    assert!(filter::combatant_filter(r, "Moon Base").is_err());
    // The loaded ruleset's own combatant filters answer the same way.
    let t = r.uniques();
    let (fid, _) = t
        .filters()
        .combatants()
        .iter()
        .find(|(fid, _)| t.combatant_filter(*fid) == "City")
        .expect("the ruleset's [City]");
    assert!(t.filters().combatant_matches(fid, &m, Combatant::City(cid(2))));
}

#[test]
fn the_ruleset_filters_are_compiled_and_reachable() {
    let r = shipped();
    let t = r.uniques();
    let f = t.filters();
    assert_eq!(f.units().len(), 29);
    assert!(f.tiles().len() > 90 && f.cities().len() >= 16 && !f.civs().is_empty());
    // Every dynamic filter folded to something that can hold.
    for (id, e) in f.units().iter() {
        assert_ne!(*e, Expr::Const(false), "{}", t.unit_filter(id));
    }
    for (id, e) in f.cities().iter() {
        assert_ne!(*e, Expr::Const(false), "{}", t.city_filter(id));
    }
    // The object filters keep only the kinds their texts match.
    let object = |text: &str| {
        t.objects().iter().map(|(_, o)| *o).find(|o| t.text(o.text) == text).expect(text)
    };
    let factory = object("Factory");
    assert!(factory.buildings.is_some() && factory.tiles.is_none());
    let desert = object("Desert");
    assert!(desert.tiles.is_some() && desert.buildings.is_none());
    let great = object("Great Improvement");
    assert!(great.tiles.is_some() && great.buildings.is_none());
    let fort = object("Fort");
    assert!(fort.improvements.is_some() && fort.tiles.is_none(), "a fort is no terrain");
    let land = object("Land");
    assert!(land.tiles.is_some() && land.improvements.is_none());
    // A static filter's bit test takes an id of its own domain.
    let (s, sf) = t
        .sets()
        .iter()
        .find(|(_, s)| s.domain == StaticDomain::Building && !s.members.is_empty())
        .expect("a building filter");
    for (b, _) in r.buildings().iter() {
        let b: BuildingId = b;
        assert_eq!(t.in_set(s, b), sf.members.contains(u32::from(b.0)));
    }
    // Improvements know their terrains, and start biases are filters map generation reads.
    let farm = &r.improvements()[id::<ImprovementId>(r, "Farm")];
    assert!(farm.terrains_can_be_built_on.contains(id(r, "Grassland")));
    assert!(!farm.terrains_can_be_built_on.contains(id(r, "Mountain")));
    let biased: Vec<_> = r.nations().as_slice().iter().flat_map(|n| n.start_bias.iter()).collect();
    assert!(biased.iter().any(|b| matches!(b, StartBias::Coast)));
    assert!(biased.iter().any(|b| matches!(b, StartBias::Avoid(_))));
    for b in biased {
        if let StartBias::Prefer(x) | StartBias::Avoid(x) = *b {
            assert!(f.tile(x.id()).terrain_level, "{}", t.tile_filter(x.id()));
        }
    }
}

// ---- Gate 3 on the real leaves: folding keeps a compiled filter's meaning ----------------------
//
// `tests/props.rs` checks folding's algorithm on leaves that merge exactly by construction. Here
// the leaves are the engine's own, whose `and` and `or` say which merges are exact (a tile's
// terrains merge under `Any` only, a unit's base unit both ways, two kinds of civilization to
// none): random trees over them, on random worlds, answer the same folded as not. Every set
// draws on the first four objects of its domain, as do the worlds, so that sets overlap and hit.

/// The objects of a four-bit mask: ids 0 to 3 of the domain.
fn pick<I: Id, S: FromIterator<I>>(mask: u8) -> S {
    (0..4).filter(|i| mask & (1 << i) != 0).filter_map(I::from_index).collect()
}

fn one<I: Id>(i: u8) -> I {
    I::from_index(usize::from(i)).expect("an id")
}

const KINDS: [NationKind; 3] = [NationKind::Major, NationKind::CityState, NationKind::Barbarian];

/// The pairs of players (0 to 3) a sixteen-bit mask names.
fn pairs(mask: u16) -> BTreeSet<(u8, u8)> {
    (0..16u8).filter(|i| mask & (1 << i) != 0).map(|i| (i / 4, i % 4)).collect()
}

fn arb_civ() -> impl Strategy<Value = Civ> {
    (0..3usize, 0..4u8, any::<bool>(), proptest::option::of(0..3u8), 0..16u8).prop_map(
        |(kind, nation, human, religion, sees)| Civ {
            nation: one(nation),
            kind: KINDS[kind],
            human,
            religion: religion.map(one),
            sees: pick(sees),
        },
    )
}

fn arb_tile() -> impl Strategy<Value = Tile> {
    let place = proptest::option::of(0..4u8);
    (0..16u8, any::<[bool; 5]>(), place.clone(), 0..16u8, place.clone(), place.clone(), place)
        .prop_map(
            |(
                terrains,
                [river, fresh, coast, pillaged, worked],
                owner,
                friendly,
                resource,
                improvement,
                route,
            )| Tile {
                terrains: pick(terrains),
                river,
                fresh,
                coast,
                owner: owner.map(PlayerId),
                friendly_to: (0..4u8).filter(|i| friendly & (1 << i) != 0).collect(),
                resource: resource.map(one),
                improvement: improvement.map(one),
                route: route.map(one),
                pillaged,
                worked,
            },
        )
}

fn arb_unit() -> impl Strategy<Value = Unit> {
    (0..4u8, 0..4u8, 0..16u8, any::<[bool; 3]>()).prop_map(
        |(owner, base, promotions, [wounded, embarked, set_up])| Unit {
            owner: PlayerId(owner),
            base: one(base),
            promotions: pick(promotions),
            wounded,
            embarked,
            set_up,
        },
    )
}

fn arb_city() -> impl Strategy<Value = City> {
    (0..4u8, 0..4u8, 0..16u8, any::<[bool; 10]>(), proptest::option::of(0..3u8)).prop_map(
        |(owner, founder, buildings, b, religion)| City {
            owner: PlayerId(owner),
            founder: PlayerId(founder),
            buildings: pick(buildings),
            capital: b[0],
            coastal: b[1],
            annex: b[2],
            puppet: b[3],
            connected: b[4],
            garrisoned: b[5],
            resisting: b[6],
            razing: b[7],
            holy: b[8],
            religion: religion.map(one),
        },
    )
}

/// A world of four civilizations, eight tiles, and six units and six cities (ids 1 to 6).
fn arb_world() -> impl Strategy<Value = Mock> {
    (
        proptest::collection::vec(arb_civ(), 4),
        proptest::collection::vec(arb_tile(), 8),
        proptest::collection::vec(arb_unit(), 6),
        proptest::collection::vec(arb_city(), 6),
        any::<[u16; 4]>(),
        any::<[(bool, bool); 3]>(),
    )
        .prop_map(|(civs, tiles, units, cities, [war, met, friends, open], religions)| Mock {
            tiles,
            civs,
            war: pairs(war),
            met: pairs(met),
            friends: pairs(friends),
            open: pairs(open),
            units: (1..).zip(units).collect(),
            cities: (1..).zip(cities).collect(),
            religions: religions.to_vec(),
        })
}

// A leaf is drawn by its number, so that a pair can share its variant. The leaves that merge
// (sets, owners, kinds) have several numbers each, so that lists often hold two of them for
// `fold` to merge.

const CIV_LEAVES: u8 = 12;

fn civ_leaf(which: u8, kind: usize, mask: u8) -> CivLeaf {
    match which {
        0 => CivLeaf::Human,
        1 => CivLeaf::Ai,
        2 => CivLeaf::OpenBorders,
        3 => CivLeaf::Friendly,
        4 => CivLeaf::Hostile,
        5 => CivLeaf::Known,
        6..=8 => CivLeaf::Kind(KINDS[kind]),
        _ => CivLeaf::Nation(pick(mask)),
    }
}

fn arb_civ_leaf_from(which: u8) -> impl Strategy<Value = CivLeaf> {
    (0..3usize, 0..16u8).prop_map(move |(kind, mask)| civ_leaf(which, kind, mask))
}

fn arb_civ_leaf() -> impl Strategy<Value = CivLeaf> {
    (0..CIV_LEAVES).prop_flat_map(arb_civ_leaf_from)
}

const TILE_LEAVES: u8 = 26;

fn tile_leaf(which: u8, mask: u8, civ: CivLeaf) -> TileLeaf {
    match which {
        0 => TileLeaf::River,
        1 => TileLeaf::FreshWater,
        2 => TileLeaf::NextToCoast,
        3 => TileLeaf::Unowned,
        4 => TileLeaf::Yours,
        5 => TileLeaf::ForeignLand,
        6 => TileLeaf::FriendlyLand,
        7 => TileLeaf::EnemyLand,
        8 => TileLeaf::AnyResource,
        9 => TileLeaf::Unimproved,
        10 => TileLeaf::Improved,
        11 => TileLeaf::Pillaged,
        12 => TileLeaf::Worked,
        13..=16 => TileLeaf::Terrains(pick(mask)),
        17..=19 => TileLeaf::Resource(pick(mask)),
        20..=22 => TileLeaf::Improvement(pick(mask)),
        _ => TileLeaf::Owner(civ),
    }
}

fn arb_tile_leaf_from(which: u8) -> impl Strategy<Value = TileLeaf> {
    (0..16u8, arb_civ_leaf()).prop_map(move |(mask, civ)| tile_leaf(which, mask, civ))
}

fn arb_tile_leaf() -> impl Strategy<Value = TileLeaf> {
    (0..TILE_LEAVES).prop_flat_map(arb_tile_leaf_from)
}

const UNIT_LEAVES: u8 = 13;

fn unit_leaf(which: u8, mask: u8, civ: CivLeaf) -> UnitLeaf {
    match which {
        0 => UnitLeaf::Other,
        1 => UnitLeaf::Wounded,
        2 => UnitLeaf::Embarked,
        3 => UnitLeaf::SetUp,
        4..=6 => UnitLeaf::Base(pick(mask)),
        7..=9 => UnitLeaf::Promotion(pick(mask)),
        _ => UnitLeaf::Owner(civ),
    }
}

fn arb_unit_leaf_from(which: u8) -> impl Strategy<Value = UnitLeaf> {
    (0..16u8, arb_civ_leaf()).prop_map(move |(mask, civ)| unit_leaf(which, mask, civ))
}

fn arb_unit_leaf() -> impl Strategy<Value = UnitLeaf> {
    (0..UNIT_LEAVES).prop_flat_map(arb_unit_leaf_from)
}

const CITY_LEAVES: u8 = 25;

fn city_leaf(which: u8, mask: u8, civ: CivLeaf) -> CityLeaf {
    match which {
        0 => CityLeaf::Yours,
        1 => CityLeaf::Coastal,
        2 => CityLeaf::Capital,
        3 => CityLeaf::NonOccupied,
        4 => CityLeaf::ConnectedToCapital,
        5 => CityLeaf::Garrisoned,
        6 => CityLeaf::MajorReligion,
        7 => CityLeaf::EnhancedReligion,
        8 => CityLeaf::NonEnemyForeign,
        9 => CityLeaf::Enemy,
        10 => CityLeaf::Foreign,
        11 => CityLeaf::Annexed,
        12 => CityLeaf::Puppeted,
        13 => CityLeaf::Resisting,
        14 => CityLeaf::Razing,
        15 => CityLeaf::Holy,
        16 => CityLeaf::FollowsViewersReligion,
        17..=20 => CityLeaf::Has(pick(mask)),
        _ => CityLeaf::Owner(civ),
    }
}

fn arb_city_leaf_from(which: u8) -> impl Strategy<Value = CityLeaf> {
    (0..16u8, arb_civ_leaf()).prop_map(move |(mask, civ)| city_leaf(which, mask, civ))
}

fn arb_city_leaf() -> impl Strategy<Value = CityLeaf> {
    (0..CITY_LEAVES).prop_flat_map(arb_city_leaf_from)
}

/// Two leaves, of the same variant half the time: the pairs `Leaf::and` and `Leaf::or` merge.
fn arb_pair<L: Clone + std::fmt::Debug, S: Strategy<Value = L>>(
    n: u8,
    from: impl Fn(u8) -> S + Copy,
) -> impl Strategy<Value = (L, L)> {
    (0..n, 0..n, any::<bool>())
        .prop_flat_map(move |(a, b, same)| (from(a), from(if same { a } else { b })))
}

/// The leaf contract (`Leaf`): a constant holds everywhere, and a merged leaf answers as the
/// conjunction or disjunction of the two, in every context `ask` evaluates.
fn merges_exactly<L: citar_engine::unique::filter::Leaf + std::fmt::Debug>(
    a: &L,
    b: &L,
    contexts: usize,
    ask: impl Fn(&L, usize) -> bool,
) -> Result<(), TestCaseError> {
    let joined = a.and(b);
    let either = a.or(b);
    for i in 0..contexts {
        let (x, y) = (ask(a, i), ask(b, i));
        if let Some(k) = a.constant() {
            prop_assert_eq!(x, k, "{:?} is constant {} but not in context {}", a, k, i);
        }
        if let Some(j) = &joined {
            prop_assert_eq!(
                ask(j, i),
                x && y,
                "{:?} and {:?} merged to {:?}, context {}",
                a,
                b,
                j,
                i
            );
        }
        if let Some(e) = &either {
            prop_assert_eq!(
                ask(e, i),
                x || y,
                "{:?} or {:?} merged to {:?}, context {}",
                a,
                b,
                e,
                i
            );
        }
    }
    Ok(())
}

/// Random trees over `leaf`, with constants, as the compiler builds them before folding.
fn arb_tree<L: Clone + std::fmt::Debug + 'static>(
    leaf: impl Strategy<Value = L> + 'static,
) -> impl Strategy<Value = Expr<L>> {
    let base =
        prop_oneof![1 => any::<bool>().prop_map(Expr::Const), 6 => leaf.prop_map(Expr::Leaf)];
    base.prop_recursive(5, 64, 5, |inner| {
        prop_oneof![
            inner.clone().prop_map(|e| Expr::Not(Box::new(e))),
            proptest::collection::vec(inner.clone(), 0..5).prop_map(|v| Expr::All(v.into())),
            proptest::collection::vec(inner, 0..5).prop_map(|v| Expr::Any(v.into())),
        ]
    })
}

/// Who asks: nobody, or each of the four civilizations.
const VIEWERS: [Option<PlayerId>; 5] = [None, Some(P0), Some(P1), Some(P2), Some(P3)];

/// The folded tree is no deeper, and folding it again changes nothing.
fn folds_cleanly<L: citar_engine::unique::filter::Leaf + std::fmt::Debug>(
    e: &Expr<L>,
    folded: &Expr<L>,
) -> Result<(), TestCaseError> {
    prop_assert!(folded.depth() <= e.depth(), "{:?} folded deeper: {:?}", e, folded);
    prop_assert_eq!(&folded.clone().fold(), folded);
    Ok(())
}

proptest! {
    #[test]
    fn civilization_leaves_merge_exactly((a, b) in arb_pair(CIV_LEAVES, arb_civ_leaf_from), m in arb_world()) {
        // Each civilization, seen by each viewer.
        merges_exactly(&a, &b, 4 * VIEWERS.len(), |l, i| {
            l.eval(&m, PlayerId(u8::try_from(i / VIEWERS.len()).expect("small")), VIEWERS[i % VIEWERS.len()])
        })?;
    }

    #[test]
    fn tile_leaves_merge_exactly((a, b) in arb_pair(TILE_LEAVES, arb_tile_leaf_from), m in arb_world()) {
        // Each tile, seen by each viewer, then each tile from its terrain alone.
        let seen = 8 * VIEWERS.len();
        merges_exactly(&a, &b, seen + 8, |l, i| {
            if i < seen {
                let t = TileIdx(u32::try_from(i / VIEWERS.len()).expect("small"));
                l.eval(&m, t, VIEWERS[i % VIEWERS.len()])
            } else {
                let t = TileIdx(u32::try_from(i - seen).expect("small"));
                l.eval_terrain(&m, t).unwrap_or(false)
            }
        })?;
    }

    #[test]
    fn unit_leaves_merge_exactly((a, b) in arb_pair(UNIT_LEAVES, arb_unit_leaf_from), m in arb_world()) {
        // Each unit, with no unit or unit 1 in context, seen by each viewer.
        let n = VIEWERS.len();
        merges_exactly(&a, &b, 6 * 2 * n, |l, i| {
            let u = uid(u32::try_from(1 + i / (2 * n)).expect("small"));
            let this = (i / n % 2 == 1).then(|| uid(1));
            l.eval(&m, u, UnitScope { this, viewer: VIEWERS[i % n] })
        })?;
    }

    #[test]
    fn city_leaves_merge_exactly((a, b) in arb_pair(CITY_LEAVES, arb_city_leaf_from), m in arb_world()) {
        // Each city, seen by each viewer.
        merges_exactly(&a, &b, 6 * VIEWERS.len(), |l, i| {
            let c = cid(u32::try_from(1 + i / VIEWERS.len()).expect("small"));
            l.eval(&m, c, VIEWERS[i % VIEWERS.len()])
        })?;
    }

    #[test]
    fn folding_keeps_a_civilization_filters_meaning(e in arb_tree(arb_civ_leaf()), m in arb_world()) {
        let folded = e.clone().fold();
        for p in [P0, P1, P2, P3] {
            for v in VIEWERS {
                let ask = |x: &Expr<CivLeaf>| x.eval(&mut |l| l.eval(&m, p, v));
                prop_assert_eq!(ask(&folded), ask(&e), "civ {:?} seen by {:?}: {:?}", p, v, folded);
            }
        }
        folds_cleanly(&e, &folded)?;
    }

    #[test]
    fn folding_keeps_a_tile_filters_meaning(e in arb_tree(arb_tile_leaf()), m in arb_world()) {
        let folded = e.clone().fold();
        for t in 0..8 {
            for v in VIEWERS {
                let ask = |x: &Expr<TileLeaf>| x.eval(&mut |l| l.eval(&m, TileIdx(t), v));
                prop_assert_eq!(ask(&folded), ask(&e), "tile {} seen by {:?}: {:?}", t, v, folded);
            }
            // Map generation's reading, from the terrain alone.
            let terrain = |x: &Expr<TileLeaf>| {
                x.eval(&mut |l| l.eval_terrain(&m, TileIdx(t)).unwrap_or(false))
            };
            prop_assert_eq!(terrain(&folded), terrain(&e), "tile {} from its terrain: {:?}", t, folded);
        }
        folds_cleanly(&e, &folded)?;
    }

    #[test]
    fn folding_keeps_a_unit_filters_meaning(e in arb_tree(arb_unit_leaf()), m in arb_world()) {
        let folded = e.clone().fold();
        for u in 1..=6 {
            for this in [None, Some(uid(1)), Some(uid(2))] {
                for viewer in VIEWERS {
                    let scope = UnitScope { this, viewer };
                    let ask = |x: &Expr<UnitLeaf>| x.eval(&mut |l| l.eval(&m, uid(u), scope));
                    prop_assert_eq!(ask(&folded), ask(&e), "unit {} in {:?}: {:?}", u, scope, folded);
                }
            }
        }
        folds_cleanly(&e, &folded)?;
    }

    /// A unit's facts, taken for a trigger about a unit that is gone, answer every filter as the
    /// unit itself did with nothing in context.
    #[test]
    fn a_units_facts_answer_as_the_unit(e in arb_tree(arb_unit_leaf()), m in arb_world()) {
        for u in 1..=6 {
            let facts = UnitFacts::of(&m, uid(u));
            for viewer in VIEWERS {
                let live = e.eval(&mut |l| l.eval(&m, uid(u), UnitScope { this: None, viewer }));
                let copy = e.eval(&mut |l| l.eval_facts(&m, &facts, viewer));
                prop_assert_eq!(live, copy, "unit {} seen by {:?}: {:?}", u, viewer, e);
            }
        }
    }

    #[test]
    fn folding_keeps_a_city_filters_meaning(e in arb_tree(arb_city_leaf()), m in arb_world()) {
        let folded = e.clone().fold();
        for c in 1..=6 {
            for v in VIEWERS {
                let ask = |x: &Expr<CityLeaf>| x.eval(&mut |l| l.eval(&m, cid(c), v));
                prop_assert_eq!(ask(&folded), ask(&e), "city {} seen by {:?}: {:?}", c, v, folded);
            }
        }
        folds_cleanly(&e, &folded)?;
    }
}

// ---- Gate 4: region conditionals only where map generation reads them --------------------------

#[test]
fn a_region_conditional_on_an_effect_is_refused() {
    let errs = load_edited("ruleset/buildings.json", |v| {
        let temple = v["Temple"]["uniques"].as_array_mut().expect("uniques");
        temple.push(json!("[+1 Gold] <in [Hill] Regions>"));
    })
    .expect_err("refused");
    assert!(errs.has(RulesetErrorKind::UniqueModifier), "{errs}");
    assert!(errs.to_string().contains("map-generation condition"), "{errs}");
    let errs = load_edited("ruleset/buildings.json", |v| {
        let temple = v["Temple"]["uniques"].as_array_mut().expect("uniques");
        temple.push(json!("[+1 Gold] <in all except [Desert] Regions>"));
    })
    .expect_err("refused");
    assert!(errs.has(RulesetErrorKind::UniqueModifier), "{errs}");
    // Where map generation reads them, they load: the shipped resources have them.
    let r = shipped();
    let regions = r
        .gen_tables()
        .resources
        .iter()
        .flat_map(|(_, g)| g.weights.iter())
        .filter(|w| !w.cond.regions.is_empty())
        .count();
    assert!(regions > 50, "{regions} weights only in a region");
}

// ---- Gate 5: every map-generation unique lands in a table --------------------------------------

#[test]
fn every_map_generation_unique_lands_in_a_table() {
    let r = shipped();
    let t = r.uniques();
    let g = r.gen_tables();
    let mapgen: Vec<_> = t
        .iter()
        .filter(|(id, _)| t.meta(*id).role == Role::Mapgen)
        .filter(|(id, _)| !matches!(t.meta(*id).source, Source::Temporary(_)))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(mapgen.len(), 322, "the shipped map-generation uniques");
    assert_eq!(g.placed, mapgen, "each one, in id order");
    // Spot checks of each table.
    let terrain = |name| &g.terrains[id::<TerrainId>(r, name)];
    assert!(terrain("Mountain").chains && terrain("Hill").groups && terrain("Forest").vegetation);
    assert_eq!(terrain("Mountain").fertility.fixed, Some(-2));
    assert_eq!(terrain("Grassland").fertility.add, 3);
    assert_eq!(terrain("Grassland").climates.len(), 8);
    assert!(matches!(terrain("Desert").changes[0].near, Near::River));
    assert!(terrain("Coast").coastal_water);
    assert_eq!(terrain("Snow").blocks_resources.len(), 1);
    let resource = |name| &g.resources[id::<ResourceId>(r, name)];
    assert_eq!(resource("Silver").not_where.len(), 5);
    assert!(!resource("Silver").never);
    assert_eq!(resource("Furs").city_state_weight, Some(15));
    assert_eq!(resource("Oil").amounts.len(), 1);
    let wonder = |name| &g.wonders[id::<TerrainId>(r, name)];
    assert_eq!(wonder("Great Barrier Reef").group, Some((2, 2)));
    assert_eq!(wonder("Mount Fuji").neighbours.len(), 6);
    assert_eq!(wonder("Krakatoa").converts.len(), 1);
    // AI weights on techs, policies, beliefs and promotions: 76 in all.
    let ai = &g.ai;
    let weights: usize = ai.techs.iter().map(|(_, w)| w.len()).sum::<usize>()
        + ai.policies.iter().map(|(_, w)| w.len()).sum::<usize>()
        + ai.beliefs.iter().map(|(_, w)| w.len()).sum::<usize>()
        + ai.promotions.iter().map(|(_, w)| w.len()).sum::<usize>();
    assert_eq!(weights, 76, "DESIGN.md 5.10");
    // Inert uniques, each with its reason.
    assert_eq!(g.inert.len(), 64, "the shipped inert uniques");
    for x in &g.inert {
        let ty = t.meta(x.unique).ty.expect("a type");
        assert_eq!(ty.role(), Some(Role::Inert));
        assert_eq!(Some(x.reason), ty.info().support.and_then(|s| s.reason), "{}", ty.name());
        assert!(!x.reason.is_empty());
    }
    // Milestones.
    let victory =
        |name: &str| r.victories().as_slice().iter().find(|v| &*v.name == name).expect("a victory");
    let apollo = id::<BuildingId>(r, "Apollo Program");
    assert_eq!(victory("Scientific").milestones[0].milestone, Milestone::Build(apollo));
    assert_eq!(victory("Scientific").milestones[1].milestone, Milestone::SpaceshipComplete);
    assert_eq!(victory("Cultural").milestones[0].milestone, Milestone::CompletePolicyBranches(5));
    assert_eq!(victory("Domination").milestones[0].milestone, Milestone::CaptureAllCapitals);
    assert_eq!(&*victory("Diplomatic").milestones[1].text, "Win diplomatic vote");
}

#[test]
fn map_generation_reads_its_conditions_from_the_terrain() {
    let r = shipped();
    let m = tile_world(r);
    let g = r.gen_tables();
    let filters = r.uniques().filters();
    // Silver does not generate on tundra hills, grassland by fresh water, and so on.
    let silver = &g.resources[id::<ResourceId>(r, "Silver")];
    let blocked = |i: u32| silver.not_where.iter().any(|c| c.holds(filters, &m, TileIdx(i)));
    assert!(!blocked(4) && !blocked(3));
    let snow_hill: TerrainSet = [id::<TerrainId>(r, "Snow"), id(r, "Hill")].into_iter().collect();
    let mut hills = tile_world(r);
    hills.tiles[3].terrains = snow_hill;
    let snow = &g.terrains[id::<TerrainId>(r, "Snow")];
    assert!(snow.blocks_resources[0].holds(filters, &hills, TileIdx(3)));
    assert!(!snow.blocks_resources[0].holds(filters, &hills, TileIdx(4)));
}

// ---- Refusals ------------------------------------------------------------------------------------

#[test]
fn filters_that_match_nothing_are_refused() {
    let errs = load_edited("ruleset/buildings.json", |v| {
        let temple = v["Temple"]["uniques"].as_array_mut().expect("uniques");
        temple.push(json!("[+1 Gold] from [{Hill} {Moonscape}] tiles [in this city]"));
    })
    .expect_err("refused");
    assert!(errs.has(RulesetErrorKind::Filter), "{errs}");
    let e = errs.0.iter().find(|e| e.kind == RulesetErrorKind::Filter).expect("one");
    assert_eq!((&*e.file, &*e.object), ("ruleset/buildings.json", "Temple"));
    assert!(e.text.contains("\"Moonscape\" matches no tile"), "{e}");
    // A static filter too.
    let errs = load_edited("ruleset/buildings.json", |v| {
        let temple = v["Temple"]["uniques"].as_array_mut().expect("uniques");
        temple.push(json!("[+10]% Production when constructing [Hovercar] units [in this city]"));
    })
    .expect_err("refused");
    assert!(errs.to_string().contains("\"Hovercar\" matches no unit"), "{errs}");
    // A filter nested too deep.
    let deep = format!("{}Hill{}", "non-[".repeat(20), "]".repeat(20));
    let errs = load_edited("ruleset/buildings.json", |v| {
        let temple = v["Temple"]["uniques"].as_array_mut().expect("uniques");
        temple.push(json!(format!("[+1 Gold] from [{deep}] tiles [in this city]")));
    })
    .expect_err("refused");
    assert!(errs.to_string().contains("nests deeper"), "{errs}");
    // An improvement's terrain that is no terrain.
    let errs = load_edited("ruleset/improvements.json", |v| {
        let farm = v["Farm"]["terrainsCanBeBuiltOn"].as_array_mut().expect("terrains");
        farm.push(json!("Cloud"));
    })
    .expect_err("refused");
    assert!(errs.to_string().contains("\"Cloud\" matches no terrain"), "{errs}");
}

/// The ruleset with `text` added to the Temple's uniques.
fn temple_with(text: &str) -> Result<Ruleset, citar_engine::rules::RulesetErrors> {
    load_edited("ruleset/buildings.json", |v| {
        v["Temple"]["uniques"].as_array_mut().expect("uniques").push(json!(text));
    })
}

#[test]
fn a_filter_read_from_the_terrain_alone_must_match_there() {
    // A building's `Must be on` and `Must not be on`, and `in cities on [..] tiles`, read the
    // city's tile by its terrain alone (`cities.py:386, 1231-1233`): a farm there never counts,
    // so the building could never be built, or never be barred.
    for text in [
        "Must be on [Farm]",
        "Must not be on [Farm]",
        "[+1 Gold] in cities on [{Farm} {Land}] tiles",
    ] {
        let errs = temple_with(text).expect_err(text);
        assert!(errs.has(RulesetErrorKind::Filter), "{text}: {errs}");
        let e = errs.0.iter().find(|e| e.kind == RulesetErrorKind::Filter).expect("one");
        assert_eq!((&*e.file, &*e.object), ("ruleset/buildings.json", "Temple"));
        assert!(e.text.contains("\"Farm\" matches no tile by its terrain alone"), "{e}");
    }
    // Read in full, the same filter matches: a farm may stand next to the city.
    temple_with("Must be next to [Farm]").expect("a full tile filter");
    // The shipped `Must be on [River]` reads the terrain, and loads.
    assert!(
        shipped()
            .uniques()
            .iter()
            .any(|(id, _)| { shipped().uniques().meta(id).ty == Some(UniqueType::MustBeOn) })
    );
}

#[test]
fn an_object_filter_that_means_another_kind_is_refused() {
    // `non-[Temple]` as tiles is every tile, though `Temple` names no tile: Python applied such a
    // unique to every tile and every other building. The text is unclear, and does not load.
    for text in ["[+10]% [Gold] from every [non-[Temple]]", "[+1 Gold] from every [non-[Temple]]"] {
        let errs = temple_with(text).expect_err(text);
        assert!(errs.has(RulesetErrorKind::Filter), "{text}: {errs}");
        let msg = errs.to_string();
        assert!(msg.contains("unclear") && msg.contains("\"Temple\" matches no tile"), "{msg}");
    }
    // A worker's `non-[Farm]` is every terrain but builds only the improvements it names.
    let errs = load_edited("ruleset/units.json", |v| {
        let worker = v["Worker"]["uniques"].as_array_mut().expect("uniques");
        worker.push(json!("Can build [non-[Farm]] improvements on tiles"));
    })
    .expect_err("refused");
    assert!(errs.to_string().contains("\"Farm\" matches no tile"), "{errs}");
    // A kind the text never selects is dropped, as before; with none left, it does not load.
    let errs = temple_with("[+10]% [Gold] from every [{Temple} {Desert}]").expect_err("refused");
    assert!(errs.to_string().contains("matches nothing of any kind"), "{errs}");
    temple_with("[+10]% [Gold] from every [Desert]").expect("tiles only");
    temple_with("[+10]% [Gold] from every [{Wonder} {Culture}]").expect("loads");
}

#[test]
fn map_generation_refuses_what_it_cannot_read() {
    // A filter that asks more than the terrain.
    let errs = load_edited("ruleset/resources.json", |v| {
        let oil = v["Oil"]["uniques"].as_array_mut().expect("uniques");
        oil.push(json!("Deposits in [Farm] tiles always provide [4] resources"));
    })
    .expect_err("refused");
    assert!(errs.has(RulesetErrorKind::Filter), "{errs}");
    assert!(errs.to_string().contains("terrain alone"), "{errs}");
    // A condition map generation has no way to read.
    let errs = load_edited("ruleset/resources.json", |v| {
        let oil = v["Oil"]["uniques"].as_array_mut().expect("uniques");
        oil.push(json!("Generated with weight [5] <when not at war>"));
    })
    .expect_err("refused");
    assert!(errs.to_string().contains("map generation cannot read"), "{errs}");
    // A natural wonder's rule on a resource.
    let errs = load_edited("ruleset/resources.json", |v| {
        let oil = v["Oil"]["uniques"].as_array_mut().expect("uniques");
        oil.push(json!("Occurs in groups of [2] to [3] tiles"));
    })
    .expect_err("refused");
    assert!(errs.to_string().contains("on a natural wonder only"), "{errs}");
    // A start bias map generation cannot read.
    let errs = load_edited("ruleset/nations.json", |v| {
        let first = v.as_object_mut().and_then(|o| o.values_mut().next()).expect("a nation");
        first["startBias"] = json!(["Farm"]);
    })
    .expect_err("refused");
    assert!(errs.has(RulesetErrorKind::Filter), "{errs}");
}

#[test]
fn an_unknown_milestone_is_refused() {
    let errs = load_edited("ruleset/victories.json", |v| {
        let first = v.as_object_mut().and_then(|o| o.values_mut().next()).expect("a victory");
        first["milestones"].as_array_mut().expect("milestones").push(json!("Win the lottery"));
    })
    .expect_err("refused");
    assert!(errs.has(RulesetErrorKind::Invalid), "{errs}");
    assert!(errs.to_string().contains("no milestone the engine knows"), "{errs}");
    let errs = load_edited("ruleset/victories.json", |v| {
        let first = v.as_object_mut().and_then(|o| o.values_mut().next()).expect("a victory");
        first["milestones"] = json!(["Build [Space Elevator]"]);
    })
    .expect_err("refused");
    assert!(errs.has(RulesetErrorKind::UnknownReference), "{errs}");
}

#[test]
fn an_ai_weight_goes_on_something_the_ai_chooses() {
    let errs = load_edited("ruleset/terrains.json", |v| {
        let hill = v["Hill"]["uniques"].as_array_mut().expect("uniques");
        hill.push(json!("[+50]% weight to this choice for AI decisions"));
    })
    .expect_err("refused");
    assert!(errs.to_string().contains("something the AI chooses"), "{errs}");
    // The type exists, with its placeholder, for the weights the AI does read.
    assert_eq!(
        UniqueType::AiChoiceWeight.placeholder(),
        "[]% weight to this choice for AI decisions"
    );
}

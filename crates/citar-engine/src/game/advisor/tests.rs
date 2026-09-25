//! The advisor and the what-if on the small test game: a puppet's picks (gate 3), and a city in
//! We Love The King Day whose owner one building would make happy.

use super::*;
use crate::base::ids::TechId;
use crate::game::cities::queue::auto_pick_production;
use crate::game::cities::what_if::{reach_for_test, toggle_building_for_test, what_if_building};
use crate::game::core::testing;
use crate::game::derive::rev::{BitEq as _, CityTouch, PlayerTouch};
use crate::state::TileClaim;

const ROME: PlayerId = PlayerId(0);

/// Rome's city on `t`, owning its neighbours, at `pop`, its citizens placed.
fn city(g: &mut Game, t: TileIdx, name: &str, pop: u16) -> CityId {
    let c = testing::city(g, ROME, t, name);
    for n in g.grid().neighbors(t).collect::<Vec<_>>() {
        g.set_tile_owner(n, TileClaim::city(ROME, c)).expect("a tile");
    }
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.pop = pop;
    }
    g.settle();
    c
}

/// Every tech, for Rome.
fn every_tech(g: &mut Game) {
    let all: Vec<TechId> = g.rules().techs().ids().collect();
    if let Some(p) = g.player_mut(ROME, PlayerTouch::INDEX) {
        for t in all {
            p.tech.known.insert(t);
        }
    }
    g.settle();
}

fn clean(g: &mut Game) {
    assert!(g.take_violations().is_empty());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

#[test]
fn a_puppet_builds_a_building_that_is_no_wonder_or_gold() {
    let mut g = testing::duel();
    let c = city(&mut g, TileIdx(22), "Roma", 6);
    every_tech(&mut g);
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.puppet = true;
    }
    g.settle();
    let r = g.rules();
    let picked = auto_pick(&g, c).expect("a pick");
    let Constructible::Building(b) = picked else { panic!("{picked:?} is no building") };
    assert!(!r.buildings()[b].any_wonder, "{} is a wonder", r.buildings()[b].name);
    assert!(buildable_items(&g, c).contains(picked));
    // What automatic production starts is at the front of its queue.
    assert_eq!(auto_pick_production(&mut g, c), Some(picked));
    assert_eq!(g.city(c).and_then(|x| x.queue.first().copied()), Some(picked));
    // With every building it could build built, a puppet converts its production to gold, where
    // a city of its own would build a unit, a wonder or science.
    for _ in 0..20 {
        let items = buildable_items(&g, c);
        if items.buildings.is_empty() {
            break;
        }
        for b in items.buildings.iter() {
            toggle_building_for_test(&mut g, c, b, true);
        }
        g.settle();
    }
    let rest = buildable_items(&g, c);
    assert!(rest.buildings.is_empty(), "left: {:?}", rest.buildings);
    assert!(rest.gold, "{rest:?}");
    assert_eq!(auto_pick(&g, c), Some(Constructible::Perpetual(Perpetual::Gold)));
    assert_eq!(puppet_pick(&g, c), Some(Constructible::Perpetual(Perpetual::Gold)));
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.puppet = false;
    }
    g.settle();
    let own = auto_pick(&g, c).expect("a pick");
    assert!(
        matches!(own, Constructible::Unit(_))
            || matches!(own, Constructible::Building(b) if g.rules().buildings()[b].any_wonder),
        "{own:?}"
    );
    clean(&mut g);
}

#[test]
fn a_city_in_we_love_the_king_day_asks_whether_the_building_makes_its_owner_happy() {
    let mut g = testing::duel();
    let a = city(&mut g, TileIdx(22), "Roma", 3);
    let b = city(&mut g, TileIdx(26), "Antium", 1);
    every_tech(&mut g);
    if let Some(x) = g.city_mut(a, CityTouch::CORE) {
        x.wltkd = 10;
    }
    g.settle();
    // Antium grows until Rome is one or two short of content.
    let mut pop = 1;
    while memo::happiness_total(&g, ROME) >= 0 && pop < 60 {
        pop += 1;
        if let Some(x) = g.city_mut(b, CityTouch::CORE) {
            x.pop = pop;
        }
        g.settle();
    }
    let unhappy = memo::happiness_total(&g, ROME);
    assert!((-2..0).contains(&unhappy), "happiness {unhappy}");
    assert!(memo::city_stats(&g, a).food() > 0.0, "Roma has no surplus");
    let colosseum = g.rules().lookup::<BuildingId>("Colosseum").expect("a colosseum");
    let reach = reach_for_test(&g, a, colosseum).expect("a what-if");
    assert!(reach.happiness, "{reach:?}");
    let before = g.digest().expect("a digest");
    let w = what_if_building(&g, a, colosseum).expect("a what-if");
    assert_eq!(g.digest().expect("a digest"), before);
    toggle_building_for_test(&mut g, a, colosseum, true);
    let with = memo::city_stats(&g, a).total;
    assert!(memo::happiness_total(&g, ROME) >= 0, "the colosseum makes Rome content");
    toggle_building_for_test(&mut g, a, colosseum, false);
    assert!(w.after.bit_eq(&with), "what-if {:?}, built {with:?}", w.after);
    // The celebration's food comes with it: a quarter more of the surplus.
    let d = w.delta();
    assert!(d[Stat::Food] > 0.0 && d[Stat::Happiness] > 0.0, "{d:?}");
    // Antium is computed again only for a building that adds to Rome's index what a city's
    // yields or happiness may read; a Monument adds only `Destroyed when the city is captured`.
    let rules: &'static crate::rules::Ruleset = g.rules;
    let adv = &rules.derived().advisor;
    let monument = rules.lookup::<BuildingId>("Monument").expect("a monument");
    let reach = reach_for_test(&g, a, monument).expect("a what-if");
    assert!(reach.civ && reach.happiness && !reach.others, "{reach:?}");
    let lacks = |x: BuildingId| g.city(a).is_some_and(|y| !y.buildings.contains(x));
    let wide = rules.buildings().ids().find(|&x| adv.widens.contains(x) && lacks(x));
    let reach = reach_for_test(&g, a, wide.expect("a building that widens")).expect("a what-if");
    assert!(reach.others, "{reach:?}");
    let adds = rules.buildings().ids().filter(|&x| !adv.adds_civ[x].is_empty()).count();
    assert!(adv.widens.len() < adds, "{} of {adds} widen", adv.widens.len());
    // What only conquest, construction or combat read widens nothing: `Destroyed when the city
    // is captured`, `Cost increases by [n] per owned city`, a city's strength; what adds to every
    // city's yields does.
    let widens = |name: &str| adv.widens.contains(rules.lookup::<BuildingId>(name).expect(name));
    for name in ["Monument", "Walls", "Courthouse", "Circus Maximus", "Statue of Zeus"] {
        assert!(!widens(name), "{name}");
    }
    for name in ["Temple of Artemis", "Sistine Chapel", "Bazaar", "Harbor"] {
        assert!(widens(name), "{name}");
    }
    // Every other building agrees too, the unhappy ones included.
    let all: Vec<BuildingId> = g.rules().buildings().ids().collect();
    for x in all {
        let Some(w) = what_if_building(&g, a, x) else { continue };
        toggle_building_for_test(&mut g, a, x, true);
        let with = memo::city_stats(&g, a).total;
        toggle_building_for_test(&mut g, a, x, false);
        assert!(
            w.after.bit_eq(&with),
            "{}: {:?} against {with:?}",
            g.rules().buildings()[x].name,
            w.after
        );
    }
    assert_eq!(g.digest().expect("a digest"), before);
    g.settle();
    clean(&mut g);
}

#[test]
fn a_building_added_to_the_indexes_gives_what_a_rebuild_with_it_gives() {
    // The what-if's overlay adds a building's uniques to its city's own index and to its
    // owner's (`Csr::plus`); a rebuild after the building is added gives the same, for every
    // building of the ruleset, a second copy of one included.
    let mut g = testing::duel();
    let c = city(&mut g, TileIdx(22), "Roma", 3);
    let r: &'static crate::rules::Ruleset = g.rules;
    let a = &r.derived().advisor;
    for b in r.buildings().ids() {
        let want_local = civ::city_local(&g, c).plus(&a.adds_local[b]);
        let want_civ = civ::civ_index(&g, ROME).plus(&a.adds_civ[b]);
        let had = g.city(c).is_some_and(|x| x.buildings.contains(b));
        toggle_building_for_test(&mut g, c, b, true);
        let name = &r.buildings()[b].name;
        if !had {
            assert!(*civ::city_local(&g, c) == want_local, "{name}: the city's index");
            assert!(*civ::civ_index(&g, ROME) == want_civ, "{name}: the civilization's index");
        }
        toggle_building_for_test(&mut g, c, b, had);
    }
    // A second city with the same building counts its uniques twice.
    let b = r.lookup::<BuildingId>("Monument").expect("a monument");
    toggle_building_for_test(&mut g, c, b, true);
    let d = city(&mut g, TileIdx(26), "Antium", 1);
    let want = civ::civ_index(&g, ROME).plus(&a.adds_civ[b]);
    toggle_building_for_test(&mut g, d, b, true);
    assert!(*civ::civ_index(&g, ROME) == want);
    g.settle();
    clean(&mut g);
}

#[test]
fn of_equally_valued_choices_the_largest_id_wins() {
    // refcheck: advisor-ties-by-id
    let b = |n| Constructible::Building(BuildingId(n));
    let choices = [
        Choice { value: 2.0, item: b(3) },
        Choice { value: 2.0, item: b(7) },
        Choice { value: 1.0, item: b(9) },
        Choice { value: 2.0, item: b(5) },
    ];
    assert_eq!(best(&choices), Some(b(7)));
    assert_eq!(best(&[]), None);
}

#[test]
fn a_unit_that_costs_nothing_is_valued_as_costing_one() {
    // refcheck: advisor-counts-a-free-unit-as-costing-one
    let g = testing::duel();
    let pp = AdvisorParams::default();
    let warrior = g.rules().lookup::<BaseUnitId>("Warrior").expect("a warrior");
    let mut d = g.rules().base_units()[warrior].clone();
    for role in [Role::default(), Role { prefer_ranged: true, offense: true, ..Role::default() }] {
        d.cost = 0;
        let free = unit_value(&g, &d, role, &pp);
        d.cost = 1;
        assert!(free.is_finite() && free > 0.0, "{free}");
        assert_eq!(free.to_bits(), unit_value(&g, &d, role, &pp).to_bits());
    }
}

#[test]
fn whether_a_city_prefers_a_ranged_unit_is_drawn_by_city_and_turn() {
    // refcheck: advisor-draws-by-city-and-turn
    let mut g = testing::duel();
    let a = city(&mut g, TileIdx(22), "Roma", 2);
    let b = city(&mut g, TileIdx(26), "Antium", 2);
    let pp = AdvisorParams::default();
    let (mut ranged, mut differ) = (0, 0);
    for _ in 0..60 {
        let x = prefers_ranged(&g, a, &pp);
        // The same city on the same turn draws the same, whoever asks and however often.
        assert_eq!(x, prefers_ranged(&g, a, &pp));
        ranged += usize::from(x);
        differ += usize::from(x != prefers_ranged(&g, b, &pp));
        let c = *g.st.clock();
        g.set_clock(crate::state::TurnClock { turn: c.turn + 1, ..c });
    }
    // About `ranged_chance` of the turns, and the cities apart: where Python's bot, seeded with
    // the city's id afresh for each pick, drew the same for a city every turn.
    assert!((5..35).contains(&ranged), "{ranged} of 60");
    assert!(differ > 0);
    assert!(prefers_ranged(&g, a, &AdvisorParams { ranged_chance: 1.0, ..pp.clone() }));
    assert!(!prefers_ranged(&g, a, &AdvisorParams { ranged_chance: 0.0, ..pp }));
}

#[test]
fn the_advisor_answers_alike_whatever_its_memos_hold() {
    // Asked on a game whose memos are warm and on a cold copy of its state, the advisor gives the
    // same answers: what it reads validates itself.
    let mut g = testing::duel();
    let a = city(&mut g, TileIdx(22), "Roma", 4);
    let b = city(&mut g, TileIdx(26), "Antium", 2);
    every_tech(&mut g);
    let unciv = AdvisorParams::default();
    let classic = AdvisorParams { prod_mode: ProductionMode::Classic, ..AdvisorParams::default() };
    let warm: Vec<_> = [a, b]
        .iter()
        .map(|&c| {
            (advise_production(&g, ROME, c, &unciv), advise_production(&g, ROME, c, &classic))
        })
        .collect();
    let cold =
        Game::assemble(g.rules, g.st.clone(), crate::state::chronicle::Chronicle::new(), false);
    let again: Vec<_> = [a, b]
        .iter()
        .map(|&c| {
            (advise_production(&cold, ROME, c, &unciv), advise_production(&cold, ROME, c, &classic))
        })
        .collect();
    assert_eq!(warm, again);
    assert!(warm.iter().all(|(x, y)| x.is_some() && y.is_some()), "{warm:?}");
}

//! Cities' production, purchases and borders, research and policies (package 1b-07), on the
//! arena:
//! - a game of three `RandomAgent`s, each with a city, researching, building, buying and adopting
//!   policies for a hundred turns, with every check (the cache oracle and the `Buildable` memo's
//!   among them) clean at every settle (gate 4);
//! - the kitchen sink's extras of these systems: the tech cost uniques, `Obsolete with [tech]`,
//!   `Cannot build [buildings]`, the purchase uniques (`May buy [] units with [] []`, `May buy []
//!   units for [] [] []`, `May buy [] buildings with [] for [] times their normal Production cost`,
//!   `[] cost of purchasing [] buildings []%`, `May buy [] buildings for [] [] [] at an increasing
//!   price ([])`, `Can be purchased for [] [] []`), and `Cost increases by [n] when built` counted
//!   as a building is finished;
//! - what a city's list reads: the conditionals of a requirement, the tiles two out that fresh
//!   water depends on, and the other cities' queues, which it reads as it is lent;
//! - a unit with no room waiting at no extra cost, and costs a ruleset's discounts cannot bring
//!   to nothing.

use citar_engine::api::{ActionError, testops, tools};
use citar_engine::base::ids::{
    BaseUnitId, BeliefId, BuildingId, CityId, PlayerId, ReligionId, RulesReligionId, TechId,
};
use citar_engine::base::sets::BeliefSet;
use citar_engine::base::stats::Stat;
use citar_engine::game::cities::construction::{self, RejectionKind};
use citar_engine::game::cities::purchase::{buy_cost, can_purchase_with};
use citar_engine::game::cities::stats as cstats;
use citar_engine::game::{
    Action, DebugOptions, DriveOptions, Drivers, Game, Stop, policies, research,
};
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::ReligionProgress;
use citar_engine::state::State;
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::cities::{Constructible, Perpetual};
use citar_engine::state::world::{Religion, ReligionName};
use citar_testkit::agents::RandomAgent;
use citar_testkit::rulesets::{KITCHEN_SINK, files_of, kitchen_sink, overlay};
use citar_testkit::script::{map_doc, new_game};
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);
const YOU: PlayerId = PlayerId(1);

/// A bare game on the arena: the seats given, no unit, no city-state, no barbarian.
fn arena(r: &'static Ruleset, seats: &[Value], extra: &Value) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let mut cfg = json!({
        "seed": 1,
        "players": seats,
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
        "map": doc,
    });
    for (k, v) in extra.as_object().into_iter().flatten() {
        cfg[k] = v.clone();
    }
    let mut g = new_game(r, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("cleared");
    g
}

fn bench(n: usize) -> Vec<Value> {
    (0..n).map(|_| json!({"nation": "BenchmarkCiv"})).collect()
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// A tool call as a host makes one.
fn tool(g: &mut Game, p: PlayerId, name: &str, args: &Value) -> Result<Value, ActionError> {
    let mut fields = tools::normalize(name, args)?;
    fields.insert("tool".into(), json!(name));
    let action: Action = serde_json::from_value(Value::Object(fields)).expect("the arguments fit");
    g.act(p, action).map(|(out, _)| out)
}

/// The id of the city an operation founded.
fn founded(out: &Value) -> CityId {
    out["city_id"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(CityId::new)
        .unwrap_or_else(|| panic!("no city in {out}"))
}

// ---- Gate 4 --------------------------------------------------------------------------------------

#[test]
fn random_agents_research_build_and_adopt_for_a_hundred_turns_cleanly() {
    let r = Ruleset::shared();
    let mut g = arena(r, &bench(3), &json!({"turn_limit": 100}));
    let (out, _) = g
        .apply_ops(&json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Athens"},
            {"op": "found_city", "player": 2, "x": 18, "y": 4, "name": "Sparta"},
            {"op": "set_player", "player": "all", "gold": 300},
        ]))
        .expect("three cities");
    let cities: Vec<CityId> = out[..3].iter().map(founded).collect();
    let (mut a, mut b, mut c) = (RandomAgent::new(), RandomAgent::new(), RandomAgent::new());
    let mut d = Drivers::none(g.state().players().len())
        .with(PlayerId(0), &mut a)
        .with(PlayerId(1), &mut b)
        .with(PlayerId(2), &mut c);
    let (stop, batch) = g.drive(&mut d, DriveOptions::default()).expect("a live game");
    assert_eq!(stop, Stop::GameOver);
    assert_eq!([a.turns(), b.turns(), c.turns()], [100, 100, 100]);
    clean(&mut g);
    // They played: techs learned, things built, borders grown, citizens born.
    let kinds = |k: &str| batch.events().iter().filter(|e| e.kind.name() == k).count();
    assert!(kinds("tech") > 10, "techs: {}", kinds("tech"));
    assert!(kinds("building_built") + kinds("unit_built") > 5);
    assert!(kinds("borders") > 3);
    assert!(kinds("city_growth") > 3);
    for p in 0..3 {
        assert!(g.player(PlayerId(p)).is_some_and(|x| x.tech.known.len() > 5));
    }
    for c in cities {
        assert!(g.city(c).is_some_and(|x| x.pop > 1 && x.buildings.len() > 1), "{c:?}");
    }
    lists_agree_with_the_rules(&g);
}

// ---- The kitchen sink ----------------------------------------------------------------------------

/// A game of the kitchen sink's own nation against a benchmark civilization, each with the
/// cities given.
fn sink_game(extra: &Value) -> Game {
    let r = kitchen_sink();
    arena(r, &[json!({"nation": "Kitchen Sink"}), json!({"nation": "BenchmarkCiv"})], extra)
}

#[test]
fn the_kitchen_sink_researches_cheaper_and_its_cities_raise_the_cost_less() {
    // `[-10]% Science cost of researching new Technologies` and `Each city founded increases
    // Science cost of Technologies [5]% less than normal`, against a civilization without them.
    let mut g = sink_game(&json!({}));
    g.apply_ops(&json!([
        {"op": "found_city", "player": 0, "x": 5, "y": 5},
        {"op": "found_city", "player": 0, "x": 5, "y": 11},
        {"op": "found_city", "player": 0, "x": 11, "y": 13},
        {"op": "found_city", "player": 1, "x": 18, "y": 10},
        {"op": "found_city", "player": 1, "x": 18, "y": 4},
        {"op": "found_city", "player": 1, "x": 12, "y": 3},
    ]))
    .expect("three cities each");
    let r = g.rules();
    let pottery = r.lookup::<TechId>("Pottery").expect("Pottery");
    let map = g.state().map();
    let per_city = r.constants().map_size_predefined(map.width, map.height).tech_cost_per_city;
    let base = |p: PlayerId| {
        let mut x = f64::from(r.techs()[pottery].cost);
        if g.is_humanlike(p) {
            x *= r.difficulties()[g.difficulty(Some(p))].research_cost_modifier;
        }
        x *= g.speed().science_cost_modifier;
        x * r.constants().map_size_predefined(map.width, map.height).tech_cost_multiplier
    };
    let trunc = citar_engine::base::num::trunc_i32;
    assert_eq!(research::tech_cost(&g, YOU, pottery), trunc(base(YOU) * (1.0 + 2.0 * per_city)));
    assert_eq!(
        research::tech_cost(&g, ME, pottery),
        trunc(base(ME) * 0.9 * (1.0 + 2.0 * per_city * 0.95))
    );
    clean(&mut g);
}

#[test]
fn the_kitchen_sink_works_go_obsolete_and_wonders_wait_for_peace() {
    let mut g = sink_game(&json!({}));
    let (out, _) = g
        .apply_ops(&json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold"},
            {"op": "grant_tech", "player": 0, "techs": ["Engineering", "Philosophy"]},
        ]))
        .expect("a city");
    let c = founded(&out[0]);
    let r = g.rules();
    let works = Constructible::Building(r.lookup::<BuildingId>("Kitchen Sink Works").expect("it"));
    let wonder =
        Constructible::Building(r.lookup::<BuildingId>("Kitchen Sink Wonder").expect("it"));
    assert!(construction::rejection_reasons(&g, c, works).is_empty());
    assert!(construction::buildable_items(&g, c).buildings.contains(match works {
        Constructible::Building(b) => b,
        _ => unreachable!(),
    }));
    // `Obsolete with [Electricity]`.
    g.apply_ops(&json!([{"op": "grant_tech", "player": 0, "tech": "Electricity"}])).expect("ok");
    let reasons = construction::rejection_reasons(&g, c, works);
    assert!(reasons.iter().any(|x| x.kind == RejectionKind::Obsoleted), "{reasons:?}");
    assert_eq!(
        construction::can_build(&g, c, works).as_deref(),
        Some("Kitchen Sink Works is obsolete.")
    );
    // `Cannot build [Wonder] buildings <when at war>`.
    assert!(construction::rejection_reasons(&g, c, wonder).is_empty());
    g.apply_ops(&json!([{"op": "set_relation", "a": 0, "b": 1, "state": "war"}])).expect("war");
    let reasons = construction::rejection_reasons(&g, c, wonder);
    assert_eq!(
        reasons.iter().map(|x| (x.kind, x.text.as_str())).collect::<Vec<_>>(),
        [(RejectionKind::CannotBeBuilt, "Kitchen Sink Wonder cannot be built.")]
    );
    assert!(!construction::buildable_items(&g, c).wonders.contains(match wonder {
        Constructible::Building(b) => b,
        _ => unreachable!(),
    }));
    let e = tool(
        &mut g,
        ME,
        "set_production",
        &json!({"city_id": c.get(), "item": "Kitchen Sink Wonder"}),
    )
    .expect_err("at war");
    assert_eq!(e.message, "Kitchen Sink Wonder cannot be built.");
    clean(&mut g);
}

/// The kitchen sink's game with its nation founding a religion of the Kitchen Sink Faith, a city
/// following it, and faith to spend.
fn faithful() -> (Game, CityId) {
    let g = sink_game(&json!({}));
    let r = g.rules();
    let mut parts = g.state().clone().into_parts();
    let faith = r.lookup::<BeliefId>("Kitchen Sink Faith").expect("the belief");
    let mut founder_beliefs = BeliefSet::new();
    founder_beliefs.insert(faith);
    parts.world.religions.push(Religion {
        name: ReligionName::Religion(RulesReligionId(0)),
        display: "Sinkism".into(),
        founder: ME,
        founder_beliefs,
        follower_beliefs: BeliefSet::new(),
        blocked_holy: false,
    });
    if let Some(p) = parts.players.get_mut(ME) {
        p.religion.founded = Some(ReligionId(0));
        p.religion.progress = ReligionProgress::Religion;
        p.econ.faith = 2000.0;
    }
    let st = State::from_parts(parts).expect("the parts fit");
    let mut g = Game::from_state(r, st, Chronicle::new()).expect("a sound state");
    g.set_debug_options(DebugOptions::ALL);
    let (out, _) = g
        .apply_ops(&json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold", "pop": 3},
            {"op": "grant_tech", "player": 0, "techs": ["Philosophy", "Engineering"]},
        ]))
        .expect("a city");
    (g, founded(&out[0]))
}

#[test]
fn the_kitchen_sink_faith_buys_what_its_uniques_name() {
    let (mut g, c) = faithful();
    let r = g.rules();
    let unit = |n: &str| Constructible::Unit(r.lookup::<BaseUnitId>(n).expect("a unit"));
    let building = |n: &str| Constructible::Building(r.lookup::<BuildingId>(n).expect("it"));
    let faith_mod = g.speed().faith_cost_modifier;
    let tens = |x: f64| citar_engine::base::num::trunc_i32(x / 10.0) * 10;
    // `May buy [Missionary] units for [200] [Faith] [in all cities]`.
    let missionary = unit("Missionary");
    assert!(can_purchase_with(&g, c, missionary, Stat::Faith));
    assert_eq!(buy_cost(&g, c, missionary, Stat::Faith), Some(tens(200.0 * faith_mod)));
    // `May buy [Great Prophet] units with [Faith] [in all cities]`: the era's price.
    let prophet = unit("Great Prophet");
    let era = citar_engine::game::query::era(&g, ME);
    let era_price = f64::from(r.eras()[era].base_unit_buy_cost);
    assert!(can_purchase_with(&g, c, prophet, Stat::Faith));
    assert_eq!(buy_cost(&g, c, prophet, Stat::Faith), Some(tens(era_price * faith_mod)));
    // `May buy [Temple] buildings with [Faith] for [2] times their normal Production cost`, then
    // `[Faith] cost of purchasing [Temple] buildings [-25]%`.
    let temple = building("Temple");
    let cost = f64::from(cstats::production_cost(&g, ME, temple, Some(c)));
    assert_eq!(buy_cost(&g, c, temple, Stat::Faith), Some(tens(cost * 2.0 * 0.75)));
    // `May buy [Shrine] buildings for [100] [Faith] [in all cities] at an increasing price
    // ([50])`: 100, then 100 + 50 / 2 x (1 + 1) = 150 once one was bought.
    let shrine = building("Shrine");
    assert_eq!(buy_cost(&g, c, shrine, Stat::Faith), Some(tens(100.0 * faith_mod)));
    let out = tool(
        &mut g,
        ME,
        "buy",
        &json!({"city_id": c.get(), "item": "Shrine", "currency": "Faith"}),
    )
    .expect("bought");
    assert_eq!(out["stat"], "Faith");
    assert_eq!(out["cost"], json!(tens(100.0 * faith_mod)));
    let (out, _) = g
        .apply_ops(
            &json!([{"op": "found_city", "player": 0, "x": 5, "y": 11, "name": "Drainport"}]),
        )
        .expect("another city");
    let c2 = founded(&out[0]);
    assert_eq!(buy_cost(&g, c2, shrine, Stat::Faith), Some(tens(150.0 * faith_mod)));
    // `Can be purchased for [150] [Faith] [in all cities]` on the Kitchen Sink Works.
    let works = building("Kitchen Sink Works");
    assert!(can_purchase_with(&g, c, works, Stat::Faith));
    assert_eq!(buy_cost(&g, c, works, Stat::Faith), Some(tens(150.0 * faith_mod)));
    // Nothing names a Monument for faith.
    assert!(!can_purchase_with(&g, c, building("Monument"), Stat::Faith));
    clean(&mut g);
}

#[test]
fn a_building_finished_counts_toward_what_the_next_costs() {
    // `Cost increases by [30] when built`: the count moves when the city's turn finishes one.
    let mut g = sink_game(&json!({}));
    let (out, _) = g
        .apply_ops(&json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold"},
            {"op": "add_unit", "player": 1, "unit": "Warrior", "x": 18, "y": 10},
            {"op": "grant_tech", "player": 0, "tech": "Engineering"},
        ]))
        .expect("a city");
    let c = founded(&out[0]);
    let r = g.rules();
    let works = Constructible::Building(r.lookup::<BuildingId>("Kitchen Sink Works").expect("it"));
    let before = cstats::production_cost(&g, ME, works, Some(c));
    tool(&mut g, ME, "set_production", &json!({"city_id": c.get(), "item": "Kitchen Sink Works"}))
        .expect("queued");
    // Its production stored in full: the city's next turn finishes it.
    let mut parts = g.state().clone().into_parts();
    if let Some(x) = parts.cities.get_mut(c) {
        x.progress.insert(works, f64::from(before));
    }
    let st = State::from_parts(parts).expect("the parts fit");
    let mut g = Game::from_state(r, st, Chronicle::new()).expect("a sound state");
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "end_round"}])).expect("a round");
    assert!(g.city(c).is_some_and(|x| x.buildings.contains(match works {
        Constructible::Building(b) => b,
        _ => unreachable!(),
    })));
    assert_eq!(g.player(ME).and_then(|p| p.civ.built_increasing.get(&works).copied()), Some(1));
    assert!(cstats::production_cost(&g, ME, works, Some(c)) > before);
    clean(&mut g);
}

// ---- What a city's list reads ------------------------------------------------------------------

/// The shipped ruleset with a merge patch over one of its files, loaded once per test.
fn patched(file: &str, patch: &Value) -> &'static Ruleset {
    let patch = patch.to_string();
    let files = overlay(&[(file, &patch)]).expect("the patch applies");
    Ruleset::leak(&files_of(&files)).unwrap_or_else(|e| panic!("the ruleset loads:\n{e}"))
}

/// Every city's list holds exactly what the rules let it build: the memo, and what
/// `buildable_items` reads as it lends it (the hangar, the other cities' queues), together agree
/// with asking each item of the rules.
fn lists_agree_with_the_rules(g: &Game) {
    let r = g.rules();
    for city in g.state().cities().iter() {
        let c = city.id();
        let list = construction::buildable_items(g, c);
        let items = r
            .base_units()
            .ids()
            .map(Constructible::Unit)
            .chain(r.buildings().ids().map(Constructible::Building))
            .chain([Perpetual::Gold, Perpetual::Science].map(Constructible::Perpetual));
        for item in items {
            assert_eq!(
                list.contains(item),
                construction::is_buildable(g, c, item),
                "{} in {}",
                construction::item_name(r, item),
                city.name
            );
        }
    }
}

#[test]
fn a_requirement_is_read_again_when_what_its_conditionals_read_changes() {
    // `Only available` and `Can only be built` are asked of their conditionals directly, not
    // through `applies`: the list must still record what they read.
    let r = patched(
        "ruleset/buildings.json",
        &json!({
            "Monument": {"uniques": ["Only available <during a Golden Age>"]},
            "Shrine": {"uniques": ["Can only be built <after turn number [5]>"]},
        }),
    );
    let mut g = arena(r, &bench(2), &json!({}));
    let (out, _) = g
        .apply_ops(&json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Athens"},
            {"op": "grant_tech", "player": 0, "tech": "Pottery"},
        ]))
        .expect("the cities");
    let c = founded(&out[0]);
    let monument = r.lookup::<BuildingId>("Monument").expect("it");
    let shrine = r.lookup::<BuildingId>("Shrine").expect("it");
    let has = |g: &Game, b| construction::buildable_items(g, c).buildings.contains(b);
    assert!(!has(&g, monument) && !has(&g, shrine));
    g.apply_ops(&json!([{"op": "set_player", "player": 0, "golden_age_turns": 10}]))
        .expect("a golden age");
    assert!(has(&g, monument) && !has(&g, shrine));
    clean(&mut g);
    testops::apply(&mut g, &json!([{"op": "set_turn", "turn": 10}])).expect("a later turn");
    assert!(has(&g, shrine));
    clean(&mut g);
    lists_agree_with_the_rules(&g);
}

#[test]
fn a_list_reads_the_tiles_beside_its_neighbours_for_fresh_water() {
    // `Must be next to [Fresh water]` asks the city's neighbours whether a lake lies beside them,
    // two tiles out. Without Machu Picchu's and Neuschwanstein's `within [2] tiles`, nothing
    // else in the ruleset reads that far.
    let r = patched(
        "ruleset/buildings.json",
        &json!({
            "Machu Picchu": {"uniques": ["[+25]% [Gold] from Trade Routes"]},
            "Neuschwanstein": {"uniques": ["[+1 Happiness, +2 Culture, +3 Gold] from every [Castle]"]},
        }),
    );
    let mut g = arena(r, &bench(2), &json!({}));
    let (out, _) = g
        .apply_ops(&json!([
            {"op": "found_city", "player": 0, "x": 12, "y": 3, "name": "Roma"},
            {"op": "grant_tech", "player": 0, "tech": "Theology"},
        ]))
        .expect("a city");
    let c = founded(&out[0]);
    let garden = r.lookup::<BuildingId>("Garden").expect("it");
    assert!(!construction::buildable_items(&g, c).buildings.contains(garden));
    assert_eq!(
        construction::rejection_kinds(&g, c, Constructible::Building(garden)).as_slice(),
        [RejectionKind::MustBeNextToTile]
    );
    g.apply_ops(&json!([{"op": "set_tile", "x": 14, "y": 3, "terrain": "Lakes"}]))
        .expect("a lake two tiles out");
    assert!(construction::buildable_items(&g, c).buildings.contains(garden));
    clean(&mut g);
}

#[test]
fn what_the_other_cities_build_is_read_as_the_list_is_lent() {
    // A wonder another city builds, and a limit the other cities' queues reach, take an item off
    // a city's list without recomputing it.
    let r = patched(
        "ruleset/buildings.json",
        &json!({"Monument": {"uniques": ["Limited to [2] per Civilization"]}}),
    );
    let mut g = arena(r, &bench(2), &json!({}));
    let (out, _) = g
        .apply_ops(&json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"},
            {"op": "found_city", "player": 0, "x": 5, "y": 11, "name": "Antium"},
            {"op": "found_city", "player": 0, "x": 11, "y": 13, "name": "Capua"},
            {"op": "grant_tech", "player": 0, "tech": "Calendar"},
        ]))
        .expect("three cities");
    let [roma, antium, capua] = [0, 1, 2].map(|i| founded(&out[i]));
    let monument = r.lookup::<BuildingId>("Monument").expect("it");
    let stonehenge = r.lookup::<BuildingId>("Stonehenge").expect("it");
    let list = |g: &Game, c| construction::buildable_items(g, c);
    let produce = |g: &mut Game, c: CityId, item: &str, append: bool| {
        tool(g, ME, "set_production", &json!({"city_id": c.get(), "item": item, "append": append}))
            .expect("queued");
    };
    lists_agree_with_the_rules(&g);
    // Roma builds Stonehenge: no other city of its owner may.
    produce(&mut g, roma, "Stonehenge", false);
    assert!(list(&g, roma).wonders.contains(stonehenge));
    assert!(!list(&g, antium).wonders.contains(stonehenge));
    assert!(!list(&g, capua).wonders.contains(stonehenge));
    lists_agree_with_the_rules(&g);
    // Roma and Antium queue a Monument, the limit of two: Capua may not.
    produce(&mut g, roma, "Monument", true);
    produce(&mut g, antium, "Monument", false);
    assert!(list(&g, roma).buildings.contains(monument));
    assert!(list(&g, antium).buildings.contains(monument));
    assert!(!list(&g, capua).buildings.contains(monument));
    lists_agree_with_the_rules(&g);
    // Roma finishes its first item and then its Monument: built, it still counts.
    testops::apply(&mut g, &json!([{"op": "complete_construction", "city": roma.get()}]))
        .expect("Stonehenge");
    testops::apply(&mut g, &json!([{"op": "complete_construction", "city": roma.get()}]))
        .expect("a Monument");
    assert!(!list(&g, capua).buildings.contains(monument));
    assert!(!list(&g, antium).wonders.contains(stonehenge));
    lists_agree_with_the_rules(&g);
    // Antium's queue cleared: one Monument left to build.
    tool(&mut g, ME, "change_queue", &json!({"city_id": antium.get(), "action": "clear"}))
        .expect("cleared");
    assert!(list(&g, capua).buildings.contains(monument));
    lists_agree_with_the_rules(&g);
    clean(&mut g);
}

// ---- What production keeps ---------------------------------------------------------------------

#[test]
fn a_unit_with_no_room_waits_and_costs_no_more() {
    // `Cost increases by [n] when built` counts what was finished: a Warrior on an island city
    // whose only tile another Warrior holds waits, and its price stays.
    let r = patched(
        "ruleset/units.json",
        &json!({"Warrior": {"uniques": [
            "May upgrade to [Spearman] through ruins-like effects",
            "Cost increases by [10] when built",
        ]}}),
    );
    let mut g = arena(r, &bench(2), &json!({}));
    let at = g.grid().idx(12, 3).expect("a tile");
    let mut ops: Vec<Value> = g
        .grid()
        .neighbors(at)
        .map(|n| {
            let (x, y) = g.xy(n);
            json!({"op": "set_tile", "x": x, "y": y, "terrain": "Coast"})
        })
        .collect();
    ops.push(json!({"op": "found_city", "player": 0, "x": 12, "y": 3, "name": "Roma"}));
    ops.push(json!({"op": "add_unit", "player": 0, "unit": "Warrior", "x": 12, "y": 3}));
    ops.push(json!({"op": "add_unit", "player": 1, "unit": "Warrior", "x": 18, "y": 10}));
    let (out, _) = g.apply_ops(&Value::Array(ops)).expect("an island city");
    let c = founded(&out[out.len() - 3]);
    let warrior = Constructible::Unit(r.lookup::<BaseUnitId>("Warrior").expect("it"));
    let before = cstats::production_cost(&g, ME, warrior, Some(c));
    tool(&mut g, ME, "set_production", &json!({"city_id": c.get(), "item": "Warrior"}))
        .expect("queued");
    let mut parts = g.state().clone().into_parts();
    if let Some(x) = parts.cities.get_mut(c) {
        x.progress.insert(warrior, f64::from(before));
    }
    let st = State::from_parts(parts).expect("the parts fit");
    let mut g = Game::from_state(r, st, Chronicle::new()).expect("a sound state");
    g.set_debug_options(DebugOptions::ALL);
    let (_, batch) = testops::apply(&mut g, &json!([{"op": "end_round"}, {"op": "end_round"}]))
        .expect("two rounds");
    assert!(batch.events().iter().any(|e| e.kind.name() == "production_blocked"));
    assert_eq!(g.city(c).map(|x| x.queue.as_slice()), Some(&[warrior][..]));
    assert_eq!(g.player(ME).and_then(|p| p.civ.built_increasing.get(&warrior).copied()), None);
    assert_eq!(cstats::production_cost(&g, ME, warrior, Some(c)), before);
    clean(&mut g);
}

// ---- Costs a ruleset can drive to nothing ----------------------------------------------------------

#[test]
fn a_tech_and_a_policy_cost_something_whatever_the_discounts() {
    // `[-100]% Science cost of researching new Technologies` would make every tech free, and a
    // free repeatable tech would be learned without end; the costs stop at 1 and 5, and the
    // science carried over pays for one Future Tech after another without nesting a call each.
    let nation = json!({"Kitchen Sink": {"uniques": [
        "[-100]% Science cost of researching new Technologies",
        "[-100]% Culture cost of adopting new Policies",
    ]}})
    .to_string();
    let mut patches = KITCHEN_SINK.to_vec();
    patches.push(("ruleset/nations.json", &nation));
    let files = overlay(&patches).expect("the patches apply");
    let r: &'static Ruleset = Ruleset::leak(&files_of(&files)).expect("the ruleset loads");
    let mut g = arena(
        r,
        &[json!({"nation": "Kitchen Sink"}), json!({"nation": "BenchmarkCiv"})],
        &json!({}),
    );
    let all: Vec<String> = r
        .techs()
        .iter()
        .filter(|&(t, _)| !research::is_repeatable(r, t))
        .map(|(_, d)| d.name.to_string())
        .collect();
    g.apply_ops(&json!([
        {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold"},
        {"op": "grant_tech", "player": 0, "techs": all},
        {"op": "set_research", "player": 0, "tech": "Future Tech"},
    ]))
    .expect("every tech but the last");
    let future = r.lookup::<TechId>("Future Tech").expect("it");
    assert_eq!(research::tech_cost(&g, ME, future), 1);
    assert_eq!(policies::culture_cost(&g, ME, None), 5);
    research::add_science(&mut g, ME, 5000.0);
    let pl = g.player(ME).expect("the player");
    assert_eq!(pl.tech.future_techs, 5000);
    assert!(pl.tech.overflow.abs() < 1e-9, "{}", pl.tech.overflow);
    assert_eq!(research::current(&g, ME), Some(future));
}

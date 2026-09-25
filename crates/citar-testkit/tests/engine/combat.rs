//! Combat and conquest (package 1c-03), on games built by hand with the kitchen-sink ruleset: the
//! extra types of these systems, each once, beside the shipped rules they refine.
//! - `[n] Strength` (the Kitchen Sink Veteran's +2 to a unit's strength);
//! - `May attack when embarked` (the Kitchen Sink Raider fights from the water);
//! - `No defensive terrain penalty` (the Raider on a marsh);
//! - `[n] Air Interception Range` (the Kitchen Sink Interceptor reaches two tiles further);
//! - `May not annex cities` (the Kitchen Sink keeps what it takes as puppets, and may not annex
//!   them);
//! - `Never destroyed when the city is captured` (the Kitchen Sink Works outlasts every capture);
//! - `upon conquering a city`, `upon losing a city`, `upon losing a [unit] unit`, `upon defeating a
//!   [unit] unit` and `upon being defeated` fire where a capture, a loss or a kill happens (their
//!   effects other than a unit's are one-time effects, package 1b-08's, so what fired is read from
//!   the trace `triggers::take_fired_for_test`).
//!
//! Beside them, the contract of the fights: damage grows with the attacker's strength (gate 2),
//! each combat event takes one `combat_seq`, and a game saved and loaded between two attacks
//! fights the second as it would have (gate 3).
//!
//! Every check, the cache oracle among them, is clean after each step.

use citar_engine::base::ids::{
    BarbarianLevelId, CityId, DifficultyId, EraId, MapSizeId, MapTypeId, NationId, PlayerId,
    SpeedId, TerrainId, TileIdx, UniqueId, UnitId,
};
use citar_engine::base::sets::PlayerVec;
use citar_engine::game::combat::{air, combatant, resolve, strength};
use citar_engine::game::conquest::{self, CityFate};
use citar_engine::game::{DebugOptions, Game, triggers};
use citar_engine::rules::{Named, Ruleset};
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::cities::{Cities, City};
use citar_engine::state::config::{GameConfig, MapEdges, MapSource};
use citar_engine::state::map::{MapInfo, Tile, Tiles};
use citar_engine::state::players::{Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides};
use citar_engine::state::{State, TileClaim};
use citar_engine::unique::trigger::TriggerKind;
use citar_engine::unique::{Combatant, UniqueType};
use citar_testkit::rulesets::kitchen_sink;
use proptest::prelude::*;
use serde_json::{Value, json};

const W: u16 = 12;
const H: u16 = 8;
const ME: PlayerId = PlayerId(0);
const THEM: PlayerId = PlayerId(1);

fn id<I: Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("the ruleset has {name}"))
}

fn at(x: u32, y: u32) -> TileIdx {
    TileIdx(y * u32::from(W) + x)
}

/// A 12 by 8 game: coast in the two westmost columns, grassland elsewhere; the first player is
/// the Kitchen Sink, with its capital Sinkhold at (5,4) whose neighbours it owns; the second has
/// nothing.
fn game(r: &'static Ruleset) -> Game {
    let size = u32::from(W) * u32::from(H);
    let player = |n: u8, nation: &str| {
        let seat = Seat::new(Controller::Human, SeatOverrides::default(), None);
        Player::new(
            PlayerId(n),
            PlayerKind::Major,
            format!("Civ {n}").into(),
            id::<NationId>(r, nation),
            Rgb::default(),
            seat,
            size,
        )
    };
    let players: PlayerVec<Player> =
        [player(0, "Kitchen Sink"), player(1, "BenchmarkCiv")].into_iter().collect();
    let map = MapInfo { width: W, height: H, wrap_x: false, wrap_y: false, continents: Vec::new() };
    let (grass, coast) = (id::<TerrainId>(r, "Grassland"), id::<TerrainId>(r, "Coast"));
    let src = MapSource::Generated {
        size: MapSizeId(0),
        map_type: MapTypeId(0),
        edges: MapEdges::IceCaps,
        dims: None,
    };
    let cfg = GameConfig::new(
        5,
        src,
        id::<SpeedId>(r, "Standard"),
        id::<DifficultyId>(r, "Prince"),
        EraId(0),
        BarbarianLevelId(1),
        500,
    );
    let tiles: Vec<Tile> =
        (0..size).map(|i| Tile::new(if i % u32::from(W) < 2 { coast } else { grass })).collect();
    let st = State::new(cfg, map, Tiles::new(tiles), players).expect("a new state");
    let mut parts = st.into_parts();
    let grid = parts.map.grid().expect("a grid");
    let c = CityId::FIRST;
    let mut tiles: Vec<Tile> = parts.tiles.as_slice().to_vec();
    for n in grid.within(at(5, 4), 1) {
        tiles[n.0 as usize] = tiles[n.0 as usize].with_claim(TileClaim::city(ME, c));
    }
    parts.tiles = Tiles::new(tiles);
    let mut sinkhold = City::new(c, "Sinkhold".into(), ME, at(5, 4), 1);
    sinkhold.original_capital = true;
    parts.cities = Cities::from_cities(vec![sinkhold]).expect("cities");
    if let Some(p) = parts.players.get_mut(ME) {
        p.capital = Some(c);
        p.original_capital = Some(c);
    }
    parts.ids = citar_engine::state::IdCounters::starting_at(2);
    let st = State::from_parts(parts).expect("the parts fit");
    let mut g = Game::from_state(r, st, Chronicle::new()).expect("a sound state");
    g.set_debug_options(DebugOptions::ALL);
    g
}

/// Scenario operations, which must succeed; what each returned.
fn ops(g: &mut Game, list: Value) -> Vec<Value> {
    g.apply_ops(&list).unwrap_or_else(|e| panic!("{list}: {e}")).0
}

/// Test operations, which must succeed; what each returned.
fn test_ops(g: &mut Game, list: Value) -> Vec<Value> {
    citar_engine::api::testops::apply(g, &list).unwrap_or_else(|e| panic!("{list}: {e}")).0
}

/// Adds a unit of `p` on a tile, and returns its id.
fn add(g: &mut Game, p: PlayerId, unit: &str, t: TileIdx) -> UnitId {
    let (x, y) = g.grid().xy(t);
    let out = ops(g, json!([{"op": "add_unit", "player": p.0, "unit": unit, "x": x, "y": y}]));
    let n = out[0]["unit_ids"][0].as_u64().expect("a unit id");
    UnitId::new(u32::try_from(n).expect("an id")).expect("an id")
}

/// Founds a city of `p` on a tile, and returns its id.
fn found(g: &mut Game, p: PlayerId, t: TileIdx, name: &str) -> CityId {
    let (x, y) = g.grid().xy(t);
    let out = ops(g, json!([{"op": "found_city", "player": p.0, "x": x, "y": y, "name": name}]));
    let n = out[0]["city_id"].as_u64().expect("a city id");
    CityId::new(u32::try_from(n).expect("an id")).expect("an id")
}

/// War between the two players.
fn war(g: &mut Game) {
    ops(g, json!([{"op": "set_relation", "a": 0, "b": 1, "state": "war"}]));
}

/// A unit attacks a tile, whoever's turn it is; what the attack reports.
fn attack(g: &mut Game, u: UnitId, t: TileIdx) -> Value {
    let (x, y) = g.grid().xy(t);
    test_ops(g, json!([{"op": "attack_as", "unit": u.get(), "x": x, "y": y}])).remove(0)
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

/// The unique of type `ty` on an object of the ruleset, found by its text.
fn unique(r: &Ruleset, ty: UniqueType, text: &str) -> UniqueId {
    let t = r.uniques();
    t.iter()
        .map(|(id, _)| id)
        .find(|&id| t.meta(id).ty == Some(ty) && t.text_of(id).contains(text))
        .unwrap_or_else(|| panic!("a {ty:?} unique with {text:?}"))
}

/// What fired since the last look, of one kind.
fn fired(kind: TriggerKind) -> Vec<UniqueId> {
    triggers::take_fired_for_test()
        .into_iter()
        .filter(|&(k, _)| k == kind)
        .map(|(_, u)| u)
        .collect()
}

#[test]
fn a_promotion_s_flat_strength_adds_to_the_unit_s_own() {
    let r = kitchen_sink();
    let mut g = game(r);
    let mine = add(&mut g, ME, "Warrior", at(6, 4));
    let theirs = add(&mut g, THEM, "Warrior", at(7, 4));
    war(&mut g);
    let (a, d) = (Combatant::Unit(mine), Combatant::Unit(theirs));
    assert_eq!(strength::base_attack(&g, a, Some(d)).to_bits(), 8.0f64.to_bits());
    assert_eq!(strength::base_defense(&g, a, Some(d)).to_bits(), 8.0f64.to_bits());
    test_ops(
        &mut g,
        json!([{"op": "set_unit", "unit": mine.get(), "promotions": ["Kitchen Sink Veteran"]}]),
    );
    clean(&mut g);
    assert_eq!(strength::base_attack(&g, a, Some(d)).to_bits(), 10.0f64.to_bits(), "[+2] Strength");
    assert_eq!(
        strength::base_defense(&g, a, Some(d)).to_bits(),
        10.0f64.to_bits(),
        "and when it defends"
    );
    assert_eq!(strength::base_attack(&g, a, None).to_bits(), 8.0f64.to_bits(), "only in a fight");
}

#[test]
fn a_unit_that_may_attack_when_embarked_attacks_from_the_water() {
    let r = kitchen_sink();
    let mut g = game(r);
    ops(&mut g, json!([{"op": "grant_tech", "player": 0, "tech": "Optics"}]));
    let raider = add(&mut g, ME, "Kitchen Sink Raider", at(1, 3));
    let warrior = add(&mut g, ME, "Warrior", at(1, 5));
    add(&mut g, THEM, "Trireme", at(0, 3));
    add(&mut g, THEM, "Trireme", at(0, 5));
    war(&mut g);
    clean(&mut g);
    assert!(citar_engine::game::movement::is_embarked(&g, raider));
    assert!(citar_engine::game::movement::is_embarked(&g, warrior));
    assert_eq!(resolve::contains_attackable_enemy(&g, at(0, 3), Combatant::Unit(raider)), None);
    assert_eq!(
        resolve::contains_attackable_enemy(&g, at(0, 5), Combatant::Unit(warrior)).as_deref(),
        Some("Embarked units can only make melee attacks onto land.")
    );
    let out = attack(&mut g, raider, at(0, 3));
    assert_eq!(out["attacker"], "Kitchen Sink Raider");
    clean(&mut g);
}

#[test]
fn a_unit_with_no_terrain_penalty_defends_a_marsh_in_full() {
    let r = kitchen_sink();
    let mut g = game(r);
    let mine = add(&mut g, ME, "Warrior", at(6, 4));
    ops(
        &mut g,
        json!([
            {"op": "set_tile", "x": 7, "y": 4, "features": ["Marsh"]},
            {"op": "set_tile", "x": 7, "y": 3, "features": ["Marsh"]},
        ]),
    );
    let raider = add(&mut g, THEM, "Kitchen Sink Raider", at(7, 4));
    let warrior = add(&mut g, THEM, "Warrior", at(7, 3));
    war(&mut g);
    clean(&mut g);
    let a = Combatant::Unit(mine);
    let raider_mods = strength::defense_modifiers(&g, a, Combatant::Unit(raider), at(6, 4));
    let warrior_mods = strength::defense_modifiers(&g, a, Combatant::Unit(warrior), at(6, 4));
    assert!(raider_mods.iter().all(|&(k, _)| k != "Tile"), "{raider_mods:?}");
    assert_eq!(warrior_mods.iter().find(|&&(k, _)| k == "Tile"), Some(&("Tile", -15)));
}

#[test]
fn an_interception_range_reaches_further() {
    let r = kitchen_sink();
    let mut g = game(r);
    found(&mut g, THEM, at(10, 4), "Hangar");
    let icpt = add(&mut g, THEM, "Kitchen Sink Interceptor", at(10, 4));
    test_ops(&mut g, json!([{"op": "ready_unit", "unit": icpt.get()}]));
    clean(&mut g);
    assert_eq!(air::intercept_chance(&g, icpt), 100);
    // Its own range is 6: the unique's +2 makes it 8.
    assert!(air::can_intercept(&g, icpt, at(3, 4)), "7 tiles off");
    assert!(air::can_intercept(&g, icpt, at(2, 4)), "8 tiles off");
    assert!(!air::can_intercept(&g, icpt, at(1, 4)), "9 tiles off");
}

/// A second city of the other player at (9,4), its first at (10,0), and a warrior of the Kitchen
/// Sink's beside the second, at war, the city's defences down.
fn conquest_setup(g: &mut Game) -> (CityId, UnitId) {
    found(g, THEM, at(10, 0), "Firstport");
    let target = found(g, THEM, at(9, 4), "Target");
    let w = add(g, ME, "Warrior", at(8, 4));
    war(g);
    ops(g, json!([{"op": "set_city", "city": target.get(), "health": 1}]));
    test_ops(g, json!([{"op": "set_turn", "turn": 10}, {"op": "ready_unit", "unit": w.get()}]));
    (target, w)
}

#[test]
fn a_civilization_that_may_not_annex_keeps_what_it_takes_a_puppet() {
    let r = kitchen_sink();
    let mut g = game(r);
    let (target, w) = conquest_setup(&mut g);
    triggers::take_fired_for_test().clear();
    let out = attack(&mut g, w, at(9, 4));
    clean(&mut g);
    assert_eq!(out["captured_city"], "Target");
    let city = g.city(target).expect("the city");
    assert_eq!((city.owner(), city.puppet), (ME, true));
    // refcheck: may-not-annex-refuses-annexing
    let e = conquest::plan_fate(&g, ME, target, CityFate::Annex).expect_err("it may not annex");
    assert_eq!(e.message, "Civ 0 may not annex cities.");
    assert!(conquest::plan_fate(&g, ME, target, CityFate::Raze).is_ok());
    // `upon conquering a city`: its Adopt [Aristocracy] (a one-time effect of package 1b-08).
    let adopt = unique(r, UniqueType::OneTimeAdoptPolicyOrBelief, "upon conquering a city");
    assert_eq!(fired(TriggerKind::ConqueringCity), [adopt]);
}

#[test]
fn a_building_never_destroyed_on_capture_outlasts_every_capture() {
    let r = kitchen_sink();
    let mut base = game(r);
    let (target, w) = conquest_setup(&mut base);
    let buildings = ["Kitchen Sink Works", "Monument", "Granary", "Library"];
    ops(&mut base, json!([{"op": "set_city", "city": target.get(), "add_buildings": buildings}]));
    ops(&mut base, json!([{"op": "set_city", "city": target.get(), "health": 1}]));
    let works = id(r, "Kitchen Sink Works");
    let mut lost_any = false;
    for turn in 10..30 {
        let mut g = base.clone();
        test_ops(&mut g, json!([{"op": "set_turn", "turn": turn}]));
        attack(&mut g, w, at(9, 4));
        clean(&mut g);
        let city = g.city(target).expect("the city");
        assert_eq!(city.owner(), ME);
        assert!(city.buildings.contains(works), "turn {turn}: the Works stands");
        lost_any |= city.buildings.len() < buildings.len();
    }
    assert!(lost_any, "the other buildings fall a third of the time");
}

#[test]
fn losing_a_city_fires_for_its_old_owner() {
    let r = kitchen_sink();
    let mut g = game(r);
    let second = found(&mut g, ME, at(9, 4), "Drainport");
    let w = add(&mut g, THEM, "Warrior", at(10, 4));
    war(&mut g);
    ops(&mut g, json!([{"op": "set_city", "city": second.get(), "health": 1}]));
    test_ops(
        &mut g,
        json!([{"op": "set_turn", "turn": 10}, {"op": "ready_unit", "unit": w.get()}]),
    );
    triggers::take_fired_for_test().clear();
    let out = attack(&mut g, w, at(9, 4));
    clean(&mut g);
    assert_eq!(out["captured_city"], "Drainport");
    let pop = unique(r, UniqueType::OneTimeGainPopulationRandomCity, "upon losing a city");
    assert_eq!(fired(TriggerKind::LosingCity), [pop]);
}

#[test]
fn defeating_a_unit_and_being_defeated_fire_for_each_side() {
    let r = kitchen_sink();
    let mut g = game(r);
    // The Raider, with an extra attack so that it keeps its movement after a kill, kills a
    // warrior: `[This Unit] gains [1] movement <upon defeating a [Military] unit>`.
    let raider = add(&mut g, ME, "Kitchen Sink Raider", at(6, 4));
    test_ops(&mut g, json!([{"op": "set_unit", "unit": raider.get(), "promotions": ["Blitz"]}]));
    let victim = add(&mut g, THEM, "Warrior", at(7, 4));
    war(&mut g);
    test_ops(
        &mut g,
        json!([{"op": "set_unit", "unit": victim.get(), "hp": 1}, {"op": "ready_unit", "unit": raider.get()}]),
    );
    let full = g.unit(raider).map_or(0, |x| x.moves);
    triggers::take_fired_for_test().clear();
    let out = attack(&mut g, raider, at(7, 4));
    clean(&mut g);
    assert_eq!(out["defender_killed"], true);
    let gains = unique(r, UniqueType::OneTimeUnitGainMovement, "upon defeating");
    assert_eq!(fired(TriggerKind::DefeatingUnit), [gains]);
    let sc = r.constants().move_scale;
    assert_eq!(g.unit(raider).map(|x| x.moves), Some(full + sc), "it moved in, then gained");

    // A swordsman of the other side kills the Raider: `Gain [50] [Gold] <upon being defeated>`
    // fires for it, and `Gain [10] [Gold] <upon losing a [Military] unit>` for its owner.
    test_ops(&mut g, json!([{"op": "set_unit", "unit": raider.get(), "hp": 1}]));
    let sword = add(&mut g, THEM, "Swordsman", at(8, 4));
    test_ops(&mut g, json!([{"op": "ready_unit", "unit": sword.get()}]));
    triggers::take_fired_for_test().clear();
    let (rx, ry) = g.grid().xy(g.unit(raider).map(|x| x.tile()).expect("the raider"));
    let out =
        attack(&mut g, sword, at(u32::try_from(rx).unwrap_or(0), u32::try_from(ry).unwrap_or(0)));
    clean(&mut g);
    assert_eq!(out["defender_killed"], true);
    let all = triggers::take_fired_for_test();
    let defeat = unique(r, UniqueType::OneTimeGainStat, "upon being defeated");
    let lost = unique(r, UniqueType::OneTimeGainStat, "upon losing a [Military] unit");
    assert!(all.contains(&(TriggerKind::Defeat, defeat)), "{all:?}");
    assert!(all.contains(&(TriggerKind::LosingUnit, lost)), "{all:?}");
}

#[test]
fn losing_a_civilian_is_not_losing_a_military_unit() {
    let r = kitchen_sink();
    let mut g = game(r);
    let worker = add(&mut g, ME, "Worker", at(7, 4));
    let archer = add(&mut g, THEM, "Archer", at(9, 4));
    war(&mut g);
    test_ops(
        &mut g,
        json!([{"op": "set_unit", "unit": worker.get(), "hp": 40}, {"op": "ready_unit", "unit": archer.get()}]),
    );
    triggers::take_fired_for_test().clear();
    // refcheck: civilians-under-fire
    let out = attack(&mut g, archer, at(7, 4));
    clean(&mut g);
    assert_eq!(
        (out["damage_to_defender"].as_i64(), out["defender_killed"].as_bool()),
        (Some(40), Some(true))
    );
    assert!(g.unit(worker).is_none());
    assert!(fired(TriggerKind::LosingUnit).is_empty(), "the filter is [Military]");
}

// ---- The contract of fights -------------------------------------------------------------------

proptest! {
    /// Gate 2: the damage a defender takes never falls as the attacker grows stronger, and the
    /// damage the attacker takes back never rises, at any roll and any wounds.
    #[test]
    fn damage_grows_with_the_attacker_s_strength(
        a in 1.0f64..400.0,
        more in 0.0f64..400.0,
        d in 1.0f64..400.0,
        rnd in 0.0f64..=1.0,
        wounds in 0.67f64..=1.0,
    ) {
        let hit = |s: f64, to_attacker| {
            citar_engine::base::num::round_half_even(
                strength::damage_modifier(s / d, to_attacker, rnd) * wounds,
            )
        };
        prop_assert!(hit(a, false) <= hit(a + more, false));
        prop_assert!(hit(a, true) >= hit(a + more, true));
    }
}

/// The combat counter as the state holds it.
fn seq(g: &Game) -> u64 {
    g.state().ids().combat_seq
}

#[test]
fn every_combat_event_takes_one_number() {
    let r = kitchen_sink();
    let mut g = game(r);
    let a = add(&mut g, ME, "Warrior", at(6, 4));
    add(&mut g, THEM, "Warrior", at(7, 4));
    war(&mut g);
    assert_eq!(seq(&g), 0);
    attack(&mut g, a, at(7, 4));
    assert_eq!(seq(&g), 1, "a fight");
    // Sinkhold bombards the same warrior, two tiles off.
    let bombard = citar_engine::game::combat::actions::CityAttack { city_id: 1, x: 7, y: 4 };
    let done = g.act(ME, citar_engine::game::Action::CityAttack(bombard));
    assert!(done.is_ok(), "{done:?}");
    assert_eq!(seq(&g), 2, "a bombardment");
    // A bomber met by an interceptor: the interception, then the fight.
    found(&mut g, THEM, at(10, 4), "Hangar");
    let icpt = add(&mut g, THEM, "Kitchen Sink Interceptor", at(10, 4));
    let bomber = add(&mut g, ME, "Bomber", at(5, 4));
    add(&mut g, THEM, "Warrior", at(7, 3));
    test_ops(&mut g, json!([{"op": "ready_unit", "unit": icpt.get()}]));
    let out = attack(&mut g, bomber, at(7, 3));
    assert!(out.get("intercepted").is_some(), "{out}");
    assert_eq!(seq(&g), 4, "an interception and a fight");
    clean(&mut g);
}

#[test]
fn a_save_between_two_attacks_fights_the_second_alike() {
    let r = kitchen_sink();
    let mut g = game(r);
    let a = add(&mut g, ME, "Warrior", at(6, 4));
    let b = add(&mut g, ME, "Warrior", at(6, 5));
    add(&mut g, THEM, "Warrior", at(7, 4));
    war(&mut g);
    attack(&mut g, a, at(7, 4));
    let mut loaded = g.clone();
    test_ops(&mut loaded, json!([{"op": "reload"}]));
    assert_eq!(seq(&loaded), seq(&g));
    let here = attack(&mut g, b, at(7, 4));
    let there = attack(&mut loaded, b, at(7, 4));
    assert_eq!(here, there);
    assert_eq!(g.digest().ok(), loaded.digest().ok());
    clean(&mut g);
    clean(&mut loaded);
}

#[test]
fn a_fight_s_modifiers_come_from_one_setup() {
    let r = kitchen_sink();
    let mut g = game(r);
    let a = add(&mut g, ME, "Warrior", at(6, 4));
    let d = add(&mut g, THEM, "Warrior", at(7, 4));
    war(&mut g);
    let (ca, cd) = (Combatant::Unit(a), Combatant::Unit(d));
    let s = strength::setup(&g, ca, combatant::tile(&g, ca), cd, false);
    let pv = resolve::preview(&g, a, at(7, 4)).expect("a preview");
    assert_eq!(
        pv["damage_to_defender"],
        json!([s.damage_to_defender(0.0), s.damage_to_defender(1.0)])
    );
    assert_eq!(
        pv["damage_to_attacker"],
        json!([s.damage_to_attacker(0.0), s.damage_to_attacker(1.0)])
    );
}

// ---- Random agents at war ------------------------------------------------------------------------

/// Two `RandomAgent`s at war on the arena, each with two cities and an army at the other's door
/// (and one city under siege, its defences down), play forty turns: they attack, bombard, take a
/// city and decide its fate, with every check (the cache oracle among them) clean at every settle.
#[test]
fn random_agents_at_war_fight_cleanly() {
    use citar_engine::game::{DriveOptions, Drivers, Stop};
    use citar_testkit::agents::RandomAgent;
    use citar_testkit::script::{map_doc, new_game};
    let r = Ruleset::shared();
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": 3,
        "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
        "turn_limit": 40,
        "map": doc,
    });
    let mut g = new_game(r, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    test_ops(&mut g, json!([{"op": "clear_units", "player": "all"}]));
    let mut list = vec![
        json!({"op": "found_city", "player": 0, "x": 8, "y": 8, "name": "Roma"}),
        json!({"op": "found_city", "player": 1, "x": 13, "y": 8, "name": "Athens"}),
        json!({"op": "found_city", "player": 0, "x": 4, "y": 4, "name": "Antium"}),
        json!({"op": "found_city", "player": 1, "x": 18, "y": 12, "name": "Sparta"}),
        json!({"op": "set_relation", "a": 0, "b": 1, "state": "war"}),
    ];
    for (p, x) in [(0, 10), (1, 11)] {
        for (unit, y) in [("Warrior", 7), ("Archer", 8), ("Spearman", 9), ("Horseman", 10)] {
            list.push(json!({"op": "add_unit", "player": p, "unit": unit, "x": x, "y": y}));
        }
    }
    ops(&mut g, Value::Array(list));
    // Athens with its defences down and an army beside it, so that it falls.
    let athens = g.city_at(TileIdx(8 * 24 + 13)).map(|c| c.id().get()).expect("Athens");
    let mut siege = vec![json!({"op": "set_city", "city": athens, "health": 1})];
    for (x, y) in [(12, 7), (13, 7), (12, 9), (13, 9)] {
        siege.push(json!({"op": "add_unit", "player": 0, "unit": "Swordsman", "x": x, "y": y}));
    }
    ops(&mut g, Value::Array(siege));
    let (mut a, mut b) = (RandomAgent::new(), RandomAgent::new());
    let mut d = Drivers::none(g.state().players().len())
        .with(PlayerId(0), &mut a)
        .with(PlayerId(1), &mut b);
    let (stop, batch) = g.drive(&mut d, DriveOptions::default()).expect("a live game");
    assert_eq!(stop, Stop::GameOver);
    clean(&mut g);
    let kinds = |k: &str| batch.events().iter().filter(|e| e.kind.name() == k).count();
    assert!(kinds("combat") + kinds("unit_killed") > 0, "they fought");
    assert!(kinds("city_captured") > 0, "Athens fell");
    assert!(g.state().ids().combat_seq > 0);
}

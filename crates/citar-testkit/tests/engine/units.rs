//! Units and movement (package 1c-02), on games built by hand with the kitchen-sink ruleset: the
//! extra types of this subsystem, each once, beside the shipped rules they refine.
//! - `Can upgrade to [unit]` (the Kitchen Sink Raider to the Swordsman);
//! - `[n] Movement point cost to embark` (the Raider embarks for one point, not all it has);
//! - `[n]% XP required for promotions` (the Kitchen Sink nation's units need 8 where 10 is usual);
//! - `May travel on Water tiles without embarking` (the Kitchen Sink Hovercraft);
//! - `Cannot embark` (the Kitchen Sink Walker);
//! - `This Promotion is free` (the Kitchen Sink Veteran costs no experience);
//! - `upon being promoted` (the Raider loses a movement point when promoted);
//! - `upon gaining a [unit]` (the nation's gift for a Great Prophet is found where a city gains
//!   one; what it gives is a one-time effect, package 1b-08's);
//! - the one-time effects on a unit (damage, a promotion, a free upgrade, movement, experience,
//!   being destroyed), which ruins and combat's triggers apply.
//!
//! Every check, the cache oracle among them, is clean after each step.

use citar_engine::base::ids::{
    BarbarianLevelId, BaseUnitId, CityId, DifficultyId, EraId, MapSizeId, MapTypeId, NationId,
    PlayerId, PromotionId, SpeedId, TechId, TerrainId, TileIdx, UniqueId, UnitId,
};
use citar_engine::base::sets::PlayerVec;
use citar_engine::game::path::{ALL, Blocked, Mover};
use citar_engine::game::units::{self, health, promotions, upgrades};
use citar_engine::game::{DebugOptions, Game, movement, triggers};
use citar_engine::rules::{Named, Ruleset};
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::cities::{Cities, City};
use citar_engine::state::config::{GameConfig, MapEdges, MapSource};
use citar_engine::state::map::{MapInfo, Tile, Tiles};
use citar_engine::state::players::{Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides};
use citar_engine::state::{State, TileClaim};
use citar_engine::unique::UniqueType;
use citar_engine::unique::trigger::{TriggerEvent, TriggerSite};
use citar_testkit::rulesets::kitchen_sink;
use serde_json::json;

const W: u16 = 10;
const H: u16 = 8;
const ME: PlayerId = PlayerId(0);
const THEM: PlayerId = PlayerId(1);

fn id<I: Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("the ruleset has {name}"))
}

fn at(x: u32, y: u32) -> TileIdx {
    TileIdx(y * u32::from(W) + x)
}

/// A 10 by 8 game: coast in the two westmost columns, grassland elsewhere; the first player is
/// the Kitchen Sink, with a city at (5,4) whose neighbours it owns; the second has nothing.
fn game(r: &'static Ruleset) -> Game {
    let size = u32::from(W) * u32::from(H);
    let player = |n: u8, nation: &str| {
        let seat = Seat::new(Controller::Bot, SeatOverrides::default(), None);
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
    parts.cities = Cities::from_cities(vec![City::new(c, "Sinkhold".into(), ME, at(5, 4), 1)])
        .expect("cities");
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

/// Adds a unit through the scenario operation, and returns its id.
fn add(g: &mut Game, p: PlayerId, unit: &str, t: TileIdx) -> UnitId {
    let (x, y) = g.grid().xy(t);
    let (out, _) = g
        .apply_ops(&json!([{"op": "add_unit", "player": p.0, "unit": unit, "x": x, "y": y}]))
        .unwrap_or_else(|e| panic!("add_unit {unit}: {e}"));
    let n = out[0]["unit_ids"][0].as_u64().expect("a unit id");
    UnitId::new(u32::try_from(n).expect("an id")).expect("an id")
}

fn grant(g: &mut Game, p: PlayerId, tech: &str) {
    let t = id::<TechId>(g.rules(), tech);
    g.apply_ops(&json!([{"op": "grant_tech", "player": p.0, "tech": g.rules().name(t)}]))
        .unwrap_or_else(|e| panic!("grant_tech {tech}: {e}"));
}

fn test_ops(g: &mut Game, ops: serde_json::Value) {
    citar_engine::api::testops::apply(g, &ops).unwrap_or_else(|e| panic!("{ops}: {e}"));
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

#[test]
fn a_unit_can_upgrade_to_what_its_uniques_say() {
    let r = kitchen_sink();
    let mut g = game(r);
    let raider = add(&mut g, ME, "Kitchen Sink Raider", at(5, 4));
    clean(&mut g);
    assert_eq!(upgrades::upgrade_targets(&g, raider, false), [id::<BaseUnitId>(r, "Swordsman")]);
    let c = upgrades::check_upgrade(&g, raider);
    assert_eq!(c.refusal.as_deref(), Some("Swordsman requires Iron Working."));
    // Free, it needs neither the tech nor iron, and keeps its experience.
    test_ops(&mut g, json!([{"op": "set_unit", "unit": raider.get(), "xp": 7}]));
    assert!(upgrades::free_upgrade(&mut g, raider, false));
    g.settle_for_test();
    clean(&mut g);
    let swords: Vec<_> =
        g.player_units(ME).filter(|u| r.name(u.base) == Some("Swordsman")).collect();
    assert_eq!(swords.len(), 1);
    assert_eq!((swords[0].xp, swords[0].moves, swords[0].tile()), (7, 0, at(5, 4)));
    assert!(g.unit(raider).is_none(), "the old unit is gone");
}

#[test]
fn a_reduced_embark_cost_is_what_embarking_costs() {
    let r = kitchen_sink();
    let mut g = game(r);
    grant(&mut g, ME, "Optics");
    let raider = add(&mut g, ME, "Kitchen Sink Raider", at(2, 4));
    let warrior = add(&mut g, ME, "Warrior", at(2, 6));
    clean(&mut g);
    let sc = r.constants().move_scale;
    assert_eq!(movement::enter_cost(&g, raider, at(2, 4), at(1, 4)), sc, "one movement point");
    assert_eq!(movement::enter_cost(&g, warrior, at(2, 6), at(1, 6)), ALL, "all it has");
    // Stepping onto the coast leaves the raider moves to spare, embarked.
    let before = g.unit(raider).map_or(0, |u| u.moves);
    movement::step(&mut g, raider, at(1, 4)).expect("a step onto the coast");
    g.settle_for_test();
    clean(&mut g);
    assert_eq!(g.unit(raider).map(|u| u.moves), Some(before - sc));
    assert!(movement::is_embarked(&g, raider));
}

#[test]
fn a_nation_can_need_less_experience_for_a_promotion() {
    let r = kitchen_sink();
    let mut g = game(r);
    let mine = add(&mut g, ME, "Warrior", at(6, 4));
    let theirs = add(&mut g, THEM, "Warrior", at(8, 1));
    clean(&mut g);
    assert_eq!(promotions::xp_for_next(&g, mine), 8, "[-20]% XP required for promotions");
    assert_eq!(promotions::xp_for_next(&g, theirs), 10);
}

#[test]
fn a_unit_may_travel_on_water_without_embarking() {
    let r = kitchen_sink();
    let mut g = game(r);
    let hover = add(&mut g, ME, "Kitchen Sink Hovercraft", at(2, 4));
    clean(&mut g);
    let m = Mover::unit(&g, hover).expect("a unit");
    assert_eq!(m.pass_reason(at(1, 4)), None, "no Optics needed");
    let sc = r.constants().move_scale;
    assert_eq!(m.edge_cost(at(2, 4), at(1, 4)), sc, "the coast's own cost, no embarking");
    movement::step(&mut g, hover, at(1, 4)).expect("a step onto the coast");
    g.settle_for_test();
    clean(&mut g);
    assert!(!movement::is_embarked(&g, hover));
    assert_eq!(health::max_movement(&g, hover), 3, "its own movement, not an embarked unit's");
}

#[test]
fn a_unit_that_cannot_embark_stays_ashore() {
    let r = kitchen_sink();
    let mut g = game(r);
    grant(&mut g, ME, "Optics");
    let walker = add(&mut g, ME, "Kitchen Sink Walker", at(2, 4));
    let worker = add(&mut g, ME, "Worker", at(2, 6));
    clean(&mut g);
    let m = Mover::unit(&g, walker).expect("a unit");
    assert_eq!(m.pass_reason(at(1, 4)), Some(Blocked::CannotEmbark));
    assert_eq!(Blocked::CannotEmbark.text(&g), "This unit cannot embark.");
    let other = Mover::unit(&g, worker).expect("a unit");
    assert_eq!(other.pass_reason(at(1, 6)), None, "other civilians embark with Optics");
    assert!(movement::find_path(&g, walker, at(0, 4), 40).is_none(), "no way over the water");
}

#[test]
fn a_free_promotion_costs_no_experience() {
    let r = kitchen_sink();
    let mut g = game(r);
    let w = add(&mut g, ME, "Swordsman", at(6, 4));
    clean(&mut g);
    let veteran = id::<PromotionId>(r, "Kitchen Sink Veteran");
    assert!(promotions::available_promotions(&g, w).contains(&veteran));
    assert!(promotions::can_promote(&g, w), "a free promotion needs no experience");
    // A paid one does, though a free one is on offer: Python let it through.
    let shock = promotions::plan_promotion(&g, w, "Shock I").map_err(|e| e.message);
    assert_eq!(shock, Err("Not enough XP (0/8).".to_owned()));
    let pr = promotions::plan_promotion(&g, w, "kitchen sink veteran").expect("available");
    promotions::add_promotion(&mut g, w, pr, false);
    g.settle_for_test();
    clean(&mut g);
    let x = g.unit(w).expect("the unit");
    assert!(x.promotions.contains(veteran));
    assert_eq!((x.xp, x.promotion_count), (0, 0), "nothing spent, nothing counted");
}

#[test]
fn a_unit_can_lose_movement_when_it_is_promoted() {
    let r = kitchen_sink();
    let mut g = game(r);
    let raider = add(&mut g, ME, "Kitchen Sink Raider", at(6, 4));
    test_ops(&mut g, json!([{"op": "set_unit", "unit": raider.get(), "xp": 20}]));
    clean(&mut g);
    let before = g.unit(raider).map_or(0, |u| u.moves);
    let pr = promotions::plan_promotion(&g, raider, "Shock I").expect("available");
    promotions::add_promotion(&mut g, raider, pr, false);
    g.settle_for_test();
    clean(&mut g);
    let x = g.unit(raider).expect("the unit");
    let sc = r.constants().move_scale;
    assert_eq!(x.moves, before - sc, "[This Unit] loses [1] movement <upon being promoted>");
    assert_eq!((x.xp, x.promotion_count), (12, 1));
    // A promotion given free (a unit's base promotions, a ruin's) fires nothing.
    let drill = id::<PromotionId>(r, "Drill I");
    promotions::add_promotion(&mut g, raider, drill, true);
    assert_eq!(g.unit(raider).map(|u| u.moves), Some(before - sc));
}

#[test]
fn gaining_a_unit_in_a_city_finds_its_trigger() {
    let r = kitchen_sink();
    let mut g = game(r);
    clean(&mut g);
    let prophet = id::<BaseUnitId>(r, "Great Prophet");
    let u = units::add_unit_in_city(&mut g, CityId::FIRST, prophet).expect("room by the city");
    g.settle_for_test();
    clean(&mut g);
    assert_eq!(g.unit(u).map(|x| (x.origin_city, x.owner())), Some((Some(CityId::FIRST), ME)));
    let site = TriggerSite { civ: ME, city: None, unit: Some(u), tile: None };
    let found = citar_engine::unique::trigger::fire(
        &g.view(),
        &site,
        &TriggerEvent::GainingUnit(prophet),
        true,
    );
    assert_eq!(found.len(), 1, "Gain [10] [Faith] <upon gaining a [Great Prophet] unit>");
    let warrior = id::<BaseUnitId>(r, "Warrior");
    let other = citar_engine::unique::trigger::fire(
        &g.view(),
        &site,
        &TriggerEvent::GainingUnit(warrior),
        true,
    );
    assert!(other.is_empty(), "the filter reads the unit gained");
}

/// The first unique of type `ty` among the ruins' and the units' of the ruleset.
fn one_time(r: &Ruleset, ty: UniqueType) -> UniqueId {
    let t = r.uniques();
    r.ruins()
        .iter()
        .flat_map(|(_, d)| d.uniques.ids())
        .chain(r.base_units().iter().flat_map(|(_, d)| d.uniques.ids()))
        .find(|&id| t.meta(id).ty == Some(ty))
        .unwrap_or_else(|| panic!("a unique of type {ty:?}"))
}

/// What a one-time unique does to the unit in context, each kind once, as ruins (package 1b-08)
/// and combat's triggers (1c-03) will apply them: the kitchen sink's trap (damage), old master
/// (a promotion), better equipment (a free upgrade) and lost unit (destroyed), the Raider's
/// `gains [1] movement`, and the shipped ruins' experience, announced with its cause.
#[test]
fn a_one_time_unique_does_its_part_to_the_unit() {
    let r = kitchen_sink();
    let mut g = game(r);
    let raider = add(&mut g, ME, "Kitchen Sink Raider", at(6, 4));
    clean(&mut g);
    let site = |u| TriggerSite { civ: ME, city: None, unit: Some(u), tile: None };
    let apply = |g: &mut Game, ty, u, note| {
        let done = triggers::apply(g, one_time(r, ty), &site(u), note);
        g.settle_for_test();
        clean(g);
        done
    };
    assert!(apply(&mut g, UniqueType::OneTimeUnitDamage, raider, None));
    assert_eq!(g.unit(raider).map(|x| x.hp), Some(80), "[This Unit] takes [20] damage");

    let veteran = id::<PromotionId>(r, "Kitchen Sink Veteran");
    assert!(apply(&mut g, UniqueType::OneTimeUnitGainPromotion, raider, None));
    let x = g.unit(raider).expect("the unit");
    assert!(x.promotions.contains(veteran));
    assert_eq!((x.xp, x.promotion_count), (0, 0), "a promotion given costs nothing");

    let before = g.unit(raider).map_or(0, |x| x.moves);
    assert!(apply(&mut g, UniqueType::OneTimeUnitGainMovement, raider, None));
    let sc = r.constants().move_scale;
    assert_eq!(
        g.unit(raider).map(|x| x.moves),
        Some(before + sc),
        "[This Unit] gains [1] movement"
    );

    let since = g.events(0, usize::MAX).last().map_or(0, |e| e.id.get());
    assert!(apply(&mut g, UniqueType::OneTimeUnitGainXP, raider, Some("from the ruins")));
    assert_eq!(g.unit(raider).map(|x| x.xp), Some(10));
    let told: Vec<&str> = g.events(since, usize::MAX).iter().map(|e| e.text.as_ref()).collect();
    assert_eq!(told, ["Civ 0's Kitchen Sink Raider gained 10 XP (from the ruins)."]);

    // A free upgrade: the Swordsman keeps the Raider's health, experience and promotions.
    assert!(apply(&mut g, UniqueType::OneTimeUnitUpgrade, raider, None));
    assert!(g.unit(raider).is_none(), "the Raider is replaced");
    let swords: Vec<_> =
        g.player_units(ME).filter(|u| r.name(u.base) == Some("Swordsman")).collect();
    assert_eq!(swords.len(), 1);
    let s = swords[0];
    assert_eq!((s.hp, s.xp, s.tile(), s.moves), (80, 10, at(6, 4), 0));
    assert!(s.promotions.contains(veteran));
    let sword = s.id();

    assert!(apply(&mut g, UniqueType::OneTimeUnitDestroyed, sword, None));
    assert!(g.unit(sword).is_none(), "[This Unit] is destroyed");
    assert_eq!(g.player_units(ME).count(), 0);
}

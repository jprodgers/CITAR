//! One `basic-1` turn of its units (package 2-03) where no rule script looks: what the bot keeps
//! in its memory, and the fixes, which Python's runner could not show.
//!
//! - The garrisons are remembered (`memory.garrisons`), which the production advisor reads.
//! - A unit with experience takes a promotion: in one of its cities the first of
//!   `promo_in_city`, elsewhere the first of `promo_lines`.
//! - A settler turned back `settler_max_retreats` times gives its site up (`memory.bad_sites`).
//! - A war plan for a city the seat is no longer at war with is forgotten (a fix).
//! - A wounded ship heals; an aircraft strikes an enemy in range.
//! - A unit walks to ancient ruins near it, and clears a barbarian camp near home.

use std::sync::Arc;

use citar_bot::memory::WarPlan;
use citar_bot::{Bot, BotSpec, Memory, Tuning, VersionId, clean};
use citar_engine::api::testops;
use citar_engine::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{DebugOptions, DriveOptions, DriverOutcome, Drivers, Game, SeatDriver};
use citar_engine::rules::Ruleset;
use citar_engine::state::players::DriverMemory;
use citar_engine::state::units::Activity;
use citar_testkit::script::map_doc;
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);
const THEM: PlayerId = PlayerId(1);

/// The arena with two majors and nothing on it, every check on.
fn arena() -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let seat = json!({"controller": "bot"});
    let cfg = json!({"seed": 11, "map": doc, "players": [seat, seat], "city_states": 0,
                     "barbarians": "off", "ruins": false});
    let r = Ruleset::shared();
    let (mut g, _) = Game::new(r, &config_from_value(r, cfg).expect("settings")).expect("a game");
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("bare");
    g
}

fn ops(g: &mut Game, ops: &Value) -> Vec<Value> {
    g.apply_ops(ops).unwrap_or_else(|e| panic!("{e:?}")).0
}

fn test_ops(g: &mut Game, ops: &Value) -> Vec<Value> {
    testops::apply(g, ops).unwrap_or_else(|e| panic!("{e:?}")).0
}

fn tile(g: &Game, x: i32, y: i32) -> TileIdx {
    let w = i32::from(g.state().map().width);
    TileIdx(u32::try_from(y * w + x).expect("on the map"))
}

/// The unit an `add_unit` made.
fn unit(out: &Value) -> UnitId {
    out["unit_ids"][0]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(UnitId::new)
        .unwrap_or_else(|| panic!("no unit in {out}"))
}

fn city(out: &Value) -> CityId {
    out["city_id"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(CityId::new)
        .unwrap_or_else(|| panic!("no city in {out}"))
}

/// The bot of `params` (overrides of basic-1's defaults, plus `tech_noise` 0).
fn bot(params: &Value) -> Bot {
    let mut p = params.as_object().cloned().unwrap_or_default();
    p.insert("tech_noise".to_owned(), json!(0));
    let o = clean("basic-1", &Value::Object(p)).expect("parameters");
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, o));
    Bot::new(Arc::new(BotSpec::new(VersionId::Basic1, tuning, None, None)))
}

/// A driver that leaves the seat `memory`, as an earlier turn would have, then plays the bot.
struct Remembering<'a>(Option<Memory>, &'a mut Bot);

impl SeatDriver for Remembering<'_> {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        if let Some(m) = self.0.take() {
            *mem = m.encode().expect("small");
        }
        self.1.play_turn(g, pid, mem)
    }

    fn respond(
        &mut self,
        g: &mut Game,
        pid: PlayerId,
        nid: citar_engine::base::ids::NegotiationId,
        mem: &mut DriverMemory,
    ) -> DriverOutcome {
        self.1.respond(g, pid, nid, mem)
    }
}

/// My turn played by `d`, every check holding after it.
fn turn(g: &mut Game, d: &mut dyn SeatDriver) {
    let n = g.state().players().len();
    let mut drivers = Drivers::none(n).with(ME, d);
    g.drive(&mut drivers, DriveOptions::default().with_seat_limit(1)).expect("live");
    drop(drivers);
    let v = g.take_violations();
    assert!(v.is_empty(), "{v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
}

/// Their turn, ended as a host ends it.
fn their_turn(g: &mut Game) {
    g.end_turn(THEM).expect("their turn ends");
}

fn memory(g: &Game) -> Memory {
    g.player(ME).and_then(|p| p.seat().driver()).map(Memory::decode).unwrap_or_default()
}

/// My capital at A and theirs at B, met.
fn two_capitals(g: &mut Game) -> CityId {
    let out = ops(
        g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "capital": true},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "meet", "a": 0, "b": 1},
        ]),
    );
    city(&out[0])
}

#[test]
fn a_garrison_is_remembered_for_production() {
    let mut g = arena();
    let c = two_capitals(&mut g);
    let out =
        ops(&mut g, &json!([{"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5}]));
    let w = unit(&out[0]);
    turn(&mut g, &mut bot(&json!({})));
    assert_eq!(memory(&g).garrisons.get(&c), Some(&w));
    assert_eq!(g.unit(w).and_then(|x| x.activity), Some(Activity::Fortify));
}

#[test]
fn a_unit_with_experience_takes_cover_in_a_city_and_drill_outside() {
    let mut g = arena();
    let _ = two_capitals(&mut g);
    let out = ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 9, "y": 9},
        ]),
    );
    let (inside, outside) = (unit(&out[0]), unit(&out[1]));
    test_ops(
        &mut g,
        &json!([
            {"op": "set_unit", "unit": inside.get(), "xp": 15},
            {"op": "set_unit", "unit": outside.get(), "xp": 15},
        ]),
    );
    turn(&mut g, &mut bot(&json!({})));
    let names = |u: UnitId| -> Vec<String> {
        let r = g.rules();
        g.unit(u)
            .map(|x| x.promotions.iter().filter_map(|p| r.name(p)).map(str::to_owned).collect())
            .unwrap_or_default()
    };
    assert_eq!(names(inside), ["Cover I"]);
    assert_eq!(names(outside), ["Drill I"]);
}

/// The escort script's arena: the best site at (9,4), an enemy warrior at (7,4) in reach of the
/// way, at war; the settler in the capital with nobody to escort it.
fn settler_in_danger(g: &mut Game) -> UnitId {
    let out = ops(
        g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "capital": true},
            {"op": "add_unit", "player": 0, "unit": "Settler", "x": 5, "y": 5},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "reveal", "player": 0},
            {"op": "set_tile", "x": 10, "y": 4, "resource": "Wine"},
            {"op": "set_tile", "x": 9, "y": 4, "resource": "Incense"},
            {"op": "set_tile", "x": 10, "y": 6, "resource": "Wheat"},
            {"op": "set_tile", "x": 8, "y": 6, "resource": "Cattle"},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "set_relation", "a": 0, "b": 1, "state": "war"},
            {"op": "add_unit", "player": 1, "unit": "Warrior", "x": 7, "y": 4},
        ]),
    );
    unit(&out[1])
}

#[test]
fn a_settler_turned_back_three_times_gives_its_site_up() {
    let mut g = arena();
    let settler = settler_in_danger(&mut g);
    let home = tile(&g, 5, 5);
    let mut b = bot(&json!({}));
    turn(&mut g, &mut b);
    let m = memory(&g);
    let (&site, &n) = m.retreats.iter().next().expect("a retreat counted");
    assert_eq!(n, 1);
    // It waits in the capital for an escort, which the capital builds (and then forgets the
    // wait, as Python's production did once it chose the defender).
    let military = |g: &Game| {
        let head = g.city_at(home).and_then(|c| c.queue.first().copied());
        matches!(head, Some(citar_engine::state::cities::Constructible::Unit(u))
            if g.rules().base_units()[u].military)
    };
    assert!(military(&g), "the capital builds a defender");
    assert_eq!(m.need_escort, None);
    their_turn(&mut g);
    turn(&mut g, &mut b);
    assert_eq!(memory(&g).retreats.get(&site), Some(&2));
    their_turn(&mut g);
    turn(&mut g, &mut b);
    let m = memory(&g);
    assert_eq!(m.bad_sites.get(&site), Some(&g.turn()), "the third retreat gives it up");
    assert!(!m.retreats.contains_key(&site));
    assert_eq!(g.unit(settler).map(citar_engine::state::units::Unit::tile), Some(home));
}

#[test]
fn a_war_plan_for_a_city_at_peace_is_forgotten() {
    let mut g = arena();
    let _ = two_capitals(&mut g);
    ops(&mut g, &json!([{"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5}]));
    let plan = WarPlan {
        city: tile(&g, 18, 10),
        since: 0,
        advance: true,
        checked: None,
        rally: Some(tile(&g, 14, 9)),
        siege_ready: false,
    };
    let planted = Memory { war_plan: Some(plan.clone()), ..Memory::default() };
    // At war, the plan stands.
    ops(&mut g, &json!([{"op": "set_relation", "a": 0, "b": 1, "state": "war"}]));
    let mut b = bot(&json!({}));
    turn(&mut g, &mut Remembering(Some(planted), &mut b));
    assert_eq!(memory(&g).war_plan.map(|w| w.city), Some(plan.city));
    their_turn(&mut g);
    // At peace it no longer holds, and is gone after the seat's next turn.
    ops(&mut g, &json!([{"op": "set_relation", "a": 0, "b": 1, "state": "peace"}]));
    turn(&mut g, &mut b);
    assert_eq!(memory(&g).war_plan, None);
}

#[test]
fn a_wounded_ship_heals() {
    let mut g = arena();
    let _ = two_capitals(&mut g);
    let out = ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Trireme", "x": 2, "y": 5},
        ]),
    );
    let ship = unit(&out[1]);
    test_ops(&mut g, &json!([{"op": "set_unit", "unit": ship.get(), "hp": 40}]));
    turn(&mut g, &mut bot(&json!({})));
    assert_eq!(g.unit(ship).and_then(|x| x.activity), Some(Activity::Heal));
}

#[test]
fn an_aircraft_strikes_an_enemy_in_range() {
    let mut g = arena();
    let _ = two_capitals(&mut g);
    let out = ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Bomber", "x": 5, "y": 5},
            {"op": "set_relation", "a": 0, "b": 1, "state": "war"},
            {"op": "add_unit", "player": 1, "unit": "Warrior", "x": 7, "y": 6},
        ]),
    );
    let enemy = unit(&out[3]);
    let mut b = bot(&json!({}));
    turn(&mut g, &mut b);
    assert_eq!(b.refusals().of("attack"), (1, 0), "{:?}", b.refusals());
    assert!(g.unit(enemy).is_none_or(|x| x.hp < 100), "the warrior was hit");
}

#[test]
fn a_unit_walks_to_ruins_near_it() {
    let mut g = arena();
    let _ = two_capitals(&mut g);
    ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 9, "y": 9},
            {"op": "set_tile", "x": 10, "y": 9, "improvement": "Ancient ruins"},
            {"op": "reveal", "player": 0},
        ]),
    );
    let ruins = tile(&g, 10, 9);
    turn(&mut g, &mut bot(&json!({})));
    // What the ruins gave may have been a new unit for the warrior (an upgrade): one of mine
    // stands there, and the ruins are gone.
    assert!(g.units_at(ruins).any(|x| x.owner() == ME), "nobody went to the ruins");
    assert_eq!(g.tile(ruins).and_then(citar_engine::state::map::Tile::improvement), None);
}

#[test]
fn a_barbarian_camp_near_home_is_cleared() {
    let mut g = arena();
    let _ = two_capitals(&mut g);
    let out = ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 8, "y": 8},
            {"op": "reveal", "player": 0},
        ]),
    );
    let w = unit(&out[1]);
    test_ops(&mut g, &json!([{"op": "create_camp", "x": 9, "y": 8}]));
    let camp = tile(&g, 9, 8);
    turn(&mut g, &mut bot(&json!({})));
    assert_eq!(g.unit(w).map(citar_engine::state::units::Unit::tile), Some(camp));
    let standing = g.state().world().camps.values().any(|c| c.tile == camp && !c.destroyed);
    assert!(!standing, "the camp is gone");
}

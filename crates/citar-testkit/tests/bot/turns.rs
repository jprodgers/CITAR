//! One `basic-1` turn's effects that no rule script pins (package 2-01b): the fixes among them,
//! which Python's runner could not show, and the records the bot keeps in its memory.
//!
//! - The danger purchase refills the queue it emptied (`basic1/gold.rs`; Python left it empty
//!   until the bot's next turn).
//! - A city that queues work boats remembers the turn (`memory.boat_turns`), which the advisor
//!   reads as a `BotFact`.
//! - A city fires at an enemy in range (`city_bombard`).
//! - A unit with gold to spare upgrades.
//! - An empire running out of gold puts its biggest city on the gold focus.

use std::sync::Arc;

use citar_bot::{Bot, BotSpec, Memory, Tuning, VersionId, clean};
use citar_engine::api::testops;
use citar_engine::base::ids::{CityId, PlayerId, TileIdx};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{DebugOptions, DriveOptions, Drivers, Game, Stop};
use citar_engine::rules::Ruleset;
use citar_engine::state::cities::{CityFocus, Constructible};
use citar_engine::state::diplo::NegStatus;
use citar_testkit::script::map_doc;
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);

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

/// A tile as the operations name it.
fn at(g: &Game, t: TileIdx) -> (i32, i32) {
    g.xy(t)
}

fn tile(g: &Game, x: i32, y: i32) -> TileIdx {
    let w = i32::from(g.state().map().width);
    TileIdx(u32::try_from(y * w + x).expect("on the map"))
}

fn founded(out: &Value) -> CityId {
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

/// Plays my turn with `b` alone, as the rule scripts' bot step does: a negotiation the bot opens
/// with a seat nobody drives is closed when the drive stops for it, as a host's wait runs out.
fn turn(g: &mut Game, b: &mut Bot) {
    let n = g.state().players().len();
    for _ in 0..100 {
        let mut d = Drivers::none(n).with(ME, &mut *b);
        let (stop, _) = g.drive(&mut d, DriveOptions::default().with_seat_limit(1)).expect("live");
        drop(d);
        let Stop::AwaitingReply { nids, .. } = stop else { break };
        for nid in nids {
            g.close_negotiation(nid, NegStatus::Expired, "No answer came.", None).expect("open");
        }
    }
    let v = g.take_violations();
    assert!(v.is_empty(), "{v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
}

/// My capital at (5, 5), then `extra`.
fn capital(g: &mut Game, extra: &Value) -> CityId {
    capital_at(g, (5, 5), extra)
}

/// My capital at `(x, y)`, then `extra`.
fn capital_at(g: &mut Game, (x, y): (i32, i32), extra: &Value) -> CityId {
    let out = ops(g, &json!([{"op": "found_city", "player": 0, "x": x, "y": y, "capital": true}]));
    let c = founded(&out[0]);
    ops(g, extra);
    c
}

#[test]
fn the_danger_purchase_refills_the_queue_it_emptied() {
    let mut g = arena();
    let c = capital(
        &mut g,
        &json!([
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "grant_tech", "player": 0, "tech": "Archery"},
            {"op": "set_player", "player": 0, "gold": 500},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "set_relation", "a": 0, "b": 1, "state": "war"},
        ]),
    );
    let home = tile(&g, 5, 5);
    let near: Vec<TileIdx> = g.grid().neighbors(home).take(3).collect();
    let spearmen: Vec<Value> = near
        .iter()
        .map(|&t| {
            let (x, y) = at(&g, t);
            json!({"op": "add_unit", "player": 1, "unit": "Spearman", "x": x, "y": y})
        })
        .collect();
    ops(&mut g, &Value::Array(spearmen));
    turn(&mut g, &mut bot(&json!({})));
    let defender = g.military_at(home).map(|u| g.rules().base_units()[u.base].name.to_string());
    assert_eq!(defender.as_deref(), Some("Archer"), "a defender was bought");
    let queue = g.city(c).map(|x| x.queue.clone()).unwrap_or_default();
    assert!(!queue.is_empty(), "the queue the purchase emptied was refilled");
}

#[test]
fn a_city_that_queues_work_boats_remembers_the_turn() {
    let mut g = arena();
    // On the coast (the arena's F), the fish on the water beside it (W).
    let c = capital_at(
        &mut g,
        (3, 5),
        &json!([
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "grant_tech", "player": 0, "tech": "Sailing"},
            {"op": "set_tile", "x": 2, "y": 5, "resource": "Fish"},
        ]),
    );
    ops(&mut g, &json!([{"op": "set_city", "city": c.get(), "claim_radius": 3}]));
    let mut b = bot(&json!({"prod_mode": "classic", "c_boat": 10_000.0}));
    turn(&mut g, &mut b);
    let boats = g.rules().derived().advisor.boats;
    let head = g.city(c).and_then(|x| x.queue.first().copied());
    assert!(matches!(head, Some(Constructible::Unit(u)) if boats.contains(u)), "{head:?}");
    let mem = g.player(ME).and_then(|p| p.seat().driver()).expect("the bot kept a memory");
    let m = Memory::decode(mem);
    assert_eq!(m.boat_turns.get(&c), Some(&g.turn()));
}

#[test]
fn a_city_fires_at_an_enemy_in_range() {
    let mut g = arena();
    let c = capital(
        &mut g,
        &json!([
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "set_relation", "a": 0, "b": 1, "state": "war"},
        ]),
    );
    let next = g.grid().neighbors(tile(&g, 5, 5)).next().expect("a neighbour");
    let (x, y) = at(&g, next);
    ops(&mut g, &json!([{"op": "add_unit", "player": 1, "unit": "Warrior", "x": x, "y": y}]));
    turn(&mut g, &mut bot(&json!({})));
    assert!(g.city(c).is_some_and(|x| x.attacked), "the city fired");
    let hp = g.military_at(next).map_or(0, |u| u.hp);
    assert!(hp < 100, "the warrior was hit ({hp})");
}

#[test]
fn a_unit_upgrades_with_gold_to_spare() {
    let mut g = arena();
    let _ = capital(
        &mut g,
        &json!([
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "grant_tech", "player": 0, "tech": "Archery"},
            {"op": "grant_tech", "player": 0, "tech": "Construction"},
            {"op": "set_player", "player": 0, "gold": 600},
            {"op": "add_unit", "player": 0, "unit": "Archer", "x": 5, "y": 5},
        ]),
    );
    turn(&mut g, &mut bot(&json!({})));
    let names: Vec<String> =
        g.player_units(ME).map(|u| g.rules().base_units()[u.base].name.to_string()).collect();
    assert!(names.iter().any(|n| n == "Composite Bowman"), "{names:?}");
    assert!(!names.iter().any(|n| n == "Archer"), "{names:?}");
}

#[test]
fn an_empire_running_out_of_gold_puts_its_biggest_city_on_gold() {
    let mut g = arena();
    let c = capital(
        &mut g,
        &json!([
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "set_player", "player": 0, "gold": 5},
        ]),
    );
    // Warriors around the city cost more than it earns.
    let home = tile(&g, 5, 5);
    let ring: Vec<TileIdx> = g.grid().within(home, 2).into_iter().filter(|&t| t != home).collect();
    let warriors: Vec<Value> = ring
        .iter()
        .filter(|&&t| !g.is_water(t))
        .take(16)
        .map(|&t| {
            let (x, y) = at(&g, t);
            json!({"op": "add_unit", "player": 0, "unit": "Warrior", "x": x, "y": y})
        })
        .collect();
    ops(&mut g, &Value::Array(warriors));
    let gpt =
        citar_engine::game::query::civ_stats(&g, ME).total[citar_engine::base::stats::Stat::Gold];
    // A deficit of more than `gold_focus_deficit` a turn that would empty the treasury.
    assert!(gpt < -1.0, "the empire loses {gpt} a turn");
    turn(&mut g, &mut bot(&json!({"gold_focus_deficit": 1})));
    assert_eq!(g.city(c).map(|x| x.focus), Some(CityFocus::Gold));
}

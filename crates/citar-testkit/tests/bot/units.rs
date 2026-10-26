//! One `basic-1` turn of its units (package 2-03) where no rule script looks: what the bot keeps
//! in its memory, and the fixes, which Python's runner could not show.
//!
//! - The garrisons are remembered (`memory.garrisons`), which the production advisor reads.
//! - A unit with experience takes a promotion: in one of its cities the first of
//!   `promo_in_city`, elsewhere the first of `promo_lines`.
//! - A settler turned back `settler_max_retreats` times gives its site up (`memory.bad_sites`).
//! - A wounded ship heals; an aircraft strikes an enemy in range.
//! - A unit walks to ancient ruins near it, and clears a barbarian camp near home.
//!
//! And the seven fixes of package 2-03, one test each at least:
//! 1. a war plan for a city the seat is no longer at war with is forgotten;
//! 2. a great writer writes its political treatise;
//! 3. an inquisitor goes past its city with no religion to the one with a heresy, and removes it;
//! 4. a unit with no experience takes the free promotion it may;
//! 5. an enemy killed earlier in the turn is no settler's danger;
//! 6. a seat with no city makes no war plan;
//! 7. a settler goes only with its own escort: not with its city's garrison, which stays, nor
//!    alone; it steps out of a garrisoned city to the escort waiting beside it.

use std::sync::Arc;

use citar_bot::memory::WarPlan;
use citar_bot::{Bot, BotSpec, Memory, Tuning, VersionId, clean};
use citar_engine::api::testops;
use citar_engine::base::ids::{BaseUnitId, CityId, PlayerId, ReligionId, TileIdx, UnitId};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{
    Action, DebugOptions, DriveOptions, DriverOutcome, Drivers, Game, SeatDriver, Stop, religion,
    units,
};
use citar_engine::rules::Ruleset;
use citar_engine::state::diplo::NegStatus;
use citar_engine::state::players::DriverMemory;
use citar_engine::state::units::{Activity, Unit};
use citar_testkit::rulesets::{files_of, kitchen_sink, overlay};
use citar_testkit::script::map_doc;
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);
const THEM: PlayerId = PlayerId(1);

/// The arena with two majors and nothing on it, every check on.
fn arena() -> Game {
    arena_of(Ruleset::shared())
}

/// The arena on ruleset `r`.
fn arena_of(r: &'static Ruleset) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let seat = json!({"controller": "bot"});
    let cfg = json!({"seed": 11, "map": doc, "players": [seat, seat], "city_states": 0,
                     "barbarians": "off", "ruins": false});
    let (mut g, _) = Game::new(r, &config_from_value(r, cfg).expect("settings")).expect("a game");
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("bare");
    g
}

/// Where unit `u` stands, if it is still there.
fn at(g: &Game, u: UnitId) -> Option<TileIdx> {
    g.unit(u).map(Unit::tile)
}

/// A tool call as a host makes one, which must succeed.
fn tool(g: &mut Game, p: PlayerId, name: &str, args: &Value) -> Value {
    let mut fields = citar_engine::api::tools::normalize(name, args).expect("known");
    fields.insert("tool".into(), json!(name));
    let action: Action = serde_json::from_value(Value::Object(fields)).expect("the arguments fit");
    g.act(p, action).map(|(out, _)| out).unwrap_or_else(|e| panic!("{name}: {e:?}"))
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

/// My turn played by `d`, every check holding after it. A negotiation the bot opens with a seat
/// nobody drives stops the drive (its diplomacy, package 2-05): it is closed as a host closes
/// one whose wait runs out, and the drive goes on to the turn's end.
fn turn(g: &mut Game, d: &mut dyn SeatDriver) {
    let n = g.state().players().len();
    for _ in 0..100 {
        let mut drivers = Drivers::none(n).with(ME, &mut *d);
        let (stop, _) =
            g.drive(&mut drivers, DriveOptions::default().with_seat_limit(1)).expect("live");
        drop(drivers);
        let Stop::AwaitingReply { nids, .. } = stop else { break };
        for nid in nids {
            g.close_negotiation(nid, NegStatus::Expired, "No answer came.", None).expect("open");
        }
    }
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

// ---- Fix 7: a settler goes only with its own escort ---------------------------------------------

#[test]
fn a_settler_in_a_garrisoned_city_waits_for_an_escort_of_its_own() {
    let mut g = arena();
    let settler = settler_in_danger(&mut g);
    let out =
        ops(&mut g, &json!([{"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5}]));
    let garrison = unit(&out[0]);
    let home = tile(&g, 5, 5);
    turn(&mut g, &mut bot(&json!({})));
    let m = memory(&g);
    // The garrison on its tile is no escort: Python's settler walked out alone here.
    assert_eq!(at(&g, settler), Some(home), "it waits in the capital");
    assert_eq!(m.retreats.values().copied().collect::<Vec<_>>(), [1]);
    assert!(m.escorts.is_empty(), "the garrison escorts nobody: {:?}", m.escorts);
    assert_eq!(at(&g, garrison), Some(home));
    assert_eq!(m.garrisons.values().copied().collect::<Vec<_>>(), [garrison]);
    assert_eq!(g.unit(garrison).and_then(|x| x.activity), Some(Activity::Fortify));
}

#[test]
fn a_settler_steps_out_to_its_escort_beside_a_garrisoned_city_and_they_go_together() {
    let mut g = arena();
    let settler = settler_in_danger(&mut g);
    let out = ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 4, "y": 5},
        ]),
    );
    let (garrison, escort) = (unit(&out[0]), unit(&out[1]));
    let (home, beside) = (tile(&g, 5, 5), tile(&g, 4, 5));
    let mut b = bot(&json!({}));
    turn(&mut g, &mut b);
    let m = memory(&g);
    assert_eq!(m.escorts.get(&settler), Some(&escort));
    assert!(m.retreats.is_empty(), "{:?}", m.retreats);
    // A tile holds one military unit: the settler stepped out onto its escort's tile, and the
    // two stay there this turn.
    assert_eq!(at(&g, settler), Some(beside));
    assert_eq!(at(&g, escort), Some(beside));
    assert_eq!(at(&g, garrison), Some(home), "the garrison stays");
    their_turn(&mut g);
    turn(&mut g, &mut b);
    let (s, e) = (at(&g, settler), at(&g, escort));
    assert!(s.is_some_and(|x| x != beside && x != home), "it set out: {s:?}");
    assert_eq!(e, s, "the escort goes with it");
    assert_eq!(at(&g, garrison), Some(home));
}

// ---- Fixes 2 to 6 -------------------------------------------------------------------------------

/// The shipped ruleset with a great writer, which it has none of: a great person of culture whose
/// one ability is the political treatise.
fn with_a_writer() -> &'static Ruleset {
    static RULES: std::sync::OnceLock<&'static Ruleset> = std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        let writer = json!({"Great Writer": {
            "name": "Great Writer",
            "unitType": "Civilian",
            "movement": 2,
            "uniques": [
                "Great Person - [Culture]",
                "Can generate a large amount of culture <by consuming this unit>",
                "Unbuildable",
            ],
            "id": "great_writer",
        }})
        .to_string();
        let files = overlay(&[("ruleset/units.json", &writer)]).expect("the patch applies");
        Ruleset::leak(&files_of(&files)).unwrap_or_else(|e| panic!("{e}"))
    })
}

#[test]
fn a_great_writer_writes_its_treatise() {
    let mut g = arena_of(with_a_writer());
    let _ = two_capitals(&mut g);
    let out = ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Great Writer", "x": 5, "y": 5},
        ]),
    );
    let writer = unit(&out[1]);
    let culture = g.player(ME).map(|p| p.econ.culture).unwrap_or_default();
    let mut b = bot(&json!({}));
    turn(&mut g, &mut b);
    // Python's bot listed no such ability, and the writer followed the army all game.
    assert!(g.unit(writer).is_none(), "the writer is spent");
    assert_eq!(b.refusals().of("unit_action"), (1, 0), "{:?}", b.refusals());
    assert!(g.player(ME).is_some_and(|p| p.econ.culture > culture), "the treatise's culture");
}

#[test]
fn an_inquisitor_goes_past_a_city_with_no_religion_to_the_heresy() {
    let mut g = arena();
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "capital": true},
            {"op": "found_city", "player": 0, "x": 5, "y": 9},
            {"op": "found_city", "player": 0, "x": 11, "y": 5},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "reveal", "player": 0},
            {"op": "set_player", "player": "all", "faith": 25},
            {"op": "add_unit", "player": 0, "unit": "Great Prophet", "x": 5, "y": 5},
            {"op": "add_unit", "player": 1, "unit": "Great Prophet", "x": 18, "y": 10},
        ]),
    );
    let (roma, near, far) = (city(&out[0]), city(&out[1]), city(&out[2]));
    let (p0, p1) = (unit(&out[7]), unit(&out[8]));
    tool(&mut g, ME, "found_pantheon", &json!({"belief": "God of the Sea"}));
    let pantheon = g.player(ME).and_then(|p| p.religion.founded).expect("a pantheon");
    test_ops(
        &mut g,
        &json!([{"op": "found_religion", "unit": p0.get(), "name": "Mine",
                 "beliefs": ["Ceremonial Burial", "Feed the World"]}]),
    );
    test_ops(&mut g, &json!([{"op": "force_turn", "player": 1}]));
    tool(&mut g, THEM, "found_pantheon", &json!({"belief": "God of War"}));
    test_ops(
        &mut g,
        &json!([{"op": "found_religion", "unit": p1.get(), "name": "Yours",
                 "beliefs": ["Church Property", "Asceticism"]}]),
    );
    test_ops(&mut g, &json!([{"op": "force_turn", "player": 0}]));
    let mine = g.player(ME).and_then(|p| p.religion.founded).expect("a religion");
    let yours = g.player(THEM).and_then(|p| p.religion.founded).expect("a religion");
    religion::add_pressure(&mut g, far, Some(yours), 300);
    let base = g.rules().lookup::<BaseUnitId>("Inquisitor").expect("a unit");
    let u = units::add_unit_in_city(&mut g, roma, base).expect("made in the capital");
    assert_eq!(g.unit(u).and_then(|x| x.religion), Some(mine));
    // Every city of mine has my pantheon's pressure, which is no heresy; the far one has theirs.
    let pressure = |g: &Game, c: CityId, r: ReligionId| {
        g.city(c).and_then(|x| x.pressures.iter().find(|&&(k, _)| k == Some(r)).map(|&(_, n)| n))
    };
    let before: Vec<_> = [roma, near, far].map(|c| pressure(&g, c, yours)).into();
    assert_eq!(before, [None, None, Some(300)]);
    let grid = g.grid();
    let (from, to) = (tile(&g, 5, 5), tile(&g, 11, 5));
    assert!(grid.distance(from, tile(&g, 5, 9)) < grid.distance(from, to), "the nearer city");
    // Python's inquisitor walked beside the nearer city, whose majority (my pantheon) is not its
    // religion, and stood there; it goes to the heresy and removes it, and leaves the pantheon.
    let mut b = bot(&json!({}));
    let mut last = at(&g, u);
    for _ in 0..4 {
        last = at(&g, u);
        turn(&mut g, &mut b);
        if g.unit(u).is_none() {
            break;
        }
        their_turn(&mut g);
    }
    assert!(g.unit(u).is_none(), "used once, then used up: {:?}", at(&g, u));
    let land = last.and_then(|x| g.tile(x)).and_then(citar_engine::state::map::Tile::city);
    assert_eq!(land, Some(far), "it acted in the far city's land");
    assert_eq!(b.refusals().of("unit_action"), (1, 0), "{:?}", b.refusals());
    // Every other religion left it; its owner's end of turn brought a little of theirs back.
    let left = pressure(&g, far, yours).unwrap_or_default();
    assert!(left < 50 && pressure(&g, far, pantheon).is_none(), "{left}");
    assert!(pressure(&g, near, pantheon).is_some() && pressure(&g, roma, pantheon).is_some());
}

#[test]
fn a_unit_with_no_experience_takes_its_free_promotion() {
    let mut g = arena_of(kitchen_sink());
    let _ = two_capitals(&mut g);
    let out = ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Swordsman", "x": 9, "y": 9},
        ]),
    );
    let (warrior, sword) = (unit(&out[0]), unit(&out[1]));
    assert_eq!(g.unit(sword).map(|x| x.xp), Some(0));
    let mut b = bot(&json!({}));
    turn(&mut g, &mut b);
    // Python offered every promotion, and the engine refuses one it has no experience for. The
    // warrior is of the Sword type too, in its city: Cover I is no pick for it either.
    let r = g.rules();
    let names = |u: UnitId| -> Vec<&str> {
        g.unit(u)
            .map(|x| x.promotions.iter().filter_map(|p| r.name(p)).collect())
            .unwrap_or_default()
    };
    assert_eq!(names(sword), ["Kitchen Sink Veteran"]);
    assert_eq!(names(warrior), ["Kitchen Sink Veteran"]);
    assert_eq!(b.refusals().of("promote_unit"), (2, 0), "{:?}", b.refusals());
}

#[test]
fn an_enemy_killed_earlier_in_the_turn_is_no_danger() {
    let mut g = arena();
    let settler = settler_in_danger(&mut g);
    let enemy = g.units_at(tile(&g, 7, 4)).next().map(Unit::id).expect("the enemy");
    test_ops(&mut g, &json!([{"op": "set_unit", "unit": enemy.get(), "hp": 1}]));
    let mut b = bot(&json!({}));
    turn(&mut g, &mut b);
    // The capital's bombardment killed it before the units moved; Python's context still held
    // it, and the settler turned back.
    assert!(g.unit(enemy).is_none(), "the city shot it");
    let m = memory(&g);
    assert!(m.retreats.is_empty() && m.escorts.is_empty(), "{:?} {:?}", m.retreats, m.escorts);
    assert!(at(&g, settler).is_some_and(|x| x != tile(&g, 5, 5)), "the settler set out");
}

#[test]
fn a_seat_with_no_city_makes_no_war_plan() {
    let mut g = arena();
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "capital": true},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 12, "y": 8},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "reveal", "player": 0},
            {"op": "set_relation", "a": 0, "b": 1, "state": "war"},
        ]),
    );
    let w = unit(&out[1]);
    // With `war_target_max_dist` at its default of 99, the nearest-city default of 99 let a plan
    // in; Python's `min` over no city raised.
    let mut b = bot(&json!({}));
    turn(&mut g, &mut b);
    assert!(g.unit(w).is_some());
    assert_eq!(memory(&g).war_plan, None);
}

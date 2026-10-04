//! `basic-1`'s memory (DESIGN.md P2.3.4, package 2-01a gate 2): the codec round-trips to equal
//! bytes, the largest memory a seat of a 64-player game could keep fits `DriverMemory::MAX_LEN`,
//! a memory of another kind or version starts empty, and the drive keeps, prunes and writes back
//! a seat's memory.

use std::collections::BTreeMap;
use std::sync::Arc;

use citar_bot::memory::{MEMORY_KIND, MEMORY_VERSION, Memory, WarPlan, WarPrep};
use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
use citar_engine::api::testops;
use citar_engine::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{
    DebugOptions, DriveOptions, DriverOutcome, Drivers, Game, SeatDriver, Stop,
};
use citar_engine::rules::Ruleset;
use citar_engine::state::players::DriverMemory;
use citar_testkit::script::map_doc;
use proptest::prelude::*;
use proptest::test_runner::Config;
use serde_json::json;

/// The widest entity id a store takes (`state::store::MAX_ENTITY_ID` less one).
const WIDEST_ID: u32 = (1 << 24) - 1;

fn unit_id() -> impl Strategy<Value = UnitId> {
    (1..=WIDEST_ID).prop_map(|n| UnitId::new(n).expect("nonzero"))
}

fn city_id() -> impl Strategy<Value = CityId> {
    (1..=WIDEST_ID).prop_map(|n| CityId::new(n).expect("nonzero"))
}

fn tile() -> impl Strategy<Value = TileIdx> {
    (0u32..65_536).prop_map(TileIdx)
}

fn war_prep() -> impl Strategy<Value = WarPrep> {
    (0u8..64, any::<i32>(), proptest::option::of(tile()), proptest::option::of(tile()))
        .prop_map(|(p, since, target, rally)| WarPrep { player: PlayerId(p), since, target, rally })
}

fn war_plan() -> impl Strategy<Value = WarPlan> {
    (tile(), any::<i32>(), any::<bool>(), proptest::option::of(any::<i32>())).prop_flat_map(
        |(city, since, advance, checked)| {
            (proptest::option::of(tile()), any::<bool>()).prop_map(move |(rally, siege_ready)| {
                WarPlan { city, since, advance, checked, rally, siege_ready }
            })
        },
    )
}

fn memory() -> impl Strategy<Value = Memory> {
    (
        proptest::option::of(war_prep()),
        proptest::option::of(war_plan()),
        prop::collection::btree_map(unit_id(), unit_id(), 0..12),
        prop::collection::btree_map(city_id(), unit_id(), 0..12),
        prop::collection::btree_map(tile(), any::<u8>(), 0..12),
        prop::collection::btree_map(tile(), any::<i32>(), 0..12),
        proptest::option::of(tile()),
        prop::collection::btree_map(city_id(), any::<i32>(), 0..12),
    )
        .prop_map(
            |(war_prep, war_plan, escorts, garrisons, retreats, bad_sites, need_escort, boats)| {
                Memory {
                    war_prep,
                    war_plan,
                    escorts,
                    garrisons,
                    retreats,
                    bad_sites,
                    need_escort,
                    boat_turns: boats,
                }
            },
        )
}

/// At least the 1,000 cases the gate asks, more when `PROPTEST_CASES` says so.
fn config() -> Config {
    #[allow(clippy::disallowed_methods, reason = "a test's switch, not a game's input")]
    let env = std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse::<u32>().ok());
    Config {
        cases: env.unwrap_or(0).max(1_000),
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"),
        )),
        ..Config::default()
    }
}

proptest! {
    #![proptest_config(config())]

    /// Any memory encodes, decodes to itself, and encodes again to the same bytes.
    #[test]
    fn a_memory_round_trips_to_equal_bytes(m in memory()) {
        let enc = m.encode().expect("under the limit");
        prop_assert_eq!((enc.kind(), enc.version()), (MEMORY_KIND, MEMORY_VERSION));
        let back = Memory::decode(&enc);
        prop_assert_eq!(&back, &m);
        let again = back.encode().expect("under the limit");
        prop_assert_eq!(again.bytes(), enc.bytes());
    }
}

/// The largest memory a seat of a 64-player game could keep after pruning, every key and value
/// at its widest: on the largest map (256 by 256 tiles), every tile a site the seat retreated
/// from and gave up (a count of 255, a turn of ten digits), a settler with its escort on every
/// tile (one civilian a tile), a city every seventh tile (no city founds within two tiles of
/// another) with its garrison and its boat's turn, and both war records. Entity ids at their
/// widest are those of the 64 seats' units, cities and camps from one counter.
#[test]
fn a_64_player_worst_case_fits_under_the_limit() {
    let tiles: u32 = 256 * 256;
    let id = |n: u32| WIDEST_ID - n;
    let unit = |n: u32| UnitId::new(id(n)).expect("nonzero");
    let city = |n: u32| CityId::new(id(n)).expect("nonzero");
    let widest_turn = i32::MIN;
    let mut m = Memory {
        war_prep: Some(WarPrep {
            player: PlayerId(63),
            since: widest_turn,
            target: Some(TileIdx(tiles - 1)),
            rally: Some(TileIdx(tiles - 2)),
        }),
        war_plan: Some(WarPlan {
            city: TileIdx(tiles - 3),
            since: widest_turn,
            advance: true,
            checked: Some(widest_turn),
            rally: Some(TileIdx(tiles - 4)),
            siege_ready: true,
        }),
        need_escort: Some(TileIdx(tiles - 5)),
        ..Memory::default()
    };
    for t in 0..tiles {
        m.retreats.insert(TileIdx(t), u8::MAX);
        m.bad_sites.insert(TileIdx(t), widest_turn);
        // Two ids per tile: the settler's and its escort's.
        m.escorts.insert(unit(2 * t), unit(2 * t + 1));
    }
    for c in 0..tiles / 7 {
        m.garrisons.insert(city(c), unit(2 * tiles + c));
        m.boat_turns.insert(city(c), widest_turn);
    }
    // About 3.8 MB of the 4 MiB allowed. A memory a real game prunes is a few kilobytes: its
    // sites are those its settlers aimed at within the blacklist's turns, its escorts and
    // garrisons those of its own settlers and cities.
    let enc = m.encode().expect("a seat's worst case fits");
    let len = enc.bytes().len();
    assert!(len < DriverMemory::MAX_LEN, "{len} bytes against {}", DriverMemory::MAX_LEN);
    assert_eq!(Memory::decode(&enc), m);
}

#[test]
fn a_memory_of_another_kind_or_version_starts_empty() {
    let mut m = Memory { need_escort: Some(TileIdx(4)), ..Memory::default() };
    m.retreats.insert(TileIdx(9), 1);
    let enc = m.encode().expect("small");
    for (kind, version) in [(0, 1), (2, 1), (MEMORY_KIND, 0), (MEMORY_KIND, 2), (u16::MAX, 9)] {
        let other = DriverMemory::new(kind, version, enc.bytes().to_vec()).expect("small");
        assert_eq!(Memory::decode(&other), Memory::default(), "kind {kind}, version {version}");
    }
    for junk in [&b""[..], b"{", b"[]", b"{\"war_prep\":null}", b"{\"unknown\":1}"] {
        let other = DriverMemory::new(MEMORY_KIND, MEMORY_VERSION, junk.to_vec()).expect("small");
        assert_eq!(Memory::decode(&other), Memory::default(), "{junk:?}");
    }
    assert_eq!(Memory::decode(&enc), m);
}

/// The same memory always encodes to the same bytes, with its fields in declaration order and
/// its keys in order: the engine digests them.
#[test]
fn the_encoding_is_canonical_json() {
    let mut m = Memory::default();
    m.retreats.insert(TileIdx(30), 2);
    m.retreats.insert(TileIdx(4), 1);
    m.boat_turns.insert(CityId::new(2).expect("an id"), 12);
    let enc = m.encode().expect("small");
    assert_eq!(
        std::str::from_utf8(enc.bytes()).expect("JSON"),
        "{\"war_prep\":null,\"war_plan\":null,\"escorts\":{},\"garrisons\":{},\
         \"retreats\":{\"4\":1,\"30\":2},\"bad_sites\":{},\"need_escort\":null,\
         \"boat_turns\":{\"2\":12}}"
    );
}

// ---- The memory in a game ------------------------------------------------------------------

/// A bare arena game of two seats, each with a settler, and seat 0's guarded by a warrior.
fn arena() -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({"seed": 3, "map": doc, "players": [{"controller": "bot"}, {}],
                     "city_states": 0, "barbarians": "off", "ruins": false});
    let r = Ruleset::shared();
    let (mut g, _) = Game::new(r, &config_from_value(r, cfg).expect("settings")).expect("a game");
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("bare");
    g.apply_ops(&json!([
        {"op": "add_unit", "player": 0, "unit": "Settler", "x": 5, "y": 5},
        {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
        {"op": "add_unit", "player": 1, "unit": "Settler", "x": 18, "y": 10},
    ]))
    .expect("units");
    g
}

fn basic1() -> Bot {
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
    Bot::new(Arc::new(BotSpec::new(VersionId::Basic1, tuning, None, None)))
}

/// Seat 0's warrior taken off the map: a warrior in the capital is its garrison, which basic-1
/// remembers (`memory.garrisons`, package 2-03), so a seat with one always has something to keep.
fn without_the_warrior(g: &mut Game) {
    let warrior = g.player_units(PlayerId(0)).map(|u| u.id()).max().expect("the warrior");
    g.apply_ops(&json!([{"op": "remove_units", "unit": warrior.get()}])).expect("removed");
}

/// A driver that only leaves the seat the memory it is given, as a bot of an earlier session
/// would have.
struct Plant(Option<DriverMemory>);

impl SeatDriver for Plant {
    fn play_turn(&mut self, _: &mut Game, _: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        if let Some(m) = self.0.take() {
            *mem = m;
        }
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        _: &mut Game,
        _: PlayerId,
        _: citar_engine::base::ids::NegotiationId,
        _: &mut DriverMemory,
    ) -> DriverOutcome {
        DriverOutcome::Done
    }
}

/// Seat 0 played by `d` for one turn; the host's seat 1 then ends its own.
fn one_turn(g: &mut Game, d: &mut dyn SeatDriver) {
    let mut drivers = Drivers::none(2).with(PlayerId(0), d);
    let (stop, _) = g.drive(&mut drivers, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(1)));
    drop(drivers);
    g.end_turn(PlayerId(1)).expect("the host ends its seat's turn");
}

fn seat_memory(g: &Game) -> Option<DriverMemory> {
    g.player(PlayerId(0)).and_then(|p| p.seat().driver().cloned())
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
}

#[test]
fn a_seat_that_remembers_nothing_keeps_no_memory() {
    let mut g = arena();
    without_the_warrior(&mut g);
    let mut bot = basic1();
    for _ in 0..3 {
        one_turn(&mut g, &mut bot);
    }
    assert_eq!(seat_memory(&g), None, "nothing to remember, nothing kept");
    assert_eq!(g.player_cities(PlayerId(0)).count(), 1, "the capital");
    clean(&mut g);
}

#[test]
fn the_drive_prunes_a_seats_memory_and_keeps_the_rest() {
    let mut g = arena();
    // The plant plays this round; basic-1 starts the next one, and prunes as it starts.
    let turn = g.turn() + 1;
    let warrior: UnitId =
        g.player_units(PlayerId(0)).map(|u| u.id()).max().expect("the warrior, the later id");
    let settler: UnitId = g.player_units(PlayerId(0)).map(|u| u.id()).min().expect("a settler");
    let theirs: UnitId = g.player_units(PlayerId(1)).map(|u| u.id()).next().expect("theirs");
    let gone = UnitId::new(9_999).expect("an id");
    let nowhere = CityId::new(9_999).expect("an id");
    let mut m = Memory {
        war_prep: Some(WarPrep { player: PlayerId(1), since: turn, target: None, rally: None }),
        need_escort: Some(TileIdx(40)),
        ..Memory::default()
    };
    m.escorts.insert(settler, warrior); // both ours: kept
    m.escorts.insert(gone, warrior); // the settler is gone
    m.escorts.insert(theirs, warrior); // not our unit
    m.garrisons.insert(nowhere, warrior); // no such city
    m.retreats.insert(TileIdx(10), 2);
    m.retreats.insert(TileIdx(11), 0); // reset: reads as no entry
    m.bad_sites.insert(TileIdx(20), turn); // fresh
    m.bad_sites.insert(TileIdx(21), turn - 30); // as old as site_blacklist_turns: over
    m.bad_sites.insert(TileIdx(22), turn - 29);
    m.boat_turns.insert(nowhere, turn);
    one_turn(&mut g, &mut Plant(Some(m.encode().expect("small"))));
    let planted = seat_memory(&g).expect("the plant left it");
    assert_eq!(Memory::decode(&planted), m, "the seat keeps what its driver left");

    let mut bot = basic1();
    one_turn(&mut g, &mut bot);
    // This turn's start pruned the escorts of units gone or not the seat's, and the garrison of
    // a city that is not; the settler then founded the capital, and its escort, finding it gone,
    // let it go (basic.py:2149-2155).
    let kept = Memory::decode(&seat_memory(&g).expect("basic-1's memory"));
    let mut want = m.clone();
    want.escorts.clear();
    want.garrisons.clear();
    want.retreats.remove(&TileIdx(11));
    want.bad_sites.remove(&TileIdx(21));
    want.boat_turns.clear();
    assert_eq!(kept, want);

    one_turn(&mut g, &mut bot);
    let kept = Memory::decode(&seat_memory(&g).expect("basic-1's memory"));
    assert!(kept.escorts.is_empty(), "the settler founded a city and is gone: {kept:?}");
    assert_eq!(kept.war_prep, want.war_prep, "war records stay");
    assert_eq!(kept.need_escort, Some(TileIdx(40)));
    assert_eq!(
        kept.bad_sites,
        BTreeMap::from([(TileIdx(20), turn)]),
        "a site blacklisted 29 turns before the last turn is 30 turns old now"
    );
    clean(&mut g);
}

/// A memory another driver left reads as none: basic-1 starts fresh, and while it remembers
/// nothing it leaves the seat's as it was, so a seat handed back to that driver finds its own.
#[test]
fn a_foreign_memory_reads_as_none_and_the_idle_bot_keeps_none() {
    let mut g = arena();
    without_the_warrior(&mut g);
    let foreign = DriverMemory::new(7, 3, b"not ours".to_vec()).expect("small");
    one_turn(&mut g, &mut Plant(Some(foreign.clone())));
    assert_eq!(seat_memory(&g).as_ref(), Some(&foreign));
    // The idle bot keeps none and leaves the seat's as it was.
    let tuning = Arc::new(Tuning::new(VersionId::Idle, Overrides::default()));
    let idle_spec = BotSpec::new(VersionId::Idle, tuning, None, None);
    one_turn(&mut g, &mut Bot::new(Arc::new(idle_spec)));
    assert_eq!(seat_memory(&g).as_ref(), Some(&foreign));
    // basic-1 reads another driver's memory as nothing: remembering nothing, it keeps nothing of
    // the other's either.
    one_turn(&mut g, &mut basic1());
    assert_eq!(seat_memory(&g), Some(foreign), "nothing of its own to write");
    clean(&mut g);
}

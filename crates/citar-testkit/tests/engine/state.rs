//! The state model (package 1a-08), against models and recorded answers:
//! - `EntityStore` behaves as a `BTreeMap` under random inserts, removals and lookups, with
//!   compaction (gate 1);
//! - random unit spawns, moves, boardings, removals and owner changes keep the occupancy lists,
//!   owner lists and carrier links right after every step (gate 1);
//! - `PairMatrix` indexes every pair of up to 64 players exactly once, and the war and contact
//!   masks agree with the relations after random edits (gate 2);
//! - seats derive their handicap and automatic decisions as the Phase 0 Python tests say
//!   (`tests/test_controllers.py`, gate 3);
//! - every deal item reads and writes as the Python dict `diplomacy._normalize_items` leaves
//!   (`data/deal_items.json`, from `scripts/refcheck/deal_items.py`, gate 4);
//! - a tile is 16 bytes and a tile memory 8 (gate 5);
//! - `EngineEvent` covers every event type the Python engine emitted, `is_private` is its
//!   `PRIVATE_EVENTS`, and `EventData` and the stats rows have every key Python passed or
//!   recorded (gate 6), as `data/python_events.json` recorded them before package 2-12 deleted
//!   the Python engine.
//!
//! Gate 7, the restricted accessors, is `cargo xtask check`'s, tested in `xtask/src/check`.
//!
//! The number of proptest cases follows `PROPTEST_CASES` (proptest's default is 256).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use citar_engine::base::ids::{BaseUnitId, CityId, Id, PlayerId, TileIdx, UnitId};
use citar_engine::base::sets::PlayerSet;
use citar_engine::rules::Ruleset;
use citar_engine::state::chronicle::{CivStats, EngineEvent, EventData};
use citar_engine::state::diplo::{DealItem, DealItemKind, Diplomacy, PairMatrix, Terms};
use citar_engine::state::map::Tile;
use citar_engine::state::memory::TileMemory;
use citar_engine::state::players::{
    AutoDecisions, AutoOverrides, Controller, Handicap, Seat, SeatOverrides,
};
use citar_engine::state::store::{Entity, EntityStore};
use citar_engine::state::units::{Unit, Units, UnitsError};
use citar_engine::state::{Change, TileClaim};
use proptest::prelude::*;
use serde_json::{Value, json};

// ---- Gate 1: EntityStore against a BTreeMap -------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Thing(UnitId, u64);

impl Entity for Thing {
    type Id = UnitId;
    fn id(&self) -> UnitId {
        self.0
    }
}

#[derive(Clone, Debug)]
enum StoreOp {
    /// Insert the next id after a gap of this many.
    Insert(u32),
    /// Insert an id at or below the highest held, which must be refused.
    Reinsert(u32),
    /// Remove the n-th live id, if any.
    Remove(usize),
    /// Remove or look up an id that may or may not be there.
    Probe(u32),
    Compact,
}

fn store_op() -> impl Strategy<Value = StoreOp> {
    prop_oneof![
        6 => (0u32..4).prop_map(StoreOp::Insert),
        1 => (0u32..400).prop_map(StoreOp::Reinsert),
        5 => (0usize..400).prop_map(StoreOp::Remove),
        2 => (0u32..450).prop_map(StoreOp::Probe),
        1 => Just(StoreOp::Compact),
    ]
}

fn uid(n: u32) -> UnitId {
    UnitId::new(n).unwrap_or(UnitId::FIRST)
}

proptest! {
    #[test]
    fn entity_store_behaves_as_a_btreemap(ops in proptest::collection::vec(store_op(), 1..400)) {
        entity_store_case(ops)?;
    }
}

fn entity_store_case(ops: Vec<StoreOp>) -> Result<(), TestCaseError> {
    let mut store: EntityStore<UnitId, Thing> = EntityStore::new();
    let mut model: BTreeMap<UnitId, u64> = BTreeMap::new();
    let mut high = 0u32;
    for (step, op) in ops.into_iter().enumerate() {
        let payload = step as u64;
        match op {
            StoreOp::Insert(gap) => {
                high += 1 + gap;
                prop_assert!(store.insert(Thing(uid(high), payload)).is_ok());
                model.insert(uid(high), payload);
            }
            StoreOp::Reinsert(n) => {
                if high > 0 {
                    let id = uid(1 + n % high);
                    prop_assert!(store.insert(Thing(id, payload)).is_err(), "{id:?} is not new");
                }
            }
            StoreOp::Remove(i) => {
                if let Some(&id) = model.keys().nth(i % model.len().max(1)) {
                    prop_assert_eq!(store.remove(id).map(|t| t.1), model.remove(&id));
                }
            }
            StoreOp::Probe(n) => {
                let id = uid(n + 1);
                prop_assert_eq!(store.get(id).map(|t| t.1), model.get(&id).copied());
                prop_assert_eq!(store.contains(id), model.contains_key(&id));
                if n % 2 == 0 {
                    prop_assert_eq!(store.remove(id).map(|t| t.1), model.remove(&id));
                }
            }
            StoreOp::Compact => {
                store.compact();
                prop_assert_eq!(store.tombstones(), 0);
            }
        }
        prop_assert_eq!(store.len(), model.len());
        let got: Vec<(UnitId, u64)> = store.iter().map(|(id, t)| (id, t.1)).collect();
        let want: Vec<(UnitId, u64)> = model.iter().map(|(&id, &v)| (id, v)).collect();
        prop_assert_eq!(got, want);
        // Compaction keeps the tombstones to at most 64 or a quarter of the live count.
        let dead = store.tombstones();
        prop_assert!(
            dead <= 64 || dead <= store.len() / 4,
            "{} tombstones for {} live",
            dead,
            store.len()
        );
        prop_assert_eq!(store.last_id(), model.keys().next_back().copied());
    }
    Ok(())
}

// ---- Gate 1: units on a map -------------------------------------------------------------------

const TILES: u32 = 24;
const OWNERS: u8 = 4;

#[derive(Clone, Debug)]
enum UnitOp {
    Spawn { owner: u8, tile: u32 },
    Move { pick: usize, tile: u32 },
    Board { pick: usize, carrier: usize },
    Unboard { pick: usize },
    Despawn { pick: usize },
    Owner { pick: usize, owner: u8 },
}

fn unit_ops() -> impl Strategy<Value = Vec<UnitOp>> {
    proptest::collection::vec(unit_op(), 1..200)
}

fn unit_op() -> impl Strategy<Value = UnitOp> {
    // Few tiles, so units share them and carriers find cargo.
    let tile = 0u32..(TILES + 1);
    let owner = 0u8..OWNERS;
    let pick = 0usize..64;
    prop_oneof![
        4 => (owner.clone(), tile.clone()).prop_map(|(owner, tile)| UnitOp::Spawn { owner, tile }),
        4 => (pick.clone(), tile).prop_map(|(pick, tile)| UnitOp::Move { pick, tile }),
        4 => (pick.clone(), pick.clone())
            .prop_map(|(pick, carrier)| UnitOp::Board { pick, carrier }),
        1 => pick.clone().prop_map(|pick| UnitOp::Unboard { pick }),
        2 => pick.clone().prop_map(|pick| UnitOp::Despawn { pick }),
        2 => (pick, owner).prop_map(|(pick, owner)| UnitOp::Owner { pick, owner }),
    ]
}

/// A unit as the model holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mu {
    owner: PlayerId,
    tile: TileIdx,
    carried_by: Option<UnitId>,
}

/// The model's answer to a board: whether it is allowed.
fn may_board(model: &BTreeMap<UnitId, Mu>, u: UnitId, c: UnitId) -> bool {
    let (Some(x), Some(y)) = (model.get(&u), model.get(&c)) else { return false };
    u != c
        && x.tile == y.tile
        && y.carried_by.is_none()
        && !model.values().any(|m| m.carried_by == Some(u))
}

fn check_units(units: &Units, model: &BTreeMap<UnitId, Mu>) -> Result<(), TestCaseError> {
    prop_assert_eq!(units.verify(), Ok(()));
    prop_assert_eq!(units.len(), model.len());
    for (&id, m) in model {
        let u = units.get(id);
        prop_assert!(u.is_some(), "{id:?} is missing");
        let u = u.expect("checked");
        prop_assert_eq!(Mu { owner: u.owner(), tile: u.tile(), carried_by: u.carried_by() }, *m);
        if let Some(c) = m.carried_by {
            prop_assert_eq!(
                model.get(&c).map(|x| x.tile),
                Some(m.tile),
                "cargo shares its carrier's tile"
            );
        }
    }
    for t in 0..TILES {
        let t = TileIdx(t);
        let at: Vec<UnitId> = units.at(t).collect();
        let want: Vec<UnitId> =
            model.iter().filter(|(_, m)| m.tile == t).map(|(&id, _)| id).collect();
        prop_assert_eq!(at, want);
    }
    for p in 0..OWNERS {
        let p = PlayerId(p);
        let want: Vec<UnitId> =
            model.iter().filter(|(_, m)| m.owner == p).map(|(&id, _)| id).collect();
        prop_assert_eq!(units.of(p), want.as_slice());
    }
    Ok(())
}

proptest! {
    #[test]
    fn units_keep_occupancy_owners_and_cargo_right(ops in unit_ops()) {
        units_case(ops)?;
    }
}

fn units_case(ops: Vec<UnitOp>) -> Result<(), TestCaseError> {
    let mut units = Units::new(TILES);
    let mut model: BTreeMap<UnitId, Mu> = BTreeMap::new();
    let mut next = 1u32;
    for op in ops {
        let ids: Vec<UnitId> = model.keys().copied().collect();
        let pick = |i: usize| ids.get(i % ids.len().max(1)).copied();
        match op {
            UnitOp::Spawn { owner, tile } => {
                let id = uid(next);
                let res =
                    units.spawn(Unit::new(id, BaseUnitId(0), PlayerId(owner), TileIdx(tile), 1));
                if tile < TILES {
                    let placed = Change::UnitPlaced {
                        u: id,
                        owner: PlayerId(owner),
                        from: None,
                        to: TileIdx(tile),
                    };
                    prop_assert_eq!(res, Ok(placed));
                    model.insert(
                        id,
                        Mu { owner: PlayerId(owner), tile: TileIdx(tile), carried_by: None },
                    );
                    next += 1;
                } else {
                    prop_assert_eq!(res, Err(UnitsError::OffMap(TileIdx(tile))));
                }
            }
            UnitOp::Move { pick: i, tile } => {
                let Some(u) = pick(i) else { continue };
                let res = units.relocate(u, TileIdx(tile));
                if tile >= TILES {
                    prop_assert!(res.is_err());
                    continue;
                }
                let cargo: Vec<UnitId> = model
                    .iter()
                    .filter(|(_, m)| m.carried_by == Some(u))
                    .map(|(&id, _)| id)
                    .collect();
                let changes = res.map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert_eq!(changes.len(), 1 + cargo.len());
                let m = model.get_mut(&u).expect("picked from the model");
                if m.tile != TileIdx(tile) {
                    m.carried_by = None;
                }
                m.tile = TileIdx(tile);
                for c in cargo {
                    if let Some(x) = model.get_mut(&c) {
                        x.tile = TileIdx(tile);
                    }
                }
            }
            UnitOp::Board { pick: i, carrier } => {
                let (Some(u), Some(c)) = (pick(i), pick(carrier)) else { continue };
                let ok = may_board(&model, u, c);
                prop_assert_eq!(units.board(u, c).is_ok(), ok, "board {:?} onto {:?}", u, c);
                if ok && let Some(m) = model.get_mut(&u) {
                    m.carried_by = Some(c);
                }
            }
            UnitOp::Unboard { pick: i } => {
                let Some(u) = pick(i) else { continue };
                prop_assert!(units.unboard(u).is_ok());
                if let Some(m) = model.get_mut(&u) {
                    m.carried_by = None;
                }
            }
            UnitOp::Despawn { pick: i } => {
                let Some(u) = pick(i) else { continue };
                let m = model.remove(&u).expect("picked from the model");
                let (gone, ch) =
                    units.despawn(u).map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert_eq!(gone.id(), u);
                // The removal, then each unit it carried leaving it, in id order.
                let mut want = vec![Change::UnitRemoved { u, owner: m.owner, at: m.tile }];
                for (&c, x) in &mut model {
                    if x.carried_by == Some(u) {
                        x.carried_by = None;
                        want.push(Change::UnitPlaced {
                            u: c,
                            owner: x.owner,
                            from: Some(x.tile),
                            to: x.tile,
                        });
                    }
                }
                prop_assert_eq!(ch.as_slice(), want.as_slice());
                prop_assert!(units.despawn(u).is_err());
            }
            UnitOp::Owner { pick: i, owner } => {
                let Some(u) = pick(i) else { continue };
                let old = model.get(&u).map(|m| m.owner).expect("picked from the model");
                let ch = units
                    .set_owner(u, PlayerId(owner))
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert_eq!(ch, Change::UnitOwner { u, old, new: PlayerId(owner) });
                if let Some(m) = model.get_mut(&u) {
                    m.owner = PlayerId(owner);
                }
            }
        }
        check_units(&units, &model)?;
    }
    // The same units, loaded fresh, index the same way.
    let fresh = Units::from_units(units.iter().cloned(), TILES)
        .map_err(|e| TestCaseError::fail(e.to_string()))?;
    prop_assert_eq!(&fresh, &units);
    check_units(&fresh, &model)?;
    Ok(())
}

// ---- Gate 2: pairs of players -----------------------------------------------------------------

#[test]
fn pair_matrix_indexes_every_pair_once_for_up_to_64_players() {
    for n in 1u8..=64 {
        let cells = PairMatrix::<u8>::cells_for(n);
        assert_eq!(cells, usize::from(n) * usize::from(n - 1) / 2);
        let mut seen = vec![false; cells];
        for hi in 0..n {
            for lo in 0..hi {
                let (a, b) = (PlayerId(lo), PlayerId(hi));
                let i = PairMatrix::<u8>::index(n, a, b).expect("a pair of players");
                assert_eq!(PairMatrix::<u8>::index(n, b, a), Some(i), "either order");
                assert!(i < cells, "n {n}: ({lo}, {hi}) at {i} of {cells}");
                assert!(!seen[i], "n {n}: ({lo}, {hi}) shares cell {i}");
                seen[i] = true;
            }
            assert_eq!(PairMatrix::<u8>::index(n, PlayerId(hi), PlayerId(hi)), None);
            assert_eq!(PairMatrix::<u8>::index(n, PlayerId(hi), PlayerId(n)), None);
        }
        assert!(seen.iter().all(|&s| s), "n {n}: every cell is used");
        // Cells come in index order from pairs(), and a matrix sized for n holds exactly them.
        let m =
            PairMatrix::from_cells(n, (0..cells).collect::<Vec<usize>>()).expect("the right size");
        for (lo, hi, &cell) in m.pairs() {
            assert_eq!(PairMatrix::<usize>::index(n, lo, hi), Some(cell));
        }
        assert_eq!(m.pairs().count(), cells);
        assert!(PairMatrix::from_cells(n, vec![0u8; cells + 1]).is_none());
    }
}

#[derive(Clone, Debug)]
enum DiploOp {
    War(u8, u8, bool),
    Meet(u8, u8),
    Unmeet(u8, u8),
    Treaty(u8, u8, i32),
}

fn diplo_op(n: u8) -> impl Strategy<Value = DiploOp> {
    let p = 0..n + 1;
    prop_oneof![
        (p.clone(), p.clone(), any::<bool>()).prop_map(|(a, b, w)| DiploOp::War(a, b, w)),
        (p.clone(), p.clone()).prop_map(|(a, b)| DiploOp::Meet(a, b)),
        (p.clone(), p.clone()).prop_map(|(a, b)| DiploOp::Unmeet(a, b)),
        (p.clone(), p, 0i32..50).prop_map(|(a, b, t)| DiploOp::Treaty(a, b, t)),
    ]
}

fn diplo_case() -> impl Strategy<Value = (u8, u64, Vec<DiploOp>)> {
    (2u8..=64)
        .prop_flat_map(|n| (Just(n), any::<u64>(), proptest::collection::vec(diplo_op(n), 1..120)))
}

proptest! {
    #[test]
    fn war_and_contact_masks_follow_the_relations((n, barb_bits, ops) in diplo_case()) {
        masks_case(n, barb_bits, ops)?;
    }
}

fn masks_case(n: u8, barb_bits: u64, ops: Vec<DiploOp>) -> Result<(), TestCaseError> {
    // About one player in four is a barbarian, so some pairs are at war whatever the relation.
    let barbarians =
        PlayerSet::from_bits(barb_bits & (barb_bits >> 7) & PlayerSet::first(n).bits());
    let mut d = Diplomacy::new(n, barbarians);
    let mut war: BTreeSet<(u8, u8)> = BTreeSet::new();
    let mut met: BTreeSet<(u8, u8)> = BTreeSet::new();
    for op in ops {
        let (a, b) = match op {
            DiploOp::War(a, b, _)
            | DiploOp::Meet(a, b)
            | DiploOp::Unmeet(a, b)
            | DiploOp::Treaty(a, b, _) => (a, b),
        };
        let key = (a.min(b), a.max(b));
        let res = match op {
            DiploOp::War(..) => d.update(PlayerId(a), PlayerId(b), |r| {
                r.war = matches!(op, DiploOp::War(_, _, true))
            }),
            DiploOp::Meet(..) => d.meet(PlayerId(a), PlayerId(b)),
            DiploOp::Unmeet(..) => d.update(PlayerId(a), PlayerId(b), |r| r.met = false),
            DiploOp::Treaty(_, _, t) => d.update(PlayerId(a), PlayerId(b), |r| r.treaty_until = t),
        };
        if a == b || a >= n || b >= n {
            prop_assert!(res.is_err());
            continue;
        }
        let changes = res.map_err(|e| TestCaseError::fail(e.to_string()))?;
        let was_met = met.contains(&key);
        match op {
            DiploOp::War(_, _, true) => {
                war.insert(key);
            }
            DiploOp::War(_, _, false) => {
                war.remove(&key);
            }
            DiploOp::Meet(..) => {
                met.insert(key);
            }
            DiploOp::Unmeet(..) => {
                met.remove(&key);
            }
            DiploOp::Treaty(..) => {}
        }
        let met_changed = was_met != met.contains(&key);
        prop_assert_eq!(
            changes.as_slice().contains(&Change::Met { a: PlayerId(a), b: PlayerId(b) }),
            met_changed
        );
        prop_assert_eq!(d.verify(), Ok(()));
        for x in 0..n {
            for y in 0..n {
                let k = (x.min(y), x.max(y));
                let barb = barbarians.contains(PlayerId(x)) || barbarians.contains(PlayerId(y));
                let want_war = x != y && (war.contains(&k) || barb);
                let want_met = x == y || met.contains(&k);
                prop_assert_eq!(d.at_war(PlayerId(x), PlayerId(y)), want_war, "war {} {}", x, y);
                prop_assert_eq!(d.has_met(PlayerId(x), PlayerId(y)), want_met, "met {} {}", x, y);
                prop_assert_eq!(d.war_mask(PlayerId(x)).contains(PlayerId(y)), want_war);
                prop_assert_eq!(d.met_mask(PlayerId(x)).contains(PlayerId(y)), x != y && want_met);
            }
        }
    }
    // Rebuilt from the relations alone, the masks come out the same.
    let fresh = Diplomacy::from_relations(d.relations().clone(), barbarians);
    prop_assert_eq!(fresh, d);
    Ok(())
}

// ---- Gate 3: seats as Phase 0 derived them ----------------------------------------------------

const ALL_ON: AutoDecisions = AutoDecisions { un_vote: true, conquest: true, free_picks: true };
const ALL_OFF: AutoDecisions = AutoDecisions { un_vote: false, conquest: false, free_picks: false };

fn overrides(handicap: Option<Value>, auto: Option<Value>) -> Result<SeatOverrides, String> {
    SeatOverrides::parse(handicap.as_ref(), auto.as_ref()).map_err(|e| e.0)
}

/// `DefaultTests.test_defaults_follow_the_controller`.
#[test]
fn seat_defaults_follow_the_controller() {
    for c in [Controller::Human, Controller::Llm, Controller::Mcp] {
        let s = Seat::new(c, SeatOverrides::default(), None);
        assert_eq!((s.handicap(), s.auto()), (Handicap::Human, ALL_OFF), "{c:?}");
    }
    for c in [Controller::Bot, Controller::Hybrid, Controller::Minor, Controller::Barbarian] {
        let s = Seat::new(c, SeatOverrides::default(), None);
        assert_eq!((s.handicap(), s.auto()), (Handicap::Ai, ALL_ON), "{c:?}");
    }
    for c in Controller::ALL {
        assert_eq!(Controller::from_name(c.name()), Some(c));
    }
}

/// `DefaultTests.test_overrides_are_kept_and_checked`.
#[test]
fn seat_overrides_are_kept_and_checked() -> Result<(), String> {
    let a = Seat::new(
        Controller::Hybrid,
        overrides(Some(json!("human")), Some(json!({"un_vote": false, "conquest": false})))?,
        None,
    );
    let b = Seat::new(Controller::Llm, overrides(None, Some(json!({"free_picks": true})))?, None);
    let mixed = AutoDecisions { un_vote: false, conquest: false, free_picks: true };
    assert_eq!((a.handicap(), a.auto()), (Handicap::Human, mixed));
    assert_eq!((b.handicap(), b.auto()), (Handicap::Human, mixed));
    for (h, auto) in [
        (Some(json!("deity")), None),
        (None, Some(json!({"trades": true}))),
        (None, Some(json!({"un_vote": "false"}))),
        (None, Some(json!({"conquest": 0}))),
    ] {
        assert!(overrides(h.clone(), auto.clone()).is_err(), "{h:?} {auto:?}");
    }
    Ok(())
}

/// `DefaultTests.test_auto_values_must_be_true_or_false`, and the messages Python gave.
#[test]
fn seat_auto_values_must_be_true_or_false() -> Result<(), String> {
    let only = overrides(None, Some(json!({"un_vote": false})))?;
    assert_eq!(
        only,
        SeatOverrides {
            handicap: None,
            auto: AutoOverrides { un_vote: Some(false), ..AutoOverrides::default() }
        }
    );
    let err = overrides(None, Some(json!({"un_vote": "false", "conquest": true})))
        .expect_err("a string is not a bool");
    assert!(err.contains("un_vote") && !err.contains("conquest"), "{err}");
    assert_eq!(err, "auto values must be true or false (un_vote is not).");
    assert_eq!(
        overrides(Some(json!("deity")), None).expect_err("not a handicap"),
        "handicap must be 'human' or 'ai', not 'deity'."
    );
    assert_eq!(
        overrides(None, Some(json!(["un_vote"]))).expect_err("not an object"),
        "auto must be an object whose keys are among un_vote, conquest, free_picks, e.g. {\"un_vote\": false}."
    );
    // Empty and absent settings leave everything to the controller, as Python's truthiness did.
    for (h, auto) in [
        (Some(json!("")), Some(json!({}))),
        (Some(Value::Null), Some(json!(0))),
        (None, Some(json!(false))),
    ] {
        assert_eq!(overrides(h, auto)?, SeatOverrides::default());
    }
    Ok(())
}

/// `DefaultTests.test_a_new_controller_rederives_what_was_not_set_explicitly`, through `State`,
/// and `SaveTests.test_saves_from_before_the_split_derive_them_from_the_controller` through
/// `Seat::restore`.
#[test]
fn a_new_controller_rederives_what_was_not_set_explicitly() -> Result<(), String> {
    let mut st = small_state()?;
    let p = PlayerId(0);
    let ov = overrides(None, Some(json!({"un_vote": false})))?;
    st.set_controller(p, Controller::Bot, None, ov.auto).map_err(|e| e.to_string())?.assert_seat(p);
    let seat = |st: &citar_engine::state::State| {
        st.player(p).map(|x| (x.seat().handicap(), x.seat().auto()))
    };
    assert_eq!(seat(&st), Some((Handicap::Ai, AutoDecisions { un_vote: false, ..ALL_ON })));
    st.set_controller(p, Controller::Human, None, AutoOverrides::default())
        .map_err(|e| e.to_string())?
        .assert_seat(p);
    assert_eq!(seat(&st), Some((Handicap::Human, AutoDecisions { un_vote: false, ..ALL_OFF })));
    st.set_controller(p, Controller::Bot, Some(Handicap::Human), AutoOverrides::default())
        .map_err(|e| e.to_string())?
        .assert_seat(p);
    assert_eq!(seat(&st), Some((Handicap::Human, AutoDecisions { un_vote: false, ..ALL_ON })));
    // A change during play is not an override: the next controller re-derives it.
    st.set_auto_decision(p, citar_engine::state::players::AutoDecision::Conquest, false)
        .map_err(|e| e.to_string())?
        .assert_seat(p);
    assert_eq!(
        seat(&st),
        Some((
            Handicap::Human,
            AutoDecisions { un_vote: false, conquest: false, free_picks: true }
        ))
    );
    st.set_controller(p, Controller::Bot, None, AutoOverrides::default())
        .map_err(|e| e.to_string())?
        .assert_seat(p);
    assert_eq!(seat(&st), Some((Handicap::Human, AutoDecisions { un_vote: false, ..ALL_ON })));

    // A save made before handicap and auto existed derives both from the controller.
    for (c, want) in
        [(Controller::Llm, (Handicap::Human, ALL_OFF)), (Controller::Bot, (Handicap::Ai, ALL_ON))]
    {
        let s =
            Seat::restore(c, None, AutoOverrides::default(), SeatOverrides::default(), None, None);
        assert_eq!((s.handicap(), s.auto()), want);
    }
    // A newer save keeps what it had, even a decision changed during play.
    let saved_auto = AutoOverrides { conquest: Some(false), ..AutoOverrides::default() };
    let s = Seat::restore(
        Controller::Bot,
        Some(Handicap::Human),
        saved_auto,
        SeatOverrides::default(),
        None,
        None,
    );
    assert_eq!(
        (s.handicap(), s.auto()),
        (Handicap::Human, AutoDecisions { conquest: false, ..ALL_ON })
    );
    Ok(())
}

trait AssertSeat {
    fn assert_seat(self, p: PlayerId);
}

impl AssertSeat for Change {
    fn assert_seat(self, p: PlayerId) {
        assert_eq!(self, Change::Seat(p));
    }
}

/// An 8x8 state with two majors, for the seat tests.
fn small_state() -> Result<citar_engine::state::State, String> {
    use citar_engine::base::ids::{
        BarbarianLevelId, DifficultyId, EraId, MapSizeId, MapTypeId, NationId, SpeedId, TerrainId,
    };
    use citar_engine::base::sets::PlayerVec;
    use citar_engine::state::config::{GameConfig, MapEdges, MapSource};
    use citar_engine::state::map::{MapInfo, Tiles};
    use citar_engine::state::players::{Player, PlayerKind, Rgb};
    let map = MapInfo { width: 8, height: 8, wrap_x: false, wrap_y: false, continents: Vec::new() };
    let tiles = Tiles::new(vec![Tile::new(TerrainId(0)); 64]);
    let src = MapSource::Generated {
        size: MapSizeId(0),
        map_type: MapTypeId(0),
        edges: MapEdges::IceCaps,
        dims: None,
    };
    let cfg =
        GameConfig::new(9, src, SpeedId(0), DifficultyId(0), EraId(0), BarbarianLevelId(0), 330);
    let players: PlayerVec<Player> = (0..2u8)
        .map(|i| {
            let seat = Seat::new(Controller::Human, SeatOverrides::default(), None);
            Player::new(
                PlayerId(i),
                PlayerKind::Major,
                format!("P{i}").into(),
                NationId(u16::from(i)),
                Rgb::default(),
                seat,
                64,
            )
        })
        .collect();
    citar_engine::state::State::new(cfg, map, tiles, players).map_err(|e| e.to_string())
}

// ---- Gate 4: deal items as Python's dicts -----------------------------------------------------

const DEAL_ITEMS: &str = include_str!("../../data/deal_items.json");

fn rules() -> &'static Ruleset {
    Ruleset::shared()
}

#[test]
fn deal_items_read_and_write_as_python_dicts() {
    let doc: Value = serde_json::from_str(DEAL_ITEMS).expect("deal_items.json is JSON");
    let rows = doc["rows"].as_array().expect("rows");
    let mut kinds = BTreeSet::new();
    for (i, row) in rows.iter().enumerate() {
        let py = &row["item"];
        let item = DealItem::from_json(py, rules()).unwrap_or_else(|e| panic!("row {i}: {e}"));
        kinds.insert(item.kind());
        let back = item.to_json(rules()).expect("every id is in the ruleset");
        assert_eq!(&back, py, "row {i}: the dict, as Python compares dicts");
        if i < DealItemKind::ALL.len() {
            // The examples of ITEM_TYPES: the same keys in the same order, byte for byte.
            assert_eq!(
                serde_json::to_string(&back).ok(),
                serde_json::to_string(py).ok(),
                "row {i}"
            );
            assert_eq!(item.kind(), DealItemKind::ALL[i], "ITEM_TYPES order");
        }
    }
    assert_eq!(kinds.len(), 13, "every kind is recorded");
}

#[test]
fn deal_items_refuse_what_python_never_wrote() {
    for bad in [
        json!({"type": "gold"}),
        json!({"type": "gold", "amount": 1.5}),
        json!({"type": "gold", "amount": 5, "turns": 3}),
        json!({"type": "tribute", "amount": 5}),
        json!({"type": "resource", "resource": "iron", "amount": 1, "turns": 30}),
        json!({"type": "tech", "tech": "Warp Drive"}),
        json!({"type": "city", "city_id": 0}),
        json!({"type": "declare_war", "target": 300}),
        json!(["gold", 5]),
    ] {
        assert!(DealItem::from_json(&bad, rules()).is_err(), "{bad}");
    }
}

#[test]
fn terms_read_and_write_as_python_proposals() {
    let py = json!({
        "3": [{"type": "gold", "amount": 50}, {"type": "peace_treaty"}],
        "1": [{"type": "peace_treaty"}],
    });
    let terms = Terms::from_json(&py, rules()).expect("a proposal");
    assert_eq!(terms.sides[0].giver, PlayerId(3), "the proposer's side first, as Python kept it");
    assert_eq!(terms.gives(PlayerId(1)), &[DealItem::PeaceTreaty]);
    assert!(terms.has(DealItemKind::Gold) && !terms.has(DealItemKind::Tech));
    assert_eq!(
        serde_json::to_string(&terms.to_json(rules())).ok(),
        serde_json::to_string(&Some(py)).ok()
    );
    assert!(Terms::from_json(&json!({"1": [], "1 ": []}), rules()).is_err());
    assert!(Terms::from_json(&json!({"1": []}), rules()).is_err());
    let city = DealItem::City { city_id: CityId::new(12).expect("12") };
    assert_eq!(city.to_json(rules()), Some(json!({"type": "city", "city_id": 12})));
}

// ---- Gate 5: sizes ----------------------------------------------------------------------------

#[test]
fn a_tile_is_16_bytes_and_a_memory_8() {
    assert_eq!(size_of::<Tile>(), 16);
    assert_eq!(size_of::<TileMemory>(), 8);
    // Four tiles to a cache line.
    assert_eq!(align_of::<Tile>(), 4);
    let t = Tile::new(citar_engine::base::ids::TerrainId(1)).with_claim(TileClaim::NONE);
    assert_eq!(Tile::from_canon_bytes(t.canon_bytes()), t);
}

// ---- Gate 6: events against the Python engine's, as recorded ---------------------------------

/// What the Python engine emitted and recorded (`data/python_events.json`): read from its
/// sources by the scanner this test once had, before package 2-12 deleted them.
struct PythonEvents {
    /// Every event type an `emit` named.
    emitted: BTreeSet<String>,
    /// The keyword arguments `emit` was passed, but `idx`, `mentions` and `players`.
    emit_keys: BTreeSet<String>,
    /// `PRIVATE_EVENTS` (`game.py:808-812`).
    private: BTreeSet<String>,
    /// The keys of `victory.record_stats`' rows.
    record_stats_keys: Vec<String>,
    /// `scripts/refcheck/baseline.py`'s `STAT_KEYS`.
    baseline_stat_keys: Vec<String>,
}

fn python_events() -> PythonEvents {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/python_events.json");
    #[allow(clippy::disallowed_methods, reason = "reads the recording the test checks against")]
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let v: Value = serde_json::from_str(&text).expect("the recording is JSON");
    let list = |key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} is a list"))
            .iter()
            .map(|s| s.as_str().unwrap_or_else(|| panic!("{key} holds strings")).to_owned())
            .collect()
    };
    PythonEvents {
        emitted: list("emitted").into_iter().collect(),
        emit_keys: list("emit_keys").into_iter().collect(),
        private: list("private").into_iter().collect(),
        record_stats_keys: list("record_stats_keys"),
        baseline_stat_keys: list("baseline_stat_keys"),
    }
}

#[test]
fn engine_events_cover_every_type_the_python_engine_emitted() {
    let py = python_events();
    let ours: BTreeSet<String> = EngineEvent::ALL.iter().map(|e| e.name().to_owned()).collect();
    let missing: Vec<&String> = py.emitted.difference(&ours).collect();
    let extra: Vec<&String> = ours.difference(&py.emitted).collect();
    assert!(missing.is_empty() && extra.is_empty(), "missing {missing:?}, never emitted {extra:?}");
    assert_eq!(py.emitted.len(), 97);

    for &e in EngineEvent::ALL {
        assert_eq!(e.is_private(), py.private.contains(e.name()), "{}", e.name());
    }
    // Two private types that nothing emitted any more.
    let dead: Vec<&String> = py.private.difference(&py.emitted).collect();
    assert_eq!(dead, ["build_cancelled", "city_razing"]);

    let data: BTreeSet<String> = EventData::KEYS.iter().map(|k| (*k).to_owned()).collect();
    assert_eq!(py.emit_keys, data, "EventData has a field for exactly the keys emit was passed");
}

#[test]
fn stats_rows_carry_every_key_python_recorded() {
    let py = python_events();
    let ours: BTreeSet<&str> = CivStats::KEYS.iter().copied().collect();
    assert!(py.record_stats_keys.len() >= 17, "record_stats' keys");
    for key in &py.record_stats_keys {
        assert!(ours.contains(key.as_str()), "record_stats key {key}");
    }
    assert!(py.baseline_stat_keys.len() >= 8, "baseline.py's keys");
    for key in &py.baseline_stat_keys {
        assert!(ours.contains(key.as_str()), "baseline key {key}");
    }
}

#[test]
fn unit_ids_index_from_one() {
    // The occupancy lists use raw id 0 as "none": no unit may have it.
    assert_eq!(UnitId::FIRST_INDEX, 1);
    assert_eq!(UnitId::FIRST.index(), 1);
}

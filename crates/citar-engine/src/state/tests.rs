//! Unit tests of the operations that span `State`'s containers.

use super::cities::City;
use super::config::{MapEdges, MapSource};
use super::players::{AutoOverrides, Controller, PlayerKind, Rgb, Seat, SeatOverrides};
use super::units::Unit;
use super::*;
use crate::base::ids::{
    BarbarianLevelId, BaseUnitId, DifficultyId, EraId, MapSizeId, MapTypeId, NationId, SpeedId,
    TerrainId,
};
use crate::state::map::Tile;

const W: u16 = 8;
const H: u16 = 8;

fn player(id: u8, kind: PlayerKind) -> Player {
    let controller = match kind {
        PlayerKind::Major => Controller::Bot,
        PlayerKind::CityState => Controller::Minor,
        PlayerKind::Barbarian => Controller::Barbarian,
    };
    let seat = Seat::new(controller, SeatOverrides::default(), None);
    Player::new(
        PlayerId(id),
        kind,
        format!("P{id}").into(),
        NationId(u16::from(id)),
        Rgb::default(),
        seat,
        u32::from(W) * u32::from(H),
    )
}

fn state() -> State {
    let map = MapInfo { width: W, height: H, wrap_x: false, wrap_y: false, continents: Vec::new() };
    let tiles = Tiles::new(vec![Tile::new(TerrainId(0)); usize::from(W) * usize::from(H)]);
    let src = MapSource::Generated {
        size: MapSizeId(0),
        map_type: MapTypeId(0),
        edges: MapEdges::IceCaps,
        dims: None,
    };
    let cfg =
        GameConfig::new(1, src, SpeedId(0), DifficultyId(0), EraId(0), BarbarianLevelId(1), 500);
    let players: PlayerVec<Player> = [
        player(0, PlayerKind::Major),
        player(1, PlayerKind::Major),
        player(2, PlayerKind::CityState),
        player(3, PlayerKind::Barbarian),
    ]
    .into_iter()
    .collect();
    State::new(cfg, map, tiles, players).expect("a valid state")
}

fn cid(n: u32) -> CityId {
    CityId::new(n).unwrap_or(CityId::FIRST)
}

fn uid(n: u32) -> UnitId {
    UnitId::new(n).unwrap_or(UnitId::FIRST)
}

#[test]
fn a_new_state_is_consistent_and_barbarians_are_at_war() -> Result<(), StateError> {
    let st = state();
    st.check_indexes()?;
    assert_eq!(st.barbarians(), PlayerSet::single(PlayerId(3)));
    assert!(st.diplo().at_war(PlayerId(0), PlayerId(3)));
    assert!(!st.diplo().at_war(PlayerId(0), PlayerId(1)));
    assert_eq!(st.clock().turn, 1);
    assert_eq!(st.seed(), 1);
    Ok(())
}

#[test]
fn a_state_whose_parts_do_not_fit_is_refused() {
    let st = state();
    let mut parts = st.clone().into_parts();
    parts.tiles = Tiles::new(vec![Tile::new(TerrainId(0)); 5]);
    assert!(matches!(State::from_parts(parts), Err(StateError::Mismatch(_))));
    let mut parts = st.clone().into_parts();
    parts.diplo = Diplomacy::new(2, PlayerSet::EMPTY);
    assert!(matches!(State::from_parts(parts), Err(StateError::Mismatch(_))));
    let mut parts = st.into_parts();
    parts.map.width = 3;
    assert!(matches!(State::from_parts(parts), Err(StateError::Grid(_))));
}

#[test]
fn city_at_reads_the_tile_and_checks_the_city() -> Result<(), StateError> {
    let mut st = state();
    let t = TileIdx(9);
    let _found = st.cities.found(City::new(cid(1), "Rome".into(), PlayerId(0), t, 1))?;
    let _claim = st.tiles.set_owner(t, TileClaim::city(PlayerId(0), cid(1)))?;
    let _claim = st.tiles.set_owner(TileIdx(10), TileClaim::city(PlayerId(0), cid(1)))?;
    assert_eq!(st.city_at(t), Some(cid(1)));
    assert_eq!(st.city_at(TileIdx(10)), None, "owned by the city, but not its tile");
    assert_eq!(st.city_at(TileIdx(99)), None);
    Ok(())
}

#[test]
fn transfer_city_moves_its_tiles_and_drops_the_capital() -> Result<(), StateError> {
    let mut st = state();
    let _found = st.cities.found(City::new(cid(1), "Rome".into(), PlayerId(0), TileIdx(9), 1))?;
    for t in [9, 10, 17] {
        let _claim = st.tiles.set_owner(TileIdx(t), TileClaim::city(PlayerId(0), cid(1)))?;
    }
    st.players[PlayerId(0)].capital = Some(cid(1));
    let changes = st.transfer_city(cid(1), PlayerId(1))?;
    let expected_first = Change::CityOwner { c: cid(1), old: PlayerId(0), new: PlayerId(1) };
    assert_eq!(changes.as_slice().first(), Some(&expected_first));
    assert_eq!(changes.len(), 4);
    assert!([9, 10, 17].iter().all(|&t| st.tiles()[TileIdx(t)].owner() == Some(PlayerId(1))));
    assert_eq!(st.players[PlayerId(0)].capital, None);
    assert_eq!(st.cities().of(PlayerId(1)), &[cid(1)]);
    assert!(st.transfer_city(cid(1), PlayerId(1))?.is_empty(), "no change to the same owner");
    assert!(st.transfer_city(cid(1), PlayerId(9)).is_err());
    st.check_indexes()
}

#[test]
fn kill_and_revive() -> Result<(), StateError> {
    let mut st = state();
    let _placed = st.units.spawn(Unit::new(uid(1), BaseUnitId(0), PlayerId(1), TileIdx(3), 1))?;
    let _placed = st.units.spawn(Unit::new(uid(2), BaseUnitId(0), PlayerId(0), TileIdx(3), 1))?;
    let _placed = st.units.spawn(Unit::new(uid(3), BaseUnitId(0), PlayerId(1), TileIdx(5), 1))?;
    // Player 0's unit rides player 1's carrier, and is left behind uncarried.
    let _boarded = st.units.board(uid(2), uid(1))?;
    let changes = st.kill_player(PlayerId(1), 40)?;
    let (t3, t5) = (TileIdx(3), TileIdx(5));
    assert_eq!(
        changes.as_slice(),
        &[
            Change::UnitRemoved { u: uid(1), owner: PlayerId(1), at: t3 },
            Change::UnitPlaced { u: uid(2), owner: PlayerId(0), from: Some(t3), to: t3 },
            Change::UnitRemoved { u: uid(3), owner: PlayerId(1), at: t5 },
            Change::PlayerAlive(PlayerId(1)),
        ]
    );
    assert_eq!(st.units().get(uid(2)).and_then(Unit::carried_by), None);
    let p = st.player(PlayerId(1)).expect("player 1");
    assert!(!p.alive());
    assert_eq!(p.eliminated_turn(), Some(40));
    assert_eq!(st.units().ids(), [uid(2)]);
    assert_eq!(st.revive_player(PlayerId(1))?, Change::PlayerAlive(PlayerId(1)));
    assert!(st.player(PlayerId(1)).is_some_and(|p| p.alive() && p.eliminated_turn().is_none()));
    let _found = st.cities.found(City::new(cid(1), "Rome".into(), PlayerId(0), TileIdx(9), 1))?;
    assert_eq!(st.kill_player(PlayerId(0), 41), Err(StateError::HasCities(PlayerId(0))));
    st.check_indexes()
}

#[test]
fn seats_and_alliances_report_their_changes() -> Result<(), StateError> {
    let mut st = state();
    let p = PlayerId(0);
    assert_eq!(
        st.set_controller(p, Controller::Human, None, AutoOverrides::default())?,
        Change::Seat(p)
    );
    assert_eq!(st.player(p).map(|x| x.seat().controller()), Some(Controller::Human));
    assert_eq!(st.set_seat_difficulty(p, Some(DifficultyId(2)))?, Change::Seat(p));
    assert_eq!(st.player(p).and_then(|x| x.seat().difficulty()), Some(DifficultyId(2)));
    let cs = PlayerId(2);
    assert_eq!(st.set_ally(cs, Some(p))?, Change::Alliance { cs, old: None, new: Some(p) });
    assert_eq!(st.set_ally(cs, None)?, Change::Alliance { cs, old: Some(p), new: None });
    assert_eq!(st.set_ally(p, None), Err(StateError::NotACityState(p)));
    assert_eq!(st.set_ally(cs, Some(PlayerId(9))), Err(StateError::NoSuchPlayer(PlayerId(9))));
    Ok(())
}

#[test]
fn rebuilding_the_indexes_is_idempotent() -> Result<(), StateError> {
    let mut st = state();
    let _placed = st.units.spawn(Unit::new(uid(1), BaseUnitId(0), PlayerId(1), TileIdx(3), 1))?;
    let _placed = st.units.spawn(Unit::new(uid(4), BaseUnitId(0), PlayerId(1), TileIdx(3), 1))?;
    let _boarded = st.units.board(uid(4), uid(1))?;
    let _met = st.diplo.meet(PlayerId(0), PlayerId(1))?;
    let before = st.clone();
    st.rebuild_indexes()?;
    assert_eq!(st, before);
    assert_eq!(st.units().carried_by(uid(1)).collect::<Vec<_>>(), [uid(4)]);
    assert!(st.diplo().has_met(PlayerId(1), PlayerId(0)));
    st.check_indexes()
}

#[test]
fn counters_hand_out_ids_from_one() {
    let mut ids = IdCounters::default();
    assert_eq!(ids.next_unit(), Some(UnitId::FIRST));
    assert_eq!(ids.next_unit().map(UnitId::get), Some(2));
    assert_eq!(ids.next_combat(), 0);
    assert_eq!(ids.next_combat(), 1);
    let mut conv = IdCounters::starting_at(57);
    assert_eq!(conv.next_city().map(CityId::get), Some(57));
    assert_eq!(conv.next_camp().map(CampId::get), Some(57));
    let mut full = IdCounters { unit: u32::MAX, ..IdCounters::default() };
    assert_eq!(
        full.next_unit(),
        None,
        "the last id is never handed out, so the counter never wraps"
    );
}

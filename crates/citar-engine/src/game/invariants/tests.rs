//! Gate 3 of package 1b-01: each corruption fires exactly its own code.

use super::{Code, check};
use crate::base::ids::{NegotiationId, PlayerId, TileIdx};
use crate::game::Game;
use crate::game::core::testing;
use crate::game::derive::rev::{CityTouch, DiploTouch, PlayerTouch};
use crate::state::diplo::{NegStatus, Negotiation};
use crate::state::{Phase, TileClaim, TurnClock};

const ROME: PlayerId = PlayerId(0);

/// A sound game: Rome with its capital on tile 22 and a warrior there.
fn sound() -> Game {
    let mut g = testing::duel();
    testing::city(&mut g, ROME, TileIdx(22), "Roma");
    testing::unit(&mut g, ROME, "Warrior", TileIdx(22));
    g.settle();
    assert_eq!(g.take_violations(), [], "the base game is sound");
    assert_eq!(check(&g), []);
    g
}

/// Drops the work a write raised, which a settle would do, so that only the corruption shows.
fn clear_pending(g: &mut Game) {
    g.pending.clear_recheck();
    g.pending.clear_sight();
}

/// The codes `check` finds after `corrupt`.
fn codes_after(corrupt: impl FnOnce(&mut Game)) -> Vec<Code> {
    let mut g = sound();
    corrupt(&mut g);
    check(&g).into_iter().map(|v| v.code).collect()
}

#[test]
fn id_1_an_id_at_its_counter() {
    assert_eq!(codes_after(|g| g.st.ids_mut().unit = 1), [Code::Id1]);
}

#[test]
fn occ_1_a_unit_missing_from_its_tile() {
    assert_eq!(
        codes_after(|g| {
            let u = g.st.units().ids()[0];
            g.corrupt_units().unlist_for_test(u);
        }),
        [Code::Occ1]
    );
}

#[test]
fn unit_1_a_unit_without_health() {
    assert_eq!(
        codes_after(|g| {
            let u = g.st.units().ids()[0];
            if let Some(x) = g.corrupt_units().get_mut(u) {
                x.hp = 0;
            }
        }),
        [Code::Unit1]
    );
}

#[test]
fn city_1_a_city_without_citizens() {
    assert_eq!(
        codes_after(|g| {
            let c = g.st.cities().ids()[0];
            if let Some(x) = g.corrupt_cities().get_mut(c) {
                x.pop = 0;
            }
        }),
        [Code::City1]
    );
}

#[test]
fn city_2_a_city_working_a_tile_not_its_own() {
    assert_eq!(
        codes_after(|g| {
            let c = g.st.cities().ids()[0];
            if let Some(x) = g.corrupt_cities().get_mut(c) {
                x.citizens_settled = true;
                x.worked = vec![TileIdx(50)];
            }
        }),
        [Code::City2]
    );
}

#[test]
fn city_2_two_cities_working_one_tile() {
    assert_eq!(
        codes_after(|g| {
            let rome = g.st.cities().ids()[0];
            let other = testing::city(g, ROME, TileIdx(24), "Antium");
            clear_pending(g);
            g.set_tile_owner(TileIdx(23), TileClaim::city(ROME, rome)).expect("a tile");
            clear_pending(g);
            for c in [rome, other] {
                if let Some(x) = g.corrupt_cities().get_mut(c) {
                    x.citizens_settled = true;
                    x.worked = vec![TileIdx(23)];
                }
            }
        }),
        [Code::City2]
    );
}

#[test]
fn tile_1_a_tile_of_a_city_owned_by_someone_else() {
    assert_eq!(
        codes_after(|g| {
            let c = g.st.cities().ids()[0];
            let claim = TileClaim { owner: Some(PlayerId(1)), city: Some(c) };
            g.set_tile_owner(TileIdx(23), claim).expect("a tile on the map");
            clear_pending(g);
        }),
        [Code::Tile1]
    );
}

#[test]
fn player_1_a_negative_stock() {
    assert_eq!(
        codes_after(|g| {
            if let Some(p) = g.player_mut(ROME, PlayerTouch::STOCKS) {
                p.econ.faith = -1.0;
            }
            clear_pending(g);
        }),
        [Code::Player1]
    );
}

#[test]
fn player_1_a_float_that_is_not_finite() {
    assert_eq!(
        codes_after(|g| {
            if let Some(p) = g.corrupt_players().get_mut(ROME) {
                p.econ.gold = f64::NAN;
            }
        }),
        [Code::Player1]
    );
}

#[test]
fn player_2_a_major_with_cities_and_no_capital() {
    assert_eq!(
        codes_after(|g| {
            if let Some(p) = g.player_mut(ROME, PlayerTouch::CAPITAL) {
                p.capital = None;
            }
            clear_pending(g);
        }),
        [Code::Player2]
    );
}

#[test]
fn diplo_1_a_pact_in_force_at_war() {
    assert_eq!(
        codes_after(|g| {
            g.update_relation(ROME, PlayerId(1), |r| {
                r.war = true;
                r.pact_until = 99;
            })
            .expect("a pair");
            clear_pending(g);
        }),
        [Code::Diplo1]
    );
}

#[test]
fn neg_1_an_open_negotiation_awaiting_nobody() {
    assert_eq!(
        codes_after(|g| {
            g.st.ids_mut().negotiation = 2;
            g.edit_diplo(DiploTouch::NEGOTIATIONS).negotiations.push(Negotiation {
                id: NegotiationId::FIRST,
                initiator: ROME,
                responder: PlayerId(1),
                turn: 1,
                status: NegStatus::Open,
                awaiting: None,
                proposal: None,
                proposal_by: None,
                history: Vec::new(),
                deal: None,
            });
        }),
        [Code::Neg1]
    );
}

#[test]
fn vis_1_a_tile_seen_but_not_explored() {
    assert_eq!(codes_after(|g| g.dv.vis.reveal_for_test(ROME, TileIdx(70))), [Code::Vis1]);
}

#[test]
fn turn_1_a_winner_in_a_game_that_goes_on() {
    assert_eq!(
        codes_after(|g| {
            let clock = TurnClock { winner: Some(ROME), ..*g.st.clock() };
            g.set_clock(clock);
            // A clock change raises no work, but settle points are where the checks run.
            assert!(g.pending.is_empty());
        }),
        [Code::Turn1]
    );
}

#[test]
fn turn_1_allows_a_game_over_without_a_winner() {
    // The turn limit with the Time victory off ends a game so (victory.py:363-365).
    let found = codes_after(|g| {
        let clock = TurnClock { phase: Phase::Over, ..*g.st.clock() };
        g.set_clock(clock);
    });
    assert_eq!(found, []);
}

#[test]
fn pend_1_work_left_at_a_settle_point() {
    assert_eq!(
        codes_after(|g| {
            let c = g.st.cities().ids()[0];
            g.city_mut(c, CityTouch::WORK);
        }),
        [Code::Pend1]
    );
}

#[test]
fn settle_1_citizens_that_never_settle() {
    let mut g = sound();
    let c = g.st.cities().ids()[0];
    g.city_mut(c, CityTouch::WORK);
    g.pending.stubborn = true;
    g.settle();
    let codes: Vec<Code> = g.take_violations().into_iter().map(|v| v.code).collect();
    assert_eq!(codes, [Code::Settle1]);
}

#[test]
fn cache_1_a_touch_that_forgot_its_flag() {
    let mut g = sound();
    g.set_debug_options(crate::game::DebugOptions::ALL);
    // The index is read, so it is verified at the current revision...
    let _names = g.dv.names(&g.st).entries().len();
    // ...then a name changes under a touch that says it changed the stocks.
    if let Some(p) = g.player_mut(ROME, PlayerTouch::STOCKS) {
        p.name = "Byzantium".into();
    }
    g.settle();
    let codes: Vec<Code> = g.take_violations().into_iter().map(|v| v.code).collect();
    assert_eq!(codes, [Code::Cache1]);
    // With the right flag the index follows.
    if let Some(p) = g.player_mut(ROME, PlayerTouch::NAME) {
        p.name = "Rome".into();
    }
    g.settle();
    assert_eq!(g.take_violations(), []);
}

#[test]
fn every_code_has_its_name() {
    let all = [
        Code::Id1,
        Code::Occ1,
        Code::Unit1,
        Code::City1,
        Code::City2,
        Code::Tile1,
        Code::Player1,
        Code::Player2,
        Code::Diplo1,
        Code::Neg1,
        Code::Vis1,
        Code::Turn1,
        Code::Pend1,
        Code::Settle1,
        Code::Cache1,
    ];
    let names: Vec<&str> = all.iter().map(|c| c.name()).collect();
    assert_eq!(
        names.join(" "),
        "ID-1 OCC-1 UNIT-1 CITY-1 CITY-2 TILE-1 PLAYER-1 PLAYER-2 DIPLO-1 NEG-1 VIS-1 TURN-1 PEND-1 SETTLE-1 CACHE-1"
    );
}

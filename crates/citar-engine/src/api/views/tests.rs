//! Gate 2 of package 1d-02: events as the views show them, against the dicts Python's
//! `events_for` and `_scrub_event` produced (`game.py:880-990`), worked through by hand:
//! possessives, the United Nations' tally, unmet city-states and coordinates, with the name
//! references in code points and a scrubbed player field as `null`.

use serde_json::{Value, json};

use super::events::{event_json, events_json};
use super::players::{py_tail, un_result};
use crate::base::ids::{EventId, PlayerId, TileIdx};
use crate::base::sets::PlayerSet;
use crate::game::Game;
use crate::game::core::testing;
use crate::game::derive::rev::PlayerTouch;
use crate::game::events::Mention;
use crate::rules::Ruleset;
use crate::state::chronicle::{Chronicle, EngineEvent, Event, EventData};
use crate::state::players::PlayerKind;
use crate::state::world::UnResult;

const ROME: PlayerId = PlayerId(0);
const GREECE: PlayerId = PlayerId(1);
const PERSIA: PlayerId = PlayerId(2);
const GENEVA: PlayerId = PlayerId(3);
const MONACO: PlayerId = PlayerId(4);

/// Three civilizations and two city-states, none of whom has met another.
fn game() -> Game {
    let st = testing::state_of(&[
        (PlayerKind::Major, "Rome", "Augustus Caesar"),
        (PlayerKind::Major, "Greece", "Alexander"),
        (PlayerKind::Major, "Persia", "Darius"),
        (PlayerKind::CityState, "Geneva", ""),
        (PlayerKind::CityState, "Monaco", ""),
        (PlayerKind::Barbarian, "Barbarians", ""),
    ]);
    Game::from_state(Ruleset::shared(), st, Chronicle::new()).expect("a sound state")
}

fn emit(
    g: &mut Game,
    kind: EngineEvent,
    text: &str,
    audience: Option<&[PlayerId]>,
    tile: Option<TileIdx>,
    data: EventData,
    mentions: &[Mention<'_>],
) -> Event {
    let audience = audience.map(|a| a.iter().copied().collect::<PlayerSet>());
    let id: EventId = g.emit(kind, text, audience, tile, data, mentions).expect("an event id");
    g.chronicle().events().iter().find(|e| e.id == id).cloned().expect("the event")
}

/// The event as `viewer` sees it in a view.
fn shown(g: &Game, ev: &Event, viewer: Option<PlayerId>) -> Value {
    let v = event_json(g, ev, &g.event_view(ev, viewer), viewer);
    assert_eq!(g.event_json(ev, viewer), v, "the host's form is the view's");
    v
}

#[test]
fn possessives_names_outside_ascii_and_coordinates() {
    let mut g = game();
    if let Some(p) = g.player_mut(GREECE, PlayerTouch::NAME) {
        p.name = "Aztecs".into();
    }
    testing::city(&mut g, GREECE, TileIdx(30), "Kraków");
    let data = EventData { killer: Some(GREECE), owner: Some(ROME), ..EventData::default() };
    let ev = emit(
        &mut g,
        EngineEvent::UnitKilled,
        "The Aztecs's Warrior was killed near Kraków at (3, -2).",
        None,
        Some(TileIdx(12)),
        data,
        &[],
    );
    let id = ev.id.get();
    // A spectator sees it as it happened; the name references count code points, as Python's
    // did ("Kraków" ends at 42, its bytes at 43).
    assert_eq!(
        shown(&g, &ev, None),
        json!({
            "id": id, "turn": 1, "type": "unit_killed",
            "text": "The Aztecs' Warrior was killed near Kraków at (3, -2).",
            "players": null, "idx": 12, "data": {"killer": 1, "owner": 0},
            "refs": [[4, 10, 1, "c"], [36, 42, 1, "t"]], "x": 2, "y": 1,
        })
    );
    // Rome has not met the Aztecs: "Aztecs'" becomes "Unknown Civilization's", the city and the
    // place are hidden, the tile and the references dropped, and the killer cleared to null.
    assert_eq!(
        shown(&g, &ev, Some(ROME)),
        json!({
            "id": id, "turn": 1, "type": "unit_killed",
            "text": "The Unknown Civilization's Warrior was killed near an unknown city at an \
                     unknown location.",
            "players": null, "idx": null, "data": {"killer": null, "owner": 0},
        })
    );
    // The Aztecs know themselves, but not Rome, whose unit it was: the text names nobody else,
    // yet the place goes and the owner is cleared.
    assert_eq!(
        shown(&g, &ev, Some(GREECE)),
        json!({
            "id": id, "turn": 1, "type": "unit_killed",
            "text": "The Aztecs' Warrior was killed near Kraków at an unknown location.",
            "players": null, "idx": null, "data": {"killer": 1, "owner": null},
        })
    );
    // Once they have met, Rome sees it whole.
    g.make_contact(ROME, GREECE);
    assert_eq!(shown(&g, &ev, Some(ROME)), shown(&g, &ev, None));
}

#[test]
fn unmet_city_states_and_capitals_at_a_sentence_start() {
    let mut g = game();
    testing::city(&mut g, GENEVA, TileIdx(44), "Geneva");
    g.make_contact(ROME, GREECE);
    let ev = emit(
        &mut g,
        EngineEvent::CsAlly,
        "Greece is now allied with Geneva. Geneva's gift arrives.",
        Some(&[ROME, GREECE]),
        None,
        EventData { player: Some(GENEVA), ..EventData::default() },
        &[],
    );
    // A city-state's name and its capital's are the same text, which the index reads as the
    // city-state; unmet, it is "Unknown City-State", with Python's possessive.
    assert_eq!(
        shown(&g, &ev, Some(ROME)),
        json!({
            "id": ev.id.get(), "turn": 1, "type": "cs_ally",
            "text": "Greece is now allied with Unknown City-State. Unknown City-State's gift \
                     arrives.",
            "players": [0, 1], "idx": null, "data": {"player": null},
        })
    );
    // An unknown city at the start of a sentence is capitalised.
    testing::city(&mut g, PERSIA, TileIdx(60), "Persepolis");
    let ev = emit(
        &mut g,
        EngineEvent::CityGrowth,
        "Persepolis grew. \"Persepolis\" is proud.",
        None,
        None,
        EventData::default(),
        &[],
    );
    assert_eq!(
        shown(&g, &ev, Some(ROME))["text"],
        json!("An unknown city grew. \"An unknown city\" is proud.")
    );
}

#[test]
fn the_un_tally_names_the_candidates_a_viewer_has_not_met_as_unknown() {
    let mut g = game();
    g.make_contact(ROME, MONACO);
    let results = UnResult {
        turn: 1,
        tally: vec![(GREECE, 5), (PERSIA, 3), (MONACO, 2), (GENEVA, 2), (ROME, 1)],
        votes_needed: 7,
        winner: Some(GREECE),
    };
    let ev = emit(
        &mut g,
        EngineEvent::UnVote,
        "United Nations vote: Greece was elected world leader.",
        None,
        None,
        EventData { results: Some(Box::new(results.clone())), ..EventData::default() },
        &[],
    );
    // Python numbered each unknown candidate as it went, and named a second of the same kind
    // with its number (`game.py:980-988`); the winner goes, as the viewer does not know it.
    let tally = json!({
        "Unknown Civilization": 5, "Unknown Civilization (2)": 3, "Monaco": 2,
        "Unknown City-State": 2, "Rome": 1,
    });
    let seen = shown(&g, &ev, Some(ROME));
    assert_eq!(
        seen["text"],
        json!("United Nations vote: Unknown Civilization was elected world leader.")
    );
    assert_eq!(
        seen["data"]["results"],
        json!({"turn": 1, "tally": tally, "votes_needed": 7, "winner": null})
    );
    let all = json!({"Greece": 5, "Persia": 3, "Monaco": 2, "Geneva": 2, "Rome": 1});
    assert_eq!(
        shown(&g, &ev, None)["data"]["results"],
        json!({"turn": 1, "tally": all, "votes_needed": 7, "winner": 1})
    );
    // The diplomacy panel's last result reads the same way.
    assert_eq!(un_result(&g, Some(ROME), &results)["tally"], tally);
    assert_eq!(un_result(&g, None, &results)["tally"], all);
}

#[test]
fn a_view_keeps_the_last_events_as_python_s_slice_did() {
    let mut g = game();
    for i in 0..5 {
        let text = format!("Event {i}.");
        emit(&mut g, EngineEvent::Era, &text, None, None, EventData::default(), &[]);
    }
    let texts = |g: &Game, limit: i64| -> Vec<String> {
        events_json(g, Some(ROME), limit)
            .iter()
            .map(|e| e["text"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    assert_eq!(texts(&g, 2), ["Event 3.", "Event 4."]);
    // `[-0:]` is the whole list, and `[--2:]` all but the first two.
    assert_eq!(texts(&g, 0).len(), 5);
    assert_eq!(texts(&g, -2), ["Event 2.", "Event 3.", "Event 4."]);
    assert_eq!(texts(&g, 99).len(), 5);
    assert_eq!((py_tail(10, 3), py_tail(10, 0), py_tail(10, -3), py_tail(2, -5)), (7, 0, 3, 2));
    // A private event reaches only its audience.
    emit(&mut g, EngineEvent::Spy, "A secret.", Some(&[GREECE]), None, EventData::default(), &[]);
    assert_eq!(texts(&g, 1), ["Event 4."]);
    assert_eq!(events_json(&g, Some(GREECE), 1)[0]["text"], json!("A secret."));
}

#[test]
fn a_sacked_city_names_no_building_as_null() {
    let mut g = game();
    let ev = emit(
        &mut g,
        EngineEvent::CitySacked,
        "Barbarians sacked Rome.",
        Some(&[ROME]),
        None,
        EventData { gold: Some(20), citizen_killed: Some(false), ..EventData::default() },
        &[],
    );
    assert_eq!(
        shown(&g, &ev, None)["data"],
        json!({"gold": 20, "citizen_killed": false, "building": null})
    );
}

#[test]
fn views_write_floats_as_python_did() {
    let v = serde_json::json!([
        2.0,
        1e16,
        5e-5,
        0.1,
        -0.0,
        3,
        12.35,
        123_456_789_012_345.0,
        1e15,
        2.5e-7
    ]);
    let text = String::from_utf8(super::to_py_json(&v)).expect("UTF-8");
    assert_eq!(
        text,
        "[2.0,1e+16,5e-05,0.1,-0.0,3,12.35,123456789012345.0,1000000000000000.0,2.5e-07]"
    );
    // A number whose kind Python kept: an int only where Python's inputs were whole, and only a
    // whole value within a float's exact integers.
    let kinds = [
        super::PyNum::int_if(true, 4.0),
        super::PyNum::int_if(false, 4.0),
        super::PyNum::int_if(true, -0.0),
        super::PyNum::int_if(true, 2.5),
        super::PyNum::int_if(true, 1e16),
    ];
    let text = String::from_utf8(super::to_py_json(&kinds)).expect("UTF-8");
    assert_eq!(text, "[4,4.0,0,2.5,1e+16]");
    let values = [4.0_f64, 4.0, 0.0, 2.5, 1e16];
    assert!(kinds.iter().zip(values).all(|(k, x)| k.as_f64().to_bits() == x.to_bits()));
}

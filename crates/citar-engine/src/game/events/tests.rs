//! Gate 4 of package 1b-01: emitting and scrubbing, against the texts Python's `emit` and
//! `_scrub_event` produce (`game.py:814-990`), worked through by hand.

use super::{EventOut, Mention, UNKNOWN_CIV};
use crate::base::ids::{EventId, PlayerId, TileIdx};
use crate::base::sets::PlayerSet;
use crate::game::Game;
use crate::game::core::testing;
use crate::game::derive::rev::PlayerTouch;
use crate::state::chronicle::{EngineEvent, Event, EventData, NameRef, RefKind};
use crate::state::world::UnResult;

const ROME: PlayerId = PlayerId(0);
const GREECE: PlayerId = PlayerId(1);
const GENEVA: PlayerId = PlayerId(2);

fn set(ps: &[PlayerId]) -> PlayerSet {
    ps.iter().copied().collect()
}

fn emit(
    g: &mut Game,
    kind: EngineEvent,
    text: &str,
    audience: Option<PlayerSet>,
    tile: Option<TileIdx>,
    data: EventData,
    mentions: &[Mention<'_>],
) -> Event {
    let id: EventId = g.emit(kind, text, audience, tile, data, mentions).expect("an event id");
    g.chronicle().events().iter().find(|e| e.id == id).cloned().expect("the event")
}

fn named(text: &str, refs: &[NameRef]) -> Vec<(String, PlayerId, RefKind)> {
    refs.iter()
        .map(|r| (text[r.start as usize..r.end as usize].to_owned(), r.player, r.kind))
        .collect()
}

fn seen_by(g: &Game, ev: &Event, p: PlayerId) -> String {
    let out: EventOut<'_> = g.event_view(ev, Some(p));
    out.event.text.to_string()
}

#[test]
fn possessives_and_names_outside_ascii() {
    let mut g = testing::duel();
    if let Some(p) = g.player_mut(GREECE, PlayerTouch::NAME) {
        p.name = "Aztecs".into();
        p.leader = "Moctezuma".into();
    }
    testing::city(&mut g, GREECE, TileIdx(30), "Kraków");
    let ev = emit(
        &mut g,
        EngineEvent::UnitKilled,
        "The Aztecs's Warrior was killed near Kraków.",
        None,
        None,
        EventData::default(),
        &[],
    );
    assert_eq!(&*ev.text, "The Aztecs' Warrior was killed near Kraków.");
    assert_eq!(
        named(&ev.text, &ev.refs),
        [("Aztecs".to_owned(), GREECE, RefKind::Civ), ("Kraków".to_owned(), GREECE, RefKind::City)]
    );
    // Byte offsets: "ó" is two bytes, so the reference ends one past the character count.
    let krakow = ev.refs[1];
    assert_eq!((krakow.end - krakow.start) as usize, "Kraków".len());
    assert_eq!("Kraków".chars().count() + 1, "Kraków".len());
    assert_eq!(
        seen_by(&g, &ev, ROME),
        "The Unknown Civilization's Warrior was killed near an unknown city."
    );
    // Greece knows itself; a spectator sees the event as it happened.
    assert_eq!(seen_by(&g, &ev, GREECE), &*ev.text);
    assert_eq!(g.event_view(&ev, None).event.text, ev.text);
    assert!(matches!(g.event_view(&ev, Some(GREECE)).event, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn a_razed_city_is_named_through_its_mention() {
    let mut g = testing::duel();
    let sparta = testing::city(&mut g, GREECE, TileIdx(30), "Sparta");
    g.remove_city(sparta).expect("a city");
    let ev = emit(
        &mut g,
        EngineEvent::CityRazed,
        "Sparta has been razed by Rome.",
        None,
        Some(TileIdx(30)),
        EventData { player: Some(ROME), ..EventData::default() },
        &[Mention::city("Sparta", GREECE)],
    );
    assert_eq!(
        named(&ev.text, &ev.refs),
        [("Sparta".to_owned(), GREECE, RefKind::City), ("Rome".to_owned(), ROME, RefKind::Civ)]
    );
    // Rome has not met Greece: the city goes, capitalised at the start of the sentence.
    assert_eq!(seen_by(&g, &ev, ROME), "An unknown city has been razed by Rome.");
    // Greece has not met Rome: the razer goes, and so do its id and the tile.
    let out = g.event_view(&ev, Some(GREECE));
    assert_eq!(&*out.event.text, "Sparta has been razed by Unknown Civilization.");
    assert_eq!(out.event.data.as_ref().and_then(|d| d.player), None);
    assert_eq!((out.event.tile, out.event.refs.len()), (None, 0));
    assert_eq!(out.unknown, set(&[ROME]));
}

#[test]
fn a_renamed_civilization_is_named_through_its_mention() {
    let mut g = testing::duel();
    if let Some(p) = g.player_mut(GREECE, PlayerTouch::NAME) {
        p.name = "Hellas".into();
    }
    let ev = emit(
        &mut g,
        EngineEvent::CivRenamed,
        "Greece is now known as Hellas, led by Alexander.",
        None,
        None,
        EventData { player: Some(GREECE), ..EventData::default() },
        &[Mention::civ("Greece", GREECE)],
    );
    assert_eq!(
        named(&ev.text, &ev.refs),
        [
            ("Greece".to_owned(), GREECE, RefKind::Civ),
            ("Hellas".to_owned(), GREECE, RefKind::Civ),
            ("Alexander".to_owned(), GREECE, RefKind::Leader),
        ]
    );
    assert_eq!(
        seen_by(&g, &ev, ROME),
        "Unknown Civilization is now known as Unknown Civilization, led by an unknown leader."
    );
    // A mention that overlaps a reference already found is not added twice.
    let refs = g.name_refs("Hellas rises.", &[Mention::civ("Hellas", GENEVA)]);
    assert_eq!(named("Hellas rises.", &refs), [("Hellas".to_owned(), GREECE, RefKind::Civ)]);
}

#[test]
fn a_private_event_on_a_tile_others_see_does_not_widen() {
    let mut g = testing::duel();
    let t = TileIdx(44);
    g.dv.vis.reveal_for_test(GREECE, t);
    let private = emit(
        &mut g,
        EngineEvent::UnitBuilt,
        "Rome built a Warrior.",
        Some(set(&[ROME])),
        Some(t),
        EventData::default(),
        &[],
    );
    assert_eq!(private.audience, Some(set(&[ROME])));
    let seen = emit(
        &mut g,
        EngineEvent::Pillaged,
        "A farm was pillaged.",
        Some(set(&[ROME])),
        Some(t),
        EventData::default(),
        &[],
    );
    assert_eq!(seen.audience, Some(set(&[ROME, GREECE])));
    let public = emit(
        &mut g,
        EngineEvent::Pillaged,
        "A mine was pillaged.",
        None,
        Some(t),
        EventData::default(),
        &[],
    );
    assert_eq!(public.audience, None);
    // Nobody outside the audience hears of the private one.
    let feed: Vec<String> =
        g.events_for(Some(GREECE), 0, 10).into_iter().map(|e| e.event.text.to_string()).collect();
    assert_eq!(feed, ["A farm was pillaged.", "A mine was pillaged."]);
    assert_eq!(g.events_for(Some(ROME), 0, 1).len(), 1, "the newest only");
    assert_eq!(g.events_for(None, 1, 10).len(), 2, "after id 1");
}

#[test]
fn coordinates_are_scrubbed_with_the_names() {
    let mut g = testing::duel();
    let ev = emit(
        &mut g,
        EngineEvent::WarDeclared,
        "Greece's army gathers at (3, -4) and (12,7).",
        Some(set(&[ROME, GREECE])),
        Some(TileIdx(9)),
        EventData { a: Some(GREECE), b: Some(ROME), ..EventData::default() },
        &[],
    );
    let out = g.event_view(&ev, Some(ROME));
    assert_eq!(
        &*out.event.text,
        "Unknown Civilization's army gathers at an unknown location and an unknown location."
    );
    let d = out.event.data.as_deref().expect("data");
    assert_eq!((d.a, d.b, out.event.tile), (None, Some(ROME), None));
    // Once met, nothing is hidden and the coordinates stay.
    g.set_met(ROME, GREECE).expect("a pair");
    assert_eq!(seen_by(&g, &ev, ROME), "Greece's army gathers at (3, -4) and (12,7).");
}

#[test]
fn a_shorter_name_matches_where_the_longest_fails_the_boundary() {
    let mut g = testing::duel();
    testing::city(&mut g, GREECE, TileIdx(30), "Rome Prime");
    let text = "Rome Primeval forces march; Rome Prime stands.";
    let refs = g.name_refs(text, &[]);
    assert_eq!(
        named(text, &refs),
        [("Rome".to_owned(), ROME, RefKind::Civ), ("Rome Prime".to_owned(), GREECE, RefKind::City)]
    );
    // An underscore and a letter outside ASCII are word characters, as Python's \w says.
    assert!(g.name_refs("Rome_x and Romeé", &[]).is_empty());
}

#[test]
fn a_un_tally_names_its_unknown_candidates() {
    let mut g = testing::duel();
    let results = UnResult {
        turn: 1,
        tally: vec![(GREECE, 3), (ROME, 1)],
        votes_needed: 3,
        winner: Some(GREECE),
    };
    let ev = emit(
        &mut g,
        EngineEvent::UnVote,
        "The world has voted.",
        None,
        None,
        EventData { results: Some(Box::new(results)), ..EventData::default() },
        &[],
    );
    let out = g.event_view(&ev, Some(ROME));
    let res = out.event.data.as_deref().and_then(|d| d.results.as_deref()).expect("results");
    assert_eq!((res.winner, out.unknown), (None, set(&[GREECE])));
    assert_eq!(res.tally, [(GREECE, 3), (ROME, 1)], "the tally keeps its ids for a view to name");
    // The barbarians are always known.
    assert!(g.known_to(Some(ROME)).is_some_and(|k| k.contains(PlayerId(3))));
    assert_eq!(UNKNOWN_CIV, "Unknown Civilization");
}

#[test]
fn engine_events_are_hashed_and_host_events_are_not() -> Result<(), crate::base::digest::CanonError>
{
    let mut g = testing::duel();
    let digest = g.digest()?;
    let batch = g.emit_host("game_paused", "The game is paused.", None, EventData::default());
    assert_eq!(batch.len(), 1);
    assert_eq!(g.digest()?, digest, "host activity never moves the digest");
    g.add_thought(ROME, "Expand east.", None);
    assert_eq!(g.digest()?, digest);
    emit(&mut g, EngineEvent::TurnStart, "Turn 1.", None, None, EventData::default(), &[]);
    assert_ne!(g.digest()?, digest, "an engine event moves the running hash");
    assert_eq!(g.st.chronicle().engine_events, 1);
    assert_eq!(g.st.host().host_events, 1);
    // Event ids are one feed, host and engine together.
    let ids: Vec<u32> = g.chronicle().events().iter().map(|e| e.id.get()).collect();
    assert_eq!(ids, [1, 2]);
    Ok(())
}

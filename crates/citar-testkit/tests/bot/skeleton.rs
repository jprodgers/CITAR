//! The bot skeleton of package 2-00a: a `Bot` drives seats through `Game::drive` and answers
//! through `Game::answer`, counting what it took and had refused, and leaving to the host a
//! negotiation its seat's model owns. Both versions found their capital (basic-1's settler through
//! `unit_action`, Python's tool, since package 2-01b; the idle bot through `found_city`) and
//! reject what they are offered (basic-1 with Python's own lines until 2-05 ports its answers).

use std::sync::Arc;

use citar_bot::{Bot, BotSpec, Overrides, Owners, Tuning, VersionId};
use citar_engine::api::testops;
use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::game::diplomacy::actions::OpenNegotiation;
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{Action, DebugOptions, DriveOptions, DriverOutcome, Drivers, Game, Stop};
use citar_engine::rules::Ruleset;
use citar_engine::state::diplo::NegStatus;
use citar_testkit::script::map_doc;
use serde_json::{Value, json};

/// A bare arena game with `seats`, a settler each, no city-states, barbarians or ruins.
fn arena(seats: &Value) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({"seed": 9, "map": doc, "players": seats, "city_states": 0,
                     "barbarians": "off", "ruins": false});
    let r = Ruleset::shared();
    let (mut g, _) = Game::new(r, &config_from_value(r, cfg).expect("settings")).expect("a game");
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("bare");
    let spots = [(5, 5), (18, 10), (18, 4)];
    let n = g.majors(true).count();
    let ops: Vec<Value> = spots
        .iter()
        .take(n)
        .enumerate()
        .map(|(p, &(x, y))| json!({"op": "add_unit", "player": p, "unit": "Settler", "x": x, "y": y}))
        .collect();
    g.apply_ops(&Value::Array(ops)).expect("settlers");
    g
}

fn bot(version: VersionId, owners: Owners) -> Bot {
    let tuning = Arc::new(Tuning::new(version, Overrides::default()));
    Bot::new(Arc::new(BotSpec::new(version, tuning, None, None).with_owners(owners)))
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

#[test]
fn the_skeleton_founds_its_capital_and_rejects_what_it_is_offered() {
    let mut g = arena(&json!([{"controller": "bot"}, {"controller": "bot"}, {}]));
    g.meet(PlayerId(0), PlayerId(2)).expect("they meet");
    let (mut a, mut b) =
        (bot(VersionId::Basic1, Owners::ALL_BOT), bot(VersionId::Idle, Owners::ALL_BOT));
    let mut d = Drivers::none(3).with(PlayerId(0), &mut a).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(2)));
    drop(d);
    for p in [PlayerId(0), PlayerId(1)] {
        assert_eq!(g.player_cities(p).count(), 1, "player {} founded its capital", p.0);
    }
    assert_eq!(a.refusals().of("unit_action"), (1, 0));
    assert_eq!(b.refusals().of("found_city"), (1, 0));
    // The host's seat offers something; the bot's driver rejects it on the next drive.
    let offer = OpenNegotiation { to: 0, message: json!("Friends?"), give: None, receive: None };
    let (out, _) = g.act(PlayerId(2), Action::OpenNegotiation(offer)).expect("it opens");
    let nid = out["negotiation_id"].as_u64().and_then(|n| u32::try_from(n).ok());
    let nid = nid.and_then(NegotiationId::new).expect("an id");
    let mut d = Drivers::none(3).with(PlayerId(0), &mut a).with(PlayerId(1), &mut b);
    let (stop, _) = g.drive(&mut d, DriveOptions::default()).expect("live");
    assert_eq!(stop, Stop::External(PlayerId(2)));
    drop(d);
    assert_eq!(g.negotiation(nid).map(|n| n.status), Some(NegStatus::Rejected));
    assert_eq!(a.refusals().of("respond_negotiation"), (1, 0));
    clean(&mut g);
}

#[test]
fn a_negotiation_the_seats_model_owns_is_left_to_the_host() {
    let mut g = arena(&json!([{}, {"controller": "hybrid"}]));
    g.meet(PlayerId(0), PlayerId(1)).expect("they meet");
    g.apply_ops(&json!([{"op": "set_player", "player": 0, "gold": 50}])).expect("gold");
    let offer = OpenNegotiation {
        to: 1,
        message: json!("A gift."),
        give: Some(json!([{"type": "gold", "amount": 20}])),
        receive: Some(json!([])),
    };
    let (out, _) = g.act(PlayerId(0), Action::OpenNegotiation(offer)).expect("it opens");
    let nid = out["negotiation_id"].as_u64().and_then(|n| u32::try_from(n).ok());
    let nid = nid.and_then(NegotiationId::new).expect("an id");
    let owners = Owners::from_json(&json!({"trades": "llm"})).expect("owners");
    let mut model_trades = bot(VersionId::Basic1, owners);
    let (outcome, _) = g.answer(PlayerId(1), nid, &mut model_trades).expect("it waits on 1");
    assert_eq!(outcome, DriverOutcome::Deferred);
    assert_eq!(g.negotiation(nid).map(|n| n.status), Some(NegStatus::Open));
    assert_eq!(model_trades.refusals().of("respond_negotiation"), (0, 0));
    // The bot that owns trades answers it itself: no.
    let mut bot_trades = bot(VersionId::Basic1, Owners::ALL_BOT);
    let (outcome, _) = g.answer(PlayerId(1), nid, &mut bot_trades).expect("it waits on 1");
    assert_eq!(outcome, DriverOutcome::Done);
    assert_eq!(g.negotiation(nid).map(|n| n.status), Some(NegStatus::Rejected));
    clean(&mut g);
}

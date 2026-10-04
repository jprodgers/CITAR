//! The host's reads against what `EngineGame` gave (`engine_api.py:413-655`), on games made as
//! a host makes them, and the test operations of package 2-06a.

use serde_json::{Value, json};

use super::{NegotiationHead, phase_name};
use crate::base::ids::{EventId, NegotiationId, PlayerId};
use crate::game::Game;
use crate::game::setup::config_from_value;
use crate::rules::Ruleset;
use crate::state::Phase;

const ROME: PlayerId = PlayerId(0);
const GREECE: PlayerId = PlayerId(1);

/// A game made as a host makes one, from the lobby's settings.
fn game(config: Value) -> Game {
    let rules = Ruleset::shared();
    let setup = config_from_value(rules, config).expect("settings that make a game");
    Game::new(rules, &setup).expect("a new game").0
}

/// A duel on a generated map with city-states and no barbarians, the second seat a bot.
fn duel() -> Game {
    game(json!({
        "seed": 11, "map_size": "duel", "map_type": "pangaea", "barbarians": "off",
        "players": [{"controller": "human"}, {"controller": "bot", "handicap": "ai"}],
        "on_disconnect": "skip", "lobby_note": "kept",
    }))
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().map(|m| m.keys().map(String::as_str).collect()).unwrap_or_default()
}

fn end_turns(g: &mut Game, n: usize) {
    for _ in 0..n {
        g.end_turn(g.current()).expect("the turn ends");
    }
}

fn sound(g: &Game) {
    let broken = g.check_invariants();
    assert!(broken.is_empty(), "{broken:?}");
}

#[test]
fn the_heads_are_where_the_game_stands() {
    let mut g = duel();
    let h = g.heads();
    assert_eq!(
        (h.turn, h.current, h.phase, h.winner, h.victory),
        (1, ROME, Phase::Playing, None, None)
    );
    assert_eq!(h.turn_limit, g.total_turns());
    assert_eq!(h.revision, g.rev());
    assert_eq!(h.alive.len(), g.state().players().len());
    assert!(h.alive.iter().all(|&a| a));
    assert_eq!((h.is_alive(GREECE), h.is_alive(PlayerId(60))), (Some(true), None));
    assert!(h.open.is_empty() && h.poisoned.is_none());

    g.meet(ROME, GREECE).expect("they meet");
    g.open_negotiation_as(ROME, GREECE, "Friends?", &[], &[]).expect("it opens");
    let h = g.heads();
    let head = NegotiationHead {
        id: 1,
        initiator: 0,
        responder: 1,
        status: "open",
        awaiting: Some(1),
        entries: 1,
    };
    assert_eq!(h.open, vec![head]);
    assert_eq!(h.revision, g.rev(), "a write moves the revision the heads carry");
    let nid = NegotiationId::new(1).expect("an id");
    assert_eq!(g.negotiation_head(nid), Some(head));
    assert_eq!(h.open_head(1), Some(&head));
    assert_eq!(h.open_heads(Some(GREECE)).count(), 1);
    assert_eq!(h.open_heads(Some(PlayerId(2))).count(), 0, "a city-state is no party");
    assert_eq!(
        serde_json::to_value(head).expect("json"),
        json!({
            "id": 1, "initiator": 0, "responder": 1, "status": "open", "awaiting": 1, "entries": 1,
        })
    );
    let rec = g.negotiation_record(nid).expect("the record");
    for k in ["id", "initiator", "responder", "turn", "status", "awaiting", "proposal", "history"] {
        assert!(rec.get(k).is_some(), "{k} in {rec}");
    }
    assert_eq!(g.negotiation_records(Some(GREECE), true), vec![rec.clone()]);
    g.close_negotiation(nid, crate::state::diplo::NegStatus::Expired, "Too late.", None)
        .expect("it closes");
    assert!(g.heads().open.is_empty());
    assert!(g.negotiation_records(None, true).is_empty());
    assert_eq!(g.negotiation_records(None, false).len(), 1, "settled ones too");
    assert_eq!(g.negotiation_head(nid).map(|h| h.status), Some("expired"));

    g.poison("a test");
    assert_eq!(g.heads().poisoned.as_deref(), Some("a test"));
}

#[test]
fn a_summary_and_its_player_rows_have_pythons_keys() {
    let g = duel();
    let s = g.summary_json();
    assert_eq!(
        keys(&s),
        ["turn", "current", "phase", "winner", "victory", "turn_limit", "players"]
    );
    let players = s["players"].as_array().expect("players");
    assert_eq!(players.len(), g.state().players().len());
    let rome = &players[0];
    assert_eq!(
        keys(rome),
        [
            "id",
            "kind",
            "name",
            "color",
            "leader",
            "nation",
            "alive",
            "eliminated_turn",
            "controller",
            "handicap",
            "auto",
            "overrides",
            "difficulty",
            "founded_city",
        ]
    );
    assert_eq!(rome["kind"], "major");
    assert_eq!(rome["controller"], "human");
    assert_eq!(rome["overrides"], json!({}));
    assert_eq!(rome["difficulty"], "Prince", "a major falls back to the game's difficulty");
    assert_eq!(rome["eliminated_turn"], Value::Null);
    assert_eq!(keys(&rome["auto"]), ["un_vote", "conquest", "free_picks"]);
    let greece = g.player_row(GREECE).expect("a player");
    assert_eq!(greece["overrides"], json!({"handicap": "ai"}), "only what its seat set");
    let cs = players.iter().find(|p| p["kind"] == "city_state").expect("a city-state");
    assert_eq!(cs["difficulty"], Value::Null);
    assert!(g.player_row(PlayerId(60)).is_none());
    assert_eq!(g.majors_json(true), players[..2].to_vec());
}

#[test]
fn the_lobby_config_is_pythons_normalised_dict() {
    let g = duel();
    let c = g.lobby_config();
    assert_eq!(
        keys(&c),
        [
            "map_size",
            "map_type",
            "width",
            "height",
            "seed",
            "speed",
            "difficulty",
            "barbarian_difficulty",
            "ai_base_values",
            "starting_era",
            "barbarians",
            "barbarian_aggression",
            "turn_limit",
            "victories",
            "city_states",
            "religion",
            "espionage",
            "nuclear_weapons",
            "tech_trading",
            "ruins",
            "map_edges",
            "river_density",
            "resources",
            "on_disconnect",
            "reconnect_seconds",
            "players",
            "lobby_note",
            "wrap_x",
            "wrap_y",
        ]
    );
    assert_eq!((c["map_size"].as_str(), c["map_type"].as_str()), (Some("duel"), Some("pangaea")));
    assert_eq!((&c["width"], &c["height"]), (&json!(44), &json!(28)));
    assert_eq!(c["seed"], 11);
    assert_eq!(c["speed"], "Standard");
    assert_eq!(c["barbarians"], "off");
    assert_eq!(c["turn_limit"], g.total_turns());
    assert_eq!(c["victories"]["Time"], true);
    assert_eq!(c["map_edges"], "ice_caps");
    assert_eq!(c["resources"], Value::Null, "normal placement");
    assert_eq!(c["on_disconnect"], "skip", "the lobby's own");
    assert_eq!(c["reconnect_seconds"], 180, "Python's default");
    assert_eq!(c["lobby_note"], "kept");
    assert_eq!(c["players"].as_array().map(Vec::len), Some(2));
    assert_eq!((&c["wrap_x"], &c["wrap_y"]), (&json!(false), &json!(false)));

    let g = game(json!({
        "seed": 4, "map_size": "duel", "turn_limit": 90, "speed": "quick",
        "victories": {"Time": false}, "resources": {"luxury": {"each": {"Wine": {"mode": "off"}}}},
    }));
    let c = g.lobby_config();
    assert_eq!((c["turn_limit"].as_i64(), c["speed"].as_str()), (Some(90), Some("Quick")));
    assert_eq!(c["victories"]["Time"], false);
    assert_eq!(c["resources"]["luxury"]["each"], json!({"Wine": {"mode": "off"}}));
    assert_eq!(c["players"], json!([{}, {}]), "a duel's two seats, as setup filled them in");
}

#[test]
fn an_editor_maps_config_names_the_map() {
    let rules = Ruleset::shared();
    let doc = crate::api::maps::blank_map(rules, 20, 16, "Grassland", "Flatland").expect("a map");
    let mut doc = doc;
    doc["starts"] = json!([5, 300]);
    let g = game(json!({"seed": 2, "map": doc, "players": [{}, {}], "city_states": 0}));
    let c = g.lobby_config();
    assert_eq!(c["map_type"], "custom");
    assert_eq!(c["map"], "flatland");
    assert_eq!(c["map_edges"], Value::Null);
    assert_eq!((&c["width"], &c["height"]), (&json!(20), &json!(16)));
}

#[test]
fn events_stats_and_thoughts_are_pythons_rows() {
    let mut g = duel();
    end_turns(&mut g, 6);
    let all = g.event_rows(None);
    assert!(!all.is_empty());
    assert_eq!(g.event_rows(Some(0)), all, "0 is every event, as Python's [-0:]");
    assert_eq!(g.event_rows(Some(2)), all[all.len() - 2..].to_vec());
    for (ev, row) in g.chronicle().events().iter().zip(&all) {
        assert_eq!(g.event_by_id(ev.id).map(|e| e.id), Some(ev.id));
        assert_eq!(row["id"], ev.id.get());
        assert_eq!(*row, g.event_json(ev, None), "as it happened");
    }
    let past = EventId::new(all.len() as u32 + 1).expect("an id");
    assert!(g.event_by_id(past).is_none());

    let stats = g.stats_rows(None);
    assert_eq!(stats.len(), 3, "a row per round played");
    assert_eq!(keys(&stats[0]), ["turn", "players"]);
    assert_eq!(g.stats_rows(Some(1)), stats[2..].to_vec());

    g.add_thought(ROME, "Expand east.", Some("thought"));
    g.add_thought(GREECE, "Hold.", None);
    let th = g.thought_rows(None, 0);
    assert_eq!(th.len(), 2);
    assert_eq!(th[0], json!({"turn": 4, "player": 0, "text": "Expand east.", "kind": "thought"}));
    assert_eq!(g.thought_rows(Some(GREECE), 0), th[1..].to_vec());
    assert_eq!(g.thought_rows(None, 1), th[1..].to_vec());
}

#[test]
fn a_whole_save_loads_back_to_the_same_game_and_leaves_the_journal_alone() {
    let mut g = duel();
    end_turns(&mut g, 8);
    g.add_thought(ROME, "A note.", None);
    let save = g.save_whole().expect("it saves");
    let chunks: Vec<&[u8]> = save.history.iter().map(Vec::as_slice).collect();
    let (loaded, report) =
        Game::load(g.rules(), &save.state, &mut chunks.into_iter()).expect("it loads");
    assert!(!report.chronicle_incomplete, "the one chunk is the whole history");
    assert_eq!(loaded.digest().ok(), g.digest().ok());
    assert_eq!(loaded.event_rows(None), g.event_rows(None));
    assert_eq!(loaded.stats_rows(None), g.stats_rows(None));
    assert_eq!(loaded.thought_rows(None, 0), g.thought_rows(None, 0));
    sound(&loaded);
    // The game's own journal has taken nothing: its first chunk is still the whole history.
    let first = g.take_journal_chunk().expect("a chunk").expect("history");
    assert_eq!(first.seq, 0);
    assert_eq!(Some(first.json), save.history);
    // And a save taken after a host took chunks is whole all the same.
    end_turns(&mut g, 2);
    let save = g.save_whole().expect("it saves");
    let chunks: Vec<&[u8]> = save.history.iter().map(Vec::as_slice).collect();
    let (_, report) = Game::load(g.rules(), &save.state, &mut chunks.into_iter()).expect("loads");
    assert!(!report.chronicle_incomplete);
}

#[test]
fn phases_have_pythons_names() {
    assert_eq!((phase_name(Phase::Playing), phase_name(Phase::Over)), ("playing", "over"));
}

#[cfg(feature = "test-ops")]
mod ops {
    use super::*;
    use crate::api::testops;

    fn op(g: &mut Game, o: Value) -> Result<Value, String> {
        testops::apply(g, &json!([o])).map(|(mut v, _)| v.swap_remove(0)).map_err(|e| e.message)
    }

    fn three() -> Game {
        game(json!({"seed": 21, "map_size": "small", "map_type": "pangaea",
                    "players": [{}, {}, {}], "barbarians": "normal"}))
    }

    #[test]
    fn eliminating_a_civilization_whose_turn_it_is_ends_its_turn_first() {
        let mut g = three();
        let done = op(&mut g, json!({"op": "eliminate", "player": 0})).expect("eliminated");
        assert_eq!(done["eliminated"], 0);
        assert_eq!((done["phase"].as_str(), done["current"].as_i64()), (Some("playing"), Some(1)));
        let rome = g.player(ROME).expect("a player");
        assert!(!rome.alive());
        assert_eq!(rome.eliminated_turn(), Some(1));
        assert_eq!((g.player_cities(ROME).count(), g.player_units(ROME).count()), (0, 0));
        assert!(g.event_rows(None).iter().any(|e| e["type"] == "eliminated"));
        assert_eq!(g.heads().is_alive(ROME), Some(false));
        sound(&g);
        let again = op(&mut g, json!({"op": "eliminate", "player": 0})).expect_err("dead");
        assert!(again.contains("eliminated already"), "{again}");
        assert!(op(&mut g, json!({"op": "eliminate", "player": 5 + 30})).is_err());
        // The game plays on without it.
        end_turns(&mut g, 4);
        sound(&g);
    }

    #[test]
    fn eliminating_a_city_state_or_a_rival_after_its_cities_were_founded() {
        let mut g = three();
        end_turns(&mut g, 6);
        let cs = g.city_states(true).map(crate::state::players::Player::id).next().expect("one");
        let done = op(&mut g, json!({"op": "eliminate", "player": cs.0})).expect("eliminated");
        assert_eq!(done["eliminated"], cs.0);
        assert!(!g.player(cs).expect("a player").alive());
        let other = if g.current() == GREECE { PlayerId(2) } else { GREECE };
        op(&mut g, json!({"op": "eliminate", "player": other.0})).expect("eliminated");
        assert!(g.heads().phase == Phase::Playing, "two majors are left");
        sound(&g);
    }

    #[test]
    fn eliminating_the_second_of_two_leaves_the_last_standing_the_winner() {
        let mut g = duel();
        let done = op(&mut g, json!({"op": "eliminate", "player": 1})).expect("eliminated");
        assert_eq!(done["phase"], "over");
        assert_eq!(
            (done["winner"].as_i64(), done["victory"].as_str()),
            (Some(0), Some("Domination"))
        );
        sound(&g);
    }

    #[test]
    fn eliminating_the_only_major_ends_the_game_with_no_winner() {
        let mut g = game(json!({"seed": 8, "map_size": "duel", "players": [{}]}));
        let done = op(&mut g, json!({"op": "eliminate", "player": 0})).expect("eliminated");
        assert_eq!((done["phase"].as_str(), &done["winner"]), (Some("over"), &Value::Null));
        assert!(g.event_rows(Some(1))[0]["type"] == "game_over");
        sound(&g);
    }

    #[test]
    fn ending_the_game_with_or_without_a_winner() {
        let mut g = duel();
        let e = op(&mut g, json!({"op": "end_game", "victory": "Time"})).expect_err("no winner");
        assert!(e.contains("needs a winner"), "{e}");
        assert!(op(&mut g, json!({"op": "end_game", "winner": 2})).is_err(), "a city-state");
        let done = op(&mut g, json!({"op": "end_game"})).expect("ended");
        assert_eq!((done["phase"].as_str(), &done["winner"]), (Some("over"), &Value::Null));
        assert_eq!(g.event_rows(Some(1))[0]["type"], "game_over");
        sound(&g);
        let e = op(&mut g, json!({"op": "end_game"})).expect_err("over");
        assert!(e.contains("The game is over."), "{e}");

        let mut g = duel();
        let done =
            op(&mut g, json!({"op": "end_game", "winner": 1, "victory": "time"})).expect("won");
        assert_eq!((done["winner"].as_i64(), done["victory"].as_str()), (Some(1), Some("Time")));
        assert_eq!(g.heads().victory, Some("Time"));
        let mut g = duel();
        let done = op(&mut g, json!({"op": "end_game", "winner": 0})).expect("won");
        assert_eq!(done["victory"], "Neutral");
        assert_eq!(g.event_rows(Some(1))[0]["type"], "victory");
        sound(&g);
    }

    #[test]
    fn the_panic_operation_panics() {
        let mut g = duel();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            testops::apply(&mut g, &json!([{"op": "panic"}])).map(|_| ())
        }));
        let payload = caught.expect_err("it panics");
        let text = payload.downcast_ref::<&str>().copied().unwrap_or_default();
        assert!(text.contains("panic test operation"), "{text}");
    }
}

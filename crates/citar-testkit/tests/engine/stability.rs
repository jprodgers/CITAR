//! The stability runner's own rules (package 1e-01, DESIGN.md 9.5; `citar_testkit::stability`):
//! - P5 reads a `None` in a refusal as the caller's own null when the call's arguments hold one
//!   inside a value, or the game keeps text a caller gave it with the word in it, and holds
//!   every other text rule;
//! - P7's bound counts the seats whose turns the host ends, the living major civilizations.

use citar_engine::api::tools::registry::TOOLS;
use citar_engine::base::text::find_word;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::state::Phase;
use citar_testkit::spec::{ActionSpec, Shape};
use citar_testkit::stability::{
    Options, Run, Step, holds_none, no_stall, null_below_top, refusal_rule_broken, stall_limit,
};
use citar_testkit::{fixtures, games};
use serde_json::json;

/// The first committed fixture, loaded.
fn fixture() -> Game {
    let f = fixtures::committed().expect("the fixtures").swap_remove(0);
    games::from_fixture(&f, b"p5", DebugOptions::OFF).expect("it loads")
}

#[test]
fn p5_reads_none_as_the_callers_null_only_when_the_call_nests_one() {
    let g = fixture();
    assert!(!holds_none(&g), "no name of the fixture holds None");
    let nested = json!({"policy": [null]});
    assert!(null_below_top(&nested));
    assert_eq!(refusal_rule_broken(&g, &nested, "Unknown policy '[None]'."), None);
    // No null of the caller's: the word is Rust's debug output.
    let plain = json!({"policy": ["x"]});
    assert!(refusal_rule_broken(&g, &plain, "Unknown policy '[None]'.").is_some());
    // A null as an argument's whole value is the argument left out, nothing to quote back.
    let left_out = json!({"policy": null});
    assert!(!null_below_top(&left_out));
    assert!(refusal_rule_broken(&g, &left_out, "Unknown policy None.").is_some());
    // Arguments that are no object: any null inside them is the caller's.
    assert!(null_below_top(&json!([1, null])) && !null_below_top(&json!(null)));
    // Every other rule still holds for a call that nests a null.
    for broken in ["Unknown policy Some(1).", "No idx::Tile.", "Unknown policy '[None]'", ""] {
        assert!(refusal_rule_broken(&g, &nested, broken).is_some(), "{broken:?}");
    }
    // Only the whole word: `Nonesuch` is no quote of anything, and no debug output either.
    assert_eq!(refusal_rule_broken(&g, &nested, "Nonesuch is unknown."), None);
}

#[test]
fn p5_reads_none_in_a_name_a_caller_gave_the_game_as_the_callers() {
    // Chaos found it (seed 610): a civilization renamed with a list that held a null is named
    // as Python's `str` writes the list, and every later refusal that names it quotes `None`,
    // whatever that refusal's own arguments.
    for f in fixtures::committed().expect("the fixtures") {
        let g = games::from_fixture(&f, b"p5", DebugOptions::OFF).expect("it loads");
        assert!(!holds_none(&g), "{}: no name holds None", f.name);
    }
    let mut g = fixture();
    let p = g.current();
    let before = refusal_rule_broken(&g, &json!({}), "You have not met [None, 'x'].");
    assert!(before.is_some(), "no caller gave the game a None yet");
    g.execute(p, "set_civ_name", &json!({"name": [null, "x"]})).expect("any name is taken");
    let name = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    assert!(!find_word(&name, "None").is_empty(), "named {name:?}");
    assert!(holds_none(&g));
    let text = format!("You have not met {name}.");
    assert_eq!(refusal_rule_broken(&g, &json!({}), &text), None);
    // The other rules still hold.
    assert!(refusal_rule_broken(&g, &json!({}), "You have not met Some(3).").is_some());
}

#[test]
fn a_refusal_quoting_a_nested_null_back_passes_p5_in_a_run() {
    // The engine quotes a caller's value back as Python does: `adopt_policy` with a null inside
    // its argument names the policy `[None]`. Random arguments meet it within a few thousand seeds.
    let g = fixture();
    let tool = TOOLS.iter().position(|t| t.name() == "adopt_policy").expect("the tool");
    let tool = u8::try_from(tool).expect("a tool byte");
    let mut met = 0;
    for seed in 0..4_000u32 {
        let spec =
            ActionSpec { tool, actor: 0, target: 0, coords: (0, 0), shape: Shape::Random(seed) };
        let call = spec.bind(&g);
        let mut probe = g.clone();
        let Err(e) = probe.execute(call.pid, call.tool, &call.args) else { continue };
        if find_word(&e.message, "None").is_empty() {
            continue;
        }
        met += 1;
        assert!(null_below_top(&call.args), "{} gave {:?}", call.args, e.message);
        let mut run = Run::new(g.clone(), Options { verify_every: 0, ..Options::default() });
        let taken = run.step(&Step::Call(spec)).unwrap_or_else(|b| panic!("{b}"));
        assert_eq!(taken.refused.as_deref(), Some(&*e.message));
        if met >= 3 {
            break;
        }
    }
    assert!(met > 0, "no random argument of adopt_policy was quoted back as None");
}

#[test]
fn p7_allows_one_end_of_turn_more_than_there_are_living_majors() {
    // In a game with city-states or barbarians, the host's end_turn plays their turns itself
    // and stops only at majors: a round from the first major's turn takes one end of turn for
    // each living major, one below the bound, whatever the number of players.
    let mut checked = 0;
    for f in fixtures::committed().expect("the fixtures") {
        let mut g = games::from_fixture(&f, b"p7", DebugOptions::OFF).expect("it loads");
        if g.phase() != Phase::Playing {
            continue;
        }
        // To the start of the next round, at its first living major's turn.
        let start = g.turn();
        while g.phase() == Phase::Playing && g.turn() == start {
            let p = g.current();
            g.end_turn(p).expect("the turn ends");
        }
        let majors = g.majors(true).count();
        if g.phase() != Phase::Playing || g.state().players().len() == majors {
            continue;
        }
        assert_eq!(stall_limit(&g), majors + 1, "{}", f.name);
        assert!(stall_limit(&g) < g.state().players().len() + 1, "{}", f.name);
        let round = g.turn();
        let mut ends = 0;
        while g.phase() == Phase::Playing && g.turn() == round {
            assert!(g.player(g.current()).is_some_and(|p| p.is_major()), "{}", f.name);
            let p = g.current();
            g.end_turn(p).expect("the turn ends");
            ends += 1;
        }
        assert!(ends <= majors, "{}: {ends} ends of turn for {majors} majors", f.name);
        no_stall(&g).unwrap_or_else(|e| panic!("{}: {e}", f.name));
        checked += 1;
    }
    assert!(checked > 0, "no committed fixture has players other than majors");
}

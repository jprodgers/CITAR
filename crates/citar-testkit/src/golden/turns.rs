//! The golden set of package 1b-03 (DESIGN.md 9.6):
//! - **`turns.json`**: a game on the arena, three civilizations, a city-state and the barbarians,
//!   set up by `Game::new` and played turn after turn to its turn limit, with the digest of
//!   every round chained (DESIGN.md 4.10). A digest that moves is a change to setup or to a
//!   turn; one that differs between targets is a determinism bug in them.
//!
//! It depends on every stage of setup and of a turn, so `golden bless` refuses it while any of
//! them is pending (DESIGN.md 6.2, 9.6): a golden set is only ever blessed on the whole pipeline.
//! Until then `golden check` computes it, and the determinism workflow compares what the targets
//! computed, with no committed file to compare them with.

use citar_engine::game::setup;
use citar_engine::game::turn::stages;
use citar_engine::rules::Ruleset;
use citar_engine::save::chain::DigestChain;
use citar_engine::state::Phase;
use serde_json::{Value, json};

use super::{SetReport, capped, diff_rows, digest_of, read_committed, render_rows};
use crate::script;

/// The stages the set depends on that are still pending: every setup stage and every stage of a
/// turn, as `name (package)`.
#[must_use]
pub fn waiting() -> Vec<String> {
    let setup = setup::waiting().map(|(s, pkg)| format!("setup: {} ({pkg})", s.name));
    let turn = stages::waiting().map(|(t, s, pkg)| format!("{t} {}: {} ({pkg})", s.id, s.name));
    setup.chain(turn).collect()
}

/// The game, and the digest of every round.
fn answers() -> (Value, Vec<String>) {
    let r = Ruleset::shared();
    let (doc, _) = match script::map_doc("arena") {
        Ok(d) => d,
        Err(e) => return (json!({"format": 1}), vec![e]),
    };
    let cfg = json!({
        "seed": 20_260_924,
        "players": [{"controller": "bot"}, {"controller": "bot"}, {"controller": "human"}],
        "city_states": 1,
        "barbarians": "normal",
        "turn_limit": 20,
        "map": doc,
    });
    let mut g = match script::new_game(r, cfg.as_object().unwrap_or(&serde_json::Map::new())) {
        Ok(g) => g,
        Err(e) => return (json!({"format": 1}), vec![format!("the game does not set up: {e}")]),
    };
    g.set_chain(Some(DigestChain::new(b"golden:turns")));
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    let mut turn = g.turn();
    while g.phase() == Phase::Playing && rows.len() < 100 {
        if let Err(e) = g.end_turn(g.current()) {
            problems.push(format!("turn {}: {}", g.turn(), e.message));
            break;
        }
        if g.turn() != turn || g.phase() != Phase::Playing {
            turn = g.turn();
            rows.push(json!([turn - 1, g.last_round_digest().map(|d| d.to_hex())]));
        }
    }
    let head = g.chain().map(|c| c.head().to_hex());
    let v = json!({
        "format": 1,
        "game": "the arena, seed 20260924: two bots and a human seat, a city-state, the barbarians, 20 turns; each round's digest, chained",
        "ruleset_id": r.id().to_hex(),
        "chain": head,
        "events": g.chronicle().events().len(),
        "rounds": rows,
    });
    (v, problems)
}

fn render(v: &Value) -> String {
    render_rows(v, &["rounds"])
}

/// The file `golden bless` writes, once nothing it depends on is pending.
#[must_use]
pub fn blessed() -> Vec<(&'static str, String)> {
    if waiting().is_empty() { vec![("turns.json", render(&answers().0))] } else { Vec::new() }
}

/// Why `golden bless` refuses the set, if it does.
#[must_use]
pub fn refusal() -> Option<String> {
    let w = waiting();
    (!w.is_empty()).then(|| {
        format!(
            "turns.json depends on every stage of setup and of a turn, and {} are pending, the \
             first {}; a golden set is only blessed on the whole pipeline",
            w.len(),
            w[0]
        )
    })
}

/// The `turns` set: computed on every target; checked against `turns.json` once it can be
/// blessed.
#[must_use]
pub fn check_turns() -> SetReport {
    let (got, mut problems) = answers();
    let waiting = waiting();
    if waiting.is_empty() {
        match read_committed("turns.json") {
            Err(e) => problems.push(e),
            Ok(want) => {
                for key in ["ruleset_id", "chain", "events"] {
                    if want.get(key) != got.get(key) {
                        problems.push(format!("turns.json: the {key} differs from this build's"));
                    }
                }
                problems.extend(diff_rows(
                    "turns.json",
                    "rounds",
                    want.get("rounds"),
                    &got["rounds"],
                ));
            }
        }
    }
    SetReport { name: "turns", computed: digest_of(&got), problems: capped(problems), waiting }
}

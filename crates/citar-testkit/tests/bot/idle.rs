//! The idle bot (`citar/bots/idle.py`, package 2-01a gate 5): an all-idle 4-seat small game of
//! 50 rounds with every check on plays without a panic or a violation, every seat founds its
//! capital, and every negotiation put to an idle seat is rejected.

use std::sync::Arc;

use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::game::{DebugOptions, Game};
use citar_engine::state::Phase;
use citar_engine::state::diplo::{DealItem, NegAction, NegStatus};
use citar_testkit::games;
use serde_json::json;

fn idle() -> Bot {
    let tuning = Arc::new(Tuning::new(VersionId::Idle, Overrides::default()));
    Bot::new(Arc::new(BotSpec::new(VersionId::Idle, tuning, None, None)))
}

fn majors(g: &Game) -> Vec<PlayerId> {
    g.majors(true).map(|p| p.id()).collect()
}

#[test]
fn an_all_idle_small_game_of_50_rounds_founds_four_capitals_and_rejects_every_offer() {
    let mut settings = games::random_settings("small", "continents", "wrap_x", 2026, 400);
    settings["players"] = json!([{"controller": "bot"}, {"controller": "bot"}, {"controller": "bot"}, {"controller": "bot"}]);
    let mut g = games::new_game(&settings, b"idle", DebugOptions::ALL).expect("a small game");
    let seats = majors(&g);
    assert_eq!(seats.len(), 4);
    // Everyone has met, so anyone may put anything to anyone.
    for (i, &a) in seats.iter().enumerate() {
        for &b in &seats[i + 1..] {
            g.meet(a, b).expect("they meet");
        }
    }
    let mut bots: Vec<Bot> = (0..g.state().players().len()).map(|_| idle()).collect();
    let mut asked: Vec<(NegotiationId, PlayerId)> = Vec::new();
    let mut capitals_checked = false;
    let mut hook = |g: &mut Game, (turn, _): games::Round| -> Result<(), String> {
        let problems = games::problems(g);
        if !problems.is_empty() {
            return Err(format!("round {turn}: {problems:?}"));
        }
        if !capitals_checked {
            // Each seat founded its capital in its first turn.
            for p in majors(g) {
                if g.player_cities(p).count() != 1 {
                    return Err(format!("round {turn}: player {} has no capital", p.0));
                }
            }
            capitals_checked = true;
        }
        // Each round a seat puts something to the next: talk, or a gift of what it has.
        let alive = majors(g);
        if alive.len() < 2 {
            return Ok(());
        }
        let from = alive[usize::try_from(turn).unwrap_or(0) % alive.len()];
        let to = alive.iter().copied().find(|&p| p != from).unwrap_or(from);
        let gold = g.player(from).map_or(0.0, |p| p.econ.gold);
        let give: Vec<DealItem> =
            if turn % 2 == 0 && gold >= 1.0 { vec![DealItem::Gold { amount: 1 }] } else { vec![] };
        let (out, _) = g
            .open_negotiation_as(from, to, "Well?", &give, &[])
            .map_err(|e| format!("round {turn}: {}", e.message))?;
        let nid = out["negotiation_id"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .and_then(NegotiationId::new)
            .ok_or("no negotiation id")?;
        asked.push((nid, to));
        Ok(())
    };
    let played = games::play_random(&mut g, &mut bots, 50, &mut hook).expect("50 rounds");
    assert_eq!(played, 50);
    assert_eq!(g.phase(), Phase::Playing);
    assert!(capitals_checked);
    // The last one is put to its seat on the next drive; the rest have been answered.
    let (last, _) = asked.pop().expect("one asked each round");
    assert_eq!(g.negotiation(last).map(|n| n.status), Some(NegStatus::Open));
    assert_eq!(asked.len(), 49);
    for (nid, to) in &asked {
        let n = g.negotiation(*nid).expect("kept");
        assert_eq!(n.status, NegStatus::Rejected, "negotiation {} to {}", nid.get(), to.0);
        let answer = n.history.last().expect("an answer");
        assert_eq!((answer.by, answer.action), (Some(*to), NegAction::Reject));
        assert_eq!(&*answer.message, "We are not interested.");
    }
    // What the bots did: each founded one city, and rejected what it was asked.
    let found: u32 = bots.iter().map(|b| b.refusals().of("found_city").0).sum();
    let rejected: u32 = bots.iter().map(|b| b.refusals().of("respond_negotiation").0).sum();
    assert_eq!(found, 4);
    assert_eq!(rejected, 49);
    // The idle bot keeps no memory.
    for p in seats {
        assert!(g.player(p).is_some_and(|x| x.seat().driver().is_none()), "player {}", p.0);
    }
}

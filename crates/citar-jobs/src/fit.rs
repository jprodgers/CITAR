//! Whether a result answers its spec (design/phase4-helper.md P4.4.4's verification).
//!
//! [`crate::result_hash`] reads a result's shape and [`crate::chain_check`] folds a chain from its
//! spec's `chain_0`; neither ties a result to the spec it claims to answer, so a modified helper
//! could hand back another game's result, or an invented one, beside a chain it folded itself
//! from the right `chain_0`. [`result_fits`] checks what one machine's result can be checked for
//! without playing the game again: its chain folds; everything the spec decides before a turn is
//! played is what this build derives from it ([`LabFacts`], [`ReferenceFacts`]: the experiment,
//! the game, its seed and map; who sat in each seat and what played it, fingerprints included;
//! the build id); the winner's label is its seat's, a crash's seat and label are the spec's; and
//! the result's turns agree with the chain's rounds. What only playing decides (the scores, the
//! stats, who won) is replication's and sampling's to check (P4.5.8).
//!
//! **Turns and rounds** (the round end's stages, `citar-engine` `game/turn/stages.rs`): R0's
//! eliminations may end the game, which then skips to R6's digest; R4 moves the turn on; R5's
//! victories and turn limit end the game after it. A finished game's `turns` (the turn number
//! less one) is therefore its rounds, or one less when its last round's eliminations ended it. A
//! crash's turn is the turn being played: from the rounds chained up to two more (a panic in R5,
//! once the turn has moved on and before the round's digest).
//!
//! [`rebased`] is the facts' other use: the `jobs` golden set's results name the build that
//! blessed them ([`crate::lab::BUILD_KEYS`] in each seat and the `engine`, a reference line's
//! [`crate::reference::BUILD_KEYS`]), which moves with any edit of the engine's or the bot's
//! sources and with CITAR's version, whether or not a game changes. The set is held to its
//! results with those keys given this build's values, so only a change of what a game computes
//! asks for a bless.

use citar_engine::rules::Ruleset;
use serde_json::{Map, Value};

use crate::hash::{Chain, ChainError, JobResult, chain_check};
use crate::lab::{self, LabFacts, LabOutcome};
use crate::reference::{self, ReferenceFacts, ReferenceOutcome};
use crate::spec::{Checked, Spec, clip};

/// Why a result does not answer its spec.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FitError {
    /// The result does not read, or says what its spec does not (`bad_result`).
    #[error("{0}")]
    Result(String),
    /// The chain does not read or does not fold (`bad_chain`).
    #[error(transparent)]
    Chain(#[from] ChainError),
}

fn result_error(what: impl Into<String>) -> FitError {
    FitError::Result(what.into())
}

/// Checks that `result`, with `chain`, answers the job `checked` describes under this build:
/// the chain folds from the spec's `chain_0`, the result holds what the spec decides, and its
/// turns follow the chain's rounds. The result hash and a claim's signature are checked apart.
///
/// # Errors
/// The first thing that does not hold.
pub fn result_fits(
    rules: &Ruleset,
    checked: &Checked,
    result: &JobResult,
    chain: &Chain,
) -> Result<(), FitError> {
    chain_check(&checked.hash, chain)?;
    match (&checked.spec, result) {
        (Spec::Lab(s), JobResult::Lab(outcome)) => {
            let facts =
                LabFacts::of(rules, s).map_err(|e| result_error(format!("the spec: {e}")))?;
            lab_fits(&facts, outcome, chain.rounds)
        }
        (Spec::Reference(s), JobResult::Reference(outcome)) => {
            let facts =
                ReferenceFacts::of(rules, s).map_err(|e| result_error(format!("the spec: {e}")))?;
            reference_fits(&facts, outcome, chain.rounds)
        }
        _ => Err(result_error(format!("the result is not a {} job's", checked.kind))),
    }
}

/// [`result_fits`] for a result and a chain as the wire carries them, each read first (an
/// unknown key refused).
///
/// # Errors
/// As [`result_fits`], and a result or a chain that does not read.
pub fn result_fits_value(
    rules: &Ruleset,
    checked: &Checked,
    result: &Value,
    chain: &Value,
) -> Result<(), FitError> {
    let result = JobResult::read(checked.kind, result).map_err(|e| result_error(e.0))?;
    let chain = <Chain as serde::Deserialize>::deserialize(chain)
        .map_err(|e| FitError::Chain(ChainError::Shape(e.to_string())))?;
    result_fits(rules, checked, &result, &chain)
}

/// `result` (a finished game's) with the keys that name the build that played it given this
/// build's values, which the spec decides: what a golden result blessed under another build id
/// is under this one. A crash names no build and comes back as it is.
///
/// # Errors
/// A spec this build cannot seat, which `check_spec` should have refused.
pub fn rebased(rules: &Ruleset, checked: &Checked, result: &Value) -> Result<Value, FitError> {
    let mut out = result.clone();
    let Some(top) = out.as_object_mut() else { return Ok(out) };
    if top.contains_key("crash") {
        return Ok(out);
    }
    match &checked.spec {
        Spec::Lab(s) => {
            let facts =
                LabFacts::of(rules, s).map_err(|e| result_error(format!("the spec: {e}")))?;
            put(top, "engine", &facts.top);
            if let Some(players) = top.get_mut("players").and_then(Value::as_object_mut) {
                for (n, seat) in facts.seats.iter().enumerate() {
                    if let Some(p) = players.get_mut(&n.to_string()).and_then(Value::as_object_mut)
                    {
                        for k in lab::BUILD_KEYS {
                            put(p, k, seat);
                        }
                    }
                }
            }
        }
        Spec::Reference(s) => {
            let facts =
                ReferenceFacts::of(rules, s).map_err(|e| result_error(format!("the spec: {e}")))?;
            for k in reference::BUILD_KEYS {
                put(top, k, &facts.top);
            }
        }
    }
    Ok(out)
}

/// `to[key]` = `from[key]`, when `from` has it.
fn put(to: &mut Map<String, Value>, key: &str, from: &Map<String, Value>) {
    if let Some(v) = from.get(key) {
        to.insert(key.to_owned(), v.clone());
    }
}

/// A value as a reason quotes it.
fn quote(v: &Value) -> String {
    clip(&v.to_string(), 120)
}

/// Every key of `want` holds the same value in `got`.
fn holds(what: &str, got: &Value, want: &Map<String, Value>) -> Result<(), FitError> {
    for (k, w) in want {
        let g = got.get(k).unwrap_or(&Value::Null);
        if g != w {
            return Err(result_error(format!(
                "{what}'s {k} is {}, and its spec gives {}",
                quote(g),
                quote(w)
            )));
        }
    }
    Ok(())
}

/// A finished game's turns against its chain's rounds (the module's "Turns and rounds").
fn finished_turns(turns: u32, rounds: u32) -> Result<(), FitError> {
    if turns == rounds || turns.checked_add(1) == Some(rounds) {
        return Ok(());
    }
    Err(result_error(format!(
        "the result's {turns} turns do not follow the chain's {rounds} rounds (the same, or one \
         fewer for a game its last round's eliminations ended)"
    )))
}

/// A crash's turn against its chain's rounds (the module's "Turns and rounds").
fn crash_turn(turn: i64, rounds: u32) -> Result<(), FitError> {
    let r = i64::from(rounds);
    if (r..=r + 2).contains(&turn) {
        return Ok(());
    }
    Err(result_error(format!(
        "the crash on turn {turn} does not follow the chain's {rounds} rounds (from them to two \
         more)"
    )))
}

fn lab_fits(facts: &LabFacts, outcome: &LabOutcome, rounds: u32) -> Result<(), FitError> {
    let seat_label = |p: usize| facts.labels.get(p).cloned().flatten();
    match outcome {
        LabOutcome::Finished(r) => {
            let v = serde_json::to_value(r).map_err(|e| result_error(e.to_string()))?;
            holds("the result", &v, &facts.top)?;
            let got: Vec<usize> = r.players.keys().map(|&p| usize::from(p)).collect();
            let want: Vec<usize> = (0..facts.seats.len()).collect();
            if got != want {
                return Err(result_error(format!(
                    "the result's players are seats {got:?}, and its spec has {} seats",
                    want.len()
                )));
            }
            for (n, seat) in facts.seats.iter().enumerate() {
                holds(&format!("player {n}"), &v["players"][n.to_string()], seat)?;
            }
            match r.winner {
                Some(w) if usize::from(w) >= facts.seats.len() => {
                    return Err(result_error(format!("the winner, seat {w}, is no seat")));
                }
                Some(w) if r.winner_label != seat_label(usize::from(w)) => {
                    return Err(result_error(format!(
                        "the winner's label is {:?}, and seat {w}'s is {:?}",
                        r.winner_label,
                        seat_label(usize::from(w))
                    )));
                }
                None if r.winner_label.is_some() => {
                    return Err(result_error("a result without a winner names its label"));
                }
                _ => {}
            }
            finished_turns(u32::try_from(r.turns).unwrap_or(u32::MAX), rounds)
        }
        LabOutcome::Crashed(c) => {
            let c = &c.crash;
            match c.player {
                Some(p) if usize::from(p) >= facts.seats.len() => {
                    return Err(result_error(format!("the crash's seat {p} is no seat")));
                }
                Some(p) if c.label != seat_label(usize::from(p)) => {
                    return Err(result_error(format!(
                        "the crash's label is {:?}, and seat {p}'s is {:?}",
                        c.label,
                        seat_label(usize::from(p))
                    )));
                }
                None if c.label.is_some() => {
                    return Err(result_error("a crash outside a seat names a label"));
                }
                _ => {}
            }
            crash_turn(i64::from(c.turn), rounds)
        }
    }
}

fn reference_fits(
    facts: &ReferenceFacts,
    outcome: &ReferenceOutcome,
    rounds: u32,
) -> Result<(), FitError> {
    let ReferenceOutcome::Finished(r) = outcome else {
        // A crash is its message only; its chain is checked, and replication does the rest.
        return Ok(());
    };
    let v = serde_json::to_value(r).map_err(|e| result_error(e.to_string()))?;
    holds("the result", &v, &facts.top)?;
    if usize::try_from(r.players).ok() != Some(r.civs.len()) {
        return Err(result_error(format!(
            "the result counts {} players and lists {} civilizations",
            r.players,
            r.civs.len()
        )));
    }
    for (n, civ) in r.civs.iter().enumerate() {
        if u32::try_from(n).ok() != Some(civ.pid) {
            return Err(result_error(format!("civilization {n} is seat {}", civ.pid)));
        }
        // The same rounding of the same number on both sides, so the same bits.
        let want = facts.aggression(civ.pid);
        if want.map(f64::to_bits) != Some(civ.aggression.to_bits()) {
            return Err(result_error(format!(
                "seat {}'s aggression is {}, and its seat and the seed give {want:?}",
                civ.pid, civ.aggression
            )));
        }
    }
    finished_turns(r.turns, rounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::GoldenSet;
    use crate::spec::check_spec;
    use serde_json::json;

    fn rules() -> &'static Ruleset {
        Ruleset::shared()
    }

    fn golden(name: &str) -> (Checked, Value, Chain) {
        let set = GoldenSet::committed().expect("the set");
        let job = &set.jobs[name];
        let checked = check_spec(rules(), job.kind, &job.spec, None).expect("its spec");
        let result = rebased(rules(), &checked, &job.result).expect("rebased");
        (checked, result, job.chain())
    }

    fn fits(checked: &Checked, result: &Value, chain: &Chain) -> Result<(), FitError> {
        let chain = serde_json::to_value(chain).expect("a chain");
        result_fits_value(rules(), checked, result, &chain)
    }

    /// Every golden job's result answers its spec, and another job's result answers none.
    #[test]
    fn a_result_fits_its_own_spec_and_no_other() {
        let names = ["duel-a", "duel-b", "small-a", "reference-a"];
        for name in names {
            let (checked, result, chain) = golden(name);
            assert_eq!(fits(&checked, &result, &chain), Ok(()), "{name}");
            for other in names.iter().filter(|o| **o != name) {
                let (_, theirs, their_chain) = golden(other);
                // Another game's result beside this game's chain, or beside its own.
                let e = fits(&checked, &theirs, &chain).expect_err("another game's result");
                assert!(matches!(e, FitError::Result(_)), "{other} for {name}: {e}");
                assert!(fits(&checked, &theirs, &their_chain).is_err(), "{other} for {name}");
            }
        }
        // The same game in a ranking job: its own spec hash, so the lab chain does not fold.
        let (checked, result, chain) = golden("duel-a");
        let ranked =
            check_spec(rules(), crate::Kind::RankingGame, &checked.canonical_value(), None)
                .expect("the spec as a ranking job");
        assert!(matches!(fits(&ranked, &result, &chain), Err(FitError::Chain(_))));
    }

    /// What the spec decides is held, field by field; what only playing decides is not.
    #[test]
    fn what_the_spec_decides_is_held() {
        let (checked, result, chain) = golden("duel-b");
        let refused = |path: &[&str], v: Value| {
            let mut r = result.clone();
            let mut at = &mut r;
            for k in &path[..path.len() - 1] {
                at = &mut at[*k];
            }
            at[path[path.len() - 1]] = v;
            match fits(&checked, &r, &chain) {
                Err(FitError::Result(why)) => why,
                other => panic!("{path:?}: {other:?}"),
            }
        };
        assert!(refused(&["exp"], json!("another")).contains("exp"));
        assert!(refused(&["i"], json!(7)).contains("'s i is 7"));
        assert!(refused(&["seed"], json!(1)).contains("seed"));
        assert!(refused(&["map"], json!("archipelago")).contains("map"));
        assert!(refused(&["engine"], json!("000000000000")).contains("engine"));
        assert!(refused(&["errors"], json!(["boom"])).contains("errors"));
        for k in ["label", "bot", "profile", "profile_name", "version", "fingerprint", "build"] {
            assert!(refused(&["players", "0", k], json!("x")).contains(k), "{k}");
        }
        assert!(refused(&["players", "0", "profile_rev"], json!(7)).contains("profile_rev"));
        let why = refused(&["players", "0", "params", "tech_noise"], json!(0.5));
        assert!(why.contains("params"), "{why}");
        assert!(refused(&["players", "0", "fixed_aggression"], json!(0.1)).contains("fixed"));
        assert!(refused(&["players", "1", "aggression"], json!(0.9)).contains("aggression"));
        assert!(refused(&["players", "0", "levels"], json!({"a": 1})).contains("levels"));
        assert!(refused(&["winner_label"], json!("nobody")).contains("winner"));
        assert!(refused(&["turns"], json!(5)).contains("rounds"));
        // A seat dropped, or one added.
        let mut r = result.clone();
        let p1 = r["players"]["1"].clone();
        r["players"].as_object_mut().expect("players").remove("1");
        assert!(fits(&checked, &r, &chain).is_err());
        r["players"]["1"] = p1.clone();
        r["players"]["2"] = p1;
        assert!(fits(&checked, &r, &chain).is_err());
        // What only playing decides is replication's: a score changes nothing here.
        let mut r = result.clone();
        r["players"]["0"]["score"] = json!(9999);
        assert_eq!(fits(&checked, &r, &chain), Ok(()));
        // One round fewer: a game its last round's eliminations ended reads so; two do not.
        let mut r = result;
        r["turns"] = json!(chain.rounds - 1);
        assert_eq!(fits(&checked, &r, &chain), Ok(()));
        r["turns"] = json!(chain.rounds - 2);
        assert!(fits(&checked, &r, &chain).is_err());
    }

    #[test]
    fn a_reference_line_and_a_crash_are_held_too() {
        let (checked, result, chain) = golden("reference-a");
        for (k, v) in [
            ("seed", json!(1)),
            ("size", json!("huge")),
            ("speed", json!("Marathon")),
            ("turn_limit", json!(39)),
            ("bot", json!("basic-1+abc")),
            ("bot_errors", json!(1)),
        ] {
            let mut r = result.clone();
            r[k] = v;
            assert!(fits(&checked, &r, &chain).is_err(), "{k}");
        }
        let mut r = result.clone();
        r["civs"][1]["aggression"] = json!(0.5);
        assert!(fits(&checked, &r, &chain).is_err(), "a seat's aggression");
        let mut r = result;
        r["players"] = json!(3);
        assert!(fits(&checked, &r, &chain).is_err(), "a civilization missing");

        // A lab crash: its seat's label, and a turn the rounds allow.
        let (checked, _, chain) = golden("duel-a");
        let crash = |turn: u32, player: Value, label: Value| json!({"crash": {"turn": turn, "player": player, "label": label, "message": "boom"}});
        let r = chain.rounds;
        assert_eq!(fits(&checked, &crash(r + 1, json!(1), json!("b")), &chain), Ok(()));
        assert_eq!(fits(&checked, &crash(r, json!(null), json!(null)), &chain), Ok(()));
        assert!(fits(&checked, &crash(r + 1, json!(1), json!("a")), &chain).is_err());
        assert!(fits(&checked, &crash(r + 1, json!(5), json!(null)), &chain).is_err());
        assert!(fits(&checked, &crash(r + 3, json!(1), json!("b")), &chain).is_err());
        assert!(fits(&checked, &crash(r - 1, json!(1), json!("b")), &chain).is_err());
    }

    /// The golden results rebased under another build id are the results under it: the keys
    /// that name the build, and nothing else.
    #[test]
    fn rebasing_changes_only_the_keys_that_name_the_build() {
        let set = GoldenSet::committed().expect("the set");
        for (name, job) in &set.jobs {
            let checked = check_spec(rules(), job.kind, &job.spec, None).expect("its spec");
            let mut other = job.result.clone();
            other["engine"] = json!("ffffffffffff");
            if let Some(players) = other.get_mut("players").and_then(Value::as_object_mut) {
                for p in players.values_mut() {
                    p["build"] = json!("ffffffffffff");
                    p["fingerprint"] = json!("eeeeeeeeeeee");
                }
            }
            let back = rebased(rules(), &checked, &other).expect("rebased");
            assert_eq!(back, rebased(rules(), &checked, &job.result).expect("rebased"), "{name}");
            let mut scored = other.clone();
            if scored.get("players").is_some_and(Value::is_object) {
                scored["players"]["0"]["score"] = json!(-1);
                assert_ne!(rebased(rules(), &checked, &scored).expect("rebased"), back, "{name}");
            }
        }
        let crash = json!({"crash": "T3 P1: panic: boom"});
        let (checked, _, _) = golden("reference-a");
        assert_eq!(rebased(rules(), &checked, &crash), Ok(crash));
    }
}

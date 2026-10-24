//! `golden dump SET:GAME [TURN]` (DESIGN.md 9.6): any game of a golden set, played on this
//! machine to the end of round `TURN` (or as it starts), written as save JSON. Run on a target
//! that agrees with the committed file, it gives the counterpart of a divergent state that
//! `golden check --states` wrote on one that does not, for `golden diff` to compare.
//!
//! The games are named as their sets' rows name them, after the set: `turns:arena`,
//! `pass:<fixture>`, `random:<name>`, `bot:<name>`, `long:<name>`, and, as they start,
//! `newgame:<name>` and `load:<fixture>`. `golden dump --list` lists them.

use citar_engine::base::ids::Turn;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::save::Snapshot;

use super::games::{BOT_GAMES, LONG_GAMES, Play, pass_games, random_games};
use super::{newgame, turns};
use crate::fixtures;
use crate::games;

/// What the hook returns to stop a game at the round asked for.
const STOP: &str = "golden dump: the round asked for";

/// Every game `dump` can play, as `SET:GAME`.
///
/// # Errors
/// If the fixtures or the arena cannot be read.
pub fn names() -> Result<Vec<String>, String> {
    let mut out = vec!["turns:arena".to_owned()];
    for (set, plays) in [
        ("pass", pass_games()),
        ("random", random_games()),
        ("bot", BOT_GAMES.to_vec()),
        ("long", LONG_GAMES.to_vec()),
    ] {
        out.extend(plays.iter().map(|p| format!("{set}:{}", p.name(set))));
    }
    out.extend(newgame::games()?.into_iter().map(|(name, _, _)| format!("newgame:{name}")));
    out.extend(fixtures::committed()?.into_iter().map(|f| format!("load:{}", f.name)));
    Ok(out)
}

/// The state of game `name` (`SET:GAME`) as round `turn` ended, or as the game starts, as save
/// JSON.
///
/// # Errors
/// An unknown game, a round the game never ends, or a game that does not set up or save.
pub fn dump(name: &str, turn: Option<Turn>) -> Result<Vec<u8>, String> {
    let (set, game) = name.split_once(':').ok_or_else(|| format!("{name}: not SET:GAME"))?;
    let found = fixtures::committed()?;
    let snapshot = match set {
        "turns" if game == "arena" => {
            let mut g = turns::game()?;
            match turn {
                None => g.snapshot(),
                Some(t) => {
                    let mut got = None;
                    let problems = turns::play(&mut g, &mut |g, round, _| {
                        if round == t {
                            got = Some(g.snapshot());
                        }
                        round < t
                    });
                    got.ok_or_else(|| no_round(name, t, &problems))?
                }
            }
        }
        "pass" | "random" | "bot" | "long" => {
            let plays = match set {
                "pass" => pass_games(),
                "random" => random_games(),
                "bot" => BOT_GAMES.to_vec(),
                _ => LONG_GAMES.to_vec(),
            };
            let play = plays
                .iter()
                .find(|p| p.name(set) == game)
                .ok_or_else(|| format!("no game {game} in the {set} set"))?;
            played_to(set, play, &found, turn, name)?
        }
        "newgame" if turn.is_none() => {
            let (_, _, settings) = newgame::games()?
                .into_iter()
                .find(|(n, _, _)| n == game)
                .ok_or_else(|| format!("no game {game} in the newgame set"))?;
            newgame::new_game(&settings)?.snapshot()
        }
        "load" if turn.is_none() => {
            let f = found
                .iter()
                .find(|f| f.name == game)
                .ok_or_else(|| format!("no committed fixture {game}"))?;
            games::from_fixture(f, b"golden:load", DebugOptions::default())?.snapshot()
        }
        "newgame" | "load" => return Err(format!("{name}: a {set} game has no rounds to stop at")),
        _ => return Err(format!("{name}: no such golden game (golden dump --list)")),
    };
    snapshot.to_json().map_err(|e| format!("{name}: does not save: {e}"))
}

/// A whole-game set's game played to the end of round `turn`, or as it starts.
fn played_to(
    set: &str,
    play: &Play,
    found: &[fixtures::Fixture],
    turn: Option<Turn>,
    name: &str,
) -> Result<Snapshot, String> {
    let mut g: Game = play.start(set, found)?;
    let Some(t) = turn else { return Ok(g.snapshot()) };
    let mut got = None;
    let mut hook = |g: &mut Game, (round, _): games::Round| -> Result<(), String> {
        if round == t {
            got = Some(g.snapshot());
            return Err(STOP.to_owned());
        }
        Ok(())
    };
    let played = play.play(&mut g, &mut hook);
    match (got, played) {
        (Some(s), _) => Ok(s),
        (None, Err(e)) if e != STOP => Err(format!("{name}: {e}")),
        (None, _) => Err(no_round(name, t, &[])),
    }
}

fn no_round(name: &str, turn: Turn, problems: &[String]) -> String {
    let why = problems.first().map(|p| format!(" ({p})")).unwrap_or_default();
    format!("{name}: the game never ends round {turn}{why}")
}

//! `citar-sim play`: one headless bot game, printed as `citar sim` prints it (`citar/sim.py`).
//!
//! It is the crash check for the bot: crashes are raised, so a panic stops the run with where it
//! happened instead of passing as a quiet game (the lab and the baseline record crashes and play
//! on). A line of standings every ten turns and at the end, then the game's last headlines.

use std::sync::Arc;
use std::time::Instant;

use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::PlayerId;
use citar_engine::game::SeatDriver;
use citar_engine::rules::Ruleset;
use serde_json::{Value, json};

use crate::result::RunResult;
use crate::runner::{RoundInfo, RunSpec, SimError, run_game};

/// The events worth printing at the end of a game (`sim._HEADLINES`).
const HEADLINES: [&str; 5] = ["war_declared", "peace", "city_captured", "eliminated", "deal"];

/// How many headlines the end of a game prints.
const LAST_HEADLINES: usize = 25;

/// What to play (`citar sim`'s command line).
#[derive(Clone, Debug, PartialEq)]
pub struct PlayOptions {
    /// Major civilizations, each a bot.
    pub players: u8,
    /// The last turn; 0 for the speed's own.
    pub turns: u32,
    pub map: String,
    pub size: String,
    pub seed: u64,
    pub barbarians: String,
    pub speed: String,
    /// Every seat's nation (`BenchmarkCiv`, ...); `None` draws them at random.
    pub nation: Option<String>,
    /// The bot version: `basic` (the latest) or another of `citar_bot::versions()`.
    pub bot: String,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            players: 4,
            turns: 0,
            map: "continents".to_owned(),
            size: "small".to_owned(),
            seed: 1,
            barbarians: "normal".to_owned(),
            speed: "Quick".to_owned(),
            nation: None,
            bot: "basic".to_owned(),
        }
    }
}

impl PlayOptions {
    /// The two short duels of `--smoke`: 40 turns on a continents map and on a pangaea, seeds
    /// `seed` and `seed + 1`.
    #[must_use]
    pub fn smoke(&self) -> [Self; 2] {
        let duel = |map: &str, seed| Self {
            players: 2,
            turns: 40,
            map: map.to_owned(),
            size: "duel".to_owned(),
            seed,
            ..self.clone()
        };
        [duel("continents", self.seed), duel("pangaea", self.seed + 1)]
    }

    /// The game's configuration (`sim.run`'s).
    #[must_use]
    pub fn config(&self) -> Value {
        let seat = json!({"controller": "bot", "nation": self.nation});
        json!({
            "map_type": self.map, "map_size": self.size, "seed": self.seed,
            "barbarians": self.barbarians, "speed": self.speed,
            "players": vec![seat; usize::from(self.players)],
            "turn_limit": (self.turns > 0).then_some(self.turns),
        })
    }
}

/// Why a game could not be played.
#[derive(Debug, thiserror::Error)]
pub enum PlayError {
    /// No bot version by that name.
    #[error("'{0}' is not a bot version")]
    Bot(String),
    /// The game stopped: a crash, a refusal or a configuration that makes no game.
    #[error(transparent)]
    Sim(#[from] SimError),
}

/// Plays one game with options `o`, printing through `say` a line at a time, and returns
/// `run_game`'s result.
///
/// # Errors
/// An unknown bot version; whatever stopped the game (a crash is raised).
pub fn play(o: &PlayOptions, say: &mut dyn FnMut(&str)) -> Result<RunResult, PlayError> {
    let rules = Ruleset::shared();
    let version = VersionId::resolve(&o.bot).ok_or_else(|| PlayError::Bot(o.bot.clone()))?;
    let tuning = Arc::new(Tuning::new(version, Overrides::default()));
    // The majors are the first players of a new game.
    let drivers = (0..o.players)
        .map(|p| {
            let aggression = 0.3 + 0.2 * f64::from(p);
            let spec = BotSpec::new(version, Arc::clone(&tuning), None, Some(aggression));
            (PlayerId(p), Box::new(Bot::new(Arc::new(spec))) as Box<dyn SeatDriver>)
        })
        .collect();
    let spec = RunSpec { config: o.config(), raise_errors: true, ..RunSpec::default() };
    let t0 = Instant::now();
    let mut turn_t = Instant::now();
    let mut headlines: Vec<String> = Vec::new();
    let result = run_game(
        rules,
        spec,
        drivers,
        &mut |info: &RoundInfo| {
            let last = info.turn > 1 && (info.turn % 10 == 0 || info.phase != "playing");
            if let (true, Some(stats)) = (last, &info.last_stats) {
                let took = turn_t.elapsed().as_secs_f64();
                say(&format!("T{:>3} ({took:.1}s) {}", info.turn - 1, standings(stats)));
                turn_t = Instant::now();
            }
        },
        &mut |events| {
            for ev in events.events() {
                if HEADLINES.contains(&ev.kind.name()) {
                    headlines.push(ev.text.to_string());
                }
            }
        },
    );
    let r = result?;
    say(&format!(
        "Game over on turn {}: winner {} by {} in {:.1}s",
        r.turn,
        r.winner.map_or_else(|| "None".to_owned(), |w| w.to_string()),
        r.victory.as_deref().unwrap_or("None"),
        t0.elapsed().as_secs_f64()
    ));
    for h in &headlines[headlines.len().saturating_sub(LAST_HEADLINES)..] {
        say(&format!("   {h}"));
    }
    Ok(r)
}

/// A round's standings: score, cities, population, techs, era, military, gold, science and
/// happiness of every civilization alive.
fn standings(stats: &Value) -> String {
    let Some(players) = stats.get("players").and_then(Value::as_object) else {
        return String::new();
    };
    let n = |v: &Value, k: &str| v.get(k).map_or_else(|| "0".to_owned(), Value::to_string);
    players
        .iter()
        .filter(|(_, v)| v.get("alive").and_then(Value::as_bool).unwrap_or(false))
        .map(|(k, v)| {
            format!(
                "P{k}: sc{} c{} pop{} t{} e{} m{} g{} s{} h{}",
                n(v, "score"),
                n(v, "cities"),
                n(v, "population"),
                n(v, "techs"),
                n(v, "era"),
                n(v, "military"),
                n(v, "gold"),
                n(v, "science"),
                n(v, "happiness")
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standings_list_the_living_as_citar_sim_did() {
        let stats = json!({"turn": 10, "players": {
            "0": {"alive": true, "score": 30, "cities": 1, "population": 2, "techs": 2, "era": 0,
                  "military": 13, "gold": 40, "science": 3.5, "happiness": 4},
            "1": {"alive": false, "score": 0}}});
        assert_eq!(standings(&stats), "P0: sc30 c1 pop2 t2 e0 m13 g40 s3.5 h4");
    }

    #[test]
    fn the_smoke_duels_are_short_and_on_two_maps() {
        let [a, b] = PlayOptions { seed: 7, ..PlayOptions::default() }.smoke();
        assert_eq!(
            (a.players, a.turns, a.size.as_str(), a.map.as_str(), a.seed),
            (2, 40, "duel", "continents", 7)
        );
        assert_eq!((b.map.as_str(), b.seed), ("pangaea", 8));
        assert_eq!(a.config()["turn_limit"], json!(40));
        assert_eq!(PlayOptions::default().config()["turn_limit"], Value::Null);
    }

    #[test]
    fn an_idle_duel_plays_to_its_end_and_prints_how_it_went() {
        let o = PlayOptions { bot: "idle".into(), ..PlayOptions::default() }.smoke()[0].clone();
        let o = PlayOptions { turns: 20, ..o };
        let mut out = Vec::new();
        let r = play(&o, &mut |l| out.push(l.to_owned())).expect("plays");
        assert_eq!((r.turns, r.phase), (20, "over"));
        // Turn 10 begins: the line is the turn just played, as `citar sim` printed it.
        assert!(out.iter().any(|l| l.starts_with("T  9 (")), "{out:?}");
        assert!(out.iter().any(|l| l.starts_with("Game over on turn 21: winner ")), "{out:?}");
        let bad = PlayOptions { bot: "frozen_1".into(), ..o };
        assert!(matches!(play(&bad, &mut |_| {}), Err(PlayError::Bot(_))));
    }
}

//! `citar-sim`, the developer's runner (DESIGN.md P2.4.1):
//!
//! ```text
//! citar-sim play [--smoke] [options]       a game, printed as `citar sim` prints it
//! citar-sim baseline [--smoke] [options]   the statistical baseline, as scripts/refcheck/baseline.py
//! ```
//!
//! The wheel exposes the same runner through `engine_api.run_game`. Exit codes: `play` 0, or 1
//! when the game stopped (a crash, a refusal); `baseline` 0 when every game finished, 1 when
//! some crashed or the run stopped early (run it again to resume), 2 when it could not start.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use citar_sim::baseline::{self, MAP_TYPES, Options};
use citar_sim::panics;
use citar_sim::play::{self, PlayOptions};
use clap::{Args, Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "citar-sim", about = "Headless CITAR games: a game, or the statistical baseline")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Plays a game with a bot in every seat and prints how it went (`citar sim`).
    Play(PlayArgs),
    /// Plays the statistical baseline, one JSON line per game (scripts/refcheck/baseline.py).
    Baseline(BaselineArgs),
}

#[derive(Args)]
struct PlayArgs {
    /// Two short duel games (40 turns) instead.
    #[arg(long)]
    smoke: bool,
    /// Major civilizations, each a bot.
    #[arg(long, default_value_t = 4)]
    players: u8,
    /// The last turn; 0 for the speed's own.
    #[arg(long, default_value_t = 0)]
    turns: u32,
    #[arg(long, default_value = "continents")]
    map: String,
    #[arg(long, default_value = "small")]
    size: String,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    #[arg(long, default_value = "normal")]
    barbarians: String,
    #[arg(long, default_value = "Quick")]
    speed: String,
    /// Every seat's nation, e.g. BenchmarkCiv (default: drawn at random).
    #[arg(long)]
    nation: Option<String>,
    /// The bot version: basic (the latest), basic-1 or idle.
    #[arg(long, default_value = "basic")]
    bot: String,
}

#[derive(Args)]
struct BaselineArgs {
    /// Two short duel games (40 turns) into <name>-smoke, made afresh every time.
    #[arg(long)]
    smoke: bool,
    #[arg(long, default_value_t = 100)]
    games: u32,
    /// The seed of game 0; game i uses seed + i.
    #[arg(long, default_value_t = 5000)]
    seed: u64,
    /// Map sizes, rotated (comma-separated).
    #[arg(long, value_delimiter = ',', default_value = "small")]
    sizes: Vec<String>,
    /// Map types, rotated (comma-separated).
    #[arg(long, value_delimiter = ',', default_values_t = MAP_TYPES.map(String::from))]
    maps: Vec<String>,
    /// Barbarian settings, rotated (comma-separated).
    #[arg(long, value_delimiter = ',', default_value = "normal")]
    barbarians: Vec<String>,
    #[arg(long, default_value = "Quick")]
    speed: String,
    /// The last turn; 0 for the speed's own (330 on Quick).
    #[arg(long, default_value_t = 0)]
    turn_limit: u32,
    /// Stop a game that runs longer (default: 60 on a small map, scaled by map area).
    #[arg(long, default_value_t = 0.0)]
    max_minutes: f64,
    /// The output file's name (default rust-<build id>-<bot>).
    #[arg(long)]
    name: Option<String>,
    /// Games played at once (default: all cores but four).
    #[arg(long)]
    workers: Option<usize>,
    /// Where the file goes (default: the repository's refcheck/baseline).
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Run the engine's invariants at every settle; a game that breaks one is a crash line.
    #[arg(long)]
    checks: bool,
    /// Parameter overrides for every bot, as JSON, or @file for a JSON file (a profile's).
    #[arg(long)]
    params: Option<String>,
}

fn main() -> ExitCode {
    panics::install();
    let cli = Cli::parse();
    match cli.command {
        Command::Play(a) => play_command(a),
        Command::Baseline(a) => baseline_command(a),
    }
}

fn play_command(a: PlayArgs) -> ExitCode {
    let o = PlayOptions {
        players: a.players,
        turns: a.turns,
        map: a.map,
        size: a.size,
        seed: a.seed,
        barbarians: a.barbarians,
        speed: a.speed,
        nation: a.nation,
        bot: a.bot,
    };
    let games = if a.smoke { o.smoke().to_vec() } else { vec![o] };
    for g in &games {
        if let Err(e) = play::play(g, &mut |l| println!("{l}")) {
            eprintln!("citar-sim play: {e}");
            if let play::PlayError::Sim(citar_sim::SimError::Crashed(c)) = &e
                && !c.trace.is_empty()
            {
                eprintln!("{}", c.trace);
            }
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

fn baseline_command(a: BaselineArgs) -> ExitCode {
    let params = match a.params.as_deref().map(read_params).transpose() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("citar-sim baseline: --params: {e}");
            return ExitCode::from(2);
        }
    };
    let defaults = Options::default();
    let o = Options {
        smoke: a.smoke,
        games: a.games,
        seed: a.seed,
        sizes: a.sizes,
        maps: a.maps,
        barbarians: a.barbarians,
        speed: a.speed,
        turn_limit: a.turn_limit,
        max_minutes: a.max_minutes,
        name: a.name,
        workers: a.workers.unwrap_or(defaults.workers).max(1),
        dir: a.dir.unwrap_or(defaults.dir),
        checks: a.checks,
        params,
    };
    let outcome = baseline::run(&o, &mut |l| println!("{l}"));
    ExitCode::from(outcome.code())
}

/// `--params`: JSON, or `@path` for a file of it.
fn read_params(arg: &str) -> Result<Value, String> {
    let text = match arg.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
        None => arg.to_owned(),
    };
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

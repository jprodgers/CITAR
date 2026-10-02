//! `citar-sim`, the developer's runner (DESIGN.md P2.4.1):
//!
//! ```text
//! citar-sim play [--smoke]        a game, printed as `citar sim` prints it
//! citar-sim baseline [--smoke]    the statistical baseline, as scripts/refcheck/baseline.py
//! ```
//!
//! Package 2-00a registered the commands; package 2-04 writes them. The wheel exposes the same
//! runner through `engine_api.run_game`.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "citar-sim", about = "Headless CITAR games: a game, or the statistical baseline")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Plays a game with a bot in every seat and prints how it went.
    Play {
        /// Two short duel games instead.
        #[arg(long)]
        smoke: bool,
    },
    /// Plays the statistical baseline, one JSON line per game.
    Baseline {
        /// Two short duel games, to prove the pipeline works.
        #[arg(long)]
        smoke: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let what = match cli.command {
        Command::Play { .. } => "play",
        Command::Baseline { .. } => "baseline",
    };
    eprintln!("citar-sim {what}: not built yet (package 2-04)");
    ExitCode::from(2)
}

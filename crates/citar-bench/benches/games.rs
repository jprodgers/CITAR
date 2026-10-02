//! The game benchmarks (DESIGN.md P2.4.2, P2.10): whole bot games through the runner,
//! single-threaded, in the bench profile, pinned to one core.
//!
//! - `game/bot_small_330`: the small 4-bot Quick game of seeds 5000-5002 to 330 rounds, the
//!   median of the three. Budget 16 s, hard limit 24 s, the plan's floor 25 s, target 5 s.
//! - `game/bot_gargantuan_330`: one gargantuan 24-bot game of seed 5000. Budget 180 s, hard
//!   limit 270 s, floor 279 s, target 90 s.
//!
//! Report-only until package 2-07. Timed runs pause the other lane (the orchestrator's job) and
//! record the machine's load.
//!
//! ```text
//! cargo bench -p citar-bench --bench games
//! cargo xtask perf --suite games
//! ```
//!
//! Package 2-00a registered the target, which writes an empty `<target>/perf/games.json`;
//! package 2-04 plays the games.

use citar_bench::Suite;

fn main() {
    let s = Suite::start("games");
    s.finish();
}

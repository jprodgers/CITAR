//! The game benchmarks (DESIGN.md P2.4.2, P2.10): whole bot games through citar-sim's runner,
//! single-threaded, in the bench profile, pinned to one core.
//!
//! - `game/bot_small_330`: the small 4-bot Quick games of seeds 5000-5002 to 330 rounds (the
//!   baseline's games 0-2: continents, pangaea and archipelago, barbarians normal), the median
//!   of the three. Budget 16 s, hard limit 24 s, the plan's floor 25 s, target 5 s.
//! - `game/bot_gargantuan_330`: one gargantuan 24-bot Quick game of seed 5000 (continents,
//!   barbarians normal). Budget 180 s, hard limit 270 s, floor 279 s, target 90 s.
//!
//! Each game is timed from its creation to its end, as a host plays it and as Python's baseline
//! timed its games, with every check off ([`citar_bench::unchecked`]'s setting) and a `basic-1`
//! bot in every major's seat, its aggression spread by seat and seed as the baseline's. Each
//! game's own time is noted beside the measure (`game/bot_small_330/5000`, ...), with how many
//! turns it played and the share of the wall time its thread had the CPU: a share well under 1
//! means the machine was busy and the time is not to be trusted.
//!
//! Report-only until package 2-07. Timed runs pause the other lane (the orchestrator's job).
//!
//! ```text
//! cargo bench -p citar-bench --bench games                    # both
//! cargo bench -p citar-bench --bench games -- game/bot_small  # one
//! cargo xtask perf --suite games
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use citar_bench::Suite;
use citar_bot::{Overrides, Tuning, VersionId};
use citar_engine::game::DebugOptions;
use citar_engine::rules::Ruleset;
use citar_sim::baseline::{GameSpec, Options, bot_seats, game_spec};
use citar_sim::{RunSpec, Runner};
use cpu_time::ThreadTime;

/// The stack of the thread a game plays on: as the baseline's workers have.
const GAME_STACK: usize = 16 << 20;

/// How one game went.
struct Played {
    took: Duration,
    cpu: Duration,
    /// The core its thread was pinned to, if it was.
    core: Option<usize>,
    turns: i32,
    victory: Option<String>,
}

/// Plays game `spec` of the baseline to its end on a thread of its own, pinned as the suite is.
fn play(spec: GameSpec) -> Played {
    let seed = spec.seed;
    std::thread::Builder::new()
        .name(format!("game-{seed}"))
        .stack_size(GAME_STACK)
        .spawn(move || {
            let core = citar_bench::pin();
            let rules = Ruleset::shared();
            let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
            let run = RunSpec {
                config: spec.config(rules),
                raise_errors: true,
                debug: Some(DebugOptions::OFF),
                ..RunSpec::default()
            };
            let cpu = ThreadTime::try_now().ok();
            let t = Instant::now();
            let mut r = Runner::new_with(rules, run, |g| {
                bot_seats(g, spec.seed, VersionId::Basic1, &tuning)
            })
            .unwrap_or_else(|e| panic!("seed {}: {e}", spec.seed));
            while !r.is_over() {
                r.step().unwrap_or_else(|e| panic!("seed {}: {e}", spec.seed));
            }
            let took = t.elapsed();
            let cpu = cpu.and_then(|c| c.try_elapsed().ok()).unwrap_or_default();
            let result = r.result();
            assert_eq!(result.phase, "over", "seed {}: the game ends", spec.seed);
            Played { took, cpu, core, turns: result.turns, victory: result.victory }
        })
        .expect("a thread for the game")
        .join()
        .unwrap_or_else(|_| panic!("the game of seed {seed} panicked"))
}

/// Plays `spec`, prints and notes how it went, and returns its time.
fn timed(s: &mut Suite, id: &str, spec: GameSpec) -> Duration {
    let (size, map, seed) = (spec.size.clone(), spec.map_type.clone(), spec.seed);
    let p = play(spec);
    let share = p.cpu.as_secs_f64() / p.took.as_secs_f64().max(1e-9);
    let core = p.core.map_or_else(|| "not pinned".to_owned(), |c| format!("core {c}"));
    println!(
        "{id} seed {seed} ({size}, {map}): {} turns, {} in {} ({core}, CPU share {share:.2})",
        p.turns,
        p.victory.as_deref().unwrap_or("no victory"),
        citar_bench::show(p.took)
    );
    s.note(&format!("{id}/{seed}"), p.took);
    p.took
}

fn main() {
    let mut s = Suite::start("games");
    let rules = Ruleset::shared();
    // The baseline's defaults: small maps, the five map types in turn, barbarians normal, Quick.
    let small = Options::default();

    if s.wants("game/bot_small_330") {
        let id = "game/bot_small_330";
        let mut times: Vec<Duration> =
            (0..3).map(|i| timed(&mut s, id, game_spec(rules, &small, i))).collect();
        times.sort();
        s.put(id, times[1]);
    }

    if s.wants("game/bot_gargantuan_330") {
        let id = "game/bot_gargantuan_330";
        let huge = Options { sizes: vec!["gargantuan".to_owned()], ..small };
        let took = timed(&mut s, id, game_spec(rules, &huge, 0));
        s.put(id, took);
    }

    s.finish();
}

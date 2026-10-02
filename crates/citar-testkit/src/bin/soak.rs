//! `cargo soak`: whole `RandomAgent` games on every map size with the invariants on, reporting
//! panics, violations, turn-time outliers and peak memory (DESIGN.md 9.5; `citar_testkit::soak`).
//!
//! ```text
//! cargo soak [--games N] [--seconds S] [--seed S] [--sizes a,b] [--max-rounds R]
//!            [--oracle-every R] [--save-every R] [--outlier-factor F] [--outlier-floor-ms MS]
//!            [--shard K/N] [--game I] [--json FILE]
//!                     play the run's first N games (12) from seed S (1), every size in turn,
//!                     or only the sizes named; each to its turn limit, capped at R rounds; the
//!                     cache oracle every R rounds (50) and a save and load every R (25), both
//!                     also at each game's end; a round over F times its game's median (8) and
//!                     over MS ms (50) is an outlier; start no game after S seconds; play only
//!                     the laps of the sizes whose number is K modulo N, or only game I; write
//!                     the report to FILE as JSON
//! ```
//!
//! Exit codes: 0 every game clean, 1 a game panicked or failed a check, 2 a usage or I/O error.
//! Outliers and memory are reported, never a failure.
//!
//! Peak memory is the most heap the game held, counted by this binary's allocator (a thin
//! wrapper of the system allocator that keeps a running total and its high-water mark), so it
//! is the same measure on every OS; on Linux the process's peak resident set is printed too.

#![deny(unsafe_code)]
// A command-line tool: it reports on the console, reads the clock and writes its report.
#![allow(
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "a command-line tool that prints its report, keeps to a time budget and writes a file"
)]

use std::process::ExitCode;
use std::time::{Duration, Instant};

use citar_testkit::soak::{self, GameReport, Probe, Settings, mib};

/// The heap high-water mark: the system allocator, counting.
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static HELD: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    pub struct Counting;

    fn grew(by: usize) {
        let now = HELD.fetch_add(by, Ordering::Relaxed) + by;
        PEAK.fetch_max(now, Ordering::Relaxed);
    }

    fn shrank(by: usize) {
        HELD.fetch_sub(by, Ordering::Relaxed);
    }

    // SAFETY: every call goes to the system allocator with the caller's own arguments, and the
    // counts beside it touch no memory the allocator hands out.
    #[allow(unsafe_code, reason = "a global allocator is an unsafe trait; this one only counts")]
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the caller's layout, passed on.
            let p = unsafe { System.alloc(layout) };
            if !p.is_null() {
                grew(layout.size());
            }
            p
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the caller's layout, passed on.
            let p = unsafe { System.alloc_zeroed(layout) };
            if !p.is_null() {
                grew(layout.size());
            }
            p
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: the caller's block, allocated by `System` with this layout.
            unsafe { System.dealloc(ptr, layout) };
            shrank(layout.size());
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: the caller's block and sizes, passed on.
            let p = unsafe { System.realloc(ptr, layout, new_size) };
            if !p.is_null() {
                grew(new_size);
                shrank(layout.size());
            }
            p
        }
    }

    /// Starts a new high-water mark from what is held now.
    pub fn reset() {
        PEAK.store(HELD.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    /// The high-water mark since the last reset, in bytes.
    pub fn peak() -> usize {
        PEAK.load(Ordering::Relaxed)
    }
}

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting;

const USAGE: &str = "usage: soak [--games N] [--seconds S] [--seed S] [--sizes a,b] \
                     [--max-rounds R] [--oracle-every R] [--save-every R] [--outlier-factor F] \
                     [--outlier-floor-ms MS] [--shard K/N] [--game I] [--json FILE]";

struct Clock {
    started: Instant,
    budget: Option<Duration>,
}

impl Probe for Clock {
    fn now_ns(&mut self) -> u64 {
        u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn reset_peak(&mut self) {
        counting::reset();
    }

    fn peak_bytes(&mut self) -> Option<u64> {
        u64::try_from(counting::peak()).ok()
    }

    fn keep_going(&mut self) -> bool {
        self.budget.is_none_or(|b| self.started.elapsed() < b)
    }
}

struct Command {
    settings: Settings,
    seconds: Option<u64>,
    json: Option<String>,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let mut settings = Settings::default();
    let mut seconds = None;
    let mut json = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let value = it.next().cloned().ok_or(format!("{a} needs a value"));
        let int = |v: Result<String, String>| -> Result<u64, String> {
            let v = v?;
            v.parse::<u64>().map_err(|_| format!("{a} takes a number, not {v}"))
        };
        let small = |v: Result<String, String>| -> Result<u32, String> {
            u32::try_from(int(v)?).map_err(|e| format!("{a}: {e}"))
        };
        let real = |v: Result<String, String>| -> Result<f64, String> {
            let v = v?;
            v.parse::<f64>().ok().filter(|x| x.is_finite()).ok_or(format!("{a} takes a number"))
        };
        match a.as_str() {
            "--games" => settings.games = small(value)?,
            "--seconds" => seconds = Some(int(value)?),
            "--seed" => settings.seed = int(value)?,
            "--sizes" => {
                settings.sizes = value?.split(',').map(|s| s.trim().to_owned()).collect();
            }
            "--max-rounds" => settings.max_rounds = Some(small(value)?),
            "--oracle-every" => settings.oracle_every = small(value)?,
            "--save-every" => settings.save_every = small(value)?,
            "--outlier-factor" => settings.outlier_factor = real(value)?,
            "--outlier-floor-ms" => settings.outlier_floor_ms = real(value)?,
            "--shard" => {
                let v = value?;
                let (k, n) = v.split_once('/').ok_or(format!("--shard takes K/N, not {v}"))?;
                let k = k.parse::<u32>().map_err(|_| format!("--shard takes K/N, not {v}"))?;
                let n = n.parse::<u32>().map_err(|_| format!("--shard takes K/N, not {v}"))?;
                settings.shard = (k, n);
            }
            "--game" => settings.only = Some(small(value)?),
            "--json" => json = Some(value?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Command { settings, seconds, json })
}

/// One line for a game as it ends.
fn line(r: &GameReport) -> String {
    let status = match (&r.panic, r.failures.len()) {
        (Some(_), _) => "PANICKED".to_owned(),
        (None, 0) => "clean".to_owned(),
        (None, n) => format!("{n} FAILURES"),
    };
    format!(
        "soak: {:<44} {:>3} rounds to turn {:>3}{} in {:>6.1} s; median round {:>6.1} ms, \
         slowest {:>7.1} ms; peak {}; {status}",
        r.game.label(),
        r.rounds,
        r.turn,
        if r.over { " (over)" } else { "" },
        r.seconds,
        r.median_round_ms,
        r.max_round_ms,
        r.peak_bytes.map_or_else(|| "not counted".to_owned(), mib),
    )
}

/// The process's peak resident set, where the OS says it without unsafe code (Linux).
fn peak_resident() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    Some(line.trim_start_matches("VmHWM:").trim().to_owned())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("soak: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let s = &cmd.settings;
    let mut clock = Clock { started: Instant::now(), budget: cmd.seconds.map(Duration::from_secs) };
    println!(
        "soak: seed {}, {} games{}{}, the oracle every {} rounds, a save every {}",
        s.seed,
        s.games,
        s.max_rounds.map(|r| format!(", at most {r} rounds each")).unwrap_or_default(),
        if s.shard.1 > 1 {
            format!(", shard {} of {}", s.shard.0, s.shard.1)
        } else {
            String::new()
        },
        s.oracle_every,
        s.save_every,
    );
    let report = match soak::run(s, &mut clock, &mut |r| println!("{}", line(r))) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("soak: {e}");
            return ExitCode::from(2);
        }
    };
    for l in report.by_size() {
        println!("soak: {l}");
    }
    let mut slowest: Vec<(&GameReport, &soak::Outlier)> =
        report.games.iter().flat_map(|g| g.outliers.iter().map(move |o| (g, o))).collect();
    slowest.sort_by(|a, b| b.1.ms.total_cmp(&a.1.ms));
    for (g, o) in slowest.iter().take(10) {
        println!(
            "soak: outlier: {} turn {}: {:.1} ms against a median of {:.1} ms",
            g.game.label(),
            o.turn,
            o.ms,
            o.median_ms
        );
    }
    if let Some(rss) = peak_resident() {
        println!("soak: the process's peak resident set: {rss}");
    }
    if let Some(path) = &cmd.json
        && let Err(e) = std::fs::write(path, report.to_json())
    {
        eprintln!("soak: cannot write {path}: {e}");
        return ExitCode::from(2);
    }
    let failed: Vec<&GameReport> = report.failed().collect();
    let rounds: u32 = report.games.iter().map(|g| g.rounds).sum();
    println!(
        "soak: {} games, {rounds} rounds in {:.0} s: {} failed",
        report.games.len(),
        clock.started.elapsed().as_secs_f64(),
        failed.len()
    );
    if failed.is_empty() {
        return ExitCode::SUCCESS;
    }
    for g in failed {
        println!(
            "soak: FAILED {} (play it again: soak --seed {} --game {})",
            g.game.label(),
            report.seed,
            g.game.index
        );
        if let Some(p) = &g.panic {
            println!("    panic: {p}");
        }
        for f in g.failures.iter().take(20) {
            println!("    {f}");
        }
        if g.failures.len() > 20 {
            println!("    ... and {} more", g.failures.len() - 20);
        }
    }
    ExitCode::from(1)
}

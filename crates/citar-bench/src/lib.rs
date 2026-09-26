//! Benchmarks of the CITAR engine, kept out of the engine's manifest (DESIGN.md 2.1, 9.7, 10).
//!
//! Four bench targets:
//! - `kernels`: the hot algorithms one at a time (tile yields, stats, citizens, indexes, sight,
//!   paths, combat, buildable lists, job maps, religion, the advisor, barbarians, map
//!   generation, hexes);
//! - `turns`: the macro benchmarks, pass rounds on every corpus state with their ratio to
//!   Python's (`refcheck/perf/python-turns.json`), the client views and a whole game;
//! - `io`: the digest, the snapshot under a host's lock, the save, the load and the ruleset;
//! - `iai`: gungraun instruction counts of eight kernels and two macro benchmarks, the per-PR
//!   gate of `rust.yml` (Linux, valgrind).
//!
//! Each criterion suite pins itself to one core ([`pin`]), times its own medians after Criterion
//! and records them ([`Suite::put`]); [`Suite::finish`] writes them to
//! `<target>/perf/<suite>.json` and fails the run on any above its hard limit, 1.5 times its
//! budget in `thresholds.toml`. `cargo xtask perf` (perfgate) runs the suites and checks every
//! budget of the file against what they wrote, with the pass rounds' Python ratios.

#![forbid(unsafe_code)]

pub mod fixtures;
pub mod thresholds;

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use criterion::Criterion;
use serde::Serialize;

pub use thresholds::Thresholds;

/// The environment variable naming the logical processor the suites pin themselves to (0, a
/// performance core of the laptop, by default); `none` leaves the thread unpinned.
pub const CORE_ENV: &str = "CITAR_BENCH_CORE";

/// Pins the calling thread to one logical processor (DESIGN.md 9.7: core 0, a P-core of the
/// laptop's i5-13420H, unless [`CORE_ENV`] names another). Returns the processor, or `None` when
/// the platform refused or pinning is off.
#[must_use]
pub fn pin() -> Option<usize> {
    let want = std::env::var(CORE_ENV).unwrap_or_default();
    if want.eq_ignore_ascii_case("none") {
        return None;
    }
    let id = want.parse::<usize>().unwrap_or(0);
    let core = core_affinity::get_core_ids()?.into_iter().find(|c| c.id == id)?;
    core_affinity::set_for_current(core).then_some(id)
}

/// The median of `n` timings of `batch` calls of `f`, as the time of one call.
pub fn median(n: usize, batch: u32, mut f: impl FnMut()) -> Duration {
    median_timed(n, || {
        let t = Instant::now();
        for _ in 0..batch {
            f();
        }
        t.elapsed() / batch.max(1)
    })
}

/// The median of `n` values of `f`, each a time `f` took itself.
pub fn median_timed(n: usize, mut f: impl FnMut() -> Duration) -> Duration {
    let mut times: Vec<Duration> = (0..n.max(1)).map(|_| f()).collect();
    times.sort();
    times[times.len() / 2]
}

/// The median of `n` timings of `f`, each on a fresh copy of `x` (the copy is not timed).
pub fn median_on_copies<T: Clone>(x: &T, n: usize, mut f: impl FnMut(&mut T)) -> Duration {
    median_timed(n, || {
        let mut copy = x.clone();
        let t = Instant::now();
        f(&mut copy);
        let took = t.elapsed();
        black_box(copy);
        took
    })
}

/// A duration as a short text with its unit.
#[must_use]
pub fn show(d: Duration) -> String {
    thresholds::show_ns(d.as_secs_f64() * 1e9)
}

/// One measure a suite recorded.
#[derive(Clone, Debug, Serialize)]
pub struct Measure {
    /// The median, in nanoseconds.
    pub ns: f64,
    /// Whether a budget of `thresholds.toml` holds it (false: printed for the record only).
    pub gated: bool,
}

/// What a suite writes to `<target>/perf/<suite>.json` for perfgate.
#[derive(Debug, Serialize)]
struct Written<'a> {
    format: u32,
    suite: &'a str,
    /// The logical processor the suite ran on, if pinned.
    core: Option<usize>,
    /// Whether the corpus was there (`CITAR_REFCHECK_CORPUS`): the budgets that need it are
    /// skipped without it.
    corpus: bool,
    /// Whether the build checks integer overflow (the release profile's setting).
    overflow_checks: bool,
    measures: &'a BTreeMap<String, Measure>,
}

/// A criterion suite under way: its core, its filter, its budgets and what it measured.
pub struct Suite {
    name: &'static str,
    core: Option<usize>,
    filter: Vec<String>,
    thresholds: Thresholds,
    measures: BTreeMap<String, Measure>,
    over: Vec<String>,
}

impl Suite {
    /// Starts suite `name`: pins the thread, reads `thresholds.toml` and the filter criterion is
    /// given on the command line (`cargo bench --bench kernels -- advisor` runs only the parts
    /// whose name holds `advisor`).
    ///
    /// # Panics
    ///
    /// If `thresholds.toml` does not read.
    #[must_use]
    pub fn start(name: &'static str) -> Self {
        let core = pin();
        let thresholds = Thresholds::load().unwrap_or_else(|e| panic!("thresholds.toml: {e}"));
        let filter = filter_args();
        println!(
            "suite {name}: {}, corpus {}",
            core.map_or_else(|| "not pinned".to_owned(), |c| format!("pinned to core {c}")),
            if fixtures::has_corpus() { "found" } else { "absent" }
        );
        Self { name, core, filter, thresholds, measures: BTreeMap::new(), over: Vec::new() }
    }

    /// Criterion, configured from the command line.
    #[must_use]
    pub fn criterion(&self) -> Criterion {
        Criterion::default().configure_from_args()
    }

    /// Whether the part named `part` runs: no filter was given, or a filter is in its name or
    /// its name in a filter (`advisor` runs for `advisor/call_per_city` as for `adv`).
    #[must_use]
    pub fn wants(&self, part: &str) -> bool {
        self.filter.is_empty()
            || self.filter.iter().any(|f| part.contains(f.as_str()) || f.contains(part))
    }

    /// The budgets.
    #[must_use]
    pub const fn thresholds(&self) -> &Thresholds {
        &self.thresholds
    }

    /// Records measure `id`, printed against its budget in `thresholds.toml`; one above its hard
    /// limit fails the run at [`finish`](Self::finish).
    ///
    /// # Panics
    ///
    /// If `thresholds.toml` has no budget named `id`: every gated measure has one.
    pub fn put(&mut self, id: &str, took: Duration) {
        let b = self
            .thresholds
            .budget(id)
            .unwrap_or_else(|| panic!("thresholds.toml has no budget for {id}"));
        let ns = took.as_secs_f64() * 1e9;
        let hard = b.ns * self.thresholds.hard;
        let verdict = if ns > hard {
            self.over.push(format!("{id}: {} over its hard limit {}", show(took), b.text()));
            "OVER THE HARD LIMIT"
        } else if ns > b.ns {
            "over the budget"
        } else {
            "ok"
        };
        println!(
            "{id} median: {} (budget {}, hard limit {}): {verdict}",
            show(took),
            b.text(),
            thresholds::show_ns(hard)
        );
        self.measures.insert(id.to_owned(), Measure { ns, gated: true });
    }

    /// Records measure `id` for the record only, with no budget.
    pub fn note(&mut self, id: &str, took: Duration) {
        println!("{id} median: {} (report-only)", show(took));
        self.measures.insert(id.to_owned(), Measure { ns: took.as_secs_f64() * 1e9, gated: false });
    }

    /// Records a measure checked elsewhere (a pass round, which perfgate holds to Python's).
    pub fn record(&mut self, id: &str, took: Duration) {
        self.measures.insert(id.to_owned(), Measure { ns: took.as_secs_f64() * 1e9, gated: true });
    }

    /// Writes what the suite measured to `<target>/perf/<suite>.json`, merged with what an
    /// earlier run of other parts wrote there, and fails if a measure is over its hard limit.
    ///
    /// # Panics
    ///
    /// On a measure above its hard limit, or if the file cannot be written.
    pub fn finish(self) {
        let path = results_dir().join(format!("{}.json", self.name));
        let mut all = read_measures(&path);
        all.extend(self.measures.clone());
        let doc = Written {
            format: 1,
            suite: self.name,
            core: self.core,
            corpus: fixtures::has_corpus(),
            overflow_checks: overflow_checks(),
            measures: &all,
        };
        let text = serde_json::to_string_pretty(&doc).expect("measures serialise");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        }
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        println!("suite {}: {} measures written to {}", self.name, all.len(), path.display());
        assert!(self.over.is_empty(), "over the hard limit:\n{}", self.over.join("\n"));
    }
}

/// Whether this build checks integer overflow (the profile's `overflow-checks`: off in release
/// and the bench profile since package 1e-03 measured their cost, on in the ci profile): an
/// addition that overflows panics.
#[must_use]
pub fn overflow_checks() -> bool {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(|| black_box(u8::MAX) + black_box(1u8));
    std::panic::set_hook(hook);
    r.is_err()
}

/// The measures an earlier run wrote to `path`, if any: a filtered run keeps the others.
fn read_measures(path: &std::path::Path) -> BTreeMap<String, Measure> {
    #[derive(serde::Deserialize)]
    struct Read {
        measures: BTreeMap<String, ReadMeasure>,
    }
    #[derive(serde::Deserialize)]
    struct ReadMeasure {
        ns: f64,
        gated: bool,
    }
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Read>(&b).ok())
        .map(|r| {
            r.measures.into_iter().map(|(k, m)| (k, Measure { ns: m.ns, gated: m.gated })).collect()
        })
        .unwrap_or_default()
}

/// `<target>/perf`: the bench binary runs from `<target>/<profile>/deps/`.
#[must_use]
pub fn results_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("the bench's own path");
    exe.ancestors().nth(3).map_or_else(|| PathBuf::from("perf"), |t| t.join("perf"))
}

/// The filters on the command line: the arguments criterion takes as its filter (not an option,
/// nor an option's value).
fn filter_args() -> Vec<String> {
    const WITH_VALUE: [&str; 12] = [
        "--save-baseline",
        "--baseline",
        "--baseline-lenient",
        "--load-baseline",
        "--sample-size",
        "--warm-up-time",
        "--measurement-time",
        "--nresamples",
        "--noise-threshold",
        "--confidence-level",
        "--significance-level",
        "--profile-time",
    ];
    let mut out = Vec::new();
    let mut skip = false;
    for a in std::env::args().skip(1) {
        if skip {
            skip = false;
            continue;
        }
        if WITH_VALUE.contains(&a.as_str()) || a == "--color" || a == "--format" {
            skip = true;
        } else if !a.starts_with('-') {
            out.push(a);
        }
    }
    out
}

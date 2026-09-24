//! The golden sets (DESIGN.md 9.6): answers that must come out identical on all five targets,
//! and equal to the files committed in `crates/citar-testkit/golden/`.
//!
//! Package 1a-02 brings the first three:
//! - **`rng.json`**: the purposes and their frozen discriminants, the first 32 values of 8 keyed
//!   streams, draws from every distribution, and chi-square statistics of `below` and `unit`,
//!   which must also stay under their critical values. Written by `golden bless`.
//! - **`libm.json`**: about 2,000 inputs to the maths wrappers of `base::num`, with the output
//!   bits. Written by `golden bless`; `check` recomputes the outputs for the committed inputs.
//! - **`pyfmt.json`**: Python's `repr(x)`, `round(x)`, `repr(round(x, n))`, `format(x, ".nf")`,
//!   `//` and `%`, recorded by `scripts/refcheck/pyfmt_vectors.py`. Never blessed: the engine
//!   must reproduce Python, every string and every bit.
//!
//! Package 1a-03 adds:
//! - **`ruleset.json`**: the `RulesetId` of the embedded ruleset, a blake3 over a canonical walk
//!   of the parsed files, with the version string saves record and the table counts. Written by
//!   `golden bless` when the ruleset data changes.
//!
//! Package 1a-05 adds:
//! - **`uniques.json`**: every compiled unique of the embedded ruleset, one row each (its source,
//!   text, type, role, flags, parameters, modifiers and key); every source object's partitions
//!   and tags, one row each; and the fractions, stats, static filters, object filters, tags and
//!   abilities they refer to. Written by `golden bless` when the ruleset data or the compiler
//!   changes; a diff shows exactly which uniques, or which partitions, moved.
//!
//! Package 1a-06 adds, in [`filters`]:
//! - **`filters.json`**: every dynamic filter of the embedded ruleset, compiled to its tree;
//! - **`gen.json`**: the tables map generation, the AI and victory read, and every
//!   map-generation unique they hold.
//!
//! Package 1a-09 adds, in [`states`]:
//! - **`states.json`**: the digests of three states checked in under `testdata/states/`, which
//!   pin `CANON_V1` and the save format on every target. Written by `golden bless`; the states
//!   themselves only by `golden states`.
//!
//! Package 1a-10 adds, in [`convert`]:
//! - **`convert.json`**: the digests of the twelve committed refcheck fixtures right after the
//!   Python-state converter reads them, before any settle. Written by `golden bless`.
//!
//! Each set's report carries a blake3 of the answers this build computed. The determinism
//! workflow compares those across targets (a determinism bug if they differ) and the problems
//! against the committed files (a behaviour change if the targets agree with each other but not
//! with the file).

use std::path::PathBuf;

use citar_engine::base::fmt::{PyFloat, PyRound};
use citar_engine::base::ids::{AbilityKey, TagId, UniqueId};
use citar_engine::base::num::{self, FloorDiv};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::base::sets::TagSet;
use citar_engine::base::stats::Stats;
use citar_engine::rules::{Ruleset, embedded};
use citar_engine::unique::params::{Param, ParamValue};
use citar_engine::unique::{Source, SourceUniques};
use serde_json::{Value, json};

/// Where the committed golden files live.
#[must_use]
pub fn golden_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/golden"))
}

/// One golden set, checked.
#[derive(Clone, Debug)]
pub struct SetReport {
    /// The set's name, which is also its file name without `.json`.
    pub name: &'static str,
    /// blake3 of the answers this build computed, as hex: equal on every target.
    pub computed: String,
    /// What differs from the committed file, or fails a bound; empty when the set passes.
    pub problems: Vec<String>,
}

pub mod convert;
pub mod filters;
pub mod states;

/// Every golden set this package knows, checked against the committed files.
#[must_use]
pub fn check_all() -> Vec<SetReport> {
    vec![
        check_rng(),
        check_libm(),
        check_pyfmt(),
        check_ruleset(),
        check_uniques(),
        filters::check_filters(),
        filters::check_gen(),
        states::check_states(),
        convert::check_convert(),
    ]
}

/// The report `golden check --out` writes: one entry per set.
#[must_use]
pub fn report_json(reports: &[SetReport]) -> Value {
    let mut sets = serde_json::Map::new();
    for r in reports {
        sets.insert(
            r.name.to_owned(),
            json!({"computed": r.computed, "matches_committed": r.problems.is_empty()}),
        );
    }
    json!({
        "format": 1,
        "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        "debug_assertions": cfg!(debug_assertions),
        "sets": sets,
    })
}

/// The new contents of the files `golden bless` writes, as (file name, text). `pyfmt.json` is
/// not among them: only the Python recorder writes it.
#[must_use]
pub fn blessed_files() -> Vec<(&'static str, String)> {
    vec![
        ("rng.json", render_rng(&rng_answers())),
        ("libm.json", render_libm(&libm_answers(&libm_inputs()))),
        ("ruleset.json", render_rows(&ruleset_answers(), &[])),
        ("uniques.json", render_rows(&uniques_answers(), &UNIQUE_LISTS)),
    ]
    .into_iter()
    .chain(filters::blessed())
    .chain(states::blessed())
    .chain(convert::blessed())
    .collect()
}

#[allow(
    clippy::disallowed_methods,
    reason = "the golden sets are files, and reading them is the point"
)]
fn read_committed(file: &str) -> Result<Value, String> {
    let path = golden_dir().join(file);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e} (run `cargo golden bless`?)", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{file} is not valid JSON: {e}"))
}

fn digest_of(v: &Value) -> String {
    blake3::hash(v.to_string().as_bytes()).to_hex().to_string()
}

/// Keeps a failure report readable: the first few problems, and a count of the rest.
fn capped(mut problems: Vec<String>) -> Vec<String> {
    const MAX: usize = 25;
    if problems.len() > MAX {
        let more = problems.len() - MAX;
        problems.truncate(MAX);
        problems.push(format!("... and {more} more"));
    }
    problems
}

fn hex64(x: u64) -> String {
    format!("{x:016x}")
}

fn parse_hex64(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?, 16).ok()
}

fn float_str(x: f64) -> String {
    if x.is_nan() { "nan".to_owned() } else { hex64(x.to_bits()) }
}

/// Compares two row lists and describes the rows that differ.
fn diff_rows(file: &str, key: &str, want: Option<&Value>, got: &Value) -> Vec<String> {
    let (Some(want), Some(got)) = (want.and_then(Value::as_array), got.as_array()) else {
        return vec![format!("{file}: `{key}` is missing or not a list")];
    };
    let mut out = Vec::new();
    if want.len() != got.len() {
        out.push(format!(
            "{file}: `{key}` has {} rows, this build computes {}",
            want.len(),
            got.len()
        ));
    }
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        if w != g {
            out.push(format!("{file}: {key}[{i}]: committed {w}, computed {g}"));
        }
    }
    out
}

// ---- rng.json ---------------------------------------------------------------------------------

/// The eight streams whose first 32 values are pinned: seed, purpose, keys.
const STREAMS: &[(u64, Purpose, &[u64])] = &[
    (0, Purpose::Combat, &[]),
    (1, Purpose::Combat, &[0]),
    (1, Purpose::Combat, &[u64::MAX]),
    (42, Purpose::MapLand, &[1, 2, 3]),
    (u64::MAX, Purpose::TestAgent, &[7]),
    (12_345, Purpose::Trigger, &[10, 500, 3]),
    (2026, Purpose::BotBase, &[]),
    (99, Purpose::Advisor, &[0; 8]),
];

/// The chi-square checks: name, degrees of freedom, statistic, and the critical value at
/// p = 0.001 for those degrees of freedom.
fn chi_square_rows() -> Vec<(String, u32, f64, f64)> {
    fn statistic(counts: &[u64], expected: f64) -> f64 {
        counts
            .iter()
            .map(|&c| {
                let d = c as f64 - expected;
                d * d / expected
            })
            .sum()
    }
    const DRAWS: u64 = 100_000;
    let mut out = Vec::new();
    for (n, crit) in [(10u64, 27.877), (7, 22.458)] {
        let mut rng = Rng::keyed(7, Purpose::TestAgent, &[1, n]);
        let mut counts = vec![0u64; n as usize];
        for _ in 0..DRAWS {
            counts[rng.below(n) as usize] += 1;
        }
        let df = (n - 1) as u32;
        out.push((format!("below({n})"), df, statistic(&counts, DRAWS as f64 / n as f64), crit));
    }
    let bins = 20usize;
    let mut rng = Rng::keyed(7, Purpose::TestAgent, &[2]);
    let mut counts = vec![0u64; bins];
    for _ in 0..DRAWS {
        // unit() < 1, so the bin is below 20.
        counts[(rng.unit() * bins as f64) as usize] += 1;
    }
    out.push((
        "unit() in 20 bins".to_owned(),
        19,
        statistic(&counts, DRAWS as f64 / bins as f64),
        43.820,
    ));
    out
}

/// Draws from each distribution: name, arguments, draws.
fn draw_rows() -> Vec<Value> {
    let mut rows = Vec::new();
    let stream = |tag: u64| Rng::keyed(42, Purpose::TestAgent, &[tag]);
    for (i, n) in [1u64, 2, 3, 6, 7, 10, 100, 1000, 1 << 32, (1 << 32) + 1, 1 << 63, u64::MAX]
        .into_iter()
        .enumerate()
    {
        let mut r = stream(100 + i as u64);
        let draws: Vec<String> = (0..8).map(|_| r.below(n).to_string()).collect();
        rows.push(json!(["below", [n.to_string()], draws]));
    }
    for (i, (lo, hi)) in
        [(-5i64, 5i64), (0, 0), (1, 6), (-1_000_000, 3), (i64::MIN, i64::MAX), (9, 2)]
            .into_iter()
            .enumerate()
    {
        let mut r = stream(200 + i as u64);
        let draws: Vec<String> = (0..8).map(|_| r.range(lo, hi).to_string()).collect();
        rows.push(json!(["range", [lo.to_string(), hi.to_string()], draws]));
    }
    let mut r = stream(300);
    let units: Vec<String> = (0..16).map(|_| PyFloat(r.unit()).to_string()).collect();
    rows.push(json!(["unit", [], units]));
    for (i, p) in [0.0, 0.25, 0.5, 0.999, 1.0].into_iter().enumerate() {
        let mut r = stream(400 + i as u64);
        let flips: String = (0..32).map(|_| if r.chance(p) { '1' } else { '0' }).collect();
        rows.push(json!(["chance", [PyFloat(p).to_string()], [flips]]));
    }
    for tag in 500..503u64 {
        let mut r = stream(tag);
        let mut v: Vec<u32> = (0..20).collect();
        r.shuffle(&mut v);
        rows.push(json!(["shuffle", ["0..20"], v]));
    }
    let letters = ["a", "b", "c", "d", "e", "f", "g"];
    let mut r = stream(600);
    let picks: Vec<&str> = (0..16).filter_map(|_| r.pick(&letters).copied()).collect();
    rows.push(json!(["pick", ["a..g"], picks]));
    let weights: [&[f64]; 5] =
        [&[1.0, 2.0, 3.0, 4.0], &[0.0, 0.0, 5.0, 0.0], &[0.5, 1e-9, 2.5], &[1e300, 1e300], &[]];
    for (i, w) in weights.into_iter().enumerate() {
        let mut r = stream(700 + i as u64);
        let draws: Vec<Value> = (0..16).map(|_| json!(r.weighted(w))).collect();
        let args: Vec<String> = w.iter().map(|x| PyFloat(*x).to_string()).collect();
        rows.push(json!(["weighted", args, draws]));
    }
    rows
}

/// Everything `rng.json` holds, computed by this build.
#[must_use]
pub fn rng_answers() -> Value {
    let purposes: Vec<Value> =
        Purpose::ALL.iter().map(|p| json!([format!("{p:?}"), p.code()])).collect();
    let streams: Vec<Value> = STREAMS
        .iter()
        .map(|&(seed, purpose, keys)| {
            let mut r = Rng::keyed(seed, purpose, keys);
            let first: Vec<String> = (0..32).map(|_| hex64(r.next_u64())).collect();
            let keys: Vec<String> = keys.iter().map(|&k| hex64(k)).collect();
            json!([hex64(seed), format!("{purpose:?}"), keys, first])
        })
        .collect();
    let chi: Vec<Value> = chi_square_rows()
        .into_iter()
        .map(|(name, df, stat, crit)| {
            json!([name, df, PyFloat(stat).to_string(), PyFloat(crit).to_string()])
        })
        .collect();
    json!({
        "format": 1,
        "generator": "base::rng: xoshiro256++ keyed through SplitMix64 (DESIGN.md 7.1)",
        "purposes": purposes,
        "streams": streams,
        "draws": draw_rows(),
        "chi_square": chi,
    })
}

fn check_rng() -> SetReport {
    let got = rng_answers();
    let mut problems = Vec::new();
    for (name, _, stat, crit) in chi_square_rows() {
        // Written so that a NaN statistic fails too.
        let under_bound = stat < crit;
        if !under_bound {
            problems.push(format!(
                "rng: the chi-square statistic of {name} is {}, over the p = 0.001 bound {}",
                PyFloat(stat),
                PyFloat(crit)
            ));
        }
    }
    match read_committed("rng.json") {
        Err(e) => problems.push(e),
        Ok(want) => {
            for key in ["purposes", "streams", "draws", "chi_square"] {
                problems.extend(diff_rows("rng.json", key, want.get(key), &got[key]));
            }
        }
    }
    SetReport { name: "rng", computed: digest_of(&got), problems: capped(problems) }
}

fn render_rows(head: &Value, lists: &[&str]) -> String {
    let mut out = String::from("{");
    let Some(obj) = head.as_object() else { return head.to_string() };
    let mut first = true;
    for (k, v) in obj {
        if !first {
            out.push_str(",\n");
        }
        first = false;
        out.push_str(&Value::from(k.as_str()).to_string());
        out.push_str(": ");
        if lists.contains(&k.as_str())
            && let Some(rows) = v.as_array()
        {
            out.push('[');
            for (i, row) in rows.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                out.push_str(&row.to_string());
            }
            out.push_str("\n]");
        } else {
            out.push_str(&v.to_string());
        }
    }
    out.push_str("}\n");
    out
}

fn render_rng(v: &Value) -> String {
    render_rows(v, &["purposes", "streams", "draws", "chi_square"])
}

// ---- libm.json --------------------------------------------------------------------------------

/// A SplitMix64 of its own, so the libm inputs do not move when the game RNG does.
struct Inputs(u64);

impl Inputs {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[lo, hi)`.
    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        let u = (self.next() >> 11) as f64 / 9_007_199_254_740_992.0;
        lo + (hi - lo) * u
    }

    /// Positive, with a binary exponent uniform in `emin..=emax` and random mantissa bits.
    fn log_uniform(&mut self, emin: i32, emax: i32) -> f64 {
        let span = (emax - emin + 1).unsigned_abs();
        let e = emin + (self.next() % u64::from(span)) as i32;
        let mantissa = self.next() >> 12;
        f64::from_bits((u64::from((1023 + e).unsigned_abs()) << 52) | mantissa)
    }
}

/// The maths wrappers and how many arguments each takes.
fn eval_libm(name: &str, args: &[f64]) -> Option<f64> {
    Some(match (name, args) {
        ("pow", &[x, y]) => num::pow(x, y),
        ("exp", &[x]) => num::exp(x),
        ("ln", &[x]) => num::ln(x),
        ("log10", &[x]) => num::log10(x),
        ("hypot", &[x, y]) => num::hypot(x, y),
        ("sin", &[x]) => num::sin(x),
        ("cos", &[x]) => num::cos(x),
        ("atan2", &[y, x]) => num::atan2(y, x),
        ("sqrt", &[x]) => x.sqrt(),
        _ => return None,
    })
}

/// About 2,000 inputs over the ranges the engine uses, and the edges.
#[must_use]
pub fn libm_inputs() -> Vec<(&'static str, Vec<f64>)> {
    let mut g = Inputs(0x0001_1B00_2026_0923);
    let mut out: Vec<(&'static str, Vec<f64>)> = Vec::new();
    let pi = core::f64::consts::PI;

    // pow: the game's exponents on game-sized bases, then wide ranges, then edges.
    for &(x, y) in &[
        (0.0, 0.0),
        (0.0, -1.0),
        (-0.0, 3.0),
        (2.0, 0.5),
        (2.0, -1074.0),
        (2.0, 1023.0),
        (2.0, 1024.0),
        (-8.0, 1.0 / 3.0),
        (-2.0, 3.0),
        (10.0, 22.0),
        (10.0, -5.0),
        (1.0, 1e300),
    ] {
        out.push(("pow", vec![x, y]));
    }
    let game_exponents = [0.3, 0.333, 0.4, 0.75, 0.9, 0.99, 1.01, 1.3, 1.45, 1.5, 2.01, 4.0];
    for i in 0..138 {
        let x = if i % 3 == 0 { (g.next() % 200) as f64 } else { g.uniform(0.0, 1000.0) };
        out.push(("pow", vec![x, game_exponents[i % game_exponents.len()]]));
    }
    for _ in 0..100 {
        out.push(("pow", vec![g.uniform(0.0, 1e6), g.uniform(-3.0, 3.0)]));
    }
    for _ in 0..50 {
        out.push(("pow", vec![g.log_uniform(-60, 60), g.uniform(-4.0, 4.0)]));
    }

    for &x in &[0.0, -0.0, 1.0, -1.0, 709.78, 710.0, -745.1, -746.0, 1e-10, -1e-10] {
        out.push(("exp", vec![x]));
    }
    for _ in 0..240 {
        out.push(("exp", vec![g.uniform(-50.0, 50.0)]));
    }

    for name in ["ln", "log10"] {
        for &x in &[1.0, 0.0, 5e-324, f64::MAX, 10.0, 100.0, 1e-300, core::f64::consts::E, 1e22] {
            out.push((name, vec![x]));
        }
        for i in 0..241 {
            let x = if i % 2 == 0 { g.uniform(0.0, 1000.0) } else { g.log_uniform(-1000, 1000) };
            out.push((name, vec![x]));
        }
    }

    for &(x, y) in &[(0.0, 0.0), (3.0, 4.0), (1e300, 1e300), (5e-324, 5e-324), (-3.0, -4.0)] {
        out.push(("hypot", vec![x, y]));
    }
    for i in 0..245 {
        let (x, y) = if i % 2 == 0 {
            (g.uniform(-200.0, 200.0), g.uniform(-200.0, 200.0))
        } else {
            (g.log_uniform(-500, 500), g.log_uniform(-500, 500))
        };
        out.push(("hypot", vec![x, y]));
    }

    for name in ["sin", "cos"] {
        for &x in &[0.0, -0.0, pi, pi / 2.0, -pi, 1e6, 1e22, 1e300, 710.0, 2.0 * pi] {
            out.push((name, vec![x]));
        }
        for i in 0..240 {
            let x = if i % 4 == 0 {
                pi / 2.0 * (g.next() % 64) as f64 + g.uniform(-1e-9, 1e-9)
            } else {
                g.uniform(-1000.0, 1000.0)
            };
            out.push((name, vec![x]));
        }
    }

    for &(y, x) in &[
        (0.0, 0.0),
        (0.0, -0.0),
        (-0.0, -0.0),
        (1.0, 0.0),
        (0.0, -1.0),
        (-1.0, -1.0),
        (1.0, 1e-300),
        (1e300, 1.0),
    ] {
        out.push(("atan2", vec![y, x]));
    }
    for _ in 0..242 {
        out.push(("atan2", vec![g.uniform(-100.0, 100.0), g.uniform(-100.0, 100.0)]));
    }

    for _ in 0..100 {
        out.push(("sqrt", vec![g.log_uniform(-1020, 1020)]));
    }
    out
}

/// The `libm.json` document for these inputs, with this build's outputs.
fn libm_answers(inputs: &[(&str, Vec<f64>)]) -> Value {
    let cases: Vec<Value> = inputs
        .iter()
        .map(|(name, args)| {
            let mut row = vec![Value::from(*name)];
            row.extend(args.iter().map(|&a| Value::from(float_str(a))));
            let out =
                eval_libm(name, args).map_or_else(|| "unknown function".to_owned(), float_str);
            row.push(Value::from(out));
            Value::Array(row)
        })
        .collect();
    json!({
        "format": 1,
        "functions": "base::num over libm =0.2.16; sqrt from std (DESIGN.md 7.3). NaN is written as \"nan\": its bits differ between targets",
        "cases": cases,
    })
}

fn render_libm(v: &Value) -> String {
    render_rows(v, &["cases"])
}

fn check_libm() -> SetReport {
    let mut problems = Vec::new();
    let committed = match read_committed("libm.json") {
        Ok(v) => v,
        Err(e) => {
            return SetReport { name: "libm", computed: String::new(), problems: vec![e] };
        }
    };
    let rows = committed.get("cases").and_then(Value::as_array).cloned().unwrap_or_default();
    if rows.len() < 1_900 {
        problems.push(format!("libm.json: only {} cases; about 2,000 were blessed", rows.len()));
    }
    let mut inputs = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let parsed = row.as_array().and_then(|cells| {
            let (name, rest) = cells.split_first()?;
            let (_, args) = rest.split_last()?;
            let args: Option<Vec<f64>> =
                args.iter().map(|a| parse_hex64(a).map(f64::from_bits)).collect();
            Some((name.as_str()?.to_owned(), args?))
        });
        match parsed {
            Some(case) => inputs.push(case),
            None => problems.push(format!("libm.json: cases[{i}] is malformed: {row}")),
        }
    }
    let named: Vec<(&str, Vec<f64>)> =
        inputs.iter().map(|(n, a)| (n.as_str(), a.clone())).collect();
    let got = libm_answers(&named);
    problems.extend(diff_rows("libm.json", "cases", committed.get("cases"), &got["cases"]));
    SetReport { name: "libm", computed: digest_of(&got["cases"]), problems: capped(problems) }
}

// ---- ruleset.json -----------------------------------------------------------------------------

/// Everything `ruleset.json` holds, computed by this build: the embedded ruleset's identity, or
/// why it does not load.
#[must_use]
pub fn ruleset_answers() -> Value {
    match Ruleset::load(&embedded()) {
        Ok(r) => json!({
            "format": 1,
            "walk": "blake3 over the canonical walk of the parsed files (DESIGN.md 5.2)",
            "ruleset_id": r.id().to_hex(),
            "version": r.version(),
            "counts": r.counts(),
        }),
        Err(e) => json!({"format": 1, "error": e.to_string()}),
    }
}

fn check_ruleset() -> SetReport {
    let got = ruleset_answers();
    let mut problems = Vec::new();
    if let Some(e) = got.get("error") {
        problems.push(format!("ruleset: the embedded ruleset does not load: {e}"));
    }
    match read_committed("ruleset.json") {
        Err(e) => problems.push(e),
        Ok(want) => {
            for key in ["ruleset_id", "version", "counts"] {
                if want.get(key) != got.get(key) {
                    problems.push(format!(
                        "ruleset.json: `{key}` is {} in the file, {} in this build",
                        want.get(key).unwrap_or(&Value::Null),
                        got.get(key).unwrap_or(&Value::Null)
                    ));
                }
            }
        }
    }
    SetReport { name: "ruleset", computed: digest_of(&got), problems: capped(problems) }
}

// ---- uniques.json -----------------------------------------------------------------------------

/// The lists of `uniques.json`, rendered one row a line.
const UNIQUE_LISTS: [&str; 8] =
    ["uniques", "sources", "fracs", "stats", "sets", "objects", "tags", "abilities"];

/// Every source object's uniques in [`Source`] order, each named as [`source_name`] names it.
fn sources_of(r: &Ruleset) -> Vec<(String, &SourceUniques)> {
    fn named<'r, T: 'r>(
        out: &mut Vec<(String, &'r SourceUniques)>,
        kind: &str,
        rows: &'r [T],
        name: impl Fn(&T) -> &str,
        uniques: impl Fn(&'r T) -> &'r SourceUniques,
    ) {
        out.extend(rows.iter().map(|x| (format!("{kind}:{}", name(x)), uniques(x))));
    }
    let mut out = Vec::new();
    named(&mut out, "Nation", r.nations().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Building", r.buildings().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Policy", r.policies().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Tech", r.techs().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Era", r.eras().as_slice(), |x| &x.name, |x| &x.uniques);
    let cs = r.city_state_types().as_slice();
    named(&mut out, "CityStateFriend", cs, |x| &x.name, |x| &x.friend);
    named(&mut out, "CityStateAlly", cs, |x| &x.name, |x| &x.ally);
    named(&mut out, "CityStateType", cs, |x| &x.name, |x| &x.uniques);
    named(&mut out, "Belief", r.beliefs().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Resource", r.resources().as_slice(), |x| &x.name, |x| &x.uniques);
    out.push(("Global:".to_owned(), r.global_uniques()));
    named(&mut out, "Terrain", r.terrains().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Improvement", r.improvements().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "UnitType", r.unit_types().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Unit", r.base_units().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Promotion", r.promotions().as_slice(), |x| &x.name, |x| &x.uniques);
    named(&mut out, "Ruins", r.ruins().as_slice(), |x| &x.name, |x| &x.uniques);
    out
}

/// One row of `uniques.json`'s `sources`: a source's `all` range, the ids in each partition, and
/// its tags by name. The partitions are what the unique indexes are built from.
fn source_row(r: &Ruleset, name: String, u: &SourceUniques) -> Value {
    let t = r.uniques();
    let ids = |list: &[UniqueId]| list.iter().map(|id| id.0).collect::<Vec<u16>>();
    let tags = |set: &TagSet| set.iter().map(|g| t.tag(g)).collect::<Vec<&str>>();
    json!([
        name,
        [u.all.start, u.all.end],
        ids(&u.civ),
        ids(&u.local),
        ids(&u.on_gain),
        ids(&u.triggered),
        ids(&u.actions),
        ids(&u.ai),
        tags(&u.tags),
        tags(&u.cond_tags),
    ])
}

/// The name of the object a unique came from, as `golden` rows write it: `Building:Temple`.
fn source_name(r: &Ruleset, s: Source) -> String {
    let name: &str = match s {
        Source::Nation(id) => &r.nations()[id].name,
        Source::Building(id) => &r.buildings()[id].name,
        Source::Policy(id) => &r.policies()[id].name,
        Source::Tech(id) => &r.techs()[id].name,
        Source::Temporary(id) => return format!("Temporary:#{}", id.0),
        Source::Era(id) => &r.eras()[id].name,
        Source::CityStateFriend(id) | Source::CityStateAlly(id) | Source::CityStateType(id) => {
            &r.city_state_types()[id].name
        }
        Source::Belief(id) => &r.beliefs()[id].name,
        Source::Resource(id) => &r.resources()[id].name,
        Source::Global => "",
        Source::Terrain(id) => &r.terrains()[id].name,
        Source::Improvement(id) => &r.improvements()[id].name,
        Source::UnitType(id) => &r.unit_types()[id].name,
        Source::Unit(id) => &r.base_units()[id].name,
        Source::Promotion(id) => &r.promotions()[id].name,
        Source::Ruins(id) => &r.ruins()[id].name,
    };
    format!("{}:{name}", s.kind_name())
}

fn stats_json(s: &Stats) -> Value {
    let mut o = serde_json::Map::new();
    for (stat, v) in s.nonzero() {
        o.insert(stat.key().into(), json!(v));
    }
    Value::Object(o)
}

fn param_json(r: &Ruleset, p: Param) -> Value {
    match p.value(r) {
        ParamValue::Int(n) => json!(n),
        ParamValue::Real(x) => json!(x),
        ParamValue::Stats(s) => stats_json(&s),
        ParamValue::Text(t) => json!(t),
    }
}

/// Everything `uniques.json` holds, computed by this build: every compiled unique of the embedded
/// ruleset, one row each (package 1a-05, gate 1), with the tables they refer to. Each row is the
/// unique's id, source, occurrence and text; its type and role; its flags; its parameters as the
/// ruleset meant them; its modifiers; its key; its temporary variant, ability and tag.
#[must_use]
pub fn uniques_answers() -> Value {
    let r = match Ruleset::load(&embedded()) {
        Ok(r) => r,
        Err(e) => return json!({"format": 1, "error": e.to_string()}),
    };
    let t = r.uniques();
    let rows: Vec<Value> = t
        .iter()
        .map(|(id, u)| {
            let m = t.meta(id);
            let params: Vec<Value> =
                u.data.params().into_iter().map(|p| param_json(&r, p)).collect();
            let mods: Vec<Value> = t
                .modifiers(id)
                .into_iter()
                .map(|(ty, ps)| {
                    let ps: Vec<Value> = ps.into_iter().map(|p| param_json(&r, p)).collect();
                    json!([ty.name(), ps])
                })
                .collect();
            let flags: Vec<&str> = u.flags().iter_names().map(|(n, _)| n).collect();
            json!([
                id.0,
                source_name(&r, m.source),
                m.occurrence,
                t.text(m.text),
                m.ty.map(|ty| ty.name()),
                format!("{:?}", m.role),
                flags,
                params,
                mods,
                hex64(m.key),
                m.temp_variant.map(|v| v.0),
                m.ability.map(|a| t.ability(a)),
                m.tag.map(|g| t.tag(g)),
            ])
        })
        .collect();
    let sources: Vec<Value> =
        sources_of(&r).into_iter().map(|(name, u)| source_row(&r, name, u)).collect();
    let fracs: Vec<Value> = r.fracs().as_slice().iter().map(|&x| json!(x)).collect();
    let stats: Vec<Value> = t.all_stats().as_slice().iter().map(stats_json).collect();
    let sets: Vec<Value> = t
        .sets()
        .as_slice()
        .iter()
        .map(|s| {
            json!([
                format!("{:?}", s.domain),
                t.text(s.text),
                s.members.iter().collect::<Vec<u32>>(),
                s.fixed
            ])
        })
        .collect();
    let objects: Vec<Value> = t
        .objects()
        .as_slice()
        .iter()
        .map(|o| {
            json!([
                o.kind.name(),
                t.text(o.text),
                o.tiles.map(|f| f.0),
                o.buildings.map(|s| s.0),
                o.improvements.map(|s| s.0),
                o.specialist.map(|s| r.name(s))
            ])
        })
        .collect();
    let tags: Vec<Value> = (0..t.tag_count())
        .filter_map(|i| u8::try_from(i).ok())
        .map(|i| json!(t.tag(TagId(i))))
        .collect();
    let abilities: Vec<Value> = (0..t.ability_count())
        .filter_map(|i| u16::try_from(i).ok())
        .map(|i| json!(t.ability(AbilityKey(i))))
        .collect();
    json!({
        "format": 1,
        "about": "every compiled unique of the embedded ruleset (DESIGN.md 5.5): [id, source, occurrence, text, type, role, flags, params, modifiers, key, temp_variant, ability, tag]",
        "about_sources": "every source object's uniques (SourceUniques): [source, [all.start, all.end], civ, local, on_gain, triggered, actions, ai, tags, cond_tags]",
        "counts": {
            "uniques": t.len(),
            "sources": sources.len(),
            "conds": t.all_conds().len(),
            "fracs": fracs.len(),
            "stats": stats.len(),
            "sets": sets.len(),
            "objects": objects.len(),
            "tags": tags.len(),
            "abilities": abilities.len(),
        },
        "uniques": rows,
        "sources": sources,
        "fracs": fracs,
        "stats": stats,
        "sets": sets,
        "objects": objects,
        "tags": tags,
        "abilities": abilities,
    })
}

fn check_uniques() -> SetReport {
    let got = uniques_answers();
    let mut problems = Vec::new();
    if let Some(e) = got.get("error") {
        problems.push(format!("uniques: the embedded ruleset does not load: {e}"));
    }
    match read_committed("uniques.json") {
        Err(e) => problems.push(e),
        Ok(want) => {
            if want.get("counts") != got.get("counts") {
                problems.push(format!(
                    "uniques.json: the counts are {} in the file, {} in this build",
                    want.get("counts").unwrap_or(&Value::Null),
                    got.get("counts").unwrap_or(&Value::Null)
                ));
            }
            for key in UNIQUE_LISTS {
                problems.extend(diff_rows("uniques.json", key, want.get(key), &got[key]));
            }
        }
    }
    SetReport { name: "uniques", computed: digest_of(&got), problems: capped(problems) }
}

// ---- pyfmt.json -------------------------------------------------------------------------------

fn parse_py_float(s: &str) -> Option<f64> {
    s.parse().ok()
}

fn same_float(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn check_pyfmt() -> SetReport {
    let want = match read_committed("pyfmt.json") {
        Ok(v) => v,
        Err(e) => return SetReport { name: "pyfmt", computed: String::new(), problems: vec![e] },
    };
    let mut problems = Vec::new();
    let ndigits: Vec<i32> = want
        .get("ndigits")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter().filter_map(|n| n.as_i64()).filter_map(|n| i32::try_from(n).ok()).collect()
        })
        .unwrap_or_default();
    let empty = Vec::new();
    let rows = |key: &str| want.get(key).and_then(Value::as_array).unwrap_or(&empty);

    let mut computed_cases = Vec::new();
    for (i, row) in rows("cases").iter().enumerate() {
        let Some(x) = row.get(0).and_then(parse_hex64).map(f64::from_bits) else {
            problems.push(format!("pyfmt.json: cases[{i}] is malformed"));
            continue;
        };
        let repr = PyFloat(x).to_string();
        if row.get(1).and_then(Value::as_str) != Some(repr.as_str()) {
            problems.push(format!("pyfmt: repr({:?}) is {} in Python, {repr} here", x, row[1]));
        }
        let whole = num::round_half_even(x);
        // Python's round(x) is an int, which has no negative zero: -0.4 rounds to 0, where the
        // float round_half_even gives -0.0. Compare the values, so the zeros are equal.
        let same_value = |py: f64| same_float(py, whole) || (py == 0.0 && whole == 0.0);
        if let Some(py) = row.get(2).and_then(Value::as_str).and_then(parse_py_float)
            && !same_value(py)
        {
            problems.push(format!("pyfmt: round({}) is {py} in Python, {whole} here", row[1]));
        }
        let py_rounded = row.get(3).and_then(Value::as_array).unwrap_or(&empty);
        let mut ours = Vec::new();
        for (n, py) in ndigits.iter().zip(py_rounded) {
            let r = num::round_ndigits(x, *n);
            // The value, then the text: PyRound is what model-facing text writes.
            let text = PyRound(x, *n).to_string();
            let value_ok = py.as_str().and_then(parse_py_float).is_some_and(|p| same_float(p, r));
            if !value_ok || py.as_str() != Some(text.as_str()) {
                problems
                    .push(format!("pyfmt: round({}, {n}) is {py} in Python, {text} here", row[1]));
            }
            ours.push(json!([float_str(r), text]));
        }
        computed_cases.push(json!([hex64(x.to_bits()), repr, float_str(whole), ours]));
    }

    let mut computed_extra = Vec::new();
    for (i, row) in rows("extra").iter().enumerate() {
        let x = row.get(0).and_then(parse_hex64).map(f64::from_bits);
        let n = row.get(1).and_then(Value::as_i64).and_then(|n| i32::try_from(n).ok());
        let (Some(x), Some(n)) = (x, n) else {
            problems.push(format!("pyfmt.json: extra[{i}] is malformed"));
            continue;
        };
        let r = num::round_ndigits(x, n);
        let text = PyRound(x, n).to_string();
        // null: Python raised OverflowError, where round_ndigits returns x unchanged.
        let ok = match &row[2] {
            Value::Null => same_float(x, r),
            v => {
                v.as_str().and_then(parse_py_float).is_some_and(|e| same_float(e, r))
                    && v.as_str() == Some(text.as_str())
            }
        };
        if !ok {
            problems.push(format!(
                "pyfmt: round({}, {n}) is {} in Python, {text} here",
                PyFloat(x),
                row[2]
            ));
        }
        computed_extra.push(json!([hex64(x.to_bits()), n, float_str(r), text]));
    }

    let mut computed_fixed = Vec::new();
    for (i, row) in rows("fixed").iter().enumerate() {
        let x = row.get(0).and_then(parse_hex64).map(f64::from_bits);
        let n = row.get(1).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok());
        let (Some(x), Some(n), Some(py)) = (x, n, row.get(2).and_then(Value::as_str)) else {
            problems.push(format!("pyfmt.json: fixed[{i}] is malformed"));
            continue;
        };
        let text = format!("{:.n$}", PyFloat(x));
        if text != py {
            problems.push(format!(
                "pyfmt: format({}, '.{n}f') is {py} in Python, {text} here",
                PyFloat(x)
            ));
        }
        computed_fixed.push(json!([hex64(x.to_bits()), n, text]));
    }

    let mut computed_floor = Vec::new();
    for (i, row) in rows("floor").iter().enumerate() {
        let cells: Option<Vec<i64>> =
            row.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect());
        let Some(&[a, b, q, r]) = cells.as_deref() else {
            problems.push(format!("pyfmt.json: floor[{i}] is malformed"));
            continue;
        };
        let (gq, gr) = (a.floor_div(b), a.floor_mod(b));
        if (gq, gr) != (q, r) {
            problems.push(format!(
                "pyfmt: {a} // {b}, {a} % {b} are {q}, {r} in Python, {gq}, {gr} here"
            ));
        }
        if let (Ok(a32), Ok(b32), Ok(q32), Ok(r32)) =
            (i32::try_from(a), i32::try_from(b), i32::try_from(q), i32::try_from(r))
        {
            let got = (a32.floor_div(b32), a32.floor_mod(b32));
            if got != (q32, r32) {
                problems.push(format!("pyfmt: as i32, {a} // {b}, {a} % {b} give {got:?} here"));
            }
        }
        computed_floor.push(json!([a, b, gq, gr]));
    }

    for (key, min) in [("cases", 2_400), ("extra", 1_000), ("fixed", 1_500), ("floor", 1_000)] {
        if rows(key).len() < min {
            problems.push(format!("pyfmt.json: only {} rows of `{key}`", rows(key).len()));
        }
    }
    let computed = json!({
        "cases": computed_cases,
        "extra": computed_extra,
        "fixed": computed_fixed,
        "floor": computed_floor,
    });
    SetReport { name: "pyfmt", computed: digest_of(&computed), problems: capped(problems) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libm_inputs_are_about_two_thousand() {
        let n = libm_inputs().len();
        assert!((1_900..=2_300).contains(&n), "{n} libm inputs");
    }

    #[test]
    fn rendering_round_trips() {
        let v = rng_answers();
        let text = render_rng(&v);
        let back: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(back, v);
    }
}

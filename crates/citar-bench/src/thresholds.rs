//! `thresholds.toml`: the budgets of DESIGN.md 10, one per measure, with the hard limit every
//! budget is held to (1.5 times it from package 1e-03 on) and the rules of the pass rounds.
//! `cargo xtask perf` reads the same file with its own copy of this reader (xtask builds without
//! the engine).

use std::path::PathBuf;

use serde::Deserialize;

/// The file, beside this crate's manifest.
#[must_use]
pub fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("thresholds.toml")
}

/// One measure's budget.
#[derive(Clone, Debug, Deserialize)]
pub struct Budget {
    /// The measure, as a suite records it (`Suite::put`).
    pub id: String,
    /// The suite that records it: `kernels`, `turns` or `io`.
    pub suite: String,
    /// The budget as written (`"5 ns"`, `"1.5 µs"`, `"20 ms"`).
    pub budget: String,
    /// The row of DESIGN.md 10 it holds.
    pub row: String,
    /// Whether it is measured only on a corpus state.
    #[serde(default)]
    pub corpus: bool,
    /// The budget in nanoseconds, filled in on load.
    #[serde(skip)]
    pub ns: f64,
}

impl Budget {
    /// The budget as written.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.budget
    }
}

/// A backstop on the pass rounds of some states: a hard limit, whatever Python took.
#[derive(Clone, Debug, Deserialize)]
pub struct Backstop {
    /// States whose case starts with this (`small-`).
    pub case_prefix: String,
    pub turn: u32,
    pub budget: String,
    #[serde(skip)]
    pub ns: f64,
}

/// The target budgets of one map size's pass rounds: `(turn, milliseconds)`, interpolated
/// between the points and held at the first and last beyond them.
#[derive(Clone, Debug, Deserialize)]
pub struct Target {
    pub case_prefix: String,
    pub points: Vec<(u32, f64)>,
}

impl Target {
    /// The budget at `turn`, in nanoseconds.
    #[must_use]
    pub fn at(&self, turn: u32) -> Option<f64> {
        let (first, last) = (self.points.first()?, self.points.last()?);
        if turn <= first.0 {
            return Some(first.1 * 1e6);
        }
        for w in self.points.windows(2) {
            let ((t0, m0), (t1, m1)) = (w[0], w[1]);
            if turn <= t1 {
                let f = f64::from(turn - t0) / f64::from((t1 - t0).max(1));
                return Some((m0 + (m1 - m0) * f) * 1e6);
            }
        }
        Some(last.1 * 1e6)
    }
}

/// The rules of the pass rounds (DESIGN.md 10, gate 2 of package 1e-03).
#[derive(Clone, Debug, Deserialize)]
pub struct PassRound {
    /// Every corpus state's round at least this many times faster than Python's.
    pub ratio: f64,
    pub backstop: Vec<Backstop>,
    pub target: Vec<Target>,
}

/// The whole file.
#[derive(Clone, Debug, Deserialize)]
pub struct Thresholds {
    /// A measure above `hard` times its budget fails.
    pub hard: f64,
    pub budget: Vec<Budget>,
    pub pass_round: PassRound,
}

impl Thresholds {
    /// Reads the file.
    ///
    /// # Errors
    ///
    /// If it does not read, parse, or a budget is not a duration.
    pub fn load() -> Result<Self, String> {
        let text = std::fs::read_to_string(path()).map_err(|e| e.to_string())?;
        Self::parse(&text)
    }

    /// Parses the file's text.
    ///
    /// # Errors
    ///
    /// If it does not parse, a budget is not a duration, or an id is given twice.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut t: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        for b in &mut t.budget {
            b.ns =
                parse_ns(&b.budget).ok_or_else(|| format!("{}: bad budget {}", b.id, b.budget))?;
        }
        for b in &mut t.pass_round.backstop {
            b.ns = parse_ns(&b.budget).ok_or_else(|| format!("bad backstop {}", b.budget))?;
        }
        let mut ids: Vec<&str> = t.budget.iter().map(|b| b.id.as_str()).collect();
        ids.sort_unstable();
        if let Some(w) = ids.windows(2).find(|w| w[0] == w[1]) {
            return Err(format!("{} is given twice", w[0]));
        }
        Ok(t)
    }

    /// The budget of measure `id`.
    #[must_use]
    pub fn budget(&self, id: &str) -> Option<&Budget> {
        self.budget.iter().find(|b| b.id == id)
    }
}

/// A duration as written in the file (`"5 ns"`, `"1.5 us"`, `"1.5 µs"`, `"20 ms"`, `"1.5 s"`), in
/// nanoseconds.
#[must_use]
pub fn parse_ns(s: &str) -> Option<f64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let x: f64 = num.parse().ok()?;
    let scale = match unit.trim() {
        "ns" => 1.0,
        "us" | "µs" => 1e3,
        "ms" => 1e6,
        "s" => 1e9,
        _ => return None,
    };
    Some(x * scale)
}

/// Nanoseconds as a short text with a unit.
#[must_use]
pub fn show_ns(ns: f64) -> String {
    if ns < 1e3 {
        format!("{ns:.1} ns")
    } else if ns < 1e6 {
        format!("{:.2} µs", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.2} s", ns / 1e9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_file_reads_and_every_budget_is_a_duration() {
        let t = Thresholds::load().expect("thresholds.toml");
        assert!((t.hard - 1.5).abs() < 1e-9, "hard limits are 1.5 times the budget");
        assert!(t.budget.iter().all(|b| b.ns > 0.0));
        assert!(t.pass_round.ratio >= 20.0);
    }

    #[test]
    fn durations_read_in_every_unit() {
        assert_eq!(parse_ns("5 ns"), Some(5.0));
        assert_eq!(parse_ns("1.5 µs"), Some(1500.0));
        assert_eq!(parse_ns("1.5 us"), Some(1500.0));
        assert_eq!(parse_ns("20 ms"), Some(2e7));
        assert_eq!(parse_ns("1.5 s"), Some(1.5e9));
        assert_eq!(parse_ns("fast"), None);
    }

    #[test]
    fn a_target_interpolates_between_its_points() {
        let t = Target {
            case_prefix: "small-".into(),
            points: vec![(50, 2.0), (150, 8.0), (300, 20.0)],
        };
        assert_eq!(t.at(1), Some(2e6));
        assert_eq!(t.at(100), Some(5e6));
        assert_eq!(t.at(280), Some(18.4e6));
        assert_eq!(t.at(400), Some(20e6));
    }
}

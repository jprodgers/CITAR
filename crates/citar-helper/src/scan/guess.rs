//! The first guess at a machine's power draw, from its scan: `citar/hwinfo.py` `guess_power`
//! (lines 327-351), ported so that a helper's scan suggests the figures the Python collector
//! suggests for the same hardware (package 4-06, gate 1: a table of recorded scans,
//! `tests/data/hwinfo_power_cases.json`, gives both the same answers).
//!
//! The port keeps Python's semantics where they decide an answer: a value counts when Python's
//! truthiness says so (`None`, `0`, `""` and empty lists do not), `round` rounds half to even, the
//! GPUs' figures add up in their order as floats, and the laptop CPU pattern
//! (`\b\d{4,5}hx?\b|hx\b|ryzen \d \d{4}h`, on the lowercased model) is matched by hand
//! ([`fast_laptop_cpu`]) rather than through a regular expression crate. A value of a type the
//! collector never writes (a power limit given as text) counts as absent where Python would have
//! failed on it.

use serde_json::{Map, Value};

/// What the guess says about itself, as the collector's.
pub const NOTE: &str = "Estimated from the hardware class; measure idle and busy wall power with a \
                        plug-in meter for better numbers.";

/// Python's truthiness of a JSON value (`None`, `False`, `0`, `""`, `[]` and `{}` are false).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn part<'a>(info: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    info.get(key).and_then(Value::as_object)
}

/// `x or default` for a number Python compares with numbers.
fn number_or(v: Option<&Value>, default: f64) -> f64 {
    if !truthy(v) {
        return default;
    }
    v.and_then(Value::as_f64).unwrap_or(default)
}

/// Python's `\w`: a letter or digit of any script, or `_`.
fn word(c: Option<&char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || *c == '_')
}

/// Python's `\b` between `s[i - 1]` and `s[i]`.
fn boundary(s: &[char], i: usize) -> bool {
    let before = i.checked_sub(1).and_then(|j| s.get(j));
    word(before) != word(s.get(i))
}

fn digit(s: &[char], i: usize) -> bool {
    s.get(i).is_some_and(char::is_ascii_digit)
}

fn at(s: &[char], i: usize, text: &str) -> bool {
    text.chars().enumerate().all(|(k, c)| s.get(i + k) == Some(&c))
}

/// `\b\d{4,5}hx?\b` at `i`, trying five digits first and an `x` before none, as the regular
/// expression's greedy quantifiers backtrack.
fn model_number_at(s: &[char], i: usize) -> bool {
    if !boundary(s, i) {
        return false;
    }
    [5, 4].into_iter().any(|n| {
        (0..n).all(|k| digit(s, i + k))
            && s.get(i + n) == Some(&'h')
            && ((s.get(i + n + 1) == Some(&'x') && boundary(s, i + n + 2))
                || boundary(s, i + n + 1))
    })
}

/// `ryzen \d \d{4}h` at `i`.
fn ryzen_h_at(s: &[char], i: usize) -> bool {
    at(s, i, "ryzen ")
        && digit(s, i + 6)
        && s.get(i + 7) == Some(&' ')
        && (8..12).all(|k| digit(s, i + k))
        && s.get(i + 12) == Some(&'h')
}

/// Whether a lowercased CPU model is one of the laptop CPUs that draw desktop power:
/// `re.search(r"\b\d{4,5}hx?\b|hx\b|ryzen \d \d{4}h", cpu)` (an H or HX model number, any
/// HX, a Ryzen H). ASCII digits only: the collector's CPU names are ASCII.
#[must_use]
pub fn fast_laptop_cpu(cpu: &str) -> bool {
    let s: Vec<char> = cpu.chars().collect();
    (0..=s.len()).any(|i| {
        model_number_at(&s, i) || (at(&s, i, "hx") && boundary(&s, i + 2)) || ryzen_h_at(&s, i)
    })
}

/// Rough wall-power figures to start from, as `hwinfo.guess_power` gives them for `info` (a scan
/// in the `citar-hardware` format): `{idle_w, cpu_max_w, gpu_max_w, source: "guess", note}`.
#[must_use]
pub fn guess_power(info: &Map<String, Value>) -> Map<String, Value> {
    let empty = Map::new();
    let system = part(info, "system").unwrap_or(&empty);
    let cpu_part = part(info, "cpu").unwrap_or(&empty);
    let memory = part(info, "memory").unwrap_or(&empty);
    let laptop = system.get("form").and_then(Value::as_str) == Some("laptop");
    let cpu = cpu_part.get("model").and_then(Value::as_str).unwrap_or_default().to_lowercase();
    let threads = number_or(cpu_part.get("threads"), 8.0);
    let unified = truthy(memory.get("unified"));
    let (idle, cpu_max): (i64, i64) = if unified {
        (8, if laptop { 30 } else { 60 })
    } else if laptop {
        (12, if fast_laptop_cpu(&cpu) { 55 } else { 25 })
    } else if cpu.contains("threadripper") || cpu.contains("xeon") || cpu.contains("epyc") {
        (110, 280)
    } else {
        (
            55,
            if threads <= 8.0 {
                65
            } else if threads <= 24.0 {
                125
            } else {
                200
            },
        )
    };
    let mut gpu_max = 0.0f64;
    for g in info.get("gpus").and_then(Value::as_array).into_iter().flatten() {
        let Some(g) = g.as_object() else { continue };
        let limit = g.get("power_limit_w");
        if truthy(limit) {
            if let Some(w) = limit.and_then(Value::as_f64) {
                gpu_max += 0.85 * w;
            }
        } else if matches!(g.get("vendor").and_then(Value::as_str), Some("NVIDIA" | "AMD"))
            && number_or(g.get("vram_gb"), 0.0) >= 6.0
        {
            gpu_max += if laptop { 80.0 } else { 220.0 };
        }
    }
    if unified {
        gpu_max = gpu_max.max(if laptop { 25.0 } else { 60.0 });
    }
    // Python's round(): half to even, to an int. A figure past i64 is no machine's.
    #[allow(clippy::cast_possible_truncation)]
    let gpu = gpu_max.round_ties_even() as i64;
    let mut out = Map::new();
    out.insert("idle_w".into(), Value::from(idle));
    out.insert("cpu_max_w".into(), Value::from(cpu_max));
    out.insert("gpu_max_w".into(), Value::from(gpu));
    out.insert("source".into(), Value::from("guess"));
    out.insert("note".into(), Value::from(NOTE));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_laptop_cpu_pattern_matches_as_the_regular_expression_does() {
        for yes in [
            "13th gen intel(r) core(tm) i9-13980hx",
            "intel core i7-12700h",
            "amd ryzen 9 7945hx with radeon graphics",
            "amd ryzen 7 8845hs w/ radeon 780m graphics",
            "amd ryzen 5 5600h",
            "something hx",
            "i5-1240h",
            "core 12900hx.",
        ] {
            assert!(fast_laptop_cpu(yes), "{yes}");
        }
        for no in [
            "13th gen intel(r) core(tm) i5-13420u",
            "intel core i7-1255u",
            "123456h",
            "i5-1240hq",
            "amd ryzen 7 7840u",
            "hxa",
            "x12900h",
            "",
        ] {
            assert!(!fast_laptop_cpu(no), "{no}");
        }
        // The Nitro this was written on: an H model number.
        assert!(fast_laptop_cpu("13th gen intel(r) core(tm) i5-13420h"));
    }

    #[test]
    fn round_is_half_to_even_and_the_gpus_add_up_in_order() {
        let info: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "system": {"form": "desktop"}, "cpu": {"model": "x", "threads": 16},
            "memory": {"unified": false},
            "gpus": [{"name": "a", "power_limit_w": 450.0}, {"name": "b", "vendor": "AMD", "vram_gb": 8}]
        }))
        .expect("a map");
        let g = guess_power(&info);
        // 0.85 x 450 = 382.5, then + 220 = 602.5: half to even is 602, not 603
        assert_eq!(g["gpu_max_w"], 602);
        assert_eq!((g["idle_w"].as_i64(), g["cpu_max_w"].as_i64()), (Some(55), Some(125)));
        assert_eq!(g["source"], "guess");
    }
}

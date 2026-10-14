//! Local quiet hours (design/phase4-helper.md P4.3.2, P4.3.3): the owner's own rule, enforced on
//! the machine whatever the server asks, and reported (`hello.quiet_until`, `status`).
//!
//! The syntax is protocol 1's (`citar/worker/__main__.py` `_parse_quiet`, lines 38-71): a window
//! `22:00-06:00` every day, or `Sat 00:00-09:00` on one weekday (a name's first three letters);
//! hours alone (`8-17`) are whole hours. A window that wraps midnight is split into two, so each
//! is a containment test on one weekday: `(weekday, start, end)` in minutes, Monday 0, end
//! exclusive, 1440 the end of the day. `helper.json` may also give those triples directly.
//! Equal start and end (`22:00-22:00`) is the whole day, as it was.

use chrono::{Datelike as _, Duration, NaiveDateTime, NaiveTime, Timelike as _};
use serde_json::Value;

/// One quiet window on one weekday.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Window {
    /// Monday 0 to Sunday 6.
    pub weekday: u8,
    /// Minutes after midnight, inclusive.
    pub start: u16,
    /// Minutes after midnight, exclusive; 1440 is midnight at the day's end.
    pub end: u16,
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Python's `repr` of a string, as protocol 1's messages quoted the entry.
fn repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// `HH:MM` (or `HH`) in minutes after midnight.
fn minutes(text: &str, raw: &str) -> Result<u16, String> {
    let (hour, minute) = text.trim().split_once(':').unwrap_or((text.trim(), ""));
    let num = |s: &str, max: u16| -> Option<u16> {
        let s = s.trim();
        if s.is_empty() || s.len() > 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse::<u16>().ok().filter(|n| *n <= max)
    };
    let h = num(hour, 24);
    let m = if minute.trim().is_empty() { Some(0) } else { num(minute, 59) };
    match (h, m) {
        (Some(h), Some(m)) if h * 60 + m <= 1440 => Ok(h * 60 + m),
        _ => Err(format!("Quiet hours look like 22:00-06:00, not {}", repr(raw))),
    }
}

/// Reads the quiet hours of a command line (`--quiet`, repeatable) or a file (`quiet_hours`):
/// window strings, or `[weekday, start, end]` triples.
///
/// # Errors
/// Protocol 1's messages for an unknown weekday or a window it cannot read.
pub fn parse(entries: &[Value]) -> Result<Vec<Window>, String> {
    let mut out = Vec::new();
    for raw in entries {
        match raw {
            Value::String(s) => parse_text(s, &mut out)?,
            Value::Array(t) => out.push(triple(t)?),
            other => {
                return Err(format!("Quiet hours look like 22:00-06:00, not {other}"));
            }
        }
    }
    Ok(out)
}

fn triple(t: &[Value]) -> Result<Window, String> {
    let n = |i: usize, max: u64| t.get(i).and_then(Value::as_u64).filter(|v| *v <= max);
    match (t.len(), n(0, 6), n(1, 1440), n(2, 1440)) {
        (3, Some(w), Some(s), Some(e)) => Ok(Window {
            weekday: u8::try_from(w).unwrap_or(0),
            start: u16::try_from(s).unwrap_or(0),
            end: u16::try_from(e).unwrap_or(0),
        }),
        _ => Err(format!(
            "A quiet window as numbers is [weekday 0-6, start minute, end minute], not {}",
            Value::Array(t.to_vec())
        )),
    }
}

fn parse_text(raw: &str, out: &mut Vec<Window>) -> Result<(), String> {
    let text = raw.trim();
    let parts: Vec<&str> = text.split_whitespace().collect();
    let (days, window): (Vec<u8>, &str) = if parts.len() == 2 {
        let key: String = parts[0].chars().take(3).collect::<String>().to_lowercase();
        let Some(day) = DAYS.iter().position(|d| *d == key) else {
            return Err(format!("Unknown weekday in quiet hours: {}", repr(parts[0])));
        };
        (vec![u8::try_from(day).unwrap_or(0)], parts[1])
    } else {
        ((0..7).collect(), text)
    };
    let Some((start, end)) = window.split_once('-') else {
        return Err(format!("Quiet hours look like 22:00-06:00, not {}", repr(raw)));
    };
    let (s, e) = (minutes(start, raw)?, minutes(end, raw)?);
    for weekday in days {
        if s < e {
            out.push(Window { weekday, start: s, end: e });
        } else {
            out.push(Window { weekday, start: s, end: 1440 });
            out.push(Window { weekday: (weekday + 1) % 7, start: 0, end: e });
        }
    }
    Ok(())
}

/// The machine's quiet hours.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Quiet(pub Vec<Window>);

impl Quiet {
    /// Whether the quiet hours hold at the local time `t` (to the minute, as protocol 1).
    #[must_use]
    pub fn in_force(&self, t: NaiveDateTime) -> bool {
        let day = u8::try_from(t.weekday().num_days_from_monday()).unwrap_or(0);
        let minute = u16::try_from(t.hour() * 60 + t.minute()).unwrap_or(0);
        self.0.iter().any(|w| w.weekday == day && w.start <= minute && minute < w.end)
    }

    /// When the quiet stretch in force at `t` ends (local time), following windows that meet
    /// across midnight; `None` when none is in force. The answer is a whole minute, the same for
    /// every `t` in one stretch, so the session's `status` (sent when it changes) goes once.
    ///
    /// Quiet every minute of the week never ends: it is reported as ending at the start (Monday
    /// 00:00) of the week after next, more than a week away and the same all week long, so the
    /// server is told again once a week rather than never (and not every second).
    #[must_use]
    pub fn until(&self, t: NaiveDateTime) -> Option<NaiveDateTime> {
        if !self.in_force(t) {
            return None;
        }
        // Any minute of the week that is not quiet comes round within a week of `t`, so a
        // stretch that reaches a week on is the whole week.
        let limit = t + Duration::days(7);
        let mut cur = t;
        while self.in_force(cur) {
            if cur >= limit {
                let monday = t.date()
                    - Duration::days(i64::from(t.weekday().num_days_from_monday()))
                    + Duration::days(14);
                return Some(monday.and_time(NaiveTime::MIN));
            }
            let day = u8::try_from(cur.weekday().num_days_from_monday()).unwrap_or(0);
            let minute = u16::try_from(cur.hour() * 60 + cur.minute()).unwrap_or(0);
            let end = self
                .0
                .iter()
                .filter(|w| w.weekday == day && w.start <= minute && minute < w.end)
                .map(|w| w.end)
                .max()
                .unwrap_or(minute + 1);
            cur = cur.date().and_time(NaiveTime::MIN) + Duration::minutes(i64::from(end));
        }
        Some(cur)
    }

    /// The first moment at or after `t` when quiet hours hold, within a week: what the
    /// transcripts' fake clock jumps to when "the quiet hours begin".
    #[must_use]
    pub fn next_start(&self, t: NaiveDateTime) -> Option<NaiveDateTime> {
        let base = t.date().and_time(NaiveTime::MIN);
        let mut best: Option<NaiveDateTime> = None;
        for d in 0..8 {
            let day = base + Duration::days(d);
            let wd = u8::try_from(day.weekday().num_days_from_monday()).unwrap_or(0);
            for w in self.0.iter().filter(|w| w.weekday == wd && w.start < w.end) {
                let at = day + Duration::minutes(i64::from(w.start));
                let hit = if at >= t {
                    Some(at)
                } else if self.in_force(t) {
                    Some(t)
                } else {
                    None
                };
                if let Some(h) = hit {
                    best = Some(best.map_or(h, |b| b.min(h)));
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn w(weekday: u8, start: u16, end: u16) -> Window {
        Window { weekday, start, end }
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").expect("a time")
    }

    #[test]
    fn protocol_one_windows_read_as_they_did() {
        let every = parse(&[json!("22:00-06:00")]).expect("reads");
        assert_eq!(every.len(), 14);
        assert_eq!(&every[..2], &[w(0, 1320, 1440), w(1, 0, 360)]);
        assert_eq!(every[13], w(0, 0, 360), "Sunday's wraps to Monday");
        assert_eq!(parse(&[json!("Sat 00:00-09:00")]).expect("reads"), [w(5, 0, 540)]);
        assert_eq!(
            parse(&[json!("Sunday 22:30-01:15")]).expect("reads"),
            [w(6, 1350, 1440), w(0, 0, 75)]
        );
        assert_eq!(parse(&[json!("Mon 8-17")]).expect("reads"), [w(0, 480, 1020)]);
        assert_eq!(
            parse(&[json!("Tue 22:00-22:00")]).expect("reads"),
            [w(1, 1320, 1440), w(2, 0, 1320)]
        );
        assert_eq!(
            parse(&[json!([0, 0, 360]), json!([1, 0, 360])]).expect("reads"),
            [w(0, 0, 360), w(1, 0, 360)]
        );
        assert_eq!(
            parse(&[json!("Xyz 1-2")]).expect_err("weekday"),
            "Unknown weekday in quiet hours: 'Xyz'"
        );
        assert_eq!(
            parse(&[json!("2200")]).expect_err("no dash"),
            "Quiet hours look like 22:00-06:00, not '2200'"
        );
        assert!(parse(&[json!("25:00-26:00")]).is_err());
        assert!(parse(&[json!("ab:cd-01:00")]).is_err());
        assert!(parse(&[json!([7, 0, 10])]).is_err());
    }

    #[test]
    fn in_force_and_until_follow_windows_across_midnight() {
        let q = Quiet(parse(&[json!("22:00-06:00")]).expect("reads"));
        // 2026-10-14 is a Wednesday.
        assert!(!q.in_force(at("2026-10-14 21:59")));
        assert!(q.in_force(at("2026-10-14 22:00")));
        assert!(q.in_force(at("2026-10-15 05:59")));
        assert!(!q.in_force(at("2026-10-15 06:00")));
        assert_eq!(q.until(at("2026-10-14 23:10")), Some(at("2026-10-15 06:00")));
        assert_eq!(q.until(at("2026-10-14 12:00")), None);
        assert_eq!(q.next_start(at("2026-10-14 12:00")), Some(at("2026-10-14 22:00")));
        assert_eq!(q.next_start(at("2026-10-14 23:00")), Some(at("2026-10-14 23:00")));
        assert_eq!(Quiet::default().next_start(at("2026-10-14 12:00")), None);
    }

    fn at_s(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").expect("a time")
    }

    #[test]
    fn until_is_the_same_all_through_a_stretch() {
        // Seconds into the stretch do not move its end.
        let q = Quiet(parse(&[json!("22:00-06:00")]).expect("reads"));
        for t in ["2026-10-14 22:00:00", "2026-10-14 22:00:07", "2026-10-15 05:59:59"] {
            assert_eq!(q.until(at_s(t)), Some(at("2026-10-15 06:00")), "{t}");
        }
        // Quiet all week never ends: the start of the week after next (for the week of Monday
        // 2026-10-19, Monday 2026-11-02), the same every second of that week, more than a week
        // away, and a week later once the week has turned.
        let all = Quiet(parse(&[json!("00:00-00:00")]).expect("reads"));
        let anchor = at("2026-11-02 00:00");
        for t in [
            "2026-10-19 00:00:00",
            "2026-10-19 00:00:01",
            "2026-10-21 12:00:05",
            "2026-10-25 23:59:59",
        ] {
            assert_eq!(all.until(at_s(t)), Some(anchor), "{t}");
            assert!(anchor > at_s(t) + Duration::days(7));
        }
        assert_eq!(all.until(at_s("2026-10-26 00:00:00")), Some(at("2026-11-09 00:00")));
        // Every day but one hour on Wednesdays: a stretch of nearly a week, with its true end.
        let mut almost = parse(&[json!("00:00-00:00")]).expect("reads");
        almost.retain(|w| w.weekday != 2);
        almost.extend([w(2, 0, 13 * 60), w(2, 14 * 60, 1440)]);
        let almost = Quiet(almost);
        assert_eq!(almost.until(at("2026-10-14 14:00")), Some(at("2026-10-21 13:00")));
    }
}

//! The helper's wall clock: frame stamps (`ts`), quiet hours in the machine's local time, and
//! `quiet_until`. Durations and timers use tokio's clock, which the transcripts pause and drive
//! (`setup.helper.clock: "fake"`); a fake wall clock follows that same paused clock, reads its
//! local time as UTC, and can be moved, as when "the quiet hours begin".

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone as _, Utc};

#[derive(Debug)]
enum Kind {
    System,
    Fake { base_unix: f64, base: tokio::time::Instant, offset: Mutex<f64> },
}

/// A wall clock: the system's, or a fake one for the transcripts.
#[derive(Clone, Debug)]
pub struct Clock(Arc<Kind>);

impl Default for Clock {
    fn default() -> Self {
        Clock::system()
    }
}

impl Clock {
    /// The system's clock, in the machine's time zone.
    #[must_use]
    pub fn system() -> Clock {
        Clock(Arc::new(Kind::System))
    }

    /// A fake clock reading `base_unix` now, advancing with tokio's (paused) clock, in UTC.
    #[must_use]
    pub fn fake(base_unix: f64) -> Clock {
        Clock(Arc::new(Kind::Fake {
            base_unix,
            base: tokio::time::Instant::now(),
            offset: Mutex::new(0.0),
        }))
    }

    /// Unix time, in seconds.
    #[must_use]
    pub fn now_unix(&self) -> f64 {
        match &*self.0 {
            Kind::System => {
                let since = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                since.as_secs_f64()
            }
            Kind::Fake { base_unix, base, offset } => {
                let off = *offset.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                base_unix + base.elapsed().as_secs_f64() + off
            }
        }
    }

    /// The local time now.
    #[must_use]
    pub fn now_local(&self) -> NaiveDateTime {
        self.local_of(self.now_unix())
    }

    /// The local time of a Unix time.
    #[must_use]
    pub fn local_of(&self, unix: f64) -> NaiveDateTime {
        let utc = DateTime::<Utc>::from_timestamp_millis(millis(unix)).unwrap_or_default();
        match &*self.0 {
            Kind::System => utc.with_timezone(&Local).naive_local(),
            Kind::Fake { .. } => utc.naive_utc(),
        }
    }

    /// The Unix time of a local time; a time the clocks skip (spring forward) is read as the
    /// moment after the gap, and one they repeat (fall back) as its first occurrence.
    #[must_use]
    pub fn unix_of(&self, local: NaiveDateTime) -> f64 {
        let secs = match &*self.0 {
            Kind::System => Local
                .from_local_datetime(&local)
                .earliest()
                .or_else(|| {
                    Local.from_local_datetime(&(local + chrono::Duration::hours(1))).earliest()
                })
                .map_or_else(|| local.and_utc().timestamp_millis(), |t| t.timestamp_millis()),
            Kind::Fake { .. } => local.and_utc().timestamp_millis(),
        };
        // Milliseconds since 1970 fit an f64 exactly for millennia.
        #[allow(clippy::cast_precision_loss)]
        let s = secs as f64 / 1000.0;
        s
    }

    /// Moves a fake clock so that it reads `unix` now; the system clock is left alone.
    pub fn jump_to(&self, unix: f64) {
        if let Kind::Fake { offset, .. } = &*self.0 {
            let now = self.now_unix();
            let mut off = offset.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            *off += unix - now;
        }
    }

    /// Whether this is a fake clock.
    #[must_use]
    pub fn is_fake(&self) -> bool {
        matches!(&*self.0, Kind::Fake { .. })
    }
}

fn millis(unix: f64) -> i64 {
    // In range for any date the helper will see; out of range saturates.
    #[allow(clippy::cast_possible_truncation)]
    let m = (unix * 1000.0).round() as i64;
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_fake_clock_follows_the_paused_clock_and_jumps() {
        let c = Clock::fake(1_760_000_000.0);
        assert!((c.now_unix() - 1_760_000_000.0).abs() < 1e-6);
        tokio::time::sleep(std::time::Duration::from_secs(90)).await;
        assert!((c.now_unix() - 1_760_000_090.0).abs() < 1e-6);
        c.jump_to(1_800_000_000.0);
        assert!((c.now_unix() - 1_800_000_000.0).abs() < 1e-6);
        let local = c.now_local();
        assert!((c.unix_of(local) - 1_800_000_000.0).abs() < 1e-3);
        assert!(c.is_fake() && !Clock::system().is_fake());
    }

    #[test]
    fn the_system_clock_round_trips_local_time() {
        let c = Clock::system();
        let now = c.now_unix().floor();
        assert!((c.unix_of(c.local_of(now)) - now).abs() < 1.0);
    }
}

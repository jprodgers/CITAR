//! Power samples (design/phase4-helper.md P4.3.5, P4.2.3): every 10 s the GPUs' draw from
//! `nvidia-smi --query-gpu=power.draw` (all of them together) and the CPU's utilisation from
//! `sysinfo`, sent as one `power` frame a minute, which the server writes as the machine's power
//! rows (`usage.PowerSampler`'s format, which costing reads). `--no-power` turns it off.
//!
//! Where no GPU's draw can be read (no NVIDIA driver, a card that does not report it), the samples
//! carry the CPU alone, and `nvidia-smi` is not run again every 10 s: once its answer has no draw,
//! or it failed a minute's readings in a row after answering, it is asked again an hour later.
//! The program is the one [`Probes`] names (an injected fake in development and test builds).
//!
//! Replaces nothing on a helper (protocol 1's sent no power); the host's own sampler is
//! `citar/usage.py` `PowerSampler` (lines 405-511), whose `_gpu_watts` this reads the same way.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::llm::BoxFuture;
use crate::scan::{Probes, probe};

/// How often a reading is taken.
pub const EVERY: Duration = Duration::from_secs(10);
/// Readings to a `power` frame: a minute's.
pub const PER_FRAME: u32 = 6;
/// How long `nvidia-smi` may take for a reading.
pub const NVIDIA_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a reading may take in all before it is given up.
pub const READ_TIMEOUT: Duration = Duration::from_secs(8);
/// How long the GPUs' draw is not asked for after it could not be read.
pub const GPU_RETRY: Duration = Duration::from_secs(3600);
/// Readings in a row without a draw after which a GPU that answered counts as unreadable.
pub const GPU_MISSES: u32 = 6;

/// One reading: what could be measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// Every GPU's draw together, in watts.
    pub gpu_w: Option<f64>,
    /// The CPU's utilisation since the last reading, a fraction.
    pub cpu_util: Option<f64>,
}

/// What takes the readings: the machine's own meters ([`SystemPower`]), or a test's.
pub trait PowerProbe: Send + Sync {
    /// One reading, or `None` when nothing could be measured.
    fn read(&self) -> BoxFuture<'static, Option<Reading>>;
}

/// `nvidia-smi --query-gpu=power.draw`'s answer, as `PowerSampler._gpu_watts` reads it: the sum
/// of every number in it (a token of digits with at most one point), or `None` when there is
/// none (`[N/A]`, nothing).
#[must_use]
pub fn gpu_watts(out: &str) -> Option<f64> {
    let vals: Vec<f64> = out
        .split_whitespace()
        .filter(|t| {
            let d = t.replacen('.', "", 1);
            !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())
        })
        .filter_map(|t| t.parse::<f64>().ok())
        .collect();
    (!vals.is_empty()).then(|| vals.iter().sum())
}

/// Where the GPUs' draw stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gpu {
    /// Not asked yet.
    Unknown,
    /// It answered; this many readings since without a draw.
    Readable(u32),
    /// Not asked again before then.
    Unreadable(Instant),
}

struct Meter {
    sys: sysinfo::System,
    gpu: Gpu,
}

impl Meter {
    fn read(&mut self, probes: &Probes) -> Reading {
        self.sys.refresh_cpu_usage();
        let used = f64::from(self.sys.global_cpu_usage());
        let cpu_util = used.is_finite().then(|| (used / 100.0).clamp(0.0, 1.0));
        let gpu_w = match self.gpu {
            Gpu::Unreadable(until) if Instant::now() < until => None,
            _ => {
                let w = gpu_watts(&probe::run(
                    &probes.nvidia_smi,
                    &["--query-gpu=power.draw", "--format=csv,noheader,nounits"],
                    NVIDIA_TIMEOUT,
                ));
                self.gpu = match (w, self.gpu) {
                    (Some(_), _) => Gpu::Readable(0),
                    (None, Gpu::Readable(n)) if n + 1 < GPU_MISSES => Gpu::Readable(n + 1),
                    (None, _) => Gpu::Unreadable(Instant::now() + GPU_RETRY),
                };
                w
            }
        };
        Reading {
            gpu_w: gpu_w.map(|w| crate::scan::round_to(w, 2)),
            cpu_util: cpu_util.map(|u| crate::scan::round_to(u, 4)),
        }
    }
}

/// The machine's own meters: `nvidia-smi` (as [`Probes`] names it) and the CPU's utilisation.
pub struct SystemPower {
    probes: Probes,
    meter: Arc<Mutex<Meter>>,
}

impl SystemPower {
    /// Meters for this machine; the CPU's utilisation counts from now.
    #[must_use]
    pub fn new(probes: Probes) -> SystemPower {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_usage();
        SystemPower { probes, meter: Arc::new(Mutex::new(Meter { sys, gpu: Gpu::Unknown })) }
    }
}

impl PowerProbe for SystemPower {
    fn read(&self) -> BoxFuture<'static, Option<Reading>> {
        let probes = self.probes.clone();
        let meter = self.meter.clone();
        Box::pin(async move {
            // On a blocking thread (it runs nvidia-smi), and given up after READ_TIMEOUT: the
            // lock keeps a slow reading from overlapping the next.
            let job = tokio::task::spawn_blocking(move || {
                meter.lock().unwrap_or_else(PoisonError::into_inner).read(&probes)
            });
            let r = tokio::time::timeout(READ_TIMEOUT, job).await.ok()?.ok()?;
            (r.gpu_w.is_some() || r.cpu_util.is_some()).then_some(r)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draw_reads_as_the_hosts_sampler_reads_it() {
        assert_eq!(gpu_watts("5.53\n"), Some(5.53));
        assert_eq!(gpu_watts("120.50\n80.25\n"), Some(200.75));
        assert_eq!(gpu_watts("[N/A]\n"), None);
        assert_eq!(gpu_watts(""), None);
        assert_eq!(gpu_watts("1.2.3 -4 7"), Some(7.0));
    }

    #[test]
    fn a_gpu_without_a_draw_is_asked_again_an_hour_later() {
        let probes = Probes { nvidia_smi: "citar-no-such-nvidia-smi".into(), ..Probes::system() };
        let mut m = Meter { sys: sysinfo::System::new(), gpu: Gpu::Unknown };
        let r = m.read(&probes);
        assert_eq!(r.gpu_w, None);
        assert!(r.cpu_util.is_some_and(|u| (0.0..=1.0).contains(&u)));
        assert!(
            matches!(m.gpu, Gpu::Unreadable(t) if t > Instant::now() + Duration::from_secs(3500))
        );
        // one that answered is given a minute of misses first
        m.gpu = Gpu::Readable(0);
        for n in 1..GPU_MISSES {
            m.read(&probes);
            assert_eq!(m.gpu, Gpu::Readable(n));
        }
        m.read(&probes);
        assert!(matches!(m.gpu, Gpu::Unreadable(_)));
    }
}

//! The job host's limits (design/phase4-helper.md P4.3.6's table, P4.4.3): what the host puts on
//! itself at start, before it plays anything, and how hard each one holds.
//!
//! | OS | CPU (`cpu_percent` of the whole machine) | Memory (`memory_mb`, all runs together) | Held |
//! |---|---|---|---|
//! | Windows | a Job Object the host puts itself in, `CpuRate = percent x 100` with `HARD_CAP`, at `BELOW_NORMAL_PRIORITY_CLASS` | the same Job Object's `JobMemoryLimit`, `KILL_ON_JOB_CLOSE` | hard, hard |
//! | Linux in a scope | the parent's `systemd-run --user --scope` set `CPUQuota`; the host reads its cgroup's `cpu.max` to confirm | `MemoryMax` (and `MemorySwapMax=0`), read back from `memory.max` | hard, hard |
//! | Linux otherwise | `nice 10`, and the thread cap | `RLIMIT_DATA` | soft, hard |
//! | macOS | the background band (`PRIO_DARWIN_BG`), and the thread cap | a watchdog reading the resident size every 500 ms, which ends the host past the limit | soft, soft |
//!
//! "Hard" means the operating system holds the figure whatever the games do; "soft" that the host
//! only lowers its priority or watches. `threads` is the most runs at once on every OS: the host
//! refuses more (`busy`). A limit that cannot be put in place is a refusal to start (`limits`),
//! never a host running without it: on Windows any failed call; on Linux an `RLIMIT_DATA` the
//! kernel refuses. A lowered priority that the OS refuses is only noted.
//!
//! **Memory deaths.** Past a hard memory limit an allocation fails (a Job Object's limit,
//! `RLIMIT_DATA`), Rust prints `memory allocation of N bytes failed` and aborts; or the kernel
//! kills the host (a cgroup's `memory.max`, its `oom_kill` count rising); or, on macOS, the
//! watchdog prints [`WATCHDOG_LINE`] and exits with [`EXIT_MEMORY`]. The parent classifies the
//! death from these (`host::parent`).

use serde::{Deserialize, Serialize};

/// How hard the OS holds a limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hardness {
    Hard,
    Soft,
}

/// How hard the CPU and the memory limits hold here: `ready.limits`, and the helper's
/// `hello.cpu.limits` and `settings_ack.limits`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Held {
    pub cpu: Hardness,
    pub memory: Hardness,
}

/// The most runs a host takes at once (the protocol's bound on `threads`).
pub const MAX_THREADS: u32 = 1024;
/// The most memory a host may be given, in MB (the protocol's bound on `memory_mb`).
pub const MAX_MEMORY_MB: u64 = 16_777_216;
/// The nice value of a host on Linux and other Unix systems: lent CPU yields to its owner's own
/// work.
pub const NICE: i32 = 10;
/// The line the macOS watchdog prints before it ends a host past its memory limit.
pub const WATCHDOG_LINE: &str = "citar job host: resident memory passed the limit";
/// The exit code of a host the macOS watchdog ended.
pub const EXIT_MEMORY: i32 = 3;
/// How often the macOS watchdog reads the resident size.
pub const WATCHDOG_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

/// The limits a parent asks a host to hold: the effective figures (P4.2.3), each concrete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requested {
    /// The most runs at once.
    pub threads: u32,
    /// The share of the whole machine's CPU, 1 to 100.
    pub cpu_percent: u32,
    /// The memory of every run together, and the host's own, in MB.
    pub memory_mb: u64,
}

impl Requested {
    /// Whether the figures are within their bounds: 1 to [`MAX_THREADS`] threads, 1 to 100
    /// percent, 1 to [`MAX_MEMORY_MB`] MB. A zero is refused rather than read as "no limit".
    ///
    /// # Errors
    /// The figure out of its bounds.
    pub fn check(&self) -> Result<(), String> {
        if !(1..=MAX_THREADS).contains(&self.threads) {
            return Err(format!("threads {} is not 1 to {MAX_THREADS}", self.threads));
        }
        if !(1..=100).contains(&self.cpu_percent) {
            return Err(format!("cpu_percent {} is not 1 to 100", self.cpu_percent));
        }
        if !(1..=MAX_MEMORY_MB).contains(&self.memory_mb) {
            return Err(format!("memory_mb {} is not 1 to {MAX_MEMORY_MB}", self.memory_mb));
        }
        Ok(())
    }

    /// The memory limit in bytes.
    #[must_use]
    pub const fn memory_bytes(&self) -> u64 {
        self.memory_mb.saturating_mul(1 << 20)
    }

    /// The memory each run may use: the limit shared by the threads, in MB.
    #[must_use]
    pub fn memory_per_thread_mb(&self) -> u64 {
        self.memory_mb / u64::from(self.threads.max(1))
    }

    /// The share as cores of a machine with `cpus` logical CPUs.
    #[must_use]
    pub fn cores(&self, cpus: u32) -> f64 {
        f64::from(self.cpu_percent) * f64::from(cpus) / 100.0
    }
}

/// The limits in place: how hard each holds, and what was done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub held: Held,
    /// What was done, one line each, for the host's log.
    pub notes: Vec<String>,
    /// The host's cgroup (Linux), which the parent reads the memory events of.
    pub cgroup: Option<String>,
}

/// What must live as long as the process: on Windows the Job Object, whose last handle closing
/// would end every process in it, this one included (`KILL_ON_JOB_CLOSE`). Kept in a static, so
/// it closes only when the process ends; a process puts its limits in place once.
#[cfg(windows)]
static KEPT: std::sync::OnceLock<crate::sys::JobObject> = std::sync::OnceLock::new();
#[cfg(not(windows))]
static KEPT: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// The logical CPUs of the whole machine, as the parent and the host both count them: on Linux
/// the online CPUs (`/sys/devices/system/cpu/online`), not what a cgroup's quota leaves, which is
/// what `std::thread::available_parallelism` reports inside a scope; elsewhere the standard
/// library's count.
#[must_use]
pub fn machine_cpus() -> u32 {
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/sys/devices/system/cpu/online")
        && let Some(n) = parse_cpu_list(&text)
    {
        return n;
    }
    std::thread::available_parallelism().ok().and_then(|n| u32::try_from(n.get()).ok()).unwrap_or(1)
}

/// Counts a kernel CPU list (`0-11`, `0,2-5`).
#[must_use]
pub fn parse_cpu_list(text: &str) -> Option<u32> {
    let mut n = 0u32;
    for part in text.trim().split(',').filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse::<u32>().ok()?, b.trim().parse::<u32>().ok()?),
            None => {
                let a = part.trim().parse::<u32>().ok()?;
                (a, a)
            }
        };
        n = n.checked_add(b.checked_sub(a)?.checked_add(1)?)?;
    }
    (n > 0).then_some(n)
}

/// Puts `req` in place on this process, per the table above.
///
/// # Errors
/// Figures out of their bounds, or a limit the OS would not put in place (the host then refuses
/// to start: `limits`).
pub fn apply(req: &Requested) -> Result<Applied, String> {
    req.check()?;
    if KEPT.get().is_some() {
        return Err("this process already holds its limits".to_owned());
    }
    apply_here(req)
}

#[cfg(windows)]
fn apply_here(req: &Requested) -> Result<Applied, String> {
    let rate = req.cpu_percent.saturating_mul(100);
    let memory = usize::try_from(req.memory_bytes()).unwrap_or(usize::MAX);
    let job = crate::sys::JobObject::hold_self(rate, memory).map_err(|e| e.to_string())?;
    if KEPT.set(job).is_err() {
        return Err("this process already holds its limits".to_owned());
    }
    Ok(Applied {
        held: Held { cpu: Hardness::Hard, memory: Hardness::Hard },
        notes: vec![format!(
            "a Job Object: CPU rate {rate} (hard cap, {}% of the machine), memory {} MB, below \
             normal priority",
            req.cpu_percent, req.memory_mb
        )],
        cgroup: None,
    })
}

#[cfg(target_os = "linux")]
fn apply_here(req: &Requested) -> Result<Applied, String> {
    let mut notes = Vec::new();
    match crate::sys::nice(NICE) {
        Ok(()) => notes.push(format!("nice {NICE}")),
        Err(e) => notes.push(format!("nice {NICE} refused: {e}")),
    }
    let cgroup = cgroup::own();
    let cpus = machine_cpus();
    let (mut cpu, mut memory) = (Hardness::Soft, Hardness::Soft);
    if let Some(path) = &cgroup {
        if let Some(cores) = cgroup::cpu_cores(path)
            && cores <= req.cores(cpus) * 1.01 + 0.01
        {
            cpu = Hardness::Hard;
            notes.push(format!("cgroup {path}: CPU held to {cores:.2} cores"));
        }
        if let Some(max) = cgroup::memory_max(path)
            && max <= req.memory_bytes()
        {
            memory = Hardness::Hard;
            notes.push(format!("cgroup {path}: memory held to {} MB", max >> 20));
        }
    }
    if cpu == Hardness::Soft {
        notes.push(format!(
            "no cgroup holds the CPU to {:.2} cores: nice and the thread cap only",
            req.cores(cpus)
        ));
    }
    if memory == Hardness::Soft {
        let cap = crate::sys::limit_data(req.memory_bytes())
            .map_err(|e| format!("RLIMIT_DATA of {} MB: {e}", req.memory_mb))?;
        memory = Hardness::Hard;
        notes.push(format!("RLIMIT_DATA {} MB", cap >> 20));
    }
    let _kept = KEPT.set(());
    Ok(Applied { held: Held { cpu, memory }, notes, cgroup })
}

#[cfg(target_os = "macos")]
fn apply_here(req: &Requested) -> Result<Applied, String> {
    let mut notes = Vec::new();
    match crate::sys::background() {
        Ok(()) => notes.push("the background band (PRIO_DARWIN_BG)".to_owned()),
        Err(e) => notes.push(format!("the background band refused: {e}")),
    }
    let limit = req.memory_bytes();
    crate::sys::resident_bytes().map_err(|e| format!("the resident size cannot be read: {e}"))?;
    std::thread::Builder::new()
        .name("citar-memory-watch".to_owned())
        .spawn(move || watch(limit))
        .map_err(|e| format!("the memory watchdog: {e}"))?;
    notes.push(format!(
        "a watchdog ends the host past {} MB resident, every {} ms",
        req.memory_mb,
        WATCHDOG_EVERY.as_millis()
    ));
    let _kept = KEPT.set(());
    Ok(Applied { held: Held { cpu: Hardness::Soft, memory: Hardness::Soft }, notes, cgroup: None })
}

/// The macOS watchdog: past `limit` resident bytes, the host ends, as a Job Object's limit ends a
/// Windows host.
#[cfg(target_os = "macos")]
#[allow(clippy::exit, reason = "a host past its memory limit ends, as the OS would end it")]
fn watch(limit: u64) {
    loop {
        std::thread::sleep(WATCHDOG_EVERY);
        if let Ok(now) = crate::sys::resident_bytes()
            && now > limit
        {
            eprintln!("{WATCHDOG_LINE}: {} MB of {} MB", now >> 20, limit >> 20);
            std::process::exit(EXIT_MEMORY);
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn apply_here(_req: &Requested) -> Result<Applied, String> {
    let _kept = KEPT.set(());
    Ok(Applied {
        held: Held { cpu: Hardness::Soft, memory: Hardness::Soft },
        notes: vec!["no limits on this OS: the thread cap only".to_owned()],
        cgroup: None,
    })
}

/// Reading cgroup v2 (Linux): a process's cgroup, the tightest CPU quota and memory limit over it
/// and its ancestors, and its memory events. The parsers compile on every OS, so their tests run
/// everywhere; the reads are Linux's.
pub mod cgroup {
    /// Where cgroup v2 is mounted.
    pub const ROOT: &str = "/sys/fs/cgroup";

    /// The cgroup v2 path in `/proc/<pid>/cgroup`'s text (its `0::` line).
    #[must_use]
    pub fn parse_proc(text: &str) -> Option<String> {
        text.lines()
            .find_map(|l| l.strip_prefix("0::"))
            .map(|p| p.trim().to_owned())
            .filter(|p| p.starts_with('/'))
    }

    /// A `cpu.max` (`<quota> <period>`, or `max <period>`) as cores; `None` for no quota.
    #[must_use]
    pub fn parse_cpu_max(text: &str) -> Option<f64> {
        let mut it = text.split_whitespace();
        let quota = it.next()?.parse::<u64>().ok()?;
        let period = it.next()?.parse::<u64>().ok().filter(|p| *p > 0)?;
        #[allow(clippy::cast_precision_loss, reason = "microseconds well under 2^52")]
        let cores = quota as f64 / period as f64;
        Some(cores)
    }

    /// A `memory.max` in bytes; `None` for `max`.
    #[must_use]
    pub fn parse_memory_max(text: &str) -> Option<u64> {
        text.trim().parse::<u64>().ok()
    }

    /// The count `key` in a `memory.events` text.
    #[must_use]
    pub fn parse_event(text: &str, key: &str) -> Option<u64> {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once(' ')?;
            (k == key).then(|| v.trim().parse::<u64>().ok()).flatten()
        })
    }

    /// `path` and each of its ancestors, below [`ROOT`].
    fn levels(path: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut p = path.trim_end_matches('/').to_owned();
        while !p.is_empty() {
            out.push(p.clone());
            match p.rfind('/') {
                Some(i) => p.truncate(i),
                None => break,
            }
        }
        out
    }

    fn read(path: &str, file: &str) -> Option<String> {
        std::fs::read_to_string(format!("{ROOT}{path}/{file}")).ok()
    }

    /// This process's cgroup v2 path.
    #[must_use]
    pub fn own() -> Option<String> {
        parse_proc(&std::fs::read_to_string("/proc/self/cgroup").ok()?)
    }

    /// The tightest CPU quota over `path` and its ancestors, as cores.
    #[must_use]
    pub fn cpu_cores(path: &str) -> Option<f64> {
        levels(path)
            .iter()
            .filter_map(|p| parse_cpu_max(&read(p, "cpu.max")?))
            .min_by(f64::total_cmp)
    }

    /// The tightest memory limit over `path` and its ancestors, in bytes.
    #[must_use]
    pub fn memory_max(path: &str) -> Option<u64> {
        levels(path).iter().filter_map(|p| parse_memory_max(&read(p, "memory.max")?)).min()
    }

    /// The `oom_kill` count of the cgroup at `path` (hierarchical: its descendants' kills
    /// included).
    #[must_use]
    pub fn oom_kills(path: &str) -> Option<u64> {
        parse_event(&read(path, "memory.events")?, "oom_kill")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_cgroup_files_read_as_the_kernel_writes_them() {
            let proc = "0::/user.slice/user-1001.slice/user@1001.service/app.slice/run-r1.scope\n";
            assert_eq!(
                parse_proc(proc).as_deref(),
                Some("/user.slice/user-1001.slice/user@1001.service/app.slice/run-r1.scope")
            );
            assert_eq!(parse_proc("12:cpu:/x\n"), None, "a cgroup v1 line is not read");
            assert_eq!(parse_cpu_max("120000 100000\n"), Some(1.2));
            assert_eq!(parse_cpu_max("max 100000\n"), None);
            assert_eq!(parse_memory_max("209715200\n"), Some(209_715_200));
            assert_eq!(parse_memory_max("max\n"), None);
            let events = "low 0\nhigh 0\nmax 4\noom 1\noom_kill 2\noom_group_kill 0\n";
            assert_eq!(parse_event(events, "oom_kill"), Some(2));
            assert_eq!(parse_event(events, "oom"), Some(1));
            assert_eq!(parse_event(events, "absent"), None);
            assert_eq!(levels("/a/b/c"), ["/a/b/c", "/a/b", "/a"]);
            assert_eq!(levels("/"), Vec::<String>::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_figures_are_held_to_their_bounds() {
        let ok = Requested { threads: 2, cpu_percent: 10, memory_mb: 200 };
        assert_eq!(ok.check(), Ok(()));
        assert_eq!(ok.memory_bytes(), 200 << 20);
        assert_eq!(ok.memory_per_thread_mb(), 100);
        assert!((ok.cores(12) - 1.2).abs() < 1e-9);
        for bad in [
            Requested { threads: 0, ..ok },
            Requested { threads: MAX_THREADS + 1, ..ok },
            Requested { cpu_percent: 0, ..ok },
            Requested { cpu_percent: 101, ..ok },
            Requested { memory_mb: 0, ..ok },
            Requested { memory_mb: MAX_MEMORY_MB + 1, ..ok },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
            assert!(apply(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cpu_lists_count_as_the_kernel_writes_them() {
        assert_eq!(parse_cpu_list("0-11\n"), Some(12));
        assert_eq!(parse_cpu_list("0,2-5"), Some(5));
        assert_eq!(parse_cpu_list("3"), Some(1));
        assert_eq!(parse_cpu_list(""), None);
        assert_eq!(parse_cpu_list("5-2"), None);
        assert!(machine_cpus() >= 1);
    }

    #[test]
    fn hardness_reads_as_the_protocol_writes_it() {
        let held = Held { cpu: Hardness::Hard, memory: Hardness::Soft };
        let text = serde_json::to_string(&held).expect("json");
        assert_eq!(text, r#"{"cpu":"hard","memory":"soft"}"#);
        assert_eq!(serde_json::from_str::<Held>(&text).expect("read"), held);
    }
}

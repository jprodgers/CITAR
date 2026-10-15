//! The hardware scan (design/phase4-helper.md P4.3.5): what the machine is, in the
//! `citar-hardware` format `citar/hwinfo.py` `collect` writes (lines 354-395), so the site's
//! readers (3-18's `valid_scan` and `clean_hardware`, the hardware classes, the machine settings
//! dialog) take a helper's scan unchanged. `collector_version` is 2 and `collector` is
//! `citar-helper`.
//!
//! - **What it reads:** `sysinfo` for the host name, the OS, the CPU's name, cores and threads,
//!   the memory and the volumes (in place of the collector's `psutil`); `nvidia-smi --query-gpu`
//!   for NVIDIA GPUs (memory, driver, power limit and draw, whether the draw is readable); then
//!   each OS's own part, read as the collector reads it: [`windows`] (PowerShell's CIM query),
//!   [`linux`] (`/proc`, DMI, `rocm-smi`, `lspci`, `lsblk`, a battery), [`macos`] (`sysctl`,
//!   `system_profiler`). Then the model runtimes (the helper's own endpoint, when it is LM Studio
//!   or Ollama and answered; nothing else is dialled, P4.3.10) and the power guess
//!   ([`guess::guess_power`]).
//! - **Best effort** ([`probe::run`]): a probe that is missing, fails or outlives its timeout
//!   gives less information, never an error.
//! - **Injectable probes:** in a development or test build only (`app::DEV_GUARD`),
//!   `CITAR_HELPER_NVIDIA_SMI`, `CITAR_HELPER_POWERSHELL` and `CITAR_HELPER_SYSTEM_PROFILER` name
//!   the probe programs ([`Probes::from_env`]), so a test runs a fake (`citar-fake-probe`). A fake
//!   first on `PATH` would not do: Windows searches System32, where the NVIDIA driver installs the
//!   real `nvidia-smi.exe`, before `PATH`. A release build never reads them.
//! - **Changes:** [`hash`] leaves out what moves without the machine changing (the scan's time,
//!   a GPU's draw now, a volume's free space), so the session sends a `hardware` frame only when
//!   the machine itself changed (P4.2.3).
//!
//! Differences from the collector, each a fix: GPUs `rocm-smi` and `lspci` both list are listed
//! once (`rocm-smi`'s, as `nvidia-smi`'s already were); a GPU memory size that does not read as a
//! number is left out rather than losing the rest of a Mac's scan; and the lists and texts are
//! held to sizes a hello carries ([`fit`]).

pub mod guess;
pub mod linux;
pub mod macos;
pub mod probe;
pub mod windows;

use std::ffi::OsString;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::llm::BoxFuture;

/// The scan's format, as every `citar-hardware` file says.
pub const FORMAT: &str = "citar-hardware";
/// This collector's version (the Python collector's is 1).
pub const COLLECTOR_VERSION: u32 = 2;
/// This collector's name.
pub const COLLECTOR: &str = "citar-helper";
/// The variables that name the probe programs, in a development or test build only.
pub const PROBE_VARS: [&str; 3] =
    ["CITAR_HELPER_NVIDIA_SMI", "CITAR_HELPER_POWERSHELL", "CITAR_HELPER_SYSTEM_PROFILER"];
/// How long `nvidia-smi` may take in a scan (the collector's default).
pub const NVIDIA_TIMEOUT: Duration = Duration::from_secs(30);
/// The most of each list a scan keeps, and the longest text.
pub const MAX_GPUS: usize = 32;
pub const MAX_DISKS: usize = 64;
pub const MAX_VOLUMES: usize = 64;
pub const MAX_MODULES: usize = 64;
pub const MAX_TEXT: usize = 1000;

/// The programs the scan and the power samples run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probes {
    pub nvidia_smi: OsString,
    pub powershell: OsString,
    pub system_profiler: OsString,
}

impl Probes {
    /// The machine's own programs.
    #[must_use]
    pub fn system() -> Probes {
        Probes {
            nvidia_smi: "nvidia-smi".into(),
            powershell: "powershell".into(),
            system_profiler: "system_profiler".into(),
        }
    }

    /// The machine's own programs, or, in a development or test build (`dev_guard`), those the
    /// [`PROBE_VARS`] name.
    #[must_use]
    pub fn from_env(env: crate::home::Env<'_>, dev_guard: bool) -> Probes {
        let mut p = Probes::system();
        if dev_guard {
            let named = |k: &str| env(k).filter(|v| !v.is_empty());
            if let Some(v) = named(PROBE_VARS[0]) {
                p.nvidia_smi = v;
            }
            if let Some(v) = named(PROBE_VARS[1]) {
                p.powershell = v;
            }
            if let Some(v) = named(PROBE_VARS[2]) {
                p.system_profiler = v;
            }
        }
        p
    }
}

/// A model runtime the scan reports: the helper's endpoint, when it is LM Studio or Ollama and
/// it answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    pub name: String,
    pub endpoint: String,
    /// Each model's key and context length.
    pub models: Vec<(String, Option<u64>)>,
}

// ---------------------------------------------------------------------------- Python's ways

/// Python's truthiness of a JSON value.
#[must_use]
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python's `a or b` of two values a dict's `get` gives.
#[must_use]
pub fn or_value(a: Option<&Value>, b: Option<&Value>) -> Value {
    if truthy(a) { a.cloned() } else { b.cloned() }.unwrap_or(Value::Null)
}

/// Python's `str()` of a JSON value.
#[must_use]
pub fn python_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// The text of a value, or `""` (the collector's `(x or "")` on what it expects to be text).
#[must_use]
pub fn text_or_empty(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// `x if isinstance(x, list) else ([x] if x else [])`.
#[must_use]
pub fn as_list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(x) if truthy(Some(x)) => vec![x.clone()],
        _ => Vec::new(),
    }
}

/// Python's `round(x, digits)`: the decimal nearest the float's exact value, ties to even (as
/// Rust's formatting rounds).
#[must_use]
pub fn round_to(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// The collector's `_gb`: bytes as gigabytes to one place, or null for what is not a number.
#[must_use]
pub fn gb(v: Option<&Value>) -> Value {
    let bytes = match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(Value::Bool(b)) => Some(f64::from(u8::from(*b))),
        _ => None,
    };
    match bytes.filter(|b| b.is_finite()) {
        Some(b) => Value::from(round_to(b / 1_073_741_824.0, 1)),
        None => Value::Null,
    }
}

/// `info[part][key] = value`, making the part an object when it is not one.
pub fn set(info: &mut Map<String, Value>, part: &str, key: &str, value: Value) {
    let slot = info.entry(part.to_owned()).or_insert_with(|| Value::Object(Map::new()));
    if !slot.is_object() {
        *slot = Value::Object(Map::new());
    }
    if let Value::Object(m) = slot {
        m.insert(key.to_owned(), value);
    }
}

/// `info[list].append(value)`, making the list when there is none.
pub fn push(info: &mut Map<String, Value>, list: &str, value: Value) {
    let slot = info.entry(list.to_owned()).or_insert_with(|| Value::Array(Vec::new()));
    if !slot.is_array() {
        *slot = Value::Array(Vec::new());
    }
    if let Value::Array(a) = slot {
        a.push(value);
    }
}

/// The collector's `_merge_gpus`: `extra`'s GPUs added to `info.gpus` unless one already there
/// has the same name (ignoring case), or one name holds the other.
pub fn merge_gpus(info: &mut Map<String, Value>, extra: Vec<Map<String, Value>>) {
    let names: Vec<String> = info
        .get("gpus")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|g| text_or_empty(g.get("name")).to_lowercase())
        .collect();
    for g in extra {
        let name = text_or_empty(g.get("name")).to_lowercase();
        let known =
            names.iter().any(|n| *n == name || name.contains(n.as_str()) || n.contains(&name));
        if !known {
            push(info, "gpus", Value::Object(g));
        }
    }
}

// ---------------------------------------------------------------------------- the probes

/// `nvidia-smi --query-gpu=name,memory.total,driver_version,power.limit,power.draw,pci.bus_id
/// --format=csv,noheader,nounits`, read as the collector's `_nvidia` reads it.
#[must_use]
pub fn nvidia(out: &str) -> Vec<Map<String, Value>> {
    let num = |s: &str| s.parse::<f64>().ok().filter(|x| x.is_finite());
    let mut gpus = Vec::new();
    for line in out.trim().lines() {
        let parts: Vec<&str> = line.split(',').map(str::trim).collect();
        if parts.len() < 6 {
            continue;
        }
        let mut g = Map::new();
        g.insert("name".into(), Value::from(parts[0]));
        g.insert("vendor".into(), Value::from("NVIDIA"));
        g.insert(
            "vram_gb".into(),
            num(parts[1])
                .filter(|mb| *mb != 0.0)
                .map_or(Value::Null, |mb| Value::from(round_to(mb / 1024.0, 1))),
        );
        g.insert("driver".into(), Value::from(parts[2]));
        g.insert("power_limit_w".into(), num(parts[3]).map_or(Value::Null, Value::from));
        g.insert("power_now_w".into(), num(parts[4]).map_or(Value::Null, Value::from));
        g.insert("bus".into(), Value::from(parts[5]));
        g.insert("power_readable".into(), Value::Bool(num(parts[4]).is_some()));
        gpus.push(g);
    }
    gpus
}

/// The NVIDIA GPUs, from the `nvidia-smi` the probes name.
fn nvidia_gpus(probes: &Probes) -> Vec<Map<String, Value>> {
    nvidia(&probe::run(
        &probes.nvidia_smi,
        &[
            "--query-gpu=name,memory.total,driver_version,power.limit,power.draw,pci.bus_id",
            "--format=csv,noheader,nounits",
        ],
        NVIDIA_TIMEOUT,
    ))
}

/// `platform.system()`'s and `platform.machine()`'s names for this build's OS and architecture.
fn os_names() -> (&'static str, &'static str) {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => ("Windows", if arch == "aarch64" { "ARM64" } else { "AMD64" }),
        "macos" => ("Darwin", if arch == "aarch64" { "arm64" } else { arch }),
        "linux" => ("Linux", arch),
        other => (other, arch),
    }
}

/// The `os` part as Python's `platform` names it: `system()`, `release()`, `version()`,
/// `platform()` (near enough: it is shown, never read), `machine()`.
fn os_block() -> Value {
    let (system, arch) = os_names();
    let kernel = sysinfo::System::kernel_version().unwrap_or_default();
    let (release, version, platform) = match std::env::consts::OS {
        "windows" => {
            let release = sysinfo::System::os_version()
                .and_then(|v| v.split_whitespace().next().map(str::to_owned))
                .unwrap_or_default();
            // Windows 10 and 11 are NT 10.0; `kernel_version` is the build.
            let version = if !kernel.is_empty() && kernel.chars().all(|c| c.is_ascii_digit()) {
                format!("10.0.{kernel}")
            } else {
                kernel.clone()
            };
            let platform = format!("{system}-{release}-{version}");
            (release, version, platform)
        }
        "macos" => {
            let os = sysinfo::System::os_version().unwrap_or_default();
            (kernel.clone(), sysinfo::System::kernel_long_version(), format!("macOS-{os}-{arch}"))
        }
        _ => {
            // `uname -v`, as platform.version() gives it on Linux.
            let version = std::fs::read_to_string("/proc/sys/kernel/version")
                .map(|v| v.trim().to_owned())
                .unwrap_or_default();
            (kernel.clone(), version, format!("{system}-{kernel}-{arch}"))
        }
    };
    serde_json::json!({"system": system, "release": release, "version": version,
                       "platform": platform, "arch": arch})
}

/// The file systems Linux mounts from a device (`/proc/filesystems` without `nodev`), as
/// `psutil.disk_partitions(all=False)` keeps them; `None` where there is no such list.
fn device_file_systems() -> Option<Vec<String>> {
    let text = std::fs::read_to_string("/proc/filesystems").ok()?;
    Some(
        text.lines()
            .filter(|l| !l.starts_with("nodev"))
            .map(|l| l.trim().to_owned())
            .filter(|f| !f.is_empty())
            .collect(),
    )
}

/// The volumes and their free space, as the collector's `_volumes`.
fn volumes(info: &mut Map<String, Value>) {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let real = if cfg!(target_os = "linux") { device_file_systems() } else { None };
    let mut seen: Vec<(u64, u64)> = Vec::new();
    for d in disks.list() {
        let fs = d.file_system().to_string_lossy().into_owned();
        if ["", "squashfs", "tmpfs", "devtmpfs", "overlay"].contains(&fs.as_str()) {
            continue;
        }
        if let Some(real) = &real
            && !real.contains(&fs)
        {
            continue;
        }
        let key = (d.total_space(), d.available_space());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let mut v = Map::new();
        v.insert("mount".into(), Value::from(d.mount_point().to_string_lossy().into_owned()));
        v.insert("fs".into(), Value::from(fs));
        v.insert("total_gb".into(), gb(Some(&Value::from(d.total_space()))));
        v.insert("free_gb".into(), gb(Some(&Value::from(d.available_space()))));
        push(info, "volumes", Value::Object(v));
    }
}

/// Everything this machine is: the scan, as `hwinfo.collect` gives it (the module's doc), at the
/// local time `collected_at` (`%Y-%m-%dT%H:%M:%S`). Blocking: it runs the probe programs.
#[must_use]
pub fn collect(probes: &Probes, runtimes: &[Runtime], collected_at: &str) -> Map<String, Value> {
    let sys = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing()
            .with_cpu(sysinfo::CpuRefreshKind::nothing())
            .with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()),
    );
    let brand = sys.cpus().first().map(|c| c.brand().trim().to_owned()).unwrap_or_default();
    let gpus = nvidia_gpus(probes);
    let sampling = gpus.iter().any(|g| g.get("power_readable") == Some(&Value::Bool(true)));
    let mut info: Map<String, Value> = Map::new();
    info.insert("format".into(), Value::from(FORMAT));
    info.insert("collector_version".into(), Value::from(COLLECTOR_VERSION));
    info.insert("collector".into(), Value::from(COLLECTOR));
    info.insert("collected_at".into(), Value::from(collected_at));
    info.insert("hostname".into(), Value::from(sysinfo::System::host_name().unwrap_or_default()));
    info.insert("os".into(), os_block());
    info.insert("system".into(), Value::Object(Map::new()));
    info.insert(
        "cpu".into(),
        serde_json::json!({
            "model": if brand.is_empty() { Value::Null } else { Value::from(brand) },
            "cores": sysinfo::System::physical_core_count().map_or(Value::Null, Value::from),
            "threads": Value::from(sys.cpus().len()),
        }),
    );
    info.insert(
        "memory".into(),
        serde_json::json!({"ram_gb": gb(Some(&Value::from(sys.total_memory()))), "unified": false}),
    );
    info.insert("gpus".into(), Value::Array(gpus.into_iter().map(Value::Object).collect()));
    for list in ["physical_disks", "volumes", "runtimes"] {
        info.insert(list.into(), Value::Array(Vec::new()));
    }
    info.insert("power".into(), serde_json::json!({"gpu_sampling": sampling}));
    #[cfg(windows)]
    windows::probe(&mut info, &probes.powershell);
    #[cfg(target_os = "linux")]
    linux::probe(&mut info);
    #[cfg(target_os = "macos")]
    macos::probe(&mut info, &probes.system_profiler, cfg!(target_arch = "aarch64"));
    volumes(&mut info);
    for rt in runtimes {
        let models: Vec<Value> = rt
            .models
            .iter()
            .map(|(key, context)| serde_json::json!({"key": key, "max_context": context}))
            .collect();
        push(
            &mut info,
            "runtimes",
            serde_json::json!({"name": rt.name, "endpoint": rt.endpoint, "models": models}),
        );
    }
    let suggested = guess::guess_power(&info);
    set(&mut info, "power", "suggested", Value::Object(suggested));
    fit(&mut info);
    info
}

/// Holds a scan to sizes a hello carries: each list cut to its most, each text to
/// [`MAX_TEXT`] characters.
pub fn fit(scan: &mut Map<String, Value>) {
    for (list, most) in
        [("gpus", MAX_GPUS), ("physical_disks", MAX_DISKS), ("volumes", MAX_VOLUMES)]
    {
        if let Some(Value::Array(a)) = scan.get_mut(list) {
            a.truncate(most);
        }
    }
    if let Some(Value::Array(a)) = scan.get_mut("memory").and_then(|m| m.get_mut("modules")) {
        a.truncate(MAX_MODULES);
    }
    if let Some(Value::Array(runtimes)) = scan.get_mut("runtimes") {
        runtimes.truncate(8);
        for rt in runtimes {
            if let Some(Value::Array(models)) = rt.get_mut("models") {
                models.truncate(crate::llm::MAX_MODELS);
            }
        }
    }
    fn cut(v: &mut Value) {
        match v {
            Value::String(s) if s.chars().count() > MAX_TEXT => {
                *s = s.chars().take(MAX_TEXT).collect();
            }
            Value::Array(a) => a.iter_mut().for_each(cut),
            Value::Object(m) => m.values_mut().for_each(cut),
            _ => {}
        }
    }
    scan.values_mut().for_each(cut);
}

/// What identifies a scan's machine: its BLAKE3, in hex, over the scan without what moves on
/// its own (when it was taken, each GPU's draw now, each volume's free space).
#[must_use]
pub fn hash(scan: &Map<String, Value>) -> String {
    let mut s = scan.clone();
    s.remove("collected_at");
    for (list, field) in [("gpus", "power_now_w"), ("volumes", "free_gb")] {
        if let Some(Value::Array(a)) = s.get_mut(list) {
            for item in a.iter_mut().filter_map(Value::as_object_mut) {
                item.remove(field);
            }
        }
    }
    let bytes = serde_json::to_vec(&s).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

/// What makes a scan: the machine's own probes ([`SystemScanner`]), or a test's.
pub trait Scanner: Send + Sync {
    /// A new scan, with these runtimes; `None` when it could not be made.
    fn scan(&self, runtimes: Vec<Runtime>) -> BoxFuture<'static, Option<Map<String, Value>>>;
}

/// The machine's own scan, on a blocking thread.
pub struct SystemScanner {
    pub probes: Probes,
    pub clock: crate::clock::Clock,
}

impl Scanner for SystemScanner {
    fn scan(&self, runtimes: Vec<Runtime>) -> BoxFuture<'static, Option<Map<String, Value>>> {
        let probes = self.probes.clone();
        let at = self.clock.now_local().format("%Y-%m-%dT%H:%M:%S").to_string();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || collect(&probes, &runtimes, &at)).await.ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)] // exactly the floats Python's round gives
    fn rounding_is_pythons() {
        // round(1.25, 1), round(0.125, 2) and round(2.675, 2) in Python
        assert_eq!(round_to(1.25, 1), 1.2);
        assert_eq!(round_to(0.125, 2), 0.12);
        assert_eq!(round_to(2.675, 2), 2.67);
        assert_eq!(round_to(15.710_906_982_421_875, 1), 15.7);
        assert_eq!(gb(Some(&Value::from(16_869_351_424u64))), Value::from(15.7));
        assert_eq!(gb(Some(&Value::from("6442450944"))), Value::from(6.0));
        assert_eq!(gb(None), Value::Null);
        assert_eq!(gb(Some(&Value::from("lots"))), Value::Null);
    }

    #[test]
    fn nvidia_smi_reads_as_the_collector_reads_it() {
        let g = nvidia(
            "NVIDIA GeForce RTX 3050 6GB Laptop GPU, 6144, 546.18, [N/A], 5.53, 00000000:01:00.0\n\nshort, line\n",
        );
        assert_eq!(g.len(), 1);
        assert_eq!(
            Value::Object(g[0].clone()),
            serde_json::json!({
            "name": "NVIDIA GeForce RTX 3050 6GB Laptop GPU", "vendor": "NVIDIA", "vram_gb": 6.0,
            "driver": "546.18", "power_limit_w": null, "power_now_w": 5.53,
            "bus": "00000000:01:00.0", "power_readable": true})
        );
        assert!(nvidia("").is_empty());
    }

    #[test]
    fn the_hash_ignores_what_moves_on_its_own() {
        let a: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "format": FORMAT, "collected_at": "2026-10-15T06:00:00",
            "gpus": [{"name": "g", "power_now_w": 5.0}], "volumes": [{"mount": "/", "free_gb": 10.0}],
            "cpu": {"cores": 8}})).expect("a map");
        let mut b = a.clone();
        b["collected_at"] = Value::from("2026-10-15T12:00:00");
        b["gpus"][0]["power_now_w"] = Value::from(90.0);
        b["volumes"][0]["free_gb"] = Value::from(9.0);
        assert_eq!(hash(&a), hash(&b));
        b["cpu"]["cores"] = Value::from(16);
        assert_ne!(hash(&a), hash(&b));
    }

    #[test]
    fn a_scan_is_held_to_what_a_hello_carries() {
        let mut s: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "gpus": (0..100).map(|i| serde_json::json!({"name": format!("g{i}")})).collect::<Vec<_>>(),
            "cpu": {"model": "x".repeat(5000)}})).expect("a map");
        fit(&mut s);
        assert_eq!(s["gpus"].as_array().map(Vec::len), Some(MAX_GPUS));
        assert_eq!(s["cpu"]["model"].as_str().map(str::len), Some(MAX_TEXT));
    }

    #[test]
    fn probes_are_named_only_in_a_development_build() {
        let env = |k: &str| (k == PROBE_VARS[0]).then(|| OsString::from("C:/fake/nvidia-smi.exe"));
        assert_eq!(Probes::from_env(&env, false), Probes::system());
        assert_eq!(
            Probes::from_env(&env, true).nvidia_smi,
            OsString::from("C:/fake/nvidia-smi.exe")
        );
        assert_eq!(Probes::from_env(&env, true).powershell, Probes::system().powershell);
    }

    #[test]
    fn python_or_and_lists() {
        assert_eq!(or_value(Some(&Value::from("")), None), Value::Null);
        assert_eq!(or_value(Some(&Value::Null), Some(&Value::from(""))), Value::from(""));
        assert_eq!(as_list(Some(&serde_json::json!({"a": 1}))).len(), 1);
        assert!(as_list(Some(&serde_json::json!({}))).is_empty());
        assert_eq!(python_str(&Value::Bool(true)), "True");
    }
}

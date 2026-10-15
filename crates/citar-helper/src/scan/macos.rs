//! The macOS part of the scan: `citar/hwinfo.py` `_macos` (lines 225-268), read the same way:
//! `sysctl` (the CPU's name, its cores and threads, the memory), `system_profiler
//! SPDisplaysDataType SPHardwareDataType SPNVMeDataType -json` (the model and its form, Apple
//! silicon's performance and efficiency cores, the GPUs, the NVMe disks), and Apple silicon's
//! unified memory.
//!
//! [`apply_profile`] reads `system_profiler`'s JSON and is compiled on every OS, so its tests run
//! everywhere; only [`probe`] runs the programs.

use serde_json::{Map, Value};

use super::{gb, push, round_to, set, text_or_empty};

/// How long `system_profiler` may take (the collector's).
pub const PROFILER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Reads the machine and applies what it says to `info`; `apple` is Apple silicon.
#[cfg(target_os = "macos")]
pub fn probe(info: &mut Map<String, Value>, system_profiler: &std::ffi::OsStr, apple: bool) {
    use std::ffi::OsStr;
    let sysctl = |key: &str| {
        super::probe::run(OsStr::new("sysctl"), &["-n", key], std::time::Duration::from_secs(30))
            .trim()
            .to_owned()
    };
    let brand = sysctl("machdep.cpu.brand_string");
    let cores = sysctl("hw.physicalcpu");
    let threads = sysctl("hw.logicalcpu");
    let memsize = sysctl("hw.memsize");
    let freq = sysctl("hw.cpufrequency_max");
    apply_sysctl(info, &brand, &cores, &threads, &memsize, &freq, apple);
    let profile = super::probe::run(
        system_profiler,
        &["SPDisplaysDataType", "SPHardwareDataType", "SPNVMeDataType", "-json"],
        PROFILER_TIMEOUT,
    );
    apply_profile(info, &profile, apple);
}

fn digits(s: &str) -> Option<u64> {
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// What `sysctl -n` said: the CPU's name, cores and threads, the memory, and (`psutil`'s
/// `cpu_freq().max`) the CPU's highest clock where macOS gives one (Intel Macs).
pub fn apply_sysctl(
    info: &mut Map<String, Value>,
    brand: &str,
    cores: &str,
    threads: &str,
    memsize: &str,
    freq: &str,
    apple: bool,
) {
    if !brand.is_empty() {
        set(info, "cpu", "model", Value::from(brand));
    }
    if let Some(n) = digits(cores) {
        set(info, "cpu", "cores", Value::from(n));
    }
    if let Some(n) = digits(threads) {
        set(info, "cpu", "threads", Value::from(n));
    }
    if let Some(n) = digits(memsize) {
        set(info, "memory", "ram_gb", gb(Some(&Value::from(n))));
    }
    if let Some(hz) = digits(freq).filter(|h| *h > 0) {
        #[allow(clippy::cast_precision_loss)]
        let ghz = hz as f64 / 1e9;
        set(info, "cpu", "max_ghz", Value::from(round_to(ghz, 2)));
    }
    set(info, "memory", "unified", Value::Bool(apple));
}

/// `proc (\d+):(\d+):(\d+)` in Apple silicon's `number_processors`: the performance and
/// efficiency cores.
fn proc_cores(s: &str) -> Option<(u64, u64)> {
    let mut rest = s;
    while let Some(i) = rest.find("proc ") {
        rest = &rest[i + 5..];
        let parts: Vec<&str> = rest.splitn(3, ':').collect();
        if parts.len() == 3 {
            let lead = |p: &str| -> Option<u64> {
                let d: String = p.chars().take_while(char::is_ascii_digit).collect();
                (!d.is_empty()).then(|| d.parse().ok()).flatten()
            };
            if digits(parts[0]).is_some()
                && let (Some(p), Some(e)) = (digits(parts[1]), lead(parts[2]))
            {
                return Some((p, e));
            }
        }
    }
    None
}

/// `([\d.]+)\s*(GB|MB)` at the start of a memory size, in gigabytes.
fn vram_gb(s: &str) -> Option<f64> {
    let num: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    if num.is_empty() {
        return None;
    }
    let unit = s[num.len()..].trim_start();
    let div = if unit.starts_with("GB") {
        1.0
    } else if unit.starts_with("MB") {
        1024.0
    } else {
        return None;
    };
    num.parse::<f64>().ok().map(|x| x / div)
}

/// Applies `system_profiler`'s JSON (`raw`, empty when it failed) to `info`, as `_macos`.
pub fn apply_profile(info: &mut Map<String, Value>, raw: &str, apple: bool) {
    let sp: Map<String, Value> = serde_json::from_str(raw).unwrap_or_default();
    let list = |k: &str| sp.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    for hw in list("SPHardwareDataType") {
        let name = hw.get("machine_name");
        set(info, "system", "model", super::or_value(name, hw.get("machine_model")));
        set(info, "system", "manufacturer", Value::from("Apple"));
        let book = text_or_empty(name).to_lowercase().contains("book");
        set(info, "system", "form", Value::from(if book { "laptop" } else { "desktop" }));
        if apple
            && let Some(procs) = hw.get("number_processors").filter(|v| super::truthy(Some(v)))
            && let Some((p, e)) = proc_cores(&super::python_str(procs))
        {
            set(info, "cpu", "performance_cores", Value::from(p));
            set(info, "cpu", "efficiency_cores", Value::from(e));
        }
    }
    let ram = info.get("memory").and_then(|m| m.get("ram_gb")).cloned().unwrap_or(Value::Null);
    for d in list("SPDisplaysDataType") {
        let name = super::or_value(d.get("sppci_model"), d.get("_name"));
        let cores = d.get("sppci_cores");
        let vram = Some(super::or_value(d.get("spdisplays_vram"), d.get("spdisplays_vram_shared")))
            .filter(|v| super::truthy(Some(v)))
            .and_then(|v| vram_gb(&super::python_str(&v)));
        let cores_str = if super::truthy(cores) {
            cores.map(super::python_str).unwrap_or_default()
        } else {
            String::new()
        };
        let mut g = Map::new();
        g.insert("name".into(), name);
        g.insert(
            "vendor".into(),
            if apple {
                Value::from("Apple")
            } else {
                d.get("spdisplays_vendor").cloned().unwrap_or(Value::Null)
            },
        );
        g.insert(
            "vram_gb".into(),
            if apple { ram.clone() } else { vram.map_or(Value::Null, Value::from) },
        );
        g.insert("gpu_cores".into(), digits(&cores_str).map_or(Value::Null, Value::from));
        g.insert("unified_memory".into(), Value::Bool(apple));
        g.insert("power_readable".into(), Value::Bool(false));
        push(info, "gpus", Value::Object(g));
    }
    for group in list("SPNVMeDataType") {
        for d in group.get("_items").and_then(Value::as_array).into_iter().flatten() {
            let name = super::or_value(d.get("device_model"), d.get("_name"));
            let mut e = Map::new();
            e.insert("name".into(), name);
            e.insert("type".into(), Value::from("SSD"));
            e.insert("bus".into(), Value::from("NVMe"));
            e.insert("size_gb".into(), gb(d.get("size_in_bytes")));
            push(info, "physical_disks", Value::Object(e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AIR: &str = r#"{"SPDisplaysDataType": [{"_name": "Apple M2", "sppci_model": "Apple M2", "sppci_cores": "10", "spdisplays_vendor": "sppci_vendor_Apple"}],
        "SPHardwareDataType": [{"machine_name": "MacBook Air", "machine_model": "Mac14,2", "number_processors": "proc 8:4:4"}],
        "SPNVMeDataType": [{"_items": [{"_name": "APPLE SSD AP0512Z", "device_model": "APPLE SSD AP0512Z", "size_in_bytes": 500277790720}]}]}"#;

    #[test]
    fn an_apple_silicon_laptop_reads_as_the_collector_reads_it() {
        let mut info: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "system": {}, "cpu": {"model": null, "cores": null, "threads": 8},
            "memory": {"ram_gb": null, "unified": false}, "gpus": [], "physical_disks": []
        }))
        .expect("a map");
        apply_sysctl(&mut info, "Apple M2", "8", "8", "17179869184", "", true);
        apply_profile(&mut info, AIR, true);
        let v = Value::Object(info);
        assert_eq!(v["cpu"]["model"], "Apple M2");
        assert_eq!(
            (v["cpu"]["performance_cores"].as_u64(), v["cpu"]["efficiency_cores"].as_u64()),
            (Some(4), Some(4))
        );
        assert_eq!(v["memory"], serde_json::json!({"ram_gb": 16.0, "unified": true}));
        assert_eq!(
            v["system"],
            serde_json::json!({"model": "MacBook Air", "manufacturer": "Apple", "form": "laptop"})
        );
        assert_eq!(
            v["gpus"][0],
            serde_json::json!({"name": "Apple M2", "vendor": "Apple", "vram_gb": 16.0,
            "gpu_cores": 10, "unified_memory": true, "power_readable": false})
        );
        assert_eq!(v["physical_disks"][0]["size_gb"], 465.9);
    }

    #[test]
    fn an_intel_mac_names_its_gpu_memory() {
        assert_eq!(vram_gb("1536 MB"), Some(1.5));
        assert_eq!(vram_gb("8 GB"), Some(8.0));
        assert_eq!(vram_gb("lots"), None);
        assert_eq!(proc_cores("proc 12:8:4"), Some((8, 4)));
        assert_eq!(proc_cores("12 cores"), None);
    }
}

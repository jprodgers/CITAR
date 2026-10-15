//! The Linux part of the scan: `citar/hwinfo.py` `_linux` (lines 161-222), read the same way:
//! `/proc/cpuinfo` (the model and the sockets), `/proc/meminfo`, the DMI chassis type, vendor and
//! product, Intel's RAPL power counters, `rocm-smi` (AMD GPUs with their memory), `lspci` (every
//! display controller), `lsblk` (the physical disks), and a battery for the form.
//!
//! The parsers take the files' and the programs' text and are compiled on every OS, so their
//! tests run everywhere; only [`probe`] reads the machine.

use serde_json::{Map, Value};

use super::{gb, push, round_to, set, text_or_empty};

/// The chassis types of a laptop, a desktop and a server (SMBIOS).
const LAPTOP: [i64; 7] = [8, 9, 10, 14, 30, 31, 32];
const DESKTOP: [i64; 7] = [3, 4, 5, 6, 7, 13, 35];
const SERVER: [i64; 4] = [17, 23, 28, 29];

/// Reads the machine and applies what it says to `info`.
#[cfg(target_os = "linux")]
pub fn probe(info: &mut Map<String, Value>) {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::merge_gpus;
    /// How long `rocm-smi`, `lspci` and `lsblk` may take (the collector's default).
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    let read = |p: &str| std::fs::read(p).ok().map(|b| super::probe::decode(&b));
    if let Some(text) = read("/proc/cpuinfo") {
        cpuinfo(info, &text);
    }
    if let Some(text) = read("/proc/meminfo") {
        meminfo(info, &text);
    }
    if let Some(ct) = read("/sys/class/dmi/id/chassis_type") {
        let vendor = read("/sys/class/dmi/id/sys_vendor");
        let product = read("/sys/class/dmi/id/product_name");
        dmi(info, &ct, vendor.as_deref(), product.as_deref());
    }
    if Path::new("/sys/class/powercap/intel-rapl:0").is_dir()
        || Path::new("/sys/class/powercap/intel-rapl").is_dir()
    {
        set(info, "power", "cpu_rapl", Value::Bool(true));
    }
    let policies: Vec<String> = std::fs::read_dir("/sys/devices/system/cpu/cpufreq")
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("policy"))
                .filter_map(|e| read(&e.path().join("scaling_max_freq").to_string_lossy()))
                .collect()
        })
        .unwrap_or_default();
    max_freq(info, &policies);
    // rocm-smi's AMD GPUs are as detailed as nvidia-smi's: lspci's lines for the same cards are
    // left out against both (the collector added rocm-smi's and lspci's together, so an AMD card
    // came out twice).
    merge_gpus(
        info,
        rocm(&super::probe::run(
            OsStr::new("rocm-smi"),
            &["--showproductname", "--showmeminfo", "vram", "--json"],
            TIMEOUT,
        )),
    );
    merge_gpus(info, lspci(&super::probe::run(OsStr::new("lspci"), &[], TIMEOUT)));
    let disks = super::probe::run(
        OsStr::new("lsblk"),
        &["-d", "-J", "-b", "-o", "NAME,MODEL,SIZE,ROTA,TRAN"],
        TIMEOUT,
    );
    lsblk(info, &disks);
    let batteries: Vec<String> = std::fs::read_dir("/sys/class/power_supply")
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    battery(info, &batteries);
}

/// `model name\s*:\s*(.+)`, the first; the sockets as the distinct `physical id`s (1 when none).
pub fn cpuinfo(info: &mut Map<String, Value>, text: &str) {
    let field = |line: &str, name: &str| -> Option<String> {
        let rest = line.strip_prefix(name)?;
        let rest = rest.trim_start_matches([' ', '\t']).strip_prefix(':')?;
        Some(rest.trim_start().to_owned())
    };
    if let Some(model) = text.lines().find_map(|l| field(l, "model name")) {
        let model = model.trim();
        if !model.is_empty() {
            set(info, "cpu", "model", Value::from(model));
        }
    }
    let mut ids: Vec<String> = text
        .lines()
        .filter_map(|l| field(l, "physical id"))
        .filter_map(|v| {
            let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
            (!digits.is_empty()).then_some(digits)
        })
        .collect();
    ids.sort();
    ids.dedup();
    set(info, "cpu", "sockets", Value::from(ids.len().max(1)));
}

/// `MemTotal:\s*(\d+)\s*kB`, in gigabytes to one place.
pub fn meminfo(info: &mut Map<String, Value>, text: &str) {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("MemTotal:") else { continue };
        let rest = rest.trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() || !rest[digits.len()..].trim_start().starts_with("kB") {
            continue;
        }
        if let Ok(kb) = digits.parse::<u64>() {
            #[allow(clippy::cast_precision_loss)]
            let gib = kb as f64 / (1024.0 * 1024.0);
            set(info, "memory", "ram_gb", Value::from(round_to(gib, 1)));
            return;
        }
    }
}

/// The form from the DMI chassis type, and the vendor and product (read only when the chassis
/// type reads, as the collector's one `try` does).
pub fn dmi(
    info: &mut Map<String, Value>,
    chassis: &str,
    vendor: Option<&str>,
    product: Option<&str>,
) {
    let Ok(ct) = chassis.trim().parse::<i64>() else { return };
    let form = if LAPTOP.contains(&ct) {
        Value::from("laptop")
    } else if DESKTOP.contains(&ct) {
        Value::from("desktop")
    } else if SERVER.contains(&ct) {
        Value::from("server")
    } else {
        Value::Null
    };
    set(info, "system", "form", form);
    // `open(...).read()` fails part-way, as the collector's does: the vendor alone may be set.
    let Some(vendor) = vendor else { return };
    set(info, "system", "manufacturer", Value::from(vendor.trim()));
    let Some(product) = product else { return };
    set(info, "system", "model", Value::from(product.trim()));
}

/// The CPU's highest clock as `psutil.cpu_freq().max` gives it: each frequency policy's
/// `scaling_max_freq` (kHz), averaged, in GHz to two places; nothing when no policy reads (as in
/// WSL).
pub fn max_freq(info: &mut Map<String, Value>, policies_khz: &[String]) {
    let khz: Vec<f64> = policies_khz.iter().filter_map(|k| k.trim().parse::<f64>().ok()).collect();
    if khz.is_empty() || khz.len() != policies_khz.len() {
        return;
    }
    #[allow(clippy::cast_precision_loss)]
    let mhz = khz.iter().map(|k| k / 1000.0).sum::<f64>() / khz.len() as f64;
    if mhz > 0.0 {
        set(info, "cpu", "max_ghz", Value::from(round_to(mhz / 1000.0, 2)));
    }
}

/// `rocm-smi --showproductname --showmeminfo vram --json`'s cards.
#[must_use]
pub fn rocm(out: &str) -> Vec<Map<String, Value>> {
    let Ok(Value::Object(cards)) = serde_json::from_str::<Value>(out) else { return Vec::new() };
    cards
        .iter()
        .filter_map(|(card, v)| {
            let v = v.as_object()?;
            let name = ["Card series", "Card model"]
                .iter()
                .find_map(|k| v.get(*k).filter(|x| super::truthy(Some(x))).cloned())
                .unwrap_or_else(|| Value::from(card.as_str()));
            let mut g = Map::new();
            g.insert("name".into(), name);
            g.insert("vendor".into(), Value::from("AMD"));
            g.insert("vram_gb".into(), gb(v.get("VRAM Total Memory (B)")));
            g.insert("power_readable".into(), Value::Bool(true));
            Some(g)
        })
        .collect()
}

/// `lspci`'s display controllers (`VGA`, `3D controller`, `Display controller`).
#[must_use]
pub fn lspci(out: &str) -> Vec<Map<String, Value>> {
    out.lines()
        .filter(|l| {
            l.contains("VGA") || l.contains("3D controller") || l.contains("Display controller")
        })
        .map(|l| {
            let name = l.split_once(": ").map_or(l, |(_, n)| n).trim();
            let vendor = if name.contains("NVIDIA") {
                "NVIDIA"
            } else if name.contains("AMD") || name.contains("ATI") {
                "AMD"
            } else if name.contains("Intel") {
                "Intel"
            } else {
                ""
            };
            let mut g = Map::new();
            g.insert("name".into(), Value::from(name));
            g.insert("vendor".into(), Value::from(vendor));
            g.insert("vram_gb".into(), Value::Null);
            g.insert("power_readable".into(), Value::Bool(false));
            g
        })
        .collect()
}

/// `lsblk -d -J -b -o NAME,MODEL,SIZE,ROTA,TRAN`'s disks, loop, zram and RAM disks left out.
pub fn lsblk(info: &mut Map<String, Value>, out: &str) {
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(out) else { return };
    for d in data.get("blockdevices").and_then(Value::as_array).into_iter().flatten() {
        let name = text_or_empty(d.get("name"));
        if ["loop", "zram", "ram"].iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let model = match d.get("model") {
            Some(m) if super::truthy(Some(m)) => text_or_empty(Some(m)),
            _ => name,
        };
        // Python's str() of the value: true is "True", 1 is "1".
        let rota = match d.get("rota") {
            Some(Value::Bool(true)) => "True".to_owned(),
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let tran = text_or_empty(d.get("tran")).to_uppercase();
        let mut e = Map::new();
        e.insert("name".into(), Value::from(model.trim()));
        e.insert(
            "type".into(),
            Value::from(if ["1", "True", "true"].contains(&rota.as_str()) { "HDD" } else { "SSD" }),
        );
        e.insert("bus".into(), if tran.is_empty() { Value::Null } else { Value::from(tran) });
        e.insert("size_gb".into(), gb(d.get("size")));
        push(info, "physical_disks", Value::Object(e));
    }
}

/// A battery (`/sys/class/power_supply/BAT*`) makes a machine with no form yet a laptop.
pub fn battery(info: &mut Map<String, Value>, supplies: &[String]) {
    let has_form =
        info.get("system").and_then(|s| s.get("form")).is_some_and(|f| super::truthy(Some(f)));
    if supplies.iter().any(|p| p.starts_with("BAT")) && !has_form {
        set(info, "system", "form", Value::from("laptop"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::merge_gpus;

    fn base() -> Map<String, Value> {
        serde_json::from_value(serde_json::json!({
            "system": {}, "cpu": {"model": null, "cores": 8, "threads": 16},
            "memory": {"ram_gb": null, "unified": false}, "gpus": [], "physical_disks": [],
            "power": {}
        }))
        .expect("a map")
    }

    #[test]
    fn the_files_read_as_the_collector_reads_them() {
        let mut info = base();
        cpuinfo(
            &mut info,
            "processor\t: 0\nmodel name\t: AMD Ryzen 9 7950X 16-Core Processor\nphysical id\t: 0\n\
             processor\t: 1\nmodel name\t: AMD Ryzen 9 7950X 16-Core Processor\nphysical id\t: 0\n",
        );
        meminfo(&mut info, "MemTotal:       65536000 kB\nMemFree:        1 kB\n");
        dmi(&mut info, "3\n", Some("ASUS\n"), Some("ROG\n"));
        battery(&mut info, &["BAT0".to_owned()]);
        max_freq(&mut info, &["5881000\n".to_owned(), "5881000".to_owned()]);
        let v = Value::Object(info);
        assert_eq!(v["cpu"]["model"], "AMD Ryzen 9 7950X 16-Core Processor");
        assert_eq!(v["cpu"]["sockets"], 1);
        assert_eq!(v["cpu"]["max_ghz"], 5.88);
        assert_eq!(v["memory"]["ram_gb"], 62.5);
        // a desktop chassis stays a desktop, battery or not
        assert_eq!(
            v["system"],
            serde_json::json!({"form": "desktop", "manufacturer": "ASUS", "model": "ROG"})
        );
    }

    #[test]
    fn the_programs_read_as_the_collector_reads_them() {
        let mut info = base();
        merge_gpus(
            &mut info,
            rocm(
                r#"{"card0": {"Card series": "Radeon RX 7900 XTX", "VRAM Total Memory (B)": "25753026560"}}"#,
            ),
        );
        merge_gpus(
            &mut info,
            lspci(
                "00:02.0 VGA compatible controller: Intel Corporation Raptor Lake-P [Iris Xe Graphics]\n\
                 03:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Navi 31 [Radeon RX 7900 XTX]\n\
                 00:1f.3 Audio device: Intel Corporation\n",
            ),
        );
        lsblk(
            &mut info,
            r#"{"blockdevices": [{"name": "loop0", "model": null, "size": 1, "rota": false, "tran": null},
                {"name": "nvme0n1", "model": "Samsung SSD 990 PRO 2TB", "size": 2000398934016, "rota": false, "tran": "nvme"},
                {"name": "sda", "model": null, "size": "1000204886016", "rota": true, "tran": "sata"}]}"#,
        );
        battery(&mut info, &["AC".to_owned(), "BAT1".to_owned()]);
        let v = Value::Object(info);
        let gpus = v["gpus"].as_array().expect("gpus");
        // lspci's line for the AMD card holds rocm-smi's name for it, so it is left out.
        assert_eq!(gpus.len(), 2, "{gpus:?}");
        assert_eq!(
            gpus[0],
            serde_json::json!({"name": "Radeon RX 7900 XTX", "vendor": "AMD", "vram_gb": 24.0, "power_readable": true})
        );
        assert_eq!(gpus[1]["vendor"], "Intel");
        assert_eq!(
            v["physical_disks"],
            serde_json::json!([
            {"name": "Samsung SSD 990 PRO 2TB", "type": "SSD", "bus": "NVME", "size_gb": 1863.0},
            {"name": "sda", "type": "HDD", "bus": "SATA", "size_gb": 931.5}])
        );
        assert_eq!(v["system"]["form"], "laptop");
    }
}

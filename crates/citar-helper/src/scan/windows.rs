//! The Windows part of the scan: `citar/hwinfo.py` `_windows` (lines 89-158), the same
//! PowerShell query (`Get-CimInstance` of the processor, the computer system, the enclosure, the
//! video controllers and the memory modules; the display adapters' memory from the registry,
//! read only; `Get-PhysicalDisk`; the batteries), read the same way, so a helper's scan and the
//! collector's agree on the same machine. One difference: the script asks PowerShell for UTF-8 on
//! its standard output, which Windows PowerShell 5 otherwise writes in the console's code page
//! (a GPU or disk name outside ASCII came out garbled to Python's UTF-8 reading).
//!
//! [`apply`] reads the script's JSON and is compiled on every OS, so its tests run everywhere;
//! only [`probe`] runs PowerShell.

use serde_json::{Map, Value};

use super::{as_list, gb, merge_gpus, round_to, set, text_or_empty};

/// The script: `hwinfo._PS_SCRIPT`, with UTF-8 output.
pub const SCRIPT: &str = r"[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$ErrorActionPreference = 'SilentlyContinue'
$o = @{}
$o.cpu = Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed
$o.cs = Get-CimInstance Win32_ComputerSystem | Select-Object Manufacturer, Model, TotalPhysicalMemory, PCSystemType
$o.enclosure = (Get-CimInstance Win32_SystemEnclosure).ChassisTypes
$o.video = Get-CimInstance Win32_VideoController | Select-Object Name, AdapterRAM, DriverVersion, AdapterCompatibility
$o.vram = Get-ItemProperty 'HKLM:\SYSTEM\ControlSet001\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0*' | Select-Object DriverDesc, 'HardwareInformation.qwMemorySize'
$o.mem = Get-CimInstance Win32_PhysicalMemory | Select-Object Capacity, Speed, Manufacturer, PartNumber
$o.disks = Get-PhysicalDisk | Select-Object FriendlyName, MediaType, BusType, Size
$o.battery = @(Get-CimInstance Win32_Battery).Count
$o | ConvertTo-Json -Depth 4 -Compress
";

/// How long the PowerShell query may take (the collector's).
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Runs the query through `powershell` (the program the probes name) and applies its answer.
#[cfg(windows)]
pub fn probe(info: &mut Map<String, Value>, powershell: &std::ffi::OsStr) {
    let raw = super::probe::run(
        powershell,
        &["-NoProfile", "-NonInteractive", "-Command", SCRIPT],
        TIMEOUT,
    );
    apply(info, &raw);
}

/// The chassis types of a laptop (SMBIOS: portable, laptop, notebook, sub-notebook, tablet,
/// convertible, detachable).
const LAPTOP_CHASSIS: [i64; 7] = [8, 9, 10, 14, 30, 31, 32];

fn int_of(v: Option<&Value>) -> i64 {
    // Python's int(x or 0) on what PowerShell writes: a number, or text of one.
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(f64_to_i64)).unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        Some(Value::Bool(b)) => i64::from(*b),
        _ => 0,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn f64_to_i64(x: f64) -> i64 {
    x.trunc() as i64
}

fn lower(v: Option<&Value>) -> String {
    text_or_empty(v).to_lowercase()
}

/// Applies the PowerShell query's JSON (`raw`, empty when it failed) to `info`, as `_windows`.
pub fn apply(info: &mut Map<String, Value>, raw: &str) {
    let d: Map<String, Value> = if raw.trim().is_empty() {
        Map::new()
    } else {
        serde_json::from_str(raw).unwrap_or_default()
    };
    let cpus = as_list(d.get("cpu"));
    if let Some(c) = cpus.first().and_then(Value::as_object) {
        let cores: i64 = cpus.iter().map(|x| int_of(x.get("NumberOfCores"))).sum();
        let threads: i64 = cpus.iter().map(|x| int_of(x.get("NumberOfLogicalProcessors"))).sum();
        let mhz = c.get("MaxClockSpeed").and_then(Value::as_f64).unwrap_or(0.0);
        let ghz = round_to(mhz / 1000.0, 2);
        set(info, "cpu", "model", Value::from(text_or_empty(c.get("Name")).trim()));
        set(info, "cpu", "sockets", Value::from(cpus.len()));
        set(info, "cpu", "cores", Value::from(cores));
        set(info, "cpu", "threads", Value::from(threads));
        set(info, "cpu", "max_ghz", if ghz == 0.0 { Value::Null } else { Value::from(ghz) });
    }
    let cs = d.get("cs").and_then(Value::as_object).cloned().unwrap_or_default();
    set(info, "system", "manufacturer", cs.get("Manufacturer").cloned().unwrap_or(Value::Null));
    set(info, "system", "model", cs.get("Model").cloned().unwrap_or(Value::Null));
    if super::truthy(cs.get("TotalPhysicalMemory")) {
        set(info, "memory", "ram_gb", gb(cs.get("TotalPhysicalMemory")));
    }
    let mods = as_list(d.get("mem"));
    if !mods.is_empty() {
        let modules: Vec<Value> = mods
            .iter()
            .map(|m| {
                let mut e = Map::new();
                e.insert("gb".into(), gb(m.get("Capacity")));
                e.insert("mhz".into(), m.get("Speed").cloned().unwrap_or(Value::Null));
                e.insert("part".into(), Value::from(text_or_empty(m.get("PartNumber")).trim()));
                Value::Object(e)
            })
            .collect();
        set(info, "memory", "modules", Value::Array(modules));
    }
    let chassis = as_list(d.get("enclosure"));
    let laptop_chassis = chassis.iter().any(|c| {
        let digits = match c {
            Value::Number(n) => n.as_u64().map(|v| v.to_string()),
            Value::String(s) => Some(s.clone()),
            _ => None,
        };
        digits
            .filter(|s| !s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit()))
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|n| LAPTOP_CHASSIS.contains(&n))
    });
    if int_of(d.get("battery")) > 0 || laptop_chassis {
        set(info, "system", "form", Value::from("laptop"));
    } else if !chassis.is_empty() {
        set(info, "system", "form", Value::from("desktop"));
    }
    let mut vram: Vec<(String, Value)> = Vec::new();
    for v in as_list(d.get("vram")) {
        let size = v.get("HardwareInformation.qwMemorySize");
        if let Some(desc) = v.get("DriverDesc").and_then(Value::as_str)
            && !desc.is_empty()
            && super::truthy(size)
        {
            vram.retain(|(k, _)| k != desc);
            vram.push((desc.to_owned(), gb(size)));
        }
    }
    let mut others = Vec::new();
    for v in as_list(d.get("video")) {
        let name = text_or_empty(v.get("Name")).trim().to_owned();
        let lname = name.to_lowercase();
        if name.is_empty() || lname.contains("basic display") || lname.contains("remote") {
            continue;
        }
        let compat = lower(v.get("AdapterCompatibility"));
        let vendor = if compat.contains("nvidia") {
            Value::from("NVIDIA")
        } else if compat.contains("amd") || compat.contains("advanced micro") {
            Value::from("AMD")
        } else if compat.contains("intel") {
            Value::from("Intel")
        } else {
            // `v.get("AdapterCompatibility") or ""`: the value as it came, or "".
            match v.get("AdapterCompatibility") {
                Some(x) if super::truthy(Some(x)) => x.clone(),
                _ => Value::from(""),
            }
        };
        let mapped = vram.iter().find(|(k, _)| *k == name).map(|(_, g)| g.clone());
        let vram_gb = match mapped {
            Some(g) if super::truthy(Some(&g)) => g,
            _ => gb(v.get("AdapterRAM")),
        };
        let mut g = Map::new();
        g.insert("name".into(), Value::from(name));
        g.insert("vendor".into(), vendor);
        g.insert("vram_gb".into(), vram_gb);
        g.insert("driver".into(), v.get("DriverVersion").cloned().unwrap_or(Value::Null));
        g.insert("power_readable".into(), Value::Bool(false));
        others.push(g);
    }
    merge_gpus(info, others);
    for disk in as_list(d.get("disks")) {
        let media = match disk.get("MediaType") {
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => match n.as_i64() {
                Some(3) => Value::from("HDD"),
                Some(4) => Value::from("SSD"),
                Some(0) => Value::Null,
                _ => Value::Number(n.clone()),
            },
            Some(other) => other.clone(),
            None => Value::Null,
        };
        let bus = match disk.get("BusType") {
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => match n.as_i64() {
                Some(17) => Value::from("NVMe"),
                Some(11) => Value::from("SATA"),
                Some(7) => Value::from("USB"),
                _ => Value::Number(n.clone()),
            },
            Some(other) => other.clone(),
            None => Value::Null,
        };
        let mut e = Map::new();
        e.insert("name".into(), Value::from(text_or_empty(disk.get("FriendlyName")).trim()));
        e.insert("type".into(), media);
        e.insert("bus".into(), bus);
        e.insert("size_gb".into(), gb(disk.get("Size")));
        super::push(info, "physical_disks", Value::Object(e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The query's answer on the laptop this was written on (an Acer Nitro: a battery, an RTX
    /// 3050 that nvidia-smi also reports, the Intel graphics only PowerShell sees).
    const NITRO: &str = r#"{"battery":1,"cpu":{"Name":"13th Gen Intel(R) Core(TM) i5-13420H","NumberOfCores":8,"NumberOfLogicalProcessors":12,"MaxClockSpeed":2100},"mem":[{"Capacity":8589934592,"Speed":5600,"Manufacturer":"Micron Technology","PartNumber":"MTC4C10163S1SC56BD1 "},{"Capacity":8589934592,"Speed":5600,"Manufacturer":"Micron Technology","PartNumber":"MTC4C10163S1SC56BD1 "}],"enclosure":[10],"video":[{"Name":"NVIDIA GeForce RTX 3050 6GB Laptop GPU","AdapterRAM":4293918720,"DriverVersion":"31.0.15.4618","AdapterCompatibility":"NVIDIA"},{"Name":"Intel(R) UHD Graphics","AdapterRAM":1073741824,"DriverVersion":"31.0.101.4146","AdapterCompatibility":"Intel Corporation"}],"disks":[{"FriendlyName":"NVMe WD PC SN740 SDDQNQD-512G-1014","MediaType":"SSD","BusType":"NVMe","Size":512110190592},{"FriendlyName":"NVMe Samsung SSD 990 EVO Plus 4TB","MediaType":"SSD","BusType":"NVMe","Size":4000787030016}],"vram":[{"DriverDesc":"Intel(R) UHD Graphics","HardwareInformation.qwMemorySize":null},{"DriverDesc":"NVIDIA GeForce RTX 3050 6GB Laptop GPU","HardwareInformation.qwMemorySize":6442450944}],"cs":{"Manufacturer":"Acer","Model":"Nitro ANV15-51","TotalPhysicalMemory":16869351424,"PCSystemType":2}}"#;

    fn base() -> Map<String, Value> {
        serde_json::from_value(serde_json::json!({
            "system": {}, "cpu": {"model": null, "cores": null, "threads": 12},
            "memory": {"ram_gb": null, "unified": false},
            "gpus": [{"name": "NVIDIA GeForce RTX 3050 6GB Laptop GPU", "vendor": "NVIDIA"}],
            "physical_disks": []
        }))
        .expect("a map")
    }

    #[test]
    fn the_query_reads_as_the_collector_reads_it() {
        let mut info = base();
        apply(&mut info, NITRO);
        let v = Value::Object(info);
        assert_eq!(v["cpu"]["model"], "13th Gen Intel(R) Core(TM) i5-13420H");
        assert_eq!((v["cpu"]["cores"].as_i64(), v["cpu"]["threads"].as_i64()), (Some(8), Some(12)));
        assert_eq!(v["cpu"]["sockets"], 1);
        assert_eq!(v["cpu"]["max_ghz"], 2.1);
        assert_eq!(v["memory"]["ram_gb"], 15.7);
        assert_eq!(
            v["memory"]["modules"][0],
            serde_json::json!({"gb": 8.0, "mhz": 5600, "part": "MTC4C10163S1SC56BD1"})
        );
        assert_eq!(
            v["system"],
            serde_json::json!({"manufacturer": "Acer", "model": "Nitro ANV15-51", "form": "laptop"})
        );
        // nvidia-smi's GPU first; the Intel one added from PowerShell, its memory from AdapterRAM
        let gpus = v["gpus"].as_array().expect("gpus");
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[1]["name"], "Intel(R) UHD Graphics");
        assert_eq!(gpus[1]["vendor"], "Intel");
        assert_eq!(gpus[1]["vram_gb"], 1.0);
        assert_eq!(
            v["physical_disks"][1],
            serde_json::json!({"name": "NVMe Samsung SSD 990 EVO Plus 4TB", "type": "SSD", "bus": "NVMe", "size_gb": 3726.0})
        );
    }

    #[test]
    fn numbers_for_the_disk_kinds_and_no_answer_at_all() {
        let mut info = base();
        apply(
            &mut info,
            r#"{"disks": {"FriendlyName": " A ", "MediaType": 3, "BusType": 11, "Size": null}, "enclosure": 3}"#,
        );
        let v = Value::Object(info);
        assert_eq!(
            v["physical_disks"][0],
            serde_json::json!({"name": "A", "type": "HDD", "bus": "SATA", "size_gb": null})
        );
        assert_eq!(v["system"]["form"], "desktop");
        let mut info = base();
        apply(&mut info, "");
        apply(&mut info, "not json");
        assert_eq!(Value::Object(info.clone())["cpu"]["threads"], 12);
        assert_eq!(info["system"], serde_json::json!({"manufacturer": null, "model": null}));
    }
}

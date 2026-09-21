"""CITAR hardware collector: what a machine has (CPU, GPUs, memory, disks, local model runtimes) and a first guess at
its power draw, as JSON for the Servers page.

This file is deliberately standalone (standard library only; uses psutil when installed) so it can be copied to any
Windows, Linux or macOS machine and run there:

    python collect_hardware.py > my-machine.json        # then import the file on the Servers page

On the machine that runs CITAR, the Servers page calls collect() directly.
"""
from __future__ import annotations

import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import time

COLLECTOR_VERSION = 1


def _run(args, timeout=30) -> str:
    """Run a command and return its output, or an empty string if it fails.

    Every probe here is best-effort: a machine without ``nvidia-smi``, or with a version that answers
    differently, should produce less information rather than an error.
    """
    try:
        r = subprocess.run(args, capture_output=True, stdin=subprocess.DEVNULL, timeout=timeout)
        return r.stdout.decode("utf-8-sig", errors="replace") if r.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def _gb(n) -> float | None:
    """Bytes as gigabytes, or None."""
    try:
        return round(float(n) / 1024 ** 3, 1)
    except (TypeError, ValueError):
        return None


def _psutil():
    """The psutil module, or None if it is not installed."""
    try:
        import psutil
        return psutil
    except ImportError:
        return None


# ----------------------------------------------------------------------------- GPUs
def _nvidia() -> list[dict]:
    """GPU details from nvidia-smi."""
    exe = shutil.which("nvidia-smi")
    if not exe:
        return []
    out = _run([exe, "--query-gpu=name,memory.total,driver_version,power.limit,power.draw,pci.bus_id",
                "--format=csv,noheader,nounits"])
    gpus = []
    for line in out.strip().splitlines():
        parts = [p.strip() for p in line.split(",")]
        if len(parts) < 6:
            continue

        def num(s):
            """Parse a number from nvidia-smi's output, tolerating its units and blanks."""
            try:
                return float(s)
            except ValueError:
                return None
        gpus.append({"name": parts[0], "vendor": "NVIDIA", "vram_gb": round(num(parts[1]) / 1024, 1) if num(parts[1]) else None,
                     "driver": parts[2], "power_limit_w": num(parts[3]), "power_now_w": num(parts[4]),
                     "bus": parts[5], "power_readable": num(parts[4]) is not None})
    return gpus


def _merge_gpus(primary: list[dict], extra: list[dict]) -> list[dict]:
    """Combine GPU information from two sources, preferring the more detailed."""
    names = {g["name"].lower() for g in primary}
    return primary + [g for g in extra if g["name"].lower() not in names
                      and not any(n in g["name"].lower() or g["name"].lower() in n for n in names)]


# ----------------------------------------------------------------------------- Windows
_PS_SCRIPT = r"""
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
"""


def _windows(info: dict):
    """Collect hardware details on Windows."""
    raw = _run(["powershell", "-NoProfile", "-NonInteractive", "-Command", _PS_SCRIPT], timeout=90)
    try:
        d = json.loads(raw) if raw.strip() else {}
    except ValueError:
        d = {}

    def as_list(x):
        """Normalise a WMI result that may be one item or several."""
        return x if isinstance(x, list) else ([x] if x else [])
    cpus = as_list(d.get("cpu"))
    if cpus:
        c = cpus[0]
        info["cpu"].update({"model": (c.get("Name") or "").strip(), "sockets": len(cpus),
                            "cores": sum(int(x.get("NumberOfCores") or 0) for x in cpus),
                            "threads": sum(int(x.get("NumberOfLogicalProcessors") or 0) for x in cpus),
                            "max_ghz": round((c.get("MaxClockSpeed") or 0) / 1000, 2) or None})
    cs = d.get("cs") or {}
    info["system"].update({"manufacturer": cs.get("Manufacturer"), "model": cs.get("Model")})
    if cs.get("TotalPhysicalMemory"):
        info["memory"]["ram_gb"] = _gb(cs["TotalPhysicalMemory"])
    mods = as_list(d.get("mem"))
    if mods:
        info["memory"]["modules"] = [{"gb": _gb(m.get("Capacity")), "mhz": m.get("Speed"),
                                      "part": (m.get("PartNumber") or "").strip()} for m in mods]
    chassis = as_list(d.get("enclosure"))
    if int(d.get("battery") or 0) > 0 or any(int(c) in (8, 9, 10, 14, 30, 31, 32) for c in chassis if str(c).isdigit()):
        info["system"]["form"] = "laptop"
    elif chassis:
        info["system"]["form"] = "desktop"
    vram = {}
    for v in as_list(d.get("vram")):
        size = v.get("HardwareInformation.qwMemorySize")
        if v.get("DriverDesc") and size:
            vram[v["DriverDesc"]] = _gb(size)
    others = []
    for v in as_list(d.get("video")):
        name = (v.get("Name") or "").strip()
        if not name or "basic display" in name.lower() or "remote" in name.lower():
            continue
        vendor = v.get("AdapterCompatibility") or ""
        others.append({"name": name, "vendor": "NVIDIA" if "nvidia" in vendor.lower() else
                       "AMD" if ("amd" in vendor.lower() or "advanced micro" in vendor.lower()) else
                       "Intel" if "intel" in vendor.lower() else vendor, "vram_gb": vram.get(name) or _gb(v.get("AdapterRAM")),
                       "driver": v.get("DriverVersion"), "power_readable": False})
    info["gpus"] = _merge_gpus(info["gpus"], others)
    for disk in as_list(d.get("disks")):
        mt = disk.get("MediaType")
        mt = {3: "HDD", 4: "SSD", 0: None}.get(mt, mt) if isinstance(mt, int) else mt
        info["physical_disks"].append({"name": (disk.get("FriendlyName") or "").strip(), "type": mt,
                                       "bus": disk.get("BusType") if not isinstance(disk.get("BusType"), int) else
                                       {17: "NVMe", 11: "SATA", 7: "USB"}.get(disk["BusType"], disk["BusType"]),
                                       "size_gb": _gb(disk.get("Size"))})


# ----------------------------------------------------------------------------- Linux
def _linux(info: dict):
    """Collect hardware details on Linux."""
    try:
        text = open("/proc/cpuinfo", encoding="utf-8", errors="replace").read()
        m = re.search(r"model name\s*:\s*(.+)", text)
        if m:
            info["cpu"]["model"] = m.group(1).strip()
        sockets = set(re.findall(r"physical id\s*:\s*(\d+)", text))
        info["cpu"]["sockets"] = len(sockets) or 1
    except OSError:
        pass
    try:
        mem = open("/proc/meminfo", encoding="utf-8").read()
        m = re.search(r"MemTotal:\s*(\d+)\s*kB", mem)
        if m:
            info["memory"]["ram_gb"] = round(int(m.group(1)) / 1024 ** 2, 1)
    except OSError:
        pass
    try:
        ct = int(open("/sys/class/dmi/id/chassis_type").read().strip())
        info["system"]["form"] = "laptop" if ct in (8, 9, 10, 14, 30, 31, 32) else "desktop" if ct in (3, 4, 5, 6, 7, 13, 35) else \
            "server" if ct in (17, 23, 28, 29) else None
        for f, k in (("sys_vendor", "manufacturer"), ("product_name", "model")):
            info["system"][k] = open(f"/sys/class/dmi/id/{f}").read().strip()
    except (OSError, ValueError):
        pass
    if os.path.isdir("/sys/class/powercap/intel-rapl:0") or os.path.isdir("/sys/class/powercap/intel-rapl"):
        info["power"]["cpu_rapl"] = True
    extra = []
    rocm = shutil.which("rocm-smi")
    if rocm:
        out = _run([rocm, "--showproductname", "--showmeminfo", "vram", "--json"])
        try:
            for card, v in json.loads(out).items():
                name = v.get("Card series") or v.get("Card model") or card
                total = v.get("VRAM Total Memory (B)")
                extra.append({"name": name, "vendor": "AMD", "vram_gb": _gb(total), "power_readable": True})
        except (ValueError, AttributeError):
            pass
    lspci = shutil.which("lspci")
    if lspci:
        for line in _run([lspci]).splitlines():
            if re.search(r"VGA|3D controller|Display controller", line):
                name = line.split(": ", 1)[-1].strip()
                vendor = "NVIDIA" if "NVIDIA" in name else "AMD" if ("AMD" in name or "ATI" in name) else "Intel" if "Intel" in name else ""
                extra.append({"name": name, "vendor": vendor, "vram_gb": None, "power_readable": False})
    info["gpus"] = _merge_gpus(info["gpus"], extra)
    if shutil.which("lsblk"):
        try:
            data = json.loads(_run(["lsblk", "-d", "-J", "-b", "-o", "NAME,MODEL,SIZE,ROTA,TRAN"]))
            for d in data.get("blockdevices", []):
                if d.get("name", "").startswith(("loop", "zram", "ram")):
                    continue
                info["physical_disks"].append({"name": (d.get("model") or d.get("name") or "").strip(),
                                               "type": "HDD" if str(d.get("rota")) in ("1", "True", "true") else "SSD",
                                               "bus": (d.get("tran") or "").upper() or None, "size_gb": _gb(d.get("size"))})
        except ValueError:
            pass
    bats = list(os.listdir("/sys/class/power_supply")) if os.path.isdir("/sys/class/power_supply") else []
    if any(p.startswith("BAT") for p in bats) and not info["system"].get("form"):
        info["system"]["form"] = "laptop"


# ----------------------------------------------------------------------------- macOS
def _macos(info: dict):
    """Collect hardware details on macOS."""
    brand = _run(["sysctl", "-n", "machdep.cpu.brand_string"]).strip()
    if brand:
        info["cpu"]["model"] = brand
    for key, field in (("hw.physicalcpu", "cores"), ("hw.logicalcpu", "threads")):
        v = _run(["sysctl", "-n", key]).strip()
        if v.isdigit():
            info["cpu"][field] = int(v)
    mem = _run(["sysctl", "-n", "hw.memsize"]).strip()
    if mem.isdigit():
        info["memory"]["ram_gb"] = _gb(int(mem))
    apple = platform.machine() == "arm64"
    info["memory"]["unified"] = apple
    try:
        sp = json.loads(_run(["system_profiler", "SPDisplaysDataType", "SPHardwareDataType", "SPNVMeDataType", "-json"], timeout=60))
    except ValueError:
        sp = {}
    for hw in sp.get("SPHardwareDataType", []):
        info["system"]["model"] = hw.get("machine_name") or hw.get("machine_model")
        info["system"]["manufacturer"] = "Apple"
        info["system"]["form"] = "laptop" if "book" in str(hw.get("machine_name", "")).lower() else "desktop"
        if apple and hw.get("number_processors"):
            m = re.search(r"proc (\d+):(\d+):(\d+)", str(hw["number_processors"]))
            if m:
                info["cpu"]["performance_cores"], info["cpu"]["efficiency_cores"] = int(m.group(2)), int(m.group(3))
    for d in sp.get("SPDisplaysDataType", []):
        name = d.get("sppci_model") or d.get("_name")
        cores = d.get("sppci_cores")
        vram = d.get("spdisplays_vram") or d.get("spdisplays_vram_shared")
        vram_gb = None
        if vram:
            m = re.match(r"([\d.]+)\s*(GB|MB)", str(vram))
            if m:
                vram_gb = float(m.group(1)) / (1024 if m.group(2) == "MB" else 1)
        info["gpus"].append({"name": name, "vendor": "Apple" if apple else d.get("spdisplays_vendor"),
                             "vram_gb": vram_gb if not apple else info["memory"].get("ram_gb"),
                             "gpu_cores": int(cores) if str(cores or "").isdigit() else None,
                             "unified_memory": apple, "power_readable": False})
    for group in sp.get("SPNVMeDataType", []):
        for d in group.get("_items", []):
            info["physical_disks"].append({"name": d.get("device_model") or d.get("_name"), "type": "SSD", "bus": "NVMe",
                                           "size_gb": _gb(d.get("size_in_bytes"))})


# ----------------------------------------------------------------------------- disks, runtimes, power guesses
def _volumes(info: dict):
    """Disk volumes and their free space."""
    ps = _psutil()
    seen = set()
    mounts = []
    if ps:
        for p in ps.disk_partitions(all=False):
            if "cdrom" in p.opts or p.fstype in ("", "squashfs", "tmpfs", "devtmpfs", "overlay"):
                continue
            mounts.append((p.mountpoint, p.fstype))
    elif sys.platform.startswith("win"):
        mounts = [(f"{c}:\\", "") for c in "CDEFGHIJ" if os.path.exists(f"{c}:\\")]
    else:
        mounts = [("/", "")]
    for mp, fs in mounts:
        try:
            u = shutil.disk_usage(mp)
        except OSError:
            continue
        key = (u.total, u.used)
        if key in seen:
            continue
        seen.add(key)
        info["volumes"].append({"mount": mp, "fs": fs or None, "total_gb": _gb(u.total), "free_gb": _gb(u.free)})


def _runtimes(info: dict):
    """Which model runtimes are installed, and their versions."""
    home = os.path.expanduser("~")
    lms = shutil.which("lms") or next((p for p in (os.path.join(home, ".lmstudio", "bin", n) for n in ("lms.exe", "lms"))
                                       if os.path.exists(p)), None)
    if lms:
        rt = {"name": "LM Studio", "cli": lms, "models": []}
        raw = _run([lms, "ls", "--json"], timeout=60)
        try:
            for m in json.loads(raw):
                if m.get("type") == "embedding":
                    continue
                rt["models"].append({"key": m.get("modelKey") or m.get("path"), "size_gb": _gb(m.get("sizeBytes")),
                                     "params": m.get("paramsString"), "arch": m.get("architecture"),
                                     "max_context": m.get("maxContextLength"),
                                     "device": m.get("deviceIdentifier") or "local"})
        except (ValueError, TypeError, AttributeError):
            pass
        info["runtimes"].append(rt)
    oll = shutil.which("ollama")
    if oll:
        rt = {"name": "Ollama", "cli": oll, "models": []}
        for line in _run([oll, "list"]).splitlines()[1:]:
            parts = line.split()
            if parts:
                rt["models"].append({"key": parts[0], "size": " ".join(parts[2:4]) if len(parts) > 3 else None})
        info["runtimes"].append(rt)


def guess_power(info: dict) -> dict:
    """Rough wall-power figures to start from; replace them with measured numbers (a plug-in power meter) if you can."""
    form = info["system"].get("form") or "desktop"
    cpu = (info["cpu"].get("model") or "").lower()
    threads = info["cpu"].get("threads") or 8
    if info["memory"].get("unified"):
        idle, cpu_max = 8, 30 if form == "laptop" else 60
    elif form == "laptop":
        idle = 12
        cpu_max = 55 if re.search(r"\b\d{4,5}hx?\b|hx\b|ryzen \d \d{4}h", cpu) else 25
    else:
        idle = 55
        cpu_max = 65 if threads <= 8 else 125 if threads <= 24 else 200
        if "threadripper" in cpu or "xeon" in cpu or "epyc" in cpu:
            idle, cpu_max = 110, 280
    gpu_max = 0.0
    for g in info["gpus"]:
        if g.get("power_limit_w"):
            gpu_max += 0.85 * g["power_limit_w"]
        elif g.get("vendor") in ("NVIDIA", "AMD") and (g.get("vram_gb") or 0) >= 6:
            gpu_max += 80 if form == "laptop" else 220
    if info["memory"].get("unified"):
        gpu_max = max(gpu_max, 25 if form == "laptop" else 60)
    return {"idle_w": idle, "cpu_max_w": cpu_max, "gpu_max_w": round(gpu_max), "source": "guess",
            "note": "Estimated from the hardware class; measure idle and busy wall power with a plug-in meter for better numbers."}


def collect() -> dict:
    """Everything this machine is, as one dictionary.

    Standard library only, so it can be downloaded as a single file and run on a machine that has
    nothing else installed - which is exactly the case it exists for.
    """
    info = {"format": "citar-hardware", "collector_version": COLLECTOR_VERSION,
            "collected_at": time.strftime("%Y-%m-%dT%H:%M:%S"), "hostname": socket.gethostname(),
            "os": {"system": platform.system(), "release": platform.release(), "version": platform.version(),
                   "platform": platform.platform(), "arch": platform.machine()},
            "system": {}, "cpu": {"model": platform.processor() or None, "cores": None, "threads": os.cpu_count()},
            "memory": {"ram_gb": None, "unified": False}, "gpus": _nvidia(), "physical_disks": [], "volumes": [],
            "runtimes": [], "power": {"gpu_sampling": any(g.get("power_readable") for g in _nvidia())}}
    ps = _psutil()
    if ps:
        info["cpu"]["cores"] = ps.cpu_count(logical=False)
        info["cpu"]["threads"] = ps.cpu_count(logical=True)
        info["memory"]["ram_gb"] = _gb(ps.virtual_memory().total)
        try:
            freq = ps.cpu_freq()
            if freq and freq.max:
                info["cpu"]["max_ghz"] = round(freq.max / 1000, 2)
        except Exception:
            pass
        try:
            if ps.sensors_battery() is not None:
                info["system"]["form"] = "laptop"
        except Exception:
            pass
    try:
        if sys.platform.startswith("win"):
            _windows(info)
        elif sys.platform == "darwin":
            _macos(info)
        else:
            _linux(info)
    except Exception as e:      # partial data beats none
        info["warnings"] = [f"{type(e).__name__}: {e}"]
    _volumes(info)
    _runtimes(info)
    info["power"]["suggested"] = guess_power(info)
    return info


def main():
    """Print this machine's hardware as JSON. The entry point when run as a standalone collector."""
    out = collect()
    text = json.dumps(out, indent=2)
    if len(sys.argv) > 1:
        with open(sys.argv[1], "w", encoding="utf-8") as f:
            f.write(text)
        print(f"Wrote {sys.argv[1]}. Import it on CITAR's Servers page.", file=sys.stderr)
    else:
        print(text)


if __name__ == "__main__":
    main()

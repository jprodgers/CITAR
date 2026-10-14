"""A machine's settings, and what its helper's reports may change of it (design/phase3-features.md P3.8).

The **machine settings** of a ``servers`` row, edited in the Servers page's "Machine settings…" dialog
(``citar/server/machines_api.py``):

- **capabilities** (``Server.capabilities``): ``llm`` (``enabled``: whether it takes model work at all;
  ``allow_swaps``: whether a pooled prompt may make it load a model it has not loaded) and ``cpu`` (CPU sharing for
  Phase 4's CPU jobs: ``enabled``, ``threads``, ``cpu_percent``, ``memory_mb`` and the kinds of ``work``, a subset of
  ``CPU_WORK``). Its LLM slots are the row's ``max_concurrent``, the one site number (P3.2.8). The prompt scheduler
  reads slots and swaps through ``servers.machine_settings``, which Phase 4's welcome and settings frames will carry
  with the CPU limits; Phase 3's helper (protocol v1) is sent none of it.
- **modes** (``hardware_mode``, ``models_mode``, ``power_mode``: ``auto`` or ``manual``; by default auto, auto, manual):
  what a helper's report may change of the machine's configuration (``record_report``).
- **class and trust**: the hardware classes it is priced by (``gpu_class_id``, ``cpu_class_id``; ``class_override``
  when an administrator pinned them) and its ``trust_tier``, both administrators' to set and shown to its owner.

**A helper's report** (``record_report``, from its hello and each catalog frame; ``workers.record_connection``) is kept
whole every time in ``hardware_detected`` (``{"hardware": the scan, "hardware_at", "models": the models it reported,
"models_at", "helper": its name and version}``) with ``detected_at``, the time of the last report. Then, by mode:

- **hardware** ``auto``: the scan becomes ``config.hardware`` (``servers.apply_hardware``'s source tag ``helper``),
  and the machine is classified again unless an administrator pinned its classes (``citar.ledger``'s flush hook);
  ``manual``: ``config.hardware`` is left alone.
- **models** ``auto``: merged into the catalogue (``merge_models``): a reported model's context and ``loaded`` are
  updated (its label too when the helper gives one that says more than its key), a new one is added, and one no
  longer reported is kept, marked ``reported: false``; ``manual``: only the ``loaded`` flags of the models listed are
  updated, nothing is added or removed.
- **power** ``auto``: the scan's suggested figures (``hardware.power.suggested``) become the power figures, as a
  guess; ``manual`` (the default): left alone.

**Apply** (``apply_scan``) copies the last scan, or one section of it, into the configuration whatever the modes (all of
it: each section it has something for), and ``differences`` says how the configuration and the last scan differ,
section by section, for the dialog.

Replaces: ``workers.record_connection``'s overwrite of a machine's hardware with each hello and its replacement of
the model list (before 3-18), which lost whatever its owner had typed in.
"""
from __future__ import annotations

import copy
import json
import logging
import math
from datetime import datetime, timezone
from typing import Iterable, Optional

log = logging.getLogger("citar.machine_settings")

#: How a machine takes a helper's report, per section.
MODES = ("auto", "manual")
SECTIONS = ("hardware", "models", "power")
DEFAULT_MODES = {"hardware": "auto", "models": "auto", "power": "manual"}
#: The kinds of CPU work a machine may share (plan C1; Phase 4 runs them).
CPU_WORK = ("lab_game", "ranking_game", "reference_game")
#: The bounds of what the dialog sets: LLM slots as ``normalize_server`` bounds ``max_parallel``.
SLOTS_MAX = 16
THREADS_MAX = 1024
MEMORY_MB_MAX = 16 * 1024 * 1024
#: The largest scan kept (its JSON): a helper's hardware report is a few kilobytes; a frame may be eight megabytes.
SCAN_BYTES_MAX = 256 * 1024
#: The most models one report keeps, and the longest key and label.
MODELS_MAX = 500
KEY_MAX = 200
#: ``servers.apply_hardware``'s source tag for a helper's scan.
SOURCE = "helper"
#: The power figures a scan suggests and the dialog compares.
POWER_FIELDS = (("idle_w", "Idle (W)"), ("cpu_max_w", "Extra at full CPU load (W)"),
                ("gpu_max_w", "Extra with the GPU busy (W)"))


class SettingsError(ValueError):
    """A settings change that cannot be made: ``status`` 400 for a bad value, 409 for an apply with no scan."""

    def __init__(self, message: str, status: int = 400):
        super().__init__(message)
        self.status = status


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


# ---------------------------------------------------------------------------- capabilities and modes

def _whole(v, lo: int, hi: int) -> int:
    """A stored number as a whole number within bounds (0 when it does not read)."""
    try:
        x = float(v)
    except (TypeError, ValueError, OverflowError):
        return lo
    if not math.isfinite(x):
        return lo
    return int(max(lo, min(hi, x)))


def capabilities(raw) -> dict:
    """A machine's capabilities, complete: what ``Server.capabilities`` holds, the defaults filled in (LLM on, swaps off,
    CPU sharing off)."""
    raw = raw if isinstance(raw, dict) else {}
    llm = raw.get("llm") if isinstance(raw.get("llm"), dict) else {}
    cpu = raw.get("cpu") if isinstance(raw.get("cpu"), dict) else {}
    work = cpu.get("work") if isinstance(cpu.get("work"), (list, tuple)) else []
    return {"llm": {"enabled": bool(llm.get("enabled", True)), "allow_swaps": bool(llm.get("allow_swaps", False))},
            "cpu": {"enabled": bool(cpu.get("enabled", False)), "threads": _whole(cpu.get("threads"), 0, THREADS_MAX),
                    "cpu_percent": _whole(cpu.get("cpu_percent"), 0, 100),
                    "memory_mb": _whole(cpu.get("memory_mb"), 0, MEMORY_MB_MAX),
                    "work": [w for w in CPU_WORK if w in work]}}


def modes(row) -> dict:
    """The row's three modes, a value it should not hold read as its default."""
    out = {}
    for section in SECTIONS:
        value = getattr(row, f"{section}_mode", None)
        out[section] = value if value in MODES else DEFAULT_MODES[section]
    return out


def _bool(v, what: str) -> bool:
    if not isinstance(v, bool):
        raise SettingsError(f"{what} is true or false.")
    return v


def _int(v, what: str, lo: int, hi: int) -> int:
    """A whole number from the dialog's body within bounds (a float with nothing after the point counts). The bounds
    are compared directly: the JSON reader takes an integer of hundreds of digits, which ``math.isfinite`` and
    ``float`` would overflow on (a 500 rather than this 400), while a comparison of an int never does."""
    whole = (isinstance(v, int) and not isinstance(v, bool)) or (isinstance(v, float) and math.isfinite(v)
                                                                  and v == int(v))
    if not whole or not lo <= v <= hi:
        raise SettingsError(f"{what} is a whole number from {lo:,} to {hi:,}.")
    return int(v)


def _object(v, what: str, keys: Iterable[str]) -> dict:
    if not isinstance(v, dict):
        raise SettingsError(f"{what} is an object.")
    unknown = sorted(set(v) - set(keys))
    if unknown:
        raise SettingsError(f"{what} has no {unknown[0]!r}.")
    return v


def update(row, body) -> dict:
    """Change a machine's settings from the dialog's body: any of ``slots``, ``capabilities`` (``llm`` and ``cpu``,
    each in part) and ``modes`` (any of the three). Checks everything before changing anything (a ``SettingsError``
    leaves the row as it was). Returns what changed, ``{field: {"from", "to"}}``, for the audit row."""
    body = _object(body, "The settings", ("slots", "capabilities", "modes"))
    caps = capabilities(row.capabilities)
    new_caps = copy.deepcopy(caps)
    slots = None
    if "slots" in body:
        slots = _int(body["slots"], "slots", 1, SLOTS_MAX)
    if "capabilities" in body:
        given = _object(body["capabilities"], "capabilities", ("llm", "cpu"))
        if "llm" in given:
            llm = _object(given["llm"], "capabilities.llm", ("enabled", "allow_swaps"))
            for k in llm:
                new_caps["llm"][k] = _bool(llm[k], f"capabilities.llm.{k}")
        if "cpu" in given:
            cpu = _object(given["cpu"], "capabilities.cpu", ("enabled", "threads", "cpu_percent", "memory_mb", "work"))
            if "enabled" in cpu:
                new_caps["cpu"]["enabled"] = _bool(cpu["enabled"], "capabilities.cpu.enabled")
            for k, hi in (("threads", THREADS_MAX), ("cpu_percent", 100), ("memory_mb", MEMORY_MB_MAX)):
                if k in cpu:
                    new_caps["cpu"][k] = _int(cpu[k], f"capabilities.cpu.{k}", 0, hi)
            if "work" in cpu:
                work = cpu["work"]
                if not isinstance(work, list) or any(w not in CPU_WORK for w in work):
                    raise SettingsError(f"capabilities.cpu.work is a list of {', '.join(CPU_WORK)}.")
                new_caps["cpu"]["work"] = [w for w in CPU_WORK if w in work]
        if new_caps["cpu"]["enabled"] and (row.kind or "owned") in ("api", "test"):
            raise SettingsError("An online API or a test machine has no CPU of its own to share.")
    new_modes = modes(row)
    if "modes" in body:
        given = _object(body["modes"], "modes", SECTIONS)
        for section, value in given.items():
            if value not in MODES:
                raise SettingsError(f"modes.{section} is 'auto' or 'manual'.")
            new_modes[section] = value

    changed: dict = {}
    if slots is not None and slots != (row.max_concurrent or 1):
        changed["slots"] = {"from": row.max_concurrent or 1, "to": slots}
        row.max_concurrent = slots
    if new_caps != caps:
        for part in ("llm", "cpu"):
            for k, v in new_caps[part].items():
                if caps[part][k] != v:
                    changed[f"{part}.{k}"] = {"from": caps[part][k], "to": v}
        row.capabilities = new_caps
    old_modes = modes(row)
    for section in SECTIONS:
        if new_modes[section] != old_modes[section]:
            changed[f"{section}_mode"] = {"from": old_modes[section], "to": new_modes[section]}
            setattr(row, f"{section}_mode", new_modes[section])
    return changed


# ---------------------------------------------------------------------------- what a helper reports

def valid_scan(hardware) -> Optional[dict]:
    """A helper's hardware scan as kept: a CITAR hardware dict (``hwinfo.collect``'s, ``format`` ``citar-hardware``),
    cleaned (``servers.clean_hardware``: a figure that is not finite or that no float holds dropped, its parts of the
    shapes the readers take), of at most ``SCAN_BYTES_MAX`` as JSON; None for anything else (none sent, or not a
    scan). A scan is stored, applied and shown to its owner, administrators and whoever may see the machine, so what
    a helper sends must never be what fails those pages."""
    if not isinstance(hardware, dict) or hardware.get("format") != "citar-hardware":
        return None
    from . import servers
    scan = servers.clean_hardware(hardware)
    try:
        size = len(json.dumps(scan, separators=(",", ":"), allow_nan=False))
    except (TypeError, ValueError):     # the cleaning leaves nothing that would not encode: a guard, logged
        log.warning("a helper's hardware scan that does not encode as JSON was not kept", exc_info=True)
        return None
    if size > SCAN_BYTES_MAX:
        log.warning("a helper's hardware scan of %d bytes was not kept (at most %d)", size, SCAN_BYTES_MAX)
        return None
    return scan


def valid_models(models) -> Optional[list]:
    """The models a helper reported, as kept: ``{key, label, context, loaded}`` each (a key of text, at most
    ``MODELS_MAX`` of them, each once); None when nothing was reported (not a list)."""
    if not isinstance(models, list):
        return None
    out, seen = [], set()
    for m in models:
        if not isinstance(m, dict):
            continue
        key = m.get("key")
        if not isinstance(key, str) or not key.strip() or len(key) > KEY_MAX or key in seen:
            continue
        seen.add(key)
        label = m.get("label") if isinstance(m.get("label"), str) else ""
        context = m.get("context")
        entry = {"key": key, "label": label[:KEY_MAX], "loaded": m.get("loaded") is True}
        if isinstance(context, int) and not isinstance(context, bool) and 0 < context <= 100_000_000:
            entry["context"] = context
        out.append(entry)
        if len(out) >= MODELS_MAX:
            break
    return out


def _key(m) -> str:
    if isinstance(m, str):
        return m
    if isinstance(m, dict):
        return str(m.get("key") or m.get("model") or "")
    return ""


def _says_more(label: str, key: str) -> bool:
    """Whether a reported label says more than the model's key (LM Studio's helper reports the key as its label):
    only such a label replaces the one the catalogue has, which may be its owner's display name."""
    return bool(label) and label != key


def merge_models(stored, reported: list, *, full: bool) -> list:
    """The catalogue ``stored`` (a machine's ``config.models``) after a report of ``reported`` (``valid_models``).
    ``full`` (models ``auto``, or an apply): each reported model's ``loaded`` and context are updated (its label when
    it says more than its key) and it is marked ``reported``; a new one is added after the others; one no longer
    reported is kept, ``reported: false`` and not loaded. Not ``full`` (models ``manual``): only the ``loaded`` flags
    of the models listed change. Everything else an entry holds (its display name, inference settings, load
    profiles, price, whether it is offered) is kept."""
    by_key = {r["key"]: r for r in reported}
    out, seen = [], set()
    for m in stored if isinstance(stored, list) else []:
        key = _key(m)
        if not key or key in seen:
            continue
        seen.add(key)
        entry = dict(m) if isinstance(m, dict) else {"key": key}
        r = by_key.get(key)
        entry["loaded"] = bool(r and r.get("loaded"))
        if full:
            entry["reported"] = r is not None
            if r is not None:
                if _says_more(r.get("label") or "", key):
                    entry["label"] = r["label"]
                if r.get("context"):
                    entry["context"] = r["context"]
        out.append(entry)
    if full:
        for r in reported:
            if r["key"] in seen:
                continue
            entry = {"key": r["key"], "label": r["label"] if _says_more(r.get("label") or "", r["key"]) else "",
                     "loaded": bool(r.get("loaded")), "reported": True}
            if r.get("context"):
                entry["context"] = r["context"]
            out.append(entry)
    return out


def _hardware_of(scan: dict) -> dict:
    """A scan as a configuration's hardware, with ``servers.apply_hardware``'s source tag (it checks the format)."""
    from . import servers
    return servers.apply_hardware({"hardware": {}, "power": {}}, scan, SOURCE, set_power=False)["hardware"]


def suggested_power(scan) -> dict:
    """The power figures a scan suggests (``hardware.power.suggested``): ``{idle_w, cpu_max_w, gpu_max_w}``, those it
    has a number for."""
    power = scan.get("power") if isinstance(scan, dict) and isinstance(scan.get("power"), dict) else {}
    sug = power.get("suggested") if isinstance(power.get("suggested"), dict) else {}
    out = {}
    for k, _label in POWER_FIELDS:
        v = sug.get(k)
        # the range first: it leaves out NaN and infinity, and never overflows on an integer no float holds
        if isinstance(v, (int, float)) and not isinstance(v, bool) and 0 <= v <= 20000:
            out[k] = v
    return out


def _apply_power(config: dict, scan: dict) -> bool:
    """Put a scan's suggested power figures into ``config.power`` (as a guess); whether there were any."""
    figures = suggested_power(scan)
    if not figures:
        return False
    from . import servers
    power = dict(config.get("power") if isinstance(config.get("power"), dict) else servers.default_power())
    power.update(figures)
    power["source"] = "guess"
    config["power"] = power
    return True


def _reads(row, config: dict) -> bool:
    """Whether a configuration still reads as a machine (``normalize_server``): a report never leaves a machine whose
    configuration the registry would leave out."""
    from . import servers
    try:
        servers.normalize_server({**config, "id": row.id, "name": row.name or "machine", "kind": row.kind})
        return True
    except Exception:
        return False


def record_report(row, *, hardware=None, models=None, helper: Optional[dict] = None,
                  now: Optional[datetime] = None) -> dict:
    """Keep what a machine's helper reported (its hello: its hardware scan and its models; a catalog frame: its
    models) in ``hardware_detected`` and ``detected_at``, then apply it as the row's modes say (the module's doc).
    A part not sent (no scan: a helper started with ``--no-hardware``; no models) leaves its last report in place.
    Returns ``{"kept": [sections], "applied": [sections]}``."""
    now = now or utcnow()
    scan = valid_scan(hardware)
    reported = valid_models(models)
    if scan is None and reported is None:
        return {"kept": [], "applied": []}
    stamp = now.isoformat()
    detected = dict(row.hardware_detected) if isinstance(row.hardware_detected, dict) else {}
    config = copy.deepcopy(dict(row.config or {}))
    mode = modes(row)
    kept, applied = [], []
    if scan is not None:
        detected.update(hardware=scan, hardware_at=stamp)
        kept.append("hardware")
        if mode["hardware"] == "auto":
            config["hardware"] = _hardware_of(scan)
            applied.append("hardware")
        if mode["power"] == "auto" and _apply_power(config, scan):
            applied.append("power")
    if reported is not None:
        detected.update(models=reported, models_at=stamp)
        kept.append("models")
        config["models"] = merge_models(config.get("models"), reported, full=mode["models"] == "auto")
        applied.append("models" if mode["models"] == "auto" else "loaded")
    if helper:
        detected["helper"] = {k: str(helper.get(k) or "")[:120] for k in ("hostname", "worker_version")}
    row.hardware_detected = detected
    row.detected_at = now
    if _reads(row, config):
        row.config = config
    else:
        log.warning("machine %s: its helper's report was kept but not applied (the configuration would not read)",
                    row.id)
        applied = []
    return {"kept": kept, "applied": applied}


def apply_scan(row, sections=None) -> list:
    """Copy the last report into the configuration, whatever the modes: the sections named (``hardware``: the scan;
    ``power``: its suggested figures; ``models``: the reported models merged as ``auto`` would), or with ``sections``
    None or empty every section the report has something for (a helper started with ``--no-hardware`` reports models
    alone; a collector's file may suggest no power figures). A section named with nothing to copy, or a report with
    nothing at all, is a ``SettingsError`` (409), and nothing is changed. Returns the sections applied."""
    every = sections is None or sections == []
    if every:
        sections = list(SECTIONS)
    if not isinstance(sections, list) or any(s not in SECTIONS for s in sections):
        raise SettingsError(f"sections is a list of {', '.join(SECTIONS)}.")
    sections = [s for s in SECTIONS if s in sections]
    detected = row.hardware_detected if isinstance(row.hardware_detected, dict) else {}
    scan = detected.get("hardware") if isinstance(detected.get("hardware"), dict) else None
    reported = detected.get("models") if isinstance(detected.get("models"), list) else None
    config = copy.deepcopy(dict(row.config or {}))
    applied = []
    for section in sections:
        missing = None
        if section == "hardware":
            if scan is None:
                missing = "No hardware scan from its helper yet"
            else:
                config["hardware"] = _hardware_of(scan)
        elif section == "power":
            if scan is None or not _apply_power(config, scan):
                missing = "Its helper's scan suggested no power figures"
        else:
            if reported is None:
                missing = "No models reported by its helper yet"
            else:
                config["models"] = merge_models(config.get("models"), valid_models(reported) or [], full=True)
        if missing is None:
            applied.append(section)
        elif not every:
            raise SettingsError(f"{missing}: nothing to apply.", 409)
    if not applied:
        raise SettingsError("Nothing reported by its helper yet: nothing to apply.", 409)
    if not _reads(row, config):
        raise SettingsError("The last scan does not read as a machine's configuration; nothing was applied.", 409)
    row.config = config
    return applied


# ---------------------------------------------------------------------------- the configuration against the scan

def _part(d, key) -> dict:
    v = d.get(key) if isinstance(d, dict) else None
    return v if isinstance(v, dict) else {}


def _many(d, key) -> list:
    v = d.get(key) if isinstance(d, dict) else None
    return [x for x in v if isinstance(x, dict)] if isinstance(v, list) else []


def _num(v) -> Optional[float]:
    """A number a scan or a configuration holds (text included), or None (an integer no float holds included)."""
    if isinstance(v, bool):
        return None
    try:
        x = float(v)
    except (TypeError, ValueError, OverflowError):
        return None
    return x if math.isfinite(x) else None


def _text(v) -> Optional[str]:
    if v is None or v == "":
        return None
    if isinstance(v, bool):
        return "yes" if v else "no"
    if isinstance(v, float):
        return f"{v:g}"
    return str(v)


def hardware_facts(hw) -> dict:
    """What the dialog compares of a machine's hardware: ``{label: text or None}``."""
    hw = hw if isinstance(hw, dict) else {}
    cpu, mem, system = _part(hw, "cpu"), _part(hw, "memory"), _part(hw, "system")
    gpus = []
    for g in _many(hw, "gpus"):
        vram = _num(g.get("vram_gb"))
        name = str(g.get("name") or "GPU")
        gpus.append(f"{name} ({vram:g} GB)" if vram else name)
    disk = sum(_num(d.get("size_gb")) or 0.0 for d in _many(hw, "physical_disks"))
    ram = _num(mem.get("ram_gb"))
    cores, threads = _num(cpu.get("cores")), _num(cpu.get("threads"))
    return {"Form": _text(system.get("form")), "CPU": _text(cpu.get("model") or cpu.get("name")),
            "Cores": _text(int(cores)) if cores else None, "Threads": _text(int(threads)) if threads else None,
            "RAM": f"{ram:g} GB" if ram else None,
            "Unified memory": _text(mem.get("unified")) if isinstance(mem.get("unified"), bool) else None,
            "GPUs": "; ".join(gpus) or None, "Disks": f"{disk:g} GB" if disk else None}


def differences(config, detected) -> dict:
    """How a machine's configuration and its helper's last report differ, section by section: ``{section: [{what,
    configured, detected}]}``, a section empty when they agree (or nothing was reported for it). Only what the report
    says is compared: a scan without a figure leaves the configured one alone, as applying it would."""
    config = config if isinstance(config, dict) else {}
    detected = detected if isinstance(detected, dict) else {}
    out: dict = {s: [] for s in SECTIONS}
    scan = detected.get("hardware") if isinstance(detected.get("hardware"), dict) else None
    if scan is not None:
        mine, theirs = hardware_facts(config.get("hardware")), hardware_facts(scan)
        for what, value in theirs.items():
            if value is not None and mine.get(what) != value:
                out["hardware"].append({"what": what, "configured": mine.get(what), "detected": value})
        figures = suggested_power(scan)
        power = _part(config, "power")
        for k, label in POWER_FIELDS:
            if k in figures and _num(power.get(k)) != float(figures[k]):
                out["power"].append({"what": label, "configured": _text(power.get(k)), "detected": _text(figures[k])})
    reported = detected.get("models") if isinstance(detected.get("models"), list) else None
    if reported is not None:
        stored = {}
        for m in config.get("models") or []:
            if _key(m):
                stored.setdefault(_key(m), m if isinstance(m, dict) else {})
        theirs = {r.get("key"): r for r in reported if isinstance(r, dict) and r.get("key")}
        for key, r in theirs.items():
            state = "reported, loaded" if r.get("loaded") else "reported"
            m = stored.get(key)
            if m is None:
                out["models"].append({"what": key, "configured": "not in the catalogue", "detected": state})
                continue
            if bool(m.get("loaded")) != bool(r.get("loaded")) or m.get("reported") is False:
                out["models"].append({"what": key, "configured": "loaded" if m.get("loaded") else
                                      "not reported" if m.get("reported") is False else "not loaded",
                                      "detected": state})
            if r.get("context") and m.get("context") != r.get("context"):
                out["models"].append({"what": f"{key}: context", "configured": _text(m.get("context")),
                                      "detected": _text(r["context"])})
            if _says_more(r.get("label") or "", key) and m.get("label") != r.get("label"):
                out["models"].append({"what": f"{key}: name", "configured": _text(m.get("label")),
                                      "detected": r["label"]})
        for key, m in stored.items():
            if key not in theirs and m.get("reported") is not False:
                out["models"].append({"what": key, "configured": "in the catalogue", "detected": "not reported"})
    return out


# ---------------------------------------------------------------------------- the dialog's read

def _when(v) -> Optional[str]:
    if isinstance(v, datetime):
        return (v if v.tzinfo else v.replace(tzinfo=timezone.utc)).isoformat()
    return v if isinstance(v, str) else None


def _class_entry(c: Optional[dict]) -> Optional[dict]:
    return None if c is None else {"id": c["id"], "name": c["name"], "kind": c["kind"], "rate": c["rate"]}


def client(session, row, viewer, *, in_effect: Optional[dict] = None) -> dict:
    """The dialog's read of a machine (``GET /api/machines/{id}/settings``): its capabilities and slots (and what the
    prompt scheduler has in effect for it, ``in_effect``), its modes, its helper's last report with the differences
    from its configuration, and its class and trust tier. An administrator also gets the classes to pin and the
    machine's ``flags`` (its helper over-reporting its generation time, P3.3.4)."""
    from sqlalchemy import select

    from . import ledger, servers
    from .db.models import HardwareClass
    admin = viewer is not None and viewer.role == "admin"
    config = dict(row.config or {})
    detected = row.hardware_detected if isinstance(row.hardware_detected, dict) else {}
    classes = [ledger.class_dict(c) for c in session.scalars(select(HardwareClass))]
    facts = {"hardware": config.get("hardware") or {}, "class_override": bool(row.class_override),
             "gpu_class_id": row.gpu_class_id, "cpu_class_id": row.cpu_class_id}
    api = (row.kind or "owned") == "api"
    gpu = ledger.class_of(facts, "api" if api else "gpu", classes)
    cpu = None if api else ledger.class_of(facts, "cpu", classes)
    scan = detected.get("hardware") if isinstance(detected.get("hardware"), dict) else None
    try:
        configured = servers.row_registry(row)
    except Exception:
        configured = {"hardware": config.get("hardware") or {}, "power": config.get("power") or {}, "models": []}
    out = {
        "id": row.id, "name": row.name, "kind": row.kind, "provider": row.provider, "reach": row.reach or "worker",
        "is_host": bool(row.is_host), "enabled": bool(row.enabled),
        "slots": row.max_concurrent or 1, "slots_max": SLOTS_MAX,
        "capabilities": capabilities(row.capabilities), "cpu_work": list(CPU_WORK),
        "modes": modes(row), "in_effect": in_effect,
        "report": {
            "at": _when(row.detected_at), "helper": detected.get("helper") or None,
            "hardware": scan, "hardware_at": detected.get("hardware_at"),
            "hardware_summary": servers.hardware_summary(dict(scan, source=SOURCE)) if scan else None,
            "power": suggested_power(scan) if scan else {},
            "models": detected.get("models") if isinstance(detected.get("models"), list) else None,
            "models_at": detected.get("models_at")},
        "configured": {"hardware_summary": servers.hardware_summary(configured.get("hardware") or {}),
                       "hardware_source": (configured.get("hardware") or {}).get("source") or "none",
                       "power": {k: (configured.get("power") or {}).get(k) for k in
                                 ("idle_w", "cpu_max_w", "gpu_max_w", "source")},
                       "models": [{"key": m.get("key"), "label": m.get("label") or "", "loaded": m.get("loaded"),
                                   "reported": m.get("reported"), "enabled": m.get("enabled", True)}
                                  for m in configured.get("models") or []]},
        "differences": differences(config, detected),
        "class": {"gpu": _class_entry(gpu), "cpu": _class_entry(cpu), "pinned": bool(row.class_override),
                  "kinds": ["api"] if api else ["gpu", "cpu"]},
        "trust_tier": row.trust_tier or "new",
        "can_admin": admin,
    }
    if admin:
        out["classes"] = [_class_entry(c) | {"is_default": c["is_default"]} for c in classes]
        out["flags"] = dict(row.flags or {})
    return out

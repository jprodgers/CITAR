"""The server registry (config/servers.json): every machine or online service that can run a model for CITAR, with
everything needed to use it and to cost it.

A server has
    connection        how to reach it: provider (lmstudio | ollama | openai_compatible | anthropic | dryrun | none),
                      base URL, LM Link device (a model on another PC reached through this PC's LM Studio), API key
                      backend (see keystore.py), how many games may use it at once, whether CITAR loads models itself
    hardware          CPU, GPUs, RAM (or unified memory), disks... collected by hwinfo.py or typed in
    power             idle watts and the extra watts at full CPU / full GPU load, used to estimate energy wherever it
                      isn't sampled live
    components        owned hardware with price, purchase date, lifespan and resale value (depreciated straight-line)
    costs             effective-dated cost periods: electricity plan, fixed monthly costs (lease, maintenance), an
                      hourly rate while in use (rented machines), per-token prices (APIs)
    restricted_hours  windows (per weekday) when queued work must pause on it, optionally unloading its models
    models            the model catalog: key, label, inference defaults and load profiles (context, GPU offload...)

Electricity plans are shared (both home PCs are on the same bill). One server is the CITAR host: the machine running
this program, whose CPU runs the game engine and the scripted bots.
"""
from __future__ import annotations

import contextlib
import copy
import json
import re
import secrets
import threading
import time
from datetime import datetime, timedelta
from typing import Optional
from . import paths

CONFIG_DIR = paths.config_path()
PATH = CONFIG_DIR / "servers.json"
PROVIDERS = ("lmstudio", "ollama", "openai_compatible", "anthropic", "dryrun", "none")
KINDS = ("owned", "leased", "api", "test")
DAY_NAMES = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")
EPOCH_DATE = "2000-01-01"

# Anthropic list prices, $ per million tokens (cache_write = 5-minute writes, what CITAR's prompt caching uses)
ANTHROPIC_PRICES = {
    "claude-fable-5-1": {"input": 10.0, "output": 50.0, "cache_write": 12.5, "cache_read": 0.25},
    "claude-fable-5": {"input": 10.0, "output": 50.0, "cache_write": 12.5, "cache_read": 1.0},
    "claude-opus-5": {"input": 5.0, "output": 25.0, "cache_write": 6.25, "cache_read": 0.5},
    "claude-opus-4-8": {"input": 5.0, "output": 25.0, "cache_write": 6.25, "cache_read": 0.5},
    "claude-opus-4-7": {"input": 5.0, "output": 25.0, "cache_write": 6.25, "cache_read": 0.5},
    "claude-opus-4-6": {"input": 5.0, "output": 25.0, "cache_write": 6.25, "cache_read": 0.5},
    "claude-sonnet-5": {"input": 2.0, "output": 10.0, "cache_write": 2.5, "cache_read": 0.2},
    "claude-sonnet-4-6": {"input": 3.0, "output": 15.0, "cache_write": 3.75, "cache_read": 0.3},
    "claude-haiku-4-5": {"input": 1.0, "output": 5.0, "cache_write": 1.25, "cache_read": 0.1},
}

_lock = threading.RLock()
_cache: dict = {"mtime": None, "data": None}


class ServerError(ValueError):
    """A registry change that cannot be made: a bad value, a missing reference, a duplicate id."""
    pass


def new_id(prefix: str) -> str:
    """A new opaque id with a type prefix, such as ``sv_`` or ``m_``."""
    return prefix + secrets.token_hex(4)


# ----------------------------------------------------------------------------- defaults
def default_power() -> dict:
    """A power profile with nothing measured yet, which reports say is an estimate."""
    return {"idle_w": None, "cpu_max_w": None, "gpu_max_w": None, "sampling": "auto", "measured_overhead_pct": 10,
            "source": "unset"}


def default_restricted() -> dict:
    """Restricted hours, off by default."""
    return {"enabled": False, "windows": [], "unload_models": True, "grace_minutes": 15}


# A load profile is LM Studio's load configuration: what CITAR passes when it loads the model. "use_defaults" leaves
# everything to LM Studio (the settings you saved for that model in the app), which is the safest option for a model
# you have already tuned there.
PROFILE_FLAGS = ("strict_vram_cap", "offload_kv_to_gpu", "flash_attention", "keep_in_memory", "try_mmap")


def default_profile(**kw) -> dict:
    """A load profile with settings that work on most machines: 32k context, full GPU offload."""
    p = {"id": "p_default", "name": "Default", "loaded_by": "citar", "use_defaults": False, "context": 32768, "gpu": "max",
         "fields": [], "main_gpu": None,
         "split_strategy": "", "strict_vram_cap": None, "offload_kv_to_gpu": None, "flash_attention": None,
         "keep_in_memory": None, "try_mmap": None, "eval_batch": None, "num_experts": None, "k_cache_quant": "",
         "v_cache_quant": "", "parallel": None, "ttl": None, "exclusive": True, "extra": ""}
    p.update(kw)
    return p


CACHE_QUANTS = ("", "f32", "f16", "q8_0", "q5_1", "q5_0", "q4_1", "q4_0", "iq4_nl")


def sdk_config(prof: Optional[dict]) -> dict:
    """A load profile as LM Studio's LlmLoadModelConfig (only the fields the profile actually sets)."""
    if not prof or prof.get("loaded_by", "citar") != "citar":
        return {}
    cfg: dict = {}
    gpu: dict = {}
    g = str(prof.get("gpu") or "").strip().lower()
    if g == "max":
        gpu["ratio"] = 1.0
    elif g == "off":
        gpu["ratio"] = 0.0
    elif g and g != "auto":
        try:
            gpu["ratio"] = max(0.0, min(1.0, float(g)))
        except ValueError:
            pass
    if prof.get("main_gpu") is not None:
        gpu["mainGpu"] = int(prof["main_gpu"])
    if prof.get("split_strategy"):
        gpu["splitStrategy"] = prof["split_strategy"]
    if gpu:
        cfg["gpu"] = gpu
    if prof.get("context"):
        cfg["contextLength"] = int(prof["context"])
    for key, field in (("strict_vram_cap", "gpuStrictVramCap"), ("offload_kv_to_gpu", "offloadKVCacheToGpu"),
                       ("flash_attention", "flashAttention"), ("keep_in_memory", "keepModelInMemory"),
                       ("try_mmap", "tryMmap")):
        if prof.get(key) is not None:
            cfg[field] = bool(prof[key])
    if prof.get("eval_batch"):
        cfg["evalBatchSize"] = int(prof["eval_batch"])
    if prof.get("num_experts"):
        cfg["numExperts"] = int(prof["num_experts"])
    if prof.get("k_cache_quant"):
        cfg["llamaKCacheQuantizationType"] = prof["k_cache_quant"]
    if prof.get("v_cache_quant"):
        cfg["llamaVCacheQuantizationType"] = prof["v_cache_quant"]
    return cfg


def profile_from_config(cfg: dict, name: str = "Copied from LM Studio") -> dict:
    """Turn a live LM Studio load configuration into a CITAR load profile (the "copy what is loaded now" button)."""
    gpu = cfg.get("gpu") or {}
    ratio = gpu.get("ratio")
    return default_profile(id=new_id("p_"), name=name,
                           gpu="max" if ratio in (1, 1.0) else "off" if ratio in (0, 0.0) else (str(ratio) if ratio is not None else "auto"),
                           main_gpu=gpu.get("mainGpu"), split_strategy=gpu.get("splitStrategy") or "",
                           context=cfg.get("contextLength") or 0, strict_vram_cap=cfg.get("gpuStrictVramCap"),
                           offload_kv_to_gpu=cfg.get("offloadKVCacheToGpu"), flash_attention=cfg.get("flashAttention"),
                           keep_in_memory=cfg.get("keepModelInMemory"), try_mmap=cfg.get("tryMmap"),
                           eval_batch=cfg.get("evalBatchSize"), num_experts=cfg.get("numExperts"),
                           k_cache_quant=cfg.get("llamaKCacheQuantizationType") if isinstance(cfg.get("llamaKCacheQuantizationType"), str) else "",
                           v_cache_quant=cfg.get("llamaVCacheQuantizationType") if isinstance(cfg.get("llamaVCacheQuantizationType"), str) else "")


def default_model(key: str, provider: str = "lmstudio", **kw) -> dict:
    """A model entry with inference defaults suited to where it runs.

    Local models default to low reasoning effort and automatic tool mode, because that is what makes
    them usable - about four times faster with no loss in play quality. Hosted models default to native
    tool calling, which they do well.
    """
    local = provider in ("lmstudio", "ollama", "openai_compatible")
    m = {"id": new_id("m_"), "key": key, "label": "", "enabled": True, "info": {},
         "inference": {"tool_mode": "auto" if local else "native", "reasoning_effort": "low" if local else "",
                       "effort": "", "max_tokens": None, "temperature": None, "max_tool_calls_per_turn": 150},
         "profiles": [default_profile()] if provider == "lmstudio" else [],
         "default_profile": "p_default" if provider == "lmstudio" else None, "price": None}
    m.update(kw)
    return m


def default_cost_period(kind: str, **kw) -> dict:
    """A cost period with everything at zero, effective from the epoch."""
    p = {"from": EPOCH_DATE, "electricity_plan_id": None, "fixed_monthly": 0.0, "fixed_note": "",
         "hourly_rate": 0.0, "api_fixed_monthly": 0.0, "default_price": None, "note": ""}
    p.update(kw)
    return p


def default_server(kind: str = "owned", provider: str = "lmstudio", **kw) -> dict:
    """A new server entry of a kind and provider, with sensible defaults throughout."""
    now = time.time()
    sv = {"id": new_id("sv_"), "name": "New server", "description": "", "kind": kind,
          "connection": {"provider": provider, "base_url": "http://localhost:1234/v1" if provider == "lmstudio" else "",
                         "lmstudio_device": "", "manage_loading": provider == "lmstudio", "max_parallel": 1,
                         "key": {"backend": "none", "env": ""}, "dry_run_delay": 1.0, "timeout": None},
          "hardware": {"source": "none"}, "power": default_power(), "components": [],
          "costs": [default_cost_period(kind)], "restricted_hours": default_restricted(), "models": [],
          "notes": "", "created": now, "updated": now}
    sv.update(kw)
    return sv


def default_plan(**kw) -> dict:
    """A new electricity plan with one flat, unpriced period."""
    p = {"id": new_id("ep_"), "name": "Home electricity", "periods": [
        {"from": EPOCH_DATE, "type": "flat", "rate_kwh": None, "tou": [], "tiers": [], "fixed_monthly": 0.0,
         "fee_allocation": "household_kwh", "household_kwh_month": None, "share_pct": 0.0, "note": ""}]}
    p.update(kw)
    return p


def empty_registry() -> dict:
    """A registry with no servers in it."""
    return {"format": "citar-servers", "version": 1, "currency": "USD", "currency_symbol": "$", "host_server_id": None,
            "electricity_plans": [], "servers": []}


# ----------------------------------------------------------------------------- normalize
def _num(v, default=None, lo=None, hi=None):
    """Coerce a value to a number within bounds, falling back to a default.

    Everything in the registry arrives from a browser or a JSON file, so every field is validated on
    the way in rather than trusted and crashed on later.
    """
    if v in (None, ""):
        return default
    try:
        x = float(v)
    except (TypeError, ValueError):
        return default
    if lo is not None:
        x = max(lo, x)
    if hi is not None:
        x = min(hi, x)
    return x


def _date(v, default=EPOCH_DATE) -> str:
    """Coerce a value to a ``YYYY-MM-DD`` date string."""
    s = str(v or "").strip()[:10]
    try:
        datetime.strptime(s, "%Y-%m-%d")
        return s
    except ValueError:
        return default


def _hhmm(v, default="00:00") -> str:
    """Coerce a value to a ``HH:MM`` time string."""
    s = str(v or "").strip()
    return s if re.fullmatch(r"([01]\d|2[0-3]):[0-5]\d", s) else default


def _price(p) -> Optional[dict]:
    """Validate a token price block, or None."""
    if not isinstance(p, dict):
        return None
    out = {k: _num(p.get(k), 0.0, 0) for k in ("input", "output", "cache_write", "cache_read")}
    return out if any(out.values()) or p.get("input") is not None else None


def normalize_server(d: dict) -> dict:
    """Validate and complete a server entry, filling in every field the rest of CITAR expects.

    The single place a server is checked. Everything downstream - costing, benchmarks, the seat
    resolver - assumes a fully-formed entry, which is only true because nothing else writes one.
    """
    kind = d.get("kind") if d.get("kind") in KINDS else "owned"
    conn_in = d.get("connection") or {}
    provider = conn_in.get("provider") if conn_in.get("provider") in PROVIDERS else "none"
    sv = default_server(kind, provider)
    sv["id"] = d.get("id") if re.fullmatch(r"[A-Za-z0-9_\-]{3,40}", str(d.get("id") or "")) else sv["id"]
    sv["name"] = (str(d.get("name") or "").strip() or "Unnamed server")[:80]
    sv["description"] = str(d.get("description") or "")[:2000]
    sv["notes"] = str(d.get("notes") or "")[:4000]
    sv["created"] = d.get("created") or sv["created"]
    c = sv["connection"]
    c["base_url"] = str(conn_in.get("base_url") or "").strip().rstrip("/")
    if provider == "lmstudio" and not c["base_url"]:
        c["base_url"] = "http://localhost:1234/v1"
    if provider == "ollama" and not c["base_url"]:
        c["base_url"] = "http://localhost:11434/v1"
    c["lmstudio_device"] = str(conn_in.get("lmstudio_device") or "").strip()
    c["manage_loading"] = bool(conn_in.get("manage_loading", provider == "lmstudio")) and provider == "lmstudio"
    c["max_parallel"] = int(_num(conn_in.get("max_parallel"), 1, 1, 16))
    key = conn_in.get("key") or {}
    backend = key.get("backend") if key.get("backend") in ("none", "keyring", "env", "file") else "none"
    c["key"] = {"backend": backend, "env": str(key.get("env") or "").strip()}
    c["dry_run_delay"] = _num(conn_in.get("dry_run_delay"), 1.0, 0, 60)
    c["timeout"] = _num(conn_in.get("timeout"), None, 5, 7200)
    hw = d.get("hardware") if isinstance(d.get("hardware"), dict) else {}
    sv["hardware"] = hw or {"source": "none"}
    pw_in = d.get("power") or {}
    pw = default_power()
    for k in ("idle_w", "cpu_max_w", "gpu_max_w"):
        pw[k] = _num(pw_in.get(k), None, 0, 20000)
    pw["sampling"] = pw_in.get("sampling") if pw_in.get("sampling") in ("auto", "off") else "auto"
    pw["measured_overhead_pct"] = _num(pw_in.get("measured_overhead_pct"), 10, 0, 100)
    pw["source"] = str(pw_in.get("source") or ("manual" if pw["idle_w"] is not None else "unset"))[:20]
    sv["power"] = pw
    sv["components"] = []
    for comp in d.get("components") or []:
        if not isinstance(comp, dict):
            continue
        sv["components"].append({"id": comp.get("id") or new_id("c_"), "name": str(comp.get("name") or "Component")[:80],
                                 "price": _num(comp.get("price"), None, 0), "purchased": _date(comp.get("purchased"), ""),
                                 "lifespan_years": _num(comp.get("lifespan_years"), 4, 0.25, 30),
                                 "resale": _num(comp.get("resale"), 0.0, 0),
                                 "retired": _date(comp.get("retired"), "") or None})
    periods = []
    for p in d.get("costs") or [default_cost_period(kind)]:
        q = default_cost_period(kind)
        q["from"] = _date(p.get("from"))
        q["electricity_plan_id"] = p.get("electricity_plan_id") or None
        for k in ("fixed_monthly", "hourly_rate", "api_fixed_monthly"):
            q[k] = _num(p.get(k), 0.0, 0)
        q["fixed_note"] = str(p.get("fixed_note") or "")[:200]
        q["note"] = str(p.get("note") or "")[:500]
        q["default_price"] = _price(p.get("default_price"))
        periods.append(q)
    periods.sort(key=lambda p: p["from"])
    sv["costs"] = periods or [default_cost_period(kind)]
    rh_in = d.get("restricted_hours") or {}
    rh = default_restricted()
    rh["enabled"] = bool(rh_in.get("enabled"))
    rh["unload_models"] = bool(rh_in.get("unload_models", True))
    rh["grace_minutes"] = _num(rh_in.get("grace_minutes"), 15, 0, 240)
    for w in rh_in.get("windows") or []:
        days = sorted({int(x) for x in (w.get("days") if w.get("days") is not None else range(7)) if str(x).lstrip("-").isdigit() and 0 <= int(x) <= 6})
        rh["windows"].append({"days": days, "start": _hhmm(w.get("start"), "21:00"), "end": _hhmm(w.get("end"), "06:00")})
    sv["restricted_hours"] = rh
    models, seen = [], set()
    for m in d.get("models") or []:
        if isinstance(m, str):
            m = {"key": m}
        key = str(m.get("key") or m.get("model") or "").strip()
        if not key or key in seen:
            continue
        seen.add(key)
        mm = default_model(key, provider)
        mm["id"] = m.get("id") or mm["id"]
        mm["label"] = str(m.get("label") or "")[:80]
        mm["enabled"] = bool(m.get("enabled", True))
        mm["info"] = m.get("info") if isinstance(m.get("info"), dict) else {}
        inf = m.get("inference") or {}
        mm["inference"].update({k: inf[k] for k in mm["inference"] if k in inf})
        if mm["inference"]["tool_mode"] not in ("auto", "native", "json"):
            mm["inference"]["tool_mode"] = "auto"
        mm["inference"]["max_tool_calls_per_turn"] = int(_num(mm["inference"].get("max_tool_calls_per_turn"), 150, 5, 1000))
        mm["inference"]["max_tokens"] = int(_num(mm["inference"].get("max_tokens"), 0, 0, 200000)) or None
        mm["inference"]["temperature"] = _num(mm["inference"].get("temperature"), None, 0, 2)
        profiles = []
        for pr in m.get("profiles") or ([default_profile()] if provider == "lmstudio" else []):
            q = default_profile()
            q["id"] = pr.get("id") or new_id("p_")
            q["name"] = str(pr.get("name") or "Profile")[:60]
            q["context"] = int(_num(pr.get("context"), 32768, 0, 4_000_000))
            gpu = str(pr.get("gpu") if pr.get("gpu") is not None else "max").strip().lower()
            q["gpu"] = gpu if gpu in ("max", "off", "auto") or re.fullmatch(r"0?\.\d+|1(\.0+)?|0", gpu) else "max"
            q["parallel"] = int(_num(pr.get("parallel"), 0, 0, 64)) or None
            q["ttl"] = int(_num(pr.get("ttl"), 0, 0, 86400 * 7)) or None
            q["exclusive"] = bool(pr.get("exclusive", True))
            q["extra"] = str(pr.get("extra") or "")[:300]
            q["loaded_by"] = pr.get("loaded_by") if pr.get("loaded_by") in ("citar", "lmstudio_defaults", "manual") else (
                "lmstudio_defaults" if pr.get("use_defaults") else "citar")
            q["use_defaults"] = q["loaded_by"] == "lmstudio_defaults"
            q["fields"] = [{"key": str(f.get("key"))[:80], "value": f.get("value")} for f in (pr.get("fields") or [])
                           if isinstance(f, dict) and str(f.get("key") or "").startswith("llm.load.")][:20]
            for flag in PROFILE_FLAGS:
                q[flag] = None if pr.get(flag) is None else bool(pr.get(flag))
            q["main_gpu"] = int(_num(pr.get("main_gpu"), -1, 0, 15)) if _num(pr.get("main_gpu"), -1, 0, 15) >= 0 else None
            q["split_strategy"] = pr.get("split_strategy") if pr.get("split_strategy") in ("evenly", "favorMainGpu") else ""
            q["eval_batch"] = int(_num(pr.get("eval_batch"), 0, 1, 8192)) or None
            q["num_experts"] = int(_num(pr.get("num_experts"), 0, 0, 1024)) or None
            q["k_cache_quant"] = pr.get("k_cache_quant") if pr.get("k_cache_quant") in CACHE_QUANTS else ""
            q["v_cache_quant"] = pr.get("v_cache_quant") if pr.get("v_cache_quant") in CACHE_QUANTS else ""
            profiles.append(q)
        mm["profiles"] = profiles
        ids = [p["id"] for p in profiles]
        mm["default_profile"] = m.get("default_profile") if m.get("default_profile") in ids else (ids[0] if ids else None)
        mm["price"] = _price(m.get("price"))
        models.append(mm)
    sv["models"] = models
    sv["updated"] = time.time()
    return sv


def normalize_plan(d: dict) -> dict:
    """Validate and complete an electricity plan."""
    p = default_plan()
    p["id"] = d.get("id") if re.fullmatch(r"[A-Za-z0-9_\-]{3,40}", str(d.get("id") or "")) else p["id"]
    p["name"] = (str(d.get("name") or "").strip() or "Electricity plan")[:80]
    periods = []
    for q in d.get("periods") or []:
        r = {"from": _date(q.get("from")), "type": q.get("type") if q.get("type") in ("flat", "tou", "tiered") else "flat",
             "rate_kwh": _num(q.get("rate_kwh"), None, 0, 100), "fixed_monthly": _num(q.get("fixed_monthly"), 0.0, 0),
             "fee_allocation": q.get("fee_allocation") if q.get("fee_allocation") in ("household_kwh", "share", "none") else "household_kwh",
             "household_kwh_month": _num(q.get("household_kwh_month"), None, 1, 1e7),
             "share_pct": _num(q.get("share_pct"), 0.0, 0, 100), "note": str(q.get("note") or "")[:500], "tou": [], "tiers": []}
        for w in q.get("tou") or []:
            days = sorted({int(x) for x in (w.get("days") if w.get("days") is not None else range(7)) if 0 <= int(x) <= 6})
            r["tou"].append({"days": days, "start": _hhmm(w.get("start")), "end": _hhmm(w.get("end"), "00:00"),
                             "rate_kwh": _num(w.get("rate_kwh"), 0.0, 0, 100), "name": str(w.get("name") or "")[:30]})
        for t in q.get("tiers") or []:
            r["tiers"].append({"up_to_kwh": _num(t.get("up_to_kwh"), None, 0), "rate_kwh": _num(t.get("rate_kwh"), 0.0, 0, 100)})
        periods.append(r)
    periods.sort(key=lambda q: q["from"])
    p["periods"] = periods or default_plan()["periods"]
    return p


# ----------------------------------------------------------------------------- storage
def _read() -> dict:
    """Read the registry from disk, returning an empty one if there is no file yet."""
    if not PATH.exists():
        return {}
    for _ in range(20):
        try:
            return json.loads(PATH.read_text(encoding="utf-8"))
        except PermissionError:
            time.sleep(0.1)
        except ValueError:
            break
    return {}


def load() -> dict:
    """The registry (cached until the file changes). Creates the default registry the first time."""
    with _lock:
        mtime = PATH.stat().st_mtime if PATH.exists() else None
        if _cache["data"] is not None and _cache["mtime"] == mtime:
            return _cache["data"]
        raw = _read()
        if not raw:
            data = bootstrap()
            save(data)
            return data
        data = empty_registry()
        data.update({k: raw[k] for k in ("currency", "currency_symbol", "host_server_id") if k in raw})
        data["electricity_plans"] = [normalize_plan(p) for p in raw.get("electricity_plans") or []]
        data["servers"] = []
        for s in raw.get("servers") or []:
            sv = normalize_server(s)
            sv["updated"] = s.get("updated") or sv["updated"]
            data["servers"].append(sv)
        _cache.update(mtime=mtime, data=data)
        return data


def save(data: dict):
    """Write the registry atomically.

    Atomic because the web page reads it while the lab writes it; a partial file would be read as a
    registry with no servers in it.
    """
    from .fsutil import write_text
    with _lock:
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        write_text(PATH, json.dumps(data, indent=1))
        _cache.update(mtime=PATH.stat().st_mtime, data=data)


def snapshot() -> dict:
    """A deep copy of the registry, safe to modify before saving."""
    return copy.deepcopy(load())


def list_servers() -> list[dict]:
    """Every server in the registry."""
    return load()["servers"]


def get(server_id: str) -> dict:
    """One server by id, raising if it does not exist."""
    for sv in load()["servers"]:
        if sv["id"] == server_id:
            return sv
    raise ServerError(f"No server '{server_id}'.")


def find(server_id: Optional[str]) -> Optional[dict]:
    """One server by id, or None."""
    try:
        return get(server_id) if server_id else None
    except ServerError:
        return None


def host() -> Optional[dict]:
    """The server representing the machine CITAR itself runs on.

    It matters for costing: that machine's CPU runs the engine, the bots and the lab, so a share of
    every game's cost is attributed to it rather than to whatever served the model.
    """
    return find(load().get("host_server_id"))


def upsert(data: dict) -> dict:
    """Create or replace a server, validating it and its references first."""
    with _lock:
        reg = snapshot()
        sv = normalize_server(data)
        for plan_id in {p.get("electricity_plan_id") for p in sv["costs"]} - {None}:
            if not any(pl["id"] == plan_id for pl in reg["electricity_plans"]):
                raise ServerError(f"Unknown electricity plan '{plan_id}'.")
        idx = next((i for i, s in enumerate(reg["servers"]) if s["id"] == sv["id"]), None)
        if idx is None:
            reg["servers"].append(sv)
        else:
            sv["created"] = reg["servers"][idx].get("created") or sv["created"]
            reg["servers"][idx] = sv
        if data.get("is_host"):
            reg["host_server_id"] = sv["id"]
        elif reg.get("host_server_id") == sv["id"] and data.get("is_host") is False:
            reg["host_server_id"] = None
        save(reg)
        return sv


def delete(server_id: str):
    """Remove a server."""
    with _lock:
        reg = snapshot()
        reg["servers"] = [s for s in reg["servers"] if s["id"] != server_id]
        if reg.get("host_server_id") == server_id:
            reg["host_server_id"] = None
        save(reg)


def upsert_plan(data: dict) -> dict:
    """Create or replace an electricity plan."""
    with _lock:
        reg = snapshot()
        p = normalize_plan(data)
        idx = next((i for i, x in enumerate(reg["electricity_plans"]) if x["id"] == p["id"]), None)
        if idx is None:
            reg["electricity_plans"].append(p)
        else:
            reg["electricity_plans"][idx] = p
        save(reg)
        return p


def delete_plan(plan_id: str):
    """Remove an electricity plan."""
    with _lock:
        reg = snapshot()
        users = [s["name"] for s in reg["servers"] if any(c.get("electricity_plan_id") == plan_id for c in s["costs"])]
        if users:
            raise ServerError(f"The plan is used by {', '.join(users)}.")
        reg["electricity_plans"] = [p for p in reg["electricity_plans"] if p["id"] != plan_id]
        save(reg)


def set_settings(patch: dict) -> dict:
    """Change registry-wide settings: currency, and which server is the host."""
    with _lock:
        reg = snapshot()
        if "currency" in patch:
            reg["currency"] = str(patch["currency"] or "USD")[:8].upper()
        if "currency_symbol" in patch:
            reg["currency_symbol"] = str(patch["currency_symbol"] or "$")[:4]
        if "host_server_id" in patch:
            if patch["host_server_id"] and not any(s["id"] == patch["host_server_id"] for s in reg["servers"]):
                raise ServerError("Unknown server.")
            reg["host_server_id"] = patch["host_server_id"] or None
        save(reg)
        return reg


def plan(plan_id: Optional[str]) -> Optional[dict]:
    """An electricity plan by id, or None."""
    return next((p for p in load()["electricity_plans"] if p["id"] == plan_id), None) if plan_id else None


# ----------------------------------------------------------------------------- time lookups
def period_at(periods: list[dict], when: datetime) -> Optional[dict]:
    """The effective-dated period in force at `when` (periods sorted by 'from')."""
    day = when.strftime("%Y-%m-%d")
    cur = None
    for p in periods:
        if p["from"] <= day:
            cur = p
        else:
            break
    return cur or (periods[0] if periods else None)


def _window_hit(windows: list[dict], when: datetime) -> Optional[tuple[dict, datetime]]:
    """The window containing `when` and the moment it ends. A window belongs to the weekday on which it starts, so an
    overnight window listed for Friday covers Friday 21:00 to Saturday 06:00."""
    for w in windows:
        sh, sm = map(int, w["start"].split(":"))
        eh, em = map(int, w["end"].split(":"))
        start_min, end_min = sh * 60 + sm, eh * 60 + em
        if start_min == end_min:
            continue
        for back in (0, 1):
            day = (when - timedelta(days=back)).replace(hour=0, minute=0, second=0, microsecond=0)
            if day.weekday() not in w["days"]:
                continue
            start = day + timedelta(minutes=start_min)
            end = day + timedelta(minutes=end_min) + (timedelta(days=1) if end_min <= start_min else timedelta())
            if start <= when < end:
                return w, end
    return None


def restricted(server: Optional[dict], when: Optional[datetime] = None) -> Optional[datetime]:
    """When the server is inside one of its restricted windows: the time the window ends. Otherwise None."""
    if not server:
        return None
    rh = server.get("restricted_hours") or {}
    if not rh.get("enabled") or not rh.get("windows"):
        return None
    hit = _window_hit(rh["windows"], when or datetime.now())
    return hit[1] if hit else None


def restricted_now(server_id: Optional[str]) -> Optional[datetime]:
    """The same, by server id."""
    return restricted(find(server_id))


def restriction_status() -> list[dict]:
    """Servers currently in restricted hours (for the page header)."""
    out = []
    for sv in list_servers():
        end = restricted(sv)
        if end:
            out.append({"id": sv["id"], "name": sv["name"], "until": end.strftime("%H:%M"), "until_ts": end.timestamp()})
    return out


# ----------------------------------------------------------------------------- models & seats
def model_entry(server: dict, model_ref: Optional[str]) -> Optional[dict]:
    """A model in a server's catalogue, by id or key."""
    if not model_ref:
        return None
    for m in server.get("models") or []:
        if m["id"] == model_ref or m["key"] == model_ref:
            return m
    return None


def profile(model: Optional[dict], profile_id: Optional[str]) -> Optional[dict]:
    """A load profile of a model, by id."""
    if not model or not model.get("profiles"):
        return None
    return next((p for p in model["profiles"] if p["id"] == profile_id), None) or \
        next((p for p in model["profiles"] if p["id"] == model.get("default_profile")), model["profiles"][0])


def display_name(server: Optional[dict], model: Optional[dict], key: str = "") -> str:
    """How to show a model: its label, its key, or the server's name as a fallback."""
    m = (model or {}).get("label") or (model or {}).get("key") or key
    return f"{m} @ {server['name']}" if server else m


def seat_ref(server_id: str, model_ref: str, profile_id: Optional[str] = None, **extra) -> dict:
    """The small, saveable description of an LLM seat: which server, model and load profile, plus seat overrides."""
    if find(server_id) is None:
        from .pool import seats as pool_seats
        pooled = pool_seats.lookup(server_id)
        if pooled is not None:
            # a machine from the Servers page: the model is named by its key, and there are no load profiles
            d = {"server_id": pooled["id"], "model_id": model_ref, "model": model_ref, "profile_id": None,
                 "server": pooled["name"], "provider": "worker"}
            d.update({k: v for k, v in extra.items() if v is not None})
            return d
    sv = get(server_id)
    m = model_entry(sv, model_ref)
    if m is None:
        raise ServerError(f"{sv['name']} has no model '{model_ref}'.")
    d = {"server_id": sv["id"], "model_id": m["id"], "model": m["key"], "profile_id": (profile(m, profile_id) or {}).get("id"),
         "server": sv["name"], "provider": sv["connection"]["provider"]}
    d.update({k: v for k, v in extra.items() if v is not None})
    return d


SEAT_OVERRIDES = ("persona", "max_tool_calls_per_turn", "max_turn_seconds", "reasoning_effort", "effort", "tool_mode",
                  "max_tokens", "temperature", "max_steps_per_turn", "stall_steps", "reconnect_seconds", "on_disconnect")


def resolve_llm(llm: dict, with_key: bool = True) -> dict:
    """Turn a seat's llm block into the agent config: provider, endpoint, key, model and inference settings. Seat
    blocks without a server_id (tests, scripts) are passed through unchanged."""
    if not llm or not llm.get("server_id"):
        return dict(llm or {})
    if find(llm["server_id"]) is None:
        # not in the registry: a machine from the Servers page, played through its helper
        from .pool import seats as pool_seats
        pooled = pool_seats.lookup(llm["server_id"])
        if pooled is not None:
            return pool_seats.resolve(llm, pooled)
    sv = get(llm["server_id"])
    m = model_entry(sv, llm.get("model_id") or llm.get("model"))
    if m is None:
        # a model that isn't in the catalog (yet): usable with catalog defaults
        m = default_model(llm.get("model") or "", sv["connection"]["provider"])
        m["id"] = None
    conn = sv["connection"]
    provider = conn["provider"]
    cfg = {"server_id": sv["id"], "server": sv["name"], "model_id": m["id"], "model": m["key"],
           "provider": {"lmstudio": "openai_compatible", "ollama": "openai_compatible"}.get(provider, provider),
           "base_url": conn.get("base_url") or None, "timeout": conn.get("timeout")}
    inf = m.get("inference") or {}
    for k in ("reasoning_effort", "effort", "max_tokens", "temperature", "max_tool_calls_per_turn"):
        if inf.get(k) not in (None, ""):
            cfg[k] = inf[k]
    tool_mode = inf.get("tool_mode") or "auto"
    for k in SEAT_OVERRIDES:
        if llm.get(k) not in (None, ""):
            cfg[k] = llm[k]
    if cfg.get("tool_mode"):
        tool_mode = cfg["tool_mode"]
    if cfg["provider"] == "openai_compatible":
        if tool_mode == "auto":
            if provider == "lmstudio":
                from .server import lmstudio
                tool_mode = lmstudio.tool_mode_for(conn.get("base_url") or "", m["key"], "auto")
            else:
                tool_mode = "native"
        cfg["tool_mode"] = tool_mode
    else:
        cfg.pop("tool_mode", None)
        cfg.pop("reasoning_effort", None)
    if cfg["provider"] != "anthropic":
        cfg.pop("effort", None)
    if provider == "dryrun":
        cfg["dry_run_delay"] = conn.get("dry_run_delay", 1.0) if llm.get("dry_run_delay") is None else llm["dry_run_delay"]
    prof = profile(m, llm.get("profile_id"))
    if prof:
        cfg["profile_id"] = prof["id"]
        cfg["load"] = dict(prof)
    if with_key and conn["key"]["backend"] != "none":
        from . import keystore
        key = keystore.get(sv)
        if key:
            cfg["api_key"] = key
        elif conn["key"]["backend"] == "env" and conn["key"].get("env"):
            cfg["api_key_env"] = conn["key"]["env"]
    return {k: v for k, v in cfg.items() if v is not None}


def describe_seat(llm: dict) -> dict:
    """Display info for a seat's llm block (server name, model label, profile) without resolving keys."""
    sv = find(llm.get("server_id"))
    if sv is None and llm.get("server_id"):
        from .pool import seats as pool_seats
        pooled = pool_seats.lookup(llm["server_id"])
        if pooled is not None:
            return pool_seats.describe(llm, pooled)
    m = model_entry(sv, llm.get("model_id") or llm.get("model")) if sv else None
    return {"server_id": llm.get("server_id"), "server": sv["name"] if sv else llm.get("server"),
            "model": (m or {}).get("key") or llm.get("model"), "label": (m or {}).get("label") or (m or {}).get("key") or llm.get("model"),
            "profile": (profile(m, llm.get("profile_id")) or {}).get("name") if m else None, "missing_server": bool(llm.get("server_id")) and sv is None,
            "restricted_until": (lambda e: e.strftime("%H:%M") if e else None)(restricted(sv))}


# ----------------------------------------------------------------------------- LM Studio
_cli_cache: dict = {}
CLI_TTL = 5.0          # seconds: the lms CLI takes a second or more per call on a busy machine


def _cached(key: str, fresh: bool, fn):
    """Return a cached LM Studio answer, refreshing it when asked or when it has expired."""
    hit = _cli_cache.get(key)
    if not fresh and hit and time.time() - hit[0] < CLI_TTL:
        return hit[1]
    val = fn()
    _cli_cache[key] = (time.time(), val)
    return val


def forget_lmstudio_state():
    """Drop the cached LM Studio state, after something that would change it."""
    _cli_cache.clear()


def lmstudio_devices(fresh: bool = False) -> dict:
    """LM Link: {"self": {id, name}, "peers": [{id, name, status, loaded}]} (empty without the lms CLI)."""
    return _cached("devices", fresh, _lmstudio_devices)


def _lmstudio_devices() -> dict:
    """Ask LM Studio which devices it can see."""
    from .server import lmstudio
    lms = lmstudio.lms_path()
    if not lms:
        return {}
    import subprocess
    try:
        r = subprocess.run([lms, "link", "status", "--json"], capture_output=True, stdin=subprocess.DEVNULL, timeout=30)
        d = json.loads(r.stdout.decode("utf-8-sig"))
    except (OSError, ValueError, subprocess.SubprocessError):
        return {}
    return {"status": d.get("status"), "self": {"id": d.get("deviceIdentifier"), "name": d.get("deviceName")},
            "peers": [{"id": p.get("deviceIdentifier"), "name": p.get("deviceName"), "status": p.get("status"),
                       "loaded": p.get("loadedModels") or []} for p in d.get("peers") or []]}


def _lms_catalog(fresh: bool = False) -> list[dict]:
    """The LM Studio model catalogue, cached."""
    return _cached("catalog", fresh, _lms_catalog_now)


def _lms_catalog_now() -> list[dict]:
    """Ask LM Studio for its model catalogue."""
    from .server import lmstudio
    lms = lmstudio.lms_path()
    if not lms:
        return []
    import subprocess
    try:
        r = subprocess.run([lms, "ls", "--json"], capture_output=True, stdin=subprocess.DEVNULL, timeout=60)
        return [m for m in json.loads(r.stdout.decode("utf-8-sig")) if isinstance(m, dict)]
    except (OSError, ValueError, subprocess.SubprocessError):
        return []


def _device_matches(server: dict, device_id: Optional[str], devices: dict) -> bool:
    """Whether a catalogue entry belongs to the device a server is configured for.

    One LM Studio can serve several machines through LM Link, so a server entry names a device and this
    is what keeps the desktop's models out of the laptop's catalogue.
    """
    want = (server["connection"].get("lmstudio_device") or "").strip().lower()
    if not want:                                      # this machine
        return not device_id or device_id == (devices.get("self") or {}).get("id")
    if device_id and device_id.lower() == want:
        return True
    peer = next((p for p in devices.get("peers") or [] if p["id"] == device_id), None)
    return bool(peer and (peer.get("name") or "").lower() == want)


def detect_models(server: dict) -> dict:
    """Ask the server which models it offers. Returns {"ok", "models": [{key, info, loaded}], "error"}."""
    import httpx
    conn = server["connection"]
    provider = conn["provider"]
    try:
        if provider == "dryrun":
            return {"ok": True, "models": [{"key": "dry-run", "info": {"note": "pretend model"}, "loaded": True}]}
        if provider == "none":
            return {"ok": True, "models": []}
        if provider == "anthropic":
            from . import keystore
            key = keystore.get(server)
            if not key:
                return {"ok": False, "error": "No API key stored for this server yet."}
            import anthropic
            client = anthropic.Anthropic(api_key=key, base_url=conn.get("base_url") or None)
            out = []
            for m in client.models.list(limit=100):
                info = {"display_name": getattr(m, "display_name", None), "max_context": getattr(m, "max_input_tokens", None),
                        "max_output": getattr(m, "max_tokens", None)}
                out.append({"key": m.id, "info": {k: v for k, v in info.items() if v}, "loaded": True,
                            "price": ANTHROPIC_PRICES.get(m.id)})
            return {"ok": True, "models": out}
        base = (conn.get("base_url") or "").rstrip("/")
        if not base:
            return {"ok": False, "error": "Base URL is required."}
        if provider == "lmstudio":
            from .server import lmstudio
            devices = lmstudio_devices()
            catalog = _lms_catalog()
            native = lmstudio.models(base)
            out = []
            if catalog:
                peer = next((p for p in devices.get("peers") or [] if _device_matches(server, p["id"], devices)), None)
                for m in catalog:
                    if m.get("type") == "embedding" or not _device_matches(server, m.get("deviceIdentifier"), devices):
                        continue
                    key = m.get("modelKey")
                    nat = native.get(key, {})
                    loaded = (key in (peer or {}).get("loaded", [])) if conn.get("lmstudio_device") else nat.get("state") == "loaded"
                    out.append({"key": key, "loaded": loaded, "info": {
                        "display_name": m.get("displayName"), "params": m.get("paramsString"), "arch": m.get("architecture"),
                        "quant": (m.get("quantization") or {}).get("name"), "size_gb": round((m.get("sizeBytes") or 0) / 1024 ** 3, 1),
                        "max_context": m.get("maxContextLength"), "tool_use": bool(m.get("trainedForToolUse")),
                        "vision": bool(m.get("vision")), "device": (peer or {}).get("name") or "this PC",
                        "loaded_context": nat.get("loaded_context_length")}})
                return {"ok": True, "models": out, "devices": devices}
            if not native:
                return {"ok": False, "error": f"LM Studio isn't answering at {base}."}
            for key, nat in native.items():
                if nat.get("type") == "embeddings":
                    continue
                out.append({"key": key, "loaded": nat.get("state") == "loaded", "info": {
                    "arch": nat.get("arch"), "quant": nat.get("quantization"), "max_context": nat.get("max_context_length"),
                    "tool_use": "tool_use" in (nat.get("capabilities") or []), "loaded_context": nat.get("loaded_context_length")}})
            return {"ok": True, "models": out}
        from . import keystore
        key = keystore.get(server)
        headers = {"Authorization": f"Bearer {key}"} if key else {}
        if provider == "ollama":
            root = base[:-3] if base.endswith("/v1") else base
            r = httpx.get(f"{root}/api/tags", timeout=8)
            r.raise_for_status()
            ps = {}
            try:
                ps = {m["name"]: m for m in httpx.get(f"{root}/api/ps", timeout=5).json().get("models", [])}
            except Exception:
                pass
            return {"ok": True, "models": [{"key": m["name"], "loaded": m["name"] in ps, "info": {
                "size_gb": round((m.get("size") or 0) / 1024 ** 3, 1), "params": (m.get("details") or {}).get("parameter_size"),
                "quant": (m.get("details") or {}).get("quantization_level"), "arch": (m.get("details") or {}).get("family")}}
                for m in r.json().get("models", [])]}
        r = httpx.get(f"{base}/models", headers=headers, timeout=8)
        r.raise_for_status()
        return {"ok": True, "models": [{"key": m["id"], "loaded": True, "info": {}} for m in r.json().get("data", [])
                                       if "embed" not in m["id"].lower()]}
    except Exception as e:
        return {"ok": False, "error": f"{type(e).__name__}: {e}"}


def load_args(model_key: str, prof: Optional[dict]) -> list[str]:
    """`lms load` arguments for a load profile."""
    args = ["load", model_key, "-y"]
    if not prof:
        return args
    if prof.get("context"):
        args += ["-c", str(int(prof["context"]))]
    if prof.get("gpu") and prof["gpu"] != "auto":
        args += ["--gpu", str(prof["gpu"])]
    if prof.get("parallel"):
        args += ["--parallel", str(int(prof["parallel"]))]
    if prof.get("ttl"):
        args += ["--ttl", str(int(prof["ttl"]))]
    extra = (prof.get("extra") or "").split()
    allowed = {"--speculative-draft-mtp", "--no-speculative-draft-mtp", "--speculative-draft-simple",
               "--speculative-draft-model", "--speculative-draft-max-tokens", "--speculative-draft-min-tokens",
               "--speculative-draft-min-continue-probability", "--identifier"}
    i = 0
    while i < len(extra):          # only known flags: this string comes from a web form
        if extra[i] in allowed:
            args.append(extra[i])
            if i + 1 < len(extra) and not extra[i + 1].startswith("--"):
                args.append(extra[i + 1])
                i += 1
        i += 1
    return args


def estimate(server: dict, model_key: str, prof: Optional[dict]) -> dict:
    """`lms load --estimate-only`: the memory a profile would need (LM Studio servers)."""
    from .server import lmstudio
    lms = lmstudio.lms_path()
    if not lms or server["connection"]["provider"] != "lmstudio":
        return {"ok": False, "error": "Estimates need LM Studio's lms CLI."}
    import subprocess
    args = [lms] + load_args(model_key, prof) + ["--estimate-only"]
    try:
        r = subprocess.run(args, capture_output=True, stdin=subprocess.DEVNULL, timeout=120)
    except (OSError, subprocess.SubprocessError) as e:
        return {"ok": False, "error": str(e)}
    text = (r.stdout + r.stderr).decode("utf-8", errors="replace").strip()
    text = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", text)      # the CLI colours its output
    return {"ok": r.returncode == 0, "text": text[-2000:]}


def loaded_state(server: dict, model_key: str, _ctx: Optional[dict] = None) -> dict:
    """Is the model loaded on this server, and with what context? (`_ctx` caches the LM Studio lookups when several
    models of one server are checked in a row.)"""
    from .server import lmstudio
    conn = server["connection"]
    ctx = _ctx if _ctx is not None else {}
    if "devices" not in ctx:
        ctx["devices"] = lmstudio_devices()
    if "native" not in ctx:
        ctx["native"] = lmstudio.models(conn["base_url"])
    devices = ctx["devices"]
    if conn.get("lmstudio_device"):
        peer = next((p for p in devices.get("peers") or [] if _device_matches(server, p["id"], devices)), None)
        loaded = model_key in (peer or {}).get("loaded", [])
        info = ctx["native"].get(model_key, {})
        return {"loaded": loaded, "context": info.get("loaded_context_length") if loaded else None,
                "others": [k for k in (peer or {}).get("loaded", []) if k != model_key]}
    info = ctx["native"]
    me = info.get(model_key, {})
    # LM Studio also lists models of LM Link devices; "others" must only be this machine's, or loading a model here
    # "exclusively" would unload another PC's model
    if devices.get("peers") and "catalog" not in ctx:
        ctx["catalog"] = _lms_catalog()
    remote = {m.get("modelKey") for m in ctx.get("catalog") or []
              if not _device_matches(server, m.get("deviceIdentifier"), devices)}
    return {"loaded": me.get("state") == "loaded", "context": me.get("loaded_context_length"),
            "others": [k for k, v in info.items() if v.get("state") == "loaded" and k != model_key and k not in remote]}


@contextlib.contextmanager
def _extra_load_fields(fields: Optional[list]):
    """Send raw LM Studio load settings the SDK has no typed field for (e.g. llm.load.numCpuExpertLayersRatio).
    Keys come from LM Studio's own configuration schema; unknown ones are ignored by the app."""
    if not fields:
        yield
        return
    import lmstudio.json_api as J
    from lmstudio._sdk_models import KvConfigField
    original = J.load_config_to_kv_config_stack

    def patched(config, config_type):
        """Intercept the load configuration so CITAR's extra fields survive the SDK's own handling."""
        stack = original(config, config_type)
        layer = stack.layers[-1]
        layer.config.fields = list(layer.config.fields) + [
            KvConfigField._from_api_dict({"key": f["key"], "value": f.get("value")}) for f in fields]
        return stack
    J.load_config_to_kv_config_stack = patched
    try:
        yield
    finally:
        J.load_config_to_kv_config_stack = original


def sdk():
    """LM Studio's Python SDK client, or None. It can load models with the full load configuration (GPU offload, KV
    cache, experts...), which the lms CLI cannot."""
    try:
        import lmstudio
        return lmstudio.get_default_client()
    except Exception:
        return None


def loaded_config(server: dict, model_key: str) -> Optional[dict]:
    """The load configuration a model is running with right now (LM Studio), as a plain dict."""
    client = sdk()
    if client is None or server["connection"]["provider"] != "lmstudio":
        return None
    try:
        for m in client.llm.list_loaded():
            if m.identifier == model_key or str(m.identifier).endswith(model_key):
                cfg = m.get_load_config()
                return cfg.to_dict() if hasattr(cfg, "to_dict") else dict(cfg)
    except Exception:
        return None
    return None


def ensure_model(server: dict, model_key: str, prof: Optional[dict]) -> float:
    """Load a model on a server that CITAR manages (LM Studio, this PC or an LM Link device) with its load profile.
    Returns the seconds spent loading (0 when it was already loaded suitably or the server isn't managed)."""
    conn = server["connection"]
    if conn["provider"] != "lmstudio" or not conn.get("manage_loading"):
        return 0.0
    from .server import lmstudio
    lms = lmstudio.lms_path()
    client = sdk()
    if (not lms and client is None) or not lmstudio.is_local(conn["base_url"]):
        return 0.0
    st = loaded_state(server, model_key, {"devices": lmstudio_devices(fresh=True), "native": lmstudio.models(conn["base_url"])})
    if (prof or {}).get("loaded_by") == "manual":
        # this model is tuned in LM Studio by hand (a preset CITAR cannot reproduce): use it, never touch it
        if not st["loaded"]:
            raise RuntimeError(f"{model_key} is set to 'I load it in LM Studio myself', but it isn't loaded on "
                               f"{server['name']} right now. Load it there (with your preset) and start again.")
        return 0.0
    want_ctx = int((prof or {}).get("context") or 0)
    exclusive = (prof or {}).get("exclusive", True)
    if st["loaded"] and (not want_ctx or (st["context"] or want_ctx) >= want_ctx) and not (exclusive and st["others"]):
        return 0.0
    started = time.time()
    import subprocess

    def run(args, timeout):
        """Run an ``lms`` command with no stdin and a timeout."""
        return subprocess.run([lms] + args, capture_output=True, stdin=subprocess.DEVNULL, timeout=timeout)
    to_unload = (st["others"] if exclusive else []) + ([model_key] if st["loaded"] else [])
    if client is not None:
        try:
            for other in to_unload:
                try:
                    client.llm.unload(other)
                except Exception:
                    pass
            cfg = sdk_config(prof)
            with _extra_load_fields((prof or {}).get("fields")):
                client.llm.load_new_instance(model_key, config=cfg or None, ttl=(prof or {}).get("ttl"))
            forget_lmstudio_state()
            return round(time.time() - started, 1)
        except Exception as e:
            if not lms:
                raise RuntimeError(f"Could not load {model_key}: {e}")
            # fall back to the CLI (it takes fewer settings, but it is the same LM Studio)
    for other in to_unload:
        run(["unload", other], 300)
    r = run(load_args(model_key, prof), 3600)
    forget_lmstudio_state()
    if r.returncode != 0:
        raise RuntimeError(f"lms load {model_key} failed: {(r.stderr or r.stdout).decode('utf-8', 'replace').strip()[-400:]}")
    return round(time.time() - started, 1)


def unload_models(server: dict, keys: Optional[list] = None):
    """Unload models from a managed LM Studio server (all of its catalog models when keys is None)."""
    conn = server["connection"]
    if conn["provider"] != "lmstudio":
        return
    from .server import lmstudio
    lms = lmstudio.lms_path()
    if not lms or not lmstudio.is_local(conn["base_url"]):
        return
    import subprocess
    st_keys = keys
    if st_keys is None:
        if conn.get("lmstudio_device"):
            devices = lmstudio_devices()
            peer = next((p for p in devices.get("peers") or [] if _device_matches(server, p["id"], devices)), None)
            st_keys = list((peer or {}).get("loaded", []))
        else:
            st_keys = loaded_state(server, "")["others"]
    for k in st_keys:
        subprocess.run([lms, "unload", k], capture_output=True, stdin=subprocess.DEVNULL, timeout=300)
    forget_lmstudio_state()


# ----------------------------------------------------------------------------- hardware
def apply_hardware(server: dict, hw: dict, source: str = "collected", set_power: bool = True) -> dict:
    """Store collected hardware on a server config; fills power figures that are still unset from the guess."""
    if not isinstance(hw, dict) or hw.get("format") != "citar-hardware":
        raise ServerError("That isn't a CITAR hardware file (run collect_hardware.py or collect_hardware.ps1).")
    sv = copy.deepcopy(server)
    hw = dict(hw)
    hw["source"] = source
    sv["hardware"] = hw
    sug = (hw.get("power") or {}).get("suggested") or {}
    if set_power:
        for k in ("idle_w", "cpu_max_w", "gpu_max_w"):
            if sv["power"].get(k) is None and sug.get(k) is not None:
                sv["power"][k] = sug[k]
                sv["power"]["source"] = "guess"
    return sv


def hardware_summary(hw: dict) -> str:
    """A one-line description of a machine, for lists and reports."""
    if not hw or hw.get("source") in (None, "none"):
        return "No hardware details yet"
    parts = []
    cpu = hw.get("cpu") or {}
    if cpu.get("model"):
        parts.append(f"{cpu['model']} ({cpu.get('cores') or '?'}C/{cpu.get('threads') or '?'}T)")
    for g in hw.get("gpus") or []:
        if g.get("vendor") in ("Intel",) and len(hw.get("gpus")) > 1:
            continue
        parts.append(f"{g.get('name')}" + (f" {g['vram_gb']:g} GB" if g.get("vram_gb") and not g.get("unified_memory") else ""))
    mem = hw.get("memory") or {}
    if mem.get("ram_gb"):
        parts.append(f"{mem['ram_gb']:g} GB {'unified memory' if mem.get('unified') else 'RAM'}")
    disk = sum(d.get("size_gb") or 0 for d in hw.get("physical_disks") or [])
    if disk:
        parts.append(f"{disk / 1024:.1f} TB disk" if disk > 1000 else f"{disk:.0f} GB disk")
    return " · ".join(parts)


# ----------------------------------------------------------------------------- checks
def issues(server: dict, reg: Optional[dict] = None) -> list[str]:
    """What's missing for good cost numbers (shown on the Servers page and in report data-quality notes)."""
    reg = reg or load()
    out = []
    kind = server["kind"]
    per = server["costs"][-1] if server["costs"] else {}
    if kind == "owned":
        if not server["components"]:
            out.append("No purchase price: add the hardware under Components so depreciation can be counted.")
        elif any(c.get("price") is None for c in server["components"]):
            out.append("A component has no price.")
        if any(not c.get("purchased") for c in server["components"]):
            out.append("A component has no purchase date (depreciation assumes it was bought before any usage).")
    if kind in ("owned", "leased"):
        pw = server["power"]
        if pw.get("idle_w") is None:
            out.append("Power figures are unset: energy can't be estimated.")
        elif pw.get("source") == "guess":
            out.append("Power figures are rough guesses from the hardware class; measure them for better numbers.")
        if kind == "owned" and not per.get("electricity_plan_id"):
            out.append("No electricity plan selected.")
        pl = next((p for p in reg["electricity_plans"] if p["id"] == per.get("electricity_plan_id")), None)
        if pl:
            cur = pl["periods"][-1]
            if cur["type"] == "flat" and cur.get("rate_kwh") is None:
                out.append(f"Electricity plan '{pl['name']}' has no $/kWh rate.")
            if cur.get("fixed_monthly") and cur["fee_allocation"] == "household_kwh" and not cur.get("household_kwh_month"):
                out.append(f"Electricity plan '{pl['name']}': enter the household's monthly kWh to spread the fixed fee.")
    if kind == "api":
        for m in server["models"]:
            if not (m.get("price") or per.get("default_price") or ANTHROPIC_PRICES.get(m["key"])):
                out.append(f"No token prices for {m['key']}.")
        if server["connection"]["key"]["backend"] == "none":
            out.append("No API key configured.")
    if server["connection"]["provider"] == "none" and kind != "test":
        out.append("No connection: this server can't run models until a provider is set.")
    return out


def model_price(server: dict, model_key: str, when: datetime) -> Optional[dict]:
    """$ per million tokens for a model on an API server at a date: model override, period default, built-in list."""
    m = model_entry(server, model_key)
    if m and m.get("price"):
        return m["price"]
    per = period_at(server["costs"], when) or {}
    if per.get("default_price"):
        return per["default_price"]
    if server["connection"]["provider"] == "anthropic":
        return ANTHROPIC_PRICES.get(model_key) or next((v for k, v in ANTHROPIC_PRICES.items() if model_key.startswith(k)), None)
    return None


# ----------------------------------------------------------------------------- first run
def bootstrap() -> dict:
    """The registry created on first run: this machine (the CITAR host), any LM Link peers of its LM Studio, the
    Anthropic API and a dry-run server. Hardware of this machine is collected; the rest is left for the user."""
    reg = empty_registry()
    home = default_plan(id="ep_home", name="Home electricity")
    reg["electricity_plans"].append(home)
    try:
        from . import hwinfo
        hw = hwinfo.collect()
    except Exception:
        hw = None
    name = (hw or {}).get("hostname") or "This computer"
    local = default_server("owned", "lmstudio", id="sv_host", name=f"{name} (this PC)")
    local["costs"][0]["electricity_plan_id"] = home["id"]
    if hw:
        local = apply_hardware(local, hw)
        local["components"] = [{"id": new_id("c_"), "name": " ".join(x for x in ((hw.get("system") or {}).get("manufacturer"),
                                (hw.get("system") or {}).get("model")) if x) or "Computer",
                                "price": None, "purchased": "", "lifespan_years": 4, "resale": 0.0, "retired": None}]
    reg["servers"].append(local)
    reg["host_server_id"] = local["id"]
    devices = {}
    try:
        devices = lmstudio_devices()
    except Exception:
        pass
    catalog = _lms_catalog() if devices else []
    for peer in devices.get("peers") or []:
        sv = default_server("owned", "lmstudio", id=new_id("sv_"), name=peer.get("name") or "LM Link device")
        sv["connection"]["lmstudio_device"] = peer.get("name") or peer.get("id")
        sv["costs"][0]["electricity_plan_id"] = home["id"]
        sv["description"] = "Reached through this PC's LM Studio over LM Link."
        sv["components"] = [{"id": new_id("c_"), "name": peer.get("name") or "Computer", "price": None, "purchased": "",
                             "lifespan_years": 4, "resale": 0.0, "retired": None}]
        for m in catalog:
            if m.get("deviceIdentifier") == peer.get("id") and m.get("type") != "embedding":
                sv["models"].append(default_model(m["modelKey"], "lmstudio", info={
                    "params": m.get("paramsString"), "arch": m.get("architecture"),
                    "size_gb": round((m.get("sizeBytes") or 0) / 1024 ** 3, 1), "max_context": m.get("maxContextLength"),
                    "tool_use": bool(m.get("trainedForToolUse")), "device": peer.get("name")}))
        reg["servers"].append(sv)
    self_id = (devices.get("self") or {}).get("id")
    for m in catalog:
        if m.get("type") != "embedding" and (not m.get("deviceIdentifier") or m.get("deviceIdentifier") == self_id):
            local["models"].append(default_model(m["modelKey"], "lmstudio", info={
                "params": m.get("paramsString"), "arch": m.get("architecture"),
                "size_gb": round((m.get("sizeBytes") or 0) / 1024 ** 3, 1), "max_context": m.get("maxContextLength"),
                "tool_use": bool(m.get("trainedForToolUse")), "device": "this PC"}))
    api = default_server("api", "anthropic", id="sv_anthropic", name="Anthropic API")
    api["connection"]["base_url"] = ""
    api["connection"]["key"] = {"backend": "keyring" if _keyring_ok() else "env", "env": "ANTHROPIC_API_KEY"}
    api["connection"]["max_parallel"] = 4
    for key in ("claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5", "claude-fable-5-1"):
        api["models"].append(default_model(key, "anthropic", inference={"tool_mode": "native", "reasoning_effort": "",
                                                                         "effort": "medium", "max_tokens": None,
                                                                         "temperature": None, "max_tool_calls_per_turn": 150}))
    reg["servers"].append(api)
    dry = default_server("test", "dryrun", id="sv_dryrun", name="Dry run (no model)")
    dry["connection"]["base_url"] = ""
    dry["connection"]["max_parallel"] = 4
    dry["models"].append(default_model("dry-run", "dryrun"))
    reg["servers"].append(dry)
    return {**reg, "servers": [normalize_server(s) for s in reg["servers"]]}


def _keyring_ok() -> bool:
    """Whether an OS credential store is actually available here.

    Checked rather than assumed: a headless Linux box usually has no Secret Service, and discovering
    that when somebody pastes an API key is too late.
    """
    try:
        from . import keystore
        return keystore.backends()["keyring"]["available"]
    except Exception:
        return False


def public(server: dict, reg: Optional[dict] = None) -> dict:
    """A server as the web UI sees it: config plus key status, restriction state and open issues."""
    from . import keystore
    reg = reg or load()
    d = copy.deepcopy(server)
    d["is_host"] = reg.get("host_server_id") == server["id"]
    d["key_status"] = keystore.status(server)
    end = restricted(server)
    d["restricted_until"] = end.strftime("%H:%M") if end else None
    d["issues"] = issues(server, reg)
    d["hardware_summary"] = hardware_summary(server.get("hardware") or {})
    return d

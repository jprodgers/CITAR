"""The server registry: every machine or online service that can run a model for CITAR, with everything needed to use
it and to cost it.

Every machine is a row of the ``servers`` table (design/phase3-features.md P3.2.8). A row's ``config`` holds the
``normalize_server()`` dict below, and ``reach`` says who dials it: ``direct``, CITAR itself (the host's LM Studio, an
online API, a machine on the LAN an administrator registered); ``worker``, the CITAR helper running on that machine,
which dialled in (``citar.server.workers``). ``Server.max_concurrent`` is the machine's one number of LLM slots. The
electricity plans and the currency live in ``settings_kv``; the host is the row with ``is_host``. ``config/servers.json``
is read once, by the import (``citar.pool.registry_import``), and renamed. The functions here keep the shapes they had
when the registry was that file: ``load()`` is the registry dict, built from the rows and cached (re-read after this
process commits a change to a machine, and at most every ``CACHE_SECONDS`` otherwise).

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

Quiet hours are read on this computer's clock for a machine CITAR dials, and on its owner's for a helper machine
(``citar.pool.seats.restricted``), as they were when the two lived in different lists.
"""
from __future__ import annotations

import contextlib
import copy
import hashlib
import itertools
import json
import logging
import re
import secrets
import threading
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Optional

from sqlalchemy import event
from sqlalchemy.orm import Session as _OrmSession

from . import ledger as _ledger  # noqa: F401  (a machine whose hardware changes is classified again: its flush hook)
from . import paths
from .db.models import Server as _ServerRow, Setting as _Setting, User as _User

log = logging.getLogger("citar.servers")

CONFIG_DIR = paths.config_path()
#: The registry file of CITAR before 0.1.6: imported once into the servers table, then renamed
#: (``citar.pool.registry_import``). Nothing else reads it.
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

#: ``settings_kv`` keys the registry keeps beside its machines: the electricity plans, and the currency.
PLANS_KEY = "electricity_plans"
CURRENCY_KEY = "registry_currency"
REGISTRY_KEYS = (PLANS_KEY, CURRENCY_KEY)
#: Who dials a machine (``Server.reach``): CITAR, or the helper running on it.
REACHES = ("direct", "worker")
#: How long a read of the machines serves when this process changed none: another process's change (a command-line
#: tool, the server seen from one) shows within this.
CACHE_SECONDS = 5.0

_lock = threading.RLock()
#: Held across a read-modify-write of the registry's settings (the plans), never while reading the cache.
_write_lock = threading.RLock()
#: The registry as last read: {"key": (database epoch, generation), "at": monotonic time, "data": registry dict,
#: "meta": {server id: what the row says beyond its config}}.
_cache: dict = {"key": None, "at": 0.0, "data": None, "meta": {}}
#: Bumped after a commit in this process that changed a machine, the registry's settings or an owner's time zone.
_generation = 0


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


def _obj(v, what: str) -> dict:
    """``v`` as an object (empty when left out), or a ServerError naming ``what``: a body from a browser or a file can
    carry any JSON where an object belongs, and a wrong shape must be the caller's 400, never a crash."""
    if not v:
        return {}
    if not isinstance(v, dict):
        raise ServerError(f"{what} must be an object.")
    return v


def _items(v, what: str, *, text_ok: bool = False) -> list:
    """``v`` as a list of objects (empty when left out; ``text_ok``: an entry may be a string too), or a ServerError
    naming ``what``."""
    if not v:
        return []
    if not isinstance(v, (list, tuple)):
        raise ServerError(f"{what} must be a list.")
    for i, x in enumerate(v):
        if not isinstance(x, dict) and not (text_ok and isinstance(x, str)):
            raise ServerError(f"{what}[{i}] must be an object{' or a name' if text_ok else ''}.")
    return list(v)


def _ref(v) -> Optional[str]:
    """An id a part of a server carries (a model's, a load profile's, a component's): text, or None for none."""
    if isinstance(v, str):
        return v or None
    return str(v) if isinstance(v, int) and not isinstance(v, bool) else None


def normalize_server(d: dict) -> dict:
    """Validate and complete a server entry, filling in every field the rest of CITAR expects.

    The single place a server is checked. Everything downstream - costing, benchmarks, the seat
    resolver - assumes a fully-formed entry, which is only true because nothing else writes one. A part of the wrong
    shape (a list where an object belongs, a number where a list does) is a ServerError, which the routes answer 400.
    """
    if not isinstance(d, dict):
        raise ServerError("A server must be an object.")
    try:
        return _normalize_server(d)
    except (TypeError, AttributeError) as exc:
        # a shape the checks below did not foresee: still the sender's mistake, and still never a 500
        raise ServerError(f"The server's settings do not read: {exc}") from exc


def _normalize_server(d: dict) -> dict:
    """``normalize_server``'s work, on a dict."""
    kind = d.get("kind") if d.get("kind") in KINDS else "owned"
    conn_in = _obj(d.get("connection"), "connection")
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
    key = _obj(conn_in.get("key"), "connection.key")
    backend = key.get("backend") if key.get("backend") in ("none", "keyring", "env", "file") else "none"
    c["key"] = {"backend": backend, "env": str(key.get("env") or "").strip()}
    c["dry_run_delay"] = _num(conn_in.get("dry_run_delay"), 1.0, 0, 60)
    c["timeout"] = _num(conn_in.get("timeout"), None, 5, 7200)
    hw = d.get("hardware") if isinstance(d.get("hardware"), dict) else {}
    sv["hardware"] = hw or {"source": "none"}
    pw_in = _obj(d.get("power"), "power")
    pw = default_power()
    for k in ("idle_w", "cpu_max_w", "gpu_max_w"):
        pw[k] = _num(pw_in.get(k), None, 0, 20000)
    pw["sampling"] = pw_in.get("sampling") if pw_in.get("sampling") in ("auto", "off") else "auto"
    pw["measured_overhead_pct"] = _num(pw_in.get("measured_overhead_pct"), 10, 0, 100)
    pw["source"] = str(pw_in.get("source") or ("manual" if pw["idle_w"] is not None else "unset"))[:20]
    sv["power"] = pw
    sv["components"] = []
    if d.get("components") and not isinstance(d.get("components"), (list, tuple)):
        raise ServerError("components must be a list.")
    for comp in d.get("components") or []:
        if not isinstance(comp, dict):
            continue
        sv["components"].append({"id": _ref(comp.get("id")) or new_id("c_"), "name": str(comp.get("name") or "Component")[:80],
                                 "price": _num(comp.get("price"), None, 0), "purchased": _date(comp.get("purchased"), ""),
                                 "lifespan_years": _num(comp.get("lifespan_years"), 4, 0.25, 30),
                                 "resale": _num(comp.get("resale"), 0.0, 0),
                                 "retired": _date(comp.get("retired"), "") or None})
    periods = []
    for p in _items(d.get("costs"), "costs") or [default_cost_period(kind)]:
        q = default_cost_period(kind)
        q["from"] = _date(p.get("from"))
        if p.get("electricity_plan_id") and not isinstance(p.get("electricity_plan_id"), str):
            raise ServerError("A cost period's electricity_plan_id is a plan's id.")
        q["electricity_plan_id"] = p.get("electricity_plan_id") or None
        for k in ("fixed_monthly", "hourly_rate", "api_fixed_monthly"):
            q[k] = _num(p.get(k), 0.0, 0)
        q["fixed_note"] = str(p.get("fixed_note") or "")[:200]
        q["note"] = str(p.get("note") or "")[:500]
        q["default_price"] = _price(p.get("default_price"))
        periods.append(q)
    periods.sort(key=lambda p: p["from"])
    sv["costs"] = periods or [default_cost_period(kind)]
    rh_in = _obj(d.get("restricted_hours"), "restricted_hours")
    rh = default_restricted()
    rh["enabled"] = bool(rh_in.get("enabled"))
    rh["unload_models"] = bool(rh_in.get("unload_models", True))
    rh["grace_minutes"] = _num(rh_in.get("grace_minutes"), 15, 0, 240)
    for w in _items(rh_in.get("windows"), "restricted_hours.windows"):
        days_in = w.get("days") if w.get("days") is not None else range(7)
        if not isinstance(days_in, (list, tuple, range)):
            raise ServerError("A quiet-hours window's days are a list of weekdays (0 is Monday).")
        days = sorted({int(x) for x in days_in if str(x).lstrip("-").isdigit() and 0 <= int(x) <= 6})
        rh["windows"].append({"days": days, "start": _hhmm(w.get("start"), "21:00"), "end": _hhmm(w.get("end"), "06:00")})
    sv["restricted_hours"] = rh
    models, seen = [], set()
    for m in _items(d.get("models"), "models", text_ok=True):
        if isinstance(m, str):
            m = {"key": m}
        key = str(m.get("key") or m.get("model") or "").strip()
        if not key or key in seen:
            continue
        seen.add(key)
        mm = default_model(key, provider)
        mm["id"] = _ref(m.get("id")) or mm["id"]
        mm["label"] = str(m.get("label") or "")[:80]
        mm["enabled"] = bool(m.get("enabled", True))
        mm["info"] = m.get("info") if isinstance(m.get("info"), dict) else {}
        inf = _obj(m.get("inference"), f"The inference settings of {key}")
        mm["inference"].update({k: inf[k] for k in mm["inference"] if k in inf})
        if mm["inference"]["tool_mode"] not in ("auto", "native", "json"):
            mm["inference"]["tool_mode"] = "auto"
        mm["inference"]["max_tool_calls_per_turn"] = int(_num(mm["inference"].get("max_tool_calls_per_turn"), 150, 5, 1000))
        mm["inference"]["max_tokens"] = int(_num(mm["inference"].get("max_tokens"), 0, 0, 200000)) or None
        mm["inference"]["temperature"] = _num(mm["inference"].get("temperature"), None, 0, 2)
        profiles = []
        for pr in (_items(m.get("profiles"), f"The load profiles of {key}")
                   or ([default_profile()] if provider == "lmstudio" else [])):
            q = default_profile()
            q["id"] = _ref(pr.get("id")) or new_id("p_")
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
    """Validate and complete an electricity plan. A part of the wrong shape is a ServerError (the routes' 400)."""
    if not isinstance(d, dict):
        raise ServerError("An electricity plan must be an object.")
    try:
        return _normalize_plan(d)
    except (TypeError, AttributeError) as exc:
        raise ServerError(f"The electricity plan does not read: {exc}") from exc


def _normalize_plan(d: dict) -> dict:
    """``normalize_plan``'s work, on a dict."""
    p = default_plan()
    p["id"] = d.get("id") if re.fullmatch(r"[A-Za-z0-9_\-]{3,40}", str(d.get("id") or "")) else p["id"]
    p["name"] = (str(d.get("name") or "").strip() or "Electricity plan")[:80]
    periods = []
    for q in _items(d.get("periods"), "periods"):
        r = {"from": _date(q.get("from")), "type": q.get("type") if q.get("type") in ("flat", "tou", "tiered") else "flat",
             "rate_kwh": _num(q.get("rate_kwh"), None, 0, 100), "fixed_monthly": _num(q.get("fixed_monthly"), 0.0, 0),
             "fee_allocation": q.get("fee_allocation") if q.get("fee_allocation") in ("household_kwh", "share", "none") else "household_kwh",
             "household_kwh_month": _num(q.get("household_kwh_month"), None, 1, 1e7),
             "share_pct": _num(q.get("share_pct"), 0.0, 0, 100), "note": str(q.get("note") or "")[:500], "tou": [], "tiers": []}
        for w in _items(q.get("tou"), "tou"):
            days_in = w.get("days") if w.get("days") is not None else range(7)
            if not isinstance(days_in, (list, tuple, range)):
                raise ServerError("A time-of-use window's days are a list of weekdays (0 is Monday).")
            days = sorted({int(x) for x in days_in if str(x).lstrip("-").isdigit() and 0 <= int(x) <= 6})
            r["tou"].append({"days": days, "start": _hhmm(w.get("start")), "end": _hhmm(w.get("end"), "00:00"),
                             "rate_kwh": _num(w.get("rate_kwh"), 0.0, 0, 100), "name": str(w.get("name") or "")[:30]})
        for t in _items(q.get("tiers"), "tiers"):
            r["tiers"].append({"up_to_kwh": _num(t.get("up_to_kwh"), None, 0), "rate_kwh": _num(t.get("rate_kwh"), 0.0, 0, 100)})
        periods.append(r)
    periods.sort(key=lambda q: q["from"])
    p["periods"] = periods or default_plan()["periods"]
    return p


# ----------------------------------------------------------------------------- storage: the servers table
def _stable_ids(cfg: dict) -> dict:
    """``cfg`` with an id on every model, load profile and component that has none, made from what it is rather than
    drawn at random: a helper's hello stores its models as bare keys, and ``normalize_server`` would give them new ids
    at every read, so a seat or a suite naming one by id would lose it."""
    def digest(prefix: str, *parts) -> str:
        return prefix + hashlib.sha1("/".join(str(p) for p in parts).encode("utf-8")).hexdigest()[:8]
    out = dict(cfg)
    models = []
    for m in cfg.get("models") or []:
        if isinstance(m, str):
            m = {"key": m}
        if not isinstance(m, dict):
            continue
        m = dict(m)
        key = str(m.get("key") or m.get("model") or "")
        m["id"] = m.get("id") or digest("m_", key)
        if isinstance(m.get("profiles"), list):
            m["profiles"] = [dict(p, id=p.get("id") or digest("p_", key, i)) if isinstance(p, dict) else p
                             for i, p in enumerate(m["profiles"])]
        models.append(m)
    out["models"] = models
    if isinstance(cfg.get("components"), list):
        out["components"] = [dict(c, id=c.get("id") or digest("c_", c.get("name"), i)) if isinstance(c, dict) else c
                             for i, c in enumerate(cfg["components"])]
    return out


def stored_config(sv: dict) -> dict:
    """What a row's ``config`` keeps of a normalized server: everything but its LLM slots, which are the row's own
    ``max_concurrent`` (the one number, P3.2.8; ``normalize_server`` still reads ``connection.max_parallel`` as its
    alias, and ``load()`` puts the row's number there)."""
    out = copy.deepcopy(sv)
    (out.get("connection") or {}).pop("max_parallel", None)
    return out


def row_registry(row, cfg: Optional[dict] = None) -> dict:
    """A row as a registry server: its config through ``normalize_server``, under the row's id, name and kind, with
    the row's slots as ``connection.max_parallel``. A helper machine's connection is the helper's to make: it carries
    no address or key here, so nothing that dials what a server dict names (detection, model loading) can reach the
    host's LM Studio for it. ``cfg`` is the row's stored config unless given."""
    cfg = _stable_ids(dict(row.config or {}) if cfg is None else cfg)
    conn = dict(cfg.get("connection") or {})
    conn["max_parallel"] = row.max_concurrent or 1
    sv = normalize_server({**cfg, "id": row.id, "name": row.name, "kind": row.kind, "connection": conn})
    sv["updated"] = cfg.get("updated") or (row.updated_at.timestamp() if row.updated_at else sv["updated"])
    if (row.reach or "worker") == "worker":
        sv["connection"].update(base_url="", manage_loading=False, key={"backend": "none", "env": ""})
    return sv


def _offline_engine():
    """An engine on the server's database for a process that has not opened one (a command-line tool reading the
    machines, ``citar lab`` say), or None: only when that database already exists with this version's tables. It is
    only read: a tool never creates, migrates or switches a database to WAL (that is the server's start's business),
    and the process's own ``db`` stays unopened, so nothing else in it starts writing there."""
    from sqlalchemy import create_engine, inspect
    from . import settings
    url = settings.get().db_url
    with _lock:
        cached = _offline.get(url)
    if cached is not None:
        return cached
    try:
        if url.startswith("sqlite"):
            file = url.split("///", 1)[-1]
            if not file or file == ":memory:" or not Path(file).exists():
                return None
            engine = create_engine(url, connect_args={"check_same_thread": False, "timeout": 10})
        else:
            engine = create_engine(url, pool_pre_ping=True)
        insp = inspect(engine)
        if not insp.has_table("servers") or "is_host" not in {c["name"] for c in insp.get_columns("servers")}:
            engine.dispose()
            return None
    except Exception:
        log.warning("could not open the server's database to read its machines", exc_info=True)
        return None
    with _lock:
        _offline[url] = engine
    return engine


_offline: dict = {}


@contextlib.contextmanager
def _reading():
    """A database session to read the machines in: the process's own, or the offline reader's (``_offline_engine``);
    None where there is no database to read (a tool on a computer with no server)."""
    from . import db
    if db.configured():
        with db.session() as s:
            yield s
        return
    engine = _offline_engine()
    if engine is None:
        yield None
        return
    with _OrmSession(engine) as s:
        yield s


def _read() -> tuple:
    """The registry dict and each row's facts beyond its config, read from the database."""
    from sqlalchemy import select
    reg = empty_registry()
    reg["aliases"] = {}
    meta: dict = {}
    with _reading() as s:
        if s is None:
            return reg, meta
        plans = s.get(_Setting, PLANS_KEY)
        currency = s.get(_Setting, CURRENCY_KEY)
        rows = list(s.scalars(select(_ServerRow).order_by(_ServerRow.created_at, _ServerRow.id)))
        owners = {r.owner_id for r in rows if r.owner_id}
        zones = {u.id: u.tz for u in s.scalars(select(_User).where(_User.id.in_(owners)))} if owners else {}
        for p in _setting_value(plans) or []:
            if isinstance(p, dict):
                reg["electricity_plans"].append(normalize_plan(p))
        cur = _setting_value(currency) or {}
        if isinstance(cur, dict):
            reg["currency"] = str(cur.get("currency") or reg["currency"])
            reg["currency_symbol"] = str(cur.get("currency_symbol") or reg["currency_symbol"])
        for row in rows:
            cfg = dict(row.config or {})
            try:
                sv = row_registry(row, cfg)
            except Exception:
                log.warning("machine %s has a configuration that does not read; it is left out", row.id, exc_info=True)
                continue
            reg["servers"].append(sv)
            if row.is_host and not reg["host_server_id"]:
                reg["host_server_id"] = row.id
            for old in cfg.get("former_ids") or []:
                if isinstance(old, str) and old != row.id:
                    reg["aliases"].setdefault(old, row.id)
            meta[row.id] = {"id": row.id, "name": row.name, "kind": row.kind, "provider": row.provider,
                            "reach": row.reach or "worker", "enabled": bool(row.enabled),
                            "owner_id": row.owner_id, "owner_tz": zones.get(row.owner_id) or "UTC",
                            "group_id": row.group_id, "is_host": bool(row.is_host),
                            "server_resource": bool(row.server_resource), "max_concurrent": row.max_concurrent or 1,
                            "capabilities": dict(row.capabilities or {}), "config": cfg,
                            # the compute ledger's pricing (P3.4.3): the machine's classes and what they are read from
                            "gpu_class_id": row.gpu_class_id, "cpu_class_id": row.cpu_class_id,
                            "class_override": bool(row.class_override), "hardware": sv.get("hardware") or {}}
    known = {sv["id"] for sv in reg["servers"]}
    reg["aliases"] = {old: new for old, new in reg["aliases"].items() if old not in known}
    return reg, meta


def _setting_value(row):
    """A ``settings_kv`` row's value (stored as ``{"v": value}``, as the policy's are), or None."""
    if row is None:
        return None
    return row.value.get("v") if isinstance(row.value, dict) else row.value


def _state() -> tuple:
    """The cached registry and row facts, read again when this process committed a change since, when the database
    is another one, or after ``CACHE_SECONDS``."""
    from . import db
    with _lock:
        key = (db.epoch(), _generation)
        if _cache["data"] is not None and _cache["key"] == key and time.monotonic() - _cache["at"] < CACHE_SECONDS:
            return _cache["data"], _cache["meta"]
    try:
        data, meta = _read()
    except Exception:
        # a database without the tables yet (a test's, before create_all) or one that does not answer: no machine is
        # usable, which every caller already handles; the next read tries again
        log.debug("could not read the machines", exc_info=True)
        return {**empty_registry(), "aliases": {}}, {}
    with _lock:
        _cache.update(key=key, at=time.monotonic(), data=data, meta=meta)
    return data, meta


def invalidate():
    """Read the machines again at the next call (a change this process made outside an ORM session, or a test)."""
    global _generation
    with _lock:
        _generation += 1


def _touches_registry(session) -> bool:
    """Whether a flush wrote a machine, the registry's settings, or an account the machines' quiet hours read."""
    from sqlalchemy import inspect as _inspect
    for obj in itertools.chain(session.new, session.dirty, session.deleted):
        if isinstance(obj, _ServerRow):
            return True
        if isinstance(obj, _Setting) and obj.key in REGISTRY_KEYS:
            return True
        if isinstance(obj, _User) and (obj in session.deleted or _inspect(obj).attrs.tz.history.has_changes()):
            return True
    return False


@event.listens_for(_OrmSession, "after_flush")
def _flushed(session, _context):
    """Remember that this transaction changed what the registry reads."""
    if not session.info.get("citar_registry") and _touches_registry(session):
        session.info["citar_registry"] = True


@event.listens_for(_OrmSession, "after_commit")
def _committed(session):
    """A committed change to the machines: the next read sees it. A savepoint's release fires this too, while the
    change is not committed yet: only the outermost commit counts."""
    if session.in_nested_transaction():
        return
    if session.info.pop("citar_registry", False):
        invalidate()


@event.listens_for(_OrmSession, "after_rollback")
def _rolled_back(session):
    """Nothing changed after all (a savepoint rolled back may leave changes made before it: those still count)."""
    if not session.in_nested_transaction():
        session.info.pop("citar_registry", None)


def load() -> dict:
    """The registry: every machine (``servers``), the electricity plans, the currency and the host's id, as one dict
    in the shape ``config/servers.json`` had. Shared and cached: copy it (``snapshot``) before changing anything."""
    return _state()[0]


def snapshot() -> dict:
    """A deep copy of the registry, safe to modify."""
    return copy.deepcopy(load())


def machine(server_id: Optional[str]) -> Optional[dict]:
    """What a machine's row says beyond its configuration: ``reach``, ``enabled``, ``owner_id``, ``owner_tz``,
    ``group_id``, ``is_host``, ``server_resource``, ``max_concurrent``, ``provider``, ``name``, ``kind`` and the stored
    ``config`` as it is (a copy). None for no such machine."""
    if not server_id:
        return None
    m = _state()[1].get(server_id)
    return {**m, "config": copy.deepcopy(m["config"])} if m else None


def machine_facts(server_id: Optional[str]) -> Optional[dict]:
    """``machine`` without the copy, for a reader that only reads (the compute ledger's writer, which prices thousands
    of events a second from the cache): the cached dict itself, never to be changed. None for no such machine."""
    return _state()[1].get(server_id or "") if server_id else None


def machine_settings(server_id: Optional[str]) -> Optional[dict]:
    """What the prompt scheduler reads of a machine, from the registry's cache (design/phase3-features.md P3.3.10): its
    LLM ``slots`` (``max_concurrent``, the one site number of P3.2.8), whether it takes model work at all (``llm``,
    ``capabilities.llm.enabled``, default on) and whether pooled prompts may make it swap models (``allow_swaps``,
    ``capabilities.llm.allow_swaps``, default off), with the row's ``enabled``, ``reach``, ``provider``, ``kind`` and
    ``name``. None for no such machine. Phase 4's settings frame will carry the same; package 3-18 adds the CPU
    limits."""
    m = _state()[1].get(server_id or "")
    if not m:
        return None
    llm = (m.get("capabilities") or {}).get("llm") or {}
    try:
        slots = max(1, int(m.get("max_concurrent") or 1))
    except (TypeError, ValueError):
        slots = 1
    return {"id": m["id"], "name": m["name"], "kind": m["kind"], "provider": m["provider"], "reach": m["reach"],
            "enabled": m["enabled"], "slots": slots, "llm": bool(llm.get("enabled", True)),
            "allow_swaps": bool(llm.get("allow_swaps", False))}


def is_helper(server_id: Optional[str]) -> bool:
    """Whether a machine is reached through the CITAR helper running on it (``reach = worker``): CITAR never dials
    it, never drives an LM Studio for it and never reads a key for it."""
    m = _state()[1].get(server_id or "")
    return bool(m) and m["reach"] == "worker"


def with_pooled(reg: Optional[dict] = None) -> dict:
    """The registry for pricing and reports: since every machine is a row it is ``load()`` itself (a copy), whose
    ``aliases`` map the old ids of a machine that was removed and registered again (its ``former_ids``) to its
    current id, so usage recorded under the old one is priced with the machine's current settings."""
    reg = copy.deepcopy(reg or load())
    reg.setdefault("aliases", {})
    return reg


def list_servers() -> list[dict]:
    """Every machine."""
    return load()["servers"]


def get(server_id: str) -> dict:
    """One machine by id, raising if it does not exist."""
    for sv in load()["servers"]:
        if sv["id"] == server_id:
            return sv
    raise ServerError(f"No server '{server_id}'.")


def find(server_id: Optional[str]) -> Optional[dict]:
    """One machine by id, or None."""
    try:
        return get(server_id) if server_id else None
    except ServerError:
        return None


def host() -> Optional[dict]:
    """The machine CITAR itself runs on (the row with ``is_host``).

    It matters for costing: that machine's CPU runs the engine, the bots and the lab, so a share of
    every game's cost is attributed to it rather than to whatever served the model.
    """
    return find(load().get("host_server_id"))


@contextlib.contextmanager
def _writing(session=None):
    """The session a change is made in: the caller's (committed with its own work), or one of its own."""
    if session is not None:
        yield session
        return
    from . import db
    with db.session() as s:
        yield s


def _check_plans(sv: dict) -> None:
    """Refuse cost periods that name an electricity plan the registry does not have."""
    plans = {p["id"] for p in load()["electricity_plans"]}
    for plan_id in {p.get("electricity_plan_id") for p in sv["costs"]} - {None}:
        if plan_id not in plans:
            raise ServerError(f"Unknown electricity plan '{plan_id}'.")


def _set_host(s, server_id: Optional[str]) -> None:
    """Make ``server_id`` the host (None: no host), the one row with ``is_host``."""
    from sqlalchemy import select
    for row in s.scalars(select(_ServerRow).where(_ServerRow.is_host.is_(True))):
        if row.id != server_id:
            row.is_host = False
    if server_id:
        row = s.get(_ServerRow, server_id)
        if row is None:
            raise ServerError("Unknown server.")
        row.is_host = True


#: What the registry's view of a helper machine leaves out of its connection (``row_registry``): a save of that view
#: keeps what the row stores for these rather than blanking it (``apply_row``).
HELPER_HIDDEN = ("base_url", "manage_loading", "key")


def stored_key(config) -> dict:
    """The key block a row's stored ``config`` names (``{}`` for none): where its API key is, whatever the registry's
    view of the machine shows (a helper machine's shows none)."""
    conn = config.get("connection") if isinstance(config, dict) else None
    key = conn.get("key") if isinstance(conn, dict) else None
    return key if isinstance(key, dict) else {}


def forget_key(server_id: str, config) -> None:
    """Remove the key a removed machine's stored ``config`` keeps in the OS credential store or the key file (call it
    once the removal is committed). The ``env`` and ``none`` backends store nothing."""
    from . import keystore
    backend = stored_key(config).get("backend")
    if backend in ("keyring", "file"):
        keystore.delete(server_id, backend)


def apply_row(row, sv: dict) -> None:
    """Write a normalized server into its row: the configuration, and the columns that mirror it (name, kind,
    provider) or replace a part of it (``max_concurrent``). On a helper machine (the row's ``reach``, set first) the
    parts of the connection its registry view hides (``HELPER_HIDDEN``) keep what the row stored: that view, saved back
    by its owner's editor, would otherwise blank the key a switch from ``direct`` left in the keystore, and nothing
    could find it to remove it."""
    cfg = stored_config(sv)
    old = (row.config or {}).get("connection") if isinstance(row.config, dict) else None
    if (row.reach or "worker") == "worker" and isinstance(old, dict):
        for k in HELPER_HIDDEN:
            if k in old:
                cfg["connection"][k] = copy.deepcopy(old[k])
    row.config = cfg
    row.name = (sv.get("name") or "Unnamed server")[:120]
    row.kind = sv.get("kind") or "owned"
    row.provider = (sv.get("connection") or {}).get("provider") or "none"
    row.max_concurrent = max(1, int((sv.get("connection") or {}).get("max_parallel") or 1))


def upsert(data: dict, *, actor=None, reach: Optional[str] = None, session=None) -> dict:
    """Create or replace a machine, validating it and its references first. A new one is ``actor``'s (nobody's when
    None: then administrators manage it), dialled by CITAR unless ``reach`` says otherwise; an existing one keeps its
    owner, group and reach unless ``reach`` is given. ``data["is_host"]`` true makes it the host, false takes that
    away from it. Whether ``actor`` may do this is the caller's to check (``citar.server.admin_api``). Written in
    ``session`` when one is given (the caller commits), else committed here. Returns the normalized server."""
    if reach is not None and reach not in REACHES:
        raise ServerError("reach is 'direct' or 'worker'.")
    sv = normalize_server(data)
    _check_plans(sv)
    with _writing(session) as s:
        row = s.get(_ServerRow, sv["id"])
        if row is None:
            admin = actor is not None and getattr(actor, "role", None) == "admin"
            row = _ServerRow(id=sv["id"], owner_id=getattr(actor, "id", None), reach=reach or "direct",
                             server_resource=(actor is None or admin) and sv["kind"] == "api")
            s.add(row)
        else:
            sv["created"] = (row.config or {}).get("created") or sv["created"]
            if reach is not None:
                row.reach = reach
        apply_row(row, sv)
        s.flush()
        if data.get("is_host"):
            _set_host(s, row.id)
            row.server_resource = True
        elif data.get("is_host") is False and row.is_host:
            row.is_host = False
        s.flush()
    return sv


def delete(server_id: str, *, session=None):
    """Remove a machine (its worker tokens go with it). Dropping its helper's connection is the caller's, once this
    is committed (``citar.server.workers.WorkerHub.drop``)."""
    with _writing(session) as s:
        row = s.get(_ServerRow, server_id)
        if row is not None:
            s.delete(row)
            s.flush()


def _settings_row(s, key: str):
    """A registry ``settings_kv`` row, made when missing."""
    row = s.get(_Setting, key)
    if row is None:
        row = _Setting(key=key, value={"v": None})
        s.add(row)
    return row


def save_plans(plans: list, *, session=None) -> list:
    """Replace the electricity plans (normalized)."""
    plans = [normalize_plan(p) for p in plans]
    with _writing(session) as s:
        _settings_row(s, PLANS_KEY).value = {"v": plans}
        s.flush()
    return plans


def save_currency(currency: str, symbol: str, *, session=None) -> dict:
    """Set the registry's currency and its symbol."""
    value = {"currency": str(currency or "USD")[:8].upper(), "currency_symbol": str(symbol or "$")[:4]}
    with _writing(session) as s:
        _settings_row(s, CURRENCY_KEY).value = {"v": value}
        s.flush()
    return value


def upsert_plan(data: dict) -> dict:
    """Create or replace an electricity plan."""
    with _write_lock:
        plans = copy.deepcopy(load()["electricity_plans"])
        p = normalize_plan(data)
        idx = next((i for i, x in enumerate(plans) if x["id"] == p["id"]), None)
        if idx is None:
            plans.append(p)
        else:
            plans[idx] = p
        save_plans(plans)
        return p


def delete_plan(plan_id: str):
    """Remove an electricity plan that no machine's costs name."""
    with _write_lock:
        reg = load()
        users = [s["name"] for s in reg["servers"] if any(c.get("electricity_plan_id") == plan_id for c in s["costs"])]
        if users:
            raise ServerError(f"The plan is used by {', '.join(users)}.")
        save_plans([p for p in reg["electricity_plans"] if p["id"] != plan_id])


def set_settings(patch: dict) -> dict:
    """Change registry-wide settings: currency, and which server is the host."""
    with _write_lock:
        reg = load()
        if "currency" in patch or "currency_symbol" in patch:
            save_currency(patch.get("currency", reg["currency"]) if "currency" in patch else reg["currency"],
                          patch.get("currency_symbol", reg["currency_symbol"]) if "currency_symbol" in patch
                          else reg["currency_symbol"])
        if "host_server_id" in patch:
            from . import db
            with db.session() as s:
                _set_host(s, patch["host_server_id"] or None)
        return load()


def plan(plan_id: Optional[str]) -> Optional[dict]:
    """An electricity plan by id, or None."""
    return next((p for p in load()["electricity_plans"] if p["id"] == plan_id), None) if plan_id else None


def register_offline(entries: list) -> list:
    """Add machines from a command-line tool (``citar setup --local``), which has no database of its own open.

    Into the server's database when it exists with this version's tables and its registry was imported (then the
    rows are the registry), owned by its first administrator; otherwise into ``config/servers.json`` (made from the
    first run's registry, ``bootstrap()``, when there is none), which the server's first start imports
    (``citar.pool.registry_import``). An entry with ``is_host`` true becomes the host only when there is none yet.
    Returns the entries as saved (normalized, with their ids)."""
    from . import db, settings
    from .auth import policy
    saved = [normalize_server(e) for e in entries]
    hosts = [sv["id"] for sv, e in zip(saved, entries) if e.get("is_host")]
    engine = _offline_engine() if not db.configured() else db.engine()
    imported = False
    if engine is not None:
        with _OrmSession(engine) as s:
            imported = bool(policy.marker(s, "registry_imported"))
    if imported:
        from . import items
        if not db.configured():
            db.configure(settings.get().db_url)
        with db.session() as s:
            owner = items.first_admin(s)
            has_host = s.query(_ServerRow).filter(_ServerRow.is_host.is_(True)).count() > 0
            for sv, entry in zip(saved, entries):
                upsert({**sv, "is_host": bool(hosts and sv["id"] == hosts[0] and not has_host)}, actor=owner,
                       session=s)
        return saved
    from .fsutil import write_text
    reg = json.loads(PATH.read_text(encoding="utf-8")) if PATH.exists() else bootstrap()
    reg.setdefault("servers", [])
    for sv in saved:
        reg["servers"] = [x for x in reg["servers"] if x.get("id") != sv["id"]] + [sv]
    if hosts and not reg.get("host_server_id"):
        reg["host_server_id"] = hosts[0]
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    write_text(PATH, json.dumps(reg, indent=1))
    return saved


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


def _helper_view(server_id: Optional[str]) -> Optional[dict]:
    """A helper machine as ``citar.pool.seats`` reads it (``lookup``), or None for any other id."""
    from .pool import seats as pool_seats
    pooled = pool_seats.lookup(server_id)
    return pooled if pooled is not None and pooled.get("reach") == "worker" else None


def restricted_at(server_id: Optional[str], when: Optional[datetime] = None) -> Optional[datetime]:
    """The same, by server id: on this computer's clock for a machine CITAR dials, on its owner's for a helper
    machine (see citar.pool.seats.restricted)."""
    pooled = _helper_view(server_id)
    if pooled is not None:
        from .pool import seats as pool_seats
        return pool_seats.restricted(pooled, when)
    return restricted(find(server_id), when)


def restricted_now(server_id: Optional[str]) -> Optional[datetime]:
    """The same, now."""
    return restricted_at(server_id)


def server_name(server_id: Optional[str]) -> str:
    """A machine's name (or its id)."""
    sv = find(server_id)
    return sv["name"] if sv else (server_id or "server")


def restricted_text(server_id: Optional[str], end: Optional[datetime]) -> str:
    """A restriction's end as the machine's owner reads it: this computer's clock for a machine CITAR dials, the
    owner's zone for a helper machine."""
    if end is None:
        return ""
    pooled = _helper_view(server_id)
    if pooled is not None:
        from .pool import seats as pool_seats
        return pool_seats.clock_text(pooled, end)
    return end.strftime("%H:%M")


def restriction_config(server_id: Optional[str]) -> dict:
    """A machine's restricted-hours settings (a helper machine's as its owner saved them)."""
    pooled = _helper_view(server_id)
    if pooled is not None:
        return (pooled.get("config") or {}).get("restricted_hours") or {}
    return (find(server_id) or {}).get("restricted_hours") or {}


def restriction_status() -> list[dict]:
    """Machines in their restricted hours right now (for the page header): those CITAR dials, then helper machines."""
    from .pool import seats as pool_seats
    out, helpers = [], []
    for sv in list_servers():
        if is_helper(sv["id"]):
            helpers.append(sv["id"])
            continue
        end = restricted(sv)
        if end:
            out.append({"id": sv["id"], "name": sv["name"], "until": end.strftime("%H:%M"), "until_ts": end.timestamp()})
    for sid in helpers:
        pooled = _helper_view(sid)
        if not (((pooled or {}).get("config") or {}).get("restricted_hours") or {}).get("enabled"):
            continue
        end = pool_seats.restricted(pooled)
        if end:
            out.append({"id": sid, "name": pooled["name"], "until": pool_seats.clock_text(pooled, end),
                        "until_ts": end.timestamp()})
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
    pooled = _helper_view(server_id)
    if pooled is not None:
        # a helper machine: the model is named by its key, and there are no load profiles
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


#: What a member's seat block keeps (design/phase3-features.md P3.2.9): which machine, which model and load
#: profile, and the seat's own overrides. Everything else (a provider, a base URL, a key, a key variable, a
#: timeout, unknown keys) decides what the server dials or sends, and only an administrator may set it.
MEMBER_LLM_KEYS = ("server_id", "model", "model_id", "profile_id") + SEAT_OVERRIDES

#: What an inline dry-run block, or a seat on a dry-run machine, may keep besides: the dry-run provider dials nothing
#: and reads no key. ``dry_run_negotiation`` is how it answers chats; ``dry_run_propose_every`` makes it open a trade
#: with a civilization it has met every that many turns (``citar.agents.providers.dryrun``).
DRYRUN_LLM_KEYS = ("dry_run_delay", "dry_run_negotiation", "dry_run_propose_every")


class SeatBlockError(ValueError):
    """A member's model block the server refuses (``member_llm``), with the HTTP status to answer."""

    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status


def llm_block(block) -> dict:
    """A seat's model block as a dict (empty for none), or ``SeatBlockError`` (400) for anything that is not an
    object: a string, a number or a list in its place is a malformed request, from an administrator too."""
    if block is None:
        return {}
    if not isinstance(block, dict):
        raise SeatBlockError(400, "A seat's model settings are an object naming a machine.")
    return dict(block)


def _is_admin(user) -> bool:
    """Whether an account may describe a machine inline: an administrator in good standing (the local owner is
    one)."""
    return user is not None and getattr(user, "role", None) == "admin" and \
        getattr(user, "status", None) in ("probation", "active")


def member_llm(block: Optional[dict], user, *, required: bool = False) -> dict:
    """The one gate on a seat's model block from a person (P3.2.9): a member may name a machine, never describe one.

    An administrator's block is returned as it is: inline blocks (a provider, a base URL, a key variable) are how
    admins, tests and scripts give a raw provider. For anyone else the block keeps ``MEMBER_LLM_KEYS`` only, and:

    * a block without a ``server_id`` is refused (400, "Pick a machine for this seat") unless its provider is
      ``dryrun``, which dials nothing (it keeps ``DRYRUN_LLM_KEYS`` too);
    * a ``server_id`` that names no machine is refused (400);
    * a machine is kept, and admission decides whether this member may use it (``pool.seats.authorize``): every
      machine is a row since the registry merge (package 3-05), the host and the site's API included, so a member
      reaches one only through a grant of its owner's.

    An empty block is returned empty, unless ``required`` (the block of a model seat in a game being made or
    changed: such a seat must name a machine). Raises ``SeatBlockError`` with the status to answer, for anything
    but an object (``llm_block``) whoever sends it.
    """
    block = llm_block(block)
    if _is_admin(user) or (not block and not required):
        return block
    sid = block.get("server_id")
    if not sid:
        if str(block.get("provider") or "").lower() == "dryrun":
            out = {k: block[k] for k in MEMBER_LLM_KEYS + DRYRUN_LLM_KEYS if k in block}
            out["provider"] = "dryrun"
            return out
        raise SeatBlockError(400, "Pick a machine for this seat: a seat names one of the machines you may use, "
                                  "not an address or a key.")
    if not isinstance(sid, str) or machine(sid) is None:
        raise SeatBlockError(400, "There is no such machine. Pick a machine for this seat.")
    keys = MEMBER_LLM_KEYS
    if str(((machine(sid) or {}).get("connection") or {}).get("provider") or "").lower() == "dryrun":
        keys = MEMBER_LLM_KEYS + DRYRUN_LLM_KEYS        # a dry-run machine's knobs: they dial nothing either
    return {k: block[k] for k in keys if k in block}


def resolve_llm(llm: dict, with_key: bool = True, *, inline_ok: bool = True) -> dict:
    """Turn a seat's llm block into the agent config: provider, endpoint, key, model and inference settings.

    A block with a ``server_id`` is built from that machine, taking only the seat overrides from the seat. A block
    without one (an administrator's inline block, tests, scripts) is passed through unchanged, but only when
    ``inline_ok``: the session's owner is an administrator, or the session has no owner. Otherwise it raises
    ``ServerError`` ("this seat has no machine"), and the seat plays no model (P3.2.9). An inline dry-run block is
    the exception, whoever owns the session: the dry-run provider dials nothing and reads no key, which is why
    ``member_llm`` admits it from members; it keeps only what ``member_llm`` keeps."""
    if not llm or not llm.get("server_id"):
        if not inline_ok:
            if str((llm or {}).get("provider") or "").lower() == "dryrun":
                out = {k: llm[k] for k in MEMBER_LLM_KEYS + DRYRUN_LLM_KEYS if k in llm}
                out["provider"] = "dryrun"
                return out
            raise ServerError("This seat has no machine: only an administrator can give a seat a model "
                              "configuration of its own. Pick a machine for this seat.")
        return dict(llm or {})
    pooled = _helper_view(llm["server_id"])
    if pooled is not None:
        # a helper machine: every request goes down the connection its helper opened
        from .pool import seats as pool_seats
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
        for k in DRYRUN_LLM_KEYS[1:]:
            if llm.get(k) not in (None, ""):
                cfg[k] = llm[k]
    prof = profile(m, llm.get("profile_id"))
    if prof:
        cfg["profile_id"] = prof["id"]
        cfg["load"] = dict(prof)
    # the dry run dials nothing and so needs no key: none is read for it, whatever its configuration names
    if with_key and conn["key"]["backend"] != "none" and provider != "dryrun":
        from . import keystore
        key = keystore.get(sv)
        if key:
            cfg["api_key"] = key
        elif conn["key"]["backend"] == "env" and conn["key"].get("env"):
            cfg["api_key_env"] = conn["key"]["env"]
    return {k: v for k, v in cfg.items() if v is not None}


def describe_seat(llm: dict) -> dict:
    """Display info for a seat's llm block (server name, model label, profile) without resolving keys."""
    pooled = _helper_view(llm.get("server_id"))
    if pooled is not None:
        from .pool import seats as pool_seats
        return pool_seats.describe(llm, pooled)
    sv = find(llm.get("server_id"))
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


def _helper_refusal(server: dict) -> Optional[str]:
    """Why CITAR must not dial or drive anything for ``server`` (a helper machine's models, loading and keys are its
    helper's: P3.2.8), or None. Every function below that reaches an address, the ``lms`` command or LM Studio's SDK
    asks it first, whatever route called it."""
    if is_helper((server or {}).get("id")):
        return f"{server.get('name') or 'This machine'} is reached through its CITAR helper, which reports its models."
    return None


def detect_models(server: dict) -> dict:
    """Ask the server which models it offers. Returns {"ok", "models": [{key, info, loaded}], "error"}."""
    import httpx
    refusal = _helper_refusal(server)
    if refusal:
        return {"ok": False, "error": refusal}
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
    refusal = _helper_refusal(server)
    if refusal:
        return {"ok": False, "error": refusal}
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
    if _helper_refusal(server):
        return {"loaded": False, "context": None, "others": []}
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
    if _helper_refusal(server) or server["connection"]["provider"] != "lmstudio":
        return None
    client = sdk()
    if client is None:
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
    if conn["provider"] != "lmstudio" or not conn.get("manage_loading") or _helper_refusal(server):
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
    if conn["provider"] != "lmstudio" or _helper_refusal(server):
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
    power = hw.get("power") if isinstance(hw.get("power"), dict) else {}
    sug = power.get("suggested") if isinstance(power.get("suggested"), dict) else {}
    if set_power:
        for k in ("idle_w", "cpu_max_w", "gpu_max_w"):
            if sv["power"].get(k) is None and sug.get(k) is not None:
                sv["power"][k] = sug[k]
                sv["power"]["source"] = "guess"
    return sv


def hardware_summary(hw: dict) -> str:
    """A one-line description of a machine, for lists and reports. Hardware comes from a collector file or a helper's
    hello, so any part of it may be missing or of another shape: what does not read is left out."""
    if not isinstance(hw, dict) or hw.get("source") in (None, "none"):
        return "No hardware details yet"

    def part(v) -> dict:
        return v if isinstance(v, dict) else {}

    def many(v) -> list:
        return [x for x in v if isinstance(x, dict)] if isinstance(v, list) else []
    parts = []
    cpu = part(hw.get("cpu"))
    if cpu.get("model"):
        parts.append(f"{cpu['model']} ({cpu.get('cores') or '?'}C/{cpu.get('threads') or '?'}T)")
    gpus = many(hw.get("gpus"))
    for g in gpus:
        if g.get("vendor") in ("Intel",) and len(gpus) > 1:
            continue
        vram = _num(g.get("vram_gb"))
        parts.append(f"{g.get('name')}" + (f" {vram:g} GB" if vram and not g.get("unified_memory") else ""))
    mem = part(hw.get("memory"))
    ram = _num(mem.get("ram_gb"))
    if ram:
        parts.append(f"{ram:g} GB {'unified memory' if mem.get('unified') else 'RAM'}")
    disk = sum(_num(d.get("size_gb"), 0) for d in many(hw.get("physical_disks")))
    if disk:
        parts.append(f"{disk / 1024:.1f} TB disk" if disk > 1000 else f"{disk:.0f} GB disk")
    return " · ".join(parts)


# ----------------------------------------------------------------------------- checks
#: The issue of an owned machine whose costs name no electricity plan: only an administrator can act on it (the plans
#: are the site's bill, ``pool.check_plans``), so a member's Servers page leaves it out.
NO_PLAN_ISSUE = "No electricity plan selected."


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
            out.append(NO_PLAN_ISSUE)
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
    """A server as the web UI sees it: config plus key status, restriction state and open issues, and what its row
    says of it (who dials it, whether it is on, whose it is)."""
    from . import keystore
    reg = reg or load()
    d = copy.deepcopy(server)
    row = machine(server.get("id"))
    d["is_host"] = bool(row["is_host"]) if row else reg.get("host_server_id") == server.get("id")
    if row:
        d.update(reach=row["reach"], enabled=row["enabled"], server_resource=row["server_resource"],
                 owner_id=row["owner_id"], group_id=row["group_id"], max_concurrent=row["max_concurrent"])
    d["key_status"] = keystore.status(server)
    end = restricted_at(server["id"]) if row else restricted(server)
    d["restricted_until"] = (restricted_text(server["id"], end) or None) if row else (end.strftime("%H:%M") if end else None)
    d["issues"] = issues(server, reg)
    d["hardware_summary"] = hardware_summary(server.get("hardware") or {})
    return d

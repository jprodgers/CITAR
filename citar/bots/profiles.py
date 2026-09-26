"""Bot profiles: named, revisioned configurations of the scripted bot, for A/B testing and rankings.

A profile is an *engine* (the bot code: ``basic`` - the live bot - or a frozen snapshot ``frozen_<hash>``, or
``idle``), an optional fixed *aggression*, and *parameter overrides* on top of that engine's defaults. Every
number the bot decides with is a parameter (see ``citar.bots.basic.PARAM_GROUPS``), so two profiles can differ in
anything from a single weight to the whole war doctrine.

Profiles are what lab experiments, lobby seats and benchmark opponents name. Two things keep results honest:

* **Revisions.** Saving a profile bumps its revision and keeps the old one in its history. Results are recorded
  against a revision, never against "whatever the profile says today".
* **Fingerprints.** A fingerprint hashes what actually plays - the code (the frozen hash of the engine's source),
  the effective overrides and the aggression. Ratings are kept per fingerprint, so a profile that follows the live
  bot (``basic``) gets a new rating line whenever the code changes, and two profiles that happen to be identical
  share one.

Built-in profiles are defined here and cannot be edited, only forked. Saved ones live in
``saves/bots/profiles/<id>.json``.
"""
from __future__ import annotations

import copy
import functools
import hashlib
import importlib
import json
import re
from datetime import datetime
from pathlib import Path
from typing import Optional

from .. import paths
from ..fsutil import replace as _fs_replace

BOTS_DIR = paths.PACKAGE / "bots"
PROFILES_DIR = paths.saves_path("bots", "profiles")
FROZEN_DIR = paths.saves_path("bots", "frozen")         # frozen copies when the package directory is read-only
ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{0,59}$")
ENGINE_RE = re.compile(r"^(basic|idle|frozen_[0-9a-f]{8})$")
DEFAULT_PROFILE = "standard"
BEST = "best"                # a seat may ask for the best-ranked profile on this server (see ratings.best_profile)

BUILTIN: list[dict] = [
    {"id": "standard", "name": "Standard", "engine": "basic", "aggression": None, "params": {},
     "tags": ["yardstick"],
     "description": "The live bot with its current defaults: the yardstick every benchmark is measured against. It "
                    "follows basic.py, so each code change starts a new rating line."},
    {"id": "classic-production", "name": "Classic production", "engine": "basic", "aggression": None,
     "params": {"prod_mode": "classic"}, "tags": ["control"],
     "description": "The live bot with the older fixed-priority production heuristic. A control to measure "
                    "production changes against."},
    {"id": "snapshot-0922", "name": "Snapshot 22 Sep", "engine": "frozen_d95d50cb", "aggression": None, "params": {},
     "tags": ["snapshot"],
     "description": "basic.py as of 2026-09-22 (v1 plus the deal-pricing fixes): the bot the server lab's "
                    "benchmark-reference experiments were queued with."},
    {"id": "v1", "name": "v1 (18 Sep)", "engine": "frozen_7149efb1", "aggression": None, "params": {},
     "tags": ["snapshot"],
     "description": "The accepted v1 baseline (2026-09-18): UnCiv-style production, luxury trades every 3 turns, "
                    "science and happiness weights from the first factorial screens."},
    {"id": "v0", "name": "v0 (18 Sep)", "engine": "frozen_4ce67344", "aggression": None, "params": {},
     "tags": ["snapshot"],
     "description": "The bot before the tuning campaign began (2026-09-18 morning)."},
    {"id": "idle", "name": "Idle", "engine": "idle", "aggression": None, "params": {}, "tags": ["control"],
     "description": "Does nothing but end its turn. The floor any real player should beat."},
]
_BUILTIN_IDS = {p["id"] for p in BUILTIN}


class ProfileError(ValueError):
    """A profile that cannot be saved or used, with a message fit to show the user."""


# ----------------------------------------------------------------------------------------------------------------------
# engines
# ----------------------------------------------------------------------------------------------------------------------
def _frozen_dirs() -> list[Path]:
    """Directories frozen bot copies are found in: the package's own, then the writable saves directory."""
    return [BOTS_DIR, FROZEN_DIR]


def code_id(engine: str) -> str:
    """The identity of an engine's code: ``frozen_<hash of basic.py>`` for the live bot, the name itself for a frozen
    copy, ``idle`` for the idle bot. The same code always gets the same id wherever it runs."""
    if engine == "basic":
        text = (BOTS_DIR / "basic.py").read_text(encoding="utf-8")
        return "frozen_" + hashlib.sha1(text.encode()).hexdigest()[:8]
    return engine


def freeze(engine: str = "basic") -> str:
    """The frozen module name for an engine, writing the frozen copy of basic.py if it does not exist yet.

    Frozen copies go in the package directory when it is writable (a source checkout, where they are committed so
    other machines can reproduce the result) and in the saves directory otherwise (an installed server)."""
    if engine != "basic":
        return engine
    text = (BOTS_DIR / "basic.py").read_text(encoding="utf-8")
    name = "frozen_" + hashlib.sha1(text.encode()).hexdigest()[:8]
    if any((d / f"{name}.py").exists() for d in _frozen_dirs()):
        return name
    body = f"# Frozen copy of citar/bots/basic.py made by citar.lab on {datetime.now():%Y-%m-%d %H:%M}\n" + text
    for d in _frozen_dirs():
        try:
            d.mkdir(parents=True, exist_ok=True)
            (d / f"{name}.py").write_text(body, encoding="utf-8")
            importlib.invalidate_caches()
            return name
        except OSError:
            continue
    raise ProfileError("Could not write a frozen copy of the bot anywhere (package and saves are read-only).")


def module(engine: str):
    """The Python module of an engine (``basic`` or a frozen copy)."""
    if engine == "idle" or not ENGINE_RE.match(engine or ""):
        raise ProfileError(f"'{engine}' is not a bot engine with parameters.")
    try:
        return importlib.import_module(f"citar.bots.{engine}")
    except ImportError as e:
        raise ProfileError(f"Bot engine {engine} is not installed here ({e}).") from None


def engines() -> list[dict]:
    """Every engine a profile can use: the live bot, each frozen snapshot (newest first) and the idle bot."""
    out = [{"id": "basic", "label": "Live bot (basic.py)", "code": code_id("basic"), "created": None,
            "description": "The current code. Profiles on it follow every change."}]
    seen = set()
    frozen = []
    for d in _frozen_dirs():
        for p in d.glob("frozen_*.py") if d.exists() else ():
            if p.stem in seen or not ENGINE_RE.match(p.stem):
                continue
            seen.add(p.stem)
            first = p.read_text(encoding="utf-8").split("\n", 1)[0]
            m = re.search(r"on (\d{4}-\d\d-\d\d \d\d:\d\d)", first)
            frozen.append({"id": p.stem, "code": p.stem, "created": m.group(1) if m else None,
                           "label": f"Snapshot {m.group(1) if m else ''} ({p.stem[7:]})".replace("  ", " "),
                           "description": "A frozen copy of basic.py. It never changes."})
    frozen.sort(key=lambda e: e["created"] or "", reverse=True)
    names = {p["engine"]: p["name"] for p in BUILTIN if p["engine"].startswith("frozen_")}
    for e in frozen:
        if e["id"] in names:
            e["label"] = f"{names[e['id']]} ({e['id'][7:]})"
    out += frozen
    out.append({"id": "idle", "label": "Idle bot", "code": "idle", "created": None,
                "description": "Ends its turn and nothing else."})
    return out


def defaults(engine: str) -> dict:
    """An engine's default parameters (empty for the idle bot)."""
    if engine == "idle":
        return {}
    return dict(getattr(module(engine), "DEFAULT_PARAMS", {}))


def schema(engine: str = "basic") -> dict:
    """The editable parameters of an engine, grouped, with labels, help and ranges. Old frozen engines predate the
    parameter descriptions, so theirs are inferred from their defaults."""
    if engine == "idle":
        return {"engine": engine, "groups": []}
    mod = module(engine)
    groups = getattr(mod, "PARAM_GROUPS", None)
    if groups:
        return {"engine": engine, "groups": [{"name": name, "help": help, "params": copy.deepcopy(specs)}
                                             for name, help, specs in groups]}
    specs = []
    for k, v in getattr(mod, "DEFAULT_PARAMS", {}).items():
        t = "bool" if isinstance(v, bool) else "int" if isinstance(v, int) else "float" if isinstance(v, float) \
            else "order" if isinstance(v, list) else "text"
        specs.append({"key": k, "default": v, "type": t, "label": k, "help": "", "min": None, "max": None})
    return {"engine": engine, "groups": [{"name": "Parameters", "help": "This snapshot predates parameter "
                                          "descriptions; names are the code's own.", "params": specs}]}


# ----------------------------------------------------------------------------------------------------------------------
# parameters
# ----------------------------------------------------------------------------------------------------------------------
@functools.lru_cache(maxsize=64)
def _spec_index(engine: str) -> dict:
    """key -> spec for an engine (cached: an imported module's parameters don't change while it is loaded)."""
    return {s["key"]: s for g in schema(engine)["groups"] for s in g["params"]}


def clean_params(engine: str, params: Optional[dict]) -> dict:
    """Validate and canonicalise overrides for an engine: unknown keys are refused, values are coerced to the
    parameter's type, and values equal to the engine's default are dropped (they are not overrides)."""
    params = dict(params or {})
    if engine == "idle":
        return {}
    specs = _spec_index(engine)
    out = {}
    for k, v in params.items():
        spec = specs.get(k)
        if spec is None:
            raise ProfileError(f"{k} is not a parameter of {engine}.")
        t = spec["type"]
        try:
            if t == "bool":
                v = v if isinstance(v, bool) else str(v).lower() in ("1", "true", "yes", "on")
            elif t == "int":
                if isinstance(v, bool):
                    raise ValueError
                f = float(v)
                v = int(f) if f == int(f) else f      # a fractional value is kept (the bot accepts floats)
            elif t == "float":
                if isinstance(v, bool):
                    raise ValueError
                v = float(v)
            elif t == "choice":
                if v not in spec["choices"]:
                    raise ValueError
            elif t in ("order", "list"):
                if isinstance(v, str):
                    if v not in spec.get("presets", {}) and v != "default":
                        raise ValueError
                elif v is not None:
                    if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
                        raise ValueError
                    v = list(v)
        except (TypeError, ValueError):
            raise ProfileError(f"{spec['label']} ({k}): {v!r} is not a valid {t}.") from None
        if v != spec["default"]:
            out[k] = v
    return out


def effective_params(engine: str, overrides: dict) -> dict:
    """Every parameter's value for an engine with overrides applied."""
    p = defaults(engine)
    p.update(overrides or {})
    return p


def fingerprint(engine: str, params: Optional[dict], aggression: Optional[float]) -> str:
    """What actually plays, hashed: code identity, canonical overrides, fixed aggression (None = set by the seat).

    ``engine`` may be ``basic`` (resolved to the hash of its current source) or a frozen copy; the same code and the
    same overrides give the same fingerprint wherever and however they were run."""
    code = code_id(engine)
    try:
        overrides = clean_params(code if code != "idle" and _importable(code) else engine, params)
    except ProfileError:
        overrides = dict(params or {})         # an old snapshot's keys we cannot validate: hash them as given
    blob = json.dumps({"code": code, "params": overrides,
                       "aggression": None if aggression is None else round(float(aggression), 3)},
                      sort_keys=True, default=str)
    return hashlib.sha1(blob.encode()).hexdigest()[:10]


def _importable(engine: str) -> bool:
    """Whether a frozen engine's module can be loaded here."""
    try:
        module(engine)
        return True
    except ProfileError:
        return False


# ----------------------------------------------------------------------------------------------------------------------
# storage
# ----------------------------------------------------------------------------------------------------------------------
def _path(pid: str) -> Path:
    """Where a saved profile lives."""
    return PROFILES_DIR / f"{pid}.json"


def _builtin(pid: str) -> Optional[dict]:
    """A built-in profile, as a full record."""
    for b in BUILTIN:
        if b["id"] == pid:
            d = copy.deepcopy(b)
            d.update({"builtin": True, "rev": 1, "parent": None, "created": None, "updated": None,
                      "created_by": None, "history": []})
            return d
    return None


def list_profiles() -> list[dict]:
    """Every profile: built-ins first, then saved ones by name."""
    out = [_builtin(b["id"]) for b in BUILTIN]
    saved = []
    if PROFILES_DIR.exists():
        for p in PROFILES_DIR.glob("*.json"):
            try:
                d = json.loads(p.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                continue
            if d.get("id") and d["id"] not in _BUILTIN_IDS:
                saved.append(d)
    saved.sort(key=lambda d: (d.get("archived", False), d.get("name", "").lower()))
    return out + saved


def get(pid: str) -> dict:
    """One profile by id, or ProfileError."""
    b = _builtin(pid)
    if b is not None:
        return b
    if not ID_RE.match(pid or ""):
        raise ProfileError(f"No bot profile called '{pid}'.")
    try:
        return json.loads(_path(pid).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        raise ProfileError(f"No bot profile called '{pid}'.") from None


def _slug(name: str) -> str:
    """A profile id from its name."""
    s = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")[:50] or "bot"
    return s if s[0].isalnum() else "bot-" + s


def new_id(name: str) -> str:
    """An unused id for a new profile."""
    base = _slug(name)
    pid, k = base, 2
    while pid in _BUILTIN_IDS or _path(pid).exists():
        pid, k = f"{base}-{k}", k + 1
    return pid


def save(data: dict, user: Optional[str] = None, note: str = "") -> dict:
    """Create or update a saved profile. An update that changes what plays (engine, aggression or parameters) makes
    a new revision; the previous one is kept in the history so results recorded against it stay explainable."""
    pid = data.get("id")
    if pid in _BUILTIN_IDS:
        raise ProfileError("Built-in profiles can't be changed. Fork it to make your own.")
    name = (data.get("name") or "").strip()
    if not name:
        raise ProfileError("A profile needs a name.")
    engine = data.get("engine") or "basic"
    if not ENGINE_RE.match(engine):
        raise ProfileError(f"Unknown engine {engine}.")
    if engine != "idle":
        module(engine)                            # installed here?
    agg = data.get("aggression")
    if agg is not None and agg != "":
        try:
            agg = max(0.0, min(1.0, float(agg)))
        except (TypeError, ValueError):
            raise ProfileError("Aggression is a number from 0 to 1, or empty to let the seat decide.") from None
    else:
        agg = None
    params = clean_params(engine, data.get("params"))
    now = datetime.now().isoformat(timespec="seconds")
    existing = None
    if pid:
        if not ID_RE.match(pid):
            raise ProfileError("A profile id is lower-case letters, digits, - and _.")
        try:
            existing = get(pid)
        except ProfileError:
            existing = None
    else:
        pid = new_id(name)
    if data.get("parent") and data["parent"] != pid:
        get(data["parent"])                       # the parent exists
    rec = existing or {"id": pid, "created": now, "created_by": user, "rev": 0, "history": [],
                       "parent": data.get("parent")}
    changed = existing is None or (existing.get("engine"), existing.get("aggression"), existing.get("params")) \
        != (engine, agg, params)
    rec.update({"name": name[:80], "description": (data.get("description") or "")[:2000],
                "tags": [str(t)[:30] for t in (data.get("tags") or [])][:12],
                "archived": bool(data.get("archived", False)),
                "engine": engine, "aggression": agg, "params": params, "builtin": False, "updated": now})
    if changed:
        rec["rev"] = int(rec.get("rev") or 0) + 1
        rec["history"] = (rec.get("history") or []) + [{
            "rev": rec["rev"], "at": now, "by": user, "note": (note or "")[:300], "engine": engine,
            "aggression": agg, "params": params}]
    PROFILES_DIR.mkdir(parents=True, exist_ok=True)
    tmp = _path(pid).with_suffix(".json.tmp")
    tmp.write_text(json.dumps(rec, indent=1), encoding="utf-8")
    _fs_replace(tmp, _path(pid))
    return rec


def fork(pid: str, name: Optional[str] = None, user: Optional[str] = None) -> dict:
    """A new saved profile copying another (built-in or saved)."""
    src = get(pid)
    return save({"name": name or f"{src['name']} (copy)", "description": src.get("description", ""),
                 "tags": [t for t in src.get("tags", []) if t not in ("yardstick", "snapshot", "control")],
                 "engine": src["engine"], "aggression": src.get("aggression"), "params": src.get("params") or {},
                 "parent": pid}, user=user, note=f"forked from {src['name']}")


def delete(pid: str):
    """Delete a saved profile. Its results stay in the record (they are kept per fingerprint)."""
    if pid in _BUILTIN_IDS:
        raise ProfileError("Built-in profiles can't be deleted.")
    get(pid)
    _path(pid).unlink(missing_ok=True)


# ----------------------------------------------------------------------------------------------------------------------
# using a profile
# ----------------------------------------------------------------------------------------------------------------------
def resolve(ref, *, freeze_code: bool = False) -> dict:
    """What a profile reference plays: engine, overrides, aggression, plus its identity.

    ``ref`` is a profile id, or a seat-like dict with "profile" (and optionally "params" to layer on top and
    "aggression"), or a raw seat {"bot": engine, "params": ...} without a profile. With ``freeze_code`` the live
    engine is replaced by a frozen copy of the current code, which is what a lab experiment needs."""
    if isinstance(ref, str) or ref is None:
        ref = {"profile": ref or DEFAULT_PROFILE}
    ref = dict(ref)
    if ref.get("profile") == BEST:
        from .ratings import best_profile
        ref["profile"] = best_profile()
    prof = get(ref["profile"]) if ref.get("profile") else None
    engine = (prof or {}).get("engine") or ref.get("bot") or "basic"
    if engine == "live":
        engine = "basic"
    params = dict((prof or {}).get("params") or {})
    params.update(ref.get("params") or {})
    agg = ref.get("aggression")
    if agg is None and prof is not None:
        agg = prof.get("aggression")
    if freeze_code:
        engine = freeze(engine)
    if engine not in ("idle",) and not engine.startswith("frozen_") and engine != "basic":
        raise ProfileError(f"Unknown bot engine {engine}.")
    try:
        params = clean_params(engine, params)
    except ProfileError:
        if prof is not None:
            raise
    return {"engine": engine, "params": params, "aggression": agg,
            "profile": prof["id"] if prof else None, "profile_rev": prof.get("rev") if prof else None,
            "profile_name": prof["name"] if prof else None, "fingerprint": fingerprint(engine, params, agg)}


def make_bot(ref=None, *, seed: Optional[int] = None, aggression: Optional[float] = None):
    """A bot instance for a profile reference (see resolve). ``aggression`` applies when the profile leaves it open;
    with neither, 0.4."""
    r = resolve(ref)
    if r["engine"] == "idle":
        from .idle import IdleBot
        return IdleBot()
    agg = r["aggression"] if r["aggression"] is not None else (aggression if aggression is not None else 0.4)
    mod = module(r["engine"])
    try:
        return mod.BasicBot(aggression=float(agg), seed=seed, params=r["params"])
    except TypeError:                     # snapshots from before parameters existed
        return mod.BasicBot(aggression=float(agg), seed=seed)


def revision_fingerprints() -> dict:
    """fingerprint -> (profile id, name, revision) for every revision of every profile whose code is pinned, and for
    the current revision of profiles on the live bot. Used to name rating entries."""
    out = {}
    for p in list_profiles():
        revs = p.get("history") or [{"rev": p.get("rev", 1), "engine": p["engine"], "aggression": p.get("aggression"),
                                     "params": p.get("params") or {}}]
        for h in revs:
            try:
                fp = fingerprint(h["engine"], h.get("params"), h.get("aggression"))
            except ProfileError:
                continue
            out.setdefault(fp, (p["id"], p["name"], h.get("rev", 1)))
    return out

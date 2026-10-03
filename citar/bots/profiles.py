"""Bot profiles: named, revisioned configurations of the scripted bot, for A/B testing and rankings.

A profile is an *engine* - a bot version compiled into the Rust engine (``basic-1``, ``idle``), or ``basic``, which
names the latest version wherever the profile is used - an optional fixed *aggression*, and *parameter overrides* on
top of that version's defaults. Every number the bot decides with is a parameter (the version's schema,
``engine_api.bot_schema``; basic-1 has 373 in 17 groups), so two profiles can differ in anything from a single weight
to the whole war doctrine (crates/citar-engine/DESIGN.md P2.8.5-P2.8.6).

Profiles are what lab experiments, lobby seats and benchmark opponents name. Two things keep results honest:

* **Revisions.** Saving a profile bumps its revision and keeps the old one in its history. Results are recorded
  against a revision, never against "whatever the profile says today".
* **Fingerprints.** A fingerprint hashes what actually plays (``engine_api.bot_fingerprint``): the build id (the
  engine's and the bot's code and the ruleset), the version, the overrides as cleaned and the profile's *fixed*
  aggression ("seat" when the seat decides), never the aggression a lab seat is given by its position. Ratings are
  kept per fingerprint, so a profile that follows the latest version (``basic``) gets a new rating line whenever a
  build changes what the bot does, and two profiles that happen to be identical share one.

Versions replace 0.1.5's frozen copies of ``basic.py``: a bot change that should not move existing results is a new
version, a deliberate copy in the Rust engine, so nothing is copied here any more. A ``frozen_*`` engine is refused
with :data:`ARCHIVED`: the snapshots were archived with 0.1.5.

Every question about a version - which exist, their parameters, cleaning, fingerprints - goes to the Rust engine
through the facade. Until package 2-12 deletes it, the Python engine can still be the facade's backend, and it has
none of those names (they raise ``engine_api.BackendError`` there): on it a profile still plays in a lobby seat
(:func:`resolve` and :func:`make_bot` then pass the stored overrides to the Python bot as they are and give no
fingerprint), but listing versions, editing profiles, the lab and the rankings' fingerprints need the Rust engine.

Built-in profiles are defined here and cannot be edited, only forked. Saved ones live in
``saves/bots/profiles/<id>.json``.
"""
from __future__ import annotations

import copy
import json
import math
import re
from datetime import datetime
from pathlib import Path
from typing import Optional

from .. import engine_api, paths
from ..fsutil import replace as _fs_replace

PROFILES_DIR = paths.saves_path("bots", "profiles")
ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{0,59}$")
#: The engines a profile may name: ``basic`` (the latest version, resolved where it is used), a version
#: ``basic-N``, or the idle bot.
ENGINE_RE = re.compile(r"^(basic|basic-\d+|idle)$")
LATEST = "basic"
DEFAULT_PROFILE = "standard"
BEST = "best"                # a seat may ask for the best-ranked profile on this server (see ratings.best_profile)
#: Why a frozen snapshot of 0.1.5's Python bot is refused.
ARCHIVED = ("{engine} is a frozen snapshot of the Python bot, archived with 0.1.5: the bot is now a version "
            "compiled into the engine. Use basic (the latest version), a version such as basic-1, or idle.")

BUILTIN: list[dict] = [
    {"id": "standard", "name": "Standard", "engine": "basic", "aggression": None, "params": {},
     "tags": ["yardstick"],
     "description": "The latest bot version with its defaults: the yardstick every benchmark is measured against. "
                    "It follows the latest version, and each build that changes what the bot does starts a new "
                    "rating line."},
    {"id": "classic-production", "name": "Classic production", "engine": "basic", "aggression": None,
     "params": {"prod_mode": "classic"}, "tags": ["control"],
     "description": "The latest bot version with the older fixed-priority production heuristic. A control to "
                    "measure production changes against."},
    {"id": "idle", "name": "Idle", "engine": "idle", "aggression": None, "params": {}, "tags": ["control"],
     "description": "Founds its capital and then does nothing but end its turn. The floor any real player should "
                    "beat."},
]
_BUILTIN_IDS = {p["id"] for p in BUILTIN}


class ProfileError(ValueError):
    """A profile that cannot be saved or used, with a message fit to show the user."""


# ----------------------------------------------------------------------------------------------------------------------
# engines: the bot versions
# ----------------------------------------------------------------------------------------------------------------------
def check_engine(engine) -> str:
    """``engine`` if a profile may name it, else ProfileError: a frozen snapshot with :data:`ARCHIVED`, anything else
    that is no ``basic``, ``basic-N`` or ``idle`` as unknown. Whether a ``basic-N`` is compiled in is the facade's to
    say (:func:`version`)."""
    if isinstance(engine, str) and engine.startswith("frozen_"):
        raise ProfileError(ARCHIVED.format(engine=engine))
    if not isinstance(engine, str) or not ENGINE_RE.match(engine):
        raise ProfileError(f"Unknown bot engine {engine}: a profile plays basic (the latest version), a version such "
                           "as basic-1, or idle.")
    return engine


def _refused(e: ValueError) -> ProfileError:
    """The facade's refusal (an unknown version, overrides that do not clean) as a profile error."""
    return ProfileError(str(e))


def engines() -> list[dict]:
    """Every engine a profile can name: ``basic``, which follows the latest version, then each version compiled into
    the engine, the latest first (``engine_api.bot_versions``; the idle bot is one). Each is {"id", "label", "code"
    (the version it plays today), "latest" (whether that is the latest version), "description", "created" (None:
    versions are compiled in, not made)}."""
    versions = engine_api.bot_versions()
    latest = next(v for v in versions if v["latest"])
    out = [{"id": LATEST, "label": f"Latest version ({latest['label']})", "code": latest["id"], "latest": True,
            "created": None,
            "description": f"Follows the latest version, today {latest['id']}: {latest['description']} A profile "
                           "on it plays each new version once one is compiled in."}]
    for v in versions:
        out.append({"id": v["id"], "label": v["label"], "code": v["id"], "latest": v["latest"], "created": None,
                    "description": v["description"] + (" Pinned: a later version does not change it."
                                                       if v["id"] != "idle" else "")})
    return out


def version(engine: str) -> str:
    """The version an engine name plays now: ``basic`` resolves to the latest, a version id to itself. ProfileError
    for a name the engine does not compile in."""
    check_engine(engine)
    versions = engine_api.bot_versions()
    if engine == LATEST:
        return next(v["id"] for v in versions if v["latest"])
    if any(v["id"] == engine for v in versions):
        return engine
    raise ProfileError(f"'{engine}' is not a bot version here (the versions are "
                       f"{', '.join(v['id'] for v in versions)}).")


def schema(engine: str = LATEST) -> dict:
    """A version's editable parameters, grouped, with labels, help, defaults and ranges: {"engine" (the version id),
    "groups"}, the Bots page's shape. ``basic`` gives the latest version's; the idle bot has no groups."""
    check_engine(engine)
    try:
        return engine_api.bot_schema(engine)
    except ValueError as e:
        raise _refused(e) from None


def defaults(engine: str) -> dict:
    """A version's default parameters, by key (empty for the idle bot)."""
    return {s["key"]: s["default"] for g in schema(engine)["groups"] for s in g["params"]}


def clean_params(engine: str, params: Optional[dict]) -> dict:
    """Validate and canonicalise overrides for a version (``engine_api.bot_clean_params``): unknown keys are refused,
    values are coerced to the parameter's type, values equal to the version's default are dropped (they are not
    overrides), and the keys are sorted. The idle bot takes none and ignores any."""
    check_engine(engine)
    try:
        return engine_api.bot_clean_params(engine, params or {})
    except ValueError as e:
        raise _refused(e) from None


def clean_aggression(value) -> Optional[float]:
    """A fixed aggression as a profile or a seat stores it: None (empty: the seat decides) or a number held to 0..1,
    as the bot holds it. ProfileError for anything else; a NaN is no number (Python's max(0, min(1, nan)) made it
    1.0, the most aggressive bot)."""
    if value is None or value == "":
        return None
    try:
        a = math.nan if isinstance(value, bool) else float(value)
    except (TypeError, ValueError):
        a = math.nan
    if math.isnan(a):
        raise ProfileError("Aggression is a number from 0 to 1, or empty to let the seat decide.")
    return max(0.0, min(1.0, a))


def fingerprint(engine: str, params: Optional[dict], aggression: Optional[float]) -> str:
    """What plays, hashed: the build, the version ``engine`` plays now, the overrides as cleaned and the profile's
    fixed ``aggression`` (None: set by the seat). ``basic`` and the version it names give the same fingerprint, and
    so do overrides that clean alike; a new build that changes the engine or the bot gives a new one."""
    check_engine(engine)
    try:
        bot = engine_api.bot_instance(engine, params=params or {},
                                      fixed_aggression=None if aggression is None else float(aggression))
    except ValueError as e:
        raise _refused(e) from None
    return engine_api.bot_fingerprint(bot)


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
    """Every profile: built-ins first, then saved ones by name. A saved profile on an archived frozen engine is
    listed (its history explains old results) but refused wherever it would play."""
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


def exists(pid: str) -> bool:
    """Whether a profile with this id exists."""
    try:
        get(pid)
        return True
    except ProfileError:
        return False


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
    engine = check_engine(data.get("engine") or LATEST)
    version(engine)                               # compiled in here?
    agg = clean_aggression(data.get("aggression"))
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
def resolve(ref, *, pin: bool = False) -> dict:
    """What a profile reference plays: {"engine", "version", "params", "aggression", "profile", "profile_rev",
    "profile_name", "fingerprint"}.

    ``ref`` is a profile id, or a seat-like dict with "profile" (and optionally "params" to layer on top and
    "aggression"), or a raw seat {"bot": engine, "params": ...} without a profile. ``engine`` is as the profile names
    it (``basic`` follows the latest version) and ``version`` the version that plays now; with ``pin`` the engine is
    that version, which is what a lab experiment records when it is submitted. ``profile`` is the profile's id ("best"
    resolved to the profile it stands for now). ``aggression`` is the profile's fixed one (or the seat's
    "aggression"), held to 0..1, None when the seat decides. The overrides are cleaned against the version's schema;
    a raw seat's that do not clean are refused too.

    On the Python backend (until package 2-12) there are no versions to resolve to and no schema or build to clean
    and fingerprint against: ``version`` and ``fingerprint`` are None and the overrides are passed on as stored, which
    lets a lobby seat play its profile there; ``pin`` raises the facade's BackendError, since only a version can be
    pinned.
    """
    if isinstance(ref, str) or ref is None:
        ref = {"profile": ref or DEFAULT_PROFILE}
    ref = dict(ref)
    if ref.get("profile") == BEST:
        from .ratings import best_profile
        ref["profile"] = best_profile()
    prof = get(ref["profile"]) if ref.get("profile") else None
    engine = (prof or {}).get("engine") or ref.get("bot") or LATEST
    if engine == "live":                          # 0.1.5's name for the live bot
        engine = LATEST
    check_engine(engine)
    params = dict((prof or {}).get("params") or {})
    extra = ref.get("params") or {}
    if not isinstance(extra, dict):
        raise ProfileError("A seat's parameter overrides are an object of names and values.")
    params.update(extra)
    agg = clean_aggression(ref.get("aggression"))
    if agg is None and prof is not None:
        agg = clean_aggression(prof.get("aggression"))
    try:
        ver = version(engine)
        params = clean_params(engine, params)
        fp = fingerprint(engine, params, agg)
    except engine_api.BackendError:
        if pin:
            raise
        ver = fp = None
    return {"engine": ver if pin else engine, "version": ver, "params": params, "aggression": agg,
            "profile": prof["id"] if prof else None, "profile_rev": prof.get("rev") if prof else None,
            "profile_name": prof["name"] if prof else None, "fingerprint": fp}


def make_bot(ref=None, *, seed: Optional[int] = None, aggression: Optional[float] = None):
    """A bot for a profile reference (see resolve), made by the engine's facade (``engine_api.bot_instance``), so it
    is the backend's own: a compiled bot's handle on the Rust engine. The profile's fixed aggression wins and is what
    its fingerprint hashes (``fixed_aggression``); ``aggression`` is the seat's, used when the profile leaves it
    open; with neither, 0.4. ``seed`` reaches only the Python backend's bot: the Rust bot draws from the game's
    seed."""
    r = resolve(ref)
    seat = float(aggression) if aggression is not None else 0.4
    try:
        return engine_api.bot_instance(r["engine"], seed=seed, aggression=seat, params=r["params"],
                                       fixed_aggression=None if r["aggression"] is None else float(r["aggression"]))
    except ValueError as e:
        raise _refused(e) from None

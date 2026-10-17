"""Trust tiers for the machines that play CPU jobs (design/phase4-helper.md P4.5.8): which results a replica checks,
how a machine climbs from ``new`` to ``trusted``, and what a mismatch does. The rules as pure functions over a
machine's tier and its ``servers.trust`` counters, so a table of cases holds each one (``tests/test_trust.py``); the
job dispatcher (``citar.jobs.dispatcher``, the only writer of the machines' trust) applies them.

**The settings** (``settings_kv.verification``, administrators', bounded: :func:`settings`): ``new_streak`` (5),
``provisional_rate`` (0.10), ``trusted_rate`` (0.02), ``promote_verified`` (50), ``overreport_days`` (30),
``new_per_owner`` (2), ``new_verify_share`` (0.5).

**The rules:**

1. **Replication is decided once a primary's result is committed** (:func:`replicate`): always for a ``new``
   machine, for a ``provisional`` or ``trusted`` one when the server's keyed sample of the job falls below its tier's
   rate (``engine_api.sample_fraction``: blake3 keyed by the server's secret over the job's id), and for the next N
   results an administrator asked for (``trust.replicate``). Nothing about a primary's offer or run differs between
   sampled and unsampled jobs, and no replica exists while a primary is in flight.
2. **Verifiers** are the local executor (when its canary and self-check pass) and the ``trusted`` machines an
   administrator marked ``verifier``; only a match against one counts toward ``streak`` and ``verified``, and a
   ``new`` machine's replicas run on the local executor alone.
3. **Promotion** (:func:`on_match`): ``new`` with ``streak >= new_streak`` becomes ``provisional``; ``provisional``
   with ``verified >= promote_verified``, no mismatch and no over-reporting (``flags.overreports`` or
   ``flags.overreported_cpu``) within ``overreport_days`` becomes ``trusted``; one step a match, each an audit row.
4. **A mismatch** (:func:`on_mismatch`) quarantines the machine, with its evidence (:func:`evidence`); a tampering
   verdict (``result``, ``signature``, ``claim``, ``chain_inconsistent``) quarantines its owner too, a ``chain``
   verdict (hardware, or an incompatible build) the machine alone.
5. **Release** (:func:`release`): back to ``new``, its counters reset and ``since`` now, no longer a verifier.
6. **A quarantined verifier's matches are re-opened** (:func:`lose_matches`): each machine whose counters held them
   loses them, and one whose promotion to its present tier counted any of them (``trust.promoted``: the tier and when
   the match that made it was counted) returns to ``new`` when it falls below that tier's threshold. A tier an
   administrator set, or matches made after the promotion, never demote.

**Credits and speed** (P4.5.7): a machine's measured speed (``trust.speed``, :func:`speed_sample`) is the median of
the reference's CPU seconds over its own on its replicated jobs (the last ``SPEED_SAMPLES``); a machine under half the
median speed of the other measured machines of its CPU class is ``class_suspect`` (:func:`class_suspect`), for an
administrator to pin its class.

**Model results by tier** (rule 11, :func:`vouched`): a machine's model results (benchmark games, probes, the
diplomacy leaderboard's games) count toward the global boards when it was a site machine, an administrator's, or
``trusted`` when they ran, or is ``trusted`` now.

A key reset never changes any of this (P4.3.7): the trust is the machine's row's, not its key's.
"""
from __future__ import annotations

import math
import threading
import time
from datetime import datetime, timedelta, timezone
from typing import Optional

#: The machines' tiers (``models.TRUST_TIERS``).
TIERS = ("new", "provisional", "trusted", "quarantined")
#: Where the settings are kept.
SETTINGS_KEY = "verification"
#: The settings' defaults (plan §1's figures where it gives them).
DEFAULTS = {"new_streak": 5, "provisional_rate": 0.10, "trusted_rate": 0.02, "promote_verified": 50,
            "overreport_days": 30, "new_per_owner": 2, "new_verify_share": 0.5}
#: Each setting's bounds: the rates and the share 0..1, the counts 1..10,000.
BOUNDS = {"new_streak": (1, 10_000), "provisional_rate": (0.0, 1.0), "trusted_rate": (0.0, 1.0),
          "promote_verified": (1, 10_000), "overreport_days": (1, 10_000), "new_per_owner": (1, 10_000),
          "new_verify_share": (0.0, 1.0)}
#: How long the settings are kept before they are read again.
SETTINGS_SECONDS = 2.0

#: The verdicts that say a machine's helper lied (P4.5.8 rule 5): they quarantine its owner too.
TAMPERING = ("result", "signature", "claim", "chain_inconsistent")
#: Every verdict a mismatch or a refused claim gives, and an administrator's quarantine.
VERDICTS = TAMPERING + ("chain", "administrator")
#: How each verdict reads in the evidence an administrator sees.
WORDS = {
    "result": "same chain, different result: tampering or an incompatible build",
    "chain": "the chains parted at round {round}: faulty hardware or an incompatible build",
    # a chain verdict whose round cannot be named (a re-opened primary's digests were pruned with its line)
    "chain_unknown": "the chains differ (the round where they parted is not known): faulty hardware or an "
                     "incompatible build",
    "signature": "a bad signature: its claim does not verify against the machine's registered key",
    "claim": "a claim that does not hold: tampering or an incompatible build",
    "chain_inconsistent": "its digests do not fold to the head it claims: tampering or an incompatible build",
    "administrator": "quarantined by an administrator",
}
#: The counters a release resets.
COUNTERS = ("verified", "streak", "mismatches", "abandons", "claim_failures", "helper_faults")


class SettingsError(ValueError):
    """A setting out of its bounds, or one that does not exist."""


def _bounded(key: str, value) -> float:
    """``value`` for setting ``key`` when it is in the setting's bounds (a count a whole number), else
    ``SettingsError``. Compared before it is converted, and never through ``float`` of an int: JSON gives
    ``1e999`` as infinity and a 400-digit count as an int no float holds, and both must be refused, not raise."""
    lo, hi = BOUNDS[key]
    if (isinstance(value, bool) or not isinstance(value, (int, float))
            or (isinstance(value, float) and not math.isfinite(value))):
        raise SettingsError(f"{key} is a number from {lo} to {hi}.")
    whole = isinstance(lo, int) and isinstance(hi, int)
    if whole and isinstance(value, float) and not value.is_integer():
        raise SettingsError(f"{key} is a whole number from {lo} to {hi}.")
    if not lo <= value <= hi:
        shown = str(value) if abs(value) < 1e9 else "a number past them"
        raise SettingsError(f"{key} is from {lo} to {hi}, not {shown}.")
    return int(value) if whole else value


def clean(changes: dict) -> dict:
    """``changes`` held to the settings' names and bounds (``SettingsError`` otherwise)."""
    if not isinstance(changes, dict):
        raise SettingsError("The settings are an object.")
    unknown = sorted(set(changes) - set(DEFAULTS))
    if unknown:
        raise SettingsError(f"Unknown verification settings: {', '.join(unknown)}.")
    return {k: _bounded(k, v) for k, v in changes.items()}


_CACHE: dict = {"at": -1e18, "value": None}
_CACHE_LOCK = threading.Lock()


def settings(session=None, *, fresh: bool = False) -> dict:
    """The verification settings: the defaults with what ``settings_kv.verification`` holds over them (a stored value
    out of its bounds is left out), read again every ``SETTINGS_SECONDS``."""
    now = time.monotonic()
    with _CACHE_LOCK:
        if not fresh and _CACHE["value"] is not None and now - _CACHE["at"] < SETTINGS_SECONDS:
            return dict(_CACHE["value"])
    stored = _stored(session)
    out = dict(DEFAULTS)
    for k, v in (stored or {}).items():
        if k in DEFAULTS:
            try:
                out[k] = _bounded(k, v)
            except SettingsError:
                pass
    with _CACHE_LOCK:
        _CACHE.update(at=now, value=dict(out))
    return out


def _stored(session) -> dict:
    from citar.db.models import Setting
    if session is not None:
        row = session.get(Setting, SETTINGS_KEY)
        return dict(row.value or {}) if row is not None else {}
    from citar import db
    if not db.configured():
        return {}
    with db.session() as s:
        row = s.get(Setting, SETTINGS_KEY)
        return dict(row.value or {}) if row is not None else {}


def save_settings(session, changes: dict) -> dict:
    """Store ``changes`` (bounded, ``clean``) over the settings kept; the caller commits. Returns the settings now."""
    from citar.db.models import Setting
    changes = clean(changes)
    row = session.get(Setting, SETTINGS_KEY)
    value = dict(row.value or {}) if row is not None else {}
    value.update(changes)
    if row is None:
        session.add(Setting(key=SETTINGS_KEY, value=value))
    else:
        row.value = value
    session.flush()
    forget()
    out = dict(DEFAULTS)
    out.update({k: v for k, v in value.items() if k in DEFAULTS})
    return out


def forget() -> None:
    """Read the settings again at the next look."""
    with _CACHE_LOCK:
        _CACHE.update(at=-1e18, value=None)


# ---------------------------------------------------------------------------- rule 1: replication

def rate(tier: str, s: dict) -> float:
    """The share of a machine's results a replica checks: every one while ``new`` (and, were it offered any, while
    ``quarantined``), its tier's rate after."""
    if tier == "provisional":
        return float(s["provisional_rate"])
    if tier == "trusted":
        return float(s["trusted_rate"])
    return 1.0


def sampled(secret: str, job_id: str, p: float) -> bool:
    """Whether the server's keyed sample takes ``job_id`` at rate ``p``."""
    if p >= 1.0:
        return True
    if p <= 0.0:
        return False
    from citar import engine_api
    return engine_api.sample_fraction(secret, job_id) < p


def replicate(tier: str, job_id: str, s: dict, secret: str, asked: int = 0) -> Optional[str]:
    """Why a primary's committed result is checked by a replica (``new``, ``asked`` for an administrator's request,
    ``sampled``), or None: accepted as it is (``unverified``). Decided only once the result is in (rule 1)."""
    if tier not in ("provisional", "trusted"):
        return "new"
    if asked > 0:
        return "asked"
    return "sampled" if sampled(secret, job_id, rate(tier, s)) else None


# ---------------------------------------------------------------------------- rules 3-6: the tier machine

def _iso(now: datetime) -> str:
    return now.astimezone(timezone.utc).isoformat(timespec="seconds")


def _iso_exact(now: datetime) -> str:
    """A time to the microsecond: a promotion's, compared with the times its matches were counted."""
    return now.astimezone(timezone.utc).isoformat(timespec="microseconds")


def _when(text) -> Optional[datetime]:
    if not isinstance(text, str):
        return None
    try:
        t = datetime.fromisoformat(text)
    except ValueError:
        return None
    return t if t.tzinfo is not None else t.replace(tzinfo=timezone.utc)


def overreported(flags: Optional[dict], now: datetime, days: int) -> bool:
    """Whether a machine over-reported (its model work's time, ``flags.overreports``; its CPU, ``flags.
    overreported_cpu``) within ``days``: that blocks its promotion to ``trusted`` (rule 6), never quarantines it."""
    for flag in ("overreports", "overreported_cpu"):
        entry = (flags or {}).get(flag)
        last = _when(entry.get("last")) if isinstance(entry, dict) else None
        if last is not None and now - last <= timedelta(days=days):
            return True
    return False


def on_match(tier: str, trust: dict, s: dict, *, counts: bool, flags: Optional[dict] = None,
             now: Optional[datetime] = None) -> tuple:
    """A primary's result matched its replica: ``(tier, trust, promoted)``. Only a match against a verifier
    ``counts`` (the local executor, or a machine an administrator marked ``verifier``); it adds to ``verified`` and
    ``streak``, and promotes one step when the next tier's threshold is met (``promoted`` the new tier, else None).
    ``now`` is when the match was counted; a promotion keeps it (``trust.promoted``, :func:`promoted_at`), so that
    rule 9 can tell the matches it counted from those made after it."""
    now = now or datetime.now(timezone.utc)
    trust = dict(trust or {})
    if not counts or tier == "quarantined":
        return tier, trust, None
    trust["verified"] = int(trust.get("verified") or 0) + 1
    trust["streak"] = int(trust.get("streak") or 0) + 1
    trust["last_verified_at"] = _iso(now)
    up = None
    if tier == "new" and trust["streak"] >= int(s["new_streak"]):
        up = "provisional"
    elif (tier == "provisional" and trust["verified"] >= int(s["promote_verified"])
            and not int(trust.get("mismatches") or 0)
            and not overreported(flags, now, int(s["overreport_days"]))):
        up = "trusted"
    if up is None:
        return tier, trust, None
    trust["promoted"] = {"tier": up, "at": _iso_exact(now)}
    return up, trust, up


def promoted_at(tier: Optional[str], trust: Optional[dict]) -> Optional[datetime]:
    """When the machine was promoted to ``tier`` (its present one) by its matches, or None when that tier was not a
    promotion's (an administrator set it, or the machine is ``new``)."""
    rec = (trust or {}).get("promoted")
    if not isinstance(rec, dict) or rec.get("tier") != tier:
        return None
    return _when(rec.get("at"))


def evidence(verdict: str, *, run: Optional[str] = None, job: Optional[str] = None, round: Optional[int] = None,
             why: str = "", against: Optional[str] = None, now: Optional[datetime] = None) -> dict:
    """What a quarantine's verdict says (kept as the machine's ``trust.last_mismatch`` and on its run): the verdict,
    its words, the run and job, the round where the chains parted (``chain``), the detail, and the verifier it was
    checked against."""
    key = "chain_unknown" if verdict == "chain" and round is None else verdict
    words = WORDS.get(key, verdict).format(round=round)
    out = {"verdict": verdict, "words": words, "at": _iso(now or datetime.now(timezone.utc))}
    for k, v in (("run", run), ("job", job), ("round", round), ("why", why[:500] if why else None),
                 ("against", against)):
        if v is not None:
            out[k] = v
    return out


def on_mismatch(trust: dict, ev: dict) -> tuple:
    """A mismatch (or a refused claim): ``("quarantined", trust)`` with the evidence kept and the streak gone."""
    trust = dict(trust or {})
    if ev.get("verdict") != "administrator":
        trust["mismatches"] = int(trust.get("mismatches") or 0) + 1
    trust["last_mismatch"] = ev
    trust["streak"] = 0
    trust["quarantined_at"] = ev.get("at")
    trust["verifier"] = False
    trust.pop("promoted", None)
    return "quarantined", trust


def tampering(verdict: str) -> bool:
    """Whether a verdict quarantines the machine's owner too (rule 7)."""
    return verdict in TAMPERING


def release(trust: dict, now: Optional[datetime] = None) -> tuple:
    """A machine released from quarantine, or set back to ``new`` by an administrator: ``("new", trust)`` with its
    counters reset, ``since`` now (what a later withdrawal reaches back to), no longer a verifier, no replication
    asked. Its evidence stays."""
    trust = dict(trust or {})
    for k in COUNTERS:
        trust[k] = 0
    trust["since"] = _iso(now or datetime.now(timezone.utc))
    trust["verifier"] = False
    trust["replicate"] = 0
    trust.pop("quarantined_at", None)
    trust.pop("promoted", None)
    return "new", trust


def lose_matches(tier: str, trust: dict, n: int, s: dict, *, counted: int = 0) -> tuple:
    """A quarantined verifier's ``n`` matches taken from a machine whose counters held them (rule 9): ``(tier, trust,
    demoted)``. ``counted`` of them were made at or before the machine's promotion to its present tier (a tier it was
    promoted to: :func:`promoted_at`; the caller counts none for a tier an administrator set). Only then can it fall:
    back to ``new`` (``demoted`` True) when its counters, the ``n`` taken off, are below its tier's threshold
    (``new_streak`` for a ``provisional`` machine, ``promote_verified`` for a ``trusted`` one). Matches made after the
    promotion are taken off the counters and never demote it: they did not earn its tier."""
    trust = dict(trust or {})
    trust["verified"] = max(0, int(trust.get("verified") or 0) - n)
    trust["streak"] = max(0, int(trust.get("streak") or 0) - n)
    below = ((tier == "provisional" and trust["streak"] < int(s["new_streak"]))
             or (tier == "trusted" and trust["verified"] < int(s["promote_verified"])))
    if counted > 0 and below:
        trust["verifier"] = False
        trust.pop("promoted", None)
        return "new", trust, True
    return tier, trust, False


def since(trust: Optional[dict]) -> Optional[datetime]:
    """When a machine's present standing began: its last release from quarantine (``trust.since``), or None (its
    registration: every result it ever gave)."""
    return _when((trust or {}).get("since"))


def is_verifier(tier: Optional[str], trust: Optional[dict]) -> bool:
    """Whether a helper machine verifies others' results: ``trusted`` and marked ``verifier`` by an administrator."""
    return tier == "trusted" and bool((trust or {}).get("verifier"))


# ---------------------------------------------------------------------------- credits: the measured speed (P4.5.7)

#: How many of a machine's replicated jobs its measured speed is the median of (the latest).
SPEED_SAMPLES = 25
#: A run whose CPU seconds (its own or its reference's) are under this says nothing of a machine's speed: Windows
#: counts a thread's CPU time in steps of 15.6 ms, so a shorter game's figures are mostly the timer's grain.
SPEED_FLOOR_S = 0.2
#: A measured speed is held within these bounds (a reference ten thousand times faster than a machine is a figure).
SPEED_MIN, SPEED_MAX = 0.001, 1000.0
#: A machine whose measured speed is under this share of the median of the other measured machines of its CPU class
#: is ``class_suspect`` (P4.5.7): its class prices its work as faster than it is, or its helper over-reports.
CLASS_SUSPECT_SHARE = 0.5


def _median(values: list) -> Optional[float]:
    xs = sorted(float(v) for v in values)
    if not xs:
        return None
    mid = len(xs) // 2
    return xs[mid] if len(xs) % 2 else (xs[mid - 1] + xs[mid]) / 2.0


def speed_sample(trust: Optional[dict], reference_s, own_s) -> dict:
    """A replicated job's figures added to a machine's measured speed (P4.5.7): ``reference_s`` (its replica's CPU
    seconds: the reference machine's) over ``own_s`` (what its helper reported), kept among its last
    ``SPEED_SAMPLES``; ``trust.speed`` the median of them. A job too short to time (either side under
    ``SPEED_FLOOR_S``) adds nothing. Returns the trust, changed or not."""
    trust = dict(trust or {})
    try:
        ref, own = float(reference_s or 0.0), float(own_s or 0.0)
    except (TypeError, ValueError):
        return trust
    if not (ref >= SPEED_FLOOR_S and own >= SPEED_FLOOR_S) or ref != ref or own != own:
        return trust
    ratio = min(SPEED_MAX, max(SPEED_MIN, ref / own))
    samples = [float(x) for x in (trust.get("speeds") or []) if isinstance(x, (int, float))]
    samples = (samples + [round(ratio, 4)])[-SPEED_SAMPLES:]
    trust["speeds"] = samples
    trust["speed"] = round(_median(samples), 3)
    return trust


def speed_of(trust: Optional[dict]) -> Optional[float]:
    """A machine's measured speed (``trust.speed``), or None while it has none."""
    v = (trust or {}).get("speed")
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) and v > 0 else None


def class_suspect(speed: Optional[float], others: list) -> Optional[float]:
    """Whether a machine's measured ``speed`` is out of line with its CPU class (P4.5.7): under
    ``CLASS_SUSPECT_SHARE`` of the median speed of ``others`` (the other measured machines of its class). Returns that
    median when it is, else None (no speed, no other measured machine, or in line)."""
    if speed is None:
        return None
    median = _median([o for o in others if isinstance(o, (int, float)) and o > 0])
    if median is None or speed >= CLASS_SUSPECT_SHARE * median:
        return None
    return median


def card(row, *, admin: bool, cpu_class: Optional[dict] = None) -> dict:
    """A machine's trust as the Servers page shows it (design/phase4-helper.md P4.5.11): its tier with its verified
    count, streak and mismatches to its owner; to an administrator (``admin``) also its last mismatch with its evidence,
    its measured speed beside its CPU class (``cpu_class``: ``{id, name, pinned}``), its ``verifier`` mark, when its
    standing began, the promotion it earned and the replication asked."""
    trust = dict(getattr(row, "trust", None) or {})
    out = {"tier": getattr(row, "trust_tier", None) or "new", "verified": int(trust.get("verified") or 0),
           "streak": int(trust.get("streak") or 0), "mismatches": int(trust.get("mismatches") or 0)}
    if admin:
        out.update({k: trust.get(k) for k in ("last_mismatch", "speed", "since", "promoted", "quarantined_at",
                                              "replicate")})
        out["verifier"] = bool(trust.get("verifier"))
        out["cpu_class"] = cpu_class
    return out


def cpu_class_of(session, row) -> Optional[dict]:
    """The CPU class a machine is priced by (``{id, name, pinned}``), the one an administrator pinned or its
    hardware's (``ledger.class_of``)."""
    from sqlalchemy import select
    from citar import ledger
    from citar.db.models import HardwareClass
    classes = [ledger.class_dict(c) for c in session.scalars(select(HardwareClass).where(HardwareClass.kind == "cpu"))]
    facts = {"hardware": (row.config or {}).get("hardware") or {}, "class_override": bool(row.class_override),
             "gpu_class_id": row.gpu_class_id, "cpu_class_id": row.cpu_class_id}
    cls = ledger.class_of(facts, "cpu", classes)
    return {"id": cls["id"], "name": cls["name"], "pinned": bool(row.class_override)} if cls else None


# ---------------------------------------------------------------------------- rule 11: model results by tier

def vouched(tier: Optional[str], *, site: bool = False, owner_admin: bool = False) -> bool:
    """Whether a machine's model results count toward the global leaderboards (P4.5.8 rule 11): a site machine (the
    host, the site's online APIs), one an administrator owns, or one ``trusted``. The others' results are listed with
    an "unverified machine" badge and left out of the global boards until an administrator trusts the machine."""
    return bool(site) or bool(owner_admin) or tier == "trusted"


def standing_of(row, owner) -> dict:
    """A machine row's standing as a model result records it when it runs (rule 11: "when they ran"): its tier and
    whether it is :func:`vouched`. ``owner`` is its owner's row (or None). A machine with no owner is the site's as the
    host is (administrators manage it: the configuration's machines, an account's deleted)."""
    from citar.auth import access
    tier = getattr(row, "trust_tier", None) or "new"
    admin = owner is not None and getattr(owner, "role", None) == "admin" and getattr(owner, "status", None) == "active"
    site = access.is_site_machine(row) or not getattr(row, "owner_id", None)
    return {"tier": tier, "vouched": vouched(tier, site=site, owner_admin=admin)}


def standing(server_id: Optional[str], session=None) -> Optional[dict]:
    """:func:`standing_of` for a machine by its id, read from the database (None: no such machine, or no database: a
    seat with no machine, a test's game)."""
    if not server_id:
        return None
    from citar.db.models import Server, User

    def read(s):
        row = s.get(Server, server_id)
        if row is None:
            return None
        owner = s.get(User, row.owner_id) if row.owner_id else None
        return standing_of(row, owner)
    if session is not None:
        return read(session)
    from citar import db
    if not db.configured():
        return None
    try:
        with db.session() as s:
            return read(s)
    except Exception:
        return None


#: How long :func:`vouched_now`'s answers are kept (the boards are read every few seconds by an open page).
VOUCHED_SECONDS = 5.0
_VOUCHED: dict = {}
_VOUCHED_LOCK = threading.Lock()


def vouched_now(ids) -> set:
    """Of the machines ``ids``, those whose model results count now (:func:`vouched` as their rows are read now: an
    administrator who trusts a machine brings its earlier results onto the boards). Read at most every
    ``VOUCHED_SECONDS`` per machine; a machine the database does not hold counts nothing."""
    ids = {str(i) for i in ids or () if i}
    if not ids:
        return set()
    now = time.monotonic()
    out, ask = set(), set()
    with _VOUCHED_LOCK:
        for i in ids:
            hit = _VOUCHED.get(i)
            if hit is not None and now - hit[0] < VOUCHED_SECONDS:
                if hit[1]:
                    out.add(i)
            else:
                ask.add(i)
    if ask:
        from citar import db
        if not db.configured():
            return out
        from sqlalchemy import select
        from citar.db.models import Server, User
        with db.session() as s:
            rows = {r.id: r for r in s.scalars(select(Server).where(Server.id.in_(sorted(ask))))}
            owners = {u.id: u for u in s.scalars(select(User).where(
                User.id.in_(sorted({r.owner_id for r in rows.values() if r.owner_id}) or [""])))}
            got = {i: bool(rows.get(i) is not None
                           and standing_of(rows[i], owners.get(rows[i].owner_id))["vouched"]) for i in ask}
        with _VOUCHED_LOCK:
            if len(_VOUCHED) > 5000:
                _VOUCHED.clear()
            for i, ok in got.items():
                _VOUCHED[i] = (now, ok)
                if ok:
                    out.add(i)
    return out


def not_counted(entries) -> set:
    """Of ``entries`` (``(machine, standing)``: a benchmark job's, a probe run's), the machines whose model results
    do not count now (rule 11): not :func:`vouched` when they ran, and not since. Their results carry the "unverified
    machine" badge."""
    ids = {str(sid) for sid, st in entries or ()
           if sid and isinstance(st, dict) and st.get("vouched") is False}
    return ids - vouched_now(ids) if ids else set()


def forget_vouched() -> None:
    """Read the machines' standing again at the next look (a tier changed)."""
    with _VOUCHED_LOCK:
        _VOUCHED.clear()

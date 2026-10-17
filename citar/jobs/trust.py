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
6. **A quarantined verifier's matches are re-opened** (:func:`lose_matches`): each machine whose promotion counted
   them loses them, and returns to ``new`` when it falls below its tier's threshold.

A key reset never changes any of this (P4.3.7): the trust is the machine's row's, not its key's.
"""
from __future__ import annotations

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
    lo, hi = BOUNDS[key]
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value != value:
        raise SettingsError(f"{key} is a number from {lo} to {hi}.")
    if isinstance(lo, int) and isinstance(hi, int):
        if float(value) != int(value):
            raise SettingsError(f"{key} is a whole number from {lo} to {hi}.")
        value = int(value)
    if not lo <= value <= hi:
        raise SettingsError(f"{key} is from {lo} to {hi}, not {value}.")
    return value


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
    ``streak``, and promotes one step when the next tier's threshold is met (``promoted`` the new tier, else None)."""
    now = now or datetime.now(timezone.utc)
    trust = dict(trust or {})
    if not counts or tier == "quarantined":
        return tier, trust, None
    trust["verified"] = int(trust.get("verified") or 0) + 1
    trust["streak"] = int(trust.get("streak") or 0) + 1
    trust["last_verified_at"] = _iso(now)
    if tier == "new" and trust["streak"] >= int(s["new_streak"]):
        return "provisional", trust, "provisional"
    if (tier == "provisional" and trust["verified"] >= int(s["promote_verified"])
            and not int(trust.get("mismatches") or 0)
            and not overreported(flags, now, int(s["overreport_days"]))):
        return "trusted", trust, "trusted"
    return tier, trust, None


def evidence(verdict: str, *, run: Optional[str] = None, job: Optional[str] = None, round: Optional[int] = None,
             why: str = "", against: Optional[str] = None, now: Optional[datetime] = None) -> dict:
    """What a quarantine's verdict says (kept as the machine's ``trust.last_mismatch`` and on its run): the verdict,
    its words, the run and job, the round where the chains parted (``chain``), the detail, and the verifier it was
    checked against."""
    words = WORDS.get(verdict, verdict).format(round=round)
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
    return "new", trust


def lose_matches(tier: str, trust: dict, n: int, s: dict) -> tuple:
    """A quarantined verifier's ``n`` matches taken from a machine whose counters held them (rule 9): ``(tier, trust,
    demoted)``, back to ``new`` (``demoted`` True) when it falls below its tier's threshold: ``new_streak`` for a
    ``provisional`` machine, ``promote_verified`` for a ``trusted`` one."""
    trust = dict(trust or {})
    trust["verified"] = max(0, int(trust.get("verified") or 0) - n)
    trust["streak"] = max(0, int(trust.get("streak") or 0) - n)
    below = ((tier == "provisional" and trust["streak"] < int(s["new_streak"]))
             or (tier == "trusted" and trust["verified"] < int(s["promote_verified"])))
    if below:
        trust["verifier"] = False
        return "new", trust, True
    return tier, trust, False


def since(trust: Optional[dict]) -> Optional[datetime]:
    """When a machine's present standing began: its last release from quarantine (``trust.since``), or None (its
    registration: every result it ever gave)."""
    return _when((trust or {}).get("since"))


def is_verifier(tier: Optional[str], trust: Optional[dict]) -> bool:
    """Whether a helper machine verifies others' results: ``trusted`` and marked ``verifier`` by an administrator."""
    return tier == "trusted" and bool((trust or {}).get("verifier"))

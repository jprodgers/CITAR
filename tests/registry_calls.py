"""The table of ``citar.servers`` calls that must answer the same before and after the registry merge
(design/phase3-features.md P3.2.8, package 3-05's gate 2).

``tests/data/registry/expected.json`` holds what the file-backed registry (``config/servers.json``, before 3-05)
answered to each call on the fixture ``tests/data/registry/servers.json``, recorded once with this module before the
change; ``tests/test_registry_merge.py`` asks the same calls of the rows the import made and compares. Phase A is the
fixture alone; phase B adds a helper machine (a row the file never held, ``sv_helper``), whose seats were played
through its helper before the merge as after it.

Every call runs with the clock frozen at ``NOW`` (a Wednesday evening, inside the host's quiet hours) and LM Studio's
model list stubbed out, so nothing here depends on the day or dials anything.
"""
from __future__ import annotations

import contextlib
import json
from datetime import datetime
from pathlib import Path
from unittest import mock

DATA = Path(__file__).resolve().parent / "data" / "registry"
FIXTURE = DATA / "servers.json"
EXPECTED = DATA / "expected.json"

#: The file keystore's passphrase and the Anthropic key stored in it for ``sv_claude``.
PASSPHRASE = "registry merge fixture passphrase"
CLAUDE_KEY = "sk-ant-fixture-0123456789"
#: The LAN machine's key variable's value (the ``env`` backend reads it).
LAN_KEY = "lan-fixture-key"

#: Wednesday 2026-09-23 23:30 on this computer's clock: inside the host's weekday window (23:00-06:00).
NOW = datetime(2026, 9, 23, 23, 30)

HELPER = {"id": "sv_helper", "name": "Bob's GPU box", "kind": "owned", "provider": "lmstudio",
          "max_concurrent": 2, "owner_tz": "America/New_York",
          "config": {"connection": {"provider": "lmstudio", "max_parallel": 2},
                     "models": [{"key": "mistral/small-4", "label": "Mistral Small",
                                 "inference": {"tool_mode": "json", "max_tokens": 2048}}],
                     "restricted_hours": {"enabled": True, "grace_minutes": 5,
                                          "windows": [{"days": [0, 1, 2, 3, 4, 5, 6], "start": "22:00",
                                                       "end": "07:00"}]}}}

#: Keys of a server dict that change every time it is written (``normalize_server`` stamps ``updated``).
VOLATILE = ("updated",)


class _Frozen(datetime):
    """``datetime`` with ``now()`` fixed at ``NOW``."""

    @classmethod
    def now(cls, tz=None):
        return NOW if tz is None else NOW.astimezone(tz)


@contextlib.contextmanager
def frozen():
    """The clock at ``NOW`` and LM Studio's model list empty, for the calls of ``citar.servers`` and of the
    Servers-page machines (``citar.pool.seats``)."""
    with mock.patch("citar.servers.datetime", _Frozen), mock.patch("citar.pool.seats.datetime", _Frozen), \
            mock.patch("citar.server.lmstudio.models", lambda *a, **k: {}):
        yield


def jsonable(value):
    """A call's answer as plain JSON: datetimes as ISO text, the volatile keys of server dicts dropped."""
    if isinstance(value, datetime):
        return {"datetime": value.isoformat()}
    if isinstance(value, dict):
        return {str(k): jsonable(v) for k, v in value.items() if k not in VOLATILE}
    if isinstance(value, (list, tuple)):
        return [jsonable(v) for v in value]
    if isinstance(value, float):
        return round(value, 9)
    return value


def _call(fn, *args, **kwargs):
    """``fn``'s answer, or the exception it raised (its type and message)."""
    try:
        return jsonable(fn(*args, **kwargs))
    except Exception as e:      # an error is an answer too: "No server 'x'." must stay what it was
        return {"error": type(e).__name__, "message": str(e)}


FILE_IDS = ("sv_host", "sv_claude", "sv_lan", "sv_test")


def phase_a() -> dict:
    """The calls on the fixture alone."""
    from citar import servers as S
    out: dict = {}
    with frozen():
        for sid in FILE_IDS + ("sv_nope",):
            out[f"find:{sid}"] = _call(S.find, sid)
            out[f"get:{sid}"] = _call(S.get, sid)
        out["find:None"] = _call(S.find, None)
        out["list_servers"] = _call(S.list_servers)
        out["host"] = _call(S.host)
        host = S.get("sv_host")
        for ref in ("m_gemma", "google/gemma-4-e2b", "m_qwen", "nope", None):
            out[f"model_entry:sv_host:{ref}"] = _call(S.model_entry, host, ref)
        gemma = S.model_entry(host, "m_gemma")
        for pid in ("p_long", "p_default", "p_nope", None):
            out[f"profile:m_gemma:{pid}"] = _call(S.profile, gemma, pid)
        out["profile:None"] = _call(S.profile, None, "p_long")
        seats = {
            "host_long": {"server_id": "sv_host", "model_id": "m_gemma", "profile_id": "p_long",
                          "max_tool_calls_per_turn": 80, "persona": "a trader", "reasoning_effort": "high"},
            "host_by_key": {"server_id": "sv_host", "model": "qwen/qwen3.8-27b"},
            "host_unknown_model": {"server_id": "sv_host", "model": "not/in-catalog"},
            "claude": {"server_id": "sv_claude", "model_id": "m_opus", "effort": "high"},
            "claude_custom": {"server_id": "sv_claude", "model": "claude-custom-1", "tool_mode": "json"},
            "lan": {"server_id": "sv_lan", "model_id": "m_llama", "max_turn_seconds": 600},
            "dry": {"server_id": "sv_test", "model_id": "m_dry", "dry_run_delay": 0},
            "missing": {"server_id": "sv_nope", "model": "x"},
        }
        for name, seat in seats.items():
            out[f"resolve_llm:{name}"] = _call(S.resolve_llm, seat)
            out[f"resolve_llm_nokey:{name}"] = _call(S.resolve_llm, seat, with_key=False)
            out[f"describe_seat:{name}"] = _call(S.describe_seat, seat)
        out["resolve_llm:inline_admin"] = _call(S.resolve_llm, {"provider": "dryrun", "model": "x", "base_url": "u"})
        out["resolve_llm:inline_member"] = _call(S.resolve_llm, {"provider": "openai_compatible", "base_url": "u"},
                                                 inline_ok=False)
        out["seat_ref:host"] = _call(S.seat_ref, "sv_host", "m_gemma", "p_long", persona="p", effort=None)
        out["seat_ref:host_default_profile"] = _call(S.seat_ref, "sv_host", "google/gemma-4-e2b")
        out["seat_ref:claude"] = _call(S.seat_ref, "sv_claude", "claude-opus-5")
        out["seat_ref:bad_model"] = _call(S.seat_ref, "sv_host", "nope")
        out["seat_ref:bad_server"] = _call(S.seat_ref, "sv_nope", "m")
        for sid in FILE_IDS + ("sv_nope", None):
            out[f"server_name:{sid}"] = _call(S.server_name, sid)
            out[f"restricted_now:{sid}"] = _call(S.restricted_now, sid)
            out[f"restriction_config:{sid}"] = _call(S.restriction_config, sid)
        for when in (datetime(2026, 9, 26, 2, 10), datetime(2026, 9, 26, 3, 0), datetime(2026, 9, 24, 5, 59),
                     datetime(2026, 9, 25, 23, 30), datetime(2026, 9, 27, 0, 30)):
            out[f"restricted_at:sv_host:{when.isoformat()}"] = _call(S.restricted_at, "sv_host", when)
        end = S.restricted_now("sv_host")
        out["restricted_text:sv_host"] = _call(S.restricted_text, "sv_host", end)
        out["restricted_text:none"] = _call(S.restricted_text, "sv_host", None)
        out["restriction_status"] = _call(S.restriction_status)
        out["plan:ep_tou"] = _call(S.plan, "ep_tou")
        out["plan:None"] = _call(S.plan, None)
        reg = S.load()
        out["registry:settings"] = {k: reg.get(k) for k in ("currency", "currency_symbol", "host_server_id")}
        out["registry:electricity_plans"] = jsonable(reg["electricity_plans"])
        for sid in FILE_IDS:
            out[f"issues:{sid}"] = _call(S.issues, S.get(sid))
            out[f"model_price:{sid}"] = {m["key"]: _call(S.model_price, S.get(sid), m["key"], NOW)
                                         for m in S.get(sid)["models"]}
        out["costing"] = costing_totals()
    return out


def phase_b() -> dict:
    """The calls that read the helper machine too (``sv_helper``, a row of its owner's, reached through its
    helper)."""
    from citar import servers as S
    out: dict = {}
    seat = {"server_id": "sv_helper", "model": "mistral/small-4", "reconnect_seconds": 30, "max_tokens": 1000}
    with frozen():
        out["resolve_llm:helper"] = _call(S.resolve_llm, seat)
        out["resolve_llm:helper_no_model"] = _call(S.resolve_llm, {"server_id": "sv_helper"})
        out["describe_seat:helper"] = _call(S.describe_seat, seat)
        out["seat_ref:helper"] = _call(S.seat_ref, "sv_helper", "mistral/small-4", None, persona="p")
        out["server_name:helper"] = _call(S.server_name, "sv_helper")
        out["restricted_now:helper"] = _call(S.restricted_now, "sv_helper")
        out["restriction_config:helper"] = _call(S.restriction_config, "sv_helper")
        out["restricted_text:helper"] = _call(S.restricted_text, "sv_helper", S.restricted_now("sv_helper"))
        out["restriction_status"] = _call(S.restriction_status)
        out["costing"] = costing_totals()
    return out


#: 2026-03-10 12:00 and 2026-07-01 18:00 on this computer's clock: before and after the host's second cost period.
T_MARCH = datetime(2026, 3, 10, 12, 0).timestamp()
T_JULY = datetime(2026, 7, 1, 18, 0).timestamp()


def synthetic_ledger() -> dict:
    """A usage ledger over the fixture's machines: a game on the host and the LAN machine, a benchmark on the API,
    a report on the helper, in March and in July (two cost periods and both electricity plans)."""
    acts = {"g": {"id": "g", "kind": "game", "name": "game"}, "b": {"id": "b", "kind": "benchmark", "name": "bench"},
            "r": {"id": "r", "kind": "report", "name": "report"}}
    spans = []
    for t in (T_MARCH, T_JULY):
        spans += [
            {"act": "g", "srv": "sv_host", "t0": t, "t1": t + 3600, "held": 3600, "busy": 900, "cpu": 1200,
             "model": "google/gemma-4-e2b", "in": 50_000, "out": 4_000},
            {"act": "g", "srv": "sv_lan", "t0": t, "t1": t + 1800, "held": 1800, "busy": 1200, "cpu": 0,
             "model": "llama-4-scout", "in": 20_000, "out": 3_000},
            {"act": "b", "srv": "sv_claude", "t0": t, "t1": t + 600, "held": 600, "busy": 300, "model": "claude-opus-5",
             "in": 1_000_000, "out": 100_000, "cr": 500_000, "cw": 20_000},
            {"act": "b", "srv": "sv_claude", "t0": t, "t1": t + 60, "held": 60, "busy": 30, "model": "claude-custom-1",
             "in": 100_000, "out": 10_000},
            {"act": "b", "srv": "sv_host", "t0": t + 1800, "t1": t + 5400, "held": 3600, "cpu": 300, "busy": 0},
            {"act": "r", "srv": "sv_helper", "t0": t, "t1": t + 300, "held": 300, "busy": 200, "model": "mistral/small-4"},
        ]
    return {"acts": acts, "spans": spans, "power": {}}


def costing_totals() -> dict:
    """``costing.compute`` over the synthetic ledger, priced from the registry ``servers.with_pooled()`` gives: each
    activity's totals and each machine's calendar and totals."""
    from citar import costing
    r = costing.compute(T_MARCH - 3600, T_JULY + 7200, None, synthetic_ledger())
    return jsonable({"acts": {a: v["total"] for a, v in sorted(r["acts"].items())},
                     "servers": {s: {k: v.get(k) for k in ("calendar", "total", "utilization") if k in v}
                                 for s, v in sorted(r["servers"].items())},
                     "currency": r.get("currency"), "symbol": r.get("symbol")})


def load_expected() -> dict:
    """The recorded answers."""
    return json.loads(EXPECTED.read_text(encoding="utf-8"))

"""Phase 4's QA script: the CITAR helper and the server's side of protocol 2, end to end, with the evidence written
down (design/phase4-helper.md P4.7.6).

    python scripts/qa_phase4.py <part>|all [--port N] [--dir D] [--keep] [--helper-bin PATH] [--skip PART]
                                [--llm-url URL] [--stand-in]

It reuses ``scripts/qa_phase3.py``'s harness, imported rather than copied: its refusals (exit 2, touching nothing, for
the dev server's port 8799, a port in use, a directory holding anything without the harness's marker, the owner's
``C:/dev/citar-play``, CITAR's own data directory and a source checkout), the dev server child on the directory, the
accounts (an administrator, two members and a moderator), the evidence (``<dir>/qa-<part>.json``), the scripted
endpoint (streamed as server-sent events, recording when its client went) and its ``Helper``: the real Rust binary
(``--helper-bin``, else the ``citar-helper`` ``cargo xtask develop`` built into ``CITAR_EXT_DIR``), each started with
``--home <dir>/helper-<n>`` (and ``CITAR_DATA_DIR`` with it) and dialling only the harness's server on loopback.

Parts:

- ``helper`` (4-05): bob's helper machine with the real helper: protocol 2 on its card and its machine key's
  fingerprint the one the binary prints; settings acknowledged with its own ceiling winning, and a change pushed at
  once; a lobby game whose model seat plays through it (streamed completions, tool calls parsed on the server); the
  Servers page's test given up and its generation stopped at the endpoint, the slot freed with nothing draining; a
  rescan asked for; a 0.1.5 worker's hello refused with the upgrade message and 4426, logged once; another key refused
  until the owner resets it; a reset while a helper is connected making it reconnect and register its key at once
  (close 1012); 20 refused tokens from an address and a good one then refused before any lookup (from
  ``127.0.0.2``, so the run's own helpers on ``127.0.0.1`` are not held up); a revoked token ending the helper with
  exit code 78.
- ``llm-real`` (4-05): 4-04's real-model checks through the hub and the scheduler, in a hub of its own in this process
  (its database in ``<dir>/llm-real``): LM Studio (``--llm-url``, ``http://localhost:1234/v1``) lists
  ``google/gemma-4-e2b`` loaded before and after (only its model list is read: the part never loads, unloads or swaps a
  model or changes LM Studio's settings); the real helper in front of it answers a request with tools with a raw
  message whose tool call (or text) the server parses; a 1,500-token generation cancelled at 2 s is acknowledged
  within a second, freeing its slot with nothing draining; three short requests after it take no longer than 1.5
  times the median of three before it plus a second. ``--stand-in`` plays LM Studio with the scripted endpoint (the
  same model name, loaded; streaming; one model generating one answer at a time; a cancelled generation stopped), to
  check the part itself without the model. The checks are one function (``exercise``), and
  ``tests/test_helper_llm_real.py`` runs it in every test run against stand-ins: it passes against one that stops a
  cancelled generation, and fails against one that runs on after the cancel, against a model that is not loaded (with
  nothing sent to it) and against one unloaded during the run.

- ``scan`` (4-06): bob's helper machine with the real helper scanning and measuring, ``citar-fake-probe`` (built beside
  the helper by ``cargo xtask develop``) named as its ``nvidia-smi`` by ``CITAR_HELPER_NVIDIA_SMI``, so the machine's
  real ``nvidia-smi`` is never run and every GPU figure is the fake's: its hello's scan is the machine's last report
  (in the collector's format, the fake's GPU first); the dialog's Rescan now brings a ``hardware`` frame within 5 s; a
  minute of 10-second samples lands as one power row of the machine, every sample the fake's draw; costing (run on the
  instance's own data, in a child with its environment) prices an activity holding the machine over that minute from
  the row, measured rather than estimated; every reading is a call in the fake's log and nothing else called it; and
  a second machine's helper with ``--no-hardware --no-power`` reports no scan and writes no row. With ``--keep`` the
  scanned machine's helper stays connected, for the browser checklist's H2 (the dialog's Rescan now).

``all`` runs every part written so far; later packages add ``lab`` (4-09), ``remote`` (4-10), ``verify`` (4-11,
4-12), ``update`` (4-13), ``service`` (4-14), ``exit`` and ``real`` (4-15).
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import statistics
import sys
import threading
import time
from pathlib import Path
from typing import Callable, Optional

ROOT = Path(__file__).resolve().parent.parent


def _phase3():
    spec = importlib.util.spec_from_file_location("qa_phase3_harness", ROOT / "scripts" / "qa_phase3.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


q3 = _phase3()
Harness, Helper, FakeEndpoint, wait_until = q3.Harness, q3.Helper, q3.FakeEndpoint, q3.wait_until

#: Parts a later package writes (P4.7.6): asked for now, the harness says so and refuses.
LATER = {"lab": "4-09", "remote": "4-10", "verify": "4-11, 4-12", "update": "4-13", "service": "4-14",
         "exit": "4-15", "real": "4-15"}
#: The v1 refusal's words (``citar.server.helper_protocol.V1_REFUSAL_MESSAGE``), as a 0.1.5 worker prints them.
V1_START = "This server runs CITAR 0.1.6, which needs the new CITAR helper (protocol 2)."
#: A protocol 1 hello, as a 0.1.5 worker sent it.
V015_HELLO = {"t": "hello", "version": 1, "token": "cwt_qa_0.1.5", "hostname": "qa-old-box", "provider": "lmstudio",
              "models": [], "max_concurrent": 1, "worker_version": "0.1.5"}
#: Where the rate-limit check connects from: another loopback address, so the limit it trips holds up none of the
#: run's own helpers (127.0.0.1).
RATE_ADDRESS = "127.0.0.2"


# ---------------------------------------------------------------------------- small helpers

def _ws(h, *, source: Optional[str] = None):
    """A raw WebSocket to the instance's ``/ws/worker`` (``websockets``' synchronous client), from ``source``."""
    import inspect

    from websockets.sync.client import connect
    options = {"open_timeout": 15, "close_timeout": 2, "ping_interval": None}
    known = inspect.signature(connect).parameters
    if "proxy" in known:
        options["proxy"] = None
    if "legacy" in known:
        options["legacy"] = True
    if source:
        options["source_address"] = (source, 0)
    return connect(f"ws://127.0.0.1:{h.server.port}/ws/worker", **options)


def _say(ws, frame: dict) -> tuple:
    """Send one frame; return (the frame the hub answered, (close code, reason)), or what was missing."""
    from websockets.exceptions import ConnectionClosed
    ws.send(json.dumps(frame))
    first, closed = None, (None, "")
    try:
        first = json.loads(ws.recv(timeout=10))
        ws.recv(timeout=10)
    except ConnectionClosed as exc:
        closed = (exc.rcvd.code, exc.rcvd.reason) if exc.rcvd is not None else (None, "")
    except TimeoutError:
        pass
    return first, closed


def _hello(token: str, key: str) -> dict:
    """A protocol 2 hello with ``token`` and machine key ``key`` (base64 of 32 bytes)."""
    return {"t": "hello", "protocol": 2, "token": token,
            "helper": {"version": "0.1.6", "build": "0" * 12, "jobs_build": None, "target": "qa", "label": "qa"},
            "machine": {"hostname": "qa", "name": "qa", "os": "windows", "arch": "x86_64", "public_key": key},
            "llm": {"enabled": True, "provider": "lmstudio", "slots": 1, "stream": True, "models": []},
            "cpu": {"enabled": False, "ceiling": {"threads": 1, "cpu_percent": 10, "memory_mb": 512, "work": []},
                    "limits": {"cpu": "soft", "memory": "soft"}, "verifiable": False, "bots": [],
                    "ruleset": "0" * 64},
            "report": {"hardware": None, "hardware_source": "none"}, "running": [], "results": [],
            "updates": "off", "quiet_until": None}


def _fingerprint(helper) -> str:
    """The machine key's fingerprint ``citar-helper --version --home`` prints for a helper's home."""
    import re
    import subprocess
    out = subprocess.run([q3.helper_binary(), "--version", "--home", str(helper.home)], capture_output=True,
                         text=True, timeout=30).stdout
    found = re.search(r"machine key ([0-9a-f]{16})", out)
    return found.group(1) if found else ""


def _workers(who, hid: str) -> dict:
    got = who.get(f"/api/pool/servers/{hid}/workers")
    return got.json() if got.status_code == 200 else {}


def _exit_code(helper, timeout: float) -> Optional[int]:
    try:
        return helper.proc.wait(timeout=timeout)
    except Exception:
        return None


# ---------------------------------------------------------------------------- the helper part (4-05)

def part_helper(h) -> None:
    """The ``helper`` part (the module's doc)."""
    r = h.report
    admin, bob = h.accounts["admin"], h.accounts["bob"]
    data = r.data.setdefault("helper", {})
    scripted = FakeEndpoint(delay=0.3)
    scripted.slow = {"Reply with exactly: CITAR OK": 8.0}
    helpers: list = []
    gid = ""
    try:
        made = bob.post("/api/pool/servers", json={"config": {"name": f"QA helper box {h.tag}", "kind": "owned",
                                                               "connection": {"provider": "openai_compatible",
                                                                              "max_parallel": 2}},
                                                    "reach": "worker"})
        hid = made.json().get("id", "") if made.status_code == 200 else ""
        issued = bob.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa helper"}) if hid else None
        token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
        first = Helper(h, token, scripted, "qa-helper-box", max_concurrent=1) if token else None
        if first is not None:
            helpers.append(first)
        online = bool(first) and bool(wait_until(lambda: (_workers(bob, hid).get("worker") or {}).get("effective"),
                                                 60, 0.25))
        info = _workers(bob, hid)
        worker = info.get("worker") or {}
        printed = _fingerprint(first) if first else ""
        r.check("bob's helper machine with the real CITAR helper: protocol 2 on its card, the helper's version and "
                "build, and its machine key's fingerprint the one the binary prints for its home",
                online and worker.get("protocol") == 2 and bool(worker.get("version")) and len(printed) == 16
                and worker.get("key_fingerprint") == printed == (info.get("key") or {}).get("fingerprint")
                and info.get("command", "").startswith("citar-helper "),
                json.dumps({"online": online, "protocol": worker.get("protocol"), "version": worker.get("version"),
                            "build": worker.get("build"), "card": worker.get("key_fingerprint"),
                            "printed": printed})[:400])
        data.update(machine=hid, fingerprint=printed, version=worker.get("version"))

        # ---- settings: its own ceiling wins, and a change is pushed at once
        effective = (worker.get("effective") or {}).get("llm") or {}
        # the scheduler's own view, as the machine settings dialog's "in effect" reads it (made on first read)
        got = bob.get(f"/api/machines/{hid}/settings")
        in_effect = (got.json().get("in_effect") or {}) if got.status_code == 200 else {}
        r.check("the site's 2 slots were acknowledged as the helper's own 1 (its --max-concurrent), capped named, and "
                "the scheduler's capacity is 1",
                effective.get("slots") == 1 and "llm.slots" in (worker.get("capped") or [])
                and in_effect.get("capacity") == 1 and in_effect.get("helper_slots") == 1,
                json.dumps({"effective": effective, "capped": worker.get("capped"), "in_effect": in_effect}))
        changed = bob.put(f"/api/machines/{hid}/settings", json={"slots": 1, "capabilities": {"llm": {
            "allow_swaps": True}}})
        t_change = time.perf_counter()
        pushed = wait_until(lambda: "llm.slots" not in ((_workers(bob, hid).get("worker") or {}).get("capped") or [])
                            and time.perf_counter(), 5, 0.05)
        r.check("a change in the machine settings is pushed to the helper at once and acknowledged (the site's 1 slot "
                "now within its ceiling: nothing capped)",
                changed.status_code == 200 and bool(pushed) and pushed - t_change < 2.0,
                f"HTTP {changed.status_code}; acknowledged after "
                f"{round(pushed - t_change, 3) if pushed else None} s")

        # ---- a lobby game's model seat plays through it, the completions parsed on the server
        before = len(scripted.completions())
        g = bob.post("/api/games", json={"name": f"QA helper game {h.tag}", "config": {**q3.CONFIG, "seed": 4051},
                                         "seats": [{"type": "llm", "llm": {"server_id": hid, "model": scripted.MODEL}},
                                                   {"type": "bot"}]})
        gid = g.json().get("id", "") if g.status_code == 200 else ""

        def turn() -> int:
            got = bob.get(f"/api/games/{gid}")
            return (got.json().get("turn") or 0) if got.status_code == 200 else 0
        played = bool(gid) and wait_until(lambda: turn() >= 3, 180, 1.0)
        bob.post(f"/api/games/{gid}/control", json={"paused": True})
        time.sleep(1.0)
        mine = [q for q in scripted.completions()[before:] if q.get("kind") == "turn"]
        metrics = bob.get(f"/api/games/{gid}/metrics").json() if gid else {}
        rows = [x for x in metrics.get("turns") or [] if x.get("controller") == "llm" and x.get("ended")]
        r.check("bob's lobby game: its model seat played two turns through the helper, every completion streamed from "
                "the endpoint, its tool calls parsed on the server and each turn ended by end_turn",
                bool(played) and len(mine) >= 4 and all(q.get("stream") for q in mine)
                and [x.get("end_reason") for x in rows[:2]] == ["end_turn", "end_turn"],
                json.dumps({"completions": len(mine), "streamed": sum(1 for q in mine if q.get("stream")),
                            "ends": [x.get("end_reason") for x in rows]})[:400])
        bob.delete(f"/api/games/{gid}")
        gid = ""

        # ---- the Servers page's test given up: the generation stopped, the slot freed
        before = len(scripted.completions())
        t0 = time.time()
        test = bob.post(f"/api/pool/servers/{hid}/test", params={"wait": 2})
        given_up = time.time()
        body = test.json() if test.status_code == 200 else {}
        late = wait_until(lambda: next((q for q in scripted.completions()[before:] if q.get("delay") == 8.0), None),
                          10, 0.05)
        status = wait_until(lambda: (lambda m: m if m.get("in_flight") == 0 else None)(
            q3._machine_status(admin, hid)), 5, 0.05) or {}
        counters = status.get("counters") or {}
        r.check("the Servers page's test given up after 2 s: cancelled, the endpoint saw its client go within a second "
                "(the generation stopped, long before its 8 s), the slot free with nothing draining or unacknowledged",
                body.get("cancelled") is True and late is not None and late.get("gone") is not None
                and late["gone"] - given_up < 1.0 and late["seconds"] < 8.0 and status.get("draining") == 0
                and counters.get("draining", 0) == 0 and counters.get("cancel_unacked", 0) == 0
                and counters.get("late_frames", 0) >= 1,
                json.dumps({"test": body, "gave_up_s": round(given_up - t0, 2),
                            "gone_after_s": round(late["gone"] - given_up, 3) if late and late.get("gone") else None,
                            "counters": counters})[:500])

        # ---- a rescan asked for
        rescan = bob.post(f"/api/machines/{hid}/rescan")
        r.check("the dialog's Rescan asks the connected helper (200); a stranger is told no machine is there (404)",
                rescan.status_code == 200
                and h.accounts["alice"].post(f"/api/machines/{hid}/rescan").status_code == 404,
                f"HTTP {rescan.status_code}")

        # ---- a 0.1.5 worker
        refusals = []
        for _ in range(5):
            with _ws(h) as ws:
                refusals.append(_say(ws, V015_HELLO))
        frame, (code, _reason) = refusals[0]
        log = h.server.log.read_text(encoding="utf-8", errors="replace") if h.server.log.exists() else ""
        lines = [x for x in log.splitlines() if "refused a protocol 1" in x]
        r.check("a 0.1.5 worker's hello gets the one-line upgrade message in protocol 1's error frame and close 4426; "
                "five of them log one line",
                isinstance(frame, dict) and frame.get("t") == "error" and str(frame.get("message", "")).startswith(
                    V1_START) and code == 4426 and all(c == 4426 for _f, (c, _r) in refusals) and len(lines) == 1,
                json.dumps({"frame": frame, "close": code, "log_lines": len(lines)})[:400])

        # ---- the machine key: another helper's is refused until the owner resets it
        first.stop()
        wait_until(lambda: not q3._online(bob, hid), 10, 0.1)
        second = Helper(h, token, scripted, "qa-helper-box-2", max_concurrent=1)
        helpers.append(second)
        refused = _exit_code(second, 30)
        said = second.text()
        reset = bob.post(f"/api/machines/{hid}/key/reset")
        third = Helper(h, token, scripted, "qa-helper-box-3", max_concurrent=1)
        helpers.append(third)
        back = wait_until(lambda: (_workers(bob, hid).get("worker") or {}).get("effective"), 60, 0.25)
        info = _workers(bob, hid)
        printed3 = _fingerprint(third)
        audit = admin.get("/api/admin/audit", params={"action": "server.key_reset"}).json().get("entries", [])
        r.check("a helper with another machine key is refused (refused key: it exits 78, saying to reset the key); "
                "after the owner's reset the next one registers its own key, the card showing its fingerprint, with "
                "one audit row",
                refused == 78 and "reset" in said.lower() and reset.status_code == 200 and bool(back)
                and (info.get("key") or {}).get("fingerprint") == printed3 != printed
                and len([a for a in audit if a.get("object_id") == hid]) == 1,
                json.dumps({"refused_exit": refused, "reset": reset.status_code, "new": printed3, "old": printed,
                            "audit": len(audit)})[:400])

        # ---- a reset while the helper is connected: closed with 1012, it reconnects and registers its key at once
        connected = (_workers(bob, hid).get("worker") or {}).get("connected_at")
        t_reset = time.perf_counter()
        again = bob.post(f"/api/machines/{hid}/key/reset")

        def rekeyed() -> bool:
            info = _workers(bob, hid)
            worker = info.get("worker") or {}
            return ((info.get("key") or {}).get("fingerprint") == printed3 and bool(worker.get("effective"))
                    and worker.get("connected_at") != connected)
        rekey = wait_until(rekeyed, 30, 0.1)
        took = round(time.perf_counter() - t_reset, 2)
        r.check("a reset while its helper is connected closes the connection (1012, no shutdown): the helper, still "
                "running, reconnects within its backoff and registers its key again at once",
                again.status_code == 200 and bool(rekey) and third.proc.poll() is None and "1012" in third.text(),
                f"its key registered again {took} s after the reset", seconds=took)

        # ---- the address's limit, before any lookup (from 127.0.0.2: the run's own helpers are on 127.0.0.1)
        codes = []
        for i in range(20):
            with _ws(h, source=RATE_ADDRESS) as ws:
                f, _c = _say(ws, _hello(f"cwt_qa_wrong_{i}_{h.tag}", "A" * 43 + "="))
                codes.append((f or {}).get("code"))
        with _ws(h, source=RATE_ADDRESS) as ws:
            f, (code, _r) = _say(ws, _hello(token, "A" * 43 + "="))
        r.check("20 different refused tokens from one address, then that address's good token is refused rate (4429) "
                "before any lookup, saying when to try again",
                codes == ["token"] * 20 and isinstance(f, dict) and f.get("code") == "rate" and code == 4429
                and (f.get("retry_after_s") or 0) > 0,
                json.dumps({"codes": sorted(set(map(str, codes))), "last": f, "close": code})[:300])

        # ---- a revoked token ends the helper
        tokens = bob.get(f"/api/pool/servers/{hid}/workers").json().get("tokens", [])
        live = next((t["id"] for t in tokens if not t["revoked"]), "")
        t_revoke = time.perf_counter()
        revoked = bob.delete(f"/api/pool/servers/{hid}/workers/tokens/{live}")
        ended = _exit_code(third, 15)
        took = round(time.perf_counter() - t_revoke, 2)
        r.check("revoking its token drops the helper and it exits 78 (a permanent refusal), without reconnecting",
                revoked.status_code == 200 and ended == 78 and not q3._online(bob, hid),
                f"exit {ended} {took} s after the revoke", seconds=took)
        data.update(counters=counters, test=body)
    finally:
        if gid:
            bob.delete(f"/api/games/{gid}")
        for helper in helpers:
            helper.stop()
        scripted.close()


# ---------------------------------------------------------------------------- the llm-real part (4-05)

REAL_URL, REAL_MODEL = q3.REAL_URL, q3.REAL_MODEL
#: The part's options (``--llm-url``, ``--stand-in``), set by ``main``.
LLM_URL = REAL_URL
STAND_IN = False
#: The long generation, cancelled ``CANCEL_AT`` seconds after it is asked for; the stand-in knows it by ``LONG_KEY``.
LONG_PROMPT = "Write a very long essay, at least 1,500 words, about the history of maps."
LONG_KEY = "Write a very long essay"
CANCEL_AT = 2.0


class _Ended(Exception):
    """A check failed whose failure leaves the later checks meaningless: the run ends there."""


def stand_in(*, loaded: bool = True, runs_on: bool = False, long_s: float = 40.0, long_words: int = 1500):
    """LM Studio as ``llm-real`` meets it, played by the scripted endpoint (``--stand-in``, and the tests of the checks
    themselves, ``tests/test_helper_llm_real.py``): ``REAL_MODEL`` listed by LM Studio's own ``/api/v0/models``, loaded
    or not; streamed answers on connections kept open; one model generating one answer at a time (``runs_on``: a
    generation whose client went keeps it busy to its end, as a server that does not stop it); each short answer in
    0.2 s, and the long generation's ``long_words`` words over ``long_s`` seconds, well past the cancel."""
    ep = FakeEndpoint(delay=0.2)
    ep.MODEL = REAL_MODEL
    ep.models = [(REAL_MODEL, loaded)]
    ep.one_model(runs_on)
    ep.slow = {LONG_KEY: long_s}
    ep.lengthy = {LONG_KEY: long_words}
    return ep


def endpoint_side(ep, cancel_at: Optional[float]) -> dict:
    """What a stand-in saw of a run: its completions (how many, how many streamed, the tools the first offered) and,
    of the long generation, its events in all, those after the cancel (``cancel_at``, ``time.time()``), how long after
    the cancel its client went, the tokens it sent and how long the model ran on."""
    served = ep.completions()
    side = {"completions": len(served), "streamed": sum(1 for r in served if r.get("stream")),
            "first_tools": served[0].get("tools") if served else None}
    long = next((r for r in served if str(r.get("head") or "").startswith(LONG_KEY)), None)
    if long is not None:
        events = list(long.get("events") or [])
        gone = long.get("gone")
        side.update(long_events=len(events), long_tokens=long.get("tokens"),
                    events_after_cancel=sum(1 for t in events if cancel_at is not None and t > cancel_at),
                    gone_after_cancel_s=round(gone - cancel_at, 3) if gone and cancel_at is not None else None,
                    ran_on_s=long.get("ran_on_s"))
    return side


def part_llm_real(h) -> None:
    """The ``llm-real`` part (the module's doc): a hub of its own in this process, its database in ``<dir>/llm-real``,
    the real helper in front of LM Studio (or ``--stand-in``'s), checked through the scheduler (``exercise``)."""
    data = h.report.data.setdefault("llm_real", {})
    ep = stand_in() if STAND_IN else None
    url = ep.url if ep is not None else LLM_URL
    data["endpoint"] = {"url": url, "model": REAL_MODEL, "stand_in": bool(STAND_IN)}
    try:
        _own_instance(h.server.state / "llm-real")
        exercise(h.report.check, url, h.server.state, data, label="the stand-in" if STAND_IN else "LM Studio")
    finally:
        if ep is not None:
            data["stand_in_side"] = endpoint_side(ep, data.get("cancel_at"))
            ep.close()


def _own_instance(home: Path) -> None:
    """This process's own instance for the part, in ``home``: its variables set before anything of citar is imported
    here, then its database made."""
    home.mkdir(exist_ok=True)
    os.environ.update({"CITAR_DATA_DIR": str(home), "CITAR_DB_URL": "sqlite:///" + (home / "hub.db").as_posix(),
                       "CITAR_SAVE_DIR": str(home / "saves"), "CITAR_CONFIG_DIR": str(home / "config"),
                       "CITAR_MODE": "local", "CITAR_KEYS_FILE": str(home / "config" / "keys.enc")})
    if str(ROOT) not in sys.path:
        sys.path.insert(0, str(ROOT))
    from citar import db, settings
    from citar.db.models import Base
    settings.reset()
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.create_all(db.engine())


def exercise(check: Callable, url: str, state: Path, data: dict, *, label: str = "LM Studio",
             ack_bound: float = 1.0) -> None:
    """``llm-real``'s checks (the module's doc: 4-04 gate 4's, through the hub and the scheduler) against ``label`` at
    ``url``, with the database this process has configured: a hub of its own on loopback, and the real helper (its
    home ``<state>/helper-llm-real``) in front of the endpoint. Each check goes to ``check(name, ok, detail,
    **numbers)`` (the harness's ``Report.check``), and what the run measured to ``data``.

    Nothing is sent to a model that is not loaded: the first check ends the run there, before a helper starts. A
    failure that leaves the later checks meaningless (the helper not connecting, a request failing) ends it at that
    check, and the model's state is read again after the run whatever happened in it. ``ack_bound`` is the seconds
    the cancel's acknowledgement may take: the gate's 1 (the tests give CI three times it, ``p3-notes.md``)."""
    ready, said = q3.lmstudio_model(url, REAL_MODEL)
    data["before"] = said
    if not check(f"{label} at {url} lists {REAL_MODEL}, loaded (read only: GET /api/v0/models; the part never loads, "
                 "unloads or swaps a model)", ready, said):
        return
    try:
        _through_the_hub(check, url, state, data, ack_bound)
    except _Ended:
        pass
    finally:
        after, said_after = q3.lmstudio_model(url, REAL_MODEL)
        data["after"] = said_after
        check(f"{REAL_MODEL} is still loaded after the part", after, said_after)


def _through_the_hub(check: Callable, url: str, state: Path, data: dict, ack_bound: float) -> None:
    from citar import db, servers
    from citar.agents.providers import make_conversation
    from citar.auth import tokens
    from citar.db.models import Server, WorkerToken
    from citar.pool import scheduler as S
    from citar.server import workers as W
    from tests import helperfake as HF

    def must(name: str, what: str, fn: Callable):
        """``fn()``; if it raises, the check ``name`` fails saying ``what`` failed, and the run ends."""
        try:
            return fn()
        except Exception as exc:
            check(name, False, f"{what} failed: {type(exc).__name__}: {exc}"[:400])
            raise _Ended from exc

    hub_server = HF.HubServer()
    helper = None
    try:
        with db.session() as s:
            row = Server(owner_id=None, name="QA llm-real", kind="owned", provider="lmstudio", reach="worker",
                         max_concurrent=1, config={"models": [{"key": REAL_MODEL}]})
            s.add(row)
            s.flush()
            raw, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=row.id, token_hash=hashed, prefix=tokens.prefix(raw), label="qa llm-real"))
            sid = row.id
        servers.invalidate()
        S.reset()
        helper = HF.RealHelper(hub_server.base, raw, url, name="qa-llm-real", slots=1,
                               extra=("--provider", "lmstudio", "--model", REAL_MODEL),
                               home=state / "helper-llm-real")
        online = wait_until(lambda: W.hub().get(sid) is not None and W.hub().get(sid).ack is not None, 60, 0.25)
        models = {m.get("key"): m.get("loaded") for m in (W.hub().get(sid).models if online else [])}
        if not check(f"the real helper in front of it connected to this hub and reports {REAL_MODEL} loaded (its "
                     "allowlist)", bool(online) and models == {REAL_MODEL: True},
                     json.dumps({"models": models})[:300] + ("" if online else "; its log ends: "
                                                            + helper.text()[-600:])):
            return
        sched = S.get()
        m = sched.machine(sid)
        tools = [{"name": "end_turn", "description": "End your turn.",
                  "input_schema": {"type": "object", "properties": {"note": {"type": "string"}}}},
                 {"name": "get_empire", "description": "Your empire's state.",
                  "input_schema": {"type": "object", "properties": {}}}]

        def conversation(binding, text: str, **cfg):
            conv = make_conversation({"provider": "worker", "server_id": sid, "model": REAL_MODEL, "timeout": 120,
                                      **cfg}, "You are playing a strategy game. Use the tools.", tools,
                                     binding=binding)
            conv.add_user_text(text)
            return conv
        lease = sched.lease(sid, REAL_MODEL, kind="game", label="qa llm-real")
        try:
            name = "a request with tools comes back as a raw message the server parses: a tool call (or text)"
            step = must(name, "the request with tools",
                        lambda: conversation(lease, "Call the end_turn tool now.", max_tokens=256).step())
            names = [c.name for c in step.tool_calls]
            data["tool_call"] = {"names": names, "text": (step.text or "")[:200]}
            check(name, bool(names) or bool((step.text or "").strip()),
                  json.dumps({"tool_calls": names, "text": (step.text or "")[:120]}))

            def short() -> float:
                t0 = time.perf_counter()
                conversation(lease, "Reply with exactly: OK", max_tokens=16).step()
                return time.perf_counter() - t0
            bound_name = ("three short requests after the cancel take no longer than 1.5 times the median of three "
                          "before it plus a second: the model stopped the cancelled generation")
            before = must(bound_name, "a short request before the cancel", lambda: [short() for _ in range(3)])
            ticket = sched.ticket(sid, REAL_MODEL, cls="test", label="qa long generation")
            conv = conversation(ticket, LONG_PROMPT, max_tokens=1500)
            late0 = m.counters["late_frames"]
            out: dict = {}

            def long():
                try:
                    out["result"] = conv.step()
                except BaseException as exc:
                    out["error"] = exc
            th = threading.Thread(target=long, daemon=True)
            th.start()
            time.sleep(CANCEL_AT)
            # a generation that ended before the cancel tests no cancel: the check fails without waiting for one
            running = th.is_alive()
            t_cancel = time.perf_counter()
            data["cancel_at"] = time.time()
            ticket.cancel()
            acked = wait_until(lambda: m.counters["late_frames"] > late0 and m.slots == [] and time.perf_counter(),
                               10, 0.01) if running else None
            th.join(10)
            ack_s = round(acked - t_cancel, 3) if acked else None
            ended = ("cancelled" if "error" in out else "answered before the cancel" if "result" in out
                     else "still running")
            check(f"a 1,500-token generation cancelled at {CANCEL_AT:g} s is acknowledged within {ack_bound:g} s: the "
                  "helper's cancelled frees its slot, nothing drains",
                  ended == "cancelled" and acked is not None and acked - t_cancel < ack_bound
                  and m.counters["draining"] == 0 and m.counters.get("cancel_unacked", 0) == 0,
                  json.dumps({"ack_s": ack_s, "the generation": ended, "counters": dict(m.counters)})[:400],
                  ack_s=ack_s)
            after = must(bound_name, "a short request after the cancel", lambda: [short() for _ in range(3)])
            bound = 1.5 * statistics.median(before) + 1.0
            check(bound_name, all(x <= bound for x in after),
                  json.dumps({"before": [round(x, 3) for x in before], "after": [round(x, 3) for x in after],
                              "bound": round(bound, 3)}))
            data.update(before_s=[round(x, 3) for x in before], after_s=[round(x, 3) for x in after], ack_s=ack_s,
                        bound_s=round(bound, 3))
        finally:
            lease.release()
    finally:
        if helper is not None:
            helper.stop()
        hub_server.stop()
        S.reset()


# ---------------------------------------------------------------------------- the scan part (4-06)

#: The fake probe's answers (``crates/citar-helper/src/bin/fake_probe.rs``).
FAKE_GPU, FAKE_WATTS = "CITAR Fake GPU", 123.45
#: Costing on the instance's own data (run in a child with the instance's environment, so it reads the server's
#: database, registry and usage ledger): an activity holding the machine ``sid`` over its first power row's window,
#: priced. Prints ``{"row", "act", "metered"}`` as JSON.
COSTING = """
import json, sys
from citar import costing, usage
sid = sys.argv[1]
ledger = usage.read(None, None)
rows = sorted(ledger["power"].get(sid, []), key=lambda r: r["t0"])
if not rows:
    print(json.dumps({"row": None}))
    raise SystemExit
row = rows[0]
since, until = row["t0"] - 1, row["t1"] + 1
ledger = usage.read(since, until)
ledger["spans"].append({"act": "qa:scan", "srv": sid, "t0": row["t0"], "t1": row["t1"],
                        "held": row["t1"] - row["t0"], "busy": 30.0})
r = costing.compute(since, until, ledger=ledger)
print(json.dumps({"row": row, "act": r["acts"]["qa:scan"]["servers"][sid],
                  "metered": r["servers"][sid].get("metered")}, default=str))
"""


def fake_probe() -> str:
    """``citar-fake-probe`` beside the helper binary (``cargo xtask develop`` builds both into ``CITAR_EXT_DIR``)."""
    helper = Path(q3.helper_binary())
    for name in ("citar-fake-probe.exe", "citar-fake-probe"):
        if (helper.parent / name).is_file():
            return str(helper.parent / name)
    raise q3.HarnessError(f"no citar-fake-probe beside {helper}: run cargo xtask develop, which builds it")


def _rows(h, sid: str) -> list:
    """The machine's power rows in the instance's usage ledger (``<dir>/saves/usage/power-<sid>-YYYY-MM.jsonl``)."""
    out = []
    for path in sorted((h.server.state / "saves" / "usage").glob(f"power-{sid}-*.jsonl")):
        out += [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    return out


def part_scan(h) -> None:
    """The ``scan`` part (the module's doc)."""
    import subprocess
    r = h.report
    bob = h.accounts["bob"]
    data = r.data.setdefault("scan", {})
    scripted = FakeEndpoint(delay=0.05)
    probe_log = h.server.state / "fake-probe.log"
    helpers: list = []

    def machine(name: str) -> tuple:
        made = bob.post("/api/pool/servers", json={"config": {
            "name": f"{name} {h.tag}", "kind": "owned",
            "connection": {"provider": "openai_compatible", "max_parallel": 1},
            "power": {"idle_w": 20, "cpu_max_w": 60, "gpu_max_w": 200}}, "reach": "worker"})
        sid = made.json().get("id", "") if made.status_code == 200 else ""
        issued = bob.post(f"/api/pool/servers/{sid}/workers/tokens", json={"label": "qa scan"}) if sid else None
        token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
        return sid, token

    def report(sid: str) -> dict:
        got = bob.get(f"/api/machines/{sid}/settings")
        return (got.json().get("report") or {}) if got.status_code == 200 else {}

    try:
        fake = fake_probe()
        sid, token = machine("QA scanned box")
        quiet_sid, quiet_token = machine("QA unscanned box")
        if not (token and quiet_token):
            raise q3.HarnessError("bob's two helper machines and their tokens could not be made")
        scanned = Helper(h, token, scripted, "qa-scanned-box", scan=True, power=True,
                         env={"CITAR_HELPER_NVIDIA_SMI": fake, "CITAR_FAKE_PROBE_LOG": str(probe_log)})
        helpers.append(scanned)
        unscanned = Helper(h, quiet_token, scripted, "qa-unscanned-box")
        helpers.append(unscanned)
        online = wait_until(lambda: (_workers(bob, sid).get("worker") or {}).get("effective")
                            and (_workers(bob, quiet_sid).get("worker") or {}).get("effective")
                            and time.perf_counter(), 90, 0.25)
        welcomed = online or time.perf_counter()

        # ---- the hello's scan is the machine's last report
        got = wait_until(lambda: (lambda x: x if x.get("hardware") else None)(report(sid)), 15, 0.25) or {}
        hw = got.get("hardware") or {}
        gpus = hw.get("gpus") or [{}]
        r.check("the hello's scan is bob's machine's last report: the collector's format (citar-helper, version 2), "
                "the fake's GPU first, a power guess, and the dialog's summary of it",
                bool(online) and hw.get("format") == "citar-hardware" and hw.get("collector") == "citar-helper"
                and hw.get("collector_version") == 2 and gpus[0].get("name") == FAKE_GPU
                and set((hw.get("power") or {}).get("suggested") or {}) >= {"idle_w", "cpu_max_w", "gpu_max_w"}
                and FAKE_GPU in (got.get("hardware_summary") or ""),
                json.dumps({"collector": hw.get("collector"), "gpus": [g.get("name") for g in gpus],
                            "cpu": (hw.get("cpu") or {}).get("model"), "summary": got.get("hardware_summary"),
                            "suggested": (hw.get("power") or {}).get("suggested")})[:600])
        data.update(machine=sid, cpu=(hw.get("cpu") or {}).get("model"), summary=got.get("hardware_summary"))

        # ---- Rescan now: a hardware frame within 5 s
        before = got.get("hardware_at")
        t0 = time.perf_counter()
        asked = bob.post(f"/api/machines/{sid}/rescan")
        done = wait_until(lambda: report(sid).get("hardware_at") not in (None, before) and time.perf_counter(),
                          15, 0.05)
        took = round(done - t0, 2) if done else None
        r.check("Rescan now (the dialog's route) brings the helper's hardware frame within 5 s: the last report's time "
                "moves",
                asked.status_code == 200 and took is not None and took < 5.0,
                f"HTTP {asked.status_code}; the new report {took} s after the rescan", seconds=took)

        # ---- a minute of samples: one power row of the machine
        row_seen = wait_until(lambda: _rows(h, sid) and time.perf_counter(), 90, 0.5)
        rows = _rows(h, sid)
        row = rows[0] if rows else {}
        after = round(row_seen - welcomed, 1) if row_seen else None
        r.check("a minute of 10-second samples lands as one power row of the machine (6 samples, every GPU reading "
                "the fake's 123.45 W, the CPU's utilisation a fraction), placed by the server's clock over a minute",
                len(rows) == 1 and row.get("n") == 6 and row.get("gpu_w") == FAKE_WATTS
                and isinstance(row.get("cpu_util"), (int, float)) and 0 <= row["cpu_util"] <= 1
                and abs((row.get("t1") or 0) - (row.get("t0") or 0) - 60) <= 1.0,
                json.dumps({"rows": rows, "after_s": after})[:500], seconds=after)

        # ---- costing prices the machine's energy from the row
        out = subprocess.run([sys.executable, "-c", COSTING, sid], cwd=str(ROOT), env=h.server.env,
                             capture_output=True, text=True, timeout=120)
        priced = {}
        try:
            priced = json.loads(out.stdout.strip().splitlines()[-1]) if out.stdout.strip() else {}
        except ValueError:
            pass
        act = priced.get("act") or {}
        metered = priced.get("metered") or {}
        r.check("costing (on the instance's own data) prices an activity holding the helper machine over that minute "
                "from the row: measured energy, none estimated, and the machine's metered minute",
                (act.get("energy_measured_h") or 0) > 0 and act.get("energy_estimated_h") == 0
                and metered.get("minutes") == 1 and (metered.get("kwh") or 0) > 0,
                json.dumps({"act": {k: act.get(k) for k in ("energy_measured_h", "energy_estimated_h", "kwh_idle",
                                                             "kwh_dynamic")},
                            "metered": metered, "stderr": out.stderr[-300:]})[:600])

        # ---- the fake's log: every reading a call of the fake's, and nothing else
        calls = [line.split(" ", 1)[1] if " " in line else ""
                 for line in (probe_log.read_text(encoding="utf-8").splitlines() if probe_log.exists() else [])]
        scans = [c for c in calls if c.startswith("--query-gpu=name,")]
        draws = [c for c in calls if c == "--query-gpu=power.draw --format=csv,noheader,nounits"]
        r.check("the fake's log holds every probe call: two scans (the hello's and the rescan's) and a reading every "
                "10 s, each sample of the row one of them; the machine's real nvidia-smi was never asked (every GPU "
                "figure is the fake's)",
                len(scans) == 2 and len(draws) >= 6 and len(scans) + len(draws) == len(calls),
                json.dumps({"calls": len(calls), "scans": len(scans), "draws": len(draws)}))
        data.update(calls=len(calls), draws=len(draws), row=row)

        # ---- the helper started --no-hardware --no-power reports and writes nothing
        quiet_report = report(quiet_sid)
        r.check("a helper started with --no-hardware --no-power reports no scan and writes no power row",
                not quiet_report.get("hardware") and _rows(h, quiet_sid) == [],
                json.dumps({"hardware": bool(quiet_report.get("hardware")), "rows": len(_rows(h, quiet_sid))}))
        if h.keep:
            # the browser checklist's H2: bob's scanned machine with its helper connected, for Rescan now
            data.update(kept_helper=True)
            helpers.remove(scanned)
    finally:
        for helper in helpers:
            helper.stop()
        if not h.keep:
            scripted.close()


# ---------------------------------------------------------------------------- the run

PARTS: dict[str, Callable] = {"helper": part_helper, "llm-real": part_llm_real, "scan": part_scan}


def main(argv=None) -> int:
    """Run the harness on Phase 4's parts."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("part", choices=sorted([*PARTS, *LATER, "all"]), help="the part to run, or all")
    ap.add_argument("--port", type=int, default=0, help="a free port by default; never 8799 or one in use")
    ap.add_argument("--dir", default=str(Path(os.environ.get("TEMP") or "/tmp") / "citar-qa-phase4"),
                    help="the state directory: new, empty, or an earlier run's (it is emptied first)")
    ap.add_argument("--keep", action="store_true", help="leave the server running afterwards (the browser checklist)")
    ap.add_argument("--skip", action="append", choices=sorted(PARTS), default=[], metavar="PART",
                    help="all: leave this part out (llm-real needs LM Studio; say so when quoting the run)")
    ap.add_argument("--helper-bin", default=None, metavar="PATH",
                    help="the CITAR helper to run (default: citar-helper in CITAR_EXT_DIR)")
    ap.add_argument("--llm-url", default=REAL_URL, help="llm-real: LM Studio's address (read only)")
    ap.add_argument("--stand-in", action="store_true",
                    help="llm-real: play LM Studio with the scripted endpoint (to check the part without the model)")
    ap.add_argument("--wsl", action="store_true", help="the server in WSL (the remote and exit parts: 4-10, 4-15)")
    args = ap.parse_args(argv)
    global LLM_URL, STAND_IN
    LLM_URL, STAND_IN = args.llm_url, args.stand_in
    q3.HELPER_BIN = args.helper_bin
    if args.wsl:
        print("refused: --wsl belongs to the remote and exit parts (packages 4-10, 4-15)", flush=True)
        return 2
    # the harness's own run, on this script's parts
    q3.PARTS, q3.LATER = PARTS, LATER
    try:
        code, report = q3.run(args)
    except q3.Refused as e:
        print(f"refused: {e}", flush=True)
        return 2
    summary = {"part": args.part, "ok": code == 0, "parts": report.summary(), "checks": report.checks, **report.data}
    folder = Path(report.data["server"]["dir"])
    out = folder / f"qa-{args.part}.json"
    try:
        out.write_text(json.dumps(summary, indent=2, default=str) + "\n", encoding="utf-8")
        where = f"evidence in {out}"
        if len(report.parts) > 1:
            for name in report.parts:
                q3.write_part(folder, args.part, name, report)
    except OSError as e:
        where = f"the evidence could not be written to {out} ({e})"
        code = 1
    passed = sum(1 for c in report.checks if c["ok"])
    print(f"{'OK' if code == 0 else 'FAILED'}: {passed} of {len(report.checks)} checks passed; {where}", flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())

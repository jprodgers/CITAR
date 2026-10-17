"""Phase 4's QA script: the CITAR helper and the server's side of protocol 2, end to end, with the evidence written
down (design/phase4-helper.md P4.7.6).

    python scripts/qa_phase4.py <part>|all [--port N] [--dir D] [--keep] [--helper-bin PATH] [--skip PART]
                                [--llm-url URL] [--stand-in]
                                [--wsl --wsl-root PATH --wsl-python PATH [--wsl-distro NAME] [--wsl-user NAME]]

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

- ``lab`` (4-09): the lab played inside the server by its job dispatcher on its own job host (the local executor),
  the host machine's CPU set in its machine settings (two threads, two cores' worth at most, 400 MB): bob, given
  ``run_lab``, queues six 30-round duels from the A/B route, which run to their end and move to done/ (six lines, each
  with ``executor.machine`` the host and ``verification: reference``; six ``cpu`` events used by bob and given by
  nobody, each observed at least as long as its job host measured it and counted whole; the ratings include them;
  ``GET /api/lab/jobs`` shows them done on "this server" to bob and "not found" to alice); ``citar lab submit`` refuses a member's ranking run and queues an administrator's ranking and reference runs
  as the server's own collection (no consumer), the reference lines read by ``summarize.py``; Stop holds a new
  experiment ("the lab is paused") and Resume plays it; the server killed twice with two games playing (each seat step
  slowed by the test build's ``CITAR_JOBS_TEST_STEP_MS``) plays exactly those two again, each game in the results
  once, and the Queue page's CPU card names bob's games to him and "someone's lab game" to alice; a game whose job host
  is killed three times fails, and an administrator's retry (``POST /api/admin/jobs/retry``) plays it to its end (a
  member's is 404); the administrators' view of the dispatcher; no traceback.

- ``remote`` (4-10): the lab's games on a helper machine. Bob's machine, in a group of his and trusted by an
  administrator, asks four threads, 50% and 1 GB in its machine settings; its real helper lends two threads, 20% of
  the machine and 512 MB in its own ``helper.json`` (the owner's ceiling, which wins); the host lends nothing here.
  Alice, given ``run_lab`` by an administrator and named by bob's grant listing Lab (CPU work), queues eight 30-round
  duels from the A/B route: they play on bob's machine (eight lines with ``executor.machine`` his machine,
  ``unverified``: accepted without a replica, its results unsampled in this part; eight events given by bob and used
  by alice); the dialog and the helper card show the CPU
  in effect, hard on Windows, and the grant; the job host's CPU stays under its cap plus slack and every run's round
  moves at least every 30 s; ``GET /api/machines/{id}/jobs`` is bob's (alice, a grantee, 403; a stranger 404). Where
  the share binds (5% of the machine, pushed to the helper) two long games stay under it, and deleting their
  experiment from the queue ends them on the helper. **Yield:** the Servers page's test streaming for 3 s holds the
  game playing (its round the same when held and let go) and it goes on within 2 s of the ``[DONE]``, its paused time
  reported and the server's deadline pushed on; with ``yield_to_llm`` off a request holds nothing, and a one-job
  benchmark's lease sends ``jobs_pause`` and its end ``jobs_resume``. With ``--wsl`` the server runs in WSL (P4.7.6's
  topology: ``--wsl-root``, a checkout there, and ``--wsl-python``, its venv's Python with the extension ``cargo xtask
  develop`` built there; bound to ``0.0.0.0`` inside WSL when ``wslinfo --networking-mode`` says ``nat``, else
  ``127.0.0.1``; its state in ``~/citar-qa/<the directory's name>``) and the Windows helper dials it through WSL's
  localhost forwarding; when Windows cannot reach it the part says so and runs with the server on Windows (the
  fallback), recorded in the evidence's ``remote.topology``.

- ``verify`` (4-11 and 4-12): helpers' results checked (design/phase4-helper.md P4.5.7, P4.5.8), with real helpers
  built to cheat on purpose (``citar-helper-faults``, which ``cargo xtask develop`` builds beside the helper:
  ``--test-fault``) and this server's job host as their verifier; the verification settings set through their route
  (``PUT /api/admin/verification``). A new machine of bob's that alters every result (``result:1``) is quarantined at
  its first job ("same chain, different result: tampering or an incompatible build"), its result never in the results
  file, its event rejected, bob quarantined; his second machine, honest, is refused CPU work meanwhile; the
  quarantined machine serves nobody but its owner (alice's pooled prompt on it is refused ``quarantined`` at
  admission); an administrator's release returns it to ``new`` (its counters reset, ``since`` now) and bob's honest
  machine plays again. An honest new machine of alice's (thresholds lowered to 2 and 4) climbs to ``trusted``, an
  audit row each step, its lines verified. A provisional machine of bob's that alters chosen jobs (``result-at:2,4``;
  its results unsampled) plays three games unnoticed, then an administrator's ``replicate 1`` checks its fourth: it
  mismatches, the machine is quarantined, and every result it gave unverified is withdrawn (lines gone, events
  rejected) and played again on this server. The trust routes are administrators'. **4-12:** the verification
  settings' routes are administrators' and bounded, and their page's data names the quarantined machine with its
  evidence, its quarantined owner, the mismatches, the withdrawals and the verifiers; credits: a helper reporting
  three times its CPU (``cpu:3``, 300-round games) is credited at most 1.5 times its replica's CPU, flagged
  ``overreported_cpu`` and kept from ``trusted``, one holding its results (``hold:2``) is capped at 1.5 times the
  reference, flagged ``capped_cpu`` only and promoted, and with an honest machine measured beside it the over-reporter
  is ``class_suspect``; model results by tier: a benchmark game on bob's ``provisional`` helper machine (an LLM through
  the scripted endpoint) is left out of the administrators' and alice's model scores, counted in ``unverified``,
  badged on bob's own board and on his run, and counted once an administrator trusts the machine.

- ``update`` (4-13): the helper updating itself (design/phase4-helper.md P4.3.8, P4.5.9), with the release side's own
  tools: ``cargo xtask keygen`` makes a throwaway key in the run's directory (and refuses a folder inside the
  repository, as ``sign-manifest`` refuses a key file there), ``helper-manifest`` lists the newer update helper
  ``cargo xtask develop`` built (``citar-helper-update-2``, which calls itself ``0.1.6-test.2``) under its release name,
  served on loopback behind a redirect as GitHub serves release assets, and ``sign-manifest`` signs it. The server,
  restarted with ``CITAR_HELPER_MANIFEST_FILE`` and the key trusted (``CITAR_HELPER_UPDATE_KEY``, the extension's test
  operations), relays it; bob's machine's older helper (``citar-helper-update-1``, ``0.1.6-test.1``), copied into an
  install folder of the run's own, downloads it, checks it, self-tests it, hands off and reconnects as
  ``0.1.6-test.2`` reporting applied, never replaced; the folder holds the new binary and the old one as ``.old``;
  his card says up to date. A helper set to notify reports the update available and downloads nothing; the run's
  own helper, which trusts no key, reports updates unavailable. With ``--keep`` the three stay connected, for the
  browser checklist's H1 (the cards' update lines).

``all`` runs every part written so far; later packages add ``service`` (4-14), ``exit`` and ``real`` (4-15).
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
LATER = {"service": "4-14", "exit": "4-15", "real": "4-15"}
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


# ---------------------------------------------------------------------------- a process's CPU (4-10)

def descendants(pid: int) -> list:
    """The processes ``pid`` started and theirs (a helper's job host, and a wrapper's child on Linux), by their
    parents' ids: Windows' process snapshot, Linux's ``/proc``; [] elsewhere (macOS: the CPU checks say so)."""
    parents: dict = {}
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes

        class Entry(ctypes.Structure):
            _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
                        ("th32ProcessID", wintypes.DWORD), ("th32DefaultHeapID", ctypes.c_size_t),
                        ("th32ModuleID", wintypes.DWORD), ("cntThreads", wintypes.DWORD),
                        ("th32ParentProcessID", wintypes.DWORD), ("pcPriClassBase", ctypes.c_long),
                        ("dwFlags", wintypes.DWORD), ("szExeFile", ctypes.c_wchar * 260)]
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
        snap = k32.CreateToolhelp32Snapshot(0x2, 0)
        if not snap or snap == wintypes.HANDLE(-1).value:
            return []
        try:
            e = Entry()
            e.dwSize = ctypes.sizeof(Entry)
            ok = k32.Process32FirstW(snap, ctypes.byref(e))
            while ok:
                parents.setdefault(int(e.th32ParentProcessID), []).append(int(e.th32ProcessID))
                ok = k32.Process32NextW(snap, ctypes.byref(e))
        finally:
            k32.CloseHandle(snap)
    elif sys.platform.startswith("linux"):
        for d in Path("/proc").iterdir():
            if not d.name.isdigit():
                continue
            try:
                stat = (d / "stat").read_text()
            except OSError:
                continue
            ppid = int(stat[stat.rindex(")") + 2:].split()[1])
            parents.setdefault(ppid, []).append(int(d.name))
    out, todo = [], [pid]
    while todo:
        for child in parents.get(todo.pop(), []):
            if child not in out and child != pid:
                out.append(child)
                todo.append(child)
    return out


def cpu_seconds(pid: int) -> Optional[float]:
    """The CPU time a process has used so far (user and kernel), or None when it is gone or cannot be read."""
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.OpenProcess.restype = wintypes.HANDLE
        handle = k32.OpenProcess(0x1000, False, int(pid))         # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return None
        try:
            times = [wintypes.FILETIME() for _ in range(4)]
            if not k32.GetProcessTimes(handle, *(ctypes.byref(t) for t in times)):
                return None
            kernel, user = ((t.dwHighDateTime << 32) | t.dwLowDateTime for t in times[2:])
            return (kernel + user) / 1e7
        finally:
            k32.CloseHandle(handle)
    if sys.platform.startswith("linux"):
        try:
            stat = Path(f"/proc/{int(pid)}/stat").read_text()
        except OSError:
            return None
        fields = stat[stat.rindex(")") + 2:].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    return None


class CpuWatch:
    """Samples the CPU of the processes a helper started (its job host) every ``every`` seconds on a thread of its
    own, from ``start()`` to ``stop()``: ``summary()`` gives the busiest window of at least ``window`` seconds and the
    whole span's mean, each as a percentage of the whole machine (all its logical CPUs, as a CPU limit is), or None
    where a process's CPU cannot be read (macOS)."""

    def __init__(self, pid: int, *, every: float = 0.25, window: float = 2.0):
        self.pid, self.every, self.window = pid, every, window
        self.samples: list = []         # (monotonic time, the descendants' CPU seconds so far)
        self._seen: dict = {}           # descendant -> its last reading (one that ended keeps its last)
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None

    def _read(self) -> Optional[float]:
        for child in descendants(self.pid):
            got = cpu_seconds(child)
            if got is not None:
                self._seen[child] = max(got, self._seen.get(child, 0.0))
        return sum(self._seen.values()) if self._seen else None

    def start(self) -> "CpuWatch":
        def loop():
            while not self._stop.is_set():
                got = self._read()
                if got is not None:
                    self.samples.append((time.monotonic(), got))
                self._stop.wait(self.every)
        self._thread = threading.Thread(target=loop, name="qa-cpu-watch", daemon=True)
        self._thread.start()
        return self

    def stop(self) -> dict:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(5)
        return self.summary()

    def summary(self) -> dict:
        cpus = os.cpu_count() or 1
        s = self.samples
        if len(s) < 2 or s[-1][0] - s[0][0] <= 0:
            return {"peak_pct": None, "mean_pct": None, "seconds": 0.0, "cpu_s": 0.0, "cpus": cpus}
        peak = 0.0
        j = 0
        for i in range(len(s)):
            while j < len(s) and s[j][0] - s[i][0] < self.window:
                j += 1
            if j >= len(s):
                break
            peak = max(peak, (s[j][1] - s[i][1]) / (s[j][0] - s[i][0]) / cpus * 100.0)
        span = s[-1][0] - s[0][0]
        used = s[-1][1] - s[0][1]
        return {"peak_pct": round(peak, 2), "mean_pct": round(used / span / cpus * 100.0, 2),
                "seconds": round(span, 2), "cpu_s": round(used, 3), "cpus": cpus}


# ---------------------------------------------------------------------------- lab (4-09)

#: The instance's lab, its jobs, its events and its ratings, read in a child with the instance's environment (its
#: database and saves folder): ``python -c LAB_STATE <experiment names...>``.
LAB_STATE = """
import json, sys
from sqlalchemy import select
from citar import db
from citar.bots import ratings
from citar.db.models import ComputeEvent, CpuJob, CpuJobRun
db.configure()
names = sys.argv[1:]
out = {}
with db.session() as s:
    for name in names:
        jobs = [j for j in s.scalars(select(CpuJob).where(CpuJob.item_ref == name))]
        runs = {}
        for r in s.scalars(select(CpuJobRun).where(CpuJobRun.job_id.in_([j.id for j in jobs] or ["-"]))):
            runs.setdefault(r.job_id, []).append({"id": r.id, "state": r.state, "verdict": r.verdict,
                                                   "error": (r.error or {}).get("code"), "machine": r.machine_id,
                                                   "executor": r.executor, "offered": r.offered_at.isoformat(),
                                                   "paused_s": r.paused_s})
        events = [{"activity": e.activity, "consumer": e.consumer_id, "provider": e.provider_id,
                   "machine": e.machine_id, "cpu": e.cpu_core_s, "observed": e.observed_s,
                   "reported": e.reported_s, "flags": e.flags or {},
                   "verification": e.verification, "request": e.request_id}
                  for e in s.scalars(select(ComputeEvent).where(ComputeEvent.activity.like(f"lab:{name}:%")))]
        out[name] = {"jobs": [{"id": j.id, "ref": j.ref, "state": j.state, "owner": j.owner_id, "kind": j.kind,
                                "runs": sorted(runs.get(j.id, []), key=lambda r: r["offered"])} for j in jobs],
                     "events": events}
games, entries = ratings.collect()
out["_rated"] = sorted({x for e in entries.values() for x in e["experiments"]})
print(json.dumps(out, default=str))
"""


def _lab_state(h, *names: str) -> dict:
    import subprocess
    got = subprocess.run([sys.executable, "-c", LAB_STATE, *names], cwd=str(ROOT), env=h.server.env,
                         capture_output=True, text=True, timeout=120)
    try:
        return json.loads(got.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"_error": got.stderr[-500:]}


def _lines(h, name: str, folder: str = "results") -> list:
    path = h.server.state / "saves" / "lab" / folder / f"{name}.jsonl"
    if not path.exists():
        return []
    return [json.loads(x) for x in path.read_text(encoding="utf-8").splitlines() if x.strip()]


def _lab_view(who, name: str) -> dict:
    got = who.get("/api/lab")
    if got.status_code != 200:
        return {}
    return next((e for e in got.json().get("experiments", []) if e.get("name") == name), {})


def part_lab(h) -> None:
    """The ``lab`` part (the module's doc)."""
    import subprocess
    r = h.report
    admin, alice, bob = h.accounts["admin"], h.accounts["alice"], h.accounts["bob"]
    data = r.data.setdefault("lab", {})
    host = "sv_qa_host"
    share = max(1, min(100, 200 // (os.cpu_count() or 1)))
    tag = h.tag

    # ---- the host's CPU, set as its machine settings (P4.5.6), within two cores' worth and 400 MB (P4.7.1)
    cpu = {"enabled": True, "threads": 2, "cpu_percent": share, "memory_mb": 400,
           "work": ["lab_game", "ranking_game", "reference_game"]}
    put = admin.put(f"/api/machines/{host}/settings", json={"capabilities": {"cpu": cpu}})
    read = admin.get(f"/api/machines/{host}/settings")
    caps = (read.json().get("capabilities") or {}).get("cpu") if read.status_code == 200 else {}
    r.check("the host's CPU sharing is set in its machine settings (2 threads, two cores' worth at most, 400 MB, "
            "every kind of work), and the dialog reads it back",
            put.status_code == 200 and {k: caps.get(k) for k in cpu} == cpu,
            json.dumps({"put": put.status_code, "cpu": caps}))

    # ---- bob, given run_lab, queues six 30-round duels from the Bots page's A/B route
    bob_id = bob.get("/api/auth/me").json()["user"]["id"]
    admin.patch(f"/api/admin/users/{bob_id}", json={"caps": {"run_lab": True}})
    duels = f"qa-duels-{tag}"
    queued = bob.post("/api/bots/experiments", json={
        "profiles": ["standard", "classic-production"], "name": duels, "players": 2, "games": 6, "seed": 6100,
        "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": 30})
    r.check("bob (given run_lab) queues six 30-round duels from the Bots page's A/B route",
            queued.status_code == 200, queued.text[:300])
    t0 = time.perf_counter()
    done = wait_until(lambda: _lab_view(bob, duels).get("state") == "complete" and time.perf_counter(), 180, 0.5)
    took = round(done - t0, 1) if done else None
    lines = _lines(h, duels)
    r.check("the experiment runs to its end on the local executor and moves to done/: six result lines, each "
            "with executor.machine the host and verification reference, each game once",
            bool(done) and sorted(x.get("i") for x in lines) == list(range(6))
            and all((x.get("executor") or {}).get("machine") == host
                    and (x.get("executor") or {}).get("verification") == "reference" for x in lines)
            and (h.server.state / "saves" / "lab" / "done" / f"{duels}.json").exists(),
            json.dumps({"seconds": took, "lines": len(lines),
                        "executors": sorted({json.dumps(x.get("executor", {}).get("machine")) for x in lines})}),
            seconds=took)
    events = []
    state: dict = {}

    def settled():
        nonlocal state, events
        state = _lab_state(h, duels)
        events = (state.get(duels) or {}).get("events") or []
        return len(events) >= 6
    wait_until(settled, 30, 1.0)
    runs = {x["executor"]["run"] for x in lines if x.get("executor")}
    played = {x["executor"]["run"]: x.get("seconds") or 0.0 for x in lines if x.get("executor")}
    # what the server observed is each run's time from its accept's arrival to its result's (never when the
    # dispatcher read them): at least the time its job host measured, so an honest run's CPU is counted whole
    timed = all(e["observed"] >= 0.9 * played.get(e["request"], 0.0) - 0.1 and not e["flags"].get("overreported_cpu")
                and e["cpu"] == round(e["reported"], 6) for e in events)
    r.check("six cpu events with bob as the consumer and no provider, on the host, verified, one a run, each "
            "timed by its frames (observed at least the time its host measured) and counted whole",
            len(events) == 6 and {(e["consumer"], e["provider"], e["machine"], e["verification"]) for e in events}
            == {(bob_id, None, host, "verified")} and {e["request"] for e in events} == runs and timed,
            json.dumps([{**{k: e[k] for k in ("cpu", "reported", "observed", "flags")},
                         "seconds": played.get(e["request"])} for e in events][:3])[:500])
    r.check("the ratings include its games", duels in (state.get("_rated") or []), str(state.get("_rated"))[:300])
    mine = bob.get("/api/lab/jobs", params={"exp": duels})
    jobs = mine.json().get("jobs", []) if mine.status_code == 200 else []
    stranger = alice.get("/api/lab/jobs", params={"exp": duels})
    r.check("GET /api/lab/jobs: bob sees his six games done on this server, their verification reference; alice, "
            "who may not see the experiment, gets 404",
            len(jobs) == 6 and {(j["state"], (j.get("executor") or {}).get("name"), j.get("verification"))
                                for j in jobs} == {("done", "this server", "reference")}
            and stranger.status_code == 404,
            json.dumps({"jobs": jobs[:1], "alice": stranger.status_code})[:400])
    data.update(duels=duels, duels_seconds=took)

    # ---- an administrator's ranking and reference runs (citar lab submit): the server's own collection
    ladder, base = f"qa-ladder-{tag}", f"qa-reference-{tag}"
    specs = h.server.state / f"qa-lab-{tag}.json"
    specs.write_text(json.dumps([
        {"name": ladder, "kind": "ranking", "games": 2, "seed": 6200, "size": "duel", "maps": ["pangaea"],
         "turns": 30, "seats": [{"profile": "standard"}, {"profile": "classic-production"}]},
        {"name": base, "kind": "reference", "games": 2, "seed": 6300, "size": "duel", "maps": ["pangaea"],
         "turns": 40}]), encoding="utf-8")
    refused = subprocess.run([sys.executable, "-m", "citar.lab", "submit", str(specs), "--owner", alice.handle],
                             cwd=str(ROOT), env=h.server.env, capture_output=True, text=True, timeout=120)
    submitted = subprocess.run([sys.executable, "-m", "citar.lab", "submit", str(specs), "--owner", admin.handle],
                               cwd=str(ROOT), env=h.server.env, capture_output=True, text=True, timeout=120)
    r.check("citar lab submit refuses a ranking run of a member (exit 2), and queues an administrator's ranking and "
            "reference runs as the server's own collection",
            refused.returncode == 2 and "is an administrator's" in refused.stderr and submitted.returncode == 0
            and submitted.stdout.count("as the server's own collection") == 2,
            json.dumps({"refused": refused.stderr.strip()[-200:], "out": submitted.stdout.strip()[-300:]}))
    both = wait_until(lambda: all((h.server.state / "saves" / "lab" / "done" / f"{n}.json").exists()
                                  for n in (ladder, base)), 180, 0.5)
    ref_path = h.server.state / "saves" / "lab" / "reference" / f"{base}.jsonl"
    summed = subprocess.run([sys.executable, str(ROOT / "scripts" / "refcheck" / "summarize.py"), str(ref_path),
                             "--json"], cwd=str(ROOT), capture_output=True, text=True, timeout=120)
    try:
        summary = json.loads(summed.stdout) if summed.returncode == 0 else {}
    except ValueError:
        summary = {}
    state = {}

    def written():
        state.clear()
        state.update(_lab_state(h, ladder, base))
        return all(len((state.get(n) or {}).get("events") or []) >= 2 for n in (ladder, base))
    wait_until(written, 30, 1.0)
    collection = [e for n in (ladder, base) for e in (state.get(n) or {}).get("events") or []]
    r.check("both run to their end: the ranking games in results/ and rated, the reference lines in reference/ "
            "(read by summarize.py as a baseline's), every event with no consumer",
            bool(both) and len(_lines(h, ladder)) == 2 and len(_lines(h, base, "reference")) == 2
            and not _lines(h, base) and summed.returncode == 0 and bool(summary)
            and len(collection) == 4 and {(e["consumer"], e["provider"]) for e in collection} == {(None, None)}
            and ladder in (state.get("_rated") or []),
            json.dumps({"summarize": summed.returncode, "keys": sorted(summary)[:6],
                        "events": len(collection)})[:400])

    # ---- the lab paused (the Lab page's Stop) and resumed
    paused = f"qa-paused-{tag}"
    stop = admin.post("/api/admin/jobs/pause")
    bob.post("/api/bots/experiments", json={
        "profiles": ["standard", "standard"], "name": paused, "players": 2, "games": 2, "seed": 6400,
        "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": 30})
    time.sleep(5.0)         # two of the dispatcher's ticks: nothing may start
    held = admin.get("/api/admin/jobs").json()
    waiting = [q.get("waiting") for q in (held.get("queued") or {}).get(paused, [])]
    still = _lab_view(bob, paused)
    resumed = admin.post("/api/admin/jobs/resume")
    finished = wait_until(lambda: _lab_view(bob, paused).get("state") == "complete", 120, 0.5)
    r.check("Stop pauses the lab (an experiment queued meanwhile waits, \"the lab is paused\"), and Resume plays it "
            "to its end; a member may not stop it (404)",
            stop.status_code == 200 and (held.get("paused") or {}).get("by") == admin.handle
            and still.get("done") == 0 and waiting and set(waiting) == {"the lab is paused"}
            and resumed.status_code == 200 and bool(finished)
            and bob.post("/api/admin/jobs/pause").status_code == 404,
            json.dumps({"waiting": waiting, "done_while_paused": still.get("done")}))

    # ---- the server killed with two games playing: exactly those two again, each game once
    killed = f"qa-killed-{tag}"
    # each seat step slowed (a test build's switch): a game of 60 rounds lasts about 7 s, so it is still playing when
    # the dispatcher's view (published every tick, 2 s) shows it accepted
    h.server.env["CITAR_JOBS_TEST_STEP_MS"] = "60"
    h.server.restart()
    for who in h.accounts.values():
        who.reconnect()
    bob.post("/api/bots/experiments", json={
        "profiles": ["standard", "classic-production"], "name": killed, "players": 2, "games": 4, "seed": 6500,
        "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": 60})

    def playing():
        got = admin.get("/api/admin/jobs")
        rows = (got.json().get("running") or {}).get(host, []) if got.status_code == 200 else []
        rows = [x for x in rows if x.get("exp") == killed and x.get("state") == "running"]
        return rows if len(rows) == 2 else None
    two = wait_until(playing, 120, 0.25) or []
    names = {g["label"] for g in (bob.get("/api/queue").json()["cpu"]["executors"] or [{}])[0].get("games", [])}
    others = {g["label"] for g in (alice.get("/api/queue").json()["cpu"]["executors"] or [{}])[0].get("games", [])}
    r.check("the Queue page's CPU card: bob's games named, the same games \"someone's lab game\" to alice",
            len(two) == 2 and any(n.startswith(killed) for n in names) and others == {"someone's lab game"},
            json.dumps({"bob": sorted(names), "alice": sorted(others)}))
    jobs_then = sorted(x["job"] for x in two)
    h.server.restart()
    for who in h.accounts.values():
        who.reconnect()
    end = wait_until(lambda: _lab_view(bob, killed).get("state") == "complete", 180, 0.5)
    after = (_lab_state(h, killed).get(killed) or {}).get("jobs") or []
    by_id = {j["id"]: j for j in after}
    again = [by_id.get(j, {}).get("runs") or [] for j in jobs_then]
    lines = _lines(h, killed)
    r.check("the server killed with two games playing comes back and plays exactly those two again (their first "
            "runs aborted at the restart), and each game is in the results once",
            bool(end) and len(jobs_then) == 2
            and all(len(rs) == 2 and rs[0]["state"] == "aborted" and rs[0]["error"] == "restart"
                    and rs[1]["state"] == "done" for rs in again)
            and all(len(j["runs"]) == 1 for j in after if j["id"] not in jobs_then)
            and sorted(x["i"] for x in lines) == [0, 1, 2, 3],
            json.dumps({"again": again, "lines": len(lines)}, default=str)[:600])
    del h.server.env["CITAR_JOBS_TEST_STEP_MS"]

    # ---- the dispatcher as administrators see it; nothing went wrong in the server
    view = admin.get("/api/admin/jobs").json()
    executor = ((view.get("dispatcher") or {}).get("executors") or [{}])[0]
    # ---- a game whose job host dies three times fails, and an administrator's retry plays it again (still slowed:
    # the server was started with the test build's switch)
    import signal
    flaky = f"qa-flaky-{tag}"
    bob.post("/api/bots/experiments", json={
        "profiles": ["standard", "standard"], "name": flaky, "players": 2, "games": 1, "seed": 6600,
        "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": 60})

    def host_playing():
        got = admin.get("/api/admin/jobs")
        if got.status_code != 200:
            return None
        body = got.json()
        ex = ((body.get("dispatcher") or {}).get("executors") or [{}])[0]
        rows = [x for x in (body.get("running") or {}).get(host, []) if x.get("exp") == flaky
                and x.get("state") == "running"]
        return ex.get("pid") if rows and ex.get("state") == "ready" else None
    kills = []
    for _ in range(3):
        pid = wait_until(host_playing, 90, 0.25)
        if not pid:
            break
        try:
            os.kill(int(pid), signal.SIGTERM)       # this server's own job host (the server's child), ended by hand
            kills.append(pid)
        except OSError:
            break
        wait_until(lambda pid=pid: host_playing() != pid, 30, 0.25)
    stalled = wait_until(lambda: (_lab_view(bob, flaky).get("failing") == [0]) and _lab_view(bob, flaky), 60, 0.5)
    retry_gone = admin.post("/api/admin/jobs/retry", params={"exp": duels})
    retry_bob = bob.post("/api/admin/jobs/retry", params={"exp": flaky})
    retry = admin.post("/api/admin/jobs/retry", params={"exp": flaky})
    replayed = wait_until(lambda: _lab_view(bob, flaky).get("state") == "complete", 120, 0.5)
    flaky_runs = [x for j in ((_lab_state(h, flaky).get(flaky) or {}).get("jobs") or []) for x in j["runs"]]
    r.check("a game whose job host is killed three times fails (the experiment stalled, its game listed failed); an "
            "administrator's retry (POST /api/admin/jobs/retry) plays it to its end, its line written once; a "
            "member's retry is 404, and so is one of an experiment no longer queued",
            len(kills) == 3 and bool(stalled) and stalled.get("state") == "stalled" and retry.status_code == 200
            and retry_bob.status_code == 404 and retry_gone.status_code == 404 and bool(replayed)
            and [(x["state"], x["error"]) for x in flaky_runs] == [("failed", "host")] * 3 + [("done", None)]
            and len(_lines(h, flaky)) == 1,
            json.dumps({"kills": len(kills), "stalled": (stalled or {}).get("state"),
                        "retry": [retry.status_code, retry_bob.status_code, retry_gone.status_code],
                        "runs": [(x["state"], x["error"]) for x in flaky_runs]}))
    r.check("GET /api/admin/jobs: the dispatcher alive, the host's job host ready under its settings (held hard on "
            "Windows, the CPU soft without a user manager on Linux) and verifiable; a member gets 404",
            (view.get("dispatcher") or {}).get("alive") is True and executor.get("machine") == host
            and executor.get("threads") == 2 and executor.get("memory_mb") == 400
            and executor.get("verifiable") in (True, None) and bob.get("/api/admin/jobs").status_code == 404,
            json.dumps({k: executor.get(k) for k in ("state", "threads", "cpu_percent", "memory_mb", "held",
                                                     "verifiable")}))
    data.update(executor={k: executor.get(k) for k in ("threads", "cpu_percent", "memory_mb", "held")})
    r.check("no traceback in the server's log", h.server.tracebacks() == 0, f"{h.server.tracebacks()} traceback(s)")


# ---------------------------------------------------------------------------- remote (4-10)

#: What bob lends in his helper's own settings (``helper.json``, as ``citar-helper setup`` writes it once he says
#: yes): gate 2's two threads, 20% of the machine and 512 MB.
REMOTE_CEILING = {"enabled": True, "threads": 2, "cpu_percent": 20, "memory_mb": 512,
                  "work": ["lab_game", "ranking_game"]}
#: What his machine's settings ask: more, so the helper's own ceiling is what plays (and the dialog says it capped).
REMOTE_SITE = {"enabled": True, "threads": 4, "cpu_percent": 50, "memory_mb": 1024,
               "work": ["lab_game", "ranking_game"], "yield_to_llm": True}
#: The Servers page's test prompt (``pool_api``'s), which the scripted endpoint streams over ``YIELD_S`` seconds.
TEST_PROMPT = "Reply with exactly: CITAR OK"
YIELD_S = 3.0
#: A game long enough to be held and let go (about 9 s of one core here): eight seats on a huge map.
LONG_GAME = {"players": 8, "size": "huge", "maps": ["pangaea"], "speed": "Quick", "turns": 330,
             "profiles": ["standard", "classic-production"]}


def _saves_text(h, rel: str) -> Optional[str]:
    """A file of the instance's saves folder (``rel`` under it), or None when it is not there: through WSL for a WSL
    server."""
    reader = getattr(h.server, "read", None)
    if reader is not None:
        return reader(f"saves/{rel}")
    try:
        return (h.server.state / "saves" / rel).read_text(encoding="utf-8")
    except OSError:
        return None


def _saves_remove(h, rel: str) -> None:
    """Delete a file of the instance's saves folder (an experiment's queue file, as its owner deleting it would)."""
    remover = getattr(h.server, "remove", None)
    if remover is not None:
        remover(f"saves/{rel}")
        return
    try:
        (h.server.state / "saves" / rel).unlink()
    except OSError:
        pass


def _server_python(h, code: str, *args: str) -> str:
    """``python -c code args`` with the instance's environment (its database and saves): in WSL for a WSL server."""
    import subprocess
    runner = getattr(h.server, "python", None)
    if runner is not None:
        got = runner("-c", code, *args)
    else:
        got = subprocess.run([sys.executable, "-c", code, *args], cwd=str(ROOT), env=h.server.env,
                             capture_output=True, text=True, timeout=120)
    return got.stdout


class JobsPoller:
    """The administrators' view of the dispatcher (``GET /api/admin/jobs``) every 50 ms on a thread of its own: per
    sample, ``time.time()``, the machine's runs (run, experiment, state, turn, paused, paused_s, extended_s) and
    whether a benchmark holds its executor."""

    def __init__(self, admin, machine: str):
        import httpx
        self.machine = machine
        self.http = httpx.Client(base_url=admin.http.base_url, cookies=admin.http.cookies,
                                 headers=admin.http.headers, timeout=10.0)
        self.samples: list = []
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._loop, name="qa-jobs-poller", daemon=True)
        self._thread.start()

    def _loop(self) -> None:
        while not self._stop.is_set():
            try:
                body = self.http.get("/api/admin/jobs").json()
            except Exception:
                body = None
            if isinstance(body, dict):
                ex = next((e for e in (body.get("dispatcher") or {}).get("executors") or []
                           if e.get("machine") == self.machine), {})
                rows = [{k: x.get(k) for k in ("run", "exp", "i", "state", "turn", "limit", "paused", "paused_s",
                                               "extended_s", "deadline_s")}
                        for x in (body.get("running") or {}).get(self.machine, [])]
                self.samples.append({"t": time.time(), "runs": rows, "benchmark": bool(ex.get("benchmark")),
                                     "executor": {k: ex.get(k) for k in ("state", "paused", "running")}})
            self._stop.wait(0.05)

    def stop(self) -> None:
        self._stop.set()
        self._thread.join(5)
        self.http.close()

    def of(self, run: str) -> list:
        """(t, turn, paused, paused_s, extended_s) of ``run`` in each sample that shows it."""
        out = []
        for s in self.samples:
            for x in s["runs"]:
                if x["run"] == run:
                    out.append((s["t"], x["turn"], bool(x["paused"]), x.get("paused_s") or 0.0,
                                x.get("extended_s") or 0.0))
        return out

    def playing(self, exp: str) -> list:
        """The runs of ``exp`` seen running with a reported round."""
        return sorted({x["run"] for s in self.samples for x in s["runs"]
                       if x["exp"] == exp and x["state"] == "running" and x["turn"]})

    def gaps(self, exp: str) -> dict:
        """Each run's longest time between the samples that showed its round move (its first and last sample
        counting as moves)."""
        runs = {x["run"] for s in self.samples for x in s["runs"] if x["exp"] == exp}
        out = {}
        for run in runs:
            rows = self.of(run)
            moves = [rows[0][0]] + [b[0] for a, b in zip(rows, rows[1:]) if b[1] != a[1]] + [rows[-1][0]]
            out[run] = round(max((b - a for a, b in zip(moves, moves[1:])), default=0.0), 2)
        return out


def part_remote(h) -> None:
    """The ``remote`` part (the module's doc)."""
    r = h.report
    admin, alice, bob = h.accounts["admin"], h.accounts["alice"], h.accounts["bob"]
    data = r.data.setdefault("remote", {"topology": getattr(h.server, "topology", "windows")})
    tag = h.tag
    host = "sv_qa_host"
    scripted = FakeEndpoint(delay=0.05)
    scripted.slow = {TEST_PROMPT: YIELD_S}
    helpers: list = []
    poller: Optional[JobsPoller] = None
    try:
        # ---- the host lends nothing in this part: every game goes to a helper, whose results are not sampled
        # (its trusted machine's rate 0: with no verifier a sampled result would wait; verification is the verify
        # part's)
        _verification(h, trusted_rate=0.0)
        off = admin.put(f"/api/machines/{host}/settings", json={"capabilities": {"cpu": {"enabled": False}}})
        # ---- bob's machine in a group of his, trusted by an administrator; alice given run_lab, and bob's grant
        # listing lab for her
        group = bob.post("/api/pool/groups", json={"name": f"QA lenders {tag}"})
        gid = group.json().get("id", "") if group.status_code == 200 else ""
        made = bob.post("/api/pool/servers", json={"config": {"name": f"QA lender {tag}", "kind": "owned",
                                                               "connection": {"provider": "openai_compatible",
                                                                              "max_parallel": 1}},
                                                    "reach": "worker", "group_id": gid})
        hid = made.json().get("id", "") if made.status_code == 200 else ""
        issued = bob.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa remote"}) if hid else None
        token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
        trusted = admin.put(f"/api/admin/machines/{hid}/trust", json={"trust_tier": "trusted"})
        site = bob.put(f"/api/machines/{hid}/settings", json={"capabilities": {"cpu": REMOTE_SITE}})
        alice_id = alice.get("/api/auth/me").json()["user"]["id"]
        lab_cap = admin.patch(f"/api/admin/users/{alice_id}", json={"caps": {"run_lab": True}})
        grant = bob.post(f"/api/pool/groups/{gid}/grants", json={"subject_type": "user", "handle": alice.handle,
                                                                 "purposes": ["lab"]}) if gid else None
        r.check("bob's helper machine in a group of his, trusted by an administrator, its CPU settings asking 4 "
                "threads, 50% and 1 GB; alice given run_lab by an administrator, and bob's grant listing Lab (CPU "
                "work) for her; the host lends no CPU in this part",
                all(x is not None and x.status_code == 200 for x in (off, group, made, issued, trusted, site, lab_cap,
                                                                     grant)) and bool(token),
                json.dumps({"statuses": [getattr(x, "status_code", None) for x in (off, group, made, issued, trusted,
                                                                                    site, lab_cap, grant)]}))
        data.update(machine=hid, group=gid)

        # ---- bob's helper, lending his CPU in its own settings (two threads, 20%, 512 MB)
        home = h.server.state / "helper-home-qa-lender"
        home.mkdir(parents=True, exist_ok=True)
        (home / "helper.json").write_text(json.dumps({"cpu": REMOTE_CEILING}), encoding="utf-8")
        helper = Helper(h, token, scripted, "qa-lender", machine="qa-lender")
        helpers.append(helper)

        def executor() -> dict:
            got = admin.get("/api/admin/jobs")
            return next((e for e in ((got.json().get("dispatcher") or {}).get("executors") or [])
                         if e.get("machine") == hid), {}) if got.status_code == 200 else {}
        ready = wait_until(lambda: executor().get("state") == "ready", 120, 0.25)
        ex = executor()
        jobs_view = admin.get("/api/admin/jobs")
        server_build = ((jobs_view.json().get("dispatcher") or {}).get("jobs_build")
                        if jobs_view.status_code == 200 else None)
        dialog = bob.get(f"/api/machines/{hid}/settings")
        lent = (dialog.json().get("cpu_helper") or {}) if dialog.status_code == 200 else {}
        eff = lent.get("effective") or {}
        grants = (dialog.json().get("lab_grants") or []) if dialog.status_code == 200 else []
        card = (_workers(bob, hid).get("cpu") or {})
        windows = os.name == "nt"
        r.check("its helper connects and its job host is ready, on the server's jobs build; the machine settings "
                "dialog shows the CPU in effect (two threads, 20%, 512 MB: its own ceiling capped the site's), each "
                "limit hard on Windows (a Job Object), the memory a run may use, and the grant that lends it for lab "
                "runs (alice); the helper card says the same",
                bool(ready) and lent.get("lent") is True and lent.get("verifiable") is True
                and bool(server_build) and ex.get("jobs_build") == server_build
                and {k: eff.get(k) for k in ("threads", "cpu_percent", "memory_mb")}
                == {"threads": 2, "cpu_percent": 20, "memory_mb": 512}
                and {"cpu.threads", "cpu.cpu_percent", "cpu.memory_mb"} <= set(lent.get("capped") or [])
                and (not windows or lent.get("limits") == {"cpu": "hard", "memory": "hard"})
                and (lent.get("memory_per_run_mb") or 0) > 0
                and [g.get("subject") for g in grants] == [alice.handle] and card.get("effective") == eff,
                json.dumps({"executor": {k: ex.get(k) for k in ("state", "reason", "threads", "cpu_percent",
                                                                 "memory_mb", "held", "memory_per_run_mb",
                                                                 "jobs_build")},
                            "dialog": lent, "grants": grants})[:700])
        data.update(executor={k: ex.get(k) for k in ("threads", "cpu_percent", "memory_mb", "held",
                                                      "memory_per_run_mb", "jobs_build")},
                    server_jobs_build=server_build)

        # ---- eight 30-round duels of alice's on bob's machine
        poller = JobsPoller(admin, hid)
        watch = CpuWatch(helper.proc.pid).start()
        eight = f"qa-remote-{tag}"
        queued = alice.post("/api/bots/experiments", json={
            "profiles": ["standard", "classic-production"], "name": eight, "players": 2, "games": 8, "seed": 7100,
            "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": 30})
        t0 = time.perf_counter()
        done = wait_until(lambda: _lab_view(alice, eight).get("state") == "complete" and time.perf_counter(),
                          240, 0.25)
        cpu = watch.stop()
        took = round(done - t0, 1) if done else None
        lines = _lab_lines(h, eight)
        r.check("alice (given run_lab) queues eight 30-round duels from the A/B route; they play on bob's machine to "
                "their end: eight lines with executor.machine his machine and verification unverified (its results "
                "unsampled in this part), "
                "each game once",
                queued.status_code == 200 and bool(done) and sorted(x.get("i") for x in lines) == list(range(8))
                and {((x.get("executor") or {}).get("machine"), (x.get("executor") or {}).get("verification"))
                     for x in lines} == {(hid, "unverified")},
                json.dumps({"seconds": took, "lines": len(lines)}), seconds=took)
        state: dict = {}

        def settled():
            state.clear()
            state.update(json.loads((_server_python(h, LAB_STATE, eight) or "{}").strip().splitlines()[-1] or "{}"))
            return len((state.get(eight) or {}).get("events") or []) >= 8
        wait_until(settled, 30, 1.0)
        events = (state.get(eight) or {}).get("events") or []
        bob_id = bob.get("/api/auth/me").json()["user"]["id"]
        runs = {(x.get("executor") or {}).get("run") for x in lines}
        r.check("eight cpu events, each given by bob (the machine's owner) and used by alice (the experiment's), on "
                "his machine, unverified, one a run",
                len(events) == 8 and {(e["provider"], e["consumer"], e["machine"], e["verification"]) for e in events}
                == {(bob_id, alice_id, hid, "unverified")} and {e["request"] for e in events} == runs,
                json.dumps([{k: e[k] for k in ("cpu", "reported", "observed", "flags")} for e in events][:3])[:500])
        gaps = poller.gaps(eight)
        limits = lent.get("limits") or {}
        bounded = cpu["peak_pct"] is None or limits.get("cpu") != "hard" or cpu["peak_pct"] <= 20 + 5
        r.check("the job host's CPU over the run stays under its 20% plus slack (where it is held hard), and every "
                "run's round advances at least every 30 s",
                bounded and bool(gaps) and max(gaps.values()) <= 30.0,
                json.dumps({"cpu": cpu, "limits": limits, "longest_gap_s": max(gaps.values()) if gaps else None}))
        data.update(eight=eight, eight_seconds=took, eight_cpu=cpu)
        mine = bob.get(f"/api/machines/{hid}/jobs")
        jobs = mine.json() if mine.status_code == 200 else {}
        r.check("GET /api/machines/{id}/jobs: bob sees his machine's eight runs done, alice's private experiment "
                "named \"a member's experiment\", what it lends now; alice, a grantee, may not (403), a stranger's "
                "view is 404",
                len([x for x in jobs.get("runs") or [] if x.get("state") == "done"]) >= 8
                and {x.get("experiment") for x in jobs.get("runs") or []} == {"a member's experiment"}
                and (jobs.get("lends") or {}).get("lent") is True
                and alice.get(f"/api/machines/{hid}/jobs").status_code == 403
                and h.accounts["mod"].get(f"/api/machines/{hid}/jobs").status_code == 404,
                json.dumps({"runs": len(jobs.get("runs") or []), "lends": jobs.get("lends")})[:500])

        # ---- the share where it binds: 5% of the machine (the site's figure, under bob's 20%), two long games
        set5 = bob.put(f"/api/machines/{hid}/settings", json={"capabilities": {"cpu": {**REMOTE_SITE,
                                                                                          "cpu_percent": 5}}})
        pushed = wait_until(lambda: executor().get("cpu_percent") == 5, 20, 0.25)
        held5 = f"qa-share-{tag}"
        alice.post("/api/bots/experiments", json={**LONG_GAME, "name": held5, "games": 2, "seed": 7200})
        both = wait_until(lambda: len(poller.playing(held5)) == 2, 60, 0.25)
        watch = CpuWatch(helper.proc.pid).start()
        time.sleep(8.0)
        share = watch.stop()
        _saves_remove(h, f"lab/queue/{held5}.json")
        playing5 = poller.playing(held5)
        ended = wait_until(lambda: not [s for s in poller.samples[-1:] for x in s["runs"] if x["exp"] == held5],
                           30, 0.25)
        bound5 = share["peak_pct"] is None or limits.get("cpu") != "hard" or (
            share["peak_pct"] <= 5 + 2 and share["mean_pct"] >= 2.5)
        r.check("the share where it binds: at 5% of the machine (pushed to the helper, which restarts its job host "
                "with it) two long games hold the job host's CPU under 5% plus slack where it is held hard, and the "
                "experiment deleted from the queue ends its runs on the helper",
                set5.status_code == 200 and bool(pushed) and bool(both) and bound5 and bool(ended),
                json.dumps({"cpu": share, "runs": playing5}))
        data.update(share_cpu=share)
        bob.put(f"/api/machines/{hid}/settings", json={"capabilities": {"cpu": REMOTE_SITE}})
        wait_until(lambda: executor().get("cpu_percent") == 20 and executor().get("running") == 0, 30, 0.25)

        # ---- yield: the Servers page's test streams for 3 s while a long game plays
        held = f"qa-yield-{tag}"
        alice.post("/api/bots/experiments", json={**LONG_GAME, "name": held, "games": 1, "seed": 7300})
        playing = wait_until(lambda: poller.playing(held), 60, 0.1) or []
        run = playing[0] if playing else ""
        before = len(scripted.completions())
        t_asked = time.time()
        test = bob.post(f"/api/pool/servers/{hid}/test", params={"wait": 30})
        entry = next((q for q in scripted.completions()[before:] if q.get("delay") == YIELD_S and q.get("events")),
                     None)
        done_at = entry["events"][-1] if entry else None
        went_on = wait_until(lambda: next((x for x in poller.of(run) if done_at and x[0] > done_at and not x[2]),
                                          None), 10, 0.05) if run else None
        rows = poller.of(run) if run else []
        holding = [x for x in rows if x[2] and x[0] >= t_asked]
        extended = max((x[4] for x in rows), default=0.0)
        r.check("yield: while the Servers page's test streams at the scripted endpoint (3 s), alice's game playing on "
                "the machine holds (paused, its round the same when held and when let go), goes on within 2 s of the "
                "stream's [DONE], with its paused time reported and the server's deadline pushed on by it",
                test.status_code == 200 and bool(run) and entry is not None and bool(holding)
                and holding[0][0] < done_at and went_on is not None and went_on[0] - done_at < 2.0
                and went_on[1] == holding[-1][1] and went_on[3] > 0 and extended > 0,
                json.dumps({"held_from_first_event_s": round(holding[0][0] - entry["events"][0], 3)
                            if holding and entry else None, "round": holding[-1][1] if holding else None,
                            "went_on_after_done_s": round(went_on[0] - done_at, 3) if went_on and done_at else None,
                            "paused_s": went_on[3] if went_on else None, "extended_s": extended}))
        finished = wait_until(lambda: _lab_view(alice, held).get("state") == "complete", 120, 0.25)
        moved = [x for x in poller.of(run) if went_on and x[0] > went_on[0] and (x[1] or 0) > (went_on[1] or 0)]
        r.check("the held game's round moves again and it ends (its line written)",
                bool(finished) and bool(moved) and len(_lab_lines(h, held)) == 1,
                f"finished {bool(finished)}; rounds reported after it went on: {[x[1] for x in moved][:3]}")

        # ---- with yield_to_llm off: a request holds nothing; a benchmark's lease pauses the games
        no_yield = bob.put(f"/api/machines/{hid}/settings", json={"capabilities": {"cpu": {**REMOTE_SITE,
                                                                                              "yield_to_llm": False}}})
        time.sleep(1.0)
        benched = f"qa-bench-{tag}"
        alice.post("/api/bots/experiments", json={**LONG_GAME, "name": benched, "games": 1, "seed": 7400})
        playing = wait_until(lambda: poller.playing(benched), 60, 0.1) or []
        run = playing[0] if playing else ""
        t_asked = time.time()
        test = bob.post(f"/api/pool/servers/{hid}/test", params={"wait": 30})
        unheld = not [x for x in poller.of(run) if x[2] and x[0] >= t_asked] if run else False
        suite = {"name": f"QA lender bench {tag}", "mode": "sequential", "repeats": 1,
                 "servers": [{"server_id": hid, "models": [{"model": scripted.MODEL}]}],
                 "scenarios": [{"name": "QA duel", "map_size": "duel", "map_type": "pangaea", "seed": 7401,
                                "opponents": 1, "turn_limit": 2, "barbarians": "off", "max_turn_minutes": 5}]}
        started = bob.post("/api/benchmarks/runs", json={"suite": suite, "name": f"QA lender bench {tag}"})
        bench_id = started.json().get("id", "") if started.status_code == 200 else ""
        on = wait_until(lambda: next((s for s in poller.samples if s["benchmark"]), None), 60, 0.05)
        held_b = wait_until(lambda: next((x for x in poller.of(run) if on and x[0] >= on["t"] and x[2]), None),
                            10, 0.05) if run else None
        off_b = wait_until(lambda: next((s for s in poller.samples if on and s["t"] > on["t"]
                                         and not s["benchmark"]), None), 180, 0.1)
        during = [x for x in poller.of(run) if held_b and off_b and held_b[0] <= x[0] < off_b["t"]]
        resumed = wait_until(lambda: next((x for x in poller.of(run) if off_b and x[0] >= off_b["t"] and not x[2]),
                                          None), 10, 0.05) if run else None
        bench = wait_until(lambda: (lambda j: j if j.get("status") in ("done", "failed", "error", "cancelled")
                                    else None)(q3._runs_job(bob, bench_id)), 120, 0.5) if bench_id else None
        bench = bench or (q3._runs_job(bob, bench_id) if bench_id else {})
        r.check("with yield_to_llm off a streamed request holds nothing; a one-job benchmark on the machine sends "
                "jobs_pause when its lease goes live (the game held, its round still, while it lasts) and "
                "jobs_resume at its end (the game goes on within 2 s)",
                no_yield.status_code == 200 and test.status_code == 200 and unheld and started.status_code == 200
                and on is not None and held_b is not None and held_b[0] - on["t"] < 2.0 and off_b is not None
                and bool(during) and all(x[2] and x[1] == held_b[1] for x in during)
                and resumed is not None and resumed[0] - off_b["t"] < 2.0,
                json.dumps({"held_after_lease_s": round(held_b[0] - on["t"], 3) if held_b and on else None,
                            "benchmark_s": round(off_b["t"] - on["t"], 2) if on and off_b else None,
                            "round": held_b[1] if held_b else None,
                            "went_on_after_s": round(resumed[0] - off_b["t"], 3) if resumed and off_b else None,
                            "benchmark_job": bench.get("status")}))
        finished = wait_until(lambda: _lab_view(alice, benched).get("state") == "complete", 120, 0.25)
        settled_b = json.loads((_server_python(h, LAB_STATE, held, benched) or "{}").strip().splitlines()[-1] or "{}")
        paused = {n: [x for j in (settled_b.get(n) or {}).get("jobs") or [] for x in j["runs"]] for n in (held, benched)}
        r.check("both held games end, their paused time in their runs' results (the 3 s stream's; the benchmark's)",
                bool(finished) and all(paused[n] and paused[n][-1]["state"] == "done" for n in (held, benched))
                and paused[held][-1]["paused_s"] >= 0.8 * YIELD_S and paused[benched][-1]["paused_s"] > 0.0,
                json.dumps({n: [(x["state"], x["machine"], x["paused_s"]) for x in v] for n, v in paused.items()}))
        if h.keep:
            # the browser checklist (H1, H2, H7): the machine stays connected, lending
            bob.put(f"/api/machines/{hid}/settings", json={"capabilities": {"cpu": REMOTE_SITE}})
            helpers.clear()
        r.check("no traceback in the server's log", h.server.tracebacks() == 0, f"{h.server.tracebacks()} traceback(s)")
    finally:
        if poller is not None:
            poller.stop()
        for helper in helpers:
            helper.stop()
        if not (h.keep and not helpers):
            scripted.close()
        try:
            _verification(h, trusted_rate=0.02)        # the default again, for the parts after this one
        except Exception:
            pass


def _lab_lines(h, name: str, folder: str = "results") -> list:
    text = _saves_text(h, f"lab/{folder}/{name}.jsonl") or ""
    return [json.loads(x) for x in text.splitlines() if x.strip()]


# ---------------------------------------------------------------------------- the server in WSL (--wsl)

class WslUnreachable(Exception):
    """The WSL server answers inside WSL, but Windows cannot reach it through localhost forwarding."""


class WslServer:
    """The dev server in WSL (P4.7.6's topology for ``remote --wsl`` and ``exit``), with ``q3.Server``'s interface:
    started from ``root`` (a checkout in WSL) with ``python`` (its venv's, the extension ``cargo xtask develop`` built
    there), its state in ``~/citar-qa/<the directory's name>`` inside WSL (emptied first), its log and the helpers'
    homes in the Windows directory. It binds ``0.0.0.0`` inside WSL only when WSL's networking is NAT (the Linux VM's
    own interface, reached from Windows only through ``localhost``), else ``127.0.0.1``. Everything in WSL goes
    through ``wsl -u <user> -e`` (no shell, and as the user whose home it is: WSL's default user may not read it):
    ``admin_cli`` and the parts' readers with the instance's environment (``cli``, ``python``), and its files
    (``read``, ``remove``, written with ``_put``), each as bytes so no line ending is changed on the way."""

    topology = "wsl"

    def __init__(self, state: Path, port: int, *, distro: str, user: str, root: str, python: str):
        import posixpath
        self.state, self.port = state, port
        self.distro, self.user, self.root, self.python_bin = distro, user, root, python
        self.remote = posixpath.join("/home", user, "citar-qa", state.name)
        self.base = f"http://127.0.0.1:{port}"
        self.public = f"http://localhost:{port}"
        self.env: dict = {}
        self.log = state / "server.log"
        self.proc = None
        self.mode = self._wsl("wslinfo", "--networking-mode").stdout.strip() or "unknown"
        self.host = "0.0.0.0" if self.mode == "nat" else "127.0.0.1"

    def _wsl(self, *args: str, stdin: Optional[str] = None, timeout: float = 180):
        """``args`` run in WSL as its user, with no shell; ``stdin`` and the output as UTF-8 bytes on the way, so a
        line ending is never translated."""
        import subprocess
        got = subprocess.run(["wsl", "-d", self.distro, "-u", self.user, "-e", *args],
                             input=None if stdin is None else stdin.encode("utf-8"), capture_output=True,
                             timeout=timeout)
        return subprocess.CompletedProcess(got.args, got.returncode, got.stdout.decode("utf-8", "replace"),
                                           got.stderr.decode("utf-8", "replace"))

    def _put(self, rel: str, text: str) -> None:
        """Write ``text`` to ``rel`` in the WSL state."""
        done = self._wsl("bash", "-c", 'cat > "$1"', "put", f"{self.remote}/{rel}", stdin=text)
        if done.returncode != 0:
            raise q3.HarnessError(f"could not write {rel} in WSL: {done.stderr.strip()[-300:]}")

    def read(self, rel: str) -> Optional[str]:
        """A file of the WSL state (``saves/lab/results/x.jsonl``, say), or None when it is not there."""
        got = self._wsl("cat", f"{self.remote}/{rel}", timeout=60)
        return got.stdout if got.returncode == 0 else None

    def remove(self, rel: str) -> None:
        self._wsl("rm", "-f", f"{self.remote}/{rel}", timeout=60)

    def _scripts(self) -> None:
        """``run.sh`` (the instance's environment, then the venv's Python with the arguments), ``serve.sh`` (the
        server, its process id kept) and ``stop.sh`` (that process signalled), in the WSL state."""
        import posixpath
        dev = _dev_server_module()
        env = dev.instance_env(_PosixBase(self.remote), self.port, host=self.host, server=True,
                               registration="invite", environ={})
        lines = ["#!/bin/bash", f"cd '{self.root}'"]
        lines += [f"export {k}='{v}'" for k, v in sorted(env.items())]
        lines += ["export PYTHONUNBUFFERED=1", f"exec '{self.python_bin}' \"$@\""]
        self._put("run.sh", "\n".join(lines) + "\n")
        pid = posixpath.join(self.remote, "server.pid")
        self._put("stop.sh", "#!/bin/bash\n"
                  f"p=$(cat '{pid}' 2>/dev/null) || exit 0\n"
                  "kill -\"${1:-TERM}\" \"$p\" 2>/dev/null\n"
                  "for i in $(seq 1 100); do kill -0 \"$p\" 2>/dev/null || exit 0; sleep 0.1; done\n"
                  "kill -KILL \"$p\" 2>/dev/null; exit 0\n")
        self._put("serve.sh", "#!/bin/bash\n"
                  f"echo $$ > '{pid}'\n"
                  f"exec bash '{self.remote}/run.sh' scripts/dev_server.py --server --registration invite "
                  f"--host {self.host} --port {self.port} --dir '{self.remote}'\n")

    def prepare(self) -> None:
        """The WSL state emptied and made, and the scripts written."""
        made = self._wsl("bash", "-c", 'rm -rf "$1" && mkdir -p "$1/config"', "prepare", self.remote)
        if made.returncode != 0:
            raise q3.HarnessError(f"the WSL state {self.remote} could not be made: {made.stderr.strip()[-300:]}")
        self._scripts()

    def start(self, timeout: float = 180.0) -> None:
        """Start it (the registry file the harness wrote copied in first) and wait until Windows reaches it."""
        import subprocess
        import httpx
        local = self.state / "config" / "servers.json"
        if local.exists():
            self._put("config/servers.json", local.read_text(encoding="utf-8"))
        out = open(self.log, "w", encoding="utf-8")
        self.proc = subprocess.Popen(["wsl", "-d", self.distro, "-u", self.user, "-e", "bash",
                                      f"{self.remote}/serve.sh"], stdout=out, stderr=subprocess.STDOUT,
                                     stdin=subprocess.DEVNULL)
        out.close()
        t0 = time.perf_counter()
        inside = False
        with httpx.Client(base_url=self.base, timeout=5.0) as c:
            while True:
                try:
                    if c.get("/api/auth/config").status_code == 200:
                        return
                except httpx.HTTPError:
                    pass
                if not inside and time.perf_counter() - t0 > 10:
                    probe = self._wsl("curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                                      f"http://127.0.0.1:{self.port}/api/auth/config", timeout=30)
                    inside = probe.stdout.strip() == "200"
                if self.proc.poll() is not None:
                    raise q3.HarnessError(f"the WSL server did not start (exit {self.proc.poll()}); see {self.log}")
                if time.perf_counter() - t0 > timeout:
                    if inside:
                        raise WslUnreachable(f"the WSL server answers inside WSL on {self.port}, but Windows "
                                             f"cannot reach {self.base} ({self.mode} networking)")
                    raise q3.HarnessError(f"the WSL server did not answer within {timeout:.0f} s; see {self.log}")
                time.sleep(0.25)

    def cli(self, *args: str, stdin: Optional[str] = None):
        """``python -m citar.server.admin_cli`` on the instance's database, in WSL."""
        return self._wsl("bash", f"{self.remote}/run.sh", "-m", "citar.server.admin_cli", *args, stdin=stdin)

    def python(self, *args: str):
        """``python args`` in WSL with the instance's environment; ``-c CODE`` goes through a file in the WSL state."""
        import hashlib
        args = list(args)
        if len(args) >= 2 and args[0] == "-c":
            name = f"qa-snippet-{hashlib.sha256(args[1].encode()).hexdigest()[:12]}.py"
            self._put(name, args[1])
            args = [f"{self.remote}/{name}", *args[2:]]
        return self._wsl("bash", f"{self.remote}/run.sh", *args)

    def stop(self, signal: str = "TERM") -> None:
        if self.proc is None:
            return
        self._wsl("bash", f"{self.remote}/stop.sh", signal, timeout=60)
        try:
            self.proc.wait(timeout=30)
        except Exception:
            self.proc.kill()

    def restart(self, timeout: float = 180.0) -> None:
        """Stopped as a crash would stop it (``SIGKILL``) and started again on the same state and port."""
        self.stop("KILL")
        kept = self.state / f"server-{len(list(self.state.glob('server-*.log'))) + 1}.log"
        if self.log.exists():
            self.log.replace(kept)
        self.start(timeout)

    def tracebacks(self) -> int:
        logs = [self.log, *sorted(self.state.glob("server-*.log"))]
        return sum(p.read_text(encoding="utf-8", errors="replace").count("Traceback") for p in logs if p.exists())


class _Reachable:
    """The WSL server, or, once Windows cannot reach it (``WslUnreachable``), the server on Windows in its place: the
    fallback P4.7.6 names, recorded in ``fallback["why"]``. Everything else is the server's."""

    def __init__(self, wsl: WslServer, plain, fallback: dict):
        self.__dict__["_server"], self.__dict__["_plain"], self.__dict__["_fallback"] = wsl, plain, fallback

    def start(self, timeout: float = 180.0) -> None:
        server = self.__dict__["_server"]
        try:
            server.start(timeout)
        except WslUnreachable as e:
            server.stop("KILL")
            self.__dict__["_fallback"]["why"] = str(e)
            windows = self.__dict__["_plain"](server.state, server.port)
            self.__dict__["_server"] = windows
            windows.start()

    def __getattr__(self, name):
        return getattr(self.__dict__["_server"], name)

    def __setattr__(self, name, value):
        setattr(self.__dict__["_server"], name, value)


class _PosixBase(str):
    """A POSIX path that ``dev_server.instance_env`` can join and print (``/``, ``as_posix``) on Windows."""

    def __truediv__(self, other: str) -> "_PosixBase":
        return _PosixBase(self.rstrip("/") + "/" + other)

    def as_posix(self) -> str:
        return str(self)


def _dev_server_module():
    spec = importlib.util.spec_from_file_location("qa4_dev_server", ROOT / "scripts" / "dev_server.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# ---------------------------------------------------------------------------- the verify part (4-11; 4-12 completes it)

#: What the instance's database says of machines and accounts (their tiers, the trust audit rows of each).
TRUST_STATE = """
import json, sys
from sqlalchemy import select
from citar import db
from citar.db.models import AuditLog, Server, User
db.configure()
out = {"machines": {}, "users": {}}
with db.session() as s:
    for sid in sys.argv[1].split(","):
        row = s.get(Server, sid)
        if row is None:
            continue
        audits = [a.detail.get("changed") for a in s.scalars(select(AuditLog).where(
            AuditLog.action == "server.trust", AuditLog.object_id == sid).order_by(AuditLog.id))]
        out["machines"][sid] = {"tier": row.trust_tier, "trust": row.trust or {}, "audits": audits}
    for uid in sys.argv[2].split(","):
        u = s.get(User, uid)
        if u is not None:
            out["users"][uid] = u.trust_tier
print(json.dumps(out, default=str))
"""


def _verification(h, **values) -> dict:
    """The instance's verification settings, ``values`` over what it has (an administrator's ``PUT
    /api/admin/verification``, package 4-12); what is in force."""
    admin = h.accounts["admin"]
    got = admin.put("/api/admin/verification", json=values) if values else admin.get("/api/admin/verification")
    if got.status_code != 200:
        raise q3.HarnessError(f"the verification settings: HTTP {got.status_code} {got.text[:300]}")
    return got.json()["settings"]


def _trust_state(h, machines: list, users: list) -> dict:
    out = _server_python(h, TRUST_STATE, ",".join(machines), ",".join(users)).strip().splitlines()
    return json.loads(out[-1]) if out else {"machines": {}, "users": {}}


def faults_binary() -> str:
    """``citar-helper-faults`` beside the helper (``cargo xtask develop`` builds it: the helper with the
    ``test-faults`` feature, which cheats on purpose, P4.4.6)."""
    helper = Path(q3.helper_binary())
    for name in ("citar-helper-faults.exe", "citar-helper-faults"):
        path = helper.with_name(name)
        if path.is_file():
            return str(path)
    raise q3.HarnessError("no citar-helper-faults beside the helper: run cargo xtask develop")


def part_verify(h) -> None:
    """The ``verify`` part (the module's doc)."""
    r = h.report
    admin, alice, bob, mod = (h.accounts[k] for k in ("admin", "alice", "bob", "mod"))
    data = r.data.setdefault("verify", {})
    tag = h.tag
    host = "sv_qa_host"
    share = max(1, min(100, 200 // (os.cpu_count() or 1)))
    scripted = FakeEndpoint(delay=0.05)
    helpers: dict = {}
    duel = {"profiles": ["standard", "classic-production"], "players": 2, "size": "duel", "maps": ["pangaea"],
            "speed": "Quick", "turns": 30}
    try:
        binary = faults_binary()
        ids = {who: h.accounts[who].get("/api/auth/me").json()["user"]["id"] for who in ("alice", "bob")}
        host_cpu = {"enabled": True, "threads": 1, "cpu_percent": share, "memory_mb": 400, "work": ["lab_game"]}
        put = admin.put(f"/api/machines/{host}/settings", json={"capabilities": {"cpu": host_cpu}})
        caps = [admin.patch(f"/api/admin/users/{ids[w]}", json={"caps": {"run_lab": True}}) for w in ids]
        defaults = _verification(h)
        r.check("this server's job host lends one thread (its verifier), alice and bob run lab experiments, and the "
                "verification settings are the defaults (P4.5.8)",
                put.status_code == 200 and all(c.status_code == 200 for c in caps)
                and defaults.get("new_streak") == 5 and defaults.get("trusted_rate") == 0.02,
                json.dumps({"settings": defaults}))

        def machine(who, label: str) -> tuple:
            """A machine of ``who``'s in a group of theirs, its helper machine registered and a token issued."""
            group = who.post("/api/pool/groups", json={"name": f"QA verify {label} {tag}"})
            gid = group.json().get("id", "") if group.status_code == 200 else ""
            made = who.post("/api/pool/servers", json={"config": {"name": f"QA {label} {tag}", "kind": "owned",
                                                                  "connection": {"provider": "openai_compatible",
                                                                                 "max_parallel": 1}},
                                                       "reach": "worker", "group_id": gid})
            sid = made.json().get("id", "") if made.status_code == 200 else ""
            issued = who.post(f"/api/pool/servers/{sid}/workers/tokens", json={"label": label}) if sid else None
            token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
            who.put(f"/api/machines/{sid}/settings", json={"capabilities": {"cpu": REMOTE_SITE}})
            return sid, token, gid

        def start(label: str, sid: str, token: str, *modes: str):
            home = h.server.state / f"helper-home-qa-verify-{label}"
            home.mkdir(parents=True, exist_ok=True)
            # one thread at 20% of the machine: a game is one thread, so it plays at a core's speed here (12 CPUs)
            ceiling = {**REMOTE_CEILING, "threads": 1}
            (home / "helper.json").write_text(json.dumps({"cpu": ceiling}), encoding="utf-8")
            extra = ("--no-llm", *(x for m in modes for x in ("--test-fault", m)))
            helper = Helper(h, token, scripted, f"qa-verify-{label}", machine=f"qa-verify-{label}",
                            binary=binary if modes else None, extra=extra)
            helpers[label] = helper
            return wait_until(lambda: executor(sid).get("state") == "ready", 120, 0.25)

        def executor(sid: str) -> dict:
            got = admin.get("/api/admin/jobs")
            return next((e for e in ((got.json().get("dispatcher") or {}).get("executors") or [])
                         if e.get("machine") == sid), {}) if got.status_code == 200 else {}

        def tier(sid: str) -> str:
            got = admin.get(f"/api/machines/{sid}/jobs")
            return (got.json().get("machine") or {}).get("trust_tier", "") if got.status_code == 200 else ""

        def runs_of(sid: str, who=admin) -> list:
            got = who.get(f"/api/machines/{sid}/jobs", params={"limit": 200})
            return got.json().get("runs") or [] if got.status_code == 200 else []

        def queue(who, name: str, games: int, seed: int, turns: int = 30):
            return who.post("/api/bots/experiments", json={**duel, "name": name, "games": games, "seed": seed,
                                                           "turns": turns})

        # ---- a new machine whose helper alters its results (--test-fault result:1): caught at its first job
        a_sid, a_token, b_gid = machine(bob, "cheat")
        ready = start("cheat", a_sid, a_token, "result:1")
        cheat = f"qa-verify-cheat-{tag}"
        queued = queue(bob, cheat, 6, 8100)
        caught = wait_until(lambda: tier(a_sid) == "quarantined", 120, 0.25)
        state = _trust_state(h, [a_sid], [ids["bob"]])
        ev = (state["machines"].get(a_sid) or {}).get("trust", {}).get("last_mismatch") or {}
        done = wait_until(lambda: _lab_view(bob, cheat).get("state") == "complete", 120, 0.5)
        lines = _lines(h, cheat)
        a_runs = [x for x in runs_of(a_sid) if x.get("state") == "done"]

        def a_events_now():
            got = json.loads((_server_python(h, LAB_STATE, cheat) or "{}").strip().splitlines()[-1] or "{}")
            evs = [e for e in (got.get(cheat) or {}).get("events") or [] if e["machine"] == a_sid]
            return evs if evs and all(e["verification"] == "rejected" for e in evs) else None
        a_events = wait_until(a_events_now, 30, 1.0) or []      # the ledger's writer writes them in batches
        r.check("a new machine whose helper alters a score of every result (--test-fault result:1) is quarantined "
                "at its first job: \"same chain, different result: tampering or an incompatible build\", checked "
                "against this server's replica; its result never reaches the results file, its event is rejected "
                "with no credits, its owner (bob) is quarantined; the experiment ends on this server",
                bool(ready) and queued.status_code == 200 and bool(caught) and ev.get("verdict") == "result"
                and ev.get("words") == "same chain, different result: tampering or an incompatible build"
                and len(a_runs) == 1 and a_runs[0]["run"] not in {(x.get("executor") or {}).get("run") for x in lines}
                and bool(done) and sorted(x["i"] for x in lines) == list(range(6))
                and {(x["executor"]["machine"], x["executor"]["verification"]) for x in lines}
                == {(host, "reference")}
                and a_events and all(e["verification"] == "rejected" and e["flags"].get("rejected") for e in a_events)
                and state["users"].get(ids["bob"]) == "quarantined",
                json.dumps({"evidence": ev, "a_runs": len(a_runs), "events": a_events})[:700])
        data.update(cheat_machine=a_sid, cheat_evidence=ev)

        # ---- bob's second machine, an honest helper, is refused CPU work while bob is quarantined
        b_sid, b_token, _gid = machine(bob, "other")
        ready_b = start("other", b_sid, b_token)
        refused_exp = f"qa-verify-refused-{tag}"
        queue(bob, refused_exp, 2, 8200)
        done_b = wait_until(lambda: _lab_view(bob, refused_exp).get("state") == "complete", 120, 0.5)
        r.check("bob's second machine, an honest helper, is refused CPU work while its owner is quarantined (his next "
                "experiment plays on this server alone)",
                bool(ready_b) and bool(done_b) and runs_of(b_sid) == [],
                json.dumps({"b_runs": runs_of(b_sid)}))

        # ---- quarantine refuses its work for other members, its owner's own still served (admission's code)
        grant = bob.post(f"/api/pool/groups/{b_gid}/grants", json={"subject_type": "user", "handle": alice.handle,
                                                                  "purposes": []})
        other = alice.post(f"/api/pool/servers/{a_sid}/test")
        own = bob.post(f"/api/pool/servers/{a_sid}/test")
        r.check("the quarantined machine serves nobody but its owner: alice's pooled prompt on it (under bob's grant) "
                "is refused at admission with code quarantined; bob's own is not",
                grant.status_code == 200 and other.status_code == 200
                and (other.json().get("stage"), other.json().get("code")) == ("admission", "quarantined")
                and own.status_code == 200 and own.json().get("code") != "quarantined",
                json.dumps({"alice": other.json() if other.status_code == 200 else other.status_code,
                            "bob": own.json() if own.status_code == 200 else own.status_code})[:500])

        # ---- released: the machine back to new with its counters reset and since now; the owner released
        helpers.pop("cheat").stop()
        rel = admin.put(f"/api/admin/machines/{a_sid}/trust", json={"trust_tier": "new"})
        rel_owner = admin.patch(f"/api/admin/users/{ids['bob']}", json={"trust_tier": "new"})
        state = _trust_state(h, [a_sid], [ids["bob"]])
        trust_a = (state["machines"].get(a_sid) or {}).get("trust") or {}
        again = f"qa-verify-again-{tag}"
        queue(bob, again, 4, 8300)
        played = wait_until(lambda: runs_of(b_sid), 120, 0.25)
        wait_until(lambda: _lab_view(bob, again).get("state") == "complete", 120, 0.5)
        r.check("an administrator releases the machine (back to new: its counters reset, since now) and its owner; "
                "bob's honest machine then plays his games again",
                rel.status_code == 200 and rel_owner.status_code == 200
                and (state["machines"].get(a_sid) or {}).get("tier") == "new"
                and trust_a.get("mismatches") == 0 and bool(trust_a.get("since"))
                and state["users"].get(ids["bob"]) == "new" and bool(played),
                json.dumps({"trust": trust_a, "b_runs": len(runs_of(b_sid))})[:500])
        helpers.pop("other").stop()

        # ---- an honest helper promoted new -> provisional -> trusted (thresholds lowered: 2 and 4)
        _verification(h, new_streak=2, promote_verified=4, provisional_rate=1.0, trusted_rate=1.0)
        c_sid, c_token, _cg = machine(alice, "honest")
        ready_c = start("honest", c_sid, c_token)
        climb = f"qa-verify-climb-{tag}"
        queue(alice, climb, 16, 8400)
        trusted = wait_until(lambda: tier(c_sid) == "trusted", 180, 0.5)
        wait_until(lambda: _lab_view(alice, climb).get("state") == "complete", 180, 0.5)
        state = _trust_state(h, [c_sid], [])
        audits = (state["machines"].get(c_sid) or {}).get("audits") or []
        lines = _lines(h, climb)
        mine = [x for x in lines if x["executor"]["machine"] == c_sid]
        r.check("an honest new helper (thresholds lowered to 2 and 4, every result of a provisional machine "
                "sampled) goes new -> provisional -> trusted, an audit row each; its results in the file are "
                "verified by this server's replicas",
                bool(ready_c) and bool(trusted)
                and audits == [{"from": "new", "to": "provisional"}, {"from": "provisional", "to": "trusted"}]
                and bool(mine) and all(x["executor"]["verification"] == "verified" for x in mine),
                json.dumps({"audits": audits, "verified": (state["machines"][c_sid]["trust"] or {}).get("verified"),
                            "lines": len(mine)}))
        helpers.pop("honest").stop()
        _verification(h, **{k: defaults[k] for k in ("new_streak", "promote_verified", "provisional_rate",
                                                     "trusted_rate")})

        # ---- a provisional machine that alters chosen jobs (--test-fault result-at:2,4), its results unsampled
        # (rate 0): caught by an administrator's replicate, everything unverified it gave withdrawn
        _verification(h, provisional_rate=0.0)
        d_sid, d_token, _dg = machine(bob, "selective")
        prov = admin.put(f"/api/admin/machines/{d_sid}/trust", json={"trust_tier": "provisional"})
        off = admin.put(f"/api/machines/{host}/settings", json={"capabilities": {"cpu": {**host_cpu,
                                                                                         "enabled": False}}})
        ready_d = start("selective", d_sid, d_token, "result-at:2,4")
        first = f"qa-verify-first-{tag}"
        queue(bob, first, 3, 8500)
        done_first = wait_until(lambda: _lab_view(bob, first).get("state") == "complete", 120, 0.5)
        before = _lines(h, first)
        r.check("a provisional machine (an administrator's tier; its results unsampled at rate 0) plays bob's three "
                "games with this server lending nothing: three lines, each its own and unverified (its second "
                "altered, unnoticed)",
                prov.status_code == 200 and off.status_code == 200 and bool(ready_d) and bool(done_first)
                and {(x["executor"]["machine"], x["executor"]["verification"]) for x in before}
                == {(d_sid, "unverified")} and len(before) == 3,
                json.dumps([x["executor"] for x in before])[:500])
        on = admin.put(f"/api/machines/{host}/settings", json={"capabilities": {"cpu": host_cpu}})
        rep = admin.post(f"/api/admin/machines/{d_sid}/replicate", json={"n": 1})
        second = f"qa-verify-second-{tag}"
        queue(bob, second, 4, 8600)
        caught_d = wait_until(lambda: tier(d_sid) == "quarantined", 120, 0.25)
        replayed = wait_until(lambda: (lambda xs: xs if len(xs) == 3 and all(
            x["executor"]["machine"] == host for x in xs) else None)(_lines(h, first)), 180, 0.5)
        wait_until(lambda: _lab_view(bob, second).get("state") == "complete", 180, 0.5)
        state = _trust_state(h, [d_sid], [ids["bob"]])
        ev_d = (state["machines"].get(d_sid) or {}).get("trust", {}).get("last_mismatch") or {}
        withdrawn = {w["run"] for w in ev_d.get("withdrawn_jobs") or []}
        old_runs = {x["executor"]["run"] for x in before}
        settled = json.loads((_server_python(h, LAB_STATE, first) or "{}").strip().splitlines()[-1] or "{}")
        old_events = [e for e in (settled.get(first) or {}).get("events") or [] if e["request"] in old_runs]
        view = alice.get(f"/api/lab/jobs?exp={first}")
        mine = bob.get(f"/api/lab/jobs?exp={first}")
        listed = mine.json().get("jobs") or [] if mine.status_code == 200 else []
        r.check("an administrator asks for its next result's replica (POST .../replicate): its fourth job, altered, "
                "mismatches this server's; it is quarantined, and every result it gave unverified (the first three, "
                "the altered one among them) is withdrawn: their lines leave the results file, their events are "
                "rejected, and their games are played again on this server and accepted (bob sees each game done "
                "once, by this server)",
                on.status_code == 200 and rep.status_code == 200 and bool(caught_d)
                and ev_d.get("verdict") == "result" and old_runs <= withdrawn and bool(replayed)
                and len(old_events) == 3 and all(e["verification"] == "rejected" for e in old_events)
                and view.status_code == 404 and sorted(j["i"] for j in listed) == [0, 1, 2]
                and all(j["state"] == "done" and (j.get("executor") or {}).get("name") == "this server"
                        for j in listed),
                json.dumps({"evidence": {k: ev_d.get(k) for k in ("verdict", "words", "withdrawn")},
                            "listed": [(j["i"], j["state"]) for j in listed]})[:700])
        data.update(selective_machine=d_sid, selective_withdrawn=sorted(withdrawn))
        helpers.pop("selective").stop()

        # ---- the routes are administrators'
        trust_path, rep_path = f"/api/admin/machines/{d_sid}/trust", f"/api/admin/machines/{d_sid}/replicate"
        statuses = {who: (h.accounts[who].put(trust_path, json={"verifier": False}).status_code,
                          h.accounts[who].post(rep_path, json={"n": 1}).status_code) for who in ("bob", "alice", "mod")}
        r.check("PUT /api/admin/machines/{id}/trust and POST .../replicate are administrators': its owner, another "
                "member and a moderator are told \"not found\"",
                all(v == (404, 404) for v in statuses.values()), json.dumps(statuses))
        _verification(h, provisional_rate=defaults["provisional_rate"])

        # ---- 4-12: the verification settings' routes and their page's data (#/admin/verification)
        page = admin.get("/api/admin/verification")
        members = {who: (h.accounts[who].get("/api/admin/verification").status_code,
                         h.accounts[who].put("/api/admin/verification", json={"trusted_rate": 0.5}).status_code)
                   for who in ("alice", "bob", "mod")}
        bounds = [admin.put("/api/admin/verification", json=b).status_code
                  for b in ({"trusted_rate": 1.5}, {"new_streak": 0}, {"new_streak": 2.5}, {"nope": 1}, {})]
        view = page.json() if page.status_code == 200 else {}
        quarantined = {q["machine"]: q for q in (view.get("quarantined") or {}).get("machines") or []}
        owners = {u["handle"] for u in (view.get("quarantined") or {}).get("owners") or []}
        mismatched = {m["machine"] for m in view.get("mismatches") or []}
        withdrawn_exps = {w["exp"] for w in view.get("withdrawals") or []}
        r.check("GET and PUT /api/admin/verification are administrators' (bob, alice and a moderator are told \"not "
                "found\") and bounded (a rate past 1, a count of 0 or a fraction, an unknown name, an empty body: 400, "
                "nothing changed); the page's data names the selective machine quarantined with its evidence and "
                "withdrawn jobs, bob quarantined, both cheats' mismatches, the withdrawn games and this server as a "
                "verifier",
                page.status_code == 200 and all(v == (404, 404) for v in members.values())
                and bounds == [400] * 5 and _verification(h)["trusted_rate"] == defaults["trusted_rate"]
                and d_sid in quarantined and quarantined[d_sid]["evidence"].get("verdict") == "result"
                and len(quarantined[d_sid]["evidence"].get("withdrawn_jobs") or []) >= 3 and bob.handle in owners
                and {a_sid, d_sid} <= mismatched and first in withdrawn_exps
                and any(v["kind"] == "local" and v["machine"] == host for v in view.get("verifiers") or []),
                json.dumps({"members": members, "bounds": bounds,
                            "quarantined": {k: (v["evidence"] or {}).get("words") for k, v in quarantined.items()},
                            "owners": sorted(owners), "withdrawals": sorted(withdrawn_exps)})[:700])
        admin.patch(f"/api/admin/users/{ids['bob']}", json={"trust_tier": "new"})

        # ---- 4-12: credits bounded by the reference cost (P4.5.7); the class check
        _verification(h, new_streak=1, promote_verified=2, provisional_rate=1.0)

        def primaries(name: str, sid: str) -> list:
            """The machine's primary runs' events of experiment ``name`` once settled (``verified``)."""
            state = _lab_state(h, name).get(name) or {}
            runs = {x["id"] for j in state.get("jobs") or [] for x in j["runs"] if x["machine"] == sid}
            return [e for e in state.get("events") or [] if e["request"] in runs and e["verification"] == "verified"
                    and e["machine"] == sid]

        def admin_trust(sid: str) -> dict:
            got = admin.get(f"/api/machines/{sid}/jobs")
            return (got.json().get("trust") or {}) if got.status_code == 200 else {}

        o_sid, o_token, _og = machine(alice, "over")
        ready_o = start("over", o_sid, o_token, "cpu:3")
        over = f"qa-verify-over-{tag}"
        queue(alice, over, 8, 8700, turns=300)          # this server plays some of them: enough for both
        settled_o = wait_until(lambda: len(primaries(over, o_sid)) >= 3, 180, 1.0)
        events_o = primaries(over, o_sid)
        trust_o = admin_trust(o_sid)
        r.check("a helper reporting three times its CPU (--test-fault cpu:3, 300-round duels at a core's speed): each "
                "game credited at most 1.5 times its replica's CPU (the reference) and what the server observed, "
                "flagged overreported_cpu on the machine, which its matches make provisional and never trusted "
                "(promote_verified 2 passed)",
                bool(ready_o) and bool(settled_o) and tier(o_sid) == "provisional"
                and int(trust_o.get("verified") or 0) >= 2
                and int(((trust_o.get("flags") or {}).get("overreported_cpu") or {}).get("count") or 0) >= 1
                and any(e["flags"].get("overreported_cpu") for e in events_o)
                and all(e["cpu"] <= 1.5 * e["flags"].get("reference_s", 0) + 1e-6
                        and e["cpu"] <= e["observed"] * 1.05 + 1 + 1e-6 for e in events_o),
                json.dumps({"tier": tier(o_sid), "verified": trust_o.get("verified"),
                            "events": [{k: e.get(k) for k in ("reported", "observed", "cpu")}
                                       | {"reference": e["flags"].get("reference_s"),
                                          "flags": sorted(k for k in e["flags"] if k != "reference_s")}
                                       for e in events_o]})[:900])
        data.update(over_machine=o_sid, over_events=[{k: e.get(k) for k in ("reported", "observed", "cpu")}
                                                     | {"reference": e["flags"].get("reference_s")} for e in events_o])
        wait_until(lambda: _lab_view(alice, over).get("state") == "complete", 180, 0.5)
        helpers.pop("over").stop()

        hd_sid, hd_token, _hg = machine(alice, "hold")
        ready_h = start("hold", hd_sid, hd_token, "hold:2")
        held = f"qa-verify-hold-{tag}"
        queue(alice, held, 8, 8800, turns=300)
        promoted_h = wait_until(lambda: tier(hd_sid) == "trusted", 180, 0.5)
        settled_h = wait_until(lambda: len(primaries(held, hd_sid)) >= 2, 60, 1.0)
        events_h = primaries(held, hd_sid)
        trust_h = admin_trust(hd_sid)
        flags_h = trust_h.get("flags") or {}
        r.check("a helper holding each result two seconds and reporting the hold as CPU (--test-fault hold:2): each "
                "game credited 1.5 times its replica's CPU, flagged capped_cpu only (no overreported_cpu: the server "
                "observed the hold), and promoted to trusted",
                bool(ready_h) and bool(promoted_h) and bool(settled_h) and "overreported_cpu" not in flags_h
                and int((flags_h.get("capped_cpu") or {}).get("count") or 0) >= 2
                and all(e["flags"].get("capped_cpu") and not e["flags"].get("overreported_cpu")
                        and abs(e["cpu"] - 1.5 * e["flags"].get("reference_s", 0)) < 1e-5 for e in events_h),
                json.dumps({"tier": tier(hd_sid), "flags": sorted(flags_h),
                            "events": [{k: e.get(k) for k in ("reported", "observed", "cpu")}
                                       | {"reference": e["flags"].get("reference_s")} for e in events_h]})[:900])
        wait_until(lambda: _lab_view(alice, held).get("state") == "complete", 180, 0.5)
        helpers.pop("hold").stop()

        ms_sid, ms_token, _mg = machine(alice, "measured")
        ready_m = start("measured", ms_sid, ms_token)
        measured = f"qa-verify-measured-{tag}"
        queue(alice, measured, 6, 8900, turns=300)
        timed = wait_until(lambda: admin_trust(ms_sid).get("speed") is not None and len(
            primaries(measured, ms_sid)) >= 2, 180, 1.0)
        suspect = ((admin_trust(o_sid).get("flags") or {}).get("class_suspect") or {})
        r.check("the class check: an honest machine measured beside the over-reporter (both of the default CPU class) "
                "leaves the over-reporter, under half the class's median speed, flagged class_suspect, and itself not",
                bool(ready_m) and bool(timed) and suspect.get("speed") is not None
                and suspect["speed"] < 0.5 * suspect.get("class_median", 0)
                and "class_suspect" not in (admin_trust(ms_sid).get("flags") or {}),
                json.dumps({"over": admin_trust(o_sid).get("speed"), "honest": admin_trust(ms_sid).get("speed"),
                            "suspect": suspect}))
        wait_until(lambda: _lab_view(alice, measured).get("state") == "complete", 180, 0.5)
        helpers.pop("measured").stop()
        _verification(h, **{k: defaults[k] for k in ("new_streak", "promote_verified", "provisional_rate")})

        # ---- 4-12: model results by their machine's tier (P4.5.8 rule 11): a benchmark on bob's provisional machine
        lm_sid, lm_token, _lg = machine(bob, "models")
        prov_lm = admin.put(f"/api/admin/machines/{lm_sid}/trust", json={"trust_tier": "provisional"})
        lm = Helper(h, lm_token, scripted, "qa-verify-models", machine="qa-verify-models")
        helpers["models"] = lm
        lm_online = wait_until(lambda: q3._online(bob, lm_sid), 60, 0.25)
        suite = {"name": f"QA verify tiers {tag}", "mode": "sequential", "repeats": 1,
                 "servers": [{"server_id": lm_sid, "models": [{"model": scripted.MODEL}]}],
                 "scenarios": [{"name": "QA tiers duel", "map_size": "duel", "map_type": "pangaea", "seed": 4121,
                                "opponents": 1, "turn_limit": 2, "barbarians": "off", "max_turn_minutes": 5}]}
        bench = bob.post("/api/benchmarks/runs", json={"suite": suite, "name": f"QA verify tiers {tag}"})
        bench_id = bench.json().get("id", "") if bench.status_code == 200 else ""
        wait_until(lambda: q3._runs_job(bob, bench_id).get("status") in ("done", "failed"), 180, 0.5)
        job_view = q3._runs_job(bob, bench_id)
        run_view = next((x for x in (bob.get("/api/benchmarks/runs").json() or []) if x.get("id") == bench_id), {})

        def row_of(who, model):
            got = who.get("/api/models/scores")
            body = got.json() if got.status_code == 200 else {}
            return next((x for x in body.get("models") or [] if x["model"] == model), None), body
        model = scripted.MODEL
        a_row, a_body = row_of(admin, model)
        al_row, al_body = row_of(alice, model)
        b_row, _b_body = row_of(bob, model)
        left = {u["model"]: u["games"] for u in a_body.get("unverified") or []}
        r.check("a benchmark game on bob's provisional helper machine (its model through the scripted endpoint) is "
                "left out of the administrators' and alice's model scores and counted in their unverified list, "
                "badged unverified machine on bob's own board and on his run and job",
                prov_lm.status_code == 200 and bool(lm_online) and bench.status_code == 200
                and job_view.get("status") == "done" and a_row is None and left.get(model, 0) >= 1
                and al_row is None and b_row is not None
                and any(g.get("unverified_machine") for g in b_row.get("games") or [])
                and run_view.get("unverified_machine") is True and job_view.get("unverified_machine") is True,
                json.dumps({"job": job_view.get("status"), "error": job_view.get("error"),
                            "admin_unverified": a_body.get("unverified"), "alice_row": al_row is not None,
                            "bob_games": [(g.get("game"), g.get("unverified_machine"))
                                          for g in (b_row or {}).get("games") or []]})[:700])
        trusted_lm = admin.put(f"/api/admin/machines/{lm_sid}/trust", json={"trust_tier": "trusted"})
        counted = wait_until(lambda: row_of(admin, model)[0], 15, 1.0)
        a_row, a_body = row_of(admin, model)
        r.check("an administrator trusts bob's machine: its earlier benchmark game now counts on the administrators' "
                "board, and nothing of it is left out",
                trusted_lm.status_code == 200 and bool(counted) and (a_row or {}).get("benchmark_games", 0) >= 1
                and model not in {u["model"] for u in a_body.get("unverified") or []},
                json.dumps({"row": {k: (a_row or {}).get(k) for k in ("overall", "benchmark_games")},
                            "unverified": a_body.get("unverified")}))
        data.update(tiers_machine=lm_sid, tiers_run=bench_id)
        helpers.pop("models").stop()
        admin.patch(f"/api/admin/users/{ids['bob']}", json={"trust_tier": "new"})      # the instance left as it was
        if h.keep:
            data["browser"] = _verify_for_the_browser(h, helpers, machine, start, queue, tier, host, host_cpu, tag)
        r.check("no traceback in the server's log", h.server.tracebacks() == 0, f"{h.server.tracebacks()} traceback(s)")
    finally:
        if not h.keep:
            for helper in helpers.values():
                helper.stop()
            scripted.close()


def _verify_for_the_browser(h, helpers: dict, machine, start, queue, tier, host: str, host_cpu: dict,
                            tag: str) -> dict:
    """With ``--keep``, the ``verify`` part leaves what the browser checklist's H1, H4 and H5 look at (P4.7.7): bob's
    cheating machine caught again with bob quarantined (H5: the machine and its owner with the evidence and the
    withdrawn job, Release); with this server's job host lending nothing (so no replica plays), alice's provisional
    machine's results waiting for their replicas (H4: done to alice with no badge and no replica, settling and
    "waiting for a verifier" to an administrator) and her new machine held, its experiment "waiting for a verifier"
    (H4); their helpers left connected (H1: the card's helper, key and trust). Returns what it made."""
    admin, alice, bob = (h.accounts[k] for k in ("admin", "alice", "bob"))
    made: dict = {}
    # H5: bob's machine whose helper alters every result, caught (and bob with it)
    c_sid, c_token, _cg = machine(bob, "caught")
    start("caught", c_sid, c_token, "result:1")
    queue(bob, f"qa-browser-caught-{tag}", 2, 9100)
    made["caught"] = c_sid if wait_until(lambda: tier(c_sid) == "quarantined", 120, 0.25) else None
    # H4: this server's job host lends nothing: replicas wait
    admin.put(f"/api/machines/{host}/settings", json={"capabilities": {"cpu": {**host_cpu, "enabled": False}}})
    _verification(h, provisional_rate=1.0)
    p_sid, p_token, _pg = machine(alice, "pending")
    admin.put(f"/api/admin/machines/{p_sid}/trust", json={"trust_tier": "provisional"})
    start("pending", p_sid, p_token)
    pending = f"qa-browser-pending-{tag}"
    queue(alice, pending, 4, 9200)
    made["pending"] = {"machine": p_sid, "experiment": pending,
                       "settling": wait_until(lambda: (_lab_view(admin, pending).get("settling") or 0) >= 4, 120, 0.25)}
    helpers.pop("pending").stop()       # its results stay waiting; it takes none of the next experiment's games
    n_sid, n_token, _ng = machine(alice, "held")
    start("held", n_sid, n_token)
    held = f"qa-browser-held-{tag}"
    queue(alice, held, 2, 9300)
    made["held"] = {"machine": n_sid, "experiment": held,
                    "waiting": wait_until(lambda: "waiting for a verifier" in (_lab_view(alice, held).get("waiting")
                                                                               or ""), 60, 0.25) and
                    _lab_view(alice, held).get("waiting")}
    made["password_in"] = "qa-verify.json"
    return made



# ---------------------------------------------------------------------------- the update part (4-13)

#: The update part's two helpers (``cargo xtask develop`` builds them beside the helper: the ``test-update-key``
#: feature, each calling itself its version; the newer another jobs build).
UPDATE_OLD, UPDATE_NEW = "0.1.6-test.1", "0.1.6-test.2"
#: Each target's release asset name (P4.10; ``cargo xtask helper-manifest`` reads them by these names).
RELEASE_NAMES = {"x86_64-pc-windows-msvc": "citar-helper-windows-x64.exe", "aarch64-apple-darwin":
                 "citar-helper-macos-arm64", "x86_64-apple-darwin": "citar-helper-macos-x64",
                 "x86_64-unknown-linux-gnu": "citar-helper-linux-x64", "aarch64-unknown-linux-gnu":
                 "citar-helper-linux-arm64"}


def update_binaries() -> tuple:
    """``citar-helper-update-1`` and ``-2`` beside the helper the run uses."""
    folder = Path(q3.helper_binary()).parent
    found = []
    for n in (1, 2):
        path = next((folder / name for name in (f"citar-helper-update-{n}.exe", f"citar-helper-update-{n}")
                     if (folder / name).is_file()), None)
        if path is None:
            raise q3.HarnessError(f"no citar-helper-update-{n} beside {q3.helper_binary()}: run cargo xtask develop")
        found.append(str(path))
    return tuple(found)


def _xtask(*args: str):
    """``cargo xtask`` from the checkout (the release side of the update channel: keygen, helper-manifest,
    sign-manifest)."""
    import subprocess
    return subprocess.run(["cargo", "xtask", *args], cwd=str(ROOT), capture_output=True, text=True, timeout=900)


class _Assets:
    """The release's assets on loopback, as GitHub serves them: ``/download/<tag>/<name>`` redirects to
    ``/cdn/<name>``, which serves the file. Every path asked is recorded."""

    def __init__(self, folder: Path):
        import http.server
        import threading as _threading
        self.folder, self.asked = folder, []
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                outer.asked.append(self.path)
                name = self.path.rsplit("/", 1)[-1]
                if self.path.startswith("/download/"):
                    self.send_response(302)
                    self.send_header("Location", f"/cdn/{name}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                path = outer.folder / name
                if not self.path.startswith("/cdn/") or not path.is_file():
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                body = path.read_bytes()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = f"http://127.0.0.1:{self.httpd.server_address[1]}/download/v{UPDATE_NEW}"
        _threading.Thread(target=self.httpd.serve_forever, name="qa-assets", daemon=True).start()

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


def _end_tree(helper) -> None:
    """A helper and what it started (after an update a console run supervises the new binary), ended."""
    for child in reversed(descendants(helper.proc.pid)):
        try:
            os.kill(child, 15)
        except OSError:
            pass
    helper.stop()


def _sha(path) -> str:
    import hashlib
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def part_update(h) -> None:
    """The ``update`` part (the module's doc)."""
    import shutil
    r = h.report
    bob = h.accounts["bob"]
    data = r.data.setdefault("update", {})
    state = h.server.state
    old_bin, new_bin = update_binaries()
    work = state / "update"
    keys, assets_dir, install = work / "keys", work / "assets", work / "install"
    for d in (assets_dir, install):
        d.mkdir(parents=True, exist_ok=True)
    helpers: list = []
    assets = None
    saved_env = {k: h.server.env.get(k) for k in ("CITAR_HELPER_MANIFEST_FILE", "CITAR_HELPER_UPDATE_KEY")}
    try:
        # ---- the release side, with a throwaway key made here, outside the repository
        inside = ROOT / "qa-update-keys-never"
        refused_keygen = _xtask("keygen", "--out", str(inside))
        made = _xtask("keygen", "--out", str(keys))
        key_file, pub_file = keys / "citar-update-signing.key", keys / "citar-update-signing.pub"
        refused_sign = _xtask("sign-manifest", "--key", str(ROOT / "citar-update-signing.key"),
                              str(work / "citar-helper-manifest.json"))
        r.check("cargo xtask keygen makes a throwaway key outside the repository, and keygen and sign-manifest refuse "
                "paths inside it (exit 2, nothing made)",
                made.returncode == 0 and key_file.is_file() and pub_file.is_file()
                and refused_keygen.returncode == 2 and "inside this repository" in refused_keygen.stderr
                and not inside.exists() and refused_sign.returncode == 2
                and "inside this repository" in refused_sign.stderr,
                json.dumps({"keygen": made.returncode, "refused": [refused_keygen.returncode, refused_sign.returncode],
                            "why": refused_keygen.stderr.strip()[-200:]}))
        import subprocess
        line = subprocess.run([new_bin, "--version"], capture_output=True, text=True, timeout=60).stdout
        target = line.split("target ", 1)[1].split(",", 1)[0] if "target " in line else ""
        name = RELEASE_NAMES.get(target, "citar-helper-test-asset")
        shutil.copy2(new_bin, assets_dir / name)
        assets = _Assets(assets_dir)
        manifest = work / "citar-helper-manifest.json"
        listed = _xtask("helper-manifest", "--assets", str(assets_dir), "--version", UPDATE_NEW,
                        "--base-url", assets.base, "--out", str(manifest))
        signed = _xtask("sign-manifest", "--key", str(key_file), str(manifest))
        doc = json.loads(manifest.read_text(encoding="utf-8")) if manifest.is_file() else {}
        asset = (doc.get("assets") or {}).get(target) or {}
        r.check("cargo xtask helper-manifest lists the newer helper for this machine's target (its build read from the "
                "binary, its SHA-256 and size), and sign-manifest signs the file as published",
                listed.returncode == 0 and signed.returncode == 0 and doc.get("version") == UPDATE_NEW
                and asset.get("sha256") == _sha(new_bin) and asset.get("size") == Path(new_bin).stat().st_size
                and asset.get("url", "").startswith(assets.base) and (work / "citar-helper-manifest.json.sig").is_file(),
                json.dumps({"target": target, "asset": asset, "out": (listed.stdout + signed.stdout).strip()[-300:],
                            "err": (listed.stderr + signed.stderr).strip()[-300:]})[:600])

        # ---- the server relays it: its manifest file, its key trusted by the extension's test operations
        h.server.env["CITAR_HELPER_MANIFEST_FILE"] = str(manifest)
        h.server.env["CITAR_HELPER_UPDATE_KEY"] = pub_file.read_text(encoding="utf-8").strip()
        h.server.restart()

        def machine(label: str) -> tuple:
            made = bob.post("/api/pool/servers", json={"config": {"name": f"QA {label} {h.tag}", "kind": "owned",
                                                                   "connection": {"provider": "openai_compatible"}},
                                                        "reach": "worker"})
            hid = made.json().get("id", "") if made.status_code == 200 else ""
            issued = bob.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": label}) if hid else None
            token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
            return hid, token

        def card(hid: str) -> dict:
            got = bob.get("/api/pool/servers")
            servers = got.json().get("servers", []) if got.status_code == 200 else []
            return next((m for m in servers if m.get("id") == hid), {})

        # ---- the update itself: the older helper in an install folder of its own, updates automatic
        hid, token = machine("updating box")
        exe = install / ("citar-helper.exe" if os.name == "nt" else "citar-helper")
        shutil.copy2(old_bin, exe)
        first = Helper(h, token, h.endpoint, "qa-update-box", binary=str(exe),
                       extra=("--no-llm", "--updates", "auto", "--update-key", str(pub_file)))
        helpers.append(first)
        relayed = wait_until(lambda: ((card(hid).get("helper_update") or {}).get("latest") or {}).get("version"),
                             30, 0.25)
        applied = wait_until(lambda: (lambda w: w if ((w.get("update") or {}).get("status") or {}).get("state")
                                      == "applied" and (w.get("worker") or {}).get("version") == UPDATE_NEW
                                      else None)(_workers(bob, hid)), 180, 0.25)
        log = first.text()
        said = [w for w in ("downloading", "verified", "restarting", "applied") if f"update: {w}" in log]
        replaced = "Another helper connected with this machine's token" in log
        r.check("the server relays the signed manifest (the card's latest signed helper), and the helper downloads "
                "the newer binary through a redirect, checks its size and SHA-256, self-tests it, hands off and "
                "reconnects as 0.1.6-test.2 reporting applied, never replaced (its old socket closed first)",
                relayed == UPDATE_NEW and bool(applied) and said == ["downloading", "verified", "restarting", "applied"]
                and not replaced and any(p.startswith("/download/") for p in assets.asked)
                and any(p.startswith("/cdn/") for p in assets.asked),
                json.dumps({"relayed": relayed, "said": said, "asked": assets.asked, "replaced": replaced,
                            "status": ((applied or {}).get("update") or {}).get("status")})[:600])
        r.check("the install folder holds the new binary, the old one kept as <name>.old; the helper's home says the "
                "update is healthy and nothing is pending",
                exe.is_file() and _sha(exe) == _sha(new_bin) and _sha(str(exe) + ".old") == _sha(old_bin)
                and (first.home / "helper" / "update" / "healthy.json").is_file()
                and not (first.home / "helper" / "update" / "pending.json").exists(),
                json.dumps({"new": _sha(exe)[:12], "old": _sha(str(exe) + ".old")[:12] if Path(str(exe) + ".old").is_file()
                            else None}))
        updated = card(hid)
        r.check("bob's card says the helper is up to date, updated, beside the latest signed helper",
                (updated.get("helper_update") or {}).get("available") is False
                and ((updated.get("helper_update") or {}).get("status") or {}).get("state") == "applied"
                and updated.get("helper_version") == UPDATE_NEW,
                json.dumps({k: updated.get(k) for k in ("helper_version", "helper_update")})[:500])
        data.update(machine=hid, said=said, asked=assets.asked, relayed=relayed)

        # ---- notify: available, nothing downloaded; a build that trusts no key: unavailable
        asked_before = len(assets.asked)
        hid2, token2 = machine("notify box")
        notify = Helper(h, token2, h.endpoint, "qa-notify-box", binary=old_bin,
                        extra=("--no-llm", "--updates", "notify", "--update-key", str(pub_file)))
        helpers.append(notify)
        available = wait_until(lambda: (((_workers(bob, hid2).get("update") or {}).get("status") or {}).get("state")
                                        == "available") or None, 60, 0.25)
        hid3, token3 = machine("keyless box")
        keyless = Helper(h, token3, h.endpoint, "qa-keyless-box", extra=("--no-llm",))
        helpers.append(keyless)
        unavailable = wait_until(lambda: (((_workers(bob, hid3).get("update") or {}).get("status") or {}).get("state")
                                          == "unavailable") or None, 60, 0.25)
        notified = card(hid2)
        r.check("a helper whose updates are set to notify reports the update available and downloads nothing; its card "
                "says update available. The run's own helper, which trusts no update key (as every build of this tree), "
                "reports updates unavailable",
                bool(available) and len(assets.asked) == asked_before
                and (notified.get("helper_update") or {}).get("available") is True and bool(unavailable),
                json.dumps({"notify": (notified.get("helper_update") or {}), "asked": assets.asked[asked_before:]})[:600])
        data["browser"] = {"updated": hid, "notify": hid2, "keyless": hid3}
    finally:
        if not h.keep:
            for helper in helpers:
                _end_tree(helper)
        if assets is not None and not h.keep:
            assets.close()
        if not h.keep:
            for k, v in saved_env.items():
                if v is None:
                    h.server.env.pop(k, None)
                else:
                    h.server.env[k] = v


# ---------------------------------------------------------------------------- the run

PARTS: dict[str, Callable] = {"helper": part_helper, "llm-real": part_llm_real, "scan": part_scan,
                              "lab": part_lab, "remote": part_remote, "verify": part_verify, "update": part_update}


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
    ap.add_argument("--wsl-root", default=None, metavar="PATH", help="--wsl: the checkout in WSL the server runs from")
    ap.add_argument("--wsl-python", default=None, metavar="PATH",
                    help="--wsl: the Python in WSL that runs it (a venv's, with the extension cargo xtask develop built)")
    ap.add_argument("--wsl-distro", default="Ubuntu-24.04", help="--wsl: the WSL distribution (P4.7.1's)")
    ap.add_argument("--wsl-user", default="claude", help="--wsl: the WSL user (P4.7.1's)")
    args = ap.parse_args(argv)
    # the instance's server plays the lab (the lab part): the test suite's switch, if this runs under it, stays off
    os.environ.pop("CITAR_TEST_NO_DISPATCHER", None)
    global LLM_URL, STAND_IN
    LLM_URL, STAND_IN = args.llm_url, args.stand_in
    q3.HELPER_BIN = args.helper_bin
    if args.wsl and args.part != "remote":
        print("refused: --wsl belongs to the remote and exit parts (packages 4-10, 4-15)", flush=True)
        return 2
    if args.wsl and not (args.wsl_root and args.wsl_python):
        print("refused: --wsl needs --wsl-root (a checkout in WSL) and --wsl-python (its venv's Python)", flush=True)
        return 2
    if args.wsl and not all(p.startswith("/") and ":" not in p for p in (args.wsl_root, args.wsl_python)):
        print("refused: --wsl-root and --wsl-python are absolute paths inside WSL (from Git Bash, set "
              "MSYS_NO_PATHCONV=1, or it rewrites them as Windows paths)", flush=True)
        return 2
    # the harness's own run, on this script's parts
    q3.PARTS, q3.LATER = PARTS, LATER
    plain = q3.Server
    fallback: dict = {}
    if args.wsl:
        def server(state: Path, port: int):
            try:
                made = WslServer(state, port, distro=args.wsl_distro, user=args.wsl_user, root=args.wsl_root,
                                 python=args.wsl_python)
                made.prepare()
                return _Reachable(made, plain, fallback)
            except Exception as e:          # WSL itself is missing: the fallback, recorded
                fallback["why"] = f"WSL could not run the server: {e}"
                return plain(state, port)
        q3.Server = server
    try:
        code, report = q3.run(args)
    except q3.Refused as e:
        print(f"refused: {e}", flush=True)
        return 2
    finally:
        q3.Server = plain
    if args.wsl:
        topo = report.data.setdefault("remote", {})
        topo["topology"] = "windows (the fallback)" if fallback else "wsl"
        if fallback:
            topo["fallback"] = fallback.get("why")
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

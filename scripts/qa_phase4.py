"""Phase 4's QA script: the CITAR helper and the server's side of protocol 2, end to end, with the evidence written
down (design/phase4-helper.md P4.7.6).

    python scripts/qa_phase4.py <part>|all [--port N] [--dir D] [--keep] [--helper-bin PATH] [--skip PART]
                                [--llm-url URL] [--stand-in]
                                [--wsl --wsl-root PATH --wsl-python PATH [--wsl-distro NAME] [--wsl-user NAME]]
                                [--wsl-helper PATH]

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
  own helper, which trusts no throwaway key, reports updates unavailable (or, once the owner's key is committed to
  ``update-keys.pub``, refuses the manifest the throwaway key signed: ``citar-helper update check`` says which). With
  ``--keep`` the three stay connected, for the browser checklist's H1 (the cards' update lines). The part runs the
  ``cargo xtask`` tools from the checkout, so ``cargo`` must be on the ``PATH``; their build goes to
  ``CARGO_TARGET_DIR``, else to the folder above ``CITAR_EXT_DIR`` (the target folder ``cargo xtask develop`` built
  the extension in), never into the checkout (which may be a synced folder: CONTRIBUTING.md, "Build outside synced
  folders").

- ``service`` (4-14): the helper as a systemd user service in WSL, against the dev server in WSL (``--wsl``, as
  ``remote --wsl`` runs it, and ``--wsl-helper``, the helper ``cargo xtask develop`` built in WSL; design P4.7.1 rule 8:
  services are installed for real only there, never on Windows). Bob's helper machine and its token; then one ``wsl``
  invocation whose script installs, checks and uninstalls under a ``trap`` that uninstalls on any exit (``claude`` has
  ``Linger=no``, so a unit left enabled would start again at the next invocation), waiting at each step for the
  Windows side: ``service install --user`` writes the unit (``Restart=on-failure``, ``RestartPreventExitStatus=78``)
  and starts it, ``service status`` says running, and the machine is online on the server; bob revokes the token, the
  hub drops the helper (``shutdown`` ``revoked``) and it idles: the same process, ``NRestarts`` 0, no new attempt; the
  unit restarted by hand under the revoked token makes exactly one attempt (one ``worker.auth_failed`` audit row) and
  idles again, ``NRestarts`` still 0; ``service uninstall`` stops and removes it, ``service status`` says not installed,
  and ``systemctl --user list-unit-files`` lists no citar unit. A last invocation checks that none is left.

- ``exit`` (4-15): Phase 4's exit (design/phase4-helper.md P4.1.4; Appendix E, 4-15 gates 2 and 3), with the server in
  WSL (``--wsl``, P4.7.6's topology; its job host, Linux, the verifier) and helpers on both sides of it. Alice's
  Windows helper machine (the Windows helper dialling the WSL server through WSL's localhost forwarding; two threads,
  25%, 512 MB) and her WSL helper machine (the helper ``cargo xtask develop`` built in WSL, ``--wsl-helper``, dialling
  it on WSL's own loopback; two threads, 50%, 512 MB), each lending CPU from its own settings (``citar-helper setup``
  answered yes after its ceiling was written), her grant to bob listing lab explicitly; bob (given ``run_lab``)
  queues 24 duels of 40 rounds and an administrator a ranking experiment of 6 and a reference run of 4 (``citar lab
  submit``), all dispatched across the two helpers and this server's job host. Checked from what the run left: every
  accepted line signed by its machine (its claim the run's, its ed25519 signature verified here with ``cryptography``,
  another implementation than the server's) or the server's own; every game played again here (Windows) folding to
  the chain head its run gave, its result hash the line's; each helper's results replicated on the job host while
  its machine was new and sampled after at the provisional rate by the server's keyed choice (set to
  ``EXIT_SAMPLE`` for the run); the games the Windows helper played and Linux replicated equal byte for byte (the
  engine's bytes played again on both, the hashes and heads of both runs, the line); each game once in its results
  file, the ratings exactly the accepted lab and ranking games; one ledger event a run with its verification, given
  by alice and used by bob (each bounded by 1.5 times the reference cost), replicas used by nobody, their sums as
  ``/api/ledger/me`` tells them; the limits hard on both and held (each job host's CPU under its cap). Then the faulty
  helpers: carol's WSL helper altering its third result (``--test-fault result:3``) is quarantined at it with the
  evidence on ``#/admin/verification``'s data, and her second machine is refused CPU work; dave's Windows helper,
  lending to bob under a budgeted grant and altering its sixth and ninth results (``result-at:6,9``), reaches
  provisional, an administrator's replica checks its seventh, sampling (the rate at 1) catches its ninth, and its
  sixth and eighth (both unverified, one before the verified match) are withdrawn, played again elsewhere and
  accepted, their events rejected and their budget settlements reversed, the ratings counting each game once. With
  ``--keep`` alice's two helpers stay connected, lending, for the browser checklist.
- ``real`` (4-15): LM Studio's ``google/gemma-4-e2b`` (loaded before and after; the part never loads, unloads or
  swaps a model, and the helper offers that model only) through alice's Windows helper: a 6-round game with her
  person seat, a hybrid seat (prompted every 2 turns), an LLM seat and two bots; the LLM seat's every turn ended by
  its own ``end_turn``, at least two chats answered by the models' own calls (parsed on the server), the ledger's
  generation seconds the scheduler's, no slot drained; then 4-04's checks through the same helper binary
  (``exercise``: a 1,500-token generation cancelled at 2 s acknowledged within a second, the next short requests
  within the run's own baseline).

``all`` runs every part in that order in two runs, each on a server of its own in a folder of ``--dir``: the parts
before ``service`` with the server on Windows, then ``service``, ``exit`` and ``real`` with the server in WSL (so it
needs ``--wsl`` and ``--wsl-helper``; ``--skip`` leaves parts out for a development run, never at the exit); its
``qa-all.json`` holds both runs' summaries.
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

#: Parts a later package writes (P4.7.6): asked for now, the harness says so and refuses. None is left.
LATER: dict = {}
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


def _xtask_target() -> str:
    """Where ``cargo xtask`` builds: ``CARGO_TARGET_DIR``, else the target folder ``cargo xtask develop`` built the
    extension in (``CITAR_EXT_DIR`` is its ``citar-ext``), never the checkout's own ``target/``."""
    given = os.environ.get("CARGO_TARGET_DIR")
    if given:
        return given
    ext = os.environ.get("CITAR_EXT_DIR")
    if ext and Path(ext).name == "citar-ext" and Path(ext).parent.is_dir():
        return str(Path(ext).parent)
    raise q3.HarnessError("set CARGO_TARGET_DIR (or CITAR_EXT_DIR, which cargo xtask develop prints) so cargo xtask "
                          "builds outside the checkout")


def _xtask(*args: str):
    """``cargo xtask`` from the checkout (the release side of the update channel: keygen, helper-manifest,
    sign-manifest), building into :func:`_xtask_target`."""
    import subprocess
    env = {**os.environ, "CARGO_TARGET_DIR": _xtask_target()}
    return subprocess.run(["cargo", "xtask", *args], cwd=str(ROOT), capture_output=True, text=True, timeout=900,
                          env=env)


def _keys_trusted(binary: str, home: Path) -> int:
    """How many update keys ``binary`` trusts of its own (``citar-helper update check``'s line): the ones
    ``update-keys.pub`` held when it was built, none until the owner's key is committed."""
    import re
    import subprocess
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CITAR_WORKER_", "CITAR_HELPER_"))}
    env["CITAR_DATA_DIR"] = str(home)
    out = subprocess.run([binary, "update", "check", "--home", str(home)], capture_output=True, text=True,
                         timeout=60, env=env).stdout
    m = re.search(r"update keys trusted: (\d+)", out)
    if not m:
        raise q3.HarnessError(f"{binary} update check said no key count: {out.strip()[-300:]}")
    return int(m.group(1))


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
        # Every build of this tree trusts no key until the owner's is committed to update-keys.pub; then the run's own
        # helper trusts that one, and refuses the manifest the throwaway key signed.
        own_keys = _keys_trusted(q3.helper_binary(), keyless.home)
        expected = ("unavailable", "") if own_keys == 0 else ("refused", "does not verify")

        def keyless_said():
            st = (_workers(bob, hid3).get("update") or {}).get("status") or {}
            return st if st.get("state") == expected[0] and expected[1] in (st.get("message") or "") else None
        keyless_status = wait_until(keyless_said, 60, 0.25)
        notified = card(hid2)
        r.check("a helper whose updates are set to notify reports the update available and downloads nothing; its card "
                "says update available. The run's own helper, which trusts no throwaway key, reports updates "
                "unavailable (no key trusted, as every build of this tree until the owner's key is committed) or "
                "refuses the throwaway key's manifest (the owner's key trusted)",
                bool(available) and len(assets.asked) == asked_before
                and (notified.get("helper_update") or {}).get("available") is True and bool(keyless_status),
                json.dumps({"notify": (notified.get("helper_update") or {}), "asked": assets.asked[asked_before:],
                            "own_keys": own_keys, "keyless": keyless_status})[:600])
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


# ---------------------------------------------------------------------------- the service part (4-14)

#: The helper ``cargo xtask develop`` built in WSL (``--wsl-helper``), which the service part installs.
WSL_HELPER: Optional[str] = None

#: The service part's script in WSL: one invocation installs, checks and uninstalls, under a trap that uninstalls on
#: any exit; it waits for the Windows side's go files (``revoked``, ``restart``, ``counted``) and says each step on a
#: ``QA`` line. Arguments: the helper, its home, the folder of go files, the server's port, the token.
SERVICE_SH = r"""#!/bin/bash
set -u
BIN="$1"; HOME_DIR="$2"; GO="$3"; PORT="$4"; TOKEN="$5"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
UNIT="$HOME/.config/systemd/user/citar-helper.service"
say() { echo "QA $*"; }
show() { systemctl --user show citar-helper.service -p ActiveState -p SubState -p MainPID -p NRestarts | tr '\n' ' '; }
uninstall() {
  "$BIN" service uninstall --user --home "$HOME_DIR" >/dev/null 2>&1
  rm -f "$UNIT"
  systemctl --user daemon-reload >/dev/null 2>&1
  systemctl --user reset-failed citar-helper.service >/dev/null 2>&1
  say "trap: uninstalled"
}
trap uninstall EXIT
wait_file() { for i in $(seq 1 "$2"); do [ -e "$1" ] && return 0; sleep 1; done; return 1; }
mkdir -p "$HOME_DIR" "$GO"
printf '{"server_url": "http://127.0.0.1:%s", "token": "%s", "no_llm": true, "no_power": true, "updates": "off"}\n' \
  "$PORT" "$TOKEN" > "$HOME_DIR/helper.json"
chmod 600 "$HOME_DIR/helper.json"
say "linger $(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null)"
say "user-manager $(systemctl --user is-system-running 2>&1)"
"$BIN" service install --user --home "$HOME_DIR"; say "install-exit $?"
for i in $(seq 1 60); do "$BIN" service status --user --home "$HOME_DIR" >/dev/null 2>&1 && break; sleep 1; done
st=$("$BIN" service status --user --home "$HOME_DIR"); say "status-exit $? $st"
say "show-installed $(show)"
say "unitfile $(grep -c -e '^Restart=on-failure' -e '^RestartPreventExitStatus=78' -e '^RestartSec=15' "$UNIT")"
say "READY"
wait_file "$GO/revoked" 240 || { say "no go: revoked"; exit 1; }
sleep 20
say "show-revoked $(show)"
say "REVOKED-IDLE"
wait_file "$GO/restart" 240 || { say "no go: restart"; exit 1; }
systemctl --user restart citar-helper.service; say "restart-exit $?"
sleep 25
say "show-restarted $(show)"
st=$("$BIN" service status --user --home "$HOME_DIR"); say "status-restarted-exit $? $st"
say "RESTARTED-IDLE"
wait_file "$GO/counted" 240 || { say "no go: counted"; exit 1; }
"$BIN" service uninstall --user --home "$HOME_DIR"; say "uninstall-exit $?"
st=$("$BIN" service status --user --home "$HOME_DIR"); say "status-after-exit $? $st"
say "unitfiles-after $(systemctl --user list-unit-files 'citar*' --no-legend | grep -c .)"
say "unitfile-after $(ls "$UNIT" 2>/dev/null | grep -c .)"
say "procs-after $(pgrep -u "$(id -u)" -f 'citar-helper run --service' | grep -c .)"
say "DONE"
"""

#: What a fresh invocation finds afterwards: citar units listed, unit files, helper processes.
SERVICE_LEFT_SH = r"""#!/bin/bash
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
echo "units $(systemctl --user list-unit-files 'citar*' --no-legend 2>/dev/null | grep -c .)"
echo "files $(ls "$HOME"/.config/systemd/user/citar* 2>/dev/null | grep -c .)"
echo "procs $(pgrep -u "$(id -u)" -f 'citar-helper run' | grep -c .)"
"""

#: The ``worker.auth_failed`` audit rows on the instance's database: one per refused token.
AUTH_FAILED = """
from sqlalchemy import func, select
from citar import db
from citar.db.models import AuditLog
db.configure()
with db.session() as s:
    print(s.scalar(select(func.count()).select_from(AuditLog).where(AuditLog.action == "worker.auth_failed")))
"""


def _qa_values(lines: list, key: str) -> str:
    """The rest of the last ``QA key ...`` line, or ''."""
    found = [line[len(f"QA {key}"):].strip() for line in lines if line.startswith(f"QA {key}")]
    return found[-1] if found else ""


def _shown(text: str) -> dict:
    """``systemctl show``'s ``KEY=value`` pairs, as the script's ``show`` prints them on one line."""
    return dict(pair.split("=", 1) for pair in text.split() if "=" in pair)


def part_service(h) -> None:
    """The ``service`` part (the module's doc)."""
    import subprocess
    r = h.report
    bob = h.accounts["bob"]
    data = r.data.setdefault("service", {})
    server = h.server
    if getattr(server, "topology", "") != "wsl" or not WSL_HELPER:
        r.check("the service part runs with the server in WSL and the helper built there (--wsl, --wsl-helper)", False,
                f"topology {getattr(server, 'topology', 'windows')}, --wsl-helper {WSL_HELPER!r}")
        return
    made = bob.post("/api/pool/servers", json={"config": {"name": f"QA service box {h.tag}", "kind": "owned",
                                                           "connection": {"provider": "openai_compatible"}},
                                                "reach": "worker"})
    hid = made.json().get("id", "") if made.status_code == 200 else ""
    issued = bob.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "service"}).json() if hid else {}
    token, tid = issued.get("token", ""), issued.get("id", "")
    if not token:
        r.check("bob's helper machine and its token", False, made.text[:300])
        return
    home, go = f"{server.remote}/helper-svc", f"{server.remote}/svc-go"
    server._put("service.sh", SERVICE_SH)
    server._put("service-left.sh", SERVICE_LEFT_SH)
    log_path = server.state / "service-wsl.log"
    lines: list = []
    proc = subprocess.Popen(["wsl", "-d", server.distro, "-u", server.user, "-e", "bash", f"{server.remote}/service.sh",
                             WSL_HELPER, home, go, str(server.port), token],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)

    def read():
        with open(log_path, "w", encoding="utf-8") as log:
            for raw in proc.stdout:
                line = raw.decode("utf-8", "replace").rstrip()
                lines.append(line)
                log.write(line + "\n")
                log.flush()

    threading.Thread(target=read, name="qa-service-wsl", daemon=True).start()

    def said(marker: str, within: float) -> bool:
        """Whether the script said ``QA marker`` within ``within`` seconds (no longer once it has ended)."""
        until = time.perf_counter() + within
        while time.perf_counter() < until:
            if f"QA {marker}" in lines:
                return True
            if proc.poll() is not None:
                time.sleep(0.5)          # its last lines, still in the reader's hands
                return f"QA {marker}" in lines
            time.sleep(0.25)
        return f"QA {marker}" in lines

    def audits() -> int:
        out = _server_python(h, AUTH_FAILED).strip().splitlines()
        return int(out[-1]) if out and out[-1].isdigit() else -1

    try:
        ready = said("READY", 300)
        installed = _shown(_qa_values(lines, "show-installed"))
        status = _qa_values(lines, "status-exit")
        r.check("in one wsl invocation under a trap that uninstalls: service install --user writes the unit with its "
                "restart rules and starts it, and service status says running",
                ready and _qa_values(lines, "install-exit") == "0" and status.startswith("0 ") and "running" in status
                and _qa_values(lines, "unitfile") == "3" and installed.get("ActiveState") == "active"
                and installed.get("NRestarts") == "0",
                json.dumps({"lines": [x for x in lines if x.startswith("QA ")][:12]})[:900])
        online = wait_until(lambda: (lambda w: w if w.get("online") else None)(_workers(bob, hid)), 90, 0.5) or {}
        worker = online.get("worker") or {}
        r.check("the helper the unit runs connects to the WSL server: bob's machine is online, protocol 2",
                bool(online) and (online.get("helper") or {}).get("protocol") == 2,
                json.dumps({"worker": {k: worker.get(k) for k in ("version", "target", "protocol")},
                            "helper": online.get("helper")})[:500])
        before = audits()
        revoked = bob.delete(f"/api/pool/servers/{hid}/workers/tokens/{tid}")
        dropped = wait_until(lambda: (not _workers(bob, hid).get("online")) or None, 30, 0.25)
        server._wsl("bash", "-c", 'mkdir -p "$1" && touch "$1/revoked"', "go", go)
        idle = said("REVOKED-IDLE", 180)
        after_revoke = _shown(_qa_values(lines, "show-revoked"))
        between = audits()
        r.check("with its token revoked the hub drops it and it idles: the same process, NRestarts 0, no new attempt",
                revoked.status_code == 200 and bool(dropped) and idle
                and after_revoke.get("MainPID") == installed.get("MainPID") and after_revoke.get("NRestarts") == "0"
                and after_revoke.get("ActiveState") == "active" and between == before,
                json.dumps({"revoke": revoked.status_code, "installed": installed, "after": after_revoke,
                            "attempts": between - before}))
        server._wsl("touch", f"{go}/restart")
        again = said("RESTARTED-IDLE", 180)
        restarted = _shown(_qa_values(lines, "show-restarted"))
        counted = audits()
        r.check("restarted by hand under the revoked token, the server sees one attempt (one refused token) and it "
                "idles: its process runs on, NRestarts stays 0, service status says running",
                again and counted - between == 1 and restarted.get("ActiveState") == "active"
                and restarted.get("NRestarts") == "0" and restarted.get("MainPID") not in ("", "0")
                and _qa_values(lines, "status-restarted-exit").startswith("0 "),
                json.dumps({"restarted": restarted, "attempts": counted - between,
                            "status": _qa_values(lines, "status-restarted-exit")}))
        server._wsl("touch", f"{go}/counted")
        done = said("DONE", 180)
        r.check("service uninstall stops and removes it: status says not installed, no unit file, systemctl --user "
                "list-unit-files lists no citar unit, no helper process",
                done and _qa_values(lines, "uninstall-exit") == "0"
                and _qa_values(lines, "status-after-exit").startswith("3 ")
                and "not installed" in _qa_values(lines, "status-after-exit")
                and _qa_values(lines, "unitfiles-after") == "0" and _qa_values(lines, "unitfile-after") == "0"
                and _qa_values(lines, "procs-after") == "0",
                json.dumps({k: _qa_values(lines, k) for k in ("uninstall-exit", "status-after-exit", "unitfiles-after",
                                                               "unitfile-after", "procs-after")}))
        try:
            code = proc.wait(timeout=60)
        except Exception:
            code = None
        r.check("the invocation ended (exit 0), its trap having uninstalled", code == 0
                and "QA trap: uninstalled" in lines, f"exit {code}")
        left = server._wsl("bash", f"{server.remote}/service-left.sh", timeout=60).stdout
        r.check("a fresh wsl invocation afterwards finds no citar unit, unit file or helper process",
                left.split() == ["units", "0", "files", "0", "procs", "0"], left.strip())
        data.update(machine=hid, lines=[x for x in lines if x.startswith("QA ")], left=left.strip(),
                    attempts={"before": before, "revoked": between, "restarted": counted})
    finally:
        if proc.poll() is None:
            # the trap uninstalls when the script ends: give it every go file, then end it
            server._wsl("bash", "-c", 'mkdir -p "$1" && touch "$1/revoked" "$1/restart" "$1/counted"', "go", go)
            try:
                proc.wait(timeout=120)
            except Exception:
                proc.kill()


# ---------------------------------------------------------------------------- helpers in WSL (4-15)

#: The script a helper in WSL runs through: its process id kept (so it is stopped by it, as WSL's own ``wsl.exe``
#: going does not end it), its data directory its home, the user manager's runtime directory known (so its job host
#: runs in a ``systemd-run --user`` scope: limits held hard). Arguments: the pid file, the home, the command.
WSL_HELPER_SH = r"""#!/bin/bash
PIDFILE="$1"; HOME_DIR="$2"; shift 2
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export CITAR_DATA_DIR="$HOME_DIR"
echo $$ > "$PIDFILE"
exec "$@"
"""

#: A process's job host's CPU sampled inside WSL (``CpuWatch``'s reading, by ``/proc``): ``python3 -c WSL_CPU_WATCH
#: <pid> <stop file> <limit s>``, sampling every 0.25 s until the stop file appears; prints ``CpuWatch.summary()``'s
#: figures, as a percentage of WSL's logical CPUs.
WSL_CPU_WATCH = r"""
import json, os, sys, time
pid, stop, limit = int(sys.argv[1]), sys.argv[2], float(sys.argv[3])
def kids():
    parents = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            stat = open(f"/proc/{d}/stat").read()
        except OSError:
            continue
        parents.setdefault(int(stat[stat.rindex(")") + 2:].split()[1]), []).append(int(d))
    out, todo = [], [pid]
    while todo:
        for c in parents.get(todo.pop(), []):
            if c not in out and c != pid:
                out.append(c)
                todo.append(c)
    return out
def cpu(p):
    try:
        f = open(f"/proc/{p}/stat").read()
    except OSError:
        return None
    f = f[f.rindex(")") + 2:].split()
    return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")
seen, samples, t0 = {}, [], time.monotonic()
while not os.path.exists(stop) and time.monotonic() - t0 < limit:
    for c in kids():
        got = cpu(c)
        if got is not None:
            seen[c] = max(got, seen.get(c, 0.0))
    if seen:
        samples.append((time.monotonic(), sum(seen.values())))
    time.sleep(0.25)
cpus = os.cpu_count() or 1
peak, j = 0.0, 0
for i in range(len(samples)):
    while j < len(samples) and samples[j][0] - samples[i][0] < 2.0:
        j += 1
    if j >= len(samples):
        break
    peak = max(peak, (samples[j][1] - samples[i][1]) / (samples[j][0] - samples[i][0]) / cpus * 100.0)
span = samples[-1][0] - samples[0][0] if len(samples) > 1 else 0.0
used = samples[-1][1] - samples[0][1] if len(samples) > 1 else 0.0
print(json.dumps({"peak_pct": round(peak, 2) if span else None,
                  "mean_pct": round(used / span / cpus * 100.0, 2) if span else None,
                  "seconds": round(span, 2), "cpu_s": round(used, 3), "cpus": cpus}))
"""


def wsl_faults() -> str:
    """``citar-helper-faults`` beside the helper ``cargo xtask develop`` built in WSL (``--wsl-helper``)."""
    import posixpath
    if not WSL_HELPER:
        raise q3.HarnessError("no helper in WSL: give --wsl-helper")
    return posixpath.join(posixpath.dirname(WSL_HELPER), "citar-helper-faults")


class WslHelper:
    """A CITAR helper in WSL dialling the WSL server on WSL's own loopback, ``Helper``'s counterpart: the helper
    ``cargo xtask develop`` built there (``--wsl-helper``), or ``citar-helper-faults`` beside it, its home
    ``helper-home-<name>`` in the WSL state (``--home``, and ``CITAR_DATA_DIR``), its log in the harness's directory,
    lending CPU only (``--no-llm``, no scan, no power readings). Its own settings are what ``citar-helper setup``
    writes once its owner answers yes to lending CPU (the ceiling ``ceiling`` written first, then the setup's three
    answers on its standard input): ``setup`` keeps its answer (``setup.stdout``)."""

    def __init__(self, h, token: str, name: str, *, ceiling: dict, faults: tuple = ()):
        import subprocess
        server = h.server
        self.server, self.name = server, name
        self.binary = wsl_faults() if faults else WSL_HELPER
        self.home = f"{server.remote}/helper-home-{name}"
        self.pidfile = f"{self.home}/helper.pid"
        self.log = server.state / f"helper-wsl-{name}.log"
        server._wsl("mkdir", "-p", self.home)
        server._put(f"helper-home-{name}/helper.json", json.dumps({"cpu": ceiling}))
        server._put("wsl-helper.sh", WSL_HELPER_SH)
        self.setup = server._wsl(self.binary, "setup", "--home", self.home,
                                 stdin=f"http://127.0.0.1:{server.port}\n{token}\ny\n", timeout=60)
        out = open(self.log, "w", encoding="utf-8")
        args = [self.binary, "--home", self.home, "--server", f"http://127.0.0.1:{server.port}", f"--token={token}",
                "--no-llm", "--no-hardware", "--no-power", "--name", name,
                *(x for m in faults for x in ("--test-fault", m))]
        self.proc = subprocess.Popen(["wsl", "-d", server.distro, "-u", server.user, "-e", "bash",
                                      f"{server.remote}/wsl-helper.sh", self.pidfile, self.home, *args],
                                     stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        out.close()

    def settings(self) -> dict:
        """Its ``helper.json`` as ``setup`` left it (the token left out)."""
        got = self.server._wsl("cat", f"{self.home}/helper.json", timeout=60)
        try:
            data = json.loads(got.stdout) if got.returncode == 0 else {}
        except ValueError:
            data = {}
        return {k: v for k, v in data.items() if k != "token"}

    def pid(self) -> Optional[int]:
        got = self.server._wsl("cat", self.pidfile, timeout=60)
        text = got.stdout.strip()
        return int(text) if got.returncode == 0 and text.isdigit() else None

    def text(self) -> str:
        return self.log.read_text(encoding="utf-8", errors="replace") if self.log.exists() else ""

    def stop(self) -> None:
        """Ended by its process id (SIGTERM, then SIGKILL after 10 s), and its ``wsl`` with it."""
        if self.proc.poll() is None:
            self.server._wsl("bash", "-c", 'p=$(cat "$1" 2>/dev/null) || exit 0; kill -TERM "$p" 2>/dev/null; '
                             'for i in $(seq 1 100); do kill -0 "$p" 2>/dev/null || exit 0; sleep 0.1; done; '
                             'kill -KILL "$p" 2>/dev/null; exit 0', "stop", self.pidfile, timeout=60)
            try:
                self.proc.wait(timeout=15)
            except Exception:
                self.proc.kill()


class WslCpuWatch:
    """``CpuWatch`` for a helper in WSL: ``WSL_CPU_WATCH`` sampling its job host inside WSL from ``start()`` until
    ``stop()`` (a stop file), which returns the same figures."""

    def __init__(self, helper: WslHelper, limit: float = 1800.0):
        self.helper, self.limit = helper, limit
        self.stopfile = f"{helper.home}/cpu-watch.stop"
        self.proc = None

    def start(self) -> "WslCpuWatch":
        import subprocess
        server = self.helper.server
        server._wsl("rm", "-f", self.stopfile)
        server._put("cpu-watch.py", WSL_CPU_WATCH)
        pid = self.helper.pid()
        self.proc = subprocess.Popen(["wsl", "-d", server.distro, "-u", server.user, "-e", "python3",
                                      f"{server.remote}/cpu-watch.py", str(pid or 0), self.stopfile, str(self.limit)],
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        return self

    def stop(self) -> dict:
        self.helper.server._wsl("touch", self.stopfile)
        try:
            out, _err = self.proc.communicate(timeout=30)
            return json.loads(out.decode("utf-8", "replace").strip().splitlines()[-1])
        except Exception:
            return {"peak_pct": None, "mean_pct": None, "seconds": 0.0, "error": "the sampler said nothing"}


# ---------------------------------------------------------------------------- the exit part (4-15)

#: Every kind of CPU work a machine can lend (``machine_settings.CPU_WORK``).
ALL_WORK = ["lab_game", "ranking_game", "reference_game"]
#: Alice's machines' own ceilings, lent at setup (4-15 gate 2): her Windows helper two threads, 25% of the machine and
#: 512 MB; her WSL helper two threads, 50% and 512 MB; every kind of work (the server's ranking and reference games
#: are lent by opting into their kinds).
EXIT_WINDOWS = {"threads": 2, "cpu_percent": 25, "memory_mb": 512, "work": ALL_WORK}
EXIT_WSL = {"threads": 2, "cpu_percent": 50, "memory_mb": 512, "work": ALL_WORK}
#: What her machines' settings ask on the site: as much or more, so each helper's own ceiling is what plays.
EXIT_SITE = {"enabled": True, "threads": 2, "cpu_percent": 50, "memory_mb": 512, "work": ALL_WORK,
             "yield_to_llm": True}
#: Gate 2's experiments: bob's 24 duels of 40 rounds; an administrator's ranking experiment of 6 and reference run of 4.
EXIT_GAMES, EXIT_TURNS, EXIT_RANKING, EXIT_REFERENCE = 24, 40, 6, 4
#: A provisional machine's results are checked by a replica at this rate in the exit (P4.5.8's ``provisional_rate``;
#: the default 0.10 over the dozen results the two helpers give once promoted would leave about one run in three
#: showing no sampled replica at all): which of them is sampled stays the server's keyed choice (``sample_fraction``),
#: and the part checks every one against it.
EXIT_SAMPLE = 0.5
#: The second pass's games (4-15 gate 3): duels long enough (300 rounds, about 2 s of a core here) that an
#: administrator's word lands between two of the selective cheater's results.
EXIT_LONG = {"profiles": ["standard", "classic-production"], "players": 2, "size": "duel", "maps": ["pangaea"],
             "speed": "Quick", "turns": 300}
#: A result line's measurements and where it came from: the rest is the job's result (P4.4.2).
LINE_EXTRAS = ("seconds", "started_ts", "cpu_s", "finished", "executor")
#: The claim's fields in their signed order (``citar_jobs::Claim``; P4.4.4).
CLAIM_FIELDS = ("v", "job", "run", "machine", "kind", "spec_hash", "jobs_build", "bots", "result_hash", "chain_head",
                "rounds", "key")

#: The instance's jobs, runs, events, machines and ratings for the exit's checks, read in the server's environment:
#: ``python -c EXIT_STATE <experiment names,> <machine ids,> <user ids,>``.
EXIT_STATE = """
import json, sys
from sqlalchemy import func, select
from citar import db, engine_api, settings as site
from citar.bots import ratings
from citar.db.models import AuditLog, BudgetEntry, ComputeEvent, CpuJob, CpuJobRun, Server, User
from citar.jobs import trust as T
db.configure()
names = [x for x in sys.argv[1].split(",") if x]
machines = [x for x in sys.argv[2].split(",") if x]
users = [x for x in sys.argv[3].split(",") if x]
secret = site.get().secret_key
def iso(t):
    return t.isoformat() if t is not None else None
out = {"exps": {}, "machines": {}, "users": {}, "audits": {}, "budgets": {}, "settings": T.settings(fresh=True)}
with db.session() as s:
    for name in names:
        jobs = list(s.scalars(select(CpuJob).where(CpuJob.item_ref == name)))
        runs = list(s.scalars(select(CpuJobRun).where(CpuJobRun.job_id.in_([j.id for j in jobs] or ["-"]))))
        out["exps"][name] = {
            "jobs": [{"id": j.id, "ref": j.ref, "kind": j.kind, "state": j.state, "owner": j.owner_id,
                      "accepted_run": j.accepted_run, "result_hash": j.result_hash, "spec_hash": j.spec_hash,
                      "spec": j.spec, "jobs_build": j.jobs_build,
                      "sample": engine_api.sample_fraction(secret, j.id)} for j in jobs],
            "runs": [{"id": r.id, "job": r.job_id, "machine": r.machine_id, "executor": r.executor, "role": r.role,
                      "state": r.state, "verdict": r.verdict, "error": r.error, "result_hash": r.result_hash,
                      "chain_head": r.chain_head, "rounds": r.rounds, "claim": r.claim, "signature": r.signature,
                      "offered": iso(r.offered_at), "ended": iso(r.ended_at), "cpu_s": r.cpu_s}
                     for r in sorted(runs, key=lambda r: r.offered_at)],
            "events": [{"request": e.request_id, "machine": e.machine_id, "consumer": e.consumer_id,
                        "provider": e.provider_id, "verification": e.verification, "cpu": e.cpu_core_s,
                        "reported": e.reported_s, "observed": e.observed_s, "credits": e.credits,
                        "self_use": e.self_use, "flags": e.flags or {}}
                       for e in s.scalars(select(ComputeEvent).where(ComputeEvent.activity.like(f"lab:{name}:%")))]}
    for sid in machines:
        row = s.get(Server, sid)
        if row is not None:
            out["machines"][sid] = {"owner": row.owner_id, "key": row.public_key, "tier": row.trust_tier or "new",
                                    "trust": row.trust or {}, "name": row.name}
            out["audits"][sid] = [{"at": iso(a.at), "changed": (a.detail or {}).get("changed")}
                                  for a in s.scalars(select(AuditLog).where(
                                      AuditLog.action == "server.trust", AuditLog.object_id == sid)
                                      .order_by(AuditLog.id))]
    for uid in users:
        u = s.get(User, uid)
        if u is not None:
            out["users"][uid] = u.trust_tier
    for bid, spent in s.execute(select(BudgetEntry.budget_id, func.sum(BudgetEntry.actual))
                                .group_by(BudgetEntry.budget_id)):
        out["budgets"][bid] = spent
games, _entries = ratings.collect()
rated = {}
for g in games:
    rated[g["id"]] = rated.get(g["id"], 0) + 1
out["rated"] = rated
print(json.dumps(out, default=str))
"""

#: Jobs played again in the server's environment (the engine there: Linux for the WSL server), for the byte-for-byte
#: comparison: ``python -c EXIT_REPLAY <file of [{kind, spec, spec_hash}]>``; prints each result as compact JSON (the
#: order the engine wrote it in), its hash and its chain's head and rounds.
EXIT_REPLAY = """
import json, sys
from citar import engine_api
jobs = json.loads(open(sys.argv[1], encoding="utf-8").read())
out = []
for j in jobs:
    got = engine_api.run_job(j["kind"], j["spec"], spec_hash=j["spec_hash"])
    out.append({"result": json.dumps(got["result"], separators=(",", ":")), "result_hash": got["result_hash"],
                "head": got["chain"]["head"], "rounds": got["chain"]["rounds"]})
print(json.dumps({"jobs_build": engine_api.jobs_build(), "out": out}))
"""


def _server_put(h, rel: str, text: str) -> str:
    """``text`` written to ``rel`` in the instance's directory; the path the server's own processes read it by."""
    putter = getattr(h.server, "_put", None)
    if putter is not None:
        putter(rel, text)
        return f"{h.server.remote}/{rel}"
    path = h.server.state / rel
    path.write_text(text, encoding="utf-8")
    return str(path)


def _server_json(h, code: str, *args: str) -> dict:
    """``python -c code args`` in the server's environment (``_server_python``), its last line read as JSON."""
    lines = (_server_python(h, code, *args) or "").strip().splitlines()
    try:
        return json.loads(lines[-1]) if lines else {}
    except ValueError:
        return {"_error": lines[-1][:500]}


def _lab_submit(h, specs: list, owner: str):
    """``citar lab submit`` of ``specs`` as ``owner`` (an administrator's ranking and reference runs), in the server's
    environment."""
    import subprocess
    path = _server_put(h, f"qa-lab-{owner}-{h.tag}.json", json.dumps(specs))
    runner = getattr(h.server, "python", None)
    if runner is not None:
        return runner("-m", "citar.lab", "submit", path, "--owner", owner)
    return subprocess.run([sys.executable, "-m", "citar.lab", "submit", path, "--owner", owner], cwd=str(ROOT),
                          env=h.server.env, capture_output=True, text=True, timeout=120)


def _member(h, key: str, handle: str):
    """Another member made with ``admin_cli create-user`` and signed in (the exit's fourth member), with the run's
    throwaway password."""
    if key in h.accounts:
        return h.accounts[key]
    password = h.report.data.get("password") or ""
    made = h.server.cli("create-user", "--handle", handle, "--email", f"{handle}@example.com", "--role", "user",
                        "--password-stdin", stdin=password + "\n")
    if made.returncode != 0:
        raise q3.HarnessError(f"admin_cli create-user {handle} failed: {made.stdout.strip()} {made.stderr[-300:]}")
    who = q3.Account(h.server, handle, "user", password)
    if who.login().status_code != 200:
        raise q3.HarnessError(f"{handle} could not sign in")
    h.accounts[key] = who
    h.report.data.setdefault("accounts", []).append({"handle": handle, "role": "user"})
    return who


def _claim_signed(claim: dict, signature: str, key: str) -> bool:
    """Whether ``signature`` is ``key``'s ed25519 signature over the claim (P4.4.4: ``CITAR-CLAIM-V1\\0`` and the
    claim's compact JSON, its fields in order), checked here by another implementation than the server's."""
    import base64
    try:
        from cryptography.exceptions import InvalidSignature
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    except ImportError:
        raise q3.HarnessError("the claims' signatures are checked with the cryptography package, which this Python "
                              "lacks") from None
    try:
        body = json.dumps({k: claim[k] for k in CLAIM_FIELDS}, separators=(",", ":")).encode("utf-8")
        Ed25519PublicKey.from_public_bytes(base64.b64decode(key)).verify(base64.b64decode(signature),
                                                                        b"CITAR-CLAIM-V1\0" + body)
        return True
    except (InvalidSignature, KeyError, TypeError, ValueError):
        return False


def _result_of(line: dict) -> dict:
    """A results line's job result: the line without its measurements and its executor (P4.4.2)."""
    return {k: v for k, v in line.items() if k not in LINE_EXTRAS}


def _when(text) -> float:
    """A database time (ISO, UTC when it names no zone) as seconds since the epoch; 0 when there is none."""
    from datetime import datetime, timezone
    if not text:
        return 0.0
    t = datetime.fromisoformat(str(text))
    return (t if t.tzinfo else t.replace(tzinfo=timezone.utc)).timestamp()


def _replicated_why(log: str) -> dict:
    """``(experiment, game) -> why`` its primary's result was checked by a replica (``new``, ``sampled``, ``asked``),
    from the lab's log (the dispatcher's line for each such result)."""
    import re
    out: dict = {}
    pattern = re.compile(r"^\S+ \S+ (\S+) #(\d+): .*; checked by a replica before it is accepted \((\w+)\)$")
    for line in (log or "").splitlines():
        m = pattern.match(line.strip())
        if m:
            out.setdefault((m.group(1), int(m.group(2))), []).append(m.group(3))
    return out


def _lender(h, token: str, name: str, *, wsl: bool, ceiling: dict, faults: tuple = ()):
    """One of the exit's lending helpers: in WSL (``WslHelper``) or on Windows (``Helper``), its own ceiling lent at
    setup (``citar-helper setup``, answered yes to lending CPU, after the ceiling was written); ``faults`` its
    ``--test-fault`` modes (``citar-helper-faults``). On the fallback topology a WSL helper is a second Windows one."""
    import subprocess
    if wsl and getattr(h.server, "topology", "") == "wsl":
        return WslHelper(h, token, name, ceiling=ceiling, faults=faults)
    home = h.server.state / f"helper-home-{name}"
    home.mkdir(parents=True, exist_ok=True)
    (home / "helper.json").write_text(json.dumps({"cpu": ceiling}), encoding="utf-8")
    binary = faults_binary() if faults else q3.helper_binary()
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CITAR_WORKER_", "CITAR_HELPER_"))}
    env["CITAR_DATA_DIR"] = str(home)
    setup = subprocess.run([binary, "setup", "--home", str(home)], input=f"{h.server.base}\n{token}\ny\n",
                           capture_output=True, text=True, timeout=60, env=env)
    helper = Helper(h, token, h.endpoint, name, machine=name, binary=binary if faults else None,
                    extra=("--no-llm", *(x for m in faults for x in ("--test-fault", m))))
    helper.setup = setup
    helper.settings = lambda: {k: v for k, v in json.loads((home / "helper.json").read_text(encoding="utf-8")).items()
                               if k != "token"}
    return helper


def part_exit(h) -> None:
    """The ``exit`` part (the module's doc)."""
    r = h.report
    admin, alice, bob, carol = (h.accounts[k] for k in ("admin", "alice", "bob", "carol"))
    data = r.data.setdefault("exit", {"topology": getattr(h.server, "topology", "windows")})
    tag = h.tag
    host = "sv_qa_host"
    lenders: dict = {}
    watches: dict = {}
    try:
        dave = _member(h, "dave", "qa-dave")
        ids = {k: h.accounts[k].get("/api/auth/me").json()["user"]["id"] for k in ("alice", "bob", "carol", "dave")}
        # ---- the server: its job host lends one thread (the verifier, and a player); provisional results sampled
        cpus = os.cpu_count() or 1
        host_cpu = {"enabled": True, "threads": 1, "cpu_percent": max(1, min(100, 200 // cpus)), "memory_mb": 400,
                    "work": ALL_WORK}
        put = admin.put(f"/api/machines/{host}/settings", json={"capabilities": {"cpu": host_cpu}})
        settings = _verification(h, provisional_rate=EXIT_SAMPLE)
        caps = [admin.patch(f"/api/admin/users/{ids[k]}", json={"caps": {"run_lab": True}}) for k in ("bob", "carol")]
        # ---- alice's two machines in a group of hers, the grant to bob listing lab
        group = alice.post("/api/pool/groups", json={"name": f"QA exit lenders {tag}"})
        a_gid = group.json().get("id", "") if group.status_code == 200 else ""

        def machine(who, label: str, gid: str, site: dict) -> tuple:
            made = who.post("/api/pool/servers", json={"config": {"name": f"QA {label} {tag}", "kind": "owned",
                                                                  "connection": {"provider": "openai_compatible",
                                                                                 "max_parallel": 1}},
                                                       "reach": "worker", "group_id": gid})
            sid = made.json().get("id", "") if made.status_code == 200 else ""
            issued = who.post(f"/api/pool/servers/{sid}/workers/tokens", json={"label": label}) if sid else None
            token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
            who.put(f"/api/machines/{sid}/settings", json={"capabilities": {"cpu": site}})
            return sid, token

        w_sid, w_token = machine(alice, "exit Windows", a_gid, EXIT_SITE)
        l_sid, l_token = machine(alice, "exit WSL", a_gid, EXIT_SITE)
        grant = alice.post(f"/api/pool/groups/{a_gid}/grants", json={"subject_type": "user", "handle": bob.handle,
                                                                     "purposes": ["lab"]}) if a_gid else None
        listed = (grant.json().get("purposes") if grant is not None and grant.status_code == 200 else None)
        r.check("the exit's server (P4.7.6's topology: in WSL, its job host the verifier) lends one thread; bob and "
                "carol given run_lab by an administrator; alice's Windows and WSL helper machines in a group of hers, "
                "her grant to bob listing lab explicitly; a provisional machine's results sampled at "
                f"{EXIT_SAMPLE:g} (the part's setting), the rest of the verification settings the defaults",
                put.status_code == 200 and all(c.status_code == 200 for c in caps) and bool(w_token)
                and bool(l_token) and listed == ["lab"] and settings.get("new_streak") == 5
                and settings.get("provisional_rate") == EXIT_SAMPLE,
                json.dumps({"topology": data["topology"], "grant": listed, "settings": settings}))
        data.update(windows_machine=w_sid, wsl_machine=l_sid, settings=settings, host_cpu=host_cpu)

        # ---- her helpers, CPU lent at setup
        lenders["win"] = _lender(h, w_token, "qa-exit-win", wsl=False, ceiling=EXIT_WINDOWS)
        lenders["wsl"] = _lender(h, l_token, "qa-exit-wsl", wsl=True, ceiling=EXIT_WSL)

        def executor(sid: str) -> dict:
            got = admin.get("/api/admin/jobs")
            return next((e for e in ((got.json().get("dispatcher") or {}).get("executors") or [])
                         if e.get("machine") == sid), {}) if got.status_code == 200 else {}
        ready = wait_until(lambda: all(executor(s).get("state") == "ready" for s in (w_sid, l_sid)), 180, 0.5)
        view = admin.get("/api/admin/jobs").json()
        server_build = (view.get("dispatcher") or {}).get("jobs_build")
        shown = {}
        for key, sid in (("win", w_sid), ("wsl", l_sid)):
            got = alice.get(f"/api/machines/{sid}/settings")
            shown[key] = (got.json().get("cpu_helper") or {}) if got.status_code == 200 else {}
        grants = alice.get(f"/api/machines/{w_sid}/settings").json().get("lab_grants") or []
        lent = {k: (lenders[k].settings().get("cpu") or {}) for k in lenders}
        effective = {k: {f: (shown[k].get("effective") or {}).get(f) for f in ("threads", "cpu_percent", "memory_mb")}
                     for k in shown}
        r.check("both helpers lend CPU from their own settings (helper.json says enabled after citar-helper setup "
                "answered yes) and connect, their job hosts ready on the server's jobs build (one profile: "
                "cargo xtask develop's); the dialog shows each in effect, Windows 2 threads, 25%, 512 MB and WSL 2 "
                "threads, 50%, 512 MB, each limit hard, and the grant lending them to bob",
                bool(ready) and all(x.get("enabled") is True for x in lent.values())
                and all(lenders[k].setup.returncode == 0 for k in lenders)
                and bool(server_build) and all(executor(s).get("jobs_build") == server_build for s in (w_sid, l_sid))
                and effective == {"win": {"threads": 2, "cpu_percent": 25, "memory_mb": 512},
                                  "wsl": {"threads": 2, "cpu_percent": 50, "memory_mb": 512}}
                and all(shown[k].get("limits") == {"cpu": "hard", "memory": "hard"} for k in shown)
                and [g.get("subject") for g in grants] == [bob.handle],
                json.dumps({"lent": lent, "shown": {k: {f: v.get(f) for f in ("effective", "limits", "lent",
                                                                               "verifiable")}
                                                    for k, v in shown.items()},
                            "jobs_build": server_build, "grants": grants})[:900])
        data.update(jobs_build=server_build, shown={k: {f: v.get(f) for f in ("effective", "limits")}
                                                    for k, v in shown.items()})

        # ---- gate 2: bob's experiment, an administrator's ranking and reference runs, across the three executors
        main, ladder, base = f"qa-exit-{tag}", f"qa-exit-ladder-{tag}", f"qa-exit-reference-{tag}"
        watches["win"] = CpuWatch(lenders["win"].proc.pid).start()
        watches["wsl"] = (WslCpuWatch(lenders["wsl"]) if isinstance(lenders["wsl"], WslHelper)
                          else CpuWatch(lenders["wsl"].proc.pid)).start()
        t0 = time.perf_counter()
        queued = bob.post("/api/bots/experiments", json={
            "profiles": ["standard", "classic-production"], "name": main, "players": 2, "games": EXIT_GAMES,
            "seed": 15100, "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": EXIT_TURNS})
        submitted = _lab_submit(h, [
            {"name": ladder, "kind": "ranking", "games": EXIT_RANKING, "seed": 15200, "size": "duel",
             "maps": ["pangaea"], "turns": EXIT_TURNS,
             "seats": [{"profile": "standard"}, {"profile": "classic-production"}]},
            {"name": base, "kind": "reference", "games": EXIT_REFERENCE, "seed": 15300, "size": "duel",
             "maps": ["pangaea"], "turns": EXIT_TURNS}], admin.handle)
        done = wait_until(lambda: _lab_view(bob, main).get("state") == "complete"
                          and all(_lab_view(admin, n).get("state") == "complete" for n in (ladder, base))
                          and time.perf_counter(), 900, 1.0)
        cpu = {k: w.stop() for k, w in watches.items()}
        watches.clear()
        took = round(done - t0, 1) if done else None
        lines = {main: _lab_lines(h, main), ladder: _lab_lines(h, ladder), base: _lab_lines(h, base, "reference")}
        state: dict = {}
        machines_all = [w_sid, l_sid, host]

        def settled() -> bool:
            state.clear()
            state.update(_server_json(h, EXIT_STATE, ",".join((main, ladder, base)), ",".join(machines_all),
                                      ",".join(ids.values())))
            exps = state.get("exps") or {}
            runs = [x for n in exps for x in exps[n]["runs"] if x["state"] == "done"]
            events = {e["request"] for n in exps for e in exps[n]["events"]}
            return bool(runs) and all(x["id"] in events for x in runs) and not any(
                e["verification"] == "pending" for n in exps for e in exps[n]["events"])
        wait_until(settled, 60, 2.0)
        by_machine: dict = {}
        for n in (main, ladder, base):
            for x in lines[n]:
                m = (x.get("executor") or {}).get("machine")
                by_machine[m] = by_machine.get(m, 0) + 1
        sizes = {main: EXIT_GAMES, ladder: EXIT_RANKING, base: EXIT_REFERENCE}
        once = all(sorted(x.get("i") for x in lines[n]) == list(range(sizes[n])) for n in sizes)
        r.check("bob's 24 duels of 40 rounds (from the A/B route), an administrator's ranking experiment of 6 and "
                "reference run of 4 (citar lab submit, the server's own collection) all run to their end, dispatched "
                "across alice's two helpers and this server's job host (each playing some), each game in its results "
                "file once",
                queued.status_code == 200 and submitted.returncode == 0 and bool(done) and once
                and all(by_machine.get(m, 0) > 0 for m in machines_all) and set(by_machine) <= set(machines_all),
                json.dumps({"seconds": took, "lines": {n: len(v) for n, v in lines.items()},
                            "by_machine": {("windows" if m == w_sid else "wsl" if m == l_sid else "host"): c
                                           for m, c in by_machine.items()}}), seconds=took)
        data.update(experiments=[main, ladder, base], seconds=took, by_machine=by_machine)
        _exit_gate2(h, data, state, lines, main, ladder, base, w_sid, l_sid, host, ids, cpu, shown)

        # ---- gate 3, first pass: carol's helper in WSL alters its third result (--test-fault result:3)
        for key in ("win", "wsl"):
            lenders.pop(key).stop()
        _exit_cheat(h, data, lenders, machine, executor, ids, host)
        # ---- gate 3, second pass: dave's helper alters chosen results either side of an administrator's replica
        _exit_selective(h, data, lenders, machine, executor, ids, host, dave)
        if h.keep:
            # the browser checklist: alice's two machines connected and lending again (H1, H2; their homes and keys
            # kept), the quarantined machines and owners with their evidence (H5), the experiments' games (H4)
            lenders["win"] = _lender(h, w_token, "qa-exit-win", wsl=False, ceiling=EXIT_WINDOWS)
            lenders["wsl"] = _lender(h, l_token, "qa-exit-wsl", wsl=True, ceiling=EXIT_WSL)
            data["browser"] = {"windows": w_sid, "wsl": l_sid, "lenders_kept": True}
        r.check("no traceback in the server's log", h.server.tracebacks() == 0, f"{h.server.tracebacks()} traceback(s)")
    finally:
        for w in watches.values():
            try:
                w.stop()
            except Exception:
                pass
        if not h.keep:
            for helper in lenders.values():
                helper.stop()
            try:
                _verification(h, provisional_rate=0.10)
            except Exception:
                pass


def _exit_gate2(h, data: dict, state: dict, lines: dict, main: str, ladder: str, base: str, w_sid: str, l_sid: str,
                host: str, ids: dict, cpu: dict, shown: dict) -> None:
    """4-15 gate 2's checks on what the run left (the module's doc)."""
    if str(ROOT) not in sys.path:
        sys.path.insert(0, str(ROOT))
    from citar import engine_api
    r = h.report
    exps = state.get("exps") or {}
    machines = state.get("machines") or {}
    helpers = {w_sid: "windows", l_sid: "wsl"}
    jobs = {j["id"]: {**j, "exp": n} for n in exps for j in exps[n]["jobs"]}
    runs = [x for n in exps for x in exps[n]["runs"]]
    by_id = {x["id"]: x for x in runs}
    events = {e["request"]: e for n in exps for e in exps[n]["events"]}

    # ---- every accepted line signed by its machine (or this server's own), its claim the run's
    unsigned, bad = [], []
    accepted = []
    for n in (main, ladder, base):
        for line in lines[n]:
            ex = line.get("executor") or {}
            run = by_id.get(ex.get("run")) or {}
            job = jobs.get(run.get("job")) or {}
            accepted.append((n, line, run, job))
            if ex.get("machine") == host:
                if run.get("executor") != "local" or ex.get("verification") != "reference":
                    bad.append((n, line.get("i"), "host line not the server's own"))
                continue
            claim, sig = run.get("claim") or {}, run.get("signature") or ""
            key = (machines.get(ex.get("machine")) or {}).get("key") or ""
            ok = (bool(claim) and claim.get("run") == run.get("id") and claim.get("machine") == ex.get("machine")
                  and claim.get("job") == job.get("id") and claim.get("key") == key
                  and claim.get("result_hash") == run.get("result_hash") == job.get("result_hash")
                  and claim.get("chain_head") == run.get("chain_head") and claim.get("rounds") == run.get("rounds")
                  and claim.get("spec_hash") == job.get("spec_hash") and claim.get("jobs_build") == job.get("jobs_build")
                  and _claim_signed(claim, sig, key))
            if not ok:
                unsigned.append((n, line.get("i"), ex.get("machine")))
    r.check("every accepted line is signed by its machine (or is this server's own): its run's claim names the run, "
            "the machine, the job, its spec, the jobs build and the machine's registered key, and its result hash and "
            "chain head the run's, and its ed25519 signature verifies here (another implementation than the server's)",
            bool(accepted) and not unsigned and not bad,
            json.dumps({"lines": len(accepted), "signed": sum(1 for a in accepted if a[2].get("executor") == "remote"),
                        "unsigned": unsigned[:5], "bad": bad[:5]}))

    # ---- each game played again here (this process's engine, Windows): the same result hash and chain head, and the
    # chain folds; the line's own bytes hash to it
    replayed, differ = 0, []
    t0 = time.perf_counter()
    for n, line, run, job in accepted:
        if not job:
            differ.append((n, line.get("i"), "no job"))
            continue
        got = engine_api.run_job(job["kind"], job["spec"], spec_hash=job["spec_hash"])
        try:
            engine_api.job_chain_check(job["spec_hash"], got["chain"])
            folds = True
        except Exception:
            folds = False
        mine = engine_api.job_result_hash(job["kind"], _result_of(line))
        if not (folds and got["result_hash"] == run.get("result_hash") == mine
                and got["chain"]["head"] == run.get("chain_head") and got["chain"]["rounds"] == run.get("rounds")):
            differ.append((n, line.get("i"), (run.get("machine") or "")[:12]))
        replayed += 1
    r.check("every accepted game's chain is consistent: played again here (Windows) it folds from its spec's chain_0 "
            "to the head its run gave (and its helper signed), with the same rounds and result hash, and the line in "
            "the results file hashes to that result",
            replayed == len(accepted) and not differ,
            json.dumps({"replayed": replayed, "seconds": round(time.perf_counter() - t0, 1), "differ": differ[:5]}))

    # ---- replicated while new, sampled after, as each machine's tier said
    why = _replicated_why(_saves_text(h, "lab/lab.log") or "")
    rate = float((state.get("settings") or {}).get("provisional_rate") or EXIT_SAMPLE)
    streak = int((state.get("settings") or {}).get("new_streak") or 5)
    tiers, wrong, counts = {}, [], {}
    for sid in helpers:
        audits = (state.get("audits") or {}).get(sid) or []
        promo = next((a["at"] for a in audits if (a.get("changed") or {}).get("to") == "provisional"), None)
        tiers[helpers[sid]] = {"tier": (machines.get(sid) or {}).get("tier"), "provisional_at": promo}
        mine = [x for x in runs if x["machine"] == sid and x["role"] == "primary" and x["state"] == "done"]
        c = counts.setdefault(helpers[sid], {"new": 0, "sampled": 0, "unsampled": 0})
        for x in mine:
            job = jobs[x["job"]]
            replicas = [y for y in runs if y["job"] == x["job"] and y["role"] == "replica"]
            said = (why.get((job["exp"], int(job["ref"].rsplit(":", 1)[-1]))) or [None])[-1]
            if replicas:
                local = all(y["executor"] == "local" and y["machine"] == host for y in replicas)
                if said == "new":
                    c["new"] += 1
                    fine = local and (promo is None or _when(x["ended"]) < _when(promo))
                elif said == "sampled":
                    c["sampled"] += 1
                    fine = local and promo is not None and job["sample"] < rate
                else:
                    fine = False
                # a replica on this server is its reference: the primary matched it
                fine = fine and x["verdict"] == "match" and all(y["verdict"] in ("reference", "match")
                                                                for y in replicas)
            else:
                c["unsampled"] += 1
                fine = promo is not None and job["sample"] >= rate and x["verdict"] == "unchecked"
            if not fine:
                wrong.append({"machine": helpers[sid], "job": job["ref"], "why": said, "sample": round(job["sample"], 3),
                              "replicas": len(replicas), "verdict": x["verdict"]})
    r.check(f"each helper's results were replicated on this server's job host while its machine was new (all of them, "
            f"{streak} matches promoting it to provisional) and sampled after at the provisional rate by the server's "
            "keyed choice: every replicated result after the promotion one whose sample falls under the rate, every "
            "unreplicated one over it, each replica a match",
            not wrong and all(t["tier"] == "provisional" and t["provisional_at"] for t in tiers.values())
            and all(c["new"] >= streak for c in counts.values()),
            json.dumps({"tiers": tiers, "counts": counts, "wrong": wrong[:5]}))
    data.update(tiers=tiers, replication=counts)

    # ---- the replicated games' Windows and Linux results, byte for byte
    pairs = []
    for x in runs:
        if x["machine"] != w_sid or x["role"] != "primary" or x["state"] != "done":
            continue
        reps = [y for y in runs if y["job"] == x["job"] and y["role"] == "replica" and y["state"] == "done"]
        if reps:
            pairs.append((x, reps[-1]))
    replays = []
    for x, _rep in pairs:
        job = jobs[x["job"]]
        replays.append({"kind": job["kind"], "spec": job["spec"], "spec_hash": job["spec_hash"]})
    path = _server_put(h, f"qa-exit-replay-{h.tag}.json", json.dumps(replays))
    linux = _server_json(h, EXIT_REPLAY, path)
    here = [engine_api.run_job(j["kind"], j["spec"], spec_hash=j["spec_hash"]) for j in replays]
    line_of = {(line.get("executor") or {}).get("run"): line for n in lines for line in lines[n]}
    same, differ = 0, []
    for k, (x, rep) in enumerate(pairs):
        theirs = (linux.get("out") or [{}] * len(pairs))[k] if k < len(linux.get("out") or []) else {}
        mine = json.dumps(here[k]["result"], separators=(",", ":"))
        line = line_of.get(x["id"])
        filed = json.dumps(_result_of(line), separators=(",", ":")) if line else None
        if (x["result_hash"] == rep["result_hash"] and x["chain_head"] == rep["chain_head"]
                and theirs.get("result") == mine and (filed is None or filed == mine)
                and theirs.get("result_hash") == x["result_hash"] and theirs.get("head") == x["chain_head"]):
            same += 1
        else:
            differ.append(jobs[x["job"]]["ref"])
    linux_os = "linux" if getattr(h.server, "topology", "") == "wsl" else "windows (the fallback)"
    r.check("the replicated games' Windows and Linux results are equal byte for byte: each game the Windows helper "
            "played and this server's job host replicated has one result hash and chain head on both, and the "
            "result's bytes as the engine writes them are the same played again on Windows (here) and on Linux (in "
            "the server's environment), and the same as its line in the results file",
            bool(pairs) and same == len(pairs) and linux_os == "linux"
            and linux.get("jobs_build") == data.get("jobs_build"),
            json.dumps({"replicated_windows_games": len(pairs), "equal": same, "differ": differ[:5],
                        "server_os": linux_os, "linux_jobs_build": linux.get("jobs_build")}))
    data.update(byte_for_byte={"games": len(pairs), "equal": same})

    # ---- the ratings: exactly the accepted lab and ranking games, each once
    rated = state.get("rated") or {}
    want = {f"lab:{n}:{x.get('i')}" for n in (main, ladder) for x in lines[n] if not x.get("crash")}
    have = {g: c for g, c in rated.items() if g.split(":")[1] in (main, ladder, base)}
    r.check("the ratings include exactly the accepted lab and ranking games, each once (the reference run's are "
            "baselines, never rated)",
            set(have) == want and all(c == 1 for c in have.values()),
            json.dumps({"rated": len(have), "accepted": len(want), "extra": sorted(set(have) - want)[:3],
                        "missing": sorted(want - set(have))[:3]}))

    # ---- the ledger: one event a run, given and used as the run says, verified or not as its line says
    off = []
    for x in runs:
        if x["state"] != "done":
            continue
        job = jobs[x["job"]]
        e = events.get(x["id"])
        owner = (machines.get(x["machine"]) or {}).get("owner")
        line = line_of.get(x["id"])
        want_v = ("verified" if x["executor"] == "local" or x["verdict"] == "match" else "unverified")
        want_consumer = job["owner"] if x["role"] == "primary" else None
        want_provider = None if x["executor"] == "local" else owner
        if (e is None or e["machine"] != x["machine"] or e["verification"] != want_v
                or e["consumer"] != want_consumer or e["provider"] != want_provider
                or (line is not None and (line.get("executor") or {}).get("verification")
                    not in ("reference" if x["executor"] == "local" else want_v,))):
            off.append({"run": x["id"], "role": x["role"], "machine": helpers.get(x["machine"], "host"),
                        "event": {k: (e or {}).get(k) for k in ("verification", "consumer", "provider")}})
    bounded = [e for e in events.values() if e["provider"] == ids["alice"]
               and not e["cpu"] <= 1.5 * (e["flags"].get("reference_s") or 0) + 1e-6]
    r.check("the ledger: one cpu event a run, on its machine, with the verification its line has (verified for this "
            "server's and for a matched replica's primary, unverified for an unsampled result); a primary used by "
            "its experiment's owner (bob; nobody for the server's collection), a replica by nobody; given by "
            "alice for her machines' runs, by nobody for this server's; every one of hers bounded by 1.5 times the "
            "reference cost",
            not off and not bounded and len(events) >= sum(1 for x in runs if x["state"] == "done"),
            json.dumps({"events": len(events), "runs": sum(1 for x in runs if x["state"] == "done"), "off": off[:4],
                        "unbounded": [(e["cpu"], e["flags"].get("reference_s")) for e in bounded][:3]}))
    agreed, seen = _ledger_me(h, {"alice": ("given",), "bob": ("used",)}, (main, ladder, base), state)
    r.check("alice's given and bob's used, as /api/ledger/me tells them, equal the sums of their events (credits and "
            "counts)", agreed, json.dumps(seen)[:700])

    # ---- the limits held, hard on both, and were shown so
    held = {}
    for key, sid in (("win", w_sid), ("wsl", l_sid)):
        cap = (shown.get(key) or {}).get("effective", {}).get("cpu_percent") or 0
        c = cpu.get(key) or {}
        held[key] = {"cap": cap, **{k: c.get(k) for k in ("peak_pct", "mean_pct", "seconds", "cpus")},
                     "shown": (shown.get(key) or {}).get("limits")}
    r.check("the limits held, hard on both, and were shown so: the dialog said hard CPU and memory for each helper, "
            "and each job host's CPU over the run stayed under its cap plus slack (peak over 2 s windows)",
            all(v["shown"] == {"cpu": "hard", "memory": "hard"} and v["peak_pct"] is not None
                and v["peak_pct"] <= v["cap"] + 5 and (v["mean_pct"] or 0) > 0 for v in held.values()),
            json.dumps(held))
    data.update(limits=held)


def _ledger_me(h, sides: dict, names: tuple, state: dict) -> tuple:
    """``q3._ledger_agrees`` for any topology: each account's sides as ``/api/ledger/me?range=all`` tells them against
    the sums of the compute ledger's events in the server's database (``SELF_USE`` left out), read until two reads
    around the route's answer agree."""
    ids = {k: h.accounts[k].get("/api/auth/me").json()["user"]["id"] for k in sides}
    code = """
import json, sys
from sqlalchemy import func, select
from citar import db
from citar.db.models import ComputeEvent
db.configure()
out = {}
with db.session() as s:
    for key, uid in json.loads(sys.argv[1]).items():
        row = {}
        for side, col in (("given", ComputeEvent.provider_id), ("used", ComputeEvent.consumer_id)):
            credits, n = s.execute(select(func.coalesce(func.sum(ComputeEvent.credits), 0), func.count())
                                   .where(col == uid, ComputeEvent.self_use.is_(False))).one()
            row[side] = [float(credits), int(n)]
        out[key] = row
print(json.dumps(out))
"""
    seen: dict = {}

    def agrees() -> bool:
        before = _server_json(h, code, json.dumps(ids))
        told = {k: h.accounts[k].get("/api/ledger/me?range=all").json() for k in sides}
        after = _server_json(h, code, json.dumps(ids))
        seen.update(events=before, told={k: {s: (v.get(s) or {}) for s in ("given", "used")} for k, v in told.items()})
        if before != after or not before:
            return False
        for key, wanted in sides.items():
            for side in wanted:
                credits, n = before[key][side]
                got = told[key].get(side) or {}
                if abs(float(got.get("credits", -1)) - credits) > 1e-3 or int(got.get("events", -1)) != n:
                    return False
        return True
    return bool(wait_until(agrees, 90, 2.0)), seen


def _exit_cheat(h, data: dict, lenders: dict, machine, executor, ids: dict, host: str) -> None:
    """4-15 gate 3's first pass: carol's new machine, its helper in WSL altering its third result (``--test-fault
    result:3``), quarantined at that job with the evidence on ``#/admin/verification``'s data; her second machine
    refused CPU work."""
    r = h.report
    admin, carol = h.accounts["admin"], h.accounts["carol"]
    tag = h.tag
    group = carol.post("/api/pool/groups", json={"name": f"QA exit cheat {tag}"})
    gid = group.json().get("id", "") if group.status_code == 200 else ""
    site = {**EXIT_SITE, "threads": 1, "work": ["lab_game"]}
    c_sid, c_token = machine(carol, "exit cheat", gid, site)
    lenders["cheat"] = _lender(h, c_token, "qa-exit-cheat", wsl=True, faults=("result:3",),
                               ceiling={**EXIT_WSL, "threads": 1, "work": ["lab_game"]})
    ready = wait_until(lambda: executor(c_sid).get("state") == "ready", 180, 0.5)
    cheat = f"qa-exit-cheat-{tag}"
    queued = carol.post("/api/bots/experiments", json={
        "profiles": ["standard", "classic-production"], "name": cheat, "players": 2, "games": 10, "seed": 15400,
        "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": EXIT_TURNS})

    def tier(sid: str) -> str:
        got = admin.get(f"/api/machines/{sid}/jobs")
        return (got.json().get("machine") or {}).get("trust_tier", "") if got.status_code == 200 else ""
    caught = wait_until(lambda: tier(c_sid) == "quarantined", 300, 0.5)
    done = wait_until(lambda: _lab_view(carol, cheat).get("state") == "complete", 300, 1.0)
    state = _server_json(h, EXIT_STATE, cheat, c_sid, ids["carol"])
    exp = (state.get("exps") or {}).get(cheat) or {}
    mine = [x for x in exp.get("runs") or [] if x["machine"] == c_sid and x["role"] == "primary"]
    ev = ((state.get("machines") or {}).get(c_sid) or {}).get("trust", {}).get("last_mismatch") or {}
    lines = _lab_lines(h, cheat)
    page = admin.get("/api/admin/verification").json()
    on_page = next((q for q in (page.get("quarantined") or {}).get("machines") or [] if q["machine"] == c_sid), {})
    owners = {u["handle"] for u in (page.get("quarantined") or {}).get("owners") or []}
    words = "same chain, different result: tampering or an incompatible build"
    r.check("carol's new machine, its helper in WSL altering its third result (--test-fault result:3), is quarantined "
            "at its third job: its first two matched this server's replicas, the third gave the evidence \"same "
            "chain, different result: tampering or an incompatible build\" (shown with the machine on "
            "#/admin/verification's data, carol among the quarantined owners); its result never in the results file "
            "and its event rejected; the experiment ends on this server",
            bool(ready) and queued.status_code == 200 and bool(caught) and bool(done)
            and [x["verdict"] for x in mine[:3]] == ["match", "match", "mismatch"] and len(mine) == 3
            and ev.get("verdict") == "result" and ev.get("words") == words and ev.get("run") == mine[2]["id"]
            and (on_page.get("evidence") or {}).get("words") == words and carol.handle in owners
            and (state.get("users") or {}).get(ids["carol"]) == "quarantined"
            and mine[2]["id"] not in {(x.get("executor") or {}).get("run") for x in lines}
            and sorted(x["i"] for x in lines) == list(range(10))
            and any(e["request"] == mine[2]["id"] and e["verification"] == "rejected" and e["credits"] == 0
                    for e in exp.get("events") or []),
            json.dumps({"runs": [(x["verdict"], (x.get("error") or {}).get("code")) for x in mine],
                        "evidence": {k: ev.get(k) for k in ("verdict", "words", "round")},
                        "on_page": bool(on_page), "owners": sorted(owners)})[:700])
    data.update(cheat_machine=c_sid, cheat_evidence={k: ev.get(k) for k in ("verdict", "words", "run")})
    lenders.pop("cheat").stop()

    # ---- carol's second machine, honest, refused CPU work while she is quarantined
    o_sid, o_token = machine(carol, "exit other", gid, site)
    lenders["other"] = _lender(h, o_token, "qa-exit-other", wsl=False,
                               ceiling={**EXIT_WINDOWS, "threads": 1, "work": ["lab_game"]})
    ready_o = wait_until(lambda: q3._online(carol, o_sid), 120, 0.5)
    other = f"qa-exit-refused-{tag}"
    carol.post("/api/bots/experiments", json={
        "profiles": ["standard", "classic-production"], "name": other, "players": 2, "games": 3, "seed": 15500,
        "size": "duel", "maps": ["pangaea"], "speed": "Quick", "turns": EXIT_TURNS})
    done_o = wait_until(lambda: _lab_view(carol, other).get("state") == "complete", 180, 1.0)
    runs_o = admin.get(f"/api/machines/{o_sid}/jobs").json().get("runs") or []
    lines_o = _lab_lines(h, other)
    r.check("carol's second machine, an honest helper, is refused CPU work while its owner is quarantined: her next "
            "experiment plays on this server alone",
            bool(ready_o) and bool(done_o) and runs_o == [] and {x["executor"]["machine"] for x in lines_o} == {host},
            json.dumps({"runs": len(runs_o), "executor": executor(o_sid).get("reason")}))
    lenders.pop("other").stop()
    data.update(other_machine=o_sid)


def _exit_selective(h, data: dict, lenders: dict, machine, executor, ids: dict, host: str, dave) -> None:
    """4-15 gate 3's second pass: dave's machine, lending to bob under a budgeted grant, its helper altering its sixth
    and ninth results (``--test-fault result-at:6,9``): promoted by five matches, its sixth result accepted
    unverified, its seventh checked by an administrator's replica (a match), its eighth unverified, its ninth sampled
    once the rate is 1 and caught; the sixth and eighth withdrawn, played again elsewhere, the ledger, the budget and
    the ratings following."""
    r = h.report
    admin, bob = h.accounts["admin"], h.accounts["bob"]
    tag = h.tag
    group = dave.post("/api/pool/groups", json={"name": f"QA exit selective {tag}"})
    gid = group.json().get("id", "") if group.status_code == 200 else ""
    site = {**EXIT_SITE, "threads": 1, "work": ["lab_game"]}
    d_sid, d_token = machine(dave, "exit selective", gid, site)
    grant = dave.post(f"/api/pool/groups/{gid}/grants", json={
        "subject_type": "user", "handle": bob.handle, "purposes": ["lab"],
        "budget": {"period": "month", "amount": 1_000_000, "unit": "credits", "behavior": "hard"}})
    _verification(h, provisional_rate=0.0)
    lenders["selective"] = _lender(h, d_token, "qa-exit-selective", wsl=False, faults=("result-at:6,9",),
                                   ceiling={"threads": 1, "cpu_percent": 20, "memory_mb": 512, "work": ["lab_game"]})
    ready = wait_until(lambda: executor(d_sid).get("state") == "ready", 180, 0.5)
    sel = f"qa-exit-selective-{tag}"
    queued = bob.post("/api/bots/experiments", json={**EXIT_LONG, "name": sel, "games": 24, "seed": 15600})

    def look() -> tuple:
        got = admin.get(f"/api/machines/{d_sid}/jobs", params={"limit": 200})
        body = got.json() if got.status_code == 200 else {}
        prim = sorted((x for x in body.get("runs") or [] if x.get("role") == "primary"), key=lambda x: x["offered"])
        return (body.get("machine") or {}).get("trust_tier", ""), prim
    marks: dict = {}
    provisional = wait_until(lambda: look()[0] == "provisional", 600, 0.2)
    six = wait_until(lambda: (lambda p: p if len([x for x in p if x["state"] == "done"]) >= 6 else None)(look()[1]),
                     300, 0.05)
    asked = admin.post(f"/api/admin/machines/{d_sid}/replicate", json={"n": 1})
    marks["asked_after"] = len([x for x in (six or []) if x["state"] == "done"])
    marks["asked_seen"] = len([x for x in look()[1] if x["state"] == "done"])
    wait_until(lambda: (lambda p: p if len([x for x in p if x["state"] == "done"]) >= 8 else None)(look()[1]), 300, 0.05)
    before_lines = _lab_lines(h, sel)
    _verification(h, provisional_rate=1.0)
    marks["rate_after"] = len([x for x in look()[1] if x["state"] == "done"])
    caught = wait_until(lambda: look()[0] == "quarantined", 300, 0.2)
    _verification(h, provisional_rate=0.10)
    done = wait_until(lambda: _lab_view(bob, sel).get("state") == "complete", 900, 1.0)
    lenders.pop("selective").stop()
    state: dict = {}

    def written() -> bool:
        state.clear()
        state.update(_server_json(h, EXIT_STATE, sel, d_sid, ",".join([ids["dave"], ids["bob"]])))
        exp = (state.get("exps") or {}).get(sel) or {}
        events = {e["request"] for e in exp.get("events") or []}
        return all(x["id"] in events for x in exp.get("runs") or [] if x["state"] == "done") and not any(
            e["verification"] == "pending" for e in exp.get("events") or [])
    wait_until(written, 60, 2.0)          # the ledger's writer writes the events (and the budget's entries) in batches
    exp = (state.get("exps") or {}).get(sel) or {}
    runs = exp.get("runs") or []
    mine = [x for x in runs if x["machine"] == d_sid and x["role"] == "primary"]
    jobs = {j["id"]: j for j in exp.get("jobs") or []}
    trust = ((state.get("machines") or {}).get(d_sid) or {}).get("trust") or {}
    ev = trust.get("last_mismatch") or {}
    withdrawn = {w["run"] for w in ev.get("withdrawn_jobs") or []}
    audits = [a.get("changed") for a in (state.get("audits") or {}).get(d_sid) or []]
    lines = _lab_lines(h, sel)
    lined = {(x.get("executor") or {}).get("run") for x in lines}
    reps = {x["job"] for x in runs if x["role"] == "replica"}
    nine = mine[8] if len(mine) > 8 else {}
    r.check("dave's new machine (its helper altering its sixth and ninth results, --test-fault result-at:6,9) reaches "
            "provisional by five matches against this server's replicas; at the provisional rate 0 its sixth result "
            "(altered) is accepted unverified; an administrator then asks for its next replica (POST .../replicate), "
            "and its seventh (honest) is a verified match; its eighth is accepted unverified; with the rate at 1 its "
            "ninth (altered) is sampled, mismatches, and the machine and dave are quarantined",
            grant.status_code == 200 and bool(ready) and queued.status_code == 200 and bool(provisional)
            and asked.status_code == 200 and bool(caught) and len(mine) >= 9
            and audits[:1] == [{"from": "new", "to": "provisional"}]
            and [x["job"] in reps for x in mine[:9]] == [True] * 5 + [False, True, False, True]
            and marks.get("asked_seen") == 6 and marks.get("rate_after") == 8
            and ev.get("verdict") == "result" and ev.get("run") == nine.get("id")
            and (state.get("users") or {}).get(ids["dave"]) == "quarantined",
            json.dumps({"marks": marks, "audits": audits,
                        "primaries": [(x["state"], x["verdict"], x["job"] in reps) for x in mine[:10]],
                        "evidence": {k: ev.get(k) for k in ("verdict", "words", "run")}})[:800])
    six_run, eight_run, seven_run = (mine[5]["id"], mine[7]["id"], mine[6]["id"]) if len(mine) > 8 else ("", "", "")
    events = {e["request"]: e for e in exp.get("events") or []}
    old = {k: v for k, v in events.items() if k in (six_run, eight_run)}
    rated = state.get("rated") or {}
    was = {(x.get("executor") or {}).get("run") for x in before_lines}
    replayed = [x for x in lines if f"lab:{sel}:{x['i']}" in {jobs[j]["ref"] for j in (mine[5]["job"], mine[7]["job"])}]\
        if len(mine) > 8 else []
    r.check("every unverified result it gave is withdrawn, the sixth (before its verified match) and the eighth: "
            "their lines, in the results file (and rated) before the catch, leave it, their games are played again elsewhere and accepted (each game once), "
            "their events are rejected with no credits (the credits they had kept in flags), the seventh (verified) "
            "stays, and the ratings count each game of the experiment once, from its accepted line",
            {six_run, eight_run} <= withdrawn and seven_run not in withdrawn and bool(done)
            and {six_run, eight_run} <= was
            and not ({six_run, eight_run} & lined) and seven_run in lined
            and sorted(x["i"] for x in lines) == list(range(24))
            and len(replayed) == 2 and all((x.get("executor") or {}).get("machine") != d_sid for x in replayed)
            and all(e["verification"] == "rejected" and e["credits"] == 0 and (e["flags"].get("rejected_credits") or 0) > 0
                    for e in old.values()) and len(old) == 2
            and {g: c for g, c in rated.items() if g.split(":")[1] == sel} == {f"lab:{sel}:{i}": 1 for i in range(24)},
            json.dumps({"withdrawn": sorted(withdrawn), "six": six_run, "eight": eight_run,
                        "events": {k: {f: v.get(f) for f in ("verification", "credits")}
                                   | {"rejected_credits": v["flags"].get("rejected_credits")} for k, v in old.items()},
                        "replayed_on": [(x["i"], (x.get("executor") or {}).get("machine")) for x in replayed]})[:800])
    # the budget: the grant's settlements for the two withdrawn runs reversed
    settled = sum(float(e["flags"].get("rejected_credits") or 0) for e in old.values())
    budgets = (state.get("budgets") or {})
    b_flags = [e["flags"].get("budget") for e in old.values()]
    bid = (b_flags[0] or {}).get("id") if b_flags and b_flags[0] else None
    spent_now = float(budgets.get(bid) or 0) if bid else None
    kept = sum(float(e["credits"] or 0) for e in events.values()
               if e["machine"] == d_sid and (e["flags"].get("budget") or {}).get("id") == bid
               and e["verification"] != "rejected")
    r.check("the budget follows: the runs on dave's machine were settled against his grant's budget, the two "
            "withdrawn runs' settlements reversed (the budget's spending is what its runs still hold), and the "
            "ledger shows them rejected",
            bid is not None and all((f or {}).get("id") == bid for f in b_flags) and settled > 0
            and spent_now is not None and abs(spent_now - kept) < 1e-3,
            json.dumps({"budget": bid, "spent": spent_now, "runs_hold": round(kept, 6), "reversed": round(settled, 6),
                        "before": [(x.get("i"), (x.get("executor") or {}).get("machine")) for x in before_lines
                                   if (x.get("executor") or {}).get("run") in (six_run, eight_run)]}))
    data.update(selective_machine=d_sid, selective_withdrawn=sorted(withdrawn), selective_marks=marks)


# ---------------------------------------------------------------------------- the real part (4-15)

#: The ``real`` part's game: a small map, 6 rounds; chat timeouts long enough for a small model on a laptop.
REAL_CONFIG = {**q3.REAL_CONFIG, "seed": 4151, "turn_limit": 6}
#: A hybrid seat's model is prompted every this many turns.
REAL_PROMPT_EVERY = 2


def part_real(h) -> None:
    """The ``real`` part (the module's doc): LM Studio's model through alice's Windows helper, a 6-round game with her
    person seat, an LLM seat, a hybrid seat and two bots, the models answering her chats by their own calls; then
    4-04's cancel and baseline checks through the same helper binary (``exercise``)."""
    r = h.report
    admin, alice = h.accounts["admin"], h.accounts["alice"]
    data = r.data.setdefault("real", {})
    ready, said = q3.lmstudio_model(REAL_URL, REAL_MODEL)
    data["lmstudio"] = {"url": REAL_URL, "model": REAL_MODEL, "before": said}
    if not r.check(f"LM Studio at {REAL_URL} lists {REAL_MODEL}, loaded (read only: GET /api/v0/models; the part never "
                   "loads, unloads or swaps a model)", ready, said):
        return
    helper = None
    driver = None
    gid = ""
    t0 = time.time()
    try:
        made = alice.post("/api/pool/servers", json={"config": {"name": f"QA real {h.tag}", "kind": "owned",
                                                                "connection": {"provider": "lmstudio",
                                                                               "max_parallel": 1}},
                                                     "reach": "worker"})
        hid = made.json().get("id", "") if made.status_code == 200 else ""
        issued = alice.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa real"}) if hid else None
        token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
        if token:
            # the model's own allowlist: the helper offers nothing else, so nothing could ask LM Studio for another
            helper = Helper(h, token, REAL_URL, "qa-real", provider="lmstudio", max_concurrent=1,
                            extra=("--model", REAL_MODEL))
        online = bool(helper) and bool(wait_until(lambda: q3._online(alice, hid), 60, 0.25))
        info = _workers(alice, hid)
        card = next((s for s in alice.get("/api/pool/servers").json().get("servers", []) if s.get("id") == hid), {})
        live = {m.get("key"): m.get("loaded") for m in card.get("live_models") or []}
        r.check("alice's helper machine, the real Windows helper in front of LM Studio (one slot, the model its only "
                "one), protocol 2, reports the model loaded",
                online and live == {REAL_MODEL: True} and (info.get("worker") or {}).get("protocol") == 2,
                json.dumps({"online": online, "live": live, "target": (info.get("worker") or {}).get("target")}))
        data["machine"] = hid
        if not online:
            return
        model = {"server_id": hid, "model": REAL_MODEL}
        seats = [{"type": "human"},
                 {"type": "hybrid", "llm": dict(model), "bot": {"profile": "standard"},
                  "hybrid": {"owners": "all_but_war", "prompt_every": REAL_PROMPT_EVERY}},
                 {"type": "llm", "llm": {**model, "max_turn_seconds": q3.REAL_TURN_S}}, {"type": "bot"}, {"type": "bot"}]
        g = alice.post("/api/games", json={"name": f"QA real game {h.tag}", "config": dict(REAL_CONFIG),
                                           "seats": seats})
        info = g.json() if g.status_code == 200 else {}
        gid = info.get("id", "")
        tokens = [s.get("token", "") for s in info.get("seats") or []]
        r.check(f"alice's game: her person seat, a hybrid seat (prompted every {REAL_PROMPT_EVERY} turns), an LLM seat "
                f"on {REAL_MODEL} and two bots, {REAL_CONFIG['turn_limit']} rounds",
                bool(gid) and [s.get("type") for s in info.get("seats") or []] == ["human", "hybrid", "llm", "bot",
                                                                                    "bot"],
                f"HTTP {g.status_code} {g.text[:200] if g.status_code != 200 else ''}")
        if not gid:
            return
        data["game"] = gid
        alice.post(f"/api/games/{gid}/debug/meet_all")
        alice.post(f"/api/games/{gid}/debug/gold")
        driver = q3.SeatDriver(h, gid, tokens[0], 0, "alice")
        driver.hold.set()
        wait_until(lambda: driver.first is not None, 120, 0.5)

        def opened(res: dict) -> Optional[int]:
            return (res.get("result") or {}).get("negotiation_id") if res.get("ok") else None
        chats = {"hybrid": opened(driver.tool("open_negotiation", {
                     "to": 1, "message": "Hello neighbour. Would you trade your map for some of my gold?",
                     "give": [{"type": "gold", "amount": 30}], "receive": [{"type": "share_map"}]})),
                 "llm": opened(driver.tool("open_negotiation", {
                     "to": 2, "message": "Hello neighbour. Shall we share our maps?",
                     "give": [{"type": "share_map"}], "receive": [{"type": "share_map"}]}))}
        r.check("on her first turn alice opened a chat with each model seat", all(chats.values()), json.dumps(chats))
        driver.hold.clear()
        last, moved = None, time.perf_counter()
        deadline = time.perf_counter() + q3.REAL_LIMIT_S
        while time.perf_counter() < deadline:
            gi = q3._game_info(alice, gid)
            mark = (gi.get("turn"), gi.get("current_player"))
            if gi.get("phase") not in ("playing", None):
                break
            if mark != last:
                if last is None or mark[0] != last[0]:
                    r.note(f"real: turn {mark[0]} ({round(time.time() - t0)} s)")
                last, moved = mark, time.perf_counter()
            elif time.perf_counter() - moved > q3.REAL_STALL_S:
                break
            time.sleep(2.0)
        gi = q3._game_info(alice, gid)
        r.check(f"the game was played to its end ({REAL_CONFIG['turn_limit']} rounds), no turn hanging",
                gi.get("phase") not in ("playing", None),
                f"phase {gi.get('phase')}, turn {gi.get('turn')}; {round(time.time() - t0)} s",
                seconds=round(time.time() - t0, 1))
        time.sleep(4.0)                 # the ledger's writer inserts what it has every 2 s
        metrics = alice.get(f"/api/games/{gid}/metrics").json()
        rows = metrics.get("turns") or []
        llm_turns = [x for x in rows if x.get("player") == 2]
        phases = [x for x in metrics.get("diplomat") or [] if x.get("player") == 1]
        negotiations = metrics.get("negotiations") or []
        ends = [x.get("end_reason") for x in llm_turns]
        r.check("the LLM seat played every turn of the game, each ended by its own end_turn call (parsed on the "
                "server from the helper's raw completion), with actions of its own",
                len(llm_turns) >= REAL_CONFIG["turn_limit"] - 1 and all(e == "end_turn" for e in ends)
                and any((x.get("actions_ok") or 0) >= 1 for x in llm_turns),
                json.dumps({"turns": len(llm_turns), "ends": q3._counts(ends),
                            "tool_calls": sum(int(x.get("tool_calls") or 0) for x in llm_turns)}))
        by_model = [x for x in negotiations if x.get("outcome") == "answered" and x.get("cause") is None
                    and x.get("player") in (1, 2)]
        wrong = [x for x in negotiations if not q3._chat_ended_by_rule(x)]
        r.check("at least two chats answered by the models' own calls (the hybrid seat's model and the LLM seat's, "
                "each a tool call parsed on the server), and none failed by a model",
                len(by_model) >= 2 and {x.get("player") for x in by_model} == {1, 2} and not wrong,
                json.dumps({"answered": [{k: x.get(k) for k in ("player", "nid", "seconds", "gen_s", "steps")}
                                         for x in by_model],
                            "outcomes": q3._counts(f"{x.get('player')} {x.get('outcome')}/{x.get('cause')}"
                                                   for x in negotiations)})[:600])
        metered = (sum(float(x.get("model_s") or 0.0) for x in llm_turns)
                   + sum(float(x.get("gen_s") or 0.0) for x in negotiations)
                   + sum(float(x.get("gen_s") or 0.0) for x in phases))
        steps = (sum(int(x.get("model_steps") or 0) for x in llm_turns)
                 + sum(int(x.get("steps") or 0) for x in negotiations) + sum(int(x.get("steps") or 0) for x in phases))
        slack = 0.001 * (steps + len(llm_turns) + len(negotiations) + len(phases)) + 0.01
        ledger = _server_json(h, """
import json, sys
from sqlalchemy import func, select
from citar import db
from citar.db.models import ComputeEvent
db.configure()
with db.session() as s:
    n, gen = s.execute(select(func.count(), func.coalesce(func.sum(ComputeEvent.gen_s), 0)).where(
        ComputeEvent.machine_id == sys.argv[1], ComputeEvent.kind == "gpu",
        ComputeEvent.activity == f"game:{sys.argv[2]}")).one()
print(json.dumps({"events": int(n), "gen_s": float(gen)}))
""", hid, gid)
        r.check("the ledger's generation seconds for the game on the machine equal the scheduler's (the game's metrics: "
                "the LLM seat's turns, the chat answers, the diplomat phases)",
                (ledger.get("events") or 0) > 0 and abs(float(ledger.get("gen_s") or 0) - metered) <= slack,
                f"metered {metered:.3f} s ({steps} steps, within {slack:.3f} s); ledger {ledger}",
                metered_s=round(metered, 3), ledger_s=round(float(ledger.get("gen_s") or 0), 3))
        status = q3._machine_status(admin, hid)
        counters = status.get("counters") or {}
        r.check("no slot of the machine drained over the game (nothing cancelled went unacknowledged)",
                status.get("draining", 0) == 0 and counters.get("draining", 0) == 0
                and counters.get("cancel_unacked", 0) == 0, json.dumps({"counters": counters}))
        data.update(seconds=round(time.time() - t0, 1), llm_turns=len(llm_turns), ends=q3._counts(ends),
                    chats=[{k: x.get(k) for k in ("player", "nid", "outcome", "cause", "seconds", "gen_s")}
                           for x in negotiations],
                    phases=[{k: x.get(k) for k in ("turn", "outcome", "cause", "steps", "gen_s")} for x in phases],
                    metered_gen_s=round(metered, 3), ledger=ledger, counters=counters,
                    alice={"turns": len(driver.ended), "answered": {str(k): v for k, v in driver.answered.items()}})
    finally:
        if driver is not None:
            driver.stop()
        if gid and not h.keep:
            try:
                alice.post(f"/api/games/{gid}/control", json={"paused": True})
                alice.delete(f"/api/games/{gid}")
            except Exception:
                pass
        if helper is not None:
            helper.stop()
    # ---- the cancel: 4-04's checks, the same Windows helper binary in front of LM Studio, through a hub and the
    # prompt scheduler of this process's own (its database in <dir>/real-cancel): a 1,500-token generation cancelled
    # at 2 s acknowledged within a second, the next short requests within the run's own baseline, nothing drained
    cancel = data.setdefault("cancel", {})
    _own_instance(h.server.state / "real-cancel")
    exercise(r.check, REAL_URL, h.server.state, cancel)
    after, said_after = q3.lmstudio_model(REAL_URL, REAL_MODEL)
    data["lmstudio"]["after"] = said_after
    r.check(f"{REAL_MODEL} is still loaded at the part's end, as at its start", after, said_after)

PARTS: dict[str, Callable] = {"helper": part_helper, "llm-real": part_llm_real, "scan": part_scan,
                              "lab": part_lab, "remote": part_remote, "verify": part_verify, "update": part_update,
                              "service": part_service, "exit": part_exit, "real": part_real}
#: The parts ``all`` runs with the server in WSL (P4.7.6's topology: the service is installed there, and the exit's
#: experiment is the cross-OS one), after the others, which run with a server on Windows; ``real`` ends the run there.
WSL_PARTS = ("service", "exit", "real")
#: The parts that need the server in WSL and the helper built there (``--wsl``, ``--wsl-helper``).
NEEDS_WSL = ("service", "exit")


def _run(args) -> tuple:
    """One harness run of ``args.part`` (or of every part in ``q3.PARTS`` for ``all``) with the server on Windows or,
    with ``args.wsl``, in WSL: (exit code, the summary written to ``qa-<part>.json``). Raises ``q3.Refused``."""
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
    summary["where"] = where
    return code, summary


def _run_all(args) -> int:
    """``all``: every part, in ``PARTS``' order, in two harness runs, each on a server of its own in a folder of the
    run's directory: the parts written for a server on Windows first (``<dir>/<name>-windows``), then ``WSL_PARTS``
    with the server in WSL (``<dir>/<name>-wsl``). ``qa-all.json`` holds both runs' summaries; exit 0 only when every
    check of both passed."""
    skip = set(args.skip or ())
    top = Path(args.dir).expanduser()
    q3.check_dir(top)
    q3.claim(top.resolve() if top.exists() else top)
    top = top.resolve()
    groups = [("windows", [p for p in PARTS if p not in skip and p not in WSL_PARTS], False),
              ("wsl", [p for p in PARTS if p not in skip and p in WSL_PARTS], True)]
    runs: dict = {}
    code = 0
    t0 = time.perf_counter()
    for label, parts, on_wsl in groups:
        if not parts:
            continue
        sub = argparse.Namespace(**vars(args))
        sub.part, sub.skip, sub.wsl = "all", [], on_wsl
        sub.dir = str(top / f"{top.name}-{label}")
        q3.PARTS = {p: PARTS[p] for p in parts}
        print(f"all: the {label} run ({', '.join(parts)})", flush=True)
        try:
            got, summary = _run(sub)
        except q3.Refused as e:
            got, summary = 1, {"ok": False, "refused": str(e)}
        finally:
            q3.PARTS = PARTS
        runs[label] = {"dir": sub.dir, "parts": parts, "ok": got == 0, "summary": summary.get("parts"),
                       "checks": len(summary.get("checks") or []),
                       "passed": sum(1 for c in summary.get("checks") or [] if c["ok"]),
                       "failed": [c["check"] for c in summary.get("checks") or [] if not c["ok"]],
                       "remote": summary.get("remote"), "seconds": summary.get("seconds")}
        code = code or got
    out = {"part": "all", "ok": code == 0, "runs": runs, "skipped": sorted(skip),
           "seconds": round(time.perf_counter() - t0, 1)}
    (top / "qa-all.json").write_text(json.dumps(out, indent=2, default=str) + "\n", encoding="utf-8")
    passed = sum(r["passed"] for r in runs.values())
    total = sum(r["checks"] for r in runs.values())
    print(f"{'OK' if code == 0 else 'FAILED'}: all: {passed} of {total} checks passed in {len(runs)} runs"
          f"{' (skipped: ' + ', '.join(sorted(skip)) + ')' if skip else ''}; evidence in {top / 'qa-all.json'}",
          flush=True)
    return code


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
    ap.add_argument("--wsl", action="store_true",
                    help="the server in WSL (the remote, service, exit and real parts, and all's last run: 4-10, "
                         "4-14, 4-15)")
    ap.add_argument("--wsl-root", default=None, metavar="PATH", help="--wsl: the checkout in WSL the server runs from")
    ap.add_argument("--wsl-python", default=None, metavar="PATH",
                    help="--wsl: the Python in WSL that runs it (a venv's, with the extension cargo xtask develop built)")
    ap.add_argument("--wsl-distro", default="Ubuntu-24.04", help="--wsl: the WSL distribution (P4.7.1's)")
    ap.add_argument("--wsl-user", default="claude", help="--wsl: the WSL user (P4.7.1's)")
    ap.add_argument("--wsl-helper", default=None, metavar="PATH",
                    help="service, exit: the helper in WSL (cargo xtask develop's citar-ext/citar-helper there; "
                         "citar-helper-faults beside it)")
    args = ap.parse_args(argv)
    # the instance's server plays the lab (the lab part): the test suite's switch, if this runs under it, stays off
    os.environ.pop("CITAR_TEST_NO_DISPATCHER", None)
    global LLM_URL, STAND_IN, WSL_HELPER
    LLM_URL, STAND_IN, WSL_HELPER = args.llm_url, args.stand_in, args.wsl_helper
    q3.HELPER_BIN = args.helper_bin
    if args.wsl and args.part not in ("remote", *WSL_PARTS, "all"):
        print("refused: --wsl belongs to the remote, service, exit, real and all runs (packages 4-10, 4-14, 4-15)",
              flush=True)
        return 2
    wanted = [p for p in PARTS if p not in set(args.skip)] if args.part == "all" else [args.part]
    if any(p in NEEDS_WSL for p in wanted) and not (args.wsl and args.wsl_helper):
        print("refused: the service and exit parts run with the server in WSL and a helper built there (design "
              "P4.7.1 rule 8, P4.7.6): give --wsl --wsl-root --wsl-python and --wsl-helper (absolute paths inside "
              "WSL), or, for a development run of all, --skip service --skip exit", flush=True)
        return 2
    if args.wsl_helper and not (args.wsl_helper.startswith("/") and ":" not in args.wsl_helper):
        print("refused: --wsl-helper is an absolute path inside WSL (from Git Bash, set MSYS_NO_PATHCONV=1)", flush=True)
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
    try:
        if args.part == "all":
            return _run_all(args)
        code, _summary = _run(args)
    except q3.Refused as e:
        print(f"refused: {e}", flush=True)
        return 2
    return code


if __name__ == "__main__":
    sys.exit(main())

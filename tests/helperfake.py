"""The fake helper: protocol 2 from the helper's side, played against the real hub (design/phase4-helper.md P4.2.9).

- ``FakeClock``: the time a transcript with ``"clock": "fake"`` runs on, shared by the hub (its keep-alive, the 30 s
  for a hello, the rate limit's window) and the prompt scheduler (the cancel's acknowledgement). It moves only when the
  player moves it, and pokes the hub so the keep-alive looks at once.
- ``hello``: a protocol 2 hello's fields, for the stubs and the fake helper.
- ``StubConnection``: a connected helper with no socket, for tests of what sits on the hub (the scheduler, the pages):
  it answers each request with a ``done`` at once, or holds it.
- ``HubServer``: the real app under uvicorn on a free loopback port, in a thread, for a test class.
- ``FakeHelper``: a real WebSocket to the hub (``websockets``' synchronous client) that sends and reads frames,
  answers pings on its own, and can close or drop its connection.
- ``HubPlayer``: plays a conformance transcript from the hub's side: the fake helper sends the transcript's ``h2s``
  frames (realized by ``tests/helper_fixtures``), checks every frame the hub sends against its ``s2h`` patterns, runs
  the hub's ``act`` words through the hub's own code (the routes' functions, the scheduler, ``WorkerHub``'s test
  hooks, the job dispatcher's) and checks its ``state`` words on the hub, its scheduler, its job dispatcher and the
  database. A word it cannot do yet (a later package's) is refused, not skipped (PROTOCOL.md, "Actions"). A job
  transcript (4-10) gets a job dispatcher of its own, with no local executor, so the machine's helper is its only
  executor; its experiment is the golden ``duel-a``'s game (``crates/citar-jobs/golden/jobs.json``), so the offers
  carry exactly the golden spec.

Replaces protocol 1's test helpers (``tests/test_worker.py``'s connections and the real Python worker in threads).
"""
from __future__ import annotations

import base64
import json
import socket
import threading
import time
from typing import Any, Optional
from unittest import mock

from citar.server import helper_protocol as HP
from citar.server import workers as W
from tests import helper_fixtures as F

#: The address every connection of the fake helper comes from (the hub's rate limit counts it).
ADDRESS = "127.0.0.1"
#: How long a step waits for the hub when the transcript gives no ``within_s`` (seconds of real time).
DEFAULT_WAIT = 10.0
#: How long a step waits in real time before it moves a fake clock forward.
REAL_GRACE = 0.4


class FakeClock:
    """A clock that moves only when told: ``time`` (wall seconds) and ``monotonic`` (the scheduler's), starting at
    the real ones."""

    def __init__(self):
        self._wall0 = time.time()
        self._mono0 = time.perf_counter()
        self.offset = 0.0
        self._lock = threading.Lock()

    def time(self) -> float:
        return self._wall0 + self.offset

    def monotonic(self) -> float:
        return self._mono0 + self.offset

    def advance(self, seconds: float) -> None:
        with self._lock:
            self.offset += seconds
        W.hub().poke()


def key_b64(seed: int = 0) -> str:
    """A machine key's public half, base64 of 32 bytes (not a real ed25519 key: the hub never verifies one before
    package 4-11)."""
    return base64.b64encode(bytes((seed + i) % 256 for i in range(32))).decode("ascii")


def hello(*, token: str = "cwt_test", models: Optional[list] = None, slots: int = 1, key: Optional[str] = None,
          llm: bool = True, hostname: str = "stub", version: str = "0.1.6", quiet_until=None) -> dict:
    """A protocol 2 hello's fields, as ``helper_protocol`` parses one (the stubs' and the fake helper's)."""
    return {
        "t": "hello", "protocol": 2, "token": token,
        "helper": {"version": version, "build": "0b412a18fe95", "jobs_build": None,
                   "target": "x86_64-pc-windows-msvc", "label": "test"},
        "machine": {"hostname": hostname, "name": hostname, "os": "windows", "arch": "x86_64",
                    "public_key": key or key_b64()},
        "llm": {"enabled": llm, "provider": "lmstudio" if llm else None, "slots": slots if llm else 0,
                "stream": True, "models": list(models or [])},
        "cpu": {"enabled": False, "ceiling": {"threads": 1, "cpu_percent": 10, "memory_mb": 512, "work": []},
                "limits": {"cpu": "soft", "memory": "soft"}, "verifiable": False, "bots": [], "ruleset": "0" * 64},
        "report": {"hardware": None, "hardware_source": "none"},
        "running": [], "results": [], "updates": "off", "quiet_until": quiet_until,
    }


def settings(*, llm: bool = True, slots: int = 1, allow_swaps: bool = False) -> dict:
    """The site's settings as the ``settings`` frame carries them (CPU sharing off)."""
    return {"llm": {"enabled": llm, "slots": slots, "allow_swaps": allow_swaps},
            "cpu": {"enabled": False, "threads": 0, "cpu_percent": 0, "memory_mb": 0, "work": [],
                    "yield_to_llm": True}}


def model(key: str, loaded: bool = True, context: Optional[int] = None, label: str = "") -> dict:
    """One model of a hello or a catalog."""
    return {"key": key, "label": label or key, "context": context, "loaded": loaded}


class StubConnection(W.WorkerConnection):
    """A connected helper with no socket. It answers each request at once with a ``done`` (``CITAR OK``), or, with
    ``hold``, keeps it until ``answer_held``; every frame the hub sends it is parsed and kept (``frames``). Register it
    with ``W.hub().register``."""

    def __init__(self, server_id: str, models: Optional[list] = None, *, slots: int = 1, hold: bool = False,
                 site: Optional[dict] = None, content: str = "CITAR OK", gen_s: float = 0.01,
                 usage: Optional[dict] = None, hostname: str = "stub"):
        super().__init__(websocket=None, server_id=server_id, loop=None,
                         hello=hello(models=models, slots=slots, hostname=hostname),
                         settings=site or settings(slots=slots))
        self.welcomed = True
        self.hold = hold
        #: Whether a ``cancel`` of a held request is acknowledged at once (``cancelled``), as a protocol 2 helper does;
        #: false plays one that never acknowledges (its slot drains, P4.5.3).
        self.ack_cancel = True
        self.held: list = []
        self.frames: list = []
        self.content = content
        self.gen_s = gen_s
        self.usage = usage if usage is not None else {"prompt_tokens": 12, "completion_tokens": 3,
                                                      "reasoning_tokens": None, "cached_tokens": None}

    def send_threadsafe(self, text: str):
        frame = HP.parse_server_frame(text)
        self.frames.append(frame)
        if frame.get("t") == "settings":
            # acknowledged at once as a correct helper does: its own slots the ceiling, no CPU lent
            llm = self.hello_llm
            self.acknowledge({"effective": {"llm": {"enabled": frame["llm"]["enabled"] and llm["enabled"],
                                                    "slots": min(frame["llm"]["slots"], llm["slots"])},
                                            "cpu": {**frame["cpu"], "enabled": False}},
                              "capped": [], "limits": {"cpu": "soft", "memory": "soft"}})
            return
        if frame.get("t") == "cancel" and self.ack_cancel and frame["id"] in self.held:
            # a protocol 2 helper stops the generation and says so: ``cancelled``, the request's late frame
            self.held.remove(frame["id"])
            self.resolve(frame["id"], error=W.WorkerError("cancelled", refusal="cancelled"))
            return
        if frame.get("t") != "request":
            return
        if self.hold:
            self.held.append(frame["id"])
        else:
            self.done(frame["id"])

    def done(self, request_id: str, *, content: Optional[str] = None, tool_calls: Optional[list] = None,
             gen_s: Optional[float] = None):
        """Answer a request as a helper does: ``done`` with the raw message."""
        self.resolve(request_id, result={
            "t": "done", "id": request_id,
            "message": {"content": self.content if content is None else content, "reasoning": None,
                        "tool_calls": list(tool_calls or [])},
            "finish_reason": "tool_calls" if tool_calls else "stop", "usage": self.usage,
            "gen_s": self.gen_s if gen_s is None else gen_s, "ttft_s": None, "streamed": True})

    def answer_held(self):
        while self.held:
            self.done(self.held.pop(0))

    def requests(self) -> list:
        return [f for f in self.frames if f.get("t") == "request"]


def until(check, limit: float = 10.0, step: float = 0.01):
    """``check()``'s first true value within ``limit`` seconds (real time), else its last."""
    deadline = time.perf_counter() + limit
    while time.perf_counter() < deadline:
        value = check()
        if value:
            return value
        time.sleep(step)
    return check()


# ---------------------------------------------------------------------------- the real hub on a port

class HubServer:
    """The app under uvicorn on a free loopback port, in a thread. The app's start makes process-wide workers that
    would outlive it: the host's power sampler is not started, games other modules left open are not restored, and the
    benchmark scheduler, the probe and report runners it made are put back as they were when it stops."""

    def __init__(self, port: Optional[int] = None):
        import uvicorn
        from citar import usage
        from citar.server import app as appmod, reports_api
        from citar.server.session import SessionManager
        if port is None:
            with socket.socket() as probe:
                probe.bind((ADDRESS, 0))
                port = probe.getsockname()[1]
        self.port = port
        # as ``python -m citar.server`` runs it: the app reads the forwarded address itself (``ratelimit.client_ip``,
        # by its own settings), so uvicorn must not rewrite the client from a header first
        self.server = uvicorn.Server(uvicorn.Config(appmod.app, host=ADDRESS, port=self.port, log_level="error",
                                                    proxy_headers=False, forwarded_allow_ips=None))
        self.thread = threading.Thread(target=self.server.run, daemon=True, name="hub-server")
        self.saved = (appmod.scheduler, appmod._probe_runner, reports_api._runner)
        self.sampler = usage._sampler
        with mock.patch.object(SessionManager, "restore_live", lambda self: []), \
                mock.patch.object(usage, "start_power_sampler", lambda follow_host=False: None):
            self.thread.start()
            if not until(lambda: self.server.started, 20):
                raise RuntimeError("the hub's server did not start")
        self.url = f"ws://{ADDRESS}:{self.port}/ws/worker"
        self.base = f"http://{ADDRESS}:{self.port}"

    def stop(self):
        from citar.server import app as appmod, reports_api
        self.server.should_exit = True
        self.thread.join(timeout=15)
        if appmod.scheduler is not None and appmod.scheduler is not self.saved[0]:
            appmod.scheduler.stop()
        appmod.scheduler, appmod._probe_runner, reports_api._runner = self.saved
        W.hub().clock = W.Clock()


# ---------------------------------------------------------------------------- the real helper

class NoHelperBinary(RuntimeError):
    """``CITAR_HELPER_BIN`` names no file: a broken setup (CI's artifact path, its exec bit), never a reason to skip."""


def helper_binary() -> Optional[str]:
    """The CITAR helper to run: ``CITAR_HELPER_BIN`` (CI's test jobs name the build-ext job's), else the one ``cargo
    xtask develop`` built (with its development guard: loopback servers only, an explicit ``--home``) beside the
    extension in ``CITAR_EXT_DIR``; None when there is none (the end-to-end tests skip, except on CI, where
    ``require_helper_binary`` fails them). A ``CITAR_HELPER_BIN`` that names no file raises ``NoHelperBinary``: it was
    meant to run them."""
    import os
    from pathlib import Path
    given = os.environ.get("CITAR_HELPER_BIN")
    if given:
        if not Path(given).is_file():
            raise NoHelperBinary(f"CITAR_HELPER_BIN names {given!r}, which is not a file")
        return given
    folder = os.environ.get("CITAR_EXT_DIR")
    if not folder:
        return None
    for name in ("citar-helper.exe", "citar-helper"):
        path = Path(folder) / name
        if path.is_file():
            return str(path)
    return None


def require_helper_binary(what: str) -> str:
    """The helper binary for tests that run it (``helper_binary``): without one they skip (``unittest.SkipTest``),
    but on CI (``CI`` set, as GitHub Actions sets it) they fail, so a workflow cannot go green without running them."""
    import os
    import unittest
    binary = helper_binary()
    if binary is not None:
        return binary
    if os.environ.get("CI"):
        raise NoHelperBinary(f"{what}: no citar-helper on CI (set CITAR_HELPER_BIN, or CITAR_EXT_DIR with the helper "
                             "cargo xtask develop builds)")
    raise unittest.SkipTest(f"{what}: no citar-helper in CITAR_EXT_DIR (cargo xtask develop builds it)")


def fake_probe() -> Optional[str]:
    """``citar-fake-probe`` beside the helper binary (``helper_binary``): the stand-in for the scan's probe programs
    that development builds of the helper run when ``CITAR_HELPER_NVIDIA_SMI`` (or ``_POWERSHELL``,
    ``_SYSTEM_PROFILER``) names it (4-06). None when there is none."""
    from pathlib import Path
    binary = helper_binary()
    if binary is None:
        return None
    for name in ("citar-fake-probe.exe", "citar-fake-probe"):
        path = Path(binary).with_name(name)
        if path.is_file():
            return str(path)
    return None


class RealHelper:
    """The real binary as a child: its own home (``--home``, and ``CITAR_DATA_DIR`` with it) in a temporary directory,
    the hub on loopback, its model endpoint the given URL; its log beside its home. It reports no hardware and takes
    no power readings (``--no-hardware --no-power``) unless ``sensors``: then it scans and measures with the probe
    programs ``env`` names (``CITAR_HELPER_NVIDIA_SMI`` and the others: the fake probe), or the machine's own."""

    def __init__(self, server_url: str, token: str, endpoint_url: str, *, name: str = "e2e helper",
                 slots: int = 1, extra: tuple = (), home=None, sensors: bool = False, env: Optional[dict] = None):
        import os
        import subprocess
        import tempfile
        from pathlib import Path
        binary = helper_binary()
        if binary is None:
            raise RuntimeError("no citar-helper in CITAR_EXT_DIR: run cargo xtask develop")
        self.home = Path(home or tempfile.mkdtemp(prefix="citar-helper-home-"))
        self.home.mkdir(parents=True, exist_ok=True)
        self.log = self.home.parent / f"{self.home.name}.log"
        given = env or {}
        env = {k: v for k, v in os.environ.items() if not k.startswith(("CITAR_WORKER_", "CITAR_HELPER_"))}
        env.update(given)
        env["CITAR_DATA_DIR"] = str(self.home)
        self.args = [binary, "--home", str(self.home), "--server", server_url, f"--token={token}",
                     "--base-url", endpoint_url, "--provider", "openai_compatible", "--name", name,
                     "--max-concurrent", str(slots), *(() if sensors else ("--no-hardware", "--no-power")), *extra]
        out = open(self.log, "w", encoding="utf-8")
        self.proc = subprocess.Popen(self.args, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                     env=env)
        out.close()
        self.binary = binary

    def version(self) -> str:
        """``--version`` with its home: the key's fingerprint is in it once the home holds a key."""
        import subprocess
        return subprocess.run([self.binary, "--version", "--home", str(self.home)], capture_output=True, text=True,
                              timeout=30).stdout

    def fingerprint(self) -> str:
        import re
        found = re.search(r"machine key ([0-9a-f]{16})", self.version())
        return found.group(1) if found else ""

    def exited(self, timeout: float) -> Optional[int]:
        """Its exit code if it stopped by itself within ``timeout`` seconds, else None."""
        import subprocess
        try:
            return self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            return None

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except Exception:
                self.proc.kill()

    def text(self) -> str:
        return self.log.read_text(encoding="utf-8", errors="replace") if self.log.exists() else ""


# ---------------------------------------------------------------------------- the fake helper's socket

class Closed(Exception):
    """The connection ended: ``code`` and ``reason`` of the close frame the hub sent, or ``code`` None for a drop."""

    def __init__(self, code: Optional[int], reason: str = ""):
        super().__init__(f"closed {code} {reason!r}")
        self.code, self.reason = code, reason


class FakeHelper:
    """One WebSocket to the hub, as a helper holds it. ``recv`` returns the hub's next frame parsed
    (``parse_server_frame``), answering pings on its own unless ``answer_pings`` is false; a frame of another type is
    kept for the next ``recv``. A closed or dropped connection raises ``Closed``."""

    def __init__(self, url: str, *, answer_pings: bool = True, headers: Optional[dict] = None):
        import inspect

        from websockets.sync.client import connect
        self.url = url
        self.answer_pings = answer_pings
        options = {"max_size": None, "open_timeout": 10, "additional_headers": headers or None,
                   "ping_interval": None, "close_timeout": 2}
        known = inspect.signature(connect).parameters
        if "legacy" in known:
            options["legacy"] = True        # a connection held across calls, not a context manager's
        if "proxy" in known:
            options["proxy"] = None         # loopback: never through a proxy the environment names
        self.ws = connect(url, **options)
        self.received: list = []          # every frame, in order (parsed), pings included
        self.ended: Optional[Closed] = None

    def send_text(self, text: str) -> None:
        self.ws.send(text)

    def send(self, t: str, **fields) -> None:
        self.ws.send(HP.helper_frame(t, **fields))

    def _read(self, timeout: float) -> Optional[dict]:
        """One frame, or None when none came within ``timeout``; raises ``Closed``."""
        from websockets.exceptions import ConnectionClosed
        if self.ended is not None:
            raise self.ended
        try:
            text = self.ws.recv(timeout=timeout)
        except TimeoutError:
            return None
        except ConnectionClosed as exc:
            rcvd = exc.rcvd
            self.ended = Closed(rcvd.code if rcvd is not None else None, rcvd.reason if rcvd is not None else "")
            raise self.ended from None
        frame = HP.parse_server_frame(text)
        if isinstance(frame, HP.Ignored):
            frame = {"t": frame.t}
        self.received.append(frame)
        if frame["t"] == "ping" and self.answer_pings:
            self.ws.send(HP.helper_frame("pong"))
        return frame

    def recv(self, timeout: float = DEFAULT_WAIT, skip=("ping",)) -> dict:
        """The hub's next frame of a type not in ``skip``."""
        deadline = time.perf_counter() + timeout
        while True:
            left = deadline - time.perf_counter()
            if left <= 0:
                raise TimeoutError(f"no frame from the hub within {timeout} s")
            frame = self._read(left)
            if frame is not None and frame["t"] not in skip:
                return frame

    def handshake(self, fields: dict, ack: Optional[dict] = None) -> dict:
        """Send a hello, read the welcome, acknowledge its settings (as given, or what a correct helper would with
        the hello's slots and no CPU work): returns the welcome."""
        self.send("hello", **{k: v for k, v in fields.items() if k != "t"})
        welcome = self.recv()
        if welcome.get("t") != "welcome":
            raise AssertionError(f"expected a welcome, got {welcome}")
        site = welcome["settings"]
        if ack is None:
            llm = fields["llm"]
            ack = {"effective": {"llm": {"enabled": site["llm"]["enabled"] and llm["enabled"],
                                         "slots": min(site["llm"]["slots"], llm["slots"]) or llm["slots"]},
                                 "cpu": {**site["cpu"], "enabled": False}},
                   "capped": [], "limits": {"cpu": "soft", "memory": "soft"}}
        self.send("settings_ack", **ack)
        return welcome

    def close(self, code: int = 1000) -> None:
        try:
            self.ws.close(code=code)
        except Exception:
            pass

    def drop(self) -> None:
        """Lose the connection without a close frame: the TCP connection is cut under both ends, as a network that
        goes away cuts it (the hub sees its socket end with no close)."""
        sock = self.ws.socket
        try:
            sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            sock.close()
        except OSError:
            pass


def done_fields(request: dict, *, content: str = "CITAR OK", tool_calls: Optional[list] = None, gen_s: float = 0.01,
                usage: Optional[dict] = None, reasoning: Optional[str] = None, finish_reason: Any = "",
                message: Optional[dict] = None) -> dict:
    """A ``done``'s fields answering ``request`` (a raw message, as a helper returns it)."""
    calls = list(tool_calls or [])
    return {"id": request["id"],
            "message": message if message is not None else {"content": content, "reasoning": reasoning,
                                                            "tool_calls": calls},
            "finish_reason": finish_reason if finish_reason != "" else ("tool_calls" if calls else "stop"),
            "usage": usage if usage is not None else {"prompt_tokens": 12, "completion_tokens": 3,
                                                      "reasoning_tokens": None, "cached_tokens": None},
            "gen_s": gen_s, "ttft_s": None, "streamed": True}


class Responder:
    """A fake helper that serves: a thread reading its connection, answering each ``request`` with what ``answer``
    returns (a ``done``'s fields, or ``("error", fields)``; None holds it, ``release`` answers it later), and each
    ``cancel`` with ``cancelled`` after ``ack_delay`` seconds (None: never, a helper that does not acknowledge). It
    keeps every request (``requests``), cancel (``cancels``) and, per answered request, the seconds it spent
    (``spent``: from reading the request to sending its answer)."""

    def __init__(self, helper: FakeHelper, answer=None, *, ack_delay: Optional[float] = 0.0, slots: int = 1):
        self.helper = helper
        self.slots = slots
        self.settings: list = []
        self.answer = answer or (lambda frame: done_fields(frame))
        self.ack_delay = ack_delay
        self.requests: list = []
        self.read_at: list = []
        self.cancels: list = []
        self.spent: dict = {}
        self.held: dict = {}
        self.lock = threading.Lock()
        self.thread = threading.Thread(target=self._run, daemon=True, name="fake-helper")
        self.thread.start()

    def _run(self) -> None:
        while True:
            try:
                frame = self.helper.recv(timeout=3600)
            except (Closed, TimeoutError, OSError):
                return
            t = frame["t"]
            if t == "request":
                got = time.perf_counter()
                with self.lock:
                    self.requests.append(frame)
                    self.read_at.append(got)
                reply = self.answer(frame)
                if reply is None:
                    with self.lock:
                        self.held[frame["id"]] = frame
                    continue
                self._send(frame, reply)
                self.spent[frame["id"]] = time.perf_counter() - got
            elif t == "settings":
                # acknowledged as a correct helper does with no CPU lent: its hello's slots its ceiling
                self.settings.append(frame)
                try:
                    self.helper.send("settings_ack", effective={
                        "llm": {"enabled": frame["llm"]["enabled"], "slots": min(frame["llm"]["slots"], self.slots)},
                        "cpu": {**{k: frame["cpu"][k] for k in ("threads", "cpu_percent", "memory_mb", "work",
                                                                 "yield_to_llm")}, "enabled": False}},
                        capped=[], limits={"cpu": "soft", "memory": "soft"})
                except Exception:
                    pass
            elif t == "cancel":
                with self.lock:
                    self.cancels.append((frame["id"], time.perf_counter()))
                    # a helper that never acknowledges goes on generating it: it stays held, for ``release``
                    held = self.held.pop(frame["id"], None) if self.ack_delay is not None else None
                if held is not None:
                    threading.Timer(self.ack_delay, self._cancelled, args=(frame["id"],)).start()

    def _send(self, frame: dict, reply) -> None:
        try:
            if isinstance(reply, tuple):
                self.helper.send(reply[0], **reply[1])
            else:
                self.helper.send("done", **reply)
        except Exception:
            pass

    def _cancelled(self, rid: str) -> None:
        try:
            self.helper.send("cancelled", id=rid, gen_s=0.1, tokens_out=1)
        except Exception:
            pass

    def release(self, rid: Optional[str] = None) -> None:
        """Answer a held request (the oldest when ``rid`` is None), as ``answer`` would have."""
        with self.lock:
            if not self.held:
                return
            rid = rid or next(iter(self.held))
            frame = self.held.pop(rid, None)
        if frame is not None:
            self._send(frame, done_fields(frame))


# ---------------------------------------------------------------------------- the transcript player, hub side

class Unsupported(AssertionError):
    """A transcript step this package's hub side cannot play yet (a later package's word): refused, never skipped."""


#: The game's tool every submitted request offers (``completion-streamed-tool-call`` expects it by name).
END_TURN = {"name": "end_turn", "description": "End your turn.",
            "input_schema": {"type": "object", "properties": {"note": {"type": "string"}}}}
#: The hub's acts and state words this package plays (the others are later packages').
ACTS_PLAYED = ("submit_request", "force_request", "cancel_request", "drop_connection", "set_settings", "rescan",
               "reset_key", "revoke_token", "disable_machine", "second_connection", "queue_job", "force_offer",
               "lab_stop", "abort_run", "expire_run", "benchmark_start", "benchmark_end", "delete_experiment")
STATES_PLAYED = ("slot_free", "draining", "flags", "step", "requeued", "request", "llm_slots", "cpu_threads",
                 "cpu_percent", "memory_mb", "work", "cpu_enabled", "quiet", "models", "loaded", "key", "audit",
                 "token_lookups", "audit_rows", "rate_count", "last_report", "power_rows", "run", "job",
                 "result_lines", "backoff", "mem_mb_raised", "claim_failures", "events", "memory_per_run_mb")
#: The golden job a job transcript's experiment plays (its offers carry its spec).
GOLDEN_JOB = "duel-a"
#: The frames of the fake helper the hub hands to the machine's executor (which times them by the fake clock).
JOB_SEEN = W.JOB_FRAMES + ("status", "settings_ack")
#: A wait on the hub's fake clock longer than this, with the helper connected, moves the clock a keep-alive interval
#: at a time (each answered ping is the hub's sign that it saw the clock move), rather than a second at a time: six
#: hours in about ten seconds, not seven minutes.
LONG_WAIT_SECONDS = 600


class Outcome:
    """What a request the player started came to: ``done`` (its ``StepResult``) or an error, by request id."""

    def __init__(self):
        self.result = None
        self.error: Optional[BaseException] = None
        self.finished = threading.Event()

    @property
    def refusal(self) -> str:
        err = self.error
        while err is not None and not getattr(err, "refusal", ""):
            err = err.__cause__
        return getattr(err, "refusal", "") if err is not None else ""

    @property
    def retryable(self) -> bool:
        from citar.agents.providers.worker_provider import WorkerConnectionError
        return isinstance(self.error, WorkerConnectionError)


class HubPlayer:
    """Plays one transcript against the real hub (``server``, a ``HubServer``), from the helper's side."""

    def __init__(self, server: HubServer, transcript: F.Transcript):
        self.server = server
        self.t = transcript
        self.bindings: dict = {}
        self.helper: Optional[FakeHelper] = None
        self.others: list = []
        self.kept: list = []                # frames a non-quiet wait read, for the steps after it
        self.outcomes: dict = {}            # request id -> Outcome
        self.convs: list = []
        self.threads: list = []
        self.examples = F.canonical_examples()
        self.clock: Optional[FakeClock] = None
        self.sid = ""
        self.token_id = ""
        hub = transcript.setup.get("hub") or {}
        self.answer_pings = hub.get("answer_pings", True)
        self.setup_hub = hub
        self.golden = F.committed_golden()
        #: The job dispatcher a job transcript plays against, its experiment's name, the job's first ``mem_mb`` and
        #: the benchmark lease a ``benchmark_start`` takes.
        self.dispatcher = None
        self.exp_name = ""
        self.mem0: Optional[int] = None
        self.bench = None
        #: The machine's last report's time before the fake helper's ``hardware`` frame (``last_report``).
        self.report_before: Optional[str] = None

    # -------------------------------------------------------------- setting the scene
    def prepare(self) -> None:
        """The hub's starting state from ``setup.hub``: the machine, its settings, its key, the token, the address's
        refused tokens, the clock and the keep-alive's interval."""
        from citar import db, servers
        from citar.auth import tokens
        from citar.db.models import Server, User, WorkerToken
        from citar.pool import scheduler as S
        hub_setup = self.setup_hub
        for k in hub_setup:
            if k not in ("machine", "token", "rate", "clock", "ping_s", "answer_pings", "local_executor",
                         "verification", "jobs"):
                raise Unsupported(f"setup.hub.{k} is a later package's")
        if hub_setup.get("local_executor", False):
            raise Unsupported("setup.hub.local_executor true: no transcript plays it")
        machine = hub_setup.get("machine") or {}
        site = machine.get("settings") or {"llm": {"enabled": False, "slots": 1, "allow_swaps": False},
                                           "cpu": {"enabled": False, "threads": 1, "cpu_percent": 10,
                                                   "memory_mb": 512, "work": ["lab_game"], "yield_to_llm": True}}
        values = self.t.values
        with db.session() as s:
            owner = s.query(User).filter(User.handle == "transcripts").one_or_none()
            if owner is None:
                from citar.auth import accounts
                owner = accounts.create_user(s, handle="transcripts", email="transcripts@example.com",
                                             password="a long enough phrase", role="user", status="active",
                                             email_verified=True)
            row = Server(owner_id=owner.id, name=f"T {self.t.name}"[:120], kind="owned", provider="lmstudio",
                         reach="worker", max_concurrent=site["llm"]["slots"] or 1,
                         capabilities={"llm": {"enabled": site["llm"]["enabled"],
                                               "allow_swaps": site["llm"]["allow_swaps"]},
                                       "cpu": dict(site["cpu"])},
                         config={"models": [{"key": "model-a"}]})
            if machine.get("trust"):
                row.trust_tier = machine["trust"]
            key = machine.get("key")
            if key == "registered":
                row.public_key = values.get("key") or key_b64()
            elif key == "other":
                row.public_key = key_b64(99)
            s.add(row)
            s.flush()
            raw = tokens.generate()          # a token of its own: the bindings carry it to the hello
            token = WorkerToken(server_id=row.id, token_hash=tokens.hash_token(raw), prefix=tokens.prefix(raw),
                                label="transcript")
            if hub_setup.get("token") == "revoked":
                from datetime import datetime, timezone
                token.revoked_at = datetime.now(timezone.utc)
            s.add(token)
            s.flush()
            self.sid, self.token_id = row.id, token.id
        servers.invalidate()
        self.bindings.update(token=raw, machine=self.sid)
        hub = W.hub()
        hub.limiter.clear()
        for i in range(int((hub_setup.get("rate") or {}).get("failures") or 0)):
            hub.limiter.failed(ADDRESS, f"refused-{i}")
        if hub_setup.get("clock") == "fake":
            self.clock = FakeClock()
            hub.clock = self.clock
            S.install(S.Scheduler(clock=self.clock.monotonic, wall=self.clock.time))
        else:
            hub.clock = W.Clock()
            S.reset()
            S.get()
        hub.ping_s = int(hub_setup.get("ping_s") or 20)
        hub.poke()
        if self._plays_jobs():
            self._prepare_jobs(owner_id=owner.id)
        self.lookups0 = hub.lookups
        self.audit0 = self._audit_count()

    def _plays_jobs(self) -> bool:
        return any(k in self.setup_hub for k in ("local_executor", "jobs")) or any(
            st.action == "act" and F.act_word(st.body["do"]) in ("queue_job", "force_offer") for st in self.t.steps)

    def _prepare_jobs(self, owner_id: str) -> None:
        """A job dispatcher of the transcript's own (no local executor: the helper is the one executor), the lab's
        folders and the jobs' tables empty, the hub's own jobs build bound for the hello, and the runs the setup says
        the dispatcher already holds."""
        from citar import db, engine_api, lab
        from citar.db.models import CpuJob, CpuJobRun
        from citar.jobs import dispatcher as D, remote
        D.stop()
        lab.resume()
        lab._dirs()
        for folder in (lab.QUEUE, lab.RESULTS, lab.DONE, lab.REFERENCE):
            for f in folder.glob("*"):
                f.unlink()
        with db.session() as s:
            s.query(CpuJobRun).delete()
            s.query(CpuJob).delete()
        remote.registry().clear()
        remote.forget_owner_tiers()
        clock = self.clock.monotonic if self.clock is not None else time.monotonic
        self.dispatcher = D.start(D.Dispatcher(local=False, clock=clock))
        self.bindings["jobs_build"] = engine_api.jobs_build()
        self.owner_id = owner_id
        job = self.golden("jobs", (GOLDEN_JOB,))
        for held in self.setup_hub.get("jobs") or []:
            run = F.realize(held["run"], self.bindings, self.t.values)
            self._held_run(run, held["state"], job)

    def _held_run(self, run: str, state: str, job: dict) -> None:
        """A run the dispatcher holds from before (``setup.hub.jobs``): the golden job's, on this machine."""
        from datetime import datetime, timezone
        from citar import db
        from citar.db.models import CpuJob, CpuJobRun, new_id
        now = datetime.now(timezone.utc)
        with db.session() as s:
            job_id = new_id("cj_")
            done = state == "done"
            s.add(CpuJob(id=job_id, kind=job["kind"], ref=f"lab:held-{run}:0", item_kind="lab_experiment",
                         item_ref=f"held-{run}"[:120], owner_id=self.owner_id, spec=job["spec"],
                         spec_hash=job["spec_hash"], jobs_build=self.bindings["jobs_build"], priority=0, mem_mb=2,
                         state="accepted" if done else "running", accepted_run=run if done else None,
                         result_hash=job["result_hash"] if done else None, attempts=0, created_at=now))
            s.add(CpuJobRun(id=run, job_id=job_id, machine_id=self.sid, executor="remote", role="primary",
                            state=state, offered_at=now, started_at=now, ended_at=now if done else None,
                            result_hash=job["result_hash"] if done else None,
                            chain_head=job["chain_head"] if done else None, rounds=job["rounds"] if done else None))

    def close(self) -> None:
        """End what the transcript left: the connections, the requests' threads, its job dispatcher."""
        for h in [self.helper, *self.others]:
            if h is not None:
                h.close()
        if self.bench is not None:
            self.bench.release()
            self.bench = None
        if self.dispatcher is not None:
            from citar import lab
            from citar.jobs import dispatcher as D
            D.stop()
            lab.resume()
            self.dispatcher = None
        for conv in self.convs:
            try:
                conv.close()
            except Exception:
                pass
        for th in self.threads:
            th.join(timeout=10)
        W.hub().clock = W.Clock()
        W.hub().limiter.clear()

    # -------------------------------------------------------------- time
    def _advance(self, seconds: float) -> None:
        if self.clock is not None:
            self.clock.advance(seconds)
            if self.dispatcher is not None:
                self.dispatcher.poke()      # it times its runs and its backoff by the same clock

    def _wait_for(self, fn, within: Optional[float]):
        """``fn(timeout)`` until it returns something (not None), in real time, moving a fake clock forward in 1 s
        steps after ``REAL_GRACE`` when the step has a ``within_s``: what a helper would see within that many
        seconds of the hub's time."""
        limit = within if within is not None else DEFAULT_WAIT
        if self.clock is None or within is None:
            got = fn(limit if self.clock is None else DEFAULT_WAIT)
            if got is None:
                raise AssertionError(f"nothing came within {limit} s")
            return got
        got = fn(REAL_GRACE)
        moved = 0.0
        while got is None and moved < within:
            self._advance(1.0)
            moved += 1.0
            got = fn(0.05)
        if got is None:
            got = fn(REAL_GRACE)
        if got is None:
            raise AssertionError(f"nothing came within {within} s of the hub's clock")
        return got

    # -------------------------------------------------------------- steps
    def play(self) -> None:
        self.prepare()
        try:
            for i, step in enumerate(self.t.steps):
                self.index = i
                try:
                    self.step(step)
                except Unsupported:
                    raise
                except AssertionError as exc:
                    raise AssertionError(f"{self.t.name}, step {i} ({step.action}): {exc}") from None
        finally:
            self.close()

    def step(self, step: F.Step) -> None:
        kind, body = step.action, step.body
        if kind == "h2s":
            self.h2s(step)
        elif kind == "s2h":
            self.s2h(step)
        elif kind == "close":
            self.closed(body, step.within_s)
        elif kind == "drop":
            self.dropped(body, step.within_s)
        elif kind == "connect":
            self.helper = FakeHelper(self.server.url, answer_pings=self.answer_pings)
        elif kind == "wait":
            self.wait(body)
        elif kind in ("endpoint", "host"):
            return                          # the helper's side plays these
        elif kind == "act":
            if body["side"] == "hub":
                self.act(body["do"])
        else:
            if body["side"] == "hub":
                self.state(body["expect"])

    def _conn(self) -> FakeHelper:
        if self.helper is None:
            self.helper = FakeHelper(self.server.url, answer_pings=self.answer_pings)
        return self.helper

    def h2s(self, step: F.Step) -> None:
        pattern = step.body
        example = self.examples.get((HP.HELPER, pattern["t"]))
        frame = F.realize(pattern, self.bindings, self.t.values, self.golden, example=example)
        if pattern["t"] == "hardware":
            # what the hello's report left, once it is kept (off the hub's loop), for ``last_report``
            until(self._hardware_at, 2.0)
            self.report_before = self._hardware_at()
        if step.lost:
            return                          # the connection took it: the hub never gets it
        text = json.dumps(frame, separators=(",", ":"), ensure_ascii=False)
        if not step.invalid:
            HP.parse_helper_frame(text)     # a realized frame passes its checks (the fixtures' rule)
        executor = self._executor() if pattern["t"] in JOB_SEEN else None
        seen = executor.received if executor is not None else 0
        self._conn().send_text(text)
        if executor is not None and self.clock is not None:
            # the dispatcher times it by the fake clock as it arrives: the hub hands it over before the clock moves
            until(lambda: executor.received > seen, 2.0, 0.002)

    def _executor(self):
        """The machine's remote executor, once its helper said it plays jobs."""
        if self.dispatcher is None:
            return None
        from citar.jobs import remote
        return remote.registry().find(self.sid)

    def _next_frame(self, wanted: str, timeout: float) -> Optional[dict]:
        """The hub's next frame for a step expecting ``wanted``: a kept one first; background frames of another type
        skipped (PROTOCOL.md, "Background frames")."""
        while self.kept:
            frame = self.kept.pop(0)
            if frame["t"] == wanted or frame["t"] not in F.BACKGROUND:
                return frame
        conn = self._conn()
        deadline = time.perf_counter() + timeout
        while True:
            left = deadline - time.perf_counter()
            if left <= 0:
                return None
            frame = conn._read(left)
            if frame is None:
                return None
            if frame["t"] == wanted or frame["t"] not in F.BACKGROUND:
                return frame

    def s2h(self, step: F.Step) -> None:
        pattern = step.body
        wanted = pattern["t"]
        frame = self._wait_for(lambda timeout: self._next_frame(wanted, timeout), step.within_s)
        try:
            F.matches(pattern, frame, self.bindings, self.golden)
        except F.Mismatch as exc:
            raise AssertionError(f"the hub sent {json.dumps(frame)[:600]}, which does not match: {exc}") from None
        if wanted == "job_offer" and self.mem0 is None:
            self.mem0 = frame.get("mem_mb")

    def closed(self, body: dict, within: Optional[float]) -> None:
        conn = self._conn()
        if body["by"] == "helper":
            conn.close(body["code"])
            return

        def ended(timeout):
            try:
                frame = self._next_frame("", timeout)
            except Closed as c:
                return c
            if frame is not None:
                raise AssertionError(f"expected a close, got {frame}")
            return None
        c = self._wait_for(ended, within)
        if c.code != body["code"]:
            raise AssertionError(f"closed with {c.code} ({c.reason!r}), not {body['code']}")
        if "reason" in body and c.reason != body["reason"]:
            raise AssertionError(f"closed with the reason {c.reason!r}, not {body['reason']!r}")

    def dropped(self, body: dict, within: Optional[float]) -> None:
        conn = self._conn()
        if body["by"] == "helper":
            conn.drop()
            return
        # the hub's side was dropped by the act before (``drop_connection``): the socket is gone, with no close
        if conn.ended is None:
            try:
                conn._read(0.5)
            except Closed:
                pass
        if conn.ended is not None and conn.ended.code not in (None, 1006):
            raise AssertionError(f"the connection was closed with {conn.ended.code}, not dropped")
        self.helper = None

    def wait(self, body: dict) -> None:
        seconds, quiet = float(body["s"]), bool(body.get("quiet", False))
        steps = int(seconds) if self.clock is not None else 0
        frames: list = []
        end_real = time.perf_counter() + (0 if self.clock is not None else seconds)
        conn = self.helper

        def drain(timeout):
            if conn is None or conn.ended is not None:
                time.sleep(timeout)
                return
            try:
                frame = conn._read(timeout)
            except Closed:
                return
            if frame is not None and frame["t"] not in F.BACKGROUND:
                frames.append(frame)
        def drain_to_ping(timeout):
            """Frames until the hub's ping (answered by ``_read``) or ``timeout``."""
            deadline = time.perf_counter() + timeout
            while conn.ended is None and time.perf_counter() < deadline:
                try:
                    frame = conn._read(max(0.0, deadline - time.perf_counter()))
                except Closed:
                    return
                if frame is None or frame["t"] == "ping":
                    return
                if frame["t"] not in F.BACKGROUND:
                    frames.append(frame)
        connected = conn is not None and conn.ended is None
        if self.clock is not None and connected and seconds > LONG_WAIT_SECONDS:
            step_s = float(max(1, W.hub().ping_s))
            moved = 0.0
            while moved + step_s <= seconds and conn.ended is None:
                self._advance(step_s)
                moved += step_s
                drain_to_ping(0.5)
            if seconds - moved > 0:
                self._advance(seconds - moved)
            drain(0.05)
        elif self.clock is not None:
            for _ in range(steps):
                self._advance(1.0)
                if conn is not None and conn.ended is None:
                    drain(0.02)             # the hub answers in real time: a moment for each second of its clock
            rest = seconds - steps
            if rest > 0:
                self._advance(rest)
            drain(0.05)
        else:
            while time.perf_counter() < end_real:
                drain(min(0.1, max(0.0, end_real - time.perf_counter())))
        if quiet and frames:
            raise AssertionError(f"the hub sent {frames[0]} during a quiet wait")
        self.kept.extend(frames)

    # -------------------------------------------------------------- acts
    def _next_request(self) -> tuple:
        """The model and the timeout of the next ``request`` the transcript shows the hub sending, where it writes
        them (``model-a`` and 60 s otherwise)."""
        for st in self.t.steps[self.index + 1:]:
            if st.action == "s2h" and st.body.get("t") == "request":
                model, timeout = st.body.get("model"), st.body.get("timeout_s")
                return (model if isinstance(model, str) and F.parse_matcher(model) is None else "model-a",
                        float(timeout) if isinstance(timeout, (int, float)) else 60.0)
        return "model-a", 60.0

    def act(self, do) -> None:
        word = F.act_word(do)
        if word not in ACTS_PLAYED:
            raise Unsupported(f"the hub's act {word!r} is a later package's")
        getattr(self, f"act_{word}")(do[word] if isinstance(do, dict) else None)

    def act_submit_request(self, _arg) -> None:
        from citar.agents.providers import make_conversation
        from citar.pool import scheduler as S
        model, timeout = self._next_request()
        lease = S.get().lease(self.sid, model, kind="game", label=f"transcript {self.t.name}")
        conv = make_conversation({"provider": "worker", "server_id": self.sid, "model": model, "timeout": timeout},
                                 "You are a test.", [END_TURN], binding=lease)
        conv.add_user_text("Reply with exactly: CITAR OK")
        self.convs.append(conv)
        out = Outcome()

        def go():
            try:
                out.result = conv.step()
            except BaseException as exc:
                out.error = exc
            finally:
                out.finished.set()
        th = threading.Thread(target=go, daemon=True, name="transcript-submit")
        th.start()
        self.threads.append(th)
        until(lambda: getattr(conv.inner, "last_request_id", None), 10)
        self.outcomes[conv.inner.last_request_id] = out
        self.last_conv = conv

    def act_force_request(self, _arg) -> None:
        import secrets
        rid = secrets.token_hex(8)
        model, timeout = self._next_request()
        out = Outcome()

        def go():
            from citar.agents.providers.worker_provider import WorkerConnectionError
            try:
                out.result = W.hub().submit(self.sid, rid, model,
                                            [{"role": "system", "content": "You are a test."},
                                             {"role": "user", "content": "Reply with exactly: CITAR OK"}],
                                            None, {}, timeout, check_slots=False)
            except W.WorkerError as exc:
                out.error = WorkerConnectionError(str(exc), exc.refusal) if exc.retryable else exc
            except BaseException as exc:
                out.error = exc
            finally:
                out.finished.set()
        th = threading.Thread(target=go, daemon=True, name="transcript-force")
        th.start()
        self.threads.append(th)
        self.outcomes[rid] = out

    def _next_offer(self) -> dict:
        """The next ``job_offer`` step's frame, realized with what is bound so far (not binding its job and run, which
        the hub's own frame binds)."""
        for st in self.t.steps[self.index + 1:]:
            if st.action == "s2h" and st.body.get("t") == "job_offer":
                example = self.examples.get((HP.SERVER, "job_offer"))
                return F.realize(st.body, dict(self.bindings), self.t.values, self.golden, example=example)
        raise AssertionError("no job_offer step follows the act")

    def act_queue_job(self, _arg) -> None:
        """The dispatcher gets a job: an experiment of one game, the golden job's (written whole, as ``lab.submit``
        writes one, its item the machine's owner's), offered as the dispatcher offers."""
        from datetime import datetime
        from citar import db, items, lab
        from citar.db.models import User
        spec = self.golden("jobs", (GOLDEN_JOB, "spec"))
        name = spec["exp"]
        exp = {"name": name, "kind": "lab", "priority": 0, "games": 1, "seed": spec["seed"], "size": spec["size"],
               "maps": [spec["map_type"]], "speed": spec["speed"], "turns": spec["turns"],
               "difficulty": spec["difficulty"], "barbarians": spec["barbarians"],
               "barbarian_difficulty": spec["barbarian_difficulty"], "nation": spec["nation"],
               "city_states": spec["city_states"], "ai_base_values": spec["ai_base_values"], "rotate": False,
               "seats": [dict(seat) for seat in spec["seats"]],
               "submitted": datetime.now().isoformat(timespec="seconds")}
        lab._dirs()
        path = lab.QUEUE / f"{name}.json"
        with db.session() as s:
            items.register(s, "lab_experiment", name, s.get(User, self.owner_id), name, path=path)
        path.write_text(json.dumps(exp), encoding="utf-8")
        self.exp_name = name
        self.dispatcher.poke()

    def act_force_offer(self, _arg) -> None:
        offer = self._next_offer()
        self.dispatcher.force_offer(self.sid, kind=offer["kind"], spec=offer["spec"], spec_hash=offer["spec_hash"],
                                    jobs_build=offer["jobs_build"], mem_mb=int(offer["mem_mb"]),
                                    owner=self.owner_id)

    def act_lab_stop(self, _arg) -> None:
        from citar import lab
        lab.pause(by="a transcript")

    def act_abort_run(self, _arg) -> None:
        if not self.dispatcher.abort_run(self.bindings["run"], "the transcript's abort"):
            raise AssertionError(f"run {self.bindings['run']} is not in flight to abort")
        self.dispatcher.poke()

    def act_delete_experiment(self, _arg) -> None:
        """The experiment ``queue_job`` queued is deleted from the lab's queue, as the Lab page's Delete does: the
        dispatcher cancels its jobs and aborts its runs at its next look."""
        if not self.exp_name:
            raise AssertionError("no experiment was queued to delete")
        from citar import lab
        (lab.QUEUE / f"{self.exp_name}.json").unlink()
        self.dispatcher.poke()

    def act_expire_run(self, _arg) -> None:
        if not self.dispatcher.expire(self.bindings["run"]):
            raise AssertionError(f"run {self.bindings['run']} is not in flight to expire")

    def act_benchmark_start(self, _arg) -> None:
        from citar.pool import scheduler as S
        self.bench = S.get().lease(self.sid, "dry-run", kind="benchmark", label=f"transcript {self.t.name}")

    def act_benchmark_end(self, _arg) -> None:
        """The lease ends; done once the dispatcher has taken it in (its resume sent, or found no connection to go to),
        so the steps after it see what follows the lease's end, not a resume still on its way."""
        if self.bench is not None:
            self.bench.release()
            self.bench = None
        if self.dispatcher is not None:
            self.dispatcher.poke()
            if not until(lambda: self.sid not in self.dispatcher._paused_exec, 5.0):
                raise AssertionError("the dispatcher did not take the benchmark's end in")

    def act_cancel_request(self, _arg) -> None:
        self.last_conv.close()

    def act_drop_connection(self, _arg) -> None:
        self._conn().drop()

    def act_set_settings(self, site: dict) -> None:
        from citar import db
        from citar import machine_settings as M
        from citar.db.models import Server
        from citar.server.machines_api import settings_changed
        with db.session() as s:
            row = s.get(Server, self.sid)
            M.update(row, {"slots": max(1, site["llm"]["slots"]),
                           "capabilities": {"llm": {"enabled": site["llm"]["enabled"],
                                                    "allow_swaps": site["llm"]["allow_swaps"]},
                                            "cpu": dict(site["cpu"])}})
        settings_changed(self.sid)

    def act_rescan(self, _arg) -> None:
        if not W.hub().rescan(self.sid):
            raise AssertionError("no helper to ask for a rescan")

    def act_reset_key(self, _arg) -> None:
        from citar import db
        from citar.db.models import Server
        from citar.server.machines_api import reset_key
        with db.session() as s:
            reset_key(s, s.get(Server, self.sid), None)

    def act_revoke_token(self, _arg) -> None:
        from datetime import datetime, timezone
        from citar import db
        from citar.db.models import WorkerToken
        with db.session() as s:
            s.get(WorkerToken, self.token_id).revoked_at = datetime.now(timezone.utc)
        W.hub().drop(self.sid, "its worker token was revoked", token_id=self.token_id)

    def act_disable_machine(self, _arg) -> None:
        from citar import db, servers
        from citar.db.models import Server
        with db.session() as s:
            s.get(Server, self.sid).enabled = False
        servers.invalidate()
        W.hub().drop(self.sid, "its machine was disabled")

    def act_second_connection(self, _arg) -> None:
        first = next((f for f in self._sent_hellos()), None)
        other = FakeHelper(self.server.url, answer_pings=True)
        self.others.append(other)
        fields = dict(first) if first else hello(token=self.bindings["token"], key=self.t.values.get("key"))
        other.handshake(fields)

    def _sent_hellos(self):
        for st in self.t.steps:
            if st.action == "h2s" and st.body.get("t") == "hello" and not st.invalid:
                example = self.examples.get((HP.HELPER, "hello"))
                yield F.realize(st.body, dict(self.bindings), self.t.values, example=example)

    # -------------------------------------------------------------- state
    def _audit_count(self, action: Optional[str] = None) -> int:
        from citar import db
        from citar.db.models import AuditLog
        with db.session() as s:
            q = s.query(AuditLog)
            if action:
                q = q.filter(AuditLog.action == action)
            return q.count()

    def _machine(self):
        from citar.pool import scheduler as S
        return S.get().machine(self.sid)

    def _row(self):
        from citar import db
        from citar.db.models import Server
        with db.session() as s:
            row = s.get(Server, self.sid)
            return {"flags": dict(row.flags or {}), "public_key": row.public_key}

    def _hardware_at(self) -> Optional[str]:
        """When the machine's last hardware report was kept (``hardware_detected.hardware_at``), or None."""
        from citar import db
        from citar.db.models import Server
        with db.session() as s:
            row = s.get(Server, self.sid)
            detected = row.hardware_detected if row is not None and isinstance(row.hardware_detected, dict) else {}
            return detected.get("hardware_at")

    def _power_rows(self) -> int:
        """The power rows written for the machine (``usage``'s ``power-<machine>-YYYY-MM.jsonl``)."""
        from citar import usage
        rows = 0
        for path in usage.USAGE_DIR.glob(f"power-{self.sid}-*.jsonl"):
            for line in path.read_text(encoding="utf-8").splitlines():
                if line.strip() and json.loads(line).get("srv") == self.sid:
                    rows += 1
        return rows

    def _request_ids(self) -> set:
        return {v for k, v in self.bindings.items() if k.startswith("req")}

    def state(self, expect: dict) -> None:
        for word in expect:
            if word not in STATES_PLAYED:
                raise Unsupported(f"the hub's state {word!r} is a later package's")
        failures: list = []

        def check() -> bool:
            failures.clear()
            for word, want in expect.items():
                try:
                    self._check(word, want)
                except AssertionError as exc:
                    failures.append(str(exc))
            return not failures
        if not until(check, 5.0, 0.02):
            raise AssertionError("; ".join(failures))

    def _outcome(self, rid) -> Outcome:
        out = self.outcomes.get(rid)
        if out is None:
            raise AssertionError(f"no request {rid} was started by the transcript")
        if not out.finished.is_set():
            raise AssertionError(f"request {rid} has not ended")
        return out

    def _check(self, word: str, want) -> None:
        def bound(v):
            """A transcript's ``<id:NAME>`` as its binding, anything else as it is."""
            return self.bindings.get(v[4:-1]) if isinstance(v, str) and v.startswith("<id:") else v
        if word == "slot_free":
            m = self._machine()
            held = [s for s in m.slots if s.request_id in self._request_ids()]
            if bool(not held) != bool(want):
                raise AssertionError(f"slot_free is {not held}, not {want}")
        elif word == "draining":
            m = self._machine()
            n = sum(1 for s in m.slots if s.draining)
            if n != want:
                raise AssertionError(f"{n} slots drain, not {want}")
        elif word == "flags":
            flags = self._row()["flags"]
            missing = [f for f in want if f not in flags]
            if missing:
                raise AssertionError(f"the machine's flags {sorted(flags)} lack {missing}")
        elif word == "step":
            outs = [o for o in self.outcomes.values() if o.finished.is_set() and o.result is not None
                    and hasattr(o.result, "tool_calls")]
            if not outs:
                raise AssertionError("no parsed step yet")
            names = [c.name for c in outs[-1].result.tool_calls]
            if names != want["tool_calls"]:
                raise AssertionError(f"the step's calls are {names}, not {want['tool_calls']}")
        elif word == "requeued":
            out = self._outcome(bound(want))
            if not out.retryable:
                raise AssertionError(f"request {bound(want)} ended {out.error!r}, which is not queued again")
        elif word == "request":
            out = self._outcome(bound(want["id"]))
            if out.retryable or out.error is None or out.refusal != want["failed"]:
                raise AssertionError(f"request {bound(want['id'])} ended {out.error!r} ({out.refusal}), not failed "
                                     f"{want['failed']}")
        elif word in ("llm_slots", "cpu_threads", "cpu_percent", "memory_mb", "work", "cpu_enabled"):
            c = W.hub().get(self.sid)
            if c is None:
                raise AssertionError("no helper connected")
            eff = c.effective()
            have = {"llm_slots": eff["llm"]["slots"], "cpu_threads": eff["cpu"]["threads"],
                    "cpu_percent": eff["cpu"]["cpu_percent"], "memory_mb": eff["cpu"]["memory_mb"],
                    "work": eff["cpu"]["work"], "cpu_enabled": eff["cpu"]["enabled"]}[word]
            if have != want:
                raise AssertionError(f"{word} is {have}, not {want}")
            if word == "llm_slots":
                from citar.pool import scheduler as S
                m = S.get().machine(self.sid)
                if m.settings.helper_slots != want or m.capacity() != min(m.settings.slots, want):
                    raise AssertionError(f"the scheduler counts {m.settings.helper_slots} slots of the helper and a "
                                         f"capacity of {m.capacity()}, not {want} and {min(m.settings.slots, want)}")
        elif word == "quiet":
            if bool(self._machine()._quiet()) != bool(want):
                raise AssertionError(f"the scheduler's quiet hours are {self._machine()._quiet()}, not {want}")
        elif word in ("models", "loaded"):
            c = W.hub().get(self.sid)
            models = c.models if c is not None else []
            have = [m["key"] for m in models if word == "models" or m.get("loaded")]
            if sorted(have) != sorted(want):
                raise AssertionError(f"{word} are {have}, not {want}")
        elif word == "key":
            have = self._row()["public_key"]
            if have != bound(want):
                raise AssertionError(f"the machine's key is {have}, not {bound(want)}")
        elif word == "audit":
            for action in want:
                if not self._audit_count(action):
                    raise AssertionError(f"no audit row {action}")
        elif word == "token_lookups":
            n = W.hub().lookups - self.lookups0
            if n != want:
                raise AssertionError(f"{n} token lookups, not {want}")
        elif word == "audit_rows":
            n = self._audit_count() - self.audit0
            if n != want:
                raise AssertionError(f"{n} audit rows, not {want}")
        elif word == "rate_count":
            n = W.hub().limiter.count(ADDRESS)
            if n != want:
                raise AssertionError(f"the address's limit counts {n}, not {want}")
        elif word == "last_report":
            if want != "updated":
                raise AssertionError(f"last_report {want!r} is not a word this player knows")
            from datetime import datetime
            now = self._hardware_at()
            before = self.report_before
            if now is None or (before is not None and datetime.fromisoformat(now) <= datetime.fromisoformat(before)):
                raise AssertionError(f"the machine's last report was kept at {now}, not after {before}")
        elif word == "power_rows":
            n = self._power_rows()
            if n != want:
                raise AssertionError(f"{n} power rows for the machine, not {want}")
        elif word in ("run", "job", "result_lines", "mem_mb_raised", "events"):
            self._job_state(word, want, bound)
        elif word == "backoff":
            have = self.dispatcher.backoff(self.sid) if self.dispatcher is not None else []
            if want not in have:
                raise AssertionError(f"the dispatcher backs the machine off for {have}, not {want}")
        elif word == "memory_per_run_mb":
            c = W.hub().get(self.sid)
            have = c.memory_per_run_mb if c is not None else None
            executor = self._executor()
            if have != want or (executor is not None and executor.memory_per_run_mb != want):
                raise AssertionError(f"the machine's memory a run is {have} (its executor's "
                                     f"{executor.memory_per_run_mb if executor else None}), not {want}")
        elif word == "claim_failures":
            from citar import db
            from citar.db.models import Server
            with db.session() as s:
                n = int((s.get(Server, self.sid).trust or {}).get("claim_failures") or 0)
            if n != want:
                raise AssertionError(f"{n} claim failures on the machine, not {want}")

    def _job_state(self, word: str, want, bound) -> None:
        """The job dispatcher's state words: a run's state, the job's, its results' lines, whether its ``mem_mb``
        rose, and the ledger's events of a run."""
        from citar import db, lab
        from citar.db.models import ComputeEvent, CpuJob, CpuJobRun
        with db.session() as s:
            if word == "run":
                rid = bound(want["id"])
                row = s.get(CpuJobRun, rid)
                have = row.state if row is not None else None
                if have != want["state"]:
                    raise AssertionError(f"run {rid} is {have}, not {want['state']}")
                return
            if word == "events":
                from citar import ledger
                ledger.get().flush(5.0)
                n = s.query(ComputeEvent).filter(ComputeEvent.request_id == self.bindings.get("run")).count()
                if n != want:
                    raise AssertionError(f"{n} events for run {self.bindings.get('run')}, not {want}")
                return
            job = s.get(CpuJob, self.bindings.get("job")) if self.bindings.get("job") else None
            if job is None:
                raise AssertionError("no job is bound yet")
            if word == "job":
                if job.state != want:
                    raise AssertionError(f"job {job.id} is {job.state}, not {want}")
            elif word == "mem_mb_raised":
                raised = self.mem0 is not None and int(job.mem_mb or 0) > int(self.mem0)
                if raised != bool(want):
                    raise AssertionError(f"job {job.id}'s mem_mb is {job.mem_mb} (offered {self.mem0}): raised is "
                                         f"{raised}, not {want}")
            elif word == "result_lines":
                lines = [ln for ln in lab.load_results(job.item_ref) if ln.get("i") == 0]
                if len(lines) != want:
                    raise AssertionError(f"{len(lines)} lines in {job.item_ref}'s results, not {want}")


def hub_transcripts(package: str = "4-05") -> list:
    """The transcripts the hub's side plays from ``package`` (``plays.hub``)."""
    return [t for t in F.transcripts() if t.plays.get("hub") == package]


def played(t: F.Transcript) -> Any:
    """Whether every hub act and state word of ``t`` is one this package plays (else the word that is not)."""
    for st in t.steps:
        if st.action == "act" and st.body["side"] == "hub" and F.act_word(st.body["do"]) not in ACTS_PLAYED:
            return F.act_word(st.body["do"])
        if st.action == "state" and st.body["side"] == "hub":
            for w in st.body["expect"]:
                if w not in STATES_PLAYED:
                    return w
    return True

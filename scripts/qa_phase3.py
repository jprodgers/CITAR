"""Phase 3's QA script: a dev server of its own, accounts, and each feature's checks over HTTP and the game socket,
with the evidence written down (design/phase3-features.md P3.9.4).

    python scripts/qa_phase3.py <part>|all [--port N] [--dir D] [--keep]

The harness:

- **refuses** (exit 2, touching nothing) the dev server's own port (8799), a port something already listens on, a
  directory that holds anything without the harness's marker (``.qa-phase3``), and directories that are never a
  harness's: ``C:/dev/citar-play`` (the owner's dev server), CITAR's own per-user data directory and the source
  checkout. A directory it may use is new, empty or one an earlier run left (marked); it empties it first. Every
  refusal comes before that: once the directory is the harness's, anything that goes wrong (it cannot be emptied,
  the server does not start, an account cannot be made, a part raises) is a failed check with its evidence
  written, exit 1;
- starts ``scripts/dev_server.py --server --registration invite`` as a child on that directory and a free port (or
  ``--port``), everything of the instance under the directory (``dev_server.instance_env``);
- makes an administrator, two members and a moderator (a third member for ``all``) with ``admin_cli create-user``,
  passwords on standard input, and signs them in through ``/api/auth/login`` (a cookie and the CSRF token each);
- writes a registry file of CITAR before 0.1.6 into the instance's ``config/`` before the server starts, which the
  first administrator's creation imports (P3.2.8);
- serves fake OpenAI-compatible endpoints on 127.0.0.1 that record every connection they accept and every request
  they serve, and answer from a scripted policy that plays (P3.9.4: tool calls from the request's tools, a delay per
  completion; streamed as server-sent events when asked, recording when the client went, design/phase4-helper.md
  P4.7.6): one is the ``accounts`` part's trap (a member's seat or machine that names it must make the server dial
  nothing), another serves the real CITAR helpers a part starts for a helper machine: the Rust binary
  (``citar-helper``, ``--helper-bin`` or the one ``cargo xtask develop`` built into ``CITAR_EXT_DIR``), each in a home
  of its own in the harness's directory (``--home <dir>/helper-<n>``);
- runs the part's checks, writes ``<dir>/qa-<part>.json`` (each check, its result, its numbers; the accounts and
  their throwaway password, for the browser checklist), stops the server unless ``--keep``, and exits 1 when a
  check failed, 0 when every one passed.

Parts: ``accounts`` (3-03: sharing of games and reports, ``/r/{slug}``, a moderator's unpublish, members' model
blocks, a member's report gathering only what they may see; 3-04: profiles, avatars, the directory, the admin users
routes, ``admin_cli``'s commands, probation and a suspension; 3-05: the registry's import, members' machines, a
helper machine with a real helper, the machine routes' gates and dropped helpers; 3-06: the one Servers page, what it
lists to a member, another member and an administrator, the reads and saves of its dialogs, sharing a machine from it;
with ``--keep`` its helper machine's helper stays connected for the browser checklist), ``queue`` (3-07: two lobby
games sharing a helper machine's one slot through the prompt scheduler, never two requests at once and no busy
refusal; the Servers page's test given up mid-completion, its generation stopped by the helper and its slot freed by
the helper's acknowledgement, so the next request goes at once (protocol 2, design/phase4-helper.md P4.5.3); 3-08: a
lobby game's lease made when it is registered and again when the server, restarted, brings it back, a benchmark job
waiting for it, and a one-job benchmark started beside a running test, which it cancels, ``drain_s`` in its result;
3-09: two members' report analyses and probe cases waiting as
pooled prompts behind a lobby game's seat mid-turn, their Queue pages, and the order they are served in, none while the
seat is mid-turn; ``--hold-line`` keeps them waiting for the browser checklist), ``ledger`` (3-10: bob's helper
machine serving alice's lobby game and her report's analysis, his given and her used read through ``/api/ledger/me``
against the compute ledger's events in the instance's database, her own machine's work on neither side, the ratio
following its visibility; with ``--keep`` the server stays up for the browser checklist's B1), ``chat`` (3-11, the
server half: a person, two dry-run model seats on a one-slot machine and a bot chatting in a short game played to its
end, both chat timeouts 20 s: a model's answer, one queued and then declined for by the chat clock, a person's chat
closed as expired, the models' trades on their turns waiting for her; every ``negotiation_status`` her socket heard,
none on a spectator's), ``hybrid`` (3-14, the server half: a hybrid seat on another member's dry-run machine shared for
pooled prompts, with a person and two bots, 30 rounds, ``prompt_every`` 3: its diplomat's phases, chats answered by its
model and by its bot, the chat clock's fallback, no lease held; 3-15: the seat route making a seat hybrid and back, and
a scenario saved with a hybrid seat launched with it; 3-16: a hybrid benchmark suite on another member's machine (two
seeds), each job paired with a bot-only control game, a second model's run reusing those controls, the diplomacy
leaderboard as the run's owner, another member, an administrator and a visitor see it, and a hybrid probe run recording
which side answered each case; with ``--keep`` the server stays up for the browser checklist's D1 and D2), ``minimap``
(3-17: a person's seat on a standard map that wraps east-west, moved on through ``POST /api/games/{gid}/debug/ops``: a
capital, scouts at the edges of what it explored, each era's techs, the whole map revealed; its view's minimap block
moving from early to renaissance to information, its home box only growing, over the seam, and at every step the
box of the land the seat explored joined to its home through land it explored; the route's gate; with ``--keep`` the
server stays up for the browser checklist's F1), ``machines`` (3-18: a helper machine whose real helper reconnects
with other hardware, models and power figures from a file, ``--hardware``: the last report kept and applied by mode,
a manual hardware entry surviving the reconnect, models merged or only marked, Apply by section with the hardware
class following it, a scan of figures no JSON answer carries kept clean with every page still reading and "Apply all"
taking what the report has, an administrator's pin and the trust tier, the routes' gates (a moderator's too), slots
and model swaps taking effect at once, LLM off; with ``--keep`` its helper stays connected for the browser
checklist's G1), and later ``real``
(3-19); ``all`` runs every part written so far.
It needs the extension and the helper (``cargo xtask develop`` builds both).
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import secrets
import select
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time
import traceback
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Callable, Optional
from urllib.parse import parse_qs, urlparse

import httpx

ROOT = Path(__file__).resolve().parent.parent
#: The file the harness leaves in its directory: a later run empties a directory that has it, and never one that holds
#: anything without it.
MARKER = ".qa-phase3"
#: ``scripts/dev_server.py``'s default port, where the owner's own dev server may be running or start at any time.
DEV_SERVER_PORT = 8799
#: The owner's dev server's directory: never a harness's, whatever is in it.
OWNER_PLAY_DIR = Path("C:/dev/citar-play")
#: Parts a later package writes (P3.9.4): asked for now, the harness says so and refuses. None is left.
LATER: dict = {}


def _dev_server():
    """``scripts/dev_server.py`` as a module, for its ``instance_env``."""
    spec = importlib.util.spec_from_file_location("qa_dev_server", ROOT / "scripts" / "dev_server.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Refused(Exception):
    """The harness will not run here: a port or a directory that is not its own. Exit 2, nothing touched: raised
    only before ``claim``."""


class HarnessError(Exception):
    """The harness could not do its own part once the directory was its own (``claim``): the directory not
    emptied, the server not started, an account not made. A failed check with the evidence written, exit 1."""


# ---------------------------------------------------------------------------- refusals

def _norm(path) -> str:
    """A path as compared: absolute, the case Windows ignores folded, forward slashes, no trailing slash."""
    return os.path.normcase(os.path.abspath(str(path))).replace("\\", "/").rstrip("/")


def _inside(path, root) -> bool:
    """Whether ``path`` is ``root`` or anything under it."""
    a, b = _norm(path), _norm(root)
    return a == b or a.startswith(b + "/")


def default_data_dir() -> Path:
    """CITAR's own per-user data directory, as an installed CITAR with no overrides uses it (``citar.paths``)."""
    if sys.platform == "win32":
        return Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local") / "CITAR"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "CITAR"
    return Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share") / "citar"


def _checkout(path: Path) -> Optional[Path]:
    """The source checkout (a git working tree, a worktree's included) that ``path`` is in, or None."""
    here = Path(os.path.abspath(str(path)))
    for folder in (here, *here.parents):
        if (folder / ".git").exists():
            return folder
    return None


def check_dir(state: Path) -> None:
    """Raise Refused, touching nothing, unless ``state`` may be the harness's: new, empty or marked, and none of
    the directories that are never a harness's (the owner's dev server's, CITAR's own data directory, a source
    checkout: the database and the saves must not land in a synced folder or beside the code)."""
    for root, what in ((OWNER_PLAY_DIR, "the owner's dev server's directory"),
                       (default_data_dir(), "CITAR's own data directory"), (ROOT, "the source checkout")):
        if _inside(state, root):
            raise Refused(f"{state} is {what} or inside it ({root}): name a directory of the harness's own")
    checkout = _checkout(state)
    if checkout is not None:
        raise Refused(f"{state} is inside a source checkout ({checkout}): name a directory of the harness's own")
    if state.exists():
        if not state.is_dir():
            raise Refused(f"{state} is a file, not a directory")
        if any(state.iterdir()) and not (state / MARKER).is_file():
            raise Refused(f"{state} holds files no harness left (it has no {MARKER}): refusing to empty it; name a "
                          "new or empty directory")


def check_port(port: int) -> None:
    """Raise Refused for a port the harness must not take: the dev server's own, or one something listens on."""
    if port == DEV_SERVER_PORT:
        raise Refused(f"port {port} is the dev server's own, where someone may be playing: name another")
    with socket.socket() as s:
        s.settimeout(1.0)
        if s.connect_ex(("127.0.0.1", port)) == 0:
            raise Refused(f"something already listens on port {port}: name another")


def claim(state: Path) -> None:
    """Empty ``state`` (``check_dir`` allowed it) and mark it as the harness's."""
    if state.exists():
        try:
            shutil.rmtree(state)
        except OSError as e:
            raise HarnessError(f"could not empty {state} ({e}): a server an earlier run kept (--keep) may still be "
                               "running on it") from None
    state.mkdir(parents=True)
    (state / MARKER).write_text("scripts/qa_phase3.py empties this directory at each run.\n", encoding="utf-8")


def free_port() -> int:
    """A port nothing listens on now."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# ---------------------------------------------------------------------------- evidence

class Report:
    """The checks and their outcomes, printed as they come and kept for ``qa-<part>.json``: each check under the
    part that made it (``current``; the harness's own checks under none), and each part's time (``parts``)."""

    def __init__(self, part: str):
        self.part = part
        self.started = time.perf_counter()
        self.checks: list[dict] = []
        self.data: dict = {}
        #: The part running now, whose name each check carries.
        self.current: Optional[str] = None
        #: Each part that ran: {"seconds": its time}.
        self.parts: dict = {}

    def check(self, name: str, ok: bool, detail: str = "", **numbers) -> bool:
        """Record one check; returns whether it passed."""
        t = time.perf_counter() - self.started
        print(f"[{t:7.1f} s] [{'ok' if ok else 'FAIL'}] {name}{': ' + detail if detail else ''}", flush=True)
        self.checks.append({"check": name, "ok": bool(ok), "at_s": round(t, 2), "detail": detail[:500],
                            **({"part": self.current} if self.current else {}),
                            **({"numbers": numbers} if numbers else {})})
        return bool(ok)

    def of(self, part: str) -> list:
        """The checks ``part`` made."""
        return [c for c in self.checks if c.get("part") == part]

    def summary(self) -> dict:
        """Each part's checks, passed and failed, and its time: what a run of several parts says at its top."""
        return {name: {"checks": len(self.of(name)), "passed": sum(1 for c in self.of(name) if c["ok"]),
                       "failed": [c["check"] for c in self.of(name) if not c["ok"]], **info}
                for name, info in self.parts.items()}

    def note(self, text: str) -> None:
        """Print a line that is no check."""
        print(f"[{time.perf_counter() - self.started:7.1f} s] {text}", flush=True)

    @property
    def ok(self) -> bool:
        """Whether every check passed (and there was one)."""
        return bool(self.checks) and all(c["ok"] for c in self.checks)


# ---------------------------------------------------------------------------- the fake endpoint

class FakeEndpoint:
    """A fake OpenAI-compatible endpoint on 127.0.0.1 that plays: a **scripted policy, not canned text** (P3.9.4).

    It records every connection it accepts (``connections``) and every request it serves (``requests``: when, what,
    the tools offered, the calls it answered with, how long it took), so a part can say what reached it. A chat
    completion is answered from the request itself (``scripted_reply``): the negotiation it is asked about is
    answered (accepted when it gets at least as much as it gives, as the dry run does); a turn reads its units,
    cities, empire and the players it has met, gives orders that are legal but not clever (founds a city, explores,
    researches, builds), opens a trade with a met civilization (``share_map`` both ways, which needs no technology;
    at most ``max_trades`` times), votes when a vote is open, and ends the turn. ``delay`` seconds pass before each
    completion, so queues, drains and cancels can be seen. ``/models`` (and LM Studio's ``/api/v0/models``) name
    ``MODEL``, loaded (``models`` says otherwise once a part sets it). So a real CITAR helper pointed at it exercises
    the hub, the hello, the requests and ``done.gen_s`` without a GPU.

    **Streaming** (design/phase4-helper.md P4.7.6), as the CITAR helper asks for by default: a request with
    ``stream`` is answered with server-sent events, as LM Studio sends them: the reply's text a word an event (its
    reasoning first, when ``reasoning`` is set), each tool call an event, the finish reason, the usage (with
    ``stream_options.include_usage``) and ``[DONE]``, ``delay`` spread evenly before the events that carry text or
    calls. Between events it watches the socket, so it sees its client go within ``POLL`` seconds; each request's
    entry records when each event went out (``events``), how many carried text (``tokens``) and when its client went
    (``gone``, None when it read to the end). Connections are kept alive between requests (HTTP/1.1), as LM Studio
    keeps them."""

    #: How often a streaming answer looks whether its client went.
    POLL = 0.02

    MODEL = "qa-fake-model"

    def __init__(self, *, delay: float = 0.05, max_trades: int = 1):
        outer = self
        self.connections: list[tuple[float, str]] = []
        self.requests: list[dict] = []
        #: When each chat completion arrived (``time.time()``), before it is answered: ``requests`` lists them once
        #: served.
        self.arrivals: list[float] = []
        #: Reasoning text a streamed answer sends before its text (a model that thinks), or None.
        self.reasoning: Optional[str] = None
        #: Every streamed answer's entry from its first byte, in progress or not (``requests`` lists it once its
        #: answer ended or its client went).
        self.live: list[dict] = []
        self.delay = delay
        #: Slower completions for some prompts: {text in the last user message: seconds} (``delay_for``), for the
        #: kinds of completion in ``slow_kinds`` (``describe``'s), or any kind when it is None.
        self.slow: dict = {}
        self.slow_kinds: Optional[set] = None
        #: Seconds a completion for a model takes, whatever its prompt: {model: seconds} (``delay_for``).
        self.model_delay: dict = {}
        self.max_trades = max_trades
        self.trades_opened = 0
        #: What ``/models`` names: (key, loaded) each. A part changes it to have a helper report other models at its
        #: next connection.
        self.models: list = [(self.MODEL, True)]
        self._lock = threading.Lock()
        #: Completions ``hold(body)`` picks wait for ``gate`` (set: open) before they are answered; ``held`` counts
        #: those waiting now. A part holds a game's turn so the work behind it queues on its machine.
        self.hold: Optional[Callable[[dict], bool]] = None
        self.gate = threading.Event()
        self.gate.set()
        self.held = 0

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def _send(self, code: int, body: dict):
                data = json.dumps(body).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                outer.requests.append({"t": time.time(), "method": "GET", "path": self.path})
                if self.path.rstrip("/").endswith("/models"):
                    return self._send(200, {"object": "list", "data": [
                        {"id": key, "object": "model", "state": "loaded" if loaded else "not-loaded",
                         "max_context_length": 32768} for key, loaded in list(outer.models)]})
                self._send(404, {"error": "not found"})

            def do_POST(self):
                size = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(size) if size else b""
                t0 = time.time()
                entry = {"t": t0, "method": "POST", "path": self.path, "bytes": len(raw)}
                try:
                    body = json.loads(raw or b"{}")
                except ValueError:
                    body = {}
                with outer._lock:
                    outer.arrivals.append(t0)
                if outer.hold is not None and outer.hold(body):
                    with outer._lock:
                        outer.held += 1
                    outer.gate.wait(300)
                    with outer._lock:
                        outer.held -= 1
                t1 = time.time()
                delay = outer.delay_for(body)
                stream = bool(body.get("stream"))
                if delay and not stream:
                    time.sleep(delay)
                reply, calls = outer.scripted_reply(body)
                entry.update(tools=len(body.get("tools") or []), calls=[c["function"]["name"] for c in calls],
                             delay=delay, served=t1, stream=stream, **outer.describe(body))
                if stream:
                    def logged():
                        entry["seconds"] = round(time.time() - t0, 3)
                        outer.requests.append(entry)
                    outer._stream(self, body, reply, delay, entry, logged)
                else:
                    # in the log before the answer goes: a client that reads the log once answered finds it there
                    entry["seconds"] = round(time.time() - t0, 3)
                    outer.requests.append(entry)
                    self._send(200, reply)

            def log_message(self, *args):
                pass

        class Server(ThreadingHTTPServer):
            daemon_threads = True

            def handle_error(self, request, client_address):
                # a connection kept alive between requests ends when its client goes, however it goes (a helper
                # stopped, a cancelled stream): that is no error of the endpoint's
                if isinstance(sys.exc_info()[1], (ConnectionError, TimeoutError)):
                    return
                super().handle_error(request, client_address)

            def get_request(self):
                conn, addr = super().get_request()
                outer.connections.append((time.time(), f"{addr[0]}:{addr[1]}"))
                return conn, addr

        self.httpd = Server(("127.0.0.1", 0), Handler)
        self.port = self.httpd.server_address[1]
        self.url = f"http://127.0.0.1:{self.port}/v1"
        threading.Thread(target=self.httpd.serve_forever, daemon=True, name="qa-fake-endpoint").start()

    # ------------------------------------------------------------------ streaming
    def _stream(self, handler, body: dict, reply: dict, delay: float, entry: dict, logged: Callable) -> None:
        """Answer ``reply`` as server-sent events (the class's doc), recording ``events``, ``tokens`` and ``gone`` in
        ``entry``; ``logged()`` puts the entry in the log once, before the events that end the answer (so a client that
        read the answer finds it there) or once its client went."""
        done = []

        def log_once():
            if not done:
                done.append(True)
                logged()
        message = reply["choices"][0]["message"]
        finish = reply["choices"][0]["finish_reason"]
        deltas: list = []
        if self.reasoning:
            deltas.append(({"role": "assistant", "reasoning_content": self.reasoning}, True))
        words = re.findall(r"\S+\s*", message.get("content") or "")
        for i, word in enumerate(words):
            deltas.append(({"content": word, **({"role": "assistant"} if i == 0 and not self.reasoning else {})}, True))
        for i, call in enumerate(message.get("tool_calls") or []):
            deltas.append(({"tool_calls": [{"index": i, "id": call["id"], "type": "function",
                                            "function": dict(call["function"])}]}, True))
        gap = (delay or 0.0) / max(1, len(deltas))
        events: list = []
        entry.update(events=events, tokens=0, gone=None)
        with self._lock:
            self.live.append(entry)

        def chunk(data: bytes) -> bool:
            try:
                handler.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
                handler.wfile.flush()
                return True
            except OSError:
                return False

        def event(payload) -> bool:
            text = payload if isinstance(payload, str) else json.dumps(payload)
            if entry["gone"] is not None or not chunk(f"data: {text}\n\n".encode()):
                entry["gone"] = entry["gone"] or time.time()
                return False
            events.append(time.time())
            return True

        def wait(seconds: float) -> bool:
            """Wait, watching the socket: False once the client went."""
            end = time.time() + seconds
            sock = handler.connection
            while True:
                left = end - time.time()
                if left <= 0:
                    return True
                try:
                    readable, _, _ = select.select([sock], [], [], min(left, self.POLL))
                    if readable and not sock.recv(1, socket.MSG_PEEK):
                        raise ConnectionResetError("the client went")
                except (OSError, ValueError):
                    entry["gone"] = time.time()
                    return False
                if readable:
                    time.sleep(min(left, self.POLL))

        try:
            handler.send_response(200)
            handler.send_header("Content-Type", "text/event-stream")
            handler.send_header("Cache-Control", "no-cache")
            handler.send_header("Transfer-Encoding", "chunked")
            handler.end_headers()
            handler.wfile.flush()
        except OSError:
            entry["gone"] = time.time()
            handler.close_connection = True
            log_once()
            return
        base = {"id": reply["id"], "object": "chat.completion.chunk", "created": reply["created"],
                "model": reply["model"]}
        for delta, carries in deltas:
            if not wait(gap) or not event({**base, "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}):
                handler.close_connection = True
                log_once()
                return
            if carries and "tool_calls" not in delta:
                entry["tokens"] += 1
        log_once()
        ok = event({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]})
        if ok and (body.get("stream_options") or {}).get("include_usage"):
            ok = event({**base, "choices": [], "usage": reply["usage"]})
        ok = ok and event("[DONE]") and chunk(b"")
        if not ok:
            handler.close_connection = True

    def streamed(self) -> list:
        """The completions it streamed, served (their entries)."""
        return [r for r in self.completions() if r.get("stream")]

    # ------------------------------------------------------------------ the policy
    def completions(self) -> list:
        """The chat completions it served."""
        return [r for r in self.requests if r.get("method") == "POST"]

    @staticmethod
    def describe(body: dict) -> dict:
        """What a completion was, for the log: ``kind`` (``turn``: a game's turn, with ``end_turn`` among its tools;
        ``diplomat``: a hybrid seat's diplomat phase; ``chat``: an answer to a negotiation; ``plain``: no tools, a
        report's narrative), the ``model`` it named, its last user message's first line (``head``) and a ``QA-...``
        marker in it (``marker``)."""
        tools = {(t.get("function") or {}).get("name") for t in body.get("tools") or [] if isinstance(t, dict)}
        messages = [m for m in body.get("messages") or [] if isinstance(m, dict)]
        last = next((str(m.get("content") or "") for m in reversed(messages) if m.get("role") == "user"), "")
        found = re.search(r"QA-[A-Za-z0-9-]+", last)
        phase = FakeEndpoint._phase_start(messages) is not None
        system = next((str(m.get("content") or "") for m in messages if m.get("role") == "system"), "")
        return {"kind": "turn" if "end_turn" in tools else "diplomat" if phase
                else "chat" if "respond_negotiation" in tools else "plain" if not tools else "other",
                "model": body.get("model"), "head": last.splitlines()[0][:120] if last else "",
                "system": system.splitlines()[0][:60] if system else "", "marker": found.group(0) if found else None}

    @staticmethod
    def _phase_start(messages: list) -> Optional[int]:
        """Where a hybrid seat's diplomat phase begins in a conversation (its first message, ``DIPLOMAT PHASE``), or
        None for a conversation that is no diplomat phase."""
        return next((i for i, m in enumerate(messages) if isinstance(m, dict) and m.get("role") == "user"
                     and str(m.get("content") or "").startswith("DIPLOMAT PHASE")), None)

    def delay_for(self, body: dict) -> float:
        """How long a completion takes: ``slow``'s seconds for a prompt whose last user message holds one of its
        texts (and whose kind is one of ``slow_kinds``, when they are named), else ``model_delay``'s for its model,
        else ``delay``."""
        if body.get("model") in self.model_delay:
            return self.model_delay[body.get("model")]
        if self.slow_kinds is not None and self.describe(body)["kind"] not in self.slow_kinds:
            return self.delay
        last = next((str(m.get("content") or "") for m in reversed(body.get("messages") or [])
                     if isinstance(m, dict) and m.get("role") == "user"), "")
        return next((seconds for text, seconds in self.slow.items() if text in last), self.delay)

    def scripted_reply(self, body: dict) -> tuple:
        """A chat completion for ``body`` (an OpenAI request) and the tool calls in it."""
        tools = {(t.get("function") or {}).get("name") for t in body.get("tools") or [] if isinstance(t, dict)}
        messages = [m for m in body.get("messages") or [] if isinstance(m, dict)]
        last_user = max((i for i, m in enumerate(messages) if m.get("role") == "user"), default=-1)
        since = messages[last_user + 1:]
        steps = sum(1 for m in since if m.get("role") == "assistant")
        prompt = str(messages[last_user].get("content") or "") if last_user >= 0 else ""
        calls: list = []

        def call(name: str, args: Optional[dict] = None) -> None:
            if name in tools:
                calls.append({"id": f"qa_{len(self.requests)}_{len(calls)}", "type": "function",
                              "function": {"name": name, "arguments": json.dumps(args or {})}})

        if "end_turn" not in tools and self._phase_start(messages) is not None:
            for name, args in self._diplomat(messages, tools):
                call(name, args)
        elif "respond_negotiation" in tools and "end_turn" not in tools:
            found = re.search(r"negotiation #(\d+)", prompt)
            if found and steps == 0:
                gives = prompt.split('"you_receive"')[0].count('"type"') if '"you_receive"' in prompt else 0
                gets = prompt.count('"type"') - gives
                action = "reply" if '"current_proposal": null' in prompt else ("accept" if gets >= gives else "reject")
                call("respond_negotiation", {"negotiation_id": int(found.group(1)), "action": action,
                                             "message": f"(qa endpoint) {action}"})
        elif steps == 0:
            # a chat that came back to it during its turn (``prompts.LATE_ANSWERED``): closed with a word, as the
            # dry run's turn would leave it to its chat thread
            for nid in dict.fromkeys(re.findall(r"answered negotiation #(\d+)", prompt)):
                call("respond_negotiation", {"negotiation_id": int(nid), "action": "reject",
                                             "message": "(qa endpoint) thank you, that is all for now"})
            for name in ("get_units", "get_cities", "get_empire", "get_players"):
                call(name)
            if not calls:
                call("end_turn")
        elif steps == 1:
            results = self._results(since)
            calls += self._orders(results, tools)
            if "vote" in prompt.lower() and "un_vote" in tools:
                call("un_vote", {"candidate": (results.get("get_empire") or {}).get("id", "abstain")})
            if not calls:
                call("end_turn")
        else:
            call("end_turn")
        message = {"role": "assistant", "content": "(qa endpoint) " + (", ".join(c["function"]["name"] for c in calls)
                                                                      or "nothing to do")}
        if calls:
            message["tool_calls"] = calls
        reply = {"id": f"qa-{len(self.requests)}", "object": "chat.completion", "created": int(time.time()),
                 "model": body.get("model") or self.MODEL,
                 "choices": [{"index": 0, "finish_reason": "tool_calls" if calls else "stop", "message": message}],
                 "usage": {"prompt_tokens": sum(len(str(m.get("content") or "")) for m in messages) // 4,
                           "completion_tokens": 10 + 10 * len(calls),
                           "total_tokens": sum(len(str(m.get("content") or "")) for m in messages) // 4 + 10}}
        return reply, calls

    @staticmethod
    def _results(since: list) -> dict:
        """The turn's tool results by tool name: each assistant call's id matched to its tool message."""
        names = {}
        for m in since:
            for c in m.get("tool_calls") or []:
                names[c.get("id")] = (c.get("function") or {}).get("name")
        out = {}
        for m in since:
            if m.get("role") == "tool" and names.get(m.get("tool_call_id")):
                try:
                    out[names[m["tool_call_id"]]] = json.loads(m.get("content") or "null")
                except ValueError:
                    pass
        return out

    def _orders(self, results: dict, tools: set) -> list:
        """Orders that are legal but not clever, as the dry run gives (``DryRunConversation._orders``), and a trade
        with a met civilization."""
        units = results.get("get_units") if isinstance(results.get("get_units"), list) else []
        cities = results.get("get_cities") if isinstance(results.get("get_cities"), list) else []
        empire = results.get("get_empire") if isinstance(results.get("get_empire"), dict) else {}
        players = results.get("get_players") if isinstance(results.get("get_players"), list) else []
        calls = []

        def call(name, args):
            if name in tools:
                calls.append({"id": f"qa_{len(self.requests)}_{len(calls)}_{name}", "type": "function",
                              "function": {"name": name, "arguments": json.dumps(args)}})
        # the engine names unit types as the ruleset does ("Settler")
        settler = next((u for u in units if isinstance(u, dict) and str(u.get("type") or "").lower() == "settler"),
                       None)
        if settler and not cities:
            call("found_city", {"unit_id": settler["id"]})
        for u in units:
            if isinstance(u, dict) and u.get("class") != "civilian" and u is not settler and not u.get("activity"):
                call("unit_order", {"unit_id": u["id"], "order": "explore"})
        if not empire.get("researching"):
            call("set_research", {"tech": "pottery"})
        for c in cities:
            if isinstance(c, dict) and not c.get("queue"):
                call("set_production", {"city_id": c["id"], "item": "warrior"})
        me = empire.get("id")
        met = [p for p in players if isinstance(p, dict) and p.get("met") and p.get("alive")
               and p.get("kind") == "major" and p.get("id") != me and p.get("name") != empire.get("name")]
        with self._lock:
            trade = bool(met) and self.trades_opened < self.max_trades and "open_negotiation" in tools
            if trade:
                self.trades_opened += 1
        if trade:
            call("open_negotiation", {"to": met[0]["id"], "message": "(qa endpoint) shall we share our maps?",
                                      "give": [{"type": "share_map"}], "receive": [{"type": "share_map"}]})
        return calls

    def _diplomat(self, messages: list, tools: set) -> list:
        """A hybrid seat's diplomat phase, played as the dry-run diplomat plays it (``DryRunConversation._diplomat``,
        P3.9.4), as ``(tool, args)`` pairs: first it looks (its diplomacy, its empire, its cities); then it proposes
        maps for maps to a civilization it has met, is not at war with and is not talking to, abstains when a United
        Nations vote is open and its vote is not cast, annexes a puppet it holds, and answers each chat the phase was
        told of (``prompts.DIPLOMAT_CHAT``: accepted when a deal is on the table, else a reply); after that it answers
        only the chats the phase is told of later, and stops by answering without a tool call."""
        start = self._phase_start(messages)
        after = messages[start + 1:]
        steps = [i for i, m in enumerate(after) if m.get("role") == "assistant"]
        if not steps:
            return [(name, {}) for name in ("get_diplomacy", "get_empire", "get_cities") if name in tools]
        out: list = []
        # what the phase was told since its last step: its first message on the second step, and the chats' news
        told = [m for m in after[steps[-1] + 1:] if m.get("role") == "user"]
        if len(steps) == 1:
            told.insert(0, messages[start])
            seen = self._results(after)
            dip = seen.get("get_diplomacy") if isinstance(seen.get("get_diplomacy"), dict) else {}
            cities = seen.get("get_cities") if isinstance(seen.get("get_cities"), list) else []
            met = [p for p in dip.get("players") or [] if isinstance(p, dict) and p.get("alive") is not False]
            talking = {n.get("with") for n in dip.get("open_negotiations") or [] if isinstance(n, dict)}
            free = next((p for p in met if p.get("id") not in talking and not p.get("at_war")), None)
            if "open_negotiation" in tools and free is not None:
                out.append(("open_negotiation", {"to": free["id"], "message": "(qa endpoint) shall we share our maps?",
                                                 "give": [{"type": "share_map"}], "receive": [{"type": "share_map"}]}))
            un = dip.get("united_nations") or {}
            if "un_vote" in tools and un.get("voting_open") and un.get("your_vote") == "not cast":
                out.append(("un_vote", {"candidate": "abstain"}))
            puppet = next((c for c in cities if isinstance(c, dict) and c.get("puppet")), None)
            if "city_status" in tools and puppet is not None:
                out.append(("city_status", {"city_id": puppet.get("id"), "status": "annex"}))
        text = "\n".join(str(m.get("content") or "") for m in told)
        if "respond_negotiation" in tools:
            for found in re.finditer(r"CHAT: .*? negotiation #(\d+): (.*)", text):
                action = "reply" if "none (no deal on the table)" in found.group(2) else "accept"
                out.append(("respond_negotiation", {"negotiation_id": int(found.group(1)), "action": action,
                                                    "message": f"(qa endpoint) {action}"}))
        return out

    def close(self) -> None:
        """Stop serving."""
        self.httpd.shutdown()
        self.httpd.server_close()


# ---------------------------------------------------------------------------- the server and its accounts

class Server:
    """The dev server as a child process, on the harness's directory and port, and ``admin_cli`` on the same
    database."""

    def __init__(self, state: Path, port: int):
        self.state, self.port = state, port
        self.env = _dev_server().instance_env(state, port, server=True, registration="invite")
        # requests go to 127.0.0.1, which sends no Origin; the browser checklist uses the public origin (localhost)
        self.base = f"http://127.0.0.1:{port}"
        self.public = self.env["CITAR_PUBLIC_ORIGIN"]
        self.log = state / "server.log"
        self.proc: Optional[subprocess.Popen] = None

    def start(self, timeout: float = 120.0) -> None:
        """Start the server and wait until it answers."""
        out = open(self.log, "w", encoding="utf-8")
        args = [sys.executable, str(ROOT / "scripts" / "dev_server.py"), "--server", "--registration", "invite",
                "--port", str(self.port), "--dir", str(self.state)]
        self.proc = subprocess.Popen(args, cwd=str(ROOT), stdout=out, stderr=subprocess.STDOUT,
                                     env={**self.env, "PYTHONUNBUFFERED": "1"})
        out.close()
        t0 = time.perf_counter()
        with httpx.Client(base_url=self.base, timeout=10.0) as c:
            while True:
                try:
                    if c.get("/api/auth/config").status_code == 200:
                        return
                except httpx.HTTPError:
                    pass
                if self.proc.poll() is not None or time.perf_counter() - t0 > timeout:
                    raise HarnessError(f"the server did not start (exit {self.proc.poll()}); see {self.log}")
                time.sleep(0.25)

    def cli(self, *args: str, stdin: Optional[str] = None) -> subprocess.CompletedProcess:
        """``python -m citar.server.admin_cli`` on the instance's database."""
        return subprocess.run([sys.executable, "-m", "citar.server.admin_cli", *args], cwd=str(ROOT), env=self.env,
                              input=stdin, capture_output=True, text=True, timeout=180)

    def stop(self) -> None:
        """Stop the server."""
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def restart(self, timeout: float = 120.0) -> None:
        """Stop the server as a crash or a power cut would (on Windows ``terminate`` is a kill: no shutdown runs, so
        its open games keep their live marks) and start it again on the same directory and port, as its owner would.
        The log so far is kept as ``server-<n>.log``."""
        self.stop()
        kept = self.state / f"server-{len(list(self.state.glob('server-*.log'))) + 1}.log"
        # A venv's python.exe on Windows is a launcher: the server it started goes a moment after it (its job object),
        # and with it the log's handle and the port. Wait for both.
        deadline = time.perf_counter() + 30.0
        while True:
            try:
                if self.log.exists():
                    self.log.replace(kept)
                with socket.socket() as probe:
                    probe.settimeout(1.0)
                    listening = probe.connect_ex(("127.0.0.1", self.port)) == 0
                if not listening:
                    break
            except PermissionError:
                pass
            if time.perf_counter() > deadline:
                raise HarnessError(f"the stopped server still holds {self.log} or port {self.port}")
            time.sleep(0.1)
        self.start(timeout)

    def tracebacks(self) -> int:
        """Tracebacks in the server's logs: this run's, and those before a restart (``restart``)."""
        logs = [self.log, *sorted(self.state.glob("server-*.log"))]
        return sum(p.read_text(encoding="utf-8", errors="replace").count("Traceback") for p in logs if p.exists())


class Account:
    """One account, signed in: an HTTP client with its session cookie and CSRF token."""

    def __init__(self, server: Server, handle: str, role: str, password: str):
        self.handle, self.role, self.password = handle, role, password
        self.http = httpx.Client(base_url=server.base, timeout=120.0)

    def login(self) -> httpx.Response:
        """Sign in through ``/api/auth/login``; the cookie stays in the client, the CSRF token in its headers."""
        r = self.http.post("/api/auth/login", json={"identifier": self.handle, "password": self.password})
        if r.status_code == 200:
            self.http.headers["x-citar-csrf"] = r.json().get("csrf_token", "")
        return r

    def reconnect(self) -> None:
        """A fresh client with the same cookie and CSRF token (the server was restarted: the session is still good,
        the old client's pooled connections are not)."""
        old = self.http
        self.http = httpx.Client(base_url=old.base_url, timeout=old.timeout, cookies=old.cookies, headers=old.headers)
        old.close()

    def __getattr__(self, name):
        # get / post / put / delete go straight to the client
        return getattr(self.http, name)


class Harness:
    """What a part is given: the report, the server, the signed-in accounts, the fake endpoint, a tag that makes
    this run's names unique, and an anonymous client maker."""

    def __init__(self, report: Report, server: Server, accounts: dict, endpoint: FakeEndpoint, *, keep: bool = False,
                 hold_line: float = 0.0):
        self.report, self.server, self.accounts, self.endpoint = report, server, accounts, endpoint
        self.tag = secrets.token_hex(3)
        #: ``--keep``: the server stays up for the browser checklist, and so does what a part leaves for it to show
        self.keep = keep
        #: ``--hold-line``: seconds the ``queue`` part keeps its pooled prompts waiting (the browser checklist's C1)
        self.hold_line = max(0.0, float(hold_line or 0.0))

    def anonymous(self, **headers) -> httpx.Client:
        """A client with no account (and the headers given, a share key's say)."""
        return httpx.Client(base_url=self.server.base, timeout=60.0, headers=headers)

    def ws_hello(self, gid: str, *, key: str = "") -> Optional[dict]:
        """The first message of the game socket, opened by a visitor with no account and no token, the share key
        ``key`` on its URL; None when the server refuses it."""
        from websockets.sync.client import connect
        url = f"ws://127.0.0.1:{self.server.port}/ws/games/{gid}?token=" + (f"&k={key}" if key else "")
        try:
            with connect(url, open_timeout=15, proxy=None) as ws:
                return json.loads(ws.recv(timeout=15))
        except Exception:
            return None


def wait_until(fn: Callable[[], object], timeout: float, interval: float = 1.0):
    """Call ``fn`` until it returns something true or ``timeout`` seconds pass; returns its last value."""
    deadline = time.perf_counter() + timeout
    while True:
        value = fn()
        if value or time.perf_counter() > deadline:
            return value
        time.sleep(interval)


# ---------------------------------------------------------------------------- the parts

#: A small game that plays quickly: no barbarians, a duel map, a turn limit far off.
CONFIG = {"map_size": "duel", "map_type": "pangaea", "seed": 3303, "barbarians": "off", "turn_limit": 400}
#: A member's model seat that dials nothing: the dry-run provider, inline (P3.2.9 admits it from a member).
DRY = {"provider": "dryrun", "model": "qa-dry-run"}


def _share_key(url: Optional[str]) -> str:
    """The ``k`` of a share URL (``.../#/game/<id>?k=<key>``)."""
    if not url or "?" not in url:
        return ""
    return (parse_qs(url.split("?", 1)[1]).get("k") or [""])[0]


def _ids(listing) -> set:
    """The ids of a listing (a list of objects with ``id``)."""
    return {x.get("id") for x in listing if isinstance(x, dict)} if isinstance(listing, list) else set()


def part_accounts(h: Harness) -> None:
    """The ``accounts`` part: 3-03's sharing checks, 3-04's profiles and administration, 3-05's machines, then 3-06's
    Servers page. Each runs whatever the others found."""
    _accounts_sharing(h)
    _accounts_profiles(h)
    _accounts_machines(h)
    _accounts_servers_page(h)


def _accounts_sharing(h: Harness) -> None:
    """3-03's checks: the sharing of a game and of a report (the visibility ladder, links and their keys, sharing by
    handle, ``/r/{slug}``, a moderator's unpublish), members' model blocks (nothing dialled), and a member's report
    gathering only what they may see."""
    r = h.report
    admin, alice, bob, mod = (h.accounts[k] for k in ("admin", "alice", "bob", "mod"))
    anon = h.anonymous()

    # ---- two games with a dry-run model seat (the usage ledger records games with a model seat): alice's, which
    # her sharing checks use, and bob's private one, which her report must not show
    secret = f"QA bob secret {h.tag}"
    seats = [{"type": "llm", "name": "Dry run", "llm": DRY}, {"type": "bot"}, {"type": "bot"}]
    g1 = alice.post("/api/games", json={"name": f"QA alice game {h.tag}", "config": CONFIG, "seats": seats})
    g2 = bob.post("/api/games", json={"name": secret, "config": CONFIG, "seats": seats[:2]})
    if not r.check("members make games with an inline dry-run model seat", g1.status_code == 200 and g2.status_code == 200,
                   f"alice HTTP {g1.status_code}, bob HTTP {g2.status_code}: {g1.text[:120]} {g2.text[:120]}"):
        return
    gid, bob_gid = g1.json()["id"], g2.json()["id"]
    r.data["games"] = {"alice": gid, "bob": bob_gid}
    sharing = f"/api/games/{gid}/sharing"

    # ---- a private game
    r.check("a private game is not listed for another member", gid not in _ids(bob.get("/api/games").json()))
    r.check("a private game is 404 for another member, and for a visitor",
            bob.get(f"/api/games/{gid}").status_code == 404 and anon.get(f"/api/games/{gid}").status_code == 404)
    r.check("another member cannot change its sharing (404)",
            bob.put(sharing, json={"visibility": "server"}).status_code == 404)

    # ---- shared with members
    put = alice.put(sharing, json={"visibility": "server"})
    r.check("its owner shares it with members", put.status_code == 200 and put.json().get("visibility") == "server",
            put.text[:200])
    info = bob.get(f"/api/games/{gid}")
    tokens = info.status_code == 200 and ("spectator_token" in info.json()
                                          or any(s.get("token") for s in info.json().get("seats", [])))
    r.check("a member sees it in the list and opens it, with no seat token",
            gid in _ids(bob.get("/api/games").json()) and info.status_code == 200 and not tokens)
    view = bob.get(f"/api/games/{gid}/view")
    r.check("a member watches it with no token (the account is the credential)", view.status_code == 200,
            f"HTTP {view.status_code}")
    r.check("a visitor still gets 404, and it is not in their list",
            anon.get(f"/api/games/{gid}").status_code == 404 and gid not in _ids(anon.get("/api/games").json()))
    r.check("a member who only sees it cannot change it (403)",
            bob.put(sharing, json={"visibility": "private"}).status_code == 403
            and bob.post(f"/api/games/{gid}/control", json={"paused": True}).status_code == 403)

    # ---- shared by link
    put = alice.put(sharing, json={"visibility": "link"})
    key = _share_key(put.json().get("share_url")) if put.status_code == 200 else ""
    r.check("by link: the share URL carries its key", put.status_code == 200 and bool(key), put.text[:200])
    keyed = h.anonymous(**{"x-citar-share-key": key})
    r.check("a link game is unlisted, even for members", gid not in _ids(bob.get("/api/games").json()))
    r.check("the key opens it, for a member and a visitor",
            bob.get(f"/api/games/{gid}", headers={"x-citar-share-key": key}).status_code == 200
            and keyed.get(f"/api/games/{gid}").status_code == 200)
    hello, refused = h.ws_hello(gid, key=key), h.ws_hello(gid)
    r.check("the game socket takes the key (k) from a visitor, and refuses one without it",
            bool(hello) and hello.get("type") == "hello" and hello.get("spectator") is True and refused is None,
            f"with the key: {hello}; without: {refused}")
    rot = alice.put(sharing, json={"rotate_slug": True})
    new_key = _share_key(rot.json().get("share_url")) if rot.status_code == 200 else ""
    r.check("a new link: the old key opens nothing",
            bool(new_key) and new_key != key and keyed.get(f"/api/games/{gid}").status_code == 404)

    # ---- shared with one person
    alice.put(sharing, json={"visibility": "private"})
    add = alice.post(f"{sharing}/users", json={"handle": bob.handle, "permission": "view"})
    r.check("shared with bob by handle: he lists and opens it",
            add.status_code == 200 and gid in _ids(bob.get("/api/games").json())
            and bob.get(f"/api/games/{gid}").status_code == 200, add.text[:200])
    rm = alice.delete(f"{sharing}/users/{bob.handle}")
    r.check("and once removed, it is 404 for him again",
            rm.status_code == 200 and bob.get(f"/api/games/{gid}").status_code == 404)

    # ---- published, and taken down by a moderator
    r.check("a member may not publish to the internet (403)",
            alice.put(sharing, json={"visibility": "global"}).status_code == 403)
    pub = admin.put(sharing, json={"visibility": "global"})
    r.check("an administrator publishes it; a visitor opens and lists it",
            pub.status_code == 200 and anon.get(f"/api/games/{gid}").status_code == 200
            and gid in _ids(anon.get("/api/games").json()), pub.text[:200])
    r.check("a moderator may not control it (403)",
            mod.post(f"/api/games/{gid}/control", json={"paused": True}).status_code == 403)
    reason = f"QA take-down {h.tag}"
    down = mod.post(f"/api/games/{gid}/unpublish", json={"reason": reason})
    seen = alice.get(sharing).json() if down.status_code == 200 else {}
    r.check("a moderator unpublishes it: private, 404 for visitors, the owner sees why",
            down.status_code == 200 and seen.get("visibility") == "private"
            and anon.get(f"/api/games/{gid}").status_code == 404
            and (seen.get("unpublished") or {}).get("by") == "a moderator"
            and (seen.get("unpublished") or {}).get("reason") == reason, json.dumps(seen.get("unpublished")))

    # ---- members' model blocks (P3.2.9): a seat that describes a machine is refused and nothing is dialled
    trap = h.endpoint
    before = len(trap.connections)
    block = {"provider": "openai_compatible", "base_url": trap.url, "api_key_env": "PATH", "model": trap.MODEL}
    made = alice.post("/api/games", json={"name": f"QA dial {h.tag}", "config": CONFIG,
                                          "seats": [{"type": "llm", "llm": block}, {"type": "bot"}]})
    seat = alice.post(f"/api/games/{gid}/seats/1", json={"type": "llm", "llm": block})
    still = alice.get(f"/api/games/{gid}").json()["seats"][1]["type"]
    time.sleep(1.0)
    r.check("a member's seat naming a base URL and a key variable is refused (400), at creation and as a seat change",
            made.status_code == 400 and seat.status_code == 400 and still == "bot",
            f"create HTTP {made.status_code}, seat HTTP {seat.status_code}, the seat is still {still}")
    r.check("and nothing connected to the address it named", len(trap.connections) == before,
            f"{len(trap.connections) - before} connection(s) to {trap.url}", connections=len(trap.connections) - before)

    # ---- the old report routes are gone
    r.check("share_api's report routes are gone (404)",
            alice.get("/api/shared/reports").status_code == 404 and anon.get("/api/shared/reports").status_code == 404)

    # ---- a member's report gathers only what they may see
    def ledger_has_both():
        opts = admin.get("/api/reports/options")
        found = {i.get("id") for i in opts.json().get("items", [])} if opts.status_code == 200 else set()
        return {gid, bob_gid} <= found
    t0 = time.perf_counter()
    landed = wait_until(ledger_has_both, timeout=150, interval=2.0)
    r.check("both games reached the usage ledger", bool(landed), f"after {time.perf_counter() - t0:.0f} s",
            seconds=round(time.perf_counter() - t0, 1))
    spec = {"title": f"QA everything {h.tag}", "sections": ["summary", "activities", "models", "costs"],
            "range": {"preset": "all"}}
    mine = alice.post("/api/reports", json=spec)
    theirs = admin.post("/api/reports", json={**spec, "title": f"QA admin everything {h.tag}"})
    if not r.check("a member and an administrator build reports over everything",
                   mine.status_code == 200 and theirs.status_code == 200, f"{mine.text[:150]} {theirs.text[:150]}"):
        return
    rid, admin_rid = mine.json()["id"], theirs.json()["id"]

    def built(who, report_id):
        meta = who.get(f"/api/reports/{report_id}").json().get("meta", {})
        return meta if meta.get("status") in ("done", "failed") else None
    done_mine, done_admin = wait_until(lambda: built(alice, rid), 180), wait_until(lambda: built(admin, admin_rid), 180)
    page = alice.get(f"/api/reports/{rid}/html").text if (done_mine or {}).get("status") == "done" else ""
    admin_page = admin.get(f"/api/reports/{admin_rid}/html").text if (done_admin or {}).get("status") == "done" else ""
    r.check("both reports were built", bool(page) and bool(admin_page),
            f"{(done_mine or {}).get('status')} / {(done_admin or {}).get('status')}")
    r.check("the member's report shows their own game and none of bob's private one (name and id)",
            f"QA alice game {h.tag}" in page and secret not in page and bob_gid not in page)
    r.check("the administrator's report shows both", secret in admin_page and f"QA alice game {h.tag}" in admin_page)

    # ---- a report's sharing, through the item routes, and its public page
    item = f"/api/items/report/{rid}"
    r.check("another member cannot see the report (404), nor list it",
            bob.get(f"/api/reports/{rid}").status_code == 404 and rid not in _ids(bob.get("/api/reports").json())
            and bob.get(f"{item}/sharing").status_code == 404)
    put = alice.put(f"{item}/sharing", json={"visibility": "link"})
    body = put.json() if put.status_code == 200 else {}
    rkey = _share_key(body.get("share_url"))
    public = urlparse(body.get("public_url") or "").path
    r.check("by link: a share URL with its key and a public page at /r/<key>",
            bool(rkey) and public == f"/r/{rkey}", json.dumps({k: body.get(k) for k in ("share_url", "public_url")}))
    pub_page = anon.get(f"/r/{rkey}")
    r.check("a visitor reads /r/<key>; the report's id is no address for a link report",
            pub_page.status_code == 200 and spec["title"] in pub_page.text and anon.get(f"/r/{rid}").status_code == 404,
            f"HTTP {pub_page.status_code}")
    r.check("the key lists it for a member who holds it, and only then",
            rid in _ids(bob.get("/api/reports", headers={"x-citar-share-key": rkey}).json())
            and rid not in _ids(bob.get("/api/reports").json()))
    r.check("a member may not publish a report to the internet (403)",
            alice.put(f"{item}/sharing", json={"visibility": "global"}).status_code == 403)
    admin.put(f"{item}/sharing", json={"visibility": "global"})
    r.check("published by an administrator: /r/<id> serves it to a visitor",
            anon.get(f"/r/{rid}").status_code == 200 and anon.get(f"/api/reports/{rid}").status_code == 200)
    down = mod.post(f"{item}/unpublish", json={"reason": reason})
    seen = alice.get(f"{item}/sharing").json() if down.status_code == 200 else {}
    r.check("a moderator unpublishes the report: /r/ answers 404, the owner sees why",
            down.status_code == 200 and anon.get(f"/r/{rid}").status_code == 404
            and anon.get(f"/r/{rkey}").status_code == 404
            and (seen.get("unpublished") or {}).get("by") == "a moderator", json.dumps(seen.get("unpublished")))

    # ---- pooled figures: a member's scores say what they were made from
    scores = bob.get("/api/models/scores")
    r.check("a member's model scores are scoped (their own and pooled games)",
            scores.status_code == 200 and "scope" in scores.json() and "scope" not in admin.get("/api/models/scores").json())

    # the games stop, so a kept server does not play on
    for who, g in ((alice, gid), (bob, bob_gid)):
        who.post(f"/api/games/{g}/control", json={"paused": True})


def _chunk(kind: bytes, data: bytes) -> bytes:
    """A PNG chunk with its checksum."""
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def _png(width: int, height: int, *, author: str = "") -> bytes:
    """A small, whole PNG (one colour), made here: the harness adds no image library. ``author`` goes in a tEXt
    chunk, as the program that saved a picture often puts its owner's name there."""
    rows = b"".join(b"\x00" + b"\x3c\x78\xd8" * width for _ in range(height))
    text = _chunk(b"tEXt", b"Author\0" + author.encode("latin-1")) if author else b""
    return (b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)) + text
            + _chunk(b"IDAT", zlib.compress(rows)) + _chunk(b"IEND", b""))


def _accounts_profiles(h: Harness) -> None:
    """3-04's checks (design/phase3-features.md P3.2.6, P3.2.7): a member's profile (bio, links, an avatar checked by
    its header, each field's visibility), the directory and its search, a public profile for a visitor with a private
    name kept back; the admin users routes (404 for anyone but an administrator, a change of capabilities, limits and
    trust with its one audit row, the last administrator kept), ``admin_cli``'s new commands on the server's database,
    probation and a suspension."""
    r = h.report
    admin, alice, bob, mod = (h.accounts[k] for k in ("admin", "alice", "bob", "mod"))
    anon = h.anonymous()
    name = f"Alice QA {h.tag}"

    # ---- a member's profile
    put = alice.put("/api/auth/me", json={"display_name": name, "bio": f"QA bio {h.tag}\nsecond line",
                                          "links": [{"label": "QA", "url": "https://example.com/qa"}]})
    r.check("a member writes a profile: a display name, a bio and a link",
            put.status_code == 200 and put.json()["user"]["bio"].startswith("QA bio"), put.text[:200])
    js = alice.put("/api/auth/me", json={"links": [{"label": "x", "url": "javascript:alert(1)"}]})
    long = alice.put("/api/auth/me", json={"bio": "x" * 1001})
    kept = alice.get("/api/auth/me").json()["user"]
    r.check("a javascript: link and a bio over 1,000 characters are refused (400) and change nothing",
            js.status_code == 400 and long.status_code == 400 and kept["bio"].startswith("QA bio")
            and kept["links"][0]["url"] == "https://example.com/qa", f"HTTP {js.status_code} / {long.status_code}")
    author = f"Alice Author QA {h.tag}"
    up = alice.post("/api/auth/me/avatar", files={"file": ("me.png", _png(48, 48, author=author), "image/png")})
    avatar = up.json()["user"]["avatar"] if up.status_code == 200 else ""
    got = bob.get(avatar) if avatar else None
    r.check("an avatar (a PNG checked by its header) is served to a member as image/png, nosniff and sandboxed",
            got is not None and got.status_code == 200 and got.headers.get("content-type") == "image/png"
            and got.headers.get("x-content-type-options") == "nosniff"
            and got.headers.get("content-security-policy") == "default-src 'none'; sandbox",
            f"upload HTTP {up.status_code} {up.text[:120]}")
    r.check("the picture's metadata (a tEXt chunk naming its author) is left out of what is served",
            got is not None and got.content == _png(48, 48) and author.encode() not in got.content,
            f"{len(got.content) if got is not None else 0} bytes served")
    garbled = alice.post("/api/auth/me/avatar", content=b"--x\r\n" + bytes(range(256)) + b"\r\n--x--\r\n",
                         headers={"content-type": "multipart/form-data; boundary=zz"})
    r.check("an upload that is not a well-formed form is a 400, and the picture stays",
            garbled.status_code == 400 and alice.get("/api/auth/me").json()["user"]["avatar"] == avatar,
            f"HTTP {garbled.status_code} {garbled.text[:120]}")
    r.check("a visitor gets 404 for the picture while it is members-only", bool(avatar) and anon.get(avatar).status_code == 404)
    svg = alice.post("/api/auth/me/avatar", files={"file": ("x.png", b"<svg xmlns='http://www.w3.org/2000/svg'/>",
                                                           "image/png")})
    r.check("an SVG named .png is refused (400) and the picture stays", svg.status_code == 400
            and alice.get("/api/auth/me").json()["user"]["avatar"] == avatar, f"HTTP {svg.status_code}")

    # ---- the directory and the profile's visibility
    listed = bob.get("/api/users", params={"q": alice.handle})
    r.check("the directory lists her for a member, with her name; a visitor gets 401",
            listed.status_code == 200 and [u["display_name"] for u in listed.json()["users"]] == [name]
            and anon.get("/api/users").status_code == 401, listed.text[:200])
    r.check("her profile is members-only by default: a member opens it, a visitor gets 404",
            bob.get(f"/api/users/{alice.handle}").status_code == 200
            and anon.get(f"/api/users/{alice.handle}").status_code == 404)
    put = alice.put("/api/auth/me", json={"field_visibility": {"profile": "global", "avatar": "global", "bio": "global",
                                                                "display_name": "private"}})
    page = anon.get(f"/api/users/{alice.handle}")
    r.check("made public with her name kept private: a visitor opens it and sees her handle, her bio, not her name",
            put.status_code == 200 and page.status_code == 200 and name not in page.text
            and page.json().get("display_name") == alice.handle and "QA bio" in page.text, page.text[:200])
    r.check("and a visitor sees her picture", anon.get(avatar).status_code == 200)
    r.check("a member's directory search for her private name finds nothing",
            bob.get("/api/users", params={"q": name}).json().get("users") == [])
    huge = str(2 ** 63)
    pages = (bob.get("/api/users", params={"offset": huge}), admin.get("/api/admin/users", params={"offset": huge}),
             admin.get("/api/admin/audit", params={"before": huge}))
    r.check("a page number past SQLite's integer is a 422 on the directory, the users list and the audit log",
            [x.status_code for x in pages] == [422, 422, 422], str([x.status_code for x in pages]))
    alice.put("/api/auth/me", json={"listed": False})
    r.check("hidden from the directory, her page still opens",
            alice.handle not in [u["handle"] for u in bob.get("/api/users").json()["users"]]
            and anon.get(f"/api/users/{alice.handle}").status_code == 200)
    alice.put("/api/auth/me", json={"listed": True, "field_visibility": {"display_name": "server"}})

    # ---- the admin users page's routes
    bob_id = bob.get("/api/auth/me").json()["user"]["id"]
    admin_id = admin.get("/api/auth/me").json()["user"]["id"]
    routes = ["/api/admin/users", f"/api/admin/users/{bob_id}", "/api/admin/audit"]
    r.check("a member and a moderator get 404 on every admin route, a visitor 401",
            all(bob.get(u).status_code == 404 and mod.get(u).status_code == 404 and anon.get(u).status_code == 401
                for u in routes) and bob.patch(f"/api/admin/users/{bob_id}", json={"trust_tier": "trusted"}).status_code == 404)
    patch = admin.patch(f"/api/admin/users/{bob_id}", json={"caps": {"run_lab": True}, "max_concurrent_games": 3,
                                                            "trust_tier": "provisional"})
    r.check("an administrator changes a member's capabilities, limits and trust",
            patch.status_code == 200 and patch.json().get("changed") == ["caps", "max_concurrent_games", "trust_tier"],
            patch.text[:200])
    r.check("the member's own capabilities follow", bob.get("/api/auth/me").json()["capabilities"].get("run_lab") is True)
    rows = admin.get("/api/admin/audit", params={"object_id": bob_id, "action": "user.updated"}).json().get("entries", [])
    r.check("one audit row names the fields, by the administrator",
            len(rows) == 1 and rows[0]["detail"].get("fields") == ["caps", "max_concurrent_games", "trust_tier"]
            and rows[0]["actor_id"] == admin_id, str(rows)[:300])
    unset = admin.patch(f"/api/admin/users/{bob_id}", json={"caps": {"run_lab": None}})
    r.check("null removes the capability override",
            unset.status_code == 200 and unset.json()["user"]["caps"] == {}
            and bob.get("/api/auth/me").json()["capabilities"].get("run_lab") is False)
    last = admin.patch(f"/api/admin/users/{admin_id}", json={"role": "user"})
    r.check("the last administrator cannot demote themselves (409)", last.status_code == 409, last.text[:200])
    zero = admin.patch(f"/api/admin/users/{bob_id}", json={"max_concurrent_games": 0})
    r.check("a game limit of 0, which the server would read as its default, is refused (400)",
            zero.status_code == 400 and admin.get(f"/api/admin/users/{bob_id}").json()["user"]["max_concurrent_games"] == 3,
            zero.text[:200])

    # ---- the command line, on the server's own database
    runs = [h.server.cli(*args) for args in (("set-trust", bob.handle, "trusted"),
                                              ("set-limits", bob.handle, "--seats", "6"),
                                              ("set-caps", bob.handle, "run_probes=off"))]
    view = admin.get(f"/api/admin/users/{bob_id}").json().get("user", {})
    r.check("admin_cli set-trust, set-limits and set-caps change the account, seen through the API",
            all(x.returncode == 0 for x in runs) and view.get("trust_tier") == "trusted"
            and view.get("max_seats_per_game") == 6 and view.get("caps") == {"run_probes": False},
            " | ".join((x.stdout + x.stderr).strip()[-150:] for x in runs))
    listing = h.server.cli("audit", "--object-id", bob_id, "--action", "user.updated", "--json")
    try:
        entries = json.loads(listing.stdout).get("entries", [])
    except ValueError:
        entries = []
    r.check("admin_cli audit lists the command line's three changes and the administrator's two",
            [e["actor"] for e in entries[:3]] == ["admin_cli"] * 3 and len(entries) == 5, listing.stdout[-300:])
    by_cli = admin.get("/api/admin/audit", params={"actor": "admin_cli", "object_id": bob_id,
                                                   "action": "user.updated"}).json().get("entries", [])
    r.check("the audit log's actor filter finds the command line's changes by its name",
            [e["id"] for e in by_cli] == [e["id"] for e in entries[:3]],
            f"{[e['id'] for e in by_cli]} against {[e['id'] for e in entries[:3]]}")

    # ---- probation and a suspension
    admin.patch(f"/api/admin/users/{bob_id}", json={"status": "probation"})
    pub = bob.put("/api/auth/me", json={"field_visibility": {"bio": "global"}})
    r.check("on probation, a member cannot make a field public (403)", pub.status_code == 403, pub.text[:200])
    no_reason = admin.patch(f"/api/admin/users/{bob_id}", json={"status": "suspended"})
    sus = admin.patch(f"/api/admin/users/{bob_id}", json={"status": "suspended", "reason": f"QA {h.tag}"})
    r.check("a suspension needs its reason (400), and signs the member out",
            no_reason.status_code == 400 and sus.status_code == 200
            and bob.get("/api/auth/me").json().get("authenticated") is False, f"HTTP {no_reason.status_code} / {sus.status_code}")
    back = admin.patch(f"/api/admin/users/{bob_id}", json={"status": "active", "caps": {"run_probes": None},
                                                           "max_seats_per_game": 0})
    r.check("reinstated, the member signs in again", back.status_code == 200 and bob.login().status_code == 200)
    r.data["profiles"] = {"alice_display_name": name, "alice_avatar": avatar, "bob_id": bob_id}


#: The registry file a server of CITAR before 0.1.6 would have, written into the instance's ``config/`` before it
#: starts (P3.2.8's import): a host, an online API whose key variable is not set, a dry run, an electricity plan.
#: Nothing in it dials anything: the host has no provider, the API's key is missing.
QA_REGISTRY = {
    "format": "citar-servers", "version": 1, "currency": "EUR", "currency_symbol": "€", "host_server_id": "sv_qa_host",
    "electricity_plans": [{"id": "ep_qa", "name": "QA household", "periods": [
        {"from": "2000-01-01", "type": "flat", "rate_kwh": 0.3, "fixed_monthly": 0, "fee_allocation": "none"}]}],
    "servers": [
        {"id": "sv_qa_host", "name": "QA host", "kind": "owned", "connection": {"provider": "none", "max_parallel": 2},
         "power": {"idle_w": 20, "cpu_max_w": 60, "gpu_max_w": 0},
         "costs": [{"from": "2000-01-01", "electricity_plan_id": "ep_qa"}]},
        {"id": "sv_qa_api", "name": "QA API", "kind": "api",
         "connection": {"provider": "anthropic", "max_parallel": 3, "key": {"backend": "env", "env": "CITAR_QA_NO_SUCH_KEY"}},
         "models": [{"id": "m_qa_opus", "key": "claude-opus-5"}]},
        {"id": "sv_qa_dry", "name": "QA dry run", "kind": "test", "connection": {"provider": "dryrun", "max_parallel": 4},
         "models": [{"id": "m_qa_dry", "key": "dry-run"}]},
    ]}


def write_registry(state: Path) -> Path:
    """Put ``QA_REGISTRY`` where the instance's server reads its registry file once (``config/servers.json``)."""
    path = state / "config" / "servers.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(QA_REGISTRY, indent=1), encoding="utf-8")
    return path


#: The CITAR helper binary the harness runs (``--helper-bin``); None: the one ``cargo xtask develop`` built.
HELPER_BIN: Optional[str] = None


def helper_binary() -> str:
    """The CITAR helper to run: ``--helper-bin``, else ``citar-helper`` beside the extension in ``CITAR_EXT_DIR``
    (``cargo xtask develop`` builds it with its development guard: loopback servers only, an explicit ``--home``)."""
    if HELPER_BIN:
        return HELPER_BIN
    folder = os.environ.get("CITAR_EXT_DIR") or ""
    for name in ("citar-helper.exe", "citar-helper"):
        path = Path(folder) / name if folder else None
        if path is not None and path.is_file():
            return str(path)
    raise HarnessError("no CITAR helper: run cargo xtask develop (it builds citar-helper into CITAR_EXT_DIR), or "
                       "give --helper-bin")


class Helper:
    """A real CITAR helper (the Rust binary, ``helper_binary``) as a child, connected to the instance with a worker
    token and serving from a fake endpoint; its home ``<dir>/helper-<n>`` (``--home``, and ``CITAR_DATA_DIR`` with it,
    so it never reads or writes the owner's own helper settings) and its log in the harness's directory."""

    def __init__(self, h: "Harness", token: str, endpoint, name: str, *,
                 hardware: Optional[Path] = None, max_concurrent: int = 0, machine: Optional[str] = None,
                 provider: str = "openai_compatible"):
        """``endpoint``: a fake endpoint, or the address of a real one (the ``real`` part's LM Studio); ``hardware``:
        the hardware file it reports (``--hardware``), instead of none (``--no-hardware``); ``max_concurrent``: its
        own slot count (``--max-concurrent``), else the helper's default, one; ``machine``: the machine it runs on,
        whose home (``<dir>/helper-home-<machine>``, its machine key with it) a helper started again for that machine
        keeps, as a real machine keeps its key across restarts (else a home of its own: a new key); ``provider``: what
        serves its models (``lmstudio`` reads which are loaded from LM Studio's own list; an ``openai_compatible``
        endpoint's every listed model counts as loaded)."""
        state = h.server.state
        n = len(list(state.glob("helper-*.log"))) + 1
        self.log = state / f"helper-{n}.log"
        self.home = state / (f"helper-home-{machine}" if machine else f"helper-{n}")
        self.home.mkdir(parents=True, exist_ok=True)
        out = open(self.log, "w", encoding="utf-8")
        env = {k: v for k, v in os.environ.items() if not k.startswith("CITAR_WORKER_")}
        env["CITAR_DATA_DIR"] = str(self.home)
        extra = ["--hardware", str(hardware)] if hardware is not None else ["--no-hardware"]
        if max_concurrent:
            extra += ["--max-concurrent", str(max_concurrent)]
        self.proc = subprocess.Popen(
            [helper_binary(), "--home", str(self.home), "--server", h.server.base, f"--token={token}",
             "--base-url", endpoint if isinstance(endpoint, str) else endpoint.url, "--provider", provider,
             "--name", name, *extra],
            cwd=str(state), stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, env=env)
        out.close()

    def exited(self, timeout: float) -> bool:
        """Whether the helper stopped by itself within ``timeout`` seconds."""
        try:
            self.proc.wait(timeout=timeout)
            return True
        except subprocess.TimeoutExpired:
            return False

    def stop(self) -> None:
        """Stop it if it still runs."""
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def text(self) -> str:
        """What it logged."""
        return self.log.read_text(encoding="utf-8", errors="replace") if self.log.exists() else ""


def _busy_lines(h: "Harness") -> list:
    """The server's log lines saying a helper refused a request as busy (the prompt scheduler's warning): a protocol 2
    helper answers ``busy`` only past the slots it acknowledged, which the scheduler never sends past."""
    log = h.server.log.read_text(encoding="utf-8", errors="replace") if h.server.log.exists() else ""
    return [line for line in log.splitlines() if "refused a request as busy" in line]


def _online(who: "Account", sid: str) -> Optional[bool]:
    """Whether a helper machine's helper is connected, as its owner's Servers page reads it."""
    r = who.get(f"/api/pool/servers/{sid}/workers")
    return r.json().get("online") if r.status_code == 200 else None


def _until_offline(who: "Account", sid: str, limit: float = 10.0) -> Optional[float]:
    """Seconds until the helper of ``sid`` is gone (polled every 50 ms), or None past ``limit``."""
    t0 = time.perf_counter()
    while time.perf_counter() - t0 < limit:
        if not _online(who, sid):
            return round(time.perf_counter() - t0, 3)
        time.sleep(0.05)
    return None


def _until_unlisted(admin: "Account", sid: str, limit: float = 10.0) -> Optional[float]:
    """Seconds until no helper connection of ``sid`` is left on the server, as an administrator's ``/api/status``
    lists them (a deleted machine has no page of its own to ask), or None past ``limit``."""
    t0 = time.perf_counter()
    while time.perf_counter() - t0 < limit:
        r = admin.get("/api/status")
        if r.status_code == 200 and sid not in {w.get("server_id") for w in r.json().get("workers") or []}:
            return round(time.perf_counter() - t0, 3)
        time.sleep(0.05)
    return None


def _accounts_machines(h: Harness) -> None:
    """3-05's checks (design/phase3-features.md P3.2.8): the registry file imported once, under its ids, when the first
    administrator was made, and renamed; a member's machine CITAR would dial refused unless it is the dry run; a
    member's dry-run machine and helper machine (a real helper behind the scripted endpoint) playing her games; the
    host-touching routes and the key routes 404 on a helper machine with nothing dialled; another member's machine 404,
    and named in no listing of another member's (the lobby's availability, the quiet hours); a machine body of the
    wrong shape 400 and the site's electricity plan 403 for a member; the model probe an administrator's; a revoked
    token, a disabled machine and a deleted one dropping the helper within a second."""
    r = h.report
    admin, alice, bob = (h.accounts[k] for k in ("admin", "alice", "bob"))
    trap = h.endpoint
    state = h.server.state

    # ---- the import
    log = h.server.log.read_text(encoding="utf-8", errors="replace")
    r.check("the boot imported nothing with no administrator yet, and said why",
            "not imported yet: there is no administrator" in log)
    renamed = sorted(p.name for p in (state / "config").glob("servers.json.imported-*"))
    r.check("the registry file was imported when the first administrator was made, and renamed (never deleted)",
            not (state / "config" / "servers.json").exists() and len(renamed) == 1, f"{renamed}")
    listing = admin.get("/api/servers").json() if admin.get("/api/servers").status_code == 200 else {}
    rows = {sv["id"]: sv for sv in listing.get("servers", [])}
    r.check("every machine of the file is a row under its own id, the host the row with is_host, slots from "
            "max_parallel, the plans and the currency carried over",
            {"sv_qa_host", "sv_qa_api", "sv_qa_dry"} <= set(rows) and rows["sv_qa_host"].get("is_host") is True
            and rows["sv_qa_host"].get("reach") == "direct" and rows["sv_qa_api"].get("max_concurrent") == 3
            and listing.get("host_server_id") == "sv_qa_host" and listing.get("currency") == "EUR"
            and [p["id"] for p in listing.get("electricity_plans", [])] == ["ep_qa"],
            json.dumps({k: {f: v.get(f) for f in ("is_host", "reach", "max_concurrent")} for k, v in rows.items()}))
    audit = admin.get("/api/admin/audit", params={"action": "migration.registry_imported"}).json().get("entries", [])
    r.check("one audit row for the import", len(audit) == 1, f"{len(audit)} rows")
    r.check("a member lists only their own machines (none yet), not the site's",
            alice.get("/api/servers").json().get("servers") == [])

    # ---- a member's machines CITAR would dial
    before = len(trap.connections)
    dial = alice.post("/api/servers", json={"name": f"QA alice LAN {h.tag}", "kind": "owned",
                                            "connection": {"provider": "lmstudio", "base_url": trap.url},
                                            "models": [{"key": trap.MODEL}]})
    probe = alice.post("/api/llm/models", json={"provider": "openai_compatible", "base_url": trap.url,
                                                "api_key_env": "PATH"})
    r.check("a member's machine the server would dial is refused (403, the reason given), and so is the model probe",
            dial.status_code == 403 and "administrator" in dial.text and probe.status_code == 403,
            f"save HTTP {dial.status_code}, probe HTTP {probe.status_code}")
    time.sleep(0.5)
    r.check("and nothing connected to the address they named", len(trap.connections) == before,
            connections=len(trap.connections) - before)
    env_key = alice.post("/api/servers", json={"name": f"QA alice env {h.tag}", "kind": "test",
                                               "connection": {"provider": "dryrun", "key": {"backend": "env", "env": "PATH"}}})
    r.check("a member's machine may not name one of the server's environment variables as its key (403)",
            env_key.status_code == 403, f"HTTP {env_key.status_code}")
    dry = alice.post("/api/servers", json={"name": f"QA alice dry run {h.tag}", "kind": "test",
                                           "connection": {"provider": "dryrun", "dry_run_delay": 0.2},
                                           "models": [{"key": "dry-run"}]})
    dry_id = dry.json().get("id") if dry.status_code == 200 else ""
    r.check("a member's direct dry-run machine is theirs to make (it dials nothing)",
            dry.status_code == 200 and dry.json().get("reach") == "direct", dry.text[:200])
    r.data["machines"] = {"alice_dry": dry_id}
    game = alice.post("/api/games", json={"name": f"QA alice dry machine {h.tag}", "config": CONFIG,
                                          "seats": [{"type": "llm", "llm": {"server_id": dry_id, "model": "dry-run"}},
                                                    {"type": "bot"}]})
    gid = game.json().get("id") if game.status_code == 200 else ""
    turned = wait_until(lambda: (alice.get(f"/api/games/{gid}").json().get("turn") or 0) >= 2, 90, 1.0) if gid else False
    r.check("her game plays a seat on her dry-run machine", bool(turned), f"HTTP {game.status_code} {game.text[:150]}")
    if gid:
        alice.delete(f"/api/games/{gid}")         # closed: her game limit has room for the next one
    r.check("another member cannot see or seat it: 404 on its configuration, 403 for a seat",
            bob.get(f"/api/servers/{dry_id}/rates").status_code == 404
            and bob.post("/api/servers", json={"id": dry_id, "name": "mine now", "kind": "test",
                                               "connection": {"provider": "dryrun"}}).status_code == 404
            and bob.post("/api/games", json={"name": "x", "config": CONFIG, "seats": [
                {"type": "llm", "llm": {"server_id": dry_id, "model": "dry-run"}}, {"type": "bot"}]}).status_code == 403)
    site = bob.post("/api/games", json={"name": "x", "config": CONFIG, "seats": [
        {"type": "llm", "llm": {"server_id": "sv_qa_dry", "model": "dry-run"}}, {"type": "bot"}]})
    r.check("the site's machines are an administrator's until shared: a member's seat on its dry run is 403, the "
            "machine not named", site.status_code == 403 and "QA dry run" not in site.text, site.text[:200])

    # ---- a helper machine behind the scripted endpoint, with a real helper
    scripted = FakeEndpoint(delay=0.05)
    helpers: list = []
    try:
        made = alice.post("/api/pool/servers", json={"config": {"name": f"QA alice helper {h.tag}", "kind": "owned",
                                                                "connection": {"provider": "openai_compatible"}},
                                                     "reach": "worker"})
        hid = made.json().get("id") if made.status_code == 200 else ""
        r.data["machines"]["alice_helper"] = hid

        def token() -> str:
            t = alice.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa"})
            return t.json().get("token", "") if t.status_code == 200 else ""

        def connect() -> Optional[Helper]:
            helper = Helper(h, token(), scripted, f"qa-helper-{len(helpers) + 1}", machine="accounts")
            helpers.append(helper)
            return helper if wait_until(lambda: _online(alice, hid), 60, 0.25) else None

        first = connect()
        info = alice.get(f"/api/pool/servers/{hid}/workers").json() if hid else {}
        r.check("a member's helper machine connects with a real helper and its hello names the endpoint's model",
                first is not None and scripted.MODEL in [m.get("key") for m in (info.get("worker") or {}).get("models", [])],
                f"made HTTP {made.status_code}; worker {json.dumps(info.get('worker'))[:200]}")

        # the routes that dial or drive LM Studio on the host: 404 for a helper machine, with nothing sent
        sent = len(scripted.requests), len(scripted.connections), len(trap.connections)
        codes = {}
        for method, path, body in (("post", "detect", None), ("get", "state", None),
                                   ("post", "load", {"model_id": scripted.MODEL}),
                                   ("post", "unload", {"model_id": scripted.MODEL}), ("get", "loading", None),
                                   ("post", "loaded-config", {"model_id": scripted.MODEL}),
                                   ("post", "estimate", {"model_id": scripted.MODEL}), ("post", "collect", None)):
            call = getattr(alice, method)
            got = call(f"/api/servers/{hid}/{path}", **({"json": body} if body is not None else {}))
            codes[path] = got.status_code
        time.sleep(0.5)
        r.check("detect, state, load, unload, loading, loaded-config, estimate and collect are 404 on her helper "
                "machine", set(codes.values()) == {404}, json.dumps(codes))
        r.check("and nothing was asked of any model server meanwhile",
                (len(scripted.requests), len(scripted.connections), len(trap.connections)) == sent,
                f"before {sent}, after {(len(scripted.requests), len(scripted.connections), len(trap.connections))}")
        host_wide = {path: alice.request(method, path, **kw).status_code for method, path, kw in (
            ("GET", "/api/lmstudio/devices", {}), ("PUT", "/api/servers-settings", {"json": {"currency": "XXX"}}),
            ("POST", "/api/keys/unlock", {"json": {"passphrase": "x"}}), ("POST", "/api/keys/lock", {}),
            ("POST", "/api/electricity-plans", {"json": {"name": "mine"}}))}
        r.check("the host-wide routes are 404 for a member", set(host_wide.values()) == {404}, json.dumps(host_wide))
        r.check("another member gets 404 on her helper machine's configuration",
                bob.get(f"/api/servers/{hid}/rates").status_code == 404
                and bob.get(f"/api/pool/servers/{hid}/workers").status_code == 404)
        keys = (alice.post(f"/api/servers/{hid}/key", json={"backend": "file", "key": "sk-qa-not-stored"}).status_code,
                alice.delete(f"/api/servers/{hid}/key").status_code)
        r.check("the key routes are 404 on her helper machine (the server holds no key of a helper machine's)",
                keys == (404, 404), f"{keys}")
        bad = (alice.post("/api/servers", json={"name": f"QA bad {h.tag}", "connection": "lmstudio"}).status_code,
               alice.post("/api/pool/servers", json={"config": {"name": f"QA bad {h.tag}", "models": [1]}}).status_code,
               alice.put(f"/api/pool/servers/{hid}", json={"config": {"name": "QA bad", "costs": "x"}}).status_code,
               alice.put(f"/api/pool/servers/{hid}/costing",
                         json={"costs": [{"from": "2000-01-01", "electricity_plan_id": "ep_qa"}]}).status_code)
        names = {sv.get("name") for sv in admin.get("/api/servers").json().get("servers", [])}
        r.check("a machine of the wrong shape is a 400 on every route that saves one, and the site's electricity plan "
                "is no member's to price a machine with (403); nothing was written",
                bad == (400, 400, 400, 403) and f"QA bad {h.tag}" not in names and "QA bad" not in names, f"{bad}")
        sees = {}
        for who, account in (("bob", bob), ("admin", admin)):
            got = account.get("/api/pool/availability").json()
            status = account.get("/api/benchmarks/status").json()
            sees[who] = ({e.get("id") for e in got.get("usable", []) + got.get("blocked", [])}
                         | {e.get("id") for e in status.get("restricted", [])})
        site = {hid, "sv_qa_host", "sv_qa_api", "sv_qa_dry"}
        r.check("another member's lobby availability names neither her helper machine nor the site's unshared "
                "machines; an administrator's names them all", not (sees["bob"] & site) and site <= sees["admin"],
                json.dumps({k: sorted(v) for k, v in sees.items()}))

        # her game plays through the helper and the scripted policy
        served = len(scripted.completions())
        g2 = alice.post("/api/games", json={"name": f"QA alice helper game {h.tag}", "config": CONFIG,
                                            "seats": [{"type": "llm", "llm": {"server_id": hid, "model": scripted.MODEL}},
                                                      {"type": "bot"}]})
        gid2 = g2.json().get("id") if g2.status_code == 200 else ""
        if gid2:
            # the civilizations meet, so the scripted policy has someone to open a trade with
            alice.post(f"/api/games/{gid2}/debug/meet_all")
        played = wait_until(lambda: (alice.get(f"/api/games/{gid2}").json().get("turn") or 0) >= 3, 120, 1.0) \
            if gid2 else False
        if gid2:
            alice.post(f"/api/games/{gid2}/control", json={"paused": True})
        mine = scripted.completions()[served:]
        ended = sum(1 for q in mine if "end_turn" in q.get("calls", []))
        called = {c for q in mine for c in q.get("calls", [])}
        r.check("her game plays two turns through the helper: the scripted endpoint answered with tool calls (a city "
                "founded, a trade opened with the civilization it met) and ended each turn",
                bool(played) and ended >= 2 and all(q.get("tools", 0) > 0 for q in mine)
                and {"found_city", "open_negotiation", "end_turn"} <= called,
                f"HTTP {g2.status_code}; {len(mine)} completions, {ended} end_turn; {sorted(called)}",
                completions=len(mine), end_turns=ended, trades=scripted.trades_opened)
        r.data["scripted_endpoint"] = {"completions": len(mine), "end_turns": ended, "trades": scripted.trades_opened,
                                       "calls": sorted({c for q in mine for c in q.get("calls", [])})}
        if gid2:
            alice.delete(f"/api/games/{gid2}")

        # a revoked token, a disabled machine and a deleted one drop the helper within a second
        tokens = alice.get(f"/api/pool/servers/{hid}/workers").json().get("tokens", [])
        live = next((t["id"] for t in tokens if not t["revoked"]), "")
        revoked = alice.delete(f"/api/pool/servers/{hid}/workers/tokens/{live}")
        gone = _until_offline(alice, hid)
        r.check("revoking the helper's token drops it within a second, and the helper stops",
                revoked.status_code == 200 and gone is not None and gone <= 1.0 and first is not None
                and first.exited(15), f"offline after {gone} s", seconds=gone)
        second = connect()
        disabled = alice.put(f"/api/pool/servers/{hid}", json={"enabled": False})
        gone = _until_offline(alice, hid)
        refused = Helper(h, token(), scripted, "qa-helper-disabled", machine="accounts")
        helpers.append(refused)
        r.check("disabling the machine drops its helper within a second, and a helper's new hello is refused",
                second is not None and disabled.status_code == 200 and gone is not None and gone <= 1.0
                and second.exited(15) and refused.exited(30) and "refused this helper's token" in refused.text(),
                f"offline after {gone} s", seconds=gone)
        alice.put(f"/api/pool/servers/{hid}", json={"enabled": True})
        third = connect()
        before = admin.get("/api/status")
        listed = third is not None and before.status_code == 200 and hid in {      # connected before the delete
            w.get("server_id") for w in before.json().get("workers") or []}
        deleted = alice.delete(f"/api/pool/servers/{hid}")
        gone = _until_unlisted(admin, hid) if deleted.status_code == 200 else None
        r.check("deleting the machine drops its helper within a second and leaves no connection",
                listed and deleted.status_code == 200 and gone is not None and gone <= 1.0 and third.exited(15)
                and alice.get(f"/api/pool/servers/{hid}").status_code == 404,
                f"HTTP {deleted.status_code}; no connection left after {gone} s", seconds=gone)
    finally:
        for helper in helpers:
            helper.stop()
        scripted.close()


def _accounts_servers_page(h: Harness) -> None:
    """3-06's checks (design/phase3-features.md P3.2.8, the one Servers page): what the page reads and writes, over
    the routes it calls. A member's page lists her machine the server dials (her dry run, from 3-05's checks) and her
    helper machine (a real helper behind the scripted endpoint) side by side, hers to manage, and nothing of the
    site's; the reads and saves its dialogs make on them answer; sharing one through a group from the page shows it to
    another member, his to use and not to manage, through its grant (the group hands out no share link); an administrator's
    page lists the site's machines and the members' alike, each his to manage. With ``--keep`` the helper stays
    connected for the browser checklist (A4), its pid in the evidence."""
    r = h.report
    admin, alice, bob = (h.accounts[k] for k in ("admin", "alice", "bob"))
    machines = r.data.setdefault("machines", {})
    dry_id = machines.get("alice_dry", "")
    scripted = FakeEndpoint(delay=0.05)
    helper: Optional[Helper] = None
    try:
        made = alice.post("/api/pool/servers", json={"config": {"name": f"QA alice page helper {h.tag}", "kind": "owned",
                                                                "connection": {"provider": "lmstudio"}},
                                                     "reach": "worker"})
        hid = made.json().get("id") if made.status_code == 200 else ""
        machines["alice_page_helper"] = hid
        t = alice.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa page"}) if hid else None
        if t is not None and t.status_code == 200:
            helper = Helper(h, t.json().get("token", ""), scripted, "qa-page-helper")
        online = bool(helper and wait_until(lambda: _online(alice, hid), 60, 0.25))

        # ---- her page: both of her machines, side by side, hers to manage; nothing of the site's
        pool = alice.get("/api/pool/servers").json().get("servers", [])
        listed = {s.get("id"): s for s in pool}
        regs = {s.get("id"): s for s in alice.get("/api/servers").json().get("servers", [])}
        site = {"sv_qa_host", "sv_qa_api", "sv_qa_dry"}
        live = [m.get("key") for m in (listed.get(hid) or {}).get("live_models") or []]
        r.check("a member's Servers page lists her machine the server dials and her helper machine side by side, both "
                "hers to manage (with their configuration), and nothing of the site's",
                {dry_id, hid} <= set(listed) and set(regs) == {dry_id, hid} and not (site & set(listed))
                and listed[dry_id].get("reach") == "direct" and listed[hid].get("reach") == "worker"
                and all(listed[x].get("can_manage") and listed[x].get("config") for x in (dry_id, hid)),
                json.dumps({k: {f: v.get(f) for f in ("reach", "can_manage")} for k, v in listed.items()}))
        r.check("her helper machine's card shows its helper online with the model it reported", online and scripted.MODEL in live,
                f"online {online}; live models {live}")

        # ---- the reads and saves her page's dialogs make on her machines
        codes = {"rates (direct)": alice.get(f"/api/servers/{dry_id}/rates").status_code,
                 "rates (helper)": alice.get(f"/api/servers/{hid}/rates").status_code,
                 "helper and tokens": alice.get(f"/api/pool/servers/{hid}/workers").status_code,
                 "test connection (her dry run)": alice.post(f"/api/servers/{dry_id}/detect").status_code}
        tested = alice.post(f"/api/pool/servers/{hid}/test")
        codes["test (through the helper)"] = tested.status_code
        view = dict(regs.get(hid) or {})
        view["connection"] = dict(view.get("connection") or {}, max_parallel=2)
        view["description"] = "set from the Servers page's connection dialog"
        saved = alice.post("/api/servers", json=view)
        codes["connection & models: save"] = saved.status_code
        quiet = alice.put(f"/api/pool/servers/{hid}/quiet-hours", json={"restricted_hours": {
            "enabled": False, "windows": [{"days": [5, 6], "start": "23:00", "end": "07:00"}], "grace_minutes": 10}})
        codes["quiet hours: save"] = quiet.status_code
        after = alice.get(f"/api/pool/servers/{hid}").json() if saved.status_code == 200 else {}
        r.check("the reads and saves of her page's dialogs answer on her machines (rates, the helper's tokens, her dry run's "
                "test, a test through the helper, the connection dialog's save and the quiet hours'), and the saved "
                "helper machine stays a helper machine with its new slots",
                set(codes.values()) == {200} and tested.json().get("ok") is True
                and saved.json().get("reach") == "worker" and after.get("max_concurrent") == 2
                and (after.get("restricted_hours") or {}).get("windows", [{}])[0].get("start") == "23:00",
                json.dumps({**codes, "test": tested.json() if tested.status_code == 200 else tested.text[:120]})[:600])

        # ---- sharing from the page: a group, her helper machine in it, every member given access
        group = alice.post("/api/pool/groups", json={"name": f"QA page group {h.tag}", "visibility": "link"})
        gid = group.json().get("id") if group.status_code == 200 else ""
        put = alice.put(f"/api/pool/servers/{hid}", json={"group_id": gid})
        # games and pooled prompts (the test is one, package 3-09)
        grant = alice.post(f"/api/pool/groups/{gid}/grants", json={"subject_type": "everyone",
                                                                    "purposes": ["game", "pooled"]})
        mine_groups = {g.get("id"): g for g in alice.get("/api/pool/groups").json().get("groups", [])}
        mine_group = mine_groups.get(gid) or {}
        his = {s.get("id"): s for s in bob.get("/api/pool/servers").json().get("servers", [])}
        shared = his.get(hid) or {}
        his_regs = {s.get("id") for s in bob.get("/api/servers").json().get("servers", [])}
        refused = {"rates": bob.get(f"/api/servers/{hid}/rates").status_code,
                   "tokens": bob.get(f"/api/pool/servers/{hid}/workers").status_code,
                   "quiet hours": bob.put(f"/api/pool/servers/{hid}/quiet-hours",
                                          json={"restricted_hours": {"enabled": True}}).status_code}
        his_test = bob.post(f"/api/pool/servers/{hid}/test")
        # he may see it, so a route of hers says 403 where it reads the machine itself and 404 where it reads her
        # registry (admin_api's routes say nothing of a machine one does not manage)
        r.check("sharing her helper machine through a group from the page shows it on another member's page, his to use "
                "(a test through it answers) and not to manage (no configuration; its dialogs' routes refuse him)",
                (group.status_code, put.status_code, grant.status_code) == (200, 200, 200)
                and shared.get("group_id") == gid and shared.get("can_manage") is False and "config" not in shared
                and hid not in his_regs and refused == {"rates": 404, "tokens": 403, "quiet hours": 403}
                and his_test.status_code == 200 and his_test.json().get("ok") is True,
                json.dumps({"made": [group.status_code, put.status_code, grant.status_code], "his": {
                    f: shared.get(f) for f in ("group_id", "can_manage")}, "refused": refused,
                    "test": his_test.text[:200]}))
        r.check("a group's listing hands its manager no share link: its grants share it (bob's above), and no route "
                "takes a group's key, so a link would open nothing",
                mine_group.get("visibility") == "link" and "share_url" not in mine_group
                and bool(mine_group.get("grants")),
                json.dumps({k: mine_group.get(k) for k in ("visibility", "share_url")}))

        # ---- an administrator's page: the site's machines and the members' alike, each his to manage
        his_pool = {s.get("id"): s for s in admin.get("/api/pool/servers").json().get("servers", [])}
        admin_regs = {s.get("id"): s for s in admin.get("/api/servers").json().get("servers", [])}
        every = site | {dry_id, hid}
        r.check("an administrator's Servers page lists the site's machines and the members' alike, each his to manage, "
                "the host marked as the host and the API as the site's",
                every <= set(his_pool) and every <= set(admin_regs) and all(his_pool[x].get("can_manage") for x in every)
                and admin_regs["sv_qa_host"].get("is_host") is True and admin_regs["sv_qa_api"].get("server_resource") is True
                and admin_regs[hid].get("reach") == "worker",
                json.dumps(sorted(set(his_pool))))
        r.data["servers_page"] = {"member": sorted(listed), "member_shared": sorted(his), "admin": sorted(his_pool),
                                  "group": gid}
    finally:
        if helper is not None and h.keep and helper.proc.poll() is None:
            # left connected for the browser checklist; the scripted endpoint goes with the harness
            r.data["kept_helper"] = {"pid": helper.proc.pid, "machine": machines.get("alice_page_helper")}
            r.note(f"the page's helper is kept connected (pid {helper.proc.pid}); stop it with the server")
        else:
            if helper is not None:
                helper.stop()
            scripted.close()


def part_queue(h: Harness) -> None:
    """The ``queue`` part: 3-07's lease and cancel checks (design/phase3-features.md P3.3.3), 3-08's leases at
    registration, a restart's restored game and a benchmark's drain, 3-09's pooled prompts (reports' analyses and probe
    cases beside a lobby game, the Queue page)."""
    _queue_leases_and_drain(h)
    _queue_restored_game_and_benchmark_drain(h)
    _queue_pooled_prompts(h)


def _queue_leases_and_drain(h: Harness) -> None:
    """3-07's checks, on a member's helper machine with one slot (a real helper behind the scripted endpoint, 0.4 s a
    completion): two lobby games with a model seat each play through the prompt scheduler, their requests never two at
    once and nothing refused as busy, both games' leases on the machine; then the Servers page's test, given up after
    2 s while the endpoint would take 8 s over it, is cancelled: the helper is sent ``cancel``, stops the generation
    (the endpoint sees its client go) and acknowledges it, which frees the slot with nothing draining
    (design/phase4-helper.md P4.5.3), so a game's model seat resumed meanwhile goes at once rather than wait out the
    8 s. While that test waits, a second of the same machine from her other tab is refused at once (one test of a
    machine per person)."""
    r = h.report
    admin, alice = h.accounts["admin"], h.accounts["alice"]
    scripted = FakeEndpoint(delay=0.4)
    scripted.slow = {"Reply with exactly: CITAR OK": 8.0}
    helper: Optional[Helper] = None
    games: list = []

    def machine_status(hid: str) -> dict:
        got = admin.get("/api/status")
        rows = (got.json().get("machines") or []) if got.status_code == 200 else []
        return next((m for m in rows if m.get("id") == hid), {})

    def turn(gid: str) -> int:
        got = alice.get(f"/api/games/{gid}")
        return (got.json().get("turn") or 0) if got.status_code == 200 else 0

    def queued(gid: str) -> float:
        got = alice.get(f"/api/games/{gid}/metrics")
        rows = (got.json().get("turns") or []) if got.status_code == 200 else []
        return sum(row.get("queue_s") or 0.0 for row in rows if row.get("controller") == "llm")

    try:
        made = alice.post("/api/pool/servers", json={"config": {"name": f"QA queue helper {h.tag}", "kind": "owned",
                                                                "connection": {"provider": "openai_compatible",
                                                                               "max_parallel": 1}},
                                                     "reach": "worker"})
        hid = made.json().get("id") if made.status_code == 200 else ""
        t = alice.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa queue"}) if hid else None
        if t is not None and t.status_code == 200:
            helper = Helper(h, t.json().get("token", ""), scripted, "qa-queue-helper")
        online = bool(helper and wait_until(lambda: _online(alice, hid), 60, 0.25))
        r.check("a member's helper machine with one slot, its real helper connected behind the scripted endpoint",
                online and made.json().get("max_concurrent") == 1, f"HTTP {made.status_code}; online {online}")
        r.data["queue"] = {"machine": hid}

        # ---- two lobby games, a model seat each, on its one slot
        for i in range(2):
            g = alice.post("/api/games", json={"name": f"QA queue game {i + 1} {h.tag}",
                                               "config": {**CONFIG, "seed": 3307 + i},
                                               "seats": [{"type": "llm", "llm": {"server_id": hid,
                                                                                  "model": scripted.MODEL}},
                                                         {"type": "bot"}]})
            if g.status_code == 200:
                games.append(g.json()["id"])
                alice.post(f"/api/games/{games[-1]}/debug/meet_all")
        played = len(games) == 2 and wait_until(lambda: all(turn(g) >= 4 for g in games), 240, 1.0)
        for g in games:
            alice.post(f"/api/games/{g}/control", json={"paused": True})
        time.sleep(1.5)                                     # a request in flight at the pause finishes
        served = scripted.completions()
        spans = sorted((q["t"], q["t"] + q["seconds"]) for q in served)
        overlaps = sum(1 for a, b in zip(spans, spans[1:]) if b[0] < a[1] - 0.05)
        status = machine_status(hid)
        counters = status.get("counters") or {}
        r.check("two lobby games on the helper machine each played three rounds through its one slot: never two "
                "completions at once at the endpoint",
                bool(played) and len(served) >= 12 and overlaps == 0,
                f"{len(served)} completions, {overlaps} overlapping; turns {[turn(g) for g in games]}",
                completions=len(served), overlaps=overlaps)
        busy_lines = _busy_lines(h)
        leases = [lease for lease in status.get("leases") or [] if lease.get("kind") == "game"]
        r.check("the helper refused nothing as busy, the scheduler saw no busy refusal and at most one request in "
                "flight, and it lists both games' leases on the machine",
                not busy_lines and counters.get("busy_refusals", 0) == 0 and status.get("max_in_flight") == 1
                and len(leases) == 2,
                json.dumps({"busy_lines": busy_lines[:3], "counters": counters,
                            "max_in_flight": status.get("max_in_flight"), "leases": len(leases)})[:500])

        # ---- the Servers page's test, given up while the helper generates it: its slot is freed at once
        before = len(scripted.completions())
        queued_before = queued(games[0]) if games else 0.0
        result: dict = {}

        def first_test():
            result["got"] = alice.post(f"/api/pool/servers/{hid}/test", params={"wait": 2})
            result["t1"] = time.time()
        t0 = time.time()
        runner = threading.Thread(target=first_test, daemon=True)
        runner.start()
        # her second tab tries the same machine while the first test waits for its reply: refused at once
        second_tab = Account(h.server, alice.handle, alice.role, alice.password)
        try:
            second_tab.login()
            running = wait_until(lambda: any(t.get("class") == "test" for t in machine_status(hid).get("tickets") or []),
                                 10, 0.1)
            t_dup = time.time()
            dup = second_tab.post(f"/api/pool/servers/{hid}/test", params={"wait": 2})
            dup_s = round(time.time() - t_dup, 2)
        finally:
            second_tab.http.close()
        dup_body = dup.json() if dup.status_code == 200 else {}
        r.check("a second test of the machine by the same member (another tab) while the first waits is refused at "
                "once, not queued behind it",
                bool(running) and dup_body.get("code") == "duplicate" and dup_s < 1.5,
                f"HTTP {dup.status_code} after {dup_s} s: {json.dumps(dup_body)[:200]}", seconds=dup_s)
        runner.join(30)
        tested = result.get("got")
        gave_up = round(result.get("t1", time.time()) - t0, 2)
        body = tested.json() if tested is not None and tested.status_code == 200 else {}
        r.check("the Servers page's test given up after 2 s is cancelled while the helper generates it",
                tested is not None and tested.status_code == 200 and body.get("cancelled") is True and gave_up < 6,
                f"HTTP {getattr(tested, 'status_code', None)} after {gave_up} s: {json.dumps(body)[:200]}",
                seconds=gave_up)
        given_up = result.get("t1", time.time())
        late = wait_until(lambda: next((q for q in scripted.completions()[before:] if q.get("delay") == 8.0), None),
                          10, 0.05)
        freed = wait_until(lambda: (lambda m: m if m.get("in_flight") == 0 else None)(machine_status(hid)), 5, 0.05)
        r.check("the helper stopped the cancelled generation: the endpoint saw its client go within a second of the "
                "test giving up, long before its 8 s, with no event after it but the one on its way",
                late is not None and late.get("stream") is True and late.get("gone") is not None
                and late["gone"] - given_up < 1.0 and late["seconds"] < 8.0
                and len([e for e in late.get("events") or [] if e > given_up]) <= 1,
                json.dumps({"gone_after_s": round(late["gone"] - given_up, 3) if late and late.get("gone") else None,
                            "seconds": (late or {}).get("seconds"), "tokens": (late or {}).get("tokens")}))
        r.check("its slot is free at once, freed by the helper's acknowledgement: nothing drains",
                bool(freed) and freed.get("draining") == 0 and freed.get("cancelling", 0) == 0,
                json.dumps({k: (freed or {}).get(k) for k in ("in_flight", "draining", "cancelling", "waiting")}))
        if games:
            alice.post(f"/api/games/{games[0]}/control", json={"paused": False})    # its model seat wants the slot
        finished = wait_until(lambda: len(scripted.completions()) > before + 1, 60, 0.25)
        for g in games:
            alice.post(f"/api/games/{g}/control", json={"paused": True})
        time.sleep(1.5)
        after = scripted.completions()[before:]
        others = [q for q in after if q is not late]
        first_other = min((q["t"] for q in others), default=None)
        waited = round((queued(games[0]) if games else 0.0) - queued_before, 2)
        r.check("the resumed game's model seat went at once: its next completion reached the endpoint without waiting "
                "for the 8 s the abandoned generation would have taken",
                bool(finished) and first_other is not None and first_other - given_up < 6.0 and waited < 2.0,
                f"the test gave up {round(given_up - t0, 2)} s after it began; the game's next completion "
                f"{round(first_other - given_up, 2) if first_other else None} s after that; its queue time +{waited} s",
                queue_s=waited)
        final = machine_status(hid)
        counters = final.get("counters") or {}
        busy_lines = _busy_lines(h)
        r.check("the helper acknowledged the cancel: its answer reached the scheduler as a late frame, nothing drained, "
                "no cancel went unacknowledged and nothing was refused as busy",
                counters.get("late_frames", 0) >= 1 and counters.get("draining", 0) == 0
                and counters.get("cancel_unacked", 0) == 0 and counters.get("drain_timeouts", 0) == 0
                and counters.get("busy_refusals", 0) == 0 and not busy_lines,
                json.dumps({"counters": counters, "busy_lines": busy_lines[:3]}))
        r.data["queue"].update({"completions": len(scripted.completions()), "overlaps": overlaps,
                                "test": body, "test_gave_up_s": gave_up, "duplicate_s": dup_s, "queue_wait_s": waited,
                                "counters": counters})
    finally:
        for g in games:
            alice.delete(f"/api/games/{g}")
        if helper is not None:
            helper.stop()
        scripted.close()


def _machine_status(admin: "Account", hid: str) -> dict:
    """A machine as an administrator's ``/api/status`` lists the prompt scheduler's (its leases, tickets, counters)."""
    got = admin.get("/api/status")
    rows = (got.json().get("machines") or []) if got.status_code == 200 else []
    return next((m for m in rows if m.get("id") == hid), {})


def _runs_job(who: "Account", run_id: str) -> dict:
    """A one-job run's job as its owner's Benchmarks page lists it."""
    got = who.get("/api/benchmarks/runs")
    runs = got.json() if got.status_code == 200 else []
    run = next((r for r in runs if isinstance(r, dict) and r.get("id") == run_id), {})
    return (run.get("jobs") or [{}])[0]


def _queue_restored_game_and_benchmark_drain(h: Harness) -> None:
    """3-08's checks (design/phase3-features.md P3.3.3, P3.3.5), on a member's helper machine with one slot (a real
    helper behind the scripted endpoint): a lobby game whose person moves first holds a lease on the machine from its
    creation, before its model seat is asked anything; the server is stopped as a crash would stop it and started
    again, and the game it brings back (``restore_live``) holds its lease again, before anything is asked of the model;
    a one-job benchmark on the machine waits for it. The member then tests the machine from the Servers page (the
    endpoint taking 10 s over it) and closes the game: the benchmark job starts at once, its lease cancels the running
    test, which the helper stops and acknowledges (protocol 2), and the job's first request waits only for that, so no
    pooled request overlaps a benchmark step; the wait is the job's ``drain_s`` in its result, short and not
    ``contaminated``."""
    r = h.report
    admin, alice = h.accounts["admin"], h.accounts["alice"]
    scripted = FakeEndpoint(delay=0.3)
    scripted.slow = {"Reply with exactly: CITAR OK": 10.0}
    helper: Optional[Helper] = None
    gid, run_id = "", ""
    try:
        made = alice.post("/api/pool/servers", json={"config": {"name": f"QA bench helper {h.tag}", "kind": "owned",
                                                                "connection": {"provider": "openai_compatible",
                                                                               "max_parallel": 1}},
                                                     "reach": "worker"})
        hid = made.json().get("id") if made.status_code == 200 else ""
        t = alice.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa bench"}) if hid else None
        if t is not None and t.status_code == 200:
            helper = Helper(h, t.json().get("token", ""), scripted, "qa-bench-helper")
        online = bool(helper and wait_until(lambda: _online(alice, hid), 60, 0.25))
        r.check("a second helper machine of the member's with one slot, its real helper connected",
                online, f"HTTP {made.status_code}; online {online}")
        data = r.data.setdefault("queue", {})
        data["restore"] = {"machine": hid}

        # ---- a lobby game holds its machine from its registration
        name = f"QA restored game {h.tag}"
        label = f"game “{name}”"
        g = alice.post("/api/games", json={"name": name, "config": {**CONFIG, "seed": 3311},
                                           "seats": [{"type": "human"},
                                                     {"type": "llm", "llm": {"server_id": hid,
                                                                             "model": scripted.MODEL}}]})
        gid = g.json().get("id", "") if g.status_code == 200 else ""

        def game_leases() -> list:
            return [x for x in _machine_status(admin, hid).get("leases") or [] if x.get("label") == label]
        made_leases = game_leases()
        r.check("a lobby game whose person moves first holds a lease on its model seat's machine from its creation, "
                "before the model is asked anything",
                bool(gid) and [x.get("kind") for x in made_leases] == ["game"] and not scripted.completions(),
                json.dumps({"game": gid, "leases": made_leases, "completions": len(scripted.completions())})[:400])

        # ---- the server stops as a crash would, and brings the game back
        t_restart = time.perf_counter()
        h.server.restart()
        for who in h.accounts.values():
            who.reconnect()
        back = alice.get(f"/api/games/{gid}") if gid else None
        restored_leases = game_leases()
        busy = next((m.get("busy_with") for m in (alice.get("/api/queue").json().get("machines") or [])
                     if m.get("server_id") == hid), None)
        r.check("the server, stopped as a crash would stop it and started again, brought the game back "
                "(restore_live) holding its lease again before the model was asked anything, and the member's Queue "
                "page shows the machine in use by it",
                back is not None and back.status_code == 200 and [x.get("kind") for x in restored_leases] == ["game"]
                and not scripted.completions() and busy == [label],
                json.dumps({"game": getattr(back, "status_code", None), "leases": restored_leases, "busy_with": busy,
                            "restart_s": round(time.perf_counter() - t_restart, 1)})[:400])
        online = wait_until(lambda: _online(alice, hid), 60, 0.25)
        r.check("the helper reconnected after the restart", bool(online), f"online {online}")

        # ---- a one-job benchmark on the machine waits for the restored game
        suite = {"name": f"QA drain {h.tag}", "mode": "sequential", "repeats": 1,
                 "servers": [{"server_id": hid, "models": [{"model": scripted.MODEL}]}],
                 "scenarios": [{"name": "QA duel", "map_size": "duel", "map_type": "pangaea", "seed": 3313,
                                "opponents": 1, "turn_limit": 2, "barbarians": "off", "max_turn_minutes": 5}]}
        started = alice.post("/api/benchmarks/runs", json={"suite": suite, "name": f"QA drain {h.tag}"})
        run_id = started.json().get("id", "") if started.status_code == 200 else ""
        waiting = wait_until(lambda: label in (_runs_job(alice, run_id).get("waiting") or ""), 30, 0.5)
        job = _runs_job(alice, run_id)
        r.check("a one-job benchmark on the machine waits for the restored game's lease",
                bool(waiting) and job.get("status") == "queued",
                f"HTTP {started.status_code}; job {job.get('status')}: {job.get('waiting')}")

        # ---- a test runs on the machine; the game closes; the benchmark starts and drains the test
        result: dict = {}
        before = len(scripted.completions())

        def test():
            result["t0"] = time.time()
            result["got"] = alice.post(f"/api/pool/servers/{hid}/test", params={"wait": 60})
            result["t1"] = time.time()
        tester = threading.Thread(target=test, daemon=True)
        tester.start()
        running = wait_until(lambda: any(x.get("class") == "test" and x.get("state") == "running"
                                         for x in _machine_status(admin, hid).get("tickets") or []), 20, 0.1)
        t_close = time.time()
        closed = alice.delete(f"/api/games/{gid}")
        gid = "" if closed.status_code == 200 else gid
        tester.join(90)
        tested = result.get("got")
        body = tested.json() if tested is not None and tested.status_code == 200 else {}
        r.check("the member's test of the machine ran on the slot the idle game lent, and was cancelled when the "
                "benchmark took the machine",
                bool(running) and closed.status_code == 200 and body.get("cancelled") is True
                and body.get("code") == "benchmark",
                f"running {bool(running)}; close HTTP {closed.status_code}; test HTTP "
                f"{getattr(tested, 'status_code', None)} after {round(result.get('t1', 0) - result.get('t0', 0), 2)} s: "
                f"{json.dumps(body)[:240]}")
        done = wait_until(lambda: _runs_job(alice, run_id).get("status") in ("done", "failed", "cancelled"), 180, 1.0)
        job = _runs_job(alice, run_id)
        res = job.get("result") or {}
        r.check("the benchmark job ran to its end with drain_s in its result: its first request waited only for the "
                "helper to stop the cancelled test (well under the test's 10 s), and its timing is not contaminated",
                bool(done) and job.get("status") == "done" and res.get("drain_s") is not None
                and res["drain_s"] < 5.0 and res.get("contaminated") is False,
                json.dumps({"status": job.get("status"), "error": job.get("error"), "drain_s": res.get("drain_s"),
                            "contaminated": res.get("contaminated"),
                            "started_after_close_s": round((job.get("started") or 0) - t_close, 2)})[:400],
                drain_s=res.get("drain_s"))
        after = scripted.completions()[before:]
        late = next((q for q in after if q.get("delay") == 10.0), None)
        bench = [q for q in after if q is not late]
        ended = late["t"] + late["seconds"] if late else None
        first = min((q["t"] for q in bench), default=None)
        r.check("no pooled request overlapped a benchmark step: the helper stopped the cancelled test (its client gone "
                "before the 10 s were up) before the benchmark's first completion reached the endpoint",
                late is not None and late.get("gone") is not None and late["seconds"] < 10.0 and first is not None
                and first >= ended - 0.05 and len(bench) >= 2,
                f"test ended {round(ended - result['t0'], 2) if ended else None} s after it began; the benchmark's "
                f"first completion at {round(first - result['t0'], 2) if first else None} s; {len(bench)} benchmark "
                f"completions")
        final = _machine_status(admin, hid)
        counters = final.get("counters") or {}
        busy_lines = _busy_lines(h)
        r.check("the helper acknowledged the cancel: its answer freed the slot (a late frame, nothing drained, no cancel "
                "unacknowledged), nothing was refused as busy, and no lease is left on the machine",
                counters.get("late_frames", 0) >= 1 and counters.get("draining", 0) == 0
                and counters.get("cancel_unacked", 0) == 0 and counters.get("drain_timeouts", 0) == 0
                and counters.get("busy_refusals", 0) == 0 and not busy_lines and not final.get("leases"),
                json.dumps({"counters": counters, "leases": final.get("leases")})[:400])
        data["restore"].update({"restart_leases": restored_leases, "test": body, "job": {
            "status": job.get("status"), "drain_s": res.get("drain_s"), "contaminated": res.get("contaminated")},
            "benchmark_completions": len(bench), "counters": counters})
    finally:
        # each step on its own: a server that did not come back must not leave the helper running
        for undo in ((lambda: alice.delete(f"/api/games/{gid}")) if gid else None,
                     (lambda: alice.post(f"/api/benchmarks/runs/{run_id}/control", json={"action": "cancel"}))
                     if run_id else None):
            try:
                if undo is not None:
                    undo()
            except httpx.HTTPError:
                pass
        if helper is not None:
            helper.stop()
        scripted.close()


def _turn_spans(served: list) -> list:
    """A game's turns at the endpoint (``kind`` ``turn``, in order): from a turn's first request to the end of the
    one answered with ``end_turn``; its seat is mid-turn across each span."""
    spans, first = [], None
    for q in sorted((q for q in served if q.get("kind") == "turn"), key=lambda q: q["t"]):
        first = q["t"] if first is None else first
        if "end_turn" in (q.get("calls") or []):
            spans.append((first, q["t"] + q["seconds"]))
            first = None
    return spans


def _queue_pooled_prompts(h: Harness) -> None:
    """3-09's checks (design/phase3-features.md P3.3.3, P3.3.7, P3.3.8), on alice's helper machine with one slot (a real
    helper behind the scripted endpoint, 0.4 s a completion, no chats opened), shared with bob for pooled prompts and
    probes: her lobby game's model seat holds the slot mid-turn (the endpoint holds its request), while bob's probe run
    (two message cases), her probe run (one), her two reports and his two (each with a written analysis) queue as
    pooled prompts. Their Queue pages show the game's lease and the prompts with their places, each member's work named
    to its owner and "someone's report" to the other, the machine not named at all to a member it is not shared with;
    bob lowers his probe run and raises his second report, and may not move hers. Released, the prompts are served by
    priority, then by turns between the members, then in order, none starting while the game's seat is mid-turn; no
    report holds a lease, nothing is refused as busy, and every report has its analysis."""
    r = h.report
    admin, alice, bob, mod = (h.accounts[k] for k in ("admin", "alice", "bob", "mod"))
    scripted = FakeEndpoint(delay=0.4, max_trades=0)
    helper: Optional[Helper] = None
    gid, runs = "", []
    data = r.data.setdefault("queue", {}).setdefault("pooled", {})

    def mine(who: "Account", hid: str) -> dict:
        got = who.get("/api/queue")
        q = got.json() if got.status_code == 200 else {}
        box = next((m for m in q.get("machines") or [] if m.get("server_id") == hid), None)
        return {"q": q, "box": box}

    def waiting(who: "Account", hid: str) -> list:
        return ((mine(who, hid)["box"] or {}).get("prompts") or {}).get("waiting") or []

    try:
        made = alice.post("/api/pool/servers", json={"config": {"name": f"QA pooled helper {h.tag}", "kind": "owned",
                                                                "connection": {"provider": "openai_compatible",
                                                                               "max_parallel": 1}},
                                                     "reach": "worker"})
        hid = made.json().get("id") if made.status_code == 200 else ""
        t = alice.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa pooled"}) if hid else None
        if t is not None and t.status_code == 200:
            helper = Helper(h, t.json().get("token", ""), scripted, "qa-pooled-helper")
        group = alice.post("/api/pool/groups", json={"name": f"QA pooled group {h.tag}", "visibility": "private"})
        group_id = group.json().get("id") if group.status_code == 200 else ""
        put = alice.put(f"/api/pool/servers/{hid}", json={"group_id": group_id})
        grant = alice.post(f"/api/pool/groups/{group_id}/grants",
                           json={"subject_type": "user", "handle": bob.handle, "purposes": ["pooled", "probe"]})
        online = bool(helper and wait_until(lambda: _online(alice, hid), 60, 0.25))
        r.check("a helper machine of alice's with one slot, its real helper connected, shared with bob for pooled prompts "
                "and probes only",
                online and (group.status_code, put.status_code, grant.status_code) == (200, 200, 200)
                and grant.json().get("purposes") == ["pooled", "probe"],
                f"online {online}; group {group.status_code}, put {put.status_code}, grant {grant.status_code}")
        data["machine"] = hid

        # ---- a seat that plays every turn needs a grant for games: bob's covers pooled prompts only
        refused = bob.post("/api/games", json={"name": f"QA no games {h.tag}", "config": CONFIG, "seats": [
            {"type": "llm", "llm": {"server_id": hid, "model": scripted.MODEL}}, {"type": "bot"}]})
        r.check("bob's grant covers pooled prompts, not a model seat that plays every turn: his game is refused (403, "
                "the purpose named)",
                refused.status_code == 403 and "pooled prompts" in refused.text, f"HTTP {refused.status_code}: "
                f"{refused.text[:200]}")

        # ---- alice's lobby game: its first turn's request held at the endpoint, its seat mid-turn on the slot
        scripted.hold = lambda body: FakeEndpoint.describe(body)["kind"] == "turn"
        scripted.gate.clear()
        g = alice.post("/api/games", json={"name": f"QA pooled game {h.tag}", "config": {**CONFIG, "seed": 3317},
                                           "seats": [{"type": "llm", "llm": {"server_id": hid, "model": scripted.MODEL}},
                                                     {"type": "bot"}]})
        gid = g.json().get("id", "") if g.status_code == 200 else ""
        holding = wait_until(lambda: scripted.held >= 1, 60, 0.2)
        r.check("alice's lobby game holds the slot mid-turn (its request held at the endpoint)", bool(gid and holding),
                f"HTTP {g.status_code}; held {scripted.held}")

        # ---- the work that queues behind it, one prompt at a time so the line's order is known
        def probe_run(who: "Account", name: str, markers: list) -> str:
            ed = who.post("/api/scenario-editor", json={"source": "generate", "config": {**CONFIG, "seed": 3319},
                                                         "players": [{"type": "bot"}, {"type": "bot"}]})
            eid = ed.json().get("editor_id", "") if ed.status_code == 200 else ""
            saved = who.post(f"/api/scenario-editor/{eid}/save", json={"name": f"QA pooled {name} {h.tag}"})
            scn = saved.json().get("id", "") if saved.status_code == 200 else ""
            who.delete(f"/api/scenario-editor/{eid}")
            pid = f"qa-pooled-{name}-{h.tag}"
            put_probe = who.put(f"/api/probes/{pid}", json={
                "name": f"QA pooled {name}", "scenario": scn, "subject": 1, "counterparty": 0,
                "cases": [{"id": f"m{i + 1}", "kind": "message", "message": f"{m} hello"} for i, m in enumerate(markers)]})
            run = who.post(f"/api/probes/{pid}/run", json={"llm": {"server_id": hid, "model": scripted.MODEL}})
            if run.status_code != 200:
                raise HarnessError(f"{who.handle}'s probe run did not start: scenario {saved.status_code}, probe "
                                   f"{put_probe.status_code}, run {run.status_code} {run.text[:200]}")
            return run.json()["id"]

        def report(who: "Account", title: str) -> str:
            got = who.post("/api/reports", json={"title": title, "sections": ["summary"], "narrative": {
                "enabled": True, "server_id": hid, "model_id": scripted.MODEL}})
            if got.status_code != 200:
                raise HarnessError(f"{who.handle}'s report did not start: {got.status_code} {got.text[:200]}")
            return got.json()["id"]

        names = {f"QA-PROBE-bob-1-{h.tag}": "bob probe 1", f"QA-PROBE-bob-2-{h.tag}": "bob probe 2",
                 f"QA-PROBE-alice-1-{h.tag}": "alice probe 1"}
        reports = {}
        n = 0
        bob_run = probe_run(bob, "bob", [f"QA-PROBE-bob-1-{h.tag}", f"QA-PROBE-bob-2-{h.tag}"])
        runs.append((bob, bob_run))
        n += 1
        in_line = wait_until(lambda: len(waiting(admin, hid)) >= n, 60, 0.25)
        alice_run = probe_run(alice, "alice", [f"QA-PROBE-alice-1-{h.tag}"])
        runs.append((alice, alice_run))
        # both runs lowered alike: a run makes way between its cases only for work of a higher priority
        lowered = bob.post("/api/queue/priority", json={"kind": "probe", "id": bob_run, "priority": -20})
        alice.post("/api/queue/priority", json={"kind": "probe", "id": alice_run, "priority": -20})
        for who, title in ((alice, "A1"), (alice, "A2"), (bob, "B1"), (bob, "B2")):
            reports[title] = (who, report(who, f"QA pooled {title} {h.tag}"))
            n += 1
            in_line = in_line and wait_until(lambda n=n: len(waiting(admin, hid)) >= n, 60, 0.25)
        raised = bob.post("/api/queue/priority", json={"kind": "report", "id": reports["B2"][1], "priority": 20})
        not_his = bob.post("/api/queue/priority", json={"kind": "report", "id": reports["A1"][1], "priority": 50})
        line = [(p.get("position"), p.get("label")) for p in waiting(admin, hid)]
        bob_probe = f"probe run “QA pooled bob · {scripted.MODEL} @ QA pooled helper {h.tag}”"
        expected_line = [f"report “QA pooled {t} {h.tag}”" for t in ("B2", "A1", "A2", "B1")] + [bob_probe]
        r.check("five pooled prompts wait in the machine's line behind the game's seat, bob's probe run lowered and his "
                "second report raised by his own priority buttons (hers is not his to move: 404); alice's probe run "
                "waits as a job behind his",
                bool(in_line) and lowered.status_code == 200 and raised.status_code == 200
                and not_his.status_code == 404 and [label for _, label in line] == expected_line
                and [pos for pos, _ in line] == [1, 2, 3, 4, 5],
                json.dumps({"line": line, "lowered": lowered.status_code, "raised": raised.status_code,
                            "not_his": not_his.status_code})[:600])

        # ---- the Queue page as each sees it
        his, hers, other = mine(bob, hid), mine(alice, hid), mine(mod, hid)
        his_box, her_box = his["box"] or {}, hers["box"] or {}
        his_line = [(p.get("label"), p.get("member"), p.get("can_manage"), p.get("can_cancel"))
                    for p in (his_box.get("prompts") or {}).get("waiting") or []]
        her_line = [(p.get("label"), p.get("member"), p.get("can_cancel"))
                    for p in (her_box.get("prompts") or {}).get("waiting") or []]
        his_leases = [(x.get("label"), x.get("member"), x.get("state")) for x in his_box.get("leases") or []]
        her_leases = [(x.get("label"), x.get("member"), x.get("state")) for x in her_box.get("leases") or []]
        text_other = json.dumps(other["q"], ensure_ascii=False)
        r.check("bob's Queue page: alice's machine with its slot in use, her game's seat mid-turn as someone's game, his "
                "prompts named with his priority and cancel buttons, hers as someone's report with neither",
                his_box.get("capacity") == 1 and his_box.get("in_flight") == 1
                and his_leases == [("someone's game", None, "active")]
                and his_line == [(f"report “QA pooled B2 {h.tag}”", bob.handle, True, True),
                                 ("someone's report", None, False, False), ("someone's report", None, False, False),
                                 (f"report “QA pooled B1 {h.tag}”", bob.handle, True, True),
                                 (expected_line[4], bob.handle, True, True)],
                json.dumps({"leases": his_leases, "line": his_line})[:700])
        r.check("alice's Queue page: her game and her reports by name; bob's reports and probe as someone's, hers to "
                "cancel as the machine's owner",
                her_leases == [(f"game “QA pooled game {h.tag}”", alice.handle, "active")]
                and her_line == [("someone's report", None, True), (f"report “QA pooled A1 {h.tag}”", alice.handle, True),
                                 (f"report “QA pooled A2 {h.tag}”", alice.handle, True), ("someone's report", None, True),
                                 ("someone's probe", None, True)],
                json.dumps({"leases": her_leases, "line": her_line})[:700])
        r.check("a member the machine is not shared with is not shown it: it is one of the other machines in use, "
                "named nowhere",
                other["box"] is None and (other["q"].get("others_busy") or 0) >= 1
                and f"QA pooled helper {h.tag}" not in text_other and "QA pooled" not in text_other,
                f"others_busy {other['q'].get('others_busy')}")
        data["queue_pages"] = {"bob": {"leases": his_leases, "line": his_line},
                               "alice": {"leases": her_leases, "line": her_line}, "line": line}
        if h.hold_line:
            # the browser checklist's C1: the line stays as it is while the Queue page is looked at
            hold = h.server.state / "qa-hold.json"
            hold.write_text(json.dumps({"base": h.server.public, "password": r.data.get("password"),
                                        "accounts": {k: a.handle for k, a in h.accounts.items()}, "machine": hid,
                                        "until": time.time() + h.hold_line}, indent=1), encoding="utf-8")
            r.note(f"holding the line for {h.hold_line:.0f} s (--hold-line); the accounts are in {hold}")
            time.sleep(h.hold_line)

        # ---- released: the order they are served in, and when
        released = time.time()
        scripted.hold = None
        scripted.gate.set()

        seen_kinds: set = set()

        def finished() -> bool:
            seen_kinds.update(x.get("kind") for x in _machine_status(admin, hid).get("leases") or [])
            def body(got) -> dict:
                return got.json() if got.status_code == 200 else {}
            done = all((body(who.get(f"/api/reports/{rid}")).get("meta") or {}).get("status") in ("done", "failed")
                       for who, rid in reports.values())
            ran = all((body(who.get(f"/api/probe-runs/{rid}")).get("run") or {}).get("status") in
                      ("finished", "stopped", "failed") for who, rid in runs)
            return done and ran
        ended = wait_until(finished, 240, 1.0)
        alice.post(f"/api/games/{gid}/control", json={"paused": True})
        time.sleep(1.0)
        served = [q for q in scripted.completions() if q["t"] >= released - 60]
        pooled = sorted((q for q in served if q.get("kind") == "plain" or (q.get("kind") == "chat" and q.get("marker"))),
                        key=lambda q: q["t"])

        def name(q) -> str:
            if q.get("kind") == "chat":
                return names.get(q.get("marker") or "", q.get("marker") or "?")
            found = re.search(r"QA pooled (A1|A2|B1|B2) ", q.get("head") or "")
            return found.group(1) if found else (q.get("head") or "?")
        order = [name(q) for q in pooled]
        expected = ["B2", "A1", "B1", "A2", "bob probe 1", "bob probe 2", "alice probe 1"]
        r.check("released, the prompts were served by priority (his raised report, then the reports, then his lowered "
                "probe's answers), then by turns between the members (B1 before A2), then in order; alice's probe run "
                "after his",
                bool(ended) and order == expected, f"served {order}; expected {expected}")
        spans = _turn_spans(served)
        inside = [(name(q), round(q["t"] - released, 2)) for q in pooled if any(a <= q["t"] < b for a, b in spans)]
        r.check("no pooled prompt started while the game's seat was mid-turn (each between its turns, on the slot it "
                "lends)", bool(spans) and not inside,
                f"{len(spans)} turns, {len(pooled)} prompts; inside a turn: {inside}", turns=len(spans), prompts=len(pooled))
        texts = {t: who.get(f"/api/reports/{rid}/html").text for t, (who, rid) in reports.items()}
        status = _machine_status(admin, hid)
        counters = status.get("counters") or {}
        lease_kinds = sorted({x.get("kind") for x in status.get("leases") or []})
        turns_queue = []
        got = alice.get(f"/api/games/{gid}/metrics")
        if got.status_code == 200:
            turns_queue = [row.get("queue_s") or 0.0 for row in got.json().get("turns") or [] if row.get("controller") == "llm"]
        r.check("every report has its analysis (written by the endpoint), no report or probe held a lease while they "
                "ran (only the game's), nothing was refused as busy, and a seat's turn waited at most one pooled step",
                all("(qa endpoint)" in text for text in texts.values()) and lease_kinds in ([], ["game"])
                and seen_kinds <= {"game"}
                and counters.get("busy_refusals", 0) == 0 and turns_queue and max(turns_queue) <= 0.4 + 1.0 + 0.5,
                json.dumps({"lease_kinds": lease_kinds, "seen": sorted(seen_kinds), "counters": counters,
                            "max_turn_queue_s": max(turns_queue) if turns_queue else None})[:500],
                max_turn_queue_s=max(turns_queue) if turns_queue else None)
        data.update({"order": order, "expected": expected, "turns": len(spans),
                     "prompts": [{"name": name(q), "at_s": round(q["t"] - released, 2), "seconds": q["seconds"]}
                                 for q in pooled],
                     "turn_spans_s": [[round(a - released, 2), round(b - released, 2)] for a, b in spans],
                     "counters": counters})
    finally:
        scripted.gate.set()
        for undo in ((lambda: alice.delete(f"/api/games/{gid}")) if gid else None,):
            try:
                if undo is not None:
                    undo()
            except httpx.HTTPError:
                pass
        for who, rid in runs:
            try:
                who.post(f"/api/probe-runs/{rid}/stop")
            except httpx.HTTPError:
                pass
        if helper is not None:
            helper.stop()
        scripted.close()


def _ledger_rows(state: Path, sql: str, args: tuple = ()) -> list:
    """Rows of the instance's own database, read-only (the server keeps writing it: SQLite's write-ahead log lets a
    reader see what is committed)."""
    import sqlite3
    con = sqlite3.connect(f"file:{(state / 'citar.db').as_posix()}?mode=ro", uri=True, timeout=10)
    try:
        return con.execute(sql, args).fetchall()
    finally:
        con.close()


def part_ledger(h: Harness) -> None:
    """3-10's checks (design/phase3-features.md P3.4): bob's helper machine (a real helper behind the scripted
    endpoint, one slot, 0.3 s a completion) shared with alice for games and pooled prompts serves her lobby game and
    her report's analysis; she also plays a game on a dry-run machine of her own. The compute ledger's events (read
    from the instance's database) put every completion the endpoint served on bob's machine, consumed by alice and
    given by bob, with the helper's reported and the server's observed times and generation time bounded by them; bob's
    given and alice's used, as ``/api/ledger/me`` tells each, equal the events' sums; her own machine's work is
    self-use, on neither side; her ratio follows its visibility (private, then members)."""
    r = h.report
    admin, alice, bob, mod = (h.accounts[k] for k in ("admin", "alice", "bob", "mod"))
    scripted = FakeEndpoint(delay=0.3, max_trades=0)
    helper: Optional[Helper] = None
    games: list = []
    state = h.server.state
    data = r.data.setdefault("ledger", {})

    def turn(gid: str) -> int:
        got = alice.get(f"/api/games/{gid}")
        return (got.json().get("turn") or 0) if got.status_code == 200 else 0

    try:
        made = bob.post("/api/pool/servers", json={"config": {"name": f"QA ledger helper {h.tag}", "kind": "owned",
                                                              "connection": {"provider": "openai_compatible",
                                                                             "max_parallel": 1}},
                                                   "reach": "worker"})
        hid = made.json().get("id") if made.status_code == 200 else ""
        tok = bob.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa ledger"}) if hid else None
        if tok is not None and tok.status_code == 200:
            helper = Helper(h, tok.json().get("token", ""), scripted, "qa-ledger-helper")
        group = bob.post("/api/pool/groups", json={"name": f"QA ledger group {h.tag}", "visibility": "private"})
        group_id = group.json().get("id") if group.status_code == 200 else ""
        put = bob.put(f"/api/pool/servers/{hid}", json={"group_id": group_id})
        grant = bob.post(f"/api/pool/groups/{group_id}/grants",
                         json={"subject_type": "user", "handle": alice.handle, "purposes": ["game", "pooled"]})
        online = bool(helper and wait_until(lambda: _online(bob, hid), 60, 0.25))
        dry = alice.post("/api/servers", json={"name": f"QA ledger alice dry {h.tag}", "kind": "test",
                                               "connection": {"provider": "dryrun", "dry_run_delay": 0.1},
                                               "models": [{"key": "dry-run"}]})
        dry_id = dry.json().get("id") if dry.status_code == 200 else ""
        r.check("bob's helper machine (its real helper connected) shared with alice for games and pooled prompts, and "
                "a dry-run machine of alice's own",
                online and (group.status_code, put.status_code, grant.status_code, dry.status_code) ==
                (200, 200, 200, 200),
                f"online {online}; group {group.status_code}, put {put.status_code}, grant {grant.status_code}, "
                f"dry {dry.status_code}")
        data["machines"] = {"bob_helper": hid, "alice_dry": dry_id}
        ids = dict(_ledger_rows(state, "SELECT handle, id FROM users"))
        a_id, b_id = ids.get(alice.handle), ids.get(bob.handle)
        start = time.time()

        # ---- alice's lobby game on bob's machine, and one on her own
        for sid, model, seed in ((hid, scripted.MODEL, 3331), (dry_id, "dry-run", 3332)):
            g = alice.post("/api/games", json={"name": f"QA ledger game {len(games) + 1} {h.tag}",
                                               "config": {**CONFIG, "seed": seed},
                                               "seats": [{"type": "llm", "llm": {"server_id": sid, "model": model}},
                                                         {"type": "bot"}]})
            if g.status_code == 200:
                games.append(g.json()["id"])
        played = len(games) == 2 and wait_until(lambda: all(turn(g) >= 3 for g in games), 240, 1.0)
        for g in games:
            alice.post(f"/api/games/{g}/control", json={"paused": True})
        # ---- her report, its analysis written on bob's machine
        rep_ = alice.post("/api/reports", json={"title": f"QA ledger report {h.tag}", "sections": ["summary"],
                                                "narrative": {"enabled": True, "server_id": hid,
                                                              "model_id": scripted.MODEL}})
        rid = rep_.json().get("id", "") if rep_.status_code == 200 else ""
        built = bool(rid) and wait_until(
            lambda: ((alice.get(f"/api/reports/{rid}").json().get("meta") or {}).get("status") in ("done", "failed")),
            180, 1.0)
        status = (alice.get(f"/api/reports/{rid}").json().get("meta") or {}) if rid else {}
        r.check("alice's games each played three rounds (one on bob's machine, one on her own) and her report was "
                "built with its analysis written on bob's machine",
                bool(played) and bool(built) and status.get("status") == "done" and status.get("narrative_written") is True,
                f"turns {[turn(g) for g in games]}; report {status.get('status')} "
                f"narrative {status.get('narrative_written')}")
        time.sleep(4.0)                          # the ledger's writer inserts what it has every 2 s

        # ---- the events on bob's machine: one a completion the endpoint served, alice's, given by bob
        served = [q for q in scripted.completions() if q["t"] >= start - 1]
        events = _ledger_rows(state, "SELECT activity, activity_kind, task_class, consumer_id, provider_id, kind, "
                                     "gen_s, reported_s, observed_s, credits, self_use FROM compute_events "
                                     "WHERE machine_id = ? ORDER BY id", (hid,))
        game_act, report_act = f"game:{games[0]}" if games else "", f"report:{rid}"
        acts = {(e[0], e[2]) for e in events}
        honest = all(e[7] is not None and e[8] is not None and e[6] <= min(e[7], e[8]) + 1e-6 for e in events)
        r.check("every completion bob's helper served is one event on his machine: alice's (her game's full seat and "
                "her report's pooled prompt), given by bob, each with the helper's reported and the server's observed "
                "time and its generation time no more than either",
                len(events) == len(served) and len(served) > 3 and acts == {(game_act, "full"), (report_act, "partial")}
                and all(e[3] == a_id and e[4] == b_id and e[5] == "gpu" and not e[10] for e in events) and honest,
                f"{len(events)} events, {len(served)} completions; activities {sorted(acts)}; honest {honest}",
                events=len(events), completions=len(served))
        own = _ledger_rows(state, "SELECT count(*), sum(self_use), sum(credits) FROM compute_events "
                                  "WHERE machine_id = ? AND kind = 'gpu'", (dry_id,))[0]
        r.check("her game on her own machine is self-use: its events count on neither side",
                (own[0] or 0) > 0 and own[0] == own[1], f"{own[0]} events, {own[1]} self-use, {own[2]} credits")

        # ---- given and used as each is told, against the events (stable between two reads: CPU lands each minute)
        def sums():
            given = _ledger_rows(state, "SELECT coalesce(sum(credits), 0), count(*) FROM compute_events "
                                        "WHERE provider_id = ? AND self_use = 0", (b_id,))[0]
            used = _ledger_rows(state, "SELECT coalesce(sum(credits), 0), count(*) FROM compute_events "
                                       "WHERE consumer_id = ? AND self_use = 0", (a_id,))[0]
            mine = _ledger_rows(state, "SELECT coalesce(sum(credits), 0) FROM compute_events "
                                       "WHERE consumer_id = ? AND self_use = 1", (a_id,))[0][0]
            # what she gave others: nothing in this part (her own machine is self-use), but an earlier part of
            # ``all`` on the same server may have had her machine serve someone
            hers_given = _ledger_rows(state, "SELECT coalesce(sum(credits), 0) FROM compute_events "
                                             "WHERE provider_id = ? AND self_use = 0", (a_id,))[0][0]
            return given, used, mine, hers_given
        seen: dict = {}

        def agrees():
            before = sums()
            his = bob.get("/api/ledger/me?range=all").json()
            hers = alice.get("/api/ledger/me?range=all").json()
            after = sums()
            seen.update(before=before, his=his, hers=hers)
            return before == after and abs(his["given"]["credits"] - before[0][0]) < 1e-3 \
                and his["given"]["events"] == before[0][1] and abs(hers["used"]["credits"] - before[1][0]) < 1e-3 \
                and hers["used"]["events"] == before[1][1] and abs(hers["given"]["credits"] - before[3]) < 1e-3 \
                and abs(hers["own_use"]["credits"] - before[2]) < 1e-3
        matched = wait_until(agrees, 90, 2.0)
        his, hers = seen.get("his") or {}, seen.get("hers") or {}
        machines = {m["name"]: m for m in his.get("by_machine") or []}
        her_machines = {m["name"]: m for m in hers.get("by_machine") or []}
        r.check("bob's given and alice's used, as /api/ledger/me tells each, equal the events' sums; her own machine's "
                "work is hers alone (own use), and nobody was given anything of it",
                bool(matched) and f"QA ledger helper {h.tag}" in machines
                and f"QA ledger helper {h.tag}" in her_machines,
                json.dumps({"bob_given": his.get("given"), "alice_used": (hers.get("used") or {}).get("credits"),
                            "events": seen.get("before"), "alice_own": hers.get("own_use")}, default=str)[:700],
                bob_given=(his.get("given") or {}).get("credits"), alice_used=(hers.get("used") or {}).get("credits"))
        kinds = {k["kind"]: k for k in hers.get("by_activity") or []}
        given_, used_ = (hers.get("given") or {}).get("credits", 0.0), (hers.get("used") or {}).get("credits", 0.0)
        told = hers.get("ratio") or {}
        r.check("alice's use is told by kind of work (her games, her report) and her ratio is given over used",
                {"game", "report"} <= set(kinds) and used_ > 0 and told.get("value") is not None
                and abs(told["value"] - given_ / used_) < 1e-3
                and abs(float(told.get("text") or "nan") - told["value"]) <= 0.0051,
                json.dumps({"kinds": sorted(kinds), "ratio": told, "given": given_, "used": used_}))

        # ---- the ratio's visibility (P3.2.6, P3.4.4): private by default, then members
        path = f"/api/users/{alice.handle}/ratio"
        hidden = (bob.get(path).status_code, h.anonymous().get(path).status_code, mod.get(path).status_code)
        for_admin = admin.get(path)
        shown = alice.put("/api/auth/me", json={"field_visibility": {"ratio": "server"}})
        to_member = bob.get(path)
        to_visitor = h.anonymous().get(path)
        profile = bob.get(f"/api/users/{alice.handle}").json()
        r.check("her ratio, private, is 404 to another member, a moderator and a visitor and the totals to an "
                "administrator; made visible to members, bob reads it (and her profile carries it), a visitor still "
                "gets 404",
                hidden == (404, 404, 404) and for_admin.status_code == 200
                and abs(for_admin.json()["used"]["credits"] - (hers.get("used") or {}).get("credits", -1)) < 1.0
                and shown.status_code == 200 and to_member.status_code == 200 and to_visitor.status_code == 404
                and "ratio" in profile,
                json.dumps({"hidden": hidden, "admin": for_admin.status_code, "member": to_member.status_code,
                            "visitor": to_visitor.status_code}))
        listed = {u["handle"]: u for u in admin.get("/api/admin/users").json().get("users") or []}
        r.check("the administrators' users page shows each account's 30 days of compute",
                (listed.get(bob.handle) or {}).get("compute_30d", {}).get("given", 0) > 0
                and (listed.get(alice.handle) or {}).get("compute_30d", {}).get("used", 0) > 0,
                json.dumps({k: (listed.get(k) or {}).get("compute_30d") for k in (alice.handle, bob.handle)}))
        data.update({"games": games, "report": rid, "events_on_helper": len(events), "completions": len(served),
                     "bob_given": his.get("given"), "alice_used": hers.get("used"), "alice_own": hers.get("own_use"),
                     "alice_by_machine": hers.get("by_machine"), "ratio": hers.get("ratio")})
        if h.keep:
            # the browser checklist's B1 reads this instance: alice's ratio back to private for its first look
            alice.put("/api/auth/me", json={"field_visibility": {"ratio": "private"}})
    finally:
        for g in games:
            try:
                alice.delete(f"/api/games/{g}")
            except httpx.HTTPError:
                pass
        if helper is not None:
            helper.stop()
        scripted.close()


class Socket:
    """A game socket read on a thread of its own: every message it receives, kept with when it came."""

    def __init__(self, h: "Harness", gid: str, token: str, *, account: Optional["Account"] = None):
        """A seat's or a spectator's socket by its ``token``, or (``account``, no token) a member's, by the session
        cookie the game screen's socket carries."""
        from websockets.sync.client import connect
        self.messages: list = []
        headers = None
        if account is not None:
            headers = {"Cookie": "; ".join(f"{k}={v}" for k, v in account.http.cookies.items())}
        self._connection = connect(f"ws://127.0.0.1:{h.server.port}/ws/games/{gid}?token={token}", open_timeout=15,
                                   proxy=None, max_size=None, additional_headers=headers)
        self._ws = self._connection.__enter__()
        self._thread = threading.Thread(target=self._read, daemon=True, name=f"qa-socket-{token[:4]}")
        self._thread.start()

    def _read(self) -> None:
        try:
            while True:
                self.messages.append((time.time(), json.loads(self._ws.recv())))
        except Exception:
            pass

    def statuses(self, nid: Optional[int] = None) -> list:
        """The ``negotiation_status`` messages it received (of chat ``nid``)."""
        return [m for _, m in self.messages if m.get("type") == "negotiation_status"
                and (nid is None or m.get("negotiation") == nid)]

    def close(self) -> None:
        try:
            self._connection.__exit__(None, None, None)
        except Exception:
            pass


def part_chat(h: Harness) -> None:
    """3-11's checks, the server half of chat (design/phase3-features.md P3.5.2-P3.5.5): alice plays a person seat,
    through the API and her seat's socket, in a short game (8 turns) with two dry-run model seats on a one-slot
    machine of hers and a bot, both chat timeouts set to 20 s. She opens a chat with each model and the bot: model A
    takes 3 s to answer and is answered back; model B, asked while A holds the machine, queues, then takes longer
    than its time and is declined for by the chat clock, "(no reply in time)"; the bot answers at once and alice lets
    that chat run out, so it closes as expired. On turn 3 both models open a trade with her: she accepts A's and lets
    B's expire, and each model's turn ends when its chat is settled. The game is played to its end. Her socket gets
    every status of her chats (queued with a place, thinking, waiting with a deadline, closed); the bot seat's socket
    gets only those of chats it is in, and a spectator's none while she plays. The metrics count the models' answers
    and the clock's declines; the chat route reads a chat with its clock.

    3-13's checks, the client's payloads (P3.5.8): her view answers with an ETag, a refresh that sends it back gets 304
    until something changes, a query between two looks (the game screen's for its selected unit) included; her view's diplomacy carries no trade options, the trade options route gives her those
    of a civilization she has met (not her own, not a stranger's); and the finished game's replay is the journal's
    frames, a keyframe and then deltas, one per round."""
    r = h.report
    alice = h.accounts["alice"]
    data = r.data.setdefault("chat", {})
    sockets: list = []
    gid = ""
    timeout = 20
    try:
        made = alice.post("/api/servers", json={"name": f"QA chat machine {h.tag}", "kind": "test",
                                                "connection": {"provider": "dryrun", "dry_run_delay": 0.3,
                                                               "max_parallel": 1},
                                                "models": [{"key": "dry-run"}]})
        mid = made.json().get("id", "") if made.status_code == 200 else ""
        model = {"server_id": mid, "model": "dry-run", "dry_run_propose_every": 3}
        g = alice.post("/api/games", json={
            "name": f"QA chat game {h.tag}",
            "config": {"map_size": "small", "map_type": "pangaea", "seed": 3311, "barbarians": "off",
                       "city_states": 0, "turn_limit": 8, "chat_model_reply_seconds": timeout,
                       "chat_person_reply_seconds": timeout},
            "seats": [{"type": "human"}, {"type": "llm", "llm": dict(model)}, {"type": "llm", "llm": dict(model)},
                      {"type": "bot"}]})
        info = g.json() if g.status_code == 200 else {}
        gid = info.get("id", "")
        tokens = [seat["token"] for seat in info.get("seats") or []]
        cfg = info.get("config") or {}
        r.check("alice's game with two dry-run model seats on her one-slot machine and a bot, both chat timeouts "
                "20 s (the lobby's keys, read back from the game)",
                bool(gid) and made.status_code == 200 and cfg.get("chat_model_reply_seconds") == timeout
                and cfg.get("chat_person_reply_seconds") == timeout and cfg.get("max_chat_messages") == 30,
                f"machine {made.status_code}, game {g.status_code}; config {json.dumps({k: cfg.get(k) for k in ('chat_model_reply_seconds', 'chat_person_reply_seconds', 'max_chat_messages')})}")
        if not gid:
            return
        data.update({"game": gid, "machine": mid})
        met = alice.post(f"/api/games/{gid}/debug/meet_all")
        mine, bots, spectator = (Socket(h, gid, tokens[0]), Socket(h, gid, tokens[3]),
                                 Socket(h, gid, info.get("spectator_token", "")))
        sockets += [mine, bots, spectator]

        def tool(name: str, args: dict) -> dict:
            got = alice.post(f"/api/games/{gid}/tool", params={"token": tokens[0]}, json={"tool": name, "args": args})
            return got.json() if got.status_code == 200 else {"ok": False, "error": got.text}

        def head(nid: int) -> dict:
            got = alice.get(f"/api/games/{gid}/negotiations/{nid}", params={"token": tokens[0]})
            return got.json() if got.status_code == 200 else {}

        def game() -> dict:
            got = alice.get(f"/api/games/{gid}")
            return got.json() if got.status_code == 200 else {}

        # ---- turn 1: alice opens a chat with each model and the bot
        t0 = time.time()
        a = tool("open_negotiation", {"to": 1, "message": "[dry run: wait 3 s] QA-A: shall we talk?"})
        a_id = (a.get("result") or {}).get("negotiation_id")
        wait_until(lambda: any(s["state"] == "thinking" for s in mine.statuses(a_id)), 15, 0.1)
        b = tool("open_negotiation", {"to": 2, "message": "[dry run: wait 45 s] QA-B: shall we talk?"})
        b_id = (b.get("result") or {}).get("negotiation_id")
        b_opened = time.time()
        c = tool("open_negotiation", {"to": 3, "message": "QA-BOT: shall we talk?"})
        c_id = (c.get("result") or {}).get("negotiation_id")
        r.check("alice opened a chat with each model and the bot (meet_all first)",
                met.status_code == 200 and all(x.get("ok") for x in (a, b, c)),
                f"meet_all {met.status_code}; {[x.get('ok') or x.get('error') for x in (a, b, c)]}")
        payloads_while_playing(h, gid, tokens, data, tool)
        answered = wait_until(lambda: (head(a_id).get("clock") or {}).get("waiting_on") == 0, 30, 0.25)
        a_view = head(a_id)
        theirs = [e for e in a_view.get("history") or [] if not e.get("you")]
        reply = tool("respond_negotiation", {"negotiation_id": a_id, "action": "reject", "message": "QA: good talk."})
        r.check("model A answered in its chat (after its 3 s), its status waiting on alice with her deadline; she "
                "answered back and the chat closed",
                bool(answered) and bool(theirs) and reply.get("ok") is True
                and (a_view.get("clock") or {}).get("kind") == "person",
                f"history {[(e.get('by'), e.get('action'), e.get('message')) for e in a_view.get('history') or []]}")
        b_closed = wait_until(lambda: head(b_id).get("status") == "rejected", 40, 0.25)
        b_took = time.time() - b_opened
        b_view = head(b_id)
        b_last = (b_view.get("history") or [{}])[-1]
        queued = [s for s in mine.statuses(b_id) if s["state"] == "queued"]
        r.check("model B's answer queued behind A's on the one-slot machine (queued, with its place), then took longer "
                "than its 20 s, queue time included: the chat clock declined for it, '(no reply in time)'",
                bool(b_closed) and b_last.get("message") == "(no reply in time)" and not b_last.get("you")
                and bool(queued) and queued[0].get("position") == 1 and 18 <= b_took <= 30,
                f"after {b_took:.1f} s; last entry {b_last}; queued {queued[:1]}", seconds=round(b_took, 1))
        c_closed = wait_until(lambda: head(c_id).get("status") == "expired", 40, 0.25)
        c_view = head(c_id)
        r.check("the bot answered at once; alice let it wait, and her chat closed as expired at 20 s, "
                "'(no reply in time)'",
                bool(c_closed) and any(not e.get("you") and e.get("action") == "reply" for e in c_view.get("history") or [])
                and (c_view.get("history") or [{}])[-1].get("note") == "(no reply in time)",
                f"history {[(e.get('by'), e.get('action'), e.get('note')) for e in c_view.get('history') or []]}",
                seconds=round(time.time() - t0, 1))
        ended = tool("end_turn", {})
        r.check("with every chat settled, alice's turn ended", ended.get("ok") is True, json.dumps(ended)[:300])

        # ---- the rest of the game: alice ends her turns, and answers the models' trades on turn 3
        decided: dict = {}
        deadline = time.time() + 600
        while time.time() < deadline:
            gi = game()
            if gi.get("phase") != "playing":
                break
            waiting = [s for s in mine.statuses() if s["state"] == "waiting" and s.get("waiting_on") == 0
                       and s["negotiation"] not in decided and s["negotiation"] not in (a_id, b_id, c_id)]
            for s in waiting:
                nid = s["negotiation"]
                view = head(nid)
                if view.get("status") != "open" or not (view.get("clock") or {}).get("waiting_on") == 0:
                    continue
                if view.get("with") == 1:
                    decided[nid] = ("accept", tool("respond_negotiation", {"negotiation_id": nid, "action": "accept",
                                                                           "message": "QA: agreed."}), time.time())
                else:
                    decided[nid] = ("ignore", {"ok": True}, time.time())
            if gi.get("current_player") == 0:
                view = alice.get(f"/api/games/{gid}/view", params={"token": tokens[0]}).json()
                if not (view.get("diplomacy") or {}).get("open_negotiations"):
                    tool("end_turn", {})
            time.sleep(0.4)
        gi = game()
        r.check("the game was played to its end (turn limit 8)", gi.get("phase") != "playing",
                f"phase {gi.get('phase')}, turn {gi.get('turn')}", seconds=round(time.time() - t0, 1))
        trades = {nid: head(nid) for nid in decided}
        accepted = [v for v in trades.values() if v.get("status") == "accepted" and v.get("with") == 1]
        expired = [v for v in trades.values() if v.get("status") == "expired" and v.get("with") == 2]
        r.check("on turn 3 both models opened a trade with alice and waited: she accepted A's, B's expired after 20 s "
                "('(no reply in time)'), and each model's turn then ended",
                len(accepted) >= 1 and len(expired) >= 1
                and all((v.get("history") or [{}])[-1].get("note") == "(no reply in time)" for v in expired),
                json.dumps({nid: [d[0], v.get("status"), v.get("turn")] for nid, (d, v) in
                            {n: (decided[n], trades[n]) for n in trades}.items()}))
        # ---- what each socket heard
        states = {s["state"] for s in mine.statuses()}
        waiting = [s for s in mine.statuses() if s["state"] == "waiting" and s.get("deadline")]
        kinds = {s.get("kind") for s in waiting}
        r.check("alice's socket got every status of her chats: queued (with a place), thinking, waiting (with a "
                "deadline, on a model and on her), closed",
                {"queued", "thinking", "waiting", "closed"} <= states and {"model", "person"} <= kinds,
                f"states {sorted(states)}; waiting kinds {sorted(k for k in kinds if k)}",
                statuses=len(mine.statuses()))
        theirs = bots.statuses()
        r.check("the bot seat's socket got only the statuses of chats it is in",
                bool(theirs) and all(3 in (s.get("players") or []) for s in theirs),
                f"{len(theirs)} statuses, players {sorted({tuple(s.get('players') or []) for s in theirs})}")
        r.check("a spectator's socket got none while a person plays", spectator.statuses() == [],
                f"{len(spectator.statuses())} statuses; {len(spectator.messages)} messages in all")
        metrics = alice.get(f"/api/games/{gid}/metrics").json().get("summary") or {}
        m1, m2 = metrics.get("1") or {}, metrics.get("2") or {}
        r.check("the metrics count model A's answers and the clock's decline for B, with its cause",
                (m1.get("chat_answers") or {}).get("answered", 0) >= 1
                and (m2.get("chat_answers") or {}).get("declined_late", 0) >= 1
                and (m2.get("chat_fallback_causes") or {}).get("timeout", 0) >= 1,
                json.dumps({"1": m1.get("chat_answers"), "2": m2.get("chat_answers"),
                            "2_causes": m2.get("chat_fallback_causes")}))
        finished_replay(h, gid, tokens, data, gi)
        data.update({"chats": {"a": a_id, "b": b_id, "bot": c_id}, "b_declined_after_s": round(b_took, 1),
                     "turn3_trades": {str(k): v[0] for k, v in decided.items()},
                     "statuses_seen": sorted(states), "statuses": mine.statuses()[:200],
                     "bot_socket_statuses": len(theirs), "spectator_statuses": len(spectator.statuses()),
                     "metrics": {"1": m1.get("chat_answers"), "2": m2.get("chat_answers")}})
    finally:
        for s in sockets:
            s.close()
        if gid and not h.keep:
            try:
                alice.delete(f"/api/games/{gid}")
            except httpx.HTTPError:
                pass


def payloads_while_playing(h: Harness, gid: str, tokens: list, data: dict, tool) -> None:
    """3-13 (P3.5.8), during alice's first turn: the view's ETag and its 304, and trade options on demand."""
    r = h.report
    alice, bob = h.accounts["alice"], h.accounts["bob"]

    def view(tag=None):
        return alice.get(f"/api/games/{gid}/view", params={"token": tokens[0]},
                         headers={"If-None-Match": tag} if tag else {})

    # the models answer their chats meanwhile, so a look can race a change: the first of three tries that finds
    # the view unchanged between two looks shows the 304
    tries = []
    for _ in range(3):
        first = view()
        tag = first.headers.get("etag")
        again = view(tag)
        tries.append((first.status_code, bool(tag), again.status_code, len(again.content)))
        if again.status_code == 304:
            break
    r.check("alice's view answers with an ETag, and a refresh that sends it back gets 304 Not Modified, empty",
            any(t[0] == 200 and t[1] and t[2] == 304 and t[3] == 0 for t in tries), json.dumps(tries))
    # the game screen asks for its selected unit (get_unit) after every refresh: a query is a read, so it never moves
    # the tag, and the next refresh is still a 304 (the same three tries, for the same race)
    units = [u["id"] for u in first.json().get("units") or [] if u.get("owner") == 0] if first.status_code == 200 else []
    query_tries = []
    for _ in range(3 if units else 0):
        before = view()
        held = before.headers.get("etag")
        asked = tool("get_unit", {"unit_id": units[0]})
        still = view(held)
        query_tries.append((before.status_code, bool(held), asked.get("ok"), still.status_code, len(still.content)))
        if still.status_code == 304:
            break
    r.check("a query between two looks (get_unit, as the game screen asks for its selected unit) leaves the view as it "
            "was: the second look, with the first's tag, is still a 304",
            any(t[0] == 200 and t[1] and t[2] is True and t[3] == 304 and t[4] == 0 for t in query_tries),
            json.dumps({"units": units[:3], "tries": query_tries}))
    moved = tool("set_research", {"tech": "Pottery"})
    after = view(tag)
    r.check("after she sets her research the same tag gets the new view (200) under a new tag",
            moved.get("ok") is True and after.status_code == 200 and after.headers.get("etag") not in (None, tag),
            f"set_research {moved.get('ok') or moved.get('error')}; {after.status_code}")
    listed = (after.json().get("diplomacy") or {}).get("players") or [] if after.status_code == 200 else []
    opts = alice.get(f"/api/games/{gid}/trade-options/1", params={"token": tokens[0]})
    body = opts.json() if opts.status_code == 200 else {}
    own = alice.get(f"/api/games/{gid}/trade-options/0", params={"token": tokens[0]})
    stranger = bob.get(f"/api/games/{gid}/trade-options/1", params={"as_player": 0})
    r.check("her view lists the civilizations she met without trade options; the trade options route gives her "
            "those of one she met, and 404s for her own civilization and for a stranger",
            len(listed) == 3 and all("trade_options" not in p for p in listed)
            and opts.status_code == 200 and body.get("with") == 1
            and {"they_could_give", "you_could_give", "agreements_possible_now"} <= set(body.get("trade_options") or {})
            and own.status_code == 404 and stranger.status_code == 404,
            f"listed {[p.get('id') for p in listed]}; options {opts.status_code} {sorted(body.get('trade_options') or {})}; "
            f"own {own.status_code}; stranger {stranger.status_code}")
    data["payloads"] = {"etag_tries": tries, "query_tries": query_tries, "trade_options": body}


def finished_replay(h: Harness, gid: str, tokens: list, data: dict, gi: dict) -> None:
    """3-13 (P3.5.8): the finished game's replay is the journal's frames as stored, one per round."""
    import base64
    r = h.report
    got = h.accounts["alice"].get(f"/api/games/{gid}/replay", params={"token": tokens[0]})
    replay = got.json() if got.status_code == 200 else {}
    frames = replay.get("frames") or []
    heads = [base64.b64decode(f.get("bytes") or "")[:4] for f in frames]
    kinds = [b"CF\x01K" if f.get("keyframe") else b"CF\x01D" for f in frames]
    r.check("the finished game's replay is Delta: a keyframe, then a delta a round, each record a version 1 frame",
            got.status_code == 200 and replay.get("format") == "delta" and len(frames) >= 7
            and frames[0].get("keyframe") is True and not any(f.get("keyframe") for f in frames[1:])
            and heads == kinds and len(frames) == len(replay.get("stats") or []),
            f"{got.status_code}; format {replay.get('format')}; {len(frames)} frames, turns "
            f"{[f.get('turn') for f in frames]}; {len(got.content)} bytes; game turn {gi.get('turn')}",
            frames=len(frames), bytes=len(got.content))
    data["replay"] = {"frames": len(frames), "bytes": len(got.content), "keyframes": sum(1 for f in frames if f.get("keyframe"))}


def _hybrid_seat_routes(h: Harness, model: dict) -> None:
    """3-15's checks (P3.6.8): what the lobby, the Join/Seats dialog and the scenario seats send. On bob's machine
    (alice's grant: pooled prompts), the seat route makes a bot seat hybrid (all but war, a person's values) and back to
    a bot, and an LLM seat is refused her there (each change that makes a model seat is admitted again); a scenario
    saved with a hybrid seat launches with it, its model's machine named at launch. The games and the scenario go
    afterwards, kept or not: the browser checklist's D1 makes its own."""
    r = h.report
    alice = h.accounts["alice"]
    made: list = []
    sid = ""
    hybrid_seat = {"type": "hybrid", "llm": dict(model), "bot": {"profile": "standard"}, "handicap": "human",
                   "hybrid": {"owners": "all_but_war", "advisor": True, "prompt_every": 4}}
    config = {"map_size": "duel", "map_type": "pangaea", "seed": 3315, "barbarians": "off", "city_states": 0}
    try:
        g = alice.post("/api/games", json={"name": f"QA seat route {h.tag}", "config": config,
                                           "seats": [{"type": "human"}, {"type": "bot"}]})
        gid = g.json().get("id", "") if g.status_code == 200 else ""
        if gid:
            made.append(gid)
        to_hybrid = alice.post(f"/api/games/{gid}/seats/1", json=hybrid_seat)
        info = alice.get(f"/api/games/{gid}").json() if gid else {}
        seat, player = (info.get("seats") or [{}, {}])[1], (info.get("players") or [{}, {}])[1]
        label = next((p.get("label") for p in alice.get(f"/api/games/{gid}/summary").json().get("players") or []
                      if p.get("id") == 1), None) if gid else None
        to_llm = alice.post(f"/api/games/{gid}/seats/1", json={"type": "llm", "llm": dict(model)})
        kept = (alice.get(f"/api/games/{gid}").json().get("seats") or [{}, {}])[1].get("type") if gid else None
        to_bot = alice.post(f"/api/games/{gid}/seats/1", json={"type": "bot"})
        back = alice.get(f"/api/games/{gid}").json() if gid else {}
        r.check("the seat route makes a bot seat hybrid on bob's machine (all but war, a person's values); an LLM seat "
                "there is refused her (no grant for games), leaving it hybrid; then back to a bot with a bot's numbers",
                to_hybrid.status_code == 200 and seat.get("type") == "hybrid"
                and (seat.get("hybrid") or {}).get("preset") == "all_but_war" and player.get("handicap") == "human"
                and label == "Hybrid: standard + dry-run" and to_llm.status_code == 403 and kept == "hybrid"
                and to_bot.status_code == 200
                and (back.get("seats") or [{}, {}])[1].get("type") == "bot"
                and (back.get("players") or [{}, {}])[1].get("handicap") == "ai",
                f"game {g.status_code}; hybrid {to_hybrid.status_code} ({seat.get('type')}, "
                f"{(seat.get('hybrid') or {}).get('preset')}, {player.get('handicap')}, {label!r}); llm "
                f"{to_llm.status_code} (still {kept}); bot {to_bot.status_code} "
                f"({(back.get('players') or [{}, {}])[1].get('handicap')})")
        ed = alice.post("/api/scenario-editor", json={"source": "generate", "config": config,
                                                       "players": [{"type": "human"}, {"type": "hybrid"}]})
        eid = ed.json().get("editor_id", "") if ed.status_code == 200 else ""
        scenario_seat = {k: v for k, v in hybrid_seat.items() if k != "llm"}
        saved = alice.post(f"/api/scenario-editor/{eid}/save", json={"name": f"QA hybrid scenario {h.tag}",
                                                                    "seats": [{"type": "human"}, scenario_seat]})
        sid = saved.json().get("id", "") if saved.status_code == 200 else ""
        if eid:
            alice.delete(f"/api/scenario-editor/{eid}")
        listed = (alice.get(f"/api/scenarios/{sid}").json().get("seats") or [{}, {}])[1] if sid else {}
        launched = alice.post(f"/api/scenarios/{sid}/launch", json={"seats": [{}, {"llm": dict(model)}]})
        lgid = launched.json().get("id", "") if launched.status_code == 200 else ""
        if lgid:
            made.append(lgid)
        linfo = alice.get(f"/api/games/{lgid}").json() if lgid else {}
        lseat, lplayer = (linfo.get("seats") or [{}, {}])[1], (linfo.get("players") or [{}, {}])[1]
        r.check("a scenario saved with a hybrid seat (all but war, a person's values) launches with it",
                saved.status_code == 200 and (listed.get("hybrid") or {}).get("preset") == "all_but_war"
                and launched.status_code == 200 and lseat.get("type") == "hybrid"
                and (lseat.get("hybrid") or {}).get("preset") == "all_but_war" and lplayer.get("handicap") == "human",
                f"editor {ed.status_code}; saved {saved.status_code} ({(listed.get('hybrid') or {}).get('preset')}); "
                f"launched {launched.status_code}: {lseat.get('type')}, {(lseat.get('hybrid') or {}).get('preset')}, "
                f"{lplayer.get('handicap')}")
    finally:
        for gid in made:
            try:
                alice.delete(f"/api/games/{gid}")
            except httpx.HTTPError:
                pass
        if sid:
            try:
                alice.delete(f"/api/scenarios/{sid}")
            except httpx.HTTPError:
                pass


def _wait_runs(who: "Account", run_ids: list, limit: float) -> dict:
    """Poll the benchmark runs' list until each of ``run_ids`` is done and its hybrid jobs have their pair lines (or
    ``limit`` seconds pass); the runs by id as the list last gave them."""
    seen: dict = {}

    def done() -> bool:
        got = who.get("/api/benchmarks/runs")
        runs = {x["id"]: x for x in (got.json() if got.status_code == 200 else []) if isinstance(x, dict)}
        seen.update({rid: runs[rid] for rid in run_ids if rid in runs})
        return all(rid in runs and runs[rid].get("status") == "done"
                   and all((j.get("result") or {}).get("pair") for j in runs[rid].get("jobs") or []
                           if j.get("mode") == "hybrid") for rid in run_ids)
    wait_until(done, limit, 0.5)
    return seen


def _hybrid_benchmarks(h: Harness) -> None:
    """3-16's checks (design/phase3-features.md P3.6.9, P3.6.10). Bob's second dry-run machine (two models, two slots)
    is shared with alice for benchmarks and pooled prompts. She runs a hybrid suite on it with one model (a small
    pangaea with three bots, two seeds, 60 turns, the model deciding all but war): each job a hybrid seat with the
    advisor off, paired with its control game; then the same suite with the other model, whose jobs share those
    controls. The diplomacy leaderboard: her lines with her pairs named; bob, who may not see her private runs, none
    of them; the administrator every pair; a visitor 401. Then a hybrid probe run on the machine (an offer of gold, an
    offer of friendship, a message, a turn; trade only): the run records its model answering the gold and the message,
    its bot the friendship, and its turn case's forced diplomat phase."""
    r = h.report
    admin, alice, bob = (h.accounts[k] for k in ("admin", "alice", "bob"))
    data = r.data.setdefault("hybrid", {}).setdefault("benchmarks", {})
    made = bob.post("/api/servers", json={"name": f"QA hybrid bench machine {h.tag}", "kind": "test",
                                          "connection": {"provider": "dryrun", "dry_run_delay": 0.02,
                                                         "max_parallel": 2},
                                          "models": [{"key": "dry-run"}, {"key": "dry-run-b"}]})
    mid = made.json().get("id", "") if made.status_code == 200 else ""
    group = bob.post("/api/pool/groups", json={"name": f"QA hybrid bench group {h.tag}", "visibility": "private"})
    group_id = group.json().get("id") if group.status_code == 200 else ""
    put = bob.put(f"/api/pool/servers/{mid}", json={"group_id": group_id})
    grant = bob.post(f"/api/pool/groups/{group_id}/grants",
                     json={"subject_type": "user", "handle": alice.handle, "purposes": ["benchmark", "pooled"]})
    r.check("bob's second dry-run machine (two models, two slots) shared with alice for benchmarks and pooled prompts",
            bool(mid) and (group.status_code, put.status_code, grant.status_code) == (200, 200, 200),
            f"machine {made.status_code}, group {group.status_code}, put {put.status_code}, grant {grant.status_code}")
    data["machine"] = mid
    scenario = {"name": "QA hybrid pangaea", "mode": "hybrid", "map_size": "small", "map_type": "pangaea",
                "seed": 3361, "opponents": 3, "turn_limit": 60, "barbarians": "off", "city_states": 2,
                "max_turn_minutes": 5, "hybrid": {"owners": "all_but_war", "prompt_every": 3}}

    def suite(model: str) -> dict:
        return {"name": f"QA hybrid suite {h.tag}", "mode": "parallel", "repeats": 2,
                "servers": [{"server_id": mid, "models": [{"model": model}]}], "scenarios": [scenario]}
    t0 = time.time()
    first = alice.post("/api/benchmarks/runs", json={"suite": suite("dry-run"), "name": f"QA hybrid A {h.tag}"})
    run_a = first.json().get("id", "") if first.status_code == 200 else ""
    runs = _wait_runs(alice, [run_a], 300) if run_a else {}
    jobs_a = (runs.get(run_a) or {}).get("jobs") or []
    controls = [(j.get("control") or {}).get("id") for j in jobs_a]
    pairs = [(j.get("result") or {}).get("pair") or {} for j in jobs_a]
    r.check("her hybrid suite (one scenario, two seeds) ran two hybrid jobs, each paired with its own control game",
            first.status_code == 200 and len(jobs_a) == 2 and all(j.get("status") == "done" for j in jobs_a)
            and all(j.get("mode") == "hybrid" and j.get("owners") == "all_but_war" for j in jobs_a)
            and len(set(controls)) == 2 and all(controls)
            and all((j.get("control") or {}).get("status") == "done" for j in jobs_a)
            and all("end" in (p.get("score_diff") or {}) for p in pairs),
            f"HTTP {first.status_code} {first.text[:160] if first.status_code != 200 else ''}; "
            f"{[(j.get('status'), j.get('seed'), (j.get('control') or {}).get('status')) for j in jobs_a]}",
            seconds=round(time.time() - t0, 1))
    game = alice.get(f"/api/games/{jobs_a[0]['game_id']}") if jobs_a and jobs_a[0].get("game_id") else None
    seat = ((game.json().get("seats") or [{}])[0] if game is not None and game.status_code == 200 else {})
    r.check("a job's model plays a hybrid seat with the advisor off (it waits for benchmarks, never the bot's view)",
            seat.get("type") == "hybrid" and (seat.get("hybrid") or {}).get("advisor") is False,
            json.dumps({"type": seat.get("type"), "hybrid": seat.get("hybrid")})[:300])
    second = alice.post("/api/benchmarks/runs", json={"suite": suite("dry-run-b"), "name": f"QA hybrid B {h.tag}"})
    run_b = second.json().get("id", "") if second.status_code == 200 else ""
    runs = _wait_runs(alice, [run_b], 300) if run_b else {}
    jobs_b = (runs.get(run_b) or {}).get("jobs") or []
    r.check("the other model's run of the suite reuses the two cached control games",
            second.status_code == 200 and [(j.get("control") or {}).get("id") for j in jobs_b] == controls
            and all((j.get("result") or {}).get("pair") for j in jobs_b),
            f"HTTP {second.status_code}; controls {[(j.get('control') or {}).get('id') for j in jobs_b]} vs {controls}")

    def board(who: Optional["Account"]) -> tuple:
        got = (who.get("/api/models/diplomacy") if who is not None
               else h.anonymous().get("/api/models/diplomacy"))
        body = got.json() if got.status_code == 200 else {}
        return got.status_code, {(x.get("model"), x.get("owners")): x for x in body.get("lines") or []}, body
    code_a, lines_a, body_a = board(alice)
    mine_a = lines_a.get(("dry-run", "all_but_war")) or {}
    mine_b = lines_a.get(("dry-run-b", "all_but_war")) or {}
    r.check("the diplomacy leaderboard: her two models' lines (all but war), two pairs each, her pairs named, with the "
            "mean score difference and its 90% interval",
            code_a == 200 and mine_a.get("pairs") == 2 and mine_b.get("pairs") == 2
            and sorted(g.get("run_id") for g in mine_a.get("games") or []) == [run_a, run_a]
            and isinstance(mine_a.get("ci90"), list) and mine_a.get("mean_score_diff") is not None
            and "deals_per_game" in mine_a and "other_fallbacks" in mine_a,
            json.dumps({k: {f: v.get(f) for f in ("pairs", "mean_score_diff", "ci90", "deals_per_game",
                                                  "mean_deal_value", "fallbacks_per_game")}
                        for k, v in ((str(k), v) for k, v in lines_a.items())})[:600])
    code_b, lines_b, body_b = board(bob)
    code_z, lines_z, _ = board(admin)
    code_v, _lines_v, _ = board(None)
    zline = lines_z.get(("dry-run", "all_but_war")) or {}
    r.check("bob, who may not see her private runs, sees none of their pairs; the administrator sees every pair named; "
            "a visitor is refused",
            code_b == 200 and not any(k[0] in ("dry-run", "dry-run-b") for k in lines_b)
            and run_a not in json.dumps(body_b) and code_z == 200 and zline.get("pairs") == 2
            and len(zline.get("games") or []) == 2 and code_v == 401,
            f"bob {code_b} {sorted(lines_b)}; admin {code_z} {zline.get('pairs')}; visitor {code_v}")
    data.update({"runs": [run_a, run_b], "controls": controls,
                 "pairs": [{"seed": j.get("seed"), **{k: (j.get("result") or {}).get("pair", {}).get(k)
                                                      for k in ("score_diff", "deals", "deal_value_mean",
                                                                "acceptance_rate")}} for j in jobs_a + jobs_b],
                 "board": {str(k): {f: v.get(f) for f in ("pairs", "mean_score_diff", "ci90", "deals_per_game",
                                                          "mean_deal_value", "acceptance_rate",
                                                          "fallbacks_per_game", "other_fallbacks")}
                           for k, v in lines_a.items()}})

    # ---- a hybrid probe run: which side answered each case
    ed = alice.post("/api/scenario-editor", json={"source": "generate", "config": {
        "map_size": "duel", "map_type": "pangaea", "seed": 3363, "barbarians": "off", "city_states": 0},
        "players": [{"type": "human"}, {"type": "llm"}]})
    eid = ed.json().get("editor_id", "") if ed.status_code == 200 else ""
    if eid:
        alice.post(f"/api/scenario-editor/{eid}/ops", json={"ops": [
            {"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": "all", "gold": 500}]})
    saved = alice.post(f"/api/scenario-editor/{eid}/save", json={"name": f"QA hybrid probe {h.tag}"})
    scn = saved.json().get("id", "") if saved.status_code == 200 else ""
    if eid:
        alice.delete(f"/api/scenario-editor/{eid}")
    pid = f"qa-hybrid-probe-{h.tag}"
    dof = [{"type": "declaration_of_friendship"}]
    put_probe = alice.put(f"/api/probes/{pid}", json={
        "name": "QA hybrid probe", "scenario": scn, "subject": 1, "counterparty": 0, "cases": [
            {"id": "gift", "kind": "offer", "message": "A gift.", "give": [{"type": "gold", "amount": 100}],
             "receive": []},
            {"id": "friends", "kind": "offer", "message": "Friends?", "give": dof, "receive": dof},
            {"id": "hello", "kind": "message", "message": "Greetings."},
            {"id": "turn", "kind": "turn"}]})
    run = alice.post(f"/api/probes/{pid}/run", json={"llm": {"server_id": mid, "model": "dry-run"}, "mode": "hybrid",
                                                     "owners": "trade_only"})
    rid = run.json().get("id", "") if run.status_code == 200 else ""

    def probe_done() -> dict:
        got = alice.get(f"/api/probe-runs/{rid}")
        body = got.json() if got.status_code == 200 else {}
        return body if (body.get("run") or {}).get("status") in ("finished", "failed", "stopped") else {}
    body = wait_until(probe_done, 180, 0.5) if rid else {}
    results = {x.get("case"): x for x in (body or {}).get("results") or []}
    sides = {c: x.get("answered_by") for c, x in results.items()}
    turn_phases = (results.get("turn") or {}).get("diplomat") or []
    r.check("a hybrid probe run records which side answered each case: its model the gold and the message (trade "
            "only), its bot the friendship, its turn case's forced diplomat phase the model's",
            run.status_code == 200 and ((body or {}).get("run") or {}).get("status") == "finished"
            and ((body or {}).get("run") or {}).get("mode") == "hybrid"
            and sides == {"gift": "model", "friends": "bot", "hello": "model", "turn": "model"}
            and [x.get("outcome") for x in turn_phases] == ["done"],
            f"scenario {saved.status_code}, probe {put_probe.status_code}, run {run.status_code} {run.text[:160]}; "
            f"status {((body or {}).get('run') or {}).get('status')}; sides {sides}; "
            f"phases {[x.get('outcome') for x in turn_phases]}")
    data["probe"] = {"run": rid, "answered_by": sides,
                     "outcomes": {c: x.get("outcome") for c, x in results.items()}}


def part_hybrid(h: Harness) -> None:
    """3-14's checks, the server half of hybrid seats (design/phase3-features.md P3.6.2-P3.6.7, P3.6.11): bob's dry-run
    machine (one slot) is shared with alice for pooled prompts only. Her game seats her person, a hybrid seat on bob's
    machine (the standard bot with trades and talk its model's, ``prompt_every`` 3) and two bots, 30 rounds, the model's
    chat timeout 20 s. A model seat that plays every turn on bob's machine is refused her (no grant for games); the
    hybrid seat is the engine's hybrid controller with the AI's handicap. On her first turn she offers the seat a trade
    (its model answers) and a declaration of friendship (its bot answers, the model never asked), on her second a trade
    whose message keeps the dry-run model past its 20 s (the chat clock's fallback bot answers, cause ``timeout``;
    the engine takes two openings a turn between a pair). She plays the game
    to its end, answering the seat's offers. The evidence: the diplomat's phases (three turns apart, or sooner on a
    trigger, each done), the chat answers by the model and by the bot, the fallback with its cause, her socket's
    statuses of the seat's chats (kind ``hybrid``), and bob's machine never holding a lease."""
    r = h.report
    alice, bob = h.accounts["alice"], h.accounts["bob"]
    data = r.data.setdefault("hybrid", {})
    sockets: list = []
    gid = ""
    timeout = 20
    try:
        made = bob.post("/api/servers", json={"name": f"QA hybrid machine {h.tag}", "kind": "test",
                                              "connection": {"provider": "dryrun", "dry_run_delay": 0.3,
                                                             "max_parallel": 1},
                                              "models": [{"key": "dry-run"}]})
        mid = made.json().get("id", "") if made.status_code == 200 else ""
        group = bob.post("/api/pool/groups", json={"name": f"QA hybrid group {h.tag}", "visibility": "private"})
        group_id = group.json().get("id") if group.status_code == 200 else ""
        put = bob.put(f"/api/pool/servers/{mid}", json={"group_id": group_id})
        grant = bob.post(f"/api/pool/groups/{group_id}/grants",
                         json={"subject_type": "user", "handle": alice.handle, "purposes": ["pooled"]})
        r.check("bob's dry-run machine (one slot) shared with alice for pooled prompts only",
                bool(mid) and (group.status_code, put.status_code, grant.status_code) == (200, 200, 200)
                and grant.json().get("purposes") == ["pooled"],
                f"machine {made.status_code}, group {group.status_code}, put {put.status_code}, grant "
                f"{grant.status_code}")
        data["machine"] = mid
        model = {"server_id": mid, "model": "dry-run"}
        config = {"map_size": "small", "map_type": "pangaea", "seed": 3341, "barbarians": "off", "city_states": 0,
                  "turn_limit": 30, "chat_model_reply_seconds": timeout, "chat_person_reply_seconds": 60}
        full = alice.post("/api/games", json={"name": f"QA no full seat {h.tag}", "config": config,
                                              "seats": [{"type": "human"}, {"type": "llm", "llm": dict(model)}]})
        r.check("a seat that plays every turn on bob's machine is refused her: her grant covers pooled prompts only",
                full.status_code == 403, f"HTTP {full.status_code}: {full.text[:200]}")
        _hybrid_seat_routes(h, model)
        hybrid_seat = {"type": "hybrid", "llm": dict(model), "bot": {"profile": "standard"},
                       "hybrid": {"owners": "trade_only", "prompt_every": 3}}
        g = alice.post("/api/games", json={"name": f"QA hybrid game {h.tag}", "config": config,
                                           "seats": [{"type": "human"}, hybrid_seat, {"type": "bot"},
                                                     {"type": "bot"}]})
        info = g.json() if g.status_code == 200 else {}
        gid = info.get("id", "")
        tokens = [seat["token"] for seat in info.get("seats") or []]
        seat = (info.get("seats") or [{}, {}])[1]
        r.check("alice's game seats the hybrid seat on bob's machine: trades and talk its model's, prompt every 3",
                bool(gid) and seat.get("type") == "hybrid" and (seat.get("hybrid") or {}).get("preset") == "trade_only"
                and (seat.get("hybrid") or {}).get("prompt_every") == 3,
                f"HTTP {g.status_code}; seat {json.dumps(seat.get('hybrid'))}")
        if not gid:
            return
        data["game"] = gid
        met = alice.post(f"/api/games/{gid}/debug/meet_all")
        rich = alice.post(f"/api/games/{gid}/debug/gold")           # 500 gold each, for the trades
        mine = Socket(h, gid, tokens[0])
        sockets.append(mine)

        def tool(name: str, args: dict) -> dict:
            got = alice.post(f"/api/games/{gid}/tool", params={"token": tokens[0]}, json={"tool": name, "args": args})
            return got.json() if got.status_code == 200 else {"ok": False, "error": got.text}

        def head(nid: int) -> dict:
            got = alice.get(f"/api/games/{gid}/negotiations/{nid}", params={"token": tokens[0]})
            return got.json() if got.status_code == 200 else {}

        def game() -> dict:
            got = alice.get(f"/api/games/{gid}")
            return got.json() if got.status_code == 200 else {}

        def metrics() -> dict:
            got = alice.get(f"/api/games/{gid}/metrics")
            return got.json() if got.status_code == 200 else {}

        def rows(nid: int) -> list:
            return [(x.get("outcome"), x.get("cause")) for x in metrics().get("negotiations") or []
                    if x.get("nid") == nid]

        controller = alice.get(f"/api/games/{gid}/summary").json()
        hybrid_row = next((p for p in controller.get("players") or [] if p.get("id") == 1), {})
        r.check("the summary labels the seat (its bot and its model) with its task class",
                hybrid_row.get("label") == "Hybrid: standard + dry-run" and hybrid_row.get("class") == "partial",
                json.dumps(hybrid_row)[:300])

        # ---- turn 1: three chats with the hybrid seat
        t0 = time.time()
        trade = tool("open_negotiation", {"to": 1, "message": "QA-T: maps for gold?",
                                          "give": [{"type": "gold", "amount": 25}],
                                          "receive": [{"type": "share_map"}]})
        t_id = (trade.get("result") or {}).get("negotiation_id")
        answered = wait_until(lambda: head(t_id).get("status") not in (None, "open"), 30, 0.25)
        r.check("a trade (its model's): the model answered it, accepting, with no fallback",
                bool(met.status_code == 200 and rich.status_code == 200 and answered)
                and head(t_id).get("status") == "accepted" and rows(t_id) == [("answered", None)],
                f"meet_all {met.status_code}, gold {rich.status_code}; {trade.get('ok') or trade.get('error')}; "
                f"status {head(t_id).get('status')}; rows {rows(t_id)}", seconds=round(time.time() - t0, 1))
        friends = tool("open_negotiation", {"to": 1, "message": "QA-F: friends?",
                                            "give": [{"type": "declaration_of_friendship"}],
                                            "receive": [{"type": "declaration_of_friendship"}]})
        f_id = (friends.get("result") or {}).get("negotiation_id")
        by_bot = wait_until(lambda: (head(f_id).get("clock") or {}).get("waiting_on") != 1, 10, 0.1)
        f_view = head(f_id)
        r.check("a declaration of friendship (an agreement: the bot's): its bot answered at once, the model never asked",
                bool(friends.get("ok") and by_bot) and rows(f_id) == []
                and any(not e.get("you") for e in f_view.get("history") or []),
                f"{friends.get('ok') or friends.get('error')}; status {f_view.get('status')}; rows {rows(f_id)}; "
                f"history {[(e.get('by'), e.get('action')) for e in f_view.get('history') or []]}")
        # the engine takes two openings a turn between a pair: the third chat is her next turn's
        ended = tool("end_turn", {})
        wait_until(lambda: game().get("current_player") == 0 and game().get("turn") == 2, 60, 0.25)
        slow = tool("open_negotiation", {"to": 1, "message": "[dry run: wait 45 s] QA-S: gold for maps?",
                                         "give": [{"type": "gold", "amount": 20}],
                                         "receive": [{"type": "share_map"}]})
        s_id = (slow.get("result") or {}).get("negotiation_id")
        s_opened = time.time()
        fell_back = wait_until(lambda: head(s_id).get("status") not in (None, "open")
                               or (head(s_id).get("clock") or {}).get("waiting_on") == 0, 40, 0.25)
        s_took = time.time() - s_opened
        s_view = head(s_id)
        r.check("a trade the model is too slow for (45 s against its 20 s): the chat clock's fallback bot answered it "
                "at 20 s, cause timeout",
                bool(ended.get("ok") and fell_back) and ("fallback", "timeout") in rows(s_id) and 18 <= s_took <= 30
                and (s_view.get("history") or [{}])[-1].get("message") != "(no reply in time)",
                f"turn 1 ended {ended.get('ok') or ended.get('error')}; {slow.get('ok') or slow.get('error')}; after "
                f"{s_took:.1f} s; rows {rows(s_id)}; last {(s_view.get('history') or [{}])[-1]}",
                seconds=round(s_took, 1))
        for nid in (f_id, s_id):
            if head(nid).get("status") == "open" and (head(nid).get("clock") or {}).get("waiting_on") == 0:
                tool("respond_negotiation", {"negotiation_id": nid, "action": "reject", "message": "QA: no."})

        # ---- the rest of the game: she ends her turns and answers the seat's offers
        answered_offers: dict = {}
        deadline = time.time() + 900
        while time.time() < deadline:
            gi = game()
            if gi.get("phase") != "playing":
                break
            for s in mine.statuses():
                nid = s["negotiation"]
                if s.get("state") != "waiting" or s.get("waiting_on") != 0 or nid in answered_offers:
                    continue
                view = head(nid)
                if view.get("status") == "open" and (view.get("clock") or {}).get("waiting_on") == 0:
                    answered_offers[nid] = tool("respond_negotiation", {"negotiation_id": nid, "action": "accept",
                                                                         "message": "QA: agreed."}).get("ok")
            if gi.get("current_player") == 0:
                view = alice.get(f"/api/games/{gid}/view", params={"token": tokens[0]}).json()
                if not (view.get("diplomacy") or {}).get("open_negotiations"):
                    tool("end_turn", {})
            time.sleep(0.3)
        gi = game()
        r.check("the game was played to its end (turn limit 30)", gi.get("phase") != "playing",
                f"phase {gi.get('phase')}, turn {gi.get('turn')}", seconds=round(time.time() - t0, 1))
        m = metrics()
        phases = [x for x in m.get("diplomat") or [] if x.get("player") == 1]
        turns = [x["turn"] for x in phases]
        scheduled = [x for x in phases if any(t.startswith("it is 3 turns") for t in x.get("triggers") or [])]
        r.check("the diplomat's phases: one at most a turn, every one done, at least every three turns",
                len(phases) >= 9 and len(turns) == len(set(turns)) and all(x["outcome"] == "done" for x in phases)
                and all(b - a <= 3 for a, b in zip([1] + turns, turns + [30])) and len(scheduled) >= 3,
                f"{len(phases)} phases on turns {turns}; outcomes {sorted({x['outcome'] for x in phases})}; "
                f"{len(scheduled)} on schedule", phases=len(phases))
        summary = (m.get("summary") or {}).get("1") or {}
        r.check("the seat's summary: its turns its bot's (bot actions), its phases and its chat answers",
                summary.get("controller") == "hybrid" and summary.get("diplomat_phases") == len(phases)
                and summary.get("avg_bot_actions", 0) > 0
                and (summary.get("chat_answers") or {}).get("answered", 0) >= 1
                and (summary.get("chat_answers") or {}).get("fallback", 0) >= 1
                and (summary.get("chat_fallback_causes") or {}).get("timeout", 0) >= 1,
                json.dumps({k: summary.get(k) for k in ("controller", "diplomat_phases", "diplomat_outcomes",
                                                         "chat_answers", "chat_fallback_causes",
                                                         "avg_bot_actions")}))
        hybrid_statuses = [s for s in mine.statuses() if s.get("kind") == "hybrid"]
        states = {s["state"] for s in mine.statuses() if 1 in (s.get("players") or [])}
        r.check("her socket heard the seat's chats as a hybrid seat's (waiting with a deadline, the model's queued or "
                "thinking, closed)",
                bool(hybrid_statuses) and any(s.get("deadline") for s in hybrid_statuses)
                and ({"thinking", "queued"} & states) and "closed" in states,
                f"{len(hybrid_statuses)} statuses of kind hybrid; states {sorted(states)}")
        q = bob.get("/api/queue")
        box = next((x for x in (q.json() if q.status_code == 200 else {}).get("machines") or []
                    if x.get("server_id") == mid), {})
        r.check("bob's machine holds no lease for the hybrid seat (its prompts are tickets)",
                q.status_code == 200 and not box.get("leases"), json.dumps(box)[:300])
        _hybrid_benchmarks(h)
        data.update({"chats": {"trade": t_id, "friendship": f_id, "slow": s_id}, "fallback_after_s": round(s_took, 1),
                     "phases": [{k: x.get(k) for k in ("turn", "triggers", "steps", "outcome", "tools")}
                                for x in phases],
                     "offers_answered": {str(k): v for k, v in answered_offers.items()},
                     "summary": {k: summary.get(k) for k in ("diplomat_phases", "diplomat_outcomes", "diplomat_tools",
                                                             "chat_answers", "chat_fallback_causes")},
                     "statuses": len(mine.statuses()), "hybrid_statuses": len(hybrid_statuses)})
    finally:
        for s in sockets:
            s.close()
        if gid and not h.keep:
            try:
                alice.delete(f"/api/games/{gid}")
            except httpx.HTTPError:
                pass


# ---------------------------------------------------------------------------- the minimap (3-17)

#: The minimap part's game: a standard map that wraps east-west, where seed 3242 starts the person's settler three
#: columns from the seam, so its home runs over it once it has seen enough.
MINIMAP_CONFIG = {"map_size": "standard", "map_type": "continents", "map_edges": "wrap_x", "seed": 3242,
                  "barbarians": "off", "city_states": 0, "turn_limit": 400}
#: The terrains a scout may not be put on.
WATER = ("Ocean", "Coast", "Lakes")


def _axis(lo: int, hi: int, n: int) -> list:
    """The positions an inclusive interval covers on an axis of ``n``, the short way round when ``lo > hi``."""
    out, i = [], lo
    while True:
        out.append(i)
        if i == hi:
            return out
        i = (i + 1) % n


def _box_covers(outer: dict, inner: dict, w: int, h: int) -> bool:
    """Whether box ``outer`` covers box ``inner`` (corners inclusive, across a seam when x0 > x1 or y0 > y1)."""
    xs, ys = set(_axis(outer["x0"], outer["x1"], w)), set(_axis(outer["y0"], outer["y1"], h))
    return set(_axis(inner["x0"], inner["x1"], w)) <= xs and set(_axis(inner["y0"], inner["y1"], h)) <= ys


def _arc(marked: set, n: int, wrap: bool):
    """The shortest interval of ``0..n`` covering ``marked``, as ``(lo, hi)`` inclusive (``lo > hi`` across the seam of
    an axis that wraps), or None: of arcs as short, one that does not cross the seam, else the first to start (the
    engine's choice)."""
    if not marked:
        return None
    if not wrap:
        return min(marked), max(marked)
    length, _, start = min((span, s + span > n, s) for s in range(n)
                           for span in [max((i - s) % n for i in marked) + 1])
    return start, (start + length - 1) % n


def _joined_home(view: dict, anchor) -> tuple:
    """The land the seat explored joined to ``anchor`` (``[x, y]``) through land it explored, worked out from its view
    alone (each tile the view lists is explored; land is any terrain not in ``WATER``), and that land's box covered the
    short way round: what the engine's home box must be (design/phase3-features.md P3.7.2), no more, so that it tells
    the seat nothing its own view does not. ``(None, set())`` without an anchor, or for one at sea or not explored."""
    w, h = view["width"], view["height"]
    wraps = (bool(view.get("wrap_x")), bool(view.get("wrap_y")))
    land = {t[0] for t in view.get("tiles") or [] if t[1] not in WATER}
    start = anchor[1] * w + anchor[0] if anchor else -1
    if start not in land:
        return None, set()
    seen, todo = {start}, [start]
    while todo:
        i = todo.pop()
        x, y = i % w, i // w
        steps = ((1, 0), (1, -1), (0, -1), (-1, 0), (0, 1), (1, 1)) if y & 1 else \
            ((1, 0), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1))
        for dx, dy in steps:
            nx, ny = (x + dx) % w if wraps[0] else x + dx, (y + dy) % h if wraps[1] else y + dy
            n = ny * w + nx
            if 0 <= nx < w and 0 <= ny < h and n in land and n not in seen:
                seen.add(n)
                todo.append(n)
    xs, ys = _arc({i % w for i in seen}, w, wraps[0]), _arc({i // w for i in seen}, h, wraps[1])
    return {"x0": xs[0], "y0": ys[0], "x1": xs[1], "y1": ys[1]}, seen


def _home_touches(view: dict, home: dict) -> bool:
    """Whether every edge of the home box is touched by a tile the seat explored inside it: the box is no wider than
    what it saw (the engine's own tests check it against the continents)."""
    w, h = view["width"], view["height"]
    xs, ys = _axis(home["x0"], home["x1"], w), _axis(home["y0"], home["y1"], h)
    seen = {(t[0] % w, t[0] // w) for t in view["tiles"]}
    inside = {(x, y) for x, y in seen if x in xs and y in ys}
    columns = all(any(x == edge for x, _ in inside) for edge in (xs[0], xs[-1]))
    return columns and all(any(y == edge for _, y in inside) for edge in (ys[0], ys[-1]))


def part_minimap(h: Harness) -> None:
    """3-17's checks (design/phase3-features.md P3.7): alice's person seat and two bots on a standard map that wraps
    east-west (``MINIMAP_CONFIG``), her game held at its first turn (she plays it, and does not end it). Through
    ``POST /api/games/{gid}/debug/ops``, the route a script moves a live game on with (P3.7.5), she founds her capital
    where her settler stands, puts scouts on land at the west and east edges of her home box, which explore past them,
    takes the techs of the eras before the Renaissance (``grant_era``), puts more scouts at the edges, takes those
    before the Information era, and has the whole map revealed. After each step her view's minimap block (P3.7.2):
    its stage moves from early to renaissance to information; its home box only grows, the short way round the seam,
    and is at every step the box of the land her view lists joined to her capital (her settler before it) through land
    it lists, no wider (``_joined_home``); at the end it is her whole continent, over the seam. Each step's
    operations, answers, block and explored tiles are in the evidence. The route's gate: another member 404, a
    visitor 401 or 404, an administrator 200. With ``--keep`` the game and the server stay for the browser
    checklist's F1."""
    r = h.report
    alice, bob, admin = h.accounts["alice"], h.accounts["bob"], h.accounts["admin"]
    data = r.data.setdefault("minimap", {})
    gid = ""
    try:
        g = alice.post("/api/games", json={"name": f"QA minimap game {h.tag}", "config": dict(MINIMAP_CONFIG),
                                           "seats": [{"type": "human"}, {"type": "bot"}, {"type": "bot"}]})
        info = g.json() if g.status_code == 200 else {}
        gid = info.get("id", "")
        token = (info.get("seats") or [{}])[0].get("token", "")

        def view() -> dict:
            got = alice.get(f"/api/games/{gid}/view", params={"token": token})
            return got.json() if got.status_code == 200 else {}

        first = view() if gid else {}
        r.check("alice's game: her person seat and two bots on a standard map that wraps east-west, larger than the "
                "early window (44 by 28)",
                bool(gid) and first.get("wrap_x") is True and first.get("width", 0) > 44
                and first.get("height", 0) > 28 and first.get("you") == 0,
                f"game {g.status_code}; {first.get('width')}x{first.get('height')}, wrap_x {first.get('wrap_x')}")
        if not gid:
            return
        data.update({"game": gid, "seat_token": token, "config": MINIMAP_CONFIG,
                     "map": {"width": first["width"], "height": first["height"], "wrap_x": first["wrap_x"]}})

        # ---- the route's gate (P3.9.1): it is the dev server's (CITAR_DEBUG=1), for whoever manages the game
        stranger = bob.post(f"/api/games/{gid}/debug/ops", json=[{"op": "reveal", "player": 0}])
        with h.anonymous() as anon:
            visitor = anon.post(f"/api/games/{gid}/debug/ops", json=[{"op": "reveal", "player": 0}])
        by_admin = admin.post(f"/api/games/{gid}/debug/ops", json=[])
        refused = alice.post(f"/api/games/{gid}/debug/ops", json=[{"op": "no_such_op"}])
        r.check("the debug ops route: another member 404, a visitor 401 or 404, an administrator 200, an unknown "
                "operation 400 naming it", stranger.status_code == 404 and visitor.status_code in (401, 404)
                and by_admin.status_code == 200 and refused.status_code == 400
                and "no_such_op" in refused.text,
                f"bob {stranger.status_code}, visitor {visitor.status_code}, admin {by_admin.status_code} "
                f"{by_admin.text[:80]}, unknown {refused.status_code} {refused.text[:120]}")

        steps: list = []

        def anchor_of(v: dict, block: dict):
            """Where her home is read from: her capital, else her first settler."""
            if block.get("capital"):
                return block["capital"]
            settlers = sorted((u for u in v.get("units") or [] if u.get("owner") == 0 and u.get("type") == "Settler"),
                              key=lambda u: u.get("id", 0))
            return [settlers[0]["x"], settlers[0]["y"]] if settlers else None

        def record(label: str, ops: list, answer) -> dict:
            v = view()
            block = v.get("minimap") or {}
            joined, _tiles = _joined_home(v, anchor_of(v, block)) if v else (None, set())
            results = None
            if answer is not None:
                results = answer.json().get("results") if answer.status_code == 200 else answer.text[:300]
            steps.append({"step": label, "ops": ops, "status": answer.status_code if answer is not None else None,
                          "results": results, "stage": block.get("stage"), "capital": block.get("capital"),
                          "home": block.get("home"), "explored": len(v.get("tiles") or []), "turn": v.get("turn"),
                          "within_explored": _home_touches(v, block["home"]) if block.get("home") else None,
                          "joined_land": joined, "is_joined_land": bool(joined) and joined == block.get("home")})
            return v

        def send(label: str, ops: list) -> dict:
            return record(label, ops, alice.post(f"/api/games/{gid}/debug/ops", json=ops))

        def scouts_at_edges(v: dict) -> list:
            """A scout on a tile of her home land (explored, and joined to her capital through explored land) at the
            west and at the east edge of the home box, on its middle row or the nearest."""
            block = v.get("minimap") or {}
            home, w = block.get("home"), v["width"]
            if not home:
                return []
            land = [(i % w, i // w) for i in _joined_home(v, anchor_of(v, block))[1]]
            rows = _axis(home["y0"], home["y1"], v["height"])
            middle = rows[len(rows) // 2]
            out = []
            for edge in (home["x0"], home["x1"]):
                spots = sorted((abs(y - middle), x, y) for x, y in land if x == edge and y in rows)
                if spots:
                    out.append({"op": "add_unit", "player": 0, "unit": "Scout", "x": spots[0][1], "y": spots[0][2]})
            return out

        v = record("the start", [], None)
        settler = next((u for u in v.get("units") or [] if u.get("owner") == 0 and u.get("type") == "Settler"), None)
        start = steps[-1]
        r.check("at the start the block is early, with no capital and the home box of what her settler's party "
                "explored", start["stage"] == "early" and start["capital"] is None and bool(start["home"])
                and settler is not None and _home_touches(v, start["home"]),
                json.dumps({k: start[k] for k in ("stage", "capital", "home", "explored")}))
        if settler is None:
            return
        v = send("her capital founded where the settler stands",
                 [{"op": "found_city", "player": 0, "x": settler["x"], "y": settler["y"], "capital": True}])
        v = send("scouts at the home box's west and east edges", scouts_at_edges(v))
        v = send("the techs of the eras before the Renaissance",
                 [{"op": "grant_era", "player": 0, "era": "Renaissance era"}])
        v = send("more scouts at the edges", scouts_at_edges(v))
        v = send("more scouts at the edges, again", scouts_at_edges(v))
        v = send("the techs of the eras before the Information era",
                 [{"op": "grant_era", "player": 0, "era": "Information era"}])
        v = send("the whole map revealed", [{"op": "reveal", "player": 0}])
        data["steps"] = steps
        w, hgt = v.get("width", 0), v.get("height", 0)
        r.check("every step's operations were applied through the route, each with its result (200)",
                all(s["status"] == 200 and len(s["results"] or []) == len(s["ops"]) and s["ops"] for s in steps[1:]),
                json.dumps([(s["step"], s["status"], len(s["ops"])) for s in steps]))
        stages = [s["stage"] for s in steps]
        r.check("the stage moved from early to renaissance to information as the seat took each era's techs",
                stages == ["early"] * 3 + ["renaissance"] * 3 + ["information"] * 2, json.dumps(stages))
        r.check("the capital is where the settler stood from the second step on",
                all(s["capital"] == [settler["x"], settler["y"]] for s in steps[1:]),
                json.dumps([s["capital"] for s in steps]))
        homes = [s["home"] for s in steps]
        grew = all(a and b and _box_covers(b, a, w, hgt) for a, b in zip(homes, homes[1:]))
        r.check("the home box only grew, the short way round the seam, and grew with the scouts' exploring",
                grew and homes[2] != homes[1] and homes[4] != homes[3] and steps[-1]["explored"] == w * hgt,
                json.dumps([(s["step"], s["home"], s["explored"]) for s in steps]))
        r.check("at every step the home box is the box of the land her view lists joined to her capital (her "
                "settler before it) through land it lists: no wider, so it tells her nothing her view does not",
                all(s["is_joined_land"] and s["within_explored"] for s in steps),
                json.dumps([(s["step"], s["home"], s["joined_land"]) for s in steps]))
        r.check("revealed, the home box is her whole continent, across the seam (x0 > x1)",
                homes[-1]["x0"] > homes[-1]["x1"] and homes[-1] != homes[-2], json.dumps(homes[-1]))
    finally:
        if gid and not h.keep:
            try:
                alice.delete(f"/api/games/{gid}")
            except httpx.HTTPError:
                pass


def _qa_scan(*, cpu: str, gpu: str, vram: float, idle: int, cpu_w: int, gpu_w: int, host: str = "qa-settings-box") -> dict:
    """A hardware file as the collectors write one (``hwinfo.collect``'s shape), for a helper's ``--hardware``."""
    return {"format": "citar-hardware", "collector_version": 1, "collected_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
            "hostname": host, "os": {"system": "Linux", "release": "6.8", "arch": "x86_64"},
            "system": {"form": "desktop"}, "cpu": {"model": cpu, "cores": 8, "threads": 16},
            "memory": {"ram_gb": 64, "unified": False}, "gpus": [{"name": gpu, "vram_gb": vram}],
            "physical_disks": [{"name": "QA SSD", "size_gb": 2000, "type": "SSD"}], "volumes": [], "runtimes": [],
            "power": {"gpu_sampling": False, "suggested": {"idle_w": idle, "cpu_max_w": cpu_w, "gpu_max_w": gpu_w,
                                                           "source": "guess", "note": "a QA guess"}}}


#: Her hand-set hardware: what she types into the Hardware & power dialog.
QA_HAND_SET = {"source": "manual", "cpu": {"model": "QA hand-set CPU", "cores": 4, "threads": 4},
               "memory": {"ram_gb": 16, "unified": False}, "gpus": [{"name": "QA hand-set GPU", "vram_gb": 12}],
               "physical_disks": [], "system": {"form": "desktop"}}


def part_machines(h: Harness) -> None:
    """3-18's checks (design/phase3-features.md P3.8): alice's helper machine with a real CITAR helper (the Rust
    ``citar-helper``, its home kept across its restarts as a machine's is) behind the scripted endpoint, reporting its
    hardware from a file (``--hardware``) that the part
    changes between reconnects, and the models the endpoint lists. Through the dialog's routes: the first hello kept as
    the last report and applied by the default modes (hardware and models auto, power manual); her hand-set hardware
    with hardware manual surviving the helper's reconnect with other hardware, the new scan kept beside it; models
    merged in auto (the unreported kept), only their loaded flags in manual; Apply per section, the hardware class
    following the hardware it copied; a hardware file of a word for a number, figures beyond any machine and parts of
    other shapes, reported by the real helper: kept without them, every page that shows the
    machine still answering (hers, the administrator's whole Servers page, the pool's listing), and "Apply all"
    taking the sections the report has (it suggests no power figures); an administrator's pin holding the class
    against the next scan, and its release; the trust tier set by an administrator and shown to her; the routes' gates
    (another member and a moderator 404, a visitor 401, the administrators' routes 404 to her and the moderator);
    slots and model swaps taking effect at once (the scheduler's capacity, and a
    test of a model not loaded refused, then taken with swaps allowed); LLM off refusing the test. With ``--keep`` her
    helper stays connected (its pid and hardware file in the evidence) for the browser checklist's G1."""
    r = h.report
    alice, bob, admin, mod = h.accounts["alice"], h.accounts["bob"], h.accounts["admin"], h.accounts["mod"]
    data = r.data.setdefault("machines", {})
    files = h.server.state / "hardware"
    files.mkdir(exist_ok=True)
    scripted = FakeEndpoint(delay=0.05)
    cold, third = "qa-cold-model", "qa-third-model"
    helpers: list = []
    try:
        cls = admin.post("/api/admin/hardware-classes", json={
            "kind": "gpu", "name": f"QA 24 GB GPUs {h.tag}", "rate": 2.0, "match": {"vram_gb_min": 24}, "priority": 10})
        big = cls.json().get("id", "") if cls.status_code == 200 else ""
        made = alice.post("/api/pool/servers", json={"config": {"name": f"QA alice settings box {h.tag}",
                                                                "kind": "owned",
                                                                "connection": {"provider": "openai_compatible"}},
                                                     "reach": "worker"})
        hid = made.json().get("id", "") if made.status_code == 200 else ""
        issued = alice.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa machines"}) if hid else None
        token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
        r.check("an administrator's hardware class for 24 GB GPUs, and alice's helper machine with a worker token",
                bool(big and hid and token), f"class {cls.status_code}, machine {made.status_code}")
        if not (big and hid and token):
            return
        data.update(machine=hid, big_class=big)

        def read(who: "Account" = alice) -> dict:
            got = who.get(f"/api/machines/{hid}/settings")
            return got.json() if got.status_code == 200 else {}

        def connect(label: str, hardware: dict, models: list) -> dict:
            """(Re)start her helper reporting ``hardware`` (its file, written now) and ``models`` (what the endpoint
            lists); returns the dialog's read once the server has recorded the new report."""
            for helper in helpers:
                helper.stop()
            wait_until(lambda: not _online(alice, hid), 15, 0.1)
            path = files / f"hardware-{len(helpers) + 1}.json"
            path.write_text(json.dumps(hardware, indent=1), encoding="utf-8")
            scripted.models = list(models)
            before = (read().get("report") or {}).get("at")
            helpers.append(Helper(h, token, scripted, "qa-settings-box", hardware=path, max_concurrent=4,
                                  machine="settings-box", provider="lmstudio"))
            fresh = wait_until(lambda: (lambda s: s if (s.get("report") or {}).get("at") not in (None, before)
                                        and _online(alice, hid) else None)(read()), 60, 0.25)
            steps.append({"step": label, "hardware_file": str(path), "reported_models": [list(m) for m in models],
                          "read": fresh})
            return fresh or {}

        def save_hardware(hardware: dict) -> int:
            """Her Hardware & power dialog's save: the registry's view of the machine, its hardware replaced, posted
            back whole."""
            listing = alice.get("/api/servers").json().get("servers", [])
            view = next((sv for sv in listing if sv.get("id") == hid), None)
            if view is None:
                return 404
            view["hardware"] = json.loads(json.dumps(hardware))
            return alice.post("/api/servers", json=view).status_code

        def put(body: dict, who: "Account" = alice) -> dict:
            got = who.put(f"/api/machines/{hid}/settings", json=body)
            return got.json() if got.status_code == 200 else {"status": got.status_code, "text": got.text[:200]}

        def apply(sections: Optional[list]) -> dict:
            got = alice.post(f"/api/machines/{hid}/hardware/apply", json={"sections": sections})
            return got.json() if got.status_code == 200 else {"status": got.status_code, "text": got.text[:200]}

        def models_of(s: dict) -> dict:
            return {m["key"]: m for m in (s.get("configured") or {}).get("models") or []}

        steps: list = []
        data["steps"] = steps
        a = _qa_scan(cpu="QA CPU A", gpu="QA GPU 8 GB", vram=8, idle=40, cpu_w=90, gpu_w=150)
        s1 = connect("first hello: 8 GB GPU, one model loaded", a, [(scripted.MODEL, True)])
        cfg = s1.get("configured") or {}
        r.check("her helper's hello is kept as the machine's last report, with its time, and applied by the default "
                "modes: the scan is her hardware (from the helper), the model is in the catalogue (loaded), the power "
                "figures are left alone (power manual), the class is the default GPU class",
                s1.get("modes") == {"hardware": "auto", "models": "auto", "power": "manual"}
                and ((s1.get("report") or {}).get("hardware") or {}).get("cpu", {}).get("model") == "QA CPU A"
                and bool((s1.get("report") or {}).get("hardware_at"))
                and cfg.get("hardware_source") == "helper" and cfg.get("hardware_summary", "").startswith("QA CPU A")
                and (cfg.get("power") or {}).get("idle_w") is None
                and models_of(s1).get(scripted.MODEL, {}).get("loaded") is True
                and ((s1.get("class") or {}).get("gpu") or {}).get("id") == "hc_gpu",
                json.dumps({k: s1.get(k) for k in ("modes", "configured", "class")})[:600])

        # her own hardware, kept: hardware manual
        saved = save_hardware(QA_HAND_SET)
        manual = put({"modes": {"hardware": "manual"}})
        r.check("she types her own hardware in (the Hardware & power dialog's save) and sets hardware to manual",
                saved == 200 and (manual.get("modes") or {}).get("hardware") == "manual"
                and "QA hand-set GPU" in (manual.get("configured") or {}).get("hardware_summary", ""),
                f"save HTTP {saved}; {json.dumps(manual.get('configured'))[:300]}")
        b = _qa_scan(cpu="QA CPU B", gpu="QA GPU 24 GB", vram=24, idle=60, cpu_w=120, gpu_w=300)
        s2 = connect("reconnect: 24 GB GPU, a second model not loaded", b,
                     [(scripted.MODEL, True), (cold, False)])
        cfg, rep2 = s2.get("configured") or {}, s2.get("report") or {}
        diffs = {d["what"]: d for d in (s2.get("differences") or {}).get("hardware") or []}
        r.check("her manual hardware entry survives the helper's reconnect with other hardware: the hand-set details "
                "stay, the new scan is kept beside them with a later time, and the differences name the GPU",
                "QA hand-set GPU" in cfg.get("hardware_summary", "")
                and (rep2.get("hardware") or {}).get("gpus") == [{"name": "QA GPU 24 GB", "vram_gb": 24}]
                and rep2.get("hardware_at", "") > (s1.get("report") or {}).get("hardware_at", "")
                and (diffs.get("GPUs") or {}).get("detected") == "QA GPU 24 GB (24 GB)",
                json.dumps({"configured": cfg.get("hardware_summary"), "scan": rep2.get("hardware_summary"),
                            "differences": list(diffs.values())}))
        got = models_of(s2)
        r.check("models auto merged the new report (the second model added, reported and not loaded) and the class "
                "still follows her hand-set hardware (12 GB: the default class); power manual left the figures unset, "
                "the scan's suggestion listed among the differences",
                set(got) == {scripted.MODEL, cold} and got[cold].get("reported") is True
                and got[cold].get("loaded") is False and ((s2.get("class") or {}).get("gpu") or {}).get("id") == "hc_gpu"
                and (cfg.get("power") or {}).get("idle_w") is None
                and len((s2.get("differences") or {}).get("power") or []) == 3,
                json.dumps({"models": got, "class": s2.get("class"), "power": s2.get("differences", {}).get("power")})[:600])

        # Apply, by section: the class follows the hardware it copies
        hw = apply(["hardware"])
        pw = apply(["power"])
        r.check("Apply copies the last scan's hardware in (from the helper) and the class follows it to the 24 GB "
                "class; Apply of the power section copies the scan's suggested figures, as a guess",
                (hw.get("configured") or {}).get("hardware_summary", "").startswith("QA CPU B")
                and ((hw.get("class") or {}).get("gpu") or {}).get("id") == big
                and (pw.get("configured") or {}).get("power") == {"idle_w": 60, "cpu_max_w": 120, "gpu_max_w": 300,
                                                                  "source": "guess"}
                and (pw.get("differences") or {}).get("hardware") == [] and (pw.get("differences") or {}).get("power") == [],
                json.dumps({"hardware": hw.get("configured"), "class": hw.get("class"), "power": pw.get("configured")})[:600])

        # models manual: the catalogue is hers
        put({"modes": {"models": "manual"}})
        s3 = connect("reconnect with models manual: only a third model, loaded", b, [(third, True)])
        got = models_of(s3)
        r.check("with models manual a report only marks what is loaded: the catalogue keeps her two models (neither "
                "loaded now), the third is not added, and the report is kept",
                set(got) == {scripted.MODEL, cold} and not got[scripted.MODEL].get("loaded")
                and not got[cold].get("loaded")
                and [m["key"] for m in (s3.get("report") or {}).get("models") or []] == [third]
                and any(d["what"] == third for d in (s3.get("differences") or {}).get("models") or []),
                json.dumps({"models": got, "report": (s3.get("report") or {}).get("models")})[:600])
        merged = apply(["models"])
        got = models_of(merged)
        r.check("Apply of the models section merges the report in: the third model added, the two others kept and "
                "marked as no longer reported",
                set(got) == {scripted.MODEL, cold, third} and got[third].get("loaded") is True
                and got[scripted.MODEL].get("reported") is False and got[cold].get("reported") is False
                and (merged.get("differences") or {}).get("models") == [],
                json.dumps(got)[:600])

        # a hardware file no page could show as it came: a word for a number, figures far beyond any machine, parts of
        # other shapes, and no power suggestion. (NaN, Infinity and a figure of 400 digits are not JSON as the helper
        # reads it: protocol 2's helper reports no file that holds them, and the hub refuses a frame that does.)
        odd = _qa_scan(cpu="QA CPU odd", gpu="QA GPU odd", vram=8, idle=1, cpu_w=1, gpu_w=1)
        odd["gpus"] = [{"name": "QA GPU odd", "vram_gb": "eight"}, "not a GPU"]
        odd["memory"] = "lots"
        odd["cpu"]["threads"] = 1e30
        odd["physical_disks"][0]["size_gb"] = 1e300
        del odd["power"]["suggested"]
        s5 = connect("reconnect: a hardware file of odd figures and shapes, suggesting no power figures", odd,
                     [(third, True)])
        kept = (s5.get("report") or {}).get("hardware") or {}

        def pages() -> dict:
            return {"her servers": alice.get("/api/servers").status_code,
                    "admin servers": admin.get("/api/servers").status_code,
                    "her pool": alice.get("/api/pool/servers").status_code,
                    "admin pool": admin.get("/api/pool/servers").status_code,
                    "admin dialog": admin.get(f"/api/machines/{hid}/settings").status_code}
        before_apply = pages()
        everything = alice.post(f"/api/machines/{hid}/hardware/apply", json={})
        after_apply = pages()
        applied_all = everything.json() if everything.status_code == 200 else {}
        audit = admin.get("/api/admin/audit", params={"object_id": hid, "action": "server.scan_applied"}).json()
        sections = [(e.get("detail") or {}).get("sections") for e in audit.get("entries", [])]
        data["odd_scan"] = {"kept": kept, "pages_before_apply": before_apply, "apply_all": everything.status_code,
                            "pages_after_apply": after_apply, "applied_sections": sections}
        r.check("a hardware file of a word for a number, figures beyond any machine and parts of other shapes is kept "
                "without them "
                "(the rest of the report as sent), every page that shows the machine still answers, and \"Apply all\" "
                "takes the sections the report has (hardware and models; it suggests no power figures)",
                kept.get("gpus") == [{"name": "QA GPU odd", "vram_gb": None}] and kept.get("memory") == {}
                and (kept.get("cpu") or {}).get("threads") is None
                and kept.get("physical_disks") == [{"name": "QA SSD", "size_gb": None, "type": "SSD"}]
                and set(before_apply.values()) == {200} and everything.status_code == 200
                and set(after_apply.values()) == {200} and ["hardware", "models"] in sections
                and (applied_all.get("configured") or {}).get("hardware_summary", "").startswith("QA CPU odd")
                and (applied_all.get("differences") or {}).get("models") == [],
                json.dumps(data["odd_scan"])[:800])

        # an administrator's pin holds the class against the next scan
        pin = admin.put(f"/api/admin/machines/{hid}/class", json={"pinned": True, "gpu_class_id": "hc_gpu"})
        put({"modes": {"hardware": "auto", "models": "auto"}})
        c = _qa_scan(cpu="QA CPU C", gpu="QA GPU 32 GB", vram=32, idle=61, cpu_w=121, gpu_w=301)
        s4 = connect("reconnect with hardware auto, the class pinned: 32 GB GPU", c,
                     [(scripted.MODEL, True), (cold, False)])
        unpin = admin.put(f"/api/admin/machines/{hid}/class", json={"pinned": False})
        after = read()
        r.check("pinned by an administrator, the class holds against a new scan applied by hardware auto; unpinned, "
                "it follows the hardware again at once",
                pin.status_code == 200 and (s4.get("configured") or {}).get("hardware_summary", "").startswith("QA CPU C")
                and (s4.get("class") or {}).get("pinned") is True and ((s4.get("class") or {}).get("gpu") or {}).get("id") == "hc_gpu"
                and unpin.status_code == 200 and ((after.get("class") or {}).get("gpu") or {}).get("id") == big
                and (after.get("class") or {}).get("pinned") is False,
                json.dumps({"pinned": s4.get("class"), "unpinned": after.get("class")}))

        # trust, and the routes' gates
        trust = admin.put(f"/api/admin/machines/{hid}/trust", json={"trust_tier": "trusted"})
        mine = read()
        with h.anonymous() as anon:
            visitor = (anon.get(f"/api/machines/{hid}/settings").status_code,
                       anon.put(f"/api/machines/{hid}/settings", json={"slots": 2}).status_code)
        gates = {"bob read": bob.get(f"/api/machines/{hid}/settings").status_code,
                 "bob change": bob.put(f"/api/machines/{hid}/settings", json={"slots": 2}).status_code,
                 "bob apply": bob.post(f"/api/machines/{hid}/hardware/apply", json={}).status_code,
                 "alice trust": alice.put(f"/api/admin/machines/{hid}/trust", json={"trust_tier": "trusted"}).status_code,
                 "alice class": alice.put(f"/api/admin/machines/{hid}/class", json={"pinned": False}).status_code,
                 "mod": [mod.get(f"/api/machines/{hid}/settings").status_code,
                         mod.put(f"/api/machines/{hid}/settings", json={"slots": 2}).status_code,
                         mod.post(f"/api/machines/{hid}/hardware/apply", json={}).status_code,
                         mod.put(f"/api/admin/machines/{hid}/trust", json={"trust_tier": "new"}).status_code,
                         mod.put(f"/api/admin/machines/{hid}/class", json={"pinned": False}).status_code],
                 "visitor": list(visitor)}
        r.check("an administrator sets the trust tier and she is shown it (not the classes to pin, nor the flags); "
                "another member and a moderator get 404 on the dialog's routes, she and the moderator 404 on the "
                "administrators' and a visitor 401",
                trust.status_code == 200 and mine.get("trust_tier") == "trusted" and mine.get("can_admin") is False
                and "classes" not in mine and "flags" not in mine
                and gates == {"bob read": 404, "bob change": 404, "bob apply": 404, "alice trust": 404,
                              "alice class": 404, "mod": [404] * 5, "visitor": [401, 401]},
                json.dumps(gates))

        # slots and swaps, at once
        def test_of(model: str) -> dict:
            got = alice.post(f"/api/pool/servers/{hid}/test", params={"model": model, "wait": 30})
            return got.json() if got.status_code == 200 else {"status": got.status_code}
        before = test_of(cold)
        three = put({"slots": 3})
        swaps = put({"capabilities": {"llm": {"allow_swaps": True}}})
        taken = test_of(cold)
        back = put({"capabilities": {"llm": {"allow_swaps": False}}})
        again = test_of(cold)
        data["slots_and_swaps"] = {"before": before, "slots_3": three.get("in_effect"), "swaps_on": swaps.get("in_effect"),
                                   "taken": taken, "swaps_off": back.get("in_effect"), "again": again}
        r.check("slots and model swaps set in the dialog take effect at once: 3 slots in effect (her helper takes 4); "
                "a test of a model her helper has not loaded is refused, taken once swaps are allowed, refused again "
                "once they are not",
                before.get("ok") is False and before.get("code") == "not_loaded"
                and (three.get("in_effect") or {}).get("capacity") == 3
                and (swaps.get("in_effect") or {}).get("allow_swaps") is True
                and taken.get("ok") is True and taken.get("model") == cold
                and again.get("ok") is False and again.get("code") == "not_loaded",
                json.dumps(data["slots_and_swaps"])[:800])
        off = put({"capabilities": {"llm": {"enabled": False}}})
        refused = test_of(scripted.MODEL)
        on = put({"capabilities": {"llm": {"enabled": True}}})
        r.check("with its LLM turned off the machine takes no model work (her test is refused, saying why); on again, "
                "it does", (off.get("in_effect") or {}).get("llm") is False and refused.get("ok") is False
                and "takes no model work" in (refused.get("reason") or "")
                and (on.get("in_effect") or {}).get("llm") is True,
                json.dumps({"refused": refused, "in_effect": on.get("in_effect")})[:400])
        audit = admin.get("/api/admin/audit", params={"object_id": hid}).json().get("entries", [])
        actions = sorted({e.get("action") for e in audit})
        r.check("every change has its audit row (settings, apply, class, trust)",
                {"server.settings", "server.scan_applied", "server.class", "server.trust"} <= set(actions),
                json.dumps(actions))
        if h.keep and helpers:
            data["kept_helper"] = {"pid": helpers[-1].proc.pid, "hardware_file": steps[-1]["hardware_file"],
                                   "token": token, "machine": hid}
    finally:
        if not h.keep:
            for helper in helpers:
                helper.stop()
        scripted.close()


# ---------------------------------------------------------------------------- the exit part (3-19)

#: The ``exit`` part's game (design/phase3-features.md 3-19): a standard map that wraps east-west, forty rounds, and
#: chat timeouts short enough to watch them run out (a model's 45 s, a person's 20 s: the lobby's keys).
EXIT_CONFIG = {"map_size": "standard", "map_type": "continents", "map_edges": "wrap_x", "seed": 3419,
               "barbarians": "off", "city_states": 0, "turn_limit": 40, "chat_model_reply_seconds": 45,
               "chat_person_reply_seconds": 20}
#: How long the ``exit`` part's endpoint takes over a completion whose prompt carries ``QA-SLOW``: longer than a
#: model's chat timeout, so the chat clock decides that chat.
EXIT_SLOW_S = 55.0
#: The model the administrator's benchmark plays on the ``exit`` part's helper machine, listed and loaded beside the
#: game's: the endpoint's log tells the benchmark's requests from everyone else's by it.
BENCH_MODEL = "qa-bench-model"


class SeatDriver:
    """One seat played over HTTP as a person at the game screen or an MCP client plays it: it long-polls
    ``GET /api/games/{gid}/wait`` with the seat's token (the MCP server's ``wait_for_turn``), answers each chat that
    waits on it, and ends its turn when nothing does, unless ``hold`` is set (the script acts on the turn first). Its
    first turn founds its city where its settler stands, sets its research and sends its other units exploring.

    A chat whose messages from this seat carry ``QA-LEAVE`` is left to run out; any other that waits on it is accepted
    when a deal is on the table and closed with a word when none is. It keeps every answer of the wait route (with the
    ``chats`` and their deadlines), each chat it answered and each turn it ended, and rides out a server restart (the
    route answers nothing, then 404 until the game is back)."""

    def __init__(self, h: "Harness", gid: str, token: str, pid: int, label: str):
        self.gid, self.token, self.pid, self.label = gid, token, pid, label
        self.http = httpx.Client(base_url=h.server.base, timeout=60.0)
        #: set: the seat ends no turn (the script acts on it first); answering chats goes on
        self.hold = threading.Event()
        self.finished = threading.Event()
        self._stop = threading.Event()
        self.waits: list = []
        self.answered: dict = {}
        self.ended: list = []
        self.left: set = set()
        self.first: Optional[dict] = None
        self.errors = 0
        self.thread = threading.Thread(target=self._run, daemon=True, name=f"qa-seat-{label}")
        self.thread.start()

    def tool(self, name: str, args: Optional[dict] = None) -> dict:
        """One of the seat's tools through ``POST /api/games/{gid}/tool`` with its token."""
        try:
            got = self.http.post(f"/api/games/{self.gid}/tool", params={"token": self.token},
                                 json={"tool": name, "args": args or {}})
        except httpx.HTTPError as e:
            return {"ok": False, "error": f"{type(e).__name__}: {e}"}
        return got.json() if got.status_code == 200 else {"ok": False, "error": f"HTTP {got.status_code} {got.text[:200]}"}

    def chat(self, nid: int) -> dict:
        """Chat ``nid`` as the seat sees it, with its clock (``GET /api/games/{gid}/negotiations/{nid}``)."""
        try:
            got = self.http.get(f"/api/games/{self.gid}/negotiations/{nid}", params={"token": self.token})
        except httpx.HTTPError:
            return {}
        return got.json() if got.status_code == 200 else {}

    def _first_turn(self) -> None:
        units = [u for u in self.tool("get_units").get("result") or [] if isinstance(u, dict)]
        settler = next((u for u in units if str(u.get("type") or "").lower() == "settler"), None)
        founded = self.tool("found_city", {"unit_id": settler["id"]}) if settler else {"ok": False}
        research = self.tool("set_research", {"tech": "Pottery"})
        # the others explore (a standing order), so the seat's explored land, and its minimap's home box, grows
        explore = [self.tool("unit_order", {"unit_id": u["id"], "order": "explore"}) for u in units
                   if u is not settler and str(u.get("type") or "").lower() not in ("worker", "work boat")]
        self.first = {"founded": bool(founded.get("ok")), "research": bool(research.get("ok")),
                      "exploring": sum(1 for x in explore if x.get("ok"))}

    def _answer(self, nid: int) -> bool:
        """Answer chat ``nid`` as the policy says; whether it did."""
        if nid in self.left:
            return False
        view = self.chat(nid)
        if view.get("status") != "open" or (view.get("clock") or {}).get("waiting_on") != self.pid:
            return False
        if any("QA-LEAVE" in str(e.get("message") or "") for e in view.get("history") or [] if e.get("you")):
            self.left.add(nid)
            return False
        action = "accept" if view.get("current_proposal") else "reject"
        said = self.tool("respond_negotiation", {"negotiation_id": nid, "action": action,
                                                 "message": "QA: agreed." if action == "accept" else "QA: thank you."})
        self.answered[nid] = {"action": action, "ok": bool(said.get("ok")), "t": time.time(),
                              "error": None if said.get("ok") else str(said.get("error"))[:200]}
        return True

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                got = self.http.get(f"/api/games/{self.gid}/wait", params={"token": self.token, "timeout": 5})
            except httpx.HTTPError:
                self.errors += 1
                time.sleep(0.5)
                continue
            if got.status_code != 200:
                self.errors += 1
                time.sleep(0.5)
                continue
            answer = got.json()
            if len(self.waits) < 20000:
                self.waits.append({"t": time.time(), **answer})
            status = answer.get("status")
            if status in ("game_over", "eliminated", "crashed"):
                self.finished.set()
                return
            acted = False
            if status == "negotiation":
                for nid in answer.get("negotiation_ids") or []:
                    acted = self._answer(nid) or acted
                if not acted:
                    time.sleep(0.5)         # only chats it leaves to run out wait on it
            elif status == "your_turn":
                if self.first is None:
                    self._first_turn()
                if self.hold.is_set():
                    time.sleep(0.2)
                    continue
                ended = self.tool("end_turn")
                if ended.get("ok"):
                    self.ended.append({"turn": answer.get("turn"), "t": time.time()})
                else:
                    time.sleep(0.25)
            elif status == "waiting_for_reply":
                time.sleep(0.1)

    def stop(self) -> None:
        self._stop.set()
        self.thread.join(15)
        self.http.close()


def _game_info(who: "Account", gid: str) -> dict:
    """A game as its lobby entry reads (``GET /api/games/{gid}``), or {} while the server cannot say."""
    try:
        got = who.get(f"/api/games/{gid}")
    except httpx.HTTPError:
        return {}
    return got.json() if got.status_code == 200 else {}


def _spans_overlap(spans: list, others: list, slack: float = 0.05) -> list:
    """The pairs of ``(start, end, label)`` spans, one from each list, that overlap by more than ``slack`` seconds."""
    return [(a[2], b[2]) for a in spans for b in others if a[0] < b[1] - slack and b[0] < a[1] - slack]


def _event_spans(state: Path, machine: str, where: str) -> list:
    """The compute ledger's model steps on ``machine`` that match ``where`` (SQL), each as ``(start, end, activity)``
    in epoch seconds: the scheduler's listener writes a step when it ends (``at``), with the time the server observed
    it take (``observed_s``)."""
    from datetime import datetime, timezone
    out = []
    for at, observed, activity in _ledger_rows(state, "SELECT at, observed_s, activity FROM compute_events WHERE "
                                                      f"machine_id = ? AND kind = 'gpu' AND ({where})", (machine,)):
        end = datetime.fromisoformat(str(at)).replace(tzinfo=timezone.utc).timestamp()
        out.append((end - float(observed or 0.0), end, activity))
    return out


def _ledger_agrees(h: Harness, sides: dict, limit: float = 90.0) -> tuple:
    """Each account's given, used and own use as ``/api/ledger/me`` tells it, against the sums of the compute ledger's
    events in the instance's database (``sides``: account key -> the sides to compare), read until two reads of the
    events around the route's answer agree (CPU lands each minute). Returns (agreed, what was compared)."""
    state = h.server.state
    ids = dict(_ledger_rows(state, "SELECT handle, id FROM users"))
    seen: dict = {}

    def sums():
        out = {}
        for key in sides:
            uid = ids.get(h.accounts[key].handle)
            given = _ledger_rows(state, "SELECT coalesce(sum(credits), 0), count(*) FROM compute_events "
                                        "WHERE provider_id = ? AND self_use = 0", (uid,))[0]
            used = _ledger_rows(state, "SELECT coalesce(sum(credits), 0), count(*) FROM compute_events "
                                       "WHERE consumer_id = ? AND self_use = 0", (uid,))[0]
            own = _ledger_rows(state, "SELECT coalesce(sum(credits), 0), count(*) FROM compute_events "
                                      "WHERE consumer_id = ? AND self_use = 1", (uid,))[0]
            out[key] = {"given": tuple(given), "used": tuple(used), "own_use": tuple(own)}
        return out

    def agrees() -> bool:
        before = sums()
        told = {key: h.accounts[key].get("/api/ledger/me?range=all").json() for key in sides}
        after = sums()
        seen.update(events=before, told={k: {s: (v.get(s) or {}) for s in ("given", "used", "own_use")}
                                         for k, v in told.items()}, breakdown=told)
        if before != after:
            return False
        for key, wanted in sides.items():
            for side in wanted:
                credits, n = before[key][side]
                got = told[key].get(side) or {}
                if abs(float(got.get("credits", -1)) - float(credits)) > 1e-3 or int(got.get("events", -1)) != n:
                    return False
        return True
    ok = bool(wait_until(agrees, limit, 2.0))
    return ok, seen


def part_exit(h: Harness) -> None:
    """3-19's end-to-end checks on one dev server (design/phase3-features.md, Appendix D's 3-19): bob's helper machine
    (a real Python helper behind the scripted endpoint, two slots) and his direct dry-run machine, shared with alice for
    games and pooled prompts and with the administrator for benchmarks. Alice's game, shared with members, on a
    standard map that wraps east-west: her person seat (played by the script through the API, the wait route and her
    seat's socket), a hybrid seat (the standard bot and the helper machine's model, all but war, prompted every 3
    turns), a full LLM seat on the same machine, an MCP seat (played by the script through the wait route with its seat
    token) and two bots, 40 rounds, both chat timeouts short (a model's 45 s, a person's 20 s).

    On her first turn alice opens chats with the hybrid seat (its model answers), the LLM seat (it answers, she closes
    the chat), a bot (she lets its answer run out: expired), the MCP seat (it accepts) and then two the models are too
    slow for (the hybrid's bot answers at the clock's deadline, cause timeout; the LLM seat is declined for). The
    models' own offers are answered as they come. Meanwhile bob's report analysis and alice's probe run on the helper
    machine, and alice's report analysis on the dry run, between the game's prompts. On a later turn the administrator
    starts a one-job benchmark on the helper machine's second slot, beside the game's lease: a trade for the hybrid's
    model is refused there and its bot answers (cause: the benchmark), and no pooled request overlaps a benchmark step,
    by the endpoint's log and by the compute ledger (the scheduler's listener). Bob types the machine's hardware in by
    hand and its helper restarts with other hardware: the hand-set entry stays, the new scan is kept beside it. The
    server is then killed with a chat waiting on the LLM seat (its answer held at the endpoint; a seat change's save
    holds the chat), brought back (``restore_live``), and the chat is answered by the model well inside its timeout;
    the game plays on to its end. Carol's socket (a member who may watch) and a spectator's get no ``negotiation_status``
    while alice's does; the MCP seat's waits list its chats with their deadlines; a seat naming a base URL is refused
    with nothing dialled; alice's minimap moves through the eras by the debug ops route, its home box never shrinking;
    bob's given, alice's and the administrator's used and bob's own use match the ledger's events."""
    r = h.report
    admin, alice, bob, carol = (h.accounts[k] for k in ("admin", "alice", "bob", "carol"))
    data = r.data.setdefault("exit", {})
    state = h.server.state
    endpoint = FakeEndpoint(delay=0.25, max_trades=4)
    endpoint.models = [(endpoint.MODEL, True), (BENCH_MODEL, True)]
    # a chat's answer only: a turn's briefing that lists the chat (or a note of how it ended) is no slower
    endpoint.slow, endpoint.slow_kinds = {"QA-SLOW": EXIT_SLOW_S}, {"chat"}
    # the benchmark's steps take a while, so the lobby game plays beside them and its pooled prompts meet them
    endpoint.model_delay = {BENCH_MODEL: 1.0}
    trap = FakeEndpoint(delay=0.0)
    files = state / "exit-hardware"
    files.mkdir(exist_ok=True)
    helpers: list = []
    sockets: list = []
    drivers: list = []
    gid, run_id = "", ""
    counters_before: dict = {}
    t_start = time.time()
    try:
        # ---- bob's two machines, shared with alice (games, pooled prompts) and the administrator (benchmarks)
        made = bob.post("/api/pool/servers", json={"config": {"name": f"QA exit helper {h.tag}", "kind": "owned",
                                                              "connection": {"provider": "openai_compatible",
                                                                             "max_parallel": 2}},
                                                   "reach": "worker"})
        hid = made.json().get("id", "") if made.status_code == 200 else ""
        issued = bob.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa exit"}) if hid else None
        token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
        first_scan = files / "scan-1.json"
        first_scan.write_text(json.dumps(_qa_scan(cpu="QA exit CPU", gpu="QA exit GPU 16 GB", vram=16, idle=30,
                                                  cpu_w=80, gpu_w=200, host="qa-exit-box"), indent=1), encoding="utf-8")
        if token:
            helpers.append(Helper(h, token, endpoint, "qa-exit-helper", hardware=first_scan, max_concurrent=2,
                                  machine="exit"))
        dry = bob.post("/api/servers", json={"name": f"QA exit dry run {h.tag}", "kind": "test",
                                             "connection": {"provider": "dryrun", "dry_run_delay": 0.1,
                                                            "max_parallel": 2},
                                             "models": [{"key": "dry-run"}]})
        dry_id = dry.json().get("id", "") if dry.status_code == 200 else ""
        group = bob.post("/api/pool/groups", json={"name": f"QA exit group {h.tag}", "visibility": "private"})
        group_id = group.json().get("id", "") if group.status_code == 200 else ""
        put = [bob.put(f"/api/pool/servers/{sid}", json={"group_id": group_id}).status_code for sid in (hid, dry_id)]
        grants = [bob.post(f"/api/pool/groups/{group_id}/grants",
                           json={"subject_type": "user", "handle": who.handle, "purposes": purposes}).status_code
                  for who, purposes in ((alice, ["game", "pooled"]), (admin, ["benchmark"]))]
        online = bool(helpers) and bool(wait_until(lambda: _online(bob, hid), 60, 0.25))
        slots = (bob.get(f"/api/pool/servers/{hid}").json() if hid else {}).get("max_concurrent")
        r.check("bob's helper machine (its real helper connected behind the scripted endpoint, two slots) and his "
                "direct dry-run machine, shared with alice for games and pooled prompts and with the administrator "
                "for benchmarks",
                online and slots == 2 and dry.status_code == 200 and put == [200, 200] and grants == [200, 200],
                f"helper {made.status_code}, online {online}, slots {slots}; dry run {dry.status_code}; group "
                f"{group.status_code}, put {put}, grants {grants}")
        data["machines"] = {"helper": hid, "dry_run": dry_id, "group": group_id}
        if not (online and dry_id):
            return

        # ---- alice's game: her person seat, a hybrid seat and a full LLM seat on bob's helper machine, an MCP seat,
        # two bots; shared with members
        model = {"server_id": hid, "model": endpoint.MODEL}
        seats = [{"type": "human"},
                 {"type": "hybrid", "llm": dict(model), "bot": {"profile": "standard"},
                  "hybrid": {"owners": "all_but_war", "prompt_every": 3}},
                 {"type": "llm", "llm": dict(model)}, {"type": "mcp"}, {"type": "bot"}, {"type": "bot"}]
        g = alice.post("/api/games", json={"name": f"QA exit game {h.tag}", "config": dict(EXIT_CONFIG),
                                           "seats": seats})
        info = g.json() if g.status_code == 200 else {}
        gid = info.get("id", "")
        tokens = [s.get("token", "") for s in info.get("seats") or []]
        shared = alice.put(f"/api/games/{gid}/sharing", json={"visibility": "server"}) if gid else None
        cfg = info.get("config") or {}
        kinds = [s.get("type") for s in info.get("seats") or []]
        with h.anonymous() as anon:
            visitor = anon.get(f"/api/games/{gid}").status_code if gid else None
        r.check("alice's game: her person seat, a hybrid seat (all but war, prompted every 3 turns) and a full LLM "
                "seat on bob's helper machine, an MCP seat and two bots, on a standard map that wraps east-west, 40 "
                "rounds, both chat timeouts short; shared with members (carol opens it, a visitor gets 404)",
                bool(gid) and kinds == ["human", "hybrid", "llm", "mcp", "bot", "bot"]
                and ((info.get("seats") or [{}, {}])[1].get("hybrid") or {}).get("preset") == "all_but_war"
                and cfg.get("chat_model_reply_seconds") == 45 and cfg.get("chat_person_reply_seconds") == 20
                and shared is not None and shared.status_code == 200
                and carol.get(f"/api/games/{gid}").status_code == 200 and visitor == 404,
                f"game {g.status_code} {g.text[:160] if g.status_code != 200 else ''}; seats {kinds}; sharing "
                f"{getattr(shared, 'status_code', None)}; visitor {visitor}")
        if not gid:
            return
        data["game"] = gid
        met = alice.post(f"/api/games/{gid}/debug/meet_all").status_code
        rich = alice.post(f"/api/games/{gid}/debug/gold").status_code

        # ---- a member's seat that names an address: refused, nothing dialled
        block = {"provider": "openai_compatible", "base_url": trap.url, "api_key_env": "PATH", "model": "x"}
        seat_try = alice.post(f"/api/games/{gid}/seats/5", json={"type": "llm", "llm": block})
        new_try = carol.post("/api/games", json={"name": f"QA exit dial {h.tag}", "config": CONFIG,
                                                 "seats": [{"type": "llm", "llm": block}, {"type": "bot"}]})
        time.sleep(1.0)
        still = (_game_info(alice, gid).get("seats") or [{}] * 6)[5].get("type")
        r.check("a member's seat naming a base URL and a key variable is refused (400): alice's seat change and "
                "carol's new game; the seat stays a bot and nothing connected to the address",
                seat_try.status_code == 400 and new_try.status_code == 400 and still == "bot"
                and not trap.connections, f"seat {seat_try.status_code}, game {new_try.status_code}, seat 5 "
                f"{still}, {len(trap.connections)} connection(s)", connections=len(trap.connections))

        # ---- the sockets: alice's seat, carol's account (a member who may watch), a spectator's token
        mine = Socket(h, gid, tokens[0])
        carols = Socket(h, gid, "", account=carol)
        spectator = Socket(h, gid, info.get("spectator_token", ""))
        sockets += [mine, carols, spectator]
        hello = [next((m for _, m in s.messages if m.get("type") == "hello"), None)
                 for s in (carols, spectator)] if wait_until(lambda: carols.messages and spectator.messages, 15, 0.1) \
            else [None, None]

        # ---- the players: alice and the MCP seat, by the wait route with their tokens
        a = SeatDriver(h, gid, tokens[0], 0, "alice")
        a.hold.set()
        m = SeatDriver(h, gid, tokens[3], 3, "mcp")
        drivers += [a, m]

        def turn_of() -> tuple:
            gi = _game_info(alice, gid)
            return gi.get("phase"), gi.get("current_player"), gi.get("turn")

        def at_alices_turn(limit: float = 240.0) -> Optional[int]:
            """Hold alice's next turn and wait until it is hers (her driver ends none while held); its number."""
            a.hold.set()
            deadline = time.perf_counter() + limit
            while time.perf_counter() < deadline:
                phase, current, turn = turn_of()
                if phase != "playing":
                    return None
                if current == 0:
                    time.sleep(0.6)             # her driver may have been ending it as the hold came
                    if turn_of() == (phase, current, turn):
                        return turn
                time.sleep(0.25)
            return None

        def head(nid: int) -> dict:
            return a.chat(nid)

        def opened(res: dict) -> Optional[int]:
            return (res.get("result") or {}).get("negotiation_id") if res.get("ok") else None

        def metric_rows(nid: int) -> list:
            got = alice.get(f"/api/games/{gid}/metrics")
            rows = got.json().get("negotiations") or [] if got.status_code == 200 else []
            return [(x.get("outcome"), x.get("cause")) for x in rows if x.get("nid") == nid]

        t1 = at_alices_turn(60)
        block0 = (alice.get(f"/api/games/{gid}/view", params={"token": tokens[0]}).json().get("minimap") or {})
        minimap = [{"step": "the start", "stage": block0.get("stage"), "home": block0.get("home")}]

        # ---- her first turn: a chat with each kind of seat
        h1 = opened(a.tool("open_negotiation", {"to": 1, "message": "QA-H1: maps for gold?",
                                                "give": [{"type": "gold", "amount": 25}],
                                                "receive": [{"type": "share_map"}]}))
        l1 = opened(a.tool("open_negotiation", {"to": 2, "message": "QA-L1: shall we talk?"}))
        b1 = opened(a.tool("open_negotiation", {"to": 4, "message": "QA-LEAVE: shall we talk?"}))
        m1 = opened(a.tool("open_negotiation", {"to": 3, "message": "QA-M1: shall we share our maps?",
                                                "give": [{"type": "share_map"}], "receive": [{"type": "share_map"}]}))
        r.check("on her first turn (after meet_all) alice opened a chat with the hybrid seat, the LLM seat, a bot and "
                "the MCP seat", all(x is not None for x in (h1, l1, b1, m1)) and met == 200 and rich == 200,
                f"turn {t1}; chats {[h1, l1, b1, m1]}; meet_all {met}, gold {rich}")
        settled = wait_until(lambda: head(h1).get("status") not in (None, "open")
                             and head(l1).get("status") not in (None, "open"), 60, 0.25)
        hv, lv = head(h1), head(l1)
        r.check("the hybrid seat's model answered her trade (trades are its model's) and the LLM seat's model "
                "replied to her word, which she closed",
                bool(settled) and hv.get("status") == "accepted" and metric_rows(h1) == [("answered", None)]
                and any(not e.get("you") and e.get("message") == "(qa endpoint) reply" for e in lv.get("history") or [])
                and lv.get("status") == "rejected",
                f"hybrid {hv.get('status')} {metric_rows(h1)}; llm {lv.get('status')} "
                f"{[(e.get('by'), e.get('action'), e.get('message')) for e in lv.get('history') or []]}")
        # two the models are too slow for: the clock decides each at 45 s
        h2 = opened(a.tool("open_negotiation", {"to": 1, "message": "QA-SLOW-H: gold for maps?",
                                                "give": [{"type": "gold", "amount": 20}],
                                                "receive": [{"type": "share_map"}]}))
        l2 = opened(a.tool("open_negotiation", {"to": 2, "message": "QA-SLOW-L: one more question?"}))
        t_slow = time.time()
        timed = wait_until(lambda: (head(h2).get("clock") or {}).get("waiting_on") != 1
                           and head(l2).get("status") not in (None, "open"), 75, 0.25)
        took = round(time.time() - t_slow, 1)
        h2v, l2v = head(h2), head(l2)
        r.check("too slow for both models (55 s against their 45 s): at the deadline the hybrid seat's bot answered "
                "its trade (fallback, cause timeout) and the chat clock declined for the LLM seat, '(no reply in "
                "time)'",
                bool(timed) and ("fallback", "timeout") in metric_rows(h2)
                and (l2v.get("history") or [{}])[-1].get("message") == "(no reply in time)"
                and ("declined_late", "timeout") in metric_rows(l2) and 40 <= took <= 60,
                f"after {took} s; hybrid {metric_rows(h2)} {h2v.get('status')}; llm {metric_rows(l2)} "
                f"{(l2v.get('history') or [{}])[-1]}", seconds=took)
        gone = wait_until(lambda: all(head(n).get("status") not in (None, "open") for n in (h1, l1, b1, m1, h2, l2)),
                          40, 0.25)
        bv, mv = head(b1), head(m1)
        r.check("the bot answered her at once and she let it wait: closed as expired at 20 s, '(no reply in time)'; "
                "the MCP seat accepted her maps through the wait route",
                bool(gone) and bv.get("status") == "expired"
                and (bv.get("history") or [{}])[-1].get("note") == "(no reply in time)"
                and mv.get("status") == "accepted" and (m.answered.get(m1) or {}).get("action") == "accept",
                f"bot {bv.get('status')} {(bv.get('history') or [{}])[-1]}; mcp {mv.get('status')} "
                f"{m.answered.get(m1)}")
        mcp_saw = next((w for w in m.waits if w.get("status") == "negotiation" and m1 in (w.get("negotiation_ids") or [])),
                       {})
        entry = next((c for c in mcp_saw.get("chats") or [] if c.get("negotiation_id") == m1), {})
        r.check("the MCP seat's wait for its turn returned for her chat, listing it waiting on it with its deadline and "
                "the seconds left (wait_for_turn's chats)",
                entry.get("waiting_on") == "you" and (entry.get("deadline") or 0) > mcp_saw.get("t", 0)
                and 0 < (entry.get("seconds_left") or 0) <= EXIT_CONFIG["chat_model_reply_seconds"], json.dumps({"wait": {k: mcp_saw.get(k) for k in (
                    "status", "negotiation_ids")}, "chat": entry})[:400])
        data["first_turn"] = {"chats": {"hybrid": h1, "llm": l1, "bot": b1, "mcp": m1, "hybrid_slow": h2,
                                        "llm_slow": l2}, "slow_settled_s": took, "mcp_wait": entry}
        a.hold.clear()

        # ---- bob's report and alice's probe on the helper machine, her report on the dry run: pooled prompts
        # between the game's own
        def report(who: "Account", title: str, sid: str, model_id: str) -> str:
            got = who.post("/api/reports", json={"title": title, "sections": ["summary"], "narrative": {
                "enabled": True, "server_id": sid, "model_id": model_id}})
            return got.json().get("id", "") if got.status_code == 200 else ""

        def game_prompts(since: float) -> list:
            return [q for q in endpoint.completions() if q["t"] >= since and q.get("model") == endpoint.MODEL
                    and q.get("kind") in ("turn", "diplomat")]
        # the game plays on (the slow answers' slots drain first): its prompts come, then the pooled work's between them
        t_go = time.time()
        wait_until(lambda: len(game_prompts(t_go)) >= 2, 120, 0.25)
        t_pooled = time.time()
        b_rep = report(bob, f"QA exit bob report {h.tag}", hid, endpoint.MODEL)
        a_rep = report(alice, f"QA exit alice report {h.tag}", dry_id, "dry-run")
        ed = alice.post("/api/scenario-editor", json={"source": "generate", "config": {**CONFIG, "seed": 3423},
                                                      "players": [{"type": "bot"}, {"type": "bot"}]})
        eid = ed.json().get("editor_id", "") if ed.status_code == 200 else ""
        saved = alice.post(f"/api/scenario-editor/{eid}/save", json={"name": f"QA exit probe {h.tag}"})
        scn = saved.json().get("id", "") if saved.status_code == 200 else ""
        if eid:
            alice.delete(f"/api/scenario-editor/{eid}")
        pid_ = f"qa-exit-{h.tag}"
        alice.put(f"/api/probes/{pid_}", json={"name": "QA exit probe", "scenario": scn, "subject": 1,
                                                "counterparty": 0, "cases": [
                                                    {"id": f"m{i}", "kind": "message",
                                                     "message": f"QA-PROBE-exit-{i}-{h.tag} hello"} for i in (1, 2)]})
        prun = alice.post(f"/api/probes/{pid_}/run", json={"llm": dict(model)})
        p_run = prun.json().get("id", "") if prun.status_code == 200 else ""

        def pooled_done() -> bool:
            reps = [(who.get(f"/api/reports/{rid}").json().get("meta") or {}).get("status") for who, rid in
                    ((bob, b_rep), (alice, a_rep)) if rid]
            run = (alice.get(f"/api/probe-runs/{p_run}").json().get("run") or {}).get("status") if p_run else None
            return len(reps) == 2 and all(s in ("done", "failed") for s in reps) and run in ("finished", "failed",
                                                                                             "stopped")
        finished = wait_until(pooled_done, 300, 1.0)
        metas = {k: (who.get(f"/api/reports/{rid}").json().get("meta") or {}) if rid else {}
                 for k, who, rid in (("bob", bob, b_rep), ("alice", alice, a_rep))}
        probe = alice.get(f"/api/probe-runs/{p_run}").json() if p_run else {}
        pooled = [(q.get("kind"), q.get("marker") or q.get("head", "")[:40], round(q["t"] - t_pooled, 2))
                  for q in endpoint.completions() if q["t"] >= t_pooled - 1 and (
                      q.get("kind") == "plain" or (q.get("marker") or "").startswith("QA-PROBE-exit"))]
        last = t_pooled + max((p[2] for p in pooled), default=0.0)
        wait_until(lambda: len(game_prompts(last + 0.01)) >= 2, 120, 0.25)
        game_ts = sorted(q["t"] for q in game_prompts(t_go))
        between = [p for p in pooled if game_ts and game_ts[0] < t_pooled + p[2] < game_ts[-1]]
        r.check("bob's report analysis and alice's probe ran as pooled prompts on the helper machine while the game "
                "played, served between the game's own prompts, and her report's analysis on the dry run",
                bool(finished) and metas["bob"].get("status") == "done" and metas["bob"].get("narrative_written")
                and metas["alice"].get("status") == "done" and metas["alice"].get("narrative_written")
                and (probe.get("run") or {}).get("status") == "finished" and len(pooled) >= 3
                and len(between) == len(pooled),
                json.dumps({"bob": {k: metas["bob"].get(k) for k in ("status", "narrative_written")},
                            "alice": {k: metas["alice"].get(k) for k in ("status", "narrative_written")},
                            "probe": (probe.get("run") or {}).get("status"), "pooled": pooled,
                            "game_prompts": len(game_ts)})[:700], prompts=len(pooled))
        data["pooled"] = {"reports": {"bob": b_rep, "alice": a_rep}, "probe_run": p_run, "served": pooled}

        # ---- the administrator's benchmark on the helper machine's second slot, beside the game's lease
        t_b = at_alices_turn()
        suite = {"name": f"QA exit bench {h.tag}", "mode": "sequential", "repeats": 1,
                 "servers": [{"server_id": hid, "models": [{"model": BENCH_MODEL}]}],
                 "scenarios": [{"name": "QA exit duel", "map_size": "duel", "map_type": "pangaea", "seed": 3421,
                                "opponents": 1, "turn_limit": 5, "barbarians": "off", "max_turn_minutes": 5}]}
        bench_t0 = time.time()
        started = admin.post("/api/benchmarks/runs", json={"suite": suite, "name": f"QA exit bench {h.tag}"})
        run_id = started.json().get("id", "") if started.status_code == 200 else ""
        live = wait_until(lambda: {"benchmark", "game"} <= {x.get("kind") for x in
                                                            _machine_status(admin, hid).get("leases") or []}, 120, 0.25)
        leases = [(x.get("kind"), x.get("active")) for x in _machine_status(admin, hid).get("leases") or []]
        r.check("on alice's turn the administrator's one-job benchmark starts on the helper machine's second slot, "
                "its lease beside the game's (the job rule: one lease of others' on a two-slot machine)",
                started.status_code == 200 and bool(live), f"turn {t_b}; HTTP {started.status_code}; leases {leases}")
        hb = opened(a.tool("open_negotiation", {"to": 1, "message": "QA-BENCH: maps for gold?",
                                                "give": [{"type": "gold", "amount": 15}],
                                                "receive": [{"type": "share_map"}]}))
        fell = wait_until(lambda: (head(hb).get("clock") or {}).get("waiting_on") != 1, 30, 0.25) if hb else False
        r.check("a trade for the hybrid seat's model while the benchmark holds the machine: its pooled prompt is "
                "refused and the seat's bot answers it in the lobby game (fallback, cause queued_behind_benchmark)",
                bool(fell) and ("fallback", "queued_behind_benchmark") in metric_rows(hb),
                f"chat {hb}; {head(hb).get('status')}; rows {metric_rows(hb)}")
        a.hold.clear()          # the game plays beside the benchmark: its LLM seat on its lease, its pooled prompts not
        done = wait_until(lambda: _runs_job(admin, run_id).get("status") in ("done", "failed", "cancelled"), 300, 1.0) \
            if run_id else False
        job = _runs_job(admin, run_id) if run_id else {}
        bench_t1 = time.time()
        r.check("the benchmark job ran to its end beside the lobby game", bool(done) and job.get("status") == "done",
                json.dumps({k: job.get(k) for k in ("status", "error")} | {"result": {
                    k: (job.get("result") or {}).get(k) for k in ("drain_s", "contaminated", "turns")}})[:400],
                seconds=round(bench_t1 - bench_t0, 1))
        time.sleep(4.0)                     # the ledger's writer inserts what it has every 2 s
        window = [q for q in endpoint.completions() if bench_t0 - 1 <= q["t"] <= bench_t1 + 1]
        bench_steps = [(q["t"], q["t"] + q["seconds"], "bench") for q in window if q.get("model") == BENCH_MODEL]
        pooled_steps = [(q["t"], q["t"] + q["seconds"], f"{q.get('kind')} {q.get('marker') or ''}".strip())
                        for q in window if q.get("model") == endpoint.MODEL
                        and (q.get("kind") in ("diplomat", "plain") or q.get("system", "").startswith("You are the diplomat")
                             or (q.get("marker") or "").startswith("QA-PROBE"))]
        lease_steps = [q for q in window if q.get("model") == endpoint.MODEL and q.get("kind") == "turn"]
        overlap = _spans_overlap(pooled_steps, bench_steps)
        ev_bench = _event_spans(state, hid, "activity_kind = 'benchmark'")
        ev_pooled = _event_spans(state, hid, "task_class = 'partial'")
        ev_overlap = _spans_overlap(ev_pooled, ev_bench)
        r.check("no pooled request overlapped a benchmark step, by the endpoint's log and by the compute ledger's "
                "events (the scheduler's listener); the game's LLM seat played on its lease beside the benchmark",
                len(bench_steps) >= 3 and not overlap and len(ev_bench) == len(bench_steps) and not ev_overlap,
                f"{len(bench_steps)} benchmark steps, {len(pooled_steps)} pooled and {len(lease_steps)} lease "
                f"steps at the endpoint meanwhile; ledger: {len(ev_bench)} benchmark and {len(ev_pooled)} pooled "
                f"events; overlaps {overlap[:3]} / {ev_overlap[:3]}", bench_steps=len(bench_steps),
                pooled_steps=len(pooled_steps), lease_steps=len(lease_steps))
        data["benchmark"] = {"run": run_id, "job": {k: job.get(k) for k in ("status", "error", "result")},
                             "steps": len(bench_steps), "pooled_meanwhile": [p[2] for p in pooled_steps],
                             "lease_steps_meanwhile": len(lease_steps)}

        # ---- a manual hardware entry survives the helper's restart; the Renaissance for alice's minimap
        t_hw = at_alices_turn()
        listing = bob.get("/api/servers").json().get("servers", [])
        view = next((sv for sv in listing if sv.get("id") == hid), None) or {}
        view["hardware"] = json.loads(json.dumps(QA_HAND_SET))
        saved_hw = bob.post("/api/servers", json=view).status_code
        manual = bob.put(f"/api/machines/{hid}/settings", json={"modes": {"hardware": "manual"}})
        before = (bob.get(f"/api/machines/{hid}/settings").json().get("report") or {}).get("at")
        for helper in helpers:
            helper.stop()
        wait_until(lambda: not _online(bob, hid), 15, 0.1)
        second_scan = files / "scan-2.json"
        second_scan.write_text(json.dumps(_qa_scan(cpu="QA exit CPU 2", gpu="QA exit GPU 24 GB", vram=24, idle=40,
                                                   cpu_w=90, gpu_w=250, host="qa-exit-box"), indent=1),
                               encoding="utf-8")
        helpers.append(Helper(h, token, endpoint, "qa-exit-helper", hardware=second_scan, max_concurrent=2,
                              machine="exit"))
        fresh = wait_until(lambda: (lambda s: s if (s.get("report") or {}).get("at") not in (None, before)
                                    and _online(bob, hid) else None)(
            bob.get(f"/api/machines/{hid}/settings").json()), 60, 0.25) or {}
        summary = (fresh.get("configured") or {}).get("hardware_summary", "")
        scanned = (fresh.get("report") or {}).get("hardware") or {}
        r.check("bob types the helper machine's hardware in by hand (hardware manual); its helper restarts with other "
                "hardware: the hand-set entry stays, and the new scan is kept beside it as the last report",
                saved_hw == 200 and manual.status_code == 200 and "QA hand-set GPU" in summary
                and scanned.get("gpus") == [{"name": "QA exit GPU 24 GB", "vram_gb": 24}],
                f"turn {t_hw}; save {saved_hw}, manual {manual.status_code}; configured {summary!r}; scan "
                f"{scanned.get('gpus')}")
        renaissance = alice.post(f"/api/games/{gid}/debug/ops", json=[{"op": "grant_era", "player": 0,
                                                                       "era": "Renaissance era"}])
        block = alice.get(f"/api/games/{gid}/view", params={"token": tokens[0]}).json().get("minimap") or {}
        minimap.append({"step": "the Renaissance's techs", "status": renaissance.status_code,
                        "stage": block.get("stage"), "home": block.get("home")})
        a.hold.clear()
        played_on = wait_until(lambda: (turn_of()[2] or 0) > (t_hw or 0), 120, 0.5)
        r.check("the game played on after the helper came back", bool(played_on), f"turn {turn_of()[2]}")

        # ---- the server killed with a chat waiting on the LLM seat, and brought back
        t_k = at_alices_turn()
        block = alice.get(f"/api/games/{gid}/view", params={"token": tokens[0]}).json().get("minimap") or {}
        information = alice.post(f"/api/games/{gid}/debug/ops", json=[{"op": "grant_era", "player": 0,
                                                                        "era": "Information era"}])
        block = alice.get(f"/api/games/{gid}/view", params={"token": tokens[0]}).json().get("minimap") or {}
        minimap.append({"step": "the Information era's techs", "status": information.status_code,
                        "stage": block.get("stage"), "home": block.get("home")})
        marker = f"QA-RESTART-{h.tag}"
        # the LLM seat's answer to that chat is held (not its turn's prompt, whose briefing may list the chat)
        endpoint.hold = lambda body: (lambda d: d["kind"] == "chat" and d["marker"] == marker)(
            FakeEndpoint.describe(body))
        endpoint.gate.clear()
        kr = opened(a.tool("open_negotiation", {"to": 2, "message": f"{marker}: are you still there?"}))
        held = wait_until(lambda: endpoint.held >= 1, 30, 0.1)
        saves = state / "saves" / gid / "autosave.citar"
        mtime = saves.stat().st_mtime_ns if saves.exists() else 0
        renamed = alice.post(f"/api/games/{gid}/seats/4", json={"name": "QA renamed bot"})
        written = wait_until(lambda: saves.exists() and saves.stat().st_mtime_ns != mtime, 15, 0.1)
        time.sleep(1.0)
        endpoint.hold = None                 # what comes after the restart is answered; the held answer stays held
        # the scheduler's counters live in the server's memory: the restart starts them again
        counters_before.update(_machine_status(admin, hid).get("counters") or {})
        t_kill = time.perf_counter()
        h.server.restart()
        t_up = time.perf_counter()
        for who in h.accounts.values():
            who.reconnect()
        back = _game_info(alice, gid)
        answered = wait_until(lambda: head(kr).get("status") != "open"
                              or (head(kr).get("clock") or {}).get("waiting_on") == 0, 90, 0.25)
        t_answer = time.perf_counter()
        endpoint.gate.set()
        kv = head(kr)
        theirs = [e for e in kv.get("history") or [] if not e.get("you")]
        # the exchange writes its row once its loop sees the chat move, just after the answer
        kr_rows = wait_until(lambda: (lambda rows: rows if ("answered", None) in rows else None)(metric_rows(kr)),
                             10, 0.25) or metric_rows(kr)
        # the model's own answer: the endpoint's reply (a chat with no proposal), and its metrics row answered; a
        # decline after a failed exchange ("(no response)", fallback) or the clock's would be neither
        r.check("the server, killed with alice's chat waiting on the LLM seat (its answer held at the endpoint; the "
                "seat change's save holds the chat), brought the game back (restore_live) and the model answered the "
                "chat well inside its 45 s, not the chat clock",
                bool(kr and held and renamed.status_code == 200 and written) and back.get("turn") == t_k
                and bool(answered) and bool(theirs)
                and (theirs[-1].get("action"), theirs[-1].get("message")) == ("reply", "(qa endpoint) reply")
                and ("answered", None) in kr_rows and t_answer - t_up < 40,
                f"chat {kr}, held {held}, rename {renamed.status_code}, saved {bool(written)}; back at turn "
                f"{back.get('turn')} (was {t_k}); answered {round(t_answer - t_up, 1)} s after the server was up "
                f"({round(t_up - t_kill, 1)} s down); last entry {(kv.get('history') or [{}])[-1]}; rows {kr_rows}",
                restart_s=round(t_up - t_kill, 1), answered_after_s=round(t_answer - t_up, 1))
        data["restart"] = {"chat": kr, "turn": t_k, "down_s": round(t_up - t_kill, 1),
                           "answered_after_s": round(t_answer - t_up, 1), "history": kv.get("history")}
        # the sockets died with the server: alice's, carol's and the spectator's again
        mine2, carols2, spectator2 = (Socket(h, gid, tokens[0]), Socket(h, gid, "", account=carol),
                                      Socket(h, gid, info.get("spectator_token", "")))
        sockets += [mine2, carols2, spectator2]
        a.hold.clear()

        # ---- the rest of the game
        over = wait_until(lambda: turn_of()[0] not in ("playing", None), 900, 1.0)
        gi = _game_info(alice, gid)
        r.check("the game played on to its end (turn limit 40)", bool(over) and gi.get("phase") != "playing",
                f"phase {gi.get('phase')}, turn {gi.get('turn')}", seconds=round(time.time() - t_start, 1))
        got = alice.get(f"/api/games/{gid}/metrics")
        metrics = got.json() if got.status_code == 200 else {}
        summary = metrics.get("summary") or {}
        llm_turns = [x for x in metrics.get("turns") or [] if x.get("player") == 2 and x.get("ended")
                     and not x.get("interrupted")]
        phases = [x for x in metrics.get("diplomat") or [] if x.get("player") == 1]
        turns = [x["turn"] for x in phases]
        hyb = summary.get("1") or {}
        r.check("the LLM seat played its turns on the helper machine through the scripted endpoint, each ended by "
                "its own end_turn",
                len(llm_turns) >= 35 and all(x.get("end_reason") == "end_turn" for x in llm_turns)
                and all(x.get("model_steps", 0) >= 1 for x in llm_turns),
                f"{len(llm_turns)} turns; end reasons {sorted({x.get('end_reason') for x in llm_turns})}",
                turns=len(llm_turns))
        r.check("the hybrid seat: its bot played every turn, its diplomat at most once a turn (at least every three "
                "turns while its machine was free), its model and its bot answering chats",
                len(phases) >= 8 and len(turns) == len(set(turns))
                and any(x.get("outcome") == "done" for x in phases)
                and (hyb.get("chat_answers") or {}).get("answered", 0) >= 1
                and (hyb.get("chat_answers") or {}).get("fallback", 0) >= 2
                and hyb.get("avg_bot_actions", 0) > 0,
                json.dumps({"phases": len(phases), "outcomes": sorted({f"{x.get('outcome')}/{x.get('cause')}"
                                                                        for x in phases}),
                            "chat_answers": hyb.get("chat_answers"), "causes": hyb.get("chat_fallback_causes")}))
        waits_with_chats = [w for w in m.waits if w.get("chats")]
        r.check("the MCP seat was played through the wait route to the end: its turns ended, and every wait answer "
                "that listed chats gave each its deadline and seconds left",
                len(m.ended) >= 35 and m.finished.is_set() and bool(waits_with_chats)
                and all(c.get("deadline") and c.get("seconds_left") is not None
                        for w in waits_with_chats for c in w["chats"]),
                f"{len(m.ended)} turns ended; {len(m.waits)} waits, {len(waits_with_chats)} with chats; answered "
                f"{len(m.answered)} chats; {m.errors} errors (the restart)", turns=len(m.ended))
        statuses = mine.statuses() + mine2.statuses()
        states = {s.get("state") for s in statuses}
        others = carols.statuses() + carols2.statuses() + spectator.statuses() + spectator2.statuses()
        heard = [len(x.messages) for x in (carols, carols2, spectator, spectator2)]
        r.check("alice's socket heard her chats' statuses (thinking, waiting with a deadline, closed); carol's (a "
                "member who may watch) and a spectator's heard none, though they heard the game",
                {"thinking", "waiting", "closed"} <= states and not others and all(n > 1 for n in heard)
                and all(x and x.get("spectator") for x in hello),
                f"alice {len(statuses)} statuses {sorted(s for s in states if s)}; carol and the spectator "
                f"{len(others)}; their messages {heard}; hello {hello}", statuses=len(statuses))
        homes = [x.get("home") for x in minimap]
        map_w, map_h = (lambda v: (v.get("width", 0), v.get("height", 0)))(
            alice.get(f"/api/games/{gid}/view", params={"token": tokens[0]}).json())
        grew = [i + 1 for i, (p, q) in enumerate(zip(homes, homes[1:])) if p and q and p != q]
        r.check("alice's minimap moved from early to renaissance to information by the debug ops route, its home box "
                "growing as her units explored and never shrinking",
                [x.get("stage") for x in minimap] == ["early", "renaissance", "information"]
                and all(x.get("status", 200) == 200 for x in minimap)
                and all(p and q and _box_covers(q, p, map_w, map_h) for p, q in zip(homes, homes[1:]))
                and bool(grew), json.dumps({"steps": minimap, "grew_at": grew,
                                            "exploring": (a.first or {}).get("exploring")})[:600])
        data["minimap"] = minimap
        status = _machine_status(admin, hid)
        after = status.get("counters") or {}
        # the whole game's: the readings before the restart and since (the scheduler's counters start again with it)
        counters = {k: int(counters_before.get(k, 0) or 0) + int(after.get(k, 0) or 0)
                    for k in sorted(set(counters_before) | set(after))}
        busy_lines = _busy_lines(h)
        r.check("the helper machine refused nothing as busy over the whole game (the scheduler's counters before the "
                "restart and after it, and the server's log) and holds no lease once the game is over",
                counters.get("busy_refusals", 0) == 0 and counters.get("granted", 0) > 0 and not busy_lines
                and not status.get("leases"),
                json.dumps({"counters": counters, "before_restart": counters_before, "after_restart": after,
                            "leases": status.get("leases")})[:600])
        data["counters"] = {"whole_game": counters, "before_restart": counters_before, "after_restart": after}

        # ---- the ledger: given and used, per member, against its events
        time.sleep(4.0)
        agreed, seen = _ledger_agrees(h, {"bob": ("given", "own_use"), "alice": ("used",), "admin": ("used",)})
        ids = dict(_ledger_rows(state, "SELECT handle, id FROM users"))
        by_act = _ledger_rows(state, "SELECT activity_kind, task_class, consumer_id, provider_id, self_use, count(*), "
                                     "sum(credits) FROM compute_events WHERE machine_id IN (?, ?) AND kind = 'gpu' "
                                     "GROUP BY 1, 2, 3, 4, 5", (hid, dry_id))
        names = {v: k for k, v in ids.items()}
        rows = sorted((kind, cls, names.get(c), names.get(p), bool(s), n) for kind, cls, c, p, s, n, _ in by_act)
        his = ((seen.get("breakdown") or {}).get("bob") or {})
        his_machines = {x.get("id"): x for x in his.get("by_machine") or []}
        machine_given = _ledger_rows(state, "SELECT coalesce(sum(credits), 0) FROM compute_events WHERE machine_id = ? "
                                            "AND provider_id = ? AND self_use = 0", (hid, ids.get(bob.handle)))[0][0]
        expected = {("game", "full", alice.handle, bob.handle, False), ("game", "partial", alice.handle, bob.handle,
                                                                          False),
                    ("probe", "partial", alice.handle, bob.handle, False),
                    ("report", "partial", alice.handle, bob.handle, False),
                    ("report", "partial", bob.handle, bob.handle, True),
                    ("benchmark", "full", admin.handle, bob.handle, False)}
        got_kinds = {row[:5] for row in rows}
        r.check("the ledger's events on bob's machines: alice's game (its full and its pooled seat), probe and report "
                "and the administrator's benchmark given by bob; bob's own report his own use; bob's given, alice's "
                "and the administrator's used and bob's own use, as /api/ledger/me tells each, equal the events' sums",
                agreed and expected <= got_kinds
                and abs((his_machines.get(hid) or {}).get("given", -1) - machine_given) < 1e-3,
                json.dumps({"rows": rows, "told": seen.get("told"), "events": seen.get("events"),
                            "helper_given": [machine_given, (his_machines.get(hid) or {}).get("given")]},
                           default=str)[:900])
        data["ledger"] = {"rows": rows, "told": seen.get("told"), "events": seen.get("events")}
        data["seats"] = {"llm_turns": len(llm_turns), "diplomat_phases": [
            {k: x.get(k) for k in ("turn", "outcome", "cause", "steps", "tools")} for x in phases],
            "hybrid_summary": {k: hyb.get(k) for k in ("chat_answers", "chat_fallback_causes", "diplomat_phases",
                                                       "diplomat_outcomes", "avg_bot_actions")},
            "mcp": {"turns": len(m.ended), "chats_answered": len(m.answered), "waits": len(m.waits),
                    "first": m.first}, "alice": {"turns": len(a.ended), "chats_answered": len(a.answered),
                                                "left": sorted(a.left), "first": a.first}}
        data["endpoint"] = {"completions": len(endpoint.completions()), "trades": endpoint.trades_opened,
                            "by_kind": {k: sum(1 for q in endpoint.completions() if q.get("kind") == k)
                                        for k in ("turn", "diplomat", "chat", "plain")}}
    finally:
        endpoint.hold = None
        endpoint.gate.set()
        for d in drivers:
            d.stop()
        for s in sockets:
            s.close()
        if run_id:
            try:
                admin.post(f"/api/benchmarks/runs/{run_id}/control", json={"action": "cancel"})
            except httpx.HTTPError:
                pass
        if gid and not h.keep:
            try:
                alice.delete(f"/api/games/{gid}")
            except httpx.HTTPError:
                pass
        for helper in helpers:
            helper.stop()
        endpoint.close()
        trap.close()


# ---------------------------------------------------------------------------- the real part (3-19)

#: The ``real`` part's model server (the owner's decision, 2026-10-11): the laptop's LM Studio, OpenAI-compatible, and
#: the model the owner keeps loaded there. The part only reads LM Studio's model list: it never loads, unloads or swaps
#: a model, changes LM Studio's settings or names another model.
REAL_URL = "http://localhost:1234/v1"
REAL_MODEL = "google/gemma-4-e2b"
#: The ``real`` part's game: a small map, 20 rounds; the chat timeouts long enough for a small model on a laptop.
REAL_CONFIG = {"map_size": "small", "map_type": "pangaea", "seed": 3431, "barbarians": "off", "city_states": 0,
               "turn_limit": 20, "chat_model_reply_seconds": 300, "chat_person_reply_seconds": 120}
#: How long the ``real`` part waits for its game to end, and for a turn that does not move: longer than the LLM seat's
#: own limit on its turn (``REAL_TURN_S``, its model's time; its waits for chats come on top) and a hybrid seat's
#: diplomat phase (three minutes), so only a hang is taken for one.
REAL_LIMIT_S = 7200.0
REAL_STALL_S = 1800.0
REAL_TURN_S = 600


def lmstudio_model(base: str = REAL_URL, model: str = REAL_MODEL, timeout: float = 10.0) -> tuple:
    """Whether LM Studio at ``base`` lists ``model`` and has it loaded (``GET /api/v0/models``, ``state ==
    "loaded"``): ``(ready, what it said)``, the words saying what to do when it is not. Only reads."""
    root = base.rstrip("/")
    root = root[:-3] if root.endswith("/v1") else root
    try:
        got = httpx.get(f"{root}/api/v0/models", timeout=timeout)
    except httpx.HTTPError as e:
        return False, (f"LM Studio does not answer at {root} ({type(e).__name__}): start it and its server, with "
                       f"{model} loaded")
    try:
        rows = got.json().get("data") if got.status_code == 200 else None
    except ValueError:
        rows = None
    if not isinstance(rows, list):
        return False, f"LM Studio at {root} gave no model list (HTTP {got.status_code})"
    row = next((x for x in rows if isinstance(x, dict) and x.get("id") == model), None)
    if row is None:
        listed = ", ".join(str(x.get("id")) for x in rows if isinstance(x, dict)) or "none"
        return False, f"{model} is not among LM Studio's models ({listed}): the part never fetches or loads a model"
    if row.get("state") != "loaded":
        return False, (f"{model} is in LM Studio but {row.get('state') or 'not loaded'}: load it there first (the part "
                       "never loads, unloads or swaps a model)")
    context = row.get("loaded_context_length") or row.get("max_context_length")
    return True, f"{model} is loaded in LM Studio at {root} (context {context})"


def part_real(h: Harness) -> None:
    """3-19's ``real`` part (design/phase3-features.md Appendix D, 3-19; the owner's decision of 2026-10-11): the
    plumbing against a real model. Bob's helper machine (a real Python helper, one slot) has the laptop's LM Studio as
    its upstream, serving ``REAL_MODEL``, which the owner keeps loaded; the part first checks that LM Studio lists it
    loaded and fails with what to do otherwise, and it never loads, unloads or swaps a model, changes LM Studio's
    settings or uses another model. Bob shares the machine with alice for games and pooled prompts. Alice's game seats
    her person seat (played by the script through the wait route), a hybrid seat (the standard bot and the model, all
    but war, prompted every 3 turns) and a full LLM seat on the machine, and two bots, 20 rounds. On her first turn she
    opens a chat with each model seat; their offers are answered as they come; her report's analysis runs on the
    machine as a pooled prompt. The model is small, so this tests the plumbing, not the play. The evidence: the turns
    each model seat played and how they ended, the tool calls made and refused, the chats with their replies, causes
    and times, every fallback to the bot with its cause, and the generation seconds the scheduler metered against the
    compute ledger's events for the machine."""
    r = h.report
    alice, bob = h.accounts["alice"], h.accounts["bob"]
    data = r.data.setdefault("real", {})
    ready, said = lmstudio_model(REAL_URL, REAL_MODEL)
    data["lmstudio"] = {"url": REAL_URL, "model": REAL_MODEL, "before": said}
    if not r.check(f"LM Studio at {REAL_URL} lists {REAL_MODEL}, loaded (read only: GET /api/v0/models)", ready, said):
        return
    state = h.server.state
    helper: Optional[Helper] = None
    drivers: list = []
    gid = ""
    t0 = time.time()
    try:
        made = bob.post("/api/pool/servers", json={"config": {"name": f"QA real LM Studio {h.tag}", "kind": "owned",
                                                              "connection": {"provider": "openai_compatible",
                                                                             "max_parallel": 1}},
                                                   "reach": "worker"})
        hid = made.json().get("id", "") if made.status_code == 200 else ""
        issued = bob.post(f"/api/pool/servers/{hid}/workers/tokens", json={"label": "qa real"}) if hid else None
        token = issued.json().get("token", "") if issued is not None and issued.status_code == 200 else ""
        if token:
            helper = Helper(h, token, REAL_URL, "qa-real-helper")
        group = bob.post("/api/pool/groups", json={"name": f"QA real group {h.tag}", "visibility": "private"})
        group_id = group.json().get("id", "") if group.status_code == 200 else ""
        put = bob.put(f"/api/pool/servers/{hid}", json={"group_id": group_id})
        grant = bob.post(f"/api/pool/groups/{group_id}/grants",
                         json={"subject_type": "user", "handle": alice.handle, "purposes": ["game", "pooled"]})
        online = bool(helper) and bool(wait_until(lambda: _online(bob, hid), 60, 0.25))
        card = next((s for s in bob.get("/api/pool/servers").json().get("servers", []) if s.get("id") == hid), {})
        live = {m.get("key"): m.get("loaded") for m in card.get("live_models") or []}
        r.check("bob's helper machine, its real helper in front of LM Studio (one slot), reports the model loaded; "
                "shared with alice for games and pooled prompts",
                online and live.get(REAL_MODEL) is True and (put.status_code, grant.status_code) == (200, 200),
                f"machine {made.status_code}, online {online}, live models {live}; put {put.status_code}, grant "
                f"{grant.status_code}")
        data["machine"] = hid
        if not online:
            return
        model = {"server_id": hid, "model": REAL_MODEL}
        seats = [{"type": "human"},
                 {"type": "hybrid", "llm": dict(model), "bot": {"profile": "standard"},
                  "hybrid": {"owners": "all_but_war", "prompt_every": 3}},
                 {"type": "llm", "llm": {**model, "max_turn_seconds": REAL_TURN_S}}, {"type": "bot"}, {"type": "bot"}]
        g = alice.post("/api/games", json={"name": f"QA real game {h.tag}", "config": dict(REAL_CONFIG),
                                           "seats": seats})
        info = g.json() if g.status_code == 200 else {}
        gid = info.get("id", "")
        tokens = [s.get("token", "") for s in info.get("seats") or []]
        r.check(f"alice's game: her person seat, a hybrid seat (all but war, prompted every 3 turns) and a full LLM "
                f"seat on {REAL_MODEL}, two bots, 20 rounds",
                bool(gid) and [s.get("type") for s in info.get("seats") or []] == ["human", "hybrid", "llm", "bot", "bot"],
                f"HTTP {g.status_code} {g.text[:200] if g.status_code != 200 else ''}")
        if not gid:
            return
        data["game"] = gid
        alice.post(f"/api/games/{gid}/debug/meet_all")
        alice.post(f"/api/games/{gid}/debug/gold")
        a = SeatDriver(h, gid, tokens[0], 0, "alice")
        drivers.append(a)
        a.hold.set()
        wait_until(lambda: a.first is not None, 120, 0.5)

        def opened(res: dict) -> Optional[int]:
            return (res.get("result") or {}).get("negotiation_id") if res.get("ok") else None
        chats = {"hybrid": opened(a.tool("open_negotiation", {
                     "to": 1, "message": "Hello neighbour. Would you trade your map for some of my gold?",
                     "give": [{"type": "gold", "amount": 30}], "receive": [{"type": "share_map"}]})),
                 "llm": opened(a.tool("open_negotiation", {
                     "to": 2, "message": "Hello neighbour. Shall we share our maps?",
                     "give": [{"type": "share_map"}], "receive": [{"type": "share_map"}]}))}
        t_chats = time.time()
        r.check("on her first turn alice opened a chat with each model seat", all(chats.values()), json.dumps(chats))
        a.hold.clear()
        rep = alice.post("/api/reports", json={"title": f"QA real report {h.tag}", "sections": ["summary"],
                                               "narrative": {"enabled": True, "server_id": hid,
                                                             "model_id": REAL_MODEL}})
        rid = rep.json().get("id", "") if rep.status_code == 200 else ""

        # ---- the game, to its end: a turn that does not move for REAL_STALL_S is a hang
        last, moved, progress = None, time.perf_counter(), []
        deadline = time.perf_counter() + REAL_LIMIT_S
        while time.perf_counter() < deadline:
            gi = _game_info(alice, gid)
            mark = (gi.get("turn"), gi.get("current_player"))
            if gi.get("phase") not in ("playing", None):
                break
            if mark != last:
                if last is None or mark[0] != last[0]:
                    r.note(f"real: turn {mark[0]} ({round(time.time() - t0)} s)")
                last, moved = mark, time.perf_counter()
                progress.append({"at_s": round(time.time() - t0, 1), "turn": mark[0], "player": mark[1]})
            elif time.perf_counter() - moved > REAL_STALL_S:
                break
            time.sleep(2.0)
        gi = _game_info(alice, gid)
        stalled = gi.get("phase") == "playing"
        r.check("the game was played to its end (20 rounds), no turn hanging", not stalled,
                f"phase {gi.get('phase')}, turn {gi.get('turn')}, player {gi.get('current_player')}; "
                f"{round(time.time() - t0)} s", seconds=round(time.time() - t0, 1), turn=gi.get("turn"))
        if rid:
            wait_until(lambda: ((alice.get(f"/api/reports/{rid}").json().get("meta") or {}).get("status")
                                in ("done", "failed")), 900, 2.0)
        meta = (alice.get(f"/api/reports/{rid}").json().get("meta") or {}) if rid else {}
        time.sleep(4.0)                 # the ledger's writer inserts what it has every 2 s
        got = alice.get(f"/api/games/{gid}/metrics")
        metrics = got.json() if got.status_code == 200 else {}
        rows = metrics.get("turns") or []
        llm_turns = [x for x in rows if x.get("player") == 2]
        hyb_turns = [x for x in rows if x.get("player") == 1]
        phases = [x for x in metrics.get("diplomat") or [] if x.get("player") == 1]
        refusals = [x for x in metrics.get("diplomat_refusals") or [] if x.get("player") == 1]
        negotiations = metrics.get("negotiations") or []
        ended = [x for x in llm_turns if x.get("ended")]
        acted = [x for x in llm_turns if (x.get("actions_ok") or 0) >= 1]
        phase_tools = [t for x in phases for t in x.get("tools") or []]
        r.check("every model turn ended, played by its model or ended with its cause recorded, none hung: the LLM "
                "seat's turns each have an end reason, the hybrid seat's turns are its bot's",
                not stalled and len(llm_turns) >= 19 and len(ended) == len(llm_turns)
                and all(x.get("end_reason") for x in ended) and len(hyb_turns) >= 19,
                json.dumps({"llm_turns": len(llm_turns), "end_reasons": _counts(x.get("end_reason") for x in ended),
                            "hybrid_turns": len(hyb_turns)}))
        r.check("each model seat played through its own tool calls: the LLM seat's actions on its turns, the hybrid "
                "seat's model's in its diplomat phases",
                bool(acted) and any(t.get("ok") for t in phase_tools),
                json.dumps({"llm_turns_with_actions": len(acted), "phases": len(phases),
                            "phase_tools": _counts(f"{t.get('tool')} {'ok' if t.get('ok') else 'refused'}"
                                                   for t in phase_tools)})[:500])
        mine = {k: a.chat(nid) for k, nid in chats.items() if nid is not None}
        rows_of = {k: [(x.get("outcome"), x.get("cause")) for x in negotiations if x.get("nid") == nid]
                   for k, nid in chats.items()}
        decided = all(v.get("status") not in (None, "open") for v in mine.values()) and all(rows_of.values())
        # the model answered, or the chat's rules ended it: the clock (its decline, or a hybrid's bot at the deadline or
        # when the ticket could not start in the time left), the machine (a refusal; behind a benchmark), or the chat
        # closing before the answer. A model that failed to answer (its steps used, text in place of the call, an
        # error: ``fallback``/``failed``) is not one of these
        wrong = [x for x in negotiations if not _chat_ended_by_rule(x)]
        by_model = {seat: sum(1 for x in negotiations if x.get("player") == p and x.get("outcome") == "answered")
                    for seat, p in (("hybrid", 1), ("llm", 2))}
        r.check("every chat was answered by the model or ended by the chat clock's rule, its cause recorded (none "
                "failed by the model): alice's two, and every chat answer of the model seats; each model seat's model "
                "answered at least one by calling its tool",
                decided and bool(negotiations) and not wrong and all(by_model.values()),
                json.dumps({"alice": {k: [v.get("status"), rows_of[k]] for k, v in mine.items()},
                            "answered_by_model": by_model,
                            "not_by_rule": [{k: x.get(k) for k in ("player", "nid", "outcome", "cause", "steps")}
                                            for x in wrong],
                            "outcomes": _counts(f"{x.get('player')} {x.get('outcome')}/{x.get('cause')}"
                                                for x in negotiations)})[:800])
        # the generation seconds the scheduler metered (the game's metrics: the LLM seat's turns, the chat answers, the
        # diplomat phases) against the compute ledger's events for the game on the machine
        metered = (sum(float(x.get("model_s") or 0.0) for x in llm_turns)
                   + sum(float(x.get("gen_s") or 0.0) for x in negotiations)
                   + sum(float(x.get("gen_s") or 0.0) for x in phases))
        pieces = len(llm_turns) + len(negotiations) + len(phases)
        # the metrics keep each record's seconds to the millisecond, a turn's summed step by step
        steps = (sum(int(x.get("model_steps") or 0) for x in llm_turns)
                 + sum(int(x.get("steps") or 0) for x in negotiations) + sum(int(x.get("steps") or 0) for x in phases))
        slack = 0.001 * (steps + pieces) + 0.01
        events = _ledger_rows(state, "SELECT count(*), coalesce(sum(gen_s), 0), coalesce(sum(observed_s), 0), "
                                     "coalesce(sum(credits), 0) FROM compute_events WHERE machine_id = ? AND "
                                     "kind = 'gpu' AND activity = ?", (hid, f"game:{gid}"))[0]
        report_events = _ledger_rows(state, "SELECT count(*), coalesce(sum(gen_s), 0) FROM compute_events WHERE "
                                            "machine_id = ? AND activity = ?", (hid, f"report:{rid}"))[0]
        r.check("the ledger's events for the game on the machine add up to the generation seconds the scheduler "
                "metered in its metrics, and her report's analysis has its events",
                events[0] > 0 and abs(events[1] - metered) <= slack and report_events[0] >= 1
                and meta.get("status") == "done" and meta.get("narrative_written") is True,
                f"metered {metered:.3f} s over {pieces} records ({steps} steps, within {slack:.3f} s); ledger "
                f"{events[1]:.3f} s over {events[0]} events "
                f"(observed {events[2]:.1f} s, {events[3]:.2f} credits); report {meta.get('status')}, written "
                f"{meta.get('narrative_written')}, {report_events[0]} events, {report_events[1]:.2f} s",
                metered_s=round(metered, 3), ledger_s=round(events[1], 3))
        after, said_after = lmstudio_model(REAL_URL, REAL_MODEL)
        r.check("LM Studio still lists the model loaded, as before the part", after, said_after)
        fallbacks = [{"where": "chat", "nid": x.get("nid"), "player": x.get("player"), "outcome": x.get("outcome"),
                      "cause": x.get("cause")} for x in negotiations if x.get("outcome") != "answered"]
        fallbacks += [{"where": "diplomat phase", "turn": x.get("turn"), "outcome": x.get("outcome"),
                       "cause": x.get("cause")} for x in phases if x.get("outcome") not in ("done",)]
        # the tool calls the LLM seat made on its turns, and those the game refused (its metrics' turn rows: each
        # turn's counts and its most used tools)
        calls_made = {k: sum(int(x.get(k) or 0) for x in llm_turns)
                      for k in ("tool_calls", "actions_ok", "queries", "errors", "repeats", "malformed")}
        data.update({
            "lmstudio": {**data["lmstudio"], "after": said_after}, "seconds": round(time.time() - t0, 1),
            "progress": progress[-60:],
            "llm_seat": {"turns": len(llm_turns), "end_reasons": _counts(x.get("end_reason") for x in llm_turns),
                         "model_s": round(sum(float(x.get("model_s") or 0) for x in llm_turns), 2),
                         "queue_s": round(sum(float(x.get("queue_s") or 0) for x in llm_turns), 2),
                         "steps": sum(int(x.get("model_steps") or 0) for x in llm_turns),
                         "tool_calls": calls_made,
                         "turn_rows": [{k: x.get(k) for k in ("turn", "wall_s", "model_steps", "model_s", "queue_s",
                                                              "tool_calls", "actions_ok", "queries", "errors",
                                                              "top_tools", "end_reason")}
                                       for x in llm_turns]},
            "hybrid_seat": {"turns": len(hyb_turns),
                            "bot_actions": sum(sum(v[0] for v in (x.get("bot_actions") or {}).values())
                                               for x in hyb_turns),
                            "phases": [{k: x.get(k) for k in ("turn", "triggers", "steps", "gen_s", "queue_s", "tools",
                                                              "outcome", "cause", "seconds")} for x in phases],
                            "refused_calls": refusals},
            "chats": {"alice": {k: {"id": chats[k], "status": v.get("status"), "rows": rows_of[k],
                                    "history": [(e.get("by"), e.get("action"), str(e.get("message") or "")[:160],
                                                 e.get("note")) for e in v.get("history") or []]}
                                for k, v in mine.items()},
                      "opened_at": t_chats,
                      "answers": [{k: x.get(k) for k in ("player", "nid", "outcome", "cause", "seconds", "gen_s",
                                                         "queue_s", "steps")} for x in negotiations]},
            "fallbacks": fallbacks,
            "metered_gen_s": round(metered, 3),
            "ledger": {"events": events[0], "gen_s": round(events[1], 3), "observed_s": round(events[2], 3),
                       "credits": round(events[3], 4), "report_events": report_events[0],
                       "report_gen_s": round(report_events[1], 3)},
            "report": {"id": rid, "status": meta.get("status"), "written": meta.get("narrative_written"),
                       "note": meta.get("narrative_note")},
            "alice": {"turns": len(a.ended), "answered": {str(k): v for k, v in a.answered.items()}}})
    finally:
        for d in drivers:
            d.stop()
        if gid and not h.keep:
            try:
                alice.post(f"/api/games/{gid}/control", json={"paused": True})
                alice.delete(f"/api/games/{gid}")
            except httpx.HTTPError:
                pass
        if helper is not None:
            helper.stop()


#: How a model seat's chat answer may end other than by its model's answer (the game's metrics, ``outcome`` and
#: ``cause``; citar/server/chat.py's CAUSES): by the chat clock (declined late; a hybrid seat's bot at the deadline, or
#: when its model's ticket could not start in the time left), by the machine (a refusal, a benchmark holding it), or
#: by the chat closing before the answer (``closed``, no cause). ``fallback`` with ``failed`` is the model's failure.
RULED_ENDINGS = {"declined_late": {"timeout"},
                 "fallback": {"timeout", "not_started", "queued_behind_benchmark", "refused"}}


def _chat_ended_by_rule(row: dict) -> bool:
    """Whether a chat answer's metrics row was the model's answer or an ending by the chat's rules (``RULED_ENDINGS``)."""
    outcome, cause = row.get("outcome"), row.get("cause")
    if outcome in ("answered", "closed"):
        return cause is None
    return cause in RULED_ENDINGS.get(outcome, ())


def _counts(items) -> dict:
    """How many of each item (a tally, sorted by item)."""
    out: dict = {}
    for x in items:
        out[str(x)] = out.get(str(x), 0) + 1
    return dict(sorted(out.items()))


#: The parts written so far, in the order ``all`` runs them.
PARTS: dict[str, Callable[[Harness], None]] = {"accounts": part_accounts, "queue": part_queue, "ledger": part_ledger,
                                               "chat": part_chat, "hybrid": part_hybrid, "minimap": part_minimap,
                                               "machines": part_machines, "exit": part_exit, "real": part_real}


# ---------------------------------------------------------------------------- the run

#: The runs that make a third member (carol): ``all``, and its ``exit`` part on its own.
EVERYONE = ("all", "exit")
#: The game limit ``all`` gives its members: its parts run one after another on one server, and each leaves its games
#: open (paused, finished, or kept for the browser checklist) where a part on its own server would have gone with it.
ALL_GAME_LIMIT = 20


def raise_game_limits(accounts: dict, report: Report) -> None:
    """For a run of several parts: alice's and carol's game limit raised to ``ALL_GAME_LIMIT`` by the administrator
    (bob's is left as it is: the ``accounts`` part sets and checks it, and his games stay under the default)."""
    admin = accounts["admin"]
    done = {}
    for key in ("alice", "carol"):
        if key not in accounts:
            continue
        me = accounts[key].get("/api/auth/me").json().get("user") or {}
        got = admin.patch(f"/api/admin/users/{me.get('id')}", json={"max_concurrent_games": ALL_GAME_LIMIT})
        if got.status_code != 200:
            raise HarnessError(f"could not raise {accounts[key].handle}'s game limit: HTTP {got.status_code} "
                               f"{got.text[:200]}")
        done[accounts[key].handle] = ALL_GAME_LIMIT
    report.note(f"game limits raised for the run of several parts: {done}")
    report.data.setdefault("server", {})["game_limits"] = done


def make_accounts(server: Server, report: Report, everyone: bool) -> dict:
    """The administrator, two members and a moderator (and a third member for ``all`` and ``exit``), made with
    ``admin_cli create-user`` and signed in. Raises HarnessError when one cannot be made or sign in: no check means
    anything without them."""
    password = "qa-" + secrets.token_hex(10)
    wanted = [("admin", "qa-admin", "admin"), ("alice", "qa-alice", "user"), ("bob", "qa-bob", "user"),
              ("mod", "qa-mod", "moderator")] + ([("carol", "qa-carol", "user")] if everyone else [])
    out = {}
    for key, handle, role in wanted:
        made = server.cli("create-user", "--handle", handle, "--email", f"{handle}@example.com", "--role", role,
                          "--password-stdin", stdin=password + "\n")
        if made.returncode != 0:
            raise HarnessError(f"admin_cli create-user {handle} failed: {made.stdout.strip()} "
                               f"{made.stderr.strip()[-400:]}")
        account = Account(server, handle, role, password)
        login = account.login()
        if login.status_code != 200:
            raise HarnessError(f"{handle} could not sign in: HTTP {login.status_code} {login.text[:200]}")
        out[key] = account
    report.check("the accounts were made with admin_cli create-user and signed in", True,
                 ", ".join(f"{a.handle} ({a.role})" for a in out.values()))
    report.data["accounts"] = [{"handle": a.handle, "role": a.role} for a in out.values()]
    # throwaway accounts on a throwaway instance: kept for the browser checklist (--keep)
    report.data["password"] = password
    return out


def _trace_tail(lines: int = 6) -> str:
    """The last lines of the exception being handled, for a failed check's detail."""
    return " | ".join(traceback.format_exc().strip().splitlines()[-lines:])


def write_part(folder: Path, run_of: str, name: str, report: Report) -> None:
    """A part's own evidence in a run of several (``qa-<part>.json``, as a run of that part alone writes it: the
    browser checklist's scripts read them): its checks only, beside the run's data so far. Written as each part ends,
    so the checklist can begin on a part while the run goes on, and again with the rest at the end."""
    mine = report.of(name)
    (folder / f"qa-{name}.json").write_text(json.dumps(
        {"part": name, "of": run_of, "ok": bool(mine) and all(c["ok"] for c in mine), "checks": mine, **report.data},
        indent=2, default=str) + "\n", encoding="utf-8")


def run(args) -> tuple:
    """Refuse (Refused, nothing touched), or claim, start, check and stop. Returns the exit code and the report,
    whose evidence ``main`` writes. Once the directory is claimed nothing raises out of here: a harness failure
    (``HarnessError``) or a part that raises is a failed check, and the checks made before it stand."""
    skip = set(getattr(args, "skip", None) or ())
    parts = [name for name in PARTS if name not in skip] if args.part == "all" else [args.part]
    state = Path(args.dir).expanduser()
    report = Report(args.part)
    if args.part in LATER:
        raise Refused(f"the '{args.part}' part is not written yet (packages {LATER[args.part]})")
    check_dir(state)
    port = args.port or free_port()
    check_port(port)
    # from here on the directory is the harness's: no refusal, every failure a check with its evidence (exit 1)
    state = state.resolve()
    report.data["server"] = {"dir": str(state), "port": port}
    if args.part == "all" and len(parts) < len(PARTS):
        report.data["skipped"] = [name for name in PARTS if name not in parts]
    try:
        claim(state)
    except HarnessError as e:
        report.check("the directory was emptied and marked", False, str(e))
        return 1, report
    server: Optional[Server] = None
    endpoint: Optional[FakeEndpoint] = None
    t0 = time.perf_counter()
    try:
        server = Server(state, port)
        endpoint = FakeEndpoint()
        report.data["server"]["registry_file"] = str(write_registry(state))
        report.data["server"].update({"base": server.base, "public": server.public, "log": str(server.log),
                                      "fake_endpoint": endpoint.url})
        server.start()
        report.note(f"the server is up at {server.public} (dir {state})")
        report.data["server"]["start_s"] = round(time.perf_counter() - t0, 1)
        accounts = make_accounts(server, report, args.part in EVERYONE)
        if len(parts) > 1:
            raise_game_limits(accounts, report)
        harness = Harness(report, server, accounts, endpoint, keep=args.keep,
                          hold_line=getattr(args, "hold_line", 0.0))
        for name in parts:
            report.note(f"part: {name}")
            report.current = name
            began = time.perf_counter()
            try:
                PARTS[name](harness)
            except Exception:
                # the checks already made stand; the rest of this part never ran
                report.check(f"the {name} part ran to its end", False, _trace_tail())
            report.parts[name] = {"seconds": round(time.perf_counter() - began, 1)}
            report.current = None
            if len(parts) > 1:
                if args.keep and server.proc is not None:
                    # where the kept server is, for the checklist's scripts that start on this part's file now
                    report.data["kept"] = {"pid": server.proc.pid, "url": server.public}
                try:
                    write_part(state, args.part, name, report)
                except OSError as e:          # written again at the end; the run goes on
                    report.note(f"qa-{name}.json could not be written yet ({e})")
        tracebacks = server.tracebacks()
        report.check("no traceback in the server's log", tracebacks == 0, f"{tracebacks} in {server.log}")
    except HarnessError as e:
        report.check("the harness started the server and made its accounts", False, str(e))
    except Exception:
        report.check("the harness ran to its end", False, _trace_tail())
    finally:
        if endpoint is not None:
            endpoint.close()
        if server is None:
            pass
        elif args.keep and server.proc is not None and server.proc.poll() is None:
            report.note(f"kept running at {server.public} (pid {server.proc.pid}); stop it when the checklist is done")
            report.data["kept"] = {"pid": server.proc.pid, "url": server.public}
        else:
            server.stop()
    report.data["seconds"] = round(time.perf_counter() - report.started, 1)
    return 0 if report.ok else 1, report


def main(argv=None) -> int:
    """Run the harness."""
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("part", choices=sorted([*PARTS, *LATER, "all"]), help="the part to run, or all")
    ap.add_argument("--port", type=int, default=0, help="a free port by default; never 8799 or one in use")
    ap.add_argument("--dir", default=str(Path(os.environ.get("TEMP") or "/tmp") / "citar-qa-phase3"),
                    help="the state directory: new, empty, or an earlier run's (it is emptied first)")
    ap.add_argument("--keep", action="store_true", help="leave the server running afterwards (the browser checklist)")
    ap.add_argument("--skip", action="append", choices=sorted(PARTS), default=[], metavar="PART",
                    help="all: leave this part out (repeatable; say so when quoting the run, which is then not "
                         "the whole of all)")
    ap.add_argument("--helper-bin", default=None, metavar="PATH",
                    help="the CITAR helper to run (default: citar-helper in CITAR_EXT_DIR, which cargo xtask develop "
                         "builds)")
    ap.add_argument("--hold-line", type=float, default=0.0, metavar="SECONDS",
                    help="queue: keep the pooled prompts waiting this long before they are served (the browser "
                         "checklist's C1; the accounts are written to <dir>/qa-hold.json)")
    args = ap.parse_args(argv)
    global HELPER_BIN
    HELPER_BIN = args.helper_bin
    try:
        code, report = run(args)
    except Refused as e:
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
                write_part(folder, args.part, name, report)
    except OSError as e:          # a directory that could not be emptied may not take the file either
        where = f"the evidence could not be written to {out} ({e})"
        code = 1
    passed = sum(1 for c in report.checks if c["ok"])
    print(f"{'OK' if code == 0 else 'FAILED'}: {passed} of {len(report.checks)} checks passed; {where}", flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())

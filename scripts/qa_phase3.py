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
  completion): one is the ``accounts`` part's trap (a member's seat or machine that names it must make the server dial
  nothing), another serves the real CITAR helpers (``python -m citar.worker``) a part starts for a helper machine;
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
refusal; the Servers page's test given up mid-completion, its request draining until the helper's late answer while
the next request waits; 3-08: a lobby game's lease made when it is registered and again when the server, restarted,
brings it back, a benchmark job waiting for it, and a one-job benchmark started beside a running test, which it
cancels and waits to drain, ``drain_s`` in its result), and later ``ledger``, ``chat``, ``hybrid``, ``minimap``,
``machines``; ``all`` runs every part written so far. It needs the
extension (``cargo xtask develop``) and the ``worker`` extra (websockets), which ``dev`` includes.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import secrets
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
#: Parts a later package writes (P3.9.4): asked for now, the harness says so and refuses.
LATER = {"ledger": "3-10", "chat": "3-11 to 3-13", "hybrid": "3-14 to 3-16", "minimap": "3-17", "machines": "3-18"}


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
    """The checks and their outcomes, printed as they come and kept for ``qa-<part>.json``."""

    def __init__(self, part: str):
        self.part = part
        self.started = time.perf_counter()
        self.checks: list[dict] = []
        self.data: dict = {}

    def check(self, name: str, ok: bool, detail: str = "", **numbers) -> bool:
        """Record one check; returns whether it passed."""
        t = time.perf_counter() - self.started
        print(f"[{t:7.1f} s] [{'ok' if ok else 'FAIL'}] {name}{': ' + detail if detail else ''}", flush=True)
        self.checks.append({"check": name, "ok": bool(ok), "at_s": round(t, 2), "detail": detail[:500],
                            **({"numbers": numbers} if numbers else {})})
        return bool(ok)

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
    ``MODEL``, loaded. So a real CITAR helper pointed at it exercises the hub, the hello, the requests and
    ``Done.duration`` without a GPU."""

    MODEL = "qa-fake-model"

    def __init__(self, *, delay: float = 0.05, max_trades: int = 1):
        outer = self
        self.connections: list[tuple[float, str]] = []
        self.requests: list[dict] = []
        self.delay = delay
        #: Slower completions for some prompts: {text in the last user message: seconds} (``delay_for``).
        self.slow: dict = {}
        self.max_trades = max_trades
        self.trades_opened = 0
        self._lock = threading.Lock()

        class Handler(BaseHTTPRequestHandler):
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
                        {"id": outer.MODEL, "object": "model", "state": "loaded", "max_context_length": 32768}]})
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
                delay = outer.delay_for(body)
                if delay:
                    time.sleep(delay)
                reply, calls = outer.scripted_reply(body)
                entry.update(tools=len(body.get("tools") or []), calls=[c["function"]["name"] for c in calls],
                             delay=delay, seconds=round(time.time() - t0, 3))
                outer.requests.append(entry)
                self._send(200, reply)

            def log_message(self, *args):
                pass

        class Server(ThreadingHTTPServer):
            daemon_threads = True

            def get_request(self):
                conn, addr = super().get_request()
                outer.connections.append((time.time(), f"{addr[0]}:{addr[1]}"))
                return conn, addr

        self.httpd = Server(("127.0.0.1", 0), Handler)
        self.port = self.httpd.server_address[1]
        self.url = f"http://127.0.0.1:{self.port}/v1"
        threading.Thread(target=self.httpd.serve_forever, daemon=True, name="qa-fake-endpoint").start()

    # ------------------------------------------------------------------ the policy
    def completions(self) -> list:
        """The chat completions it served."""
        return [r for r in self.requests if r.get("method") == "POST"]

    def delay_for(self, body: dict) -> float:
        """How long a completion takes: ``slow``'s seconds for a prompt whose last user message holds one of its
        texts, else ``delay``."""
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

        if "respond_negotiation" in tools and "end_turn" not in tools:
            found = re.search(r"negotiation #(\d+)", prompt)
            if found and steps == 0:
                gives = prompt.split('"you_receive"')[0].count('"type"') if '"you_receive"' in prompt else 0
                gets = prompt.count('"type"') - gives
                action = "reply" if '"current_proposal": null' in prompt else ("accept" if gets >= gives else "reject")
                call("respond_negotiation", {"negotiation_id": int(found.group(1)), "action": action,
                                             "message": f"(qa endpoint) {action}"})
        elif steps == 0:
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
        if self.log.exists():
            self.log.replace(self.state / f"server-{len(list(self.state.glob('server-*.log'))) + 1}.log")
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

    def __init__(self, report: Report, server: Server, accounts: dict, endpoint: FakeEndpoint, *, keep: bool = False):
        self.report, self.server, self.accounts, self.endpoint = report, server, accounts, endpoint
        self.tag = secrets.token_hex(3)
        #: ``--keep``: the server stays up for the browser checklist, and so does what a part leaves for it to show
        self.keep = keep

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


class Helper:
    """A real CITAR helper (``python -m citar.worker``) as a child, connected to the instance with a worker token and
    serving from a fake endpoint; its log in the harness's directory."""

    def __init__(self, h: "Harness", token: str, endpoint: FakeEndpoint, name: str):
        state = h.server.state
        n = len(list(state.glob("helper-*.log"))) + 1
        self.log = state / f"helper-{n}.log"
        empty = state / f"helper-{n}.json"
        empty.write_text("{}", encoding="utf-8")      # the helper reads this, never a saved helper configuration
        out = open(self.log, "w", encoding="utf-8")
        env = {k: v for k, v in os.environ.items() if not k.startswith("CITAR_WORKER_")}
        self.proc = subprocess.Popen(
            [sys.executable, "-m", "citar.worker", "--server", h.server.base, f"--token={token}",
             "--base-url", endpoint.url, "--provider", "openai_compatible", "--name", name, "--no-hardware",
             "--config", str(empty)],
            cwd=str(ROOT), stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
            env={**env, "PYTHONUNBUFFERED": "1"})
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
            helper = Helper(h, token(), scripted, f"qa-helper-{len(helpers) + 1}")
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
        refused = Helper(h, token(), scripted, "qa-helper-disabled")
        helpers.append(refused)
        r.check("disabling the machine drops its helper within a second, and a helper's new hello is refused",
                second is not None and disabled.status_code == 200 and gone is not None and gone <= 1.0
                and second.exited(15) and refused.exited(30) and "rejected" in refused.text(),
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
        grant = alice.post(f"/api/pool/groups/{gid}/grants", json={"subject_type": "everyone", "purposes": ["game"]})
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
                and his_test.status_code == 200,
                json.dumps({"made": [group.status_code, put.status_code, grant.status_code], "his": {
                    f: shared.get(f) for f in ("group_id", "can_manage")}, "refused": refused,
                    "test": his_test.status_code}))
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
    registration, a restart's restored game and a benchmark's drain; 3-09 adds its own."""
    _queue_leases_and_drain(h)
    _queue_restored_game_and_benchmark_drain(h)


def _queue_leases_and_drain(h: Harness) -> None:
    """3-07's checks, on a member's helper machine with one slot (a real helper behind the scripted endpoint, 0.4 s a
    completion): two lobby games with a model seat each play through the prompt scheduler, their requests never two at
    once and nothing refused as busy, both games' leases on the machine; then the Servers page's test, given up after
    2 s while the helper takes 8 s over it, is cancelled: the helper is sent ``CANCEL`` and finishes it anyway, the
    slot drains until its late answer, and a game's model seat resumed meanwhile waits for it (its queue time in its
    turn's metrics), with the late frame counted by the scheduler and no drain timed out. While that test waits, a
    second of the same machine from her other tab is refused at once (one test of a machine per person)."""
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
        busy_lines = [line for line in (helper.text() if helper else "").splitlines() if "(busy)" in line]
        leases = [lease for lease in status.get("leases") or [] if lease.get("kind") == "game"]
        r.check("the helper refused nothing as busy, the scheduler saw no busy refusal and at most one request in "
                "flight, and it lists both games' leases on the machine",
                not busy_lines and counters.get("busy_refusals", 0) == 0 and status.get("max_in_flight") == 1
                and len(leases) == 2,
                json.dumps({"busy_lines": busy_lines[:3], "counters": counters,
                            "max_in_flight": status.get("max_in_flight"), "leases": len(leases)})[:500])

        # ---- the Servers page's test, given up while the helper generates it: its slot drains
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
        draining = machine_status(hid)
        r.check("its slot drains: the scheduler still counts the request the helper is finishing",
                draining.get("draining") == 1 and draining.get("in_flight") == 1,
                json.dumps({k: draining.get(k) for k in ("in_flight", "draining", "waiting")}))
        if games:
            alice.post(f"/api/games/{games[0]}/control", json={"paused": False})    # its model seat wants the slot
        finished = wait_until(lambda: any(q.get("delay") == 8.0 for q in scripted.completions()[before:])
                              and len(scripted.completions()) > before + 1, 60, 0.25)
        for g in games:
            alice.post(f"/api/games/{g}/control", json={"paused": True})
        time.sleep(1.5)
        after = scripted.completions()[before:]
        late = next((q for q in after if q.get("delay") == 8.0), None)
        others = [q for q in after if q is not late]
        ended = late["t"] + late["seconds"] if late else None
        first_other = min((q["t"] for q in others), default=None)
        waited = round((queued(games[0]) if games else 0.0) - queued_before, 2)
        r.check("the resumed game's model seat waited for the drain: its next completion reached the endpoint only "
                "after the helper finished the cancelled test, and the wait is in its turn's queue time",
                bool(finished) and late is not None and first_other is not None and first_other >= ended - 0.05
                and waited > 2.0,
                f"test finished {round(ended - t0, 2) if ended else None} s after it began; the game's next "
                f"completion at {round(first_other - t0, 2) if first_other else None} s; its queue time +{waited} s",
                queue_s=waited)
        final = machine_status(hid)
        counters = final.get("counters") or {}
        text = helper.text() if helper else ""
        cancel_seen = "server cancelled" in text
        busy_lines = [line for line in text.splitlines() if "(busy)" in line]
        r.check("the helper was sent CANCEL, its late answer reached the scheduler (a late frame, no drain timed out) "
                "and nothing was refused as busy",
                cancel_seen and counters.get("late_frames", 0) >= 1 and counters.get("drain_timeouts", 0) == 0
                and counters.get("busy_refusals", 0) == 0 and not busy_lines,
                json.dumps({"cancel_seen": cancel_seen, "counters": counters}))
        r.data["queue"].update({"completions": len(scripted.completions()), "overlaps": overlaps,
                                "test": body, "test_gave_up_s": gave_up, "duplicate_s": dup_s, "drain_wait_s": waited,
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
    test (the helper keeps generating it), and the job's first request waits for that request to drain, so no pooled
    request overlaps a benchmark step; the wait is the job's ``drain_s`` in its result, not ``contaminated``."""
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
                "benchmark took the machine (its request left draining)",
                bool(running) and closed.status_code == 200 and body.get("cancelled") is True
                and body.get("code") == "benchmark",
                f"running {bool(running)}; close HTTP {closed.status_code}; test HTTP "
                f"{getattr(tested, 'status_code', None)} after {round(result.get('t1', 0) - result.get('t0', 0), 2)} s: "
                f"{json.dumps(body)[:240]}")
        done = wait_until(lambda: _runs_job(alice, run_id).get("status") in ("done", "failed", "cancelled"), 180, 1.0)
        job = _runs_job(alice, run_id)
        res = job.get("result") or {}
        r.check("the benchmark job ran to its end with drain_s in its result: its first request waited for the "
                "cancelled test to drain, and its timing is not contaminated",
                bool(done) and job.get("status") == "done" and (res.get("drain_s") or 0) > 1.0
                and res.get("contaminated") is False,
                json.dumps({"status": job.get("status"), "error": job.get("error"), "drain_s": res.get("drain_s"),
                            "contaminated": res.get("contaminated"),
                            "started_after_close_s": round((job.get("started") or 0) - t_close, 2)})[:400],
                drain_s=res.get("drain_s"))
        after = scripted.completions()[before:]
        late = next((q for q in after if q.get("delay") == 10.0), None)
        bench = [q for q in after if q is not late]
        ended = late["t"] + late["seconds"] if late else None
        first = min((q["t"] for q in bench), default=None)
        r.check("no pooled request overlapped a benchmark step: the benchmark's first completion reached the endpoint "
                "only after the helper finished the cancelled test",
                late is not None and first is not None and first >= ended - 0.05 and len(bench) >= 2,
                f"test ended {round(ended - result['t0'], 2) if ended else None} s after it began; the benchmark's "
                f"first completion at {round(first - result['t0'], 2) if first else None} s; {len(bench)} benchmark "
                f"completions")
        final = _machine_status(admin, hid)
        counters = final.get("counters") or {}
        text = helper.text() if helper else ""
        busy_lines = [line for line in text.splitlines() if "(busy)" in line]
        r.check("the helper was sent CANCEL, its late answer freed the slot (a late frame, no drain timed out), "
                "nothing was refused as busy, and no lease is left on the machine",
                "server cancelled" in text and counters.get("late_frames", 0) >= 1
                and counters.get("drain_timeouts", 0) == 0 and counters.get("busy_refusals", 0) == 0
                and not busy_lines and not final.get("leases"),
                json.dumps({"counters": counters, "leases": final.get("leases")})[:400])
        data["restore"].update({"restart_leases": restored_leases, "test": body, "job": {
            "status": job.get("status"), "drain_s": res.get("drain_s"), "contaminated": res.get("contaminated")},
            "benchmark_completions": len(bench), "counters": counters})
    finally:
        if gid:
            alice.delete(f"/api/games/{gid}")
        if run_id:
            alice.post(f"/api/benchmarks/runs/{run_id}/control", json={"action": "cancel"})
        if helper is not None:
            helper.stop()
        scripted.close()


#: The parts written so far, in the order ``all`` runs them.
PARTS: dict[str, Callable[[Harness], None]] = {"accounts": part_accounts, "queue": part_queue}


# ---------------------------------------------------------------------------- the run

def make_accounts(server: Server, report: Report, everyone: bool) -> dict:
    """The administrator, two members and a moderator (and a third member for ``all``), made with ``admin_cli
    create-user`` and signed in. Raises HarnessError when one cannot be made or sign in: no check means anything
    without them."""
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


def run(args) -> tuple:
    """Refuse (Refused, nothing touched), or claim, start, check and stop. Returns the exit code and the report,
    whose evidence ``main`` writes. Once the directory is claimed nothing raises out of here: a harness failure
    (``HarnessError``) or a part that raises is a failed check, and the checks made before it stand."""
    parts = list(PARTS) if args.part == "all" else [args.part]
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
        accounts = make_accounts(server, report, args.part == "all")
        harness = Harness(report, server, accounts, endpoint, keep=args.keep)
        for name in parts:
            report.note(f"part: {name}")
            try:
                PARTS[name](harness)
            except Exception:
                # the checks already made stand; the rest of this part never ran
                report.check(f"the {name} part ran to its end", False, _trace_tail())
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
    args = ap.parse_args(argv)
    try:
        code, report = run(args)
    except Refused as e:
        print(f"refused: {e}", flush=True)
        return 2
    summary = {"part": args.part, "ok": code == 0, "checks": report.checks, **report.data}
    out = Path(report.data["server"]["dir"]) / f"qa-{args.part}.json"
    try:
        out.write_text(json.dumps(summary, indent=2, default=str) + "\n", encoding="utf-8")
        where = f"evidence in {out}"
    except OSError as e:          # a directory that could not be emptied may not take the file either
        where = f"the evidence could not be written to {out} ({e})"
        code = 1
    passed = sum(1 for c in report.checks if c["ok"])
    print(f"{'OK' if code == 0 else 'FAILED'}: {passed} of {len(report.checks)} checks passed; {where}", flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())

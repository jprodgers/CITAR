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
helper machine with a real helper, the machine routes' gates and dropped helpers; 3-06 adds its own), and later ``queue``,
``ledger``, ``chat``, ``hybrid``, ``minimap``, ``machines``; ``all`` runs every part written so far. It needs the
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
LATER = {"queue": "3-07 to 3-09", "ledger": "3-10", "chat": "3-11 to 3-13", "hybrid": "3-14 to 3-16",
         "minimap": "3-17", "machines": "3-18"}


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
                if outer.delay:
                    time.sleep(outer.delay)
                reply, calls = outer.scripted_reply(body)
                entry.update(tools=len(body.get("tools") or []), calls=[c["function"]["name"] for c in calls],
                             seconds=round(time.time() - t0, 3))
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

    def tracebacks(self) -> int:
        """Tracebacks in the server's log."""
        return self.log.read_text(encoding="utf-8", errors="replace").count("Traceback") if self.log.exists() else 0


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

    def __getattr__(self, name):
        # get / post / put / delete go straight to the client
        return getattr(self.http, name)


class Harness:
    """What a part is given: the report, the server, the signed-in accounts, the fake endpoint, a tag that makes
    this run's names unique, and an anonymous client maker."""

    def __init__(self, report: Report, server: Server, accounts: dict, endpoint: FakeEndpoint):
        self.report, self.server, self.accounts, self.endpoint = report, server, accounts, endpoint
        self.tag = secrets.token_hex(3)

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
    """The ``accounts`` part: 3-03's sharing checks, 3-04's profiles and administration, then 3-05's machines. Each
    runs whatever the others found."""
    _accounts_sharing(h)
    _accounts_profiles(h)
    _accounts_machines(h)


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
            [sys.executable, "-m", "citar.worker", "--server", h.server.base, "--token", token,
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


def _accounts_machines(h: Harness) -> None:
    """3-05's checks (design/phase3-features.md P3.2.8): the registry file imported once, under its ids, when the first
    administrator was made, and renamed; a member's machine CITAR would dial refused unless it is the dry run; a
    member's dry-run machine and helper machine (a real helper behind the scripted endpoint) playing her games; the
    host-touching routes 404 on a helper machine with nothing dialled; another member's machine 404; the model probe
    an administrator's; a revoked token, a disabled machine and a deleted one dropping the helper within a second."""
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
        deleted = alice.delete(f"/api/pool/servers/{hid}")
        gone = _until_offline(alice, hid) if deleted.status_code == 200 else None
        r.check("deleting the machine drops its helper within a second and leaves no connection",
                third is not None and deleted.status_code == 200 and third.exited(15)
                and alice.get(f"/api/pool/servers/{hid}").status_code == 404, f"HTTP {deleted.status_code}")
    finally:
        for helper in helpers:
            helper.stop()
        scripted.close()


#: The parts written so far, in the order ``all`` runs them.
PARTS: dict[str, Callable[[Harness], None]] = {"accounts": part_accounts}


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
        harness = Harness(report, server, accounts, endpoint)
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

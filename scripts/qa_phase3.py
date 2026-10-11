"""Phase 3's QA script: a dev server of its own, accounts, and each feature's checks over HTTP and the game socket,
with the evidence written down (design/phase3-features.md P3.9.4).

    python scripts/qa_phase3.py <part>|all [--port N] [--dir D] [--keep]

The harness:

- **refuses** (exit 2, touching nothing) the dev server's own port (8799), a port something already listens on, a
  directory that holds anything without the harness's marker (``.qa-phase3``), and directories that are never a
  harness's: ``C:/dev/citar-play`` (the owner's dev server), CITAR's own per-user data directory and the source
  checkout. A directory it may use is new, empty or one an earlier run left (marked); it empties it first;
- starts ``scripts/dev_server.py --server --registration invite`` as a child on that directory and a free port (or
  ``--port``), everything of the instance under the directory (``dev_server.instance_env``);
- makes an administrator, two members and a moderator (a third member for ``all``) with ``admin_cli create-user``,
  passwords on standard input, and signs them in through ``/api/auth/login`` (a cookie and the CSRF token each);
- serves a fake OpenAI-compatible endpoint on 127.0.0.1 that records every connection it accepts (the
  ``accounts`` part's trap: a member's seat that names it must make the server dial nothing). Package 3-05 teaches
  it the scripted policy that plays (P3.9.4);
- runs the part's checks, writes ``<dir>/qa-<part>.json`` (each check, its result, its numbers; the accounts and
  their throwaway password, for the browser checklist), stops the server unless ``--keep``, and exits 1 when a
  check failed, 0 when every one passed.

Parts: ``accounts`` (3-03: sharing of games and reports, ``/r/{slug}``, a moderator's unpublish, members' model
blocks, a member's report gathering only what they may see; 3-04 to 3-06 add theirs), and later ``queue``,
``ledger``, ``chat``, ``hybrid``, ``minimap``, ``machines``; ``all`` runs every part written so far. It needs the
extension (``cargo xtask develop``) and the ``worker`` extra (websockets), which ``dev`` includes.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import secrets
import shutil
import socket
import subprocess
import sys
import threading
import time
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
    """The harness will not run here: a port or a directory that is not its own. Exit 2, nothing touched."""


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
            raise Refused(f"could not empty {state} ({e}): a server an earlier run kept (--keep) may still be "
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
    """A fake OpenAI-compatible endpoint on 127.0.0.1, recording every connection it accepts and every request it
    serves (when, what). Today it answers ``/v1/models`` and a plain chat completion; package 3-05 makes it the
    scripted policy of P3.9.4 (valid tool calls from the request's tools, a delay per completion)."""

    MODEL = "qa-fake-model"

    def __init__(self):
        outer = self
        self.connections: list[tuple[float, str]] = []
        self.requests: list[dict] = []

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
                    return self._send(200, {"object": "list", "data": [{"id": outer.MODEL, "object": "model"}]})
                self._send(404, {"error": "not found"})

            def do_POST(self):
                size = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(size) if size else b""
                outer.requests.append({"t": time.time(), "method": "POST", "path": self.path, "bytes": len(body)})
                self._send(200, {"id": "qa-1", "object": "chat.completion", "created": int(time.time()),
                                 "model": outer.MODEL,
                                 "choices": [{"index": 0, "finish_reason": "stop",
                                              "message": {"role": "assistant", "content": "(qa fake endpoint)"}}],
                                 "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})

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
                    raise Refused(f"the server did not start (exit {self.proc.poll()}); see {self.log}")
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


#: The parts written so far, in the order ``all`` runs them.
PARTS: dict[str, Callable[[Harness], None]] = {"accounts": part_accounts}


# ---------------------------------------------------------------------------- the run

def make_accounts(server: Server, report: Report, everyone: bool) -> dict:
    """The administrator, two members and a moderator (and a third member for ``all``), made with ``admin_cli
    create-user`` and signed in. Raises Refused when one cannot be made or sign in: no check means anything
    without them."""
    password = "qa-" + secrets.token_hex(10)
    wanted = [("admin", "qa-admin", "admin"), ("alice", "qa-alice", "user"), ("bob", "qa-bob", "user"),
              ("mod", "qa-mod", "moderator")] + ([("carol", "qa-carol", "user")] if everyone else [])
    out = {}
    for key, handle, role in wanted:
        made = server.cli("create-user", "--handle", handle, "--email", f"{handle}@example.com", "--role", role,
                          "--password-stdin", stdin=password + "\n")
        if made.returncode != 0:
            raise Refused(f"admin_cli create-user {handle} failed: {made.stdout.strip()} {made.stderr.strip()[-400:]}")
        account = Account(server, handle, role, password)
        login = account.login()
        if login.status_code != 200:
            raise Refused(f"{handle} could not sign in: HTTP {login.status_code} {login.text[:200]}")
        out[key] = account
    report.check("the accounts were made with admin_cli create-user and signed in", True,
                 ", ".join(f"{a.handle} ({a.role})" for a in out.values()))
    report.data["accounts"] = [{"handle": a.handle, "role": a.role} for a in out.values()]
    # throwaway accounts on a throwaway instance: kept for the browser checklist (--keep)
    report.data["password"] = password
    return out


def run(args) -> tuple:
    """Refuse or claim, start, check, write, stop. Returns the exit code and the report."""
    parts = list(PARTS) if args.part == "all" else [args.part]
    state = Path(args.dir).expanduser()
    report = Report(args.part)
    if args.part in LATER:
        raise Refused(f"the '{args.part}' part is not written yet (packages {LATER[args.part]})")
    check_dir(state)
    port = args.port or free_port()
    check_port(port)
    claim(state)
    state = state.resolve()
    server = Server(state, port)
    endpoint = FakeEndpoint()
    report.data["server"] = {"base": server.base, "public": server.public, "dir": str(state), "log": str(server.log),
                             "fake_endpoint": endpoint.url}
    t0 = time.perf_counter()
    try:
        server.start()
        report.note(f"the server is up at {server.public} (dir {state})")
        report.data["server"]["start_s"] = round(time.perf_counter() - t0, 1)
        accounts = make_accounts(server, report, args.part == "all")
        harness = Harness(report, server, accounts, endpoint)
        for name in parts:
            report.note(f"part: {name}")
            PARTS[name](harness)
        tracebacks = server.tracebacks()
        report.check("no traceback in the server's log", tracebacks == 0, f"{tracebacks} in {server.log}")
    finally:
        endpoint.close()
        if args.keep and server.proc is not None and server.proc.poll() is None:
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
    out.write_text(json.dumps(summary, indent=2, default=str) + "\n", encoding="utf-8")
    passed = sum(1 for c in report.checks if c["ok"])
    print(f"{'OK' if code == 0 else 'FAILED'}: {passed} of {len(report.checks)} checks passed; evidence in {out}",
          flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())

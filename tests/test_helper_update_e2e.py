"""The real CITAR helper updating itself (design/phase4-helper.md P4.3.8, P4.5.9; package 4-13's gates 1, 3 and 4).

The binaries are the update tests' two helpers ``cargo xtask develop`` builds beside the extension (``CITAR_EXT_DIR``):
``citar-helper-update-1`` calls itself ``0.1.6-test.1`` and ``-2`` ``0.1.6-test.2`` (and is another jobs build), both
with the ``test-update-key`` feature, so each trusts the update key it is given (``--update-key``) and, like every
development build, dials loopback only. Without them the module skips, except on CI. The older one is copied into an
install folder of each test's own (never the build's output) and run as a console run (no ``--service``) against the
real hub in this process, whose relay holds a manifest file signed by a throwaway key made at run time; the asset is
served on loopback behind a redirect. Nothing reaches beyond this machine.

- **The update** (gate 1): the helper downloads the newer binary through the redirect, checks it, self-tests it,
  hands off (``restarting``, its socket closed first) and the new binary reconnects reporting ``0.1.6-test.2`` and
  ``applied``; the install folder holds the new binary and the old one as ``<name>.old``; the hub never replaced a
  connection (the old socket closed before the new one dialled).
- **By hand** (``notify``): a helper whose updates are set to notify reports the update available, downloads nothing
  and keeps the relayed manifest; ``citar-helper update check`` says it is newer, ``update apply`` installs it (the old
  binary kept), and the helper started again is the new one and reports ``applied``. ``update rollback`` then puts the
  old binary back and holds the release back: started again with its updates automatic, the old binary refuses it at
  its welcome, downloads nothing and stays (4-13's fix round: before, the next welcome installed it again).
- **A download that fails is tried again** (4-13's fix round): the first request for the binary answered 503, the
  helper says ``failed`` with when it tries again (cut to a second by ``CITAR_HELPER_TEST_RETRY_S``), tries again, and
  the update goes through to ``applied``.
- **The console rollback** (gate 3): a newer binary made to exit at its start (the hook
  ``CITAR_HELPER_TEST_EXIT_AT_START``) is swapped back by the supervisor; the old one serves again and reports
  ``failed``, and is not offered the same binary again.
- **No stale job host** (gate 4): the older helper lends its CPU and plays the lab's games until the update; the hand-off
  waits for its games, and every result it relayed came before its ``restarting``; with a long game playing when the
  update is verified, it takes work as usual for its wait (cut to a second by ``CITAR_HELPER_TEST_IDLE_WAIT_S``), then
  no new work until the game ends, whose result goes before the hand-off. The new binary, told to start its first job host from the binary it
  replaced (``CITAR_HELPER_TEST_STALE_HOST``: a host left from before the swap), refuses it for its other jobs build
  and starts its own, and no result reaches the hub from the old build after the swap.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import hashlib
import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from citar import db, engine_api, items, lab, ledger, servers, settings
from citar.auth import accounts, tokens
from citar.db.models import Base, CpuJobRun, Server, ServerGroup, User, WorkerToken
from citar.jobs import dispatcher as D, remote
from citar.pool import scheduler as S
from citar.server import helper_updates as U
from citar.server import workers as W
from tests import has_test_ops
from tests import helperfake as H

_TMP = Path(tempfile.mkdtemp(prefix="citar_helper_update_e2e_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
CI = bool(os.environ.get("CI"))
SLACK = 3.0 if CI else 1.0
#: The throwaway update key's seed (the extension's test operations make it at run time).
SEED = 71
OLD, NEW = "0.1.6-test.1", "0.1.6-test.2"
_SAVED: dict = {}


def setUpModule():
    H.require_update_binaries("the helper's update tests")
    if not has_test_ops():
        raise unittest.SkipTest("needs the extension's test operations (cargo xtask develop)")
    _SAVED.update({k: os.environ.get(k) for k in ("CITAR_DATA_DIR", "CITAR_DB_URL", "CITAR_HELPER_UPDATE_KEY")})
    os.environ["CITAR_DATA_DIR"] = str(_TMP)
    os.environ["CITAR_DB_URL"] = _URL
    os.environ["CITAR_HELPER_UPDATE_KEY"] = H.update_public_key(SEED)
    settings.reset()
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()
    S.reset()


def tearDownModule():
    U.relay().clear()
    db.dispose()
    for k, v in _SAVED.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v
    settings.reset()
    servers.invalidate()
    shutil.rmtree(_TMP, ignore_errors=True)


def sha256(path) -> str:
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def exe_name() -> str:
    return "citar-helper.exe" if os.name == "nt" else "citar-helper"


def described(binary: str) -> dict:
    """What ``binary --version`` says of it: version, build, jobs build, target."""
    import re
    out = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=60).stdout
    m = re.match(r"citar-helper (\S+) \(build ([0-9a-f]{12}), jobs build ([0-9a-f]{12}), protocol \d+, "
                 r"target ([^,]+),", out)
    assert m, out
    return {"version": m.group(1), "build": m.group(2), "jobs_build": m.group(3), "target": m.group(4)}


class AssetServer:
    """The release's assets on loopback: ``/download/<name>`` redirects to ``/cdn/<name>``, which serves the file, as
    GitHub's release downloads redirect to another host. Every path asked is recorded."""

    def __init__(self, files: dict):
        self.files = dict(files)
        self.asked: list = []
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                outer.asked.append(self.path)
                if self.path.startswith("/download/"):
                    self.send_response(302)
                    self.send_header("Location", "/cdn/" + self.path.rsplit("/", 1)[1])
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                body = outer.files.get(self.path.rsplit("/", 1)[1]) if self.path.startswith("/cdn/") else None
                if body is None:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, name="asset-server", daemon=True)
        self.thread.start()

    def url(self, name: str) -> str:
        return f"http://127.0.0.1:{self.port}/download/{name}"

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


class _Update(unittest.TestCase):
    """The pieces: the hub, the two binaries, the throwaway key in a file, a manifest of the newer one served."""

    @classmethod
    def setUpClass(cls):
        cls.old_bin, cls.new_bin = H.require_update_binaries("the helper's update tests")
        cls.old_info, cls.new_info = described(cls.old_bin), described(cls.new_bin)
        assert (cls.old_info["version"], cls.new_info["version"]) == (OLD, NEW), (cls.old_info, cls.new_info)
        with db.session() as s:
            owner = s.query(User).filter(User.handle == "uma").one_or_none()
            if owner is None:
                owner = accounts.create_user(s, handle="uma", email="uma@x.cc", password="a long enough phrase",
                                             role="user", status="active", email_verified=True)
            cls.owner = owner.id
        cls.server = H.HubServer()

    @classmethod
    def tearDownClass(cls):
        cls.server.stop()
        S.reset()

    def setUp(self):
        U.relay().clear()
        self.dir = Path(tempfile.mkdtemp(prefix="update-", dir=_TMP))
        self.keyfile = self.dir / "update-key.pub"
        self.keyfile.write_text(H.update_public_key(SEED) + "\n", encoding="utf-8")
        name = "citar-helper-next"
        self.assets = AssetServer({name: Path(self.new_bin).read_bytes()})
        self.addCleanup(self.assets.close)
        manifest = json.dumps({
            "v": 1, "version": NEW, "protocol": 2, "build": self.new_info["build"],
            "released_at": "2026-11-01T00:00:00Z",
            "assets": {self.new_info["target"]: {"name": name, "url": self.assets.url(name),
                                                  "sha256": sha256(self.new_bin),
                                                  "size": Path(self.new_bin).stat().st_size}},
        }, indent=2) + "\n"
        # the manifest file the server reads, its bytes as published, signed by the throwaway key
        self.manifest = self.dir / "citar-helper-manifest.json"
        self.manifest.write_bytes(manifest.encode("utf-8"))
        (self.dir / "citar-helper-manifest.json.sig").write_text(H.sign_manifest_text(manifest, SEED) + "\n",
                                                                  encoding="utf-8")
        U.relay().source = U.file_source(str(self.manifest))
        self.assertIsNotNone(U.relay().check(), "the server relays the manifest file")
        self.install = self.dir / "install"
        self.install.mkdir()
        self.exe = self.install / exe_name()
        shutil.copy2(self.old_bin, self.exe)
        self.home = self.dir / "home"
        self.home.mkdir()

    def tearDown(self):
        U.relay().source = None
        U.relay().clear()

    def machine(self, name: str, **row) -> tuple:
        with db.session() as s:
            server = Server(owner_id=row.pop("owner_id", self.owner), name=name, kind="owned", provider="lmstudio",
                            reach="worker", max_concurrent=1, config={"models": [{"key": "model-a"}]}, **row)
            s.add(server)
            s.flush()
            raw, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=server.id, token_hash=hashed, prefix=tokens.prefix(raw), label="update e2e"))
            sid = server.id
        servers.invalidate()
        return sid, raw

    def start(self, token: str, *, env: dict = None, extra: tuple = ()) -> H.RealHelper:
        """The installed (older) helper as a console run, its updates automatic, trusting the throwaway key."""
        helper = H.RealHelper(self.server.base, token, "http://127.0.0.1:9/v1", name="update e2e", home=self.home,
                              binary=str(self.exe), env=env or {},
                              extra=("--no-llm", "--updates", "auto", "--update-key", str(self.keyfile), *extra))
        self.addCleanup(self.end, helper)
        return helper

    def end(self, helper: H.RealHelper) -> None:
        """The supervisor and the helper it runs, ended."""
        if helper.proc.poll() is None:
            H.kill_tree(helper.proc.pid)
            try:
                helper.proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                helper.proc.kill()

    def states(self, sid: str) -> list:
        return [(s["state"], s["version"]) for s in W.hub().update_log.get(sid, [])]

    def wait_state(self, sid: str, state: str, limit: float, helper: H.RealHelper) -> None:
        ok = H.until(lambda: (W.hub().update_states.get(sid) or {}).get("state") == state, limit * SLACK, 0.05)
        self.assertTrue(ok, f"no {state}: {self.states(sid)}\n{helper.text()[-4000:]}")


class Update(_Update):
    def test_an_update_downloads_checks_hands_off_and_reconnects(self):
        """Gate 1."""
        sid, token = self.machine("updating")
        replaced = W.hub().replaced
        helper = self.start(token)
        self.wait_state(sid, "applied", 120, helper)
        conn = W.hub().get(sid)
        self.assertIsNotNone(conn)
        self.assertEqual(conn.helper_version, NEW, "the new binary reconnected")
        states = [s for s, _ in self.states(sid)]
        for a, b in (("downloading", "verified"), ("verified", "restarting"), ("restarting", "applied")):
            self.assertLess(states.index(a), states.index(b), states)
        self.assertEqual(W.hub().replaced, replaced, "the old socket closed before the new binary dialled")
        self.assertEqual(sha256(self.exe), sha256(self.new_bin), "the new binary is in place")
        self.assertEqual(sha256(str(self.exe) + ".old"), sha256(self.old_bin), "the old one is kept as .old")
        self.assertIn("/download/citar-helper-next", self.assets.asked)
        self.assertIn("/cdn/citar-helper-next", self.assets.asked, "the redirect was followed")
        upd = self.home / "helper" / "update"
        self.assertTrue((upd / "healthy.json").is_file() and not (upd / "pending.json").exists())
        self.assertIn("supervises it", helper.text())
        # the Servers page's view: the latest signed helper, not newer than the machine's now
        view = conn.client()["update"]
        self.assertEqual((view["latest"]["version"], view["available"], view["status"]["state"]),
                         (NEW, False, "applied"))
        # a revoke ends the new binary (78) and its supervisor with it
        W.hub().drop(sid, "its worker token was revoked", code="revoked")
        self.assertEqual(helper.exited(30), 78, helper.text()[-3000:])
        print(f"\n[update e2e] gate 1: states {self.states(sid)}; asked {self.assets.asked}", file=sys.stderr)

    def test_notify_reports_it_and_update_apply_installs_it_by_hand(self):
        sid, token = self.machine("by hand")
        helper = self.start(token, extra=("--updates", "notify"))
        self.wait_state(sid, "available", 60, helper)
        self.assertEqual(self.assets.asked, [], "notify downloads nothing")
        kept = self.home / "helper" / "update" / "manifest.json"
        self.assertTrue(H.until(lambda: kept.is_file(), 10), "the relayed manifest is kept in the home")
        W.hub().drop(sid, "its worker token was revoked", code="revoked")
        self.assertEqual(helper.exited(30), 78, helper.text()[-3000:])
        env = {**{k: v for k, v in os.environ.items() if not k.startswith("CITAR_HELPER_")},
               "CITAR_DATA_DIR": str(self.home)}
        common = ["--home", str(self.home), "--update-key", str(self.keyfile)]
        check = subprocess.run([str(self.exe), "update", "check", *common], capture_output=True, text=True,
                               timeout=60, env=env)
        self.assertEqual(check.returncode, 0, check.stderr)
        self.assertIn(f"CITAR helper {NEW}", check.stdout)
        self.assertIn("newer than this helper", check.stdout)
        applied = subprocess.run([str(self.exe), "update", "apply", *common], capture_output=True, text=True,
                                 timeout=300, env=env)
        self.assertEqual(applied.returncode, 0, applied.stdout + applied.stderr)
        self.assertIn(f"installed CITAR helper {NEW}", applied.stdout)
        self.assertEqual(sha256(self.exe), sha256(self.new_bin))
        self.assertEqual(sha256(str(self.exe) + ".old"), sha256(self.old_bin))
        self.assertIn("/cdn/citar-helper-next", self.assets.asked)
        # started again by its owner: the new binary, which reports the update applied after its welcome
        helper = self.start(token, extra=("--updates", "notify"))
        ok = H.until(lambda: W.hub().get(sid) is not None and W.hub().get(sid).helper_version == NEW
                     and (W.hub().update_states.get(sid) or {}).get("state") == "applied", 60 * SLACK, 0.05)
        self.assertTrue(ok, f"{self.states(sid)}\n{helper.text()[-3000:]}")
        W.hub().drop(sid, "its worker token was revoked", code="revoked")
        self.assertEqual(helper.exited(30), 78, helper.text()[-3000:])
        # rolled back by hand: the old binary is back and the release held back, so an automatic helper does not
        # install it again at its next welcome
        back = subprocess.run([str(self.exe), "update", "rollback", "--home", str(self.home)], capture_output=True,
                              text=True, timeout=60, env=env)
        self.assertEqual(back.returncode, 0, back.stdout + back.stderr)
        self.assertIn(f"does not install CITAR helper {NEW} again", back.stdout)
        self.assertEqual(sha256(self.exe), sha256(self.old_bin), "the old binary is back")
        asked = len(self.assets.asked)
        helper = self.start(token)                      # updates automatic
        held = H.until(lambda: W.hub().get(sid) is not None and W.hub().get(sid).helper_version == OLD
                       and (W.hub().update_states.get(sid) or {}).get("state") == "refused", 60 * SLACK, 0.05)
        self.assertTrue(held, f"{self.states(sid)}\n{helper.text()[-3000:]}")
        refusal = W.hub().update_states[sid]
        self.assertEqual(refusal["version"], NEW)
        self.assertIn("was rolled back on this machine", refusal["message"])
        self.assertEqual(len(self.assets.asked), asked, "nothing downloaded again")
        self.assertEqual(sha256(self.exe), sha256(self.old_bin), "the old binary stays")
        W.hub().drop(sid, "its worker token was revoked", code="revoked")
        self.assertEqual(helper.exited(30), 78, helper.text()[-3000:])
        print(f"\n[update e2e] by hand: {check.stdout.strip().splitlines()[-1]}; states {self.states(sid)}",
              file=sys.stderr)

    def test_a_download_that_fails_is_tried_again(self):
        """The first request for the binary answered 503: failed, with when it is tried again; then tried again (a
        second later, the test hook) and applied."""
        failures = {"left": 1}
        handler_class = self.assets.httpd.RequestHandlerClass
        real = handler_class.do_GET
        assets = self.assets

        def flaky(handler):
            if handler.path.startswith("/cdn/") and failures["left"] > 0:
                failures["left"] -= 1
                assets.asked.append(handler.path + " (503)")
                handler.send_response(503)
                handler.send_header("Content-Length", "0")
                handler.end_headers()
                return
            real(handler)

        patcher = mock.patch.object(handler_class, "do_GET", flaky)
        patcher.start()
        self.addCleanup(patcher.stop)
        sid, token = self.machine("flaky download")
        helper = self.start(token, env={"CITAR_HELPER_TEST_RETRY_S": "1"})
        self.wait_state(sid, "applied", 120, helper)
        log = list(W.hub().update_log.get(sid, []))
        states = [s["state"] for s in log]
        first_failed = states.index("failed")
        self.assertLess(states.index("downloading"), first_failed, states)
        self.assertIn("tried again", log[first_failed]["message"])
        self.assertIn("503", log[first_failed]["message"])
        self.assertIn("downloading", states[first_failed:], states)
        for a, b in (("verified", "restarting"), ("restarting", "applied")):
            self.assertLess(states.index(a), states.index(b), states)
        self.assertEqual([p for p in self.assets.asked if p.startswith("/cdn/")],
                         ["/cdn/citar-helper-next (503)", "/cdn/citar-helper-next"])
        self.assertEqual(sha256(self.exe), sha256(self.new_bin), "the new binary is in place")
        W.hub().drop(sid, "its worker token was revoked", code="revoked")
        self.assertEqual(helper.exited(30), 78, helper.text()[-3000:])
        print(f"\n[update e2e] retry: states {self.states(sid)}; asked {self.assets.asked}", file=sys.stderr)

    def test_a_new_binary_that_exits_before_it_is_healthy_is_swapped_back(self):
        """Gate 3, a console run: the supervisor puts the old binary back, which serves and says failed."""
        sid, token = self.machine("rolled back")
        helper = self.start(token, env={"CITAR_HELPER_TEST_EXIT_AT_START": NEW})
        ok = H.until(lambda: ("failed", NEW) in self.states(sid) and W.hub().get(sid) is not None
                     and W.hub().get(sid).helper_version == OLD, 120 * SLACK, 0.05)
        self.assertTrue(ok, f"{self.states(sid)}\n{helper.text()[-4000:]}")
        self.assertEqual(sha256(self.exe), sha256(self.old_bin), "the old binary is back")
        failed = json.loads((self.home / "helper" / "update" / "failed.json").read_text(encoding="utf-8"))
        self.assertEqual(failed["version"], NEW)
        self.assertTrue(failed["reported"])
        self.assertIn("before it was healthy", failed["message"])
        self.assertIn("the update failed", helper.text())
        # the same binary is not tried again (the welcome relayed it once more)
        self.assertTrue(H.until(lambda: any(s == "refused" and v == NEW for s, v in self.states(sid)), 10), self.states(sid))
        self.assertEqual(sha256(self.exe), sha256(self.old_bin))
        W.hub().drop(sid, "its worker token was revoked", code="revoked")
        self.assertEqual(helper.exited(30), 78, helper.text()[-3000:])
        print(f"\n[update e2e] gate 3 (console): states {self.states(sid)}; failed.json {failed['message']}",
              file=sys.stderr)


#: What the machine's owner lends in its helper's own settings, and what the site asks.
CEILING = {"enabled": True, "threads": 1, "cpu_percent": 25, "memory_mb": 512, "work": ["lab_game"]}
SITE = {"enabled": True, "threads": 1, "cpu_percent": 25, "memory_mb": 512, "work": ["lab_game"],
        "yield_to_llm": True}


class NoStaleHost(_Update):
    """Gate 4, with the lab's games: the dispatcher in this process offers them to the older helper."""

    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        from citar import pool
        labdir = _TMP / "saves" / "lab"
        cls.patches = [mock.patch.object(lab, name, labdir / sub if sub else labdir)
                       for name, sub in (("LAB", ""), ("QUEUE", "queue"), ("DONE", "done"), ("RESULTS", "results"),
                                         ("REFERENCE", "reference"))]
        for p in cls.patches:
            p.start()
        lab._dirs()
        cls.led = ledger.reset(background=False)
        remote.forget_owner_tiers()
        with db.session() as s:
            alice = accounts.create_user(s, handle="ali", email="ali@x.cc", password="a long enough phrase",
                                         role="user", status="active", email_verified=True)
            alice.caps = {"run_lab": True}
            cls.alice = alice.id
            group = ServerGroup(owner_id=cls.owner, name="Uma's machines", slug="umas-machines")
            s.add(group)
            s.flush()
            cls.group_id = group.id
            pool.create_grant(s, s.get(User, cls.owner), group, subject_type="user", subject_handle="ali",
                              purposes=["lab"])
            from citar.jobs import trust
            trust.save_settings(s, {"trusted_rate": 0.0})
        cls.dispatcher = D.start(D.Dispatcher(local=False))

    @classmethod
    def tearDownClass(cls):
        D.stop()
        for p in cls.patches:
            p.stop()
        ledger.reset(background=False)
        remote.forget_owner_tiers()
        super().tearDownClass()

    def lending(self, name: str) -> tuple:
        """A trusted machine of uma's lending its CPU in her group, its helper's own settings lending it too; the
        relay holds no manifest yet: (the machine, its token, the manifest and its signature to relay later)."""
        sid, token = self.machine(name, group_id=self.group_id, trust_tier="trusted", capabilities={"cpu": SITE})
        (self.home / "helper.json").write_text(json.dumps({"cpu": CEILING}), encoding="utf-8")
        manifest, signature = U.relay().current().manifest, U.relay().current().signature
        U.relay().clear()
        return sid, token, manifest, signature

    def queue(self, name: str, games: int, turns: int, *, size: str = "duel", seats: int = 2) -> str:
        exp = lab.submit({"name": name, "games": games, "seed": 9500, "size": size, "maps": ["pangaea"],
                          "turns": turns, "seats": [{"label": f"s{i}", "bot": "basic"} for i in range(seats)]})
        with db.session() as s:
            items.register(s, "lab_experiment", exp["name"], s.get(User, self.alice), exp["name"],
                           path=lab.QUEUE / f"{exp['name']}.json", collection=False)
        D.poke()
        return exp["name"]

    def test_a_game_playing_at_the_update_ends_before_the_hand_off(self):
        """The wait, then no new work: the game playing when the update is verified ends, its result goes, and only
        then the hand-off; the next game offered meanwhile is refused (busy) and plays elsewhere later."""
        sid, token, manifest, signature = self.lending("draining")
        seen: list = []
        real_frame = W._frame

        async def recording(the_hub, connection, raw):
            if connection.server_id == sid:
                try:
                    data = json.loads(raw)
                except ValueError:
                    data = {}
                seen.append((time.monotonic(), data.get("t"), data.get("state") or data.get("reason")))
            return await real_frame(the_hub, connection, raw)

        patcher = mock.patch.object(W, "_frame", recording)
        patcher.start()
        self.addCleanup(patcher.stop)
        helper = self.start(token, env={"CITAR_HELPER_TEST_IDLE_WAIT_S": "1"})
        ready = H.until(lambda: (lambda e: e is not None and e.state == "ready")(remote.registry().find(sid)), 90)
        self.assertTrue(ready, helper.text()[-3000:])
        name = self.queue("long-at-the-swap", 3, 300, size="standard", seats=4)
        self.assertTrue(H.until(lambda: any(t == "job_progress" for _, t, _ in seen), 120 * SLACK, 0.02),
                        helper.text()[-3000:])
        U.relay().accept(manifest, signature)           # verified while the first long game plays
        self.wait_state(sid, "applied", 240, helper)
        log = helper.text()
        self.assertIn("no new work until what runs ends", log)
        restart_at = next(at for at, t, state in seen if t == "update_status" and state == "restarting")
        verified_at = next(at for at, t, state in seen if t == "update_status" and state == "verified")
        results = [at for at, t, _ in seen if t == "job_result"]
        self.assertTrue(results and all(at < restart_at for at in results), (results, restart_at))
        self.assertTrue(any(verified_at < at < restart_at for at in results),
                        "the game playing at the update ended, and its result went, before the hand-off")
        self.assertTrue(H.until(lambda: lab.load_results(name), 30 * SLACK), "the game's line")
        self.assertEqual(lab.load_results(name)[0]["executor"]["machine"], sid)
        print(f"\n[update e2e] gate 4, the drain: verified at {verified_at:.2f}, the game's result at "
              f"{[round(r, 2) for r in results]}, restarting at {restart_at:.2f}; rejects "
              f"{[x for x in seen if x[1] == 'job_reject']}", file=sys.stderr)
        W.hub().drop(sid, "its worker token was revoked", code="revoked")
        self.assertEqual(helper.exited(30), 78, helper.text()[-3000:])
        # its other games are no other test's: deleted from the queue, as the Lab page's Delete does
        (lab.QUEUE / f"{name}.json").unlink()
        D.poke()

    def test_no_result_of_the_old_build_after_the_swap_and_the_stale_host_refused(self):
        sid, token, manifest, signature = self.lending("lending")
        # every frame the machine's helpers send, as the hub takes it: (when, type, update state, helper version)
        seen: list = []
        real_frame = W._frame

        async def recording(the_hub, connection, raw):
            if connection.server_id == sid:
                try:
                    data = json.loads(raw)
                except ValueError:
                    data = {}
                seen.append((time.monotonic(), data.get("t"), data.get("state"), connection.helper_version))
            return await real_frame(the_hub, connection, raw)

        patcher = mock.patch.object(W, "_frame", recording)
        patcher.start()
        self.addCleanup(patcher.stop)
        helper = self.start(token, env={"CITAR_HELPER_TEST_IDLE_WAIT_S": "2", "CITAR_HELPER_TEST_STALE_HOST": "1"})
        ready = H.until(lambda: (lambda e: e is not None and e.state == "ready")(remote.registry().find(sid)), 90)
        self.assertTrue(ready, helper.text()[-3000:])
        self.assertEqual(W.hub().get(sid).jobs_build, engine_api.jobs_build(), "the older helper plays this hub's jobs")
        exp = {"name": self.queue("before-the-swap", 12, 30)}
        self.assertTrue(H.until(lambda: any(t == "job_result" for _, t, _, _ in seen), 120 * SLACK, 0.05),
                        helper.text()[-3000:])
        # the older helper's games: the experiment's lines say its machine played them
        U.relay().accept(manifest, signature)           # the server learns of the update while games play
        self.wait_state(sid, "applied", 180, helper)
        conn = W.hub().get(sid)
        self.assertEqual(conn.helper_version, NEW)
        self.assertNotEqual(conn.jobs_build, engine_api.jobs_build(), "the newer helper is another jobs build")
        restart_at = next(at for at, t, state, v in seen if t == "update_status" and state == "restarting")
        results = [(at, v) for at, t, _, v in seen if t == "job_result"]
        self.assertTrue(results, "the older helper relayed results")
        self.assertTrue(all(at < restart_at and v == OLD for at, v in results),
                        f"every result came from the older helper before its restarting: {results}, {restart_at}")
        # the dispatcher appends a result's line once it has committed it: a moment after the frame came
        self.assertTrue(H.until(lambda: lab.load_results(exp["name"]), 30 * SLACK), "its games are the experiment's lines")
        # the new binary refused the job host of the build it replaced, and started its own
        self.assertTrue(H.until(lambda: "refused a job host of another jobs build" in helper.text(), 60 * SLACK),
                        helper.text()[-4000:])
        log = helper.text()
        self.assertIn("TEST: starting the job host from", log)
        refused_at = log.index("refused a job host of another jobs build")
        self.assertIn("the job host is ready", log[refused_at:], "its own host started after the stale one")
        self.assertTrue(H.until(lambda: (W.hub().get(sid) or conn).memory_per_run_mb is not None, 60 * SLACK),
                        "the new binary's own host acknowledged its figures")
        # no run of the old build is still open on the machine, and nothing more came from it
        with db.session() as s:
            open_runs = [r for r in s.query(CpuJobRun).filter(CpuJobRun.machine_id == sid)
                         if r.state in ("offered", "running")]
        self.assertEqual(open_runs, [], "the hand-off left no run playing")
        after = [x for x in seen if x[1] == "job_result" and x[0] > restart_at]
        self.assertEqual(after, [], "no result reached the hub after the swap")
        W.hub().drop(sid, "its worker token was revoked", code="revoked")
        self.assertEqual(helper.exited(30), 78, helper.text()[-3000:])
        lines = [x for x in log.splitlines() if any(w in x for w in ("update:", "handing off", "refused a job host",
                                                                     "job host is ready", "TEST:", "supervis"))]
        print(f"\n[update e2e] gate 4: {len(results)} results from {OLD} before its restarting, none after; "
              f"states {self.states(sid)}\n  " + "\n  ".join(lines), file=sys.stderr)


if __name__ == "__main__":
    unittest.main()

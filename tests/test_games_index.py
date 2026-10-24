"""The games index (design/phase3-features.md P3.2.5; package 3-01's gate 3).

A game's row says whether it is played (`playing`), over (`finished`) or stopped before its end (`closed`), kept
by the session's save writer after each round's autosave and by its stop, never under the game's lock. Only live
lobby games count against an account's game limit; rows left `playing` with no game are closed at boot; a game
loaded from disk gets its row back, and a session coming back to life takes its owner from it.
"""
from __future__ import annotations

import os
import shutil
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import tests  # noqa: F401
from tests import has_test_ops

_TMP = Path(tempfile.mkdtemp(prefix="citar_index_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, settings
from citar.auth import accounts, policy, sessions
from citar.db.models import Base, Game as GameRow, User
from citar.server import ownership
from citar.server import session as sess

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()

HUMAN_FIRST = [{"type": "human"}, {"type": "bot"}]       # nothing plays until the person does
CONFIG = {"map_size": "duel", "seed": 11, "barbarians": "off"}


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


def _row(gid: str):
    with db.session() as s:
        row = s.get(GameRow, gid)
        if row is not None:
            s.expunge(row)
        return row


def _write_lock_free() -> bool:
    """Whether another connection could write the database now: no transaction anywhere holds SQLite's write lock."""
    import sqlite3
    con = sqlite3.connect(str(_TMP / "test.db"), timeout=0.05, isolation_level=None)
    try:
        con.execute("BEGIN IMMEDIATE")
        con.execute("ROLLBACK")
        return True
    except sqlite3.OperationalError:
        return False
    finally:
        con.close()


class _IndexWrites:
    """Records, at each write of the games index the stop and the load make, whether the write lock was free: the
    route that closed or loaded the game holds no write of its own while the index writes on the same thread."""

    def __init__(self):
        self.free: list = []

    def __enter__(self):
        real_sync, real_loaded = ownership.sync_quietly, ownership.index_loaded

        def sync(gid, **kw):
            self.free.append(("sync", _write_lock_free()))
            return real_sync(gid, **kw)

        def loaded(gs, **kw):
            self.free.append(("loaded", _write_lock_free()))
            return real_loaded(gs, **kw)
        self._patches = [mock.patch.object(ownership, "sync_quietly", sync),
                         mock.patch.object(ownership, "index_loaded", loaded)]
        for p in self._patches:
            p.start()
        return self

    def __exit__(self, *exc):
        for p in reversed(self._patches):
            p.stop()
        return False


def _wait(cond, timeout: float = 15.0) -> bool:
    end = time.time() + timeout
    while time.time() < end:
        if cond():
            return True
        time.sleep(0.05)
    return bool(cond())


class IndexCase(unittest.TestCase):
    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        _reset()
        self.app = appmod
        self.manager = appmod.manager
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for handle, role in (("member", "user"), ("zara", "admin")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
        self.games: list = []

    def tearDown(self):
        for gid in self.games:
            s = self.manager.get(gid)
            if s is not None:
                self.manager.delete(gid)
                if s._driver is not None:
                    s._driver.join(10)
            shutil.rmtree(sess.SAVE_DIR / gid, ignore_errors=True)
        self.client.close()

    def set_limit(self, n: int):
        with db.session() as s:
            s.get(User, self.ids["member"]).max_concurrent_games = n

    def create(self, who="member", seats=None, expect=200, name="A game"):
        r = self.client.post("/api/games", json={"name": name, "config": CONFIG, "seats": seats or HUMAN_FIRST},
                             headers=self.headers[who])
        self.assertEqual(r.status_code, expect, r.text)
        if r.status_code == 200:
            self.games.append(r.json()["id"])
            return r.json()["id"]
        return None


class Statuses(IndexCase):
    def test_a_new_game_is_playing_and_owned(self):
        gid = self.create()
        row = _row(gid)
        self.assertEqual((row.status, row.kind, row.owner_id), ("playing", "game", self.ids["member"]))
        self.assertEqual(self.manager.get(gid).owner_id, self.ids["member"])

    @unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")
    def test_a_finished_game_no_longer_counts(self):
        self.set_limit(1)
        gid = self.create()
        self.create(expect=429)
        s = self.manager.get(gid)
        with s.lock:
            before = (s.game.turn, s.game.current)
            s.game.test_ops([{"op": "end_game"}])
            s._after_action(*before)            # what any action does: the game is over, so its last autosave
        self.assertTrue(_wait(lambda: _row(gid).status == "finished"), _row(gid).status)
        self.assertIsNotNone(_row(gid).finished_at)
        self.create(name="the next one")

    def test_closing_a_running_game_marks_it_closed_and_loading_it_playing(self):
        self.set_limit(1)
        gid = self.create()
        r = self.client.delete(f"/api/games/{gid}", headers=self.headers["member"])
        self.assertEqual(r.status_code, 200, r.text)
        row = _row(gid)
        self.assertEqual(row.status, "closed")
        self.assertIsNone(row.deleted_at, "closing is not deleting: its saves stay the owner's")
        self.create(name="room for another")          # a closed game counts for nothing
        other = self.games[-1]
        self.client.delete(f"/api/games/{other}", headers=self.headers["member"])
        r = self.client.post("/api/saves/load", json={"path": f"{gid}/autosave.citar"}, headers=self.headers["member"])
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(_row(gid).status, "playing")
        self.assertEqual(self.manager.get(gid).owner_id, self.ids["member"])
        saves = self.client.get("/api/saves", headers=self.headers["member"]).json()
        self.assertIn(f"{gid}/autosave.citar", [e["path"] for e in saves])

    def test_the_row_follows_the_rounds_off_the_lock(self):
        held = []
        real = ownership.sync_quietly

        def spy(gid, **kw):
            s = self.manager.get(gid)
            held.append(bool(s is not None and s.lock._is_owned()))
            return real(gid, **kw)

        with mock.patch.object(ownership, "sync_quietly", spy):
            gid = self.create(seats=[{"type": "bot"}, {"type": "bot"}])
            s = self.manager.get(gid)
            self.assertTrue(_wait(lambda: s.game.turn >= 4, 60), s.game.turn)
            s.set_paused(True)
            self.assertTrue(s.flush_saves(30))
            self.assertTrue(_wait(lambda: _row(gid).turn >= 3), _row(gid).turn)
        self.assertTrue(held, "the writer synced the row")
        self.assertFalse(any(held), "never with the game's lock held")
        self.assertEqual(_row(gid).status, "playing")


class IdleSignIn(IndexCase):
    """A request whose sign-in was last seen over a minute ago writes its ``last_seen_at``. The request's own
    database session must not still hold that write when the route closes or loads a game, whose row the games
    index writes from a session of its own on the same thread: under SQLite the index would wait out the whole busy
    timeout (10 s) and then give up, leaving a closed game counting against its owner's limit."""

    def _age_sign_ins(self):
        from datetime import timedelta
        from sqlalchemy import select
        from citar.db.models import AuthSession
        with db.session() as s:
            for row in s.scalars(select(AuthSession)):
                row.last_seen_at = row.last_seen_at - timedelta(minutes=5)

    def _timed(self, method, path, **kw):
        t0 = time.perf_counter()
        r = self.client.request(method, path, headers=self.headers["member"], **kw)
        return r, time.perf_counter() - t0

    def test_closing_and_loading_after_an_idle_spell_write_the_row_at_once(self):
        self.set_limit(1)
        gid = self.create()
        self.assertTrue(self.manager.get(gid).flush_saves(30))
        self._age_sign_ins()
        with _IndexWrites() as writes:
            r, took = self._timed("DELETE", f"/api/games/{gid}")
        self.assertEqual(r.status_code, 200, r.text)
        self.assertLess(took, 2.0, "closing waited on the request's own write")
        self.assertEqual(writes.free, [("sync", True)])
        self.assertEqual(_row(gid).status, "closed")
        self._age_sign_ins()
        with _IndexWrites() as writes:
            r, took = self._timed("POST", "/api/saves/load", json={"path": f"{gid}/autosave.citar"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertLess(took, 2.0, "loading waited on the request's own write")
        self.assertEqual(writes.free, [("loaded", True)])
        self.assertEqual(_row(gid).status, "playing")
        self.assertEqual(self.manager.get(gid).owner_id, self.ids["member"])
        self.create(expect=429)                       # loaded, it counts again

    def test_the_sign_in_is_still_marked_seen(self):
        from sqlalchemy import select
        from citar.db.models import AuthSession
        self._age_sign_ins()
        with db.session() as s:
            before = {r.id: r.last_seen_at for r in s.scalars(select(AuthSession))}
        r = self.client.get("/api/games", headers=self.headers["member"])
        self.assertEqual(r.status_code, 200, r.text)
        with db.session() as s:
            row = s.scalar(select(AuthSession).where(AuthSession.user_id == self.ids["member"]))
            self.assertGreater(row.last_seen_at, before[row.id])


class FailedRegistration(IndexCase):
    """A game the index could not take is closed again (create_game, the scenario launch). The request has written
    to the index by then; that write is undone before the close, whose own write of the row would otherwise wait
    behind it for the whole busy timeout."""

    def _failing_register(self):
        real = ownership.register

        def register(session, gs, owner, **kw):
            real(session, gs, owner, **kw)       # the row is written (flushed), as when a later step fails
            raise RuntimeError("the index refused it")
        return mock.patch.object(ownership, "register", register)

    def _attempt(self, path, body):
        from fastapi.testclient import TestClient
        client = TestClient(self.app.app, base_url="https://citar.test", raise_server_exceptions=False)
        live = set(self.manager.sessions)
        t0 = time.perf_counter()
        with self._failing_register(), _IndexWrites() as writes:
            r = client.post(path, json=body, headers=self.headers["member"])
        took = time.perf_counter() - t0
        client.close()
        self.assertEqual(r.status_code, 500, r.text)
        self.assertLess(took, 2.0, "the close waited on the request's own write")
        self.assertIn(("sync", True), writes.free)
        self.assertNotIn(("sync", False), writes.free, "the request's write was still open when the game was closed")
        self.assertEqual(set(self.manager.sessions), live, "the game is not left running")
        from sqlalchemy import func, select
        with db.session() as s:
            self.assertEqual(s.scalar(select(func.count()).select_from(GameRow)), 0, "no row is left")

    def test_a_new_game_the_index_refuses_is_closed_at_once(self):
        self._attempt("/api/games", {"name": "refused", "config": CONFIG, "seats": HUMAN_FIRST})

    def test_a_launch_the_index_refuses_is_closed_at_once(self):
        from citar import engine_api
        r = self.client.post("/api/scenario-editor", json={"source": "generate", "config": CONFIG,
                                                            "players": [{"type": "human"}, {"type": "bot"}]},
                             headers=self.headers["member"])
        self.assertEqual(r.status_code, 200, r.text)
        r = self.client.post(f"/api/scenario-editor/{r.json()['editor_id']}/save", json={"name": "refused launch"},
                             headers=self.headers["member"])
        self.assertEqual(r.status_code, 200, r.text)
        sid = r.json()["id"]
        try:
            self._attempt(f"/api/scenarios/{sid}/launch", {"name": "refused"})
        finally:
            engine_api.delete_scenario(sid)


class Limit(IndexCase):
    def test_benchmark_rows_do_not_count_against_the_lobby_limit(self):
        with db.session() as s:
            for n in range(3):
                s.add(GameRow(id=f"bench{n}", owner_id=self.ids["member"], kind="benchmark", status="playing"))
            s.add(GameRow(id="done1", owner_id=self.ids["member"], kind="game", status="finished"))
            s.add(GameRow(id="gone1", owner_id=self.ids["member"], kind="game", status="closed"))
        self.create(name="one")
        self.create(name="two")
        r = self.client.post("/api/games", json={"name": "three", "config": CONFIG, "seats": HUMAN_FIRST},
                             headers=self.headers["member"])
        self.assertEqual(r.status_code, 429, r.text)
        self.assertIn("2 games", r.json()["detail"])


class Loading(IndexCase):
    def _closed_game_without_a_row(self) -> str:
        gid = self.create()
        self.client.delete(f"/api/games/{gid}", headers=self.headers["member"])
        with db.session() as s:
            s.delete(s.get(GameRow, gid))     # as when the database was set aside for 0.1.6's
        return gid

    def test_a_loaded_save_without_a_row_gets_one(self):
        gid = self._closed_game_without_a_row()
        s = self.manager.load(sess.SAVE_DIR / gid / "autosave.citar")
        row = _row(gid)
        self.assertEqual((row.status, row.kind, row.owner_id, row.visibility), ("playing", "game", None, "private"))
        self.assertEqual(row.data_sharing, "private", "a game nobody owns pools nothing")
        self.assertIsNone(s.owner_id)

    def test_an_opened_probe_case_is_a_probes_game_not_a_lobby_game(self):
        """``POST /api/probe-runs/{rid}/open/{case}/{rep}`` loads a case's saved end state to be looked at, for whoever
        may see the run (3-02): its row, should it have none, is of kind `probe`, so it never counts as a lobby game,
        and is the run's owner's, with their pooling default, at the run's visibility."""
        import types
        from citar import items
        case = self.manager.create(CONFIG, HUMAN_FIRST, "probe p/c", start=False, track=False)
        run_dir = Path(tempfile.mkdtemp(prefix="citar_index_probe_"))
        try:
            case.save_copy(run_dir / "c1-0.citar")
            self.manager.delete(case.id)
            self.games.append(case.id)
            self.assertIsNone(_row(case.id))
            with db.session() as s:
                items.register(s, "probe_run", "r1", self.ids["member"], "a member's run")
                s.get(User, self.ids["member"]).data_sharing = "private"
            runner = types.SimpleNamespace(_run_dir=lambda rid: run_dir)
            with mock.patch.object(self.app, "_probes", return_value=runner):
                r = self.client.post("/api/probe-runs/r2/open/c1/0", headers=self.headers["zara"])
                self.assertEqual(r.status_code, 404, "a run with no row is nobody's, an administrator's either")
                r = self.client.post("/api/probe-runs/r1/open/c1/0", headers=self.headers["member"])
            self.assertEqual(r.status_code, 200, r.text)
            self.assertEqual(r.json()["game_id"], case.id)
            row = _row(case.id)
            self.assertEqual((row.kind, row.owner_id, row.data_sharing, row.status, row.visibility),
                             ("probe", self.ids["member"], "private", "playing", "private"))
            self.assertEqual(self.manager.get(case.id).owner_id, self.ids["member"])
        finally:
            shutil.rmtree(run_dir, ignore_errors=True)

    def test_whoever_loads_an_ownerless_save_owns_it(self):
        gid = self._closed_game_without_a_row()
        # a save whose game has no row is an administrator's to load
        r = self.client.post("/api/saves/load", json={"path": f"{gid}/autosave.citar"}, headers=self.headers["member"])
        self.assertEqual(r.status_code, 404, r.text)
        with db.session() as s:
            s.get(User, self.ids["zara"]).data_sharing = "pool"
        r = self.client.post("/api/saves/load", json={"path": f"{gid}/autosave.citar"}, headers=self.headers["zara"])
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual((_row(gid).owner_id, _row(gid).data_sharing), (self.ids["zara"], "pool"),
                         "its new owner, with their default for pooling")
        self.assertEqual(self.manager.get(gid).owner_id, self.ids["zara"])

    def test_deleting_every_save_deletes_the_game(self):
        gid = self.create()
        self.client.delete(f"/api/games/{gid}", headers=self.headers["member"])
        r = self.client.post("/api/saves/delete", json={"path": f"{gid}/autosave.citar", "whole_game": True},
                             headers=self.headers["member"])
        self.assertEqual(r.status_code, 200, r.text)
        self.assertIsNotNone(_row(gid).deleted_at)


class NoDatabase(unittest.TestCase):
    def test_a_process_that_never_opened_its_database_leaves_it_alone(self):
        """``citar bench`` plays games without a server: the index must not open (or create, or put in WAL mode) the
        default database for them."""
        import subprocess
        import sys
        data = Path(tempfile.mkdtemp(prefix="citar_index_nodb_"))
        env = {k: v for k, v in os.environ.items() if k != "CITAR_DB_URL"}
        env["CITAR_DATA_DIR"] = str(data)
        code = ("from citar import db; from citar.server import ownership; "
                "ownership.sync_quietly('g1', turn=3, phase='playing', stopped=True); "
                "print(ownership.index_loaded(object()), db.configured())")
        out = subprocess.run([sys.executable, "-c", code], cwd=str(Path(__file__).resolve().parent.parent), env=env,
                             capture_output=True, text=True, timeout=120)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.split(), ["None", "False"])
        self.assertEqual(list(data.iterdir()), [])
        shutil.rmtree(data, ignore_errors=True)


class Boot(IndexCase):
    def test_a_playing_row_whose_save_folder_went_is_closed_at_boot(self):
        gid = self.create()
        kept = self.create(name="still here")
        s = self.manager.sessions.pop(gid)
        # the process dies: nothing tells the index
        with mock.patch.object(ownership, "sync_quietly"):
            s.stop()
        shutil.rmtree(sess.SAVE_DIR / gid)
        self.assertEqual(_row(gid).status, "playing")
        self.manager.restore_live()          # it cannot come back
        with self.assertLogs("citar.ownership", "WARNING") as logged:
            closed = self.app._close_orphan_rows()
        self.assertEqual(closed, [gid])
        self.assertIn(gid, "\n".join(logged.output))
        self.assertEqual(_row(gid).status, "closed")
        self.assertEqual(_row(kept).status, "playing")

    def test_a_game_restore_live_brings_back_has_its_rows_owner(self):
        saves = Path(tempfile.mkdtemp(prefix="citar_index_saves_"))
        try:
            with mock.patch.object(sess, "SAVE_DIR", saves):
                gid = self.create()
                live = self.manager.get(gid)
                self.assertTrue(live.flush_saves(30))
                self.assertTrue((saves / gid / sess.GameSession.LIVE_MARK).exists())
                # the server stops: the session goes without being closed by anyone
                self.manager.sessions.pop(gid)
                live.stop()
                restored = self.manager.restore_live()
                self.assertEqual(len(restored), 1, restored)
                s = self.manager.get(gid)
                self.assertIsNotNone(s)
                self.assertEqual(s.owner_id, self.ids["member"])
                self.assertEqual(_row(gid).status, "playing")
                self.manager.delete(gid)
                s._driver.join(10)
        finally:
            shutil.rmtree(saves, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()

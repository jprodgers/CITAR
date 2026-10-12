"""The routes package 3-07 changed, by every kind of caller (design/phase3-features.md P3.9.1).

- ``POST /api/pool/servers/{id}/test``: the Servers page's test of a machine, now a ``test`` ticket of the prompt
  scheduler (P3.3.3) with a ``wait`` (1 to 120 s). A visitor 401; another member, and a moderator, 404 on a private
  machine; its owner, a member her grant reaches and an administrator get the helper's reply (a connected helper with
  no socket, which answers at once); ``wait`` out of its bounds 422; a model the helper has not loaded refused by the
  queue with its code. Each caller has one test of a machine at a time and ``TESTS_PER_ACCOUNT`` in all, refused at
  once past them (each holds a request until its reply); the request's database connection is back in the pool while
  it waits; the test is metered under no activity.
- ``GET /api/status``: an administrator's lists the scheduler's machines (with its leases' game labels); a member's and
  a moderator's do not; a visitor gets 401.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import os
import tempfile
import threading
import time
import unittest
from pathlib import Path

_TMP = Path(tempfile.mkdtemp(prefix="citar_scheduler_routes_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, pool, servers, settings
from citar.auth import accounts, policy, sessions
from citar.db.models import Base, Server, User
from citar.pool import scheduler as S
from citar.server import pool_api
from citar.server import workers as W
from citar.worker import protocol as P

settings.reset()

MODEL = "qa/scripted"


class StubHelper(W.WorkerConnection):
    """A connected helper with no socket. It answers each request at once with a ``Done`` (``CITAR OK``), or, with
    ``hold``, keeps it until ``answer_held``; it records every frame the hub sends it."""

    def __init__(self, server_id: str, models: list, *, hold: bool = False):
        super().__init__(websocket=None, server_id=server_id, loop=None,
                         hello={"hostname": "stub", "provider": "lmstudio", "models": models, "max_concurrent": 1})
        self.hold = hold
        self.held: list = []
        self.frames: list = []

    def send_threadsafe(self, text: str):
        frame = json.loads(text)
        self.frames.append(frame)
        if frame.get("t") != P.REQUEST:
            return
        if self.hold:
            self.held.append(frame["id"])
        else:
            self._done(frame["id"])

    def _done(self, request_id: str):
        self.resolve(request_id, result={"text": "CITAR OK", "duration": 0.01, "tool_calls": [],
                                         "usage": {"input_tokens": 12, "output_tokens": 3}})

    def answer_held(self):
        while self.held:
            self._done(self.held.pop(0))


def _until(check, limit: float = 10.0):
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        if check():
            return True
        time.sleep(0.01)
    return bool(check())


class TestRoute(unittest.TestCase):
    """The test route, and the status route's machines, by alice (the machine's owner), bob (a member her group's
    grant reaches), carol (another member), moe (a moderator), zara (an administrator) and a visitor."""

    WHO = (("alice", "user"), ("bob", "user"), ("carol", "user"), ("moe", "moderator"), ("zara", "admin"))

    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        db.dispose()
        db.configure(os.environ["CITAR_DB_URL"])
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        servers.invalidate()
        S.reset()
        self.addCleanup(S.reset)
        self.app = appmod.app
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for handle, role in self.WHO:
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
            alice = s.get(User, self.ids["alice"])
            group = pool.create_group(s, alice, name="Alice's", visibility="private")
            pool.create_grant(s, alice, group, subject_type="user", subject_handle="bob", purposes=["game"])
            self.machines = {}
            for key in ("gpu", "spare", "picky"):
                row = Server(owner_id=alice.id, name=f"Alice {key}", kind="owned", provider="lmstudio", reach="worker",
                             group_id=group.id, max_concurrent=1, config={"models": [{"key": MODEL}]})
                s.add(row)
                s.flush()
                self.machines[key] = row.id
        servers.invalidate()
        self.helpers = {}
        for key, sid in self.machines.items():
            loaded = key != "picky"           # the picky box's helper reports its model, not loaded
            self.helpers[key] = StubHelper(sid, [{"key": MODEL, "loaded": loaded}])
            W.hub().register(self.helpers[key])
            self.addCleanup(W.hub().unregister, self.helpers[key])
        pool_api._tests_waiting.clear()

    def call(self, who, method, path, params=None, client=None):
        headers = self.headers.get(who, {"origin": "https://citar.test"})
        return (client or self.client).request(method, path, params=params, headers=headers)

    def run_test(self, who, key="gpu", client=None, **params):
        return self.call(who, "POST", f"/api/pool/servers/{self.machines[key]}/test", params=params or None,
                         client=client)

    # -------------------------------------------------------------- the test route
    def test_who_may_test_a_machine(self):
        events: list = []
        S.get().add_listener(events.append)
        for who, code in (("anon", 401), ("carol", 404), ("moe", 404), ("alice", 200), ("bob", 200), ("zara", 200)):
            with self.subTest(who=who):
                got = self.run_test(who, wait=30)
                self.assertEqual(got.status_code, code, got.text[:300])
                if code == 200:
                    body = got.json()
                    self.assertTrue(body["ok"], body)
                    self.assertEqual((body["model"], body["reply"]), (MODEL, "CITAR OK"))
                    self.assertGreaterEqual(body["queue_s"], 0.0)
                    self.assertLessEqual(body["gen_s"], 0.01)
        self.assertEqual(len(events), 3, "each answered test went through the scheduler")
        self.assertEqual({e.activity for e in events}, {None}, "and is metered under no activity")
        self.assertEqual({e.cls for e in events}, {"test"})
        self.assertEqual(sorted(e.consumer for e in events),
                         sorted(self.ids[h] for h in ("alice", "bob", "zara")))

    def test_wait_out_of_its_bounds_is_refused(self):
        for wait in ("0", "0.5", "121", "nan", "inf", "-1", "soon"):
            with self.subTest(wait=wait):
                self.assertEqual(self.call("alice", "POST", f"/api/pool/servers/{self.machines['gpu']}/test",
                                           params={"wait": wait}).status_code, 422)
        self.assertEqual(self.helpers["gpu"].frames, [], "nothing was sent")

    def test_a_model_the_helper_has_not_loaded_is_refused_by_the_queue(self):
        got = self.run_test("alice", "picky", model=MODEL, wait=5)
        self.assertEqual(got.status_code, 200)
        body = got.json()
        self.assertEqual((body["ok"], body["stage"], body["code"]), (False, "queue", "not_loaded"), body)
        self.assertEqual(self.helpers["picky"].frames, [], "nothing was sent to the helper")

    def test_a_caller_has_one_test_of_a_machine_at_a_time_and_two_in_all(self):
        """Each test holds a request until its reply (or its ``wait``): a second of the same machine by the same
        caller, and a third of any, are refused at once; the database connection of a waiting one is back in the
        pool; another caller's test of the same machine queues as usual."""
        from fastapi.testclient import TestClient
        for helper in self.helpers.values():
            helper.hold = True
        out: dict = {}

        def background(key):
            client = TestClient(self.app, base_url="https://citar.test")
            try:
                out[key] = self.run_test("alice", key, client=client, wait=30)
            finally:
                client.close()
        first = threading.Thread(target=background, args=("gpu",), daemon=True)
        first.start()
        self.assertTrue(_until(lambda: self.helpers["gpu"].held), "her first test reached the helper")
        self.assertEqual(db.engine().pool.checkedout(), 0, "the waiting test holds no database connection")

        t0 = time.monotonic()
        again = self.run_test("alice", "gpu", wait=30)
        self.assertLess(time.monotonic() - t0, 2.0, "refused at once, not queued")
        self.assertEqual((again.status_code, again.json().get("code"), again.json().get("stage")),
                         (200, "duplicate", "queue"), again.text[:300])

        second = threading.Thread(target=background, args=("spare",), daemon=True)
        second.start()
        self.assertTrue(_until(lambda: self.helpers["spare"].held), "a test of another machine of hers goes")
        third = self.run_test("alice", "picky", wait=30)
        self.assertEqual(third.json().get("code"), "too_many", third.text[:300])

        # bob's test of the same machine is his own: it queues behind hers
        bobs: dict = {}

        def bob():
            client = TestClient(self.app, base_url="https://citar.test")
            try:
                bobs["got"] = self.run_test("bob", "gpu", client=client, wait=30)
            finally:
                client.close()
        his = threading.Thread(target=bob, daemon=True)
        his.start()
        machine = S.get().machine(self.machines["gpu"])
        self.assertTrue(_until(lambda: len(machine.waiting) == 1), "his test waits for the slot")

        for helper in self.helpers.values():
            helper.answer_held()
        first.join(10)
        second.join(10)
        self.assertTrue(_until(lambda: self.helpers["gpu"].held))
        self.helpers["gpu"].answer_held()
        his.join(10)
        for key, got in (("gpu", out.get("gpu")), ("spare", out.get("spare")), ("bob", bobs.get("got"))):
            with self.subTest(test=key):
                self.assertIsNotNone(got)
                self.assertTrue(got.json().get("ok"), got.text[:300])
        self.assertGreater(bobs["got"].json()["queue_s"], 0.0, "he waited his turn")
        self.assertEqual(pool_api._tests_waiting, {}, "every test counted out")
        self.helpers["gpu"].hold = False
        self.assertTrue(self.run_test("alice", "gpu", wait=30).json().get("ok"), "and she may test it again")

    def test_a_test_given_up_is_cancelled_and_its_slot_drains(self):
        helper = self.helpers["gpu"]
        helper.hold = True
        t0 = time.monotonic()
        got = self.run_test("alice", "gpu", wait=1)
        self.assertLess(time.monotonic() - t0, 5.0)
        body = got.json()
        self.assertEqual((body["ok"], body.get("cancelled")), (False, True), body)
        self.assertIn(P.CANCEL, [f.get("t") for f in helper.frames], "the helper was sent CANCEL")
        machine = S.get().machine(self.machines["gpu"])
        self.assertEqual([s.draining for s in machine.slots], [True], "its slot drains until the helper's answer")
        helper.answer_held()                    # the late answer: the hub hands it to the scheduler
        self.assertEqual(machine.slots, [])
        self.assertEqual(machine.counters["late_frames"], 1)

    def test_a_drain_ends_when_the_helper_disconnects_or_reconnects(self):
        """The helper's late answer can come only over the connection the request went out on: when the hub loses
        that connection, or a new one replaces it, the scheduler gives the draining slot back at once."""
        machine = S.get().machine(self.machines["gpu"])
        for how in ("disconnect", "reconnect"):
            with self.subTest(how=how):
                helper = self.helpers["gpu"]
                helper.hold = True
                body = self.run_test("alice", "gpu", wait=1).json()
                self.assertTrue(body.get("cancelled"), body)
                self.assertEqual([s.draining for s in machine.slots], [True])
                fresh = StubHelper(self.machines["gpu"], [{"key": MODEL, "loaded": True}])
                if how == "disconnect":
                    W.hub().unregister(helper)
                    self.assertEqual(machine.slots, [], "the helper went, and the drain with it")
                W.hub().register(fresh)
                self.addCleanup(W.hub().unregister, fresh)
                self.helpers["gpu"] = fresh
                self.assertEqual(machine.slots, [], "a new connection: nothing drains on the old one")
                self.assertTrue(self.run_test("alice", "gpu", wait=5).json().get("ok"), "the machine serves at once")
        self.assertEqual((machine.counters["drains_dropped"], machine.counters["drain_timeouts"]), (2, 0))

    # -------------------------------------------------------------- the status route
    def test_only_an_administrators_status_lists_the_scheduler_machines(self):
        S.get().lease(self.machines["gpu"], MODEL, kind="game", label="game “Alice's private game”")
        self.assertEqual(self.call("anon", "GET", "/api/status").status_code, 401)
        for who in ("alice", "bob", "carol", "moe"):
            with self.subTest(who=who):
                got = self.call(who, "GET", "/api/status")
                self.assertEqual(got.status_code, 200)
                self.assertNotIn("machines", got.json())
                self.assertNotIn("Alice's private game", got.text)
        got = self.call("zara", "GET", "/api/status")
        self.assertEqual(got.status_code, 200)
        rows = {m["id"]: m for m in got.json()["machines"]}
        self.assertEqual([lease["label"] for lease in rows[self.machines["gpu"]]["leases"]],
                         ["game “Alice's private game”"])


if __name__ == "__main__":
    unittest.main()

"""A hybrid seat through the routes (design/phase3-features.md P3.6.2, P3.6.7; package 3-14's gate 8): ``POST
/api/games`` makes it (its block checked: 400 for settings that are not allowed; the members' gate on its model block,
P3.2.9; admission by its task class, P3.3.7), and ``GET /api/games/{gid}/summary`` labels it, "Hybrid: <bot> +
<model>", with its class (and answers a stranger 404, P3.9.1)."""
from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_hybrid_routes_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, servers, settings
from citar.auth import accounts, policy, sessions
from citar.db.models import Base
from citar.pool import scheduler as S

settings.reset()

HYBRID = {"type": "hybrid", "bot": {"profile": "standard"}, "hybrid": {"owners": "trade_only", "prompt_every": 4},
          "llm": {"provider": "dryrun", "model": "dry-run", "dry_run_delay": 0}}


class Routes(unittest.TestCase):
    WHO = (("alice", "user"), ("bob", "user"), ("zara", "admin"))

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
        self.manager = appmod.manager
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        self.headers = {}
        with db.session() as s:
            for handle, role in self.WHO:
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                user.max_concurrent_games = 20
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
        self.games: list = []

    def tearDown(self):
        for gid in self.games:
            if self.manager.get(gid) is not None:
                self.manager.delete(gid)

    def create(self, who: str, seats: list):
        r = self.client.post("/api/games", headers=self.headers[who], json={
            "name": "hybrid", "config": {"map_size": "duel", "seed": 9, "barbarians": "off", "city_states": 0},
            "seats": seats})
        if r.status_code == 200:
            self.games.append(r.json()["id"])
            s = self.manager.get(r.json()["id"])
            s.set_paused(True)              # nothing plays on but the test
        return r

    def test_an_administrator_makes_a_hybrid_seat_and_the_summary_labels_it(self):
        r = self.create("zara", [{"type": "human"}, dict(HYBRID)])
        self.assertEqual(r.status_code, 200, r.text)
        seat = r.json()["seats"][1]
        self.assertEqual((seat["type"], seat["hybrid"]["preset"], seat["hybrid"]["prompt_every"]),
                         ("hybrid", "trade_only", 4))
        s = self.manager.get(r.json()["id"])
        p = s.game.player(1)
        self.assertEqual((p["controller"], p["handicap"]), ("hybrid", "ai"))
        got = self.client.get(f"/api/games/{s.id}/summary", headers=self.headers["zara"])
        self.assertEqual(got.status_code, 200, got.text)
        row = next(x for x in got.json()["players"] if x["id"] == 1)
        self.assertEqual((row["seat"], row["model"], row["class"], row["label"]),
                         ("hybrid", "dry-run", "partial", "Hybrid: standard + dry-run"))
        person = next(x for x in got.json()["players"] if x["id"] == 0)
        self.assertNotIn("label", person)
        self.assertEqual(self.client.get(f"/api/games/{s.id}/summary", headers=self.headers["bob"]).status_code, 404,
                         "another member's private game")
        anonymous = self.client.get(f"/api/games/{s.id}/summary").status_code
        self.assertIn(anonymous, (401, 404))

    def test_settings_that_are_not_allowed_are_refused(self):
        for hybrid, words in (({"owners": {}}, "nothing for its model"), ({"owners": {"bribes": "llm"}}, "bribes"),
                              ({"owners": "trade_only", "prompt_every": 99}, "prompt_every")):
            with self.subTest(hybrid=hybrid):
                r = self.create("zara", [{"type": "human"}, {**HYBRID, "hybrid": hybrid}])
                self.assertEqual(r.status_code, 400, r.text)
                self.assertIn(words, r.json()["detail"])
        r = self.create("zara", [{"type": "human"}, {**HYBRID, "handicap": "god"}])
        self.assertEqual(r.status_code, 400, r.text)

    def test_a_member_s_hybrid_seat_names_a_machine_and_is_admitted_by_its_class(self):
        r = self.create("alice", [{"type": "human"}, {**HYBRID, "llm": {"base_url": "http://evil.test/v1",
                                                                         "model": "m"}}])
        self.assertEqual(r.status_code, 400, r.text)
        self.assertIn("Pick a machine", r.json()["detail"])
        # a machine she has no grant on: refused by admission (a hybrid seat is task class partial: pooled)
        r = self.create("alice", [{"type": "human"}, {**HYBRID, "llm": {"server_id": "sv_gpu", "model": "dry-run"}}])
        self.assertEqual(r.status_code, 403, r.text)

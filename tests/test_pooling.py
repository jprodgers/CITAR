"""Pooling in the model figures (design/phase3-features.md P3.2.4; package 3-03, gate 1).

``/api/models/scores`` and ``/api/metrics/compare`` read, for a member, their own games and the games other members
pool (``aggregate.scope_for``: the game's row says `pool` and its owner has not opted out), a model's pooled part only
when enough other accounts are behind it (``aggregate.publishable``), and name only the member's own games. An
administrator's figures cover every game. The games' figures are made up here (``scoring._reports`` is replaced by
one that honours its ``allowed`` as the real one does, which ``ReadsOnlyAllowed`` checks on real sessions and saves).
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="citar_pooling_"))
DB_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = DB_URL
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import aggregate, db
from citar.auth import accounts, policy, sessions
from citar.db.models import Base, Game as GameRow, User
from citar.server import scoring

WEIGHTS = {"benchmark": 0.6, "reliability": 0.25, "speed": 0.15}
#: The games and who plays which model in them: (game id, owner, model, a benchmark game).
GAMES = [("ga", "alice", "m-shared", True), ("gb", "bob", "m-shared", True), ("gc", "carol", "m-shared", True),
         ("gd", "dave", "m-shared", True), ("gd2", "dave", "m-dave", True),
         ("gb2", "bob", "m-two", False), ("gc2", "carol", "m-two", False)]


def _metrics(model: str) -> dict:
    """One model seat's metrics summary, as ``Metrics.summary`` gives it."""
    return {"model": model, "controller": "llm", "turns": 5, "avg_turn_s": 12.0, "avg_model_steps": 2.0,
            "avg_tool_calls": 3.0, "avg_errors": 0.2, "avg_repeats": 0.0, "avg_output_tokens": 100.0,
            "output_tokens_per_s": 25.0, "malformed_calls": 0, "blocked_repeats": 0, "stall_nudges": 0,
            "max_turn_s": 20.0, "end_reasons": {"end_turn": 5}}


def _report(gid: str, model: str, bench: bool) -> dict:
    """A game's entry in ``scoring._reports``."""
    return {"name": f"game {gid}", "turn": 10, "summary": {0: _metrics(model)},
            "benchmark": {"model": model, "server": "x", "scenario": "duel", "run_id": "r", "job_id": "j", "turns": 9,
                          "turn_limit": 100, "phase": "playing", "performance": 50.0, "score": 10,
                          "best_bot_score": 10} if bench else None}


REPORTS = {gid: _report(gid, model, bench) for gid, _owner, model, bench in GAMES}


def _fake_reports(manager, allowed=None):
    """``scoring._reports`` over the made-up games, honouring ``allowed`` as the real one does."""
    return {gid: rep for gid, rep in REPORTS.items() if allowed is None or gid in allowed}


class PoolingRoutes(unittest.TestCase):
    """The two routes, by a member, a member who opted out, an administrator and an anonymous caller."""

    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        cls.client = TestClient(appmod.app, base_url="https://citar.test")
        cls.patches = [mock.patch.object(scoring, "_reports", _fake_reports),
                       mock.patch.object(appmod, "_scheduler",
                                         lambda: SimpleNamespace(get_settings=lambda: {"score_weights": WEIGHTS}))]
        for p in cls.patches:
            p.start()

    @classmethod
    def tearDownClass(cls):
        for p in reversed(cls.patches):
            p.stop()
        cls.client.close()

    def setUp(self):
        db.dispose()
        db.configure(DB_URL)
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for handle, role in (("alice", "user"), ("bob", "user"), ("carol", "user"), ("dave", "user"),
                                 ("zara", "admin")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
            for gid, owner, _model, bench in GAMES:
                s.add(GameRow(id=gid, owner_id=self.ids[owner], name=f"game {gid}", kind="benchmark" if bench else "game",
                              visibility="private", data_sharing="pool", status="finished"))

    def tearDown(self):
        policy.invalidate()

    # ---------------------------------------------------------------- helpers
    def compare(self, who: str) -> dict:
        r = self.client.get("/api/metrics/compare", headers=self.headers[who])
        self.assertEqual(r.status_code, 200, r.text)
        return {row["model"]: row for row in r.json()}

    def scores(self, who: str) -> dict:
        r = self.client.get("/api/models/scores", headers=self.headers[who])
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()

    def threshold(self, n: int) -> None:
        policy.set("aggregate_min_contributors", n)
        policy.invalidate()

    def opt_out(self, who: str) -> None:
        r = self.client.put("/api/shared/pooling", json={"data_sharing": "private"}, headers=self.headers[who])
        self.assertEqual(r.status_code, 200, r.text)

    # ---------------------------------------------------------------- gate 1
    def test_a_member_reads_their_own_games_and_the_pooled_ones_never_named(self):
        rows = self.compare("alice")
        shared = rows["m-shared"]
        self.assertEqual(shared["games"], 4, "alice's own and three other accounts' pooled games")
        self.assertEqual(shared["games_list"], ["game ga (T10)"], "only her own game is named")
        self.assertEqual(shared["pooled_games"], 3)
        self.assertIsNone(shared["pooled_withheld"])
        scores = {m["model"]: m for m in self.scores("alice")["models"]}
        self.assertEqual(scores["m-shared"]["benchmark_games"], 4)
        self.assertEqual([g["game_id"] for g in scores["m-shared"]["games"]], ["ga"])
        self.assertEqual(scores["m-shared"]["pooled_games"], 3)

    def test_an_account_that_opted_out_is_absent_for_others_and_present_for_itself(self):
        self.threshold(1)
        self.assertIn("m-dave", self.compare("alice"))
        self.opt_out("dave")
        alice, dave = self.compare("alice"), self.compare("dave")
        self.assertNotIn("m-dave", alice, "dave's game no longer feeds anybody else's figures")
        self.assertEqual(alice["m-shared"]["games"], 3, "and leaves the shared model's figures too")
        self.assertNotIn("m-dave", {m["model"] for m in self.scores("alice")["models"]})
        self.assertNotIn("m-dave", {w["model"] for w in self.scores("alice")["withheld"]},
                         "nor is it a withheld marker: it is not in the pool at all")
        self.assertEqual(dave["m-dave"]["games"], 1, "his own figures are still his")
        self.assertEqual(dave["m-dave"]["games_list"], ["game gd2 (T10)"])
        self.assertIn("m-dave", {m["model"] for m in self.scores("dave")["models"]})

    def test_a_game_whose_row_opted_out_is_absent_for_others_and_present_for_its_owner(self):
        self.threshold(1)
        with db.session() as s:
            s.get(GameRow, "gd2").data_sharing = "private"
        self.assertNotIn("m-dave", self.compare("alice"))
        self.assertNotIn("m-dave", {m["model"] for m in self.scores("bob")["models"]})
        self.assertEqual(self.compare("dave")["m-dave"]["games"], 1)

    def test_a_bucket_under_the_threshold_is_withheld(self):
        two = self.compare("alice")["m-two"]
        self.assertTrue(two.get("withheld"))
        self.assertEqual(two["contributors"], 2)
        self.assertNotIn("games", two, "a withheld model carries no figures")
        self.assertIn("m-two", {w["model"] for w in self.scores("alice")["withheld"]})
        self.threshold(2)
        self.assertEqual(self.compare("alice")["m-two"]["games"], 2, "two accounts are enough at a threshold of two")

    def test_a_members_own_figures_are_never_withheld_but_others_are_not_added_under_the_threshold(self):
        two = self.compare("bob")["m-two"]
        self.assertEqual(two["games"], 1, "bob's own game; carol's alone would be one account's figures")
        self.assertEqual(two["pooled_withheld"], 1)
        self.assertEqual(two["games_list"], ["game gb2 (T10)"])

    def test_an_administrator_reads_every_game(self):
        self.opt_out("dave")
        rows = self.compare("zara")
        self.assertEqual(rows["m-shared"]["games"], 4)
        self.assertEqual(rows["m-two"]["games"], 2)
        self.assertEqual(rows["m-dave"]["games"], 1)
        self.assertIn("game gb (T10)", rows["m-shared"]["games_list"])
        scores = self.scores("zara")
        self.assertNotIn("scope", scores)
        self.assertEqual({m["model"] for m in scores["models"]}, {"m-shared", "m-dave", "m-two"})

    def test_an_anonymous_caller_gets_401(self):
        self.assertEqual(self.client.get("/api/metrics/compare").status_code, 401)
        self.assertEqual(self.client.get("/api/models/scores").status_code, 401)

    def test_a_suspended_member_is_refused(self):
        with db.session() as s:
            s.get(User, self.ids["bob"]).status = "suspended"
        self.assertIn(self.client.get("/api/metrics/compare", headers=self.headers["bob"]).status_code, (401, 403))


class Scope(unittest.TestCase):
    """``aggregate.scope_for`` and ``publishable`` on their own."""

    def setUp(self):
        db.dispose()
        db.configure(DB_URL)
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()

    def test_a_game_pools_when_its_row_and_its_owner_say_so(self):
        with db.session() as s:
            a = accounts.create_user(s, handle="anna", email="anna@example.com", password="a long enough phrase",
                                     status="active", email_verified=True)
            b = accounts.create_user(s, handle="ben", email="ben@example.com", password="a long enough phrase",
                                     status="active", email_verified=True)
            gone = accounts.create_user(s, handle="gone", email="gone@example.com", password="a long enough phrase",
                                        status="active", email_verified=True)
            b.data_sharing = "private"
            gone.status = "deleted"
            for gid, owner, sharing in (("p1", a, "pool"), ("p2", a, "private"), ("p3", b, "pool"),
                                        ("p4", gone, "pool"), ("p5", None, "pool")):
                s.add(GameRow(id=gid, owner_id=owner.id if owner else None, name=gid, kind="game",
                              visibility="private", data_sharing=sharing, status="finished"))
            s.flush()
            self.assertEqual(aggregate.pooled_game_ids(s), {"p1"})
            scope = aggregate.scope_for(s, b)
            self.assertEqual(scope.own_ids, {"p3"}, "ben's own game is his, opted out or not")
            self.assertEqual(scope.pooled_ids, {"p1"})
            self.assertEqual(scope.owner_of, {"p3": b.id, "p1": a.id})
            summary = aggregate.summarize_sharing(s)
            self.assertEqual((summary["games_pooled"], summary["games_private"]), (1, 4))

    def test_publishable_counts_only_the_other_accounts(self):
        scope = aggregate.Scope(viewer=None, game_ids={"a", "b", "c"}, own_ids={"a"}, pooled_ids={"b", "c"},
                                owner_of={"a": "me", "b": "u1", "c": "u2"})
        allowed, withheld = aggregate.publishable(scope, {"m": {"a", "b", "c"}, "n": {"b"}, "o": {"a"}}, threshold=2)
        self.assertEqual(allowed, {"m": {"a", "b", "c"}, "n": set(), "o": {"a"}})
        self.assertEqual(withheld, {"n": 1})
        allowed, withheld = aggregate.publishable(scope, {"m": {"a", "b", "c"}}, threshold=3)
        self.assertEqual((allowed, withheld), ({"m": {"a"}}, {"m": 2}),
                         "the viewer and two others are not three others: the others' part is withheld")


class ReadsOnlyAllowed(unittest.TestCase):
    """The real ``scoring._reports`` reads only the games in its ``allowed``: live sessions and saves alike, a save it
    is not allowed never opened."""

    def test_only_allowed_sessions_and_saves_are_read(self):
        from citar.server.session import SAVE_DIR, SessionManager
        mgr = SessionManager()
        cfg = {"map_size": "duel", "seed": 7, "barbarians": "off"}
        live = mgr.create(cfg, [{"type": "bot"}, {"type": "bot"}], "pool live", start=False, track=False)
        saved = mgr.create(cfg, [{"type": "bot"}, {"type": "bot"}], "pool saved", start=False, track=False)
        saved.save("autosave")
        mgr.delete(saved.id)
        other = mgr.create(cfg, [{"type": "bot"}, {"type": "bot"}], "pool other", start=False, track=False)
        other.save("autosave")
        mgr.delete(other.id)
        try:
            opened = []
            real = scoring._digest_save
            with mock.patch.object(scoring, "_digest_save", lambda p: (opened.append(p.parent.name), real(p))[1]):
                out = scoring._reports(mgr, {live.id, saved.id})
            self.assertEqual(set(out), {live.id, saved.id})
            self.assertNotIn(other.id, opened, "a save outside the scope is never read")
            self.assertEqual(set(scoring._reports(mgr, set())), set())
        finally:
            mgr.delete(live.id)
            for gid in (saved.id, other.id):
                shutil.rmtree(SAVE_DIR / gid, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()

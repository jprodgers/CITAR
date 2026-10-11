"""Owned items: every job type owned, gated and shareable (design/phase3-features.md P3.2.4; package 3-02's gates).

The table here is every route of P3.2.4, each with the kind of gate it has, and is checked against the app's own route
list: a route under one of the item prefixes that the table does not name fails ``test_the_table_covers_every_route``.
Each route is then asked by six callers (P3.9.1): another member on a private item (404 for reads and writes), an
account with a view entry on the item's allowlist (200 for reads, 403 for writes), a moderator on a `global` item (200
for reads and unpublish, 403 for every control), the owner and an administrator (200), and an anonymous caller (401 or
404; a `global` item's reads 200 where the route serves visitors, its writes 401).

The other gates: every producer writes its row when its item is made through its route, and a delete marks it deleted
(gate 2), with system items and adoption; the control routes and queue priority (gate 3); benchmark games' rows (gate
4); a member's suite or probe subject that describes a machine instead of naming one (gate 5).
"""
from __future__ import annotations

import json
import os
import secrets
import shutil
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import tests  # noqa: F401
# before this module's own database below: importing it points the environment at that module's
from tests.test_seat_blocks import INLINE, SECRET_VAR, NoOutbound

_TMP = Path(tempfile.mkdtemp(prefix="citar_items_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from sqlalchemy import select

from citar import db, engine_api, items, lab, probes, settings
from citar.auth import access, accounts, policy, sessions
from citar.bots import profiles
from citar.db.models import AclEntry, AuditLog, Base, Game as GameRow, User

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()

CONFIG = {"map_size": "duel", "seed": 11, "barbarians": "off"}
BOTS = [{"type": "bot"}, {"type": "bot"}]
#: The callers of the table: handle, role, capabilities beyond the role's.
CALLERS = {"owner": ("olive", "user", {"edit_bots": True, "run_lab": True}), "stranger": ("stran", "user", {}),
           "reader": ("reader", "user", {}), "mod": ("maggie", "moderator", {}), "admin": ("zara", "admin", {})}

#: Every route under these prefixes is P3.2.4's, and must be in ROUTES.
PREFIXES = ("/api/benchmarks", "/api/maps", "/api/scenario-editor", "/api/scenario-ops", "/api/scenarios",
            "/api/probes", "/api/probe-runs", "/api/lab", "/api/bots", "/api/reports", "/api/queue", "/api/items",
            "/api/models/scores", "/api/metrics/compare")


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


class ItemCase(unittest.TestCase):
    """The app with five accounts, a benchmark scheduler, a probe runner and a report runner of their own that start
    nothing by themselves, and a factory of items of every kind."""

    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient
        from citar.reports import ReportRunner
        from citar.server import app as appmod, reports_api
        from citar.server.benchmarks import BenchmarkScheduler
        from citar.probes import ProbeRunner
        cls.app = appmod
        cls.manager = appmod.manager
        cls.dir = Path(tempfile.mkdtemp(prefix="citar_items_files_"))
        cls.patches = [mock.patch.object(BenchmarkScheduler, "tick", lambda self: None),
                       mock.patch.object(ProbeRunner, "_kick", lambda self: None),
                       mock.patch.object(ReportRunner, "_wake", lambda self: None)]
        for p in cls.patches:
            p.start()
        cls.saved = (appmod.scheduler, appmod._probe_runner, reports_api._runner)
        appmod.scheduler = BenchmarkScheduler(cls.manager, cls.dir / "bench", autostart=False)
        appmod._probe_runner = ProbeRunner(cls.manager)
        reports_api._runner = ReportRunner(cls.dir / "reports")
        cls.client = TestClient(appmod.app, base_url="https://citar.test")
        # one finished game's saves, for the routes that open a game (a benchmark job's, a probe case's)
        s = cls.manager.create(CONFIG, BOTS, "items fixture", start=False, track=False)
        cls.bench_game = s.id
        s.save("benchmark")
        cls.case_file = cls.dir / "case.citar"
        s.save_copy(cls.case_file)
        cls.manager.delete(s.id)
        # a scenario for probes to play on, and a game state to save scenarios from
        cls.scn_game = engine_api.EngineGame.new({**CONFIG, "players": [{"controller": "bot"}, {"controller": "bot"}]})
        cls.base_scenario = "items-base-" + secrets.token_hex(3)
        cls.scn_game.save_scenario(cls.base_scenario, "Items base")

    @classmethod
    def tearDownClass(cls):
        from citar.server import reports_api
        cls.app.scheduler, cls.app._probe_runner, reports_api._runner = cls.saved
        for p in reversed(cls.patches):
            p.stop()
        for sid in list(cls.manager.sessions):
            if sid == cls.bench_game or sid in getattr(cls, "_games", set()):
                cls.manager.delete(sid)
        cls.client.close()
        try:
            engine_api.delete_scenario(cls.base_scenario)
        except Exception:
            pass
        shutil.rmtree(cls.dir, ignore_errors=True)

    def setUp(self):
        _reset()
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for who, (handle, role, caps) in CALLERS.items():
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                user.caps = dict(caps)
                user.max_concurrent_games = 50
                row, raw = sessions.start(s, user)
                self.headers[who] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                     sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[who] = user.id
        self.headers["anonymous"] = {}
        self.made: list = []
        with db.session() as s:
            # as the server's start does: the shipped profiles, and the scenario the probes play on (the server's)
            items.register_system(s)
            items.register(s, "scenario", self.base_scenario, None, "Items base", visibility="global")

    def tearDown(self):
        for kind, ref in self.made:
            try:
                self._remove(kind, ref)
            except Exception:
                pass

    # ---------------------------------------------------------------- requests
    def call(self, who, method, path, body=None, **kw):
        return self.client.request(method, path, json=body, headers=self.headers[who], **kw)

    # ---------------------------------------------------------------- items
    def make(self, kind, visibility="private", owner="owner", share_with="reader"):
        """An item of ``kind`` on disk with its row: ``owner``'s, at ``visibility``, with a view entry for
        ``share_with``. Returns its id on disk."""
        ref = "t" + secrets.token_hex(4)
        path = getattr(self, "_make_" + kind)(ref, self.ids[owner])
        with db.session() as s:
            row = items.register(s, kind, ref, self.ids[owner], f"{kind} {ref}", path=path)
            row.visibility = visibility
            if share_with:
                access.grant(s, row, s.get(User, self.ids[share_with]), access.VIEW, actor=s.get(User, self.ids[owner]))
        self.made.append((kind, ref))
        return ref

    def _make_map(self, ref, owner_id):
        m = engine_api.blank_map(12, 10, "Grassland", ref)
        m["id"] = ref
        engine_api.save_map(m)
        return engine_api.map_file(ref)

    def _make_scenario(self, ref, owner_id):
        self.scn_game.save_scenario(ref, f"scenario {ref}")
        return engine_api.scenario_file(ref)

    def _make_benchmark_suite(self, ref, owner_id):
        from citar.server.benchmarks import normalize_suite
        sch = self.app._scheduler()
        sch.save_suite(normalize_suite({"id": ref, "name": f"suite {ref}",
                                        "servers": [{"server_id": "sv_dryrun", "models": [{"model_id": "m_dry"}]}]}))
        return sch._suite_path(ref)

    def _make_benchmark_run(self, ref, owner_id):
        sch = self.app._scheduler()
        now = time.time()
        job = {"id": "job1", "server_id": "sv_dryrun", "server_name": "Dry run", "server_key": "sv_dryrun",
               "model_id": "m1", "model": "dry-run", "profile": None, "label": "dry", "scenario_id": "sc1",
               "scenario_name": "Tiny", "repeat": 1, "seed": 1, "status": "queued", "pause_reason": None,
               "game_id": None, "created": now, "started": None, "finished": None, "error": None, "result": None,
               "progress": None}
        done = dict(job, id="job2", status="done", game_id=self.bench_game, finished=now)
        run = {"id": ref, "name": f"run {ref}", "suite_id": "x", "owner_id": owner_id, "status": "paused",
               "created": now, "started": now, "finished": None, "jobs": [job, done],
               "suite": {"id": "x", "name": "x", "mode": "sequential", "repeats": 1,
                         "servers": [{"id": "sv_dryrun", "server_id": "sv_dryrun", "name": "Dry run",
                                      "max_parallel": 1, "models": []}],
                         "scenarios": [{"id": "sc1", "name": "Tiny", "turn_limit": 3, "map_size": "duel",
                                        "map_type": "pangaea", "opponents": 1}]}}
        sch.runs[ref] = run
        sch._save_run(run)
        return sch.runs_dir / f"{ref}.json"

    def _make_probe(self, ref, owner_id):
        probes.save_probe({"id": ref, "name": f"probe {ref}", "scenario": self.base_scenario, "subject": 1,
                           "counterparty": 0, "cases": [{"id": "hello", "kind": "message", "message": "Hi."}]})
        return probes._path(ref)

    def _make_probe_run(self, ref, owner_id):
        d = probes.RUNS / ref
        d.mkdir(parents=True, exist_ok=True)
        (d / "run.json").write_text(json.dumps({
            "id": ref, "name": f"probe run {ref}", "probe": {"id": "p", "name": "p"}, "llm": {"provider": "bot"},
            "repeats": 1, "jobs": [], "status": "finished", "done": 0, "created": time.time(), "started": None,
            "finished": time.time(), "summary": {}, "error": None, "owner_id": owner_id}), encoding="utf-8")
        shutil.copy(self.case_file, d / "c1-0.citar")
        shutil.copy(self.case_file.with_suffix(".cjnl"), d / self.case_file.with_suffix(".cjnl").name)
        return d

    def _make_lab_experiment(self, ref, owner_id):
        lab.QUEUE.mkdir(parents=True, exist_ok=True)
        path = lab.QUEUE / f"{ref}.json"
        path.write_text(json.dumps({"name": ref, "games": 2, "priority": 0, "seats": [{"label": "a", "bot": "basic"}],
                                    "submitted": "2026-10-11T00:00:00", "owner_id": owner_id}), encoding="utf-8")
        return path

    def _make_bot_profile(self, ref, owner_id):
        profiles.save({"id": ref, "name": f"profile {ref}", "engine": "basic"})
        return profiles.PROFILES_DIR / f"{ref}.json"

    def _make_report(self, ref, owner_id):
        from citar.server import reports_api
        r = reports_api.reports_runner()
        d = r.dir / ref
        d.mkdir(parents=True, exist_ok=True)
        (d / "spec.json").write_text(json.dumps({"title": f"report {ref}", "narrative": {"enabled": False}}),
                                     encoding="utf-8")
        (d / "meta.json").write_text(json.dumps({"id": ref, "title": f"report {ref}", "status": "done",
                                                 "created": time.time(), "owner_id": owner_id}), encoding="utf-8")
        (d / "report.html").write_text("<!doctype html><title>r</title><p>secret figures</p>", encoding="utf-8")
        return d / "report.html"

    def _remove(self, kind, ref):
        if kind == "map":
            engine_api.delete_map(ref)
        elif kind == "scenario":
            engine_api.delete_scenario(ref)
        elif kind == "benchmark_suite":
            self.app._scheduler().delete_suite(ref)
        elif kind == "benchmark_run":
            sch = self.app._scheduler()
            sch.runs.pop(ref, None)
            (sch.runs_dir / f"{ref}.json").unlink(missing_ok=True)
        elif kind == "probe":
            probes.delete_probe(ref)
        elif kind == "probe_run":
            shutil.rmtree(probes.RUNS / ref, ignore_errors=True)
        elif kind == "lab_experiment":
            (lab.QUEUE / f"{ref}.json").unlink(missing_ok=True)
        elif kind == "bot_profile":
            (profiles.PROFILES_DIR / f"{ref}.json").unlink(missing_ok=True)
        elif kind == "report":
            from citar.server import reports_api
            shutil.rmtree(reports_api.reports_runner().dir / ref, ignore_errors=True)

    def row(self, kind, ref, deleted=False):
        with db.session() as s:
            row = items.get(s, kind, ref, deleted=deleted)
            if row is not None:
                s.expunge(row)
            return row


# ---------------------------------------------------------------------------- the table
#: The classes of gate (the expected status per caller is EXPECT's):
#:   read      VIEW on the item; a visitor is served a `global` one
#:   use       VIEW on the item plus an account (it starts work or loads a game), and a capability the reader has
#:   use_cap   VIEW plus a capability the reader lacks (edit_bots): the reader's 403 is the capability's
#:   write     MANAGE on the item
#:   unpublish MODERATE or MANAGE
#:   list      a listing filtered by visibility (EXPECT does not apply; ``test_listings_filter``)
#:   member    any signed-in account; admin: administrators only; cap: a capability; editor: the editing session's
#:   creator; create: making a new item (``test_producers``); priority: MANAGE through the body
ROUTES = {
    ("GET", "/api/benchmarks/status"): ("member", None, None),
    ("GET", "/api/benchmarks/settings"): ("member", None, None),
    ("PUT", "/api/benchmarks/settings"): ("admin", None, None),
    ("GET", "/api/benchmarks/suites"): ("list", "benchmark_suite", None),
    ("POST", "/api/benchmarks/suites"): ("create", "benchmark_suite", None),
    ("GET", "/api/benchmarks/suites/new"): ("member", None, None),
    ("GET", "/api/benchmarks/suites/{suite_id}"): ("read", "benchmark_suite", None),
    ("DELETE", "/api/benchmarks/suites/{suite_id}"): ("write", "benchmark_suite", None),
    ("POST", "/api/benchmarks/runs"): ("use", "benchmark_suite", "suite_id"),
    ("GET", "/api/benchmarks/runs"): ("list", "benchmark_run", None),
    ("POST", "/api/benchmarks/runs/{run_id}/control"): ("write", "benchmark_run", {"action": "pause"}),
    ("POST", "/api/benchmarks/runs/{run_id}/jobs/{job_id}/control"): ("write", "benchmark_run", {"action": "skip"}),
    ("POST", "/api/benchmarks/runs/{run_id}/jobs/{job_id}/watch"): ("use", "benchmark_run", None),
    ("GET", "/api/maps"): ("list", "map", None),
    ("GET", "/api/maps/{map_id}"): ("read", "map", None),
    ("PUT", "/api/maps/{map_id}"): ("write", "map", "map_body"),
    ("DELETE", "/api/maps/{map_id}"): ("write", "map", None),
    ("POST", "/api/maps/validate"): ("cap", None, None),
    ("POST", "/api/maps/generate"): ("cap", None, None),
    ("POST", "/api/scenario-editor"): ("create", "scenario", None),
    ("GET", "/api/scenario-editor/{eid}"): ("editor", None, None),
    ("DELETE", "/api/scenario-editor/{eid}"): ("editor", None, None),
    ("POST", "/api/scenario-editor/{eid}/ops"): ("editor", None, {"ops": []}),
    ("POST", "/api/scenario-editor/{eid}/undo"): ("editor", None, None),
    ("POST", "/api/scenario-editor/{eid}/meta"): ("editor", None, {"description": "d"}),
    ("POST", "/api/scenario-editor/{eid}/save"): ("editor", None, {"name": "saved"}),
    ("GET", "/api/scenario-ops"): ("member", None, None),
    ("GET", "/api/scenarios"): ("list", "scenario", None),
    ("GET", "/api/scenarios/{sid}"): ("read", "scenario", None),
    ("DELETE", "/api/scenarios/{sid}"): ("write", "scenario", None),
    ("POST", "/api/scenarios/{sid}/launch"): ("use", "scenario", {"name": "launched"}),
    ("GET", "/api/probes"): ("list", "probe", None),
    ("GET", "/api/probes/example"): ("member", None, None),
    ("POST", "/api/probes/validate"): ("cap", None, None),
    ("GET", "/api/probes/{pid}"): ("read", "probe", None),
    ("PUT", "/api/probes/{pid}"): ("write", "probe", "probe_body"),
    ("DELETE", "/api/probes/{pid}"): ("write", "probe", None),
    ("POST", "/api/probes/{pid}/run"): ("use", "probe", {"llm": {"provider": "bot"}}),
    ("GET", "/api/probe-runs"): ("list", "probe_run", None),
    ("GET", "/api/probe-runs/{rid}"): ("read", "probe_run", None),
    ("POST", "/api/probe-runs/{rid}/stop"): ("write", "probe_run", None),
    ("DELETE", "/api/probe-runs/{rid}"): ("write", "probe_run", None),
    ("POST", "/api/probe-runs/{rid}/open/{case}/{rep}"): ("use", "probe_run", None),
    ("GET", "/api/lab"): ("list", "lab_experiment", None),
    ("GET", "/api/lab/report/{name}"): ("read", "lab_experiment", None),
    ("GET", "/api/bots/profiles"): ("list", "bot_profile", None),
    ("POST", "/api/bots/profiles"): ("create", "bot_profile", None),
    ("GET", "/api/bots/profiles/{pid}"): ("read", "bot_profile", None),
    ("PUT", "/api/bots/profiles/{pid}"): ("write", "bot_profile", {"name": "renamed", "engine": "basic"}),
    ("DELETE", "/api/bots/profiles/{pid}"): ("write", "bot_profile", None),
    ("POST", "/api/bots/profiles/{pid}/fork"): ("use_cap", "bot_profile", {"name": "a fork"}),
    ("GET", "/api/bots/engines"): ("member", None, None),
    ("GET", "/api/bots/schema"): ("member", None, None),
    ("GET", "/api/bots/rankings"): ("member", None, None),
    ("POST", "/api/bots/experiments"): ("create", "lab_experiment", None),
    ("GET", "/api/reports"): ("list", "report", None),
    ("POST", "/api/reports"): ("create", "report", None),
    ("GET", "/api/reports/options"): ("member", None, None),
    ("GET", "/api/reports/{rid}"): ("read", "report", None),
    ("POST", "/api/reports/{rid}/rerun"): ("write", "report", None),
    ("GET", "/api/reports/{rid}/html"): ("read", "report", None),
    ("GET", "/api/reports/{rid}/download"): ("read", "report", None),
    ("DELETE", "/api/reports/{rid}"): ("write", "report", None),
    ("GET", "/api/queue"): ("member", None, None),
    ("POST", "/api/queue/priority"): ("priority", None, None),
    ("GET", "/api/models/scores"): ("member", None, None),
    ("GET", "/api/metrics/compare"): ("member", None, None),
    ("GET", "/api/items/{kind}/{ref}/sharing"): ("write", "map", None),
    ("PUT", "/api/items/{kind}/{ref}/sharing"): ("write", "map", {"visibility": "allowlist"}),
    ("POST", "/api/items/{kind}/{ref}/sharing/users"): ("write", "map", {"handle": "stran"}),
    ("DELETE", "/api/items/{kind}/{ref}/sharing/users/{handle}"): ("write", "map", None),
    ("POST", "/api/items/{kind}/{ref}/sharing/rotate"): ("write", "map", None),
    ("POST", "/api/items/{kind}/{ref}/unpublish"): ("unpublish", "map", {"reason": "spam"}),
}

#: The status each caller gets, per gate class, on a private item shared with the reader (a view entry). A set is
#: "any of these".
EXPECT = {
    "read": {"anonymous": {401, 404}, "stranger": {404}, "reader": {200}, "owner": {200}, "admin": {200}},
    "use": {"anonymous": {401, 404}, "stranger": {404}, "reader": {200}, "owner": {200}, "admin": {200}},
    "use_cap": {"anonymous": {401, 404}, "stranger": {404}, "reader": {403}, "owner": {200}, "admin": {200}},
    "write": {"anonymous": {401, 404}, "stranger": {404}, "reader": {403}, "owner": {200}, "admin": {200}},
    "unpublish": {"anonymous": {401, 404}, "stranger": {404}, "reader": {403}, "owner": {200}, "admin": {200}},
}
#: On a `global` item: a visitor reads where the route serves visitors and is refused every write; a moderator reads,
#: may unpublish, and is refused every control.
EXPECT_GLOBAL = {
    "read": {"anonymous": {200}, "mod": {200}},
    "use": {"anonymous": {401}, "mod": {200}},
    "use_cap": {"anonymous": {401}, "mod": {403}},
    "write": {"anonymous": {401}, "mod": {403}},
    "unpublish": {"anonymous": {401}, "mod": {200}},
}


class Table(ItemCase):
    def test_the_table_covers_every_route(self):
        """Every route of the app under an item prefix is in ROUTES, and ROUTES names no route that is not there."""
        import sys
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "scripts"))
        import audit_routes
        found = set()
        for route, path, _deps in audit_routes.all_routes(self.app.app):
            if not path.startswith(PREFIXES) or not hasattr(route, "methods"):
                continue
            for method in route.methods or ():
                if method != "HEAD":
                    found.add((method, path))
        self.assertEqual(sorted(found - set(ROUTES)), [], "routes the table does not cover")
        self.assertEqual(sorted(set(ROUTES) - found), [], "the table names routes the app does not have")

    # ---------------------------------------------------------------- the callers
    def _path(self, path, kind, ref):
        return (path.replace("{kind}", kind or "").replace("{ref}", ref)
                .replace("{suite_id}", ref).replace("{run_id}", ref).replace("{map_id}", ref)
                .replace("{sid}", ref).replace("{pid}", ref).replace("{rid}", ref).replace("{name}", ref)
                .replace("{handle}", "reader").replace("{case}", "c1").replace("{rep}", "0"))

    def _job(self, path):
        if "/jobs/{job_id}/control" in path:
            return path.replace("{job_id}", "job1")
        return path.replace("{job_id}", "job2")

    def _body(self, spec, ref):
        if spec == "map_body":
            m = engine_api.blank_map(12, 10, "Grassland", "replaced")
            m["id"] = ref
            return m
        if spec == "probe_body":
            return {"name": "replaced", "scenario": self.base_scenario, "subject": 1, "counterparty": 0,
                    "cases": [{"id": "hi", "kind": "message", "message": "Hi."}]}
        if spec == "suite_id":
            return {"suite_id": ref}
        return spec

    def _ask(self, who, method, path, kind, ref, body):
        url = self._job(self._path(path, kind, ref))
        r = self.call(who, method, url, self._body(body, ref))
        launched = r.json().get("id") if r.status_code == 200 and path.endswith("/launch") else None
        if launched:
            self.manager.delete(launched)
        return r

    def _check(self, gate, expect, visibility):
        for (method, path), (cls, kind, body) in ROUTES.items():
            if cls != gate:
                continue
            for who, want in expect[gate].items():
                # a write is asked of a fresh item each time, as it may delete or change it
                ref = self.make(kind, visibility)
                with self.subTest(route=f"{method} {path}", caller=who, visibility=visibility):
                    r = self._ask(who, method, path, kind, ref, body)
                    self.assertIn(r.status_code, want, r.text[:300])

    def test_reads(self):
        self._check("read", EXPECT, "private")

    def test_uses(self):
        self._check("use", EXPECT, "private")
        self._check("use_cap", EXPECT, "private")

    def test_writes(self):
        self._check("write", EXPECT, "private")

    def test_unpublish(self):
        self._check("unpublish", EXPECT, "private")

    def test_a_global_item(self):
        """A `global` item's reads serve visitors, its writes refuse them (401); a moderator reads it and may take it
        down, and nothing else."""
        for gate in ("read", "use", "use_cap", "write", "unpublish"):
            self._check(gate, EXPECT_GLOBAL, "global")

    def test_every_kind_has_the_sharing_routes(self):
        """The sharing routes take every kind: a stranger 404, the reader 403, the owner and an administrator 200; an
        unknown kind is a 404."""
        for kind in items.KINDS:
            ref = self.make(kind)
            with self.subTest(kind=kind):
                path = f"/api/items/{kind}/{ref}/sharing"
                self.assertEqual(self.call("stranger", "GET", path).status_code, 404)
                self.assertEqual(self.call("reader", "GET", path).status_code, 403)
                self.assertEqual(self.call("anonymous", "GET", path).status_code, 401)
                for who in ("owner", "admin"):
                    r = self.call(who, "GET", path)
                    self.assertEqual(r.status_code, 200, r.text)
                    self.assertEqual(r.json()["owner"]["handle"], "olive")
        self.assertEqual(self.call("owner", "GET", "/api/items/game/x/sharing").status_code, 404)

    def test_member_admin_and_capability_routes(self):
        bodies = {("POST", "/api/maps/validate"): engine_api.blank_map(12, 10, "Grassland", "v"),
                  ("POST", "/api/maps/generate"): {"map_size": "duel", "seed": 1},
                  ("POST", "/api/probes/validate"): {"scenario": self.base_scenario, "subject": 1, "counterparty": 0,
                                                     "cases": [{"id": "a", "kind": "turn"}]},
                  ("PUT", "/api/benchmarks/settings"): {}}
        params = {"/api/probes/example": {"scenario": "x"}}
        for (method, path), (cls, _kind, _body) in ROUTES.items():
            if cls not in ("member", "admin", "cap"):
                continue
            body = bodies.get((method, path))
            with self.subTest(route=f"{method} {path}"):
                self.assertIn(self.call("anonymous", method, path, body, params=params.get(path)).status_code, (401,))
                r = self.call("stranger", method, path, body, params=params.get(path))
                self.assertEqual(r.status_code, 403 if cls == "admin" else 200, r.text[:300])
                r = self.call("admin", method, path, body, params=params.get(path))
                self.assertEqual(r.status_code, 200, r.text[:300])
        with db.session() as s:
            s.get(User, self.ids["stranger"]).caps = {"create_games": False, "run_probes": False}
        for path, body in (("/api/maps/validate", bodies[("POST", "/api/maps/validate")]),
                           ("/api/maps/generate", {"map_size": "duel"}),
                           ("/api/probes/validate", bodies[("POST", "/api/probes/validate")])):
            with self.subTest(without_the_capability=path):
                self.assertEqual(self.call("stranger", "POST", path, body).status_code, 403)

    def test_the_scenario_editor_is_its_creators(self):
        """An editing session is its creator's (and an administrator's): another member gets 404 on each of its routes,
        as for one that has expired."""
        r = self.call("owner", "POST", "/api/scenario-editor", {"source": "generate", "config": CONFIG,
                                                               "players": [{"type": "bot"}, {"type": "bot"}]})
        self.assertEqual(r.status_code, 200, r.text)
        eid = r.json()["editor_id"]
        editor_routes = [(m, p) for (m, p), (cls, _k, _b) in ROUTES.items() if cls == "editor"]
        for method, path in editor_routes:
            body = ROUTES[(method, path)][2]
            url = path.replace("{eid}", eid)
            with self.subTest(route=f"{method} {path}"):
                self.assertEqual(self.call("anonymous", method, url, body).status_code, 401)
                self.assertEqual(self.call("stranger", method, url, body).status_code, 404)
                self.assertEqual(self.call("reader", method, url, body).status_code, 404)
        for method, path in (("GET", "/api/scenario-editor/{eid}"), ("POST", "/api/scenario-editor/{eid}/meta")):
            for who in ("owner", "admin"):
                with self.subTest(route=f"{method} {path}", caller=who):
                    r = self.call(who, method, path.replace("{eid}", eid), ROUTES[(method, path)][2])
                    self.assertEqual(r.status_code, 200, r.text[:300])
        self.assertEqual(self.call("admin", "DELETE", f"/api/scenario-editor/{eid}").status_code, 200)

    def test_listings_filter(self):
        """Each listing shows a member their own items, those shared with them and `server` and `global` ones, never
        another member's private or `link` one; an administrator sees everything; a visitor `global` ones where the
        listing serves visitors."""
        listings = {"benchmark_suite": ("/api/benchmarks/suites", lambda j: [x["id"] for x in j]),
                    "benchmark_run": ("/api/benchmarks/runs", lambda j: [x["id"] for x in j]),
                    "map": ("/api/maps", lambda j: [x["id"] for x in j]),
                    "scenario": ("/api/scenarios", lambda j: [x["id"] for x in j]),
                    "probe": ("/api/probes", lambda j: [x["id"] for x in j]),
                    "probe_run": ("/api/probe-runs", lambda j: [x["id"] for x in j["runs"]]),
                    "lab_experiment": ("/api/lab", lambda j: [x["name"] for x in j["experiments"]]),
                    "bot_profile": ("/api/bots/profiles", lambda j: [x["id"] for x in j["profiles"]]),
                    "report": ("/api/reports", lambda j: [x["id"] for x in j])}
        self.assertEqual(set(listings), set(items.KINDS))
        members_only = {"benchmark_run", "lab_experiment"}
        for kind, (path, ids) in listings.items():
            refs = {v: self.make(kind, v, share_with=None) for v in access.VISIBILITY}
            shared = self.make(kind, "private", share_with="reader")
            mine = self.make(kind, "private", owner="stranger", share_with=None)
            with self.subTest(kind=kind):
                for who, want in (("stranger", {refs["server"], refs["global"], mine}),
                                  ("reader", {refs["server"], refs["global"], shared}),
                                  ("owner", set(refs.values()) | {shared}),
                                  ("admin", set(refs.values()) | {shared, mine})):
                    r = self.call(who, "GET", path)
                    self.assertEqual(r.status_code, 200, r.text[:300])
                    got = set(ids(r.json())) & (set(refs.values()) | {shared, mine})
                    self.assertEqual(got, want, f"{who} on {path}")
                r = self.call("anonymous", "GET", path)
                if kind in members_only:
                    self.assertEqual(r.status_code, 401)
                else:
                    self.assertEqual(r.status_code, 200, r.text[:300])
                    self.assertEqual(set(ids(r.json())) & set(refs.values()), {refs["global"]})


class Priority(ItemCase):
    def test_queue_priority_is_the_managers(self):
        """Gate 3's priority: a run's, a probe run's, a report's and an experiment's by their owner and an
        administrator, refused to a stranger (404) and the reader (403); a lobby game's only by its owner (and an
        administrator)."""
        for kind, item in (("benchmark", "benchmark_run"), ("probe", "probe_run"), ("report", "report"),
                           ("lab", "lab_experiment")):
            ref = self.make(item)
            for who, want in (("stranger", 404), ("reader", 403), ("anonymous", 401), ("owner", 200), ("admin", 200)):
                with self.subTest(kind=kind, caller=who):
                    r = self.call(who, "POST", "/api/queue/priority", {"kind": kind, "id": ref, "priority": 5})
                    self.assertEqual(r.status_code, want, r.text[:300])
        r = self.call("owner", "POST", "/api/games", {"name": "mine", "config": CONFIG, "seats": BOTS})
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        try:
            for who, want in (("stranger", 404), ("anonymous", 401), ("owner", 200), ("admin", 200)):
                with self.subTest(kind="game", caller=who):
                    r = self.call(who, "POST", "/api/queue/priority", {"kind": "game", "id": gid, "priority": 3})
                    self.assertEqual(r.status_code, want, r.text[:300])
        finally:
            self.manager.delete(gid)

    def test_the_queue_names_only_what_the_caller_may_see(self):
        """Another member's queued run is "someone's benchmark run" on the Queue page, with no name and no priority
        controls; the owner sees it by name with them."""
        ref = self.make("benchmark_run", share_with=None)
        name = f"run {ref}"
        with mock.patch("citar.pool.queue.waiting", lambda *a, **k: [
                {"kind": "benchmark", "id": "job1", "group": ref, "run": name, "label": name, "server_id": "sv_dryrun",
                 "server": "Dry run", "created": 1, "waiting": f"waiting: {name} first", "priority": 0}]):
            mine = self.call("owner", "GET", "/api/queue").json()
            theirs = self.call("stranger", "GET", "/api/queue").json()
        entry = next(m for m in mine["machines"] if m["server_id"] == "sv_dryrun")["waiting"][0]
        self.assertEqual((entry["label"], entry["can_manage"]), (name, True))
        entry = next(m for m in theirs["machines"] if m["server_id"] == "sv_dryrun")["waiting"][0]
        self.assertEqual((entry["label"], entry["can_manage"], entry["group"]), ("someone's benchmark run", False, None))
        self.assertNotIn(ref, json.dumps(theirs))


class Producers(ItemCase):
    """Gate 2: an item made through its route has a row with the caller as its owner; a delete marks it deleted;
    listings filter (``Table.test_listings_filter``)."""

    def _owned(self, kind, ref, who="owner"):
        row = self.row(kind, ref)
        self.assertIsNotNone(row, f"no row for {kind} {ref}")
        self.assertEqual((row.owner_id, row.visibility), (self.ids[who], "private"))
        self.made.append((kind, ref))
        return row

    def _deleted(self, kind, ref):
        self.assertIsNone(self.row(kind, ref))
        self.assertIsNotNone(self.row(kind, ref, deleted=True).deleted_at)

    def test_maps(self):
        m = engine_api.blank_map(12, 10, "Grassland", "Mine")
        r = self.call("stranger", "PUT", "/api/maps/my-map", m)
        self.assertEqual(r.status_code, 200, r.text)
        self._owned("map", "my-map", "stranger")
        # another member's save under that id is refused before anything is written, and the map is unchanged
        r = self.call("owner", "PUT", "/api/maps/my-map", engine_api.blank_map(12, 10, "Desert", "Theirs"))
        self.assertEqual(r.status_code, 404, r.text)
        self.assertEqual(engine_api.load_map("my-map")["name"], "Mine")
        self.assertEqual(self.call("stranger", "DELETE", "/api/maps/my-map").status_code, 200)
        self._deleted("map", "my-map")
        # an id whose map was deleted is a new map, the next maker's, with nothing of the old sharing
        r = self.call("owner", "PUT", "/api/maps/my-map", m)
        self.assertEqual(r.status_code, 200, r.text)
        self._owned("map", "my-map", "owner")

    def test_scenarios(self):
        r = self.call("owner", "POST", "/api/scenario-editor", {"source": "generate", "config": CONFIG,
                                                               "players": [{"type": "bot"}, {"type": "bot"}]})
        eid = r.json()["editor_id"]
        r = self.call("owner", "POST", f"/api/scenario-editor/{eid}/save", {"name": "My scenario"})
        self.assertEqual(r.status_code, 200, r.text)
        sid = r.json()["id"]
        self._owned("scenario", sid)
        # an editor of somebody else's saves under its own id only when that id is theirs
        r = self.call("stranger", "POST", "/api/scenario-editor", {"source": "generate", "config": CONFIG,
                                                                  "players": [{"type": "bot"}, {"type": "bot"}]})
        eid2 = r.json()["editor_id"]
        self.assertEqual(self.call("stranger", "POST", f"/api/scenario-editor/{eid2}/save",
                                   {"id": sid, "name": "Overwrite"}).status_code, 404)
        self.assertEqual(self.call("stranger", "DELETE", f"/api/scenarios/{sid}").status_code, 404)
        self.assertEqual(self.call("owner", "DELETE", f"/api/scenarios/{sid}").status_code, 200)
        self._deleted("scenario", sid)

    def test_benchmark_suites_and_runs(self):
        new = self.call("owner", "GET", "/api/benchmarks/suites/new").json()
        new["servers"] = [{"server_id": "sv_dryrun", "models": [{"model_id": "m_dry"}]}]
        r = self.call("owner", "POST", "/api/benchmarks/suites", new)
        self.assertEqual(r.status_code, 200, r.text)
        suite = r.json()["id"]
        self._owned("benchmark_suite", suite)
        r = self.call("owner", "POST", "/api/benchmarks/runs", {"suite_id": suite})
        self.assertEqual(r.status_code, 200, r.text)
        run = r.json()["id"]
        self._owned("benchmark_run", run)
        self.assertEqual(self.app._scheduler().runs[run]["owner_id"], self.ids["owner"])
        self.app._scheduler().control_run(run, "cancel")
        self.assertEqual(self.call("owner", "POST", f"/api/benchmarks/runs/{run}/control",
                                   {"action": "remove"}).status_code, 200)
        self._deleted("benchmark_run", run)
        self.assertEqual(self.call("owner", "DELETE", f"/api/benchmarks/suites/{suite}").status_code, 200)
        self._deleted("benchmark_suite", suite)

    def test_probes_and_probe_runs(self):
        body = {"name": "My probe", "scenario": self.base_scenario, "subject": 1, "counterparty": 0,
                "cases": [{"id": "hi", "kind": "message", "message": "Hi."}]}
        r = self.call("owner", "PUT", "/api/probes/my-probe", body)
        self.assertEqual(r.status_code, 200, r.text)
        self._owned("probe", "my-probe")
        r = self.call("owner", "POST", "/api/probes/my-probe/run", {"llm": {"provider": "bot"}})
        self.assertEqual(r.status_code, 200, r.text)
        rid = r.json()["id"]
        self._owned("probe_run", rid)
        self.assertEqual(probes.ProbeRunner.get_run(self.app._probes(), rid)["owner_id"], self.ids["owner"])
        self.app._probes().stop(rid)
        self.assertEqual(self.call("owner", "DELETE", f"/api/probe-runs/{rid}").status_code, 200)
        self._deleted("probe_run", rid)
        self.assertEqual(self.call("owner", "DELETE", "/api/probes/my-probe").status_code, 200)
        self._deleted("probe", "my-probe")

    def test_a_probe_on_a_scenario_the_caller_cannot_see_is_refused(self):
        private = self.make("scenario", share_with=None)
        body = {"name": "Peek", "scenario": private, "subject": 1, "counterparty": 0,
                "cases": [{"id": "hi", "kind": "turn"}]}
        self.assertEqual(self.call("stranger", "PUT", "/api/probes/peek", body).status_code, 404)
        self.assertEqual(self.call("stranger", "POST", "/api/probes/validate", body).status_code, 404)
        self.assertIsNone(self.row("probe", "peek"))
        self.assertFalse(probes._path("peek").exists())

    def test_bot_profiles_and_lab_experiments(self):
        r = self.call("owner", "POST", "/api/bots/profiles", {"name": "My bot", "engine": "basic"})
        self.assertEqual(r.status_code, 200, r.text)
        pid = r.json()["id"]
        self._owned("bot_profile", pid)
        r = self.call("owner", "POST", "/api/bots/profiles/standard/fork", {"name": "Standard mine"})
        self.assertEqual(r.status_code, 200, r.text)
        self._owned("bot_profile", r.json()["id"])
        # a member without edit_bots creates none; with run_lab they queue an experiment, theirs
        self.assertEqual(self.call("stranger", "POST", "/api/bots/profiles", {"name": "x"}).status_code, 403)
        r = self.call("owner", "POST", "/api/bots/experiments", {"profiles": ["standard", pid], "games": 2})
        self.assertEqual(r.status_code, 200, r.text)
        self._owned("lab_experiment", r.json()["name"])
        self.assertEqual(self.call("stranger", "POST", "/api/bots/experiments",
                                   {"profiles": ["standard"], "games": 2}).status_code, 403)
        # another member's private profile is not theirs to play
        with db.session() as s:
            s.get(User, self.ids["stranger"]).caps = {"run_lab": True}
        self.assertEqual(self.call("stranger", "POST", "/api/bots/experiments",
                                   {"profiles": [pid], "games": 2}).status_code, 404)
        self.assertEqual(self.call("owner", "DELETE", f"/api/bots/profiles/{pid}").status_code, 200)
        self._deleted("bot_profile", pid)

    def test_reports(self):
        r = self.call("owner", "POST", "/api/reports", {"title": "Mine", "sections": ["summary"]})
        self.assertEqual(r.status_code, 200, r.text)
        rid = r.json()["id"]
        row = self._owned("report", rid)
        self.assertEqual(row.meta["spec"]["title"], "Mine")
        from citar.server import reports_api
        self.assertEqual(reports_api.reports_runner().meta(rid)["owner_id"], self.ids["owner"])
        with db.session() as s:
            s.get(User, self.ids["stranger"]).caps = {"run_reports": False}
        self.assertEqual(self.call("stranger", "POST", "/api/reports", {"title": "x"}).status_code, 403)
        self.assertEqual(self.call("owner", "DELETE", f"/api/reports/{rid}").status_code, 200)
        self._deleted("report", rid)

    def test_a_game_on_a_map_or_with_a_profile_the_caller_cannot_see_is_refused(self):
        m = self.make("map", share_with=None)
        p = self.make("bot_profile", share_with=None)
        for body in ({"config": {**CONFIG, "map": m}, "seats": BOTS},
                     {"config": CONFIG, "seats": [{"type": "bot", "bot": {"profile": p}}, {"type": "bot"}]}):
            with self.subTest(body=body):
                self.assertEqual(self.call("stranger", "POST", "/api/games", body).status_code, 404)


class SystemItemsAndAdoption(ItemCase):
    def test_system_items_are_global_and_the_servers(self):
        with db.session() as s:
            items.register_system(s)
        row = self.row("bot_profile", "standard")
        self.assertEqual((row.owner_id, row.visibility), (None, "global"))
        self.assertEqual(self.call("anonymous", "GET", "/api/bots/profiles/standard").status_code, 200)
        self.assertEqual(self.call("stranger", "PUT", "/api/items/bot_profile/standard/sharing",
                                   {"visibility": "private"}).status_code, 403)
        self.assertEqual(self.call("owner", "DELETE", "/api/bots/profiles/standard").status_code, 403)
        r = self.call("admin", "DELETE", "/api/bots/profiles/standard")
        self.assertEqual(r.status_code, 400, "the shipped profiles cannot be deleted, by anyone")

    def _stray(self):
        """A map file with no row, as a crash between the two or a hand copy leaves one."""
        ref = "stray-" + secrets.token_hex(3)
        self._make_map(ref, None)
        self.made.append(("map", ref))
        return ref

    def test_a_stray_file_is_adopted_by_the_first_administrator(self):
        ref = self._stray()
        for who in ("owner", "admin"):
            self.assertEqual(self.call(who, "GET", f"/api/maps/{ref}").status_code, 404, "nobody's until adopted")
        with db.session() as s:
            counts = items.adopt_orphans(s, items.first_admin(s))
        self.assertEqual(counts.get("map"), 1)
        row = self.row("map", ref)
        self.assertEqual((row.owner_id, row.visibility), (self.ids["admin"], "private"))
        with db.session() as s:
            audits = list(s.scalars(select(AuditLog).where(AuditLog.action == "migration.items_adopted")))
            self.assertEqual(len(audits), 1)
            self.assertIsNotNone(policy.marker(s, "orphans_adopted"))
        self.assertEqual(self.call("admin", "GET", f"/api/maps/{ref}").status_code, 200)
        self.assertEqual(self.call("owner", "GET", f"/api/maps/{ref}").status_code, 404)
        with db.session() as s:
            self.assertEqual(items.adopt_orphans(s, items.first_admin(s)), {}, "adopting twice adopts nothing")

    def test_a_file_that_names_its_owner_goes_back_to_them(self):
        """A run's file records its owner: adopted, it is theirs, not the administrator's; a report's too."""
        from citar import paths
        ref = "t" + secrets.token_hex(4)
        folder = paths.bench_path("runs")
        folder.mkdir(parents=True, exist_ok=True)
        (folder / f"{ref}.json").write_text(json.dumps({"id": ref, "name": "theirs", "owner_id": self.ids["owner"]}),
                                            encoding="utf-8")
        report = "t" + secrets.token_hex(4)
        from citar.reports import REPORTS
        (REPORTS / report).mkdir(parents=True, exist_ok=True)
        (REPORTS / report / "meta.json").write_text(json.dumps({"id": report, "title": "theirs",
                                                                "owner_id": self.ids["stranger"]}), encoding="utf-8")
        try:
            with db.session() as s:
                items.adopt_orphans(s, items.first_admin(s))
            self.assertEqual(self.row("benchmark_run", ref).owner_id, self.ids["owner"])
            self.assertEqual(self.row("report", report).owner_id, self.ids["stranger"])
        finally:
            (folder / f"{ref}.json").unlink(missing_ok=True)
            shutil.rmtree(REPORTS / report, ignore_errors=True)

    def test_a_fresh_server_database_waits_for_create_admin(self):
        """On a fresh server-mode database the boot adopts nothing (no administrator) and the stray file's routes are
        404 for everyone; ``admin_cli create-admin`` adopts it, with one audit row."""
        from citar.server import admin_cli
        with db.session() as s:
            for user in s.scalars(select(User).where(User.role == "admin")):
                user.role = "user"
        ref = self._stray()
        self.assertIsNone(items.on_boot(), "no administrator: nothing adopted")
        self.assertIsNone(self.row("map", ref))
        for who in ("owner", "admin"):
            self.assertEqual(self.call(who, "GET", f"/api/maps/{ref}").status_code, 404)
        with mock.patch.object(admin_cli, "_prompt_password", return_value="a long enough phrase"):
            code = admin_cli.cmd_create_admin(SimpleNamespace(handle="firstadmin", email="first@example.com",
                                                              name="", force=False))
        self.assertEqual(code, 0)
        with db.session() as s:
            first = accounts.by_handle(s, "firstadmin")
            row = items.get(s, "map", ref)
            self.assertEqual((row.owner_id, row.visibility), (first.id, "private"))
            self.assertEqual(len(list(s.scalars(select(AuditLog).where(
                AuditLog.action == "migration.items_adopted")))), 1)
            self.assertEqual(items.get(s, "bot_profile", "standard").visibility, "global")

    def test_set_role_to_admin_adopts_too(self):
        from citar.server import admin_cli
        with db.session() as s:
            for user in s.scalars(select(User).where(User.role == "admin")):
                user.role = "user"
        ref = self._stray()
        self.assertEqual(admin_cli.cmd_set_role(SimpleNamespace(handle="olive", role="admin")), 0)
        self.assertEqual(self.row("map", ref).owner_id, self.ids["owner"])

    def test_a_deleted_items_id_is_a_new_item(self):
        ref = self.make("map", "allowlist", share_with="reader")
        with db.session() as s:
            old_slug = items.get(s, "map", ref).slug
            items.delete(s, "map", ref)
            row = items.register(s, "map", ref, self.ids["stranger"], "again")
            self.assertEqual((row.owner_id, row.visibility), (self.ids["stranger"], "private"))
            self.assertNotEqual(row.slug, old_slug)
            self.assertEqual(list(s.scalars(select(AclEntry).where(AclEntry.object_id == row.id))), [])


class BenchmarkGames(ItemCase):
    def test_benchmark_games_are_the_runs_owners(self):
        """Gate 4: a benchmark job's game has a row of kind `benchmark` with the run's owner and visibility, seen by the
        run's owner and not by another member; a run made `server` shows its games to members, and taking the run
        down takes its games down too."""
        from citar.server.benchmarks import BenchmarkScheduler
        sch = BenchmarkScheduler(self.manager, self.dir / "bench-real", autostart=False)
        saved = self.app.scheduler
        self.app.scheduler = sch
        suite = {"name": "Owned", "servers": [{"server_id": "sv_dryrun", "models": [{"model_id": "m_dry"}]}],
                 "scenarios": [{"name": "Tiny", "map_size": "duel", "map_type": "pangaea", "seed": 5, "opponents": 1,
                                "turn_limit": 2, "barbarians": "off"}]}
        try:
            r = self.call("owner", "POST", "/api/benchmarks/runs", {"suite": suite})
            self.assertEqual(r.status_code, 200, r.text)
            run_id = r.json()["id"]
            self.made.append(("benchmark_run", run_id))
            run = sch.runs[run_id]
            job = run["jobs"][0]
            job["status"] = "loading"
            sch._launch_job(run_id, job["id"])
            self.assertEqual(job["status"], "running", job.get("error"))
            gid = job["game_id"]
            with db.session() as s:
                row = s.get(GameRow, gid)
                self.assertEqual((row.kind, row.owner_id, row.visibility), ("benchmark", self.ids["owner"], "private"))
            self.assertEqual(self.manager.get(gid).owner_id, self.ids["owner"])
            listed = lambda who: {g["id"] for g in self.call(who, "GET", "/api/games").json()}  # noqa: E731
            self.assertIn(gid, listed("owner"))
            self.assertNotIn(gid, listed("stranger"))
            self.assertEqual(self.call("stranger", "GET", f"/api/games/{gid}").status_code, 404)
            r = self.call("owner", "PUT", f"/api/items/benchmark_run/{run_id}/sharing", {"visibility": "server"})
            self.assertEqual(r.status_code, 200, r.text)
            self.assertIn(gid, listed("stranger"))
            r = self.call("owner", "POST", f"/api/items/benchmark_run/{run_id}/unpublish", {"reason": "done"})
            self.assertEqual(r.status_code, 200, r.text)
            self.assertNotIn(gid, listed("stranger"))
        finally:
            for run in sch.runs.values():
                for j in run["jobs"]:
                    if j.get("game_id") and self.manager.get(j["game_id"]):
                        self.manager.delete(j["game_id"])
            self.app.scheduler = saved


class ModelBlocks(ItemCase):
    """Gate 5 (P3.2.9): a member's suite or probe subject that describes a machine (a base URL, a key, a key
    variable) instead of naming one is refused, and nothing is dialled or read while it is."""

    def setUp(self):
        super().setUp()
        self.watch = NoOutbound().__enter__()

    def tearDown(self):
        self.watch.__exit__(None, None, None)
        super().tearDown()
        self.assertEqual(self.watch.calls, [], "something was dialled or a key variable read")

    def test_a_suite_that_describes_a_machine_is_refused(self):
        scenarios = [{"name": "Tiny", "map_size": "duel", "map_type": "pangaea", "opponents": 1, "turn_limit": 2}]
        for name, group in (("base_url", {"server_id": "sv_dryrun", "base_url": "http://127.0.0.1:9/v1",
                                          "models": [{"model_id": "m_dry"}]}),
                            ("api_key_env", {"server_id": "sv_dryrun",
                                             "models": [{"model_id": "m_dry", "api_key_env": SECRET_VAR}]}),
                            ("no machine", {"models": [{"model": "x"}]}),
                            ("registry gpu", {"server_id": "sv_api", "models": [{"model_id": "m_opus"}]})):
            suite = {"name": f"bad {name}", "servers": [group], "scenarios": scenarios}
            with self.subTest(block=name):
                r = self.call("owner", "POST", "/api/benchmarks/suites", suite)
                self.assertIn(r.status_code, (400, 403), r.text)
                r = self.call("owner", "POST", "/api/benchmarks/runs", {"suite": suite})
                self.assertIn(r.status_code, (400, 403), r.text)
        good = {"name": "good", "servers": [{"server_id": "sv_dryrun", "models": [{"model_id": "m_dry"}]}],
                "scenarios": scenarios}
        r = self.call("owner", "POST", "/api/benchmarks/suites", good)
        self.assertEqual(r.status_code, 200, r.text)
        self.made.append(("benchmark_suite", r.json()["id"]))
        r = self.call("admin", "POST", "/api/benchmarks/suites",
                      {**good, "name": "admins", "servers": [{"server_id": "sv_api", "models": [{"model_id": "m_opus"}]}]})
        self.assertEqual(r.status_code, 200, "an administrator's suite may name any machine")
        self.made.append(("benchmark_suite", r.json()["id"]))

    def test_a_probe_subject_that_describes_a_machine_is_refused(self):
        pid = self.make("probe")
        for name, llm in INLINE.items():
            with self.subTest(block=name):
                r = self.call("owner", "POST", f"/api/probes/{pid}/run", {"llm": llm})
                self.assertIn(r.status_code, (400, 403), r.text)
        r = self.call("owner", "POST", f"/api/probes/{pid}/run", {"llm": {"server_id": "sv_api", "model": "m"}})
        self.assertEqual(r.status_code, 403, r.text)
        r = self.call("owner", "POST", f"/api/probes/{pid}/run", {"llm": {"server_id": "sv_dryrun", "model": "dry-run"}})
        self.assertEqual(r.status_code, 200, r.text)
        self.made.append(("probe_run", r.json()["id"]))
        self.assertEqual(r.json()["llm"], {"server_id": "sv_dryrun", "model": "dry-run"})


if __name__ == "__main__":
    unittest.main()

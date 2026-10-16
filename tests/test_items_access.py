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

import gzip
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

import tests
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

from sqlalchemy import event, func, select

from citar import db, engine_api, items, lab, probes, settings
from citar.auth import access, accounts, policy, sessions
from citar.bots import profiles
from citar.db.models import AclEntry, AuditLog, Base, Game as GameRow, OwnedItem, Server, ServerGroup, User

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
            "/api/models/scores", "/api/models/diplomacy", "/api/metrics/compare")


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    tests.share_with_members("sv_dryrun")         # the site's dry run, as members' suites and probes name it
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
#:   creator; create: making a new item (``test_producers``); priority: MANAGE through the body; ticket: a pooled
#:   prompt's work's owner, the machine's owner and administrators (``tests/test_pooled_work.py``'s ``QueuePage``)
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
    ("GET", "/api/lab/jobs"): ("read", "lab_experiment", None),       # ?exp=NAME (``_path``)
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
    ("POST", "/api/queue/tickets/{tid}/cancel"): ("ticket", None, None),
    ("GET", "/api/models/scores"): ("member", None, None),
    ("GET", "/api/models/diplomacy"): ("member", None, None),
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
        if path == "/api/lab/jobs":
            return f"{path}?exp={ref}"         # the experiment in the query: P4.5.10's ``GET /api/lab/jobs?exp=NAME``
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

    def test_a_share_key_lists_its_item_and_nothing_else(self):
        """3-03: a listing asked with a share key (the page a link opens) shows the one `link` item the key opens, to
        whoever holds it, as something they may see and not change; another item's key, a private item's key or no
        key shows nothing more; the owner and an administrator list it anyway (P3.9.1 for the changed listings)."""
        listings = {"benchmark_suite": ("/api/benchmarks/suites", lambda j: j),
                    "benchmark_run": ("/api/benchmarks/runs", lambda j: j),
                    "map": ("/api/maps", lambda j: j),
                    "scenario": ("/api/scenarios", lambda j: j),
                    "probe": ("/api/probes", lambda j: j),
                    "probe_run": ("/api/probe-runs", lambda j: j["runs"]),
                    "lab_experiment": ("/api/lab", lambda j: [dict(x, id=x["name"]) for x in j["experiments"]]),
                    "bot_profile": ("/api/bots/profiles", lambda j: j["profiles"]),
                    "report": ("/api/reports", lambda j: j)}
        self.assertEqual(set(listings), set(items.KINDS))
        members_only = {"benchmark_run", "lab_experiment"}
        for kind, (path, entries) in listings.items():
            linked = self.make(kind, "link", share_with=None)
            other = self.make(kind, "link", share_with=None)
            private = self.make(kind, "private", share_with=None)
            key, private_key = self.row(kind, linked).slug, self.row(kind, private).slug
            mine = {linked, other, private}

            def listed(who, k=None, path=path, entries=entries, mine=mine):
                r = self.client.get(path, headers={**self.headers[who], **({"x-citar-share-key": k} if k else {})})
                self.assertEqual(r.status_code, 200, f"{who} on {path}: {r.text[:300]}")
                return {e["id"]: e for e in entries(r.json()) if e["id"] in mine}

            with self.subTest(kind=kind):
                self.assertEqual(set(listed("stranger")), set(), "unlisted without the key")
                got = listed("stranger", key)
                self.assertEqual(set(got), {linked}, "the key lists its own item only")
                self.assertTrue(got[linked]["can_view"])
                self.assertFalse(got[linked]["can_manage"], "holding the link is seeing, not managing")
                self.assertEqual(set(listed("stranger", private_key)), set(), "a private item's key opens nothing")
                self.assertTrue(listed("owner", key)[linked]["can_manage"])
                self.assertEqual(set(listed("admin")), mine)
                r = self.client.get(path, headers={"x-citar-share-key": key})
                if kind in members_only:
                    self.assertEqual(r.status_code, 401)
                else:
                    self.assertEqual(r.status_code, 200, r.text[:300])
                    self.assertEqual({e["id"] for e in entries(r.json()) if e["id"] in mine}, {linked},
                                     "a visitor holding the link sees it listed too")


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

    def test_the_queue_says_what_holds_a_machine_as_the_caller_may_see_it(self):
        """A machine's ``busy_with`` on the Queue page is its live leases (package 3-08: a lobby game holds them from
        its creation, whoever moves first), each holder once: the owner and an administrator see the owner's game by
        name, another member "someone's game", a visitor nothing (401)."""
        from citar.pool import scheduler as prompt_scheduler
        prompt_scheduler.reset()                # this test's leases only
        self.addCleanup(prompt_scheduler.reset)
        seat = {"type": "llm", "llm": {"server_id": "sv_dryrun", "model": "dry-run"}}
        r = self.call("owner", "POST", "/api/games", {"name": "held", "config": {**CONFIG, "map_size": "small"},
                                                      "seats": [{"type": "human"}, seat, seat]})
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        try:
            self.assertEqual(len(prompt_scheduler.get().leases("sv_dryrun")), 2, "a lease per model seat")

            def busy(who):
                q = self.call(who, "GET", "/api/queue")
                self.assertEqual(q.status_code, 200, q.text[:300])
                return next((m["busy_with"] for m in q.json()["machines"] if m["server_id"] == "sv_dryrun"), None)
            self.assertEqual(busy("owner"), ["game “held”"])
            self.assertEqual(busy("admin"), ["game “held”"])
            self.assertEqual(busy("stranger"), ["someone's game"])
            self.assertEqual(self.call("anonymous", "GET", "/api/queue").status_code, 401)
        finally:
            self.manager.delete(gid)
        self.assertEqual(prompt_scheduler.get().leases("sv_dryrun"), [], "a closed game holds nothing")

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

    def test_what_queued_work_waits_for_names_only_what_the_caller_may_see(self):
        """The owner's private game holds the dry run, and the owner's private benchmark run too; the stranger's queued
        benchmark job waits for the game, the stranger's probe run for the benchmark (a probe run waits for benchmarks
        only, package 3-09). Their routes and the Queue page tell the stranger "someone's game" and "someone's
        benchmark run", never the names; an administrator is told the names (P3.9.1). The stored notes name nobody."""
        from citar.pool import scheduler as prompt_scheduler
        prompt_scheduler.reset()                # this test's leases only
        self.addCleanup(prompt_scheduler.reset)
        seat = {"type": "llm", "llm": {"server_id": "sv_dryrun", "model": "dry-run"}}
        r = self.call("owner", "POST", "/api/games", {"name": "secret plans", "config": CONFIG,
                                                      "seats": [{"type": "human"}, seat]})
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        self.addCleanup(self.manager.delete, gid)
        secret = "secret plans"
        # the stranger's benchmark run (its suite's machine takes one game at a time) and probe run
        run_ref = self.make("benchmark_run", owner="stranger", share_with=None)
        sch = self.app._scheduler()
        sch.runs[run_ref]["status"] = "running"
        sch._start_jobs()
        job = sch.runs[run_ref]["jobs"][0]
        self.assertEqual(job["status"], "queued")
        self.assertEqual(job["waiting"], "waiting: Dry run is in use by someone's game", "its stored text names nobody")
        # the owner's private benchmark run holding the machine as well: what the stranger's probe run waits for
        owners_run = self.make("benchmark_run", share_with=None)
        bench = prompt_scheduler.get().lease("sv_dryrun", "dry-run", kind="benchmark",
                                             label="benchmark run “secret bench”", ref=("benchmark", owners_run))
        self.addCleanup(bench.release)
        probe_ref = self.make("probe_run", owner="stranger", share_with=None)
        runner = self.app._probes()
        run = runner.get_run(probe_ref)
        run.update(status="queued", llm={"provider": "dryrun", "server_id": "sv_dryrun", "model": "dry-run"},
                   finished=None)
        runner._write(run)
        self.assertTrue(runner._machine_busy(probe_ref))
        self.assertEqual(runner.get_run(probe_ref)["waiting"], "waiting for someone's benchmark run")

        def told(who):
            runs = self.call(who, "GET", "/api/benchmarks/runs").json()
            job_note = next(j for rr in runs if rr["id"] == run_ref for j in rr["jobs"] if j["id"] == "job1")
            probe = self.call(who, "GET", f"/api/probe-runs/{probe_ref}").json()["run"]
            for entry in (job_note, probe):
                self.assertNotIn("waiting_note", entry)
            return job_note["waiting"], probe["waiting"]
        job_told, probe_told = told("stranger")
        self.assertIn("someone's game", job_told)
        self.assertIn("someone's benchmark run", probe_told)
        for text in (job_told, probe_told):
            self.assertNotIn(secret, text)
            self.assertNotIn("secret bench", text)
        job_told, probe_told = told("admin")
        self.assertIn(f"game “{secret}”", job_told)
        self.assertIn("benchmark run “secret bench”", probe_told)
        queue = self.call("stranger", "GET", "/api/queue").json()
        self.assertNotIn(secret, json.dumps(queue))
        self.assertNotIn("secret bench", json.dumps(queue))
        mine = [it for m in queue["machines"] for it in m["waiting"] if it.get("group") in (run_ref, probe_ref)]
        self.assertTrue(mine, queue)
        for it in mine:
            self.assertNotIn("waiting_note", it)
            self.assertIn("someone's", it["waiting"])
        self.assertIn(f"game “{secret}”", json.dumps(self.call("admin", "GET", "/api/queue").json(), ensure_ascii=False))


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
            before = s.scalar(select(func.count()).select_from(AuditLog))
            counts = items.adopt_orphans(s, items.first_admin(s))
        self.assertGreaterEqual(counts.get("map", 0), 1)    # other suites' maps on the shared disk may be strays too
        row = self.row("map", ref)
        self.assertEqual((row.owner_id, row.visibility), (self.ids["admin"], "private"))
        with db.session() as s:
            audits = list(s.scalars(select(AuditLog).where(AuditLog.action == "migration.items_adopted")))
            self.assertEqual(len(audits), 1)
            self.assertEqual(s.scalar(select(func.count()).select_from(AuditLog)) - before, 1,
                             "one audit row for everything adopted, none per item: nobody made them now")
            self.assertIsNotNone(policy.marker(s, "orphans_adopted"))
        self.assertEqual(self.call("admin", "GET", f"/api/maps/{ref}").status_code, 200)
        self.assertEqual(self.call("owner", "GET", f"/api/maps/{ref}").status_code, 404)
        with db.session() as s, mock.patch.object(items, "_describe", side_effect=AssertionError("read")) as read:
            self.assertEqual(items.adopt_orphans(s, items.first_admin(s)), {}, "adopting twice adopts nothing")
            self.assertEqual(read.call_count, 0, "a file whose id has a row is not read: a start lists, not parses")

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

    def _damaged_scenarios(self):
        """Two scenario files that do not read, as a crash mid-write or a bad disk leaves them: one cut short (gzip
        raises EOFError), one whose compressed data is garbage (zlib.error). Removed by the test's cleanup."""
        folder = engine_api.scenario_dir()
        folder.mkdir(parents=True, exist_ok=True)
        cut = "cut-" + secrets.token_hex(3)
        whole = gzip.compress(json.dumps({"name": "whole", "state": {"x": list(range(4000))}}).encode())
        (folder / f"{cut}.citarscn").write_bytes(whole[:len(whole) // 2])
        bad = "bad-" + secrets.token_hex(3)
        (folder / f"{bad}.citarscn").write_bytes(b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff" + b"\xff\xfe" * 64)
        self.addCleanup(lambda: [(folder / f"{r}.citarscn").unlink(missing_ok=True) for r in (cut, bad)])
        return cut, bad

    def test_an_unreadable_file_stops_neither_the_boot_nor_the_first_administrator(self):
        """A damaged item file is adopted under its id by the administrator (private: nobody else's, theirs to remove)
        and the files after it are adopted as usual: neither the boot nor ``create-admin`` fails on it."""
        from citar.server import admin_cli
        cut, bad = self._damaged_scenarios()
        ref = self._stray()
        counts = items.on_boot()
        self.assertGreaterEqual(counts.get("map", 0), 1)
        self.assertEqual(self.row("map", ref).owner_id, self.ids["admin"])
        for damaged in (cut, bad):
            row = self.row("scenario", damaged)
            self.assertEqual((row.owner_id, row.visibility, row.name), (self.ids["admin"], "private", damaged))
            self.assertEqual(self.call("owner", "GET", f"/api/scenarios/{damaged}").status_code, 404)
            self.assertEqual(self.call("admin", "DELETE", f"/api/scenarios/{damaged}").status_code, 200,
                             "the administrator may remove it through its route")
        # a fresh server-mode database: the first administrator is made with damaged files on the disk
        with db.session() as s:
            for user in s.scalars(select(User).where(User.role == "admin")):
                user.role = "user"
        cut, bad = self._damaged_scenarios()
        ref = self._stray()
        with mock.patch.object(admin_cli, "_prompt_password", return_value="a long enough phrase"):
            code = admin_cli.cmd_create_admin(SimpleNamespace(handle="firstadmin", email="first@example.com",
                                                              name="", force=False))
        self.assertEqual(code, 0)
        with db.session() as s:
            first = accounts.by_handle(s, "firstadmin")
            self.assertEqual(first.role, "admin")
            self.assertEqual(items.get(s, "map", ref).owner_id, first.id)
            self.assertEqual(items.get(s, "scenario", cut).owner_id, first.id)

    def test_a_failed_adoption_keeps_the_administrator_and_the_boot(self):
        """Whatever adoption meets, it never takes the new administrator with it, nor stops the server's start: it is
        rolled back alone and tried again at the next start."""
        from citar.server import admin_cli
        with db.session() as s:
            for user in s.scalars(select(User).where(User.role == "admin")):
                user.role = "user"
        ref = self._stray()
        with mock.patch.object(items, "adopt_orphans", side_effect=RuntimeError("disk on fire")):
            with mock.patch.object(admin_cli, "_prompt_password", return_value="a long enough phrase"):
                code = admin_cli.cmd_create_admin(SimpleNamespace(handle="firstadmin", email="first@example.com",
                                                                  name="", force=False))
            self.assertEqual(code, 0)
            self.assertIsNone(items.on_boot(), "a failed adoption is logged, not raised")
        with db.session() as s:
            self.assertEqual(accounts.by_handle(s, "firstadmin").role, "admin")
            self.assertIsNone(items.get(s, "map", ref), "nothing adopted: it answers 404 until the next start")
            self.assertIsNone(policy.marker(s, "orphans_adopted"))
            self.assertEqual(items.get(s, "bot_profile", "standard").visibility, "global",
                             "the shipped items' rows are written apart from adoption")
        self.assertGreaterEqual(items.on_boot().get("map", 0), 1, "the next start adopts it")
        self.assertIsNotNone(self.row("map", ref))

    def test_a_deleted_items_id_is_a_new_item(self):
        ref = self.make("map", "allowlist", share_with="reader")
        with db.session() as s:
            old = items.get(s, "map", ref)
            old_id, old_slug = old.id, old.slug
            access.unpublish(s, old, actor=s.get(User, self.ids["admin"]), reason="old")
            items.delete(s, "map", ref)
            row = items.register(s, "map", ref, self.ids["stranger"], "again")
            self.assertEqual((row.owner_id, row.visibility), (self.ids["stranger"], "private"))
            self.assertNotEqual((row.id, row.slug), (old_id, old_slug))
            self.assertIsNone(access.last_unpublished(s, row), "the old item's unpublish is not the new one's")
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


class GameCsrf(ItemCase):
    def test_a_cookie_callers_game_write_carries_the_csrf_token(self):
        """3-01 left the game routes gated by ``_gate`` without the CSRF check ``require_user`` makes: a write by a
        caller a session cookie signed in now needs the session's token; a valid seat token is a credential of its own
        and needs none; a made-up one does not excuse the check."""
        r = self.call("owner", "POST", "/api/games", {"name": "csrf", "config": CONFIG, "seats": BOTS})
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        try:
            cookie_only = {k: v for k, v in self.headers["owner"].items() if k != sessions.CSRF_HEADER}
            path = f"/api/games/{gid}/control"
            self.assertEqual(self.client.post(path, json={"paused": True}, headers=cookie_only).status_code, 403)
            self.assertEqual(self.client.post(path + "?token=made-up", json={"paused": True},
                                              headers=cookie_only).status_code, 403)
            self.assertEqual(self.call("owner", "POST", path, {"paused": True}).status_code, 200)
            self.assertEqual(self.client.post(f"/api/games/{gid}/unpublish", json={},
                                              headers=cookie_only).status_code, 403)
            for route in ("/api/saves/load", "/api/saves/delete"):
                self.assertEqual(self.client.post(route, json={"path": f"{gid}/autosave.citar"},
                                                  headers=cookie_only).status_code, 403, route)
            spectator = self.manager.get(gid).spectator_token
            r = self.client.get(f"/api/games/{gid}/view?token={spectator}", headers=cookie_only)
            self.assertEqual(r.status_code, 200, "a read needs no token")
        finally:
            self.manager.delete(gid)


class CommandLine(ItemCase):
    def test_lab_submit_registers_its_experiment(self):
        """``citar lab submit`` registers what it queues when the server's database is there: to ``--owner``, else to
        the first administrator."""
        import contextlib
        import io
        for owner, expect in ((None, "admin"), ("olive", "owner")):
            name = "cli-" + secrets.token_hex(3)
            spec = self.dir / f"{name}.json"
            spec.write_text(json.dumps({"name": name, "games": 1, "seats": [{"bot": "basic"}, {"bot": "basic"}]}),
                            encoding="utf-8")
            self.made.append(("lab_experiment", name))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                lab.main(["submit", str(spec)] + (["--owner", owner] if owner else []))
            with self.subTest(owner=owner):
                self.assertIn("registered to", out.getvalue())
                self.assertEqual(self.row("lab_experiment", name).owner_id, self.ids[expect])


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

    def test_a_report_narrative_names_a_machine_the_member_may_use(self):
        """A member's report writes its analysis on a machine they may use: not one of the site's own machines
        nobody shared with them (403), and it must name one (400); the site's dry run, shared with members here, is
        theirs to use."""
        def ask(narrative):
            return self.call("owner", "POST", "/api/reports", {"title": "n", "narrative": {"enabled": True, **narrative}})
        self.assertEqual(ask({"server_id": "sv_api", "model_id": "m_opus"}).status_code, 403)
        self.assertEqual(ask({}).status_code, 400)
        r = ask({"server_id": "sv_dryrun", "model_id": "m_dry"})
        self.assertEqual(r.status_code, 200, r.text)
        self.made.append(("report", r.json()["id"]))

    def test_a_report_narrative_is_admitted_to_its_pooled_machine(self):
        """A pooled machine a member has no grant on (another account's, in a private group) is refused for their
        report's narrative, on a new report and on a rerun, without being named and with no report made; their own
        machine is theirs to use."""
        with db.session() as s:
            group = ServerGroup(owner_id=self.ids["admin"], name="Admin's box", visibility="private")
            s.add(group)
            s.flush()
            s.add(Server(id="sv_private", owner_id=self.ids["admin"], group_id=group.id, name="Admin's GPU",
                         kind="owned", provider="worker", reach="worker", config={"models": [{"key": "m"}]}))
            s.add(Server(id="sv_mine", owner_id=self.ids["owner"], name="Olive's GPU", kind="owned",
                         provider="worker", reach="worker", config={"models": [{"key": "m"}]}))

        def reports():
            with db.session() as s:
                return s.scalar(select(func.count()).select_from(OwnedItem).where(OwnedItem.kind == "report"))
        before = reports()
        narrative = {"enabled": True, "server_id": "sv_private", "model_id": "m"}
        r = self.call("owner", "POST", "/api/reports", {"title": "n", "narrative": narrative})
        self.assertEqual(r.status_code, 403, r.text)
        self.assertNotIn("Admin", r.json()["detail"], "a machine the member cannot see is not named")
        self.assertEqual(reports(), before, "no report was made")
        rid = self.make("report")
        from citar.server import reports_api
        spec_file = reports_api.reports_runner().dir / rid / "spec.json"
        spec_file.write_text(json.dumps({"title": "rerun", "narrative": narrative}), encoding="utf-8")
        before = reports()
        r = self.call("owner", "POST", f"/api/reports/{rid}/rerun")
        self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(reports(), before, "no report was made")
        r = self.call("owner", "POST", "/api/reports", {"title": "n", "narrative": dict(narrative, server_id="sv_mine")})
        self.assertEqual(r.status_code, 200, r.text)
        self.made.append(("report", r.json()["id"]))
        r = self.call("admin", "POST", "/api/reports", {"title": "n", "narrative": narrative})
        self.assertEqual(r.status_code, 200, "an administrator may use any machine")
        self.made.append(("report", r.json()["id"]))

    def test_another_members_run_shows_no_machine_address(self):
        """A run or a probe run another member may see carries no machine's address or key variable."""
        ref = self.make("benchmark_run", "server")
        self.app._scheduler().runs[ref]["suite"]["servers"][0]["base_url"] = "http://10.0.0.5:1234/v1"
        runs = self.call("stranger", "GET", "/api/benchmarks/runs").json()
        self.assertNotIn("10.0.0.5", json.dumps(runs))
        self.assertIn("10.0.0.5", json.dumps(self.call("admin", "GET", "/api/benchmarks/runs").json()))
        rid = self.make("probe_run", "server")
        run = json.loads((probes.RUNS / rid / "run.json").read_text(encoding="utf-8"))
        run["llm"] = {"provider": "openai_compatible", "base_url": "http://10.0.0.6/v1", "api_key_env": SECRET_VAR}
        (probes.RUNS / rid / "run.json").write_text(json.dumps(run), encoding="utf-8")
        seen = self.call("stranger", "GET", f"/api/probe-runs/{rid}").json()
        self.assertNotIn("10.0.0.6", json.dumps(seen))
        self.assertNotIn(SECRET_VAR, json.dumps(seen))


class BadBodies(ItemCase):
    def test_a_body_of_the_wrong_shape_is_a_400(self):
        """A member's request whose item references, suite or report specification has the wrong shape is refused
        with 400, never a 500 (the test client raises a server error)."""
        pid = self.make("probe")
        r = self.call("owner", "POST", "/api/games", {"name": "g", "config": CONFIG, "seats": BOTS})
        self.assertEqual(r.status_code, 200, r.text)
        gid = r.json()["id"]
        self.addCleanup(lambda: self.manager.delete(gid))
        r = self.call("owner", "POST", "/api/scenario-editor", {"source": "generate", "config": CONFIG,
                                                               "players": [{"type": "bot"}, {"type": "bot"}]})
        eid = r.json()["editor_id"]
        servers_ = [{"server_id": "sv_dryrun", "models": [{"model_id": "m_dry"}]}]
        bad = [
            ("POST", "/api/games", {"config": CONFIG, "seats": [{"type": "bot", "bot": {"profile": ["x"]}},
                                                               {"type": "bot"}]}),
            ("POST", f"/api/games/{gid}/seats/1", {"type": "bot", "bot": {"profile": {"a": 1}}}),
            ("POST", f"/api/scenarios/{self.base_scenario}/launch", {"seats": [{"type": "bot",
                                                                                "bot": {"profile": [1]}}]}),
            ("POST", f"/api/scenario-editor/{eid}/save", {"name": "bad", "seats": [{"type": "bot",
                                                                                   "bot": {"profile": [1]}},
                                                                                  {"type": "bot"}]}),
            ("POST", "/api/benchmarks/suites", {"name": "x", "servers": servers_,
                                                "scenarios": [{"name": "s", "bot_profile": ["x"]}]}),
            ("POST", "/api/benchmarks/suites", {"id": 5, "name": "x", "servers": servers_}),
            ("POST", "/api/benchmarks/suites", {"id": ["a"], "name": "x", "servers": servers_}),
            ("POST", "/api/benchmarks/suites", {"name": ["x"], "servers": servers_}),
            ("POST", "/api/benchmarks/runs", {"suite": {"name": "x", "servers": [{"server_id": "sv_dryrun",
                                                                                  "models": [5]}]}}),
            ("POST", f"/api/probes/{pid}/run", {"llm": {"provider": "bot", "profile": ["x"]}}),
            ("POST", "/api/reports", {"title": "x", "narrative": "yes"}),
            ("POST", "/api/reports", {"title": "x", "scope": [1]}),
            ("POST", "/api/reports", {"title": "x", "range": 3}),
            ("POST", "/api/reports", {"title": "x", "scope": {"servers": 4}}),
            ("POST", "/api/reports", {"title": "x", "sections": "summary"}),
            ("POST", "/api/reports", {"title": "x", "options": {"lifespan_range": [{}, 2]}}),
            ("POST", "/api/reports", {"title": "x", "narrative": {"enabled": True, "server_id": ["sv_dryrun"]}}),
        ]
        for method, path, body in bad:
            with self.subTest(route=f"{method} {path}", body=body):
                r = self.call("owner", method, path, body)
                self.assertEqual(r.status_code, 400, r.text[:300])
        # a preset that is not one is no preset, as before
        r = self.call("owner", "POST", "/api/reports", {"title": "x", "preset": ["x"]})
        self.assertEqual(r.status_code, 200, r.text)
        self.made.append(("report", r.json()["id"]))


class ItemsInsideOthers(ItemCase):
    """Using an item in something else needs VIEW on it when the thing is run or launched, not only when it is saved:
    sharing the outer item does not share the inner one."""

    def test_a_shared_probe_runs_only_for_those_who_see_its_scenario(self):
        theirs = self.make("scenario", owner="reader", share_with="owner")     # shared with the probe's owner only
        body = {"name": "On theirs", "scenario": theirs, "subject": 1, "counterparty": 0,
                "cases": [{"id": "hi", "kind": "message", "message": "Hi."}]}
        self.assertEqual(self.call("owner", "PUT", "/api/probes/on-theirs", body).status_code, 200)
        self.made.append(("probe", "on-theirs"))
        r = self.call("owner", "PUT", "/api/items/probe/on-theirs/sharing", {"visibility": "server"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.call("stranger", "GET", "/api/probes/on-theirs").status_code, 200)
        r = self.call("stranger", "POST", "/api/probes/on-theirs/run", {"llm": {"provider": "bot"}})
        self.assertEqual(r.status_code, 404, r.text)
        for who in ("owner", "reader", "admin"):
            with self.subTest(caller=who):
                r = self.call(who, "POST", "/api/probes/on-theirs/run", {"llm": {"provider": "bot"}})
                self.assertEqual(r.status_code, 200, r.text)
                self.made.append(("probe_run", r.json()["id"]))

    def test_a_launch_checks_the_profiles_the_scenarios_own_seats_play(self):
        mine = self.make("bot_profile", share_with=None)
        r = self.call("owner", "POST", "/api/scenario-editor", {"source": "generate", "config": CONFIG,
                                                               "players": [{"type": "bot"}, {"type": "bot"}]})
        eid = r.json()["editor_id"]
        r = self.call("owner", "POST", f"/api/scenario-editor/{eid}/save",
                      {"name": "With my bot", "seats": [{"type": "bot", "bot": {"profile": mine}}, {"type": "bot"}]})
        self.assertEqual(r.status_code, 200, r.text)
        sid = r.json()["id"]
        self.made.append(("scenario", sid))
        self.assertEqual(engine_api.load_scenario(sid)["seats"][0]["bot"]["profile"], mine)
        r = self.call("owner", "PUT", f"/api/items/scenario/{sid}/sharing", {"visibility": "server"})
        self.assertEqual(r.status_code, 200, r.text)
        r = self.call("stranger", "POST", f"/api/scenarios/{sid}/launch", {"name": "theirs"})
        self.assertEqual(r.status_code, 404, r.text)
        self.assertNotIn("theirs", {s.name for s in self.manager.sessions.values()})
        # a seat the launch gives a profile of its own plays that one instead
        for who, body in (("stranger", {"name": "own seat", "seats": [{"type": "bot", "bot": {"profile": "standard"}}]}),
                          ("owner", {"name": "owners"})):
            with self.subTest(caller=who):
                r = self.call(who, "POST", f"/api/scenarios/{sid}/launch", body)
                self.assertEqual(r.status_code, 200, r.text)
                self.manager.delete(r.json()["id"])


class BestBot(ItemCase):
    def test_best_is_a_profile_every_member_may_see(self):
        """"Best bot" stands for the best-rated profile every member may see (the shipped ones, `server` and `global`
        ones), the same one for a seat, the lab and the Bots page, whoever asks: never a member's private profile,
        however it ranks."""
        from citar.bots import ratings
        from citar.server import session as sess
        private = self.make("bot_profile", share_with=None)
        shared = self.make("bot_profile", "server", share_with=None)
        board = [{"id": pid, "name": pid, "engine": "basic", "archived": False, "rating_same_settings": True,
                  "rating": {"rated": True, "rating": rating}}
                 for pid, rating in ((private, 1700), (shared, 1600), ("standard", 1500))]
        with mock.patch.object(ratings, "ranked_profiles", lambda board_=None: [dict(p) for p in board]):
            self.assertEqual(ratings.best_profile(), shared)
            self.assertEqual(sess._seat_bot({"profile": "best"}), {"profile": shared, "chosen_as": "best"})
            self.assertEqual(profiles.resolve("best")["profile"], shared)
            for who in ("owner", "stranger", "admin", "anonymous"):
                with self.subTest(caller=who):
                    r = self.call(who, "GET", "/api/bots/profiles")
                    self.assertEqual(r.json()["best"], shared, r.text[:300])
            self.assertEqual(ratings.best_profile(among={private, shared}), private, "a caller may name the pool")
            with db.session() as s:
                items.get(s, "bot_profile", shared).visibility = "private"
            self.assertEqual(ratings.best_profile(), "standard")


class ListingPermissions(ItemCase):
    def test_a_listings_permissions_are_each_items_in_two_queries(self):
        """``permissions_by_ref`` answers what ``access.on`` answers for each item, with a fixed number of queries
        however many items the listing has."""
        refs = [self.make("map", v, share_with="reader") for v in ("private", "server", "global", "link", "private")]
        refs.append("no-row-" + secrets.token_hex(3))
        statements: list = []

        def count(*_a, **_k):
            statements.append(1)
        event.listen(db.engine(), "before_cursor_execute", count)
        self.addCleanup(event.remove, db.engine(), "before_cursor_execute", count)
        for who in ("owner", "reader", "stranger", "mod", "admin", "anonymous"):
            with db.session() as s:
                viewer = s.get(User, self.ids[who]) if who in self.ids else None
                statements.clear()
                got = items.permissions_by_ref(s, "map", viewer, refs)
                queries = len(statements)
                want = {ref: access.on(s, viewer, items.get(s, "map", ref)).client()
                        for ref in refs if items.get(s, "map", ref) is not None}
            with self.subTest(caller=who):
                self.assertEqual(got, want)
                self.assertLessEqual(queries, 2 if viewer is not None else 1)


if __name__ == "__main__":
    unittest.main()

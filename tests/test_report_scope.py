"""What a report gathers, and its public page (design/phase3-features.md P3.2.4; package 3-03, gate 2).

A member's report covers what its owner may see (``reports.reach.Reach``): the activities of their own games, runs,
probe runs, experiments and reports and of those shared with them, with costs on their own machines only; an
administrator's covers everything. ``/r/{slug}`` serves a `link` report by its key and a `global` one by its id (or
key), and 404s otherwise. The usage ledger here is made up, in a folder of its own.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import os
import secrets
import shutil
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="citar_report_scope_"))
DB_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = DB_URL
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, items, pool, reports, usage
from citar.auth import access, accounts, policy, sessions
from citar.db.models import Base, Game as GameRow
from citar.reports import data as report_data, render as report_render
from citar.reports.reach import Reach

#: A machine priced like the test registry's GPU box: power, a component, the registry's electricity plan.
RIG = {"kind": "owned", "connection": {"provider": "dryrun", "max_parallel": 1},
       "power": {"idle_w": 50, "cpu_max_w": 100, "gpu_max_w": 300, "sampling": "off"},
       "components": [{"name": "Box", "price": 876.6, "purchased": "2020-01-01", "lifespan_years": 10}],
       "costs": [{"from": "2000-01-01", "electricity_plan_id": "ep_home", "fixed_monthly": 0}],
       "models": [{"id": "m_rig", "key": "rig-model"}]}


def _db():
    db.dispose()
    db.configure(DB_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


class World:
    """Two members and an administrator, a machine each for the members, their games, runs and probe runs, and a
    usage ledger in which all of them ran in the last hour."""

    def __init__(self, ledger_dir: Path):
        self.tag = secrets.token_hex(3)
        t = self.tag
        _db()
        with db.session() as s:
            self.users = {}
            for handle, role in (("alice", "user"), ("bob", "user"), ("zara", "admin")):
                u = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                         password="a long enough phrase", role=role, status="active",
                                         email_verified=True)
                self.users[handle] = u.id
            alice, bob = (s.get(accounts.User, self.users[h]) for h in ("alice", "bob"))
            self.alice_rig = pool.create_server(s, alice, {**RIG, "name": f"Alice home rig {t}"}).id
            self.bob_rig = pool.create_server(s, bob, {**RIG, "name": f"Bob private rig {t}"}).id
            self.games = {"alice": f"ga{t}", "bob": f"gb{t}", "shared": f"gs{t}", "bench": f"gx{t}"}
            for key, owner, kind, visibility in (("alice", alice, "game", "private"), ("bob", bob, "game", "private"),
                                                 ("shared", bob, "game", "server"),
                                                 ("bench", bob, "benchmark", "private")):
                s.add(GameRow(id=self.games[key], owner_id=owner.id, name=key, kind=kind, visibility=visibility,
                              data_sharing="pool", status="finished"))
            self.run, self.probe_run = f"run{t}", f"prun{t}"
            items.register(s, "benchmark_run", self.run, bob, f"Bob secret run {t}")
            items.register(s, "probe_run", self.probe_run, bob, f"Bob secret probe run {t}")
        now = time.time()
        t0, t1 = now - 1800, now - 600
        acts = [
            ("game:" + self.games["alice"], {"kind": "game", "name": f"Alice game {t}",
                                             "ref": {"game_id": self.games["alice"]},
                                             "seats": [{"player": 0, "type": "llm", "server_id": self.alice_rig,
                                                        "model": "m-alice"}]}, [self.alice_rig, "sv_host"]),
            ("game:" + self.games["bob"], {"kind": "game", "name": f"Bob secret game {t}",
                                           "ref": {"game_id": self.games["bob"]},
                                           "seats": [{"player": 0, "type": "llm", "server_id": self.bob_rig,
                                                      "model": "m-bob"}]}, [self.bob_rig, "sv_host"]),
            ("game:" + self.games["shared"], {"kind": "game", "name": f"Bob shared game {t}",
                                              "ref": {"game_id": self.games["shared"]},
                                              "seats": [{"player": 0, "type": "llm", "server_id": self.bob_rig,
                                                         "model": "m-shared"}]}, [self.bob_rig]),
            ("game:" + self.games["bench"], {"kind": "benchmark", "name": f"Bob secret bench game {t}",
                                             "parent": {"kind": "benchmark_run", "id": self.run,
                                                        "name": f"Bob secret run {t}"},
                                             "ref": {"game_id": self.games["bench"], "job_id": "j1"},
                                             "seats": [{"player": 0, "type": "llm", "server_id": self.bob_rig,
                                                        "model": "m-bob"}]}, [self.bob_rig]),
            ("probe:" + self.probe_run, {"kind": "probe", "name": f"Bob secret probe run {t}",
                                         "parent": {"kind": "probe", "id": f"probe{t}", "name": f"Bob probe {t}"},
                                         "ref": {"probe_run": self.probe_run}, "server_id": self.bob_rig,
                                         "model": "m-bob"}, [self.bob_rig]),
        ]
        rows = []
        for act, info, machines in acts:
            rows.append({"k": "act", "id": act, "t": t0, **info})
            for srv in machines:
                rows.append({"k": "span", "act": act, "srv": srv, "t0": round(t0, 1), "t1": round(t1, 1),
                             "held": 1200.0, "busy": 300.0, "cpu": 20.0, "model": "m", "in": 1000, "out": 500})
        ledger_dir.mkdir(parents=True, exist_ok=True)
        (ledger_dir / f"server-{usage._month(now)}.jsonl").write_text(
            "".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")

    def secrets_of_bob(self) -> list:
        """Everything of bob's that alice may not see: names and ids."""
        t = self.tag
        return [f"Bob secret game {t}", self.games["bob"], f"Bob secret bench game {t}", self.games["bench"],
                f"Bob secret run {t}", self.run, f"Bob secret probe run {t}", self.probe_run,
                f"Bob private rig {t}", self.bob_rig]


class ReportReach(unittest.TestCase):
    """``data.gather`` and ``render`` as a member's and an administrator's report."""

    SPEC = reports.normalize_spec({"title": "everything", "sections": reports.ALL_SECTIONS, "range": {"preset": "all"}})

    def setUp(self):
        self.ledger = Path(tempfile.mkdtemp(prefix="citar_ledger_"))
        self.addCleanup(shutil.rmtree, self.ledger, ignore_errors=True)
        patch = mock.patch.object(usage, "USAGE_DIR", self.ledger)
        patch.start()
        self.addCleanup(patch.stop)
        self.w = World(self.ledger)

    def build(self, who=None, spec=None):
        reach = Reach.of(self.w.users[who]) if who else Reach()
        data = report_data.gather(spec or self.SPEC, reach=reach)
        page, _summary = report_render.render(data)
        return data, page

    def test_a_members_report_holds_none_of_another_members_private_things(self):
        data, page = self.build("alice")
        self.assertNotIn("This section failed", page)
        for secret in self.w.secrets_of_bob():
            self.assertNotIn(secret, page, f"{secret!r} is bob's and private")
        self.assertIn(f"Alice game {self.w.tag}", page)
        self.assertIn(f"Alice home rig {self.w.tag}", page, "her own machine and its costs are hers")
        self.assertIn(f"Bob shared game {self.w.tag}", page, "a game bob shared with members is one she may see")
        self.assertIn("Costs are counted on the report owner&#x27;s own machines only", page, "escaped, as the page prints it")

    def test_a_members_costs_are_on_their_own_machines_only(self):
        data, _page = self.build("alice")
        self.assertEqual(set(data["cost"]["servers"]), {self.w.alice_rig})
        self.assertEqual({s["id"] for s in data["registry"]["servers"]}, {self.w.alice_rig})
        acts = {a["id"]: a for a in data["acts"]}
        mine = acts["game:" + self.w.games["alice"]]
        self.assertEqual(set(mine["servers"]), {self.w.alice_rig}, "the host's part of her game is not hers")
        shared = acts["game:" + self.w.games["shared"]]
        self.assertEqual(shared["servers"], {}, "a shared game's cost on bob's machine is bob's")
        self.assertEqual(shared["total"], 0)
        admin, _ = self.build("zara")
        full = {a["id"]: a for a in admin["acts"]}["game:" + self.w.games["alice"]]
        self.assertGreater(full["cost"]["depreciation"], mine["cost"]["depreciation"],
                           "an administrator's report prices the host's part too")
        self.assertGreater(mine["cost"]["depreciation"], 0)
        self.assertAlmostEqual(mine["cost"]["depreciation"], full["servers"][self.w.alice_rig]["depreciation"],
                               msg="her machine's part is priced as for anyone: the whole ledger shares its minutes")

    def _api_machines(self):
        """An API machine for each member, each with a model that has no price and used in the last hour: bob's by
        his private game, its name holding the name of alice's own machine (a note matched by machine name would
        be kept for her), and alice's by hers; and usage of bob's private game on no machine at all."""
        t = self.w.tag
        api = {"kind": "api", "costs": [{"from": "2000-01-01"}],
               "connection": {"provider": "openai_compatible", "base_url": "https://api.example.com/v1"}}
        with db.session() as s:
            alice, bob = (s.get(accounts.User, self.w.users[h]) for h in ("alice", "bob"))
            bob_api = pool.create_server(s, bob, {**api, "name": f"Bob box beside Alice home rig {t}",
                                                  "models": [{"id": "m_b", "key": f"bob-unpriced-{t}"}]}).id
            alice_api = pool.create_server(s, alice, {**api, "name": f"Alice api {t}",
                                                      "models": [{"id": "m_a", "key": f"alice-unpriced-{t}"}]}).id
        now = time.time()
        rows = [{"k": "span", "act": "game:" + self.w.games[who], "srv": srv, "t0": round(now - 1700, 1),
                 "t1": round(now - 700, 1), "held": 0.0, "busy": 30.0, "cpu": 0.0, "model": model, "in": 2000,
                 "out": 800}
                for who, srv, model in (("bob", bob_api, f"bob-unpriced-{t}"), ("alice", alice_api, f"alice-unpriced-{t}"),
                                        ("bob", None, f"bob-unpriced-{t}"))]
        ledger = next(self.ledger.glob("server-*.jsonl"))
        with open(ledger, "a", encoding="utf-8") as f:
            f.writelines(json.dumps(r) + "\n" for r in rows)
        return bob_api, alice_api

    def test_a_pricing_note_about_another_members_machine_never_reaches_her(self):
        """``costing.compute``'s notes name a machine and the model used on it; a member's report keeps those filed
        under her own machines, never one matched by her machine's name inside another's."""
        bob_api, _alice_api = self._api_machines()
        t = self.w.tag
        data, page = self.build("alice")
        for secret in (f"Bob box beside Alice home rig {t}", bob_api, f"bob-unpriced-{t}"):
            self.assertNotIn(secret, page, f"{secret!r} is bob's")
            self.assertFalse(any(secret in n for n in data["cost"]["notes"]), secret)
        self.assertIn(f"No token price for alice-unpriced-{t} on Alice api {t}", page, "a note about her own machine")
        self.assertNotIn("Some model usage has no server", page, "the usage with no machine was bob's private game's")
        admin, admin_page = self.build("zara")
        self.assertIn(f"No token price for bob-unpriced-{t} on Bob box beside Alice home rig {t}", admin_page)
        self.assertIn("Some model usage has no server", admin_page)
        self.assertEqual(admin["cost"]["notes"], sorted(admin["cost"]["notes"]))

    def test_an_owner_who_lost_good_standing_reaches_what_a_visitor_would(self):
        """A report built after its owner was suspended (or their account still pending): published things only, no
        machine of theirs priced; an administrator's role counts for nothing then (not every run with no game)."""
        t = self.w.tag
        with db.session() as s:
            s.get(GameRow, self.w.games["shared"]).visibility = "global"
            s.get(accounts.User, self.w.users["alice"]).status = "suspended"
            s.get(accounts.User, self.w.users["zara"]).status = "suspended"
        for who in ("alice", "zara"):
            with self.subTest(who=who):
                reach = Reach.of(self.w.users[who])
                self.assertFalse(reach.everything)
                self.assertEqual(reach.machines, set())
                self.assertEqual(reach.games, {self.w.games["shared"]})
                self.assertEqual(reach.items["benchmark_run"], set())
                self.assertEqual(reach.items["probe_run"], set())
                data, page = self.build(who)
                self.assertEqual(data["cost"]["servers"], {})
                self.assertIn(f"Bob shared game {t}", page, "a published game is anyone's to see")
                for hidden in (f"Alice game {t}", f"Alice home rig {t}", *self.w.secrets_of_bob()):
                    self.assertNotIn(hidden, page)
        with db.session() as s:
            s.get(accounts.User, self.w.users["alice"]).status = "probation"
        self.assertEqual(Reach.of(self.w.users["alice"]).machines, {self.w.alice_rig}, "probation is good standing")

    def test_an_administrators_report_contains_everything(self):
        _data, page = self.build("zara")
        t = self.w.tag
        # the names: a report prints names, not ids (the run and the probe run have no files here, so their own
        # sections are empty; their activities carry them)
        for name in (f"Bob secret game {t}", f"Bob secret bench game {t}", f"Bob secret probe run {t}",
                     f"Bob private rig {t}"):
            self.assertIn(name, page)
        self.assertIn(f"Alice game {self.w.tag}", page)

    def test_a_report_with_no_owner_is_the_servers_and_sees_everything(self):
        _data, page = self.build(None)
        self.assertIn(f"Bob secret game {self.w.tag}", page)

    def test_a_shared_run_and_probe_run_reach_their_viewer(self):
        with db.session() as s:
            alice = s.get(accounts.User, self.w.users["alice"])
            for kind, ref in (("benchmark_run", self.w.run), ("probe_run", self.w.probe_run)):
                row = items.get(s, kind, ref)
                access.grant(s, row, alice, "view", actor=s.get(accounts.User, self.w.users["bob"]))
        _data, page = self.build("alice")
        self.assertIn(f"Bob secret bench game {self.w.tag}", page)
        self.assertIn(f"Bob secret probe run {self.w.tag}", page)
        self.assertNotIn(f"Bob secret game {self.w.tag}", page)
        self.assertNotIn(f"Bob private rig {self.w.tag}", page)

    def test_the_builder_offers_a_member_what_their_report_could_gather(self):
        opts = reports.scope_options(Reach.of(self.w.users["alice"]))
        offered = {(i["kind"], i["id"]) for i in opts["items"]}
        self.assertIn(("game", self.w.games["alice"]), offered)
        self.assertIn(("game", self.w.games["shared"]), offered)
        for kind, ref in (("game", self.w.games["bob"]), ("game", self.w.games["bench"]),
                          ("benchmark_run", self.w.run), ("probe_run", self.w.probe_run)):
            self.assertNotIn((kind, ref), offered)
        machines = {sv["id"] for sv in opts["servers"]}
        self.assertIn(self.w.alice_rig, machines)
        self.assertNotIn(self.w.bob_rig, machines)
        self.assertNotIn("sv_host", machines, "the site's host is an administrator's")
        self.assertNotIn("m-bob", opts["models"])
        everything = reports.scope_options(Reach.of(self.w.users["zara"]))
        self.assertIn(("game", self.w.games["bob"]), {(i["kind"], i["id"]) for i in everything["items"]})

    def test_a_pooled_only_report_has_no_activities_of_its_own(self):
        spec = reports.normalize_spec({**self.SPEC, "pooling": "pooled", "sections": ["summary", "models"]})
        data, page = self.build("alice", spec)
        self.assertEqual(data["acts"], [])
        self.assertIn("Pooled figures", page)
        self.assertNotIn(f"Bob shared game {self.w.tag}", page)
        self.assertEqual(reports.normalize_spec({"pooling": "everything"})["pooling"], "own")

    def test_the_runner_builds_with_its_owners_reach(self):
        runner = reports.ReportRunner(Path(tempfile.mkdtemp(prefix="citar-reports-")))
        self.addCleanup(shutil.rmtree, runner.dir, ignore_errors=True)
        with mock.patch.object(reports.ReportRunner, "_wake", lambda self: None):
            mine = runner.start({"title": "mine", "sections": ["activities"]}, owner_id=self.w.users["alice"])
            runner.run(mine["id"])
        page = runner.html_path(mine["id"]).read_text(encoding="utf-8")
        self.assertIn(f"Alice game {self.w.tag}", page)
        self.assertNotIn(f"Bob secret game {self.w.tag}", page)


class PublicReportPage(unittest.TestCase):
    """``GET /r/{slug}``: a `link` report by its key, a `global` one by its id or key, 404 for anything else, its owner
    and administrators included (who read it in the app)."""

    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod, reports_api
        cls.client = TestClient(appmod.app, base_url="https://citar.test")
        cls.saved = reports_api._runner
        cls.dir = Path(tempfile.mkdtemp(prefix="citar-r-"))
        with mock.patch.object(reports.ReportRunner, "_wake", lambda self: None):
            reports_api._runner = reports.ReportRunner(cls.dir / "reports")

    @classmethod
    def tearDownClass(cls):
        from citar.server import reports_api
        reports_api._runner = cls.saved
        cls.client.close()
        shutil.rmtree(cls.dir, ignore_errors=True)

    def setUp(self):
        from citar.server import reports_api
        _db()
        self.headers = {}
        with db.session() as s:
            for handle, role in (("alice", "user"), ("bob", "user"), ("zara", "admin")):
                u = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                         password="a long enough phrase", role=role, status="active",
                                         email_verified=True)
                row, raw = sessions.start(s, u)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}", sessions.CSRF_HEADER: row.csrf_token,
                                        "origin": "https://citar.test"}
            self.rid = "20261011-120000-" + secrets.token_hex(2)
            folder = reports_api._runner.dir / self.rid
            folder.mkdir(parents=True)
            (folder / "report.html").write_text("<!doctype html><title>QA</title><p>the published figures</p>",
                                                encoding="utf-8")
            alice = accounts.by_handle(s, "alice")
            row = items.register(s, "report", self.rid, alice, "Published title", path=folder / "report.html")
            self.slug = row.slug

    def set(self, visibility: str, *, deleted: bool = False):
        with db.session() as s:
            row = items.get(s, "report", self.rid, deleted=True)
            row.visibility = visibility
            if deleted:
                row.deleted_at = row.updated_at

    def codes(self, key: str) -> dict:
        return {who: self.client.get(f"/r/{key}", headers=self.headers.get(who, {})).status_code
                for who in ("anonymous", "bob", "alice", "zara")}

    def test_a_link_report_by_its_key_and_never_by_its_id(self):
        self.set("link")
        r = self.client.get(f"/r/{self.slug}")
        self.assertEqual(r.status_code, 200)
        self.assertIn("the published figures", r.text)
        self.assertIn("noindex", r.headers["x-robots-tag"])
        self.assertIn("default-src 'none'", r.headers["content-security-policy"])
        # a report loads nothing from anywhere: no remote image could tell a third party who reads it
        framed = self.client.get(f"/api/reports/{self.rid}/html", headers=self.headers["alice"])
        for csp in (r.headers["content-security-policy"], framed.headers["content-security-policy"]):
            self.assertIn("img-src data:;", csp)
            self.assertNotIn("https:", csp)
            self.assertNotIn("http:", csp)
        self.assertEqual(set(self.codes(self.rid).values()), {404}, "a link report's id is no address")

    def test_a_global_report_by_its_id_or_its_key(self):
        self.set("global")
        self.assertEqual(set(self.codes(self.rid).values()), {200})
        self.assertEqual(self.client.get(f"/r/{self.slug}").status_code, 200)

    def test_anything_else_is_404_for_everyone(self):
        for visibility in ("private", "allowlist", "server"):
            self.set(visibility)
            for key in (self.rid, self.slug):
                self.assertEqual(set(self.codes(key).values()), {404}, f"{visibility} by {key}")
        self.set("global", deleted=True)
        self.assertEqual(self.client.get(f"/r/{self.rid}").status_code, 404, "a deleted report")
        self.assertEqual(self.client.get("/r/" + "x" * 200).status_code, 404)

    def test_another_kinds_key_opens_no_report(self):
        with db.session() as s:
            other = items.register(s, "map", "qa-map-" + secrets.token_hex(2), accounts.by_handle(s, "alice"), "map",
                                   visibility="link")
            key = other.slug
        self.assertEqual(self.client.get(f"/r/{key}").status_code, 404)

    def test_a_row_pointing_outside_the_reports_folder_reads_nothing(self):
        outside = self.dir / "elsewhere.html"
        outside.write_text("<p>not a report</p>", encoding="utf-8")
        with db.session() as s:
            row = items.get(s, "report", self.rid)
            row.path = outside.as_posix()
            row.visibility = "global"
        r = self.client.get(f"/r/{self.rid}")
        self.assertEqual(r.status_code, 200)
        self.assertNotIn("not a report", r.text)
        self.assertIn("not been generated yet", r.text)

    def test_its_manager_is_given_the_public_page_in_the_sharing_block(self):
        self.set("link")
        r = self.client.get(f"/api/items/report/{self.rid}/sharing", headers=self.headers["alice"])
        self.assertEqual(r.status_code, 200)
        self.assertTrue(r.json()["public_url"].endswith(f"/r/{self.slug}"))
        self.assertIn(f"#/reports/{self.rid}?k={self.slug}", r.json()["share_url"])
        self.set("global")
        r = self.client.get(f"/api/items/report/{self.rid}/sharing", headers=self.headers["alice"])
        self.assertTrue(r.json()["public_url"].endswith(f"/r/{self.rid}"))
        self.set("server")
        self.assertNotIn("public_url", self.client.get(f"/api/items/report/{self.rid}/sharing",
                                                       headers=self.headers["alice"]).json())

    def test_a_failed_reports_traceback_is_its_managers(self):
        from citar.server import reports_api
        folder = reports_api._runner.dir / self.rid
        (folder / "spec.json").write_text(json.dumps({"title": "t", "narrative": {"enabled": False}}), encoding="utf-8")
        (folder / "meta.json").write_text(json.dumps({"id": self.rid, "title": "t", "status": "failed", "created": 1,
                                                      "error": "boom", "trace": "Traceback: C:/server/path.py",
                                                      "owner_id": "u_x"}), encoding="utf-8")
        self.set("link")
        viewer = self.client.get(f"/api/reports/{self.rid}", headers={**self.headers["bob"], "x-citar-share-key": self.slug})
        self.assertEqual(viewer.status_code, 200, viewer.text)
        self.assertNotIn("trace", viewer.json()["meta"])
        self.assertNotIn("owner_id", viewer.json()["meta"])
        self.assertEqual(viewer.json()["meta"]["error"], "boom")
        listed = self.client.get("/api/reports", headers={**self.headers["bob"], "x-citar-share-key": self.slug}).json()
        self.assertNotIn("trace", next(m for m in listed if m["id"] == self.rid))
        owner = self.client.get(f"/api/reports/{self.rid}", headers=self.headers["alice"]).json()["meta"]
        self.assertIn("trace", owner)

    def test_a_reports_specification_is_its_managers_and_a_reader_gets_what_they_could_choose(self):
        """``GET /api/reports/{rid}``'s spec: whole for its owner and an administrator; for a member holding its link
        and a visitor, the scope's items and machines they may see and of the narrative only whether there is one,
        still a specification's shape ("Edit copy")."""
        from citar.server import reports_api
        with db.session() as s:
            alice = accounts.by_handle(s, "alice")
            rig = pool.create_server(s, alice, {**RIG, "name": "Alice private rig"}).id
            for gid, visibility in (("g-private-qa", "private"), ("g-global-qa", "global")):
                s.add(GameRow(id=gid, owner_id=alice.id, name=gid, kind="game", visibility=visibility,
                              data_sharing="pool", status="finished"))
            run = "run-qa-" + secrets.token_hex(2)
            items.register(s, "benchmark_run", run, alice, "Alice private run")
        spec = reports.normalize_spec({
            "title": "t", "scope": {"servers": [rig, "sv_host", "sv_dryrun"], "items": [
                {"kind": "game", "id": "g-private-qa", "name": "Alice private game"},
                {"kind": "game", "id": "g-global-qa", "name": "Alice global game"},
                {"kind": "benchmark_run", "id": run, "name": "Alice private run"},
                {"kind": "activity", "id": "game:g-private-qa", "name": "an activity"}]},
            "narrative": {"enabled": True, "server_id": rig, "model_id": "m_rig", "profile_id": "p_private",
                          "instructions": "Mention the secret plan."}})
        folder = reports_api._runner.dir / self.rid
        (folder / "spec.json").write_text(json.dumps(spec), encoding="utf-8")
        (folder / "meta.json").write_text(json.dumps({"id": self.rid, "title": "t", "status": "done", "created": 1}),
                                          encoding="utf-8")
        self.set("link")
        # the site's dry run is a row like any machine (P3.2.8): a member reads its name once it is shared with them
        tests.share_with_members("sv_dryrun")
        readers = (("bob", {**self.headers["bob"], "x-citar-share-key": self.slug}, ["sv_dryrun"]),
                   ("anonymous", {"x-citar-share-key": self.slug}, []))
        for who, headers, machines in readers:
            with self.subTest(who=who):
                r = self.client.get(f"/api/reports/{self.rid}", headers=headers)
                self.assertEqual(r.status_code, 200, r.text)
                got = r.json()["spec"]
                text = json.dumps(got)
                for secret in ("Mention the secret plan", rig, "m_rig", "p_private", "g-private-qa",
                               "Alice private game", run, "Alice private run", "an activity", "sv_host"):
                    self.assertNotIn(secret, text)
                self.assertEqual(got["scope"]["items"], [{"kind": "game", "id": "g-global-qa", "name": "Alice global game"}])
                self.assertEqual(got["scope"]["servers"], machines)
                self.assertEqual(got["narrative"], {"enabled": True, "server_id": None, "model_id": None,
                                                    "profile_id": None, "instructions": ""})
                self.assertEqual(reports.normalize_spec(got)["scope"], got["scope"], "still a specification")
        for manager in ("alice", "zara"):
            self.assertEqual(self.client.get(f"/api/reports/{self.rid}", headers=self.headers[manager]).json()["spec"],
                             spec, manager)

    def test_the_old_report_routes_are_gone(self):
        for method, path in (("GET", "/api/shared/reports"), ("GET", f"/api/shared/reports/{self.rid}"),
                             ("PUT", f"/api/shared/reports/{self.rid}/sharing")):
            self.assertIn(self.client.request(method, path, headers=self.headers["zara"]).status_code, (404, 405))


if __name__ == "__main__":
    unittest.main()

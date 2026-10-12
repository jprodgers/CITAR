"""The admin users page's routes, the audit log and the command line (design/phase3-features.md P3.2.7; package 3-04,
gates 3 and 4).

Gate 3: a PATCH changes each field and writes one audit row naming it; `null` removes a capability override; the last
administrator cannot demote, suspend or put themselves on probation; a member gets 404 on every admin route (with
the moderator, the probation member and a visitor's 401 in ``test_profiles.Routes``); the audit route filters and
pages. Gate 4: ``admin_cli`` ``set-caps``, ``set-limits``, ``set-trust`` and ``audit`` (and ``set-role`` and
``set-status``, now on the same rules), each against this module's temporary database and seen through the API.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="citar_admin_users_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from sqlalchemy import select

from citar import db
from citar.auth import accounts, audit, policy, sessions, user_admin
from citar.db.models import AuditLog, AuthSession, Base, User

ROOT = Path(__file__).resolve().parent.parent


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


class Base_(unittest.TestCase):
    """An administrator (ada), a member (sam), a moderator (moe), each signed in."""

    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient
        from citar.server.app import app
        cls.client = TestClient(app, base_url="https://citar.test")

    @classmethod
    def tearDownClass(cls):
        cls.client.close()

    def setUp(self):
        _reset()
        self.headers, self.ids = {None: {}}, {}
        with db.session() as s:
            for handle, role in (("ada", "admin"), ("sam", "user"), ("moe", "moderator")):
                self.sign_in(s, handle, role)

    def sign_in(self, s, handle: str, role: str = "user") -> None:
        user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com", role=role, status="active",
                                    email_verified=True)
        row, raw = sessions.start(s, user)
        self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}", sessions.CSRF_HEADER: row.csrf_token,
                                "origin": "https://citar.test"}
        self.ids[handle] = user.id

    def patch(self, handle: str, body: dict, who: str = "ada"):
        return self.client.patch(f"/api/admin/users/{self.ids[handle]}", json=body, headers=self.headers[who])

    def admin_view(self, handle: str) -> dict:
        r = self.client.get(f"/api/admin/users/{self.ids[handle]}", headers=self.headers["ada"])
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()

    def audit_rows(self, action: str = "user.updated") -> list:
        with db.session() as s:
            return list(s.scalars(select(AuditLog).where(AuditLog.action == action).order_by(AuditLog.id)))


# ---------------------------------------------------------------------------------------------------------- gate 3

class Patch(Base_):
    """PATCH /api/admin/users/{id}: every field, one audit row each, all checked before any changes."""

    #: Each field, a value to change it to, and how the account then reads.
    CASES = [("role", "moderator"), ("status", "probation"), ("caps", {"run_lab": True}), ("invite_quota", 5),
             ("max_concurrent_games", 7), ("max_seats_per_game", 12), ("trust_tier", "trusted"), ("listed", False)]

    def test_each_field_changes_with_one_audit_row_naming_it(self):
        for field, value in self.CASES:
            with self.subTest(field=field):
                before = len(self.audit_rows())
                r = self.patch("sam", {field: value})
                self.assertEqual(r.status_code, 200, r.text)
                self.assertEqual(r.json()["changed"], [field])
                self.assertEqual(r.json()["user"][field], value)
                rows = self.audit_rows()
                self.assertEqual(len(rows), before + 1)
                row = rows[-1]
                self.assertEqual((row.object_type, row.object_id, row.actor_id), ("user", self.ids["sam"],
                                                                                   self.ids["ada"]))
                self.assertEqual(row.detail["fields"], [field])
                self.assertEqual(row.detail["changes"][field]["to"], value)
                # the same again changes nothing and records nothing
                self.assertEqual(self.patch("sam", {field: value}).json()["changed"], [])
                self.assertEqual(len(self.audit_rows()), before + 1)

    def test_a_suspension_needs_its_reason_and_signs_the_account_out(self):
        r = self.patch("sam", {"status": "suspended"})
        self.assertEqual(r.status_code, 400)
        self.assertEqual(self.client.get("/api/auth/me", headers=self.headers["sam"]).json()["authenticated"], True)
        r = self.patch("sam", {"status": "suspended", "reason": "spam"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual((r.json()["user"]["status"], r.json()["user"]["suspended_reason"]), ("suspended", "spam"))
        self.assertEqual(self.audit_rows()[-1].detail["reason"], "spam")
        with db.session() as s:
            live = s.scalars(select(AuthSession).where(AuthSession.user_id == self.ids["sam"],
                                                       AuthSession.revoked_at.is_(None))).all()
        self.assertEqual(live, [])
        self.assertEqual(self.client.get("/api/auth/me", headers=self.headers["sam"]).json()["authenticated"], False)
        r = self.patch("sam", {"status": "active"})
        self.assertIsNone(r.json()["user"]["suspended_reason"])

    def test_null_removes_a_capability_override(self):
        self.patch("sam", {"caps": {"run_lab": True, "publish_global": True}})
        view = self.admin_view("sam")
        self.assertEqual(view["user"]["caps"], {"run_lab": True, "publish_global": True})
        self.assertTrue(view["user"]["capabilities"]["run_lab"])
        r = self.patch("sam", {"caps": {"run_lab": None}})
        self.assertEqual(r.json()["user"]["caps"], {"publish_global": True})
        self.assertFalse(r.json()["user"]["capabilities"]["run_lab"], "back to the role's default")
        self.assertFalse(self.admin_view("sam")["role_defaults"]["run_lab"])
        row = self.audit_rows()[-1]
        self.assertEqual(row.detail["changes"]["caps"], {"from": {"run_lab": True, "publish_global": True},
                                                         "to": {"publish_global": True}})
        # the member's own view of what they may do follows
        me = self.client.get("/api/auth/me", headers=self.headers["sam"]).json()
        self.assertTrue(me["capabilities"]["publish_global"])
        self.assertFalse(me["capabilities"]["run_lab"])

    def test_wrong_values_are_refused_and_change_nothing(self):
        bad = [{"role": "king"}, {"status": "deleted"}, {"status": "pending"}, {"caps": {"fly": True}},
               {"caps": {"run_lab": "yes"}}, {"caps": ["run_lab"]}, {"invite_quota": -1}, {"invite_quota": True},
               {"max_concurrent_games": 101}, {"max_seats_per_game": "8"}, {"trust_tier": "gold"},
               {"listed": "no"}, {"email": "x@example.com"}, {"password_hash": "x"},
               {"trust_tier": "trusted", "role": "king"}, {"trust_tier": "trusted", "reason": 5}]
        for body in bad:
            with self.subTest(body=body):
                self.assertEqual(self.patch("sam", body).status_code, 400)
        user = self.admin_view("sam")["user"]
        self.assertEqual((user["role"], user["trust_tier"]), ("user", "new"))
        self.assertEqual(self.audit_rows(), [])
        self.assertEqual(self.client.patch("/api/admin/users/u_nobody", json={"listed": False},
                                           headers=self.headers["ada"]).status_code, 404)

    def test_the_last_admin_cannot_demote_or_suspend_themselves(self):
        for body in ({"role": "moderator"}, {"role": "user"}, {"status": "suspended", "reason": "x"},
                     {"status": "probation"}):
            with self.subTest(body=body):
                r = self.patch("ada", body)
                self.assertEqual(r.status_code, 409, r.text)
                self.assertIn("only active administrator", r.json()["detail"])
        self.assertEqual(self.admin_view("ada")["user"]["role"], "admin")
        # with another administrator it may
        self.assertEqual(self.patch("sam", {"role": "admin"}).status_code, 200)
        self.assertEqual(self.patch("ada", {"role": "user"}).status_code, 200)
        self.assertEqual(self.client.get("/api/admin/users", headers=self.headers["ada"]).status_code, 404,
                         "no longer an administrator: the page is gone for them")
        # and now sam is the last one
        self.assertEqual(self.patch("sam", {"role": "user"}, who="sam").status_code, 409)

    def test_a_new_administrator_adopts_what_nobody_owns(self):
        with mock.patch("citar.auth.accounts.on_first_admin") as hook:
            self.patch("sam", {"role": "admin"})
        hook.assert_called_once()
        self.assertEqual(hook.call_args.args[1].id, self.ids["sam"])


class Listing(Base_):
    """GET /api/admin/users and /api/admin/users/{id}."""

    def test_every_account_filtered_and_paged(self):
        with db.session() as s:
            for i in range(55):
                accounts.create_user(s, handle=f"acct{i:02d}", email=f"acct{i:02d}@example.com", status="active")
            accounts.create_user(s, handle="hidden1", status="suspended", display_name="Quiet One").listed = False
        r = self.client.get("/api/admin/users", headers=self.headers["ada"])
        body = r.json()
        self.assertEqual((len(body["users"]), body["next"]), (50, 50))
        self.assertEqual(body["users"][0]["handle"], "hidden1", "newest first, hidden and suspended included")
        rest = self.client.get("/api/admin/users", params={"offset": 50}, headers=self.headers["ada"]).json()
        self.assertEqual(len(rest["users"]), 59 - 50)
        self.assertIsNone(rest["next"])

        def handles(**params):
            got = self.client.get("/api/admin/users", params=params, headers=self.headers["ada"])
            self.assertEqual(got.status_code, 200, got.text)
            return {u["handle"] for u in got.json()["users"]}
        self.assertEqual(handles(q="quiet"), {"hidden1"}, "by display name")
        self.assertEqual(handles(q="acct07@"), {"acct07"}, "by email")
        self.assertEqual(handles(status="suspended"), {"hidden1"})
        self.assertEqual(handles(role="moderator"), {"moe"})
        self.assertEqual(self.client.get("/api/admin/users", params={"role": "king"},
                                         headers=self.headers["ada"]).status_code, 400)

    def test_one_account_with_its_audit_rows(self):
        self.patch("sam", {"trust_tier": "provisional"})
        with db.session() as s:
            audit.record(s, "game.created", actor=s.get(User, self.ids["sam"]), object_type="game", object_id="g1")
            audit.record(s, "game.created", actor=s.get(User, self.ids["moe"]), object_type="game", object_id="g2")
        view = self.admin_view("sam")
        self.assertEqual(view["user"]["trust_tier"], "provisional")
        self.assertIn("last_seen_at", view["user"])
        actions = [(e["action"], e["object_id"]) for e in view["audit"]]
        self.assertEqual(actions[:2], [("game.created", "g1"), ("user.updated", self.ids["sam"])])
        self.assertNotIn(("game.created", "g2"), actions)


class AuditRoute(Base_):
    """GET /api/admin/audit: filters (actor, action, object type and id, before) and pages of at most 200."""

    def setUp(self):
        super().setUp()
        with db.session() as s:
            ada, sam = s.get(User, self.ids["ada"]), s.get(User, self.ids["sam"])
            for i in range(250):
                audit.record(s, "test.tick", actor=ada if i % 2 else sam, object_type="thing", object_id=f"t{i % 5}")
            audit.record(s, "user.updated", actor=ada, object_type="user", object_id=sam.id, fields=["listed"])

    def page(self, **params):
        r = self.client.get("/api/admin/audit", params=params, headers=self.headers["ada"])
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()

    def test_filters(self):
        self.assertEqual(len(self.page(actor="sam", action="test.tick", limit=200)["entries"]), 125)
        self.assertEqual(len(self.page(actor=self.ids["sam"], limit=200)["entries"]), 125, "by id too")
        self.assertEqual(self.page(actor="nobody")["entries"], [])
        upd = self.page(action="user.*")["entries"]
        self.assertEqual([e["action"] for e in upd], ["user.updated"])
        self.assertEqual(len(self.page(object_type="thing", object_id="t3", limit=200)["entries"]), 50)
        self.assertEqual(self.page(object_type="user", object_id=self.ids["sam"])["entries"][0]["detail"]["fields"],
                         ["listed"])

    def test_pages_of_at_most_two_hundred(self):
        first = self.page(limit=500)
        self.assertEqual(len(first["entries"]), 200)
        ids = [e["id"] for e in first["entries"]]
        self.assertEqual(ids, sorted(ids, reverse=True), "newest first")
        second = self.page(limit=500, before=first["next_before"])
        self.assertEqual(len(second["entries"]), 51)
        self.assertIsNone(second["next_before"])
        self.assertTrue(all(e["id"] < ids[-1] for e in second["entries"]))
        self.assertEqual(len(self.page()["entries"]), user_admin.AUDIT_PAGE)


# ---------------------------------------------------------------------------------------------------------- gate 4

class CommandLine(Base_):
    """``admin_cli``'s commands on this module's temporary database (its ``_setup``, which opens the configured one,
    is the only thing replaced), each change then read through the API."""

    def cli(self, *argv: str) -> tuple:
        from citar.server import admin_cli
        out = io.StringIO()
        with mock.patch.object(admin_cli, "_setup", lambda: None), contextlib.redirect_stdout(out):
            code = admin_cli.main(list(argv))
        return code, out.getvalue()

    def test_set_caps(self):
        code, out = self.cli("set-caps", "sam", "run_lab=on", "publish_global=off")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.admin_view("sam")["user"]["caps"], {"run_lab": True, "publish_global": False})
        me = self.client.get("/api/auth/me", headers=self.headers["sam"]).json()
        self.assertTrue(me["capabilities"]["run_lab"])
        code, out = self.cli("set-caps", "sam", "run_lab=default")
        self.assertEqual((code, self.admin_view("sam")["user"]["caps"]), (0, {"publish_global": False}))
        self.assertEqual(self.cli("set-caps", "sam", "fly=on")[0], 1)
        self.assertEqual(self.cli("set-caps", "sam", "run_lab")[0], 1)
        self.assertEqual(self.cli("set-caps", "nobody", "run_lab=on")[0], 1)
        row = self.audit_rows()[-1]
        self.assertEqual((row.actor_label, row.detail["fields"]), ("admin_cli", ["caps"]))

    def test_set_limits(self):
        code, out = self.cli("set-limits", "sam", "--games", "4", "--seats", "8", "--invites", "2")
        self.assertEqual(code, 0, out)
        user = self.admin_view("sam")["user"]
        self.assertEqual((user["max_concurrent_games"], user["max_seats_per_game"], user["invite_quota"]), (4, 8, 2))
        self.assertEqual(self.audit_rows()[-1].detail["fields"],
                         ["invite_quota", "max_concurrent_games", "max_seats_per_game"])
        self.assertEqual(self.cli("set-limits", "sam")[0], 1, "nothing named")
        self.assertEqual(self.cli("set-limits", "sam", "--games", "1000")[0], 1)

    def test_set_trust(self):
        self.assertEqual(self.cli("set-trust", "sam", "quarantined")[0], 0)
        self.assertEqual(self.admin_view("sam")["user"]["trust_tier"], "quarantined")
        with self.assertRaises(SystemExit):
            with contextlib.redirect_stderr(io.StringIO()):
                self.cli("set-trust", "sam", "gold")

    def test_set_role_and_set_status_keep_the_last_admin(self):
        code, out = self.cli("set-role", "ada", "user")
        self.assertEqual(code, 1)
        self.assertIn("only active administrator", out)
        self.assertEqual(self.cli("set-status", "sam", "suspended")[0], 1, "a suspension needs its reason")
        self.assertEqual(self.cli("set-status", "sam", "suspended", "--reason", "spam")[0], 0)
        user = self.admin_view("sam")["user"]
        self.assertEqual((user["status"], user["suspended_reason"]), ("suspended", "spam"))
        self.assertEqual(self.cli("set-role", "moe", "admin")[0], 0)
        self.assertEqual(self.admin_view("moe")["user"]["role"], "admin")

    def test_audit(self):
        self.cli("set-trust", "sam", "trusted")
        self.patch("moe", {"listed": False})
        code, out = self.cli("audit", "--action", "user.*", "--json")
        self.assertEqual(code, 0)
        page = json.loads(out)
        self.assertEqual([(e["actor"], e["object_id"]) for e in page["entries"]],
                         [("ada (admin)", self.ids["moe"]), ("admin_cli", self.ids["sam"])])
        code, out = self.cli("audit", "--actor", "ada")
        self.assertEqual(code, 0)
        self.assertIn("user.updated", out)
        self.assertNotIn("admin_cli", out)
        self.assertEqual(self.cli("audit", "--object-id", "nothing")[1].strip(), "No entries.")
        # the API reads the same entries
        api = self.client.get("/api/admin/audit", params={"action": "user.*"}, headers=self.headers["ada"]).json()
        self.assertEqual([e["id"] for e in api["entries"]], [e["id"] for e in page["entries"]])

    def test_the_real_command_on_a_database_of_its_own(self):
        """The entry point as an operator runs it: a child process, its own temporary database, ``create-user`` then
        ``set-trust`` then ``audit --json``."""
        folder = Path(tempfile.mkdtemp(prefix="citar_admin_cli_"))
        env = {**os.environ, "CITAR_DATA_DIR": str(folder), "CITAR_DB_URL": "sqlite:///" + (folder / "c.db").as_posix(),
               "PYTHONPATH": str(ROOT)}
        try:
            def run(*args, stdin=None):
                return subprocess.run([sys.executable, "-m", "citar.server.admin_cli", *args], cwd=str(ROOT), env=env,
                                      input=stdin, capture_output=True, text=True, timeout=180)
            made = run("create-user", "--handle", "olga", "--email", "olga@example.com", "--password-stdin",
                       stdin="a long enough phrase\n")
            self.assertEqual(made.returncode, 0, made.stdout + made.stderr)
            self.assertEqual(run("set-trust", "olga", "trusted").returncode, 0)
            page = json.loads(run("audit", "--json").stdout)
            self.assertEqual(page["entries"][0]["detail"]["changes"], {"trust_tier": {"from": "new", "to": "trusted"}})
        finally:
            shutil.rmtree(folder, ignore_errors=True)


def tearDownModule():
    db.dispose()
    shutil.rmtree(_TMP, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()

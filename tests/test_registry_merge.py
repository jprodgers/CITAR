"""The registry merge: every machine a row of the ``servers`` table (design/phase3-features.md P3.2.8; package 3-05's
gates 1, 2, 3 and 5; gate 4, revocation on a real helper, is ``test_worker.Revocation``).

Gate 1: a fixture registry file (``tests/data/registry/servers.json``: the host's LM Studio with models and load
profiles, an Anthropic server whose key is in the file keystore, a LAN machine, a dry-run test server, electricity
plans) imports to rows under its own ids, equal to ``normalize_server()`` of its entries, ``max_parallel`` carried
into ``max_concurrent``; the file is renamed; a second boot imports nothing; keys still resolve; the host is the host
row. A fresh server-mode database imports nothing at boot and says why; ``create-admin`` then imports once.
Gate 2: every public function of ``servers.py`` answers as the file-backed code did (``tests/registry_calls.py``'s
table, recorded before the change), and ``costing`` prices a synthetic ledger to the same totals.
Gate 3: the machine routes' gates, by a member on their own helper machine (whose stored address is the host's LM
Studio) with every way out watched (``NoOutbound``: the network, processes, the OpenAI and Anthropic clients, the
server's environment; and LM Studio's SDK), by another member, an administrator and a visitor (P3.9.1).
Gate 5: a suite naming the host and a helper machine resolves both through their rows, and a run's jobs start.
Plus: the seeded test database against ``TEST_REGISTRY``, and ``servers.register_offline`` (``citar setup --local``).
"""
import tests  # temporary saves folder and server registry; must be imported before citar
import contextlib
import copy
import io
import json
import logging
import os
import shutil
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

from tests import registry_calls as RC
from tests.test_seat_blocks import SECRET_VAR, NoOutbound

_TMP = Path(tempfile.mkdtemp(prefix="citar_registry_merge_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from sqlalchemy import select

from citar import db, keystore, pool, servers, settings
from citar.auth import accounts, policy, sessions
from citar.db.models import AuditLog, Base, Server, Setting, User, WorkerToken
from citar.pool import registry_import

settings.reset()


def _fresh(seed: bool = False):
    """This module's database, empty (``seed``: with the test registry, as every other module's)."""
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    (db.create_all if seed else db.create_all.original)()
    policy.invalidate()
    servers.invalidate()


def _admin(s, handle="ada") -> User:
    """An active administrator, made as ``admin_cli create-admin`` makes one (``on_first_admin`` included)."""
    user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com", password="a long enough phrase",
                                role="admin", status="active", email_verified=True)
    accounts.on_first_admin(s, user)
    return user


def _strip(sv: dict) -> dict:
    """A server dict without what is stamped at each write."""
    return {k: v for k, v in sv.items() if k not in RC.VOLATILE}


class RegistryFile(unittest.TestCase):
    """A temporary config directory holding the fixture as ``servers.json``, ``servers.PATH`` pointed at it."""

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="citar_registry_file_"))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.file = self.dir / "servers.json"
        shutil.copy(RC.FIXTURE, self.file)
        patch = mock.patch.object(servers, "PATH", self.file)
        patch.start()
        self.addCleanup(patch.stop)
        self.entries = {e["id"]: e for e in json.loads(RC.FIXTURE.read_text(encoding="utf-8"))["servers"]}
        keys = mock.patch.dict(os.environ, {"CITAR_KEYS_FILE": str(self.dir / "keys.enc"),
                                            "CITAR_TEST_LAN_KEY": RC.LAN_KEY})
        keys.start()
        self.addCleanup(keys.stop)
        keystore.unlock(RC.PASSPHRASE)
        self.addCleanup(keystore.lock)
        keystore.store("sv_claude", "file", RC.CLAUDE_KEY)
        _fresh()

    def renamed(self) -> list:
        return sorted(p.name for p in self.dir.glob("servers.json.imported-*"))


# ---------------------------------------------------------------------------------------------------------- gate 1

class Import(RegistryFile):
    def test_the_file_becomes_rows_under_its_own_ids(self):
        with db.session() as s:
            ada = _admin(s)
            ada_id = ada.id
        self.assertFalse(self.file.exists(), "the file is renamed once the import commits")
        self.assertEqual(len(self.renamed()), 1)
        with db.session() as s:
            rows = {r.id: r for r in s.scalars(select(Server))}
            self.assertEqual(set(rows), set(self.entries))
            for sid, entry in self.entries.items():
                row = rows[sid]
                with self.subTest(server=sid):
                    self.assertEqual(_strip(servers.get(sid)), _strip(servers.normalize_server(entry)))
                    self.assertEqual(row.max_concurrent, entry["connection"]["max_parallel"])
                    self.assertNotIn("max_parallel", row.config["connection"], "the one number is the row's")
                    self.assertEqual((row.reach, row.group_id, row.owner_id, row.enabled), ("direct", None, ada_id, True))
                    self.assertEqual(row.is_host, sid == "sv_host")
                    self.assertEqual(row.server_resource, sid in ("sv_host", "sv_claude"))
            marker = policy.marker(s, "registry_imported")
            self.assertEqual(sorted(marker["servers"]), sorted(self.entries))
            entries = list(s.scalars(select(AuditLog).where(AuditLog.action == "migration.registry_imported")))
            self.assertEqual(len(entries), 1)
            self.assertEqual(entries[0].actor_id, ada_id)
        reg = servers.load()
        fixture = json.loads(RC.FIXTURE.read_text(encoding="utf-8"))
        self.assertEqual((reg["currency"], reg["currency_symbol"], reg["host_server_id"]), ("EUR", "€", "sv_host"))
        self.assertEqual(reg["electricity_plans"], [servers.normalize_plan(p) for p in fixture["electricity_plans"]])
        self.assertEqual(servers.host()["id"], "sv_host")
        self.assertEqual(servers.resolve_llm({"server_id": "sv_claude", "model_id": "m_opus"})["api_key"],
                         RC.CLAUDE_KEY, "the keystore's entries resolve under the same ids")
        self.assertEqual(servers.resolve_llm({"server_id": "sv_lan", "model_id": "m_llama"})["api_key"], RC.LAN_KEY)

    def test_a_second_start_imports_nothing(self):
        with db.session() as s:
            _admin(s)
        with db.session() as s:
            s.get(Server, "sv_test").name = "Renamed since"
        shutil.copy(RC.FIXTURE, self.file)                      # the file back, as an old backup restored would be
        with self.assertLogs("citar.registry_import", logging.WARNING) as logged:
            self.assertIsNone(registry_import.on_boot())
        self.assertIn("not read any more", "\n".join(logged.output))
        self.assertTrue(self.file.exists(), "a file that reappears is left for its owner, never imported again")
        self.assertEqual(servers.get("sv_test")["name"], "Renamed since")
        with db.session() as s:
            self.assertEqual(len(list(s.scalars(select(AuditLog).where(
                AuditLog.action == "migration.registry_imported")))), 1)

    def test_at_boot_with_an_administrator(self):
        with db.session() as s:
            accounts.create_user(s, handle="ada", email="ada@example.com", password="a long enough phrase",
                                 role="admin", status="active", email_verified=True)     # no on_first_admin
        self.assertEqual(servers.list_servers(), [])
        result = registry_import.on_boot()
        self.assertEqual(sorted(result["imported"]), sorted(self.entries))
        self.assertFalse(self.file.exists())
        self.assertEqual({sv["id"] for sv in servers.list_servers()}, set(self.entries))
        self.assertIsNone(registry_import.on_boot(), "once")

    def test_a_fresh_server_mode_database_waits_for_its_first_administrator(self):
        with self.assertLogs("citar.registry_import", logging.WARNING) as logged:
            self.assertIsNone(registry_import.on_boot())
        self.assertIn("there is no administrator", "\n".join(logged.output))
        self.assertTrue(self.file.exists())
        self.assertEqual(servers.list_servers(), [], "until then no machine is usable")
        from citar.server import admin_cli
        out = io.StringIO()
        with mock.patch.object(admin_cli, "_setup", lambda: None), \
                mock.patch.object(admin_cli, "_prompt_password", lambda *a, **k: "a long enough phrase"), \
                contextlib.redirect_stdout(out):
            self.assertEqual(admin_cli.main(["create-admin", "--handle", "ada", "--email", "ada@example.com"]), 0)
        self.assertFalse(self.file.exists())
        self.assertEqual({sv["id"] for sv in servers.list_servers()}, set(self.entries))
        with db.session() as s:
            ada = accounts.by_handle(s, "ada")
            self.assertEqual({r.owner_id for r in s.scalars(select(Server))}, {ada.id})
            second = accounts.create_user(s, handle="bea", email="bea@example.com", password="a long enough phrase",
                                          role="admin", status="active", email_verified=True)
            accounts.on_first_admin(s, second)
        with db.session() as s:
            self.assertEqual(len(list(s.scalars(select(AuditLog).where(
                AuditLog.action == "migration.registry_imported")))), 1, "a later administrator imports nothing")

    def test_no_file_imports_the_first_runs_registry(self):
        self.file.unlink()
        with mock.patch("citar.hwinfo.collect", side_effect=RuntimeError("no scan in tests")), \
                mock.patch("citar.server.lmstudio.lms_path", lambda: None):
            with db.session() as s:
                _admin(s)
        ids = {sv["id"] for sv in servers.list_servers()}
        self.assertEqual(ids, {"sv_host", "sv_anthropic", "sv_dryrun"})
        self.assertEqual(servers.host()["id"], "sv_host")
        self.assertEqual(self.renamed(), [])

    def test_a_file_that_does_not_read_is_left_and_tried_again(self):
        self.file.write_text("{ not json", encoding="utf-8")
        with db.session() as s:
            _admin(s)                                       # the administrator is made all the same
        with db.session() as s:
            self.assertIsNotNone(accounts.by_handle(s, "ada"))
            self.assertIsNone(policy.marker(s, "registry_imported"))
        self.assertTrue(self.file.exists())
        self.assertEqual(servers.list_servers(), [])
        shutil.copy(RC.FIXTURE, self.file)
        self.assertEqual(sorted(registry_import.on_boot()["imported"]), sorted(self.entries))

    def test_a_rename_that_fails_leaves_the_file_never_read_again(self):
        real = os.replace

        def refuse(src, dst):
            if str(src) == str(self.file):
                raise PermissionError("held by another program")
            return real(src, dst)
        with mock.patch.object(registry_import.os, "replace", refuse):
            with self.assertLogs("citar.registry_import", logging.WARNING):
                with db.session() as s:
                    _admin(s)
        self.assertTrue(self.file.exists())
        self.assertEqual({sv["id"] for sv in servers.list_servers()}, set(self.entries))
        self.assertIsNone(registry_import.on_boot())

    def test_an_import_rolled_back_with_its_administrator_leaves_the_file(self):
        with self.assertRaises(RuntimeError):
            with db.session() as s:
                _admin(s)
                raise RuntimeError("the transaction that made the administrator failed")
        self.assertTrue(self.file.exists(), "renamed only once the import commits")
        self.assertEqual(servers.list_servers(), [])

    def test_another_instances_registry_file_is_never_called_imported(self):
        """A checkout and an installed CITAR can share one database with two configuration folders: once one's file
        is imported, the other's is not, and the boot says so rather than advise removing it."""
        with db.session() as s:
            _admin(s)
        other = self.dir / "other-config"
        other.mkdir()
        shutil.copy(RC.FIXTURE, other / "servers.json")
        with mock.patch.object(servers, "PATH", other / "servers.json"):
            with self.assertLogs("citar.registry_import", logging.WARNING) as logged:
                self.assertIsNone(registry_import.on_boot())
        said = "\n".join(logged.output)
        self.assertIn("was NOT imported", said)
        self.assertIn(str(self.file), said, "it names the file the machines came from")
        self.assertNotIn("Remove or rename", said)
        self.assertTrue((other / "servers.json").exists())

    def test_the_old_pool_imports_copies_are_named_and_kept(self):
        """A database where the pool's old import route (retired) copied the registry's machines under new ids: the
        machines still come in under their own ids (what seats, suites, the ledger and the keystore name), and the
        copies are named in the log and the audit row for an administrator to remove, not removed."""
        with db.session() as s:
            ada = accounts.create_user(s, handle="ada", email="ada@example.com", password="a long enough phrase",
                                       role="admin", status="active", email_verified=True)
            s.add(Server(id="sv_old_copy", owner_id=ada.id, name="Desktop on the LAN", kind="owned",
                         provider="openai_compatible", reach="direct", config=copy.deepcopy(self.entries["sv_lan"])))
        with self.assertLogs("citar.registry_import", logging.WARNING) as logged:
            result = registry_import.on_boot()
        self.assertEqual(result["earlier_copies"], {"sv_lan": ["sv_old_copy"]})
        self.assertIn("sv_old_copy", "\n".join(logged.output))
        self.assertEqual({sv["id"] for sv in servers.list_servers()}, set(self.entries) | {"sv_old_copy"})
        with db.session() as s:
            row = s.scalar(select(AuditLog).where(AuditLog.action == "migration.registry_imported"))
            self.assertEqual(row.detail["earlier_copies"], {"sv_lan": ["sv_old_copy"]})

    def test_the_first_runs_registry_is_made_once_the_administrator_is_committed(self):
        """With no file, the import makes the first run's registry, a scan of this computer: it waits for the
        transaction that made the administrator to commit, so nothing holds the database's write lock meanwhile."""
        self.file.unlink()
        real = servers.bootstrap
        seen = {}

        def bootstrap():
            t0 = time.perf_counter()
            with db.session() as s:
                seen["admin committed"] = accounts.by_handle(s, "ada") is not None
                s.add(Setting(key="a_write_from_another_connection", value={"v": True}))
            seen["write took"] = time.perf_counter() - t0
            return real()
        with mock.patch("citar.hwinfo.collect", side_effect=RuntimeError("no scan in tests")), \
                mock.patch("citar.server.lmstudio.lms_path", lambda: None), \
                mock.patch.object(servers, "bootstrap", bootstrap):
            with db.session() as s:
                _admin(s)
                self.assertEqual(seen, {}, "not inside the administrator's transaction")
        self.assertTrue(seen["admin committed"])
        self.assertLess(seen["write took"], 2.0, "another writer is not kept waiting")
        self.assertEqual({sv["id"] for sv in servers.list_servers()}, {"sv_host", "sv_anthropic", "sv_dryrun"})
        with db.session() as s:
            self.assertEqual(policy.marker(s, "registry_imported")["source"], "first run")


# ---------------------------------------------------------------------------------------------------------- gate 2

class Answers(RegistryFile):
    """``tests/registry_calls.py``'s table, asked of the rows the import made, against what the file answered."""

    def test_every_function_answers_as_before(self):
        with db.session() as s:
            _admin(s)
        expected = RC.load_expected()
        got = RC.phase_a()
        for key in sorted(expected["phase_a"]):
            with self.subTest(call=key):
                self.assertEqual(json.dumps(got.get(key), sort_keys=True), json.dumps(expected["phase_a"][key], sort_keys=True))
        with db.session() as s:
            bob = accounts.create_user(s, handle="bob", email="bob@example.com", password="a long enough phrase",
                                       status="active", email_verified=True)
            bob.tz = RC.HELPER["owner_tz"]
            s.add(Server(id=RC.HELPER["id"], owner_id=bob.id, name=RC.HELPER["name"], kind=RC.HELPER["kind"],
                         provider=RC.HELPER["provider"], reach="worker", max_concurrent=RC.HELPER["max_concurrent"],
                         config=copy.deepcopy(RC.HELPER["config"])))
        got = RC.phase_b()
        for key in sorted(expected["phase_b"]):
            with self.subTest(call=key):
                self.assertEqual(json.dumps(got.get(key), sort_keys=True), json.dumps(expected["phase_b"][key], sort_keys=True))
        self.assertIn("costing", expected["phase_a"], "costing priced the synthetic ledger to the same totals")

    def test_a_change_in_this_process_is_read_at_once(self):
        with db.session() as s:
            _admin(s)
        self.assertEqual(servers.get("sv_lan")["name"], "Desktop on the LAN")
        with db.session() as s:
            s.get(Server, "sv_lan").name = "Desk"
        self.assertEqual(servers.server_name("sv_lan"), "Desk")
        with db.session() as s:
            s.get(User, s.scalar(select(Server.owner_id).where(Server.id == "sv_lan"))).tz = "Asia/Tokyo"
        self.assertEqual(servers.machine("sv_lan")["owner_tz"], "Asia/Tokyo")
        servers.upsert_plan({"id": "ep_new", "name": "New"})
        self.assertEqual(servers.plan("ep_new")["name"], "New")
        with self.assertRaises(servers.ServerError):
            servers.delete_plan("ep_home")              # the host's and the LAN machine's costs name it

    def test_a_tool_reads_the_servers_database_without_opening_it(self):
        """A command-line tool (``citar lab``) has no database of its own open: it reads the server's machines from
        the file it finds, without configuring its own (which would start the games index writing there)."""
        with db.session() as s:
            _admin(s)
        db.dispose()
        servers.invalidate()
        try:
            self.assertFalse(db.configured())
            self.assertEqual(servers.host()["id"], "sv_host")
            self.assertFalse(db.configured())
        finally:
            db.configure(os.environ["CITAR_DB_URL"])


# ---------------------------------------------------------------------------------------------------------- seed

class Seed(unittest.TestCase):
    """The suite's machines: built through the import in every database ``create_all`` makes, equal to the old
    ``TEST_REGISTRY`` file's."""

    def test_the_seed_is_the_old_test_registry(self):
        _fresh(seed=True)
        reg = servers.load()
        self.assertEqual(reg["host_server_id"], tests.TEST_REGISTRY["host_server_id"])
        self.assertEqual((reg["currency"], reg["currency_symbol"]), ("USD", "$"))
        self.assertEqual(reg["electricity_plans"],
                         [servers.normalize_plan(p) for p in tests.TEST_REGISTRY["electricity_plans"]])
        self.assertEqual([sv["id"] for sv in reg["servers"]], [e["id"] for e in tests.TEST_REGISTRY["servers"]])

        def comparable(sv):
            # what normalize_server draws when the entry has none: the components' ids, the creation time
            return {**{k: v for k, v in _strip(sv).items() if k != "created"},
                    "components": [{k: v for k, v in c.items() if k != "id"} for c in sv["components"]]}
        for entry in tests.TEST_REGISTRY["servers"]:
            with self.subTest(server=entry["id"]):
                self.assertEqual(comparable(servers.get(entry["id"])), comparable(servers.normalize_server(entry)))
        with db.session() as s:
            self.assertIsNotNone(policy.marker(s, "registry_imported"))
            self.assertEqual(s.scalar(select(AuditLog.id).limit(1)), None, "the seed writes no audit row")


# ---------------------------------------------------------------------------------------------------------- gate 3

class Routes(unittest.TestCase):
    """The machine routes by alice on her own helper machine (its stored address the host's LM Studio), by bob,
    a stranger to it, by zara, an administrator, and by a visitor."""

    LOCAL = "http://localhost:1234/v1"

    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        _fresh(seed=True)
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for handle, role in (("alice", "user"), ("bob", "user"), ("zara", "admin")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
            alice = s.get(User, self.ids["alice"])
            self.helper = pool.create_server(s, alice, {"name": "Alice GPU", "kind": "owned",
                                                        "connection": {"provider": "lmstudio", "base_url": self.LOCAL},
                                                        "models": [{"id": "m_a", "key": "alice/model"}]}).id
        self.assertEqual(servers.machine(self.helper)["config"]["connection"]["base_url"], self.LOCAL)
        keys = Path(tempfile.mkdtemp(prefix="citar_merge_keys_"))
        self.addCleanup(shutil.rmtree, keys, ignore_errors=True)
        patch = mock.patch.dict(os.environ, {"CITAR_KEYS_FILE": str(keys / "keys.enc")})
        patch.start()
        self.addCleanup(patch.stop)
        self.addCleanup(keystore.lock)
        self.watch = NoOutbound().__enter__()
        sdk = mock.patch.object(servers, "sdk", self._sdk)
        sdk.start()
        self.addCleanup(sdk.stop)

    def _sdk(self):
        self.watch.calls.append(("LM Studio's SDK", ()))
        raise RuntimeError("LM Studio's SDK was loaded")

    def tearDown(self):
        self.watch.__exit__(None, None, None)

    def call(self, who, method, path, body=None):
        headers = self.headers.get(who, {"origin": "https://citar.test"})
        return self.client.request(method, path, json=body, headers=headers)

    def quiet(self):
        self.assertEqual(self.watch.calls, [], "nothing dialled, spawned, loaded or read")

    def test_a_members_helper_machine_never_reaches_the_host(self):
        for method, path, body in (("POST", "detect", None), ("GET", "state", None),
                                   ("POST", "load", {"model_id": "m_a"}), ("POST", "unload", {"model_id": "m_a"}),
                                   ("GET", "loading", None), ("POST", "loaded-config", {"model_id": "m_a"}),
                                   ("POST", "estimate", {"model_id": "m_a"}), ("POST", "collect", None)):
            for who, code in (("alice", 404), ("bob", 404), ("anon", 401), ("zara", 404)):
                with self.subTest(route=path, who=who):
                    self.assertEqual(self.call(who, method, f"/api/servers/{self.helper}/{path}", body).status_code,
                                     code)
        self.quiet()

    def test_the_host_wide_routes_are_administrators(self):
        with mock.patch.object(servers, "lmstudio_devices", lambda fresh=False: {"peers": []}):
            for method, path, body, admin_code in (("GET", "/api/lmstudio/devices", None, 200),
                                                   ("PUT", "/api/servers-settings", {"currency": "eur"}, 200),
                                                   ("POST", "/api/keys/unlock", {"passphrase": "x"}, 200),
                                                   ("POST", "/api/keys/lock", None, 200),
                                                   ("POST", "/api/electricity-plans", {"name": "Mine"}, 200),
                                                   ("DELETE", "/api/electricity-plans/ep_nope", None, 200)):
                for who, code in (("alice", 404), ("anon", 401), ("zara", admin_code)):
                    with self.subTest(route=path, who=who):
                        self.assertEqual(self.call(who, method, path, body).status_code, code)
        self.assertEqual(servers.load()["currency"], "EUR")
        self.quiet()

    def test_the_key_routes_env_backend_is_an_administrators(self):
        dry = self.call("alice", "POST", "/api/servers", {"name": "Alice's dry run", "kind": "test",
                                                          "connection": {"provider": "dryrun"}}).json()["id"]
        r = self.call("alice", "POST", f"/api/servers/{dry}/key", {"backend": "env", "env": SECRET_VAR})
        self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(self.call("bob", "POST", f"/api/servers/{dry}/key",
                                   {"backend": "file", "key": "x"}).status_code, 404)
        self.assertEqual(self.call("anon", "DELETE", f"/api/servers/{dry}/key").status_code, 401)
        r = self.call("alice", "POST", f"/api/servers/{dry}/key", {"backend": "none"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.call("alice", "DELETE", f"/api/servers/{dry}/key").status_code, 200)
        self.quiet()
        r = self.call("zara", "POST", "/api/servers/sv_api/key", {"backend": "env", "env": "CITAR_TEST_NO_KEY"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(r.json()["key_status"]["backend"], "env")

    def test_a_helper_machine_has_no_key_on_the_server(self):
        """The server never reads a helper machine's key, so the key routes are 404 on one, for its owner and for an
        administrator alike, and nothing is stored."""
        keystore.unlock("a passphrase for this test")
        for who in ("alice", "zara"):
            with self.subTest(who=who):
                r = self.call(who, "POST", f"/api/servers/{self.helper}/key", {"backend": "file", "key": "sk-member-secret"})
                self.assertEqual(r.status_code, 404, r.text)
                self.assertEqual(self.call(who, "DELETE", f"/api/servers/{self.helper}/key").status_code, 404)
        self.assertIsNone(keystore.get({"id": self.helper, "connection": {"key": {"backend": "file"}}}))
        self.assertEqual(servers.stored_key(servers.machine(self.helper)["config"]).get("backend"), "none")
        self.quiet()

    def test_a_removed_machine_takes_its_stored_key_with_it(self):
        """A key stored while a machine was one the server dials stays where the row says, whatever the machine's
        registry view shows once it is a helper machine (no key): saving that view keeps it, the key routes no longer
        reach it, and deleting the machine, through either family of routes, removes it from the keystore."""
        keystore.unlock("a passphrase for this test")
        hw = {"format": "citar-hardware", "cpu": {"model": "Ryzen", "threads": 16}}
        for route in ("/api/servers/{}", "/api/pool/servers/{}"):
            with self.subTest(route=route):
                made = self.call("zara", "POST", "/api/servers", {
                    "name": "Keyed box", "kind": "owned",
                    "connection": {"provider": "openai_compatible", "base_url": "http://192.168.1.9:8080/v1"}})
                sid = made.json()["id"]
                r = self.call("zara", "POST", f"/api/servers/{sid}/key", {"backend": "file", "key": "sk-site-secret-1"})
                self.assertEqual(r.status_code, 200, r.text)
                view = {"id": sid, "connection": {"key": {"backend": "file"}}}
                self.assertEqual(keystore.get(view), "sk-site-secret-1")
                r = self.call("zara", "POST", "/api/servers", {**servers.get(sid), "reach": "worker"})
                self.assertEqual((r.status_code, r.json()["reach"]), (200, "worker"), r.text)
                self.assertEqual(servers.get(sid)["connection"]["key"]["backend"], "none", "the view shows no key")
                self.assertEqual(self.call("zara", "POST", f"/api/servers/{sid}/hardware", hw).status_code, 200)
                self.assertEqual(self.call("zara", "POST", "/api/servers",
                                           {**servers.get(sid), "notes": "saved from its view"}).status_code, 200)
                stored = servers.machine(sid)["config"]["connection"]
                self.assertEqual((stored["key"]["backend"], stored["base_url"]), ("file", "http://192.168.1.9:8080/v1"),
                                 "saving the view keeps what the row stores")
                self.assertEqual(self.call("zara", "DELETE", f"/api/servers/{sid}/key").status_code, 404)
                self.assertEqual(self.call("zara", "DELETE", route.format(sid)).status_code, 200)
                self.assertIsNone(servers.find(sid))
                self.assertIsNone(keystore.get(view), "the key went with its machine")

    def test_a_machine_of_the_wrong_shape_is_a_400_on_every_route_that_saves_one(self):
        """Every part of a machine body arrives from a browser: the wrong JSON shape anywhere is the caller's 400 on
        each route that creates or saves a machine, for a member and an administrator alike, and nothing is written."""
        bad = {"connection a string": {"connection": "lmstudio"},
               "connection.key a string": {"connection": {"provider": "dryrun", "key": "sk"}},
               "power a string": {"power": "300 W"}, "models: [1]": {"models": [1]},
               "costs a string": {"costs": "free"}, "costs: [1]": {"costs": [1]},
               "a plan id that is a list": {"costs": [{"electricity_plan_id": ["ep_home"]}]},
               "components a string": {"components": "PC"},
               "restricted_hours a string": {"restricted_hours": "nights"},
               "windows: [1]": {"restricted_hours": {"windows": [1]}},
               "a window's days a number": {"restricted_hours": {"windows": [{"days": 5}]}},
               "inference a list": {"models": [{"key": "m", "inference": ["auto"]}]},
               "profiles: [1]": {"models": [{"key": "m", "profiles": [1]}]}}
        with db.session() as s:
            rows = s.query(Server).count()
        stored = servers.machine(self.helper)["config"]
        mine = servers.get(self.helper)
        for name, part in bad.items():
            for who in ("alice", "zara"):
                for method, path, body in (("POST", "/api/servers", {"name": "Bad", "kind": "test", **part}),
                                           ("POST", "/api/servers", {**mine, **part}),
                                           ("POST", "/api/pool/servers", {"config": {"name": "Bad", **part}}),
                                           ("PUT", f"/api/pool/servers/{self.helper}", {"config": {**stored, **part}})):
                    with self.subTest(body=name, who=who, route=f"{method} {path}"):
                        r = self.call(who, method, path, body)
                        self.assertEqual(r.status_code, 400, r.text)
        with db.session() as s:
            self.assertEqual(s.query(Server).count(), rows, "no machine was made")
        self.assertEqual(servers.machine(self.helper)["config"], stored, "and hers is as it was")
        self.quiet()

    def test_listings_name_only_machines_the_caller_may_see(self):
        """The lobby's availability and the quiet hours in the benchmark scheduler's status and the phone site's
        overview name another member's private machine, and the site's machines nobody shared, to nobody but their
        owner and administrators (P3.9.1: they are 404 to the rest)."""
        from citar.server import app as appmod
        from citar.server.benchmarks import BenchmarkScheduler
        always = {"enabled": True, "windows": [{"days": list(range(7)), "start": "00:00", "end": "12:00"},
                                               {"days": list(range(7)), "start": "12:00", "end": "00:00"}]}
        with db.session() as s:
            for sid in ("sv_gpu", self.helper):
                row = s.get(Server, sid)
                row.config = {**row.config, "restricted_hours": always}
        saved = appmod.scheduler
        appmod.scheduler = BenchmarkScheduler(appmod.manager, Path(tempfile.mkdtemp(prefix="citar_merge_status_")),
                                              autostart=False)
        try:
            def named(who):
                avail = self.call(who, "GET", "/api/pool/availability").json()
                return ({e["id"] for e in avail["usable"] + avail["blocked"]},
                        {e["id"] for e in self.call(who, "GET", "/api/benchmarks/status").json()["restricted"]},
                        {e["id"] for e in self.call(who, "GET", "/api/status").json()["benchmarks"]["restricted"]})
            self.assertEqual(named("bob"), (set(), set(), set()))
            self.assertEqual(named("alice"), ({self.helper}, {self.helper}, {self.helper}))
            everything = {"sv_host", "sv_dryrun", "sv_gpu", "sv_api", self.helper}
            self.assertEqual(named("zara"), (everything, {"sv_gpu", self.helper}, {"sv_gpu", self.helper}))
            tests.share_with_members("sv_gpu")
            self.assertEqual(named("bob"), ({"sv_gpu"}, {"sv_gpu"}, {"sv_gpu"}), "once shared, its users see it")
            for path in ("/api/pool/availability", "/api/benchmarks/status", "/api/status"):
                self.assertEqual(self.call("anon", "GET", path).status_code, 401)
        finally:
            appmod.scheduler.stop()
            appmod.scheduler = saved

    def test_the_sites_machines_are_administrators_whoever_owns_them(self):
        """The import makes the first administrator the owner of the host and the site's API. Once that account is a
        member, owning them gives it nothing: not their configuration, their key or their deletion, nor a seat on
        them without a grant."""
        from citar.pool import admission
        with db.session() as s:
            for sid in ("sv_host", "sv_api"):
                s.get(Server, sid).owner_id = self.ids["bob"]        # bob was that administrator; a member now
        hw = {"format": "citar-hardware", "cpu": {"model": "Ryzen", "threads": 16}}
        for method, path, body in (("DELETE", "/api/servers/sv_host", None), ("DELETE", "/api/servers/sv_api", None),
                                   ("POST", "/api/servers/sv_api/key", {"backend": "none"}),
                                   ("DELETE", "/api/servers/sv_api/key", None),
                                   ("POST", "/api/servers/sv_host/hardware", hw),
                                   ("GET", "/api/servers/sv_host/rates", None),
                                   ("POST", "/api/servers", {**servers.get("sv_host"), "notes": "mine"}),
                                   ("GET", "/api/pool/servers/sv_api", None),
                                   ("PUT", "/api/pool/servers/sv_api", {"enabled": False}),
                                   ("DELETE", "/api/pool/servers/sv_api", None)):
            with self.subTest(route=f"{method} {path}"):
                self.assertEqual(self.call("bob", method, path, body).status_code, 404)
        self.assertEqual(self.call("bob", "GET", "/api/servers").json()["servers"], [])
        self.assertEqual(self.call("bob", "GET", "/api/pool/servers").json()["servers"], [])
        with db.session() as s:
            bob = s.get(User, self.ids["bob"])
            self.assertFalse(admission.check(s, bob, s.get(Server, "sv_api")).allowed, "no seat without a grant")
        self.assertEqual(self.call("zara", "GET", "/api/servers/sv_host/rates").status_code, 200)
        self.assertEqual(self.call("zara", "PUT", "/api/pool/servers/sv_api", {"enabled": True}).status_code, 200)
        self.quiet()

    def test_a_member_prices_no_machine_with_the_sites_electricity_plans(self):
        """The plans are the household's bill: a member names none in a machine's costs (403), and where an
        administrator put one there the member keeps it but is never shown its price per kWh."""
        plan = [{"from": "2000-01-01", "electricity_plan_id": "ep_home"}]
        stored = servers.machine(self.helper)["config"]
        for method, path, body in (("POST", "/api/servers", {**servers.get(self.helper), "costs": plan}),
                                   ("POST", "/api/servers", {"name": "Mine", "kind": "owned", "costs": plan,
                                                             "connection": {"provider": "dryrun"}}),
                                   ("POST", "/api/pool/servers", {"config": {"name": "Mine", "costs": plan}}),
                                   ("PUT", f"/api/pool/servers/{self.helper}", {"config": {**stored, "costs": plan}}),
                                   ("PUT", f"/api/pool/servers/{self.helper}/costing", {"costs": plan})):
            with self.subTest(route=f"{method} {path}"):
                r = self.call("alice", method, path, body)
                self.assertEqual(r.status_code, 403, r.text)
                self.assertIn("electricity", r.json()["detail"])
        new = self.call("alice", "GET", "/api/servers/new?kind=owned&provider=dryrun").json()
        self.assertIsNone(new["costs"][0]["electricity_plan_id"])
        self.assertEqual(self.call("alice", "GET", "/api/servers").json()["electricity_plans"], [])
        r = self.call("zara", "PUT", f"/api/pool/servers/{self.helper}/costing",
                      {"power": {"idle_w": 100, "gpu_max_w": 200}, "costs": plan})
        self.assertEqual(r.status_code, 200, r.text)
        r = self.call("alice", "POST", "/api/servers", {**servers.get(self.helper), "notes": "the plan stays"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.call("alice", "GET", "/api/servers").json()["electricity_plans"],
                         [{"id": "ep_home", "name": "Home"}], "the name of the plan her machine has")
        for path in (f"/api/servers/{self.helper}/rates", f"/api/pool/servers/{self.helper}/rates"):
            with self.subTest(route=path):
                for side in self.call("alice", "GET", path).json().values():
                    self.assertFalse(side["kwh_price"])
                    self.assertFalse(side["energy"])
                self.assertGreater(self.call("zara", "GET", path).json()["busy"]["kwh_price"], 0)

    def test_a_members_new_machine_gets_an_id_of_the_servers_choosing(self):
        """Seats, suites and the usage ledger name machines by id: a member may not make a machine under an id of
        their choosing (a removed machine's, say). An administrator may."""
        r = self.call("alice", "POST", "/api/servers", {"id": "sv_removed_once", "name": "Mine", "kind": "test",
                                                        "connection": {"provider": "dryrun"}})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertNotEqual(r.json()["id"], "sv_removed_once")
        self.assertIsNone(servers.find("sv_removed_once"))
        self.assertEqual(servers.machine(r.json()["id"])["owner_id"], self.ids["alice"])
        r = self.call("zara", "POST", "/api/servers", {"id": "sv_chosen", "name": "Site's", "kind": "test",
                                                       "connection": {"provider": "dryrun"}})
        self.assertEqual((r.status_code, r.json()["id"]), (200, "sv_chosen"))

    def test_a_member_manages_their_own_machines_configuration_only(self):
        mine = servers.get(self.helper)
        saved = self.call("alice", "POST", "/api/servers", {**mine, "name": "Alice's renamed GPU", "notes": "hers"})
        self.assertEqual(saved.status_code, 200, saved.text)
        self.assertEqual((saved.json()["name"], saved.json()["reach"]), ("Alice's renamed GPU", "worker"))
        hw = {"format": "citar-hardware", "cpu": {"model": "Ryzen", "threads": 16}}
        self.assertEqual(self.call("alice", "POST", f"/api/servers/{self.helper}/hardware", hw).status_code, 200)
        self.assertEqual(self.call("alice", "GET", f"/api/servers/{self.helper}/rates").status_code, 200)
        listing = self.call("alice", "GET", "/api/servers").json()
        self.assertEqual([sv["id"] for sv in listing["servers"]], [self.helper])
        self.assertTrue(all(set(p) == {"id", "name"} for p in listing["electricity_plans"]), "no tariffs for members")
        self.assertNotIn("env", listing["key_backends"])
        for method, path, body in (("POST", "/api/servers", {**mine, "name": "Bob's now"}),
                                   ("POST", f"/api/servers/{self.helper}/hardware", hw),
                                   ("GET", f"/api/servers/{self.helper}/rates", None),
                                   ("DELETE", f"/api/servers/{self.helper}", None),
                                   ("POST", "/api/servers", {**servers.get("sv_gpu"), "name": "Bob's GPU"})):
            with self.subTest(route=path, who="bob"):
                self.assertEqual(self.call("bob", method, path, body).status_code, 404)
            with self.subTest(route=path, who="anon"):
                self.assertEqual(self.call("anon", method, path, body).status_code, 401)
        self.assertEqual(servers.get(self.helper)["name"], "Alice's renamed GPU")
        self.assertEqual({sv["id"] for sv in self.call("zara", "GET", "/api/servers").json()["servers"]},
                         {"sv_host", "sv_dryrun", "sv_gpu", "sv_api", self.helper}, "every machine for an administrator")
        self.assertEqual(self.call("alice", "DELETE", f"/api/servers/{self.helper}").status_code, 200)
        self.assertIsNone(servers.find(self.helper))
        self.quiet()

    def test_a_direct_machine_is_an_administrators_unless_a_dry_run(self):
        lan = {"name": "LAN box", "kind": "owned", "connection": {"provider": "openai_compatible",
                                                                   "base_url": "http://192.168.1.9:8080/v1"}}
        r = self.call("alice", "POST", "/api/servers", lan)
        self.assertEqual(r.status_code, 403, r.text)
        self.assertIn("administrator", r.json()["detail"])
        mine = servers.get(self.helper)
        r = self.call("alice", "POST", "/api/servers", {**mine, "reach": "direct"})
        self.assertEqual(r.status_code, 403, "switching to direct is refused too")
        r = self.call("alice", "PUT", f"/api/pool/servers/{self.helper}",
                      {"config": servers.machine(self.helper)["config"], "reach": "direct"})
        self.assertEqual(r.status_code, 403, r.text)
        r = self.call("alice", "POST", "/api/pool/servers", {"config": lan, "reach": "direct"})
        self.assertEqual(r.status_code, 403, r.text)
        r = self.call("alice", "POST", "/api/servers", {**lan, "connection": {"provider": "dryrun"}})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(r.json()["reach"], "direct")
        dry = r.json()["id"]
        self.assertEqual(self.call("alice", "POST", f"/api/servers/{dry}/detect").json()["models"][0]["key"], "dry-run")
        self.assertEqual(self.call("alice", "GET", f"/api/servers/{dry}/state").json(), {"supported": False})
        r = self.call("alice", "POST", "/api/servers", {**servers.get(dry), "connection": {"provider": "lmstudio"}})
        self.assertEqual(r.status_code, 403, "a member's dry run cannot become a machine the server dials")
        r = self.call("alice", "POST", "/api/servers", {**lan, "is_host": True, "connection": {"provider": "dryrun"}})
        self.assertEqual(r.status_code, 403, "the host is an administrator's choice")
        self.assertEqual(self.call("anon", "POST", "/api/servers", lan).status_code, 401)
        self.assertEqual(self.call("alice", "GET", "/api/servers/new?provider=lmstudio").json()["reach"], "worker")
        self.quiet()
        r = self.call("zara", "POST", "/api/servers", lan)
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual((r.json()["reach"], r.json()["owner_id"]), ("direct", self.ids["zara"]))

    def test_the_model_probe_is_an_administrators_but_the_dry_run(self):
        body = {"provider": "openai_compatible", "base_url": "http://127.0.0.1:9/v1", "api_key_env": SECRET_VAR}
        self.assertEqual(self.call("alice", "POST", "/api/llm/models", body).status_code, 403)
        self.assertEqual(self.call("anon", "POST", "/api/llm/models", body).status_code, 401)
        r = self.call("alice", "POST", "/api/llm/models", {"provider": "dryrun"})
        self.assertEqual((r.status_code, r.json()["models"]), (200, ["dry-run"]))
        r = self.call("alice", "POST", "/api/llm/models", {"provider": "dryrun", "api_key_env": SECRET_VAR})
        self.assertEqual((r.status_code, r.json()["models"]), (200, ["dry-run"]))
        self.quiet()                                    # the dry run's answer reads no variable it is given
        r = self.call("zara", "POST", "/api/llm/models", body)
        self.assertEqual(r.status_code, 200)
        self.assertIn("httpx", [c[0] for c in self.watch.calls], "an administrator's probe dials")
        self.watch.calls.clear()

    def test_the_setup_wizards_host_routes_are_an_administrators(self):
        for path, body in (("/api/setup/scan", None), ("/api/setup/apply", {"provider": "lmstudio",
                                                                            "base_url": self.LOCAL}),
                           ("/api/setup/dismiss", None), ("/api/setup/reopen", None)):
            with self.subTest(route=path):
                self.assertEqual(self.call("alice", "POST", path, body).status_code, 404)
                self.assertEqual(self.call("anon", "POST", path, body).status_code, 401)
        state = self.call("alice", "GET", "/api/setup/state").json()
        self.assertFalse(state["needed"], "setup is not a member's")
        self.assertIsNone(state["paths"], "nor where this computer keeps its data")
        self.assertFalse(state["dismissed"], "a member set no site-wide policy")
        self.quiet()
        self.assertEqual(self.call("zara", "POST", "/api/setup/dismiss").json(), {"dismissed": True})
        self.assertTrue(self.call("zara", "GET", "/api/setup/state").json()["dismissed"])
        self.assertIsNotNone(self.call("zara", "GET", "/api/setup/state").json()["paths"])
        self.assertEqual(self.call("zara", "POST", "/api/setup/reopen").json(), {"dismissed": False})

    def test_the_collector_script_is_any_members(self):
        self.assertEqual(self.call("alice", "GET", "/api/hardware-collector/collect_hardware.py").status_code, 200)
        self.assertEqual(self.call("anon", "GET", "/api/hardware-collector/collect_hardware.py").status_code, 401)
        self.quiet()

    def test_an_administrator_manages_every_machine_and_drives_the_host(self):
        mine = servers.get(self.helper)
        r = self.call("zara", "POST", "/api/servers", {**mine, "notes": "checked by an administrator"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(r.json()["owner_id"], self.ids["alice"], "the owner stays the owner")
        with mock.patch("citar.hwinfo.collect", return_value={"format": "citar-hardware", "cpu": {"threads": 4}}):
            self.assertEqual(self.call("zara", "POST", "/api/servers/sv_host/collect").status_code, 200)
            self.assertEqual(self.call("zara", "POST", "/api/servers/sv_gpu/collect").status_code, 404,
                             "it reads the host: only the host's row")
        self.assertEqual(servers.get("sv_host")["hardware"]["cpu"]["threads"], 4)
        self.assertEqual(self.call("zara", "POST", "/api/servers/sv_gpu/detect").json()["models"][0]["key"], "dry-run")
        self.assertEqual(self.call("zara", "DELETE", f"/api/servers/{self.helper}").status_code, 200)
        self.quiet()

    def test_the_retired_import_route_is_gone(self):
        self.assertEqual(self.call("zara", "POST", "/api/pool/import-registry").status_code, 404)

    def test_the_pool_routes_that_drop_a_helper_by_caller(self):
        """P3.9.1 for the pool routes 3-05 changed (disable, delete, revoke): a stranger 404, a visitor 401, the
        owner and an administrator 200."""
        from citar.auth import tokens
        with db.session() as s:
            raw, hashed = tokens.new_pair()
            token = WorkerToken(server_id=self.helper, token_hash=hashed, prefix=tokens.prefix(raw), label="t")
            s.add(token)
            s.flush()
            tid = token.id
        routes = (("PUT", f"/api/pool/servers/{self.helper}", {"enabled": False}),
                  ("DELETE", f"/api/pool/servers/{self.helper}/workers/tokens/{tid}", None))
        for method, path, body in routes:
            with self.subTest(route=path):
                self.assertEqual(self.call("bob", method, path, body).status_code, 404)
                self.assertEqual(self.call("anon", method, path, body).status_code, 401)
                self.assertEqual(self.call("alice", method, path, body).status_code, 200)
                self.assertEqual(self.call("zara", method, path, body).status_code, 200)
        self.assertEqual(self.call("bob", "DELETE", f"/api/pool/servers/{self.helper}").status_code, 404)
        self.assertEqual(self.call("anon", "DELETE", f"/api/pool/servers/{self.helper}").status_code, 401)
        self.assertEqual(self.call("zara", "DELETE", f"/api/pool/servers/{self.helper}").status_code, 200)
        self.quiet()

    def test_new_machines_and_suites_start_on_what_the_caller_may_use(self):
        self.assertEqual(self.call("anon", "GET", "/api/servers/new").status_code, 401)
        self.assertEqual(self.call("zara", "GET", "/api/servers/new?provider=lmstudio").json()["reach"], "direct")
        self.assertEqual(self.call("alice", "GET", "/api/servers/new?provider=dryrun").json()["reach"], "direct")
        suite = lambda who: self.call(who, "GET", "/api/benchmarks/suites/new")  # noqa: E731
        self.assertEqual(suite("anon").status_code, 401)
        self.assertEqual([g["server_id"] for g in suite("bob").json()["servers"]], [],
                         "nothing of the site's for a member nobody shared a machine with")
        self.assertEqual([g["server_id"] for g in suite("zara").json()["servers"]], ["sv_gpu"])
        self.assertEqual([g["server_id"] for g in suite("alice").json()["servers"]], [self.helper],
                         "her own machine")
        self.quiet()

    def test_an_administrators_setup_wizard_registers_a_machine(self):
        r = self.call("zara", "POST", "/api/setup/apply", {"provider": "anthropic", "label": "Wizard API",
                                                           "kind": "api", "models": ["claude-opus-5"]})
        self.assertEqual(r.status_code, 200, r.text)
        row = servers.machine(r.json()["server"]["id"])
        self.assertEqual((row["owner_id"], row["reach"], row["server_resource"]), (self.ids["zara"], "direct", True))
        self.quiet()

    def test_quiet_hours_lists_only_machines_the_caller_may_see(self):
        with db.session() as s:
            for sid in ("sv_gpu", self.helper):
                row = s.get(Server, sid)
                row.config = {**row.config, "restricted_hours": {"enabled": True, "windows": [
                    {"days": list(range(7)), "start": "00:00", "end": "23:59"}]}}
        ids = lambda who: {e["id"] for e in self.call(who, "GET", "/api/servers/restricted").json()["restricted"]}  # noqa: E731
        self.assertEqual(ids("alice"), {self.helper})
        self.assertEqual(ids("bob"), set())
        self.assertEqual(ids("zara"), {"sv_gpu", self.helper})


# ---------------------------------------------------------------------------------------------------------- gate 5

class Benchmarks(unittest.TestCase):
    """A suite naming the host and a helper machine resolves both through their rows; a run's jobs start."""

    def setUp(self):
        _fresh(seed=True)
        with db.session() as s:
            owner = accounts.create_user(s, handle="olive", email="olive@example.com", password="a long enough phrase",
                                         status="active", email_verified=True)
            self.helper = pool.create_server(s, owner, {"name": "Olive's box", "kind": "owned",
                                                        "connection": {"provider": "openai_compatible",
                                                                       "max_parallel": 3},
                                                        "models": [{"key": "olive/model"}]}).id
            s.get(Server, "sv_host").config = {**s.get(Server, "sv_host").config,
                                               "models": [{"id": "m_h", "key": "host/model"}]}

    def test_the_host_and_a_helper_machine_resolve_through_their_rows(self):
        from citar.server.benchmarks import resolve_group
        host = resolve_group({"server_id": "sv_host", "models": [{"model_id": "m_h"}]})
        self.assertEqual((host["pooled"], host["missing"], host["provider"], host["max_parallel"]),
                         (False, False, "none", 1))
        self.assertEqual(host["models"][0]["model"], "host/model")
        helper = resolve_group({"server_id": self.helper, "models": [{"model": "olive/model"}, {"model": "gone"}]})
        self.assertEqual((helper["pooled"], helper["missing"], helper["provider"], helper["max_parallel"]),
                         (True, False, "worker", 3))
        self.assertEqual([m.get("missing", False) for m in helper["models"]], [False, True])
        gone = resolve_group({"server_id": "sv_nowhere", "models": [{"model": "x"}]})
        self.assertTrue(gone["missing"] and gone["models"][0]["missing"])

    def test_a_runs_jobs_start_as_before(self):
        from citar.server.benchmarks import BenchmarkScheduler
        from citar.server.session import SessionManager
        manager = SessionManager()
        sch = BenchmarkScheduler(manager, Path(tempfile.mkdtemp(prefix="citar_merge_bench_")), autostart=False)
        try:
            run = sch.create_run({"name": "merge", "mode": "parallel", "repeats": 1,
                                  "servers": [{"server_id": "sv_dryrun", "models": [{"model_id": "m_dry"}]},
                                              {"server_id": self.helper, "models": [{"model": "olive/model"}]}],
                                  "scenarios": [{"name": "s", "map_size": "duel", "turn_limit": 2, "opponents": 1}]})
            groups = {g["id"]: g for g in run["suite"]["servers"]}
            self.assertFalse(groups["sv_dryrun"]["pooled"])
            self.assertTrue(groups[self.helper]["pooled"])
            sch.tick()
            dry = next(j for j in run["jobs"] if j["server_id"] == "sv_dryrun")
            self.assertIn(dry["status"], ("loading", "running"))
            seat = sch._llm_config(run, dry)
            self.assertEqual((seat["server_id"], seat["model"], seat["provider"]), ("sv_dryrun", "dry-run", "dryrun"))
            helper_job = next(j for j in run["jobs"] if j["server_id"] == self.helper)
            self.assertEqual(sch._llm_config(run, helper_job)["provider"], "worker")
            self.assertEqual(sch._ensure_model(run, helper_job), 0.0, "never loads a model on a helper machine")
        finally:
            sch.stop()
            for sid in list(manager.sessions):
                session = manager.get(sid)
                manager.delete(sid)
                if session is not None and session._driver is not None:
                    session._driver.join(10)          # its last write to the index lands before the next test


# ---------------------------------------------------------------------------------------------------------- power

class HostSampler(unittest.TestCase):
    """The server samples the host's power from its start, whether or not a host exists yet: on a fresh server-mode
    database the host appears with its first administrator, after the boot."""

    def test_the_servers_sampler_follows_the_host(self):
        from citar import usage
        _fresh()
        with mock.patch.object(usage, "_sampler", None), mock.patch.object(usage.PowerSampler, "start") as started:
            sampler = usage.start_power_sampler(follow_host=True)
            self.assertIsNotNone(sampler, "started with no host yet")
            started.assert_called_once()
            self.assertIsNone(sampler.target())
            tests.seed_registry()                       # the import, as the first administrator's makes it
            self.assertEqual(sampler.target(), "sv_host")
            with db.session() as s:
                s.get(Server, "sv_host").is_host = False
                s.get(Server, "sv_gpu").is_host = True
            self.assertEqual(sampler.target(), "sv_gpu", "a host chosen later is the one sampled")
        with mock.patch.object(usage, "_sampler", None), mock.patch.object(usage.PowerSampler, "start"):
            _fresh()
            self.assertIsNone(usage.start_power_sampler(), "the lab samples only a host there is when it starts")


# ---------------------------------------------------------------------------------------------------------- the CLI

class CommandLine(unittest.TestCase):
    """``citar setup --local`` writes machines with no database of its own open (``servers.register_offline``)."""

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="citar_offline_"))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        patch = mock.patch.object(servers, "PATH", self.dir / "servers.json")
        patch.start()
        self.addCleanup(patch.stop)
        patch = mock.patch.object(servers, "CONFIG_DIR", self.dir)
        patch.start()
        self.addCleanup(patch.stop)
        self.entry = {**servers.default_server("owned", "lmstudio"), "name": "LM Studio (this computer)",
                      "is_host": True}

    def test_before_the_first_start_into_the_file_it_imports(self):
        db.dispose()
        missing = mock.patch.dict(os.environ, {"CITAR_DB_URL": "sqlite:///" + (self.dir / "none.db").as_posix()})
        missing.start()
        self.addCleanup(missing.stop)
        settings.reset()
        self.addCleanup(settings.reset)
        with mock.patch("citar.hwinfo.collect", side_effect=RuntimeError("no scan in tests")), \
                mock.patch("citar.server.lmstudio.lms_path", lambda: None):
            saved = servers.register_offline([self.entry])
        self.assertFalse(db.configured(), "a tool opens no database")
        self.assertFalse((self.dir / "none.db").exists(), "and makes none")
        data = json.loads((self.dir / "servers.json").read_text(encoding="utf-8"))
        self.assertIn(saved[0]["id"], [sv["id"] for sv in data["servers"]])
        self.assertEqual(data["host_server_id"], "sv_host", "the first run's host stays the host")
        self.assertIn("sv_dryrun", [sv["id"] for sv in data["servers"]])

    def test_after_the_import_into_the_rows(self):
        _fresh(seed=True)
        with db.session() as s:
            ada = _admin(s)
            ada_id = ada.id
        saved = servers.register_offline([self.entry])
        row = servers.machine(saved[0]["id"])
        self.assertEqual((row["owner_id"], row["reach"], row["is_host"]), (ada_id, "direct", False))
        self.assertEqual(servers.host()["id"], "sv_host")
        self.assertFalse((self.dir / "servers.json").exists())


if __name__ == "__main__":
    unittest.main()

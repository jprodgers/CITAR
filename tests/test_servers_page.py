"""The Servers page's calls against the route table's gates (design/phase3-features.md P3.2.8; Appendix D, 3-06 gate 2).

The page (``citar/web/js/pool.js``, with the editors of ``servers.js`` and the machine settings dialog of
``machine_settings.js``, 3-18) sends every request through servers.js's
``call(name, target, ...)``, which asks ``machines.js``'s ``mayCall`` first, and shows a control only when ``may``
says its route would take it. ``mayCall`` reads each route's gate from ``machines.js``'s ``GATES``; its semantics are
tested in Node (``citar/web/tests/machines.test.mjs``: a member never may call an ``admin`` or ``host`` route, and
dials only their own dry run). This module holds the other half, statically:

- every request of servers.js, pool.js and machine_settings.js goes through ``call`` (no other use of ``api``,
  aliases included), and
  names a route in ``GATES``; every ``may`` names one too; nothing else in them, or in the other modules they import,
  reaches the network (what they import from api.js, ``fetch``, ``request``, sockets, dynamic imports);
- every name in ``GATES`` is an ``api.js`` function whose method and path are a route of the app;
- each route's gate as the server's code declares it (its dependencies: ``require_role("admin")``,
  ``require_cap("register_servers")``; the gate its body calls: ``_dialled``, ``_not_a_helper``, ``_row``/``_server``
  with MANAGE or VIEW, ``_group`` with MANAGE, ``check_cap``) is the gate ``GATES`` gives it, so a route made stricter
  on the server fails here until the page's table follows;
- the page's header (``nav.js``) calls only routes any member may.

And the routes 3-06 changed for the page, over the app (``PageRoutes``): the pool's routes that change something
commit before they answer (the page reads a change back at once), with P3.9.1's callers (the owner and an
administrator 200, another member 404 or, on a group shared with him, 403, a visitor 401); a group's listing hands
out no share link (its grants share it, and no route takes its key); the pool's listing reads a machine's quiet
hours on the clock that pauses its work, as /api/servers and the header do; a member's listing leaves out the issue
asking for an electricity plan, which only an administrator can name.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import inspect
import os
import re
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

_TMP = Path(tempfile.mkdtemp(prefix="citar_servers_page_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from sqlalchemy import event

from citar import db, pool, servers, settings
from citar.auth import accounts, policy, sessions
from citar.db.models import AccessGrant, Base, ServerGroup, User

settings.reset()

ROOT = Path(__file__).resolve().parent.parent
JS = ROOT / "citar" / "web" / "js"

#: The Servers page's own modules: every request they make goes through ``call`` (the machine settings dialog,
#: machine_settings.js, through servers.js's).
PAGE_MODULES = ("pool.js", "servers.js", "machine_settings.js")

#: What the page's modules may import from ``api.js``: servers.js the ``api`` object its ``call`` uses, the others
#: nothing.
API_IMPORTS = {"servers.js": "{ api }", "pool.js": None, "machine_settings.js": None}

#: The modules the page imports that are held to it elsewhere: ``nav.js`` (the header, below) and ``auth.js`` (the
#: session, and the sign-in wall a visitor gets instead of the page). Any other module it imports makes no request.
HELD_ELSEWHERE = ("api.js", "nav.js", "auth.js", "servers.js")

#: Every other way JavaScript reaches the network: none may appear in the page's code, so a request cannot go around
#: ``call`` (``api.js``'s own ``request``, a plain ``fetch``, a socket, a beacon, a module loaded at run time).
NETWORK = re.compile(r"\bfetch\s*\(|\bXMLHttpRequest\b|\bWebSocket\b|\bEventSource\b|\bsendBeacon\b"
                     r"|(?<![\w.$])request\s*\(|\bimport\s*\(")

#: A use of the identifier ``api`` (with the rest of its line, up to 40 characters): not an object's key
#: (``{ api: "Online API" }``), not part of a word, a path or a quoted value (``"/api/servers"``, ``kind === "api"``).
#: So an alias (``const a = api``), an argument or ``api.x`` all count, not only ``api.`` and ``api[``.
API_USE = re.compile(r"(?<![\w$.\"'/`-])api(?![\w$\"'/`:-])[^\n]{0,40}")


def api_routes() -> dict:
    """``api.js``'s functions: {name: (METHOD, path)}, the path's template parts as ``{}`` and its query dropped."""
    text = (JS / "api.js").read_text(encoding="utf-8")
    body = text[text.index("export const api = {"):]
    body = body[:body.index("\n};")]
    starts = [(m.start(), m.group(1)) for m in re.finditer(r"^  (\w+): ", body, re.M)]
    out = {}
    for i, (at, name) in enumerate(starts):
        chunk = body[at:starts[i + 1][0] if i + 1 < len(starts) else len(body)]
        found = re.search(r'request\("(GET|POST|PUT|PATCH|DELETE)",\s*(`[^`]*`|"[^"]*")', chunk)
        if not found:
            continue
        path = re.sub(r"\$\{[^}]*\}", "{}", found.group(2)[1:-1]).split("?")[0]
        out[name] = (found.group(1), path)
    return out


def page_gates() -> dict:
    """``machines.js``'s ``GATES``: {name: gate}."""
    text = (JS / "machines.js").read_text(encoding="utf-8")
    block = text[text.index("export const GATES = {"):]
    block = block[:block.index("};")]
    return dict(re.findall(r'^\s+(\w+): "([\w-]+)",', block, re.M))


def _code(module: str, *, imports: bool = False) -> str:
    """A module's code without its comments (a comment may name a call it does not make), and without its static
    imports unless ``imports``. An import ends at its own semicolon, so nothing after it goes with it."""
    text = (JS / module).read_text(encoding="utf-8")
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    if not imports:
        text = re.sub(r"(?m)^import\s[^;]*;$", "", text)
    return re.sub(r"(?m)^\s*//.*$|(?<=[;,{}()\s])//[^\n\"'`]*$", "", text)


def _imports(module: str) -> dict:
    """A module's static imports: {the module it imports from: what it imports (``{ a, b }``, ``* as x``, or "" for
    one imported for its effects)}."""
    text = (JS / module).read_text(encoding="utf-8")
    out = {}
    for what, source in re.findall(r"""(?m)^import\s+(?:([^;]*?)\s+from\s+)?["']([^"']+)["'];""", text):
        out[source.removeprefix("./")] = re.sub(r"\s+", " ", what or "").strip()
    return out


def _closure(fn) -> dict:
    """What a gate factory's returned dependency closed over (``require_role``'s ``role``, ``require_cap``'s
    ``capability``)."""
    try:
        return dict(inspect.getclosurevars(fn).nonlocals)
    except (TypeError, ValueError):
        return {}


def _dependencies(route) -> list:
    """The callables a route depends on: its signature's ``Depends`` and the route's own ``dependencies``."""
    calls = [getattr(d, "dependency", None) for d in getattr(route, "dependencies", None) or []]
    for param in inspect.signature(route.endpoint).parameters.values():
        calls.append(getattr(param.default, "dependency", None))
    return [c for c in calls if c is not None]


def server_gate(route) -> str:
    """A route's gate as the server's code declares it, in ``GATES``' words."""
    deps = _dependencies(route)
    source = inspect.getsource(route.endpoint)
    for dep in deps:
        name = getattr(dep, "__qualname__", "")
        if name.startswith("require_role") and _closure(dep).get("role") == "admin":
            # an administrator's route; the host's own when it is refused on any other row
            return "host" if "row.is_host" in source else "admin"
    for dep in deps:
        if getattr(dep, "__qualname__", "").startswith("require_cap") and _closure(dep).get("capability") == "register_servers":
            return "register"
    if "_dialled(" in source:
        return "dialled"
    if "_not_a_helper(" in source:
        return "direct"
    if 'check_cap(me, "register_servers")' in source:
        return "save" if "access.MANAGE not in access.on(" in source else "register"
    if re.search(r"_server\(s, \w+, me, access\.VIEW\)", source):
        return "view"
    if (re.search(r"_(server|group)\(s, [\w.]+, me, access\.MANAGE\)", source)
            or re.search(r"_(row|server)\(s, sid, me\)", source)):
        return "manage"
    return "member"


def app_routes() -> dict:
    """The app's routes: {(METHOD, path with ``{}`` for each parameter): route}."""
    sys.path.insert(0, str(ROOT / "scripts"))
    import audit_routes
    from citar.server.app import app
    out = {}
    for route, path, _inherited in audit_routes.all_routes(app):
        if not hasattr(route, "endpoint") or not hasattr(route, "methods"):
            continue
        for method in route.methods or ():
            out[(method, re.sub(r"\{[^}]*\}", "{}", path))] = route
    return out


def tearDownModule():
    # the route audit, imported for its walk over the app's routers, made a scratch directory of its own
    audit = sys.modules.get("audit_routes")
    if audit is not None:
        shutil.rmtree(getattr(audit, "_TMP", ""), ignore_errors=True)
    db.dispose()
    shutil.rmtree(_TMP, ignore_errors=True)


class ServersPageCalls(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.api = api_routes()
        cls.gates = page_gates()
        cls.routes = app_routes()

    def test_the_parsers_read_the_files(self):
        """The readers above found what they look for (a parser that reads nothing would pass everything below)."""
        self.assertEqual(self.api["detectModels"], ("POST", "/api/servers/{}/detect"))
        self.assertEqual(self.api["newServer"], ("GET", "/api/servers/new"))
        self.assertEqual(self.api["testPoolServer"], ("POST", "/api/pool/servers/{}/test"))
        self.assertEqual(self.api["revokeWorkerToken"], ("DELETE", "/api/pool/servers/{}/workers/tokens/{}"))
        self.assertGreater(len(self.gates), 30)
        self.assertIn(("POST", "/api/servers/{}/collect"), self.routes)

    def test_every_request_of_the_page_goes_through_its_gate(self):
        """servers.js, pool.js and machine_settings.js use ``api`` only inside ``call`` (``api[name](...)``, after
        ``mayCall``), and every ``call`` and ``may`` names a route of ``GATES``. Nothing else in them reaches the
        network: they import only ``api`` from api.js (servers.js) or nothing of it, and use no ``fetch``, ``request``, socket or
        dynamic import; the other modules they import make no request either."""
        for module in PAGE_MODULES:
            imports = _imports(module)
            self.assertIn("util.js", imports, f"{module}: the import reader read nothing")
            self.assertEqual(imports.get("api.js"), API_IMPORTS[module], f"{module} imports from api.js")
            self.assertEqual(NETWORK.findall(_code(module, imports=True)), [],
                             f"{module} reaches the network around call()")
            for other in sorted(set(imports) - set(HELD_ELSEWHERE)):
                with self.subTest(module=module, imports=other):
                    self.assertNotIn("api.js", _imports(other), f"{other}, imported by {module}, imports api.js")
                    self.assertEqual(NETWORK.findall(_code(other, imports=True)), [],
                                     f"{other}, imported by {module}, makes a request")
            code = _code(module)
            uses = API_USE.findall(code)
            allowed = ["api[name](...args);"] if module == "servers.js" else []
            self.assertEqual(sorted(uses), allowed, f"{module} uses api outside call()")
            names = set(re.findall(r"""\b(?:call|may)\(\s*["'](\w+)["']""", code))
            self.assertTrue(names, module)
            self.assertEqual(sorted(names - set(self.gates)), [], f"{module} names routes GATES does not")
            # a name computed at run time would get past the check above
            dynamic = re.findall(r"\b(?:call|may)\(\s*(?![\"'])([^)\s,]+)", code)
            self.assertEqual([d for d in dynamic if d != "name"], [], f"{module}: call()/may() with a computed name")
        self.assertIn("if (!may(name, target))", _code("servers.js"))

    def test_every_gate_names_a_route_of_the_app(self):
        missing = {name: self.api.get(name) for name in self.gates
                   if name not in self.api or self.api[name] not in self.routes}
        self.assertEqual(missing, {}, "GATES names an api.js function with no route")

    def test_the_gates_are_the_route_tables(self):
        """Each route the page may call has the gate GATES gives it, read off the server's own code."""
        wrong = {}
        for name, gate in sorted(self.gates.items()):
            route = self.routes[self.api[name]]
            found = server_gate(route)
            if found != gate:
                wrong[name] = f"{' '.join(self.api[name])}: the page says {gate}, the server {found}"
        self.assertEqual(wrong, {})

    def test_the_admin_only_routes_are_gated_as_such(self):
        """The host-wide routes and the host's own read as administrators' (the classifier itself, on the routes
        3-05 made administrators')."""
        for name in ("serverSettings", "lmDevices", "unlockKeys", "savePlan", "deletePlan"):
            self.assertEqual(server_gate(self.routes[self.api[name]]), "admin", name)
        self.assertEqual(server_gate(self.routes[self.api["collectHardware"]]), "host")
        for path in ("/api/keys/lock",):
            self.assertEqual(server_gate(self.routes[("POST", path)]), "admin", path)

    def test_the_header_calls_only_what_any_member_may(self):
        """The page's header (nav.js: the benchmark and lab badges) calls routes every member may."""
        code = _code("nav.js")
        names = set(re.findall(r"\bapi\.(\w+)\(", code))
        self.assertTrue(names)
        for name in sorted(names):
            self.assertEqual(server_gate(self.routes[self.api[name]]), "member", name)


class PageRoutes(unittest.TestCase):
    """The routes the page writes through, by alice on her own helper machine and group, by bob (another member),
    zara (an administrator) and a visitor."""

    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.auth.deps import get_db
        from citar.server import app as appmod
        db.dispose()
        db.configure(os.environ["CITAR_DB_URL"])
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        servers.invalidate()
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
                                                        "connection": {"provider": "lmstudio"}}).id
            self.group = pool.create_group(s, alice, name="Alice's", visibility="link").id
        # What the request's session still held, flushed or not, when the route returned. The request-scoped
        # session commits only after the answer is sent, so anything left here was not yet there for a reader.
        self.pending: list = []

        def recording_db():
            with db.session() as s:
                flushed = {"n": 0}
                event.listen(s, "after_flush", lambda *_: flushed.__setitem__("n", flushed["n"] + 1))
                event.listen(s, "after_commit", lambda *_: flushed.__setitem__("n", 0))
                yield s
                self.pending.append(flushed["n"] > 0 or bool(s.new or s.dirty or s.deleted))

        appmod.app.dependency_overrides[get_db] = recording_db
        self.addCleanup(appmod.app.dependency_overrides.pop, get_db, None)

    def call(self, who, method, path, body=None):
        self.pending.clear()
        return self.client.request(method, path, json=body,
                                   headers=self.headers.get(who, {"origin": "https://citar.test"}))

    def grant(self, subject="everyone", handle=None) -> str:
        """A grant on alice's group, made as her."""
        with db.session() as s:
            group = s.get(ServerGroup, self.group)
            return pool.create_grant(s, s.get(User, self.ids["alice"]), group, subject_type=subject,
                                     subject_handle=handle, purposes=["game"]).id

    def test_a_change_is_committed_before_the_answer(self):
        """Each pool route the page writes through commits before it answers, so the page's read-back sees it."""
        gid = self.group
        routes = [
            ("POST", "/api/pool/servers", {"config": {"name": "Box", "connection": {"provider": "ollama"}},
                                           "reach": "worker"}),
            ("PUT", f"/api/pool/servers/{self.helper}", {"group_id": gid}),
            ("PUT", f"/api/pool/servers/{self.helper}", {"enabled": True}),
            ("PUT", f"/api/pool/servers/{self.helper}/costing", {"costs": [{"from": "2000-01-01", "fixed_monthly": 5}]}),
            ("PUT", f"/api/pool/servers/{self.helper}/quiet-hours", {"restricted_hours": {"enabled": True, "windows": [
                {"days": [0], "start": "22:00", "end": "06:00"}]}}),
            ("POST", f"/api/pool/servers/{self.helper}/workers/tokens", {"label": "t"}),
            ("POST", "/api/pool/groups", {"name": "Another"}),
            ("PUT", f"/api/pool/groups/{gid}", {"name": "Renamed", "visibility": "link"}),
            ("POST", f"/api/pool/groups/{gid}/grants", {"subject_type": "everyone", "purposes": ["game"]}),
        ]
        for method, path, body in routes:
            with self.subTest(route=f"{method} {path}"):
                r = self.call("alice", method, path, body)
                self.assertEqual(r.status_code, 200, r.text)
                self.assertEqual(self.pending, [False], "the change was still uncommitted when the route answered")
        grant = self.grant()
        for method, path, body in (("PUT", f"/api/pool/grants/{grant}", {"subject_type": "everyone", "purposes": []}),
                                   ("DELETE", f"/api/pool/grants/{grant}", None),
                                   ("DELETE", f"/api/pool/groups/{gid}", None)):
            with self.subTest(route=f"{method} {path}"):
                r = self.call("alice", method, path, body)
                self.assertEqual(r.status_code, 200, r.text)
                self.assertEqual(self.pending, [False])

    def test_the_changed_routes_by_caller(self):
        """P3.9.1 for the routes 3-06 changed: the owner and an administrator 200, another member 404, a visitor 401;
        a route that makes something needs only an account that may."""
        owned = [("PUT", f"/api/pool/servers/{self.helper}", {"enabled": True}),
                 ("PUT", f"/api/pool/servers/{self.helper}/costing", {"costs": [{"from": "2000-01-01"}]}),
                 ("PUT", f"/api/pool/servers/{self.helper}/quiet-hours", {"restricted_hours": {"enabled": False}}),
                 ("POST", f"/api/pool/servers/{self.helper}/workers/tokens", {"label": "t"}),
                 ("PUT", f"/api/pool/groups/{self.group}", {"name": "Alice's", "visibility": "link"}),
                 ("POST", f"/api/pool/groups/{self.group}/grants", {"subject_type": "everyone", "purposes": ["game"]})]
        for method, path, body in owned:
            with self.subTest(route=f"{method} {path}"):
                self.assertEqual(self.call("bob", method, path, body).status_code, 404)
                self.assertEqual(self.call("anon", method, path, body).status_code, 401)
                self.assertEqual(self.call("alice", method, path, body).status_code, 200)
                self.assertEqual(self.call("zara", method, path, body).status_code, 200)
        for method, path, body in (("POST", "/api/pool/servers", {"config": {"name": "B"}, "reach": "worker"}),
                                   ("POST", "/api/pool/groups", {"name": "G"})):
            with self.subTest(route=f"{method} {path}"):
                self.assertEqual(self.call("anon", method, path, body).status_code, 401)
                for who in ("bob", "zara"):
                    self.assertEqual(self.call(who, method, path, body).status_code, 200)
        grant = self.grant()
        for method, path, body in (("PUT", f"/api/pool/grants/{grant}", {"subject_type": "everyone", "purposes": []}),
                                   ("DELETE", f"/api/pool/grants/{grant}", None)):
            with self.subTest(route=f"{method} {path}"):
                self.assertEqual(self.call("bob", method, path, body).status_code, 404)
                self.assertEqual(self.call("anon", method, path, body).status_code, 401)
        self.assertEqual(self.call("alice", "PUT", f"/api/pool/grants/{grant}",
                                   {"subject_type": "everyone", "purposes": []}).status_code, 200)
        self.assertEqual(self.call("zara", "DELETE", f"/api/pool/grants/{grant}").status_code, 200)
        self.assertEqual(self.call("bob", "DELETE", f"/api/pool/groups/{self.group}").status_code, 404)
        self.assertEqual(self.call("anon", "DELETE", f"/api/pool/groups/{self.group}").status_code, 401)
        self.assertEqual(self.call("alice", "DELETE", f"/api/pool/groups/{self.group}").status_code, 200)

    def test_a_member_given_the_group_may_not_change_it(self):
        """Bob, whom alice's group gives access to, sees the group and her machine in it (the page lists both to
        him) but changes neither, and his listing carries no configuration. The machine's routes say 403 (he may see
        it); the group's say 404, as to anyone without MANAGE on it (his access is through the group, not to it)."""
        self.assertEqual(self.call("alice", "PUT", f"/api/pool/servers/{self.helper}", {"group_id": self.group})
                         .status_code, 200)
        grant = self.grant("user", "bob")
        listed = {s["id"]: s for s in self.call("bob", "GET", "/api/pool/servers").json()["servers"]}
        self.assertFalse(listed[self.helper]["can_manage"])
        self.assertNotIn("config", listed[self.helper])
        self.assertNotIn(self.helper, {s["id"] for s in self.call("bob", "GET", "/api/servers").json()["servers"]})
        for method, path, body, code in (
                ("PUT", f"/api/pool/groups/{self.group}", {"name": "Bob's"}, 404),
                ("POST", f"/api/pool/groups/{self.group}/grants", {"subject_type": "everyone"}, 404),
                ("PUT", f"/api/pool/grants/{grant}", {"subject_type": "everyone"}, 404),
                ("DELETE", f"/api/pool/grants/{grant}", None, 404),
                ("DELETE", f"/api/pool/groups/{self.group}", None, 404),
                ("PUT", f"/api/pool/servers/{self.helper}", {"enabled": False}, 403),
                ("PUT", f"/api/pool/servers/{self.helper}/quiet-hours", {"restricted_hours": {}}, 403),
                ("POST", f"/api/pool/servers/{self.helper}/workers/tokens", {"label": "t"}, 403)):
            with self.subTest(route=f"{method} {path}"):
                self.assertEqual(self.call("bob", method, path, body).status_code, code)
        with db.session() as s:
            self.assertIsNotNone(s.get(AccessGrant, grant))

    def test_a_group_hands_out_no_link(self):
        """A group's machines are shared by its grants alone: no route takes a group's key, so its listing hands its
        manager no share link that would open nothing. Another member does not see the group, with its key or
        without, and a visitor gets 401."""
        groups = {g["id"]: g for g in self.call("alice", "GET", "/api/pool/groups").json()["groups"]}
        self.assertEqual(groups[self.group]["visibility"], "link")
        self.assertNotIn("share_url", groups[self.group])
        with db.session() as s:
            key = s.get(ServerGroup, self.group).slug
        self.assertTrue(key)
        for path in ("/api/pool/groups", f"/api/pool/groups?k={key}"):
            with self.subTest(path=path):
                self.assertNotIn(self.group, {g["id"] for g in self.call("bob", "GET", path).json()["groups"]})
                self.assertEqual(self.call("anon", "GET", path).status_code, 401)

    def test_quiet_hours_are_read_on_the_clock_that_pauses_the_work(self):
        """The pool's listing (the cards' "quiet until"), /api/servers and the page's header agree on when a machine
        rests: a machine the server dials by this computer's clock (servers.restricted_at, what the scheduler reads),
        whatever its owner's zone; a helper machine by its owner's. zara, an administrator, lives in Kiritimati
        (UTC+14, never this computer's zone here or on CI); her dry run's window covers now on this computer's clock
        and her helper machine's the same hours, which are not yet hers."""
        from tests.registry_calls import NOW, frozen
        with db.session() as s:
            s.get(User, self.ids["zara"]).tz = "Pacific/Kiritimati"
        window = {"enabled": True, "grace_minutes": 5,
                  "windows": [{"days": [0, 1, 2, 3, 4, 5, 6], "start": "23:00", "end": "06:00"}]}
        self.assertEqual((NOW.hour, NOW.minute), (23, 30))
        r = self.call("zara", "POST", "/api/servers", {"name": "Zara's dry run", "kind": "test",
                                                        "connection": {"provider": "dryrun"}, "restricted_hours": window})
        self.assertEqual(r.status_code, 200, r.text)
        dry = r.json()["id"]
        r = self.call("zara", "POST", "/api/pool/servers", {"config": {"name": "Zara GPU", "restricted_hours": window,
                                                                       "connection": {"provider": "lmstudio"}},
                                                            "reach": "worker"})
        self.assertEqual(r.status_code, 200, r.text)
        helper = r.json()["id"]
        with frozen():
            listed = {sv["id"]: sv for sv in self.call("zara", "GET", "/api/pool/servers").json()["servers"]}
            reg = self.call("zara", "GET", "/api/servers").json()
            one = self.call("zara", "GET", f"/api/pool/servers/{dry}").json()
        regs = {sv["id"]: sv for sv in reg["servers"]}
        header = {x["id"]: x["until"] for x in reg["restricted"]}
        self.assertEqual(listed[dry]["reach"], "direct")
        self.assertEqual(listed[dry]["restricted_until"], "06:00")
        self.assertEqual(regs[dry]["restricted_until"], "06:00")
        self.assertEqual(header.get(dry), "06:00")
        self.assertEqual((one["restricted_until"], one["config"]["restricted_until"]), ("06:00", "06:00"))
        # 23:30 here (New York or UTC) is the next afternoon in Kiritimati: outside the helper machine's hours there
        self.assertEqual(listed[helper]["reach"], "worker")
        self.assertIsNone(listed[helper]["restricted_until"])
        self.assertIsNone(regs[helper]["restricted_until"])
        self.assertNotIn(helper, header)

    def test_a_members_listing_asks_her_for_no_electricity_plan(self):
        """Only an administrator names a plan in a machine's costs (pool.check_plans): the issue that asks for one
        is on an administrator's listing of her machine, not on hers."""
        mine = {s["id"]: s for s in self.call("alice", "GET", "/api/servers").json()["servers"]}[self.helper]
        theirs = {s["id"]: s for s in self.call("zara", "GET", "/api/servers").json()["servers"]}[self.helper]
        self.assertIn(servers.NO_PLAN_ISSUE, theirs["issues"])
        self.assertNotIn(servers.NO_PLAN_ISSUE, mine["issues"])
        self.assertEqual([i for i in theirs["issues"] if i != servers.NO_PLAN_ISSUE], mine["issues"])
        self.assertNotIn(self.helper, {s["id"] for s in self.call("bob", "GET", "/api/servers").json()["servers"]})
        self.assertEqual(self.call("anon", "GET", "/api/servers").status_code, 401)


if __name__ == "__main__":
    unittest.main()

"""The Servers page's calls against the route table's gates (design/phase3-features.md P3.2.8; Appendix D, 3-06 gate 2).

The page (``citar/web/js/pool.js``, with the editors of ``servers.js``) sends every request through servers.js's
``call(name, target, ...)``, which asks ``machines.js``'s ``mayCall`` first, and shows a control only when ``may``
says its route would take it. ``mayCall`` reads each route's gate from ``machines.js``'s ``GATES``; its semantics are
tested in Node (``citar/web/tests/machines.test.mjs``: a member never may call an ``admin`` or ``host`` route, and
dials only their own dry run). This module holds the other half, statically:

- every request of servers.js and pool.js goes through ``call`` (no other use of ``api``), and names a route in
  ``GATES``; every ``may`` names one too;
- every name in ``GATES`` is an ``api.js`` function whose method and path are a route of the app;
- each route's gate as the server's code declares it (its dependencies: ``require_role("admin")``,
  ``require_cap("register_servers")``; the gate its body calls: ``_dialled``, ``_not_a_helper``, ``_row``/``_server``
  with MANAGE or VIEW, ``_group`` with MANAGE, ``check_cap``) is the gate ``GATES`` gives it, so a route made stricter
  on the server fails here until the page's table follows;
- the page's header (``nav.js``) calls only routes any member may.
"""
from __future__ import annotations

import inspect
import re
import shutil
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
JS = ROOT / "citar" / "web" / "js"

#: The Servers page's own modules: every request they make goes through ``call``.
PAGE_MODULES = ("pool.js", "servers.js")


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


def _code(module: str) -> str:
    """A module's code without its imports and comments (a comment may name a call it does not make)."""
    text = (JS / module).read_text(encoding="utf-8")
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    text = re.sub(r"(?ms)^import\b.*?;$", "", text)
    return re.sub(r"(?m)^\s*//.*$|(?<=[;,{}()\s])//[^\n\"'`]*$", "", text)


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
            or re.search(r"_(row|server)\(s, sid, me\)", source) or "pool.revoke_grant(" in source):
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
        """servers.js and pool.js use ``api`` only inside ``call`` (``api[name](...)``, after ``mayCall``), and every
        ``call`` and ``may`` names a route of ``GATES``."""
        for module in PAGE_MODULES:
            code = _code(module)
            uses = [m.group(0) for m in re.finditer(r"\bapi\b[.\[][^\n]{0,40}", code)]
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


if __name__ == "__main__":
    unittest.main()

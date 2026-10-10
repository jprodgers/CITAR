"""Audit every HTTP route for whether it is actually gated.

The failure this exists to catch is a route added later that nobody remembered to protect. A
deployment probe already found nine of those — the whole operator API was readable anonymously —
so this turns "did we remember" into something a machine answers.

    python scripts/audit_routes.py            # report
    python scripts/audit_routes.py --strict   # non-zero exit if anything is unexpectedly open

A route counts as gated when it declares a dependency that resolves an account (require_user,
require_admin, require_cap, ...) or when its body calls the access gate. Routes that are meant to
be public are listed explicitly below, so making one public is a visible decision rather than an
omission.
"""
from __future__ import annotations

import inspect
import os
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

_TMP = Path(tempfile.mkdtemp(prefix="citar_audit_"))
os.environ.setdefault("CITAR_DATA_DIR", str(_TMP))
os.environ.setdefault("CITAR_DB_URL", "sqlite:///" + (_TMP / "audit.db").as_posix())
os.environ.setdefault("CITAR_SAVE_DIR", str(_TMP / "saves"))
os.environ.setdefault("CITAR_CONFIG_DIR", str(_TMP / "config"))
os.environ.setdefault("CITAR_MODE", "local")
(_TMP / "saves").mkdir(exist_ok=True)
(_TMP / "config").mkdir(exist_ok=True)

#: Routes that are public on purpose, each with the reason. Anything not here and not gated is
#: reported. The reason matters: it is what a reviewer checks against later.
#:
#: A route the audit already sees as gated does not belong here. An entry for it changes nothing
#: today, and hides the route if its gate is ever removed: /debug/errors and /replay sat here as
#: "visibility checked in the body" while their bodies checked nothing.
INTENTIONALLY_PUBLIC = {
    "/": "the web client shell; it renders the sign-in screen for anonymous visitors",
    "/static": "the client's own JavaScript and CSS",
    "/m": "the phone site's shell; like /, it renders the sign-in screen for anonymous visitors",
    "/api/rules": "the ruleset — public game data, identical for everyone",
    "/api/tools": "the tool schema — public game data",
    "/api/meta": "server version and capabilities",
    "/api/auth/config": "what the sign-in page needs before anybody has signed in",
    "/api/auth/me": "returns authenticated:false for a stranger; the way to ask who you are",
    "/api/auth/login": "the sign-in endpoint; rate limited",
    "/api/auth/signup": "registration; rate limited, captcha, invite-gated",
    "/api/auth/logout": "ending a session you may not have",
    "/api/auth/verify/resend": "rate limited, and answers identically for unknown addresses",
    "/api/auth/password/forgot": "rate limited, and answers identically for unknown addresses",
    "/api/auth/password/reset": "the token in the link is the credential",
    "/api/auth/oauth/{provider}/start": "begins sign-in; rate limited",
    "/api/auth/oauth/{provider}/callback": "returns from the provider; state cookie is the check",
    "/api/auth/sessions": "checks the session itself",
    "/auth/verify": "the token in the emailed link is the credential",
    "/auth/reset": "hands the token to the form; consumes nothing",
    "/api/invites/check": "lets the signup form validate a code before creating anything",
    "/api/games": "list is filtered per viewer; anonymous sees only public games",
    "/ws/games/{gid}": "seat token, share key or session, checked in the handler",
    "/ws/worker": "worker token is the credential, checked as the first frame",
    "/openapi.json": "FastAPI's own schema",
    "/docs": "FastAPI's docs UI",
    "/docs/oauth2-redirect": "FastAPI's docs UI",
    "/redoc": "FastAPI's docs UI",
}

#: Names that, as a dependency, mean an account was resolved and checked.
GATE_NAMES = ("require_user", "require_admin", "require_moderator", "require_cap", "require_role")
#: Calls in a body that perform the check themselves.
BODY_GATES = ("require_user(", "_gate(", ".require(", "access.on(", "ownership.require(",
              "require_cap(", "_report(", "_server(", "_group(",
              # The saves routes filter by viewer or call their own checker.
              "_require_save_access(", "visible_ids(",
              # Routes that take the Principal and refuse an anonymous caller themselves, because
              # they need the account object rather than just the fact that there is one.
              "p.user is None", "principal.user is None")


def dep_name(call) -> str:
    """The name to report for a dependency callable.

    ``__qualname__``, not ``__name__``: the gates that take an argument — ``require_cap("invite")``,
    ``require_role("admin")`` — return an inner function that is literally called ``dependency``, so
    a name-only check sees the most important gates in the codebase as anonymous. The qualified name
    keeps the factory that produced it (``require_cap.<locals>.dependency``), which is what
    :data:`GATE_NAMES` matches against.
    """
    if call is None:
        return ""
    return getattr(call, "__qualname__", "") or getattr(call, "__name__", "") or repr(call)


def is_gate(names) -> bool:
    """True when any of *names* is one of the dependencies that resolves an account."""
    return any(name.split(".")[0] in GATE_NAMES for name in names)


def dependency_names(route) -> set:
    names = set()
    if not hasattr(route, "endpoint"):
        return names            # a Mount (the static files); no endpoint to inspect
    for dep in getattr(route, "dependencies", []) or []:
        names.add(dep_name(getattr(dep, "dependency", None)))
    sig = None
    try:
        sig = inspect.signature(route.endpoint)
    except (TypeError, ValueError):
        pass
    if sig:
        for param in sig.parameters.values():
            call = getattr(param.default, "dependency", None)
            if call is not None:
                names.add(dep_name(call))
    return names - {""}


def body_gated(route) -> bool:
    try:
        source = inspect.getsource(route.endpoint)
    except (OSError, TypeError):
        return False
    return any(marker in source for marker in BODY_GATES)


def all_routes(app):
    """Every route reachable on *app*, including those inside included routers.

    ``app.routes`` is not a flat list. Recent FastAPI wraps each ``include_router`` call in an
    ``_IncludedRouter`` object that holds the original router and the prefix and dependencies it
    was included with, and that wrapper has no ``.path`` of its own. A loop over ``app.routes``
    therefore walks straight past every route on every included router — which here is the entire
    authenticated API: accounts, sharing, the pool, the operator console and the server registry.

    That is precisely the blind spot this audit exists to close, so the traversal is explicit:
    recurse into the wrapper, carry its prefix down, and merge its router-level dependencies into
    each route's, since a requirement declared on the router (as ``admin_api`` declares
    ``require_admin``) gates every route on it.

    Yields ``(route, full path, extra dependency names)``.
    """
    def walk(routes, prefix: str, inherited: set):
        for route in routes:
            wrapped = getattr(route, "original_router", None)
            if wrapped is not None:
                context = getattr(route, "include_context", None)
                sub_prefix = (getattr(context, "prefix", "") or "") if context else ""
                sub_deps = set(inherited)
                for source in (context, wrapped):
                    for dep in (getattr(source, "dependencies", None) or []):
                        sub_deps.add(dep_name(getattr(dep, "dependency", None)))
                sub_deps.discard("")
                yield from walk(wrapped.routes, prefix + sub_prefix, sub_deps)
                continue
            path = getattr(route, "path", None)
            if path is None:
                continue
            yield route, prefix + path, inherited

    yield from walk(app.routes, "", set())


def main() -> int:
    strict = "--strict" in sys.argv
    from citar.server.app import app

    gated, public, unguarded = [], [], []
    for route, path, inherited in all_routes(app):
        if not path:
            continue
        if not hasattr(route, "endpoint"):
            # StaticFiles is mounted, not routed. It serves the client's own assets.
            public.append(("MOUNT", path, INTENTIONALLY_PUBLIC.get(path, "static assets")))
            continue
        methods = sorted(getattr(route, "methods", {"WS"}) or {"WS"})
        method = methods[0] if methods else "WS"
        deps = dependency_names(route) | inherited

        if is_gate(deps):
            gated.append((method, path, ", ".join(sorted(deps)) or "body"))
        elif body_gated(route):
            gated.append((method, path, "checked in body"))
        elif path in INTENTIONALLY_PUBLIC or path.split("?")[0] in INTENTIONALLY_PUBLIC:
            public.append((method, path, INTENTIONALLY_PUBLIC.get(path, "")))
        else:
            unguarded.append((method, path, ", ".join(sorted(deps)) or "no dependencies"))

    print(f"ROUTE AUDIT — {len(gated) + len(public) + len(unguarded)} routes\n")
    print(f"  gated                 {len(gated)}")
    print(f"  public on purpose     {len(public)}")
    print(f"  UNGUARDED             {len(unguarded)}")

    if unguarded:
        print("\n" + "=" * 78)
        print("UNGUARDED ROUTES — each needs a gate, or an entry in INTENTIONALLY_PUBLIC")
        print("=" * 78)
        for method, path, why in sorted(unguarded, key=lambda r: r[1]):
            print(f"  {method:7} {path:52} {why}")

    if "-v" in sys.argv:
        print("\ngated:")
        for method, path, why in sorted(gated, key=lambda r: r[1]):
            print(f"  {method:7} {path:52} {why}")
        print("\npublic on purpose:")
        for method, path, why in sorted(public, key=lambda r: r[1]):
            print(f"  {method:7} {path:52} {why}")

    if unguarded and strict:
        print("\nFAILED: unguarded routes present.")
        return 1
    print("\nOK: every route is gated or explicitly public.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

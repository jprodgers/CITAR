"""The HTTP server: the API, the sessions that drive games, and everything multi-user.

:mod:`citar.server.app`         the FastAPI application, routes, WebSockets and static files
:mod:`citar.server.session`     ``SessionManager`` and ``GameSession``: seats, the turn driver, saves
:mod:`citar.server.benchmarks`  the benchmark scheduler: suites, runs, restricted hours, resuming
:mod:`citar.server.scoring`     model scores from finished games
:mod:`citar.server.metrics`     per-seat, per-turn measurement
:mod:`citar.server.boot`        migrations and the checks that refuse to start a broken server
:mod:`citar.server.workers`     the endpoint worker agents connect back to
:mod:`citar.server.admin_cli`   ``citar admin``

The rest are HTTP surfaces, split by who they are for: ``auth_api`` (accounts), ``admin_api`` (the
server registry and reports), ``pool_api`` (shared hardware), ``share_api`` (published games and
reports) and ``setup_api`` (first-run setup and the operator console).

Two things hold across all of it.

**The engine is untouched.** Everything here is a caller of :mod:`citar.engine`, which does no I/O
and knows nothing about requests, and it calls through :mod:`citar.engine_api` only. A game is a value that
this package loads, drives and saves.

**Authorisation happens in one place.** :mod:`citar.auth.access` answers "what may this viewer do
with this object", and no route decides for itself. Refusals are 404 rather than 403 when the
caller cannot see the object, because a 403 confirms it exists and turns id-guessing into
enumeration. ``scripts/audit_routes.py`` checks that every route here declares a gate or is listed
as public with a reason.
"""

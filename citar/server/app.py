"""HTTP + WebSocket API and static web client.

    python -m citar.server  [--host 127.0.0.1] [--port 8765]
"""
from __future__ import annotations

import asyncio
import time
import traceback
from contextlib import contextmanager
from pathlib import Path
from typing import Optional

from fastapi import (Depends, FastAPI, HTTPException, Request, WebSocket, WebSocketDisconnect)
from sqlalchemy.orm import Session as DbSession
from fastapi.responses import FileResponse, JSONResponse, Response
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from .. import engine_api
from ..engine_api import ActionError, EngineCrash, EngineGame
from .session import CRASHED, SessionManager, GameSession, SAVE_DIR
from .benchmarks import BenchmarkScheduler
from .admin_api import router as admin_router
from .items_api import router as items_router
from .reports_api import router as reports_router
from .setup_api import router as setup_router
from .auth_api import router as auth_router
from .pool_api import router as pool_router
from .share_api import router as share_router
from .bots_api import router as bots_router
from .users_api import router as users_router
from .admin_users_api import router as admin_users_router
from .. import settings as citar_settings
from ..auth import access, audit
from ..auth.deps import Principal, get_db, principal, require_cap, require_role, require_user
from .. import items, pool
from ..pool import seats as pool_seats
from . import ownership, work_access

WEB_DIR = Path(__file__).resolve().parent.parent / "web"

app = FastAPI(title="CITAR — Civ Inspired Tool for AI Research", version="1.0")
app.include_router(auth_router)
app.include_router(pool_router)
app.include_router(share_router)
app.include_router(bots_router)
app.include_router(admin_router)
app.include_router(reports_router)
app.include_router(items_router)
app.include_router(setup_router)
app.include_router(users_router)
app.include_router(admin_users_router)
manager = SessionManager()
scheduler: Optional[BenchmarkScheduler] = None
_loop: Optional[asyncio.AbstractEventLoop] = None


@app.exception_handler(EngineCrash)
async def _engine_crash(request: Request, exc: EngineCrash):
    """An internal error of the engine that no route turned into its session's crash (``_crash_guard`` does): a 500
    that says what happened, never a refusal the caller could fix (DESIGN.md P2.6.3)."""
    return JSONResponse({"detail": CRASHED}, status_code=500)


@contextmanager
def _crash_guard(s: "GameSession"):
    """Runs a route's calls into session ``s``'s game: an ``EngineCrash`` crashes the session (GameSession._crashed:
    paused, its driver and agents stopped, the crash saved) and is a 500 saying so."""
    try:
        yield
    except EngineCrash as e:
        s._crashed(str(e))
        raise HTTPException(500, CRASHED) from None


@app.on_event("startup")
async def _startup():
    """Run migrations, check the configuration, and reload games that were in progress.

    The reload is what makes multi-day benchmarks survive a restart: a run whose games were mid-turn
    when the process stopped picks them up from their autosaves rather than losing them.
    """
    global _loop, scheduler
    _loop = asyncio.get_running_loop()
    from .boot import startup as boot_startup
    boot_startup()                                # database, migrations, configuration checks
    from ..pool import registry_import
    registry_import.on_boot()                     # config/servers.json into rows, once, before any seat needs one
    from .workers import hub
    hub().attach_loop(_loop)                      # lets game threads reach connected workers
    asyncio.create_task(hub().ping_loop(citar_settings.get().worker_ping_seconds))
    # lobby games first, so queued benchmarks and probes see which machines they hold
    from .session import register_games_with_queue
    register_games_with_queue()
    restored = manager.restore_live()
    if restored:
        print("restored games: " + "; ".join(restored), flush=True)
    if scheduler is None:
        scheduler = BenchmarkScheduler(manager)   # reloads benchmark games that were in progress
    _probes()                                     # resumes probe runs that were queued or running
    from .reports_api import reports_runner
    reports_runner()                              # resumes queued reports (some wait for a model machine)
    _close_orphan_rows()                          # now that everything that comes back is back
    items.on_boot()                               # shipped items' rows; strays adopted when there is an admin
    asyncio.create_task(_close_finished_loop())   # finished lobby games leave the list after a while
    from .. import usage
    usage.start_power_sampler(follow_host=True)   # live power samples of the host for cost reports (a host made
                                                  # later, by the first administrator, from then on)
    usage.tracker().start()


def _close_orphan_rows() -> list:
    """Close every games-index row left `setup` or `playing` whose game has no live session (P3.2.5): run at boot
    once the lobby games, the benchmarks and the probes are back. Returns the ids closed."""
    from .session import all_sessions
    return ownership.close_orphans(s.id for s in all_sessions())


async def _close_finished_loop():
    """Every minute, close finished lobby games nobody is watching (see SessionManager.close_finished)."""
    while True:
        await asyncio.sleep(60)
        try:
            closed = await asyncio.to_thread(manager.close_finished)
            if closed:
                print("closed finished games: " + "; ".join(closed), flush=True)
        except Exception:
            traceback.print_exc()


@app.on_event("shutdown")
def _shutdown():
    """Flush the usage ledger, let every game's writer finish the saves it has taken, and stop background work
    cleanly."""
    from .. import usage
    for s in list(manager.sessions.values()):
        try:
            s.flush_saves(timeout=10.0)
        except Exception:
            pass
    try:
        for s in list(manager.sessions.values()):
            if s.usage_act:
                usage.finish_session(s)
        usage.tracker().flush()
    except Exception:
        pass


def _scheduler() -> BenchmarkScheduler:
    """The benchmark scheduler, created on first use."""
    global scheduler
    if scheduler is None:
        scheduler = BenchmarkScheduler(manager)
    return scheduler


# ----------------------------------------------------------------------------
# helpers
# ----------------------------------------------------------------------------
def _session(gid: str) -> GameSession:
    """The live session for a game id, or a 404."""
    s = manager.get(gid)
    if s is None:
        raise HTTPException(404, f"No game '{gid}'.")
    return s


def _token(request: Request, token: Optional[str]) -> Optional[str]:
    """The seat token for this request, from the query string or the Authorization header."""
    if token:
        return token
    auth = request.headers.get("authorization", "")
    if auth.lower().startswith("bearer "):
        return auth[7:].strip()
    return request.query_params.get("token")


def _seat_pid(s: GameSession, token: Optional[str]) -> int:
    """The player id a seat token grants, or a 403."""
    seat = s.seat_for_token(token)
    if seat is None:
        raise HTTPException(403, "Invalid seat token.")
    return seat.player


def _share_key(request: Request, key: Optional[str] = None) -> Optional[str]:
    """The secret from a `link`-shared URL, however it arrived (?k=, ?key= or a header)."""
    return key or request.query_params.get("k") or request.query_params.get("key") \
        or request.headers.get("x-citar-share-key")


def _gate(sdb: DbSession, gid: str, p: Principal, permission: str, request: Request,
          token: Optional[str] = None) -> tuple:
    """Look up a game and check the caller may do `permission` to it.

    Every game route goes through here. A seat token still works on its own — LLM players and MCP
    clients hold one and have no account — and it grants play on that seat alone.

    A write by a caller that a session cookie signed in carries the session's CSRF token, as every
    ``require_user`` route's does (``deps._enforce_csrf``): the cookie is ambient, so without it another
    site the person visits could pause, reseat or delete their games behind SameSite's back. A request
    that holds a valid seat or spectator token for this game, or an Authorization header, is not riding
    a cookie and needs none; a made-up token does not excuse one.
    """
    s = _session(gid)
    tok = _token(request, token)
    if p.auth_session is not None and not (tok and (s.seat_for_token(tok) is not None or s.is_spectator(tok))):
        from ..auth.deps import _enforce_csrf
        _enforce_csrf(request, p)
    row, perms = ownership.require(sdb, s, p.user, permission,
                                   slug=_share_key(request), seat_token=tok)
    return s, row, perms


def _game_limit(sdb: DbSession, user) -> None:
    """Stop one account filling the server with games.

    The VPS runs every game, so concurrency is a shared resource: without a cap, one person starting
    twenty 24-civ games is a denial of service against everybody else on the box. Only lobby games
    count (P3.2.5): a member's benchmark and probe games belong to their runs, which the benchmark and
    probe queues limit, and a finished or closed game counts for nothing.
    """
    if user is None or user.role == "admin":
        return
    from ..db.models import Game as _GameRow
    from sqlalchemy import func, select as _select
    live = sdb.scalar(_select(func.count()).select_from(_GameRow).where(
        _GameRow.owner_id == user.id, _GameRow.deleted_at.is_(None), _GameRow.kind == "game",
        _GameRow.status.in_(ownership.LIVE_STATUSES)))
    cap = max(1, user.max_concurrent_games or 2)
    if (live or 0) >= cap:
        raise HTTPException(429,
                            f"You already have {live} games running and your limit is {cap}. "
                            "Finish or delete one first, or ask an administrator to raise the limit.")


#: The seat types that play a model, and so must name a machine when a game is made or a seat changed: those
#: ``pool.seats.seat_class`` gives a task class (P3.3.7).
MODEL_SEATS = pool_seats.MODEL_SEAT_TYPES


def _member_seat(seat: dict, user, *, playing: bool = True) -> dict:
    """One seat from a person, its model block through ``servers.member_llm`` (P3.2.9): a member's seat names a
    machine and never describes one. ``playing`` is for a seat of a game being made or changed, where a model seat
    without a machine is refused too; a scenario's seats are defaults its launch may still fill in. Returns the seat
    with its block cleaned; raises the refusal as HTTP.

    A seat that plays no model (a bot, a person, an MCP client) is never refused for its block, which nothing plays:
    the lobby sends the block a seat had while it was a model seat, and a scenario's bot seats keep theirs. Such a
    block is kept as the gate cleans it (an administrator's as it is, a member's to the machine it names), or dropped
    where the gate would refuse it; making the seat a model seat later checks the block it then has
    (``update_seat``). Only a block that is not an object at all is refused, on any seat."""
    from .. import servers
    try:
        llm = servers.llm_block(seat.get("llm"))
        model_seat = seat.get("type") in MODEL_SEATS
        required = playing and model_seat
        if not llm and not required:
            return seat
        try:
            return {**seat, "llm": servers.member_llm(llm, user, required=required)}
        except servers.SeatBlockError:
            if model_seat:
                raise
            return {**seat, "llm": {}}
    except servers.SeatBlockError as e:
        raise HTTPException(e.status, str(e)) from None


def _authorize_seats(sdb: DbSession, user, seats: list) -> None:
    """Whether ``user`` may seat these models on the machines they name (``pool.seats.authorize``: each model seat by
    its task class, its capability and a grant for its purpose), as HTTP: a 403 with the reason."""
    try:
        pool_seats.authorize(sdb, user, seats)
    except ValueError as e:
        raise HTTPException(403, str(e)) from None


# ----------------------------------------------------------------------------
# static
# ----------------------------------------------------------------------------
_STARTED = time.time()
_PHONE = ("iphone", "ipod", "android", "mobile", "blackberry", "iemobile", "opera mini", "windows phone")
SITE_COOKIE = "citar_site"


def _wants_mobile(request: Request) -> bool:
    """Whether to serve the phone site: an explicit choice (a cookie) wins, otherwise the user agent.

    Tablets are left on the full site: an iPad reports itself as a Mac, and an Android tablet's user
    agent has no "Mobile" in it, which is exactly the split wanted - the full game plays fine on a
    tablet, and badly on a phone.
    """
    choice = request.cookies.get(SITE_COOKIE)
    if choice in ("mobile", "desktop"):
        return choice == "mobile"
    ua = (request.headers.get("user-agent") or "").lower()
    return any(k in ua for k in _PHONE) and "ipad" not in ua


@app.get("/")
def index(request: Request, site: Optional[str] = None):
    """The web client. Everything else in the browser is loaded from here.

    Phones get the check-in site (mobile.html) instead of the full game client. ``/?site=desktop`` or
    ``/?site=mobile`` switches, and the choice is remembered for a year in a cookie.
    """
    if site in ("mobile", "desktop"):
        from fastapi.responses import RedirectResponse
        resp = RedirectResponse("/", status_code=303)
        resp.set_cookie(SITE_COOKIE, site, max_age=365 * 86400, samesite="lax",
                        secure=request.url.scheme == "https")
        return resp
    page = "mobile.html" if _wants_mobile(request) else "index.html"
    return FileResponse(WEB_DIR / page, headers={"Vary": "User-Agent, Cookie", "Cache-Control": "no-cache"})


@app.get("/m")
def mobile_index():
    """The phone site, whatever the device (handy for checking it from a desktop browser)."""
    return FileResponse(WEB_DIR / "mobile.html", headers={"Cache-Control": "no-cache"})


app.mount("/static", StaticFiles(directory=WEB_DIR), name="static")


@app.middleware("http")
async def _no_cache_static(request: Request, call_next):
    # the web client is served straight from disk; always revalidate so updates show up without a hard refresh
    """Tell browsers to revalidate the client's files on every request.

    The client is served straight from disk with no build step and no content hashing, so without this
    an upgrade leaves people running yesterday's JavaScript against today's API until they think to
    hard-refresh.
    """
    response = await call_next(request)
    if request.url.path == "/" or request.url.path.startswith("/static/"):
        response.headers["Cache-Control"] = "no-cache"
    return response


@app.middleware("http")
async def _security_headers(request: Request, call_next):
    """Headers that cost nothing and close off whole categories of attack.

    The CSP is strict because it can be: the web client is ES modules and an external stylesheet,
    with no inline script anywhere, so 'unsafe-inline' is not needed for scripts. hCaptcha needs its
    own frame and script origins, which are the only third-party entries here.
    """
    response = await call_next(request)
    cfg = citar_settings.get()
    response.headers.setdefault("X-Content-Type-Options", "nosniff")
    response.headers.setdefault("Referrer-Policy", "same-origin")
    response.headers.setdefault("X-Frame-Options", "DENY")
    response.headers.setdefault("Cross-Origin-Opener-Policy", "same-origin")
    response.headers.setdefault(
        "Content-Security-Policy",
        "default-src 'self'; "
        "script-src 'self' https://js.hcaptcha.com https://*.hcaptcha.com; "
        "style-src 'self' 'unsafe-inline' https://*.hcaptcha.com; "
        "img-src 'self' data: https:; "
        "connect-src 'self' https://*.hcaptcha.com; "
        "frame-src 'self' https://*.hcaptcha.com; "
        "frame-ancestors 'none'; base-uri 'self'; form-action 'self'")
    if cfg.require_https:
        response.headers.setdefault("Strict-Transport-Security", "max-age=31536000; includeSubDomains")
    # Shared links are meant to be shareable, not indexed and not archived by search engines.
    if request.url.path.startswith("/api/") or "slug" in request.query_params:
        response.headers.setdefault("X-Robots-Tag", "noindex, nofollow")
    return response


# ----------------------------------------------------------------------------
# rules & tools
# ----------------------------------------------------------------------------
@app.get("/api/rules")
def rules():
    """The whole ruleset, as the client needs it. Public: it is the same for everybody."""
    return engine_api.rules_client()


@app.get("/api/tools")
def tool_list():
    """Every player tool with its JSON schema. The authoritative reference for an agent."""
    return engine_api.tool_list()


class ModelProbe(BaseModel):
    """A request to check whether a model endpoint answers."""
    provider: str = "openai_compatible"
    base_url: Optional[str] = None
    api_key_env: Optional[str] = None


@app.post("/api/llm/models")
def probe_models(body: ModelProbe, me=Depends(require_user)):
    """List the models an endpoint offers (the lobby's 'Detect models' button, and a connectivity test). It fetches
    a caller-supplied base URL with a key from one of the server's environment variables, so it is an
    administrator's (P3.2.8), the dry run excepted, which dials nothing: a member's machine reports its models
    through its helper. Refused before any address is dialled or any variable read."""
    import os
    import httpx
    provider = (body.provider or "").lower()
    if provider != "dryrun" and not _is_admin(me):
        raise HTTPException(403, "Only an administrator can have the server fetch an address. A machine you add "
                                 "reports its models through its CITAR helper.")
    if provider == "dryrun":
        # answered before the key's variable is read: the dry run needs none, and it is anybody's to ask
        return {"ok": True, "models": ["dry-run"], "loaded": ["dry-run"], "details": {}}
    key = os.environ.get(body.api_key_env) if body.api_key_env else None
    try:
        if provider == "anthropic":
            if not key and not os.environ.get("ANTHROPIC_API_KEY"):
                return {"ok": False, "error": f"Environment variable {body.api_key_env or 'ANTHROPIC_API_KEY'} is not set on the server."}
            import anthropic
            client = anthropic.Anthropic(api_key=key) if key else anthropic.Anthropic()
            return {"ok": True, "models": [m.id for m in client.models.list(limit=50)]}
        base = (body.base_url or "").rstrip("/")
        if not base:
            return {"ok": False, "error": "Base URL is required."}
        headers = {"Authorization": f"Bearer {key}"} if key else {}
        r = httpx.get(f"{base}/models", headers=headers, timeout=6)
        r.raise_for_status()
        data = r.json().get("data", [])
        ids = [m["id"] for m in data if "embed" not in m["id"].lower()]
        loaded, details = [], {}
        try:  # LM Studio's native API also reports load state and context lengths
            root = base[:-3] if base.endswith("/v1") else base
            r2 = httpx.get(f"{root}/api/v0/models", timeout=3)
            if r2.status_code == 200:
                info = {m["id"]: m for m in r2.json().get("data", [])}
                ids = [i for i in ids if info.get(i, {}).get("type") != "embeddings"]
                loaded = [i for i in ids if info.get(i, {}).get("state") == "loaded"]
                ids.sort(key=lambda i: i not in loaded)
                details = {i: {"state": info[i].get("state"), "loaded_context": info[i].get("loaded_context_length"),
                               "max_context": info[i].get("max_context_length"),
                               "tool_use": "tool_use" in (info[i].get("capabilities") or [])} for i in ids if i in info}
        except Exception:
            pass
        return {"ok": True, "models": ids, "loaded": loaded, "details": details}
    except Exception as e:
        return {"ok": False, "error": f"Could not reach {body.base_url or provider}: {e}"}


@app.get("/api/meta")
def meta(request: Request):
    """Server version, mode and capabilities, for the client to adapt to."""
    import sys
    root = Path(__file__).resolve().parent.parent.parent
    return {"project_root": str(root), "python": sys.executable, "server_url": str(request.base_url).rstrip("/"),
            "mcp_script": str(root / "citar_mcp.py")}


# ----------------------------------------------------------------------------
# games
# ----------------------------------------------------------------------------
class CreateGame(BaseModel):
    """A new game: its name, its configuration and its seats."""
    name: str = ""
    config: dict = {}
    seats: list[dict] = []
    visibility: str = "private"
    data_sharing: Optional[str] = None


@app.get("/api/games")
def list_games(request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """Only the games this caller may see.

    Filtered per session rather than by a single query because the live sessions are the source of
    truth for what is running; the table is the index over them. `link`-shared games are reachable
    by their URL but deliberately never listed.
    """
    out = []
    for s in manager.sessions.values():
        # Resolved WITHOUT the share key on purpose: that is what makes a `link` game unlisted.
        # Holding the link gets you the game, not a place in anyone's listing — but somebody who was
        # explicitly granted access has an ACL entry and does see it here.
        row, perms = ownership.resolve(sdb, s, p.user)
        if access.VIEW not in perms:
            continue
        info = s.info()
        info["sharing"] = ownership.to_client(row, perms, p.user, base_url=citar_settings.get().public_origin)
        out.append(info)
    return out


@app.get("/api/status")
def server_status(p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """A check-in on the whole server, for the phone site's overview.

    Everyone signed in sees the games they may see and what the benchmark queue is doing; an
    administrator also sees the machine (load, memory, disk) and every connected worker.
    """
    import os
    import shutil
    import sys
    from .. import __version__, paths
    from .workers import hub
    if p.user is None:
        raise HTTPException(401, "Sign in to continue.")
    admin = p.user.role == "admin"
    games = {"playing": 0, "paused": 0, "over": 0, "ai_thinking": 0, "ai_reconnecting": 0, "problems": 0}
    for s in list(manager.sessions.values()):
        _row, perms = ownership.resolve(sdb, s, p.user)
        if access.VIEW not in perms:
            continue
        phase = s.game.phase
        games["over" if phase != "playing" else "paused" if s.paused else "playing"] += 1
        statuses = list(s.agent_status.values())
        games["ai_thinking"] += statuses.count("thinking") + statuses.count("queued")   # queued: mid-turn too
        games["ai_reconnecting"] += statuses.count("reconnecting")
        if any(getattr(a, "last_error", None) for a in s.agents.values()) or (s.pause_reason or {}).get("kind") == "disconnect":
            games["problems"] += 1
    workers = hub().status()
    out = {"version": __version__, "uptime": round(time.time() - _STARTED), "user": p.user.handle, "admin": admin,
           "mode": "server" if citar_settings.get().server_mode else "local", "games": games,
           "benchmarks": ({k: v for k, v in scheduler.status(items.listed(sdb, "benchmark_run", p.user),
                                                             pool.visible_restrictions(sdb, p.user)).items()
                           if k != "log"} if scheduler else None),
           "workers_online": len(workers) if admin else None}
    if admin:
        host = {"python": sys.version.split()[0], "platform": sys.platform}
        if hasattr(os, "getloadavg"):
            host["load"] = [round(x, 2) for x in os.getloadavg()]
        host["cpus"] = os.cpu_count()
        try:
            mem = dict(line.split(":", 1) for line in open("/proc/meminfo", encoding="utf-8"))
            total, avail = (int(mem[k].split()[0]) * 1024 for k in ("MemTotal", "MemAvailable"))
            host["memory"] = {"total": total, "used": total - avail}
        except (OSError, KeyError, ValueError):
            pass
        try:
            du = shutil.disk_usage(paths.state_dir())
            host["disk"] = {"total": du.total, "used": du.used}
        except OSError:
            pass
        out["host"] = host
        out["workers"] = [{"server_id": w["server_id"], "hostname": w["hostname"], "models": len(w["models"] or []),
                           "in_flight": w["in_flight"], "max_concurrent": w["max_concurrent"], "alive": w["alive"]}
                          for w in workers]
        # the prompt scheduler's machines (design/phase3-features.md P3.3.2): slots, leases, pooled prompts, what drains
        from ..pool import scheduler as prompt_scheduler
        sched = prompt_scheduler.current()
        out["machines"] = sched.status() if sched is not None else []
    return out


@app.get("/api/games/{gid}/summary")
def game_summary(gid: str, request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """One game at a glance, for the phone site: standings, whose turn, what each AI is doing, recent news.

    Standings are shown to whoever manages the game, or to anyone once the game is over or has no
    human in it; a spectator of a game somebody is still playing gets names and the turn, nothing
    the fog of war would hide.
    """
    s, _row, perms = _gate(sdb, gid, p, access.VIEW, request)
    with s.lock, _crash_guard(s):
        g = s.game
        full = access.MANAGE in perms or s.god_view_allowed()
        seats = {seat.player: seat for seat in s.seats}
        summ = g.summary()
        players = []
        for pl in summ["players"]:
            if pl["kind"] != "major":
                continue
            seat = seats.get(pl["id"])
            d = {"id": pl["id"], "name": pl["name"], "color": pl["color"], "alive": pl["alive"],
                 "seat": seat.type if seat else None,
                 "model": (seat.llm.get("model") or seat.llm.get("model_id")) if seat and seat.type == "llm" else None,
                 "status": s.agent_status.get(pl["id"]),
                 "error": getattr(s.agents.get(pl["id"]), "last_error", None) if full else None}
            if full:
                st = g.standing(pl["id"])
                d.update({"score": st["score"], "cities": st["cities"], "pop": st["population"], "techs": st["techs"],
                          "era": st["era"]})
            players.append(d)
        if full:
            players.sort(key=lambda d: (not d["alive"], -d.get("score", 0)))
        events = [{"turn": e["turn"], "type": e["type"], "text": e["text"]}
                  for e in g.events(last=300) if e.get("players") is None][-20:]
        info = s.info()
        current = g.player_name(summ["current"]) if summ["phase"] == "playing" else None
        return {**{k: info[k] for k in ("id", "name", "turn", "phase", "winner", "victory", "paused", "pause_reason",
                                        "ai_delay", "created", "config", "crashed")},
                "turn_limit": summ["turn_limit"], "current": current, "players": players, "events": events,
                "full": full, "can_manage": access.MANAGE in perms}


@app.post("/api/games")
def create_game(body: CreateGame, request: Request,
                me=Depends(require_cap("create_games")), sdb: DbSession = Depends(get_db)):
    """Create a game and return it, with a seat token for each seat."""
    top = engine_api.max_players()
    if not 1 <= len(body.seats) <= top:
        raise HTTPException(400, f"A game needs 1 to {top} seats.")
    if len(body.seats) > max(1, me.max_seats_per_game or top):
        raise HTTPException(403, f"Your account is limited to {me.max_seats_per_game} seats per game.")
    _game_limit(sdb, me)
    access.check_publish(me, body.visibility)
    _may_use(sdb, me, "map", (body.config or {}).get("map"))     # a custom map, and bot profiles, the caller may see
    _seat_profiles(sdb, me, body.seats)
    seats = [_member_seat(seat, me) for seat in body.seats]
    _authorize_seats(sdb, me, seats)
    try:
        # not started until it has its row and its owner: the first AI turn reads both
        s = manager.create(body.config, seats, body.name, start=False, owner_id=me.id)
    except ValueError as e:
        raise HTTPException(400, str(e))
    try:
        row = ownership.register(sdb, s, me, visibility=access.PRIVATE, data_sharing=body.data_sharing)
        if body.visibility != access.PRIVATE:
            # through set_visibility, for its audit row (check_publish above has refused what it would)
            access.set_visibility(sdb, row, body.visibility, actor=me)
        sdb.flush()
    except BaseException:
        # a game the index could not take is not left running unowned; the request's write is undone first, or
        # the stop's own write of the row would wait behind it
        sdb.rollback()
        manager.delete(s.id)
        raise
    s.start()
    audit.record(sdb, "game.created", actor=me, object_type="game", object_id=s.id,
                 seats=len(body.seats), visibility=row.visibility)
    info = s.info(include_tokens=True)
    info["sharing"] = ownership.to_client(row, access.Access({access.VIEW, access.PLAY, access.MANAGE}), me,
                                          base_url=citar_settings.get().public_origin)
    return info


@app.get("/api/games/{gid}")
def game_info(gid: str, request: Request, p: Principal = Depends(principal),
              sdb: DbSession = Depends(get_db)):
    """Seat tokens go only to somebody who may manage the game.

    Handing them to every viewer — which is what this did when the server was a single-user LAN
    tool — would let any spectator of a public game take over every seat in it.
    """
    s, row, perms = _gate(sdb, gid, p, access.VIEW, request)
    info = s.info(include_tokens=access.MANAGE in perms)
    info["sharing"] = ownership.to_client(row, perms, p.user, base_url=citar_settings.get().public_origin)
    return info


@app.delete("/api/games/{gid}")
def delete_game(gid: str, request: Request, p: Principal = Depends(principal),
                sdb: DbSession = Depends(get_db)):
    """Close a game (the lobby's "End game"): its session stops and leaves the list, its saves stay on disk, and its
    row in the games index is `closed` (P3.2.5), or `finished` if it was over, so it no longer counts against its
    owner's game limit and its saves load again. Deleting the saves (``/api/saves/delete`` of the whole game) is what
    deletes the game."""
    _gate(sdb, gid, p, access.MANAGE, request)
    manager.delete(gid)
    if p.user is not None:
        audit.record(sdb, "game.closed", actor=p.user, object_type="game", object_id=gid)
    return {"deleted": gid}


class SeatUpdate(BaseModel):
    """A change to one seat: its type, and the model configuration if it is an LLM seat."""
    type: Optional[str] = None
    llm: Optional[dict] = None
    bot: Optional[dict] = None
    name: Optional[str] = None


@app.post("/api/games/{gid}/seats/{pid}")
def update_seat(gid: str, pid: int, body: SeatUpdate, request: Request,
                p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """Change who controls a seat (e.g. hand a bot seat to an MCP client, or take over as human).

    A new model block, or a seat made a model seat, goes through the same gates as a new game's (P3.2.9): a member's
    block names a machine, and the caller must be admitted to a pooled machine it names. A refusal leaves the seat as
    it was."""
    s, _row, _perms = _gate(sdb, gid, p, access.MANAGE, request)
    if not 0 <= pid < len(s.seats):
        raise HTTPException(404, "No such seat.")
    llm = body.llm
    seat = s.seats[pid]
    new_type = body.type or seat.type
    if isinstance(body.bot, dict):
        _may_use(sdb, p.user, "bot_profile", body.bot.get("profile"))
    if llm is not None or (new_type in MODEL_SEATS and new_type != seat.type):
        # the block the seat will play with: the new one, or the one it has when only its type makes it a model seat.
        # A change that leaves a model seat's block as it is (a new name) checks nothing: that block was checked when
        # it was set, and a machine's window or budget must not stop its seat being renamed.
        checked = _member_seat({"type": new_type, "llm": llm if llm is not None else dict(seat.llm)}, p.user)
        _authorize_seats(sdb, p.user, [checked])          # admitted again by its task class (P3.3.7)
        if llm is not None:
            llm = checked["llm"]
    try:
        # the session keeps the civilization's engine controller in step with the seat type
        s.update_seat(pid, type=body.type, llm=llm, bot=body.bot, name=body.name)
    except ValueError:
        raise HTTPException(400, "Invalid seat type.")
    s.autosave(force=True)
    return s.info(include_tokens=True)


class Control(BaseModel):
    """Playback controls for a game: paused, and the delay between AI turns."""
    paused: Optional[bool] = None
    ai_delay: Optional[float] = None


@app.post("/api/games/{gid}/control")
def control(gid: str, body: Control, request: Request, p: Principal = Depends(principal),
            sdb: DbSession = Depends(get_db)):
    """Pause, resume or slow down a game. Chiefly for watching AI players."""
    s, _row, _perms = _gate(sdb, gid, p, access.MANAGE, request)
    with s.lock:
        if body.paused is not None:
            s.set_paused(body.paused)      # a person paused or resumed it: no longer the game's own pause
        if body.ai_delay is not None:
            s.ai_delay = max(0.0, min(30.0, body.ai_delay))
        s.cond.notify_all()
    s.mark_live()
    s._broadcast({"type": "control", "paused": s.paused, "ai_delay": s.ai_delay, "pause_reason": None})
    return {"paused": s.paused, "ai_delay": s.ai_delay}


@app.get("/api/games/{gid}/view")
def view(gid: str, request: Request, token: Optional[str] = None, as_player: Optional[int] = None,
         p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """The game as one seat sees it, honouring fog of war.

    ``as_player`` is for spectators of AI-only games, who may look through any civilization's eyes -
    and is refused for anyone who is actually playing, since that would be seeing the whole map.

    The engine writes the view as JSON with the server's keys (``seat`` or ``spectator``, ``session``,
    ``version``) spliced in (``EngineGame.view_json``), and the route returns those bytes as they are: a
    gargantuan god view would otherwise cost a parse and a re-dump of megabytes on every refresh.
    """
    s, _row, perms = _gate(sdb, gid, p, access.VIEW, request, token)
    tok = _token(request, token)
    with s.lock, _crash_guard(s):
        seat = s.seat_for_token(tok)
        if seat is not None:
            pid, extra = seat.player, {"seat": seat.public()}
        elif s.is_spectator(tok) or access.VIEW in perms:
            # Looking through one civilization's eyes is as revealing as the god view while a human is
            # playing: it shows that civ's private cities, units and diplomacy, and for the AI seats it
            # is a map of everything they have scouted. Both follow the same rule.
            if not s.god_view_allowed():
                raise HTTPException(403, "Spectator views are disabled while humans are playing.")
            if as_player is not None and not 0 <= as_player < len(s.seats):
                raise HTTPException(404, "No such player.")
            pid, extra = as_player, {"spectator": True}
        else:
            raise HTTPException(403, "Invalid token.")  # unreachable: _gate already checked
        extra["session"] = s.info()
        extra["version"] = s.version
        # which controls the game screen offers (pause, the AI delay, a named save): the routes check it again
        extra["can_manage"] = access.MANAGE in perms
        body = s.game.view_json(pid, extra)
    return Response(body, media_type="application/json")


class ToolCallBody(BaseModel):
    """A tool call: the name and its arguments."""
    tool: str
    args: dict = {}
    wait_seconds: float = 0.0


@app.post("/api/games/{gid}/tool")
def call_tool(gid: str, body: ToolCallBody, request: Request, token: Optional[str] = None,
              p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """Execute a player tool. The single entry point for every action in the game."""
    s, _row, _perms = _gate(sdb, gid, p, access.PLAY, request, token)
    pid = _seat_pid(s, _token(request, token))
    seat = s.seats[pid]
    seat.connected = True
    wait = max(0.0, min(body.wait_seconds, 600.0))
    return s.call_tool(pid, body.tool, body.args, wait_negotiation=wait)


@app.get("/api/games/{gid}/path")
def path_preview(gid: str, request: Request, unit_id: int, x: int, y: int,
                 token: Optional[str] = None, p: Principal = Depends(principal),
                 sdb: DbSession = Depends(get_db)):
    """Route preview for the browser: the path a move order would take and how many turns it needs.
    Read-only and not part of the AI tool set."""
    s, _row, _perms = _gate(sdb, gid, p, access.PLAY, request, token)
    pid = _seat_pid(s, _token(request, token))
    with s.lock, _crash_guard(s):
        return s.game.path_preview(pid, unit_id, x, y)


@app.get("/api/games/{gid}/wait")
def wait_for_turn(gid: str, request: Request, token: Optional[str] = None, timeout: float = 50.0,
                  p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """Long-poll until it is this seat's turn, or a negotiation needs an answer; on its own turn, until
    the other side answers a negotiation it is in (see GameSession.wait_for_turn).

    Why an agent should use this rather than polling: it returns for a negotiation too, and an agent
    that only watched for its own turn would leave the other side of a deal waiting forever.
    """
    s, _row, _perms = _gate(sdb, gid, p, access.PLAY, request, token)
    pid = _seat_pid(s, _token(request, token))
    s.seats[pid].connected = True
    return s.wait_for_turn(pid, max(1.0, min(timeout, 300.0)))


@app.post("/api/games/{gid}/debug/{action}")
def debug_action(gid: str, action: str, request: Request, p: Principal = Depends(principal),
                 sdb: DbSession = Depends(get_db)):
    """Developer helpers, only available when the server runs with --debug.

    These hand out gold and reveal the map, so they are gated on managing the game as well as on
    the debug flag — otherwise a spectator on a public game could cheat in it.
    """
    import os
    if os.environ.get("CITAR_DEBUG") != "1":
        raise HTTPException(404, "Debug endpoints are disabled.")
    s, _row, _perms = _gate(sdb, gid, p, access.MANAGE, request)
    with s.lock, _crash_guard(s):
        g = s.game
        try:
            g.debug(action)
        except ValueError:
            raise HTTPException(400, "Unknown debug action.")
        s._after_action(g.turn, g.current)
    return {"ok": True}


@app.get("/api/games/{gid}/metrics")
def game_metrics(gid: str, request: Request, p: Principal = Depends(principal),
                 sdb: DbSession = Depends(get_db)):
    """Per-seat, per-turn metrics for a game."""
    s, _row, _perms = _gate(sdb, gid, p, access.VIEW, request)
    with s.lock:
        return s.metrics_report()


@app.get("/api/games/{gid}/metrics.csv")
def game_metrics_csv(gid: str, request: Request, p: Principal = Depends(principal),
                     sdb: DbSession = Depends(get_db)):
    """The same metrics as CSV, for analysis outside CITAR."""
    import csv
    import io
    from .metrics import bot_actions_text
    s, _row, _perms = _gate(sdb, gid, p, access.VIEW, request)
    with s.lock, _crash_guard(s):
        rows = s.metrics.turn_rows()
        names = {p["id"]: p["name"] for p in s.game.summary()["players"]}
    cols = ["turn", "player", "civ", "controller", "model", "wall_s", "model_steps", "model_s", "input_tokens",
            "output_tokens", "reasoning_tokens", "tool_calls", "actions_ok", "queries", "errors", "repeats",
            "blocked_repeats", "malformed", "stall_nudges", "peak_prompt_tokens", "slowest_step_s", "context_length",
            "model_loaded_at_start", "end_reason", "top_tools", "bot_actions"]
    buf = io.StringIO()
    w = csv.DictWriter(buf, fieldnames=cols, extrasaction="ignore")
    w.writeheader()
    for r in rows:
        w.writerow({**r, "civ": names.get(r["player"], ""), "bot_actions": bot_actions_text(r.get("bot_actions"))})
    return Response(buf.getvalue(), media_type="text/csv",
                    headers={"Content-Disposition": f'attachment; filename="citar-{gid}-metrics.csv"'})


@app.get("/api/metrics/compare")
def compare_models(me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Aggregate AI metrics per model across running and saved games (latest save per game): an administrator's over
    every game, a member's over their own and the pooled ones (``scoring.viewer_scope``, P3.2.4)."""
    from .scoring import compare_models as compare, viewer_scope
    return compare(manager, scope=viewer_scope(sdb, me))


@app.get("/api/games/{gid}/debug/errors")
def errors(gid: str, request: Request, p: Principal = Depends(principal),
           sdb: DbSession = Depends(get_db)):
    """Recent errors from AI seats. The first place to look when a model does nothing.

    Gated on managing the game, not just viewing it: these are server tracebacks and the tool
    arguments each seat sent, which show what a player was trying to do and are nobody else's
    business. It used to answer anyone who knew the game id.

    ``save_lock`` is how long this session's saves held the game's lock since it was opened (its
    saves, their total, the longest and the last, in seconds): a save is taken under the lock and
    written off it, and a slow one stalls every reader and the turn driver (budget 10 ms).
    ``save_writer`` is the writing, off the lock (``SaveWriter.stats``): how many saves were written,
    passed over or failed, what the writes took, and the turn of the autosave on the disk.
    """
    s, _row, _perms = _gate(sdb, gid, p, access.MANAGE, request)
    return {"errors": s.errors[-50:], "agents": {pid: {"usage": getattr(a, "usage_total", None),
                                                       "last_error": getattr(a, "last_error", None)}
                                                 for pid, a in s.agents.items()},
            "save_lock": dict(s.save_lock), "save_writer": s.save_writer_stats()}


# ----------------------------------------------------------------------------
# benchmarks & model scores
# ----------------------------------------------------------------------------
def _bench_call(fn, *args, **kw):
    """Call a scheduler method, turning its errors into HTTP status codes."""
    try:
        return fn(*args, **kw)
    except KeyError as e:
        raise HTTPException(404, f"Not found: {e}")
    except ValueError as e:
        raise HTTPException(400, str(e))


def _is_admin(user) -> bool:
    """Whether an account is an administrator in good standing, who sees and manages every item."""
    return user is not None and user.role == "admin" and user.status == "active"


#: What a member's suite or probe subject may not carry (P3.2.9): keys that describe a machine, the address the
#: server would dial and the key it would send, instead of naming one.
DESCRIBING_KEYS = ("base_url", "api_key", "api_key_env")


def _may_use(sdb: DbSession, user, kind: str, ref) -> None:
    """Whether ``user`` may use the item ``kind``/``ref`` in something they make (a map in a game, a scenario in a
    probe, a bot profile in a seat): VIEW on it, a 404 otherwise. A file with no row is an administrator's to use
    (it is reached by nobody else until adoption gives it an owner); a built-in bot profile and the "best" one are
    everybody's. Anything but an id is a 400."""
    if not ref:
        return
    if not isinstance(ref, str):
        raise HTTPException(400, f"A {items.noun(kind)} is named by its id.")
    if kind == "bot_profile":
        from ..bots import profiles as _profiles
        if ref == _profiles.BEST or ref in {b["id"] for b in _profiles.BUILTIN}:
            return
    row = items.get(sdb, kind, str(ref))
    if row is None:
        if _is_admin(user):
            return
        raise HTTPException(404, f"No such {items.noun(kind)}.")
    access.on(sdb, user, row).require(access.VIEW)


def _seat_profiles(sdb: DbSession, user, seats) -> None:
    """Each bot seat's profile, which ``user`` must be able to see (``_may_use``)."""
    for seat in seats or []:
        if isinstance(seat, dict) and isinstance(seat.get("bot"), dict):
            _may_use(sdb, user, "bot_profile", seat["bot"].get("profile"))


def _suite_gate(sdb: DbSession, user, suite: dict) -> None:
    """A suite's model entries through the members' gate (P3.2.9): a member's suite names the machines its models
    play on and never describes one (a base URL, a key, a key variable: 400), and each machine passes
    ``servers.member_llm`` (an unknown one: 400) and is one the member may see (403 otherwise, the site's own
    included until they are shared; admission decides the rest when a run starts). Its scenarios' bot profiles must
    be ones ``user`` may see. An administrator's suite is taken as it is."""
    from .. import servers
    from ..db.models import Server
    if not isinstance(suite, dict):
        raise HTTPException(400, "A suite is an object.")
    for sc in suite.get("scenarios") or []:
        if isinstance(sc, dict):
            _may_use(sdb, user, "bot_profile", sc.get("bot_profile"))
    if _is_admin(user):
        return
    for group in suite.get("servers") or []:
        if not isinstance(group, dict):
            raise HTTPException(400, "A suite's machines are objects naming a machine.")
        for entry in [group] + [m for m in group.get("models") or [] if isinstance(m, dict)]:
            bad = [k for k in DESCRIBING_KEYS if entry.get(k) not in (None, "")]
            if bad:
                raise HTTPException(400, "A suite names the machines its models play on; it does not describe "
                                         f"them ({', '.join(bad)}). Pick one of the machines you may use.")
        try:
            servers.member_llm({"server_id": group.get("server_id")}, user, required=True)
        except servers.SeatBlockError as e:
            raise HTTPException(e.status, str(e)) from None
        # a suite names machines its maker may see; whether they may run it there is decided when a run starts
        # (``_admit``), so a machine nobody shared with them is refused now without being named
        row = sdb.get(Server, group.get("server_id"))
        if row is None or access.VIEW not in access.on(sdb, user, row):
            raise HTTPException(403, "You may not use that machine. Pick one of the machines you may use.")


def _admit(sdb: DbSession, user, server_ids, purpose: str) -> None:
    """Admission for work that will queue for its machines (a run): may ``user`` use each pooled machine for
    ``purpose`` at all (``pool.seats.authorize``, ``queued``: when is the runner's business). A 403 with the reason."""
    from ..pool import seats as pool_seats
    try:
        pool_seats.admit(sdb, user, server_ids, purpose)
    except ValueError as e:
        raise HTTPException(403, str(e)) from None


def _run_games_follow(sdb: DbSession, row) -> None:
    """A benchmark run's games are shown as the run is (P3.2.4): when the run's visibility changes, or it is taken
    down, its games' rows follow it, and an unpublished run's games get new share links too."""
    import secrets as _sec
    from sqlalchemy import select as _select
    from ..db.models import Game as _GameRow
    # the scheduler the server made at its start, never one made here: making one reloads games, whose rows it writes
    # from sessions of its own while this request holds its write
    run = scheduler.runs.get(row.ref) if scheduler is not None else None
    ids = [j["game_id"] for j in (run or {}).get("jobs", []) if j.get("game_id")]
    if not ids:
        return
    for game in sdb.scalars(_select(_GameRow).where(_GameRow.id.in_(ids))):
        if game.visibility != row.visibility and row.visibility == access.PRIVATE:
            game.slug = _sec.token_urlsafe(16)
        game.visibility = row.visibility


items.VISIBILITY_HOOKS["benchmark_run"] = _run_games_follow


@app.get("/api/benchmarks/status")
def bench_status(me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """What the benchmark scheduler is doing: running jobs, queues, restricted servers. A member's counts and log
    cover the runs they may see, and the machines in their quiet hours those the member may see."""
    return _scheduler().status(items.listed(sdb, "benchmark_run", me), pool.visible_restrictions(sdb, me))


@app.get("/api/benchmarks/settings", dependencies=[Depends(require_user)])
def bench_settings():
    """The scoring weights."""
    return _scheduler().get_settings()


@app.put("/api/benchmarks/settings", dependencies=[Depends(require_role("admin"))])
def bench_update_settings(body: dict):
    """Change the scoring weights: the server's own, so an administrator's."""
    return _bench_call(_scheduler().update_settings, body)


@app.get("/api/benchmarks/suites")
def bench_suites(request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """The saved benchmark suites the caller may see (and the one a share key opens), each with what they may do to
    it."""
    key = _share_key(request)
    seen = items.listed(sdb, "benchmark_suite", p.user, slug=key)
    out = [s for s in _scheduler().list_suites() if seen is None or s["id"] in seen]
    return _with_perms(sdb, "benchmark_suite", p.user, out, slug=key)


@app.get("/api/benchmarks/suites/new")
def bench_new_suite(me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """A blank suite with sensible defaults, for the editor to start from: on the host when its caller may use it,
    else the first machine with models they may (every machine is a row, P3.2.8; a member's suite names only those)."""
    from .benchmarks import normalize_suite
    from .. import servers as REG
    from ..db.models import Server

    def usable(sv) -> bool:
        row = sdb.get(Server, sv["id"])
        return row is not None and access.VIEW in access.on(sdb, me, row)
    host = REG.host()
    first = host if host and host["models"] and usable(host) else next(
        (s for s in REG.list_servers() if s["models"] and s["kind"] != "test" and usable(s)), None)
    return normalize_suite({"name": "New benchmark suite", "servers": [{"server_id": first["id"], "models": []}] if first else []})


@app.get("/api/benchmarks/suites/{suite_id}")
def bench_suite(suite_id: str, request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """One suite, for whoever may see it."""
    items.require(sdb, "benchmark_suite", suite_id, p.user, access.VIEW, slug=_share_key(request))
    return _bench_call(_scheduler().get_suite, suite_id)


@app.post("/api/benchmarks/suites")
def bench_save_suite(body: dict, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Create a suite (``run_benchmarks``; it is the caller's) or replace one they manage. Its model entries name
    machines (P3.2.9)."""
    from .benchmarks import normalize_suite
    sch = _scheduler()
    sid = body.get("id") if isinstance(body, dict) else None
    if sid and not isinstance(sid, str):
        raise HTTPException(400, "A suite's id is a string of letters, digits, - and _.")
    exists = bool(sid) and _bench_call(sch._suite_path, sid).exists()
    if sid:
        items.writable(sdb, "benchmark_suite", sid, me, exists=exists)
    else:
        items.check_create(me, "benchmark_suite")
    _suite_gate(sdb, me, body)
    suite = _bench_call(normalize_suite, body)
    items.register(sdb, "benchmark_suite", suite["id"], me, suite["name"], path=sch._suite_path(suite["id"]))
    return _bench_call(sch.save_suite, suite)


@app.delete("/api/benchmarks/suites/{suite_id}")
def bench_delete_suite(suite_id: str, request: Request, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Delete a suite the caller manages. Its finished runs are kept."""
    items.require(sdb, "benchmark_suite", suite_id, me, access.MANAGE, slug=_share_key(request))
    _bench_call(_scheduler().delete_suite, suite_id)
    items.delete(sdb, "benchmark_suite", suite_id, actor=me)
    return {"deleted": suite_id}


class RunBody(BaseModel):
    """A request to start a run: a saved suite, or one supplied inline."""
    suite_id: Optional[str] = None
    suite: Optional[dict] = None
    name: Optional[str] = None


@app.post("/api/benchmarks/runs")
def bench_start_run(body: RunBody, request: Request, me=Depends(require_cap("run_benchmarks")),
                    sdb: DbSession = Depends(get_db)):
    """Start a benchmark run, the caller's: of a suite they may see, or of one given inline. Every model entry
    passes the members' gate for the caller (P3.2.9), whoever saved the suite, and the caller must be admitted to
    each pooled machine for benchmarks."""
    sch = _scheduler()
    if body.suite_id:
        items.require(sdb, "benchmark_suite", body.suite_id, me, access.VIEW, slug=_share_key(request))
        suite = _bench_call(sch.get_suite, body.suite_id)
    elif body.suite:
        suite = body.suite
    else:
        raise HTTPException(400, "Give a suite_id or a suite.")
    _suite_gate(sdb, me, suite)
    _admit(sdb, me, [g.get("server_id") for g in suite.get("servers") or [] if isinstance(g, dict)], "benchmark")
    run = _bench_call(sch.create_run, suite, body.name, owner_id=me.id, session=sdb)
    return {"id": run["id"], "jobs": len(run["jobs"])}


@app.get("/api/benchmarks/runs")
def bench_runs(request: Request, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """The runs the caller may see (and the one a share key opens), with per-game progress and what they may do to
    each."""
    key = _share_key(request)
    out = _scheduler().list_runs(items.listed(sdb, "benchmark_run", me, slug=key))
    admin = _is_admin(me)
    # what a queued job waits for names only the work the caller may see (P3.9.1)
    name = work_access.namer(sdb, me, admin=admin)
    for r in out:
        r["jobs"] = [work_access.with_waiting(j, name) for j in r.get("jobs", [])]
        if not admin:
            # a machine's address is its owner's and the administrators' (the registry's routes are theirs)
            r["servers"] = [{k: v for k, v in sv.items() if k != "base_url"} for sv in r.get("servers", [])]
    return _with_perms(sdb, "benchmark_run", me, out, slug=key)


class RunControl(BaseModel):
    """An action on a run or a job: pause, resume, cancel, skip, retry."""
    action: str


@app.post("/api/benchmarks/runs/{run_id}/control")
def bench_run_control(run_id: str, body: RunControl, request: Request, me=Depends(require_user),
                      sdb: DbSession = Depends(get_db)):
    """Pause, resume, cancel or remove a whole run the caller manages."""
    items.require(sdb, "benchmark_run", run_id, me, access.MANAGE, slug=_share_key(request))
    _bench_call(_scheduler().control_run, run_id, body.action)
    if body.action == "remove":
        items.delete(sdb, "benchmark_run", run_id, actor=me)
    return {"ok": True}


@app.post("/api/benchmarks/runs/{run_id}/jobs/{job_id}/control")
def bench_job_control(run_id: str, job_id: str, body: RunControl, request: Request, me=Depends(require_user),
                      sdb: DbSession = Depends(get_db)):
    """Pause, resume, skip or retry one game in a run the caller manages."""
    items.require(sdb, "benchmark_run", run_id, me, access.MANAGE, slug=_share_key(request))
    _bench_call(_scheduler().control_job, run_id, job_id, body.action)
    return {"ok": True}


@app.post("/api/benchmarks/runs/{run_id}/jobs/{job_id}/watch")
def bench_job_watch(run_id: str, job_id: str, request: Request, me=Depends(require_user),
                    sdb: DbSession = Depends(get_db)):
    """Open a benchmark game so it can be watched live, for whoever may see its run. Opening one that is not
    running loads it on the server, which is why this needs an account as well as the run's VIEW."""
    items.require(sdb, "benchmark_run", run_id, me, access.VIEW, slug=_share_key(request))
    s = _bench_call(_scheduler().open_job_game, run_id, job_id)
    return {"game_id": s.id, "spectator_token": s.spectator_token, "phase": s.game.phase}


# ----------------------------------------------------------------------------
# custom maps (map editor)
# ----------------------------------------------------------------------------
def _map_call(fn, *args, **kw):
    """Call a map function, turning its errors into HTTP status codes."""
    try:
        return fn(*args, **kw)
    except engine_api.MapError as e:
        raise HTTPException(400, str(e))


def _with_perms(sdb: DbSession, kind: str, user, entries: list, key: str = "id", *, slug: Optional[str] = None) -> list:
    """A listing's entries, each with what ``user`` may do to it (``Access.client``, with the share key ``slug`` the
    listing was asked with), for the page's buttons."""
    perms = items.permissions_by_ref(sdb, kind, user, [e[key] for e in entries], slug=slug)
    return [dict(e, **perms.get(e[key], {})) for e in entries]


@app.get("/api/maps")
def maps_list(request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """The saved maps the caller may see (and the one a share key opens)."""
    key = _share_key(request)
    seen = items.listed(sdb, "map", p.user, slug=key)
    return _with_perms(sdb, "map", p.user, [m for m in engine_api.list_maps() if seen is None or m["id"] in seen],
                       slug=key)


@app.get("/api/maps/{map_id}")
def maps_get(map_id: str, request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """One saved map, for whoever may see it."""
    items.require(sdb, "map", map_id, p.user, access.VIEW, slug=_share_key(request))
    return _map_call(engine_api.load_map, map_id)


@app.put("/api/maps/{map_id}")
def maps_put(map_id: str, body: dict, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Create a map (``create_games``; it is the caller's) or replace one the caller manages. The id it is saved under
    is checked before anything is written."""
    ref = engine_api.map_ref(map_id)
    if not ref:
        raise HTTPException(400, f"Invalid map id '{map_id}'.")
    items.writable(sdb, "map", ref, me, exists=_map_call(engine_api.map_file, ref).exists())
    body = dict(body)
    body["id"] = ref
    items.register(sdb, "map", ref, me, str(body.get("name") or ref), path=engine_api.map_file(ref))
    clean, warnings = _map_call(engine_api.save_map, body)
    return {"map": engine_api.map_summary(clean), "warnings": warnings}


@app.delete("/api/maps/{map_id}")
def maps_delete(map_id: str, request: Request, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Delete a saved map the caller manages."""
    items.require(sdb, "map", map_id, me, access.MANAGE, slug=_share_key(request))
    _map_call(engine_api.delete_map, map_id)
    items.delete(sdb, "map", map_id, actor=me)
    return {"deleted": map_id}


@app.post("/api/maps/validate", dependencies=[Depends(require_cap("create_games"))])
def maps_validate(body: dict):
    """Check a map for problems - unreachable starts, missing resources - without saving it."""
    clean, warnings = _map_call(engine_api.validate_map, body)
    return {"map": engine_api.map_summary(clean), "warnings": warnings}


class MapGenBody(BaseModel):
    """Parameters for generating a map: size, type, seed."""
    map_size: Optional[str] = None
    width: Optional[int] = None
    height: Optional[int] = None
    map_type: str = "continents"
    players: Optional[int] = None
    city_states: Optional[int] = None
    seed: Optional[int] = None
    ruins: bool = False
    blank: Optional[str] = None       # a terrain name: an empty map of that terrain instead of a generated one
    name: str = ""
    map_edges: Optional[str] = None   # ice_caps | wrap_x | wrap_y | wrap_both | boxed
    river_density: Optional[float] = None
    resources: Optional[dict] = None  # density and per-resource rules, as in a game's config


@app.post("/api/maps/generate", dependencies=[Depends(require_cap("create_games"))])
def maps_generate(body: MapGenBody):
    """Generate a map, for the editor to start from."""
    sizes = engine_api.map_sizes()
    size = sizes.get(body.map_size or "small", sizes["small"])
    w, h = body.width or size["width"], body.height or size["height"]
    if body.blank:
        return _map_call(engine_api.blank_map, w, h, body.blank, body.name)
    players = body.players if body.players is not None else size["players"]
    cs = body.city_states if body.city_states is not None else size["city_states"]
    options = {"map_edges": body.map_edges, "river_density": body.river_density, "resources": body.resources}
    return _map_call(engine_api.generate_map, w, h, body.map_type, max(1, players), max(0, cs), body.seed,
                     body.ruins, body.name, options)


@app.post("/api/games/{gid}/export_map")
def maps_from_game(gid: str, request: Request, p: Principal = Depends(principal),
                   sdb: DbSession = Depends(get_db)):
    """Extract the terrain of a game as a reusable map.

    Gated on VIEW: the map is the shape of somebody's game world, and handing it to a stranger
    reveals the layout of a private game. Found by scripts/audit_routes.py, which exists because
    this is exactly the kind of route that gets added and forgotten.
    """
    s, _row, _perms = _gate(sdb, gid, p, access.VIEW, request)
    with s.lock, _crash_guard(s):
        return s.game.export_map(f"{s.name} terrain")


# ----------------------------------------------------------------------------
# scenarios and the scenario editor
# ----------------------------------------------------------------------------
import secrets as _secrets
import threading as _threading

EDITORS: dict[str, dict] = {}     # editor id -> {"game" (EngineGame), "undo", "meta", "lock", "t", "owner"}
EDITOR_UNDO = 25


def _editor(eid: str, user) -> dict:
    """The open scenario editor with this id, for the account that opened it or an administrator; a 404 for anyone
    else (P3.2.4: an editing session is its creator's), as for one that has expired."""
    ed = EDITORS.get(eid)
    if ed is None or (user is None or (ed.get("owner") != user.id and not _is_admin(user))):
        raise HTTPException(404, "This scenario editor session has expired; reopen the scenario.")
    ed["t"] = time.time()
    return ed


def _editor_payload(eid: str, ed: dict, results=None) -> dict:
    """The editor's current state, as the client needs it."""
    g = ed["game"]
    view = g.view(None, event_limit=0)
    for k in ("events", "thoughts", "messages", "negotiations", "stats", "empires"):
        view.pop(k, None)
    return {"editor_id": eid, "meta": ed["meta"], "view": view, "overview": g.scenario_overview(),
            "can_undo": bool(ed["undo"]), "results": results}


class EditorOpen(BaseModel):
    """What to open a scenario editor on: a map, a game, a save, or nothing."""
    source: str = "map"               # map | generate | scenario | save | game
    map: Optional[str] = None
    scenario: Optional[str] = None
    save: Optional[str] = None
    game_id: Optional[str] = None
    config: dict = {}
    players: list[dict] = []          # [{"nation", "difficulty", "type"}] for new games


@app.post("/api/scenario-editor")
def editor_open(body: EditorOpen, request: Request, p: Principal = Depends(principal),
                sdb: DbSession = Depends(get_db)):
    """Open a scenario editor and return its id: the caller's (``create_games``), on a map or a scenario they may
    see. An editor opened on a live game or a save holds that game's whole state, every civilization's included, so
    it needs what the game's god view does not: managing the game."""
    me = require_user(request, p)
    from ..auth.deps import check_cap
    check_cap(me, "create_games")
    meta = {"id": "", "name": "", "description": "", "seats": None}
    try:
        if body.source in ("map", "generate"):
            cfg = dict(body.config)
            if body.source == "map":
                if not body.map:
                    raise HTTPException(400, "Choose a map.")
                _may_use(sdb, me, "map", body.map)
                cfg["map"] = body.map
            elif cfg.get("map"):
                _may_use(sdb, me, "map", cfg["map"])
            players = body.players or [{"type": "human"}, {"type": "llm"}]
            if len(players) > engine_api.max_players():
                raise HTTPException(400, "Too many players.")
            cfg["players"] = [{"nation": pl.get("nation") or None, "difficulty": pl.get("difficulty") or None,
                               "controller": (pl.get("type") if pl.get("type") in ("human", "llm", "mcp", "bot") else "human"),
                               "name": pl.get("civ_name") or None} for pl in players]
            g = EngineGame.new(cfg)
            meta["seats"] = [{"type": pl.get("type") or "bot"} for pl in players]
            meta["name"] = f"Scenario on {cfg.get('map') or cfg.get('map_type', 'a new map')}"
        elif body.source == "scenario":
            _may_use(sdb, me, "scenario", body.scenario or "")
            data = engine_api.load_scenario(body.scenario or "")
            g = EngineGame.from_state(data["state"])
            meta.update({k: data.get(k) for k in ("id", "name", "description", "seats")})
        elif body.source == "save":
            _require_save_access(sdb, p, body.save or "", access.MANAGE)
            path = (SAVE_DIR / (body.save or "")).resolve()
            if SAVE_DIR.resolve() not in path.parents or not path.exists():
                raise HTTPException(404, "Save not found.")
            # the state alone: a scenario starts with no history, so its journal is never read
            g = EngineGame.from_save(engine_api.read_save(path), history=False)
            meta["name"] = f"Scenario from {body.save}"
        elif body.source == "game":
            s, _row, _perms = _gate(sdb, body.game_id or "", p, access.MANAGE, request)
            with s.lock:
                g = EngineGame.from_state(s.game.state_dict())
            meta["name"] = f"Scenario from {s.name} turn {g.turn}"
            meta["seats"] = [{"type": seat.type if seat.type != "human" else "human", "label": seat.name} for seat in s.seats]
        else:
            raise HTTPException(400, f"Unknown source '{body.source}'.")
    except (ValueError, ActionError) as e:
        raise HTTPException(400, str(e))
    if meta["seats"] is None:
        meta["seats"] = g.default_seats()
    meta["seats"] = g.normalize_seats(meta["seats"])
    # drop idle editor sessions (an hour without use)
    for k in [k for k, v in EDITORS.items() if time.time() - v["t"] > 3600]:
        EDITORS.pop(k, None)
    eid = _secrets.token_hex(4)
    EDITORS[eid] = {"game": g, "undo": [], "meta": meta, "lock": _threading.Lock(), "t": time.time(), "owner": me.id}
    return _editor_payload(eid, EDITORS[eid])


@app.get("/api/scenario-editor/{eid}")
def editor_get(eid: str, me=Depends(require_user)):
    """The current state of an open editor."""
    return _editor_payload(eid, _editor(eid, me))


class EditorOps(BaseModel):
    """A list of edit operations to apply."""
    ops: list[dict]


@app.post("/api/scenario-editor/{eid}/ops")
def editor_ops(eid: str, body: EditorOps, me=Depends(require_user)):
    """Apply edits to the scenario being edited, keeping an undo step."""
    ed = _editor(eid, me)
    with ed["lock"]:
        g = ed["game"]
        before = g.state_dict()
        try:
            results = g.apply_ops(body.ops)
        except ActionError as e:
            ed["game"] = EngineGame.from_state(before)        # operations are all-or-nothing
            raise HTTPException(400, str(e))
        ed["undo"].append(before)
        del ed["undo"][:-EDITOR_UNDO]
        return _editor_payload(eid, ed, results)


@app.post("/api/scenario-editor/{eid}/undo")
def editor_undo(eid: str, me=Depends(require_user)):
    """Undo the last set of edits."""
    ed = _editor(eid, me)
    with ed["lock"]:
        if not ed["undo"]:
            raise HTTPException(400, "Nothing to undo.")
        ed["game"] = EngineGame.from_state(ed["undo"].pop())
        return _editor_payload(eid, ed)


class EditorMeta(BaseModel):
    """A scenario's name and description."""
    id: Optional[str] = None
    name: Optional[str] = None
    description: Optional[str] = None
    seats: Optional[list] = None


@app.post("/api/scenario-editor/{eid}/meta")
def editor_meta(eid: str, body: EditorMeta, me=Depends(require_user)):
    """Rename the scenario being edited, or set its seats (a member's model blocks name machines, P3.2.9)."""
    ed = _editor(eid, me)
    seats = None
    if body.seats is not None:
        seats = [_member_seat(seat, me, playing=False) if isinstance(seat, dict) else seat
                 for seat in ed["game"].normalize_seats(body.seats)]
    for k in ("id", "name", "description"):
        if getattr(body, k) is not None:
            ed["meta"][k] = getattr(body, k)
    if seats is not None:
        ed["meta"]["seats"] = seats
    return {"meta": ed["meta"]}


@app.post("/api/scenario-editor/{eid}/save")
def editor_save(eid: str, body: EditorMeta, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Save the scenario being edited: a new scenario is the caller's (``create_games``), and one already saved under
    that id needs MANAGE on it, both checked before anything is written. Every seat's model block passes the members'
    gate (P3.2.9), the ones it was opened with included: a scenario saved by an administrator with a seat that
    describes its own provider is saved by a member only once that seat names a machine. Its bot seats' profiles
    must be ones the caller may see."""
    ed = _editor(eid, me)
    editor_meta(eid, body, me)
    m = ed["meta"]
    m["seats"] = [_member_seat(seat, me, playing=False) if isinstance(seat, dict) else seat
                  for seat in (m.get("seats") or [])]
    if not (m.get("id") or m.get("name")):
        raise HTTPException(400, "Give the scenario a name.")
    _seat_profiles(sdb, me, m["seats"])
    ref = engine_api.scenario_ref(m.get("id") or m["name"])
    if not ref:
        raise HTTPException(400, "Give the scenario a name with a letter or a digit in it.")
    try:
        path = engine_api.scenario_file(ref)
    except ActionError as e:
        raise HTTPException(400, str(e))
    items.writable(sdb, "scenario", ref, me, exists=path.exists())
    items.register(sdb, "scenario", ref, me, m.get("name") or ref, path=path)
    with ed["lock"]:
        try:
            summ = ed["game"].save_scenario(ref, m.get("name") or ref, m.get("description", ""), m.get("seats"))
        except ActionError as e:
            raise HTTPException(400, str(e))
    m["id"] = summ["id"]
    return summ


@app.delete("/api/scenario-editor/{eid}")
def editor_close(eid: str, me=Depends(require_user)):
    """Close an editor and release its game."""
    _editor(eid, me)
    EDITORS.pop(eid, None)
    return {"closed": eid}


@app.get("/api/scenario-ops", dependencies=[Depends(require_user)])
def scenario_ops_help():
    """The list of edit operations, with their parameters. The reference for the Operations tab."""
    return engine_api.scenario_ops_help()


@app.get("/api/scenarios")
def scenarios_list(request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """The saved scenarios the caller may see (and the one a share key opens)."""
    key = _share_key(request)
    seen = items.listed(sdb, "scenario", p.user, slug=key)
    return _with_perms(sdb, "scenario", p.user,
                       [s for s in engine_api.list_scenarios() if seen is None or s["id"] in seen], slug=key)


@app.get("/api/scenarios/{sid}")
def scenarios_get(sid: str, request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """One scenario, for whoever may see it."""
    items.require(sdb, "scenario", sid, p.user, access.VIEW, slug=_share_key(request))
    try:
        return engine_api.scenario_summary(engine_api.load_scenario(sid))
    except ActionError as e:
        raise HTTPException(404, str(e))


@app.delete("/api/scenarios/{sid}")
def scenarios_delete(sid: str, request: Request, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Delete a scenario the caller manages."""
    items.require(sdb, "scenario", sid, me, access.MANAGE, slug=_share_key(request))
    try:
        engine_api.delete_scenario(sid)
    except ActionError as e:
        raise HTTPException(400, str(e))
    items.delete(sdb, "scenario", sid, actor=me)
    return {"deleted": sid}


class LaunchBody(BaseModel):
    """How to start a game from a scenario: which seats are played by what."""
    name: str = ""
    seats: list[dict] = []


@app.post("/api/scenarios/{sid}/launch")
def scenarios_launch(sid: str, body: LaunchBody, request: Request, me=Depends(require_cap("create_games")),
                     sdb: DbSession = Depends(get_db)):
    """Start a playable game from a saved scenario the caller may see, as the caller's lobby game: it counts against
    their game limit, it has its row in the games index with them as its owner, and every seat's model block, the
    scenario's own included, passes the members' gate (P3.2.9) and admission before anything starts."""
    items.require(sdb, "scenario", sid, me, access.VIEW, slug=_share_key(request))
    _game_limit(sdb, me)
    checked: list = []

    def check(seat: dict) -> dict:
        """One seat as the scenario and the request make it, through the members' gate: its bot profile one the
        caller may see, whichever of the two named it."""
        _seat_profiles(sdb, me, [seat])
        seat = _member_seat(seat, me)
        checked.append(seat)
        return seat

    try:
        scn = engine_api.load_scenario(sid)
        # made but not yet registered: a refusal below leaves nothing behind, on the disk or in the list
        s = manager.create_from_scenario(scn, body.seats, body.name or scn["name"], register=False, start=False,
                                         check_seat=check)
    except (ActionError, ValueError) as e:
        raise HTTPException(400, str(e))
    _authorize_seats(sdb, me, checked)
    s.owner_id = me.id
    manager.register_new(s)
    try:
        ownership.register(sdb, s, me, visibility=access.PRIVATE)
        sdb.flush()
    except BaseException:
        sdb.rollback()                # as create_game: the request's write undone before the stop writes the row
        manager.delete(s.id)
        raise
    s.start()
    audit.record(sdb, "game.created", actor=me, object_type="game", object_id=s.id, scenario=sid,
                 seats=len(s.seats), visibility=access.PRIVATE)
    return s.info(include_tokens=True)


# ----------------------------------------------------------------------------
# probes (scripted experiments on a scenario)
# ----------------------------------------------------------------------------
_probe_runner = None


def _probes():
    """The probes module, imported lazily."""
    global _probe_runner
    if _probe_runner is None:
        from ..probes import ProbeRunner
        _probe_runner = ProbeRunner(manager)
    return _probe_runner


def _probe_call(fn, *a, **kw):
    """Call a probe function, turning its errors into HTTP status codes."""
    from ..probes import ProbeError
    try:
        return fn(*a, **kw)
    except (ProbeError, ActionError) as e:
        raise HTTPException(400, str(e))


def _probe_scenario(sdb: DbSession, user, data) -> None:
    """A probe plays on a scenario, which the caller must be able to see: checking a probe reads its scenario's
    civilizations, so without this a probe would be a way to read somebody's private scenario."""
    if isinstance(data, dict) and data.get("scenario"):
        _may_use(sdb, user, "scenario", data["scenario"])


@app.get("/api/probes")
def probes_list(request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """The saved probes the caller may see (and the one a share key opens)."""
    from .. import probes as P
    key = _share_key(request)
    seen = items.listed(sdb, "probe", p.user, slug=key)
    return _with_perms(sdb, "probe", p.user, [x for x in P.list_probes() if seen is None or x["id"] in seen],
                       slug=key)


@app.get("/api/probes/example", dependencies=[Depends(require_user)])
def probes_example(scenario: str, subject: int = 1, counterparty: int = 0):
    """An example probe, as a starting point."""
    from .. import probes as P
    return P.example_probe(scenario, subject, counterparty)


@app.get("/api/probes/{pid}")
def probes_get(pid: str, request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """One probe, for whoever may see it."""
    from .. import probes as P
    items.require(sdb, "probe", pid, p.user, access.VIEW, slug=_share_key(request))
    return _probe_call(P.load_probe, pid)


@app.put("/api/probes/{pid}")
def probes_put(pid: str, body: dict, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Create a probe (``run_probes``; it is the caller's) or replace one the caller manages, on a scenario they may
    see. The id it is saved under is checked before anything is written."""
    from .. import probes as P
    ref = P.slug(pid)
    path = _probe_call(P._path, ref)
    items.writable(sdb, "probe", ref, me, exists=path.exists())
    _probe_scenario(sdb, me, body)
    body = dict(body)
    body["id"] = ref
    items.register(sdb, "probe", ref, me, str(body.get("name") or ref)[:100], path=path)
    return _probe_call(P.save_probe, body)


@app.post("/api/probes/validate")
def probes_validate(body: dict, me=Depends(require_cap("run_probes")), sdb: DbSession = Depends(get_db)):
    """Check a probe's cases against its scenario, one the caller may see, without running them."""
    from .. import probes as P
    _probe_scenario(sdb, me, body)
    return _probe_call(P.validate, body)


@app.delete("/api/probes/{pid}")
def probes_delete(pid: str, request: Request, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Delete a probe the caller manages."""
    from .. import probes as P
    items.require(sdb, "probe", pid, me, access.MANAGE, slug=_share_key(request))
    _probe_call(P.delete_probe, pid)
    items.delete(sdb, "probe", pid, actor=me)
    return {"deleted": pid}


class ProbeRunBody(BaseModel):
    """Which model to run a probe against, and how many repeats."""
    llm: dict
    repeats: Optional[int] = None
    name: str = ""
    cases: Optional[list[str]] = None


def _probe_subject(sdb: DbSession, user, llm, purposes=("probe",)) -> dict:
    """A probe run's subject through the members' gate (P3.2.9), as it will be played: the scripted bot (``provider:
    bot``, a profile the caller may see), or a model on a machine the caller names, never describes, and is admitted
    to for each of ``purposes`` (``probes.case_purposes``: ``probe`` for its turn cases, ``pooled`` for its offer and
    message cases, P3.3.7)."""
    from .. import servers
    llm = servers.llm_block(llm) if not isinstance(llm, dict) else dict(llm)
    if str(llm.get("provider") or "").lower() == "bot":
        _may_use(sdb, user, "bot_profile", llm.get("profile"))
        return {k: llm[k] for k in ("provider", "aggression", "profile") if k in llm}
    if not _is_admin(user):
        bad = [k for k in DESCRIBING_KEYS if llm.get(k) not in (None, "")]
        if bad:
            raise HTTPException(400, "A probe's subject names the machine its model plays on; it does not describe "
                                     f"it ({', '.join(bad)}). Pick one of the machines you may use.")
    try:
        checked = servers.member_llm(llm, user, required=True)
    except servers.SeatBlockError as e:
        raise HTTPException(e.status, str(e)) from None
    for purpose in purposes:
        _admit(sdb, user, [checked.get("server_id")], purpose)
    return checked


@app.post("/api/probes/{pid}/run")
def probes_run(pid: str, body: ProbeRunBody, request: Request, me=Depends(require_cap("run_probes")),
               sdb: DbSession = Depends(get_db)):
    """Run a probe the caller may see: every case, from the same starting state, against one model. The run is the
    caller's, and its subject passes the members' gate."""
    from .. import probes as P
    items.require(sdb, "probe", pid, me, access.VIEW, slug=_share_key(request))
    # the cases play on the probe's scenario, and an opened case is its state: a probe shared wider than its
    # scenario runs only for those who may see the scenario too
    probe = _probe_call(P.load_probe, pid)
    _probe_scenario(sdb, me, probe)
    llm = _probe_subject(sdb, me, body.llm, _probe_call(P.case_purposes, probe, body.cases))
    return _probe_call(_probes().start, pid, llm, body.repeats, body.name, body.cases, owner_id=me.id, session=sdb)


def _probe_status(sdb: DbSession, user, runner) -> dict:
    """The probe runner's status as ``user`` may see it: the queue and the live cases of the runs they may see only
    (a live case carries the subject's thoughts)."""
    status = runner.status()
    seen = items.listed(sdb, "probe_run", user)
    if seen is None:
        return status
    live = status.get("live") or {}
    return {"queue": [r for r in status.get("queue", []) if r in seen],
            "live": live if live.get("run") in seen else {},
            "running": [r for r in status.get("running", []) if r.get("run") in seen]}


@app.get("/api/probe-runs")
def probe_runs(request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """The probe runs the caller may see (and the one a share key opens)."""
    runner = _probes()
    key = _share_key(request)
    seen = items.listed(sdb, "probe_run", p.user, slug=key)
    out = []
    for r in runner.list_runs():
        if seen is not None and r.get("id") not in seen:
            continue
        out.append({k: r.get(k) for k in ("id", "name", "status", "done", "created", "started", "finished",
                                          "summary", "error", "repeats")} |
                   {"total": len(r.get("jobs", [])), "probe_id": r["probe"].get("id"), "probe_name": r["probe"].get("name"),
                    "model": r.get("llm", {}).get("model"), "provider": r.get("llm", {}).get("provider")})
    return {"runs": _with_perms(sdb, "probe_run", p.user, out, slug=key), "status": _probe_status(sdb, p.user, runner)}


@app.get("/api/probe-runs/{rid}")
def probe_run(rid: str, request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """One probe run, with per-case results, for whoever may see it."""
    runner = _probes()
    _row, perms = items.require(sdb, "probe_run", rid, p.user, access.VIEW, slug=_share_key(request))
    run = _probe_call(runner.get_run, rid)
    admin = _is_admin(p.user)
    # what a queued run waits for names only the work the caller may see (P3.9.1)
    run = work_access.with_waiting(run, work_access.namer(sdb, p.user, admin=admin))
    if not admin:
        # an administrator's subject may describe its machine (P3.2.9); its address and key variable stay theirs
        run["llm"] = {k: v for k, v in (run.get("llm") or {}).items() if k not in DESCRIBING_KEYS}
    return {"run": run, "results": runner.results(rid), "status": _probe_status(sdb, p.user, runner),
            **perms.client()}


@app.post("/api/probe-runs/{rid}/stop")
def probe_run_stop(rid: str, request: Request, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Stop a probe run in progress that the caller manages."""
    items.require(sdb, "probe_run", rid, me, access.MANAGE, slug=_share_key(request))
    return _probe_call(_probes().stop, rid)


@app.delete("/api/probe-runs/{rid}")
def probe_run_delete(rid: str, request: Request, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Delete a probe run the caller manages."""
    items.require(sdb, "probe_run", rid, me, access.MANAGE, slug=_share_key(request))
    _probe_call(_probes().delete, rid)
    items.delete(sdb, "probe_run", rid, actor=me)
    return {"deleted": rid}


@app.post("/api/probe-runs/{rid}/open/{case}/{rep}")
def probe_run_open(rid: str, case: str, rep: int, request: Request, me=Depends(require_user),
                   sdb: DbSession = Depends(get_db)):
    """Opens the saved end state of one case as a (paused) game for inspection, for whoever may see the run: the
    game is the run's owner's, at the run's visibility. Opening it loads it on the server, which is why this needs an
    account as well as the run's VIEW."""
    runner = _probes()
    row, _perms = items.require(sdb, "probe_run", rid, me, access.VIEW, slug=_share_key(request))
    path = _probe_call(runner._run_dir, rid) / f"{case}-{rep}.citar"
    if not path.exists():
        raise HTTPException(404, "No saved game for that case.")
    owner_id = row.owner_id
    # a probe's game, should the index not have it (never a lobby game), opened to be looked at: it holds no machine
    # until its subject is asked something (P3.3.3)
    s = manager.load(path, kind="probe", leases=False)
    ownership.claim_quietly(s, owner_id, item=("probe_run", rid))
    return {"game_id": s.id, "spectator_token": s.spectator_token}


# ----------------------------------------------------------------------------
# the lab and the work queue
# ----------------------------------------------------------------------------
@app.get("/api/lab")
def lab_overview(request: Request, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """What the bot lab is doing: runner health, experiments, per-game progress and the log. A member sees the
    experiments they may see; the log and the side runs, which name every experiment and the server's own jobs,
    are an administrator's."""
    from .. import lab
    out = lab.overview()
    key = _share_key(request)
    seen = items.listed(sdb, "lab_experiment", me, slug=key)
    if seen is None:
        return out
    out["experiments"] = _with_perms(sdb, "lab_experiment", me,
                                     [e for e in out.get("experiments", []) if e.get("name") in seen], key="name",
                                     slug=key)
    out["running"] = [r for r in out.get("running", []) if r.get("exp") in seen]
    out["log"] = []
    out["side_runs"] = []
    return out


#: The queue's kinds and the item each one's ``id`` (its run) names; a game is its row in the games index.
_QUEUE_ITEMS = work_access.QUEUE_ITEMS
#: How work the caller may not see is named on the Queue page (P3.2.4).
_SOMEONES = work_access.SOMEONES
_queue_access = work_access.access_to


def _queue_entry(sdb: DbSession, user, it: dict, cache: dict, name=None) -> dict:
    """One queued or running item as ``user`` may see it: work they cannot see is "someone's <kind>", with no name,
    no run and no reason that would name other work; ``can_manage`` says whether its priority is theirs to change.
    The reason of one they may see names only the work they may see too (``name``, ``work_access.namer``)."""
    key = (it.get("kind"), it.get("group"))
    if key not in cache:
        cache[key] = _queue_access(sdb, user, *key)
    perms = cache[key]
    out = work_access.with_waiting(dict(it, can_manage=access.MANAGE in perms),
                                   name or work_access.namer(sdb, user, cache=cache))
    if access.VIEW not in perms:
        out.update(label=_SOMEONES.get(it.get("kind"), "someone's work"), run=None, waiting=None, server=None,
                   id=None, group=None)
    return out


@app.get("/api/queue")
def work_queue_view(me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Everything waiting to run, in the order it will run (P3.3.8).

    Only the machines the caller may view (their own, those their grants reach, every one for an administrator), each
    with its capacity; the leases live on it (a model seat that plays every turn: active, lending its slot between its
    turns, or holding it); its pooled prompts, running and waiting, in the order the prompt scheduler will serve them
    (``scheduler.Machine.view``); what holds it (``busy_with``); and the benchmark jobs and probe runs the shared job
    queue has for it, best first (``citar.pool.queue``). Work the caller may not see is "someone's <kind>" (or
    "someone's <class>" for a prompt), with no name and no member (``work_access.binding_rights``); the other machines
    in use are a count (``others_busy``), with no names or loads. Then this server's CPU: the lab's experiments, the
    lab runner, and the load average, since the lab's bot games share the CPU with every game being played here.
    """
    import os
    from .. import lab, servers as registry
    from ..db.models import Server, User
    from ..pool import queue as work_queue, scheduler as prompt_scheduler
    from .workers import hub
    _scheduler()
    _probes()
    items_waiting = work_queue.waiting()
    active = work_queue.running()
    sched = prompt_scheduler.current()
    views = sched.views() if sched is not None else {}
    ids = list(dict.fromkeys([i["server_id"] for i in active + items_waiting if i.get("server_id")] + list(views)))
    admin = _is_admin(me)
    cache: dict = {}
    name = work_access.namer(sdb, me, admin=admin, cache=cache)
    handles: dict = {}

    def handle(user_id):
        if not user_id:
            return None
        if user_id not in handles:
            row = sdb.get(User, user_id)
            handles[user_id] = row.handle if row is not None else None
        return handles[user_id]

    names = {sv["id"]: sv["name"] for sv in registry.list_servers()}
    machines, others = [], 0
    for sid in ids:
        row = sdb.get(Server, sid)
        if not admin and (row is None or not pool.visible(sdb, me, row)):
            others += 1             # in use, but not the caller's to see: counted, never named
            continue
        view = views.get(sid) or {}
        waiting = [_queue_entry(sdb, me, dict(it, position=n + 1), cache, name)
                   for n, it in enumerate(i for i in items_waiting if i.get("server_id") == sid)]
        # one line per holder: a game with two model seats on the machine holds two leases
        busy = list(dict.fromkeys(pool_seats.occupied(sid))) if admin else _occupants(sdb, me, sid, cache)

        def told(entry, fn, row=row):
            rights = work_access.binding_rights(sdb, me, entry, row, admin=admin, cache=cache)
            return fn(entry, rights, handle(entry.get("consumer")))
        machines.append({
            "server_id": sid, "name": row.name if row is not None else names.get(sid, sid),
            "online": hub().is_online(sid) or None, "capacity": view.get("capacity"),
            "slots": view.get("slots"), "in_flight": view.get("in_flight", 0), "draining": view.get("draining", 0),
            "busy_with": busy, "waiting": waiting,
            "running": [_queue_entry(sdb, me, i, cache, name) for i in active if i.get("server_id") == sid],
            "leases": [told(entry, work_access.told_lease) for entry in view.get("leases") or []],
            "prompts": {"running": [told(entry, work_access.told_ticket) for entry in view.get("running") or []],
                        "waiting": [told(entry, work_access.told_ticket) for entry in view.get("waiting") or []]}})
    try:
        overview = lab.overview()
    except Exception:
        overview = {"experiments": [], "runner": {}}
    experiments = sorted([e for e in overview.get("experiments", []) if e.get("state") != "complete"],
                         key=lambda e: (-(e.get("priority") or 0), e.get("submitted") or ""))
    if not admin:
        shown = []
        for e in experiments:
            perms = _queue_access(sdb, me, "lab", e.get("name"))
            if access.VIEW in perms:
                shown.append(dict(e, can_manage=access.MANAGE in perms))
            else:
                shown.append({"name": None, "label": _SOMEONES["lab"], "state": e.get("state"),
                              "priority": e.get("priority"), "games": e.get("games"), "done": e.get("done"),
                              "running": e.get("running"), "can_manage": False})
        experiments = shown
    cpu = {"cpus": os.cpu_count(), "load": [round(x, 2) for x in os.getloadavg()] if hasattr(os, "getloadavg") else None,
           "lab_runner": overview.get("runner"),
           "lab_running": overview.get("running", []) if admin else [],
           "experiments": experiments}
    return {"machines": machines, "others_busy": others, "cpu": cpu}


def _occupants(sdb: DbSession, user, server_id: str, cache: dict) -> list:
    """What is using a machine, as a member may be told: the names of the games and runs they may see, and
    "someone's <kind>" for the rest (``pool.seats.holders``: the leases live on it, each holder once)."""
    name = work_access.namer(sdb, user, cache=cache)
    return [name(h) for h in work_access.holders(server_id)]


class PriorityBody(BaseModel):
    """A new priority for a benchmark run, a probe run, a report, a lobby game or a lab experiment. Higher goes
    first."""
    kind: str
    id: str
    priority: int


@app.post("/api/queue/priority")
def work_queue_priority(body: PriorityBody, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Move work up or down the queue: the caller's own work, or work they manage (P3.2.4). A game is managed
    through its row in the games index. The work's pooled prompts follow it (P3.3.3: a ticket's priority is its
    class's base plus its work's)."""
    import json as _json
    from ..pool import queue as work_queue, scheduler as prompt_scheduler
    if body.kind not in work_queue.KINDS and body.kind != "lab":
        raise HTTPException(400, "kind is benchmark, probe, report, game or lab.")
    if body.kind == "game":
        row = ownership.row_for(sdb, body.id)
        if row is None:
            raise HTTPException(404, "No such game.")
        access.on(sdb, me, row).require(access.MANAGE)
    else:
        items.require(sdb, _QUEUE_ITEMS[body.kind], body.id, me, access.MANAGE)
    if body.kind in work_queue.KINDS:
        value = work_queue.set_priority(body.kind, body.id, body.priority)
        sched = prompt_scheduler.current()
        if sched is not None:
            sched.set_priority((body.kind, body.id), value)
        return {"kind": body.kind, "id": body.id, "priority": value}
    from .. import lab
    import re as _re
    if not _re.fullmatch(r"[A-Za-z0-9._-]{1,80}", body.id or ""):
        raise HTTPException(400, "Invalid experiment name.")
    path = lab.QUEUE / f"{body.id}.json"
    if not path.exists():
        raise HTTPException(404, "No queued experiment of that name.")
    spec = _json.loads(path.read_text(encoding="utf-8"))
    spec["priority"] = max(-100, min(100, body.priority))
    tmp = path.with_suffix(".tmp")
    tmp.write_text(_json.dumps(spec, indent=1), encoding="utf-8")
    tmp.replace(path)
    return {"kind": "lab", "id": body.id, "priority": spec["priority"]}


@app.post("/api/queue/tickets/{tid}/cancel")
def work_queue_cancel(tid: str, me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Cancel a pooled prompt (P3.3.8): its work's owner (MANAGE on it; for a prompt of no work, the member it is
    for), the machine's owner and administrators. Somebody who may see the prompt but none of those gets 403; anybody
    else 404, as for a prompt that is not there. A queued prompt leaves the queue; a running one is abandoned, its
    request draining on a helper that keeps generating it, and its caller falls back."""
    from ..db.models import Server
    from ..pool import scheduler as prompt_scheduler
    sched = prompt_scheduler.current()
    ticket = sched.find_ticket(tid) if sched is not None else None
    if ticket is None:
        raise HTTPException(404, "No such prompt.")
    row = sdb.get(Server, ticket.server_id) if ticket.server_id else None
    admin = _is_admin(me)
    entry = {"ref": list(ticket.ref) if ticket.ref else None, "consumer": ticket.consumer}
    view, _manage, cancel = work_access.binding_rights(sdb, me, entry, row, admin=admin, cache={})
    if not cancel:
        seen = admin or (row is not None and pool.visible(sdb, me, row) and view)
        raise HTTPException(403 if seen else 404, "You may not cancel that prompt." if seen else "No such prompt.")
    ticket.cancel("cancelled from the Queue page")
    audit.record(sdb, "queue.prompt_cancelled", actor=me, object_type="server", object_id=ticket.server_id,
                 cls=ticket.ticket_class, work=list(ticket.ref) if ticket.ref else None)
    return {"cancelled": tid, "state": ticket.state}


@app.get("/api/lab/report/{name}")
def lab_report(name: str, request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """The results of one lab experiment, for whoever may see it."""
    from .. import lab
    items.require(sdb, "lab_experiment", name, p.user, access.VIEW, slug=_share_key(request))
    if not (lab.QUEUE / f"{name}.json").exists() and not (lab.DONE / f"{name}.json").exists():
        raise HTTPException(404, f"No experiment '{name}'.")
    return {"name": name, "text": lab.report_text(name)}


@app.get("/api/models/scores")
def models_scores(me=Depends(require_user), sdb: DbSession = Depends(get_db)):
    """Every model's overall score and its components, over the games the caller's figures may use (as
    ``/api/metrics/compare``)."""
    from .scoring import model_scores, viewer_scope
    return model_scores(manager, _scheduler().get_settings()["score_weights"], scope=viewer_scope(sdb, me))


# ----------------------------------------------------------------------------
# replay / recap
# ----------------------------------------------------------------------------
@app.get("/api/games/{gid}/replay")
def replay(gid: str, request: Request, token: Optional[str] = None,
           p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """The whole game, turn by turn, for the recap.

    Gated on viewing the game, like everything else about it: a finished private game is still
    private. Whoever may watch it (the spectator token, or an account or share key the ladder lets see it)
    may replay an AI-only game live; a seat's own token sees it once it is over, as the view route says.
    """
    s, _row, perms = _gate(sdb, gid, p, access.VIEW, request, token)
    tok = _token(request, token)
    watcher = s.is_spectator(tok) or (s.seat_for_token(tok) is None and access.VIEW in perms)
    if not (s.game.phase != "playing" or (watcher and s.god_view_allowed())):
        raise HTTPException(403, "The recap is available when the game is over (or to spectators of AI-only games).")
    return Response(_replay_payload(s), media_type="application/json")


def _replay_payload(s: GameSession) -> bytes:
    """The replay as JSON bytes, built under the session lock so it cannot catch a half-applied turn: the engine
    writes it with the game's ``id`` and ``name`` first and each player's ``seat`` (``EngineGame.replay_json``), and the
    route returns those bytes as they are, as /view does."""
    with s.lock, _crash_guard(s):
        seats = [seat.public() for seat in s.seats]
        return s.game.replay_json({"id": s.id, "name": s.name, "seats": seats})


# ----------------------------------------------------------------------------
# saves
# ----------------------------------------------------------------------------
class SaveBody(BaseModel):
    """A named save."""
    name: Optional[str] = None


@app.post("/api/games/{gid}/save")
def save_game(gid: str, body: SaveBody, request: Request, p: Principal = Depends(principal),
              sdb: DbSession = Depends(get_db)):
    """Write a named save beside the autosaves."""
    s, _row, _perms = _gate(sdb, gid, p, access.MANAGE, request)
    path = s.save(body.name)
    return {"saved": str(path.relative_to(SAVE_DIR)).replace("\\", "/")}


@app.get("/api/saves")
def list_saves(p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """Saves belonging to games this caller may see.

    A save on disk has no owner of its own; it inherits the visibility of the game it came from.
    A save whose game has no index row is shown only to administrators.
    """
    allowed = ownership.visible_ids(sdb, p.user)
    saves = manager.list_saves()
    if allowed is None:
        return saves
    return [entry for entry in saves if (entry.get("game_id") or entry.get("id")) in allowed]


class LoadBody(BaseModel):
    """Which save to load."""
    path: str


class DeleteSaveBody(BaseModel):
    """Which save to delete."""
    path: str
    whole_game: bool = False


@app.post("/api/saves/delete")
def delete_save(body: DeleteSaveBody, request: Request, p: Principal = Depends(principal),
                sdb: DbSession = Depends(get_db)):
    """Delete a save, or every save of its game. A game none of whose saves are left, and that is not open, is
    deleted from the games index too."""
    _csrf(request, p)
    _require_save_access(sdb, p, body.path, access.MANAGE)
    try:
        deleted = manager.delete_save(body.path, body.whole_game)
    except KeyError:
        raise HTTPException(404, "Save not found.")
    gid = (body.path or "").replace("\\", "/").split("/")[0]
    if gid and manager.get(gid) is None and not any((SAVE_DIR / gid).glob("*.citar")):
        ownership.delete(sdb, gid, actor=p.user)
    return {"deleted": deleted}


@app.post("/api/saves/load")
def load_save(body: LoadBody, request: Request, p: Principal = Depends(principal),
              sdb: DbSession = Depends(get_db)):
    """Load a save into a live session.

    Games with AI seats start paused, so their model configuration can be checked before anything
    spends tokens against a stale endpoint.
    """
    _csrf(request, p)
    _require_save_access(sdb, p, body.path, access.MANAGE)
    path = (SAVE_DIR / body.path).resolve()
    if SAVE_DIR.resolve() not in path.parents or not path.exists():
        raise HTTPException(404, "Save not found.")
    try:
        s = manager.load(path)
    except ValueError as e:        # a save that does not load (LoadError): its message says why
        raise HTTPException(400, str(e))
    # The load indexed the game (ownership.index_loaded). A save whose row has no owner (its row was made by
    # the load, or by a database set aside for 0.1.6's) belongs to whoever loads it.
    row = ownership.row_for(sdb, s.id)
    if row is None:
        row = ownership.register(sdb, s, p.user, visibility=access.PRIVATE)
    elif row.owner_id is None and p.user is not None:
        row.owner_id = p.user.id
        row.data_sharing = p.user.data_sharing      # its new owner's default, as for a game they create
        s.owner_id = p.user.id
    return s.info(include_tokens=True)


def _csrf(request: Request, p: Principal) -> None:
    """A write by a caller a session cookie signed in carries the session's CSRF token (``_gate``'s rule, for the
    save routes, which have no game token to excuse it)."""
    if p.auth_session is not None:
        from ..auth.deps import _enforce_csrf
        _enforce_csrf(request, p)


def _require_save_access(sdb: DbSession, p: Principal, rel_path: str, permission: str):
    """Saves live under saves/<game id>/, so the game id is the first path segment."""
    gid = (rel_path or "").replace("\\", "/").split("/")[0]
    row = ownership.row_for(sdb, gid) if gid else None
    if row is None:
        # No index row: only an administrator can act on it. This covers pre-accounts saves until
        # the migration adopts them.
        if p.user is None or p.user.role != "admin":
            raise HTTPException(404, "Save not found.")
        return
    access.on(sdb, p.user, row).require(permission)


# ----------------------------------------------------------------------------
# sharing
# ----------------------------------------------------------------------------
class ShareBody(BaseModel):
    """A game's visibility and its spectator link."""
    visibility: Optional[str] = None
    data_sharing: Optional[str] = None
    rotate_slug: bool = False


@app.get("/api/games/{gid}/sharing")
def get_sharing(gid: str, request: Request, p: Principal = Depends(principal),
                sdb: DbSession = Depends(get_db)):
    """Who can see this game, and by what link; for whoever manages it, also who it is shared with and the last
    unpublish (who took it down, when and why)."""
    s, row, perms = _gate(sdb, gid, p, access.VIEW, request)
    out = ownership.to_client(row, perms, p.user, base_url=citar_settings.get().public_origin)
    if row is not None and access.MANAGE in perms:
        out["shared_with"] = access.shared_with(sdb, row, p.user)
        out["unpublished"] = access.last_unpublished(sdb, row, p.user.id if p.user else None)
    return out


class UnpublishBody(BaseModel):
    """Why a game is being taken down: the owner sees it."""
    reason: str = ""


@app.post("/api/games/{gid}/unpublish")
def unpublish_game(gid: str, body: UnpublishBody, request: Request, p: Principal = Depends(principal),
                   sdb: DbSession = Depends(get_db)):
    """Take a game down (P3.2.3): it becomes private, its share link is rotated so every copy of it dies, and the
    audit log keeps who did it and why, which its owner sees. For a moderator on a game published to the internet
    (or a link they were given), and for whoever manages it; nothing else of the game is a moderator's to touch."""
    me = require_user(request, p)
    s, row, perms = _gate(sdb, gid, p, access.VIEW, request)
    if row is None:
        raise HTTPException(404, "That game is not registered.")
    if access.MODERATE not in perms and access.MANAGE not in perms:
        perms.require(access.MODERATE)
    access.unpublish(sdb, row, actor=me, reason=body.reason)
    return ownership.to_client(row, access.on(sdb, me, row), me, base_url=citar_settings.get().public_origin)


@app.put("/api/games/{gid}/sharing")
def set_sharing(gid: str, body: ShareBody, request: Request, p: Principal = Depends(principal),
                sdb: DbSession = Depends(get_db)):
    """Change a game's visibility."""
    s, row, perms = _gate(sdb, gid, p, access.MANAGE, request)
    if row is None:
        raise HTTPException(404, "That game is not registered.")
    if body.visibility is not None:
        access.set_visibility(sdb, row, body.visibility, actor=p.user)
    if body.data_sharing is not None:
        if body.data_sharing not in ("pool", "private"):
            raise HTTPException(400, "data_sharing is 'pool' or 'private'.")
        row.data_sharing = body.data_sharing
        audit.record(sdb, "game.data_sharing", actor=p.user, object_type="game", object_id=gid,
                     to=body.data_sharing)
    if body.rotate_slug:
        access.rotate_slug(sdb, row, actor=p.user)
    return ownership.to_client(row, perms, p.user, base_url=citar_settings.get().public_origin)


class ShareWithBody(BaseModel):
    """The account to share a game with, and what they may do."""
    handle: str
    permission: str = "view"


@app.post("/api/games/{gid}/sharing/users")
def share_with_user(gid: str, body: ShareWithBody, request: Request,
                    p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """Share a game with one account."""
    from ..auth import accounts as _accounts
    s, row, _perms = _gate(sdb, gid, p, access.MANAGE, request)
    if row is None:
        raise HTTPException(404, "That game is not registered.")
    target = _accounts.by_handle(sdb, body.handle)
    if target is None or target.status in ("deleted", "suspended"):
        raise HTTPException(404, f"No account called '{body.handle}'.")
    access.grant(sdb, row, target, body.permission, actor=p.user)
    return {"shared_with": access.shared_with(sdb, row, p.user)}


@app.delete("/api/games/{gid}/sharing/users/{handle}")
def unshare_with_user(gid: str, handle: str, request: Request,
                      p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """Stop sharing a game with an account."""
    from ..auth import accounts as _accounts
    s, row, _perms = _gate(sdb, gid, p, access.MANAGE, request)
    if row is None:
        raise HTTPException(404, "That game is not registered.")
    target = _accounts.by_handle(sdb, handle)
    if target is None:
        raise HTTPException(404, f"No account called '{handle}'.")
    access.revoke(sdb, row, target, actor=p.user)
    return {"shared_with": access.shared_with(sdb, row, p.user)}


# ----------------------------------------------------------------------------
# websocket
# ----------------------------------------------------------------------------
def _ws_origin_ok(websocket: WebSocket) -> bool:
    """A websocket upgrade is not covered by SameSite, so the Origin is checked explicitly.

    Without this, any web page the viewer visits could open a socket to this server and ride their
    session cookie into a game they are watching.
    """
    origin = websocket.headers.get("origin")
    if not origin:
        return True          # not a browser: a script or the MCP client, with no ambient cookie
    cfg = citar_settings.get()
    if origin == cfg.public_origin:
        return True
    if cfg.local:
        from urllib.parse import urlparse
        host = urlparse(origin).hostname or ""
        return (host in ("localhost", "127.0.0.1", "::1") or host.startswith("192.168.")
                or host.startswith("10.") or host.endswith(".local"))
    return False


def _ws_viewer_may_watch(s: GameSession, websocket: WebSocket, share_key: str) -> bool:
    """Whether the account behind this socket's cookie (if any) may watch the game."""
    from ..auth import sessions as _sessions
    from .. import db as _db
    try:
        with _db.session() as sdb:
            row = _sessions.resolve(sdb, websocket.cookies.get(_sessions.COOKIE_NAME))
            user = _sessions.user_for(sdb, row)
            _row, perms = ownership.resolve(sdb, s, user, slug=share_key or None)
            return access.VIEW in perms
    except Exception:
        return False


@app.websocket("/ws/worker")
async def worker_socket(websocket: WebSocket):
    """Where a citar-worker dials in.

    The worker connects OUT to here, which is what makes a GPU behind a home router usable without
    exposing anything at that end. Authentication is the first frame; nothing else is accepted
    until a valid token has been presented.
    """
    from ..worker import protocol as P
    from .workers import WorkerConnection, authenticate, hub, record_connection

    await websocket.accept()
    connection = None
    try:
        raw = await asyncio.wait_for(websocket.receive_text(), timeout=30)
        hello = P.parse(raw)
        if hello.get("t") != P.HELLO:
            await websocket.send_text(P.error_frame(P.Failure(id="", message="Expected a hello frame.")))
            await websocket.close(code=4400)
            return
        if int(hello.get("version") or 0) != P.PROTOCOL_VERSION:
            await websocket.send_text(P.error_frame(P.Failure(
                id="", message=f"This server speaks worker protocol v{P.PROTOCOL_VERSION}; "
                               f"the worker speaks v{hello.get('version')}. Update citar-worker.")))
            await websocket.close(code=4426)
            return

        ip = request_ip(websocket)
        identity = await asyncio.to_thread(authenticate, hello.get("token") or "")
        if identity is None:
            # Deliberately vague, and rate limited: this endpoint is reachable by anyone.
            await asyncio.to_thread(_worker_auth_fail, ip)
            await websocket.send_text(P.error_frame(P.Failure(id="", message="That worker token was refused.")))
            await websocket.close(code=4401)
            return
        server_id, server_name, token_id = identity

        connection = WorkerConnection(websocket, server_id, hello, asyncio.get_running_loop(), token_id=token_id)
        refused = await hub().admit(connection)
        if refused:
            # taken away between the hello's check and the registration: told why, as a drop tells a helper
            await websocket.send_text(P.frame(P.SHUTDOWN, reason=refused))
            await websocket.close(code=4403)
            return
        await websocket.send_text(P.welcome_frame(P.Welcome(
            server_id=server_id, server_name=server_name,
            ping_seconds=citar_settings.get().worker_ping_seconds)))
        await asyncio.to_thread(record_connection, server_id, ip=ip,
                                hardware=hello.get("hardware"), models=hello.get("models"))
        print(f"worker connected: {server_name} ({hello.get('hostname')}, "
              f"{len(hello.get('models') or [])} models)", flush=True)

        while True:
            raw = await websocket.receive_text()
            try:
                data = P.parse(raw)
            except P.ProtocolError:
                continue
            connection.touch()
            kind = data.get("t")
            if kind == P.DONE:
                connection.resolve(data.get("id"), result=data)
            elif kind == P.ERROR:
                from .workers import WorkerError
                connection.resolve(data.get("id"), error=WorkerError(
                    data.get("message") or "The worker reported an error.",
                    refusal=data.get("refusal") or "", retryable=bool(data.get("retryable"))))
            elif kind == P.CATALOG:
                connection.models = list(data.get("models") or [])
                hub().models_changed(connection)
                await asyncio.to_thread(record_connection, server_id, models=connection.models)
            elif kind == P.PONG:
                pass
    except WebSocketDisconnect:
        pass
    except Exception:
        import traceback
        traceback.print_exc()
    finally:
        if connection is not None:
            hub().unregister(connection)
            print(f"worker disconnected: {connection.server_id}", flush=True)


def request_ip(websocket) -> str:
    """The client address for a WebSocket, honouring the trusted proxy count."""
    return (websocket.headers.get("x-forwarded-for", "").split(",")[-1].strip()
            or (websocket.client.host if websocket.client else "unknown"))


def _worker_auth_fail(ip: str):
    """Rate limit and record a bad worker token, so this endpoint cannot be brute forced."""
    from ..auth import audit, ratelimit
    from .. import db
    try:
        ratelimit.check("worker_auth", ip)
    except ratelimit.RateLimited:
        pass
    try:
        with db.session() as s:
            audit.record(s, "worker.auth_failed", actor_label="worker", ip=ip)
    except Exception:
        pass


@app.websocket("/ws/games/{gid}")
async def ws(websocket: WebSocket, gid: str, token: str = "", k: str = ""):
    """The live game connection: state updates, events and AI thoughts as they happen.

    Authenticated by seat token, share key or session - whichever the connection carries - and checked
    in the handler rather than by a dependency, because a WebSocket has no place to put a 401.
    """
    s = manager.get(gid)
    if s is None:
        await websocket.close(code=4404)
        return
    if not _ws_origin_ok(websocket):
        await websocket.close(code=4403)
        return
    seat = s.seat_for_token(token)
    spectator = s.is_spectator(token)
    if seat is None and not spectator:
        # No seat or spectator token: fall back to the account, or a `link` share key.
        if not _ws_viewer_may_watch(s, websocket, k):
            await websocket.close(code=4403)
            return
        spectator = True
    await websocket.accept()
    queue: asyncio.Queue = asyncio.Queue(maxsize=2000)
    loop = asyncio.get_running_loop()
    pid = seat.player if seat else None
    if seat:
        seat.connected = True

    def push(msg: dict):
        """Queue a message for this connection."""
        t = msg.get("type")
        if t == "event":
            ev = msg["event"]
            if pid is not None and ev.get("players") is not None and pid not in ev["players"]:
                return
            if pid is None and not s.god_view_allowed() and ev.get("players") is not None:
                return
            if pid is not None:
                # anonymise civilizations this seat has not met
                msg = {**msg, "event": s.game.event_view(ev, pid)}
        if t == "thought" and not (spectator and s.god_view_allowed()):
            return
        try:
            loop.call_soon_threadsafe(_safe_put, queue, msg)
        except RuntimeError:
            pass

    s.subscribers.append(push)

    async def sender():
        """Drain the queue to the socket, ending when the client goes away."""
        while True:
            msg = await queue.get()
            await websocket.send_json(msg)

    send_task = asyncio.create_task(sender())
    try:
        await websocket.send_json({"type": "hello", "player": pid, "spectator": spectator, "version": s.version})
        while True:
            await websocket.receive_text()
    except (WebSocketDisconnect, RuntimeError):
        pass
    finally:
        send_task.cancel()
        if push in s.subscribers:
            s.subscribers.remove(push)


def _safe_put(q: asyncio.Queue, msg: dict):
    """Put a message on a queue, dropping it if the queue is full.

    A slow or dead client must not be able to block a turn. The dropped message is a missed update in
    a browser, which the next one repairs; blocking here would stall the game for everybody.
    """
    try:
        q.put_nowait(msg)
    except asyncio.QueueFull:
        pass

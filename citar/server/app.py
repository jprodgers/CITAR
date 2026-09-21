"""HTTP + WebSocket API and static web client.

    python -m citar.server  [--host 127.0.0.1] [--port 8765]
"""
from __future__ import annotations

import asyncio
import time
from pathlib import Path
from typing import Optional

from fastapi import (Depends, FastAPI, HTTPException, Request, WebSocket, WebSocketDisconnect)
from sqlalchemy.orm import Session as DbSession
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from ..engine import tools as toolreg
from ..engine.game import ActionError
from ..engine.rules import get_rules
from ..engine.views import client_view
from .session import SessionManager, GameSession, SAVE_DIR
from .benchmarks import BenchmarkScheduler
from .admin_api import router as admin_router
from .setup_api import router as setup_router
from .auth_api import router as auth_router
from .pool_api import router as pool_router
from .share_api import router as share_router
from .. import settings as citar_settings
from ..auth import access, audit
from ..auth.deps import Principal, get_db, principal, require_cap, require_user
from . import ownership

WEB_DIR = Path(__file__).resolve().parent.parent / "web"

app = FastAPI(title="CITAR — Civ Inspired Tool for AI Research", version="1.0")
app.include_router(auth_router)
app.include_router(pool_router)
app.include_router(share_router)
app.include_router(admin_router)
app.include_router(setup_router)
manager = SessionManager()
scheduler: Optional[BenchmarkScheduler] = None
_loop: Optional[asyncio.AbstractEventLoop] = None


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
    from .workers import hub
    hub().attach_loop(_loop)                      # lets game threads reach connected workers
    asyncio.create_task(hub().ping_loop(citar_settings.get().worker_ping_seconds))
    # lobby games first, so queued benchmarks and probes see which machines they hold
    restored = manager.restore_live()
    if restored:
        print("restored games: " + "; ".join(restored), flush=True)
    if scheduler is None:
        scheduler = BenchmarkScheduler(manager)   # reloads benchmark games that were in progress
    _probes()                                     # resumes probe runs that were queued or running
    from .. import usage
    usage.start_power_sampler()                   # live power samples of this machine for cost reports
    usage.tracker().start()


@app.on_event("shutdown")
def _shutdown():
    """Flush the usage ledger and stop background work cleanly."""
    from .. import usage
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
    """
    s = _session(gid)
    row, perms = ownership.require(sdb, s, p.user, permission,
                                   slug=_share_key(request), seat_token=_token(request, token))
    return s, row, perms


def _game_limit(sdb: DbSession, user) -> None:
    """Stop one account filling the server with games.

    The VPS runs every game, so concurrency is a shared resource: without a cap, one person starting
    twenty 24-civ games is a denial of service against everybody else on the box.
    """
    if user is None or user.role == "admin":
        return
    from ..db.models import Game as _GameRow
    from sqlalchemy import func, select as _select
    live = sdb.scalar(_select(func.count()).select_from(_GameRow).where(
        _GameRow.owner_id == user.id, _GameRow.deleted_at.is_(None),
        _GameRow.status.in_(("setup", "playing"))))
    cap = max(1, user.max_concurrent_games or 2)
    if (live or 0) >= cap:
        raise HTTPException(429,
                            f"You already have {live} games running and your limit is {cap}. "
                            "Finish or delete one first, or ask an administrator to raise the limit.")


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
        "frame-src https://*.hcaptcha.com; "
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
    return get_rules().to_client()


@app.get("/api/tools")
def tool_list():
    """Every player tool with its JSON schema. The authoritative reference for an agent."""
    return toolreg.tool_list()


class ModelProbe(BaseModel):
    """A request to check whether a model endpoint answers."""
    provider: str = "openai_compatible"
    base_url: Optional[str] = None
    api_key_env: Optional[str] = None


# Fetches from a caller-supplied base_url, so this is an SSRF vector; never anonymous.
@app.post("/api/llm/models", dependencies=[Depends(require_user)])
def probe_models(body: ModelProbe):
    """List the models an endpoint offers (used by the lobby's 'Detect models' button and to test connectivity)."""
    import os
    import httpx
    provider = (body.provider or "").lower()
    key = os.environ.get(body.api_key_env) if body.api_key_env else None
    try:
        if provider == "dryrun":
            return {"ok": True, "models": ["dry-run"], "loaded": ["dry-run"], "details": {}}
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
        info["sharing"] = ownership.to_client(row, perms, base_url=citar_settings.get().public_origin)
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
        phase = s.game.s.phase
        games["over" if phase != "playing" else "paused" if s.paused else "playing"] += 1
        statuses = list(s.agent_status.values())
        games["ai_thinking"] += statuses.count("thinking")
        games["ai_reconnecting"] += statuses.count("reconnecting")
        if any(getattr(a, "last_error", None) for a in s.agents.values()) or (s.pause_reason or {}).get("kind") == "disconnect":
            games["problems"] += 1
    workers = hub().status()
    out = {"version": __version__, "uptime": round(time.time() - _STARTED), "user": p.user.handle, "admin": admin,
           "mode": "server" if citar_settings.get().server_mode else "local", "games": games,
           "benchmarks": ({k: v for k, v in scheduler.status().items() if k != "log"} if scheduler else None),
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
    return out


@app.get("/api/games/{gid}/summary")
def game_summary(gid: str, request: Request, p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """One game at a glance, for the phone site: standings, whose turn, what each AI is doing, recent news.

    Standings are shown to whoever manages the game, or to anyone once the game is over or has no
    human in it; a spectator of a game somebody is still playing gets names and the turn, nothing
    the fog of war would hide.
    """
    from ..engine import research, victory
    s, _row, perms = _gate(sdb, gid, p, access.VIEW, request)
    with s.lock:
        g = s.game
        full = access.MANAGE in perms or s.god_view_allowed()
        seats = {seat.player: seat for seat in s.seats}
        players = []
        for pl in g.s.players:
            if pl.kind != "major":
                continue
            seat = seats.get(pl.id)
            d = {"id": pl.id, "name": pl.name, "color": pl.color, "alive": pl.alive,
                 "seat": seat.type if seat else None,
                 "model": (seat.llm.get("model") or seat.llm.get("model_id")) if seat and seat.type == "llm" else None,
                 "status": s.agent_status.get(pl.id),
                 "error": getattr(s.agents.get(pl.id), "last_error", None) if full else None}
            if full:
                cities = g.player_cities(pl.id)
                d.update({"score": victory.score(g, pl.id)["total"], "cities": len(cities),
                          "pop": sum(c.pop for c in cities), "techs": len(pl.techs),
                          "era": g.rules.era_list[research.player_era(g, pl.id)]})
            players.append(d)
        if full:
            players.sort(key=lambda d: (not d["alive"], -d.get("score", 0)))
        events = [{"turn": e["turn"], "type": e["type"], "text": e["text"]}
                  for e in g.s.events[-300:] if e.get("players") is None][-20:]
        info = s.info()
        current = g.player(g.s.current).name if g.s.phase == "playing" else None
        return {**{k: info[k] for k in ("id", "name", "turn", "phase", "winner", "victory", "paused", "pause_reason",
                                        "ai_delay", "created", "config")},
                "turn_limit": g.total_turns(), "current": current, "players": players, "events": events,
                "full": full, "can_manage": access.MANAGE in perms}


@app.post("/api/games")
def create_game(body: CreateGame, request: Request,
                me=Depends(require_cap("create_games")), sdb: DbSession = Depends(get_db)):
    """Create a game and return it, with a seat token for each seat."""
    top = get_rules().const["max_players"]
    if not 1 <= len(body.seats) <= top:
        raise HTTPException(400, f"A game needs 1 to {top} seats.")
    if len(body.seats) > max(1, me.max_seats_per_game or top):
        raise HTTPException(403, f"Your account is limited to {me.max_seats_per_game} seats per game.")
    _game_limit(sdb, me)
    if body.visibility not in ("private", "allowlist", "link", "public"):
        raise HTTPException(400, "Visibility is private, allowlist, link or public.")
    from ..pool import seats as pool_seats
    try:
        pool_seats.authorize(sdb, me, body.seats)
    except ValueError as e:
        raise HTTPException(403, str(e))
    try:
        s = manager.create(body.config, body.seats, body.name)
    except ValueError as e:
        raise HTTPException(400, str(e))
    row = ownership.register(sdb, s, me, visibility="private", data_sharing=body.data_sharing)
    if body.visibility != "private":
        # Routed through set_visibility so publishing checks the publish_public capability.
        access.set_visibility(sdb, row, body.visibility, actor=me)
    audit.record(sdb, "game.created", actor=me, object_type="game", object_id=s.id,
                 seats=len(body.seats), visibility=row.visibility)
    info = s.info(include_tokens=True)
    info["sharing"] = ownership.to_client(row, access.Access({access.VIEW, access.PLAY, access.MANAGE}),
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
    info["sharing"] = ownership.to_client(row, perms, base_url=citar_settings.get().public_origin)
    return info


@app.delete("/api/games/{gid}")
def delete_game(gid: str, request: Request, p: Principal = Depends(principal),
                sdb: DbSession = Depends(get_db)):
    """Delete a game and every save of it."""
    _gate(sdb, gid, p, access.MANAGE, request)
    ownership.delete(sdb, gid, actor=p.user)
    manager.delete(gid)
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
    """Change who controls a seat (e.g. hand a bot seat to an MCP client, or take over as human)."""
    s, _row, _perms = _gate(sdb, gid, p, access.MANAGE, request)
    if not 0 <= pid < len(s.seats):
        raise HTTPException(404, "No such seat.")
    with s.lock:
        seat = s.seats[pid]
        if body.type:
            if body.type not in ("human", "mcp", "llm", "bot"):
                raise HTTPException(400, "Invalid seat type.")
            seat.type = body.type
        if body.llm is not None:
            seat.llm = body.llm
        if body.bot is not None:
            seat.bot = body.bot
        if body.name is not None:
            seat.name = body.name
        s.cancel_agent(pid)  # aborts a turn in progress so the new controller takes over
        s.cond.notify_all()
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
            s.paused = body.paused
            s.pause_reason = None          # a person paused or resumed it: no longer the game's own pause
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
    """
    s, _row, perms = _gate(sdb, gid, p, access.VIEW, request, token)
    tok = _token(request, token)
    with s.lock:
        seat = s.seat_for_token(tok)
        if seat is not None:
            v = client_view(s.game, seat.player)
            v["seat"] = seat.public()
        elif s.is_spectator(tok) or access.VIEW in perms:
            if as_player is not None:
                if not 0 <= as_player < len(s.seats):
                    raise HTTPException(404, "No such player.")
                v = client_view(s.game, as_player)
            elif s.god_view_allowed():
                v = client_view(s.game, None)
            else:
                raise HTTPException(403, "God view is disabled while humans are playing.")
            v["spectator"] = True
        else:
            raise HTTPException(403, "Invalid token.")  # unreachable: _gate already checked
        v["session"] = s.info()
        v["version"] = s.version
        return JSONResponse(v)


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
    from ..engine import movement
    s, _row, _perms = _gate(sdb, gid, p, access.PLAY, request, token)
    pid = _seat_pid(s, _token(request, token))
    with s.lock:
        g = s.game
        u = g.unit(unit_id)
        if u is None or u.owner != pid or not g.grid.in_bounds(x, y):
            return {"path": None}
        target = g.grid.idx(x, y)
        path = movement.find_path(g, u, target)
        if not path:
            return {"path": None}
        return {"path": [list(g.grid.xy(i)) for i in path], "turns": movement.path_turns(g, u, path)}


@app.get("/api/games/{gid}/wait")
def wait_for_turn(gid: str, request: Request, token: Optional[str] = None, timeout: float = 50.0,
                  p: Principal = Depends(principal), sdb: DbSession = Depends(get_db)):
    """Long-poll until it is this seat's turn, or a negotiation needs an answer.

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
    from ..engine import visibility
    with s.lock:
        g = s.game
        if action == "meet_all":
            for a in g.majors():
                for b in g.majors():
                    g.meet(a.id, b.id)
        elif action == "reveal":
            for p in g.majors():
                visibility.reveal_tiles(g, p.id, range(g.grid.size))
        elif action == "gold":
            for p in g.majors():
                p.gold += 500
        else:
            raise HTTPException(400, "Unknown debug action.")
        s._after_action(g.turn, g.s.current)
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
    from fastapi.responses import Response
    s, _row, _perms = _gate(sdb, gid, p, access.VIEW, request)
    with s.lock:
        rows = s.metrics.turn_rows()
        names = {p.id: p.name for p in s.game.s.players}
    cols = ["turn", "player", "civ", "controller", "model", "wall_s", "model_steps", "model_s", "input_tokens",
            "output_tokens", "reasoning_tokens", "tool_calls", "actions_ok", "queries", "errors", "repeats",
            "blocked_repeats", "malformed", "stall_nudges", "peak_prompt_tokens", "slowest_step_s", "context_length",
            "model_loaded_at_start", "end_reason", "top_tools"]
    buf = io.StringIO()
    w = csv.DictWriter(buf, fieldnames=cols, extrasaction="ignore")
    w.writeheader()
    for r in rows:
        w.writerow({**r, "civ": names.get(r["player"], "")})
    return Response(buf.getvalue(), media_type="text/csv",
                    headers={"Content-Disposition": f'attachment; filename="citar-{gid}-metrics.csv"'})


@app.get("/api/metrics/compare", dependencies=[Depends(require_user)])
def compare_models():
    """Aggregate AI metrics per model across all running games and saved games (latest save per game)."""
    from .scoring import compare_models as compare
    return compare(manager)


@app.get("/api/games/{gid}/debug/errors")
def errors(gid: str):
    """Recent errors from AI seats. The first place to look when a model does nothing."""
    s = _session(gid)
    return {"errors": s.errors[-50:], "agents": {pid: {"usage": getattr(a, "usage_total", None),
                                                       "last_error": getattr(a, "last_error", None)}
                                                 for pid, a in s.agents.items()}}


# ----------------------------------------------------------------------------
# benchmarks & model scores
# ----------------------------------------------------------------------------
def _bench_call(fn, *args):
    """Call a scheduler method, turning its errors into HTTP status codes."""
    try:
        return fn(*args)
    except KeyError as e:
        raise HTTPException(404, f"Not found: {e}")
    except ValueError as e:
        raise HTTPException(400, str(e))


@app.get("/api/benchmarks/status", dependencies=[Depends(require_user)])
def bench_status():
    """What the benchmark scheduler is doing: running jobs, queues, restricted servers."""
    return _scheduler().status()


@app.get("/api/benchmarks/settings", dependencies=[Depends(require_user)])
def bench_settings():
    """The scoring weights."""
    return _scheduler().get_settings()


@app.put("/api/benchmarks/settings", dependencies=[Depends(require_user)])
def bench_update_settings(body: dict):
    """Change the scoring weights."""
    return _bench_call(_scheduler().update_settings, body)


@app.get("/api/benchmarks/suites", dependencies=[Depends(require_user)])
def bench_suites():
    """Every saved benchmark suite."""
    return _scheduler().list_suites()


@app.get("/api/benchmarks/suites/new", dependencies=[Depends(require_user)])
def bench_new_suite():
    """A blank suite with sensible defaults, for the editor to start from."""
    from .benchmarks import normalize_suite
    from .. import servers as REG
    host = REG.host()
    first = host if host and host["models"] else next((s for s in REG.list_servers() if s["models"] and s["kind"] != "test"), None)
    return normalize_suite({"name": "New benchmark suite", "servers": [{"server_id": first["id"], "models": []}] if first else []})


@app.get("/api/benchmarks/suites/{suite_id}", dependencies=[Depends(require_user)])
def bench_suite(suite_id: str):
    """One suite."""
    return _bench_call(_scheduler().get_suite, suite_id)


@app.post("/api/benchmarks/suites", dependencies=[Depends(require_user)])
def bench_save_suite(body: dict):
    """Create or update a suite."""
    return _bench_call(_scheduler().save_suite, body)


@app.delete("/api/benchmarks/suites/{suite_id}", dependencies=[Depends(require_user)])
def bench_delete_suite(suite_id: str):
    """Delete a suite. Its finished runs are kept."""
    _bench_call(_scheduler().delete_suite, suite_id)
    return {"deleted": suite_id}


class RunBody(BaseModel):
    """A request to start a run: a saved suite, or one supplied inline."""
    suite_id: Optional[str] = None
    suite: Optional[dict] = None
    name: Optional[str] = None


@app.post("/api/benchmarks/runs", dependencies=[Depends(require_user)])
def bench_start_run(body: RunBody):
    """Start a benchmark run."""
    sch = _scheduler()
    suite = body.suite or (_bench_call(sch.get_suite, body.suite_id) if body.suite_id else None)
    if not suite:
        raise HTTPException(400, "Give a suite_id or a suite.")
    run = _bench_call(sch.create_run, suite, body.name)
    return {"id": run["id"], "jobs": len(run["jobs"])}


@app.get("/api/benchmarks/runs", dependencies=[Depends(require_user)])
def bench_runs():
    """Every run, with per-game progress."""
    return _scheduler().list_runs()


class RunControl(BaseModel):
    """An action on a run or a job: pause, resume, cancel, skip, retry."""
    action: str


@app.post("/api/benchmarks/runs/{run_id}/control", dependencies=[Depends(require_user)])
def bench_run_control(run_id: str, body: RunControl):
    """Pause, resume or cancel a whole run."""
    _bench_call(_scheduler().control_run, run_id, body.action)
    return {"ok": True}


@app.post("/api/benchmarks/runs/{run_id}/jobs/{job_id}/control", dependencies=[Depends(require_user)])
def bench_job_control(run_id: str, job_id: str, body: RunControl):
    """Pause, resume, skip or retry one game in a run."""
    _bench_call(_scheduler().control_job, run_id, job_id, body.action)
    return {"ok": True}


@app.post("/api/benchmarks/runs/{run_id}/jobs/{job_id}/watch", dependencies=[Depends(require_user)])
def bench_job_watch(run_id: str, job_id: str):
    """Open a running benchmark game so it can be watched live."""
    s = _bench_call(_scheduler().open_job_game, run_id, job_id)
    return {"game_id": s.id, "spectator_token": s.spectator_token, "phase": s.game.s.phase}


# ----------------------------------------------------------------------------
# custom maps (map editor)
# ----------------------------------------------------------------------------
def _map_call(fn, *args, **kw):
    """Call a map function, turning its errors into HTTP status codes."""
    from ..engine import maps
    try:
        return fn(*args, **kw)
    except maps.MapError as e:
        raise HTTPException(400, str(e))


@app.get("/api/maps", dependencies=[Depends(require_user)])
def maps_list():
    """Saved maps."""
    from ..engine import maps
    return maps.list_maps()


@app.get("/api/maps/{map_id}", dependencies=[Depends(require_user)])
def maps_get(map_id: str):
    """One saved map."""
    from ..engine import maps
    return _map_call(maps.load_map, map_id)


@app.put("/api/maps/{map_id}", dependencies=[Depends(require_user)])
def maps_put(map_id: str, body: dict):
    """Create or replace a saved map."""
    from ..engine import maps
    body = dict(body)
    body["id"] = map_id
    clean, warnings = _map_call(maps.save_map, get_rules(), body)
    return {"map": maps.summary(clean), "warnings": warnings}


@app.delete("/api/maps/{map_id}", dependencies=[Depends(require_user)])
def maps_delete(map_id: str):
    """Delete a saved map."""
    from ..engine import maps
    _map_call(maps.delete_map, map_id)
    return {"deleted": map_id}


@app.post("/api/maps/validate", dependencies=[Depends(require_user)])
def maps_validate(body: dict):
    """Check a map for problems - unreachable starts, missing resources - without saving it."""
    from ..engine import maps
    clean, warnings = _map_call(maps.validate, get_rules(), body)
    return {"map": maps.summary(clean), "warnings": warnings}


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


@app.post("/api/maps/generate", dependencies=[Depends(require_user)])
def maps_generate(body: MapGenBody):
    """Generate a map, for the editor to start from."""
    from ..engine import maps
    R = get_rules()
    size = R.const["map_sizes"].get(body.map_size or "small", R.const["map_sizes"]["small"])
    w, h = body.width or size["width"], body.height or size["height"]
    if body.blank:
        if body.blank not in R.terrains or R.terrains[body.blank]["type"] not in ("Land", "Water"):
            raise HTTPException(400, f"Unknown base terrain '{body.blank}'.")
        return _map_call(maps.blank_map, w, h, body.blank, body.name)
    players = body.players if body.players is not None else size["players"]
    cs = body.city_states if body.city_states is not None else size["city_states"]
    options = {"map_edges": body.map_edges, "river_density": body.river_density, "resources": body.resources}
    return _map_call(maps.generated_map, R, w, h, body.map_type, max(1, players), max(0, cs), body.seed,
                     body.ruins, body.name, options)


@app.post("/api/games/{gid}/export_map")
def maps_from_game(gid: str, request: Request, p: Principal = Depends(principal),
                   sdb: DbSession = Depends(get_db)):
    """Extract the terrain of a game as a reusable map.

    Gated on VIEW: the map is the shape of somebody's game world, and handing it to a stranger
    reveals the layout of a private game. Found by scripts/audit_routes.py, which exists because
    this is exactly the kind of route that gets added and forgotten.
    """
    from ..engine import maps
    s, _row, _perms = _gate(sdb, gid, p, access.VIEW, request)
    with s.lock:
        return maps.map_from_game(s.game, f"{s.name} terrain")


# ----------------------------------------------------------------------------
# scenarios and the scenario editor
# ----------------------------------------------------------------------------
import copy as _copy
import secrets as _secrets
import threading as _threading

EDITORS: dict[str, dict] = {}     # editor id -> {"game", "undo": [state dicts], "meta", "lock", "t"}
EDITOR_UNDO = 25


def _editor(eid: str) -> dict:
    """The open scenario editor with this id, or a 404."""
    ed = EDITORS.get(eid)
    if ed is None:
        raise HTTPException(404, "This scenario editor session has expired; reopen the scenario.")
    ed["t"] = time.time()
    return ed


def _editor_payload(eid: str, ed: dict, results=None) -> dict:
    """The editor's current state, as the client needs it."""
    from ..engine import scenario as S
    g = ed["game"]
    view = client_view(g, None, event_limit=0)
    for k in ("events", "thoughts", "messages", "negotiations", "stats", "empires"):
        view.pop(k, None)
    return {"editor_id": eid, "meta": ed["meta"], "view": view, "overview": S.overview(g),
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


@app.post("/api/scenario-editor", dependencies=[Depends(require_user)])
def editor_open(body: EditorOpen):
    """Open a scenario editor and return its id."""
    from ..engine import scenario as S
    from ..engine.game import Game
    meta = {"id": "", "name": "", "description": "", "seats": None}
    try:
        if body.source in ("map", "generate"):
            cfg = dict(body.config)
            if body.source == "map":
                if not body.map:
                    raise HTTPException(400, "Choose a map.")
                cfg["map"] = body.map
            players = body.players or [{"type": "human"}, {"type": "llm"}]
            if len(players) > get_rules().const["max_players"]:
                raise HTTPException(400, "Too many players.")
            cfg["players"] = [{"nation": p.get("nation") or None, "difficulty": p.get("difficulty") or None,
                               "controller": (p.get("type") if p.get("type") in ("human", "llm", "mcp", "bot") else "human"),
                               "name": p.get("civ_name") or None} for p in players]
            g = Game.new(cfg)
            meta["seats"] = [{"type": p.get("type") or "bot"} for p in players]
            meta["name"] = f"Scenario on {cfg.get('map') or cfg.get('map_type', 'a new map')}"
        elif body.source == "scenario":
            data = S.load_scenario(body.scenario or "")
            g = S.game_from_state(data["state"])
            meta.update({k: data.get(k) for k in ("id", "name", "description", "seats")})
        elif body.source == "save":
            from .session import load_save_file
            p = (SAVE_DIR / (body.save or "")).resolve()
            if SAVE_DIR.resolve() not in p.parents or not p.exists():
                raise HTTPException(404, "Save not found.")
            g = S.game_from_state(load_save_file(p)["state"])
            meta["name"] = f"Scenario from {body.save}"
        elif body.source == "game":
            s = _session(body.game_id or "")
            with s.lock:
                s.game.save_rng()
                g = S.game_from_state(s.game.s.to_dict())
            meta["name"] = f"Scenario from {s.name} turn {g.turn}"
            meta["seats"] = [{"type": seat.type if seat.type != "human" else "human", "label": seat.name} for seat in s.seats]
        else:
            raise HTTPException(400, f"Unknown source '{body.source}'.")
    except (ValueError, ActionError) as e:
        raise HTTPException(400, str(e))
    if meta["seats"] is None:
        meta["seats"] = S.default_seats(g)
    meta["seats"] = S.normalize_seats(g, meta["seats"])
    # drop idle editor sessions (an hour without use)
    for k in [k for k, v in EDITORS.items() if time.time() - v["t"] > 3600]:
        EDITORS.pop(k, None)
    eid = _secrets.token_hex(4)
    EDITORS[eid] = {"game": g, "undo": [], "meta": meta, "lock": _threading.Lock(), "t": time.time()}
    return _editor_payload(eid, EDITORS[eid])


@app.get("/api/scenario-editor/{eid}", dependencies=[Depends(require_user)])
def editor_get(eid: str):
    """The current state of an open editor."""
    return _editor_payload(eid, _editor(eid))


class EditorOps(BaseModel):
    """A list of edit operations to apply."""
    ops: list[dict]


@app.post("/api/scenario-editor/{eid}/ops", dependencies=[Depends(require_user)])
def editor_ops(eid: str, body: EditorOps):
    """Apply edits to the scenario being edited, keeping an undo step."""
    from ..engine import scenario as S
    ed = _editor(eid)
    with ed["lock"]:
        g = ed["game"]
        g.save_rng()
        before = _copy.deepcopy(g.s.to_dict())
        try:
            results = S.apply_ops(g, body.ops)
        except ActionError as e:
            ed["game"] = S.game_from_state(before)        # operations are all-or-nothing
            raise HTTPException(400, str(e))
        ed["undo"].append(before)
        del ed["undo"][:-EDITOR_UNDO]
        return _editor_payload(eid, ed, results)


@app.post("/api/scenario-editor/{eid}/undo", dependencies=[Depends(require_user)])
def editor_undo(eid: str):
    """Undo the last set of edits."""
    from ..engine import scenario as S
    ed = _editor(eid)
    with ed["lock"]:
        if not ed["undo"]:
            raise HTTPException(400, "Nothing to undo.")
        ed["game"] = S.game_from_state(ed["undo"].pop())
        return _editor_payload(eid, ed)


class EditorMeta(BaseModel):
    """A scenario's name and description."""
    id: Optional[str] = None
    name: Optional[str] = None
    description: Optional[str] = None
    seats: Optional[list] = None


@app.post("/api/scenario-editor/{eid}/meta", dependencies=[Depends(require_user)])
def editor_meta(eid: str, body: EditorMeta):
    """Rename the scenario being edited."""
    from ..engine import scenario as S
    ed = _editor(eid)
    for k in ("id", "name", "description"):
        if getattr(body, k) is not None:
            ed["meta"][k] = getattr(body, k)
    if body.seats is not None:
        ed["meta"]["seats"] = S.normalize_seats(ed["game"], body.seats)
    return {"meta": ed["meta"]}


@app.post("/api/scenario-editor/{eid}/save", dependencies=[Depends(require_user)])
def editor_save(eid: str, body: EditorMeta):
    """Save the scenario being edited."""
    from ..engine import scenario as S
    ed = _editor(eid)
    editor_meta(eid, body)
    m = ed["meta"]
    if not (m.get("id") or m.get("name")):
        raise HTTPException(400, "Give the scenario a name.")
    with ed["lock"]:
        try:
            summ = S.save_scenario(ed["game"], m.get("id") or m["name"], m.get("name") or m["id"],
                                   m.get("description", ""), m.get("seats"))
        except ActionError as e:
            raise HTTPException(400, str(e))
    m["id"] = summ["id"]
    return summ


@app.delete("/api/scenario-editor/{eid}", dependencies=[Depends(require_user)])
def editor_close(eid: str):
    """Close an editor and release its game."""
    EDITORS.pop(eid, None)
    return {"closed": eid}


@app.get("/api/scenario-ops", dependencies=[Depends(require_user)])
def scenario_ops_help():
    """The list of edit operations, with their parameters. The reference for the Operations tab."""
    from ..engine import scenario as S
    return S.ops_help()


@app.get("/api/scenarios", dependencies=[Depends(require_user)])
def scenarios_list():
    """Saved scenarios."""
    from ..engine import scenario as S
    return S.list_scenarios()


@app.get("/api/scenarios/{sid}", dependencies=[Depends(require_user)])
def scenarios_get(sid: str):
    """One scenario."""
    from ..engine import scenario as S
    try:
        return S.summary(S.load_scenario(sid))
    except ActionError as e:
        raise HTTPException(404, str(e))


@app.delete("/api/scenarios/{sid}", dependencies=[Depends(require_user)])
def scenarios_delete(sid: str):
    """Delete a scenario."""
    from ..engine import scenario as S
    try:
        S.delete_scenario(sid)
    except ActionError as e:
        raise HTTPException(400, str(e))
    return {"deleted": sid}


class LaunchBody(BaseModel):
    """How to start a game from a scenario: which seats are played by what."""
    name: str = ""
    seats: list[dict] = []


@app.post("/api/scenarios/{sid}/launch", dependencies=[Depends(require_user)])
def scenarios_launch(sid: str, body: LaunchBody):
    """Start a playable game from a saved scenario."""
    from ..engine import scenario as S
    try:
        scn = S.load_scenario(sid)
        s = manager.create_from_scenario(scn, body.seats, body.name or scn["name"])
    except (ActionError, ValueError) as e:
        raise HTTPException(400, str(e))
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


@app.get("/api/probes", dependencies=[Depends(require_user)])
def probes_list():
    """Saved probes."""
    from .. import probes as P
    return P.list_probes()


@app.get("/api/probes/example", dependencies=[Depends(require_user)])
def probes_example(scenario: str, subject: int = 1, counterparty: int = 0):
    """An example probe, as a starting point."""
    from .. import probes as P
    return P.example_probe(scenario, subject, counterparty)


@app.get("/api/probes/{pid}", dependencies=[Depends(require_user)])
def probes_get(pid: str):
    """One probe."""
    from .. import probes as P
    return _probe_call(P.load_probe, pid)


@app.put("/api/probes/{pid}", dependencies=[Depends(require_user)])
def probes_put(pid: str, body: dict):
    """Create or replace a probe."""
    from .. import probes as P
    body = dict(body)
    body["id"] = pid
    return _probe_call(P.save_probe, body)


@app.post("/api/probes/validate", dependencies=[Depends(require_user)])
def probes_validate(body: dict):
    """Check a probe's cases against its scenario without running them."""
    from .. import probes as P
    return _probe_call(P.validate, body)


@app.delete("/api/probes/{pid}", dependencies=[Depends(require_user)])
def probes_delete(pid: str):
    """Delete a probe."""
    from .. import probes as P
    _probe_call(P.delete_probe, pid)
    return {"deleted": pid}


class ProbeRunBody(BaseModel):
    """Which model to run a probe against, and how many repeats."""
    llm: dict
    repeats: Optional[int] = None
    name: str = ""
    cases: Optional[list[str]] = None


@app.post("/api/probes/{pid}/run", dependencies=[Depends(require_user)])
def probes_run(pid: str, body: ProbeRunBody):
    """Run a probe: every case, from the same starting state, against one model."""
    return _probe_call(_probes().start, pid, body.llm, body.repeats, body.name, body.cases)


@app.get("/api/probe-runs", dependencies=[Depends(require_user)])
def probe_runs():
    """Every probe run."""
    runner = _probes()
    out = []
    for r in runner.list_runs():
        out.append({k: r.get(k) for k in ("id", "name", "status", "done", "created", "started", "finished",
                                          "summary", "error", "repeats")} |
                   {"total": len(r.get("jobs", [])), "probe_id": r["probe"].get("id"), "probe_name": r["probe"].get("name"),
                    "model": r.get("llm", {}).get("model"), "provider": r.get("llm", {}).get("provider")})
    return {"runs": out, "status": runner.status()}


@app.get("/api/probe-runs/{rid}", dependencies=[Depends(require_user)])
def probe_run(rid: str):
    """One probe run, with per-case results."""
    runner = _probes()
    run = _probe_call(runner.get_run, rid)
    return {"run": run, "results": runner.results(rid), "status": runner.status()}


@app.post("/api/probe-runs/{rid}/stop", dependencies=[Depends(require_user)])
def probe_run_stop(rid: str):
    """Stop a probe run in progress."""
    return _probe_call(_probes().stop, rid)


@app.delete("/api/probe-runs/{rid}", dependencies=[Depends(require_user)])
def probe_run_delete(rid: str):
    """Delete a probe run."""
    _probe_call(_probes().delete, rid)
    return {"deleted": rid}


@app.post("/api/probe-runs/{rid}/open/{case}/{rep}", dependencies=[Depends(require_user)])
def probe_run_open(rid: str, case: str, rep: int):
    """Opens the saved end state of one case as a (paused) game for inspection."""
    runner = _probes()
    path = _probe_call(runner._run_dir, rid) / f"{case}-{rep}.citar"
    if not path.exists():
        raise HTTPException(404, "No saved game for that case.")
    s = manager.load(path)
    return {"game_id": s.id, "spectator_token": s.spectator_token}


@app.get("/api/lab", dependencies=[Depends(require_user)])
def lab_overview():
    """What the bot lab is doing: runner health, experiments, per-game progress and the log."""
    from .. import lab
    return lab.overview()


@app.get("/api/lab/report/{name}", dependencies=[Depends(require_user)])
def lab_report(name: str):
    """The results of one lab experiment."""
    from .. import lab
    if not (lab.QUEUE / f"{name}.json").exists() and not (lab.DONE / f"{name}.json").exists():
        raise HTTPException(404, f"No experiment '{name}'.")
    return {"name": name, "text": lab.report_text(name)}


@app.get("/api/models/scores", dependencies=[Depends(require_user)])
def models_scores():
    """Every model's overall score and its components."""
    from .scoring import model_scores
    return model_scores(manager, _scheduler().get_settings()["score_weights"])


# ----------------------------------------------------------------------------
# replay / recap
# ----------------------------------------------------------------------------
@app.get("/api/games/{gid}/replay")
def replay(gid: str, request: Request, token: Optional[str] = None):
    """The whole game, turn by turn, for the recap."""
    s = _session(gid)
    tok = _token(request, token)
    if not (s.game.s.phase != "playing" or (s.is_spectator(tok) and s.god_view_allowed())):
        raise HTTPException(403, "The recap is available when the game is over (or to spectators of AI-only games).")
    return _replay_payload(s)


def _replay_payload(s: GameSession) -> dict:
    """Build the replay, under the session lock so it cannot catch a half-applied turn."""
    with s.lock:
        g = s.game
        return {
            "id": s.id, "name": s.name, "width": g.s.width, "height": g.s.height,
            "wrap_x": g.grid.wrap_x, "wrap_y": g.grid.wrap_y,
            "terrain": [[t.terrain, 1 if t.hills else 0, int(t.river or 0), t.resource, t.wonder] for t in g.s.tiles],
            "improvement_ids": list(g.rules.improvements), "feature_ids": list(g.rules.terrains),
            "players": [{"id": p.id, "name": p.name, "leader": p.leader, "color": p.color, "kind": p.kind, "alive": p.alive,
                         "eliminated_turn": p.eliminated_turn, "seat": s.seats[p.id].public() if p.id < len(s.seats) else None}
                        for p in g.s.players],
            "frames": g.frames, "stats": g.s.stats, "events": g.s.events, "messages": g.s.messages,
            "thoughts": g.s.thoughts, "negotiations": g.s.negotiations, "deals": g.s.deals,
            "winner": g.s.winner, "victory": g.s.victory, "phase": g.s.phase, "turn": g.turn,
            "config": g.s.config,
        }


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
    """Delete a save."""
    _require_save_access(sdb, p, body.path, access.MANAGE)
    try:
        return {"deleted": manager.delete_save(body.path, body.whole_game)}
    except KeyError:
        raise HTTPException(404, "Save not found.")


@app.post("/api/saves/load")
def load_save(body: LoadBody, request: Request, p: Principal = Depends(principal),
              sdb: DbSession = Depends(get_db)):
    """Load a save into a live session.

    Games with AI seats start paused, so their model configuration can be checked before anything
    spends tokens against a stale endpoint.
    """
    _require_save_access(sdb, p, body.path, access.MANAGE)
    path = (SAVE_DIR / body.path).resolve()
    if SAVE_DIR.resolve() not in path.parents or not path.exists():
        raise HTTPException(404, "Save not found.")
    s = manager.load(path)
    # A save from before accounts existed has no owner; whoever loads it becomes one.
    row = ownership.row_for(sdb, s.id)
    if row is None:
        row = ownership.register(sdb, s, p.user, visibility="private")
    elif row.owner_id is None and p.user is not None:
        row.owner_id = p.user.id
    return s.info(include_tokens=True)


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
    """Who can see this game, and by what link."""
    s, row, perms = _gate(sdb, gid, p, access.VIEW, request)
    out = ownership.to_client(row, perms, base_url=citar_settings.get().public_origin)
    if row is not None and access.MANAGE in perms:
        out["shared_with"] = access.shared_with(sdb, row)
    return out


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
    return ownership.to_client(row, perms, base_url=citar_settings.get().public_origin)


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
    return {"shared_with": access.shared_with(sdb, row)}


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
    return {"shared_with": access.shared_with(sdb, row)}


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
        server_id, server_name = identity

        connection = WorkerConnection(websocket, server_id, hello, asyncio.get_running_loop())
        previous = hub().register(connection)
        if previous is not None:
            try:
                await previous.ws.close(code=4409)
            except Exception:
                pass
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

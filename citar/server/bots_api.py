"""HTTP API for bot profiles: list, edit and fork them, read their parameters, see the rankings, and queue lab
experiments that pit profiles against each other.

A profile and a lab experiment are owned items (design/phase3-features.md P3.2.4): reading one needs VIEW on it (the
built-in profiles are the server's, `global`); creating and forking a profile need ``edit_bots`` and queueing an
experiment ``run_lab``, both an administrator's by default (profiles are what benchmarks are measured against, and
experiments use the server's CPU); changing or deleting a profile needs MANAGE on it.

The bot versions, their parameter schemas, cleaning and fingerprints come from the engine through the facade
(``engine_api.bot_versions``, ``bot_schema``, ``bot_clean_params``, ``bot_fingerprint``; DESIGN.md P2.7.3, P2.8.6),
in the shapes this page always had.
"""
from __future__ import annotations

import re
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel, Field
from sqlalchemy.orm import Session

from .. import items
from ..auth import access
from ..auth.deps import Principal, check_cap, get_db, principal, require_cap, require_user
from ..bots import profiles, ratings
from ..db.models import User

router = APIRouter(prefix="/api/bots")
EXP_NAME = re.compile(r"^[A-Za-z0-9._-]{1,80}$")


def _key(request: Request) -> Optional[str]:
    """The share key a request carries, if any."""
    return (request.query_params.get("k") or request.query_params.get("key")
            or request.headers.get("x-citar-share-key"))


def _fail(e: Exception, status: int = 400) -> HTTPException:
    """An HTTP error carrying the message of a profile error."""
    return HTTPException(status, str(e))


_REFUSED = (profiles.ProfileError,)


def _with_rating(p: dict, board: list) -> dict:
    """A profile with its rating (see ratings.profile_rating)."""
    return ratings.profile_rating(p, board)


def _builtin(pid: Optional[str]) -> bool:
    """Whether a profile id names a shipped profile (or "best", which stands for one the server picks)."""
    return pid == profiles.BEST or pid in {b["id"] for b in profiles.BUILTIN}


def _filter_board(board: list, seen: Optional[set]) -> list:
    """A leaderboard's entries without those of profiles the caller may not see (an entry with no profile is a raw
    bot seat's, everybody's); all of them for ``seen`` None (an administrator)."""
    if seen is None:
        return board
    return [b for b in board if not b.get("profile") or b["profile"] in seen]


@router.get("/profiles")
def list_profiles(p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """The profiles the caller may see with their ratings, best first, and which one "Best bot" currently stands
    for. Each says what the caller may do to it."""
    seen = items.listed(s, "bot_profile", p.user)
    board = _filter_board(ratings.rankings(history=False)["entries"], seen)
    shown = [x for x in ratings.ranked_profiles(board) if seen is None or x["id"] in seen]
    perms = items.permissions_by_ref(s, "bot_profile", p.user, [x["id"] for x in shown])
    return {"profiles": [dict(x, **perms.get(x["id"], {})) for x in shown], "default": profiles.DEFAULT_PROFILE,
            "best": ratings.best_profile(board)}


@router.get("/profiles/{pid}")
def get_profile(pid: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """One profile the caller may see, with every rating entry it has had and the experiments it played in (those
    the caller may see)."""
    _row, perms = items.require(s, "bot_profile", pid, p.user, access.VIEW, slug=_key(request))
    try:
        prof = profiles.get(pid)
    except profiles.ProfileError as e:
        raise _fail(e, 404)
    r = ratings.rankings()
    mine = [b for b in r["entries"] if b.get("profile") == pid]
    seen = items.listed(s, "lab_experiment", p.user)
    return {"profile": dict(_with_rating(prof, r["entries"]), **perms.client()), "entries": mine,
            "history": {b["id"]: r["history"].get(b["id"], []) for b in mine},
            "experiments": [e for e in _experiments_with(pid) if seen is None or e["name"] in seen]}


class ProfileBody(BaseModel):
    """A profile as the editor sends it."""
    name: str = Field(max_length=80)
    description: str = Field("", max_length=2000)
    tags: list[str] = []
    engine: str = "basic"
    aggression: Optional[float] = None
    params: dict = {}
    parent: Optional[str] = None
    archived: bool = False
    note: str = Field("", max_length=300)


def _parent(s: Session, user: User, parent: Optional[str]) -> None:
    """A profile names the profile it came from; the caller must be able to see it."""
    if parent and not _builtin(parent):
        items.require(s, "bot_profile", parent, user, access.VIEW)


@router.post("/profiles")
def create_profile(body: ProfileBody, user: User = Depends(require_cap("edit_bots")), s: Session = Depends(get_db)):
    """Save a new profile: the caller's."""
    _parent(s, user, body.parent)
    try:
        pid = profiles.new_id(body.name)
        items.register(s, "bot_profile", pid, user, body.name, path=profiles.PROFILES_DIR / f"{pid}.json")
        return profiles.save({"id": pid, **body.model_dump(exclude={"note"})}, user=user.handle,
                             note=body.note or "created")
    except _REFUSED as e:
        raise _fail(e)


@router.put("/profiles/{pid}")
def update_profile(pid: str, body: ProfileBody, request: Request, user: User = Depends(require_user),
                   s: Session = Depends(get_db)):
    """Save changes to a profile the caller manages (a new revision when what plays changes). The shipped ones
    cannot be changed, by anyone: fork one instead."""
    items.require(s, "bot_profile", pid, user, access.MANAGE, slug=_key(request))
    _parent(s, user, body.parent)
    try:
        profiles.get(pid)
        out = profiles.save({"id": pid, **body.model_dump(exclude={"note"})}, user=user.handle, note=body.note)
    except _REFUSED as e:
        raise _fail(e)
    items.rename(s, "bot_profile", pid, out["name"])
    return out


class ForkBody(BaseModel):
    """The name for a copy of a profile."""
    name: Optional[str] = Field(None, max_length=80)


@router.post("/profiles/{pid}/fork")
def fork_profile(pid: str, body: ForkBody, request: Request, user: User = Depends(require_user),
                 s: Session = Depends(get_db)):
    """Copy a profile the caller may see (built-in or saved) into a new, editable one: the caller's (``edit_bots``,
    asked after the profile, so a stranger learns nothing of a private one from the refusal)."""
    items.require(s, "bot_profile", pid, user, access.VIEW, slug=_key(request))
    check_cap(user, "edit_bots")
    try:
        src = profiles.get(pid)
        name = body.name or f"{src['name']} (copy)"
        new = profiles.new_id(name)
        items.register(s, "bot_profile", new, user, name, path=profiles.PROFILES_DIR / f"{new}.json")
        return profiles.save({"id": new, "name": name, "description": src.get("description", ""),
                              "tags": [t for t in src.get("tags", []) if t not in ("yardstick", "snapshot", "control")],
                              "engine": src["engine"], "aggression": src.get("aggression"),
                              "params": src.get("params") or {}, "parent": pid},
                             user=user.handle, note=f"forked from {src['name']}")
    except _REFUSED as e:
        raise _fail(e)


@router.delete("/profiles/{pid}")
def delete_profile(pid: str, request: Request, user: User = Depends(require_user), s: Session = Depends(get_db)):
    """Delete a saved profile the caller manages. Its games stay in the rankings, under its fingerprint."""
    items.require(s, "bot_profile", pid, user, access.MANAGE, slug=_key(request))
    try:
        profiles.delete(pid)
    except profiles.ProfileError as e:
        raise _fail(e)
    items.delete(s, "bot_profile", pid, actor=user)
    return {"deleted": pid}


@router.get("/engines", dependencies=[Depends(require_user)])
def list_engines():
    """The bot code a profile can run: ``basic`` (the latest version), each bot version compiled into the engine
    (``engine_api.bot_versions``), the idle bot among them."""
    return {"engines": profiles.engines()}


@router.get("/schema", dependencies=[Depends(require_user)])
def param_schema(engine: str = "basic"):
    """A version's parameters, {engine, groups}: groups, labels, help, defaults and ranges (``engine_api.bot_schema``;
    ``basic`` is the latest version's). A frozen snapshot of 0.1.5 is refused as archived."""
    try:
        return profiles.schema(engine)
    except _REFUSED as e:
        raise _fail(e, 404)


@router.get("/rankings")
def get_rankings(factorial: bool = False, user: User = Depends(require_user), s: Session = Depends(get_db)):
    """The leaderboard, rating history and head-to-head records, of the profiles the caller may see (a private
    profile's parameters and results are its owner's)."""
    r = ratings.rankings(include_factorial=factorial)
    seen = items.listed(s, "bot_profile", user)
    if seen is None:
        return r
    board = _filter_board(r["entries"], seen)
    keep = {b["id"] for b in board}
    return dict(r, entries=board, history={k: v for k, v in r["history"].items() if k in keep},
                head_to_head={k: v for k, v in r["head_to_head"].items()
                              if all(part in keep for part in k.split("|"))})


class ExperimentBody(BaseModel):
    """A lab experiment between profiles, as the Bots page's A/B form sends it."""
    profiles: list[str] = Field(min_length=1, max_length=8)
    name: Optional[str] = Field(None, max_length=80)
    note: str = Field("", max_length=300)
    players: int = Field(4, ge=2, le=8)
    games: int = Field(12, ge=1, le=500)
    seed: int = Field(5000, ge=0)
    size: str = "small"
    maps: list[str] = ["continents", "pangaea", "fractal"]
    speed: str = "Quick"
    turns: int = Field(0, ge=0, le=1000)
    difficulty: str = "Prince"
    priority: int = Field(0, ge=-100, le=100)


@router.post("/experiments")
def queue_experiment(body: ExperimentBody, user: User = Depends(require_cap("run_lab")),
                     s: Session = Depends(get_db)):
    """Queue a lab experiment (``run_lab``: it uses the server's own CPU), the caller's: the chosen profiles, which
    the caller must be able to see, fill the seats in turn (A, B, A, B...) and rotate through the start positions, so
    each plays every position on the same maps and seeds."""
    from .. import engine_api, lab
    sizes, map_types = engine_api.map_sizes(), engine_api.map_types()
    speeds, difficulties = engine_api.speeds(), engine_api.difficulties()
    if body.size not in sizes:
        raise HTTPException(400, f"Map size is one of {', '.join(sizes)}.")
    if not body.maps or any(m not in map_types for m in body.maps):
        raise HTTPException(400, f"Maps are from {', '.join(map_types)}.")
    if body.speed not in speeds:
        raise HTTPException(400, f"Speed is one of {', '.join(speeds)}.")
    if body.difficulty not in difficulties:
        raise HTTPException(400, f"Difficulty is one of {', '.join(difficulties)}.")
    for pid in body.profiles:
        if not _builtin(pid):
            items.require(s, "bot_profile", pid, user, access.VIEW)
    try:
        resolved = [profiles.get(pid) for pid in body.profiles]
    except profiles.ProfileError as e:
        raise _fail(e)
    names = [p["name"] for p in resolved]
    labels = [n if names.count(n) == 1 else f"{n} [{p['id']}]" for n, p in zip(names, resolved)]
    seats = [{"profile": resolved[i % len(resolved)]["id"], "label": labels[i % len(resolved)]}
             for i in range(body.players)]
    name = body.name or "ab-" + "-vs-".join(p["id"] for p in resolved)[:60]
    if not EXP_NAME.match(name):
        raise HTTPException(400, "An experiment name is letters, digits, dots, dashes and underscores.")
    base, k = name, 2
    # an experiment's name is its id; a deleted one's is not reused either, so its results stay its own
    while ((lab.QUEUE / f"{name}.json").exists() or (lab.DONE / f"{name}.json").exists()
           or items.get(s, "lab_experiment", name, deleted=True) is not None):
        name, k = f"{base}-{k}", k + 1
    spec = {"name": name, "note": body.note or f"A/B from the Bots page: {' vs '.join(names)}",
            "priority": body.priority, "games": body.games, "seed": body.seed, "size": body.size,
            "maps": body.maps or ["continents"], "speed": body.speed, "turns": body.turns,
            "difficulty": body.difficulty, "seats": seats, "rotate": True, "submitted_by": user.handle,
            "owner_id": user.id}
    items.register(s, "lab_experiment", name, user, name, path=lab.QUEUE / f"{name}.json")
    try:
        return lab.submit(spec)
    except (ValueError, *_REFUSED) as e:
        raise _fail(e)


def _experiments_with(pid: str) -> list[dict]:
    """Lab experiments with a seat playing this profile, newest first."""
    import json
    from .. import lab
    out = []
    for d, state in ((lab.QUEUE, "queued"), (lab.DONE, "complete")):
        if not d.exists():
            continue
        for p in d.glob("*.json"):
            try:
                spec = json.loads(p.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                continue
            if any(s.get("profile") == pid for s in spec.get("seats", [])) or spec.get("profile") == pid:
                out.append({"name": spec["name"], "state": state, "games": spec.get("games"),
                            "done": len({r.get("i") for r in lab.load_results(spec["name"])}),
                            "submitted": spec.get("submitted"), "note": spec.get("note"),
                            "seats": [s.get("label") for s in spec.get("seats", [])]})
    out.sort(key=lambda e: e.get("submitted") or "", reverse=True)
    return out

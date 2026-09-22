"""HTTP API for bot profiles: list, edit and fork them, read their parameters, see the rankings, and queue lab
experiments that pit profiles against each other.

Reading is open to any signed-in account. Changing profiles and queueing experiments is for administrators: profiles
are shared by everyone on the server (benchmarks are measured against them), and experiments use its CPU.
"""
from __future__ import annotations

import re
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel, Field

from ..auth.deps import require_role, require_user
from ..bots import profiles, ratings
from ..db.models import User

router = APIRouter(prefix="/api/bots")
EXP_NAME = re.compile(r"^[A-Za-z0-9._-]{1,80}$")


def _fail(e: Exception, status: int = 400) -> HTTPException:
    """An HTTP error carrying the message of a profile error."""
    return HTTPException(status, str(e))


def _with_rating(p: dict, board: list) -> dict:
    """A profile with its rating (see ratings.profile_rating)."""
    return ratings.profile_rating(p, board)


@router.get("/profiles", dependencies=[Depends(require_user)])
def list_profiles():
    """Every profile with its rating, best first, and which one "Best bot" currently stands for."""
    board = ratings.rankings(history=False)["entries"]
    return {"profiles": ratings.ranked_profiles(board), "default": profiles.DEFAULT_PROFILE,
            "best": ratings.best_profile(board)}


@router.get("/profiles/{pid}", dependencies=[Depends(require_user)])
def get_profile(pid: str):
    """One profile, with every rating entry it has had and the experiments it played in."""
    try:
        p = profiles.get(pid)
    except profiles.ProfileError as e:
        raise _fail(e, 404)
    r = ratings.rankings()
    mine = [b for b in r["entries"] if b.get("profile") == pid]
    return {"profile": _with_rating(p, r["entries"]), "entries": mine,
            "history": {b["id"]: r["history"].get(b["id"], []) for b in mine},
            "experiments": _experiments_with(pid)}


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


@router.post("/profiles")
def create_profile(body: ProfileBody, user: User = Depends(require_role("admin"))):
    """Save a new profile."""
    try:
        return profiles.save(body.model_dump(exclude={"note"}), user=user.handle, note=body.note or "created")
    except profiles.ProfileError as e:
        raise _fail(e)


@router.put("/profiles/{pid}")
def update_profile(pid: str, body: ProfileBody, user: User = Depends(require_role("admin"))):
    """Save changes to a profile (a new revision when what plays changes)."""
    try:
        profiles.get(pid)
        return profiles.save({"id": pid, **body.model_dump(exclude={"note"})}, user=user.handle, note=body.note)
    except profiles.ProfileError as e:
        raise _fail(e)


class ForkBody(BaseModel):
    """The name for a copy of a profile."""
    name: Optional[str] = Field(None, max_length=80)


@router.post("/profiles/{pid}/fork")
def fork_profile(pid: str, body: ForkBody, user: User = Depends(require_role("admin"))):
    """Copy a profile (built-in or saved) into a new, editable one."""
    try:
        return profiles.fork(pid, body.name, user=user.handle)
    except profiles.ProfileError as e:
        raise _fail(e)


@router.delete("/profiles/{pid}", dependencies=[Depends(require_role("admin"))])
def delete_profile(pid: str):
    """Delete a saved profile. Its games stay in the rankings, under its fingerprint."""
    try:
        profiles.delete(pid)
    except profiles.ProfileError as e:
        raise _fail(e)
    return {"deleted": pid}


@router.get("/engines", dependencies=[Depends(require_user)])
def list_engines():
    """The bot code a profile can run: the live bot, frozen snapshots, the idle bot."""
    return {"engines": profiles.engines()}


@router.get("/schema", dependencies=[Depends(require_user)])
def param_schema(engine: str = "basic"):
    """An engine's parameters: groups, labels, help, defaults and ranges."""
    try:
        return profiles.schema(engine)
    except profiles.ProfileError as e:
        raise _fail(e, 404)


@router.get("/rankings", dependencies=[Depends(require_user)])
def get_rankings(factorial: bool = False):
    """The leaderboard, rating history and head-to-head records."""
    return ratings.rankings(include_factorial=factorial)


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
def queue_experiment(body: ExperimentBody, user: User = Depends(require_role("admin"))):
    """Queue a lab experiment: the chosen profiles fill the seats in turn (A, B, A, B...) and rotate through the start
    positions, so each plays every position on the same maps and seeds."""
    from .. import lab
    from ..engine.mapgen import MAP_TYPES
    from ..engine.rules import get_rules
    R = get_rules()
    if body.size not in R.const["map_sizes"]:
        raise HTTPException(400, f"Map size is one of {', '.join(R.const['map_sizes'])}.")
    if not body.maps or any(m not in MAP_TYPES for m in body.maps):
        raise HTTPException(400, f"Maps are from {', '.join(MAP_TYPES)}.")
    if body.speed not in R.speeds:
        raise HTTPException(400, f"Speed is one of {', '.join(R.speeds)}.")
    if body.difficulty not in R.difficulty_list:
        raise HTTPException(400, f"Difficulty is one of {', '.join(R.difficulty_list)}.")
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
    while (lab.QUEUE / f"{name}.json").exists() or (lab.DONE / f"{name}.json").exists():
        name, k = f"{base}-{k}", k + 1
    spec = {"name": name, "note": body.note or f"A/B from the Bots page: {' vs '.join(names)}",
            "priority": body.priority, "games": body.games, "seed": body.seed, "size": body.size,
            "maps": body.maps or ["continents"], "speed": body.speed, "turns": body.turns,
            "difficulty": body.difficulty, "seats": seats, "rotate": True, "submitted_by": user.handle}
    try:
        return lab.submit(spec)
    except (ValueError, profiles.ProfileError) as e:
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

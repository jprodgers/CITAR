"""What rule scripts read of a game: small documented shapes, the same from both engines, with every set sorted.

The Rust engine's side is ``crates/citar-engine/src/api/inspect.rs``, and ``tests/rules/README.md`` documents each
shape. Reached through ``EngineGame.inspect`` (not in the facade's ``__all__``: tests only). A query only reads.
"""
from __future__ import annotations

from typing import Any

from .game import ActionError, Game
from .state import AUTO_DECISIONS

QUERIES = ("city", "events", "find_tiles", "game", "ops", "pending", "player", "relation", "tile", "unit", "units")


def inspect(g: Game, q: dict) -> Any:
    """Answer one query ``{"what": ..., ...}``."""
    from .scenario import _pid
    q = q if isinstance(q, dict) else {}
    what = q.get("what")
    if what == "game":
        return _game(g)
    if what == "player":
        return _player(g, _any_pid(g, q.get("player")))
    if what == "tile":
        from .scenario import _idx
        return _tile(g, _idx(g, q))
    if what == "relation":
        a, b = _pid(g, q.get("a"), majors_only=False), _pid(g, q.get("b"), majors_only=False)
        if a == b:
            raise ActionError("A relation needs two different players.")
        return _relation(g, a, b)
    if what == "unit":
        u = g.unit(_whole(q.get("unit")))
        if u is None:
            raise ActionError("No such unit.")
        return _unit(g, u)
    if what == "units":
        return _units(g, q)
    if what == "city":
        c = g.city(_whole(q.get("city")))
        if c is None:
            raise ActionError("No such city.")
        return _city(g, c)
    if what == "events":
        return _events(g, q)
    if what == "find_tiles":
        return find_tiles(g, q)
    if what == "ops":
        from . import scenario, testops
        return {"scenario": scenario.ops_help(), "test": testops.ops_help()}
    if what == "pending":
        return []
    raise ActionError(f"Unknown inspect query {what!r}. Known: {', '.join(QUERIES)}.")


def _whole(v) -> int:
    """An id as a whole number, or -1 for anything that is none."""
    try:
        return int(v)
    except (TypeError, ValueError):
        return -1


def _any_pid(g: Game, v) -> int:
    """A player by id, the barbarians included (which scenario operations may not name)."""
    try:
        pid = int(v)
    except (TypeError, ValueError):
        raise ActionError(f"'{v}' is not a player id.")
    if not 0 <= pid < len(g.s.players):
        raise ActionError(f"No player {pid}.")
    return pid


def _game(g: Game) -> dict:
    """``game``: where the game is, and its players by kind."""
    s = g.s
    barbarians = [p.id for p in s.players if p.kind == "barbarian"]
    return {"turn": s.turn, "current": s.current, "phase": s.phase, "winner": s.winner, "victory": s.victory,
            "width": s.width, "height": s.height, "players": len(s.players),
            "majors": [p.id for p in s.players if p.kind == "major"],
            "city_states": [p.id for p in s.players if p.kind == "city_state"],
            "barbarians": barbarians[0] if barbarians else None}


def _player(g: Game, pid: int) -> dict:
    """``player``: one civilization, city-state or the barbarians."""
    p = g.player(pid)
    overrides: dict = {}
    if p.overrides.get("handicap"):
        overrides["handicap"] = p.overrides["handicap"]
    auto_set = p.overrides.get("auto") or {}
    if auto_set:
        overrides["auto"] = {k: bool(auto_set[k]) for k in AUTO_DECISIONS if k in auto_set}
    difficulty = p.difficulty or (g.s.config.get("difficulty") if p.kind == "major" else None)
    city_state = None
    if p.kind == "city_state":
        city_state = {"type": p.cs_type, "ally": p.ally,
                      "influence": {str(m.id): float(p.influence.get(str(m.id), 0.0))
                                    for m in g.majors(alive_only=False)}}
    return {
        "id": p.id, "kind": p.kind, "name": p.name, "leader": p.leader, "nation": p.nation, "alive": bool(p.alive),
        "controller": p.controller, "handicap": p.handicap,
        "auto": {k: bool(p.auto.get(k)) for k in AUTO_DECISIONS},
        "overrides": overrides, "difficulty": difficulty,
        "gold": float(p.gold), "culture": float(p.culture), "faith": float(p.faith),
        "golden_age_turns": int(p.golden_age_turns), "free_policies": int(p.free_policies),
        "free_techs": int(p.free_techs), "future_techs": int(p.future_techs),
        "techs": sorted(p.techs), "research": {"queue": list(p.research_queue), "goal": p.research_goal},
        "policies": sorted(p.policies), "met": sorted(q for q in p.met if q != pid), "capital": p.capital,
        "cities": sorted(c.id for c in g.player_cities(pid)), "units": sorted(u.id for u in g.player_units(pid)),
        "explored": sum(1 for b in p.explored if b), "notes": p.notes if p.kind == "major" else "",
        "city_state": city_state,
    }


def _tile(g: Game, idx: int) -> dict:
    """``tile``: one tile; a deposit counts only with its resource, as the Rust engine keeps it."""
    t = g.s.tiles[idx]
    x, y = g.grid.xy(idx)
    return {"x": x, "y": y, "terrain": t.terrain, "features": sorted(t.features), "wonder": t.wonder,
            "resource": t.resource, "resource_amount": int(t.resource_amount or 0) if t.resource else 0,
            "improvement": t.improvement, "pillaged": bool(t.pillaged), "route": t.route,
            "route_pillaged": bool(t.route_pillaged), "river": int(t.river or 0), "owner": t.owner, "city": t.city,
            "units": sorted(u.id for u in g.units_at(idx))}


def _opinion(rel: dict, holder: int, about: int) -> float:
    """What ``holder`` thinks of ``about``: its own entry, and a scenario's under "holder>about" (which Python's
    ``diplomacy.opinion`` never read; the Rust engine keeps the two together)."""
    op = rel.get("opinion") or {}
    return float(sum((op.get(str(holder)) or {}).values()) + sum((op.get(f"{holder}>{about}") or {}).values()))


def _relation(g: Game, a: int, b: int) -> dict:
    """``relation``: two players' relation, the two-sided terms as ``[a's, b's]``."""
    from .diplomacy import has_embassy, has_pact, is_friends, new_relation
    rel = g.relation(a, b) or new_relation()
    ob = g.s.open_borders
    return {"a": a, "b": b, "met": g.has_met(a, b), "war": bool(rel.get("war")),
            "war_declared_by": rel.get("war_declared_by"), "since": int(rel.get("since") or 0),
            "treaty_until": int(rel.get("treaty_until") or 0), "friendship_until": int(rel.get("friendship_until") or 0),
            "pact_until": int(rel.get("pact_until") or 0), "ra_until": int(rel.get("ra_until") or 0),
            "embassy": [has_embassy(g, a, b), has_embassy(g, b, a)],
            "open_borders_until": [int(ob.get(f"{a}>{b}") or 0), int(ob.get(f"{b}>{a}") or 0)],
            "opinion": [_opinion(rel, a, b), _opinion(rel, b, a)],
            "friends": is_friends(g, a, b), "pact": has_pact(g, a, b)}


def _unit(g: Game, u) -> dict:
    """``unit``: one unit."""
    x, y = g.grid.xy(u.idx)
    return {"id": u.id, "owner": u.owner, "type": u.type, "x": x, "y": y, "hp": u.hp, "xp": u.xp,
            "promotions": sorted(u.promotions)}


def _units(g: Game, q: dict) -> list:
    """``units``: every unit, or a player's, or those on a tile, by id."""
    owner = None
    if q.get("player") is not None:
        owner = _any_pid(g, q["player"])
    at = None
    if "x" in q or "y" in q:
        from .scenario import _idx
        at = _idx(g, q)
    return [_unit(g, u) for u in sorted(g.s.units.values(), key=lambda u: u.id)
            if (owner is None or u.owner == owner) and (at is None or u.idx == at)]


def _city(g: Game, c) -> dict:
    """``city``: one city."""
    x, y = g.grid.xy(c.idx)
    return {"id": c.id, "name": c.name, "owner": c.owner, "x": x, "y": y, "pop": c.pop,
            "buildings": sorted(c.buildings)}


def _events(g: Game, q: dict) -> list:
    """``events``: after id ``since``, of a ``type``, and only what ``player`` hears of, each if given."""
    from .scenario import _pid
    since = q.get("since") or 0
    kind = q.get("type")
    hearer = _pid(g, q["player"], majors_only=False) if q.get("player") is not None else None
    out = []
    for e in g.s.events:
        if e["id"] <= since or (kind is not None and e["type"] != kind):
            continue
        if hearer is not None and e.get("players") is not None and hearer not in e["players"]:
            continue
        out.append({"id": e["id"], "turn": e["turn"], "type": e["type"], "text": e["text"],
                    "audience": sorted(e["players"]) if e.get("players") is not None else None})
    return out


FIND_FILTERS = ("x", "y", "radius", "terrain", "feature", "resource", "improvement", "owner", "land", "river", "city",
                "units", "bare", "limit")


def find_tiles(g: Game, q: dict) -> list:
    """The tiles that pass every filter given, sorted by distance from ``x``, ``y`` (0 for all without them), then
    row, then column; the first ``limit``, if given. See tests/rules/README.md for the filters."""
    from .scenario import _idx, _name
    for k in q:
        if k != "what" and k not in FIND_FILTERS:
            raise ActionError(f"find_tiles has no filter '{k}'. Filters: {', '.join(FIND_FILTERS)}.")
    R = g.rules
    origin = _idx(g, q) if ("x" in q or "y" in q) else None
    radius = q.get("radius")
    limit = q.get("limit")
    terrain = _name(g, "terrain", q["terrain"]) if "terrain" in q else None
    feature = _name(g, "terrain", q["feature"]) if "feature" in q else None
    if feature is not None and R.terrains[feature]["type"] != "TerrainFeature":
        raise ActionError(f"{feature} is not a terrain feature.")
    resource = _name(g, "resource", q["resource"]) if "resource" in q else None
    improvement = _name(g, "improvement", q["improvement"]) if "improvement" in q else None
    owner, want_owner = None, False
    if q.get("owner") is not None:
        want_owner = True
        owner = None if q["owner"] == "none" else _any_pid(g, q["owner"])

    def flag(k):
        """A true-or-false filter, or None when it is not given."""
        return None if q.get(k) is None else bool(q[k])

    land, river, city, units, bare = flag("land"), flag("river"), flag("city"), flag("units"), flag("bare")
    cities = {c.idx for c in g.s.cities.values()}
    found = []
    for i, t in enumerate(g.s.tiles):
        d = g.grid.distance(origin, i) if origin is not None else 0
        is_bare = not t.features and not t.resource and not t.improvement and not t.wonder
        keep = ((radius is None or d <= int(radius))
                and (terrain is None or t.terrain == terrain)
                and (feature is None or feature in t.features)
                and (resource is None or t.resource == resource)
                and (improvement is None or t.improvement == improvement)
                and (not want_owner or t.owner == owner)
                and (land is None or (R.terrains[t.terrain]["type"] == "Land") == land)
                and (river is None or bool(t.river) == river)
                and (city is None or (i in cities) == city)
                and (units is None or bool(g.units_at(i)) == units)
                and (bare is None or is_bare == bare))
        if keep:
            x, y = g.grid.xy(i)
            found.append((d, y, x))
    found.sort()
    if limit is not None:
        found = found[:max(0, int(limit))]
    return [{"x": x, "y": y, "distance": d} for d, y, x in found]

"""What rule scripts read of a game: small documented shapes, the same from both engines, with every set sorted.

The Rust engine's side is ``crates/citar-engine/src/api/inspect.rs``, and ``tests/rules/README.md`` documents each
shape. Reached through ``EngineGame.inspect`` (not in the facade's ``__all__``: tests only). A query only reads.
"""
from __future__ import annotations

from typing import Any

from .game import ActionError, Game
from .state import AUTO_DECISIONS

QUERIES = ("build_options", "buildable", "camps", "city", "city_state", "costs", "events", "find_tiles", "game",
           "great_people", "negotiation", "ops", "pending", "player", "preview", "relation", "religion", "spies", "tile",
           "un", "unit", "unit_actions", "units", "victory")


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
    if what == "preview":
        from . import combat
        from .scenario import _idx
        u = g.unit(_whole(q.get("unit")))
        if u is None:
            raise ActionError("No such unit.")
        idx = _idx(g, q)
        try:
            return combat.preview(g, u, idx)
        except ActionError as e:
            return {"error": str(e)}
    if what in ("unit_actions", "build_options"):
        u = g.unit(_whole(q.get("unit")))
        if u is None:
            raise ActionError("No such unit.")
        return _unit_actions(g, u) if what == "unit_actions" else _build_options(g, u)
    if what == "city":
        c = g.city(_whole(q.get("city")))
        if c is None:
            raise ActionError("No such city.")
        return _city(g, c)
    if what == "buildable":
        c = g.city(_whole(q.get("city")))
        if c is None:
            raise ActionError("No such city.")
        return _buildable(g, c)
    if what == "costs":
        return _costs(g, _pid(g, q.get("player"), majors_only=False))
    if what == "religion":
        if q.get("city") is not None:
            c = g.city(_whole(q.get("city")))
            if c is None:
                raise ActionError("No such city.")
            return _city_religion(g, c)
        return _civ_religion(g, _pid(g, q.get("player"), majors_only=False))
    if what == "great_people":
        return _great_people(g, _pid(g, q.get("player"), majors_only=False))
    if what == "negotiation":
        from . import diplomacy
        n = diplomacy.get_negotiation(g, _whole(q.get("negotiation")))
        if q.get("player") is not None:
            return diplomacy.negotiation_view(g, n, _any_pid(g, q.get("player")))
        return _negotiation(n)
    if what == "spies":
        return [{"name": s["name"], "rank": int(s["rank"]), "city": s.get("city"), "action": s["action"],
                 "turns": int(s.get("turns") or 0), "progress": int(s.get("progress") or 0)}
                for s in g.player(_any_pid(g, q.get("player"))).spies]
    if what == "camps":
        return [{"id": cid, "x": g.grid.xy(c["idx"])[0], "y": g.grid.xy(c["idx"])[1],
                 "countdown": int(c["countdown"]), "spawned": int(c["spawned"]), "destroyed": bool(c.get("destroyed"))}
                for cid, c in sorted(g.s.camps.items())]
    if what == "city_state":
        return _city_state(g, _any_pid(g, q.get("player")))
    if what == "victory":
        return _victory(g, _any_pid(g, q.get("player")))
    if what == "un":
        return _un(g)
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


def _victory(g: Game, pid: int) -> dict:
    """``victory``: a civilization's score, military strength, progress toward each victory, spaceship, and the
    victory it would win now."""
    from . import victory as V
    return {"score": V.score(g, pid), "military_strength": V.military_strength(g, pid),
            "progress": V.victory_progress(g, pid), "spaceship": V.spaceship_status(g, pid),
            "achieved": V.victory_achieved(g, pid)}


def _un(g: Game) -> dict:
    """``un``: the United Nations' vote, read without creating its state as ``victory._un`` does. The last result's
    tally is by player id, most votes first and equals by id, as the Rust engine keeps it."""
    from . import victory as V
    un = g.s.un or {}
    nv = un.get("next_vote")
    res = un.get("results")
    if res:
        ids = {p.name: p.id for p in g.s.players}
        tally = sorted(([ids.get(n), v] for n, v in res["tally"].items()), key=lambda x: (-x[1], x[0]))
        res = {"turn": res["turn"], "tally": tally, "votes_needed": res["votes_needed"], "winner": res["winner"]}
    votes = un.get("votes") or {}
    return {"next_vote": nv, "votes": {str(k): votes[k] for k in sorted(votes, key=int)}, "results": res,
            "won": sorted(un.get("won") or []), "processed_turn": un.get("processed_turn"),
            "open": nv is not None and g.turn >= nv - 1 and un.get("processed_turn") != g.turn,
            "votes_needed": V.votes_needed(g), "owner": V.un_owner(g)}


def _city_state(g: Game, cs: int) -> dict:
    """``city_state``: a city-state's standing with the majors it has met, its protectors and its quests."""
    from . import city_states as CS
    p = g.player(cs)
    if p.kind != "city_state":
        raise ActionError(f"Player {cs} is not a city-state.")
    met = [m.id for m in g.majors(alive_only=False) if g.has_met(cs, m.id)]
    return {"ally": p.ally, "protectors": sorted(p.protectors),
            "influence": {str(m.id): float(p.influence.get(str(m.id), 0.0)) for m in g.majors(alive_only=False)},
            "relationship": {str(m): CS.relationship(g, cs, m) for m in met},
            "resting_point": {str(m): float(CS.resting_point(g, cs, m)) for m in met},
            "quests": [{"name": x["name"], "assignee": x["assignee"], "scope": x["kind"]} for x in p.quests],
            "war_quests": {str(k): int(v["needed"]) for k, v in sorted(p.flags.get("war_quests", {}).items(),
                                                                         key=lambda kv: int(kv[0]))},
            "recently_bullied": int(p.flags.get("recently_bullied", 0))}


def _negotiation(n: dict) -> dict:
    """``negotiation``: one negotiation as it is kept, every entry with its ``note`` (null when it has none)."""
    return {"id": n["id"], "initiator": n["initiator"], "responder": n["responder"], "turn": n["turn"],
            "status": n["status"], "awaiting": n["awaiting"], "proposal": n["proposal"],
            "proposal_by": n["proposal_by"],
            "history": [{"seq": h["seq"], "by": h["by"], "action": h["action"], "message": h["message"],
                         "proposal": h["proposal"], "turn": h["turn"], "note": h.get("note")} for h in n["history"]],
            "deal_id": n.get("deal_id")}


def _civ_religion(g: Game, pid: int) -> dict:
    """``religion`` with ``player``: its pantheon or religion, its beliefs and what the next pantheon and prophet
    cost it."""
    from . import religion
    p = g.player(pid)
    hc = religion.holy_city(g, p.religion) if p.religion else None
    return {"state": p.religion_state, "religion": p.religion, "display": religion.display_name(g, p.religion),
            "beliefs": sorted(religion.all_beliefs(g, p.religion)) if p.religion else [],
            "free_beliefs": {k: int(v) for k, v in (p.flags.get("free_beliefs") or {}).items() if v},
            "pantheon_cost": religion.faith_for_pantheon(g, pid), "prophet_cost": religion.faith_for_next_prophet(g, pid),
            "prophets_earned": religion.prophets_earned(g, pid), "holy_city": hc.id if hc is not None else None}


def _city_religion(g: Game, c) -> dict:
    """``religion`` with ``city``: its majority, followers and pressures by religion, and whose holy city it is."""
    from . import religion
    pressures = dict(c.pressures) or {religion.NONE: 100}
    return {"majority": religion.majority_religion(g, c), "followers": dict(religion.followers(g, c)),
            "pressures": {k: int(v) for k, v in pressures.items()}, "holy_city_of": c.holy_city_of}


def _great_people(g: Game, pid: int) -> dict:
    """``great_people``: great person points, free great people, golden ages and the uniques held for some turns."""
    from . import great_people
    p = g.player(pid)
    return {"points": {k: float(v) for k, v in sorted(p.gp_points.items())}, "free": int(p.free_great_people),
            "earned": int(p.great_people_earned), "golden_age_points": float(p.golden_age_points),
            "golden_ages": int(p.golden_ages), "golden_age_turns": int(p.golden_age_turns),
            "golden_age_needed": great_people.happiness_for_golden_age(g, pid),
            "temp_uniques": [{"text": t["text"], "turns": int(t["turns"])} for t in p.temp_uniques]}


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
    from .economy import happiness
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
        "techs": sorted(p.techs),
        "research": {"queue": list(p.research_queue), "goal": p.research_goal,
                     "progress": {t: float(v) for t, v in sorted(p.research_progress.items())},
                     "overflow": float(p.overflow_science)},
        "policies": sorted(p.policies), "met": sorted(q for q in p.met if q != pid), "capital": p.capital,
        "cities": sorted(c.id for c in g.player_cities(pid)), "units": sorted(u.id for u in g.player_units(pid)),
        "explored": sum(1 for b in p.explored if b), "natural_wonders": sorted(p.natural_wonders),
        "notes": p.notes if p.kind == "major" else "",
        "city_state": city_state,
        # Python read happiness live where the Rust engine commits it at fixed stages of a turn:
        # the checks that see the difference are intended (happiness-seen-committed).
        "happiness": int(happiness(g, pid)["total"]), "happiness_seen": int(happiness(g, pid)["total"]),
        "gold_rate": float(p.flags.get("last_gold_rate", 0.0)),
    }


def _tile(g: Game, idx: int) -> dict:
    """``tile``: one tile; a deposit counts only with its resource, as the Rust engine keeps it."""
    t = g.s.tiles[idx]
    x, y = g.grid.xy(idx)
    return {"x": x, "y": y, "terrain": t.terrain, "features": sorted(t.features), "wonder": t.wonder,
            "resource": t.resource, "resource_amount": int(t.resource_amount or 0) if t.resource else 0,
            "improvement": t.improvement, "pillaged": bool(t.pillaged), "route": t.route,
            "route_pillaged": bool(t.route_pillaged), "river": int(t.river or 0), "owner": t.owner, "city": t.city,
            "units": sorted(u.id for u in g.units_at(idx)), "visible": _seen_by(g, idx),
            "builds": [[name, int(turns)] for name, turns in (t.build or [])]}


def _seen_by(g: Game, idx: int) -> list:
    """Who sees a tile now: every living player but the barbarians whose sight covers it."""
    from .visibility import visible_tiles
    return [p.id for p in g.s.players if p.alive and p.kind != "barbarian" and idx in visible_tiles(g, p.id)]


def _opinion(g: Game, holder: int, about: int) -> float:
    """What ``holder`` thinks of ``about``, as ``diplomacy.opinion`` reads it. A scenario's opinion is not in it: Python
    stored it under "holder>about", which nothing read, where the Rust engine counts it as the holder's own (intended:
    scenario-opinion-counts), so scripts mark the checks that see it."""
    from .diplomacy import opinion
    return float(opinion(g, holder, about))


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
            "opinion": [_opinion(g, a, b), _opinion(g, b, a)],
            "friends": is_friends(g, a, b), "pact": has_pact(g, a, b)}


def _unit(g: Game, u) -> dict:
    """``unit``: one unit."""
    from .movement import is_embarked, max_moves
    x, y = g.grid.xy(u.idx)
    return {"id": u.id, "owner": u.owner, "type": u.type, "x": x, "y": y, "hp": u.hp, "xp": u.xp,
            "promotions": sorted(u.promotions), "moves": u.moves, "max_moves": max_moves(g, u),
            "activity": u.activity, "goto": g.xy(u.goto) if u.goto is not None else None,
            "fortify": u.fortify, "embarked": is_embarked(g, u), "carried_by": u.carried_by,
            "set_up": "Set Up" in u.status, "original_owner": u.original_owner, "return_offer": u.return_offer}


def _unit_actions(g: Game, u) -> list:
    """``unit_actions``: what a unit could do now with ``unit_action``, as ``get_unit`` lists it."""
    from .actions import unit_actions
    return [dict(a) for a in unit_actions(g, u)]


def _build_options(g: Game, u) -> list:
    """``build_options``: what a unit could start building where it stands, and what it makes at once."""
    from .workers import build_options
    out = []
    for o in build_options(g, u):
        d = {"name": o["name"], "turns": o["turns"]}
        if o.get("first_removes"):
            d["first_removes"] = o["first_removes"]
        if o.get("replaces"):
            d["replaces"] = o["replaces"]
        if o.get("instant"):
            d = {"name": o["name"], "turns": 0, "instant": True}
        out.append(d)
    return out


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
    """``city``: one city, with where its citizens work and its yields."""
    from . import cities as C
    x, y = g.grid.xy(c.idx)

    def xys(tiles):
        """Tiles as sorted ``[x, y]`` pairs."""
        return [list(t) for t in sorted(g.grid.xy(i) for i in tiles)]

    total = C.city_stats(g, c)["total"]
    return {"id": c.id, "name": c.name, "owner": c.owner, "x": x, "y": y, "pop": c.pop,
            "buildings": sorted(c.buildings), "worked": xys(c.worked), "locked": xys(c.locked),
            "workable": xys(C.workable_tiles(g, c)),
            "specialists": {k: int(v) for k, v in sorted(c.specialists.items()) if v > 0},
            "focus": c.focus, "avoid_growth": bool(c.avoid_growth), "food": float(c.food),
            "yields": {k: float(total.get(k, 0.0)) for k in C.STATS},
            "queue": list(c.queue), "progress": {k: float(v) for k, v in sorted(c.progress.items())},
            "overflow": float(c.overflow), "culture": float(c.culture), "health": int(c.health),
            "max_health": int(C.max_health(g, c)), "tiles": len(C.city_tiles(g, c)), "founder": c.founder,
            "previous_owner": c.previous_owner, "original_capital": bool(c.original_capital),
            "puppet": bool(c.puppet), "razing": bool(c.razing), "resistance": int(c.resistance),
            "attacked": bool(c.attacked)}


def _buildable(g: Game, c) -> dict:
    """``buildable``: what a city can build now, by kind, each list sorted, and what each unit, building and wonder
    costs in production."""
    from . import cities as C
    items = C.buildable_items(g, c)
    out = {k: sorted(v) for k, v in items.items()}
    out["production"] = {n: C.production_cost(g, c.owner, n, c)
                         for k in ("units", "buildings", "wonders") for n in sorted(items[k])}
    return out


def _costs(g: Game, pid: int) -> dict:
    """``costs``: what each tech a civilization could research now costs it, its next policy's culture, and (a major's)
    the policies and branches it could adopt, sorted."""
    from . import policies, research
    return {"tech": {t: research.tech_cost(g, pid, t) for t in sorted(research.available_techs(g, pid))},
            "policy": policies.culture_cost(g, pid),
            "adoptable": sorted(policies.adoptable_policies(g, pid)) if g.player(pid).kind == "major" else []}


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

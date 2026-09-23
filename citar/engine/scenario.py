"""Scenarios: a prepared game state (map, empires, cities, units, diplomacy) plus default seats, saved so it can be
played from the same starting point many times — the basis of scripted LLM probes.

Scenarios are edited with *operations*: small JSON objects applied to a game. The scenario editor sends them, and
probe cases (citar/probes.py) use the same operations as their "setup". Coordinates are (x, y) like everywhere in
CITAR; players are ids (0 = first seat). Every operation is listed in OPS with its parameters.

File: saves/scenarios/<id>.citarscn (gzip JSON)
    {"format": "citar-scenario", "version": 1, "id", "name", "description",
     "seats": [{"type": "human" | "llm" | "bot" | "mcp" | "script", "label": "..."}],   # one per major civ
     "state": GameState.to_dict(), "created", "modified"}
"""
from __future__ import annotations

import copy
import gzip
import json
import re
import time
from pathlib import Path

from ..fsutil import replace as _fs_replace
from typing import Callable, Optional

from .game import Game, ActionError
from .state import GameState, seat_overrides
from .. import paths

SCENARIO_DIR = paths.saves_path("scenarios")
SEAT_TYPES = ("human", "llm", "bot", "mcp", "hybrid", "script")


# ----------------------------------------------------------------------------
# helpers
# ----------------------------------------------------------------------------
def _pid(g: Game, v, majors_only: bool = False) -> int:
    """Resolve a player reference - an id or a name - to a player id."""
    try:
        pid = int(v)
    except (TypeError, ValueError):
        raise ActionError(f"'{v}' is not a player id.")
    if not 0 <= pid < len(g.s.players) or g.player(pid).kind == "barbarian":
        raise ActionError(f"No player {pid}.")
    if majors_only and g.player(pid).kind != "major":
        raise ActionError(f"Player {pid} is not a major civilization.")
    return pid


def _players(g: Game, v, majors_only: bool = True) -> list[int]:
    """Resolve a player reference that may also be "all"."""
    if v in (None, "all", "*"):
        return [p.id for p in (g.majors() if majors_only else [q for q in g.s.players if q.kind != "barbarian"])]
    if isinstance(v, list):
        return [_pid(g, x, majors_only) for x in v]
    return [_pid(g, v, majors_only)]


def _idx(g: Game, op: dict) -> int:
    """The tile an operation refers to, from x and y."""
    try:
        x, y = int(op["x"]), int(op["y"])
    except (KeyError, TypeError, ValueError):
        raise ActionError("This operation needs integer x and y.")
    if not g.grid.in_bounds(x, y):
        raise ActionError(f"({x}, {y}) is off the map.")
    return g.grid.idx(x, y)


def _city(g: Game, op: dict):
    """The city an operation refers to, by id or by position."""
    if op.get("city") is not None:
        c = g.city(int(op["city"]))
    else:
        c = next((c for c in g.s.cities.values() if c.idx == _idx(g, op)), None)
    if c is None:
        raise ActionError("No such city.")
    return c


def _name(g: Game, kind: str, v: str) -> str:
    """Resolve a ruleset name loosely, so an operation can say "warrior" for "Warrior"."""
    n = g.rules.resolve(kind, v)
    if n is None:
        raise ActionError(f"Unknown {kind} '{v}'.")
    return n


# ----------------------------------------------------------------------------
# operations
# ----------------------------------------------------------------------------
OPS: dict[str, tuple[Callable, str]] = {}


def op(name: str, doc: str):
    """Register a scenario edit operation.

    Each operation is a small, declared change - grant a technology, found a city, set a relationship -
    with its parameters documented in the decorator. That documentation is what the Operations tab
    shows, so it is the reference rather than a comment about one.
    """
    def deco(fn):
        """Record the operation and its documentation."""
        OPS[name] = (fn, doc)
        return fn
    return deco


@op("grant_era", "player (id or 'all'), era. Grants every tech of earlier eras, so the civ is in that era (like "
                 "UnCiv's starting era). include=true also grants that era's own techs (which moves it to the next era).")
def _grant_era(g: Game, o: dict):
    """Advance a civilization to an era by granting every technology before it."""
    from . import research
    era = _name(g, "era", o.get("era"))
    num = g.rules.eras[era]["number"]
    include = o.get("include", False)
    out = {}
    for pid in _players(g, o.get("player"), majors_only=False):
        added = []
        for t in g.rules.tech_order:
            td = g.rules.techs[t]
            if (td["_era"] < num or (include and td["_era"] == num)) and not g.has_tech(pid, t) \
                    and not research.is_repeatable(g, t):
                research.add_tech(g, pid, t, source="scenario")
                added.append(t)
        out[pid] = len(added)
    return {"techs_added": out}


@op("grant_tech", "player (id or 'all'), tech (or techs: [...]). Also grants missing prerequisites.")
def _grant_tech(g: Game, o: dict):
    """Grant technologies, including any missing prerequisites."""
    from . import research
    names = o.get("techs") or [o.get("tech")]
    out = {}
    for pid in _players(g, o.get("player"), majors_only=False):
        added = []

        # pid and added are bound as defaults rather than captured: the recursion means this
        # helper outlives the statement that defines it, and a closure over the loop variable
        # would silently grant the last player's techs if this were ever made lazy.
        def need(t, pid=pid, added=added):
            """Grant one technology after everything it depends on."""
            if g.has_tech(pid, t) or t in added:
                return
            for pre in g.rules.techs[t].get("prerequisites", []):
                need(pre)
            research.add_tech(g, pid, t, source="scenario")
            added.append(t)
        for n in names:
            need(_name(g, "tech", n))
        out[pid] = added
    return {"techs_added": out}


@op("remove_tech", "player, tech. Removes a tech (and every tech that needs it).")
def _remove_tech(g: Game, o: dict):
    """Remove a technology, and everything that depended on it."""
    tech = _name(g, "tech", o.get("tech"))
    out = {}
    for pid in _players(g, o.get("player"), majors_only=False):
        p = g.player(pid)
        drop = {tech}
        changed = True
        while changed:
            changed = False
            for t in p.techs:
                if t not in drop and any(pre in drop for pre in g.rules.techs[t].get("prerequisites", [])):
                    drop.add(t)
                    changed = True
        p.techs = [t for t in p.techs if t not in drop]
        out[pid] = sorted(drop & set(g.rules.techs))
    g.invalidate()
    g.clear_static()
    return {"techs_removed": out}


@op("set_player", "player (id or 'all'); any of gold, faith, culture, golden_age_turns, name, difficulty, "
                  "notes, free_policies, free_techs")
def _set_player(g: Game, o: dict):
    """Set a civilization's gold, faith, culture, name, difficulty and similar."""
    for pid in _players(g, o.get("player"), majors_only=False):
        p = g.player(pid)
        for k in ("gold", "faith", "culture"):
            if o.get(k) is not None:
                setattr(p, k, float(o[k]))
        for k in ("golden_age_turns", "free_policies", "free_techs"):
            if o.get(k) is not None:
                setattr(p, k, int(o[k]))
        if o.get("name"):
            p.name = str(o["name"])[:40]
        if o.get("difficulty"):
            p.difficulty = _name(g, "difficulty", o["difficulty"])
        if o.get("notes") is not None:
            p.notes = str(o["notes"])
    g.invalidate()
    return {}


@op("adopt_policy", "player, policy (or policies: [...]); branches open automatically")
def _adopt_policy(g: Game, o: dict):
    """Adopt policies, opening their branches as needed."""
    from . import policies
    names = o.get("policies") or [o.get("policy")]
    out = {}
    for pid in _players(g, o.get("player")):
        done = []
        for n in names:
            d = policies.policy_def(g, n)
            if d is None:
                raise ActionError(f"Unknown policy '{n}'.")
            branch = policies.branch_of(g, n)
            if branch and branch != n and branch not in g.player(pid).policies:
                _adopt_counted(g, pid, branch)
            for pre in d.get("requires", []):
                if pre not in g.player(pid).policies:
                    _adopt_counted(g, pid, pre)
            if n not in g.player(pid).policies:
                _adopt_counted(g, pid, n)
                done.append(n)
        out[pid] = done
    return {"adopted": out}


def _adopt_counted(g: Game, pid: int, name: str):
    """Adopts without spending culture (the era check is skipped too) but counts toward later policy costs."""
    from . import policies
    p = g.player(pid)
    reason = policies.adoptable(g, pid, name, check_era=False)
    if reason:
        raise ActionError(reason)
    p.free_policies += 1
    try:
        policies.adopt(g, pid, name)
    except ActionError:
        # the era check inside adopt: add it directly, as a completed adoption
        p.free_policies -= 1
        p.culture += policies.culture_cost(g, pid)
        policies.adopt(g, pid, name)
        return
    p.policies_adopted_count += 1


@op("found_city", "player, x, y; optional name, pop, buildings: [...], capital (bool)")
def _found_city(g: Game, o: dict):
    """Found a city, optionally with population and buildings already in place."""
    from . import cities
    pid = _pid(g, o.get("player"))
    idx = _idx(g, o)
    for u in list(g.units_at(idx)):
        if u.owner != pid:
            g.remove_unit(u)
    c = cities.found_city(g, pid, idx, o.get("name"))
    if o.get("capital") and g.player(pid).capital != c.id:
        old = g.city(g.player(pid).capital) if g.player(pid).capital is not None else None
        ind = cities.capital_indicator(g, pid)
        if old is not None:
            cities.remove_building(g, old, ind)
        cities.add_building(g, c, ind, try_free=False)
        g.player(pid).capital = c.id
    _set_city_fields(g, c, o)
    return {"city_id": c.id, "name": c.name}


def _set_city_fields(g: Game, c, o: dict):
    """Apply the editable fields of a city."""
    from . import cities
    if o.get("pop") is not None:
        c.pop = max(1, int(o["pop"]))
        c.food = 0.0
    for b in o.get("buildings") or o.get("add_buildings") or []:
        b = _name(g, "building", b)
        if b not in c.buildings:
            if g.rules.buildings[b].get("isWonder") or g.rules.buildings[b].get("isNationalWonder"):
                g.s.wonders_built.setdefault(b, c.id) if g.rules.buildings[b].get("isWonder") else None
            cities.add_building(g, c, b, try_free=False)
    for b in o.get("remove_buildings") or []:
        cities.remove_building(g, c, _name(g, "building", b))
    if o.get("name"):
        cities.rename_city(g, c, o["name"])
    if o.get("claim_radius") is not None:
        r = max(1, min(int(o["claim_radius"]), g.rules.k["city_expand_range"]))
        for i in g.grid.within(c.idx, r):
            t = g.s.tiles[i]
            if t.city is None and t.owner is None:
                t.owner, t.city = c.owner, c.id
    if o.get("production"):
        cities.set_production(g, c, o["production"])
    g.invalidate()
    cities.assign_citizens(g, c)


@op("set_city", "city (id) or x, y; any of pop, add_buildings, remove_buildings, name, claim_radius (border radius), "
                "production")
def _set_city(g: Game, o: dict):
    """Change an existing city: population, buildings, name, borders, production."""
    c = _city(g, o)
    _set_city_fields(g, c, o)
    return {"city_id": c.id}


@op("remove_city", "city (id) or x, y")
def _remove_city(g: Game, o: dict):
    """Remove a city from the scenario."""
    from . import cities
    c = _city(g, o)
    cities.destroy_city(g, c)
    t = g.s.tiles[c.idx]
    if t.improvement == "City ruins" and not o.get("ruins"):
        t.improvement = None
    return {"removed": c.id}


@op("add_unit", "player, unit, x, y; optional count, promotions: [...], xp, hp")
def _add_unit(g: Game, o: dict):
    """Add units, optionally with promotions, experience and damage."""
    pid = _pid(g, o.get("player"), majors_only=False)
    utype = _name(g, "unit", o.get("unit"))
    idx = _idx(g, o)
    ids = []
    for _ in range(max(1, min(int(o.get("count") or 1), 50))):
        u = g.create_unit(pid, utype, idx, xp=int(o.get("xp") or 0))
        for pr in o.get("promotions") or []:
            pr = _name(g, "promotion", pr)
            if pr not in u.promotions:
                u.promotions.append(pr)
        if o.get("hp") is not None:
            u.hp = max(1, min(100, int(o["hp"])))
        ids.append(u.id)
    g.invalidate()
    return {"unit_ids": ids}


@op("remove_units", "x, y (every unit there), or unit (one id); optional player (only theirs)")
def _remove_units(g: Game, o: dict):
    """Remove units from a tile, or one unit by id."""
    if o.get("unit") is not None:
        u = g.unit(int(o["unit"]))
        if u is None:
            raise ActionError("No such unit.")
        targets = [u]
    else:
        idx = _idx(g, o)
        targets = [u for u in g.s.units.values() if u.idx == idx]
    if o.get("player") is not None:
        pid = _pid(g, o["player"], majors_only=False)
        targets = [u for u in targets if u.owner == pid]
    for u in targets:
        g.remove_unit(u)
    g.invalidate()
    return {"removed": [u.id for u in targets]}


@op("set_tile", "x, y; any of terrain, features: [...], resource (null to clear), amount, improvement, route, wonder, "
                "river (bitmask), owner (player id or null, for unclaimed tiles)")
def _set_tile(g: Game, o: dict):
    """Change a tile: terrain, features, resource, improvement, route, river, owner."""
    idx = _idx(g, o)
    t = g.s.tiles[idx]
    if "terrain" in o:
        t.terrain = _name(g, "terrain", o["terrain"])
    if "features" in o:
        t.features = [_name(g, "terrain", f) for f in (o["features"] or [])]
    if "resource" in o:
        t.resource = _name(g, "resource", o["resource"]) if o["resource"] else None
        t.resource_amount = int(o.get("amount") or 0)
    elif "amount" in o:
        t.resource_amount = int(o["amount"] or 0)
    if "improvement" in o:
        t.improvement = _name(g, "improvement", o["improvement"]) if o["improvement"] else None
        t.pillaged = False
    if "route" in o:
        t.route = o["route"] if o["route"] in ("Road", "Railroad") else None
    if "wonder" in o:
        t.wonder = _name(g, "terrain", o["wonder"]) if o["wonder"] else None
    if "river" in o:
        t.river = int(o["river"] or 0) & 63
    if "owner" in o and t.city is None:
        t.owner = _pid(g, o["owner"], majors_only=False) if o["owner"] is not None else None
    g.clear_static()
    g.invalidate()
    return {}


@op("meet", "a, b (player ids; b may be 'all'): the civilizations know each other")
def _meet(g: Game, o: dict):
    """Make civilizations known to each other."""
    a = _pid(g, o.get("a"), majors_only=False)
    for b in _players(g, o.get("b"), majors_only=False):
        if b != a:
            g.meet(a, b)
    return {}


@op("set_relation", "a, b; any of state ('war' | 'peace'), embassies (bool), friends (bool), defensive_pact (bool), "
                    "open_borders (bool), turns (length of agreements, default 30), opinion (number: a's opinion of b)")
def _set_relation(g: Game, o: dict):
    """Set war, peace, embassies, friendship, pacts, open borders and opinion between two civilizations."""
    from . import diplomacy as D
    a, b = _pid(g, o.get("a"), majors_only=False), _pid(g, o.get("b"), majors_only=False)
    if a == b:
        raise ActionError("A relation needs two different players.")
    g.meet(a, b)
    rel = D.relation(g, a, b)
    turns = int(o.get("turns") or 30)
    state = o.get("state")
    if state == "war" and not rel["war"]:
        D.set_war(g, a, b, reason="scenario")
    elif state == "peace" and rel["war"]:
        D.make_peace(g, a, b)
        rel["treaty_until"] = 0
    if o.get("embassies") is not None:
        emb = rel.setdefault("embassy", {})
        for x, y in ((a, b), (b, a)):
            if o["embassies"]:
                emb[f"{x}>{y}"] = True
            else:
                emb.pop(f"{x}>{y}", None)
    if o.get("friends") is not None:
        rel["friendship_until"] = g.turn + turns if o["friends"] else 0
    if o.get("defensive_pact") is not None:
        rel["pact_until"] = g.turn + turns if o["defensive_pact"] else 0
    if o.get("open_borders") is not None:
        for x, y in ((a, b), (b, a)):
            if o["open_borders"]:
                g.s.open_borders[f"{x}>{y}"] = g.turn + turns
            else:
                g.s.open_borders.pop(f"{x}>{y}", None)
    if o.get("opinion") is not None:
        rel.setdefault("opinion", {})[f"{a}>{b}"] = {"scenario": float(o["opinion"])}
    g.invalidate()
    return {"war": rel["war"]}


@op("set_influence", "city_state, player, amount: the civ's influence with a city-state")
def _set_influence(g: Game, o: dict):
    """Set a civilization's influence with a city-state."""
    from . import city_states
    cs = _pid(g, o.get("city_state"), majors_only=False)
    if g.player(cs).kind != "city_state":
        raise ActionError(f"Player {cs} is not a city-state.")
    pid = _pid(g, o.get("player"))
    g.meet(cs, pid)
    cur = g.player(cs).influence.get(str(pid), 0)
    city_states.add_influence(g, cs, pid, float(o.get("amount") or 0) - cur)
    return {"influence": g.player(cs).influence.get(str(pid))}


@op("reveal", "player (id or 'all'): explore the whole map; meet (bool) also meets every civilization")
def _reveal(g: Game, o: dict):
    """Reveal the map to a civilization, optionally meeting everyone as well."""
    for pid in _players(g, o.get("player"), majors_only=False):
        p = g.player(pid)
        p.explored = bytearray(b"\x01" * g.grid.size)
        if o.get("meet"):
            for q in g.s.players:
                if q.kind != "barbarian" and q.id != pid:
                    g.meet(pid, q.id)
    from . import visibility
    visibility.refresh(g, force=True)
    return {}


@op("set_research", "player, tech: what the civ is researching")
def _set_research(g: Game, o: dict):
    """Set what a civilization is researching."""
    from . import research
    pid = _pid(g, o.get("player"))
    return research.set_research(g, pid, _name(g, "tech", o.get("tech")))


def apply_ops(g: Game, ops: list[dict]) -> list[dict]:
    """Applies operations in order; stops at the first failure (ActionError with the op number)."""
    from . import visibility
    results = []
    for n, o in enumerate(ops or []):
        if not isinstance(o, dict) or o.get("op") not in OPS:
            raise ActionError(f"Operation {n + 1}: unknown op {o.get('op') if isinstance(o, dict) else o!r}. "
                              f"Known: {', '.join(sorted(OPS))}.")
        try:
            results.append(OPS[o["op"]][0](g, o) or {})
        except ActionError as e:
            raise ActionError(f"Operation {n + 1} ({o['op']}): {e}")
        except (KeyError, TypeError, ValueError) as e:
            raise ActionError(f"Operation {n + 1} ({o['op']}): bad parameters ({type(e).__name__}: {e}).")
    g.invalidate()
    visibility.refresh(g, force=True)
    return results


def ops_help() -> list[dict]:
    """Every operation with its parameters. The reference the editor shows."""
    return [{"op": k, "params": doc} for k, (_, doc) in sorted(OPS.items())]


# ----------------------------------------------------------------------------
# summaries for the editor
# ----------------------------------------------------------------------------
def overview(g: Game) -> dict:
    """A summary of the scenario's state: civilizations, cities, units, era."""
    from . import research
    from .diplomacy import relation, has_embassy, is_friends, has_pact
    R = g.rules
    players = []
    for p in g.s.players:
        if p.kind == "barbarian":
            continue
        d = {"id": p.id, "name": p.name, "nation": p.nation, "kind": p.kind, "color": p.color, "alive": p.alive,
             "controller": p.controller, "handicap": p.handicap, "auto": dict(p.auto), "difficulty": p.difficulty}
        if p.kind == "major":
            d.update({"gold": int(p.gold), "faith": int(p.faith), "culture": int(p.culture), "techs": len(p.techs),
                      "era": R.era_list[research.player_era(g, p.id)], "policies": list(p.policies),
                      "cities": len(g.player_cities(p.id)), "units": len(g.player_units(p.id)),
                      "met": list(p.met)})
        else:
            d.update({"cs_type": p.cs_type, "influence": dict(p.influence), "ally": p.ally,
                      "cities": len(g.player_cities(p.id))})
        players.append(d)
    rels = []
    majors = [p.id for p in g.majors()]
    for i, a in enumerate(majors):
        for b in majors[i + 1:]:
            rel = relation(g, a, b)
            rels.append({"a": a, "b": b, "met": g.has_met(a, b), "war": bool(rel["war"]),
                         "embassies": has_embassy(g, a, b) and has_embassy(g, b, a), "friends": is_friends(g, a, b),
                         "defensive_pact": has_pact(g, a, b),
                         "open_borders": g.has_open_borders(a, b) and g.has_open_borders(b, a)})
    cities = [{"id": c.id, "name": c.name, "owner": c.owner, "x": g.grid.xy(c.idx)[0], "y": g.grid.xy(c.idx)[1],
               "pop": c.pop, "buildings": list(c.buildings)} for c in g.s.cities.values()]
    return {"turn": g.turn, "current": g.s.current, "players": players, "relations": rels, "cities": cities,
            "config": {k: g.s.config.get(k) for k in ("map", "map_size", "map_type", "speed", "difficulty",
                                                      "starting_era", "turn_limit", "barbarians", "city_states")}}


# ----------------------------------------------------------------------------
# files
# ----------------------------------------------------------------------------
def slug(name: str) -> str:
    """A filesystem-safe identifier from a name."""
    s = re.sub(r"[^a-z0-9]+", "-", (name or "").lower()).strip("-")
    return s[:60] or f"scenario-{int(time.time())}"


def _path(sid: str) -> Path:
    """The file for a scenario id, rejecting anything that is not a plain slug.

    Validated rather than trusted: the id arrives from a URL, and a path separator here would be a way
    out of the scenario directory.
    """
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,79}", sid or ""):
        raise ActionError(f"Invalid scenario id '{sid}'.")
    return SCENARIO_DIR / f"{sid}.citarscn"


def default_seats(g: Game) -> list[dict]:
    """Default seat types for a scenario, from how the game was being played."""
    return [{"type": p.controller if p.controller in SEAT_TYPES else "bot", "label": p.name} for p in g.majors()]


def normalize_seats(g: Game, seats: Optional[list]) -> list[dict]:
    """Validate a seat list against the scenario's civilizations."""
    base = default_seats(g)
    for i, s in enumerate(seats or []):
        if i < len(base) and isinstance(s, dict):
            if s.get("type") in SEAT_TYPES:
                base[i]["type"] = s["type"]
            if s.get("label"):
                base[i]["label"] = str(s["label"])[:60]
            for k in ("llm", "bot"):
                if isinstance(s.get(k), dict):
                    base[i][k] = {kk: vv for kk, vv in s[k].items() if kk != "api_key"}
            try:
                base[i].update(seat_overrides(s.get("handicap"), s.get("auto")))
            except ValueError as e:
                raise ActionError(f"Seat {i}: {e}")
    return base


def save_scenario(g: Game, sid: str, name: str, description: str = "", seats: Optional[list] = None) -> dict:
    """Save a scenario: its state, its seats and its description."""
    SCENARIO_DIR.mkdir(parents=True, exist_ok=True)
    sid = slug(sid or name)
    p = _path(sid)
    now = time.strftime("%Y-%m-%dT%H:%M:%S")
    created = now
    if p.exists():
        try:
            created = load_scenario(sid).get("created", now)
        except Exception:
            pass
    g.save_rng()
    data = {"format": "citar-scenario", "version": 1, "id": sid, "name": name or sid, "description": description or "",
            "seats": normalize_seats(g, seats), "state": g.s.to_dict(), "created": created, "modified": now}
    tmp = p.with_suffix(".tmp")
    with gzip.open(tmp, "wt", encoding="utf-8") as f:
        json.dump(data, f)
    _fs_replace(tmp, p)
    return summary(data)


def load_scenario(sid: str) -> dict:
    """Load a scenario from disk."""
    p = _path(sid)
    if not p.exists():
        raise ActionError(f"No scenario '{sid}'.")
    with gzip.open(p, "rt", encoding="utf-8") as f:
        return json.load(f)


def delete_scenario(sid: str):
    """Delete a scenario."""
    p = _path(sid)
    if p.exists():
        p.unlink()


def summary(data: dict) -> dict:
    """A scenario's headline facts, for lists."""
    st = data["state"]
    majors = [p for p in st["players"] if p.get("kind") == "major"]
    return {"id": data["id"], "name": data["name"], "description": data.get("description", ""),
            "width": st["width"], "height": st["height"], "turn": st["turn"],
            "players": [{"id": p["id"], "name": p["name"], "nation": p.get("nation")} for p in majors],
            "city_states": sum(1 for p in st["players"] if p.get("kind") == "city_state"),
            "seats": data.get("seats", []), "modified": data.get("modified")}


def list_scenarios() -> list[dict]:
    """Every saved scenario, in summary."""
    SCENARIO_DIR.mkdir(parents=True, exist_ok=True)
    out = []
    for p in sorted(SCENARIO_DIR.glob("*.citarscn"), key=lambda p: p.stat().st_mtime, reverse=True):
        try:
            with gzip.open(p, "rt", encoding="utf-8") as f:
                out.append(summary(json.load(f)))
        except (OSError, ValueError, KeyError):
            continue
    return out


def game_from_state(state: dict) -> Game:
    """A fresh, independent game from a scenario (or save) state."""
    from . import visibility
    g = Game(GameState.from_dict(copy.deepcopy(state)))
    visibility.refresh(g, force=True)
    return g

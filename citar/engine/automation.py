"""Standing orders: multi-turn goto, automated exploration, automated workers, and city-site suggestions.

Worker job choice follows the idea of UnCiv's WorkerAutomation (MPL-2.0): tiles near cities are ranked (worked tiles,
resources and pillaged improvements first), and on each tile the improvement with the best yield gain is chosen;
roads connect cities to the capital.
"""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import tiles as T
from .game import ActionError

if TYPE_CHECKING:
    from .game import Game
    from .state import Unit

YIELD_WEIGHTS = {"food": 1.4, "production": 1.2, "gold": 0.9, "science": 1.0, "culture": 0.9, "faith": 0.8,
                 "happiness": 1.0}


def _ud(g, u):
    """A unit's ruleset definition."""
    return g.rules.units[u.type]


def run_unit_orders(g: "Game", pid: int):
    """Carry out every unit's standing orders at the start of a turn.

    This is what makes "explore", "automate" and a long move work without the player doing anything -
    and why a unit can have no movement left on a turn nobody touched it.
    """
    from . import movement
    claimed: set = set()
    for u in sorted(g.player_units(pid), key=lambda u: u.id):
        if g.unit(u.id) is None:
            continue
        try:
            t = g.s.tiles[u.idx]
            if u.activity == "build" and t.build and _civilian_in_danger(g, u):
                what = t.build[-1][0]
                u.activity = None
                g.emit("unit_woke", f"{u.type} #{u.id} stopped building {what}: enemies nearby.", [pid],
                       idx=u.idx, unit=u.id)
            if u.activity == "goto" and u.goto is not None:
                dest = u.goto
                res = movement.move_toward(g, u, dest)
                if res.get("stopped") == "blocked by a friendly unit" and res["from"] == res["to"]:
                    u.activity, u.goto = None, None
                    g.emit("orders_interrupted", f"{u.type} #{u.id} could not continue to {g.fmt_xy(dest)}: "
                           f"the way is blocked by your own unit.", [pid], idx=u.idx, unit=u.id)
                elif res.get("stopped") and res["stopped"] not in ("out of moves", "blocked by a friendly unit"):
                    g.emit("orders_interrupted", f"{u.type} #{u.id} stopped its move: {res['stopped']}.",
                           [pid], idx=u.idx, unit=u.id)
            elif u.activity == "explore":
                explore(g, u)
            elif u.activity == "automate":
                automate_worker(g, u, claimed)
            elif u.activity in ("sleep", "fortify"):
                if u.activity == "sleep" and _enemy_near(g, u, 2):
                    u.activity = None
                    g.emit("unit_woke", f"{u.type} #{u.id} woke up: enemies nearby.", [pid], idx=u.idx, unit=u.id)
        except ActionError:
            if u.activity == "goto":
                u.activity, u.goto = None, None


def _threat_reach(g: "Game", enemy: "Unit") -> int:
    """How far an enemy unit could strike from where it stands."""
    from .movement import max_moves, scale
    return max(2, int(max_moves(g, enemy) // scale(g)))


def _hostile_military(g: "Game", pid: int, other: "Unit") -> bool:
    """Whether another unit is a military threat to this civilization."""
    return g.at_war(pid, other.owner) and _ud(g, other)["_military"]


def _civilian_in_danger(g: "Game", u: "Unit") -> bool:
    """An unescorted civilian with a hostile military unit close enough to capture it next turn."""
    if _ud(g, u)["_military"] or g.military_at(u.idx) is not None or g.city_at(u.idx):
        return False
    from .visibility import visible_tiles
    vis = visible_tiles(g, u.owner)
    for idx in g.grid.within(u.idx, 5):
        if idx not in vis:
            continue
        for other in g.units_at(idx):
            if _hostile_military(g, u.owner, other) and _ud(g, other)["_domain"] == "Land" \
                    and g.grid.distance(idx, u.idx) <= _threat_reach(g, other):
                return True
    return False


def _enemy_near(g: "Game", u: "Unit", radius: int) -> bool:
    """Whether an enemy is within a radius of this unit."""
    return any(_hostile_military(g, u.owner, o) for idx in g.grid.within(u.idx, radius) for o in g.units_at(idx))


# ----------------------------------------------------------------------------
# City site suggestions (shared by bots and text briefings)
# ----------------------------------------------------------------------------
def city_site_score(g: "Game", pid: int, idx: int) -> Optional[float]:
    """How good a city site this tile is, or None if a city cannot go there."""
    from .cities import found_check
    p = g.player(pid)
    if not p.explored[idx] or found_check(g, pid, idx):
        return None
    if any(g.s.tiles[n].improvement == "Barbarian encampment" for n in g.grid.within(idx, 2)):
        return None
    v = 0.0
    for n in g.grid.within(idx, 2):
        if not p.explored[n]:
            v += 1
            continue
        y = T.tile_stats(g, n, pid)
        v += y.get("food", 0) * 1.4 + y.get("production", 0) + y.get("gold", 0) * 0.5
        t = g.s.tiles[n]
        if t.resource and T.resource_visible(g, pid, t.resource):
            v += 3 if g.rules.resources[t.resource]["resourceType"] == "Luxury" else 2
        if t.owner not in (None, pid):
            v -= 4
    if g.is_coastal(idx):
        v += 3
    if g.s.tiles[idx].river:
        v += 3
    if g.s.tiles[idx].hills:
        v += 2
    return v


def suggest_city_sites(g: "Game", pid: int, center: int, radius: int = 8, count: int = 3) -> list[tuple[int, float]]:
    """The best city sites near a point, which the client shows as green dashed tiles."""
    scored = []
    for idx in g.grid.within(center, radius):
        s = city_site_score(g, pid, idx)
        if s is not None:
            scored.append((idx, s - g.grid.distance(center, idx) * 2.2))
    scored.sort(key=lambda t: -t[1])
    out: list = []
    for idx, s in scored:
        if all(g.grid.distance(idx, o) >= 3 for o, _ in out):
            out.append((idx, s))
        if len(out) >= count:
            break
    return out


# ----------------------------------------------------------------------------
# Exploration
# ----------------------------------------------------------------------------
def _danger_tiles(g: "Game", pid: int, radius: Optional[int] = None) -> set:
    """Tiles a civilization's civilians should keep out of."""
    from .visibility import visible_tiles
    vis = visible_tiles(g, pid)
    out: set = set()
    for other in g.s.units.values():
        if other.idx in vis and _hostile_military(g, pid, other):
            out.update(g.grid.within(other.idx, radius if radius is not None else _threat_reach(g, other)))
    return out


def _passable(g, u, idx) -> bool:
    """Whether automation may route a unit through a tile."""
    from .movement import terrain_reason
    return terrain_reason(g, u.owner, _ud(g, u), idx, u) is None


def _off_limits(g, u, idx) -> bool:
    """Whether automation should refuse to send a unit to a tile at all."""
    t = g.s.tiles[idx]
    c = g.city_at(idx)
    return (not g.can_enter_territory(u.owner, idx) or t.improvement == "Barbarian encampment"
            or (c is not None and c.owner != u.owner))


def explore_target(g: "Game", u: "Unit", radius: int = 14, exclude: Optional[set] = None) -> Optional[int]:
    """Where an exploring unit should head next."""
    p = g.player(u.owner)
    ud = _ud(g, u)
    fighter = ud["_military"] and ud.get("unitType") != "Scout" and u.hp >= 60
    danger = (set() if fighter else _danger_tiles(g, u.owner)) | (exclude or set())
    land_unit = ud["_domain"] == "Land"

    def interesting(n):
        # land explorers care about unexplored tiles next to known land (not the open-ocean fog along coasts)
        """Whether a tile is worth exploring toward."""
        return not land_unit or any(p.explored[m] and T.is_land(g, m) for m in g.grid.neighbors(n))
    best, best_v = None, 0.0
    for idx in g.grid.within(u.idx, radius):
        if idx == u.idx or not p.explored[idx] or idx in danger:
            continue
        if not _passable(g, u, idx) or _off_limits(g, u, idx):
            continue
        unexplored = sum(1 for n in g.grid.within(idx, 2) if not p.explored[n] and interesting(n))
        if unexplored == 0:
            continue
        d = g.grid.distance(u.idx, idx)
        v = unexplored / (1 + d * 0.6)
        if g.s.tiles[idx].improvement == "Ancient ruins" and p.kind == "major":
            v += 6
        if v > best_v:
            best, best_v = idx, v
    return best if best is not None else _far_frontier(g, u, danger)


def _far_frontier(g: "Game", u: "Unit", danger: set, limit: int = 60) -> Optional[int]:
    """Nearest explored tile bordering unexplored tiles anywhere on the map, so explorers keep going."""
    p = g.player(u.owner)
    seen = {u.idx}
    frontier = [u.idx]
    depth = 0
    while frontier and depth < limit:
        depth += 1
        nxt = []
        for cur in frontier:
            for nb in g.grid.neighbors(cur):
                if nb in seen or not p.explored[nb]:
                    continue
                seen.add(nb)
                if not _passable(g, u, nb) or _off_limits(g, u, nb) or nb in danger:
                    continue
                if any(not p.explored[n] for n in g.grid.neighbors(nb)) and                         (_ud(g, u)["_domain"] != "Land" or T.is_land(g, nb)):
                    return nb
                nxt.append(nb)
        frontier = nxt
    return None


def _unexplored_near(g, pid, idx) -> int:
    """How much unexplored territory is near a tile, for ranking exploration targets."""
    p = g.player(pid)
    return sum(1 for n in g.grid.within(idx, 2) if not p.explored[n]
               and any(p.explored[m] and T.is_land(g, m) for m in g.grid.neighbors(n)))


def explore(g: "Game", u: "Unit") -> dict:
    """Move toward the best exploration target and keep that target until it is reached (so explorers don't
    oscillate); targets that turn out unreachable, or whose unexplored tiles can't be seen from there, are skipped."""
    from . import movement
    fl = g.player(u.owner).flags
    skip = set(fl.get("skip_explore") or [])
    targets = fl.setdefault("explore_targets", {})
    tries = 0
    moved_any = False
    while u.moves > 0 and tries < 6 and g.unit(u.id):
        tries += 1
        tgt = targets.get(str(u.id))
        if tgt is not None and (tgt in skip or tgt == u.idx or _unexplored_near(g, u.owner, tgt) == 0):
            if tgt == u.idx and _unexplored_near(g, u.owner, tgt):
                skip.add(tgt)                  # reached, but what's left can't be seen from here
            tgt = None
        if tgt is None:
            tgt = explore_target(g, u, exclude=skip)
        if tgt is None:
            u.activity = None
            targets.pop(str(u.id), None)
            g.emit("explore_done", f"{u.type} #{u.id} has nothing left to explore nearby.", [u.owner],
                   idx=u.idx, unit=u.id)
            return {"exploring": False}
        targets[str(u.id)] = tgt
        try:
            res = movement.move_toward(g, u, tgt, set_goto=False)
        except ActionError:
            skip.add(tgt)                      # no path from here: pick another target
            targets.pop(str(u.id), None)
            continue
        if res["from"] != res["to"]:
            moved_any = True
        if res.get("stopped") and res["stopped"] not in ("out of moves",):
            if res["from"] == res["to"]:
                skip.add(tgt)
                targets.pop(str(u.id), None)
                continue
            break
    if g.unit(u.id):
        u.activity = "explore"
        # back where it was two turns ago (fog changes the best path each turn): give up on this target
        hist = fl.setdefault("explore_hist", {}).setdefault(str(u.id), [])
        hist.append(u.idx)
        del hist[:-4]
        tgt = targets.get(str(u.id))
        if tgt is not None and len(hist) >= 3 and hist[-1] == hist[-3] and hist[-1] != hist[-2]:
            skip.add(tgt)
            targets.pop(str(u.id), None)
    if g.turn % 15 == 0:
        skip = set()
    fl["skip_explore"] = sorted(skip)[-300:]
    fl.pop("unreachable_explore", None)
    return {"exploring": True, "moved": moved_any}


# ----------------------------------------------------------------------------
# Worker automation
# ----------------------------------------------------------------------------
def _improvement_value(g: "Game", pid: int, idx: int, name: str) -> float:
    """Rough yield gain of building `name` here (improvement stats + the resource it would improve)."""
    R = g.rules
    t = g.s.tiles[idx]
    d = R.improvements[name]
    v = sum(d.get(k, 0) * w for k, w in YIELD_WEIGHTS.items())
    if t.resource and T.resource_visible(g, pid, t.resource) and T.resource_improved_by(g, t.resource, name):
        rd = R.resources[t.resource]
        v += sum(rd.get("improvementStats", {}).get(k, 0) * w for k, w in YIELD_WEIGHTS.items())
        v += {"Luxury": 6, "Strategic": 4}.get(rd["resourceType"], 1)
        if rd["resourceType"] == "Luxury":
            from .economy import happiness
            if t.resource not in happiness(g, pid).get("luxury_types", []):
                v += 8                      # a new luxury type: +4 happiness empire-wide
    if t.improvement and t.improvement != name:
        old = R.improvements.get(t.improvement, {})
        v -= sum(old.get(k, 0) * w for k, w in YIELD_WEIGHTS.items()) + 1
    return v


def _best_job(g: "Game", u: "Unit", idx: int) -> Optional[tuple[str, float]]:
    """The most valuable improvement for this tile. Cached for the rest of the owner's turn per tile state: every
    worker of the civ asks about the same tiles, and build_options is expensive."""
    t = g.s.tiles[idx]
    p = g.player(u.owner)
    sig = (g.turn, u.owner, t.improvement, t.route, tuple(t.features), t.resource, t.pillaged, t.route_pillaged,
           t.owner, t.fallout, tuple(tuple(b) for b in (t.build or ())), len(p.techs))
    cache = g._jobcache
    key = (u.type, idx)
    hit = cache.get(key)
    if hit is not None and hit[0] == sig:
        return hit[1]
    res = _best_job_uncached(g, u, idx)
    cache[key] = (sig, res)
    return res


def _best_job_uncached(g: "Game", u: "Unit", idx: int) -> Optional[tuple[str, float]]:
    """The most valuable improvement a worker could build on a tile, and what it is worth."""
    from . import workers
    t = g.s.tiles[idx]
    orig = u.idx
    u.idx = idx            # evaluate as if standing there (no occupancy change, so no cache invalidation)
    try:
        opts = workers.build_options(g, u)
    finally:
        u.idx = orig
    best = None
    for o in opts:
        name = o["name"]
        if o.get("instant") or name in workers.ROADS or name.startswith(workers.REMOVE):
            continue
        if name == workers.REPAIR:
            return name, 30.0
        if g.rules.improvements[name]["_umap"].has_tag("Great Improvement"):
            continue
        if t.improvement and g.rules.improvements.get(t.improvement, {}).get("_great"):
            continue
        v = _improvement_value(g, u.owner, idx, name) - o["turns"] * 0.15
        if v > 0.5 and (best is None or v > best[1]):
            best = (name, v)
    if best is None and t.fallout and any(o["name"] == "Remove Fallout" for o in opts):
        return "Remove Fallout", 8.0
    return best


def worker_jobs(g: "Game", u: "Unit", claimed: set) -> Optional[tuple[int, str]]:
    """The best job for an automated worker, avoiding tiles another worker has claimed."""
    from .cities import connected_cities, city_tiles, work_range
    pid = u.owner
    candidates = []
    cities = g.player_cities(pid)
    danger = _danger_tiles(g, pid)
    for c in cities:
        worked = set(c.worked)
        for idx in city_tiles(g, c):
            if idx == c.idx or idx in claimed or idx in danger:
                continue
            t = g.s.tiles[idx]
            # tiles beyond the work range are only worth improving for the resource they provide
            if g.grid.distance(idx, c.idx) > work_range(g) and not (t.resource and T.resource_visible(g, pid, t.resource)):
                continue
            if T.is_water(g, idx):
                continue
            other = g.civilian_at(idx)
            if other and other.id != u.id:
                continue
            job = _best_job(g, u, idx)
            if not job:
                continue
            name, value = job
            prio = 10.0 + value * 2
            if idx in worked:
                prio += 12
            prio -= g.grid.distance(u.idx, idx) * 1.5
            candidates.append((prio, idx, name))
    # roads toward the capital
    if g.has_tech(pid, g.rules.improvements["Road"].get("techRequired")):
        p = g.player(pid)
        cap = g.city(p.capital) if p.capital is not None else None
        connected = connected_cities(g, pid)
        if cap:
            for c in cities:
                if c.id in connected or c.id == cap.id or g.grid.distance(c.idx, cap.idx) > 14:
                    continue
                for idx in g.grid.line(c.idx, cap.idx)[1:-1]:
                    t = g.s.tiles[idx]
                    if T.is_water(g, idx) or T.is_impassable(g, idx):
                        break
                    if idx in claimed or idx in danger or (t.route and not t.route_pillaged):
                        continue
                    if t.owner not in (None, pid):
                        break
                    candidates.append((18 - g.grid.distance(u.idx, idx) * 1.2, idx, "Road"))
                    break
    if not candidates:
        return None
    candidates.sort(reverse=True)
    _, idx, job = candidates[0]
    return idx, job


def automate_worker(g: "Game", u: "Unit", claimed: Optional[set] = None) -> dict:
    """Send an automated worker to its next job."""
    from . import movement, workers
    claimed = claimed if claimed is not None else set()
    u.activity = "automate"
    if _civilian_in_danger(g, u) or (g.city_at(u.idx) and _enemy_near(g, u, 2)):
        cities = g.player_cities(u.owner)
        if cities:
            safe = min(cities, key=lambda c: g.grid.distance(c.idx, u.idx))
            if u.idx != safe.idx:
                try:
                    movement.move_toward(g, u, safe.idx, set_goto=False)
                except ActionError:
                    pass
        return {"status": "retreating"}
    t = g.s.tiles[u.idx]
    if t.build and t.build[0][1] >= 0 and workers.unit_can_build(g, u, t.build[0][0]):
        claimed.add(u.idx)
        return {"status": "working", "tile": g.xy(u.idx), "job": t.build[-1][0]}
    job = worker_jobs(g, u, claimed)
    if job is None:
        return {"status": "idle"}
    idx, target = job
    claimed.add(idx)
    if u.idx != idx:
        try:
            movement.move_toward(g, u, idx, set_goto=False)
        except ActionError:
            return {"status": "no path"}
    if g.unit(u.id) and u.idx == idx:
        try:
            workers.start_build(g, u, target)
        except ActionError:
            pass
        if g.unit(u.id):
            u.activity = "automate"
    return {"status": "working", "tile": g.xy(idx), "job": target}

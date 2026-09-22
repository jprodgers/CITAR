"""Unit movement: movement costs, embarkation, passability, stacking, zones of control, pathfinding.
Movement costs follow UnCiv's MovementCost / UnitMovement (MPL-2.0). Movement points are integers in
move_scale units (60 = one movement point): roads cost 30 (20 after "Improves movement speed on roads"), railroads 6.
"""
from __future__ import annotations

import heapq
from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .state import Unit
from .uniques import Ctx, applies, tile_matches, unit_matches
from . import tiles as T

if TYPE_CHECKING:
    from .game import Game

ALL = 10**6  # "consumes all remaining movement"


def scale(g: "Game") -> int:
    """The movement scale: internal movement points per displayed point.

    Movement is stored scaled so that fractional costs - a road, a river crossing - are integers
    internally. Every comparison in this module is in scaled units.
    """
    return g.rules.move_scale


def is_air(ud: dict) -> bool:
    """Whether a unit definition is an aircraft."""
    return ud["_domain"] == "Air"


def is_civilian(ud: dict) -> bool:
    """Whether a unit definition is a civilian."""
    return not ud["_military"]


class Profile:
    """Movement-relevant properties of a unit (MapUnitCache)."""
    __slots__ = ("ignores_terrain", "ignores_zoc", "all_1", "rough_penalty", "double", "disembark", "embark",
                 "on_water", "cannot_embark", "no_ocean", "foreign_ok", "cs_ok", "impassable_ok", "ice_ok",
                 "domain", "military", "ud", "pid", "unit")


def profile(g: "Game", u: Unit) -> Profile:
    """The movement rules applying to this unit, cached per type and promotions."""
    p = g.player(u.owner)
    key = ("mprof", u.id, u.type, len(u.promotions), len(p.techs), len(p.policies), u.idx)
    pr = g._cache.get(key)
    if pr is not None:
        return pr
    from .units import unit_umap, unit_uniques
    ud = g.rules.units[u.type]
    ctx = Ctx(g, civ=u.owner, unit=u, tile=u.idx)
    um = unit_umap(g, u)
    pr = Profile()
    pr.ud, pr.pid, pr.unit = ud, u.owner, u
    pr.domain = ud["_domain"]
    pr.military = ud["_military"]
    pr.all_1 = um.has(U.AllTilesCost1Move, ctx)
    pr.impassable_ok = um.has(U.CanPassImpassable, ctx)
    pr.ignores_terrain = um.has(U.IgnoresTerrainCost, ctx)
    pr.ignores_zoc = um.has(U.IgnoresZOC, ctx)
    pr.rough_penalty = um.has(U.RoughTerrainPenalty, ctx)
    pr.on_water = um.has(U.CanMoveOnWater, ctx)
    pr.double = [(x.p(0), x) for x in um.get(U.DoubleMovementOnTerrain)]
    dis = [int(x.n(0)) for x in unit_uniques(g, u, U.ReducedDisembarkCost, with_civ=True)]
    pr.disembark = min(dis) * g.rules.move_scale if dis else None
    emb = [int(x.n(0)) for x in unit_uniques(g, u, U.ReducedEmbarkCost, with_civ=True)] if hasattr(U, "ReducedEmbarkCost") else []
    pr.embark = min(emb) * g.rules.move_scale if emb else None
    pr.cannot_embark = um.has(U.CannotEmbark, ctx) if hasattr(U, "CannotEmbark") else False
    pr.no_ocean = um.has(U.CannotEnterOcean, ctx)
    pr.foreign_ok = um.has(U.CanEnterForeignTiles, ctx) or um.has(U.CanEnterForeignTilesButLosesReligiousStrength, ctx)
    pr.cs_ok = um.has(U.CanTradeWithCityStateForGoldAndInfluence, ctx)
    pr.ice_ok = um.has(U.CanEnterIceTiles, ctx)
    g._cache[key] = pr
    return pr


def civ_can_embark(g: "Game", pid: int) -> bool:
    """Whether this civilization's land units can cross water yet."""
    if g.is_barbarian(pid):
        return False
    return g.civ_has(pid, U.LandUnitEmbarkation)


def ocean_permissions(g: "Game", pid: int) -> tuple[bool, bool, list[str]]:
    """(all units may enter ocean, embarked units may enter ocean, specific unit filters)."""
    us = g.civ_uniques(pid, U.UnitsMayEnterOcean)
    allu = any(x.p(0) in ("All", "all") for x in us)
    emb = allu or any(x.p(0) == "Embarked" for x in us)
    spec = [x.p(0) for x in us if x.p(0) not in ("All", "all", "Embarked")]
    return allu, emb, spec


def is_embarked(g: "Game", u: Unit) -> bool:
    """Whether a land unit is currently at sea, which changes its strength and movement."""
    ud = g.rules.units[u.type]
    if ud["_domain"] != "Land":
        return False
    if T.is_water(g, u.idx) and g.city_at(u.idx) is None:
        from .units import unit_umap
        return not unit_umap(g, u).has_tag(U.CanMoveOnWater)
    return False


def max_moves(g: "Game", u: Unit) -> int:
    """This unit's movement allowance for a turn, after every modifier."""
    ud = g.rules.units[u.type]
    if is_air(ud):
        return scale(g)
    from .units import max_movement
    return max_movement(g, u) * scale(g)


# ---------------------------------------------------------------------------------------------------------------
# Passability
# ---------------------------------------------------------------------------------------------------------------
def terrain_reason(g: "Game", pid: int, ud: dict, idx: int, u: Optional[Unit] = None) -> Optional[str]:
    """Why the unit type cannot pass through this tile's terrain (None = OK)."""
    t = g.s.tiles[idx]
    city = g.city_at(idx)
    if is_air(ud):
        return None
    pr = profile(g, u) if u is not None else None
    if T.is_impassable(g, idx) and city is None:
        ok = (pr and pr.impassable_ok) or (pr and pr.ice_ok and "Ice" in t.features)
        if not ok and ud["_domain"] == "Land" and T.is_mountain(g, idx):
            # Carthage: land units may cross mountains after the first Great General
            for x in g.civ_uniques(pid, U.LandUnitsCrossTerrainAfterUnitGained):
                if x.p(0) == "Mountain" and g.player(pid).flags.get("gained_" + x.p(1)):
                    ok = True
        if not ok:
            return f"{T.last_terrain(t)} is impassable."
    water = T.is_water(g, idx)
    if not water and ud["_domain"] == "Water" and city is None:
        return "Naval units cannot move onto land."
    allu, emb, spec = ocean_permissions(g, pid)
    spec_ok = bool(u is not None and spec and any(unit_matches(g, u, f) for f in spec))
    if water and ud["_domain"] == "Land" and not (pr and pr.on_water):
        if city is None:
            if not civ_can_embark(g, pid):
                return "Your civilization cannot embark land units yet (requires Optics)."
            if pr and pr.cannot_embark:
                return "This unit cannot embark."
            if T.is_ocean(g, idx) and not emb and not spec_ok:
                return "Embarked units cannot enter ocean yet (requires Astronomy)."
    if T.is_ocean(g, idx) and not allu and not spec_ok:
        if u is not None and pr.no_ocean:
            return f"{u.type} cannot enter ocean tiles yet."
        if u is None and ud["_umap"].get(U.CannotEnterOcean):
            from .uniques import applies as _ap
            if any(_ap(x, Ctx(g, civ=pid, tile=idx)) for x in ud["_umap"].get(U.CannotEnterOcean)):
                return f"{ud['name']} cannot enter ocean tiles yet."
    return None


def can_pass_through(g: "Game", pid: int, ud: dict, idx: int, u: Optional[Unit] = None) -> bool:
    """Whether this unit could move through a tile."""
    return pass_reason(g, pid, ud, idx, u) is None


def pass_reason(g: "Game", pid: int, ud: dict, idx: int, u: Optional[Unit] = None) -> Optional[str]:
    """Why this unit cannot move through a tile, or None.

    The reason is returned rather than a boolean because it is shown: "you need Optics to cross water"
    is a rule somebody can learn, and a refusal without one is not.
    """
    r = terrain_reason(g, pid, ud, idx, u)
    if r:
        return r
    t = g.s.tiles[idx]
    owner = t.owner
    pr = profile(g, u) if u is not None else None
    if owner is not None and owner != pid:
        cs_ok = pr is not None and pr.cs_ok and g.player(owner).kind == "city_state"
        if not cs_ok and not (pr and pr.foreign_ok) and not g.can_enter_territory(pid, idx):
            return f"Cannot enter {g.player(owner).name}'s territory without open borders (or war)."
        city = g.city_at(idx)
        if city is not None and g.at_war(pid, owner):
            return "Enemy city: attack it instead."
    first = _first_unit(g, idx)
    if first is not None and first.owner != pid:
        fd = g.rules.units[first.type]
        water_embarked = ud["_domain"] == "Land" and T.is_water(g, idx) and not (pr and pr.on_water)
        if not water_embarked and not fd["_military"] and g.at_war(pid, first.owner) and ud["_military"]:
            return None
        if g.at_war(pid, first.owner):
            return "Enemy unit in the way: attack it instead."
    return None


def _first_unit(g, idx) -> Optional[Unit]:
    """The unit that would be met on a tile - military first, then civilian."""
    m = g.military_at(idx)
    if m is not None:
        return m
    c = g.civilian_at(idx)
    if c is not None:
        return c
    a = g.air_units_at(idx)
    return a[0] if a else None


def stack_reason(g: "Game", pid: int, ud: dict, idx: int, ignore_uid: Optional[int] = None) -> Optional[str]:
    """Why the unit type cannot END its move on idx because of other units (None = OK)."""
    city = g.city_at(idx)
    if is_air(ud):
        if city and city.owner == pid:
            from .units import air_capacity_ok
            if not air_capacity_ok(g, city, pid, ignore_uid):
                return "That city has no room for more aircraft."
            return None
        for other in g.units_at(idx):
            if other.owner == pid and _can_carry(g, other, ud, ignore_uid):
                return None
        return "Aircraft can only be based in your own cities or on carriers."
    for other in g.units_at(idx):
        if other.id == ignore_uid:
            continue
        od = g.rules.units[other.type]
        if is_air(od):
            continue
        if other.owner != pid:
            if not od["_military"] and ud["_military"] and g.at_war(pid, other.owner):
                continue           # capture
            return "Tile is occupied by a foreign unit."
        if od["_military"] != ud["_military"]:
            continue
        if not ud["_military"]:
            return "Another civilian unit is already there."
        return "Another military unit is already there."
    return None


def _can_carry(g, carrier: Unit, ud: dict, ignore_uid) -> bool:
    """Whether a carrier has room for this unit."""
    from .units import unit_uniques
    cap = 0
    for x in unit_uniques(g, carrier, U.CarryAirUnits):
        from .uniques import base_unit_matches
        if base_unit_matches(g.rules, ud["name"], x.p(1)):
            cap += int(x.n(0))
    for x in unit_uniques(g, carrier, U.CarryExtraAirUnits):
        from .uniques import base_unit_matches
        if base_unit_matches(g.rules, ud["name"], x.p(1)):
            cap += int(x.n(0))
    if cap <= 0:
        return False
    from .uniques import base_unit_matches
    if any(base_unit_matches(g.rules, carrier.type, x.p(0)) for x in ud["_umap"].get(U.CannotBeCarriedBy)):
        return False
    carried = sum(1 for o in g.s.units.values() if o.carried_by == carrier.id and o.id != ignore_uid)
    return carried < cap


def can_stand(g: "Game", pid: int, ud: dict, idx: int, u: Optional[Unit] = None) -> bool:
    """Whether this unit could end its move on a tile, which is stricter than passing through."""
    if is_air(ud):
        return stack_reason(g, pid, ud, idx) is None
    if pass_reason(g, pid, ud, idx, u) is not None:
        return False
    city = g.city_at(idx)
    if city and city.owner != pid:
        return False
    if ud["_domain"] == "Water" and city is not None and not (T.is_water(g, idx) or T.adjacent_to_coast(g, idx)):
        return False
    return stack_reason(g, pid, ud, idx, u.id if u else None) is None


# ---------------------------------------------------------------------------------------------------------------
# Costs
# ---------------------------------------------------------------------------------------------------------------
def river_between(g: "Game", a: int, b: int) -> bool:
    """Whether a river runs along the edge between two tiles."""
    ta = g.s.tiles[a]
    if not ta.river or not g.s.tiles[b].river:
        return False
    for d in range(6):
        if g.grid.neighbor_in_dir(a, d) == b:
            return bool(ta.river & (1 << d))
    return False


def route_at(g: "Game", idx: int) -> Optional[str]:
    """Effective route: city centres count as road/railroad once their owner knows the tech (City.tryUpdateRoadStatus)."""
    t = g.s.tiles[idx]
    r = T.unpillaged_route(t)
    c = g.city_at(idx)
    if c is not None:
        R = g.rules
        if g.has_tech(c.owner, R.improvements["Railroad"].get("techRequired")):
            return "Railroad"
        if g.has_tech(c.owner, R.improvements["Road"].get("techRequired")):
            return r or "Road"
    return r


def has_connection(g: "Game", pid: int, idx: int) -> bool:
    """Whether a tile has a road or railway this civilization may use."""
    t = g.s.tiles[idx]
    if route_at(g, idx):
        return True
    if t.owner == pid and t.feature in ("Forest", "Jungle") and g.civ_has(pid, U.ForestsAndJunglesAreRoads):
        return True
    return False


def zoc_between(g: "Game", pid: int, u_land: bool, a: int, b: int) -> bool:
    """UnCiv anyTilesExertingZoneOfControl: an enemy (city or unit) adjacent to both tiles."""
    for n in g.grid.neighbors(a):
        if g.grid.distance(n, b) != 1:
            continue
        c = g.city_at(n)
        if c is not None:
            if g.at_war(pid, c.owner):
                return True
            continue
        m = g.military_at(n)
        if m is not None and g.at_war(pid, m.owner):
            md = g.rules.units[m.type]
            if md["_domain"] == "Water" or (u_land and not is_embarked(g, m)):
                return True
    return False


def enter_cost(g: "Game", u: Unit, a: int, b: int, zoc: bool = True) -> int:
    """Movement cost from a to adjacent b (MovementCost.getMovementCostBetweenAdjacentTiles)."""
    pr = profile(g, u)
    sc = scale(g)
    pid = u.owner
    land_a, land_b = T.is_land(g, a) or g.city_at(a) is not None, T.is_land(g, b) or g.city_at(b) is not None
    if pr.domain == "Land" and land_a != land_b and not pr.on_water:
        if not land_a and land_b:
            return pr.disembark if pr.disembark is not None else ALL
        return pr.embark if pr.embark is not None else ALL
    if zoc and not g.is_barbarian(pid) and not pr.ignores_zoc and zoc_between(g, pid, pr.domain == "Land", a, b):
        return ALL
    if pr.all_1:
        return sc
    extra = 0
    owner = g.s.tiles[b].owner
    if owner is not None and g.at_war(pid, owner):
        for x in g.civ_uniques(owner, U.EnemyUnitsSpendExtraMovement):
            if unit_matches(g, u, x.p(0)):
                extra += int(x.n(1)) * sc
    _ta, tb = g.s.tiles[a], g.s.tiles[b]
    if route_at(g, a) == "Railroad" and route_at(g, b) == "Railroad":
        return sc // 10 + extra
    crossing = river_between(g, a, b)
    if has_connection(g, pid, a) and has_connection(g, pid, b) and \
            (not crossing or g.civ_has(pid, U.RoadsConnectAcrossRivers)):
        road = sc // 3 if g.civ_has(pid, U.RoadMovementSpeed) else sc // 2
        return road + extra
    if pr.ignores_terrain:
        return sc + extra
    if crossing:
        return ALL
    terrain_cost = sc if g.city_at(b) is not None else g.rules.terrains[T.last_terrain(tb)]["movementCost"] * sc
    ctx = Ctx(g, civ=pid, unit=u, tile=b)
    for f, x in pr.double:
        if f in tb.features and applies(x, ctx):
            return terrain_cost // 2 + extra
    if pr.rough_penalty and T.is_rough(g, b):
        return ALL
    if "Hill" in tb.features and g.civ_has(pid, U.IgnoreHillMovementCost):
        return sc + extra
    for f, x in pr.double:
        if (f == tb.terrain or (f == "Hill" and "Hill" in tb.features)) and applies(x, ctx):
            return terrain_cost // 2 + extra
    for f, x in pr.double:
        if f not in tb.features and f != tb.terrain and f != "Hill" and applies(x, ctx) and tile_matches(g, b, f, pid):
            return terrain_cost // 2 + extra
    return terrain_cost + extra


# ---------------------------------------------------------------------------------------------------------------
# Pathfinding
# ---------------------------------------------------------------------------------------------------------------
def _known(g, pid, idx) -> bool:
    """Whether a player has explored a tile."""
    return g.player(pid).explored[idx] != 0


def passable_for_path(g: "Game", u: Unit, idx: int, target: int, visible: set) -> bool:
    """Whether pathfinding may route through a tile.

    Deliberately optimistic about what has not been explored: a path through unknown territory is how
    exploration happens, and refusing to plan through fog would make every long move a series of short
    ones.
    """
    pid = u.owner
    ud = g.rules.units[u.type]
    if not (g.is_barbarian(pid) or _known(g, pid, idx)):
        return True
    if terrain_reason(g, pid, ud, idx, u) is not None:
        return False
    t = g.s.tiles[idx]
    pr = profile(g, u)
    if t.owner is not None and t.owner != pid and not g.can_enter_territory(pid, idx) and not pr.foreign_ok and \
            not (pr.cs_ok and g.player(t.owner).kind == "city_state"):
        return False
    city = g.city_at(idx)
    if city and city.owner != pid:
        return False
    if idx in visible or g.is_barbarian(pid):
        for other in g.units_at(idx):
            od = g.rules.units[other.type]
            if other.owner != pid and not is_air(od):
                if idx == target and not od["_military"] and ud["_military"] and g.at_war(pid, other.owner) \
                        and g.military_at(idx) is None:
                    continue
                return False
    return True


def find_path(g: "Game", u: Unit, target: int, max_turns: int = 40) -> Optional[list[int]]:
    """The best path to a target, or None if there is none within the turn limit."""
    from .visibility import visible_tiles
    if target == u.idx:
        return [u.idx]
    ud = g.rules.units[u.type]
    if is_air(ud):
        return None
    pid = u.owner
    visible = visible_tiles(g, pid) if not g.is_barbarian(pid) else set()
    own_occupied = {o.idx for o in g.player_units(pid) if o.id != u.id}
    full = max_moves(g, u)
    best = {u.idx: (0, -u.moves)}
    prev: dict = {}
    heap = [(0, -u.moves, u.idx)]
    if (g.is_barbarian(pid) or _known(g, pid, target)) and terrain_reason(g, pid, ud, target, u) is not None:
        return None
    while heap:
        turns, negleft, cur = heapq.heappop(heap)
        if best.get(cur) != (turns, negleft):
            continue
        if cur == target or turns > max_turns:
            break
        left = -negleft
        for nb in g.grid.neighbors(cur):
            if not passable_for_path(g, u, nb, target, visible):
                continue
            t2, l2 = turns, left
            if l2 <= 0:
                t2 += 1
                l2 = full
            cost = enter_cost(g, u, cur, nb)
            l2 = 0 if cost >= l2 else l2 - cost
            # a unit may pass through its own units but can't end its turn stacked with one of the same kind
            if l2 == 0 and nb != target and nb in own_occupied and stack_reason(g, pid, ud, nb, u.id):
                continue
            key = (t2, -l2)
            old = best.get(nb)
            if old is None or key < old:
                best[nb] = key
                prev[nb] = cur
                heapq.heappush(heap, (t2, -l2, nb))
    if target not in best:
        return None
    path = [target]
    while path[-1] != u.idx:
        path.append(prev[path[-1]])
    path.reverse()
    return path


def path_turns(g: "Game", u: Unit, path: list[int]) -> int:
    """How many turns a path takes at this unit's movement rate."""
    full = max_moves(g, u)
    left = u.moves
    turns = 1
    for a, b in zip(path, path[1:]):
        if left <= 0:
            turns += 1
            left = full
        cost = enter_cost(g, u, a, b)
        left = 0 if cost >= left else left - cost
    return turns


def reachable_this_turn(g: "Game", u: Unit) -> dict[int, int]:
    """Every tile this unit could reach this turn, with what it would cost."""
    from .visibility import visible_tiles
    ud = g.rules.units[u.type]
    if is_air(ud) or u.moves <= 0:
        return {}
    pid = u.owner
    visible = visible_tiles(g, pid)
    best = {u.idx: u.moves}
    heap = [(-u.moves, u.idx)]
    while heap:
        negleft, cur = heapq.heappop(heap)
        left = -negleft
        if best.get(cur, -1) != left or left <= 0:
            continue
        for nb in g.grid.neighbors(cur):
            if not _known(g, pid, nb) or not passable_for_path(g, u, nb, -1, visible):
                continue
            cost = enter_cost(g, u, cur, nb)
            l2 = 0 if cost >= left else left - cost
            if l2 > best.get(nb, -1):
                best[nb] = l2
                heapq.heappush(heap, (-l2, nb))
    del best[u.idx]
    return {k: v for k, v in best.items() if stack_reason(g, pid, ud, k, u.id) is None}


# ---------------------------------------------------------------------------------------------------------------
# Execution
# ---------------------------------------------------------------------------------------------------------------
def step(g: "Game", u: Unit, nb: int) -> Optional[str]:
    """Move a unit one tile, returning why it could not if it could not."""
    from . import units as unitmod
    ud = g.rules.units[u.type]
    pid = u.owner
    if u.moves <= 0:
        return "no moves left"
    if nb not in g.grid.neighbors(u.idx):
        return "not adjacent"
    reason = pass_reason(g, pid, ud, nb, u)
    if reason:
        return reason
    city = g.city_at(nb)
    if city and city.owner != pid:
        return "Foreign city: you cannot enter it."
    capture = None
    for other in g.units_at(nb):
        od = g.rules.units[other.type]
        if other.owner == pid or is_air(od):
            continue
        if not od["_military"] and ud["_military"] and g.at_war(pid, other.owner) and g.military_at(nb) is None:
            capture = other
            continue
        return "Tile is occupied by a foreign unit."
    cost = enter_cost(g, u, u.idx, nb)
    u.moves = 0 if cost >= u.moves else u.moves - cost
    if capture is not None:
        unitmod.capture_civilian(g, u, capture)
    g.place_unit(u, nb)
    u.fortify = 0
    if u.activity in ("fortify", "fortify_heal", "sleep", "sleep_heal"):
        u.activity = None
    u.acted = True
    on_enter_tile(g, u, nb)
    return None


def on_enter_tile(g: "Game", u: Unit, idx: int):
    """Ruins, barbarian camps, natural wonders."""
    t = g.s.tiles[idx]
    if t.improvement == "Ancient ruins" and g.player(u.owner).kind == "major":
        from . import ruins
        ruins.enter(g, u, idx)
    elif t.improvement == "Barbarian encampment" and g.rules.units[u.type]["_military"] and not g.is_barbarian(u.owner):
        from . import barbarians
        barbarians.clear_camp(g, idx, u.owner, unit=u)
    if g.unit(u.id) is not None:
        terrain_promotions(g, u)


def terrain_promotions(g: "Game", u: Unit):
    """Natural wonders such as the Fountain of Youth promote adjacent units for the rest of the game."""
    from .tiles import all_terrains
    from .uniques import unit_matches
    from .units import add_promotion
    R = g.rules
    for n in g.grid.within(u.idx, 1):
        for tn in all_terrains(g.s.tiles[n]):
            for x in R.terrains[tn]["_umap"].get(U.TerrainGrantsPromotion):
                promo = x.p(0)
                if promo in R.promotions and promo not in u.promotions and unit_matches(g, u, x.p(2)):
                    add_promotion(g, u, promo, free=True)


ORDER_PATIENCE = 3       # turns a standing move order waits for its next tile to clear before it is given up
WAITING = ("out of moves", "blocked by a friendly unit", "blocked by a foreign unit")


def move_toward(g: "Game", u: Unit, target: int, set_goto: bool = True, continuing: bool = False) -> dict:
    """Move a unit toward a target, as far as this turn allows, remembering the destination.

    The standing order is what makes a distant move one instruction rather than one per turn - and is
    why a unit that appears to have done nothing may simply have had its movement spent by the order
    it was already carrying out.

    A new order plans its route; carrying on with a standing order (``continuing``) follows the route planned
    when the order was given and never re-plans it. A unit whose next tile is taken (by one of its own units it
    can't stop on, or by a foreign unit) waits there with its order intact and carries on when the way clears.
    The order ends when the unit arrives, when a new enemy comes into view, when the route turns out to be
    impassable, when the unit is no longer on its route, or after ORDER_PATIENCE turns without getting further.
    """
    from . import visibility
    if continuing and u.path:
        if u.path[-1] != target or u.idx not in u.path:
            u.activity, u.goto, u.path, u.order_wait = None, None, None, 0
            return {"from": g.xy(u.idx), "to": g.xy(u.idx), "arrived": False, "moves_left": round(u.moves / scale(g), 2),
                    "stopped": "no longer on its planned route", "order_kept": False, "gave_up": True,
                    "turns_remaining": 0}
        path = u.path[u.path.index(u.idx):]
    else:
        path = find_path(g, u, target)       # a new order (or one saved before routes were kept)
        if path is None:
            raise ActionError(f"No path from {g.fmt_xy(u.idx)} to {g.fmt_xy(target)}.")
        if not continuing:
            u.order_wait = 0
    ud = g.rules.units[u.type]
    start = u.idx
    stop_reason = None
    for nb in path[1:]:
        if u.moves <= 0:
            stop_reason = "out of moves"
            break
        others = [o for o in g.units_at(nb) if o.owner != u.owner and not is_air(g.rules.units[o.type])]
        if not others and stack_reason(g, u.owner, ud, nb, u.id):
            cost = enter_cost(g, u, u.idx, nb)
            if nb == target or cost >= u.moves:
                stop_reason = "blocked by a friendly unit"
                break
        capturable = bool(others) and ud["_military"] and g.at_war(u.owner, others[0].owner) \
            and not any(g.rules.units[o.type]["_military"] for o in others)
        if others and not capturable:
            stop_reason = "blocked by a foreign unit"
            break
        seen_before = {m.id for m in _visible_enemies(g, u.owner)}
        reason = step(g, u, nb)
        if reason:
            stop_reason = reason
            break
        visibility.refresh(g)
        if g.unit(u.id) is None:
            stop_reason = "unit lost"
            break
        new_enemies = [m for m in _visible_enemies(g, u.owner) if m.id not in seen_before]
        if new_enemies and u.idx != target:
            stop_reason = "enemy spotted"
            break
    alive = g.unit(u.id) is not None
    arrived = alive and u.idx == target
    gave_up = False
    if alive:
        if arrived:
            if u.activity == "goto":
                u.activity = None
            u.goto, u.path, u.order_wait = None, None, 0
        elif set_goto and stop_reason in WAITING:
            if u.idx != start:
                u.order_wait = 0
            elif stop_reason != "out of moves":
                u.order_wait += 1           # held up where it stands: wait for the way to clear, but not forever
            if u.order_wait >= ORDER_PATIENCE:
                gave_up = True
                stop_reason = f"{stop_reason}, no progress for {u.order_wait} turns"
                u.goto, u.path, u.order_wait = None, None, 0
                if u.activity == "goto":
                    u.activity = None
            else:
                u.activity = "goto"
                u.goto = target
                u.path = path if not continuing or not u.path else u.path
        elif stop_reason != "out of moves":
            u.goto, u.path, u.order_wait = None, None, 0
            if u.activity == "goto":
                u.activity = None
    return {"from": g.xy(start), "to": g.xy(u.idx) if alive else None, "arrived": arrived,
            "moves_left": round(u.moves / scale(g), 2) if alive else 0, "stopped": None if arrived else stop_reason,
            "order_kept": bool(alive and u.goto == target), "gave_up": gave_up,
            "turns_remaining": (_remaining_turns(g, u, path, target) if (not arrived and alive and u.goto) else 0)}


def _remaining_turns(g: "Game", u: Unit, path: list[int], target: int) -> int:
    """Turns left on the planned path from where the unit stopped (avoids a second pathfinding search)."""
    if u.idx in path:
        rest = path[path.index(u.idx):]
        if rest and rest[-1] == target:
            return path_turns(g, u, rest)
    return path_turns(g, u, find_path(g, u, target) or [u.idx])


def _visible_enemies(g: "Game", pid: int):
    """Enemies this civilization can currently see, for deciding when to stop a move."""
    from .visibility import visible_tiles
    if g.is_barbarian(pid):
        return []
    vis = visible_tiles(g, pid)
    return [u for u in g.s.units.values() if u.owner != pid and u.idx in vis and g.at_war(pid, u.owner)
            and g.rules.units[u.type]["_military"]]


def teleport_to_closest(g: "Game", u: Unit):
    """Move a unit that may not stay where it is to the nearest legal tile (UnitMovement.teleportToClosestMoveableTile)."""
    ud = g.rules.units[u.type]
    for r in range(1, 8):
        for idx in g.grid.ring(u.idx, r):
            t = g.s.tiles[idx]
            if (t.owner is None or t.owner == u.owner or g.can_enter_territory(u.owner, idx)) and can_stand(g, u.owner, ud, idx, u):
                g.place_unit(u, idx)
                return
    g.remove_unit(u)

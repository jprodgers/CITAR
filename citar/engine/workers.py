"""Tile improvements: building (workers), instant great improvements, work boats, routes, feature removal, repair
and pillaging. Port of UnCiv's ImprovementFunctions, TileImprovementFunctions, Tile.doWorkerTurn, MapUnit
.canBuildImprovement, TileImprovement.getTurnsToBuild and UnitActionsPillage (MPL-2.0).

Work in progress lives on the tile (`tile.build` = [[improvement, turns left], ...]) like UnCiv's improvement queue;
a worker standing on the tile with movement left at the end of its turn advances the first entry.
"""
from __future__ import annotations

import math
from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from . import tiles as T
from .game import ActionError
from .uniques import Ctx, applies, improvement_matches, tile_matches, tile_terrain_matches, terrain_matches

if TYPE_CHECKING:
    from .game import Game
    from .state import Unit

REMOVE = "Remove "
REPAIR = "Repair"
CANCEL = "Cancel improvement order"
ROADS = ("Road", "Railroad")
ROAD_RANK = {None: 0, "Road": 1, "Railroad": 2}


def _imp(g, name) -> dict:
    """An improvement's ruleset definition."""
    return g.rules.improvements[name]


def _has(g, name, ph, ctx=None) -> bool:
    """Whether an improvement carries a unique."""
    return any(applies(x, ctx) for x in _imp(g, name)["_umap"].get(ph))


def _uniques(g, name, ph, ctx=None) -> list:
    """An improvement's uniques matching a placeholder."""
    return [x for x in _imp(g, name)["_umap"].get(ph) if applies(x, ctx)]


def can_be_built_on(g, name: str, terrain: str) -> bool:
    """TileImprovement.canBeBuiltOn(Terrain): any entry of terrainsCanBeBuiltOn matches the terrain."""
    return any(terrain_matches(g.rules, terrain, f) for f in _imp(g, name).get("terrainsCanBeBuiltOn", []))


def allowed_on_feature(g, name: str, terrain: str) -> bool:
    """Whether an improvement may be built on this terrain or feature."""
    return can_be_built_on(g, name, terrain) or any(
        terrain_matches(g.rules, terrain, x.p(0)) for x in _imp(g, name)["_umap"].get(U.NoFeatureRemovalNeeded))


def _feature_removals(g) -> list[str]:
    """The pseudo-improvements that represent removing a feature, such as chopping forest."""
    return [n for n in g.rules.improvements if n.startswith(REMOVE) and n[len(REMOVE):] not in ROADS]


def _built_here_ok(g: "Game", idx: int, name: str, pid: Optional[int], ctx: Ctx, features: Optional[list] = None,
                   known_removals: Optional[list] = None) -> bool:
    """TileImprovementFunctions.canImprovementBeBuiltHere."""
    R = g.rules
    t = g.s.tiles[idx]
    feats = list(t.features) if features is None else features
    last = feats[-1] if feats else (t.wonder or t.terrain)
    if t.wonder and features is None:
        last = t.wonder
    res_visible = t.resource is not None and T.resource_visible(g, pid, t.resource)
    if name == t.improvement:
        return False
    if g.city_at(idx) is not None:
        return False
    if name == CANCEL:
        return bool(t.build)
    if name in (REMOVE + r for r in ROADS):
        return t.route == name[len(REMOVE):]
    if name.startswith(REMOVE):
        return name[len(REMOVE):] in feats or name[len(REMOVE):] == t.improvement
    if name in ROADS:
        return not T.is_water(g, idx) and ROAD_RANK[name] > ROAD_RANK[t.route]
    if t.improvement and _has(g, t.improvement, U.Irremovable, ctx):
        return False
    if R.terrains[last].get("unbuildable") and not allowed_on_feature(g, name, last):
        if not _has(g, name, U.RemovesFeaturesIfBuilt, ctx) or not known_removals:
            return False
        rem = [f for f in feats if REMOVE + f in R.improvements]
        if not rem or any(REMOVE + f not in known_removals for f in rem):
            return False
        return _built_here_ok(g, idx, name, pid, ctx, [f for f in feats if f not in rem], known_removals)
    restrict = [x for tn in [t.terrain] + feats for x in R.terrains[tn]["_umap"].get(U.RestrictedBuildableImprovements)]
    if restrict and not any(improvement_matches(R, name, x.p(0)) for x in restrict):
        return False
    if any(tile_matches(g, idx, x.p(0), pid) for x in _uniques(g, name, U.CannotBuildOnTile, ctx)):
        return False
    only = _uniques(g, name, U.CanOnlyBeBuiltOnTile, ctx)
    if only and any(not tile_matches(g, idx, x.p(0), pid) for x in only):
        return False
    for x in _uniques(g, name, U.MustBeNextTo, ctx):
        if not any(tile_matches(g, n, x.p(0), pid) for n in g.grid.neighbors(idx)):
            return False
    improves_res = res_visible and T.resource_improved_by(g, t.resource, name)
    if _has(g, name, U.CanOnlyImproveResource, ctx) and not improves_res:
        return False
    if allowed_on_feature(g, name, last):
        return True
    cbo = _imp(g, name).get("terrainsCanBeBuiltOn", [])
    if T.is_land(g, idx) and "Land" in cbo:
        return True
    if T.is_water(g, idx) and "Water" in cbo:
        return True
    if _has(g, name, U.ImprovementBuildableByFreshWater, ctx) and any(T.fresh_water(g, n) for n in [idx]) \
            and T.fresh_water(g, idx):
        return True
    if improves_res and _domain_ok(g, idx, name):
        return True
    return False


def _domain_ok(g, idx, name) -> bool:
    """TileImprovementFunctions.extendedDomainCheck."""
    R = g.rules
    cbo = _imp(g, name).get("terrainsCanBeBuiltOn", [])
    if not cbo:
        return True
    types = set()
    for c in cbo:
        if c in ("Land", "Water"):
            types.add(c)
        td = R.terrains.get(c)
        if td is None:
            continue
        types.add("Water" if td["type"] == "Water" else "Land")
        for o in td.get("occursOn", []):
            if o in R.terrains:
                types.add("Water" if R.terrains[o]["type"] == "Water" else "Land")
    return (T.is_land(g, idx) and "Land" in types) or (T.is_water(g, idx) and "Water" in types)


def building_problems(g: "Game", pid: int, idx: int, name: str, unit: Optional["Unit"] = None,
                      ignore_tech: bool = False) -> list[str]:
    """ImprovementFunctions.getImprovementBuildingProblems, as readable reasons (empty = can build)."""
    R = g.rules
    d = _imp(g, name)
    ctx = Ctx(g, civ=pid, unit=unit, tile=idx)
    out = []
    from .uniques import civ_matches
    if d.get("uniqueTo") and not civ_matches(g, pid, d["uniqueTo"]):
        out.append(f"{name} is unique to {d['uniqueTo']}.")
    nation = g.player(pid).nation
    if any(R.improvements[u].get("replaces") == name for u in R.unique_improvements.get(nation, [])):
        out.append(f"Your civilization builds a replacement for {name}.")
    if d.get("techRequired") and not ignore_tech and not g.has_tech(pid, d["techRequired"]):
        out.append(f"{name} requires {d['techRequired']}.")
    unb = d["_umap"].get(U.Unbuildable)
    if any(not x.mods for x in unb) and name != REPAIR:
        out.append(f"{name} cannot be built.")
    elif any(applies(x, ctx) for x in unb if x.mods):
        out.append(f"{name} cannot be built right now.")
    if any(applies(x, ctx) for x in d["_umap"].get(U.Unavailable)):
        out.append(f"{name} is unavailable.")
    if any(not applies(x, ctx) for x in d["_umap"].get(U.OnlyAvailable)):
        out.append(f"{name} is not available yet ({'; '.join(m for x in d['_umap'].get(U.OnlyAvailable) for m in [x.text])}).")
    if any(g.has_tech(pid, x.p(0)) for x in _uniques(g, name, U.ObsoleteWith, ctx)):
        out.append(f"{name} is obsolete.")
    for x in _uniques(g, name, U.ConsumesResources, ctx):
        if g.resource_amount(pid, x.p(1)) < x.n(0):
            out.append(f"{name} needs {int(x.n(0))} {x.p(1)}.")
    t = g.s.tiles[idx]
    if t.owner != pid and not _has(g, name, U.CanBuildOutsideBorders, ctx) and (
            not _has(g, name, U.CanBuildJustOutsideBorders, ctx)
            or not any(g.s.tiles[n].owner == pid for n in g.grid.neighbors(idx))):
        out.append(f"{name} must be built inside your borders" +
                   (" or next to them." if _has(g, name, U.CanBuildJustOutsideBorders, ctx) else "."))
    known = [n for n in _feature_removals(g) if not R.improvements[n].get("techRequired")
             or g.has_tech(pid, R.improvements[n]["techRequired"])]
    if not _built_here_ok(g, idx, name, pid, ctx, known_removals=known):
        out.append(f"{name} cannot be built on this tile.")
    return out


def unit_can_build(g: "Game", u: "Unit", name: str, idx: Optional[int] = None) -> bool:
    """MapUnit.canBuildImprovement (the unit side)."""
    from .units import unit_uniques
    from .uniques import multi_filter
    idx = u.idx if idx is None else idx
    if g.is_barbarian(u.owner):
        return False
    t = g.s.tiles[idx]
    in_progress = t.build[0][0] if t.build else None
    d = _imp(g, name)
    if d.get("turnsToBuild") is None and name != CANCEL and in_progress != name:
        return False
    builds = unit_uniques(g, u, U.BuildImprovements)
    if in_progress == REPAIR or name == REPAIR:
        return bool(builds) and not T.is_enemy_territory(g, idx, u.owner)
    ctx = Ctx(g, civ=u.owner, unit=u, tile=idx)
    if any(not applies(x, ctx) for x in d["_umap"].get(U.OnlyAvailable)) or \
            any(applies(x, ctx) for x in d["_umap"].get(U.Unavailable)):
        return False
    return any(multi_filter(x.p(0), lambda f: improvement_matches(g.rules, name, f)
                            or tile_terrain_matches(g, idx, f, u.owner)) for x in builds)


def turns_to_build(g: "Game", u: "Unit", name: str) -> int:
    """TileImprovement.getTurnsToBuild."""
    from .units import unit_uniques
    d = _imp(g, name)
    base = d.get("turnsToBuild", 0) or 0
    speed = [x for x in unit_uniques(g, u, U.SpecificImprovementTime, with_civ=True)
             if improvement_matches(g.rules, name, x.p(1))]
    incr = [x for x in unit_uniques(g, u, U.ImprovementTimeIncrease, with_civ=True)
            if improvement_matches(g.rules, name, x.p(0))]
    increase = 1 + sum(x.n(1) for x in incr) / 100 if incr else 1.0
    if increase == 0:
        t = 0.0
    else:
        t = g.speed["improvementBuildLengthModifier"] * base / increase
    for x in speed:
        t *= 1 + x.n(0) / 100
    return max(1, int(math.floor(t + 0.5)))


def repair_turns(g: "Game", u: "Unit") -> int:
    """How long repairing a pillaged improvement takes."""
    t = g.s.tiles[u.idx]
    if t.build and t.build[0][0] == REPAIR:
        return t.build[0][1]
    rt = turns_to_build(g, u, REPAIR)
    target = t.improvement if t.pillaged and t.improvement else t.route
    return min(rt, turns_to_build(g, u, target)) if target in g.rules.improvements else rt


# ----------------------------------------------------------------------------
# Options and orders
# ----------------------------------------------------------------------------
def build_options(g: "Game", u: "Unit") -> list[dict]:
    """What this unit may start building on its tile (for views and tools)."""
    from .units import unit_uniques
    out = []
    t = g.s.tiles[u.idx]
    if unit_uniques(g, u, U.BuildImprovements) and not g.is_barbarian(u.owner):
        if (t.pillaged and t.improvement) or (t.route and t.route_pillaged):
            if not T.is_enemy_territory(g, u.idx, u.owner):
                out.append({"id": "repair", "name": REPAIR, "turns": repair_turns(g, u)})
        for name, d in g.rules.improvements.items():
            if name in (REPAIR, CANCEL):
                continue
            if not unit_can_build(g, u, name):
                continue
            probs = building_problems(g, u.owner, u.idx, name, u)
            if probs:
                continue
            opt = {"id": d["id"], "name": name, "turns": turns_to_build(g, u, name)}
            removes = _needed_removal(g, u.idx, name)
            if removes:
                opt["first_removes"] = removes
                opt["turns"] += turns_to_build(g, u, REMOVE + removes)
            if name in g.rules.improvements and t.improvement and not name.startswith(REMOVE) and name not in ROADS:
                opt["replaces"] = t.improvement
            out.append(opt)
    for x in _water_option(g, u):
        out.append(x)
    for x in great_improvement_options(g, u):
        out.append(x)
    return out


def _needed_removal(g, idx, name) -> Optional[str]:
    """A feature that must be removed before `name` can be built (ImprovementPicker queues the removal first)."""
    if name.startswith(REMOVE) or name in ROADS or _has(g, name, U.RemovesFeaturesIfBuilt):
        return None
    t = g.s.tiles[idx]
    for f in reversed(t.features):
        if REMOVE + f in g.rules.improvements and g.rules.terrains[f].get("unbuildable") \
                and not allowed_on_feature(g, name, f):
            return f
    return None


def start_build(g: "Game", u: "Unit", target: str) -> dict:
    """Order a worker to build something on its tile, queueing a feature removal if one is needed."""
    from .movement import is_civilian  # noqa: F401
    t = g.s.tiles[u.idx]
    if str(target).lower() in ("repair",):
        name = REPAIR
    elif str(target).lower() in ("cancel", "cancel improvement order"):
        t.build = None
        u.activity = None
        return {"cancelled": True}
    else:
        name = g.rules.resolve("improvement", target)
        if name is None:
            raise ActionError(f"Unknown improvement '{target}'.")
    opts = build_options(g, u)
    for o in opts:
        if o.get("instant") and o["name"] == name:
            return o["action"]()
    if name == REPAIR:
        if not any(o["name"] == REPAIR for o in opts):
            raise ActionError("There is nothing here this unit can repair.")
    else:
        if not unit_can_build(g, u, name):
            raise ActionError(f"A {u.type} cannot build {name}.")
        probs = building_problems(g, u.owner, u.idx, name, u)
        if probs:
            raise ActionError(" ".join(probs))
    if u.moves <= 0 and not (t.build and t.build[-1][0] == name):
        pass   # can still be ordered; work happens only if the unit ends a turn here with movement left
    current = t.build[-1][0] if t.build else None
    if current != name:
        queue = []
        rem = _needed_removal(g, u.idx, name) if name != REPAIR else None
        if rem:
            queue.append([REMOVE + rem, turns_to_build(g, u, REMOVE + rem)])
        queue.append([name, repair_turns(g, u) if name == REPAIR else turns_to_build(g, u, name)])
        t.build = queue
    u.activity = "build"
    u.goto = None
    total = sum(e[1] for e in t.build)
    return {"building": name, "turns": total, "queue": [e[0] for e in t.build]}


def _water_option(g, u) -> list[dict]:
    """The improvements a work boat could build here."""
    from .units import unit_uniques
    t = g.s.tiles[u.idx]
    if not T.is_water(g, u.idx) or not unit_uniques(g, u, U.CreateWaterImprovements) or not t.resource:
        return []
    for name in g.rules.improvements:
        if T.resource_improved_by(g, t.resource, name) and not building_problems(g, u.owner, u.idx, name, u):
            def act(name=name):
                """The action entry for one water improvement."""
                if u.moves <= 0:
                    raise ActionError("The unit has no movement left.")
                set_improvement(g, u.idx, name, u.owner, u)
                g.remove_unit(u)
                return {"created": name, "unit_consumed": True}
            return [{"id": _imp(g, name)["id"], "name": name, "turns": 0, "instant": True, "consumes_unit": True,
                     "action": act}]
    return []


def great_improvement_options(g: "Game", u: "Unit") -> list[dict]:
    """UnitActionsFromUniques.getImprovementConstructionActionsFromGeneralUnique."""
    from .units import usable_action_uniques, consume_action
    out = []
    for x in usable_action_uniques(g, u, U.ConstructImprovementInstantly):
        for name in g.rules.improvements:
            if not improvement_matches(g.rules, name, x.p(0)):
                continue
            probs = building_problems(g, u.owner, u.idx, name, u)
            if any("cannot be built." in p or "unique to" in p or "replacement" in p for p in probs):
                continue

            def act(name=name, x=x):
                """The action entry for one great-person improvement."""
                if u.moves <= 0:
                    raise ActionError("The unit has no movement left.")
                probs2 = building_problems(g, u.owner, u.idx, name, u)
                if probs2:
                    raise ActionError(" ".join(probs2))
                if g.s.tiles[u.idx].build and g.s.tiles[u.idx].build[0][1] < 0:
                    raise ActionError("This tile is reserved for a building's improvement.")
                idx, pid = u.idx, u.owner
                set_improvement(g, idx, name, pid, u)
                consume_action(g, u, x)
                return {"created": name}
            out.append({"id": _imp(g, name)["id"], "name": name, "turns": 0, "instant": True, "action": act,
                        "blocked": probs or None})
    return out


# ----------------------------------------------------------------------------
# Applying improvements
# ----------------------------------------------------------------------------
def set_improvement(g: "Game", idx: int, name: Optional[str], pid: Optional[int] = None, unit=None):
    """TileImprovementFunctions.setImprovement."""
    from . import triggers
    t = g.s.tiles[idx]
    if name is None:
        was_camp = t.improvement == "Barbarian encampment"
        t.improvement = None
        t.pillaged = False
        if was_camp:
            from . import city_states
            city_states.camp_removed(g, idx)
        _after_change(g, idx, pid)
        return
    if name.startswith(REMOVE):
        _activate_removal(g, idx, name, pid)
    elif name in ROADS:
        t.route = name
        t.route_pillaged = False
    elif name == REPAIR:
        t.build = None
        if t.pillaged:
            t.pillaged = False
        else:
            t.route_pillaged = False
    else:
        t.pillaged = False
        t.improvement = name
        if t.build:
            t.build = [e for e in t.build if e[0] not in ROADS]
    d = _imp(g, name)
    if d["_umap"].has_tag(U.RemovesFeaturesIfBuilt):
        t.features = [f for f in t.features if not (REMOVE + f in g.rules.improvements and not allowed_on_feature(g, name, f))]
        g.clear_static()
    if pid is not None:
        ctx = Ctx(g, civ=pid, unit=unit, tile=idx)
        for x in d["_umap"].all:
            if triggers.has_trigger_conditional(x) or not applies(x, ctx) or not triggers.is_triggerable(x):
                continue
            triggers.trigger(g, x, pid, unit=unit, tile=idx)
        triggers.fire(g, pid, U.TriggerUponBuildingImprovement, tile=idx, unit=unit,
                      filt=lambda m: improvement_matches(g.rules, name, m.p(0)))
    _after_change(g, idx, pid)


def _after_change(g, idx, pid):
    """Update everything affected by a tile's improvement changing."""
    from .cities import assign_citizens
    g.invalidate()
    t = g.s.tiles[idx]
    if t.city is not None and g.city(t.city) is not None:
        assign_citizens(g, g.city(t.city))
    from . import visibility
    for p in g.s.players:
        if p.kind == "major" and p.alive and (p.id == pid or idx in visibility.visible_tiles(g, p.id)):
            p.memory.pop(idx, None)


def _activate_removal(g, idx, name, pid):
    """Complete a feature removal, granting the production it yields."""
    t = g.s.tiles[idx]
    what = name[len(REMOVE):]
    R = g.rules
    if t.improvement and what in t.features and what in R.improvements[t.improvement].get("terrainsCanBeBuiltOn", []) \
            and t.terrain not in R.improvements[t.improvement].get("terrainsCanBeBuiltOn", []):
        t.improvement = None
        t.pillaged = False
    if what in ROADS:
        t.route = None
        t.route_pillaged = False
    elif t.improvement == what:
        t.improvement = None
    else:
        if pid is not None and what in R.terrains and R.terrains[what]["_umap"].has_tag(U.ProductionBonusWhenRemoved):
            _chop_bonus(g, idx, what, pid)
        if what in t.features:
            t.features = [f for f in t.features if f != what]
        if what == "Fallout":
            t.fallout = False
        g.clear_static()


def _chop_bonus(g, idx, feature, pid):
    """The production a city gets from chopping a forest nearby."""
    from .uniques import parse_stats
    from .cities import add_city_stat
    cs = g.player_cities(pid)
    if not cs:
        return
    city = min(cs, key=lambda c: g.grid.distance(c.idx, idx))
    dist = g.grid.distance(city.idx, idx)
    if dist > 5:
        return
    total: dict = {}
    for x in g.rules.terrains[feature]["_umap"].get(U.ProductionBonusWhenRemoved):
        st = parse_stats(x.p(0)) or {}
        mult = g.speed["modifier"] if x.has_mod("(modified by game speed)") else 1.0
        for k, v in st.items():
            total[k] = total.get(k, 0) + v * mult
    if dist != 1:
        total = {k: v * (6 - dist) / 4 for k, v in total.items()}
    t = g.s.tiles[idx]
    if t.city is None or g.city(t.city) is None or g.city(t.city).owner != pid:
        total = {k: v * 2 / 3 for k, v in total.items()}
    for k, v in total.items():
        add_city_stat(g, city, k, int(v))
    if total:
        g.emit("chop", f"Clearing a {feature} has created " +
               ", ".join(f"{int(v)} {k}" for k, v in total.items()) + f" for {city.name}.", [pid], idx=idx)


def progress_builds(g: "Game", pid: int):
    """UnitTurnManager.endTurn -> Tile.doWorkerTurn for every unit that ends its turn on a tile being improved."""
    for u in list(g.player_units(pid)):
        if g.unit(u.id) is None or u.moves <= 0:
            continue
        t = g.s.tiles[u.idx]
        if not t.build or t.build[0][1] < 0:
            if u.activity == "build" and not t.build:
                u.activity = None
            continue
        name = t.build[0][0]
        if not (unit_can_build(g, u, name) or (name.startswith(REMOVE) and unit_can_build(g, u, name))):
            continue
        t.build[0][1] = max(0, t.build[0][1] - 1)
        if t.build[0][1] > 0:
            continue
        t.build.pop(0)
        if not t.build:
            t.build = None
        # the improvement may have become impossible (e.g. borders changed) while the work was going on
        if name not in (REPAIR,) and not name.startswith(REMOVE) and name not in ROADS and \
                building_problems(g, pid, u.idx, name, u):
            g.emit("build_failed", f"{name} could not be completed.", [pid], idx=u.idx)
            continue
        set_improvement(g, u.idx, name, pid, u)
        g.emit("improvement_built", f"{g.player(pid).name} finished {name}.", [pid], idx=u.idx, improvement=name)
        if not t.build and u.activity == "build":
            u.activity = None


# ----------------------------------------------------------------------------
# Pillaging
# ----------------------------------------------------------------------------
def improvement_to_pillage(g: "Game", idx: int) -> Optional[str]:
    """What pillaging this tile would destroy, if anything."""
    t = g.s.tiles[idx]
    if t.improvement and not t.pillaged and not _has(g, t.improvement, U.Unpillagable) \
            and not _has(g, t.improvement, U.Irremovable) and g.city_at(idx) is None:
        return t.improvement
    if t.route and not t.route_pillaged:
        return t.route
    return None


def can_pillage(g: "Game", u: "Unit") -> Optional[str]:
    """Why this unit cannot pillage here, or None."""
    from .units import unit_has
    ud = g.rules.units[u.type]
    if not ud["_military"]:
        return "Civilian units cannot pillage."
    if u.carried_by is not None:
        return "Transported units cannot pillage."
    what = improvement_to_pillage(g, u.idx)
    if what is None:
        return "There is nothing here to pillage."
    owner = g.s.tiles[u.idx].owner
    if owner == u.owner:
        return "You cannot pillage your own tiles."
    if unit_has(g, u, U.CannotPillage, with_civ=True):
        return "This unit cannot pillage."
    if owner is not None and not g.at_war(u.owner, owner):
        return "You can only pillage tiles of civilizations you are at war with."
    if u.moves <= 0:
        return "The unit has no movement left."
    return None


def pillage(g: "Game", u: "Unit") -> dict:
    """Pillage the improvement or route on this tile, taking whatever it yields."""
    from .units import unit_uniques, unit_has, heal_by
    reason = can_pillage(g, u)
    if reason:
        raise ActionError(reason)
    idx = u.idx
    t = g.s.tiles[idx]
    name = improvement_to_pillage(g, idx)
    is_imp = name == t.improvement and not t.pillaged
    owner = t.owner
    if owner is not None:
        g.emit("pillaged", f"An enemy {u.type} has pillaged our {name}.", [owner], idx=idx)
    loot = _loot(g, u, idx, name)
    _set_pillaged(g, idx)
    if not unit_has(g, u, U.NoMovementToPillage, with_civ=True):
        u.moves = max(0, u.moves - g.rules.move_scale)
    healed = 0
    if is_imp:
        amount = 25.0
        for x in unit_uniques(g, u, U.PercentHealthFromPillaging, with_civ=True):
            amount *= 1 + x.n(0) / 100
        before = u.hp
        heal_by(g, u, int(amount))
        healed = u.hp - before
        if t.improvement and _has(g, t.improvement, U.DestroyedWhenPillaged):
            t.improvement = None
            t.pillaged = False
    g.invalidate()
    return {"pillaged": name, "loot": loot, "healed": healed}


def _set_pillaged(g, idx):
    """Tile.setPillaged: sea improvements are destroyed; others become pillaged (repairable)."""
    t = g.s.tiles[idx]
    can_imp = t.improvement and not t.pillaged and not _has(g, t.improvement, U.Unpillagable) \
        and not _has(g, t.improvement, U.Irremovable)
    if T.is_water(g, idx):
        t.improvement = None
        t.pillaged = False
    else:
        if t.build and t.build[0][1] >= 0:
            t.build = None
        if can_imp:
            t.pillaged = True
        else:
            t.route_pillaged = True
            g.clear_static()
    _after_change(g, idx, None)


def _loot(g, u, idx, name) -> dict:
    """The gold and other spoils from pillaging."""
    from .uniques import parse_stats, CIV_WIDE_STATS
    from .units import unit_uniques
    from .cities import add_city_stat
    if name not in g.rules.improvements:
        return {}
    rng = g.state_rng("pillage", g.turn, idx)
    total: dict = {}
    for x in _imp(g, name)["_umap"].get(U.PillageYieldRandom):
        for k, v in (parse_stats(x.p(0)) or {}).items():
            amt = rng.randrange(int(v) + 1) + rng.randrange(int(v) + 1)
            if x.has_mod("(modified by game speed)"):
                amt *= g.speed["modifier"]
            total[k] = total.get(k, 0) + amt
    for x in _imp(g, name)["_umap"].get(U.PillageYieldFixed):
        for k, v in (parse_stats(x.p(0)) or {}).items():
            total[k] = total.get(k, 0) + v * (g.speed["modifier"] if x.has_mod("(modified by game speed)") else 1)
    for x in unit_uniques(g, u, U.PercentYieldFromPillaging, with_civ=True):
        total = {k: v * (1 + x.n(0) / 100) for k, v in total.items()}
    if not total:
        return {}
    cs = g.player_cities(u.owner)
    city = min(cs, key=lambda c: g.grid.distance(c.idx, idx)) if cs else None
    out = {}
    for k, v in total.items():
        if k in CIV_WIDE_STATS:
            g.add_stat(u.owner, k, int(v))
            out[k] = int(v)
        elif city is not None:
            add_city_stat(g, city, k, int(v))
            out[k] = int(v)
    if out:
        g.emit("loot", f"We have looted {', '.join(f'{v} {k}' for k, v in out.items())} from a {name}.", [u.owner],
               idx=idx)
    return out

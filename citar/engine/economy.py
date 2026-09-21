"""Civilization-level rules: which uniques apply to a civ, resources, happiness, unit upkeep and supply, and the
civ's stats for next turn. Port of UnCiv's Civilization.getMatchingUniques, CivInfoTransientCache.updateCivResources
and CivInfoStatsForNextTurn (MPL-2.0)."""
from __future__ import annotations

import math
from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .uniques import Unique, UniqueMap, Ctx, applies, city_matches, resource_matches, tile_matches, STAT_KEY

if TYPE_CHECKING:
    from .game import Game

STATS = ("food", "production", "gold", "science", "culture", "happiness", "faith")
_TEMP_CACHE: dict[str, Unique] = {}


# ---------------------------------------------------------------------------------------------------------------
# Unique sources
# ---------------------------------------------------------------------------------------------------------------
def is_humanlike(g: "Game", pid: int) -> bool:
    """UnCiv's isHuman(): difficulty applies to these seats; scripted bots get the AI modifiers instead."""
    return g.player(pid).controller in ("human", "llm", "mcp")


def seat_difficulty(g: "Game", pid: Optional[int] = None) -> dict:
    """The difficulty chosen for this seat, which plays the role of UnCiv's game difficulty for that civ: humans get
    its player values, AIs its AI bonuses. Seats without their own setting use the game difficulty."""
    name = None
    if pid is not None:
        name = g.player(pid).difficulty
    return g.rules.difficulty(name or g.s.config.get("difficulty"))


def difficulty(g: "Game", pid: Optional[int] = None) -> dict:
    """The civ's effective difficulty (UnCiv Civilization.getDifficulty): humanlike seats use their seat difficulty,
    AI civs use its aiDifficultyLevel (a Deity AI plays on Chieftain's base values plus Deity's AI bonuses)."""
    base = seat_difficulty(g, pid)
    if pid is None or is_humanlike(g, pid):
        return base
    level = base.get("aiDifficultyLevel") or base["name"]
    if g.s.config.get("ai_base_values") == "monotonic":
        # CITAR option: UnCiv gives every non-Prince AI Chieftain's lenient base values, so a Chieftain AI can
        # out-expand a Prince AI. Here the easier AIs play on Prince's base values plus their own penalties.
        prince = g.rules.difficulty_index("Prince")
        if g.rules.difficulty_index(base["name"]) < prince:
            level = "Prince"
    return g.rules.difficulty(level)


def game_difficulty(g: "Game") -> dict:
    """The game's difficulty definition."""
    return g.rules.difficulty(g.s.config.get("difficulty"))


def barbarian_difficulty(g: "Game") -> dict:
    """Barbarian strength (UnCiv reads these from the game difficulty): players' combat bonus against barbarians,
    camp spawn delay, and the turn from which barbarians may enter civilizations' territory."""
    return g.rules.difficulty(g.s.config.get("barbarian_difficulty") or g.s.config.get("difficulty"))


def temp_unique(text: str) -> Unique:
    """Parse a unique from text and cache it, for rules constructed at runtime."""
    u = _TEMP_CACHE.get(text)
    if u is None:
        u = Unique(text, "Temporary", "Temporary")
        # strip the timing conditional: it has been consumed when the unique was granted
        u.mods = [m for m in u.mods if m.ph != "for [] turns"]
        u.mod_phs = {m.ph for m in u.mods}
        u.timed = False
        _TEMP_CACHE[text] = u
    return u


def civ_umaps(g: "Game", pid: int) -> list[UniqueMap]:
    """Every UniqueMap whose uniques apply civ-wide (Civilization.getMatchingUniques)."""
    key = ("civ_umaps", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    v = list(civ_umaps_no_resources(g, pid))
    rm = resource_umap(g, pid)
    if rm is not None:
        v.insert(len(v) - 1, rm)
    g._ycache[key] = v
    return v


def civ_umaps_no_resources(g: "Game", pid: int) -> list[UniqueMap]:
    """civ_umaps without the uniques of owned resources (those depend on the resource supply itself)."""
    key = ("civ_umaps_nores", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    R = g.rules
    p = g.player(pid)
    maps: list[UniqueMap] = []
    nd = R.nations.get(p.nation)
    if nd:
        maps.append(nd["_umap"])
    # non-local building uniques of every city
    nonlocal_units = []
    for c in g.player_cities(pid):
        for b in c.buildings:
            for u in R.buildings[b]["_umap"].all:
                if not u.is_local:
                    nonlocal_units.append(u)
    if nonlocal_units:
        maps.append(UniqueMap(nonlocal_units))
    for pol in p.policies:
        d = R.policies.get(pol) or R.policy_branches.get(pol)
        if d:
            maps.append(d["_umap"])
    techs = [R.techs[t]["_umap"] for t in p.techs if R.techs[t]["_umap"].all]
    maps.extend(techs)
    if p.temp_uniques:
        maps.append(UniqueMap(temp_unique(t["text"]) for t in p.temp_uniques))
    from .research import player_era
    maps.append(R.eras[R.era_list[player_era(g, pid)]]["_umap"])
    if p.kind == "major":
        from . import city_states
        maps.extend(city_states.bonus_umaps(g, pid))
    if p.religion and p.religion in g.s.religions:
        from . import religion
        fm = religion.founder_umap(g, p.religion)
        if fm is not None:
            maps.append(fm)
    maps.append(R.global_uniques)
    g._ycache[key] = maps
    return maps


def civ_index(g: "Game", pid: int) -> dict:
    """All civ-wide uniques merged into one placeholder index (in civ_umaps order), so a lookup is one dict access
    instead of one per tech, policy, building... Cached with civ_umaps."""
    key = ("civ_index", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    v = {}
    for m in civ_umaps(g, pid):
        for ph, lst in m.by_ph.items():
            v.setdefault(ph, []).extend(lst)
    g._ycache[key] = v
    return v


def civ_uniques(g: "Game", pid: Optional[int], ph: str, ctx: Optional[Ctx] = None) -> list[Unique]:
    """Every civilization-wide unique matching a placeholder.

    The sum of a civilization's nation, policies, beliefs, wonders, city-state bonuses and anything
    else that applies empire-wide. Cached, because almost every calculation in the game asks.
    """
    if pid is None:
        return []
    lst = civ_index(g, pid).get(ph)
    if not lst:
        return []
    if ctx is None:
        ctx = Ctx(g, civ=pid)
    return [u for u in lst if not u.timed and applies(u, ctx)]


def civ_uniques_raw(g: "Game", pid: int, ph: str) -> list[Unique]:
    """Ignoring conditionals."""
    return list(civ_index(g, pid).get(ph, ()))


def civ_has(g: "Game", pid: Optional[int], ph: str, ctx: Optional[Ctx] = None) -> bool:
    """Whether any civilization-wide unique with this placeholder applies."""
    if pid is None:
        return False
    lst = civ_index(g, pid).get(ph)
    if not lst:
        return False
    if ctx is None:
        ctx = Ctx(g, civ=pid)
    return any(not u.timed and applies(u, ctx) for u in lst)


# ---------------------------------------------------------------------------------------------------------------
# Resources
# ---------------------------------------------------------------------------------------------------------------
def owned_tiles(g: "Game", pid: int) -> list[int]:
    """Every tile a civilization owns, cached."""
    key = ("owned", pid)
    v = g._ycache.get(key)
    if v is None:
        v = [i for i, t in enumerate(g.s.tiles) if t.owner == pid]
        g._ycache[key] = v
    return v


def tile_provides_resource(g: "Game", idx: int, pid: int) -> bool:
    """Whether this tile actually supplies its resource - improved, unpillaged and connected."""
    from . import tiles as T
    t = g.s.tiles[idx]
    res = t.resource
    if not res or not T.resource_visible(g, pid, res):
        return False
    rd = g.rules.resources[res]
    if g.city_at(idx) is not None:
        imps = [rd.get("improvement")] + list(rd.get("improvedBy") or [])
        imps = [i for i in imps if i]
        if not imps:
            return True
        for i in imps:
            idef = g.rules.improvements.get(i)
            if idef and idef.get("turnsToBuild", 0) != -1 and g.has_tech(pid, idef.get("techRequired")):
                return True
        return False
    imp = T.unpillaged_improvement(t)
    if imp is None:
        return False
    if T.resource_improved_by(g, res, imp):
        return True
    return rd["resourceType"] == "Strategic" and g.rules.improvements[imp]["_great"]


def resource_modifiers(g: "Game", city) -> dict:
    """Per-resource quantity modifiers applying in a city."""
    from .cities import local_uniques
    mods: dict = {}
    ctx = Ctx(g, city=city)
    for u in local_uniques(g, city, U.PercentResourceProduction, ctx) + _civ_uniques_nores(g, city.owner, U.PercentResourceProduction):
        bonus = u.n(0) / 100
        for r in g.rules.resources:
            if resource_matches(g.rules, r, u.p(1)):
                mods[r] = mods.get(r, 1.0) + bonus
    return mods


def city_resources(g: "Game", city) -> list[tuple[str, str, int]]:
    """(resource, origin, amount) generated (or consumed) by a city (CityResources.getResourcesGeneratedByCity)."""
    key = ("city_res", city.id)
    v = g._ycache.get(key)
    if v is not None:
        return v
    R = g.rules
    pid = city.owner
    from .cities import city_tiles, is_free_building
    mods = resource_modifiers(g, city)
    tile_amounts: dict = {}
    extra_lux = any(R.buildings[b]["_umap"].has_tag(U.ProvidesExtraLuxuryFromCityResources) for b in city.buildings)
    for i in city_tiles(g, city):
        t = g.s.tiles[i]
        if not t.resource or not tile_provides_resource(g, i, pid):
            continue
        rd = R.resources[t.resource]
        amt = t.resource_amount if rd["resourceType"] == "Strategic" else 1
        if rd["resourceType"] == "Luxury" and extra_lux:
            amt += 1
        if amt > 0:
            tile_amounts[t.resource] = tile_amounts.get(t.resource, 0) + amt
    out = []
    for r, a in tile_amounts.items():
        out.append((r, "Tiles", int(a * mods.get(r, 1.0))))
    for b in city.buildings:
        if is_free_building(g, city, b):
            continue
        rr = R.buildings[b].get("requiredResource")
        if rr:
            out.append((rr, "Buildings", -1))
    p = g.player(pid)
    if p.kind == "city_state" and p.capital == city.id and p.cs_resource:
        out.append((p.cs_resource, "Mercantile City-State", 1))
    # "Provides [n] [resource]" from buildings in this city
    ctx = Ctx(g, city=city)
    for b in city.buildings:
        for u in R.buildings[b]["_umap"].matching(U.ProvidesResources, ctx):
            if u.p(1) in R.resources:
                out.append((u.p(1), b, int(u.n(0) * mods.get(u.p(1), 1.0))))
    g._ycache[key] = out
    return out


def detailed_resources(g: "Game", pid: int) -> list[tuple[str, str, int]]:
    """Civ-wide resource supply entries (CivInfoTransientCache.updateCivResources)."""
    key = ("civ_res", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    R = g.rules
    p = g.player(pid)
    out: list = []
    for c in g.player_cities(pid):
        out.extend(city_resources(g, c))
    if p.kind == "major":
        from . import city_states
        pct = 1.0
        for u in _civ_uniques_nores(g, pid, U.CityStateResources):
            pct += u.n(0) / 100
        for cs in city_states.allied_city_states(g, pid):
            for r, _, a in city_states.resources_for_ally(g, cs):
                out.append((r, "City-States", int(a * pct)))
    for u in _civ_uniques_nores(g, pid, U.ProvidesResources):
        if u.src_type != "Building" and u.p(1) in R.resources:
            out.append((u.p(1), u.src_name or "Uniques", int(u.n(0))))
    from .diplomacy import deal_resource_flows
    for r, amt in deal_resource_flows(g, pid):
        out.append((r, "Trade", amt))
    for unit in g.player_units(pid):
        ud = R.units[unit.type]
        if ud.get("requiredResource"):
            out.append((ud["requiredResource"], "Units", -1))
        for u in ud["_umap"].get(U.ConsumesResources):
            out.append((u.p(1), "Units", -int(u.n(0))))
    g._ycache[key] = out
    return out


def _civ_uniques_nores(g, pid, ph):
    """civ uniques without the resource-unique map (avoids recursion while computing resources)."""
    ctx = Ctx(g, civ=pid)
    out = []
    for m in civ_umaps_no_resources(g, pid):
        out.extend(m.matching(ph, ctx))
    return out


def resource_umap(g: "Game", pid: int) -> Optional[UniqueMap]:
    """The uniques affecting a civilization's resources."""
    key = ("resource_umap", pid)
    if key in g._ycache:
        return g._ycache[key]
    g._ycache[key] = None        # guards against re-entry while the supply is computed
    supply = resource_supply(g, pid)
    us = []
    for r, a in supply.items():
        if a > 0:
            us.extend(g.rules.resources[r]["_umap"].all)
    m = UniqueMap(us) if us else None
    g._ycache[key] = m
    return m


def resource_supply(g: "Game", pid: int) -> dict:
    """How much of every resource a civilization has available, cached.

    Net of what units and buildings consume and what deals give away, which is why a unit can be
    refused for want of iron the civilization appears to own.
    """
    key = ("res_supply", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    v = {}
    for r, _, a in detailed_resources(g, pid):
        v[r] = v.get(r, 0) + a
    g._ycache[key] = v
    return v


def resource_amount(g: "Game", pid: int, res: str) -> int:
    """How much of one resource is available."""
    return resource_supply(g, pid).get(res, 0)


def strategic_resources(g: "Game", pid: int) -> dict:
    """{resource: {"sources", "used", "imported", "exported", "available"}} for display."""
    R = g.rules
    out = {}
    for r, d in R.resources.items():
        if d["resourceType"] == "Strategic":
            out[r] = {"sources": 0, "used": 0, "imported": 0, "exported": 0, "available": 0}
    for r, origin, a in detailed_resources(g, pid):
        if r not in out:
            continue
        if origin == "Units" or origin == "Buildings":
            out[r]["used"] -= a
        elif origin == "Trade":
            out[r]["imported" if a > 0 else "exported"] += abs(a)
        else:
            out[r]["sources"] += a
    for r, e in out.items():
        e["available"] = e["sources"] + e["imported"] - e["exported"] - e["used"]
    return out


def luxury_resources(g: "Game", pid: int) -> dict:
    """The luxuries a civilization has, which is what happiness is built on."""
    R = g.rules
    out = {}
    for r, d in R.resources.items():
        if d["resourceType"] == "Luxury":
            out[r] = {"owned": 0, "imported": 0, "exported": 0, "net": 0}
    for r, origin, a in detailed_resources(g, pid):
        if r not in out:
            continue
        if origin == "Trade":
            out[r]["imported" if a > 0 else "exported"] += abs(a)
        else:
            out[r]["owned"] += a
    for e in out.values():
        e["net"] = e["owned"] + e["imported"] - e["exported"]
    return out


# ---------------------------------------------------------------------------------------------------------------
# Happiness
# ---------------------------------------------------------------------------------------------------------------
def happiness_for_conditionals(g: "Game", pid: int) -> float:
    """Empire happiness as seen by conditionals ("while the empire is happy", "when above [n] [Happiness]").
    Uses the current value when it is cached, otherwise the last fully computed value, as UnCiv's
    civInfo.getHappiness() returns the stored stat (refreshed at stat updates). Recomputing it re-entrantly for every city was the engine's
    single biggest cost in large empires."""
    cached = g._ycache.get(("happiness", pid))
    if cached is not None and pid not in g._hap_busy:
        return cached["total"]
    if pid in g._last_hap:
        return g._last_hap[pid]
    if pid in g._hap_busy:
        return 0
    return happiness(g, pid)["total"]


def happiness(g: "Game", pid: int) -> dict:
    """Happiness breakdown (CivInfoStatsForNextTurn.getHappinessBreakdown). 'total' is the empire happiness."""
    key = ("happiness", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    g._hap_busy.add(pid)
    try:
        v = _happiness(g, pid)
    finally:
        g._hap_busy.discard(pid)
    if v.get("_partial"):
        v.pop("_partial")
    else:
        g._last_hap[pid] = v["total"]
    return v


def _happiness(g: "Game", pid: int) -> dict:
    """Compute empire happiness from every city and every civilization-wide source.

    The number that limits everything: growth, expansion, golden ages and, in the scripted bot's case,
    the whole game. Cached per turn because it is consulted constantly and is expensive to derive.
    """
    key = ("happiness", pid)
    # placeholder while computing (conditionals may ask for happiness recursively)
    g._ycache[key] = {"total": 0, "breakdown": {}, "status": "content"}
    R = g.rules
    p = g.player(pid)
    bd: dict = {}
    if p.kind != "major":
        v = {"total": 0, "breakdown": {}, "status": "content"}
        g._ycache[key] = v
        return v
    diff = difficulty(g, pid)
    bd["Base happiness"] = float(diff["baseHappiness"])
    per_lux = 4 + diff["extraHappinessPerLuxury"]
    for u in _civ_uniques_nores(g, pid, U.BonusHappinessFromLuxury):
        per_lux += u.n(0)
    supply = resource_supply(g, pid)
    owned_lux = [r for r, a in supply.items() if a > 0 and R.resources[r]["resourceType"] == "Luxury"]
    bd["Luxury resources"] = len(owned_lux) * per_lux
    from . import city_states
    bonus = sum(u.n(0) for u in _civ_uniques_nores(g, pid, U.CityStateLuxuryHappiness)) / 100
    if bonus:
        cs_lux = set()
        for cs in city_states.allied_city_states(g, pid):
            for r, _, a in city_states.resources_for_ally(g, cs):
                if a > 0 and R.resources[r]["resourceType"] == "Luxury" and r in owned_lux:
                    cs_lux.add(r)
        bd["City-State Luxuries"] = per_lux * len(cs_lux) * bonus
    retain = sum(u.n(0) for u in _civ_uniques_nores(g, pid, U.RetainHappinessFromLuxury)) / 100
    if retain:
        traded_away = {r for r, origin, a in detailed_resources(g, pid) if origin == "Trade" and a < 0
                       and R.resources[r]["resourceType"] == "Luxury" and r not in owned_lux}
        bd["Traded Luxuries"] = len(traded_away) * per_lux * retain
    from .cities import city_happiness
    partial = False
    for c in g.player_cities(pid):
        hl = city_happiness(g, c)
        if not hl and g.player(pid).kind == "major":
            partial = True       # re-entered while this city's happiness is being computed: don't cache
        for k, val in hl.items():
            bd[k] = bd.get(k, 0.0) + val
    up = transport_upkeep(g, pid)
    if up.get("happiness"):
        bd["Transportation Upkeep"] = -up["happiness"]
    for k, s in global_stats_from_uniques(g, pid).items():
        if s.get("happiness"):
            bd[k] = bd.get(k, 0.0) + s["happiness"]
    total = sum(bd.values())
    total_i = int(round(total)) if abs(total - round(total)) < 1e-6 else int(math.floor(total))
    v = {"total": total_i, "breakdown": {k: round(x, 2) for k, x in bd.items() if x},
         "luxury_types": sorted(owned_lux),
         "status": "very unhappy" if total_i < -10 else ("unhappy" if total_i < 0 else "content")}
    if partial:
        g._ycache.pop(key, None)
        v["_partial"] = True
    else:
        g._ycache[key] = v
    return v


# ---------------------------------------------------------------------------------------------------------------
# Upkeep & supply
# ---------------------------------------------------------------------------------------------------------------
def unit_maintenance(g: "Game", pid: int) -> int:
    """Gold spent on unit upkeep, after the free allowance."""
    free = 3
    for u in civ_uniques(g, pid, U.FreeUnits):
        free += int(u.n(0))
    units = g.player_units(pid)
    from .units import can_garrison
    if civ_has(g, pid, U.UnitsInCitiesNoMaintenance):
        units = [u for u in units if not (g.city_at(u.idx) is not None and can_garrison(g, u))]
    civwide = civ_uniques_raw(g, pid, U.UnitMaintenanceDiscount)
    costs = []
    R = g.rules
    for unit in units:
        ctx = Ctx(g, unit=unit)
        m = 1.0
        for u in R.units[unit.type]["_umap"].matching(U.UnitMaintenanceDiscount, ctx):
            m *= 1 + u.n(0) / 100
        for pr in unit.promotions:
            for u in R.promotions[pr]["_umap"].matching(U.UnitMaintenanceDiscount, ctx):
                m *= 1 + u.n(0) / 100
        from .uniques import applies
        for u in civwide:
            if applies(u, ctx):
                m *= 1 + u.n(0) / 100
        costs.append(m)
    costs.sort(reverse=True)
    to_pay = max(0.0, sum(costs[free:]))
    turn_limit = g.total_turns()
    progress = min(g.s.turn / turn_limit, 1.0)
    cost = 0.5 * to_pay * (1 + progress)
    cost = cost ** (1 + progress / 3) if cost > 0 else 0.0
    if not is_humanlike(g, pid) and g.player(pid).kind == "major":
        cost *= seat_difficulty(g, pid)["aiUnitMaintenanceModifier"]
    return int(cost)


def transport_upkeep(g: "Game", pid: int) -> dict:
    """Upkeep for roads and railways."""
    key = ("transport", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    from . import tiles as T
    R = g.rules
    out: dict = {}
    ignored = [u.p(0) for u in civ_uniques(g, pid, U.NoImprovementMaintenanceInSpecificTiles)]
    for i in owned_tiles(g, pid):
        t = g.s.tiles[i]
        road = T.unpillaged_route(t)
        if road is None or g.city_at(i) is not None:
            continue
        if any(tile_matches(g, i, f, pid) for f in ignored):
            continue
        ctx = Ctx(g, civ=pid, tile=i)
        for ph in (U.ImprovementMaintenance, U.ImprovementAllMaintenance):
            for u in R.improvements[road]["_umap"].matching(ph, ctx):
                k = STAT_KEY.get(u.p(1), u.p(1).lower())
                out[k] = out.get(k, 0.0) + u.n(0)
    for u in civ_uniques(g, pid, U.RoadMaintenance):
        for k in out:
            out[k] *= 1 + u.n(0) / 100
    g._ycache[key] = out
    return out


def unit_supply(g: "Game", pid: int) -> int:
    """How many units a civilization can support before penalties."""
    diff = difficulty(g, pid)
    base = diff["unitSupplyBase"] + sum(int(u.n(0)) for u in civ_uniques(g, pid, U.BaseUnitSupply))
    cities = g.player_cities(pid)
    per_city = diff["unitSupplyPerCity"] + sum(int(u.n(0)) for u in civ_uniques(g, pid, U.UnitSupplyPerCity))
    from_cities = len(cities) * per_city
    from_pop = sum(c.pop for c in cities) * g.rules.k["unit_supply_per_population"]
    for u in civ_uniques(g, pid, U.UnitSupplyPerPop):
        from_pop += u.n(0) * sum(c.pop // int(u.n(1)) for c in cities if city_matches(g, c, u.p(2)))
    supply = base + from_cities + int(from_pop)
    p = g.player(pid)
    if p.kind == "major" and not is_humanlike(g, pid):
        supply = int(supply * (1 + seat_difficulty(g, pid)["aiUnitSupplyModifier"]))
    return supply


def unit_supply_deficit(g: "Game", pid: int) -> int:
    """How far over its supply limit a civilization is."""
    key = ("supply_deficit", pid)
    v = g._ycache.get(key)
    if v is None:
        v = max(0, len(g.player_units(pid)) - unit_supply(g, pid))
        g._ycache[key] = v
    return v


def unit_supply_penalty(g: "Game", pid: int) -> float:
    """The production penalty for exceeding unit supply, capped at -70%."""
    return -min(unit_supply_deficit(g, pid) * 10.0, 70.0)


# ---------------------------------------------------------------------------------------------------------------
# Civ stats for next turn
# ---------------------------------------------------------------------------------------------------------------
def global_stats_from_uniques(g: "Game", pid: int) -> dict:
    """Empire-wide yields from uniques, kept per source for the breakdown."""
    key = ("global_stats", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    p = g.player(pid)
    out: dict = {}

    def add(src, stats, mult=1.0):
        """Accumulate one source's yields."""
        d = out.setdefault(src, {})
        for k, x in stats.items():
            d[k] = d.get(k, 0.0) + x * mult

    from . import religion
    if p.religion and religion.is_major(g, p.religion):
        fm = religion.founder_umap(g, p.religion)
        ctx = Ctx(g, civ=pid)
        if fm is not None:
            for u in fm.matching(U.StatsFromGlobalCitiesFollowingReligion, ctx):
                add("Religion", u.stats, religion.cities_following(g, p.religion))
            for u in fm.matching(U.StatsFromGlobalFollowers, ctx):
                add("Religion", u.stats, religion.followers_of(g, p.religion, u.p(2), pid) / u.n(1))
    for u in civ_uniques(g, pid, U.StatsPerPolicies):
        n = sum(1 for x in p.policies if not x.endswith(" Complete")) // int(u.n(1))
        add("Policies", u.stats, n)
    for u in civ_uniques(g, pid, U.Stats):
        if u.src_type != "Building":
            src = "City-States" if u.src_type == "CityState" else (u.src_type or "Uniques")
            add(src, u.stats)
    nw = {"happiness": 1.0}
    for u in civ_uniques(g, pid, U.StatsFromNaturalWonders):
        for k, x in u.stats.items():
            nw[k] = nw.get(k, 0.0) + x
    if p.natural_wonders:
        add("Natural Wonders", nw, len(p.natural_wonders))
    if "City-States" in out:
        for u in civ_uniques(g, pid, U.BonusStatsFromCityStates):
            k = STAT_KEY.get(u.p(1), u.p(1).lower())
            if k in out["City-States"]:
                out["City-States"][k] *= 1 + u.n(0) / 100
    g._ycache[key] = out
    return out


def stat_map(g: "Game", pid: int) -> dict:
    """Source -> stats for next turn (getStatMapForNextTurn)."""
    key = ("stat_map", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    from .cities import city_stats
    out: dict = {}

    def add(src, stats):
        """Accumulate one source's yields."""
        d = out.setdefault(src, {})
        for k, x in stats.items():
            if k in STATS:
                d[k] = d.get(k, 0.0) + x

    for c in g.player_cities(pid):
        for src, s in city_stats(g, c)["final"].items():
            add(src, s)
    from . import city_states
    for cs in city_states.allied_city_states(g, pid):
        for u in civ_uniques(g, pid, U.CityStateStatPercent):
            k = STAT_KEY.get(u.p(0), u.p(0).lower())
            cs_stats = civ_stats(g, cs)
            add("City-States", {k: cs_stats.get(k, 0.0) * u.n(1) / 100})
    up = transport_upkeep(g, pid)
    add("Transportation upkeep", {k: -x for k, x in up.items()})
    add("Unit upkeep", {"gold": -unit_maintenance(g, pid)})
    hap = happiness(g, pid)["total"]
    if hap > 0:
        for u in civ_uniques(g, pid, U.ExcessHappinessToGlobalStat):
            add("Policies", {STAT_KEY.get(u.p(1), u.p(1).lower()): u.n(0) / 100 * hap})
    gold = sum(s.get("gold", 0) for s in out.values())
    if gold < 0 and g.player(pid).gold < 0:
        sci = sum(s.get("science", 0) for s in out.values())
        add("Treasury deficit", {"science": max(gold, 1 - sci)})
    from .diplomacy import deal_gold_per_turn
    gpt = deal_gold_per_turn(g, pid)
    if gpt:
        add("Trade", {"gold": gpt})
    for src, s in global_stats_from_uniques(g, pid).items():
        add(src, s)
    g._ycache[key] = out
    return out


def civ_stats(g: "Game", pid: int) -> dict:
    """Total stats for next turn (statsForNextTurn)."""
    key = ("civ_stats", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    v = dict.fromkeys(STATS, 0.0)
    for s in stat_map(g, pid).values():
        for k, x in s.items():
            v[k] += x
    v["happiness"] = happiness(g, pid)["total"]
    g._ycache[key] = v
    return v


def empire_yields(g: "Game", pid: int) -> dict:
    """The civilization's yields per turn, rounded for display."""
    return {k: round(x, 1) for k, x in civ_stats(g, pid).items()}


def gold_per_turn(g: "Game", pid: int) -> dict:
    """Net gold per turn: income, maintenance and active deals."""
    sm = stat_map(g, pid)
    cities = sum(s.get("gold", 0) for k, s in sm.items() if k not in ("Unit upkeep", "Transportation upkeep", "Trade", "Maintenance"))
    return {
        "income": round(cities, 1),
        "building_maintenance": round(sum(s.get("gold", 0) for k, s in sm.items() if k == "Maintenance"), 1),
        "unit_upkeep": round(sm.get("Unit upkeep", {}).get("gold", 0), 1),
        "route_maintenance": round(sm.get("Transportation upkeep", {}).get("gold", 0), 1),
        "trade": round(sm.get("Trade", {}).get("gold", 0), 1),
        "net": round(civ_stats(g, pid)["gold"], 1),
    }


def process_gold(g: "Game", pid: int, gold: float):
    """TurnManager.endTurn: while the treasury is at -200 or below and income is negative, military units are
    disbanded (those in our territory and with the fewest promotions first); then the income is added."""
    p = g.player(pid)
    while p.gold <= -200 and gold < 0:
        mil = [u for u in g.player_units(pid) if g.rules.units[u.type]["_military"]]
        if not mil:
            break
        victim = min(mil, key=lambda u: (g.s.tiles[u.idx].owner != pid, len(u.promotions) * 10 + u.xp))
        name = victim.type
        from .units import disband
        disband(g, victim)
        g.emit("bankrupt", f"Cannot provide unit upkeep for {name} - unit has been disbanded!", [pid], idx=victim.idx)
        g.invalidate()
        gold = civ_stats(g, pid)["gold"]
    p.gold += int(gold)

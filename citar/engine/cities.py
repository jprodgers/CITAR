"""Cities: uniques, stats, population & specialists, borders, constructions, founding, capital connections.

Port of UnCiv's City, CityStats, CityPopulationManager, CityExpansionManager, CityConstructions, Building/BaseUnit
cost & rejection rules, CityFounder and CapitalConnectionsFinder (MPL-2.0).
"""
from __future__ import annotations

import math
import re
from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .state import City, BOT_MANAGED
from .uniques import (Ctx, Unique, UniqueMap, applies, city_matches, building_matches, base_unit_matches,
                      tile_matches, tile_terrain_matches, STAT_KEY)
from . import tiles as T

if TYPE_CHECKING:
    from .game import Game

STATS = ("food", "production", "gold", "science", "culture", "happiness", "faith")
QUEUE_MAX = 10
PERPETUAL = {"Gold": "gold", "Science": "science", "Nothing": None}
FOCUSES = {
    "balanced": {}, "manual": {},
    "food": {"food": 3.05}, "production": {"production": 3.05}, "gold": {"gold": 3.05},
    "science": {"science": 3.05}, "culture": {"culture": 3.05}, "faith": {"faith": 3.05},
    "happiness": {"happiness": 3.05},
    "gold_growth": {"gold": 2.0, "food": 1.5}, "production_growth": {"production": 2.0, "food": 1.5},
}


def zero() -> dict:
    """A fresh stat dictionary with every yield at zero."""
    return dict.fromkeys(STATS, 0.0)


def add_into(a: dict, b: dict, mult: float = 1.0):
    """Add *b* into *a* in place, optionally scaled. The accumulator used by every stat calculation."""
    for k, v in b.items():
        a[k] = a.get(k, 0.0) + v * mult


# ---------------------------------------------------------------------------------------------------------------
# Uniques
# ---------------------------------------------------------------------------------------------------------------
def local_umaps(g: "Game", city: City) -> list[UniqueMap]:
    """The unique maps that apply only inside this city: its buildings, plus its religion.

    Cached on the game's yield cache because it is consulted for every stat of every tile, and rebuilding
    it per tile turns a city recalculation into a noticeable pause.
    """
    key = ("local_umaps", city.id)
    v = g._ycache.get(key)
    if v is not None:
        return v
    R = g.rules
    us = [u for b in city.buildings for u in R.buildings[b]["_umap"].all if u.is_local]
    v = [UniqueMap(us)] if us else []
    from . import religion
    rm = religion.city_follower_umap(g, city)
    if rm is not None:
        v.append(rm)
    g._ycache[key] = v
    return v


def local_uniques(g: "Game", city: City, ph: str, ctx: Optional[Ctx] = None) -> list[Unique]:
    """Uniques of one placeholder text that apply locally in this city."""
    if ctx is None:
        ctx = city_ctx(g, city)
    out = []
    for m in local_umaps(g, city):
        if ph in m.by_ph:
            out.extend(m.matching(ph, ctx))
    return out


def city_ctx(g: "Game", city: City) -> Ctx:
    """The context object a unique is evaluated against for this city."""
    return Ctx(g, civ=city.owner, city=city, tile=city.idx)


def city_uniques(g: "Game", city: City, ph: str, ctx: Optional[Ctx] = None) -> list[Unique]:
    """Local uniques of this city plus every civ-wide unique (City.forEachMatchingUnique)."""
    if ctx is None:
        ctx = city_ctx(g, city)
    return local_uniques(g, city, ph, ctx) + g.civ_uniques(city.owner, ph, ctx)


def city_has(g: "Game", city: City, ph: str) -> bool:
    """Whether any unique with this placeholder applies to the city."""
    return bool(city_uniques(g, city, ph))


def building_unique_in_city(g: "Game", city: City, ph: str) -> bool:
    """Whether one of the city's own buildings carries this unique.

    Distinct from :func:`city_has`, which also counts civ-wide uniques: some rules ask specifically
    whether the *building* is present, such as a courthouse removing annexation unhappiness.
    """
    ctx = city_ctx(g, city)
    return any(g.rules.buildings[b]["_umap"].has(ph, ctx) for b in city.buildings)


def contains_building(g: "Game", city: City, name: str) -> bool:
    """containsBuildingOrEquivalent."""
    if name in city.buildings:
        return True
    R = g.rules
    return any(R.buildings[b].get("replaces") == name or R.buildings[b]["_umap"].has_tag(name) for b in city.buildings)


def is_capital(g: "Game", city: City) -> bool:
    """Whether this city is its owner's capital."""
    return g.player(city.owner).capital == city.id


def has_annex_unhappiness(g: "Game", city: City) -> bool:
    """Whether this city still carries the unhappiness of a conquered city.

    A city founded by its current owner never has it, nor does a puppet - puppets trade control for
    contentment - and a courthouse removes it.
    """
    if city.owner == city.founder or city.puppet:
        return False
    return not building_unique_in_city(g, city, U.RemovesAnnexUnhappiness)


def is_garrisoned(g: "Game", city: City) -> bool:
    """Whether a friendly land unit is standing in the city."""
    m = g.military_at(city.idx)
    return m is not None and m.owner == city.owner and g.rules.units[m.type]["_domain"] == "Land"


def is_coastal(g: "Game", city: City) -> bool:
    """Whether the city is next to water, and so can build harbours and ships."""
    return T.adjacent_to_coast(g, city.idx)


def free_population(city: City) -> int:
    """Citizens not working a tile and not employed as specialists."""
    return city.pop - len(city.worked) - sum(city.specialists.values())


# ---------------------------------------------------------------------------------------------------------------
# Tiles
# ---------------------------------------------------------------------------------------------------------------
def work_range(g: "Game") -> int:
    """How far from its centre a city may work tiles."""
    return g.rules.k["city_work_range"]


def city_tiles(g: "Game", city: City) -> list[int]:
    """Every tile this city owns, cached because border checks ask constantly."""
    key = ("ctiles", city.id)
    v = g._ycache.get(key)
    if v is None:
        v = [i for i in g.grid.within(city.idx, g.rules.k["city_expand_range"]) if g.s.tiles[i].city == city.id]
        g._ycache[key] = v
    return v


def tiles_in_range(g: "Game", city: City) -> list[int]:
    """Every tile within working distance, owned or not."""
    return g.grid.within(city.idx, work_range(g))


def workable_tiles(g: "Game", city: City) -> list[int]:
    """Tiles this city's citizens could actually be put to work on.

    Four things disqualify a tile that is otherwise in range: it belongs to somebody else, another of
    your own cities is already working it, there is a city on it, or an enemy unit is standing on it -
    the last being a blockade, and the reason a war next door quietly starves a city.
    """
    out = []
    for i in tiles_in_range(g, city):
        if i == city.idx:
            continue
        t = g.s.tiles[i]
        if t.owner != city.owner:
            continue
        other = g.s.cities.get(t.city) if t.city is not None else None
        if other is not None and other.id != city.id and i in other.worked:
            continue
        if g.city_at(i) is not None:
            continue
        m = g.military_at(i)
        if m and g.at_war(city.owner, m.owner):
            continue
        out.append(i)
    return out


def provides_yield_without_pop(g: "Game", idx: int) -> bool:
    """Whether a tile yields something with no citizen on it, such as a Citadel."""
    t = g.s.tiles[idx]
    imp = T.unpillaged_improvement(t)
    if imp and g.rules.improvements[imp]["_umap"].has_tag(U.TileProvidesYieldWithoutPopulation):
        return True
    return T.terrain_has(g, idx, U.TileProvidesYieldWithoutPopulation)


def worked_or_free_tiles(g: "Game", city: City) -> list[int]:
    """Every tile contributing yields: the centre, worked tiles, and free-yield tiles."""
    out = [city.idx]
    for i in city.worked:
        out.append(i)
    for i in city_tiles(g, city):
        if i != city.idx and i not in city.worked and provides_yield_without_pop(g, i):
            out.append(i)
    return out


# ---------------------------------------------------------------------------------------------------------------
# Specialists
# ---------------------------------------------------------------------------------------------------------------
def max_specialists(g: "Game", city: City) -> dict:
    """Specialist slots by type, summed over the city's buildings."""
    out: dict = {}
    for b in city.buildings:
        for s, n in (g.rules.buildings[b].get("specialistSlots") or {}).items():
            out[s] = out.get(s, 0) + n
    return out


def specialist_stats(g: "Game", city: City, name: str) -> dict:
    """What one specialist of this type produces here, after local and civ-wide bonuses."""
    sp = g.rules.specialists.get(name)
    if sp is None:
        return {}
    s = dict(sp["_stats"])
    for u in city_uniques(g, city, U.StatsFromSpecialist):
        if city_matches(g, city, u.p(1)):
            add_into(s, u.stats)
    for u in city_uniques(g, city, U.StatsFromObject):
        if u.p(1) == name:
            add_into(s, u.stats)
    return s


# ---------------------------------------------------------------------------------------------------------------
# Buildings
# ---------------------------------------------------------------------------------------------------------------
def building_stats(g: "Game", city: City, b: str) -> dict:
    """The flat yields a building gives *in this city*.

    Its own stats, plus every unique that adds stats to matching objects. Wonders are excluded from the
    "stats from buildings" bonuses, which is what stops a policy that boosts buildings from also
    boosting every wonder in the empire.
    """
    R = g.rules
    bd = R.buildings[b]
    s = T.obj_stats(bd)
    ctx = city_ctx(g, city)
    for u in city_uniques(g, city, U.StatsFromObject, ctx):
        if building_matches(R, b, u.p(1)):
            add_into(s, u.stats)
    for u in bd["_umap"].matching(U.Stats, ctx):
        add_into(s, u.stats)
    if not bd.get("isWonder"):
        for u in city_uniques(g, city, U.StatsFromBuildings, ctx):
            if building_matches(R, b, u.p(1)):
                add_into(s, u.stats)
    return s


def building_pct(g: "Game", city: City, b: str) -> dict:
    """The percentage yield bonuses a building gives in this city."""
    R = g.rules
    s = {k: float(v) for k, v in (R.buildings[b].get("percentStatBonus") or {}).items()}
    ctx = city_ctx(g, city)
    for u in city_uniques(g, city, U.StatPercentFromObject, ctx):
        if building_matches(R, b, u.p(2)):
            k = STAT_KEY.get(u.p(1), u.p(1).lower())
            s[k] = s.get(k, 0.0) + u.n(0)
    for u in city_uniques(g, city, U.AllStatsPercentFromObject, ctx):
        if building_matches(R, b, u.p(1)):
            for k in STATS:
                s[k] = s.get(k, 0.0) + u.n(0)
    return s


def free_building_names(g: "Game", city: City) -> set:
    """Buildings this city was given rather than built, which cost no maintenance."""
    p = g.player(city.owner)
    return set(p.free_buildings.get(str(city.id), []))


def is_free_building(g: "Game", city: City, b: str) -> bool:
    """Whether this particular building was free in this city."""
    return b in g.player(city.owner).free_buildings.get(str(city.id), [])


def maintenance(g: "Game", city: City) -> float:
    """Total gold upkeep for the city's buildings.

    Free buildings are skipped, uniques may discount matching buildings, and an AI player's total is
    scaled by its difficulty's maintenance modifier - which is where the difficulty levels do much of
    their work, without changing any rule a player can see.
    """
    free = free_building_names(g, city)
    mus = [u for u in city_uniques(g, city, U.BuildingMaintenance) if city_matches(g, city, u.p(2))]
    total = 0.0
    for b in city.buildings:
        if b in free:
            continue
        m = float(g.rules.buildings[b].get("maintenance", 0))
        for u in mus:
            if building_matches(g.rules, b, u.p(1)):
                m *= 1 + u.n(0) / 100
        total += m
    from .economy import is_humanlike, seat_difficulty
    if not is_humanlike(g, city.owner) and g.player(city.owner).kind == "major":
        total *= seat_difficulty(g, city.owner)["aiBuildingMaintenanceModifier"]
    return total


# ---------------------------------------------------------------------------------------------------------------
# City stats
# ---------------------------------------------------------------------------------------------------------------
def current_construction(city: City) -> Optional[str]:
    """What the city is building now, or None."""
    return city.queue[0] if city.queue else None


def is_unit(g: "Game", name: Optional[str]) -> bool:
    """Whether a name refers to a unit in the ruleset."""
    return name in g.rules.units


def is_building(g: "Game", name: Optional[str]) -> bool:
    """Whether a name refers to a building in the ruleset."""
    return name in g.rules.buildings


def trade_route_stats(g: "Game", city: City) -> dict:
    """The yields of this city's trade route to the capital, if it is connected.

    The capital itself and unconnected cities earn nothing, which is what makes roads worth their
    maintenance and what a player notices first when a war cuts a route.
    """
    p = g.player(city.owner)
    cap = g.city(p.capital) if p.capital is not None else None
    s: dict = {}
    if cap is None or cap.id == city.id or not connected_to_capital(g, city):
        return s
    s["gold"] = cap.pop * 0.15 + city.pop * 1.1 - 1
    for u in city_uniques(g, city, U.StatsFromTradeRoute):
        add_into(s, u.stats)
    pct: dict = {}
    for u in city_uniques(g, city, U.StatPercentFromTradeRoutes):
        k = STAT_KEY.get(u.p(1), u.p(1).lower())
        pct[k] = pct.get(k, 0) + u.n(0)
    for k in list(s):
        s[k] *= 1 + pct.get(k, 0) / 100
    return s


def _uniques_by_source(g, city) -> dict:
    """Flat stats from city/civ uniques, per source (getStatsFromUniquesBySource)."""
    out: dict = {}
    from .economy import civ_uniques
    cs_mults = civ_uniques(g, city.owner, U.BonusStatsFromCityStates)

    def add(u, stats, mult=1.0):
        """Accumulate one unique's stats under its source, applying city-state multipliers."""
        st = dict(stats)
        if u.src_type == "CityState":
            for m in cs_mults:
                k = STAT_KEY.get(m.p(1), m.p(1).lower())
                if k in st:
                    st[k] *= 1 + m.n(0) / 100
        src = u.src_type or "Uniques"
        d = out.setdefault(src, {})
        add_into(d, st, mult)

    for u in city_uniques(g, city, U.StatsPerCity):
        if city_matches(g, city, u.p(1)):
            add(u, u.stats)
    for u in city_uniques(g, city, U.StatsPerPopulation):
        if city_matches(g, city, u.p(2)):
            add(u, u.stats, city.pop // max(1, int(u.n(1))))
    for u in city_uniques(g, city, U.StatsFromCitiesOnSpecificTiles):
        if tile_terrain_matches(g, city.idx, u.p(1), city.owner):
            add(u, u.stats)
    return out


def _pct_bonuses(g, city, construction: Optional[str]) -> dict:
    """Every percentage modifier applying to this city's yields.

    What goes in here rather than into a building's own bonus is anything conditional on the empire
    rather than the building: a golden age, the railway connection to the capital, the puppet penalty,
    the production penalty for exceeding unit supply, and the bonuses that depend on what is being
    built right now.

    That last part is why *construction* is a parameter. "+25% production toward wonders" is not a
    property of the city; it is a property of the city building a wonder, and the value changes the
    moment the queue does.
    """
    R = g.rules
    p = g.player(city.owner)
    pct: dict = {}
    if p.golden_age_turns > 0:
        add_into(pct, {"production": 20.0, "culture": 20.0})
    rr = R.improvements.get("Railroad")
    if rr and g.has_tech(city.owner, rr.get("techRequired")) and (is_capital(g, city) or connected_to_capital(g, city, rail=True)):
        add_into(pct, {"production": 25.0})
    if city.puppet:
        add_into(pct, {"science": -25.0, "culture": -25.0})
    from .economy import unit_supply_deficit, unit_supply_penalty
    if p.kind == "major" and unit_supply_deficit(g, city.owner) > 0:
        add_into(pct, {"production": unit_supply_penalty(g, city.owner)})
    ctx = city_ctx(g, city)
    for u in city_uniques(g, city, U.StatPercentBonus, ctx):
        add_into(pct, {STAT_KEY.get(u.p(1), u.p(1).lower()): u.n(0)})
    for u in city_uniques(g, city, U.StatPercentBonusCities, ctx):
        if city_matches(g, city, u.p(2)):
            add_into(pct, {STAT_KEY.get(u.p(1), u.p(1).lower()): u.n(0)})
    if construction and construction in R.units:
        for u in city_uniques(g, city, U.PercentProductionUnits, ctx):
            if base_unit_matches(R, construction, u.p(1)) and city_matches(g, city, u.p(2)):
                add_into(pct, {"production": u.n(0)})
    elif construction and construction in R.buildings:
        wonder = R.buildings[construction]["_any_wonder"]
        ph = U.PercentProductionWonders if wonder else U.PercentProductionBuildings
        for u in city_uniques(g, city, ph, ctx):
            if building_matches(R, construction, u.p(1)) and city_matches(g, city, u.p(2)):
                add_into(pct, {"production": u.n(0)})
        cap = g.city(p.capital) if p.capital is not None else None
        if cap is not None and construction in cap.buildings:
            for u in city_uniques(g, city, U.PercentProductionBuildingsInCapital, ctx):
                add_into(pct, {"production": u.n(0)})
    from . import religion
    for u in city_uniques(g, city, U.StatPercentFromReligionFollowers, ctx):
        k = STAT_KEY.get(u.p(1), u.p(1).lower())
        add_into(pct, {k: min(u.n(0) * religion.followers_of_majority(g, city), u.n(2))})
    for b in city.buildings:
        add_into(pct, building_pct(g, city, b))
    return pct


def food_eaten(g: "Game", city: City) -> float:
    """Food consumed by this city's citizens each turn.

    Specialists are counted separately from tile workers because several uniques reduce specialist
    consumption specifically - which is the mechanism that makes a tall, specialist-heavy city viable.
    """
    specs = sum(city.specialists.values())
    by_specialists = 2.0 * specs
    eaten = city.pop * 2.0 - by_specialists
    for u in city_uniques(g, city, U.FoodConsumptionBySpecialists):
        if city_matches(g, city, u.p(1)):
            by_specialists *= 1 + u.n(0) / 100
    eaten += by_specialists
    from .uniques import population_amount
    for u in city_uniques(g, city, U.FoodConsumptionByPopulation):
        if city_matches(g, city, u.p(2)):
            amt = 2.0 * population_amount(g, city, u.p(1))
            eaten -= amt * (1 - (1 + u.n(0) / 100))
    return eaten


def growth_bonus(g: "Game", city: City, total_food: float) -> dict:
    """Extra food from growth-percentage uniques, kept per source so the city screen can show why."""
    out = {}
    for u in city_uniques(g, city, U.GrowthPercentBonus):
        if city_matches(g, city, u.p(1)):
            src = u.src_type or "Uniques"
            out[src] = out.get(src, 0.0) + u.n(0) / 100 * total_food
    return out


def production_from_excess_food(food: float) -> float:
    """How much production surplus food converts into, for the buildings that do that.

    The steps are UnCiv's, not a formula: 1 food gives 1, 2 gives 2, and every further 4 gives one more.
    """
    if food >= 4:
        return 2.0 + int(food / 4)
    if food >= 2:
        return 2.0
    if food >= 1:
        return 1.0
    return 0.0


def can_convert_food(g: "Game", food: float, construction: Optional[str]) -> bool:
    """Whether the thing being built converts surplus food into production."""
    if food <= 0 or not construction or construction in PERPETUAL:
        return False
    obj = g.rules.units.get(construction) or g.rules.buildings.get(construction)
    return bool(obj) and obj["_umap"].has_tag(U.ConvertFoodToProductionWhenConstructed)


def city_stats(g: "Game", city: City, construction: Optional[str] = "__current__") -> dict:
    """Full city stats (CityStats.update). Returns {"final": {source: stats}, "total": stats, "happiness_list",
    "tiles": stats, "food_surplus", ...}. Cached until yields are invalidated."""
    if construction == "__current__":
        construction = current_construction(city)
        key = ("cstats", city.id)
        v = g._ycache.get(key)
        if v is not None:
            return v
        g._ycache[key] = _EMPTY_STATS          # re-entry guard
        v = _compute_city_stats(g, city, construction)
        g._ycache[key] = v
        return v
    return _compute_city_stats(g, city, construction)


_EMPTY_STATS = {"final": {}, "total": dict.fromkeys(STATS, 0.0), "happiness_list": {}, "tiles": {}, "food_surplus": 0.0,
                "production": 0.0}


def city_happiness(g: "Game", city: City) -> dict:
    """The city's happiness sources (CityStats.updateCityHappiness). Computed separately from the other stats so
    that empire happiness never needs full city stats (city growth in turn depends on empire happiness)."""
    key = ("chappy", city.id)
    v = g._ycache.get(key)
    if v is not None:
        return v
    busy = city.owner not in g._hap_busy
    g._hap_busy.add(city.owner)
    try:
        return _city_happiness(g, city)
    finally:
        if busy:
            g._hap_busy.discard(city.owner)


def _city_happiness(g: "Game", city: City) -> dict:
    """Compute happiness for one city: the cost of existing, of population, and of occupation.

    The cache is seeded with an empty result before anything else runs, so a unique that asks about
    happiness while happiness is being computed gets zero rather than recursing forever.

    Annexed cities are punished twice - a flat penalty and doubled population unhappiness - which is
    what makes puppeting the default choice for a conquered city and courthouses worth their cost.
    City-states have no happiness at all.
    """
    key = ("chappy", city.id)
    g._ycache[key] = {}
    p = g.player(city.owner)
    tiles_s = zero()
    for i in worked_or_free_tiles(g, city):
        add_into(tiles_s, T.tile_stats(g, i, city.owner, city))
    bstats_total = zero()
    for b in city.buildings:
        add_into(bstats_total, building_stats(g, city, b))
    spec = zero()
    for name, n in city.specialists.items():
        if n > 0:
            add_into(spec, specialist_stats(g, city, name), n)
    by_source = _uniques_by_source(g, city)
    from .economy import difficulty, is_humanlike, seat_difficulty, civ_uniques
    unhap_mod = difficulty(g, city.owner)["unhappinessModifier"]
    if not is_humanlike(g, city.owner) and p.kind == "major":
        unhap_mod *= seat_difficulty(g, city.owner)["aiUnhappinessModifier"]
    annex = has_annex_unhappiness(g, city)
    from_city = -3.0 - (2.0 if annex else 0.0)
    umod = sum(u.n(0) for u in civ_uniques(g, city.owner, U.UnhappinessFromCitiesPercentage))
    hl: dict = {"Cities": from_city * unhap_mod * (1 + umod / 100)}
    from .uniques import population_amount
    citizens = float(city.pop)
    for u in city_uniques(g, city, U.UnhappinessFromPopulationTypePercentageChange):
        if city_matches(g, city, u.p(2)):
            citizens += u.n(0) / 100 * population_amount(g, city, u.p(1))
    if annex:
        citizens *= 2
    citizens = max(0.0, citizens)
    hl["Population"] = -citizens * unhap_mod
    if annex:
        hl["Occupied City"] = -2.0
    if int(spec.get("happiness", 0)) > 0:
        hl["Specialists"] = float(int(spec["happiness"]))
    hl["Buildings"] = float(int(bstats_total.get("happiness", 0)))
    hl["Tile yields"] = tiles_s.get("happiness", 0.0)
    for src, s in by_source.items():
        if s.get("happiness"):
            hl[src] = hl.get(src, 0.0) + s["happiness"]
    if p.kind != "major":
        hl = {}
    g._ycache[key] = hl
    return hl


def _compute_city_stats(g: "Game", city: City, construction: Optional[str]) -> dict:
    """Compute every yield for a city, keeping each source separate.

    The result is a breakdown rather than a total, because "why is this city producing 14 science" is a
    question both a player and a model ask constantly, and only a per-source answer answers it.

    Order matters here. Percentages apply to production first, because production is what the
    construction conversion consumes; food is computed after consumption so growth bonuses apply to the
    surplus rather than the gross; and the floor of one production is applied last so that a city under
    every possible penalty still builds something eventually.

    A city in resistance produces nothing at all - the whole breakdown is discarded - which is the
    strongest argument in the game against annexing something you cannot hold.
    """
    p = g.player(city.owner)
    # tiles
    tiles_s = zero()
    for i in worked_or_free_tiles(g, city):
        add_into(tiles_s, T.tile_stats(g, i, city.owner, city))
    # buildings
    bstats_total = zero()
    for b in city.buildings:
        add_into(bstats_total, building_stats(g, city, b))
    # specialists
    spec = zero()
    for name, n in city.specialists.items():
        if n > 0:
            add_into(spec, specialist_stats(g, city, name), n)
    by_source = _uniques_by_source(g, city)
    hl = city_happiness(g, city)

    # base stat tree
    final: dict = {}
    pop_s = {"science": float(city.pop), "production": float(max(0, free_population(city)))}
    final["Population"] = dict(pop_s)
    final["Tile yields"] = dict(tiles_s)
    final["Specialists"] = dict(spec)
    final["Trade routes"] = trade_route_stats(g, city)
    final["Buildings"] = dict(bstats_total)
    for src, s in by_source.items():
        d = final.setdefault(src, {})
        add_into(d, s)
    for s in final.values():
        s.pop("happiness", None)
    pct = _pct_bonuses(g, city, construction)
    for s in final.values():
        if "production" in s:
            s["production"] *= 1 + pct.get("production", 0) / 100
    prod_total = sum(s.get("production", 0) for s in final.values())
    if construction in ("Gold", "Science"):
        k = PERPETUAL[construction]
        rate = 0.25
        final["Construction"] = {k: prod_total * rate}
    for s in final.values():
        for k in ("gold", "culture", "food", "faith"):
            if k in s:
                s[k] *= 1 + pct.get(k, 0) / 100
    for s in final.values():
        if "science" in s:
            s["science"] *= 1 + pct.get("science", 0) / 100
    # food
    final["Population"]["food"] = final["Population"].get("food", 0.0) - food_eaten(g, city)
    total_food = sum(s.get("food", 0) for s in final.values())
    if total_food > 0:
        for src, amt in growth_bonus(g, city, total_food).items():
            d = final.setdefault(f"{src} (growth)", {})
            d["food"] = d.get("food", 0.0) + amt
        from .economy import happiness
        if city.wltkd > 0 and p.kind == "major" and happiness(g, city.owner)["total"] >= 0:
            final["We Love The King Day"] = {"food": total_food / 4}
        total_food = sum(s.get("food", 0) for s in final.values())
    final["Maintenance"] = {"gold": -float(int(maintenance(g, city)))}
    if can_convert_food(g, total_food, construction):
        final["Excess food to production"] = {"production": production_from_excess_food(total_food), "food": -total_food}
    if city_uniques(g, city, U.NullifiesGrowth):
        cur = sum(s.get("food", 0) for s in final.values())
        if cur > 0:
            final["Unhappiness"] = {"food": -cur}
    if city.resistance > 0:
        final = {}
    if sum(s.get("production", 0) for s in final.values()) < 1:
        final["Production"] = {"production": 1.0}
    total = zero()
    for s in final.values():
        add_into(total, s)
    total["happiness"] = sum(hl.values())
    return {"final": final, "total": total, "happiness_list": hl, "tiles": tiles_s,
            "food_surplus": total["food"], "production": total["production"], "pct": pct}


def food_for_next_turn(g: "Game", city: City) -> int:
    """The city's food surplus this turn, rounded."""
    return int(round(city_stats(g, city)["total"]["food"]))


def food_to_next_pop(g: "Game", city: City) -> int:
    """Food needed for the next citizen.

    Superlinear in population, scaled by game speed, and further scaled for city-states and for AI
    difficulty - which is where an AI on a high difficulty gets much of its advantage.
    """
    n = city.pop - 1
    req = 15 + 8 * n + math.floor(n ** 1.5)
    req *= g.speed["modifier"]
    p = g.player(city.owner)
    if p.kind == "city_state":
        req *= 1.5
    from .economy import is_humanlike, seat_difficulty
    if p.kind == "major" and not is_humanlike(g, city.owner):
        req *= seat_difficulty(g, city.owner)["aiCityGrowthModifier"]
    return int(req)


# ---------------------------------------------------------------------------------------------------------------
# Citizen assignment (CityPopulationManager.autoAssignPopulation + Automation.rankStatsForCityWork)
# ---------------------------------------------------------------------------------------------------------------
_WEIGHTS = {"food": 14, "production": 12.01, "gold": 6, "science": 9.01, "culture": 8, "happiness": 10, "faith": 7}


def rank_stats_for_work(g: "Game", city: City, stats: dict, specialist: bool, surplus_food: float) -> float:
    """Score a set of yields for the purpose of assigning a citizen.

    This is the function that decides what a city does with its people, and it is the most heavily
    conditioned code in the module because the right answer depends on the situation rather than on the
    numbers alone:

    * **Starving beats everything.** Food that closes a deficit is weighted eight times over, because a
      shrinking city loses more than any tile yields.
    * **Growth is discounted when it is not wanted** - avoid-growth, deep unhappiness, or a balanced
      focus in a city that is already large.
    * **Science scales with size.** A small city's science is halved, because a library in a size-3
      city is not where the empire's science comes from.
    * **Bankruptcy doubles the value of gold**, and unhappiness doubles the value of happiness.

    The explicit focus multipliers are applied last, so a player who has said "production" gets
    production even where the heuristics would have chosen otherwise.
    """
    y = {k: stats.get(k, 0.0) for k in STATS}
    focus = FOCUSES.get(city.focus, {})
    from .economy import happiness
    if specialist:
        for u in city_uniques(g, city, U.FoodConsumptionBySpecialists):
            if city_matches(g, city, u.p(1)):
                y["food"] -= u.n(0) / 100 * 2
        if y["science"] == 3 or y["science"] >= 5:
            y["science"] *= 1.3
    starving = surplus_food < 0
    cons = current_construction(city)
    if can_convert_food(g, surplus_food, cons):
        y["production"] += production_from_excess_food(surplus_food + y["food"]) - production_from_excess_food(surplus_food)
        y["food"] = 0
    feed = min(y["food"], -surplus_food) if starving else 0.0
    feed = max(0.0, feed)
    growth = 0.0 if city.avoid_growth else y["food"] - feed
    for k in STATS:
        y[k] *= _WEIGHTS[k] if k != "food" else 1
    food_w = _WEIGHTS["food"]
    y["food"] = feed * food_w * 8
    hap = happiness(g, city.owner)["total"] if g.player(city.owner).kind == "major" else 0
    if not city_uniques(g, city, U.NullifiesGrowth):
        ng = growth + sum(growth_bonus(g, city, growth).values()) if growth > 0 else growth
        if city.wltkd > 0 and hap >= 0:
            ng += growth / 4
        ng = max(0.0, ng)
        if hap < -8:
            fmod = 0.0
        elif g.player(city.owner).controller in BOT_MANAGED:        # the bots' citizen weighting
            fmod = 1.5
        elif city.focus == "balanced":
            fmod = 2.0 if city.pop < 5 else (0.75 if surplus_food > food_to_next_pop(g, city) / (10 * g.speed["modifier"]) else 1.0)
        else:
            fmod = 1.0
        y["food"] += ng * food_w * fmod
    if city.pop < 10:
        y["science"] /= 2
    if civ_stats_gold(g, city.owner) < 0:
        y["gold"] *= 2
    if hap < 0:
        y["happiness"] *= 2
    if cons in PERPETUAL:
        y["production"] /= 6
    for k, m in focus.items():
        y[k] *= m
    return sum(y.values())


def civ_stats_gold(g, pid) -> float:
    """This civilization's gold per turn as of the last completed calculation.

    Deliberately the *previous* value: citizen assignment consults it, and gold depends on citizen
    assignment. Using last turn's number breaks the cycle at a cost of being one turn stale, which
    nobody can see.
    """
    key = ("gold_rate_prev", pid)
    return g._ycache.get(key, g.player(pid).flags.get("last_gold_rate", 0.0))


def rank_specialist(g: "Game", city: City, name: str, surplus: float) -> float:
    """Score a specialist slot, including the great-person points it generates."""
    s = specialist_stats(g, city, name)
    r = rank_stats_for_work(g, city, s, True, surplus)
    sp = g.rules.specialists.get(name)
    if sp:
        from .great_people import city_gpp_bonus
        r += sum((sp.get("greatPersonPoints") or {}).values()) * (100 + city_gpp_bonus(g, city)) / 100
    return r


def assign_citizens(g: "Game", city: City, reset: bool = False):
    """Reassign all population (City.reassignPopulation): keep locked tiles, fill the rest by rank."""
    if reset:
        city.locked = []
    avail = set(workable_tiles(g, city))
    city.locked = [i for i in city.locked if i in avail]
    city.worked = list(city.locked[: city.pop])
    if not city.manual_specialists:
        city.specialists = {}
    else:
        maxs = max_specialists(g, city)
        city.specialists = {k: min(v, maxs.get(k, 0)) for k, v in city.specialists.items() if v > 0}
    g.invalidate_city(city, citizens_only=True)
    auto_assign_population(g, city)


def auto_assign_population(g: "Game", city: City):
    """Put every unemployed citizen somewhere, one at a time, best first.

    Greedy rather than optimal, and deliberately so: the marginal value of a tile depends on how much
    food the city already has, so each assignment changes the ranking for the next one. Solving that
    properly is a small optimisation problem per city per turn, and the greedy answer is
    indistinguishable in play.

    Ties break on coordinates so that two identical tiles are always chosen in the same order. Without
    it, a city's worked tiles would shuffle between turns for no reason, and a replay would not
    reproduce.

    Specialists compete with tiles on the same scale, unless the player has set them by hand, in which
    case their choice is left alone.
    """
    free = free_population(city)
    if free <= 0:
        _unassign_extra(g, city)
        return
    stats = city_stats(g, city)
    surplus = stats["total"]["food"]
    tiles = [i for i in workable_tiles(g, city) if i not in city.worked and not provides_yield_without_pop(g, i)]
    ts = {i: T.tile_stats(g, i, city.owner, city) for i in tiles}
    maxs = max_specialists(g, city)
    spec_food_bonus = 2.0
    for u in city_uniques(g, city, U.FoodConsumptionBySpecialists):
        if city_matches(g, city, u.p(1)):
            spec_food_bonus *= 1 + u.n(0) / 100
    spec_food_bonus = 2.0 - spec_food_bonus
    spec_rank_cache = {}
    for _ in range(free):
        best, best_v = None, 0.0
        for i in tiles:
            if i in city.worked:
                continue
            v = rank_stats_for_work(g, city, ts[i], False, surplus)
            x, y = g.grid.xy(i)
            if best is None or (v, x, y) > (best_v, *g.grid.xy(best)):
                best, best_v = i, v
        best_job, best_job_v = None, 0.0
        if not city.manual_specialists:
            for name, mx in maxs.items():
                if city.specialists.get(name, 0) < mx:
                    v = spec_rank_cache.get(name)
                    if v is None:
                        v = rank_specialist(g, city, name, surplus)
                        spec_rank_cache[name] = v
                    if best_job is None or v > best_job_v:
                        best_job, best_job_v = name, v
        if best is not None and (best_job is None or best_v > best_job_v):
            city.worked.append(best)
            surplus += ts[best].get("food", 0)
        elif best_job is not None:
            city.specialists[best_job] = city.specialists.get(best_job, 0) + 1
            surplus += spec_food_bonus
        else:
            break
    g.invalidate_city(city, citizens_only=True)


def _unassign_extra(g: "Game", city: City):
    """Take citizens off tiles and out of jobs when the city has fewer people than assignments.

    Happens when a city shrinks, loses a tile to a rival's border growth, or has a tile blockaded. The
    worst tile goes first, and a locked tile is given up last - a lock is a statement of intent, and
    breaking it silently when the city starves would lose information the player cannot get back.
    """
    avail = set(workable_tiles(g, city))
    city.worked = [i for i in city.worked if i in avail]
    maxs = max_specialists(g, city)
    for k in list(city.specialists):
        if city.specialists[k] > maxs.get(k, 0):
            city.specialists[k] = maxs.get(k, 0)
        if city.specialists[k] <= 0:
            del city.specialists[k]
    while free_population(city) < 0:
        if city.worked:
            worst = min(city.worked, key=lambda i: (i in city.locked,
                        rank_stats_for_work(g, city, T.tile_stats(g, i, city.owner, city), False, 0)))
            city.worked.remove(worst)
            if worst in city.locked:
                city.locked.remove(worst)
        elif city.specialists:
            k = next(iter(city.specialists))
            city.specialists[k] -= 1
            if city.specialists[k] <= 0:
                del city.specialists[k]
        else:
            break
    g.invalidate_city(city, citizens_only=True)


def add_population(g: "Game", city: City, n: int):
    """Change a city's population and re-assign its citizens.

    Clamped so a city cannot be reduced below one: a city with no citizens is not a smaller city, it is
    a different thing entirely, and razing is how that happens.
    """
    n = max(n, 1 - city.pop)
    city.pop += n
    if g.religion_enabled:
        from . import religion
        religion.on_population_change(g, city, n)
    g.invalidate_city(city)
    if free_population(city) < 0:
        _unassign_extra(g, city)
    else:
        auto_assign_population(g, city)


# ---------------------------------------------------------------------------------------------------------------
# Borders
# ---------------------------------------------------------------------------------------------------------------
def tiles_claimed(g: "Game", city: City) -> int:
    """Tiles owned beyond the first ring, which is what border growth costs are based on."""
    ring = set(g.grid.neighbors(city.idx))
    return sum(1 for i in city_tiles(g, city) if i != city.idx and i not in ring)


def culture_to_next_tile(g: "Game", city: City) -> int:
    """Culture needed for this city's next tile, which grows as it expands."""
    c = 6 * (max(0, tiles_claimed(g, city)) + 1.4813) ** 1.3
    c *= g.speed["cultureCostModifier"]
    if g.player(city.owner).kind == "city_state":
        c *= 1.5
    for u in city_uniques(g, city, U.BorderGrowthPercentage):
        if city_matches(g, city, u.p(1)):
            c *= 1 + u.n(0) / 100
    return int(round(c))


def rank_tile_for_expansion(g: "Game", city: City, idx: int) -> int:
    """Score a tile for border growth. **Lower is better.**

    Inverted because the original ranks by cost rather than by value, and keeping the sign makes this
    comparable against UnCiv's numbers.

    What moves the score: distance (near is better), a visible strategic or luxury resource (a large
    bonus, and a bonus resource counts only within working range), a natural wonder, the tile's own
    yields, and a smaller nudge for resources and wonders on adjacent tiles - because claiming a tile
    next to something good is how you get to claim the good thing next. Contested tiles, where a
    neighbour's borders are already adjacent, are penalised rather than preferred; racing for a tile
    you will lose is rarely worth the culture.
    """
    t = g.s.tiles[idx]
    wr = work_range(g)
    d = g.grid.distance(idx, city.idx)
    score = d * 100
    pid = city.owner
    if t.resource and T.resource_visible(g, pid, t.resource):
        if g.rules.resources[t.resource]["resourceType"] != "Bonus":
            score -= 105
        elif d <= wr:
            score -= 104
    else:
        if T.is_water(g, idx):
            score += 3
        if d > wr:
            score += 100
    if t.wonder:
        score -= 105
    score -= int(sum(T.tile_stats(g, idx, pid, city).values()))
    adj_nw, contested = False, False
    p = g.player(pid)
    for n in g.grid.neighbors(idx):
        nt = g.s.tiles[n]
        if not p.explored[n] or nt.owner == pid:
            continue
        if nt.owner is not None:
            contested = True
            continue
        nd = g.grid.distance(city.idx, n)
        if nt.resource and T.resource_visible(g, pid, nt.resource) and (
                nd <= wr or g.rules.resources[nt.resource]["resourceType"] != "Bonus"):
            score -= 1
        if nt.wonder:
            if not adj_nw:
                score -= 1
            adj_nw = True
    if contested:
        score += 10
    return score


def choosable_tiles(g: "Game", city: City) -> list[int]:
    """Unowned tiles adjacent to this city's territory, which are the ones borders can grow into."""
    return [i for i in g.grid.within(city.idx, g.rules.k["city_expand_range"]) if g.s.tiles[i].owner is None
            and any(g.s.tiles[n].city == city.id for n in g.grid.neighbors(i))]


def take_ownership(g: "Game", city: City, idx: int):
    """Transfer a tile to this city, tidying up whatever was there.

    Three consequences are handled here because forgetting any of them leaves the game inconsistent: a
    rival city loses the tile from its worked and locked lists, a barbarian camp on it is destroyed,
    and foreign units that may not be in this territory are teleported out.
    """
    t = g.s.tiles[idx]
    if t.city is not None and t.city != city.id:
        other = g.city(t.city)
        if other is not None:
            if idx in other.worked:
                other.worked.remove(idx)
            if idx in other.locked:
                other.locked.remove(idx)
    if t.improvement == "Barbarian encampment":
        from . import barbarians
        barbarians.remove_camp(g, idx)
    t.owner = city.owner
    t.city = city.id
    g.invalidate()
    for u in list(g.units_at(idx)):
        if u.owner != city.owner and not g.can_enter_territory(u.owner, idx):
            from .movement import teleport_to_closest
            teleport_to_closest(g, u)


def expand_borders(g: "Game", city: City) -> Optional[int]:
    """Claim the best available adjacent tile."""
    cands = choosable_tiles(g, city)
    if not cands:
        return None
    best = min(cands, key=lambda i: rank_tile_for_expansion(g, city, i))
    take_ownership(g, city, best)
    return best


def buy_tile_cost(g: "Game", city: City, idx: int) -> int:
    """Gold to buy a tile, rising with distance and with how much the city has already claimed."""
    d = g.grid.distance(idx, city.idx)
    cost = 50 * (d - 1) + tiles_claimed(g, city) * 5.0
    cost *= g.speed["goldCostModifier"]
    for u in city_uniques(g, city, U.TileCostPercentage):
        if city_matches(g, city, u.p(1)):
            cost *= 1 + u.n(0) / 100
    return int(round(cost))


def can_buy_tile(g: "Game", city: City, idx: int) -> Optional[str]:
    """Why this tile cannot be bought, or None if it can.

    Returns the reason rather than a boolean so that the caller can say it. "You cannot do that" is
    the least useful message a game can produce, and it is read by models as often as by people.
    """
    if city.puppet or city.razing:
        return "Puppets and cities being razed cannot buy tiles."
    t = g.s.tiles[idx]
    if t.owner is not None:
        return "That tile is already owned."
    if city.resistance > 0:
        return "The city is in resistance."
    if g.grid.distance(idx, city.idx) > work_range(g):
        return f"Tiles can only be bought within {work_range(g)} tiles of the city."
    if not any(g.s.tiles[n].city == city.id for n in g.grid.neighbors(idx)):
        return "You can only buy tiles adjacent to the city's borders."
    return None


def buy_tile(g: "Game", city: City, idx: int) -> dict:
    """Buy a tile with gold, and put a citizen on it if it is worth working."""
    reason = can_buy_tile(g, city, idx)
    if reason:
        raise ActionError(reason)
    cost = buy_tile_cost(g, city, idx)
    p = g.player(city.owner)
    if p.gold < cost:
        raise ActionError(f"Buying that tile costs {cost} gold; you have {int(p.gold)}.")
    p.gold -= cost
    take_ownership(g, city, idx)
    city.tiles_bought += 1
    assign_citizens(g, city)
    return {"bought": g.xy(idx), "gold_spent": cost}


# ---------------------------------------------------------------------------------------------------------------
# Construction costs & availability
# ---------------------------------------------------------------------------------------------------------------
def production_cost(g: "Game", pid: int, name: str, city: Optional[City] = None) -> int:
    """What *name* costs to build here, in production.

    Three layers, in order: uniques on the object itself (some wonders cost more with each one built,
    some buildings cost more per city), the difficulty modifiers, and the game speed. Difficulty is
    applied differently to humans and to AI players, and to units, buildings and wonders separately -
    which is how a higher difficulty makes the AI faster without changing a single rule the player can
    read.
    """
    R = g.rules
    p = g.player(pid)
    obj = R.units.get(name) or R.buildings.get(name)
    if obj is None:
        return 0
    cost = float(obj.get("cost", 0))
    ctx = city_ctx(g, city) if city is not None else Ctx(g, civ=pid)
    for u in obj["_umap"].matching(U.CostIncreasesWhenBuilt, ctx):
        cost += p.built_increasing.get(name, 0) * u.n(0)
    for u in obj["_umap"].matching(U.CostIncreasesPerCity, ctx):
        cost += len(g.player_cities(pid)) * u.n(0)
    for u in obj["_umap"].matching(U.CostPercentageChange, ctx):
        cost *= 1 + u.n(0) / 100
    from .economy import is_humanlike, difficulty, seat_difficulty
    is_unit_ = name in R.units
    if p.kind == "city_state":
        cost *= 1.5
    elif is_humanlike(g, pid):
        if is_unit_:
            cost *= difficulty(g, pid)["unitCostModifier"]
        elif not obj.get("isWonder"):
            cost *= difficulty(g, pid)["buildingCostModifier"]
    elif p.kind == "major":
        gd = seat_difficulty(g, pid)
        if is_unit_:
            cost *= gd["aiUnitCostModifier"]
        else:
            cost *= gd["aiWonderCostModifier"] if obj.get("isWonder") else gd["aiBuildingCostModifier"]
    cost *= g.speed["productionCostModifier"]
    return int(cost)


def equivalent_unit(g: "Game", pid: int, name: str) -> str:
    """The civ's unique replacement for a unit, if any."""
    nation = g.player(pid).nation
    return g.rules.unique_units.get(nation, {}).get(name, name)


def equivalent_building(g: "Game", pid: int, name: str) -> str:
    """The building this civilization builds in place of *name*, if it has a unique one.

    Resolves in both directions: given a unique building it finds what it replaces, and given the
    standard one it finds the civilization's version. Both are needed, because requirements name the
    standard building while cities contain the unique one.
    """
    R = g.rules
    b = R.buildings.get(name)
    if b and b.get("replaces") and b["replaces"] in R.buildings:
        name = b["replaces"]
    nation = g.player(pid).nation
    return R.unique_buildings.get(nation, {}).get(name, name)


def count_constructed(g: "Game", pid: int, name: str, exclude: Optional[City] = None) -> int:
    """How many of this thing the civilization has, including queued and in the spaceship.

    Queued items count, which is what stops a limited wonder from being started in four cities at once
    and wasting three cities' production. The city being checked (``exclude``) doesn't count its own queue:
    otherwise an item's own place in the queue counted against its limit, and a queue check removed the last
    one allowed (the third spaceship booster with two built, say) the turn after it was ordered.
    """
    in_space = g.s.spaceship.get(pid, {}).get(name, 0)
    if name in g.rules.buildings:
        return in_space + sum(1 for c in g.player_cities(pid)
                              if contains_building(g, c, name) or (name in c.queue and c is not exclude))
    return in_space + sum(1 for u in g.player_units(pid) if u.type == name) + \
        sum(1 for c in g.player_cities(pid) if name in c.queue and c is not exclude)


def _not_met(g, u: Unique, ctx: Ctx, pid: int, built_variant: bool) -> list[tuple[str, str]]:
    """Turn an unsatisfied condition on a unique into a reason a player can act on.

    The two building-count conditions get spelled out with the civilization's own building names,
    because "requires a Temple in all cities" is actionable and "condition not met" is not.
    """
    out = []
    from .uniques import _COND, _META
    for m in u.mods:
        fn = _COND.get(m.ph)
        if fn is None and m.ph in _META:
            continue
        if fn is not None and fn(u, m, ctx):
            continue
        if m.ph == "if [] is constructed in all [] cities":
            b = equivalent_building(g, pid, m.p(0))
            out.append(("RequiresBuildingInAllCities", f"Requires a {b} in all {m.p(1)} cities"))
        elif m.ph == "if [] is constructed in at least [] of [] cities":
            b = equivalent_building(g, pid, m.p(0))
            out.append(("RequiresBuildingInSomeCities", f"Requires a {b} in at least {m.p(1)} cities"))
        elif built_variant:
            out.append(("CanOnlyBeBuiltInSpecificCities", u.text))
        else:
            out.append(("ShouldNotBeDisplayed", f"Not available ({m.text})"))
    return out


def rejection_reasons(g: "Game", city: City, name: str, pid: Optional[int] = None) -> list[tuple[str, str]]:
    """(type, message) reasons this city cannot build `name` now (empty -> buildable)."""
    R = g.rules
    pid = city.owner if pid is None else pid
    p = g.player(pid)
    ctx = city_ctx(g, city)
    out: list = []
    if name in PERPETUAL:
        if name == "Nothing":
            return []
        STAT_KEY.get(name)
        if not g.civ_has(pid, U.EnablesStatProduction) or not any(
                u.p(0) == name for u in g.civ_uniques(pid, U.EnablesStatProduction)):
            out.append(("RequiresTech", f"Converting production to {name} needs the right technology."))
        return out
    if name in R.buildings:
        bd = R.buildings[name]
        if name in city.buildings:
            out.append(("AlreadyBuilt", f"{city.name} already has {name}."))
        for u in bd["_umap"].all:
            if u.ph not in (U.OnlyAvailable, U.CanOnlyBeBuiltWhen) and not applies(u, ctx):
                continue
            ph = u.ph
            if ph == U.Unbuildable and not u.mods:
                out.append(("Unbuildable", f"{name} cannot be built directly."))
            elif ph == U.Unbuildable:
                out.append(("Unbuildable", f"{name} cannot be built directly."))
            elif ph == U.OnlyAvailable:
                out.extend(_not_met(g, u, ctx, pid, False))
            elif ph == U.CanOnlyBeBuiltWhen:
                out.extend(_not_met(g, u, ctx, pid, True))
            elif ph == U.Unavailable:
                out.append(("ShouldNotBeDisplayed", f"{name} is unavailable."))
            elif ph == U.RequiresPopulation and u.n(0) > city.pop:
                out.append(("PopulationRequirement", f"{name} requires {int(u.n(0))} population."))
            elif ph == U.MustBeOn and not tile_terrain_matches(g, city.idx, u.p(0), pid):
                out.append(("MustBeOnTile", f"{name} must be built in a city on {u.p(0)}."))
            elif ph == U.MustNotBeOn and tile_terrain_matches(g, city.idx, u.p(0), pid):
                out.append(("MustNotBeOnTile", f"{name} cannot be built in a city on {u.p(0)}."))
            elif ph == U.MustBeNextTo:
                f = u.p(0)
                ok = tile_matches(g, city.idx, f, pid) or any(tile_matches(g, n, f, pid) for n in g.grid.neighbors(city.idx))
                if f in ("Fresh water", "Fresh Water"):
                    ok = ok or T.fresh_water(g, city.idx)
                if f == "River":
                    ok = ok or g.s.tiles[city.idx].river
                if not ok:
                    out.append(("MustBeNextToTile", f"{name} must be next to {f}."))
            elif ph == U.MustHaveOwnedWithinTiles:
                if not any(tile_matches(g, i, u.p(0), pid) and g.s.tiles[i].owner == pid
                           for i in g.grid.within(city.idx, int(u.n(1)))):
                    out.append(("MustOwnTile", f"{name} requires an owned {u.p(0)} within {u.p(1)} tiles."))
            elif ph == U.ObsoleteWith and g.has_tech(pid, u.p(0)):
                out.append(("Obsoleted", f"{name} is obsolete."))
            elif ph == U.MaxNumberBuildable and count_constructed(g, pid, name, exclude=city) >= u.n(0):
                out.append(("MaxNumberBuildable", f"{name} is limited to {int(u.n(0))}."))
            elif ph == U.SpaceshipPart and not g.civ_has(pid, U.EnablesConstructionOfSpaceshipParts):
                out.append(("RequiresBuildingInSomeCity", "Apollo Program not built."))
        if bd.get("uniqueTo") and bd["uniqueTo"] != p.nation:
            out.append(("UniqueToOtherNation", f"{name} is unique to {bd['uniqueTo']}."))
        if equivalent_building(g, pid, name) != name:
            out.append(("ReplacedByOurUnique", f"Your civilization builds {equivalent_building(g, pid, name)} instead."))
        if bd.get("requiredTech") and not g.has_tech(pid, bd["requiredTech"]):
            out.append(("RequiresTech", f"{name} requires {bd['requiredTech']}."))
        if bd["_any_wonder"]:
            if any(c.id != city.id and name in c.queue for c in g.player_cities(pid)):
                out.append(("WonderBeingBuiltElsewhere", f"{name} is being built in another of your cities."))
            if p.kind == "city_state":
                out.append(("CityStateWonder", "City-states cannot build wonders."))
            if city.puppet:
                out.append(("PuppetWonder", "Puppets cannot build wonders."))
        if bd.get("isWonder") and name in g.s.wonders_built:
            out.append(("WonderAlreadyBuilt", f"{name} has already been built."))
        if bd.get("isNationalWonder") and any(name in c.buildings for c in g.player_cities(pid)):
            out.append(("NationalWonderAlreadyBuilt", f"You already have {name}."))
        if bd.get("requiredBuilding") and not contains_building(g, city, bd["requiredBuilding"]):
            out.append(("RequiresBuildingInThisCity",
                        f"{name} requires a {equivalent_building(g, pid, bd['requiredBuilding'])} in this city."))
        rr = bd.get("requiredResource")
        if rr and g.resource_amount(pid, rr) < 1:
            out.append(("ConsumesResources", f"{name} needs 1 {rr} (you have {g.resource_amount(pid, rr)})."))
        req_near = bd.get("requiredNearbyImprovedResources")
        if req_near:
            ok = False
            for i in city_tiles(g, city):
                t = g.s.tiles[i]
                imp = T.unpillaged_improvement(t)
                if t.resource in req_near and t.owner == pid and imp and (
                        T.resource_improved_by(g, t.resource, imp) or g.city_at(i) is not None):
                    ok = True
                    break
            if not ok:
                out.append(("RequiresNearbyResource", f"{name} needs an improved {' or '.join(req_near)} nearby."))
        for u in g.civ_uniques(pid, U.CannotBuildBuildings, ctx):
            if building_matches(R, name, u.p(0)):
                out.append(("CannotBeBuilt", f"{name} cannot be built."))
        return out
    if name in R.units:
        ud = R.units[name]
        if ud["_domain"] == "Water" and not (T.is_water(g, city.idx) or is_coastal(g, city)):
            out.append(("WaterUnitsInCoastalCities", f"{name} can only be built in coastal cities."))
        for u in ud["_umap"].get(U.OnlyAvailable):
            out.extend(_not_met(g, u, ctx, pid, False))
        for u in ud["_umap"].get(U.CanOnlyBeBuiltWhen):
            out.extend(_not_met(g, u, ctx, pid, True))
        for u in ud["_umap"].matching(U.Unavailable, ctx):
            out.append(("ShouldNotBeDisplayed", f"{name} is unavailable."))
        for u in ud["_umap"].get(U.RequiresPopulation):
            if u.n(0) > city.pop:
                out.append(("PopulationRequirement", f"{name} requires the city to have {int(u.n(0))} population."))
        if ud.get("requiredTech") and not g.has_tech(pid, ud["requiredTech"]):
            out.append(("RequiresTech", f"{name} requires {ud['requiredTech']}."))
        if ud.get("obsoleteTech") and g.has_tech(pid, ud["obsoleteTech"]):
            out.append(("Obsoleted", f"{name} is obsolete ({ud['obsoleteTech']})."))
        if ud.get("uniqueTo") and ud["uniqueTo"] != p.nation:
            out.append(("UniqueToOtherNation", f"{name} is unique to {ud['uniqueTo']}."))
        if equivalent_unit(g, pid, name) != name:
            out.append(("ReplacedByOurUnique", f"Your civilization trains {equivalent_unit(g, pid, name)} instead."))
        if not g.nukes_enabled and ud["_umap"].get(U.NuclearWeapon):
            out.append(("DisabledBySetting", "Nuclear weapons are disabled."))
        if ud["_umap"].has(U.Unbuildable, ctx):
            out.append(("Unbuildable", f"{name} cannot be built."))
        if p.kind == "city_state" and ud["_umap"].get(U.FoundCity):
            out.append(("NoSettlerForOneCityPlayers", "City-states cannot build settlers."))
        for u in ud["_umap"].matching(U.MaxNumberBuildable, ctx):
            if count_constructed(g, pid, name, exclude=city) >= u.n(0):
                out.append(("MaxNumberBuildable", f"{name} is limited to {int(u.n(0))}."))
        if p.kind != "barbarian":
            rr = ud.get("requiredResource")
            if rr and g.resource_amount(pid, rr) < 1:
                out.append(("ConsumesResources", f"{name} needs 1 {rr} (you have {g.resource_amount(pid, rr)} available)."))
            for u in ud["_umap"].matching(U.ConsumesResources, ctx):
                if g.resource_amount(pid, u.p(1)) < u.n(0):
                    out.append(("ConsumesResources", f"{name} needs {int(u.n(0))} {u.p(1)}."))
        for u in g.civ_uniques(pid, U.CannotBuildUnits, ctx):
            if base_unit_matches(R, name, u.p(0)):
                out.append(("CannotBeBuilt", f"{name} cannot be built right now ({u.text})."))
        if ud["_domain"] == "Air":
            from .units import air_capacity_ok
            if not air_capacity_ok(g, city, pid):
                out.append(("NoPlaceToPutUnit", "No room for more aircraft in this city."))
        return out
    return [("Unknown", f"Unknown item '{name}'.")]


def can_build(g: "Game", city: City, name: str) -> Optional[str]:
    """Whether this city can build *name* right now."""
    rr = rejection_reasons(g, city, name)
    return rr[0][1] if rr else None


def buildable_items(g: "Game", city: City) -> dict:
    """Everything this city could build, grouped by kind, with the reasons for what it cannot."""
    out = {"units": [], "buildings": [], "wonders": [], "other": []}
    R = g.rules
    for n in R.units:
        if not rejection_reasons(g, city, n):
            out["units"].append(n)
    for n, b in R.buildings.items():
        if not rejection_reasons(g, city, n):
            out["wonders" if b["_any_wonder"] else "buildings"].append(n)
    for n in ("Gold", "Science"):
        if not rejection_reasons(g, city, n):
            out["other"].append(n)
    return out


# ---------------------------------------------------------------------------------------------------------------
# Purchasing
# ---------------------------------------------------------------------------------------------------------------
def _stat_cost_mod(g, stat: str) -> float:
    """The game-speed modifier for buying with a particular stat."""
    return g.speed["goldCostModifier"] if stat == "Gold" else g.speed.get(f"{stat.lower()}CostModifier", g.speed["modifier"])


def base_gold_cost(g: "Game", pid: int, name: str, city: Optional[City]) -> float:
    """The standard gold price of something, from its production cost.

    The 0.75 exponent is what makes buying expensive things comparatively cheaper than buying cheap
    ones, and is UnCiv's formula rather than a choice made here.
    """
    obj = g.rules.units.get(name) or g.rules.buildings.get(name)
    hurry = obj.get("hurryCostModifier", 0)
    return (30.0 * production_cost(g, pid, name, city)) ** 0.75 * (1 + hurry / 100)


def _increasing(base: int, inc: int, n: int) -> int:
    """The nth price in a sequence that rises quadratically, for things that cost more each time."""
    return int(base + inc / 2 * (n * n + n))


def can_purchase_with(g: "Game", city: City, name: str, stat: str) -> bool:
    """Whether this city may buy *name* with this stat at all.

    Separate from affordability: this answers "is buying a Temple with faith a thing that can happen
    here", which depends on beliefs, policies and the building itself. Note that an item rejected only
    as ``Unbuildable`` can still be purchasable - that is exactly how faith-bought religious units
    work, since they cannot be put in a production queue.
    """
    R = g.rules
    obj = R.units.get(name) or R.buildings.get(name)
    if obj is None or stat in ("Production", "Happiness"):
        return False
    ctx = city_ctx(g, city)
    if obj["_umap"].has(U.CannotBePurchased, ctx):
        return False
    is_unit_ = name in R.units
    match = (lambda f: base_unit_matches(R, name, f)) if is_unit_ else (lambda f: building_matches(R, name, f))
    if is_unit_:
        if any(t != "Unbuildable" for t, _ in rejection_reasons(g, city, name)):
            return False
        if any(u.p(2) == stat and match(u.p(0)) and city_matches(g, city, u.p(3))
               for u in city_uniques(g, city, U.BuyUnitsIncreasingCost, ctx)):
            return True
        if any(u.p(1) == stat and match(u.p(0)) for u in city_uniques(g, city, U.BuyUnitsByProductionCost, ctx)):
            return True
        if any(u.p(1) == stat and match(u.p(0)) and city_matches(g, city, u.p(2))
               for u in city_uniques(g, city, U.BuyUnitsWithStat, ctx)):
            return True
        if any(u.p(2) == stat and match(u.p(0)) and city_matches(g, city, u.p(3))
               for u in city_uniques(g, city, U.BuyUnitsForAmountStat, ctx)):
            return True
    else:
        unique_allowed = any(u.p(0) == stat and applies(u, ctx) and city_matches(g, city, u.p(1))
                             for u in obj["_umap"].get(U.CanBePurchasedWithStat))
        if not unique_allowed and stat == "Gold" and obj["_any_wonder"]:
            return False
        if any(u.p(2) == stat and match(u.p(0)) and city_matches(g, city, u.p(3))
               for u in city_uniques(g, city, U.BuyBuildingsIncreasingCost, ctx)):
            return True
        if any(u.p(1) == stat and match(u.p(0)) for u in city_uniques(g, city, U.BuyBuildingsByProductionCost, ctx)):
            return True
        if any(u.p(1) == stat and match(u.p(0)) and city_matches(g, city, u.p(2))
               for u in city_uniques(g, city, U.BuyBuildingsWithStat, ctx)):
            return True
        if any(u.p(2) == stat and match(u.p(0)) and city_matches(g, city, u.p(3))
               for u in city_uniques(g, city, U.BuyBuildingsForAmountStat, ctx)):
            return True
    # generic (INonPerpetualConstruction.canBePurchasedWithStatReasons)
    if any(u.p(0) == stat and applies(u, ctx) and city_matches(g, city, u.p(1)) for u in obj["_umap"].get(U.CanBePurchasedWithStat)):
        return True
    if any(u.p(1) == stat and applies(u, ctx) and city_matches(g, city, u.p(2)) for u in obj["_umap"].get(U.CanBePurchasedForAmountStat)):
        return True
    return stat == "Gold" and not obj["_umap"].has(U.Unbuildable, ctx)


def buy_cost(g: "Game", city: City, name: str, stat: str = "Gold") -> Optional[int]:
    """What *name* costs in this stat here, or None if it cannot be bought with it.

    Specific prices from uniques win over the generic formula, and the cheapest applies when several
    offer a price - a player with two sources of discount should get the better one rather than the
    first one found. Discounts multiply afterwards, and the result is rounded down to ten so that
    prices read as prices.
    """
    R = g.rules
    pid = city.owner
    p = g.player(pid)
    obj = R.units.get(name) or R.buildings.get(name)
    if obj is None:
        return None
    ctx = city_ctx(g, city)
    is_unit_ = name in R.units
    match = (lambda f: base_unit_matches(R, name, f)) if is_unit_ else (lambda f: building_matches(R, name, f))
    pre = "Units" if is_unit_ else "Buildings"
    specific = []
    for u in city_uniques(g, city, getattr(U, f"Buy{pre}IncreasingCost"), ctx):
        if u.p(2) == stat and match(u.p(0)) and city_matches(g, city, u.p(3)):
            specific.append(_increasing(int(u.n(1)), int(u.n(4)), p.bought_increasing.get(name, 0)) * _stat_cost_mod(g, stat))
    for u in city_uniques(g, city, getattr(U, f"Buy{pre}ByProductionCost"), ctx):
        if u.p(1) == stat and match(u.p(0)):
            specific.append(production_cost(g, pid, name, city) * u.n(2))
    if any(u.p(1) == stat and match(u.p(0)) and city_matches(g, city, u.p(2)) for u in city_uniques(g, city, getattr(U, f"Buy{pre}WithStat"), ctx)):
        from .research import player_era
        specific.append(R.eras[R.era_list[player_era(g, pid)]]["baseUnitBuyCost"] * _stat_cost_mod(g, stat))
    for u in city_uniques(g, city, getattr(U, f"Buy{pre}ForAmountStat"), ctx):
        if u.p(2) == stat and match(u.p(0)) and city_matches(g, city, u.p(3)):
            specific.append(u.n(1) * _stat_cost_mod(g, stat))
    if specific:
        cost = min(specific)
    else:
        lows = [u for u in obj["_umap"].matching(U.CanBePurchasedForAmountStat, ctx) if u.p(1) == stat and city_matches(g, city, u.p(2))]
        if lows:
            cost = min(u.n(0) for u in lows) * _stat_cost_mod(g, stat)
        elif stat == "Gold":
            cost = base_gold_cost(g, pid, name, city)
        elif any(u.p(0) == stat and city_matches(g, city, u.p(1)) for u in obj["_umap"].matching(U.CanBePurchasedWithStat, ctx)):
            from .research import player_era
            cost = R.eras[R.era_list[player_era(g, pid)]]["baseUnitBuyCost"] * _stat_cost_mod(g, stat)
        else:
            return None
    if is_unit_:
        for u in city_uniques(g, city, U.BuyUnitsDiscount, ctx):
            if u.p(0) == stat and match(u.p(1)):
                cost *= 1 + u.n(2) / 100
        for u in city_uniques(g, city, U.BuyItemsDiscount, ctx):
            if u.p(0) == stat:
                cost *= 1 + u.n(1) / 100
    else:
        for u in city_uniques(g, city, U.BuyItemsDiscount, ctx):
            if u.p(0) == stat:
                cost *= 1 + u.n(1) / 100
        for u in city_uniques(g, city, U.BuyBuildingsDiscount, ctx):
            if u.p(0) == stat and match(u.p(1)):
                cost *= 1 + u.n(2) / 100
    return int(cost / 10) * 10


def purchase_check(g: "Game", city: City, name: str, stat: str = "Gold") -> tuple[Optional[str], Optional[int]]:
    """Whether this purchase can go ahead: ``(reason or None, cost or None)``.

    Returning the cost alongside the refusal is deliberate - "costs 340 gold; you have 210" tells the
    player how long to wait, which "you cannot afford that" does not.

    The occupied-tile check exists because a bought unit appears in the city, and a city that already
    holds a unit of that kind has nowhere to put it. Saying so beforehand is better than taking the
    gold and failing.
    """
    R = g.rules
    if name not in R.units and name not in R.buildings:
        return f"Unknown item '{name}'.", None
    if city.puppet:
        return "Puppet cities cannot purchase.", None
    if city.resistance > 0:
        return f"{city.name} is in resistance.", None
    rr = rejection_reasons(g, city, name)
    if any(t != "Unbuildable" for t, _ in rr):
        return next(m for t, m in rr if t != "Unbuildable"), None
    if name in R.units:
        ud = R.units[name]
        if ud["_domain"] != "Air":
            occ = g.civilian_at(city.idx) if not ud["_military"] else g.military_at(city.idx)
            if occ is not None:
                return f"Move the unit out of {city.name} first: a bought {name} appears in the city.", None
    if not can_purchase_with(g, city, name, stat):
        return f"{name} cannot be bought with {stat}.", None
    cost = buy_cost(g, city, name, stat)
    if cost is None:
        return f"{name} cannot be bought with {stat}.", None
    have = g.player(city.owner).gold if stat == "Gold" else g.stat_reserve(city.owner, STAT_KEY.get(stat, stat.lower()))
    if have < cost:
        return f"{name} costs {cost} {stat}; you have {int(have)}.", cost
    return None, cost


def purchase(g: "Game", city: City, name: str, stat: str = "Gold") -> dict:
    """Buy something outright, place it, and take the payment.

    Order matters: the unit is placed *before* the payment is taken, because placement can fail - the
    tile may be full - and a purchase that charges for a unit it could not deliver would be the worse
    failure.
    """
    R = g.rules
    name = resolve_item(g, name, city.owner)
    reason, cost = purchase_check(g, city, name, stat)
    if reason:
        raise ActionError(reason)
    p = g.player(city.owner)
    if not complete_construction(g, city, name, bought_with=stat):
        raise ActionError("No room to place the unit.")
    if stat == "Gold":
        p.gold -= cost
    else:
        g.add_stat(city.owner, STAT_KEY.get(stat, stat.lower()), -cost)
    ctx = city_ctx(g, city)
    inc_ph = U.BuyUnitsIncreasingCost if name in R.units else U.BuyBuildingsIncreasingCost
    match = (lambda f: base_unit_matches(R, name, f)) if name in R.units else (lambda f: building_matches(R, name, f))
    if any(match(u.p(0)) and city_matches(g, city, u.p(3)) and u.p(2) == stat for u in city_uniques(g, city, inc_ph, ctx)):
        p.bought_increasing[name] = p.bought_increasing.get(name, 0) + 1
    if name in city.queue and name in R.buildings:
        city.queue.remove(name)
    city.bought_this_turn.append(name)
    validate_queue(g, city)
    return {"bought": name, "cost": cost, "stat": stat, "left": int(p.gold if stat == "Gold" else g.stat_reserve(city.owner, STAT_KEY.get(stat, stat.lower())))}


# ---------------------------------------------------------------------------------------------------------------
# Queue & production
# ---------------------------------------------------------------------------------------------------------------
def resolve_item(g: "Game", item: str, pid: Optional[int] = None) -> str:
    """Turn a name a human or model typed into the exact ruleset name.

    Case-insensitive, and it maps a standard name to the civilization's unique replacement, so "build a
    library" works for a civilization whose library is called something else.
    """
    R = g.rules
    if item in PERPETUAL:
        return item
    for kind in ("unit", "building"):
        n = R.resolve(kind, item)
        if n:
            if pid is not None:
                n = equivalent_unit(g, pid, n) if kind == "unit" else equivalent_building(g, pid, n)
            return n
    low = str(item).strip().lower()
    for n in PERPETUAL:
        if n.lower() == low:
            return n
    raise ActionError(f"Unknown item '{item}'. Use unit or building names like 'Warrior' or 'Granary'.")


def work_done(city: City, name: str) -> float:
    """Production already invested in *name* in this city."""
    return city.progress.get(name, 0.0)


def remaining_work(g: "Game", city: City, name: str) -> float:
    """Production still needed to finish *name*."""
    if name in PERPETUAL:
        return 0
    return production_cost(g, city.owner, name, city) - work_done(city, name)


def turns_to_build(g: "Game", city: City, name: str) -> Optional[int]:
    """Turns to finish at the current rate, or None when it will never finish."""
    left = remaining_work(g, city, name)
    if left <= 0:
        return 0
    if left <= city.overflow:
        return 1
    prod = city_stats(g, city, name if name != current_construction(city) else "__current__")["production"] \
        if name != current_construction(city) else city_stats(g, city)["production"]
    prod = max(1, round(prod))
    return math.ceil((left - city.overflow) / prod)


def validate_queue(g: "Game", city: City):
    """Drop anything from the queue that can no longer be built.

    A wonder somebody else completed, a unit made obsolete, a building whose resource was traded away:
    all of them silently vanish from the queue rather than blocking it.
    """
    new = []
    for n in city.queue:
        if not rejection_reasons(g, city, n):
            new.append(n)
    city.queue = new


def set_production(g: "Game", city: City, item: str, append: bool = False) -> dict:
    """Put an item at the front of the queue, or append it to the end.

    A perpetual item - converting production to gold or science - is kept last when appending, because
    nothing after it would ever be reached.
    """
    name = resolve_item(g, item, city.owner)
    rr = rejection_reasons(g, city, name)
    if rr:
        raise ActionError(rr[0][1])
    if name in g.rules.buildings and name in city.queue and not (not append and city.queue[0] == name):
        if not append:
            city.queue.remove(name)
        else:
            raise ActionError(f"{name} is already queued in {city.name}.")
    if append:
        if len(city.queue) >= QUEUE_MAX:
            raise ActionError(f"The production queue is full ({QUEUE_MAX} items).")
        if city.queue and city.queue[-1] in PERPETUAL:
            city.queue.insert(len(city.queue) - 1, name)
        else:
            city.queue.append(name)
    else:
        if city.queue and city.queue[0] == name:
            pass
        else:
            city.queue.insert(0, name)
        city.queue = city.queue[:QUEUE_MAX]
    g.invalidate_city(city)
    return {"city": city.name, "queue": list(city.queue),
            "turns": turns_to_build(g, city, city.queue[0]) if city.queue[0] not in PERPETUAL else None}


QUEUE_ACTIONS = ("up", "down", "first", "last", "remove", "clear")


def change_queue(g: "Game", city: City, index: int, action: str) -> dict:
    """Move, remove or clear entries in the production queue."""
    action = (action or "").lower()
    if action not in QUEUE_ACTIONS:
        raise ActionError(f"Action must be one of {', '.join(QUEUE_ACTIONS)}.")
    q = city.queue
    if action == "clear":
        q.clear()
    else:
        if not q:
            raise ActionError(f"{city.name}'s production queue is empty.")
        if not isinstance(index, int) or not 0 <= index < len(q):
            raise ActionError(f"Queue position must be 0-{len(q) - 1} (0 is the item in production).")
        item = q.pop(index)
        if action == "up":
            q.insert(max(0, index - 1), item)
        elif action == "down":
            q.insert(min(len(q), index + 1), item)
        elif action == "first":
            q.insert(0, item)
        elif action == "last":
            q.append(item)
    g.invalidate_city(city)
    return {"city": city.name, "queue": list(q)}


def auto_pick_production(g: "Game", city: City) -> Optional[str]:
    """Pick the next construction with the built-in advisor. Puppets (like UnCiv's) only build buildings, or gold."""
    from ..bots.basic import BasicBot
    bot = BasicBot(aggression=0.25, seed=city.id)
    try:
        if city.puppet:
            items = buildable_items(g, city)
            ctx = bot.context(g, city.owner)
            scored = [(bot._building_value(g, city.owner, city, b, ctx), b) for b in items["buildings"]]
            scored = [x for x in scored if x[0] > 0]
            item = max(scored)[1] if scored else ("Gold" if "Gold" in items.get("other", []) else None)
        else:
            item = bot.advise_production(g, city.owner, city)
    except Exception:
        return None
    if not item:
        return None
    try:
        set_production(g, city, item)
    except ActionError:
        return None
    return city.queue[0]


def construct_if_enough(g: "Game", city: City):
    """Start-of-turn completion (CityConstructions.constructIfEnough)."""
    validate_queue(g, city)
    _validate_progress(g, city)
    name = current_construction(city)
    if name is None or name in PERPETUAL:
        return
    cost = production_cost(g, city.owner, name, city)
    done = work_done(city, name)
    if done >= cost:
        overflow = done - cost
        if complete_construction(g, city, name):
            max_over = max(cost, round(city_stats(g, city)["production"]))
            city.overflow = min(max_over, overflow)
        else:
            g.emit("production_blocked", f"No room to place a {name} near {city.name}.", [city.owner], idx=city.idx)
        p = g.player(city.owner)
        p.built_increasing[name] = p.built_increasing.get(name, 0) + 1


def _validate_progress(g: "Game", city: City):
    """Deal with stored production for things that can no longer be built.

    Not simply discarded, because that production was real. A wonder somebody else finished refunds its
    stored production as gold - UnCiv's rule, and the one that stops a lost wonder race from being a
    catastrophe - and an obsolete unit's progress transfers to whatever it upgrades into.
    """
    cur = current_construction(city)
    for name in list(city.progress):
        if name == cur:
            continue
        if name not in g.rules.units and name not in g.rules.buildings:
            del city.progress[name]
            continue
        rr = rejection_reasons(g, city, name)
        if not any(t in ("Obsoleted", "WonderAlreadyBuilt", "NationalWonderAlreadyBuilt", "CannotBeBuiltWith",
                         "MaxNumberBuildable") for t, _ in rr):
            continue
        done = int(city.progress[name])
        if name in g.rules.buildings:
            if g.rules.buildings[name].get("isWonder") and done:
                g.player(city.owner).gold += done
                g.emit("wonder_refund", f"Excess production for {name} converted to {done} gold in {city.name}.",
                       [city.owner], idx=city.idx)
        else:
            up = g.rules.units[name].get("upgradesTo")
            if up and all(t == "Obsoleted" for t, _ in rr):
                up = equivalent_unit(g, city.owner, up)
                if not rejection_reasons(g, city, up):
                    city.progress[up] = city.progress.get(up, 0) + done
        del city.progress[name]


def end_turn_production(g: "Game", city: City, production: float):
    """CityConstructions.endTurn: add this turn's production (plus overflow) to the current construction."""
    validate_queue(g, city)
    _validate_progress(g, city)
    name = current_construction(city)
    if name is None:
        return
    if name in PERPETUAL:
        city.overflow += round(production)
        return
    if work_done(city, name) == 0:
        _construction_begun(g, city, name)
    city.progress[name] = city.progress.get(name, 0) + round(production) + city.overflow
    city.overflow = 0


def _construction_begun(g, city, name):
    """Announce the start of something that other civilizations notice, such as a wonder."""
    obj = g.rules.units.get(name) or g.rules.buildings.get(name)
    if obj and obj["_umap"].has_tag(U.TriggersAlertOnStart):
        g.emit("wonder_started", f"{g.player(city.owner).name} has started constructing {name}!", None, idx=city.idx,
               item=name, player=city.owner)


def complete_construction(g: "Game", city: City, name: str, bought_with: Optional[str] = None) -> bool:
    """Finish something: add the building or place the unit, and announce it.

    Returns False when a unit cannot be placed, which is the caller's signal not to charge for it. A
    bought unit normally cannot move on the turn it appears, which is what stops gold from being
    converted directly into a surprise attack.
    """
    R = g.rules
    if name in R.buildings:
        add_building(g, city, name)
        if R.buildings[name].get("isWonder"):
            g.s.wonders_built[name] = city.id
            g.emit("wonder_built", f"{g.player(city.owner).name} has built {name} in {city.name}.", None,
                   idx=city.idx, item=name, player=city.owner)
        else:
            g.emit("building_built", f"{city.name} completed {name}.", [city.owner], idx=city.idx, item=name,
                   player=city.owner)
        if R.buildings[name]["_umap"].has_tag(U.TriggersAlertOnCompletion) and not R.buildings[name].get("isWonder"):
            g.emit("wonder_built", f"{g.player(city.owner).name} has completed {name}!", None, idx=city.idx, item=name,
                   player=city.owner)
    else:
        from . import units as unitmod
        u = unitmod.add_unit_in_city(g, city, name)
        if u is None:
            return False
        if bought_with is not None and not R.units[name]["_umap"].has_tag(U.CanMoveImmediatelyOnceBought):
            u.moves = 0
        unitmod.add_construction_bonuses(g, u, city)
        if R.units[name]["_umap"].has_tag(U.SpaceshipPart):
            pass
        g.emit("unit_built", f"{city.name} trained a {name}.", [city.owner], idx=city.idx, unit=u.id, item=name,
               player=city.owner)
    city.progress.pop(name, None)
    if city.queue and city.queue[0] == name:
        city.queue.pop(0)
    validate_queue(g, city)
    g.invalidate_city(city)
    return True


def add_building(g: "Game", city: City, name: str, try_free: bool = True):
    """Add a building to a city and apply everything that follows from it."""
    R = g.rules
    bd = R.buildings[name]
    if bd.get("cityHealth"):
        mx = max_health(g, city)
        city.health += int(bd["cityHealth"] * city.health / max(1, mx))
    if name not in city.buildings:
        city.buildings.append(name)
    g.invalidate()
    if bd["_umap"].has_tag(U.IndicatesCapital):
        g.player(city.owner).capital = city.id
    # one-time triggers on the building
    from . import triggers
    ctx = city_ctx(g, city)
    for u in bd["_umap"].all:
        if triggers.has_trigger_conditional(u) or not applies(u, ctx):
            continue
        triggers.trigger(g, u, city.owner, city=city, note=f"due to constructing {name}")
    # Korean unique
    if "science" in bd["_stat_related"] and is_capital(g, city) and g.civ_has(city.owner, U.TechBoostWhenScientificBuildingsBuiltInCapital):
        from . import research
        research.research_agreement_boost(g, city.owner)
    assign_citizens(g, city)
    if try_free:
        try_add_free_buildings(g, city.owner)


def remove_building(g: "Game", city: City, name: str):
    """Remove a building, as when a city is conquered or sold."""
    if name in city.buildings:
        city.buildings.remove(name)
    g.invalidate()


def max_health(g: "Game", city: City) -> int:
    """The city's maximum hit points, from its population and defensive buildings."""
    return 200 + sum(g.rules.buildings[b].get("cityHealth", 0) for b in city.buildings)


# ---------------------------------------------------------------------------------------------------------------
# Free buildings (CivConstructions)
# ---------------------------------------------------------------------------------------------------------------
def _add_free(g, pid, city, name):
    """Record a building as free in this city, so it costs no maintenance."""
    lst = g.player(pid).free_buildings.setdefault(str(city.id), [])
    if name not in lst:
        lst.append(name)


def cheapest_stat_building(g: "Game", city: City, stat: str) -> Optional[str]:
    """The cheapest building in this city that produces a given stat.

    Used by the uniques that grant "a free building that produces culture": the cheapest is the
    conventional interpretation, and picking the most expensive would make some of them absurd.
    """
    k = STAT_KEY.get(stat, stat.lower())
    best = None
    for n, b in g.rules.buildings.items():
        if b["_any_wonder"] or k not in b["_stat_related"]:
            continue
        if rejection_reasons(g, city, n) and n not in city.queue:
            continue
        if best is None or b["cost"] < g.rules.buildings[best]["cost"]:
            best = n
    return best


def add_free_stat_buildings(g: "Game", pid: int, stat: str, amount: int):
    """Grant free buildings producing a stat, spread across the civilization's cities."""
    p = g.player(pid)
    for c in g.player_cities(pid)[:amount]:
        if [stat, c.id] in p.free_stat_buildings:
            continue
        b = cheapest_stat_building(g, c, stat)
        if b is None:
            continue
        p.free_stat_buildings.append([stat, c.id])
        _add_free(g, pid, c, b)
        complete_construction(g, c, b)


def add_free_specific_buildings(g: "Game", pid: int, building: str, amount: int):
    """Grant a specific free building in several cities."""
    p = g.player(pid)
    b = equivalent_building(g, pid, building)
    for c in g.player_cities(pid)[:amount]:
        if [b, c.id] in p.free_specific_buildings or contains_building(g, c, building):
            continue
        p.free_specific_buildings.append([b, c.id])
        _add_free(g, pid, c, b)
        complete_construction(g, c, b)


def try_add_free_buildings(g: "Game", pid: int):
    """Hand out any free buildings the civilization is owed but has not yet received.

    Run repeatedly rather than once, because entitlements arrive at odd times - a policy adopted, a
    belief enhanced, a city conquered - and a city that did not exist when the entitlement appeared
    still deserves its building.
    """
    from . import triggers
    stat_amounts: dict = {}
    for u in g.civ_uniques(pid, U.FreeStatBuildings):
        if not triggers.has_trigger_conditional(u):
            stat_amounts[u.p(0)] = stat_amounts.get(u.p(0), 0) + int(u.n(1))
    for stat, amt in stat_amounts.items():
        add_free_stat_buildings(g, pid, stat, amt)
    spec: dict = {}
    for u in g.civ_uniques(pid, U.FreeSpecificBuildings):
        if not triggers.has_trigger_conditional(u):
            spec[u.p(0)] = spec.get(u.p(0), 0) + int(u.n(1))
    for b, amt in spec.items():
        add_free_specific_buildings(g, pid, b, amt)
    civ_free = [u for m in g.civ_umaps(pid) for u in m.get(U.GainFreeBuildings)]
    for c in list(g.player_cities(pid)):
        ctx = city_ctx(g, c)
        loc = [u for m in local_umaps(g, c) for u in m.get(U.GainFreeBuildings)]
        for u in civ_free + loc:
            if triggers.has_trigger_conditional(u) or not city_matches(g, c, u.p(1)) or not applies(u, ctx):
                continue
            b = equivalent_building(g, pid, u.p(0))
            _add_free(g, pid, c, b)
            if not contains_building(g, c, b):
                complete_construction(g, c, b)


# ---------------------------------------------------------------------------------------------------------------
# Capital connections (CapitalConnectionsFinder)
# ---------------------------------------------------------------------------------------------------------------
def _can_enter_borders(g, pid, other) -> bool:
    """Whether a trade route may pass through another civilization's territory."""
    if other == pid:
        return True
    if g.is_barbarian(other) or g.is_barbarian(pid):
        return False
    if not g.has_met(pid, other):
        return False
    if g.player(other).kind == "city_state" and not g.at_war(pid, other):
        return True
    return g.has_open_borders(other, pid)


def _has_connection(g, pid, idx) -> bool:
    """Whether a tile carries a road, railway or river that a trade route can use."""
    t = g.s.tiles[idx]
    if T.unpillaged_route(t):
        return True
    if g.civ_has(pid, U.ForestsAndJunglesAreRoads) and t.owner == pid and t.feature in ("Forest", "Jungle") \
            and g.has_tech(pid, "The Wheel"):
        return True
    return False


def connected_cities(g: "Game", pid: int) -> dict:
    """city id -> set of mediums ("road", "railroad", "harbor") connecting it to the capital."""
    key = ("connected", pid)
    v = g._ycache.get(key)
    if v is not None:
        return v
    p = g.player(pid)
    cap = g.city(p.capital) if p.capital is not None else None
    out: dict = {}
    if cap is None or cap.owner != pid:
        g._ycache[key] = out
        return out
    R = g.rules
    road_ok = g.has_tech(pid, R.improvements["Road"].get("techRequired"))
    rail_ok = g.has_tech(pid, R.improvements["Railroad"].get("techRequired"))
    all_cities = {c.idx: c for c in g.s.cities.values() if _can_enter_borders(g, pid, c.owner)}

    def harbor(c):
        """Whether this city has a harbour, which connects it to others across water."""
        return any(R.buildings[b]["_umap"].has_tag(U.ConnectTradeRoutes) for b in c.buildings)

    def bfs(start, ok):
        """Walk outward from the capital, recording which cities are reachable and by what."""
        seen = {start}
        stack = [start]
        while stack:
            cur = stack.pop()
            for n in g.grid.neighbors(cur):
                if n in seen:
                    continue
                t = g.s.tiles[n]
                if (n in all_cities or ok(n)) and (t.owner is None or _can_enter_borders(g, pid, t.owner)):
                    seen.add(n)
                    stack.append(n)
        return seen

    out[cap.id] = {"start"}
    frontier = [cap]
    done_bfs = set()
    while frontier:
        nxt = []
        for c in frontier:
            meds = out[c.id]
            checks = []
            if harbor(c):
                checks.append(("harbor", lambda i: T.is_water(g, i), lambda x: x.owner == pid and harbor(x)))
            if rail_ok and ("start" in meds or "railroad" in meds):
                checks.append(("railroad", lambda i: T.unpillaged_route(g.s.tiles[i]) == "Railroad", lambda x: True))
            if road_ok:
                checks.append(("road", lambda i: _has_connection(g, pid, i), lambda x: True))
            for medium, ok, cfilter in checks:
                if (c.id, medium) in done_bfs:
                    continue
                done_bfs.add((c.id, medium))
                reached = bfs(c.idx, ok)
                for idx, rc in all_cities.items():
                    if idx in reached and cfilter(rc):
                        if rc.id not in out:
                            out[rc.id] = set()
                            nxt.append(rc)
                        out[rc.id].add(medium)
        frontier = nxt
    out = {cid: m for cid, m in out.items() if g.city(cid) is not None and g.city(cid).owner == pid}
    g._ycache[key] = out
    return out


def connected_to_capital(g: "Game", city: City, rail: bool = False) -> bool:
    """Whether this city has a trade route to the capital, optionally requiring a railway."""
    if len(g.player_cities(city.owner)) < 2:
        return False
    meds = connected_cities(g, city.owner).get(city.id)
    if meds is None:
        return False
    if rail:
        return "railroad" in meds
    return True


# ---------------------------------------------------------------------------------------------------------------
# Founding
# ---------------------------------------------------------------------------------------------------------------
def found_check(g: "Game", pid: int, idx: int) -> Optional[str]:
    """Why a city cannot be founded here, or None if it can."""
    t = g.s.tiles[idx]
    if T.is_water(g, idx) or T.is_impassable(g, idx):
        return "Cities must be founded on land (not mountains, ice or natural wonders)."
    if t.owner is not None and t.owner != pid:
        return "That tile belongs to another civilization."
    k = g.rules.k
    for c in g.s.cities.values():
        same = g.continent(c.idx) == g.continent(idx)
        md = k["minimal_city_distance"] if same else k["minimal_city_distance_other_continents"]
        if g.grid.distance(c.idx, idx) <= md:
            return f"Too close to {c.name} (at least {md} tiles must lie between cities)."
    if t.improvement == "Barbarian encampment":
        return "Clear the barbarian camp first."
    return None


def clean_name(name, limit: int = 40) -> str:
    """Strip a player-supplied name to plain text of a sane length.

    Names reach other players' screens and the game log, so this is the boundary where a name stops
    being arbitrary input.
    """
    text = re.split(r"[<>\n\r{}]", str(name or ""))[0]
    return " ".join(text.split())[:limit].strip(" \"'")


def new_city_name(g: "Game", pid: int, name: Optional[str]) -> str:
    """Pick a name for a new city: the one asked for, or the next from the civilization's list."""
    used = {c.name.lower() for c in g.s.cities.values()}
    if name:
        name = clean_name(name)
        if name and name.lower() not in used:
            return name
        if name:
            for i in range(2, 100):
                if f"{name} {i}".lower() not in used:
                    return f"{name} {i}"
    p = g.player(pid)
    nd = g.rules.nations.get(p.nation, {})
    for n in nd.get("cities", []):
        if n.lower() not in used:
            return n
    if g.civ_has(pid, U.BorrowsCityNames):
        for q in g.s.players:
            for n in g.rules.nations.get(q.nation, {}).get("cities", []):
                if n.lower() not in used:
                    return n
    for prefix in ("New", "Neo", "Nova", "Altera"):
        for n in nd.get("cities", []):
            cand = f"{prefix} {n}"
            if cand.lower() not in used:
                return cand
    p.city_counter += 1
    return f"{p.name} City {p.city_counter}"


def found_city(g: "Game", pid: int, idx: int, name: Optional[str] = None, unit=None) -> City:
    """Found a city: claim its tiles, give it its first buildings, and tell everyone who can see."""
    reason = found_check(g, pid, idx)
    if reason:
        raise ActionError(reason)
    p = g.player(pid)
    t = g.s.tiles[idx]
    first = not p.founded_city and p.kind == "major"
    city = City(id=g.new_id(), name=new_city_name(g, pid, name), owner=pid, idx=idx, founded_turn=g.turn,
                founder=pid, turn_acquired=g.turn)
    city.original_capital = first or (p.kind == "city_state" and not g.player_cities(pid))
    g.s.cities[city.id] = city
    # remove removable features (any "Remove X" improvement exists)
    for f in list(t.features):
        if f"Remove {f}" in g.rules.improvements:
            t.features.remove(f)
    t.improvement = "City center" if "City center" in g.rules.improvements else None
    t.pillaged = False
    g.clear_static()
    # borders: centre + ring 1 (unowned or ours)
    t.owner = pid
    t.city = city.id
    for n in g.grid.neighbors(idx):
        nt = g.s.tiles[n]
        if nt.city is None and (nt.owner is None or nt.owner == pid):
            if nt.improvement == "Barbarian encampment":
                from . import barbarians
                barbarians.remove_camp(g, n)
            nt.owner = pid
            nt.city = city.id
    g.invalidate()
    start_era = g.s.config.get("starting_era", "Ancient era")
    city.pop = g.rules.eras[start_era]["settlerPopulation"]
    city.health = 200
    if p.religion_state == "pantheon" and p.religion:
        city.pressures[p.religion] = city.pressures.get(p.religion, 0) + 200 * city.pop
    if not g.player_cities(pid)[:-1] or p.capital is None or g.city(p.capital) is None:
        add_building(g, city, capital_indicator(g, pid), try_free=False)
        p.capital = city.id
        if first:
            p.original_capital = city.id
    for b in g.rules.eras[start_era].get("settlerBuildings", []):
        eb = equivalent_building(g, pid, b)
        if eb in g.rules.buildings and not rejection_reasons(g, city, eb):
            add_building(g, city, eb, try_free=False)
    p.founded_city = True
    assign_citizens(g, city)
    try_add_free_buildings(g, pid)
    from . import triggers
    triggers.fire(g, pid, U.TriggerUponFoundingCity, city=city, unit=unit, note="due to founding a city")
    if unit is not None and g.unit(unit.id) is not None:
        g.remove_unit(unit)          # "Founds a new city <by consuming this unit>"
    g.invalidate()
    g.emit("city_founded", f"{p.name} founded {city.name} at {g.fmt_xy(idx)}.", [pid], idx=idx, city=city.id)
    return city


def capital_indicator(g: "Game", pid: int) -> str:
    """The marker shown beside a capital's name."""
    for n, b in g.rules.buildings.items():
        if b["_umap"].has_tag(U.IndicatesCapital) and not b.get("uniqueTo"):
            return equivalent_building(g, pid, n)
    return "Palace"


def rename_city(g: "Game", city: City, name: str) -> str:
    """Rename a city, after cleaning the name and checking it is not taken."""
    used = {c.name.lower() for c in g.s.cities.values() if c.id != city.id}
    name = clean_name(name)
    if not name:
        raise ActionError("City names must contain letters.")
    if name.lower() in used:
        raise ActionError(f"There is already a city called {name}.")
    city.name = name
    return name


# ---------------------------------------------------------------------------------------------------------------
# Turn processing (CityTurnManager)
# ---------------------------------------------------------------------------------------------------------------
def start_turn(g: "Game", city: City):
    """Begin a city's turn: resistance, we-love-the-king, and the flags for what happens next."""
    construct_if_enough(g, city)
    city.attacked = False
    city.bought_this_turn = []
    if city.wltkd <= 0 and city.demanded_resource:
        if g.resource_amount(city.owner, city.demanded_resource) > 0:
            city.wltkd = int(round(20 * g.speed["modifier"])) + 1
            city.demand_countdown = 0
            g.emit("wltkd", f"Because they have {city.demanded_resource}, the citizens of {city.name} are celebrating "
                            f"We Love The King Day!", [city.owner], idx=city.idx)
    _next_turn_flags(g, city)
    if city.puppet:
        city.focus = "gold"
        assign_citizens(g, city, reset=True)
    else:
        assign_citizens(g, city)
    if not city.queue and g.player(city.owner).kind == "major":
        picked = auto_pick_production(g, city) if city.puppet or city.auto_production else None
        if picked and not city.puppet:
            g.emit("city_auto_production", f"{city.name} started {picked} (automatic production).", [city.owner],
                   idx=city.idx)
        elif not picked and g.player(city.owner).controller not in BOT_MANAGED:   # bots choose during their turn
            g.emit("city_idle", f"{city.name} has nothing to produce.", [city.owner], idx=city.idx)
    if not city.demanded_resource and city.demand_countdown <= 0 and city.wltkd <= 0:
        _set_demand_cooldown(g, city, True)


def _set_demand_cooldown(g, city, new_city):
    """Set how long before this city may demand a luxury resource again."""
    rng = g.state_rng("demand", city.id, g.turn)
    d = 15 + rng.randrange(10)
    if new_city and is_capital(g, city):
        d += 10
    city.demand_countdown = d


def _next_turn_flags(g, city):
    """Work out what this city will announce at the start of the next turn."""
    if city.demand_countdown > 0:
        city.demand_countdown -= 1
        if city.demand_countdown == 0 and city.wltkd <= 0:
            _demand_new_resource(g, city)
    if city.wltkd > 0:
        city.wltkd -= 1
        if city.wltkd == 0:
            g.emit("wltkd_end", f"We Love The King Day in {city.name} has ended.", [city.owner], idx=city.idx)
            _demand_new_resource(g, city)
    if city.resistance > 0:
        city.resistance -= 1
        if city.resistance == 0:
            g.emit("resistance_end", f"The resistance in {city.name} has ended!", [city.owner], idx=city.idx)


def _demand_new_resource(g, city):
    """Pick a luxury the city wants, starting a we-love-the-king day if it is supplied."""
    R = g.rules
    rng = g.state_rng("demand_new", city.id, g.turn)
    on_map = g.resources_on_map()
    near = {g.s.tiles[i].resource for i in g.grid.within(city.idx, work_range(g))}
    cands = [r for r, d in R.resources.items() if d["resourceType"] == "Luxury"
             and not d["_umap"].has_tag(U.CityStateOnlyResource) and r != city.demanded_resource
             and r in on_map and r not in near]
    missing = [r for r in cands if g.resource_amount(city.owner, r) <= 0]
    if not missing:
        city.demanded_resource = rng.choice(cands) if cands else None
        return
    city.demanded_resource = rng.choice(sorted(missing))
    _set_demand_cooldown(g, city, False)
    g.emit("city_demand", f"{city.name} demands {city.demanded_resource}!", [city.owner], idx=city.idx)


def end_turn(g: "Game", city: City):
    """End a city's turn: production, growth, borders, and everything that accrues."""
    stats = city_stats(g, city)
    total = dict(stats["total"])
    end_turn_production(g, city, total["production"])
    # borders
    city.culture += int(total["culture"])
    cost = culture_to_next_tile(g, city)
    if city.culture >= cost:
        claimed = expand_borders(g, city)
        if claimed is not None:
            city.culture -= cost
            g.emit("borders", f"{city.name} has expanded its borders.", [city.owner], idx=city.idx)
    # razing / growth
    if city.razing:
        removed = 1 + sum(int(u.n(0)) - 1 for u in g.civ_uniques(city.owner, U.CitiesAreRazedXTimesFaster))
        if city.pop <= removed:
            g.emit("city_razed", f"{city.name} has been razed to the ground!", None, idx=city.idx)
            destroy_city(g, city)
            return
        add_population(g, city, -removed)
        thr = food_to_next_pop(g, city)
        if city.food >= thr:
            city.food = thr - 1
    else:
        _grow(g, city, int(round(total["food"])))
    if g.religion_enabled:
        from . import religion
        religion.city_end_turn(g, city)
    if city.id in g.s.cities:
        city.health = min(max_health(g, city), city.health + 20)
        _unassign_extra(g, city)
    g.invalidate_city(city)


def _grow(g, city, food: int):
    """Add or remove a citizen when the food box fills or empties."""
    city.food += food
    if food < 0:
        g.emit("city_starving", f"{city.name} is starving!", [city.owner], idx=city.idx)
    if city.food < 0:
        if city.pop > 1:
            add_population(g, city, -1)
        city.food = 0
    need = food_to_next_pop(g, city)
    if city.food < need:
        return
    if city_uniques(g, city, U.NullifiesGrowth):
        return
    if city.avoid_growth:
        city.food = need
        return
    city.food -= need
    carry = 0
    for u in city_uniques(g, city, U.CarryOverFood):
        if city_matches(g, city, u.p(1)):
            carry += int(u.n(0))
    carry = min(carry, 95)
    city.food += int(need * carry / 100)
    add_population(g, city, 1)
    g.emit("city_growth", f"{city.name} grew to size {city.pop}.", [city.owner], idx=city.idx)


# ---------------------------------------------------------------------------------------------------------------
# Destruction (capture lives in conquest.py)
# ---------------------------------------------------------------------------------------------------------------
def destroy_city(g: "Game", city: City):
    """Remove a city from the game, releasing its tiles and units."""
    owner = city.owner
    for i in g.grid.within(city.idx, g.rules.k["city_expand_range"]):
        t = g.s.tiles[i]
        if t.city == city.id:
            t.city = None
            t.owner = None
    t = g.s.tiles[city.idx]
    t.improvement = "City ruins" if "City ruins" in g.rules.improvements else None
    p = g.player(owner)
    for b in city.buildings:
        if b in g.s.wonders_built and g.s.wonders_built[b] == city.id:
            pass            # a destroyed world wonder stays "built" (cannot be rebuilt), as in UnCiv
    del g.s.cities[city.id]
    from . import espionage
    espionage.city_removed(g, city)
    if p.capital == city.id:
        p.capital = None
        remaining = g.player_cities(owner)
        if remaining:
            newcap = max(remaining, key=lambda c: c.pop)
            add_building(g, newcap, capital_indicator(g, owner), try_free=False)
            p.capital = newcap.id
    g.invalidate()
    g.emit("city_destroyed", f"{city.name} has been destroyed.", None, idx=city.idx,
           mentions={city.name: (owner, "t")}, owner=owner)
    from . import victory
    victory.check_elimination(g, owner)


def add_city_stat(g: "Game", city: City, stat: str, amount: float):
    """City.addStat: production goes to the current construction, food to the granary, the rest to the civ."""
    if stat == "production":
        name = current_construction(city)
        if name and name not in ("Gold", "Science", "Nothing"):
            city.progress[name] = city.progress.get(name, 0) + amount
        else:
            city.overflow += amount
    elif stat == "food":
        city.food = max(0.0, city.food + amount)
    else:
        g.add_stat(city.owner, stat, amount)

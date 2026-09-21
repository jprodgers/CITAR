"""Tile helpers and tile yields (port of UnCiv's Tile / TileStatFunctions, MPL-2.0)."""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .uniques import Ctx, tile_matches, improvement_matches, STAT_KEY

if TYPE_CHECKING:
    from .game import Game
    from .state import City

STATS = ("food", "production", "gold", "science", "culture", "happiness", "faith")
CITY_CENTER_MIN = {"food": 2.0, "production": 1.0}


def zero() -> dict:
    """A stat dictionary with every yield at zero."""
    return dict.fromkeys(STATS, 0.0)


def add(a: dict, b: dict, mult: float = 1.0) -> dict:
    """Add one stat dictionary into another, optionally scaled."""
    for k, v in b.items():
        if k in a:
            a[k] += v * mult
    return a


def obj_stats(d: dict) -> dict:
    """The yields declared directly on a ruleset object."""
    return {k: float(d[k]) for k in STATS if d.get(k)}


# ---------------------------------------------------------------------------------------------------------------
# Terrain helpers
# ---------------------------------------------------------------------------------------------------------------
def all_terrains(t) -> list[str]:
    """A tile's base terrain and every feature on it."""
    out = [t.terrain]
    if t.wonder:
        out.append(t.wonder)
    out.extend(t.features)
    return out


def last_terrain(t) -> str:
    """The terrain that governs a tile: its topmost feature, or its base."""
    if t.features:
        return t.features[-1]
    if t.wonder:
        return t.wonder
    return t.terrain


def is_water(g: "Game", idx: int) -> bool:
    """Whether a tile is water."""
    return g.rules.terrains[g.s.tiles[idx].terrain]["type"] == "Water"


def is_land(g: "Game", idx: int) -> bool:
    """Whether a tile is land."""
    return not is_water(g, idx)


def is_ocean(g: "Game", idx: int) -> bool:
    """Whether a tile is deep ocean rather than coast."""
    return g.s.tiles[idx].terrain == "Ocean"


def is_rough(g: "Game", idx: int) -> bool:
    """Whether a tile is rough ground, which several combat rules care about."""
    R = g.rules
    return any(R.terrains[x]["_rough"] for x in all_terrains(g.s.tiles[idx]))


def is_impassable(g: "Game", idx: int) -> bool:
    """Whether nothing can enter this tile."""
    return bool(g.rules.terrains[last_terrain(g.s.tiles[idx])].get("impassable"))


def is_mountain(g: "Game", idx: int) -> bool:
    """Whether a tile is a mountain."""
    return g.s.tiles[idx].terrain == "Mountain"


def is_hill(g: "Game", idx: int) -> bool:
    """Whether a tile is a hill."""
    return "Hill" in g.s.tiles[idx].features


def terrain_has(g: "Game", idx: int, ph: str, ctx: Optional[Ctx] = None) -> bool:
    """Whether the terrain or features here carry a unique."""
    R = g.rules
    for x in all_terrains(g.s.tiles[idx]):
        if R.terrains[x]["_umap"].has(ph, ctx) if ctx is not None else R.terrains[x]["_umap"].get(ph):
            return True
    return False


def terrain_uniques(g: "Game", idx: int, ph: str, ctx: Optional[Ctx] = None) -> list:
    """Every unique from this tile's terrain and features matching a placeholder."""
    R = g.rules
    out = []
    for x in all_terrains(g.s.tiles[idx]):
        um = R.terrains[x]["_umap"]
        out.extend(um.matching(ph, ctx) if ctx is not None else um.get(ph))
    return out


def fresh_water(g: "Game", idx: int) -> bool:
    """Adjacent to fresh water: a river, a lake, or an oasis (UnCiv 'Fresh water' filter)."""
    key = ("fresh", idx)
    v = g._static.get(key)
    if v is None:
        t = g.s.tiles[idx]
        v = t.river or any(_is_fresh_source(g, n) for n in g.grid.neighbors(idx)) or _is_fresh_source(g, idx)
        g._static[key] = v
    return v


def _is_fresh_source(g, idx) -> bool:
    """Whether this tile is itself a source of fresh water."""
    R = g.rules
    return any(R.terrains[x]["_umap"].has_tag(U.FreshWater) for x in all_terrains(g.s.tiles[idx]))


def adjacent_to_coast(g: "Game", idx: int) -> bool:
    """Whether a tile touches coastal water. Cached: harbours and ships ask constantly."""
    key = ("coastal", idx)
    v = g._static.get(key)
    if v is None:
        v = any(g.s.tiles[n].terrain == "Coast" for n in g.grid.neighbors(idx))
        g._static[key] = v
    return v


def is_coastal_land(g: "Game", idx: int) -> bool:
    """Whether a tile is land next to the coast."""
    return is_land(g, idx) and adjacent_to_coast(g, idx)


def resource_visible(g: "Game", pid: Optional[int], res: Optional[str]) -> bool:
    """Whether a player can see a resource yet, which needs the revealing technology."""
    if res is None:
        return False
    rt = g.rules.resources[res].get("revealedBy")
    return rt is None or (pid is not None and g.has_tech(pid, rt))


def is_friendly_territory(g: "Game", idx: int, pid: int) -> bool:
    """Whether this tile belongs to the player or an ally."""
    owner = g.s.tiles[idx].owner
    if owner is None:
        return False
    if owner == pid:
        return True
    if not g.has_met(pid, owner):
        return False
    op = g.player(owner)
    if op.kind == "city_state":
        from . import city_states
        if city_states.is_friend_level(g, owner, pid) or g.civ_has(pid, U.CityStateTerritoryAlwaysFriendly):
            return True
    return g.has_open_borders(owner, pid)


def is_enemy_territory(g: "Game", idx: int, pid: int) -> bool:
    """Whether this tile belongs to somebody the player is at war with."""
    owner = g.s.tiles[idx].owner
    return owner is not None and g.at_war(pid, owner)


def unpillaged_improvement(t) -> Optional[str]:
    """The tile's improvement, or None if it has been pillaged.

    Pillaging leaves the improvement in place but inert, so every yield calculation has to ask this
    rather than reading the field.
    """
    return t.improvement if t.improvement and not t.pillaged else None


def unpillaged_route(t) -> Optional[str]:
    """The tile's road or railway, or None if pillaged."""
    return t.route if t.route and not t.route_pillaged else None


def resource_improved_by(g: "Game", res: str, imp: Optional[str]) -> bool:
    """Whether an improvement is the one that makes a resource available."""
    if imp is None:
        return False
    rd = g.rules.resources[res]
    return imp == rd.get("improvement") or imp in (rd.get("improvedBy") or [])


def is_city_center(g: "Game", idx: int) -> bool:
    """Whether a city stands on this tile."""
    return g.city_at(idx) is not None


# ---------------------------------------------------------------------------------------------------------------
# Yields
# ---------------------------------------------------------------------------------------------------------------
def _single_terrain_stats(R, name: str, ctx: Ctx) -> dict:
    """The yields of one terrain or feature."""
    td = R.terrains[name]
    s = obj_stats(td)
    for u in td["_umap"].matching(U.Stats, ctx):
        add_into(s, u.stats)
    return s


def add_into(a: dict, b: dict, mult: float = 1.0):
    """Add stats into an accumulator, optionally scaled."""
    for k, v in b.items():
        a[k] = a.get(k, 0.0) + v * mult


def terrain_stats(g: "Game", idx: int, ctx: Ctx) -> dict:
    """The combined yields of a tile's terrain and features, before improvements."""
    R = g.rules
    t = g.s.tiles[idx]
    total: dict = {}
    for name in all_terrains(t):
        td = R.terrains[name]
        s = _single_terrain_stats(R, name, ctx)
        if td["_umap"].has(U.NullifyYields, ctx):
            return s
        if td.get("overrideStats"):
            total = dict(s)
        else:
            add_into(total, s)
    return total


def nullified(g: "Game", idx: int, ctx: Ctx) -> bool:
    """Whether a feature suppresses the yields beneath it, as forest does to the tile under it."""
    R = g.rules
    return any(R.terrains[x]["_umap"].has(U.NullifyYields, ctx) for x in all_terrains(g.s.tiles[idx]))


def _extra_improvement_stats(g: "Game", idx: int, imp: str, pid: int, city, ctx: Ctx) -> dict:
    """Extra yields an improvement gets from technologies, policies and adjacency."""
    R = g.rules
    t = g.s.tiles[idx]
    s: dict = {}
    if t.resource and resource_visible(g, pid, t.resource) and resource_improved_by(g, t.resource, imp):
        add_into(s, R.resources[t.resource].get("improvementStats") or {})
    idef = R.improvements[imp]
    for u in idef["_umap"].matching(U.Stats, ctx):
        add_into(s, u.stats)
    for u in idef["_umap"].matching(U.ImprovementStatsForAdjacencies, ctx):
        f = u.p(1)
        n = sum(1 for nb in g.grid.neighbors(idx)
                if tile_matches(g, nb, f, pid) or unpillaged_route(g.s.tiles[nb]) == f)
        add_into(s, u.stats, n)
    for u in idef["_umap"].matching(U.ImprovementStatsOnTile, ctx):
        f = u.p(1)
        if (f == "Fresh water" and fresh_water(g, idx)) or (f == "non-fresh water" and not fresh_water(g, idx)) \
                or (f not in ("Fresh water", "non-fresh water") and tile_matches(g, idx, f, pid)):
            add_into(s, u.stats)
    return s


def tile_stats(g: "Game", idx: int, pid: Optional[int], city: Optional["City"] = None) -> dict:
    """Yields of a tile for an observing civ, as worked by `city` (city-based uniques apply only with a city)."""
    key = ("tstats", idx, pid, city.id if city is not None else None)
    v = g._cache.get(key)
    if v is not None:
        return v
    v = _tile_stats(g, idx, pid, city)
    g._cache[key] = v
    return v


def _tile_stats(g: "Game", idx: int, pid: Optional[int], city) -> dict:
    """Everything a tile yields to a given city and player.

    The sum of terrain, features, resource, improvement, route and every unique that adds to any of
    them - which is why it is the hottest function in the engine and why its callers cache the result.
    """
    R = g.rules
    t = g.s.tiles[idx]
    ctx = Ctx(g, civ=pid, city=city, tile=idx)
    base = terrain_stats(g, idx, ctx)
    ignored = nullified(g, idx, ctx)
    imp = None if ignored else unpillaged_improvement(t)
    road = None if ignored else unpillaged_route(t)
    imp_s = obj_stats(R.improvements[imp]) if imp else {}
    road_s = obj_stats(R.improvements[road]) if road else {}
    extra_terrain: dict = {}

    if city is not None:
        from .cities import city_uniques
        def add_stats(u, f):
            """Accumulate one source's yields."""
            if imp and improvement_matches(R, imp, f):
                add_into(imp_s, u.stats)
            elif tile_matches(g, idx, f, pid):
                add_into(extra_terrain, u.stats)
            elif road and improvement_matches(R, road, f):
                add_into(road_s, u.stats)
        from .uniques import city_matches
        for u in city_uniques(g, city, U.StatsFromTiles, ctx):
            if city_matches(g, city, u.p(2)):
                add_stats(u, u.p(1))
        for u in city_uniques(g, city, U.StatsFromObject, ctx):
            add_stats(u, u.p(1))
        for u in city_uniques(g, city, U.StatsFromTilesWithout, ctx):
            if city_matches(g, city, u.p(3)) and not tile_matches(g, idx, u.p(2), pid):
                add_stats(u, u.p(1))

    total = dict(base)
    add_into(total, extra_terrain)
    if t.river and "River" in R.terrains:
        add_into(total, _single_terrain_stats(R, "River", ctx))

    minimum = dict(CITY_CENTER_MIN) if g.city_at(idx) is not None else {}
    if pid is not None:
        if t.resource and resource_visible(g, pid, t.resource):
            add_into(total, obj_stats(R.resources[t.resource]))
        if imp:
            add_into(imp_s, _extra_improvement_stats(g, idx, imp, pid, city, ctx))
            mu = R.improvements[imp]["_umap"].matching(U.EnsureMinimumStats, ctx)
            if mu:
                minimum = mu[0].stats
        if road:
            add_into(road_s, _extra_improvement_stats(g, idx, road, pid, city, ctx))

    # percentage bonuses by category
    pct_t, pct_i, pct_r = _tile_percentages(g, idx, pid, city, imp, road, ctx)
    for k in list(total):
        total[k] *= 1 + pct_t.get(k, 0) / 100
    for k in list(imp_s):
        imp_s[k] *= 1 + pct_i.get(k, 0) / 100
    for k in list(road_s):
        road_s[k] *= 1 + pct_r.get(k, 0) / 100
    add_into(total, imp_s)
    add_into(total, road_s)
    for k, v in minimum.items():
        if total.get(k, 0.0) < v:
            total[k] = v
    if pid is not None and total.get("gold", 0) != 0 and g.player(pid).golden_age_turns > 0:
        total["gold"] = total.get("gold", 0) + 1
    out = zero()
    for k, v in total.items():
        if k in out:
            out[k] = v
    return out


def _tile_percentages(g, idx, pid, city, imp, road, ctx):
    """Percentage modifiers applying to this tile's yields."""
    R = g.rules
    pt, pi, pr = {}, {}, {}

    def addp(f, stat, amount):
        """Accumulate one percentage modifier."""
        if imp and improvement_matches(R, imp, f):
            pi[stat] = pi.get(stat, 0) + amount
        elif tile_matches(g, idx, f, pid):
            pt[stat] = pt.get(stat, 0) + amount
        elif road and improvement_matches(R, road, f):
            pr[stat] = pr.get(stat, 0) + amount

    if city is not None:
        from .cities import city_uniques
        src = lambda ph: city_uniques(g, city, ph, ctx)  # noqa: E731
    elif pid is not None:
        src = lambda ph: g.civ_uniques(pid, ph, ctx)  # noqa: E731
    else:
        return pt, pi, pr
    for u in src(U.StatPercentFromObject):
        addp(u.p(2), STAT_KEY.get(u.p(1), u.p(1).lower()), u.n(0))
    for u in src(U.AllStatsPercentFromObject):
        for k in STATS:
            addp(u.p(1), k, u.n(0))
    return pt, pi, pr


def start_yield(g: "Game", idx: int, minimum: Optional[dict] = None) -> float:
    """Food + production + gold of the bare tile (map generation / start scoring)."""
    ctx = Ctx(g, tile=idx)
    s = terrain_stats(g, idx, ctx)
    t = g.s.tiles[idx]
    if t.resource:
        add_into(s, obj_stats(g.rules.resources[t.resource]))
    for k, v in (minimum or {}).items():
        if s.get(k, 0) < v:
            s[k] = v
    return s.get("food", 0) + s.get("production", 0) + s.get("gold", 0)

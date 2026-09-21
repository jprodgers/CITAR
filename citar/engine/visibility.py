"""Fog of war: per-civ viewable tiles, explored tiles, last-seen memory, first contact and natural wonder discovery.
Line of sight follows UnCiv's TileMap.getViewableTiles (terrain elevations) and CivInfoTransientCache (MPL-2.0).
"""
from __future__ import annotations

from typing import TYPE_CHECKING

from . import unique_types as U

if TYPE_CHECKING:
    from .game import Game


def _heights(g: "Game") -> tuple[list[int], list[int]]:
    """(unit height, tile height) per tile. Hills 1, mountains 4 (G&K: 'Has an elevation of [n]'); forests and
    jungles block sight at the same elevation."""
    key = ("vis_heights",)
    h = g._static.get(key)
    if h is not None:
        return h
    from . import tiles as T
    R = g.rules
    uh, th = [], []
    for i in range(g.grid.size):
        terr = T.all_terrains(g.s.tiles[i])
        u = sum(int(x.n(0)) for t in terr for x in R.terrains[t]["_umap"].get(U.VisibilityElevation))
        blocks = any(R.terrains[t]["_umap"].has_tag(U.BlocksLineOfSightAtSameElevation) for t in terr)
        uh.append(u)
        th.append(u + 1 if blocks else u)
    g._static[key] = (uh, th)
    return uh, th


def viewable_from(g: "Game", idx: int, sight: int, for_attack: bool = False) -> tuple:
    """Every tile visible from a point at a given sight range, respecting line of sight.

    Hills and mountains block sight, which is why this is not simply a radius - and why it is cached:
    it is recomputed for every unit whenever anything moves.
    """
    key = ("los", idx, sight, for_attack)
    res = g._static.get(key)
    if res is not None:
        return res
    uh, th = _heights(g)
    a = uh[idx]
    seen = {idx: a}              # tile -> max height seen along the way
    visible = [idx]
    for i in range(1, sight + 2):
        layer = []
        for c in g.grid.ring(idx, i):
            ch = th[c]
            if i == sight + 1 and (ch <= a or for_attack):
                continue
            prev = [seen[n] for n in g.grid.neighbors(c) if n in seen and g.grid.distance(idx, n) == i - 1]
            if not prev:
                continue
            b = min(prev)
            layer.append((c, max(ch, b)))
            ok = (a >= b or uh[c] > b) if for_attack else (a >= b or ch > b)
            if ok:
                visible.append(c)
        for c, m in layer:
            seen[c] = m
    res = tuple(visible)
    g._static[key] = res
    return res


def has_los(g: "Game", frm: int, to: int) -> bool:
    """Ranged attack line of sight (TileMap.getViewableTiles(forAttack = true))."""
    return to in viewable_from(g, frm, g.grid.distance(frm, to), for_attack=True)


def unit_viewable(g: "Game", u) -> tuple:
    """What a unit can see, cached by its position and sight range."""
    sig = (u.idx, u.type, len(u.promotions), g._ygen)
    hit = g._viewcache.get(u.id)
    if hit is not None and hit[0] == sig:
        return hit[1]
    res = _unit_viewable(g, u)
    g._viewcache[u.id] = (sig, res)
    return res


def _unit_viewable(g: "Game", u) -> tuple:
    """Compute what a unit can see."""
    from .units import sight, unit_has
    if unit_has(g, u, U.NoSight):
        return (u.idx,)
    r = sight(g, u)
    if unit_has(g, u, U.CanSeeOverObstacles):
        return tuple(g.grid.within(u.idx, r))
    return viewable_from(g, u.idx, r)


def compute_visible(g: "Game", pid: int) -> set:
    """Every tile a civilization can currently see, from its units, cities and spies."""
    p = g.player(pid)
    vis: set = set()
    for c in g.player_cities(pid):
        from .cities import city_tiles
        for i in city_tiles(g, c):
            vis.add(i)
            vis.update(g.grid.neighbors(i))
    for u in g.player_units(pid):
        vis.update(unit_viewable(g, u))
    for q in g.s.players:
        if q.kind == "city_state" and q.alive and (q.ally == pid or (p.kind == "city_state" and p.ally == q.id)):
            from .cities import city_tiles
            for c in g.player_cities(q.id):
                vis.update(city_tiles(g, c))
    if p.kind == "major" and g.espionage_enabled:
        from .espionage import visible_tiles as spy_tiles
        vis |= spy_tiles(g, pid)
    return vis


def snapshot(g: "Game", idx: int) -> dict:
    """What a tile looks like right now, to be remembered once it falls back into fog."""
    t = g.s.tiles[idx]
    c = g.city_at(idx)
    snap = {"f": list(t.features), "i": t.improvement, "r": t.route, "o": t.owner, "p": t.pillaged}
    if c is not None:
        snap["c"] = [c.name, c.pop, c.owner]
    return snap


def refresh(g: "Game", force: bool = False):
    """Recompute visibility for every civilization, and record what they now know.

    Called after anything that moves a unit, changes a border or destroys something. Tiles that have
    left sight keep the snapshot taken when they were last seen, which is what fog of war *is*: not an
    absence of information but stale information.
    """
    if not g._vis_dirty and not force and g._vis:
        return
    g._vis_dirty = False
    new_all = {}
    for p in g.s.players:
        if p.kind == "barbarian" or not p.alive:
            continue
        new_all[p.id] = compute_visible(g, p.id)
    for pid, new in new_all.items():
        p = g.player(pid)
        old = g._vis.get(pid, set())
        for idx in new - old:
            p.explored[idx] = 1
        for idx in old - new:
            p.memory[idx] = snapshot(g, idx)
    g._vis = new_all
    # first contact: owners of viewed tiles and units
    for pid, vis in new_all.items():
        seen = set()
        for idx in vis:
            o = g.s.tiles[idx].owner
            if o is not None:
                seen.add(o)
            for u in g.units_at(idx):
                seen.add(u.owner)
        for q in seen:
            if q == pid or g.is_barbarian(q) or g.has_met(pid, q) or not g.player(q).alive:
                continue
            if g.player(pid).kind == "city_state" and g.player(q).kind == "city_state":
                continue
            g.meet(pid, q)
    for pid, vis in new_all.items():
        if g.player(pid).kind == "major":
            _discover_natural_wonders(g, pid, vis)


def _discover_natural_wonders(g: "Game", pid: int, vis: set):
    """Award the bonuses for finding a natural wonder, once per civilization."""
    from .uniques import parse_stats
    p = g.player(pid)
    for idx in vis:
        w = g.s.tiles[idx].wonder
        if not w or w in p.natural_wonders:
            continue
        p.natural_wonders.append(w)
        others = {x for q in g.majors() if q.id != pid for x in q.natural_wonders}
        gained: dict = {}

        def add(stats, gained=gained):
            """Accumulate the stats a discovery grants."""
            for k, v in (stats or {}).items():
                gained[k] = gained.get(k, 0) + v
        if w not in others:
            for x in g.rules.terrains[w]["_umap"].get(U.GrantsStatsToFirstToDiscover):
                add(parse_stats(x.p(0)))
        for x in g.civ_uniques(pid, U.StatBonusWhenDiscoveringNaturalWonder):
            add(parse_stats(x.p(1) if w not in others else x.p(0)))
        for k, v in gained.items():
            g.add_stat(pid, k, v)
        text = f"We have discovered {w}!"
        if gained:
            text += " (" + ", ".join(f"+{int(v)} {k}" for k, v in gained.items()) + ")"
        g.emit("natural_wonder", text, [pid], idx=idx)
        g.invalidate()


def visible_tiles(g: "Game", pid: int) -> set:
    """The tiles a civilization can see right now, cached for the turn."""
    if g._vis_dirty or pid not in g._vis:
        refresh(g)
    return g._vis.get(pid, set())


def is_visible(g: "Game", pid: int, idx: int) -> bool:
    """Whether a civilization can see a tile right now."""
    return idx in visible_tiles(g, pid)


def unit_visible_to(g: "Game", pid: int, u) -> bool:
    """MapUnit.isVisibleTo: fog of war plus invisibility (submarines) unless detected."""
    if u.owner == pid:
        return True
    if u.idx not in visible_tiles(g, pid):
        return False
    from .units import unit_has, unit_uniques
    from .uniques import unit_matches
    if unit_has(g, u, U.Invisible):
        for o in g.player_units(pid):
            for x in unit_uniques(g, o, U.CanSeeInvisibleUnits):
                if u.idx in unit_viewable(g, o) and unit_matches(g, u, x.p(0)):
                    return True
        return False
    if hasattr(U, "InvisibleToNonAdjacent") and unit_has(g, u, U.InvisibleToNonAdjacent):
        return any(o.owner == pid for n in g.grid.within(u.idx, 1) for o in g.units_at(n))
    return True


def reveal_tiles(g: "Game", pid: int, tiles) -> int:
    """Mark tiles explored (map trade, ruins, embassies). Returns the number newly explored."""
    p = g.player(pid)
    vis = visible_tiles(g, pid)
    n = 0
    for idx in tiles:
        if not p.explored[idx]:
            p.explored[idx] = 1
            n += 1
        if idx not in vis:
            p.memory[idx] = snapshot(g, idx)
    return n

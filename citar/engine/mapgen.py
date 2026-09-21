"""Procedural map generation: landmass shapes (continents, pangaea, archipelago, inland sea, fractal) followed by
UnCiv's MapGenerator steps (MPL-2.0): climate-driven terrain from the ruleset's "Occurs at temperature..." uniques,
mountains and hills, lakes and coasts, vegetation, rare features, polar ice, rivers along tile edges, terrain
conversion, natural wonders with their placement constraints, strategic/luxury/bonus resources from the resources'
generation uniques, start positions with nation start biases, city-state sites and ancient ruins.

The lobby can change what happens at the map's edges (ice caps, wrapping, boxed in), how many rivers there are,
and how much of each resource is generated - see :class:`MapOptions`."""
from __future__ import annotations

import heapq
import math
import random

from typing import Optional

from . import unique_types as U
from .hexmap import HexGrid
from .rules import Rules
from .state import Tile
from .uniques import multi_filter, terrain_matches

MAP_TYPES = ["continents", "pangaea", "archipelago", "inland_sea", "fractal"]

# What happens at the edges of the map: (wraps east-west, wraps north-south, sides that get an ice cap).
EDGE_MODES = {
    "ice_caps": (False, False, "ns"),       # the default: polar ice north and south, open ocean east and west
    "wrap_x": (True, False, "ns"),          # a cylinder, like a globe: east-west wraps, ice at the poles
    "wrap_y": (False, True, ""),            # north-south wraps, open ocean east and west
    "wrap_both": (True, True, ""),          # a torus: no edges and no ice at all
    "boxed": (False, False, "nsew"),        # boxed in: ice on all four sides
}
DEFAULT_EDGES = "ice_caps"
RESOURCE_MODES = ("normal", "off", "cap", "share")


def edge_wraps(edges: Optional[str]) -> tuple[bool, bool]:
    """Whether a map with these edges wraps east-west and north-south."""
    wx, wy, _ = EDGE_MODES.get(edges or DEFAULT_EDGES, EDGE_MODES[DEFAULT_EDGES])
    return wx, wy


def _num(v, default: float, lo: float, hi: float) -> float:
    """A number from a lobby option, with a default and clamped to a sane range."""
    try:
        return max(lo, min(hi, float(v)))
    except (TypeError, ValueError):
        return default


class MapOptions:
    """The generator's knobs, read from a game or map configuration.

    ``map_edges`` picks a key of :data:`EDGE_MODES`. ``river_density`` scales how many rivers are
    traced (0 = none, 1 = normal). ``resources`` is::

        {"density": 1.0,                                   # scales every kind of resource
         "strategic": {"density": 1.0, "each": {"Uranium": {"mode": "cap", "value": 1}}},
         "luxury":    {"density": 0.5, "each": {"Silk": {"mode": "off"}}},
         "bonus":     {"density": 1.0}}

    A resource's mode is ``normal``, ``off`` (never generated), ``cap`` (at most ``value`` tiles of it)
    or ``share`` (``value`` percent of all the resources of its kind). Unknown names are ignored, so a
    configuration written for one ruleset does not break another.
    """

    def __init__(self, cfg: Optional[dict] = None, rules: Optional[Rules] = None):
        cfg = cfg or {}
        self.edges = cfg.get("map_edges") if cfg.get("map_edges") in EDGE_MODES else DEFAULT_EDGES
        self.wrap_x, self.wrap_y, self.ice_sides = EDGE_MODES[self.edges]
        self.rivers = _num(cfg.get("river_density"), 1.0, 0.0, 5.0)
        res = cfg.get("resources") if isinstance(cfg.get("resources"), dict) else {}
        overall = _num(res.get("density"), 1.0, 0.0, 5.0)
        self.density: dict[str, float] = {}
        self.rule: dict[str, tuple[str, float]] = {}
        for kind in ("Strategic", "Luxury", "Bonus"):
            sub = res.get(kind.lower()) if isinstance(res.get(kind.lower()), dict) else {}
            self.density[kind] = overall * _num(sub.get("density"), 1.0, 0.0, 5.0)
            each = sub.get("each") if isinstance(sub.get("each"), dict) else {}
            for name, r in each.items():
                if not isinstance(r, dict) or r.get("mode") not in RESOURCE_MODES or r.get("mode") == "normal":
                    continue
                if rules is not None:
                    name = rules.resolve("resource", name) or name
                    if name not in rules.resources or rules.resources[name]["resourceType"] != kind:
                        continue
                value = 0.0 if r["mode"] == "off" else _num(r.get("value"), 0.0, 0.0, 10000.0)
                self.rule[name] = (r["mode"], value)

    def describe(self) -> str:
        """A one-line summary for a generated map's description."""
        parts = [f"edges {self.edges.replace('_', ' ')}"]
        if self.rivers != 1:
            parts.append(f"rivers x{self.rivers:g}")
        for kind, d in self.density.items():
            if d != 1:
                parts.append(f"{kind.lower()} x{d:g}")
        for name, (mode, v) in sorted(self.rule.items()):
            parts.append(f"{name} {'off' if mode == 'off' else f'max {v:g}' if mode == 'cap' else f'{v:g}%'}")
        return ", ".join(parts)


# ----------------------------------------------------------------------------
# Noise
# ----------------------------------------------------------------------------
class ValueNoise:
    """Value noise on a coarse grid, interpolated smoothly.

    The basis for terrain generation. Cheaper than Perlin and good enough for deciding where land is,
    and with no dependency beyond the standard library - which matters, because the engine has none.
    """
    def __init__(self, rng: random.Random, width: float, height: float, cell: float,
                 period_x: Optional[float] = None, period_y: Optional[float] = None):
        # On a wrapping axis the lattice repeats with the map, so the two sides of the seam are the
        # same noise and no coastline is cut off at the edge. The cell is stretched slightly so a whole
        # number of cells fits the period.
        self.per_x = max(1, round(period_x / cell)) if period_x else 0
        self.per_y = max(1, round(period_y / cell)) if period_y else 0
        self.cx = period_x / self.per_x if self.per_x else cell
        self.cy = period_y / self.per_y if self.per_y else cell
        self.gw = self.per_x or int(width / cell) + 3
        self.gh = self.per_y or int(height / cell) + 3
        self.vals = [rng.random() for _ in range(self.gw * self.gh)]

    def at(self, fx: float, fy: float) -> float:
        """The noise value at a point, interpolated from the surrounding lattice."""
        gx, gy = fx / self.cx, fy / self.cy
        x0, y0 = math.floor(gx), math.floor(gy)
        tx, ty = gx - x0, gy - y0
        tx = tx * tx * (3 - 2 * tx)
        ty = ty * ty * (3 - 2 * ty)
        x1, y1 = x0 + 1, y0 + 1
        if self.per_x:
            x0, x1 = x0 % self.per_x, x1 % self.per_x
        if self.per_y:
            y0, y1 = y0 % self.per_y, y1 % self.per_y
        g = self.gw
        v00 = self.vals[y0 * g + x0]
        v10 = self.vals[y0 * g + x1]
        v01 = self.vals[y1 * g + x0]
        v11 = self.vals[y1 * g + x1]
        a = v00 + (v10 - v00) * tx
        b = v01 + (v11 - v01) * tx
        return a + (b - a) * ty


def fractal_field(rng, grid: HexGrid, base_cell: float, octaves: int = 4, persistence: float = 0.5) -> list[float]:
    """Several octaves of noise summed, giving both continents and coastline detail.

    Each octave halves in amplitude and doubles in frequency, which is what makes the result look like
    land rather than like blobs.
    """
    layers = []
    cell = base_cell
    amp = 1.0
    for _ in range(octaves):
        layers.append((_value_noise(rng, grid, max(cell, 1.0)), amp))
        cell /= 2
        amp *= persistence
    total_amp = sum(a for _, a in layers)
    out = []
    for i in range(grid.size):
        x, y = grid.xy(i)
        px, py = x + 0.5 * (y & 1), y * 0.866
        v = sum(n.at(px, py) * a for n, a in layers) / total_amp
        out.append(v)
    return _normalize(out)


def _value_noise(rng, grid: HexGrid, cell: float) -> ValueNoise:
    """Value noise for this grid, repeating along whichever axes the map wraps."""
    return ValueNoise(rng, grid.width + 2, grid.height + 2, cell,
                      period_x=grid.width if grid.wrap_x else None,
                      period_y=grid.height * 0.866 if grid.wrap_y else None)


def _normalize(vals: list[float]) -> list[float]:
    """Rescale values to 0-1, so thresholds mean the same thing on any map."""
    lo, hi = min(vals), max(vals)
    span = (hi - lo) or 1.0
    return [(v - lo) / span for v in vals]


def _pos(grid: HexGrid, i: int) -> tuple[float, float]:
    """A tile's position in continuous space, with the hex offset applied."""
    x, y = grid.xy(i)
    return x + 0.5 * (y & 1), y * 0.866


# ----------------------------------------------------------------------------
# Land shape
# ----------------------------------------------------------------------------
def _land_scores(rng, grid: HexGrid, map_type: str, num_players: int, ice: set[int]
                 ) -> tuple[list[float], float]:
    """Score every tile for how land-like it is, shaped by the map type.

    This is where continents, pangaea and archipelago differ: the same noise is masked differently -
    pushed away from the edges, split down the middle, or broken up - and everything after this point
    is identical. On a wrapping axis every distance is measured the short way round, so a continent
    can straddle the seam.
    """
    w, h = grid.width, grid.height
    cw, ch = w / 2, h * 0.866 / 2
    ph = h * 0.866

    def dx(a, b):
        """Horizontal separation, the short way round on a map that wraps east-west."""
        d = a - b
        return (d + w / 2) % w - w / 2 if grid.wrap_x else d

    def dy(a, b):
        """Vertical separation, the short way round on a map that wraps north-south."""
        d = a - b
        return (d + ph / 2) % ph - ph / 2 if grid.wrap_y else d
    noise = fractal_field(rng, grid, base_cell=max(w, h) / 5, octaves=5, persistence=0.55)
    scores = [0.0] * grid.size
    land_fraction = 0.40

    if map_type == "pangaea":
        for i in range(grid.size):
            px, py = _pos(grid, i)
            d = math.hypot(dx(px, cw) / (w * 0.42), dy(py, ch) / (h * 0.866 * 0.40))
            scores[i] = noise[i] * 0.9 - d * 0.9
        land_fraction = 0.42
    elif map_type == "continents":
        n = max(2, min(4, math.ceil(num_players / 2)))
        centers = []
        # spread continent centers horizontally with some vertical jitter
        for k in range(n):
            if n == 2:
                cx = w * (0.27 + 0.46 * k)
                cy = h * 0.866 * rng.uniform(0.42, 0.58)
            else:
                cols = math.ceil(n / 2)
                row, col = k % 2, k // 2
                cx = w * (col + 0.5) / cols + rng.uniform(-w * 0.04, w * 0.04)
                cy = h * 0.866 * (0.3 + 0.4 * row) if n > 2 else ch
            centers.append((cx, cy))
        for i in range(grid.size):
            px, py = _pos(grid, i)
            ds = sorted(math.hypot(dx(px, cx) / w, dy(py, cy) / (h * 0.866)) for cx, cy in centers)
            gap = ds[1] - ds[0] if len(ds) > 1 else 1.0
            gap_penalty = max(0.0, 0.07 - gap) * 9.0
            scores[i] = noise[i] * 0.85 - ds[0] * 1.6 - gap_penalty
        land_fraction = 0.40
    elif map_type == "archipelago":
        noise2 = fractal_field(rng, grid, base_cell=max(w, h) / 12, octaves=4, persistence=0.5)
        n = max(8, num_players * 5)
        centers = [(rng.uniform(w * 0.08, w * 0.92), rng.uniform(h * 0.866 * 0.1, h * 0.866 * 0.9)) for _ in range(n)]
        for i in range(grid.size):
            px, py = _pos(grid, i)
            d = min(math.hypot(dx(px, cx), dy(py, cy)) for cx, cy in centers) / max(w, h)
            scores[i] = noise2[i] * 0.8 + noise[i] * 0.2 - d * 3.2
        land_fraction = 0.30
    elif map_type == "inland_sea":
        for i in range(grid.size):
            px, py = _pos(grid, i)
            d = math.hypot(dx(px, cw) / (w * 0.5), dy(py, ch) / (h * 0.866 * 0.5))
            inner = max(0.0, 0.45 - d) * 2.2
            scores[i] = noise[i] * 0.7 - inner
        land_fraction = 0.58
    else:  # fractal
        scores = list(noise)
        land_fraction = 0.40

    # Push land away from the edges that do not wrap, and from the ice. The ice itself is always sea;
    # the three rows inside it are discouraged the same way a bare map edge is, so land meets the ice
    # at a coastline rather than being cut off by it.
    near_ice = _distance_from(grid, ice, 3)
    for i in range(grid.size):
        if i in ice:
            scores[i] = -1e9
            continue
        x, y = grid.xy(i)
        edges = []
        if not grid.wrap_x:
            edges += [x, w - 1 - x]
        if not grid.wrap_y:
            edges += [y, h - 1 - y]
        edge = min(edges) if edges else 99
        if i in near_ice:
            edge = min(edge, near_ice[i] - 1)
        if edge < 3:
            scores[i] -= (3 - edge) * 0.35
    return scores, land_fraction


def _distance_from(grid: HexGrid, sources: set[int], limit: int) -> dict[int, int]:
    """Steps from the nearest source tile, for every tile within ``limit`` of one."""
    dist = dict.fromkeys(sources, 0)
    frontier = list(sources)
    for d in range(1, limit + 1):
        nxt = []
        for c in frontier:
            for n in grid.neighbors(c):
                if n not in dist:
                    dist[n] = d
                    nxt.append(n)
        frontier = nxt
    return dist


def _ice_band(rng, grid: HexGrid, sides: str) -> set[int]:
    """The polar ice: a band one to four tiles deep along each capped side.

    The depth drifts slowly along the edge - a smooth profile rather than noise - so the ice reads as a
    ragged but essentially straight shelf, not as scattered floes. On a small map the band is thinner,
    so it never eats more than about a quarter of the height.
    """
    band: set[int] = set()
    w, h = grid.width, grid.height
    for side in sides:
        along = w if side in "ns" else h
        across = h if side in "ns" else w
        periodic = grid.wrap_x if side in "ns" else grid.wrap_y
        hi = max(1, min(4, across // 8))
        for s, depth in enumerate(_ice_profile(rng, along, periodic, 1, hi)):
            for d in range(depth):
                x, y = {"n": (s, d), "s": (s, h - 1 - d), "w": (d, s), "e": (w - 1 - d, s)}[side]
                band.add(y * w + x)
    return band


def _ice_profile(rng, n: int, periodic: bool, lo: int, hi: int) -> list[int]:
    """Depths lo..hi along an edge of length n: smoothed random control points about seven tiles apart.

    Periodic when the edge runs along a wrapping axis, so the shelf meets itself at the seam.
    """
    if hi <= lo:
        return [lo] * n
    k = max(1, round(n / 7))
    step = n / k
    pts = [rng.random() for _ in range(k if periodic else k + 1)]
    out = []
    for s in range(n):
        f = s / step
        i0 = int(f)
        t = f - i0
        t = t * t * (3 - 2 * t)
        a = pts[i0 % len(pts)]
        b = pts[(i0 + 1) % len(pts)] if periodic else pts[min(i0 + 1, len(pts) - 1)]
        v = a + (b - a) * t
        out.append(lo + min(hi - lo, int(v * (hi - lo + 1))))
    return out


def _threshold(scores: list[float], fraction: float) -> float:
    """The score cutoff that makes a given fraction of the map land."""
    s = sorted(scores, reverse=True)
    k = max(1, min(len(s) - 1, int(len(s) * fraction)))
    return s[k]


def _components(grid: HexGrid, mask: list[bool]) -> list[list[int]]:
    """Connected regions of a boolean mask, used for landmasses and lakes."""
    seen = [False] * grid.size
    comps = []
    for i in range(grid.size):
        if not mask[i] or seen[i]:
            continue
        stack = [i]
        seen[i] = True
        comp = []
        while stack:
            c = stack.pop()
            comp.append(c)
            for nb in grid.neighbors(c):
                if mask[nb] and not seen[nb]:
                    seen[nb] = True
                    stack.append(nb)
        comps.append(comp)
    return comps


# ----------------------------------------------------------------------------
# Ruleset-driven terrain helpers (no Game object exists yet during generation)
# ----------------------------------------------------------------------------
class _Map:
    """The map being generated: tiles, grid, rules and per-tile climate."""

    def __init__(self, rules: Rules, grid: HexGrid, tiles: list[Tile], rng: random.Random,
                 opts: Optional[MapOptions] = None, ice: Optional[set[int]] = None):
        self.R, self.grid, self.tiles, self.rng = rules, grid, tiles, rng
        self.opts = opts or MapOptions()
        self.ice = ice or set()
        self.placed: dict[str, int] = {}       # resource -> tiles holding it, for caps and shares
        self.temp = [0.0] * grid.size
        self.humid = [0.0] * grid.size
        half = max(1.0, (grid.height - 1) / 2)
        self.lat = [abs(grid.xy(i)[1] - (grid.height - 1) / 2) / half for i in range(grid.size)]
        self.continent = [-1] * grid.size
        self.continents_by_size: list[int] = []

    # --- terrain classification ------------------------------------------------------------------------------
    def td(self, name: str) -> dict:
        """A terrain's definition."""
        return self.R.terrains[name]

    def all_terrains(self, i: int) -> list[str]:
        """A tile's base terrain and its features."""
        t = self.tiles[i]
        out = [t.terrain] + list(t.features)
        if t.wonder:
            out.append(t.wonder)
        return out

    def last(self, i: int) -> str:
        """The topmost terrain on a tile - the feature if there is one, otherwise the base."""
        t = self.tiles[i]
        if t.wonder:
            return t.wonder
        return t.features[-1] if t.features else t.terrain

    def water(self, i: int) -> bool:
        """Whether a tile is water."""
        return self.td(self.tiles[i].terrain)["type"] == "Water"

    def land(self, i: int) -> bool:
        """Whether a tile is land."""
        return not self.water(i)

    def has_u(self, name: str, ph: str) -> bool:
        """Whether a terrain carries a unique."""
        return self.td(name)["_umap"].has_tag(ph)

    def mountain(self, i: int) -> bool:
        """Whether a tile is a mountain, which generate in chains."""
        return any(self.has_u(t, U.OccursInChains) for t in self.all_terrains(i))

    def hill(self, i: int) -> bool:
        """Whether a tile is a hill, which generate in groups."""
        return any(self.has_u(t, U.OccursInGroups) for t in self.all_terrains(i))

    def impassable(self, i: int) -> bool:
        """Whether anything on this tile blocks movement."""
        return any(self.td(t).get("impassable") for t in self.all_terrains(i))

    def fresh_water(self, i: int) -> bool:
        """Whether a tile has fresh water, from a river, lake or oasis."""
        if self.tiles[i].river:
            return True
        return any(self.has_u(t, U.FreshWater) for n in self.grid.neighbors(i) + [i] for t in self.all_terrains(n))

    def coastal(self, i: int) -> bool:
        """Whether a tile touches coastal water."""
        return any(self.tiles[n].terrain == "Coast" or self.has_u(self.tiles[n].terrain, U.CoastalWater)
                   for n in self.grid.neighbors(i))

    # --- filters (Tile.matchesFilter subset used by the G&K generation uniques) --------------------------------
    def matches(self, i: int, f: str) -> bool:
        """Whether a tile satisfies a ruleset filter."""
        return multi_filter(f, lambda s: self._single(i, s))

    def _single(self, i: int, s: str) -> bool:
        """Match one filter term against a tile."""
        t = self.tiles[i]
        if s in ("All", "all"):
            return True
        if s in self.all_terrains(i):
            return True
        if s == "Featureless":
            return not t.features
        if s in ("Fresh Water", "Fresh water"):
            return self.fresh_water(i)
        if s == "River":
            return bool(t.river)
        if s == "Water":
            return self.water(i)
        if s == "Land":
            return self.land(i)
        if s == "Coastal":
            return self.coastal(i)
        if s == "Elevated":
            return self.mountain(i) or self.hill(i)
        if s == "Impassable":
            return self.impassable(i)
        if s in ("Rough", "Rough terrain"):
            return any(self.has_u(x, U.RoughTerrain) for x in self.all_terrains(i))
        if s == "Vegetation":
            return any(self.has_u(x, U.Vegetation) for x in self.all_terrains(i))
        return any(terrain_matches(self.R, x, s) for x in self.all_terrains(i))

    def cond(self, i: int, u) -> bool:
        """Whether a unique's conditions hold for this tile during generation."""
        for m in u.mods:
            if m.ph == "in [] tiles":
                if not self.matches(i, m.p(0)):
                    return False
            elif m.ph == "in tiles without []":
                if self.matches(i, m.p(0)):
                    return False
            elif m.ph == "in [] Regions":
                return False           # CITAR does not build UnCiv's start regions; region-only variants are skipped
            elif m.ph == "in all except [] Regions":
                continue
            else:
                continue
        return True

    def occurs(self, name: str) -> list[tuple[float, float, float, float]]:
        """'Occurs at temperature between [a] and [b] and humidity between [c] and [d]' ranges."""
        out = []
        for x in self.td(name)["_umap"].get(U.TileGenerationConditions):
            a, b, c, d = (float(p) for p in x.params)
            if a == -1:
                a -= 1e-6
            if c == 0:
                c -= 1e-6
            out.append((a, b, c, d))
        return out

    def climate_ok(self, name: str, i: int, temp: Optional[float] = None) -> bool:
        """Whether a terrain may occur here, given latitude and temperature."""
        rs = self.occurs(name)
        if not rs:
            return True
        tt = self.temp[i] if temp is None else temp
        h = self.humid[i]
        return any(a < tt <= b and c < h <= d for a, b, c, d in rs)


def _noise(rng, grid: HexGrid, scale: float, octaves: int = 1) -> list[float]:
    """Smooth noise in about -1..1 (stand-in for UnCiv's Perlin with the given scale in tiles)."""
    layers = []
    cell, amp = max(scale, 1.0), 1.0
    for _ in range(octaves):
        layers.append((_value_noise(rng, grid, max(cell, 1.0)), amp))
        cell /= 2
        amp *= 0.5
    tot = sum(a for _, a in layers)
    out = []
    for i in range(grid.size):
        px, py = _pos(grid, i)
        out.append(2 * sum(n.at(px, py) * a for n, a in layers) / tot - 1)
    return out


# ----------------------------------------------------------------------------
# MapGenerator steps
# ----------------------------------------------------------------------------
def _humidity_and_temperature(m: _Map):
    """MapGenerator.applyHumidityAndTemperature."""
    R = m.R
    hum = _noise(m.rng, m.grid, 6, 1)
    tmp = _noise(m.rng, m.grid, 6, 1)
    intensity = 0.6
    lands = [n for n, d in R.terrains.items() if d["type"] == "Land" and not d.get("impassable")
             and not d["_umap"].has_tag(U.RoughTerrain) and not d["_umap"].has_tag(U.RareFeature)
             and not d["_umap"].has_tag(U.NoNaturalGeneration)]
    for i in range(m.grid.size):
        m.humid[i] = max(0.0, min(1.0, (hum[i] + 1) / 2))
        expected = 1.0 - 2.0 * m.lat[i]
        t = (5.0 * expected + tmp[i]) / 6.0
        t = abs(t) ** (1.0 - intensity) * (1 if t >= 0 else -1)
        m.temp[i] = max(-1.0, min(1.0, t))
        if m.land(i):
            ok = [n for n in lands if m.occurs(n) and m.climate_ok(n, i)]
            m.tiles[i].terrain = m.rng.choice(ok or lands)


def _mountains_and_hills(m: _Map):
    """MapElevationGenerator.raiseMountainsAndHills (quantile thresholds instead of Perlin cut-offs)."""
    elev = _noise(m.rng, m.grid, 2.5, 4)
    land = [i for i in range(m.grid.size) if m.land(i)]
    if not land:
        return
    ranked = sorted(land, key=lambda i: elev[i], reverse=True)
    n_mtn = int(len(land) * 0.06)
    n_hill = int(len(land) * 0.14)
    mountains, hills = set(ranked[:n_mtn]), set(ranked[n_mtn:n_mtn + n_hill])
    for i in land:
        t = m.tiles[i]
        if i in mountains:
            t.terrain = "Mountain"
            t.features = []
        elif i in hills:
            t.features = ["Hill"]
    rng = m.rng
    target_m = sum(1 for i in land if m.tiles[i].terrain == "Mountain") * 2
    for _ in range(5):
        total = sum(1 for i in land if m.tiles[i].terrain == "Mountain") * 2
        rise, lower = [], []
        for i in land:
            is_m = m.tiles[i].terrain == "Mountain"
            adj = sum(1 for n in m.grid.neighbors(i) if m.tiles[n].terrain == "Mountain")
            if adj == 0:
                if is_m and rng.randrange(4) == 0:
                    lower.append(i)
            elif adj == 1:
                if not is_m and rng.randrange(10) == 0:
                    rise.append(i)
            elif adj == 3:
                if is_m and rng.randrange(2) == 0:
                    lower.append(i)
            elif is_m and adj > 3:
                lower.append(i)
        for i in rise:
            if total >= target_m:
                break
            total += 1
            m.tiles[i].terrain = "Mountain"
            m.tiles[i].features = []
        for i in lower:
            if total * 2 <= target_m:
                break
            total -= 1
            m.tiles[i].terrain = _flat_for(m, i)
    target_h = sum(1 for i in land if m.hill(i))
    for it in range(1, 6):
        total = sum(1 for i in land if m.hill(i))
        rise, lower = [], []
        for i in land:
            if m.tiles[i].terrain == "Mountain":
                continue
            is_h = m.hill(i)
            am = sum(1 for n in m.grid.neighbors(i) if m.tiles[n].terrain == "Mountain")
            ah = sum(1 for n in m.grid.neighbors(i) if m.hill(n))
            if ah <= 1 and am == 0 and is_h and rng.randrange(2) == 0:
                lower.append(i)
            elif ah > 3 and am == 0 and is_h and rng.randrange(2) == 0:
                lower.append(i)
            elif 2 <= ah + am <= 3 and not is_h and rng.randrange(2) == 0:
                rise.append(i)
        for i in rise:
            if total > target_h and it != 1:
                continue
            total += 1
            m.tiles[i].features = ["Hill"]
        for i in lower:
            if total >= target_h * 0.9 or it == 1:
                total -= 1
                m.tiles[i].features = []


def _flat_for(m: _Map, i: int) -> str:
    """The flat terrain that belongs at this latitude - desert, plains, tundra and so on."""
    R = m.R
    lands = [n for n, d in R.terrains.items() if d["type"] == "Land" and not d.get("impassable")
             and not d["_umap"].has_tag(U.RoughTerrain) and m.occurs(n)]
    ok = [n for n in lands if m.climate_ok(n, i)]
    return m.rng.choice(ok or lands)


def _lakes_and_coasts(m: _Map):
    """MapGenerator.spawnLakesAndCoasts: small enclosed water bodies become lakes, coasts spread from land."""
    water = [i for i in range(m.grid.size) if m.water(i)]
    for comp in _components(m.grid, [m.water(i) for i in range(m.grid.size)]):
        if len(comp) <= 10:
            for i in comp:
                m.tiles[i].terrain = "Lakes"
    for i in water:
        if m.tiles[i].terrain != "Lakes":
            m.tiles[i].terrain = "Ocean"
    for _ in range(3):
        to_coast = []
        for i in range(m.grid.size):
            if m.tiles[i].terrain != "Ocean":
                continue
            for n in m.grid.within(i, 1):
                if m.land(n):
                    to_coast.append(i)
                    break
                if m.tiles[n].terrain == "Coast":
                    if m.rng.random() < 0.5:
                        to_coast.append(i)
                    break
        for i in to_coast:
            m.tiles[i].terrain = "Coast"


def _vegetation(m: _Map):
    """MapGenerator.spawnVegetation (forest and jungle; jungle prefers warm tiles)."""
    veg = _noise(m.rng, m.grid, 3, 1)
    feats = [n for n, d in m.R.terrains.items() if d["type"] == "TerrainFeature" and d["_umap"].has_tag(U.Vegetation)
             and not d["_umap"].has_tag(U.RareFeature)]
    cand_bases = {b for f in feats for b in m.td(f).get("occursOn", [])}
    for i in range(m.grid.size):
        t = m.tiles[i]
        if t.terrain not in cand_bases or m.last(i) not in cand_bases:
            continue
        if (veg[i] + 1) / 2 > 0.4:
            continue
        options = [f for f in feats if m.last(i) in m.td(f)["occursOn"] and m.climate_ok(f, i) and _fits(m, f, i)]
        if not options:
            continue
        if "Jungle" in options and "Forest" in options:
            options = ["Jungle"] if m.temp[i] > 0.45 else (["Forest"] if m.temp[i] < 0.15 else options)
        t.features.append(m.rng.choice(options))


def _rare_features(m: _Map):
    """MapGenerator.spawnRareFeatures."""
    rare = [n for n, d in m.R.terrains.items() if d["type"] == "TerrainFeature" and d["_umap"].has_tag(U.RareFeature)]
    for i in range(m.grid.size):
        t = m.tiles[i]
        if t.features or m.rng.random() > 0.05:
            continue
        options = [f for f in rare if t.terrain in m.td(f).get("occursOn", []) and m.climate_ok(f, i) and _fits(m, f, i)]
        if options:
            t.features.append(m.rng.choice(options))


def _ice(m: _Map):
    """Freeze the polar band (see :func:`_ice_band`). There is no other sea ice: scattered floes out in
    the ocean were the old behaviour and read as noise rather than as a pole."""
    if "Ice" not in m.R.terrains:
        return
    occurs = m.td("Ice").get("occursOn") or ["Ocean", "Coast"]
    for i in m.ice:
        t = m.tiles[i]
        if t.terrain in occurs:
            t.features = ["Ice"]


def _fits(m: _Map, name: str, i: int) -> bool:
    """NaturalWonderGenerator.fitsTerrainUniques."""
    d = m.td(name)
    for x in d["_umap"].all:
        if x.ph == U.NaturalWonderNeighborCount:
            if sum(1 for n in m.grid.neighbors(i) if m.matches(n, x.p(1))) != int(x.n(0)):
                return False
        elif x.ph == U.NaturalWonderNeighborsRange:
            c = sum(1 for n in m.grid.neighbors(i) if m.matches(n, x.p(2)))
            if not int(x.n(0)) <= c <= int(x.n(1)):
                return False
        elif x.ph == U.NaturalWonderSmallerLandmass:
            if m.continent[i] in m.continents_by_size[:int(x.n(0))]:
                return False
        elif x.ph == U.NaturalWonderLargerLandmass:
            if m.continent[i] not in m.continents_by_size[:int(x.n(0))]:
                return False
        elif x.ph == U.NaturalWonderLatitude:
            if not x.n(0) / 100 <= m.lat[i] <= x.n(1) / 100:
                return False
    return True


def _assign_continents(m: _Map):
    """Number the landmasses, so later code can ask whether two tiles are on the same one."""
    comps = _components(m.grid, [m.land(i) for i in range(m.grid.size)])
    comps.sort(key=len, reverse=True)
    m.continent = [-1] * m.grid.size
    for k, comp in enumerate(comps):
        for i in comp:
            m.continent[i] = k
    m.continents_by_size = list(range(len(comps)))


# ----------------------------------------------------------------------------
# Rivers (RiverGenerator, walking along hex corners)
# ----------------------------------------------------------------------------
def _dir(grid: HexGrid, a: int, b: int) -> int:
    """The direction from one tile to an adjacent one, as a hex edge index."""
    for d in range(6):
        if grid.neighbor_in_dir(a, d) == b:
            return d
    return -1


def _paint_river(m: _Map, a: int, b: int):
    """Draw a river along the edge between two tiles.

    Rivers are edges rather than tiles, which is why they need their own representation and why
    anything that asks "is there a river here" has to name two tiles.
    """
    d = _dir(m.grid, a, b)
    if d < 0:
        return
    m.tiles[a].river |= 1 << d
    m.tiles[b].river |= 1 << ((d + 3) % 6)


def _common(grid: HexGrid, a: int, b: int) -> list[int]:
    """The tiles adjacent to both of two tiles - the pair a river edge separates."""
    nb = set(grid.neighbors(b))
    return [c for c in grid.neighbors(a) if c in nb]


def _salt(m: _Map, i: int) -> bool:
    """Whether a tile is sea - the water a river has to reach. Lakes do not count."""
    return m.water(i) and not m.has_u(m.tiles[i].terrain, U.FreshWater)


def _rivers(m: _Map):
    """Trace rivers from high ground down to the sea, along hex edges.

    Rivers run between tiles, so the walk is over hex *corners*: each corner is where three tiles meet,
    and stepping to the next corner runs the river along the edge between the two tiles they share.

    Every corner first learns its way to the sea: a shortest-path search outward from the river mouths
    (corners where two land tiles meet the sea), over land-to-land edges, with random costs so the
    channels meander and a penalty for running between two mountains. That gives a drainage *tree*,
    and every river just follows it downhill. So a river always reaches the sea - a source that
    cannot drain is never used - and two rivers that meet merge into one channel instead of crossing,
    exactly as tributaries do.
    """
    grid = m.grid
    land = [i for i in range(grid.size) if m.land(i)]
    n = round(len(land) * 0.01 * m.opts.rivers)
    if not land or len(land) == grid.size or n <= 0:
        return
    rng = m.rng

    corners: set[frozenset] = set()
    for a in range(grid.size):
        for b in grid.neighbors(a):
            for c in _common(grid, a, b):
                corners.add(frozenset((a, b, c)))

    def steps(v: frozenset):
        """(next corner, the edge's two tiles) for each land-to-land edge leaving a corner."""
        vs = tuple(v)
        for x in range(3):
            for y in range(x + 1, 3):
                a, b = vs[x], vs[y]
                if m.water(a) or m.water(b):
                    continue
                for c in _common(grid, a, b):
                    if c not in v:
                        yield frozenset((a, b, c)), a, b

    # mouths: two land tiles and one sea tile, so the last edge of the river runs straight into the sea
    dist: dict[frozenset, float] = {}
    down: dict[frozenset, tuple] = {}         # corner -> (next corner, edge tile a, edge tile b)
    heap = []
    for v in corners:
        if sum(1 for t in v if _salt(m, t)) == 1 and sum(1 for t in v if m.land(t)) == 2:
            dist[v] = 0.0
            heap.append((0.0, rng.random(), v))
    heapq.heapify(heap)
    cost: dict[frozenset, float] = {}
    while heap:
        d, _, v = heapq.heappop(heap)
        if d > dist.get(v, math.inf):
            continue
        for u, a, b in steps(v):
            key = frozenset((a, b))
            if key not in cost:
                both_mountains = m.tiles[a].terrain == "Mountain" and m.tiles[b].terrain == "Mountain"
                cost[key] = 1.0 + rng.random() * 1.5 + (4.0 if both_mountains else 0.0)
            nd = d + cost[key]
            if nd < dist.get(u, math.inf):
                dist[u] = nd
                down[u] = (v, a, b)
                heapq.heappush(heap, (nd, rng.random(), u))

    # Sources: high, inland tiles, spread out. A source corner must lie entirely on land and drain.
    to_sea = _distance_from(grid, {i for i in range(grid.size) if _salt(m, i)}, grid.width + grid.height)
    far = [i for i in land if to_sea.get(i, 0) >= 3]
    opts = [i for i in far if m.tiles[i].terrain == "Mountain"]
    if len(opts) < n:
        opts += [i for i in far if m.hill(i) and i not in opts]
    if len(opts) < n:
        opts = far or [i for i in land if to_sea.get(i, 0) >= 2]
    on_river: set[frozenset] = set()
    for s in _spread_out(m, n, opts):
        heads = [v for v in corners if s in v and v in down and all(m.land(t) for t in v)]
        if not heads:
            continue
        v = max(heads, key=lambda v: dist[v])
        if v in on_river:
            continue
        path, visited = [], []
        while v in down and v not in on_river:
            nv, a, b = down[v]
            path.append((a, b))
            visited.append(v)
            v = nv
        if len(path) < 2:
            continue
        # v is now a mouth, or a corner of an earlier river that this one flows into
        on_river.update(visited)
        on_river.add(v)
        for a, b in path:
            _paint_river(m, a, b)


def _convert_terrains(m: _Map):
    """Helpers.convertTerrains: 'Becomes [X] when adjacent to [River]' (tundra->plains, desert->flood plains...)."""
    for i in range(m.grid.size):
        t = m.tiles[i]
        for x in m.td(t.terrain)["_umap"].get(U.ChangesTerrain):
            if x.p(1) == "River" and not t.river:
                continue
            if x.p(1) != "River" and not any(m.matches(n, x.p(1)) for n in m.grid.neighbors(i)):
                continue
            to = x.p(0)
            if to not in m.R.terrains:
                continue
            if m.td(to)["type"] != "TerrainFeature":
                t.terrain = to
            elif m.last(i) in m.td(to).get("occursOn", []):
                t.features.append(to)
            break


# ----------------------------------------------------------------------------
# Spread-out placement (MapGenerationRandomness.chooseSpreadOutLocations)
# ----------------------------------------------------------------------------
def _spread_out(m: _Map, number: int, tiles: list[int], radius: Optional[int] = None) -> list[int]:
    """Choose *number* tiles from a list, as far apart as they can reasonably be.

    Used for starts, city-states and natural wonders. The radius shrinks until enough fit, so a crowded
    map still places everything rather than giving up.
    """
    if number <= 0 or not tiles:
        return []
    radius = radius or max(10, int(math.sqrt(m.grid.size) / 2))

    def hex_radius(area):
        """The radius a given area corresponds to on a hex grid."""
        return math.sqrt(max(area, 1) / 3.0)
    sparsity = (hex_radius(len(tiles)) / radius) ** 0.333
    if number == 1 or number * 5 >= len(tiles) * 3:
        initial = 1
    else:
        initial = max(1, round(radius * 0.666 / hex_radius(number) ** 0.9 * sparsity))
    groups: dict[str, list[int]] = {}
    for i in tiles:
        groups.setdefault(m.tiles[i].terrain, []).append(i)
    for dist in range(initial, 0, -1):
        avail = {k: set(v) for k, v in groups.items()}
        counts = dict.fromkeys(groups, 0)
        chosen = []
        for _ in range(number):
            key = next((k for k in sorted(counts, key=lambda k: counts[k]) if avail[k]), None)
            if key is None:
                break
            c = m.rng.choice(sorted(avail[key]))
            close = set(m.grid.within(c, dist))
            for s in avail.values():
                s -= close
            chosen.append(c)
            counts[key] += 1
        if len(chosen) == number or dist == 1:
            return chosen
    return []


# ----------------------------------------------------------------------------
# Start positions
# ----------------------------------------------------------------------------
def _fertility(m: _Map, i: int) -> float:
    """Start-location fertility from terrain uniques ('[+n] to Fertility', 'Always Fertility [n]')."""
    if m.water(i):
        return 1.0 if m.tiles[i].terrain == "Coast" else 0.0
    total = 0.0
    always = None
    for tn in m.all_terrains(i):
        for x in m.td(tn)["_umap"].get(U.OverrideFertility):
            always = x.n(0)
        for x in m.td(tn)["_umap"].get(U.AddFertility):
            total += x.n(0)
    f = always if always is not None else total
    if m.tiles[i].river:
        f += 1
    if m.fresh_water(i):
        f += 1
    return f


def _start_score(m: _Map, i: int) -> float:
    # cached per map: terrain is final by the time starts are chosen
    """How good a starting position this tile is, from the yields around it."""
    cache = m.__dict__.setdefault("_start_scores", {})
    if i in cache:
        return cache[i]
    fert = m.__dict__.setdefault("_fert", {})
    s = 0.0
    for n in m.grid.within(i, 3):
        d = m.grid.distance(i, n)
        w = 1.0 if d <= 1 else (0.7 if d == 2 else 0.35)
        if n not in fert:
            fert[n] = _fertility(m, n)
        s += fert[n] * w
    if m.coastal(i):
        s += 2
    if m.tiles[i].river:
        s += 2
    cache[i] = s
    return s


def _bias_score(m: _Map, i: int, bias: list[str]) -> float:
    """How well a tile suits a civilization's start bias, normalised."""
    if not bias:
        return 0.0
    cache = m.__dict__.setdefault("_bias_scores", {})
    key = (i, tuple(bias))
    if key not in cache:
        cache[key] = _bias_score_raw(m, i, bias)
    return cache[key]


def _bias_score_raw(m: _Map, i: int, bias: list[str]) -> float:
    """The unnormalised bias score - how much of the preferred terrain is nearby."""
    s = 0.0
    area = m.grid.within(i, 3)
    for b in bias:
        if b.startswith("Avoid ["):
            f = b[len("Avoid ["):-1]
            s -= 3 * sum(1 for n in area if m.matches(n, f))
        elif b == "Coast":
            s += 25 if m.coastal(i) else -25
        else:
            s += 4 * sum(1 for n in area if m.matches(n, b))
    return s


def _start_candidates(m: _Map) -> list[int]:
    """Every tile that could be a starting position at all."""
    grid = m.grid
    big = {m.continent[i] for i in range(grid.size) if m.continent[i] >= 0}
    sizes = {k: sum(1 for i in range(grid.size) if m.continent[i] == k) for k in big}
    out = []
    for i in range(grid.size):
        if not m.land(i) or m.impassable(i) or m.continent[i] < 0 or sizes[m.continent[i]] < 25:
            continue
        x, y = grid.xy(i)
        if not grid.wrap_x and (x < 2 or x > grid.width - 3):
            continue
        if not grid.wrap_y and (y < 2 or y > grid.height - 3):
            continue
        if m.tiles[i].terrain in ("Snow",) or m.last(i) in ("Marsh", "Oasis", "Ice"):
            continue
        if m.tiles[i].terrain == "Tundra" and m.lat[i] > 0.8:
            continue
        out.append(i)
    return out


def _choose_starts(m: _Map, n: int, nations: list[Optional[str]]) -> Optional[list[int]]:
    """Place the major civilizations, honouring start biases and keeping them apart.

    Returns None when it cannot, which is the caller's signal to regenerate the map rather than to
    squash everybody together.
    """
    cand = _start_candidates(m)
    if len(cand) < n:
        return None
    score = {i: _start_score(m, i) for i in cand}
    cand.sort(key=lambda i: -score[i])
    land_total = sum(1 for i in range(m.grid.size) if m.land(i) and not m.impassable(i))
    min_dist = max(7, int(math.sqrt(land_total / max(n, 1)) * 0.8))
    order = sorted(range(n), key=lambda k: -len((m.R.nations.get(nations[k] or "", {}) or {}).get("startBias", [])))
    best = None
    for trial in range(24):
        md = max(5, min_dist - trial // 6)
        starts: dict[int, int] = {}
        top = cand[: max(n * 25, len(cand) // 2)]
        nearest = dict.fromkeys(top)     # distance to the closest chosen start, kept up to date
        for k in order:
            bias = (m.R.nations.get(nations[k] or "", {}) or {}).get("startBias", [])
            best_i, best_v = None, -1e9
            for i in top:
                d = nearest[i]
                if d is not None and d < md:
                    continue
                spread = d if d is not None else md * 2
                v = score[i] + _bias_score(m, i, bias) + min(spread, md * 2) * 1.5 + m.rng.random() * 6
                if v > best_v:
                    best_v, best_i = v, i
            if best_i is None:
                break
            starts[k] = best_i
            for i in top:
                d = m.grid.distance(i, best_i)
                if nearest[i] is None or d < nearest[i]:
                    nearest[i] = d
        if len(starts) == n:
            s_list = [starts[k] for k in range(n)]
            spread = min((m.grid.distance(a, b) for a in s_list for b in s_list if a != b), default=99)
            quality = spread * 3 + min(score[s] for s in s_list)
            if best is None or quality > best[0]:
                best = (quality, s_list)
            if trial >= 6:
                break
    return best[1] if best else None


def _choose_cs_starts(m: _Map, n: int, starts: list[int]) -> list[int]:
    """Place city-states, away from the major civilizations."""
    cand = [i for i in _start_candidates(m) if not any(m.grid.distance(i, s) < 6 for s in starts)]
    cand.sort(key=lambda i: -_start_score(m, i))
    out: list[int] = []
    for md in (6, 5, 4):
        for i in cand:
            if len(out) >= n:
                break
            if any(m.grid.distance(i, s) < md for s in out):
                continue
            out.append(i)
        if len(out) >= n:
            break
    return out[:n]


# ----------------------------------------------------------------------------
# Natural wonders (NaturalWonderGenerator)
# ----------------------------------------------------------------------------
def _natural_wonders(m: _Map, radius: int, avoid: list[int]):
    """Place natural wonders where their terrain requirements allow."""
    R = m.R
    number = round(radius * 0.124 + 0.1)
    wonders = [n for n, d in R.terrains.items() if d["type"] == "NaturalWonder"]
    chosen = []
    pool = list(wonders)
    while pool and len(chosen) < number:
        tot = sum(R.terrains[w].get("weight", 10) for w in pool)
        r = m.rng.random() * tot
        acc = 0.0
        for w in pool:
            acc += R.terrains[w].get("weight", 10)
            if r <= acc:
                chosen.append(w)
                pool.remove(w)
                break
    too_close = {j for s in avoid for j in m.grid.within(s, 5)}
    blocked: set[int] = set()

    def cands(w):
        """Tiles where this particular wonder could go."""
        occ = R.terrains[w].get("occursOn", [])
        return [i for i in range(m.grid.size) if m.tiles[i].resource is None and i not in too_close
                and not m.tiles[i].wonder and m.last(i) in occ and _fits(m, w, i)]
    spawned = []
    for w in sorted(chosen, key=lambda w: len(cands(w))):
        if _try_wonder(m, w, [i for i in cands(w) if i not in blocked], blocked):
            spawned.append(w)
    if len(spawned) < number:
        for w in sorted(pool, key=lambda w: len(cands(w))):
            if len(spawned) >= number:
                break
            if _try_wonder(m, w, [i for i in cands(w) if i not in blocked], blocked):
                spawned.append(w)


def _try_wonder(m: _Map, w: str, spots: list[int], blocked: set) -> bool:
    """Attempt to place one natural wonder, widening the search until it fits or fails."""
    lo = hi = 1
    for x in m.td(w)["_umap"].get(U.NaturalWonderGroups):
        lo, hi = int(x.n(0)), int(x.n(1))
    size = hi if lo == hi else m.rng.randint(lo, hi)
    if len(spots) < lo:
        return False
    loc = m.rng.choice(spots)
    group = [loc]
    while len(group) < size:
        nb = {n for g_ in group for n in m.grid.neighbors(g_)} - set(group)
        c = [s for s in spots if s in nb]
        if not c:
            break
        group.append(m.rng.choice(c))
    if len(group) < lo:
        return False
    for i in group:
        _place_wonder(m, w, i)
        blocked.update(m.grid.within(i, max(2, m.grid.height // 5)))
    return True


def _place_wonder(m: _Map, w: str, i: int):
    """Put a natural wonder on a tile and apply the terrain changes it brings."""
    d = m.td(w)
    t = m.tiles[i]
    t.wonder = w
    t.resource = None
    t.resource_amount = 0
    t.features = []
    if d.get("turnsInto") in m.R.terrains:
        t.terrain = d["turnsInto"]
    for x in d["_umap"].get(U.NaturalWonderConvertNeighbors):
        to = x.p(0)
        for n in m.grid.neighbors(i):
            nt = m.tiles[n]
            if nt.wonder or nt.terrain == to:
                continue
            if any(mm.ph == "in tiles without []" and m.matches(n, mm.p(0)) for mm in x.mods):
                continue
            if m.td(to)["type"] in ("Land", "Water"):
                nt.terrain = to
                nt.features = []
                nt.resource = None


# ----------------------------------------------------------------------------
# Resources
# ----------------------------------------------------------------------------
def _kind(m: _Map, res: str) -> str:
    """Bonus, Strategic or Luxury."""
    return m.R.resources[res]["resourceType"]


def _allowed(m: _Map, res: str) -> bool:
    """Whether the lobby's resource settings still let this resource be placed.

    'off' never; 'cap' until the cap is reached; a kind at zero density not at all. Shares are settled
    afterwards by :func:`_rebalance`, so here they are allowed like any normal resource.
    """
    if m.opts.density.get(_kind(m, res), 1.0) <= 0:
        return False
    mode, value = m.opts.rule.get(res, ("normal", 0))
    if mode == "off":
        return False
    if mode == "cap":
        return m.placed.get(res, 0) < int(value)
    return True


def _never_generates(d: dict) -> bool:
    """Whether a resource has an unconditional "Doesn't generate naturally".

    Most luxuries carry the unique with a condition (Silk: not on hills; Gems: only on some terrain),
    which restricts where they go rather than whether they exist - see :func:`_natural_on`.
    """
    return any(not x.mods for x in d["_umap"].get(U.NoNaturalGeneration))


def _natural_on(m: _Map, res: str, i: int) -> bool:
    """TileResource.generatesNaturallyOn, ignoring what is on the tile now and the lobby settings."""
    d = m.R.resources[res]
    if m.tiles[i].wonder:
        return False
    if m.last(i) not in d.get("terrainsCanBeFoundOn", []):
        return False
    # "Doesn't generate naturally <in [Hill] tiles>": only where the conditions hold
    if any(m.cond(i, x) for x in d["_umap"].get(U.NoNaturalGeneration)):
        return False
    for tn in m.all_terrains(i):
        for x in m.td(tn)["_umap"].get(U.BlocksResources):
            if m.cond(i, x):
                return False
    if any(m.tiles[n].wonder for n in m.grid.neighbors(i)):
        return False
    return True


def _can_hold(m: _Map, res: str, i: int) -> bool:
    """Whether this resource may be generated on this empty tile now."""
    if m.tiles[i].resource or m.tiles[i].wonder:
        return False
    return _allowed(m, res) and _natural_on(m, res, i)


def _set_resource(m: _Map, res: str, i: int, major: Optional[bool] = None):
    """Place a resource on a tile, with the amount its definition calls for."""
    _clear_resource(m, i)
    d = m.R.resources[res]
    t = m.tiles[i]
    t.resource = res
    t.resource_amount = 0
    m.placed[res] = m.placed.get(res, 0) + 1
    if d["resourceType"] != "Strategic":
        return
    for x in d["_umap"].get(U.ResourceAmountOnTiles):
        if m.matches(i, x.p(0)):
            t.resource_amount = int(x.n(1))
            return
    if major is None:
        major = m.rng.random() < 0.5
    amt = d.get("majorDepositAmount" if major else "minorDepositAmount", {})
    t.resource_amount = int(amt.get("default", 1 if not major else 3))


def _clear_resource(m: _Map, i: int):
    """Take the resource off a tile, keeping the per-resource counts right."""
    t = m.tiles[i]
    if t.resource:
        m.placed[t.resource] = max(0, m.placed.get(t.resource, 0) - 1)
    t.resource = None
    t.resource_amount = 0


def _scaled(m: _Map, count: float, kind: str) -> int:
    """A placement count scaled by the lobby's density for a kind of resource.

    The fraction is settled randomly, so half density on a quantity of one still places it half the time.
    """
    x = count * m.opts.density.get(kind, 1.0)
    whole = int(x)
    return whole + (1 if m.rng.random() < x - whole else 0)


def _weighted(m: _Map, i: int, resources: list[str], ph: str) -> Optional[str]:
    """Pick a resource for a tile, weighted by how well each suits it."""
    opts, weights = [], []
    for r in resources:
        if not _can_hold(m, r, i):
            continue
        for x in m.R.resources[r]["_umap"].get(ph):
            if m.cond(i, x):
                opts.append(r)
                weights.append(x.n(0))
    if not opts:
        return None
    return m.rng.choices(opts, weights=weights)[0]


def _strategic(m: _Map):
    """Major deposits: 'Every [n] tiles with this terrain will receive a major deposit'; minor deposits spread."""
    R = m.R
    if m.opts.density["Strategic"] <= 0:
        return
    strat = [r for r, d in R.resources.items() if d["resourceType"] == "Strategic"
             and not _never_generates(d)]
    # at high density the usual two-tile spacing between major deposits cannot fit them all
    spacing = 2 if m.opts.density["Strategic"] <= 1.5 else 1
    for tn, td in R.terrains.items():
        for x in td["_umap"].get(U.MajorStrategicFrequency):
            freq = int(x.n(0))
            tiles = [i for i in range(m.grid.size) if tn in m.all_terrains(i) and not m.tiles[i].resource]
            count = _scaled(m, len(tiles) / max(freq, 1), "Strategic")
            m.rng.shuffle(tiles)
            placed = 0
            for i in tiles:
                if placed >= count:
                    break
                r = _weighted(m, i, strat, U.ResourceWeighting)
                if r is None or any(m.tiles[n].resource for n in m.grid.within(i, spacing)):
                    continue
                _set_resource(m, r, i, major=True)
                placed += 1
    land = [i for i in range(m.grid.size) if m.land(i) and not m.impassable(i)]
    minor = [i for i in land if not m.tiles[i].resource]
    for i in _spread_out(m, _scaled(m, len(land) * 0.03, "Strategic"), minor):
        r = _weighted(m, i, strat, U.MinorDepositWeighting)
        if r is not None:
            _set_resource(m, r, i, major=False)


def _bonus(m: _Map):
    """'Generated on every [n] tiles <in [filter] tiles>'."""
    R = m.R
    if m.opts.density["Bonus"] <= 0:
        return
    for r, d in R.resources.items():
        if d["resourceType"] != "Bonus":
            continue
        for x in d["_umap"].get(U.ResourceFrequency):
            if any(mm.ph == "in [] Regions" for mm in x.mods):
                continue
            tiles = [i for i in range(m.grid.size) if _can_hold(m, r, i) and m.cond(i, x)]
            count = _scaled(m, len(tiles) / max(int(x.n(0)), 1), "Bonus")
            for i in _spread_out(m, count, tiles):
                if not any(m.tiles[n].resource for n in m.grid.neighbors(i)) and _can_hold(m, r, i):
                    _set_resource(m, r, i)


def _luxuries(m: _Map, starts: list[int], cs_starts: list[int]) -> dict[int, str]:
    """Each civ gets a regional luxury near its start; others are scattered; city-states get their own."""
    R = m.R
    if m.opts.density["Luxury"] <= 0:
        return {}
    lux = [r for r, d in R.resources.items() if d["resourceType"] == "Luxury"
           and not _never_generates(d) and not d["_umap"].has_tag(U.CityStateOnlyResource)]
    m.rng.shuffle(lux)
    regional: dict[int, str] = {}
    free = list(lux)
    for s in starts:
        area = [i for i in m.grid.within(s, 5) if m.grid.distance(i, s) >= 1]
        best, best_n = None, 0
        for r in free:
            n = sum(1 for i in area if _can_hold(m, r, i))
            if n > best_n:
                best, best_n = r, n
        if best is None:
            continue
        free.remove(best)
        regional[s] = best
        spots = [i for i in area if _can_hold(m, best, i)]
        m.rng.shuffle(spots)
        for i in sorted(spots, key=lambda i: m.grid.distance(i, s))[: _scaled(m, 2 + m.rng.randrange(2), "Luxury")]:
            if _can_hold(m, best, i):
                _set_resource(m, best, i)
    near_cs = [r for r in lux if R.resources[r]["_umap"].has_tag(U.LuxuryWeightingForCityStates)]
    for c in cs_starts:
        if not _scaled(m, 1, "Luxury"):
            continue
        area = m.grid.within(c, 3)[1:]
        opts = [r for r in (near_cs or lux) if any(_can_hold(m, r, i) for i in area)]
        if opts:
            r = m.rng.choice(opts)
            spots = [i for i in area if _can_hold(m, r, i)]
            _set_resource(m, r, m.rng.choice(spots))
    land = [i for i in range(m.grid.size) if not m.impassable(i)]
    random_lux = free or lux
    count = _scaled(m, sum(1 for i in land if m.land(i)) * 0.02, "Luxury")
    spots = [i for i in land if any(_can_hold(m, r, i) for r in random_lux)
             and min((m.grid.distance(i, s) for s in starts), default=99) > 3]
    for i in _spread_out(m, count, spots):
        opts = [r for r in random_lux if _can_hold(m, r, i)]
        if opts and not any(m.tiles[n].resource for n in m.grid.neighbors(i)):
            _set_resource(m, m.rng.choice(opts), i)
    return regional


def _rebalance(m: _Map, kind: str, starts: list[int]):
    """Make each 'share' resource the lobby's percentage of all the resources of its kind.

    The kind's total stays what the density produced; only the mix changes. A resource short of its
    share first takes over tiles held by 'normal' resources of the kind where its terrain allows, then
    goes onto fresh tiles, each paid for by removing a normal one elsewhere. One over its share hands
    the surplus back to normal resources, or clears it if none fit. Tiles near a start are the last
    to change, so the start positions stay as balanced as they were. When the shares add up to more
    than 100%, or no normal resource of the kind is left to make up the rest, they are scaled to
    exactly 100%.
    """
    R = m.R
    shares = {r: v for r, (mode, v) in m.opts.rule.items() if mode == "share" and _kind(m, r) == kind}
    if not shares:
        return
    total = sum(1 for t in m.tiles if t.resource and _kind(m, t.resource) == kind)
    if not total:
        return
    normal = [r for r, d in R.resources.items() if d["resourceType"] == kind and r not in shares
              and not _never_generates(d) and _allowed(m, r)
              and not d["_umap"].has_tag(U.CityStateOnlyResource)]
    s = sum(shares.values())
    scale = 100.0 / s if s > 100 or (not normal and s > 0) else 1.0
    want = {r: round(total * v * scale / 100) for r, v in shares.items()}

    def remoteness(i):
        """Distance from the nearest start: tiles far from everyone change first."""
        return min((m.grid.distance(i, st) for st in starts), default=99)

    for r, n in want.items():
        extra = sorted((i for i in range(m.grid.size) if m.tiles[i].resource == r), key=remoteness, reverse=True)
        for i in extra[:max(0, m.placed.get(r, 0) - n)]:
            _clear_resource(m, i)
            opts = [x for x in normal if _can_hold(m, x, i)]
            if opts:
                _set_resource(m, m.rng.choice(opts), i)
    for r, n in want.items():
        need = n - m.placed.get(r, 0)
        if need <= 0:
            continue
        swap = [i for i in range(m.grid.size) if m.tiles[i].resource in normal and _natural_on(m, r, i)]
        swap.sort(key=remoteness, reverse=True)
        for i in swap[:need]:
            _set_resource(m, r, i)
        need = n - m.placed.get(r, 0)
        if need <= 0:
            continue
        empty = [i for i in range(m.grid.size) if _can_hold(m, r, i)
                 and not any(m.tiles[x].resource for x in m.grid.neighbors(i))]
        victims = sorted((i for i in range(m.grid.size) if m.tiles[i].resource in normal),
                         key=remoteness, reverse=True)
        for i in _spread_out(m, need, empty):
            _set_resource(m, r, i)
            if victims:
                _clear_resource(m, victims.pop(0))


def _normalize_start(m: _Map, s: int):
    """Keep starts playable (MapRegions.normalizeStart in spirit): no mountains/snow next door, some food and
    production bonuses, and horses and iron within reach."""
    t = m.tiles[s]
    t.features = [f for f in t.features if f == "Hill"]
    _clear_resource(m, s)
    t.improvement = None
    for n in m.grid.within(s, 1)[1:]:
        nt = m.tiles[n]
        if nt.wonder:
            continue
        if nt.terrain == "Mountain":
            nt.terrain = "Plains" if m.land(s) else nt.terrain
            nt.features = ["Hill"]
        if nt.terrain == "Snow":
            nt.terrain = "Tundra"
        if "Ice" in nt.features:
            nt.features.remove("Ice")
    food_bonus = [r for r, d in m.R.resources.items() if d["resourceType"] == "Bonus" and d.get("food")]

    def count(kind, radius, food=False):
        """Count nearby resources of a kind, for checking a start is good enough."""
        return sum(1 for n in m.grid.within(s, radius)[1:] if m.tiles[n].resource
                   and m.R.resources[m.tiles[n].resource]["resourceType"] == kind
                   and (not food or m.R.resources[m.tiles[n].resource].get("food")))

    def add(options, radius, min_r=1, major=None) -> bool:
        """Add a resource near the start to bring a deficient position up to standard."""
        spots = [n for n in m.grid.within(s, radius) if m.grid.distance(s, n) >= min_r]
        m.rng.shuffle(spots)
        opts = list(options)
        m.rng.shuffle(opts)
        for r in opts:
            for n in spots:
                if _can_hold(m, r, n):
                    _set_resource(m, r, n, major)
                    return True
        return False
    for _ in range(max(0, 2 - count("Bonus", 2, food=True)) if m.opts.density["Bonus"] > 0 else 0):
        if not add(food_bonus, 2):
            for n in m.grid.within(s, 2)[1:]:
                nt = m.tiles[n]
                if nt.terrain in ("Plains", "Desert", "Tundra") and not nt.features and not nt.resource and not nt.wonder:
                    nt.terrain = "Grassland"
                    if _can_hold(m, "Cattle", n):
                        _set_resource(m, "Cattle", n)
                    break
    for strat in ("Horses", "Iron"):
        if strat in m.R.resources and not any(m.tiles[n].resource == strat for n in m.grid.within(s, 5)):
            add([strat], 5, 2, major=False)
    rough = sum(1 for n in m.grid.within(s, 2)[1:] if m.hill(n) or "Forest" in m.tiles[n].features)
    for n in m.grid.within(s, 2)[1:]:
        if rough >= 2:
            break
        nt = m.tiles[n]
        if m.land(n) and not m.impassable(n) and not nt.features and not nt.resource and not nt.wonder \
                and nt.terrain in m.td("Hill").get("occursOn", []):
            nt.features = ["Hill"]
            rough += 1


def _ruins(m: _Map, starts: list[int], cs_starts: list[int]):
    """MapGenerator.spreadAncientRuins."""
    if "Ancient ruins" not in m.R.improvements:
        return
    occupied = {j for s in starts + cs_starts for j in m.grid.within(s, 2)}
    ok = [i for i in range(m.grid.size) if m.land(i) and not m.impassable(i) and not m.tiles[i].improvement
          and i not in occupied and not m.tiles[i].wonder
          and (not m.tiles[i].features or all(f in ("Hill", "Forest", "Jungle") for f in m.tiles[i].features))]
    for i in _spread_out(m, round(len(ok) * 0.025), ok):
        m.tiles[i].improvement = "Ancient ruins"


# ----------------------------------------------------------------------------
# Entry point
# ----------------------------------------------------------------------------
def generate_map(rules: Rules, width: int, height: int, map_type: str, num_players: int, num_city_states: int,
                 rng: random.Random, ruins: bool = True, nations: Optional[list] = None,
                 options: Optional[dict] = None) -> tuple[list[Tile], list[int], list[int], list[int]]:
    """Returns (tiles, major start tiles, city-state start tiles, continent id per tile (-1 = water)).

    ``options`` carries the lobby's map settings (see :class:`MapOptions`): the edges, river density
    and resource settings. A map that wraps north-south needs an even height; the caller is expected
    to have rounded it up, and an odd height simply does not wrap.
    """
    if map_type not in MAP_TYPES:
        map_type = "continents"
    opts = MapOptions(options, rules)
    grid = HexGrid(width, height, wrap_x=opts.wrap_x, wrap_y=opts.wrap_y)
    nations = list(nations or [None] * num_players)
    radius = rules.map_size_predefined(width, height)["radius"]
    for attempt in range(12):
        ice = _ice_band(rng, grid, opts.ice_sides)
        scores, frac = _land_scores(rng, grid, map_type, num_players, ice)
        thr = _threshold(scores, frac)
        is_land = [s >= thr for s in scores]
        for i in range(grid.size):
            if is_land[i] and not any(is_land[n] for n in grid.neighbors(i)):
                is_land[i] = False
        tiles = [Tile(terrain="Plains" if is_land[i] else "Ocean") for i in range(grid.size)]
        m = _Map(rules, grid, tiles, rng, opts, ice)
        _humidity_and_temperature(m)
        _mountains_and_hills(m)
        _lakes_and_coasts(m)
        _vegetation(m)
        _rare_features(m)
        _ice(m)
        _assign_continents(m)
        _rivers(m)
        _convert_terrains(m)
        starts = _choose_starts(m, num_players, nations)
        if starts is not None:
            break
    else:
        raise RuntimeError("Could not generate a map with valid start positions")
    cs_starts = _choose_cs_starts(m, num_city_states, starts)
    _natural_wonders(m, radius, starts + cs_starts)
    _strategic(m)
    _luxuries(m, starts, cs_starts)
    _bonus(m)
    for s in starts + cs_starts:
        _normalize_start(m, s)
    for kind in ("Strategic", "Luxury"):
        _rebalance(m, kind, starts)
    if ruins:
        _ruins(m, starts, cs_starts)
    _assign_continents(m)
    return tiles, starts, cs_starts, m.continent

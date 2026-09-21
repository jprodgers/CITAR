"""Custom maps: the map editor's file format, validation, and placing civilizations on a hand-made map.

A map file (saves/maps/<id>.json) is plain JSON so it can also be written by hand or by a script:

    {"format": "citar-map", "version": 1, "id": "twin-islands", "name": "Twin islands", "description": "...",
     "width": 40, "height": 26,
     "tiles": [[terrain, [features], wonder, river_mask, resource, resource_amount, improvement, route], ...],
     "starts": [idx, ...],          # major civilization start tiles, in seat order (may be shorter than the seats)
     "cs_starts": [idx, ...]}       # city-state start tiles

Tiles are listed row by row (idx = y * width + x, "odd-r" offset hexes: odd rows are shifted right). Missing start
positions are chosen automatically when a game is created, like on a generated map.
"""
from __future__ import annotations

import json
import random
import re
import time
from pathlib import Path

from ..fsutil import replace as _fs_replace
from typing import Optional

from .hexmap import HexGrid
from .state import Tile
from .. import paths

MAP_DIR = paths.saves_path("maps")
TILE_KEYS = ("terrain", "features", "wonder", "river", "resource", "resource_amount", "improvement", "route")
MAX_SIDE = 256
# improvements a map may carry before the game starts (others belong to a scenario)
MAP_IMPROVEMENTS = ("Ancient ruins", "Barbarian encampment", "City ruins", "Farm", "Mine", "Pasture", "Plantation",
                    "Camp", "Quarry", "Lumber mill", "Trading post", "Fishing Boats", "Oil well", "Offshore Platform",
                    "Fort", "Citadel", "Moai", "Terrace farm", "Polder")


class MapError(ValueError):
    """A map that cannot be loaded or saved as written."""
    pass


def slug(name: str) -> str:
    """A filesystem-safe identifier from a name."""
    s = re.sub(r"[^a-z0-9]+", "-", (name or "").lower()).strip("-")
    return s[:60] or f"map-{int(time.time())}"


# ----------------------------------------------------------------------------
# conversion
# ----------------------------------------------------------------------------
def tile_row(t: Tile) -> list:
    """One tile in the compact row form maps are stored in."""
    return [t.terrain, list(t.features), t.wonder, int(t.river or 0), t.resource, int(t.resource_amount or 0),
            t.improvement if t.improvement in MAP_IMPROVEMENTS else None, t.route]


def tiles_from_rows(rows: list) -> list[Tile]:
    """Read tiles back from their compact form."""
    out = []
    for r in rows:
        r = list(r) + [None] * (len(TILE_KEYS) - len(r))
        d = dict(zip(TILE_KEYS, r))
        d["features"] = list(d["features"] or [])
        d["river"] = int(d["river"] or 0)
        d["resource_amount"] = int(d["resource_amount"] or 0)
        out.append(Tile(**d))
    return out


def blank_map(width: int, height: int, terrain: str = "Ocean", name: str = "") -> dict:
    """An empty map of a size, for the editor to start from."""
    _check_size(width, height)
    return {"format": "citar-map", "version": 1, "id": slug(name) if name else "", "name": name or "Untitled map",
            "description": "", "width": width, "height": height,
            "tiles": [[terrain, [], None, 0, None, 0, None, None] for _ in range(width * height)],
            "starts": [], "cs_starts": []}


def generated_map(rules, width: int, height: int, map_type: str, players: int, city_states: int,
                  seed: Optional[int] = None, ruins: bool = True, name: str = "") -> dict:
    """A map from the random generator, as a starting point for editing."""
    from . import mapgen
    _check_size(width, height)
    seed = seed if seed is not None else random.randrange(1, 2**31)
    tiles, starts, cs_starts, _ = mapgen.generate_map(rules, width, height, map_type, players, city_states,
                                                      random.Random(seed), ruins=ruins)
    return {"format": "citar-map", "version": 1, "id": slug(name) if name else "",
            "name": name or f"{map_type.replace('_', ' ').title()} {width}x{height} (seed {seed})",
            "description": f"Generated: {map_type}, {players} players, {city_states} city-states, seed {seed}.",
            "width": width, "height": height, "tiles": [tile_row(t) for t in tiles],
            "starts": list(starts), "cs_starts": list(cs_starts)}


def map_from_game(g, name: str = "") -> dict:
    """The terrain of a game in progress (cities, borders and units dropped; capitals become start positions)."""
    starts, cs_starts = [], []
    for p in g.s.players:
        if p.kind == "barbarian":
            continue
        spot = p.flags.get("start")
        cap = g.city(p.capital) if p.capital is not None else None
        spot = cap.idx if cap is not None else spot
        if spot is None:
            continue
        (starts if p.kind == "major" else cs_starts).append(spot)
    rows = []
    for t in g.s.tiles:
        row = tile_row(t)
        if t.improvement == "City center":
            row[6] = None
        rows.append(row)
    return {"format": "citar-map", "version": 1, "id": slug(name) if name else "", "name": name or "Map from game",
            "description": f"Terrain of game turn {g.turn}.", "width": g.s.width, "height": g.s.height,
            "tiles": rows, "starts": starts, "cs_starts": cs_starts}


# ----------------------------------------------------------------------------
# validation
# ----------------------------------------------------------------------------
def _check_size(width: int, height: int):
    """Reject a map size that is out of bounds."""
    if not (8 <= int(width) <= MAX_SIDE and 8 <= int(height) <= MAX_SIDE):
        raise MapError(f"Maps must be between 8 and {MAX_SIDE} tiles on each side.")


def validate(rules, data: dict, fix: bool = True) -> tuple[dict, list[str]]:
    """Checks a map against the ruleset. With fix=True, invalid entries are removed (and reported as warnings);
    unknown base terrain is always an error. Returns (clean map, warnings)."""
    if not isinstance(data, dict):
        raise MapError("A map must be a JSON object.")
    try:
        width, height = int(data["width"]), int(data["height"])
    except (KeyError, TypeError, ValueError):
        raise MapError("A map needs integer width and height.")
    _check_size(width, height)
    rows = data.get("tiles") or []
    if len(rows) != width * height:
        raise MapError(f"A {width}x{height} map needs {width * height} tiles, got {len(rows)}.")
    tiles = tiles_from_rows(rows)
    R = rules
    warnings: list[str] = []
    grid = HexGrid(width, height)

    def warn(i, msg):
        """Record a problem with a tile."""
        if len(warnings) < 200:
            warnings.append(f"({i % width},{i // width}) {msg}")

    for i, t in enumerate(tiles):
        td = R.terrains.get(t.terrain)
        if td is None or td["type"] not in ("Land", "Water"):
            raise MapError(f"Tile ({i % width},{i // width}) has unknown base terrain '{t.terrain}'.")
        water = td["type"] == "Water"
        feats = []
        for f in t.features:
            fd = R.terrains.get(f)
            if fd is None or fd["type"] != "TerrainFeature":
                warn(i, f"unknown feature '{f}' removed")
                continue
            ok = not fd.get("occursOn") or t.terrain in fd["occursOn"] or any(x in fd["occursOn"] for x in feats)
            if not ok:
                warn(i, f"{f} cannot be on {t.terrain}; removed")
                continue
            if f not in feats:
                feats.append(f)
        # hills first, like the generator
        feats.sort(key=lambda f: 0 if f == "Hill" else 1)
        t.features = feats
        if t.wonder and (R.terrains.get(t.wonder) or {}).get("type") != "NaturalWonder":
            warn(i, f"unknown natural wonder '{t.wonder}' removed")
            t.wonder = None
        if t.resource:
            rd = R.resources.get(t.resource)
            last = feats[-1] if feats else t.terrain
            if rd is None:
                warn(i, f"unknown resource '{t.resource}' removed")
                t.resource, t.resource_amount = None, 0
            elif last not in rd.get("terrainsCanBeFoundOn", []) and t.terrain not in rd.get("terrainsCanBeFoundOn", []):
                warn(i, f"{t.resource} does not normally occur on {last} (kept)")
            if t.resource and rd and rd["resourceType"] == "Strategic" and not t.resource_amount:
                t.resource_amount = int((rd.get("minorDepositAmount") or {}).get("default", 2))
            if t.resource and rd and rd["resourceType"] != "Strategic":
                t.resource_amount = 0
        if t.improvement and t.improvement not in MAP_IMPROVEMENTS:
            warn(i, f"improvement '{t.improvement}' is not allowed on a map (use a scenario); removed")
            t.improvement = None
        if t.route not in (None, "Road", "Railroad"):
            warn(i, f"unknown route '{t.route}' removed")
            t.route = None
        if water and (t.route or t.improvement in ("Farm", "Mine", "Ancient ruins", "Barbarian encampment")):
            warn(i, "land-only improvement or route on water removed")
            t.route = None
            t.improvement = None if t.improvement in ("Farm", "Mine", "Ancient ruins", "Barbarian encampment") else t.improvement
        t.river &= 63
    # rivers are stored on both sides of an edge and never along water
    for i, t in enumerate(tiles):
        for d in range(6):
            if not t.river & (1 << d):
                continue
            n = grid.neighbor_in_dir(i, d)
            if n is None or R.terrains[tiles[n].terrain]["type"] == "Water" or R.terrains[t.terrain]["type"] == "Water":
                t.river &= ~(1 << d)
                continue
            tiles[n].river |= 1 << ((d + 3) % 6)

    def clean_starts(key):
        """Drop start positions that are no longer valid."""
        out = []
        for s in data.get(key) or []:
            try:
                s = int(s)
            except (TypeError, ValueError):
                continue
            if not 0 <= s < width * height:
                continue
            td = R.terrains[tiles[s].terrain]
            if td["type"] == "Water" or td.get("impassable") or tiles[s].wonder:
                warn(s, f"start position on {tiles[s].terrain} removed")
                continue
            if s in out:
                continue
            out.append(s)
        return out

    starts = clean_starts("starts")
    cs_starts = [s for s in clean_starts("cs_starts") if s not in starts]
    clean = {"format": "citar-map", "version": 1, "id": data.get("id") or slug(data.get("name", "")),
             "name": str(data.get("name") or "Untitled map")[:80], "description": str(data.get("description") or "")[:2000],
             "width": width, "height": height, "tiles": [tile_row(t) for t in tiles],
             "starts": starts, "cs_starts": cs_starts}
    for k in ("created", "modified", "author", "recommended"):
        if k in data:
            clean[k] = data[k]
    return clean, warnings


def summary(data: dict) -> dict:
    """A map's headline facts, for lists."""
    land = sum(1 for r in data["tiles"] if r[0] not in ("Ocean", "Coast", "Lakes"))
    return {"id": data.get("id"), "name": data.get("name"), "description": data.get("description", ""),
            "width": data["width"], "height": data["height"], "starts": len(data.get("starts") or []),
            "cs_starts": len(data.get("cs_starts") or []), "land_share": round(land / max(1, len(data["tiles"])), 3),
            "modified": data.get("modified")}


# ----------------------------------------------------------------------------
# storage
# ----------------------------------------------------------------------------
def _path(map_id: str) -> Path:
    """The file for a map id, rejecting anything that is not a plain slug."""
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,79}", map_id or ""):
        raise MapError(f"Invalid map id '{map_id}'.")
    return MAP_DIR / f"{map_id}.json"


def list_maps() -> list[dict]:
    """Every saved map, in summary."""
    MAP_DIR.mkdir(parents=True, exist_ok=True)
    out = []
    for p in sorted(MAP_DIR.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True):
        try:
            out.append(summary(json.loads(p.read_text(encoding="utf-8"))))
        except (OSError, ValueError, KeyError):
            continue
    return out


def load_map(map_id: str) -> dict:
    """Load a map from disk."""
    p = _path(map_id)
    if not p.exists():
        raise MapError(f"No map '{map_id}'.")
    return json.loads(p.read_text(encoding="utf-8"))


def save_map(rules, data: dict, overwrite: bool = True) -> tuple[dict, list[str]]:
    """Validate and save a map, returning it with any problems that were fixed."""
    clean, warnings = validate(rules, data)
    clean["id"] = slug(clean["id"] or clean["name"])
    p = _path(clean["id"])
    if p.exists() and not overwrite:
        raise MapError(f"A map with id '{clean['id']}' already exists.")
    now = time.strftime("%Y-%m-%dT%H:%M:%S")
    clean["modified"] = now
    if p.exists():
        try:
            clean.setdefault("created", json.loads(p.read_text(encoding="utf-8")).get("created", now))
        except (OSError, ValueError):
            clean.setdefault("created", now)
    else:
        clean.setdefault("created", now)
    MAP_DIR.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(clean, separators=(",", ":")), encoding="utf-8")
    _fs_replace(tmp, p)
    return clean, warnings


def delete_map(map_id: str):
    """Delete a map."""
    p = _path(map_id)
    if p.exists():
        p.unlink()


# ----------------------------------------------------------------------------
# games on custom maps
# ----------------------------------------------------------------------------
def prepare(rules, data: dict, n_major: int, n_cs: int, rng: random.Random, nations: list, ruins: bool
            ) -> tuple[list[Tile], list[int], list[int], list[int]]:
    """Tiles, major starts, city-state starts and continent ids for a game on this map. Start positions the map
    doesn't provide are chosen like the generator does (fertile, far from the others)."""
    from . import mapgen
    clean, _ = validate(rules, data)
    width, height = clean["width"], clean["height"]
    tiles = tiles_from_rows(clean["tiles"])
    grid = HexGrid(width, height)
    m = mapgen._Map(rules, grid, tiles, rng)
    mapgen._assign_continents(m)
    starts = clean["starts"][:n_major]
    if len(starts) < n_major:
        starts = _fill_starts(m, starts, n_major, min_gap=7)
        if len(starts) < n_major:
            raise MapError(f"This map has room for only {len(starts)} civilizations (asked for {n_major}).")
    cs_starts = [s for s in clean["cs_starts"] if all(grid.distance(s, x) >= 3 for x in starts)][:n_cs]
    if len(cs_starts) < n_cs:
        cs_starts = _fill_starts(m, cs_starts, n_cs, min_gap=4, avoid=starts, avoid_gap=6)
    if ruins and not any(t.improvement == "Ancient ruins" for t in tiles):
        mapgen._ruins(m, starts, cs_starts)
    return tiles, starts, cs_starts, m.continent


def _fill_starts(m, have: list[int], n: int, min_gap: int, avoid: list[int] = (), avoid_gap: int = 0) -> list[int]:
    """Add start positions to a map that does not have enough, spread out from the existing ones."""
    from . import mapgen
    grid = m.grid
    out = list(have)
    cand = [i for i in mapgen._start_candidates(m) if not m.tiles[i].wonder and m.tiles[i].improvement is None]
    cand.sort(key=lambda i: -mapgen._start_score(m, i))
    for gap in range(min_gap, 1, -1):
        for i in cand:
            if len(out) >= n:
                return out
            if i in out or any(grid.distance(i, s) < gap for s in out):
                continue
            if avoid and any(grid.distance(i, s) < max(avoid_gap, gap) for s in avoid):
                continue
            out.append(i)
        if len(out) >= n:
            break
    return out

"""Hex grid math.

Tiles are addressed externally by "odd-r" offset coordinates (x = column, y = row,
odd rows shifted right by half a hex) and internally by a flat index idx = y * width + x.
Cube coordinates are used for distance, rings and lines.

A map may wrap east-west, north-south or both. Wrapping is entirely a property of the grid: every
neighbour, distance, ring and line below already accounts for it, so nothing that goes through
:class:`HexGrid` needs to know. North-south wrapping needs an even height, because odd rows are
shifted and a seam between two rows of the same parity would not tile.
"""
from __future__ import annotations

DIRS_EVEN = [(1, 0), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1)]
DIRS_ODD = [(1, 0), (1, -1), (0, -1), (-1, 0), (0, 1), (1, 1)]
DIR_NAMES = ["E", "NE", "NW", "W", "SW", "SE"]


def offset_to_cube(x: int, y: int) -> tuple[int, int, int]:
    """Convert offset coordinates to cube coordinates.

    Offset coordinates are what the map is stored and displayed in; cube coordinates are what hex
    arithmetic is simple in. Distance in particular is trivial in cube form and awkward in offset form,
    which is the whole reason for the conversion.
    """
    q = x - (y - (y & 1)) // 2
    r = y
    return q, r, -q - r


def cube_to_offset(q: int, r: int) -> tuple[int, int]:
    """Convert cube coordinates back to offset coordinates."""
    return q + (r - (r & 1)) // 2, r


def cube_distance(a, b) -> int:
    """The distance between two tiles in cube coordinates: half the sum of the absolute differences."""
    return (abs(a[0] - b[0]) + abs(a[1] - b[1]) + abs(a[2] - b[2])) // 2


class HexGrid:
    """The hex map's geometry: size, neighbours, distance and rings.

    Pure geometry with no game state at all, which is why it can be shared by the engine, the map
    editor and the generator without any of them knowing about the others.
    """
    def __init__(self, width: int, height: int, wrap_x: bool = False, wrap_y: bool = False):
        self.width = width
        self.height = height
        self.wrap_x = bool(wrap_x)
        self.wrap_y = bool(wrap_y) and height % 2 == 0
        self.size = width * height
        self._cube = [offset_to_cube(i % width, i // width) for i in range(self.size)]
        # The cube translations that land on the same tile again. One map width east is (+W, 0); one
        # map height south is (-H/2, +H), because every second row is shifted half a hex.
        xs = (0, -width, width) if self.wrap_x else (0,)
        ys = (0, -1, 1) if self.wrap_y else (0,)
        self._shifts = [(sx - sy * (height // 2), sy * height) for sy in ys for sx in xs]
        self._neighbors: list[list[int]] = []
        for i in range(self.size):
            x, y = i % width, i // width
            dirs = DIRS_ODD if y & 1 else DIRS_EVEN
            nb = []
            for dx, dy in dirs:
                n = self.wrap(x + dx, y + dy)
                if n is not None and n != i and n not in nb:
                    nb.append(n)
            self._neighbors.append(nb)
        self._within_cache: dict[tuple[int, int], list[int]] = {}

    @property
    def wraps(self) -> bool:
        """Whether the map wraps in either direction."""
        return self.wrap_x or self.wrap_y

    def wrap(self, x: int, y: int) -> int | None:
        """The index of a coordinate that may lie past a wrapping edge, or None if it is off the map."""
        if self.wrap_x:
            x %= self.width
        if self.wrap_y:
            y %= self.height
        if 0 <= x < self.width and 0 <= y < self.height:
            return y * self.width + x
        return None

    def idx(self, x: int, y: int) -> int:
        """The tile index at these coordinates."""
        return y * self.width + x

    def xy(self, idx: int) -> tuple[int, int]:
        """The coordinates of a tile index."""
        return idx % self.width, idx // self.width

    def in_bounds(self, x: int, y: int) -> bool:
        """Whether coordinates are on the map."""
        return 0 <= x < self.width and 0 <= y < self.height

    def neighbors(self, idx: int) -> list[int]:
        """The tiles adjacent to this one, cached.

        Cached because pathfinding, visibility and yield calculation all walk neighbours constantly, and
        the answer never changes for a given map.
        """
        return self._neighbors[idx]

    def neighbor_in_dir(self, idx: int, d: int) -> int | None:
        """The neighbour in one of the six directions, or None at the map edge."""
        x, y = self.xy(idx)
        dx, dy = (DIRS_ODD if y & 1 else DIRS_EVEN)[d]
        return self.wrap(x + dx, y + dy)

    def _nearest_shift(self, a: int, b: int) -> tuple[int, int]:
        """The cube translation that brings b's copy closest to a ((0, 0) on a map that does not wrap)."""
        if len(self._shifts) == 1:
            return 0, 0
        (aq, ar, _), (bq, br, _) = self._cube[a], self._cube[b]
        best, best_d = (0, 0), None
        for sq, sr in self._shifts:
            dq, dr = bq + sq - aq, br + sr - ar
            d = abs(dq) + abs(dr) + abs(dq + dr)
            if best_d is None or d < best_d:
                best, best_d = (sq, sr), d
        return best

    def distance(self, a: int, b: int) -> int:
        """The number of steps between two tiles - the short way round, on a wrapping map."""
        if len(self._shifts) == 1:
            return cube_distance(self._cube[a], self._cube[b])
        (aq, ar, _), (bq, br, _) = self._cube[a], self._cube[b]
        best = None
        for sq, sr in self._shifts:
            dq, dr = bq + sq - aq, br + sr - ar
            d = abs(dq) + abs(dr) + abs(dq + dr)
            if best is None or d < best:
                best = d
        return best // 2

    def within(self, idx: int, radius: int) -> list[int]:
        """All tiles within radius (inclusive), including idx itself, sorted by distance."""
        key = (idx, radius)
        cached = self._within_cache.get(key)
        if cached is not None:
            return cached
        q0, r0, _ = self._cube[idx]
        out = []
        seen = set()
        for dq in range(-radius, radius + 1):
            for dr in range(max(-radius, -dq - radius), min(radius, -dq + radius) + 1):
                x, y = cube_to_offset(q0 + dq, r0 + dr)
                t = self.wrap(x, y)
                if t is not None and t not in seen:
                    seen.add(t)
                    out.append(t)
        out.sort(key=lambda t: self.distance(idx, t))
        if len(self._within_cache) < 200000:
            self._within_cache[key] = out
        return out

    def ring(self, idx: int, radius: int) -> list[int]:
        """Every tile at exactly this distance from a centre."""
        return [t for t in self.within(idx, radius) if self.distance(idx, t) == radius]

    def line(self, a: int, b: int) -> list[int]:
        """Tiles on the straight line from a to b, inclusive of both ends (the short way round)."""
        n = self.distance(a, b)
        if n == 0:
            return [a]
        ca, cb = self._cube[a], self._cube[b]
        sq, sr = self._nearest_shift(a, b)
        cb = (cb[0] + sq, cb[1] + sr, cb[2] - sq - sr)
        out = []
        for i in range(n + 1):
            t = i / n
            fq = ca[0] + (cb[0] - ca[0]) * t + 1e-6
            fr = ca[1] + (cb[1] - ca[1]) * t + 1e-6
            fs = ca[2] + (cb[2] - ca[2]) * t - 2e-6
            rq, rr, rs = round(fq), round(fr), round(fs)
            dq, dr, ds = abs(rq - fq), abs(rr - fr), abs(rs - fs)
            if dq > dr and dq > ds:
                rq = -rr - rs
            elif dr > ds:
                rr = -rq - rs
            x, y = cube_to_offset(rq, rr)
            idx = self.wrap(x, y)
            if idx is not None and (not out or out[-1] != idx):
                out.append(idx)
        return out

    def unwrapped_xy(self, a: int, b: int) -> tuple[int, int]:
        """b's offset coordinates as seen from a: the copy of b nearest to a, which may lie off the map.

        On a map that does not wrap this is just b's coordinates. Anything that measures a direction
        or draws a line between two tiles wants this rather than :meth:`xy`.
        """
        bx, by = self.xy(b)
        sq, sr = self._nearest_shift(a, b)
        # a cube shift of (sq, sr) is (sq + sr/2, sr) in offset terms; sr is a multiple of an even height
        return bx + sq + sr // 2, by + sr

    def direction_name(self, a: int, b: int) -> str:
        """Rough compass direction from a to b (for text briefings)."""
        ax, ay = self.xy(a)
        bx, by = self.unwrapped_xy(a, b)
        dx = (bx + (0.5 if by & 1 else 0)) - (ax + (0.5 if ay & 1 else 0))
        dy = by - ay
        if dx == 0 and dy == 0:
            return "here"
        ns = "N" if dy < 0 else ("S" if dy > 0 else "")
        ew = "E" if dx > 0 else ("W" if dx < 0 else "")
        if abs(dy) > 2 * abs(dx):
            ew = ""
        if abs(dx) > 2 * abs(dy):
            ns = ""
        return ns + ew

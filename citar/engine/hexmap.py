"""Hex grid math.

Tiles are addressed externally by "odd-r" offset coordinates (x = column, y = row,
odd rows shifted right by half a hex) and internally by a flat index idx = y * width + x.
Cube coordinates are used for distance, rings and lines. No wraparound.
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
    def __init__(self, width: int, height: int):
        self.width = width
        self.height = height
        self.size = width * height
        self._cube = [offset_to_cube(i % width, i // width) for i in range(self.size)]
        self._neighbors: list[list[int]] = []
        for i in range(self.size):
            x, y = i % width, i // width
            dirs = DIRS_ODD if y & 1 else DIRS_EVEN
            nb = []
            for dx, dy in dirs:
                nx, ny = x + dx, y + dy
                if 0 <= nx < width and 0 <= ny < height:
                    nb.append(ny * width + nx)
            self._neighbors.append(nb)
        self._within_cache: dict[tuple[int, int], list[int]] = {}

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
        nx, ny = x + dx, y + dy
        if self.in_bounds(nx, ny):
            return self.idx(nx, ny)
        return None

    def distance(self, a: int, b: int) -> int:
        """The number of steps between two tiles."""
        return cube_distance(self._cube[a], self._cube[b])

    def within(self, idx: int, radius: int) -> list[int]:
        """All tiles within radius (inclusive), including idx itself, sorted by distance."""
        key = (idx, radius)
        cached = self._within_cache.get(key)
        if cached is not None:
            return cached
        q0, r0, _ = self._cube[idx]
        out = []
        for dq in range(-radius, radius + 1):
            for dr in range(max(-radius, -dq - radius), min(radius, -dq + radius) + 1):
                x, y = cube_to_offset(q0 + dq, r0 + dr)
                if self.in_bounds(x, y):
                    out.append(y * self.width + x)
        out.sort(key=lambda t: self.distance(idx, t))
        if len(self._within_cache) < 200000:
            self._within_cache[key] = out
        return out

    def ring(self, idx: int, radius: int) -> list[int]:
        """Every tile at exactly this distance from a centre."""
        return [t for t in self.within(idx, radius) if self.distance(idx, t) == radius]

    def line(self, a: int, b: int) -> list[int]:
        """Tiles on the straight line from a to b, inclusive of both ends."""
        n = self.distance(a, b)
        if n == 0:
            return [a]
        ca, cb = self._cube[a], self._cube[b]
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
            idx = y * self.width + x
            if not out or out[-1] != idx:
                out.append(idx)
        return out

    def direction_name(self, a: int, b: int) -> str:
        """Rough compass direction from a to b (for text briefings)."""
        ax, ay = self.xy(a)
        bx, by = self.xy(b)
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

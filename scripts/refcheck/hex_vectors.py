"""Record the Python HexGrid's answers, for the Rust port's hex test (crates/citar-engine DESIGN.md 9.1, package 1a-02).

    PYTHONHASHSEED=0 python scripts/refcheck/hex_vectors.py            # writes crates/citar-testkit/data/hex_vectors.json
    PYTHONHASHSEED=0 python scripts/refcheck/hex_vectors.py --check    # re-records and compares with the committed file

For duel, standard and gargantuan maps, each with no wrap, east-west, north-south and both, it records:

* samples, written out in full so a failure is readable: the neighbours of 16 tiles (corners, edge midpoints, the
  centre and 7 seeded picks); the distance of 120 pairs and the line of 40 (seam-crossing pairs included); within
  and ring round the sample tiles at a few radii; on duel, one radius large enough to wrap all the way round;
* checksums over every tile, so nothing is left out: the neighbour lists, within at radius 1 and 2, ring at radius
  1 to 3, the distance from 8 tiles to every tile, and the line from 2 tiles to every tile.

Neighbours, distances and lines must match exactly. within and ring are compared as sets (written sorted), since
the Rust port lists a ring's tiles in its own documented order.

A checksum folds each value v of a list as h = (h * 1000003 + v + 2) mod 2^64, closing the list with v = -1, over
the lists in tile order. It is written as 16 hex digits.
"""
from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from citar.engine.hexmap import HexGrid  # noqa: E402

OUT = ROOT / "crates" / "citar-testkit" / "data" / "hex_vectors.json"
SIZES = (("duel", 44, 28), ("standard", 76, 48), ("gargantuan", 160, 100))
WRAPS = ((False, False), (True, False), (False, True), (True, True))
MASK = (1 << 64) - 1


class Checksum:
    """The fold described in the module docstring."""

    def __init__(self):
        self.h = 0

    def value(self, v: int):
        self.h = (self.h * 1000003 + v + 2) & MASK

    def seq(self, xs):
        for x in xs:
            self.value(x)
        self.value(-1)

    def hex(self) -> str:
        return f"{self.h:016x}"


def sample_tiles(g: HexGrid, rng: random.Random) -> list[int]:
    """Corners, edge midpoints, the centre, and seeded picks: 16 distinct tiles."""
    w, h = g.width, g.height
    fixed = [g.idx(0, 0), g.idx(w - 1, 0), g.idx(0, h - 1), g.idx(w - 1, h - 1),
             g.idx(w // 2, 0), g.idx(w // 2, h - 1), g.idx(0, h // 2), g.idx(w - 1, h // 2 + 1),
             g.idx(w // 2, h // 2)]
    out = list(dict.fromkeys(fixed))
    while len(out) < 16:
        t = rng.randrange(g.size)
        if t not in out:
            out.append(t)
    return out


def record_grid(name: str, w: int, h: int, wrap_x: bool, wrap_y: bool) -> dict:
    g = HexGrid(w, h, wrap_x=wrap_x, wrap_y=wrap_y)
    label = f"{name}-{'x' if wrap_x else ''}{'y' if wrap_y else ''}".rstrip("-")
    rng = random.Random(f"hex:{label}")
    tiles = sample_tiles(g, rng)

    pairs = [(rng.randrange(g.size), rng.randrange(g.size)) for _ in range(100)]
    for y in (0, h // 2, h - 1):  # across the east-west seam
        pairs.append((g.idx(0, y), g.idx(w - 1, y)))
        pairs.append((g.idx(1, y), g.idx(w - 2, (y + 3) % h)))
    for x in (0, w // 2, w - 1):  # across the north-south seam
        pairs.append((g.idx(x, 0), g.idx(x, h - 1)))
        pairs.append((g.idx(x, 1), g.idx((x + 5) % w, h - 2)))
    pairs.extend((t, t) for t in tiles[:8])

    rec = {
        "name": label, "width": w, "height": h, "wrap_x": wrap_x, "wrap_y": wrap_y,
        "tiles": tiles,
        "neighbors": [g.neighbors(t) for t in tiles],
        "distance": [[a, b, g.distance(a, b)] for a, b in pairs],
        "line": [[a, b, g.line(a, b)] for a, b in pairs[:26] + pairs[100:114]],
        "within": [[t, r, sorted(g.within(t, r))] for t in tiles for r in (0, 1, 2, 4)],
        "ring": [[t, r, sorted(g.ring(t, r))] for t in tiles for r in (1, 3, 5)],
    }
    if name == "duel":  # radii that wrap all the way round, on the smallest map
        centre = tiles[8]
        rec["within"] += [[t, r, sorted(g.within(t, r))] for t in (tiles[0], centre) for r in (15, 30)]
        rec["ring"] += [[t, r, sorted(g.ring(t, r))] for t in (tiles[0], centre) for r in (14, 20, 30)]

    sums = {}
    every = range(g.size)
    c = Checksum()
    for t in every:
        c.seq(g.neighbors(t))
    sums["neighbors"] = c.hex()
    for r in (1, 2):
        c = Checksum()
        for t in every:
            c.seq(sorted(g.within(t, r)))
        sums[f"within{r}"] = c.hex()
    for r in (1, 2, 3):
        c = Checksum()
        for t in every:
            c.seq(sorted(g.ring(t, r)))
        sums[f"ring{r}"] = c.hex()
    c = Checksum()
    for src in tiles[:8]:
        c.seq([g.distance(src, t) for t in every])
    sums["distance"] = c.hex()
    c = Checksum()
    for src in (tiles[0], tiles[8]):
        for t in every:
            c.seq(g.line(src, t))
    sums["line"] = c.hex()
    rec["sums"] = sums
    return rec


def render(grids: list[dict]) -> str:
    """One grid per line: small enough to diff by grid, without a line per number."""
    head = {"format": 1, "source": "citar/engine/hexmap.py", "recorder": "scripts/refcheck/hex_vectors.py"}
    body = ",\n".join(json.dumps(g, separators=(",", ":")) for g in grids)
    return json.dumps(head)[:-1] + ', "grids": [\n' + body + "\n]}\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing it")
    args = ap.parse_args()
    grids = [record_grid(name, w, h, wx, wy) for name, w, h in SIZES for wx, wy in WRAPS]
    text = render(grids)
    if args.check:
        same = OUT.exists() and OUT.read_text(encoding="utf-8") == text
        print("hex_vectors.json is up to date" if same else "hex_vectors.json differs from a fresh recording")
        return 0 if same else 1
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT)} ({len(text) // 1024} KB, {len(grids)} grids)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

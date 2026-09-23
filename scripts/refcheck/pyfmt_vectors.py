"""Record how Python writes and rounds floats, and divides ints, for the Rust port (crates/citar-engine DESIGN.md 7.3,
package 1a-02).

    PYTHONHASHSEED=0 python scripts/refcheck/pyfmt_vectors.py            # writes crates/citar-testkit/golden/pyfmt.json
    PYTHONHASHSEED=0 python scripts/refcheck/pyfmt_vectors.py --check    # re-records and compares with the committed file

About 2,000 floats: special values (zeros, the bounds, NaN and the infinities), exact ties at many digit counts
(0.125, 2.5, 5.0 at the tens), near-ties that are not ties (2.675, 1.005), the values games produce (moves in sixtieths,
small fractions, one- to three-decimal amounts), a log-uniform spread from 1e-12 to 1e22, and random bit patterns.
For each one it records the bits, repr(x), float(round(x)), and repr(round(x, n)) for n = -1..3; the ties and
specials also get the digit counts in EXTRA_NDIGITS. It also records Python's a // b and a % b over signed 64-bit
operands, the edges included.

The Rust side (citar-testkit's golden check) must reproduce every string and every bit. The file is also one of the
determinism goldens, so it is compared on all five targets; only this script writes it.
"""
from __future__ import annotations

import argparse
import json
import random
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "crates" / "citar-testkit" / "golden" / "pyfmt.json"
NDIGITS = (-1, 0, 1, 2, 3)
EXTRA_NDIGITS = (-308, -22, -3, -2, 4, 5, 8, 12, 17, 20, 300, 323, 324, -309)


def bits(x: float) -> str:
    return f"{struct.unpack('<Q', struct.pack('<d', x))[0]:016x}"


def from_bits(b: int) -> float:
    return struct.unpack("<d", struct.pack("<Q", b))[0]


def specials() -> list[float]:
    xs = [0.0, -0.0, 0.5, 1.5, 2.5, 3.5, -0.5, -1.5, -2.5, 0.125, 0.375, 0.625, 0.875, -0.125, -0.375, 2.675, -2.675,
          1.005, 0.045, 1.0625, 0.1, 0.2, 0.3, 0.7, 1 / 3, 2 / 3, 1e16, 1e15, 9999999999999998.0, 1e17, 1e22, 1e23,
          1.5e16, 1e-4, 1e-5, 1.5e-5, 0.0001, 0.00012345, 123456789.0, 2.0 ** 52, 2.0 ** 52 + 0.5, 2.0 ** 53,
          2.0 ** 53 + 2, 2.0 ** 63, 2.0 ** 64, 5e-324, 1e-323, 2.2250738585072014e-308, 2.225073858507201e-308,
          1.7976931348623157e308, -1.7976931348623157e308, -1e-7, 3.141592653589793, 2.718281828459045, 100.0, 1e100,
          1e-100, 123.456, 0.05, 0.15, 0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85, 0.95, 5.0, 15.0, 25.0, 35.0, 45.0,
          55.0, 150.0, 250.0, 1250.0, 5e21, 5e22, 1e21, 999999.5, 0.999999999, 9.995, 99.995, 0.0005, 0.00015, 1e-3,
          4.35, 8.675, 10.0, 12.5, 7.0, -7.25, 1234.5678, 0.3333333333333333, 1e300, 1.23e-300]
    return xs + [float("inf"), float("-inf"), float("nan")]


def ties() -> list[float]:
    """k / 2^j for odd k: an exact tie at j - 1 decimals, and at other counts a clean or rounding case."""
    out = []
    for j in range(1, 14):
        for k in (1, 3, 5, 7, 9, 11, 13, 15, 17, 19, 21, 23, 25, 27, 29, 31, 101, 1001, 12345):
            x = k / 2 ** j
            out.append(x)
            if k % 3 == 0:
                out.append(-x)
    for m in range(1, 60):  # multiples of 5 and 50: ties at the tens and hundreds
        out.append(5.0 * (2 * m - 1))
        out.append(50.0 * (2 * m - 1))
    return out


def game_values(rng: random.Random) -> list[float]:
    out = [i / 60 for i in range(0, 600, 7)]
    out += [a / b for a in range(1, 40, 3) for b in (3, 6, 7, 9, 11, 12)]
    for _ in range(250):
        out.append(round(rng.uniform(-500, 5000), rng.choice((1, 2, 3))))
    for _ in range(150):
        out.append(rng.uniform(0, 100) * rng.choice((1, 1.5, 0.75, 1.25, 0.9)))
    return out


def spread(rng: random.Random) -> list[float]:
    out = []
    for _ in range(400):
        x = 10 ** rng.uniform(-12, 22)
        out.append(x if rng.random() < 0.8 else -x)
    while len(out) < 800:
        x = from_bits(rng.getrandbits(64))
        if x == x and abs(x) != float("inf"):
            out.append(x)
    return out


def floor_cases(rng: random.Random) -> list[list[int]]:
    edges = [0, 1, -1, 2, -2, 3, -3, 7, -7, 10, -10, 60, -60, 999, -1000, 2 ** 31 - 1, -2 ** 31, 2 ** 31, -2 ** 31 - 1,
             2 ** 63 - 1, -2 ** 63]
    ops = edges + [rng.randrange(-10 ** 6, 10 ** 6) for _ in range(10)] + [rng.randrange(-2 ** 63, 2 ** 63)
                                                                           for _ in range(6)]
    out = []
    for a in ops:
        for b in ops:
            if b == 0:
                continue
            q, r = a // b, a % b
            if -2 ** 63 <= q < 2 ** 63:  # MIN // -1 overflows i64; the Rust side saturates it
                out.append([a, b, q, r])
    return out


def record() -> dict:
    rng = random.Random(20260923)
    xs = specials() + ties() + game_values(rng) + spread(rng)
    seen, cases = set(), []
    for x in xs:
        key = bits(x)
        if key in seen:
            continue
        seen.add(key)
        whole = repr(float(round(x))) if x == x and abs(x) != float("inf") else None
        cases.append([key, repr(x), whole, [repr(round(x, n)) for n in NDIGITS]])
    extra = []
    for x in specials() + ties()[:60]:
        for n in EXTRA_NDIGITS:
            try:
                extra.append([bits(x), n, repr(round(x, n))])
            except OverflowError:
                extra.append([bits(x), n, None])
    return {"format": 1, "recorder": "scripts/refcheck/pyfmt_vectors.py", "python": sys.version.split()[0],
            "ndigits": list(NDIGITS), "cases": cases, "extra": extra, "floor": floor_cases(rng)}


def render(doc: dict) -> str:
    """One case per line."""
    lists = ("cases", "extra", "floor")
    head = {k: v for k, v in doc.items() if k not in lists}
    parts = [json.dumps(head)[:-1]]
    for k in lists:
        rows = ",\n".join(json.dumps(row, separators=(",", ":")) for row in doc[k])
        parts.append(f', "{k}": [\n{rows}\n]')
    return "".join(parts) + "}\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing it")
    args = ap.parse_args()
    text = render(record())
    if args.check:
        # The Python version is recorded but not compared: the answers must not depend on it.
        def strip(t: str) -> dict:
            d = json.loads(t)
            d.pop("python", None)
            return d
        same = OUT.exists() and strip(OUT.read_text(encoding="utf-8")) == strip(text)
        print("pyfmt.json is up to date" if same else "pyfmt.json differs from a fresh recording")
        return 0 if same else 1
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    doc = json.loads(text)
    print(f"wrote {OUT.relative_to(ROOT)} ({len(text) // 1024} KB: {len(doc['cases'])} floats, "
          f"{len(doc['extra'])} extra roundings, {len(doc['floor'])} divisions)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

// The minimap's window (citar/web/js/minimap.js; design/phase3-features.md P3.7.3 and P3.7.5): the shortest interval
// across a seam, the cap of each stage, never shrinking, the letterbox maths, a click mapped to a tile and back on a
// wrapping map, and the window kept in storage. Run with `node --test "citar/web/tests/*.test.mjs"` from the checkout,
// or `npm test` in citar/web.
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  EARLY, PAD, SQRT3, computeWindow, covering, exploredBox, fit, inWindow, loadWindow, sameBox, saveWindow,
  tileCentre, toScreen, toTile, validBox, wholeMap, windowKey,
} from "../js/minimap.js";

const marks = (n, at) => Array.from({ length: n }, (_, i) => at.includes(i));
const box = (b) => ({ x0: b.x0, y0: b.y0, x1: b.x1, y1: b.y1 });

// the positions a box covers on one axis
function positions(lo, hi, n) {
  const out = [];
  for (let i = lo; ; i = (i + 1) % n) {
    out.push(i);
    if (i === hi) return out;
  }
}

function covers(outer, inner, map) {
  const xs = positions(outer.x0, outer.x1, map.width), ys = positions(outer.y0, outer.y1, map.height);
  return positions(inner.x0, inner.x1, map.width).every((x) => xs.includes(x))
    && positions(inner.y0, inner.y1, map.height).every((y) => ys.includes(y));
}

test("the covering interval leaves out the longest gap, across a seam when it wraps", () => {
  assert.equal(covering(marks(10, []), true), null);
  assert.deepEqual(covering(marks(10, [0, 1, 8, 9]), false), [0, 9]);
  assert.deepEqual(covering(marks(10, [0, 1, 8, 9]), true), [8, 1], "across the seam");
  assert.deepEqual(covering(marks(10, [9, 0]), true), [9, 0]);
  assert.deepEqual(covering(marks(10, [2, 3, 4]), true), [2, 4]);
  assert.deepEqual(covering(marks(10, [6]), true), [6, 6]);
  assert.deepEqual(covering(new Array(10).fill(true), true), [0, 9], "every position");
  // the engine's tie-breaks (api/views/minimap.rs): the seam's gap first, then the first inside
  assert.deepEqual(covering(marks(8, [2, 5]), true), [2, 5]);
  assert.deepEqual(covering(marks(12, [0, 4, 8, 11]), true), [4, 0]);
});

test("the explored box reads the view's tiles, the short way round a wrapping map", () => {
  const w = 20, h = 10;
  const at = (x, y) => y * w + x;
  const rows = [[at(18, 4), "Grassland"], [at(19, 5), "Plains"], [at(1, 5), "Plains"], [at(2, 6), "Coast"]];
  assert.deepEqual(exploredBox(rows, w, h, true, false), { x0: 18, y0: 4, x1: 2, y1: 6 });
  assert.deepEqual(exploredBox(rows, w, h, false, false), { x0: 1, y0: 4, x1: 19, y1: 6 });
  assert.deepEqual(exploredBox(rows.map((r) => r[0]), w, h, true, false), { x0: 18, y0: 4, x1: 2, y1: 6 }, "indexes");
  assert.equal(exploredBox([], w, h, true, true), null);
  assert.equal(exploredBox([[-1], [w * h], ["x"]], w, h, false, false), null, "nothing on the map");
});

const BIG = { width: 80, height: 52, wrapX: true, wrapY: false };

test("early on a large map: the 44 by 28 tiles around the capital, within what was explored", () => {
  const explored = { x0: 10, y0: 5, x1: 70, y1: 45 };
  const win = computeWindow({ ...BIG, stage: "early", explored, capital: [40, 25], home: explored });
  assert.deepEqual(box(win), { x0: 18, y0: 11, x1: 61, y1: 38 });
  assert.deepEqual([win.cols, win.rows], [EARLY.cols, EARLY.rows]);
  // a small explored area: itself and its margin, the cap reaching further
  const small = computeWindow({ ...BIG, stage: "early", explored: { x0: 38, y0: 22, x1: 44, y1: 28 }, capital: [41, 25] });
  assert.deepEqual(box(small), { x0: 38 - PAD, y0: 22 - PAD, x1: 44 + PAD, y1: 28 + PAD });
  // the margin stops at an edge that does not wrap, and runs over the one that does
  const edge = computeWindow({ ...BIG, stage: "early", explored: { x0: 0, y0: 0, x1: 5, y1: 4 }, capital: [2, 2] });
  assert.deepEqual(box(edge), { x0: 77, y0: 0, x1: 8, y1: 7 });
  // near an edge that does not wrap, the 44 rows keep their size, moved inside the map
  const flat = { width: 80, height: 52, wrapX: false, wrapY: false };
  const west = computeWindow({ ...flat, stage: "early", explored: { x0: 0, y0: 0, x1: 50, y1: 50 }, capital: [2, 49] });
  assert.deepEqual(box(west), { x0: 0, y0: 24, x1: 43, y1: 51 });
  // without a capital, the explored box's middle
  const middle = computeWindow({ ...BIG, stage: "early", explored, capital: null });
  assert.deepEqual([middle.x0, middle.x1, middle.cols, middle.rows], [18, 61, 44, 28]);
});

test("early near the seam of a wrapping map: the window crosses it", () => {
  const explored = { x0: 60, y0: 10, x1: 20, y1: 40 };          // across the seam, 41 columns
  const win = computeWindow({ ...BIG, stage: "early", explored, capital: [2, 25] });
  assert.deepEqual(box(win), { x0: 60, y0: 11, x1: 23, y1: 38 }, "44 columns from 60 over the seam");
  assert.equal(win.cols, 44);
});

test("early on a map no larger than 44 by 28: no cap", () => {
  const duel = { width: 40, height: 26, wrapX: false, wrapY: false };
  const win = computeWindow({ ...duel, stage: "early", explored: { x0: 2, y0: 2, x1: 37, y1: 23 }, capital: [5, 5] });
  assert.deepEqual(box(win), { x0: 0, y0: 0, x1: 39, y1: 25 });
});

test("the Renaissance: what was explored near the home continent", () => {
  const explored = { x0: 0, y0: 0, x1: 79, y1: 51 };
  const home = { x0: 70, y0: 10, x1: 9, y1: 30 };               // the home continent across the seam
  const win = computeWindow({ ...BIG, stage: "renaissance", explored, home, capital: [75, 20] });
  assert.deepEqual(box(win), { x0: 67, y0: 7, x1: 12, y1: 33 });
  // the home's margin stays within what was explored
  const near = computeWindow({ ...BIG, stage: "renaissance", explored: { x0: 68, y0: 9, x1: 10, y1: 31 }, home });
  assert.deepEqual(box(near), { x0: 67, y0: 7, x1: 12, y1: 33 }, "the explored box's own margin");
  // no home (no city, no settler): what was explored
  const none = computeWindow({ ...BIG, stage: "renaissance", explored: { x0: 30, y0: 20, x1: 40, y1: 30 }, home: null });
  assert.deepEqual(box(none), { x0: 27, y0: 17, x1: 43, y1: 33 });
});

test("the Information era: the whole map", () => {
  const win = computeWindow({ ...BIG, stage: "information", explored: { x0: 30, y0: 20, x1: 40, y1: 30 } });
  assert.deepEqual(win, wholeMap(BIG));
  assert.deepEqual([win.cols, win.rows], [80, 52]);
});

test("the window never shrinks: each covers the one before, the short way round", () => {
  const steps = [
    { stage: "early", explored: { x0: 2, y0: 20, x1: 8, y1: 26 }, capital: [5, 23] },
    { stage: "early", explored: { x0: 70, y0: 18, x1: 9, y1: 30 }, capital: [5, 23] },
    { stage: "early", explored: { x0: 4, y0: 22, x1: 6, y1: 24 }, capital: [5, 23] },  // less explored: no matter
    { stage: "renaissance", explored: { x0: 50, y0: 5, x1: 30, y1: 45 }, home: { x0: 72, y0: 15, x1: 12, y1: 35 } },
    { stage: "information", explored: { x0: 50, y0: 5, x1: 30, y1: 45 } },
  ];
  let prev = null;
  const seen = [];
  for (const s of steps) {
    const win = computeWindow({ ...BIG, ...s, prev });
    if (prev) assert.ok(covers(win, prev, BIG), `${JSON.stringify(win)} covers ${JSON.stringify(prev)}`);
    seen.push(box(win));
    prev = box(win);
  }
  assert.deepEqual(seen[1], { x0: 67, y0: 15, x1: 12, y1: 33 }, "grown over the seam, not round the far side");
  assert.deepEqual(seen[2], seen[1], "less explored changes nothing");
  assert.deepEqual(seen[4], box(wholeMap(BIG)));
  // a window kept from before that is not one of this map is passed over
  const fresh = computeWindow({ ...BIG, stage: "early", explored: steps[0].explored, capital: [5, 23],
                                prev: { x0: 200, y0: 0, x1: 3, y1: 2 } });
  assert.deepEqual(box(fresh), box(computeWindow({ ...BIG, ...steps[0] })));
  // nothing explored: the window before, or the whole map
  assert.deepEqual(box(computeWindow({ ...BIG, stage: "early", explored: null, prev: seen[1] })), seen[1]);
  assert.deepEqual(computeWindow({ ...BIG, stage: "early", explored: null }), wholeMap(BIG));
});

test("fit: one scale for both axes, hex-correct, letterboxed", () => {
  const wide = fit({ cols: 44, rows: 28 }, 200, 130);
  const w = SQRT3 * 44.5, h = 1.5 * 28 + 0.5;
  assert.ok(Math.abs(wide.scale - 200 / w) < 1e-9, "the width decides");
  assert.ok(Math.abs(wide.ox) < 1e-9);
  assert.ok(Math.abs(wide.oy - (130 - h * wide.scale) / 2) < 1e-9 && wide.oy > 0, "bands above and below");
  const tall = fit({ cols: 10, rows: 30 }, 200, 130);
  assert.ok(Math.abs(tall.scale - 130 / (1.5 * 30 + 0.5)) < 1e-9, "the height decides");
  assert.ok(tall.ox > 0 && Math.abs(tall.oy) < 1e-9, "bands left and right");
  // a hex is √3 wide for each 1.5 of rows, whatever the window's shape
  const map = { width: 30, height: 20, wrapX: false, wrapY: false };
  const win = { x0: 0, y0: 0, x1: 29, y1: 19, cols: 30, rows: 20 };
  const f = fit(win, 200, 130);
  const [ax, ay] = tileCentre(f, win, map, 4, 4), [bx] = tileCentre(f, win, map, 5, 4), [, cy] = tileCentre(f, win, map, 4, 6);
  assert.ok(Math.abs((bx - ax) / ((cy - ay) / 2) - SQRT3 / 1.5) < 1e-9);
  // the window fills the widget's width or height and stays inside it
  for (const [x, y] of [[0, 0], [29, 19], [29, 1], [0, 19]]) {
    const [px, py] = tileCentre(f, win, map, x, y);
    assert.ok(px - f.scale * SQRT3 / 2 >= f.ox - 1e-9 && px + f.scale * SQRT3 / 2 <= 200 - f.ox + 1e-9, `${x},${y}`);
    assert.ok(py - f.scale >= f.oy - 1e-9 && py + f.scale <= 130 - f.oy + 1e-9, `${x},${y}`);
  }
});

test("a click maps to a tile and back, across a wrapping map's seams", () => {
  const map = { width: 24, height: 16, wrapX: true, wrapY: true };
  const win = computeWindow({ ...map, stage: "early", explored: { x0: 20, y0: 13, x1: 3, y1: 2 }, capital: [1, 0] });
  assert.ok(win.x0 > win.x1 && win.y0 > win.y1, "the window crosses both seams");
  const f = fit(win, 200, 130);
  let checked = 0;
  for (let y = 0; y < map.height; y++) {
    for (let x = 0; x < map.width; x++) {
      if (!inWindow(win, map, x, y)) continue;
      const [px, py] = tileCentre(f, win, map, x, y);
      assert.deepEqual(toTile(f, win, map, px, py), [x, y], `tile ${x},${y} at ${px.toFixed(1)},${py.toFixed(1)}`);
      // anywhere well inside the tile's hex finds it too
      assert.deepEqual(toTile(f, win, map, px + f.scale * 0.6, py - f.scale * 0.5), [x, y]);
      checked++;
    }
  }
  assert.equal(checked, win.cols * win.rows);
  // either side of the seam: the window's first column (17, the explored 20 less its margin) at the far left, its
  // last (6) at the far right
  assert.deepEqual(box(win), { x0: 17, y0: 10, x1: 6, y1: 5 });
  const [lx, ly] = tileCentre(f, win, map, 17, 10), [rx] = tileCentre(f, win, map, 6, 10);
  assert.ok(lx < rx, "column 17 is drawn left of column 6");
  assert.deepEqual(toTile(f, win, map, 0, ly), [17, 10], "the letterbox's edge finds the window's first column");
  assert.deepEqual(toTile(f, win, map, 199, ly), [6, 10]);
  // a point of the map (the camera's) is placed the short way round from the window's start
  const [cx] = toScreen(f, win, map, 22.5, 14);
  assert.ok(cx > lx && cx < rx);
  const whole = wholeMap({ width: 24, height: 16 });
  const g = fit(whole, 200, 130);
  for (const [x, y] of [[0, 0], [23, 15], [12, 7], [23, 0]]) {
    const [px, py] = tileCentre(g, whole, map, x, y);
    assert.deepEqual(toTile(g, whole, map, px, py), [x, y]);
  }
});

test("the window is kept per game and seat, whatever the storage does", () => {
  const map = { width: 80, height: 52, wrapX: true, wrapY: false };
  const kept = new Map();
  const storage = { getItem: (k) => kept.get(k) ?? null, setItem: (k, v) => kept.set(k, v) };
  const key = windowKey("g123", 2);
  assert.equal(key, "citar:minimap:g123:2");
  assert.equal(loadWindow(storage, key, map), null);
  saveWindow(storage, key, { x0: 70, y0: 3, x1: 9, y1: 30, cols: 20, rows: 28 }, map);
  assert.deepEqual(loadWindow(storage, key, map), { x0: 70, y0: 3, x1: 9, y1: 30 });
  assert.equal(loadWindow(storage, key, { ...map, width: 60 }), null, "another map's");
  assert.equal(loadWindow(storage, key, { ...map, wrapX: false }), null, "not a box of a map that does not wrap");
  for (const raw of ["not json", "null", "[1, 2]", '{"x0": 1.5, "y0": 0, "x1": 3, "y1": 2, "width": 80, "height": 52}']) {
    kept.set(key, raw);
    assert.equal(loadWindow(storage, key, map), null, raw);
  }
  const refusing = { getItem() { throw new Error("blocked"); }, setItem() { throw new Error("quota"); } };
  assert.equal(loadWindow(refusing, key, map), null);
  assert.doesNotThrow(() => saveWindow(refusing, key, { x0: 1, y0: 1, x1: 2, y1: 2 }, map));
  assert.equal(loadWindow(null, key, map), null);
  assert.doesNotThrow(() => saveWindow(undefined, key, { x0: 1, y0: 1, x1: 2, y1: 2 }, map));
  assert.ok(validBox({ x0: 70, y0: 3, x1: 9, y1: 30 }, map) && !validBox({ x0: 3, y0: 30, x1: 9, y1: 3 }, map));
  assert.ok(sameBox({ x0: 1, y0: 2, x1: 3, y1: 4, cols: 3 }, { x0: 1, y0: 2, x1: 3, y1: 4 }) && !sameBox(null, null));
});

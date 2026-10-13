// The game screen's minimap window (design/phase3-features.md P3.7.3, package 3-17): which part of the map a person's
// minimap shows, and the maths of drawing it and of pointing at it. Pure: no DOM and no network; the game screen
// (game.js) draws with it, and citar/web/tests/minimap.test.mjs tests it.
//
// A person's minimap shows what their civilization has explored, a little more, and no more: early on a window
// around the capital, from the Renaissance the explored tiles near the home continent (those the civilization has
// seen joined to its home), from the Information era the whole map. The window only grows, so the minimap zooms out over the game and never back in; the last one is kept
// per game and seat in localStorage. Spectators, model seats and the recap show the whole map.
//
// A box is {x0, y0, x1, y1}, its corners inclusive (as the engine's `minimap.home` is). On an axis that wraps it may
// cross the seam: then x0 > x1 (or y0 > y1), and it runs from x0 to the map's edge and on from 0 to x1.

export const SQRT3 = Math.sqrt(3);

/** Tiles of margin around what was explored (P3.7.3). */
export const PAD = 3;

/** The early window's size, in tiles, on a map larger than it (P3.7.3). */
export const EARLY = { cols: 44, rows: 28 };

const mod = (a, n) => ((a % n) + n) % n;

/**
 * The shortest interval of the positions 0..n-1 that covers every one marked, as [lo, hi] inclusive, or null when
 * none is. On a circle (`wrap`) it leaves out the longest run of unmarked positions, so it may cross the seam (lo >
 * hi); of two runs as long, the one across the seam is left out first (the interval then does not cross it), else
 * the first. The engine chooses the same way (crates/citar-engine/src/api/views/minimap.rs, `covering`).
 */
export function covering(marked, wrap) {
  const n = marked.length;
  let first = -1, last = -1;
  for (let i = 0; i < n; i++) {
    if (marked[i]) {
      if (first < 0) first = i;
      last = i;
    }
  }
  if (first < 0) return null;
  if (!wrap) return [first, last];
  let gap = n - 1 - last + first, after = last, prev = first;
  for (let i = first + 1; i <= last; i++) {
    if (!marked[i]) continue;
    if (i - prev - 1 > gap) { gap = i - prev - 1; after = prev; }
    prev = i;
  }
  if (after === last) return [first, last];
  let next = after + 1;
  while (!marked[next]) next++;
  return [next, after];
}

// ---- spans: one axis of a box, {start, len} ------------------------------------------------------------------

/** The span [lo, hi] covers on an axis of n positions. */
function spanOf(lo, hi, n, wrap) {
  const len = wrap && lo > hi ? hi + n - lo + 1 : hi - lo + 1;
  return len >= n ? { start: 0, len: n } : { start: lo, len };
}

/** The span's [lo, hi]. */
function endsOf(s, n, wrap) {
  return [s.start, wrap ? mod(s.start + s.len - 1, n) : s.start + s.len - 1];
}

function inSpan(s, p, n, wrap) {
  return wrap ? mod(p - s.start, n) < s.len : p >= s.start && p < s.start + s.len;
}

/** The span grown by `by` on both sides: clamped at the map's edges, or round a wrapping axis (whole at most). */
function grow(s, by, n, wrap) {
  if (wrap) return s.len + 2 * by >= n ? { start: 0, len: n } : { start: mod(s.start - by, n), len: s.len + 2 * by };
  const lo = Math.max(0, s.start - by), hi = Math.min(n - 1, s.start + s.len - 1 + by);
  return { start: lo, len: hi - lo + 1 };
}

/** `len` positions centred on `c`: round a wrapping axis; on one that does not, moved inside the map's edges, so the
 * window keeps its size near an edge. */
function centred(c, len, n, wrap) {
  if (len >= n) return { start: 0, len: n };
  const start = c - Math.floor(len / 2);
  if (wrap) return { start: mod(start, n), len };
  return { start: Math.min(Math.max(0, start), n - len), len };
}

/** The positions of `a` that `b` also covers, from the first to the last along `a` (null when none): so a window is
 * never wider than `a`, even where two spans round a circle overlap at both ends. */
function intersect(a, b, n, wrap) {
  let first = -1, last = -1;
  for (let i = 0; i < a.len; i++) {
    if (inSpan(b, wrap ? (a.start + i) % n : a.start + i, n, wrap)) {
      if (first < 0) first = i;
      last = i;
    }
  }
  if (first < 0) return null;
  return { start: wrap ? (a.start + first) % n : a.start + first, len: last - first + 1 };
}

/** The shortest span covering both. */
function union(a, b, n, wrap) {
  const marked = new Array(n).fill(false);
  for (const s of [a, b]) for (let i = 0; i < s.len; i++) marked[wrap ? (s.start + i) % n : s.start + i] = true;
  const [lo, hi] = covering(marked, wrap);
  return spanOf(lo, hi, n, wrap);
}

/** A box's two spans. */
function spans(box, map) {
  return [spanOf(box.x0, box.x1, map.width, map.wrapX), spanOf(box.y0, box.y1, map.height, map.wrapY)];
}

/** The box two spans make, with its size in tiles (`cols`, `rows`). */
function boxOf(sx, sy, map) {
  const [x0, x1] = endsOf(sx, map.width, map.wrapX), [y0, y1] = endsOf(sy, map.height, map.wrapY);
  return { x0, y0, x1, y1, cols: sx.len, rows: sy.len };
}

// ---- boxes ------------------------------------------------------------------------------------------------------

/** The whole map as a box. */
export function wholeMap(map) {
  return { x0: 0, y0: 0, x1: map.width - 1, y1: map.height - 1, cols: map.width, rows: map.height };
}

/** Whether `box` is a box of this map: whole numbers on it. A box read back from storage may be anything. */
export function validBox(box, map) {
  if (!box || typeof box !== "object") return false;
  const on = (v, n) => Number.isInteger(v) && v >= 0 && v < n;
  return on(box.x0, map.width) && on(box.x1, map.width) && on(box.y0, map.height) && on(box.y1, map.height)
    && (map.wrapX || box.x0 <= box.x1) && (map.wrapY || box.y0 <= box.y1);
}

/** Whether two boxes are the same. */
export function sameBox(a, b) {
  return !!a && !!b && a.x0 === b.x0 && a.y0 === b.y0 && a.x1 === b.x1 && a.y1 === b.y1;
}

/**
 * The box of the explored tiles: the view's tiles (its rows, whose first item is the tile's index, or the indexes),
 * covered the short way round on an axis that wraps. Null when there are none.
 */
export function exploredBox(tiles, width, height, wrapX, wrapY) {
  const cols = new Array(width).fill(false), rows = new Array(height).fill(false);
  for (const t of tiles || []) {
    const i = Array.isArray(t) ? t[0] : t;
    if (!Number.isInteger(i) || i < 0 || i >= width * height) continue;
    cols[i % width] = true;
    rows[Math.floor(i / width)] = true;
  }
  const xs = covering(cols, wrapX), ys = covering(rows, wrapY);
  if (!xs || !ys) return null;
  return { x0: xs[0], y0: ys[0], x1: xs[1], y1: ys[1] };
}

/**
 * The window a person's minimap shows (P3.7.3):
 *  1. the explored box (`explored`, exploredBox's), padded by PAD tiles, clamped on an axis that does not wrap;
 *  2. capped by the stage (the engine's `minimap.stage`): `early`, on an axis longer than EARLY's, to EARLY's size
 *     centred on the capital (or the explored box's middle; inside the map near an edge that does not wrap), within
 *     the explored box; `renaissance`, to the home box
 *     (`home`, the engine's: the explored tiles of the home continent joined to the capital through explored
 *     tiles) padded by PAD, within the explored box;
 *     `information`, the whole map;
 *  3. united with `prev`, the window shown before (the smallest box covering both), so that it never shrinks.
 * Returns the box with its size in tiles (`cols`, `rows`). With nothing explored, `prev` or the whole map.
 */
export function computeWindow({ explored, home, stage, capital, width, height, wrapX, wrapY, prev }) {
  const map = { width, height, wrapX: !!wrapX, wrapY: !!wrapY };
  const before = validBox(prev, map) ? spans(prev, map) : null;
  if (stage === "information" || !validBox(explored, map)) {
    if (stage === "information" || !before) return wholeMap(map);
    return boxOf(before[0], before[1], map);
  }
  const axes = [[map.width, map.wrapX, "x"], [map.height, map.wrapY, "y"]];
  let win = spans(explored, map).map((s, k) => grow(s, PAD, axes[k][0], axes[k][1]));
  if (stage === "early") {
    const cap = [EARLY.cols, EARLY.rows];
    win = win.map((s, k) => {
      const [n, wrap] = axes[k];
      if (n <= cap[k]) return s;
      const at = Array.isArray(capital) && Number.isInteger(capital[k]) ? capital[k] : (s.start + Math.floor(s.len / 2)) % n;
      return intersect(centred(at, cap[k], n, wrap), s, n, wrap) || s;
    });
  } else if (stage === "renaissance" && validBox(home, map)) {
    win = spans(home, map).map((h, k) => {
      const [n, wrap] = axes[k];
      return intersect(grow(h, PAD, n, wrap), win[k], n, wrap) || win[k];
    });
  }
  if (before) win = win.map((s, k) => union(s, before[k], axes[k][0], axes[k][1]));
  return boxOf(win[0], win[1], map);
}

// ---- drawing and pointing ------------------------------------------------------------------------------------------

/**
 * How a window fits a widget of `widgetW` by `widgetH` pixels: one scale for both axes (`scale` pixels to a hex's
 * radius, a hex √3 radii wide and its rows 1.5 apart, an odd row half a hex to the right), and the offsets that
 * centre it (`ox`, `oy`), the letterbox. The window is √3 (cols + ½) radii wide and 1.5 rows + ½ high.
 */
export function fit(win, widgetW, widgetH) {
  const w = SQRT3 * (win.cols + 0.5), h = 1.5 * win.rows + 0.5;
  const scale = Math.min(widgetW / w, widgetH / h);
  return { scale, ox: (widgetW - w * scale) / 2, oy: (widgetH - h * scale) / 2, cols: win.cols, rows: win.rows };
}

/** The offset of position `p` from the window's first along an axis (round the seam on one that wraps). */
function offset(p, start, n, wrap) {
  return wrap ? mod(p - start, n) : p - start;
}

/**
 * Where a point of the map is on the widget: (u, v) in tiles, u a column with an odd row's half included (a tile's
 * centre is u = x + ½ (y odd), v = y), as the main map's camera reads them. On a wrapping axis the point is placed
 * the short way round from the window's first column or row, so a point left of it lands at the far right: the
 * caller draws a box again one map over where it may straddle the seam.
 */
export function toScreen(f, win, map, u, v) {
  const du = map.wrapX ? mod(u - win.x0, map.width) : u - win.x0;
  const dv = map.wrapY ? mod(v - win.y0, map.height) : v - win.y0;
  return [f.ox + f.scale * SQRT3 * (du + 0.5), f.oy + f.scale * (1.5 * dv + 1)];
}

/** Where tile (x, y)'s centre is on the widget. */
export function tileCentre(f, win, map, x, y) {
  const dx = offset(x, win.x0, map.width, map.wrapX), dy = offset(y, win.y0, map.height, map.wrapY);
  return [f.ox + f.scale * SQRT3 * (dx + 0.5 * (y & 1) + 0.5), f.oy + f.scale * (1.5 * dy + 1)];
}

/** Whether tile (x, y) is inside the window. */
export function inWindow(win, map, x, y) {
  const dx = offset(x, win.x0, map.width, map.wrapX), dy = offset(y, win.y0, map.height, map.wrapY);
  return dx >= 0 && dx < win.cols && dy >= 0 && dy < win.rows;
}

/** The tile under pixel (px, py) of the widget, [x, y]: the nearest of the window's, so a click in the letterbox
 * finds the edge's. */
export function toTile(f, win, map, px, py) {
  const dy = Math.min(win.rows - 1, Math.max(0, Math.round(((py - f.oy) / f.scale - 1) / 1.5)));
  const y = map.wrapY ? mod(win.y0 + dy, map.height) : win.y0 + dy;
  const du = (px - f.ox) / (f.scale * SQRT3) - 0.5 - 0.5 * (y & 1);
  const dx = Math.min(win.cols - 1, Math.max(0, Math.round(du)));
  return [map.wrapX ? mod(win.x0 + dx, map.width) : win.x0 + dx, y];
}

// ---- keeping the window --------------------------------------------------------------------------------------------

/** The localStorage key of a game's and seat's window. */
export function windowKey(gid, pid) {
  return `citar:minimap:${gid}:${pid}`;
}

/** The window kept for `key`, if it is one of this map; null when there is none, the storage is not there or
 * refuses (a private window, a sandbox, blocked site data), or what it holds is not a box of this map. */
export function loadWindow(storage, key, map) {
  try {
    const raw = storage ? storage.getItem(key) : null;
    if (!raw) return null;
    const box = JSON.parse(raw);
    if (box.width !== map.width || box.height !== map.height || !validBox(box, map)) return null;
    return { x0: box.x0, y0: box.y0, x1: box.x1, y1: box.y1 };
  } catch {
    return null;
  }
}

/** Keeps the window for `key`; a storage that is not there or refuses is passed over. */
export function saveWindow(storage, key, win, map) {
  try {
    if (storage) storage.setItem(key, JSON.stringify({ x0: win.x0, y0: win.y0, x1: win.x1, y1: win.y1,
                                                     width: map.width, height: map.height }));
  } catch {
    // the window is kept for this page's life all the same
  }
}

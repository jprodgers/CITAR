// Pointy-top hex geometry for "odd-r" offset coordinates (odd rows shifted right).

export const SQRT3 = Math.sqrt(3);
const DIRS_EVEN = [[1, 0], [0, -1], [-1, -1], [-1, 0], [-1, 1], [0, 1]];
const DIRS_ODD = [[1, 0], [1, -1], [0, -1], [-1, 0], [0, 1], [1, 1]];
// corner pairs for the edge facing each direction (E, NE, NW, W, SW, SE)
export const EDGE_CORNERS = [[0, 1], [5, 0], [4, 5], [3, 4], [2, 3], [1, 2]];

export function hexCenter(x, y, size) {
  return [size * SQRT3 * (x + 0.5 * (y & 1)), size * 1.5 * y];
}

export function hexCorners(cx, cy, size) {
  const pts = [];
  for (let i = 0; i < 6; i++) {
    const a = (Math.PI / 180) * (60 * i - 30);
    pts.push([cx + size * Math.cos(a), cy + size * Math.sin(a)]);
  }
  return pts;
}

export function neighbor(x, y, dir, w, h) {
  const d = (y & 1 ? DIRS_ODD : DIRS_EVEN)[dir];
  const nx = x + d[0], ny = y + d[1];
  if (nx < 0 || ny < 0 || nx >= w || ny >= h) return null;
  return [nx, ny];
}

export function neighbors(x, y, w, h) {
  const out = [];
  for (let d = 0; d < 6; d++) {
    const n = neighbor(x, y, d, w, h);
    if (n) out.push(n);
  }
  return out;
}

export function pixelToHex(px, py, size) {
  const q = (SQRT3 / 3 * px - py / 3) / size;
  const r = (2 / 3 * py) / size;
  let cx = q, cz = r, cy = -cx - cz;
  let rx = Math.round(cx), ry = Math.round(cy), rz = Math.round(cz);
  const dx = Math.abs(rx - cx), dy = Math.abs(ry - cy), dz = Math.abs(rz - cz);
  if (dx > dy && dx > dz) rx = -ry - rz;
  else if (dy > dz) ry = -rx - rz;
  else rz = -rx - ry;
  const col = rx + (rz - (rz & 1)) / 2;
  return [col, rz];
}

export function offsetToCube(x, y) {
  const q = x - (y - (y & 1)) / 2;
  return [q, y, -q - y];
}

export function hexDistance(x1, y1, x2, y2) {
  const a = offsetToCube(x1, y1), b = offsetToCube(x2, y2);
  return (Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2])) / 2;
}

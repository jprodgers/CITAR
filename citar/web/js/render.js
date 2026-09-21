// Canvas renderer for the hex map: terrain, features, rivers, routes, borders, fog, cities, units, overlays.
import { hexCenter, hexCorners, pixelToHex, neighbor, EDGE_CORNERS, SQRT3 } from "./hex.js";

export const TERRAIN_COLORS = {
  Grassland: "#5b8f3d", Plains: "#a19f4c", Desert: "#d9c47f", Tundra: "#8c9b88", Snow: "#e6edf0",
  Mountain: "#7a6e63", Coast: "#3e7fb6", Ocean: "#244f7d", Lakes: "#4a8dca",
};
const RESOURCE_KIND_COLORS = { Bonus: "#72d45f", Strategic: "#ff8a3d", Luxury: "#d377ff" };

export class MapRenderer {
  constructor(canvas, rules) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.rules = rules;
    this.model = null;
    this.overlay = {};
    this.cam = { x: 0, y: 0, zoom: 1 };
    this.baseSize = 30;
    this._dirty = true;
    this._raf = null;
    this.showYields = false;
    this.showGrid = false;
    const ro = new ResizeObserver(() => this.resize());
    ro.observe(canvas);
    this.resize();
  }

  get size() { return this.baseSize * this.cam.zoom; }

  resize() {
    const dpr = window.devicePixelRatio || 1;
    const r = this.canvas.getBoundingClientRect();
    this.canvas.width = Math.max(1, Math.floor(r.width * dpr));
    this.canvas.height = Math.max(1, Math.floor(r.height * dpr));
    this.dpr = dpr;
    this.invalidate();
  }

  setModel(model) {
    this.model = model;
    this._unitIndex = new Map();
    for (const u of model.units || []) {
      const k = u.y * model.width + u.x;
      if (!this._unitIndex.has(k)) this._unitIndex.set(k, []);
      this._unitIndex.get(k).push(u);
    }
    this._cityIndex = new Map();
    for (const c of model.cities || []) this._cityIndex.set(c.y * model.width + c.x, c);
    this.invalidate();
  }

  setOverlay(ov) { this.overlay = ov || {}; this.invalidate(); }

  invalidate() {
    this._dirty = true;
    if (!this._raf) this._raf = requestAnimationFrame(() => {
      this._raf = null;
      if (this._dirty) this.draw();
      // let the owner redraw things tied to the camera (the minimap viewport box)
      const key = `${Math.round(this.cam.x)},${Math.round(this.cam.y)},${this.cam.zoom}`;
      if (key !== this._camKey) { this._camKey = key; if (this.onViewChange) this.onViewChange(); }
    });
  }

  centerOn(x, y) {
    const [cx, cy] = hexCenter(x, y, this.size);
    const r = this.canvas.getBoundingClientRect();
    this.cam.x = cx - r.width / 2;
    this.cam.y = cy - r.height / 2;
    this.invalidate();
  }

  zoomAt(factor, sx, sy) {
    const before = this.screenToWorld(sx, sy);
    const oldSize = this.size;
    this.cam.zoom = Math.min(3, Math.max(0.25, this.cam.zoom * factor));
    const scale = this.size / oldSize;
    this.cam.x = before[0] * scale - sx;
    this.cam.y = before[1] * scale - sy;
    this.invalidate();
  }

  pan(dx, dy) { this.cam.x -= dx; this.cam.y -= dy; this.invalidate(); }

  screenToWorld(sx, sy) { return [sx + this.cam.x, sy + this.cam.y]; }

  // visible and not hidden behind the overlay panels (unit panel bottom-left, event/city panel top-right)
  isOnScreen(x, y, margin = 60) {
    const [cx, cy] = hexCenter(x, y, this.size);
    const r = this.canvas.getBoundingClientRect();
    const sx = cx - this.cam.x, sy = cy - this.cam.y;
    if (!(sx > margin && sy > margin && sx < r.width - margin && sy < r.height - margin)) return false;
    if (sx > r.width - 340 && sy < Math.min(r.height * 0.5, 440)) return false;
    if (sx < 350 && sy > r.height - 230) return false;
    return true;
  }

  // id of the city whose name banner is under a screen point (banners are drawn on the canvas)
  cityBannerAt(sx, sy) {
    const [wx, wy] = this.screenToWorld(sx, sy);
    for (const b of this._banners || []) {
      if (wx >= b.x0 && wx <= b.x1 && wy >= b.y0 && wy <= b.y1) return b.id;
    }
    return null;
  }

  screenToTile(sx, sy) {
    if (!this.model) return null;
    const [wx, wy] = this.screenToWorld(sx, sy);
    const [x, y] = pixelToHex(wx, wy, this.size);
    if (x < 0 || y < 0 || x >= this.model.width || y >= this.model.height) return null;
    return { x, y };
  }

  playerColor(pid) {
    const p = this.model.players && this.model.players[pid];
    return (p && p.color) || "#999";
  }

  // ------------------------------------------------------------------
  draw() {
    this._dirty = false;
    const ctx = this.ctx;
    const m = this.model;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    const W = this.canvas.width / this.dpr, H = this.canvas.height / this.dpr;
    ctx.fillStyle = "#0b0d12";
    ctx.fillRect(0, 0, W, H);
    if (!m) return;
    const size = this.size;
    ctx.translate(-this.cam.x, -this.cam.y);

    const hexW = size * SQRT3, rowH = size * 1.5;
    const y0 = Math.max(0, Math.floor(this.cam.y / rowH) - 1);
    const y1 = Math.min(m.height - 1, Math.ceil((this.cam.y + H) / rowH) + 1);
    const x0 = Math.max(0, Math.floor(this.cam.x / hexW) - 1);
    const x1 = Math.min(m.width - 1, Math.ceil((this.cam.x + W) / hexW) + 1);
    const range = [];
    for (let y = y0; y <= y1; y++) for (let x = x0; x <= x1; x++) range.push([x, y]);

    const tileAt = (x, y) => m.tiles[y * m.width + x];

    // terrain
    for (const [x, y] of range) {
      const t = tileAt(x, y);
      if (!t) continue;
      const [cx, cy] = hexCenter(x, y, size);
      this.hexPath(cx, cy, size + 0.6);
      ctx.fillStyle = TERRAIN_COLORS[t.terrain] || "#444";
      ctx.fill();
      if (this.showGrid && size > 14) { ctx.strokeStyle = "rgba(0,0,0,0.15)"; ctx.lineWidth = 1; ctx.stroke(); }
    }
    // owner tint
    for (const [x, y] of range) {
      const t = tileAt(x, y);
      if (!t || t.owner == null) continue;
      const [cx, cy] = hexCenter(x, y, size);
      this.hexPath(cx, cy, size);
      ctx.fillStyle = hexAlpha(this.playerColor(t.owner), 0.13);
      ctx.fill();
    }
    // relief & features
    for (const [x, y] of range) {
      const t = tileAt(x, y);
      if (!t) continue;
      const [cx, cy] = hexCenter(x, y, size);
      if (t.terrain === "Mountain") this.drawMountain(cx, cy, size);
      else if (t.hills) this.drawHills(cx, cy, size);
      for (const f of t.features) if (f !== "Hill") this.drawFeature(f, cx, cy, size);
      if (t.wonder) this.drawWonder(t.wonder, cx, cy, size);
    }
    // rivers run along hex edges (bit d of t.river = the edge facing direction d)
    ctx.lineCap = "round";
    ctx.strokeStyle = "#5fb2f0";
    ctx.lineWidth = Math.max(1.5, size * 0.11);
    for (const [x, y] of range) {
      const t = tileAt(x, y);
      if (!t || !t.river) continue;
      const [cx, cy] = hexCenter(x, y, size);
      const corners = hexCorners(cx, cy, size);
      for (let d = 0; d < 6; d++) {
        if (!(t.river & (1 << d))) continue;
        const [a, b] = EDGE_CORNERS[d];
        ctx.beginPath(); ctx.moveTo(...corners[a]); ctx.lineTo(...corners[b]); ctx.stroke();
      }
    }
    // routes
    for (const [x, y] of range) {
      const t = tileAt(x, y);
      const cityHere = this._cityIndex.get(y * m.width + x);
      if (!t || (!t.route && !cityHere)) continue;
      const [cx, cy] = hexCenter(x, y, size);
      for (let d = 0; d < 3; d++) {
        const n = neighbor(x, y, d, m.width, m.height);
        if (!n) continue;
        const nt = tileAt(n[0], n[1]);
        const nCity = this._cityIndex.get(n[1] * m.width + n[0]);
        if (!nt || (!nt.route && !nCity)) continue;
        if (cityHere && nCity) continue;
        const rail = (t.route === "Railroad" || cityHere) && (nt.route === "Railroad" || nCity) && (t.route === "Railroad" || nt.route === "Railroad");
        const [nx, ny] = hexCenter(n[0], n[1], size);
        const pill = t.routePillaged || nt.routePillaged;
        ctx.strokeStyle = pill ? "rgba(90,60,40,0.5)" : rail ? "#3a3a3a" : "#8a6a44";
        ctx.lineWidth = Math.max(1, size * (rail ? 0.1 : 0.08));
        ctx.setLineDash(pill ? [size * 0.1, size * 0.15] : []);
        ctx.beginPath(); ctx.moveTo(cx, cy); ctx.lineTo(nx, ny); ctx.stroke();
        ctx.setLineDash([]);
      }
    }
    // improvements, resources, camps, villages
    for (const [x, y] of range) {
      const t = tileAt(x, y);
      if (!t) continue;
      const [cx, cy] = hexCenter(x, y, size);
      if (t.improvement && !t.camp && !t.village && t.improvement !== "City center") this.drawImprovement(t.improvement, cx, cy, size, t.pillaged);
      if (t.resource) this.drawResource(t.resource, cx, cy, size);
      if (t.camp) this.drawCamp(cx, cy, size);
      if (t.village) this.drawVillage(cx, cy, size);
    }
    // borders
    for (const [x, y] of range) {
      const t = tileAt(x, y);
      if (!t || t.owner == null) continue;
      const [cx, cy] = hexCenter(x, y, size);
      const corners = hexCorners(cx, cy, size - Math.max(1, size * 0.05));
      ctx.strokeStyle = this.playerColor(t.owner);
      ctx.lineWidth = Math.max(1.5, size * 0.09);
      for (let d = 0; d < 6; d++) {
        const n = neighbor(x, y, d, m.width, m.height);
        const nt = n ? tileAt(n[0], n[1]) : null;
        if (nt && nt.owner === t.owner) continue;
        const [a, b] = EDGE_CORNERS[d];
        ctx.beginPath(); ctx.moveTo(...corners[a]); ctx.lineTo(...corners[b]); ctx.stroke();
      }
    }
    // fog
    for (const [x, y] of range) {
      const t = tileAt(x, y);
      const [cx, cy] = hexCenter(x, y, size);
      if (!t) {
        this.hexPath(cx, cy, size + 0.8);
        ctx.fillStyle = "#07080b";
        ctx.fill();
      } else if (!t.visible) {
        this.hexPath(cx, cy, size + 0.6);
        ctx.fillStyle = "rgba(8,10,16,0.45)";
        ctx.fill();
      }
    }
    // overlays
    const ov = this.overlay;
    if (ov.cityTiles) for (const [x, y, worked, locked] of ov.cityTiles) {
      const [cx, cy] = hexCenter(x, y, size);
      this.hexPath(cx, cy, size * 0.92);
      ctx.strokeStyle = worked ? "rgba(255,255,255,0.8)" : "rgba(255,255,255,0.25)";
      ctx.lineWidth = worked ? 2 : 1;
      ctx.stroke();
      if (worked) { ctx.fillStyle = "rgba(255,255,255,0.08)"; ctx.fill(); }
      if (locked) this.label("🔒", cx, cy - size * 0.66, "#fff", Math.max(10, size * 0.32), false);
    }
    if (ov.buyable) for (const b of ov.buyable) {
      const [cx, cy] = hexCenter(b.x, b.y, size);
      this.hexPath(cx, cy, size * 0.85);
      ctx.strokeStyle = "rgba(255,215,0,0.7)"; ctx.setLineDash([4, 3]); ctx.lineWidth = 1.5; ctx.stroke(); ctx.setLineDash([]);
      if (size > 16) this.label(`${b.gold}g`, cx, cy + size * 0.55, "#ffd700", Math.max(9, size * 0.3));
    }
    if (ov.reachable) for (const [x, y] of ov.reachable) {
      const [cx, cy] = hexCenter(x, y, size);
      this.hexPath(cx, cy, size * 0.9);
      ctx.fillStyle = "rgba(120,200,255,0.18)"; ctx.fill();
    }
    if (ov.path && ov.path.length) {
      ctx.strokeStyle = "rgba(255,255,255,0.7)"; ctx.lineWidth = 2; ctx.setLineDash([6, 4]);
      ctx.beginPath();
      ov.path.forEach(([x, y], i) => { const [cx, cy] = hexCenter(x, y, size); i ? ctx.lineTo(cx, cy) : ctx.moveTo(cx, cy); });
      ctx.stroke(); ctx.setLineDash([]);
    }
    if (ov.sites) ov.sites.forEach((st, i) => {
      const [cx, cy] = hexCenter(st.x, st.y, size);
      this.hexPath(cx, cy, size * 0.8);
      ctx.strokeStyle = "rgba(120,255,160,0.95)"; ctx.setLineDash([5, 3]); ctx.lineWidth = 2.5; ctx.stroke(); ctx.setLineDash([]);
      ctx.fillStyle = "rgba(120,255,160,0.12)"; ctx.fill();
      this.label(st.resource ? "fishing boats" : i === 0 ? "★ best site" : "site", cx, cy - size * 0.55, "#aaffc4", Math.max(9, size * 0.3));
    });
    if (ov.preview && ov.preview.length > 1) {
      ctx.strokeStyle = "rgba(255,230,120,0.95)"; ctx.lineWidth = Math.max(2, size * 0.1); ctx.lineJoin = "round";
      ctx.beginPath();
      ov.preview.forEach(([x, y], i) => { const [cx, cy] = hexCenter(x, y, size); i ? ctx.lineTo(cx, cy) : ctx.moveTo(cx, cy); });
      ctx.stroke();
      const [ex, ey] = ov.preview[ov.preview.length - 1];
      const [cx, cy] = hexCenter(ex, ey, size);
      ctx.beginPath(); ctx.arc(cx, cy, Math.max(9, size * 0.34), 0, Math.PI * 2);
      ctx.fillStyle = "rgba(20,20,20,0.85)"; ctx.fill(); ctx.strokeStyle = "rgba(255,230,120,0.95)"; ctx.lineWidth = 2; ctx.stroke();
      this.label(ov.previewTurns > 1 ? `${ov.previewTurns}t` : "✓", cx, cy + 0.5, "#ffe680", Math.max(10, size * 0.36));
    }
    if (ov.noPath) {
      const [cx, cy] = hexCenter(ov.noPath[0], ov.noPath[1], size);
      this.label("✕", cx, cy, "#ff6b6b", Math.max(12, size * 0.5));
    }
    if (ov.targets) for (const tg of ov.targets) {
      const [cx, cy] = hexCenter(tg.x, tg.y, size);
      this.hexPath(cx, cy, size * 0.9);
      ctx.strokeStyle = "#ff4d4d"; ctx.lineWidth = 2.5; ctx.stroke();
      ctx.fillStyle = "rgba(255,60,60,0.15)"; ctx.fill();
    }
    if (ov.markers) for (const mk of ov.markers) {
      const [cx, cy] = hexCenter(mk.x, mk.y, size);
      const r = Math.max(7, size * (mk.small ? 0.3 : 0.42));
      ctx.beginPath(); ctx.arc(cx, cy, r, 0, Math.PI * 2);
      ctx.fillStyle = mk.color || "#fff"; ctx.fill();
      ctx.strokeStyle = "rgba(0,0,0,0.8)"; ctx.lineWidth = 2; ctx.stroke();
      if (size > 9) this.label(mk.label, cx, cy + 0.5, textColorFor(mk.color || "#fff"), Math.max(8, r * 0.95));
    }
    if (ov.brush) for (const [x, y] of ov.brush) {
      const [cx, cy] = hexCenter(x, y, size);
      this.hexPath(cx, cy, size * 0.96);
      ctx.strokeStyle = "rgba(255,255,255,0.85)"; ctx.lineWidth = 1.5; ctx.setLineDash([3, 3]); ctx.stroke(); ctx.setLineDash([]);
    }
    if (ov.edge) {
      const [cx, cy] = hexCenter(ov.edge.x, ov.edge.y, size);
      const corners = hexCorners(cx, cy, size);
      const [a, b] = EDGE_CORNERS[ov.edge.d];
      ctx.strokeStyle = "rgba(255,255,255,0.9)"; ctx.lineWidth = Math.max(3, size * 0.15); ctx.lineCap = "round";
      ctx.beginPath(); ctx.moveTo(...corners[a]); ctx.lineTo(...corners[b]); ctx.stroke();
    }
    if (ov.yields) for (const yy of ov.yields) {
      const [cx, cy] = hexCenter(yy.x, yy.y, size);
      this.drawYields(yy.yields, cx, cy, size);
    }
    // cities, then their banners (units are drawn on top so a banner never hides one)
    const visibleCities = [];
    for (const [x, y] of range) {
      const c = this._cityIndex.get(y * m.width + x);
      if (c) { this.drawCity(c, size); visibleCities.push(c); }
    }
    this._banners = [];
    for (const c of visibleCities) this.drawCityBanner(c, size);
    // units
    for (const [x, y] of range) {
      const us = this._unitIndex.get(y * m.width + x);
      if (!us) continue;
      const [cx, cy] = hexCenter(x, y, size);
      const city = this._cityIndex.get(y * m.width + x);
      const military = us.filter((u) => u.class !== "civilian" && u.domain !== "air");
      const civilians = us.filter((u) => u.class === "civilian");
      const air = us.filter((u) => u.domain === "air");
      let offX = city ? size * 0.42 : 0, offY = city ? size * 0.28 : 0;
      military.forEach((u, i) => this.drawUnit(u, cx + offX + i * size * 0.3, cy + offY, size * (city ? 0.78 : 1)));
      civilians.forEach((u) => this.drawUnit(u, cx - size * 0.38 + (city ? -size * 0.05 : 0), cy + size * 0.3, size * 0.72));
      if (air.length) this.drawAirStack(air, cx - size * 0.45, cy - size * 0.3, size);
    }
    // selection & hover
    if (ov.selected) {
      const [cx, cy] = hexCenter(ov.selected[0], ov.selected[1], size);
      this.hexPath(cx, cy, size * 0.95);
      ctx.strokeStyle = "#ffe066"; ctx.lineWidth = 3; ctx.stroke();
    }
    if (ov.hover) {
      const [cx, cy] = hexCenter(ov.hover[0], ov.hover[1], size);
      this.hexPath(cx, cy, size * 0.97);
      ctx.strokeStyle = "rgba(255,255,255,0.6)"; ctx.lineWidth = 1.5; ctx.stroke();
    }
    if (ov.flash) {
      const [cx, cy] = hexCenter(ov.flash[0], ov.flash[1], size);
      ctx.beginPath(); ctx.arc(cx, cy, size * 1.2, 0, Math.PI * 2);
      ctx.strokeStyle = "rgba(255,230,120,0.9)"; ctx.lineWidth = 3; ctx.stroke();
    }
  }

  // ------------------------------------------------------------------
  hexPath(cx, cy, size) {
    const ctx = this.ctx;
    const pts = hexCorners(cx, cy, size);
    ctx.beginPath();
    ctx.moveTo(pts[0][0], pts[0][1]);
    for (let i = 1; i < 6; i++) ctx.lineTo(pts[i][0], pts[i][1]);
    ctx.closePath();
  }

  label(text, x, y, color, px, bold = true) {
    const ctx = this.ctx;
    ctx.font = `${bold ? "600 " : ""}${px}px system-ui, sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.lineWidth = 3;
    ctx.strokeStyle = "rgba(0,0,0,0.75)";
    ctx.strokeText(text, x, y);
    ctx.fillStyle = color;
    ctx.fillText(text, x, y);
  }

  drawHills(cx, cy, s) {
    const ctx = this.ctx;
    ctx.strokeStyle = "rgba(60,40,20,0.55)";
    ctx.fillStyle = "rgba(90,70,40,0.18)";
    ctx.lineWidth = Math.max(1, s * 0.06);
    for (const [dx, dy, r] of [[-0.3, 0.15, 0.32], [0.25, 0.05, 0.38]]) {
      ctx.beginPath();
      ctx.arc(cx + dx * s, cy + dy * s + r * s * 0.3, r * s, Math.PI * 1.1, Math.PI * 1.9);
      ctx.fill(); ctx.stroke();
    }
  }

  drawMountain(cx, cy, s) {
    const ctx = this.ctx;
    const peaks = [[-0.28, 0.35, 0.55], [0.22, 0.4, 0.7]];
    for (const [dx, w, h] of peaks) {
      const bx = cx + dx * s, by = cy + s * 0.45;
      ctx.beginPath();
      ctx.moveTo(bx - w * s, by); ctx.lineTo(bx, by - h * s * 1.2); ctx.lineTo(bx + w * s, by); ctx.closePath();
      ctx.fillStyle = "#5c524a"; ctx.fill();
      ctx.strokeStyle = "#3a332e"; ctx.lineWidth = 1; ctx.stroke();
      ctx.beginPath();
      ctx.moveTo(bx - w * s * 0.3, by - h * s * 0.85); ctx.lineTo(bx, by - h * s * 1.2); ctx.lineTo(bx + w * s * 0.3, by - h * s * 0.85);
      ctx.closePath(); ctx.fillStyle = "#eef2f4"; ctx.fill();
    }
  }

  drawFeature(f, cx, cy, s) {
    const ctx = this.ctx;
    if (f === "Forest" || f === "Jungle") {
      const spots = [[-0.35, -0.2], [0.05, -0.35], [0.38, -0.12], [-0.15, 0.2], [0.28, 0.3], [-0.45, 0.35]];
      for (const [dx, dy] of spots) {
        const x = cx + dx * s, y = cy + dy * s;
        if (f === "Forest") {
          ctx.beginPath(); ctx.moveTo(x, y - s * 0.22); ctx.lineTo(x - s * 0.13, y + s * 0.08); ctx.lineTo(x + s * 0.13, y + s * 0.08); ctx.closePath();
          ctx.fillStyle = "#2e5e2a"; ctx.fill();
        } else {
          ctx.beginPath(); ctx.arc(x, y, s * 0.13, 0, Math.PI * 2);
          ctx.fillStyle = "#1f6b35"; ctx.fill();
          ctx.beginPath(); ctx.arc(x - s * 0.04, y - s * 0.04, s * 0.05, 0, Math.PI * 2);
          ctx.fillStyle = "#3fa05a"; ctx.fill();
        }
      }
    } else if (f === "Marsh") {
      ctx.strokeStyle = "rgba(40,110,120,0.8)"; ctx.lineWidth = Math.max(1, s * 0.05);
      for (const [dx, dy] of [[-0.35, -0.15], [0.1, -0.25], [-0.1, 0.2], [0.35, 0.15]]) {
        const x = cx + dx * s, y = cy + dy * s;
        ctx.beginPath(); ctx.moveTo(x - s * 0.15, y); ctx.lineTo(x + s * 0.15, y); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(x, y); ctx.lineTo(x - s * 0.04, y - s * 0.15); ctx.stroke();
      }
    } else if (f === "Flood plains") {
      ctx.strokeStyle = "rgba(90,150,60,0.7)"; ctx.lineWidth = Math.max(1, s * 0.06);
      for (const dy of [-0.25, 0, 0.25]) {
        ctx.beginPath(); ctx.moveTo(cx - s * 0.45, cy + dy * s); ctx.lineTo(cx + s * 0.45, cy + dy * s); ctx.stroke();
      }
    } else if (f === "Oasis") {
      ctx.beginPath(); ctx.ellipse(cx, cy + s * 0.1, s * 0.3, s * 0.18, 0, 0, Math.PI * 2);
      ctx.fillStyle = "#3d9be0"; ctx.fill();
      ctx.beginPath(); ctx.arc(cx + s * 0.25, cy - s * 0.15, s * 0.12, 0, Math.PI * 2);
      ctx.fillStyle = "#2f7d3a"; ctx.fill();
    } else if (f === "Ice") {
      this.hexPath(cx, cy, s * 0.85);
      ctx.fillStyle = "rgba(235,245,255,0.85)"; ctx.fill();
    } else if (f === "Atoll") {
      ctx.beginPath(); ctx.ellipse(cx, cy, s * 0.35, s * 0.2, 0.3, 0, Math.PI * 2);
      ctx.strokeStyle = "#e9dca0"; ctx.lineWidth = Math.max(1.5, s * 0.08); ctx.stroke();
    } else if (f === "Fallout") {
      this.label("☢", cx, cy, "#d6ff3a", Math.max(10, s * 0.5));
    }
  }

  drawWonder(name, cx, cy, s) {
    const ctx = this.ctx;
    ctx.beginPath(); ctx.arc(cx, cy, s * 0.42, 0, Math.PI * 2);
    ctx.strokeStyle = "#ffe27a"; ctx.lineWidth = Math.max(1.5, s * 0.07); ctx.stroke();
    this.label("✦", cx, cy - s * 0.02, "#ffe27a", Math.max(10, s * 0.45));
    if (s > 26) this.label(name, cx, cy + s * 0.62, "#ffe27a", Math.max(9, s * 0.24), false);
  }

  drawResource(res, cx, cy, s) {
    if (s < 12) return;
    const rd = this.rules.resources[res];
    if (!rd) return;
    const ctx = this.ctx;
    const x = cx + s * 0.45, y = cy - s * 0.4, r = Math.max(5, s * 0.22);
    ctx.beginPath(); ctx.arc(x, y, r, 0, Math.PI * 2);
    ctx.fillStyle = "rgba(15,15,20,0.8)"; ctx.fill();
    const col = RESOURCE_KIND_COLORS[rd.resourceType] || "#ccc";
    ctx.strokeStyle = col; ctx.lineWidth = 2; ctx.stroke();
    ctx.fillStyle = col;
    ctx.font = `700 ${Math.max(7, r * 0.95)}px system-ui, sans-serif`;
    ctx.textAlign = "center"; ctx.textBaseline = "middle";
    ctx.fillText(rd.name.slice(0, 2), x, y + 0.5);
  }

  drawImprovement(imp, cx, cy, s, pillaged) {
    const ctx = this.ctx;
    const y = cy + s * 0.42;
    ctx.save();
    ctx.globalAlpha = pillaged ? 0.45 : 1;
    if (imp === "Farm" || imp === "Terrace farm") {
      ctx.strokeStyle = "#e8d06a"; ctx.lineWidth = Math.max(1, s * 0.05);
      for (let i = -2; i <= 2; i++) {
        ctx.beginPath(); ctx.moveTo(cx + i * s * 0.12 - s * 0.08, y - s * 0.28); ctx.lineTo(cx + i * s * 0.12 + s * 0.08, y + s * 0.02); ctx.stroke();
      }
    } else if (imp === "Fort" || imp === "Citadel") {
      ctx.strokeStyle = "#ddd"; ctx.lineWidth = 2;
      ctx.strokeRect(cx - s * 0.3, cy - s * 0.1, s * 0.6, s * 0.4);
    } else {
      const glyph = { "Mine": "⛏", "Pasture": "∩", "Camp": "⛺", "Plantation": "✿", "Quarry": "▦", "Fishing Boats": "⚓",
                      "Oil well": "⛽", "Offshore Platform": "⛽", "Lumber mill": "≡", "Trading post": "$", "Academy": "⚗",
                      "Manufactory": "⚙", "Customs house": "$", "Holy site": "✝", "Landmark": "♜", "Moai": "☗",
                      "Kasbah": "♖", "Polder": "≈", "Feitoria": "⚓", "Chateau": "♔" }[imp] || "+";
      ctx.beginPath(); ctx.arc(cx - s * 0.02, y - s * 0.08, Math.max(5, s * 0.18), 0, Math.PI * 2);
      ctx.fillStyle = "rgba(60,40,20,0.8)"; ctx.fill();
      ctx.fillStyle = "#f3e2b8"; ctx.font = `${Math.max(8, s * 0.26)}px system-ui, sans-serif`;
      ctx.textAlign = "center"; ctx.textBaseline = "middle";
      ctx.fillText(glyph, cx - s * 0.02, y - s * 0.07);
    }
    if (pillaged) {
      ctx.globalAlpha = 1;
      ctx.strokeStyle = "#ff3b3b"; ctx.lineWidth = 2;
      ctx.beginPath(); ctx.moveTo(cx - s * 0.2, y - s * 0.25); ctx.lineTo(cx + s * 0.2, y + s * 0.05);
      ctx.moveTo(cx + s * 0.2, y - s * 0.25); ctx.lineTo(cx - s * 0.2, y + s * 0.05); ctx.stroke();
    }
    ctx.restore();
  }

  drawCamp(cx, cy, s) {
    const ctx = this.ctx;
    ctx.beginPath(); ctx.moveTo(cx, cy - s * 0.4); ctx.lineTo(cx - s * 0.35, cy + s * 0.25); ctx.lineTo(cx + s * 0.35, cy + s * 0.25); ctx.closePath();
    ctx.fillStyle = "#6b2a1a"; ctx.fill(); ctx.strokeStyle = "#1a0a05"; ctx.lineWidth = 2; ctx.stroke();
    ctx.beginPath(); ctx.arc(cx, cy + s * 0.05, s * 0.09, 0, Math.PI * 2); ctx.fillStyle = "#ff9b2f"; ctx.fill();
  }

  drawVillage(cx, cy, s) {
    const ctx = this.ctx;
    for (const dx of [-0.2, 0.2]) {
      const x = cx + dx * s, y = cy;
      ctx.fillStyle = "#c7a46a"; ctx.fillRect(x - s * 0.13, y - s * 0.05, s * 0.26, s * 0.2);
      ctx.beginPath(); ctx.moveTo(x - s * 0.17, y - s * 0.05); ctx.lineTo(x, y - s * 0.22); ctx.lineTo(x + s * 0.17, y - s * 0.05); ctx.closePath();
      ctx.fillStyle = "#8b5a2b"; ctx.fill();
    }
    this.label("!", cx, cy - s * 0.42, "#ffe27a", Math.max(9, s * 0.35));
  }

  drawYields(y, cx, cy, s) {
    if (s < 12 || !y) return;
    const ctx = this.ctx;
    const items = [["food", "#6fd36f"], ["production", "#f0a040"], ["gold", "#ffd84a"], ["science", "#6fb7ff"]]
      .map(([k, col]) => [Math.round(y[k] || 0), col]).filter(([v]) => v);
    if (!items.length) return;
    // readable badges: two per row, centred on the tile
    const r = Math.max(6.5, s * 0.19);
    const perRow = 2, rows = Math.ceil(items.length / perRow);
    items.forEach(([v, col], i) => {
      const row = Math.floor(i / perRow), inRow = Math.min(perRow, items.length - row * perRow);
      const col_ = i - row * perRow;
      const x = cx + (col_ - (inRow - 1) / 2) * r * 2.15;
      const yy = cy + (row - (rows - 1) / 2) * r * 2.15;
      ctx.beginPath(); ctx.arc(x, yy, r, 0, Math.PI * 2);
      ctx.fillStyle = col; ctx.fill();
      ctx.lineWidth = 1; ctx.strokeStyle = "rgba(0,0,0,0.6)"; ctx.stroke();
      ctx.fillStyle = "#111"; ctx.font = `700 ${Math.round(r * 1.25)}px system-ui`; ctx.textAlign = "center"; ctx.textBaseline = "middle";
      ctx.fillText(String(v), x, yy + 0.5);
    });
  }

  drawCity(c, s) {
    const ctx = this.ctx;
    const [cx, cy] = hexCenter(c.x, c.y, s);
    const color = c.owner != null ? this.playerColor(c.owner) : "#999";
    // walls/base
    const r = s * 0.42;
    ctx.beginPath();
    ctx.roundRect(cx - r, cy - r * 0.8, r * 2, r * 1.6, s * 0.12);
    ctx.fillStyle = shade(color, -0.25); ctx.fill();
    ctx.strokeStyle = "#f5f5f5"; ctx.lineWidth = Math.max(1.5, s * 0.07); ctx.stroke();
    // little buildings
    ctx.fillStyle = shade(color, 0.35);
    ctx.fillRect(cx - r * 0.6, cy - r * 0.2, r * 0.45, r * 0.7);
    ctx.fillRect(cx + r * 0.1, cy - r * 0.5, r * 0.5, r * 1.0);
    if (c.capital) this.label("★", cx, cy - r * 1.05, "#ffd84a", Math.max(10, s * 0.4));
  }

  drawCityBanner(c, s) {
    const ctx = this.ctx;
    const [cx, cy] = hexCenter(c.x, c.y, s);
    const color = c.owner != null ? this.playerColor(c.owner) : "#999";
    // banner: along the bottom edge of the city hex, where it overlaps neighbouring units the least
    if (s >= 10) {
      const fs = Math.max(10, Math.min(14, s * 0.4));
      ctx.font = `600 ${fs}px system-ui, sans-serif`;
      const name = c.name || "?";
      const tw = ctx.measureText(name).width;
      const bw = tw + fs * 2.2, bh = fs * 1.45;
      const bx = cx - bw / 2, by = cy + s * 0.6;
      this._banners.push({ id: c.id, x0: bx, y0: by, x1: bx + bw, y1: by + bh });
      ctx.beginPath(); ctx.roundRect(bx, by, bw, bh, bh / 2);
      ctx.fillStyle = c.stale ? "rgba(30,30,30,0.75)" : hexAlpha(shade(color, -0.45), 0.92); ctx.fill();
      ctx.strokeStyle = color; ctx.lineWidth = 1.5; ctx.stroke();
      ctx.beginPath(); ctx.arc(bx + bh / 2, by + bh / 2, bh * 0.42, 0, Math.PI * 2);
      ctx.fillStyle = color; ctx.fill();
      ctx.fillStyle = "#fff"; ctx.textAlign = "center"; ctx.textBaseline = "middle";
      ctx.font = `700 ${fs * 0.85}px system-ui, sans-serif`;
      ctx.fillText(c.pop != null ? String(c.pop) : "?", bx + bh / 2, by + bh / 2 + 0.5);
      ctx.font = `600 ${fs}px system-ui, sans-serif`;
      ctx.fillStyle = c.stale ? "#bbb" : "#fff";
      ctx.fillText(name, bx + bh + tw / 2 + fs * 0.25, by + bh / 2 + 0.5);
      if (c.hp != null && c.max_hp && c.hp < c.max_hp) {
        const w = bw * 0.8, frac = Math.max(0, c.hp / c.max_hp);
        ctx.fillStyle = "#300"; ctx.fillRect(cx - w / 2, by + bh + 2, w, 4);
        ctx.fillStyle = frac > 0.5 ? "#4cd964" : frac > 0.25 ? "#ffcc00" : "#ff3b30";
        ctx.fillRect(cx - w / 2, by + bh + 2, w * frac, 4);
      }
      if (c.razing) this.label("🔥", cx + bw / 2 + fs * 0.4, by + bh / 2, "#f80", fs);
    }
  }

  drawAirStack(air, x, y, s) {
    const ctx = this.ctx;
    const color = this.playerColor(air[0].owner);
    ctx.beginPath();
    ctx.moveTo(x, y - s * 0.2); ctx.lineTo(x + s * 0.18, y + s * 0.15); ctx.lineTo(x, y + s * 0.07); ctx.lineTo(x - s * 0.18, y + s * 0.15); ctx.closePath();
    ctx.fillStyle = color; ctx.fill(); ctx.strokeStyle = "#fff"; ctx.lineWidth = 1.2; ctx.stroke();
    if (air.length > 1) this.label(String(air.length), x + s * 0.2, y - s * 0.15, "#fff", Math.max(8, s * 0.26));
  }

  drawUnit(u, cx, cy, s) {
    const ctx = this.ctx;
    const color = this.playerColor(u.owner);
    const r = s * (u.class === "civilian" ? 0.26 : 0.34);
    ctx.save();
    // base shape by class
    ctx.beginPath();
    if (u.domain === "sea" && u.class !== "civilian") {
      ctx.moveTo(cx - r * 1.2, cy - r * 0.4); ctx.lineTo(cx + r * 1.2, cy - r * 0.4); ctx.lineTo(cx + r * 0.8, cy + r * 0.6);
      ctx.lineTo(cx - r * 0.8, cy + r * 0.6); ctx.closePath();
    } else if (["ranged"].includes(u.class)) {
      ctx.moveTo(cx, cy - r * 1.15); ctx.lineTo(cx + r * 1.15, cy); ctx.lineTo(cx, cy + r * 1.15); ctx.lineTo(cx - r * 1.15, cy); ctx.closePath();
    } else if (["mounted", "armor"].includes(u.class)) {
      ctx.moveTo(cx, cy - r * 1.2); ctx.lineTo(cx + r * 1.15, cy + r * 0.8); ctx.lineTo(cx - r * 1.15, cy + r * 0.8); ctx.closePath();
    } else if (u.class === "siege") {
      ctx.rect(cx - r, cy - r, r * 2, r * 2);
    } else if (u.class === "civilian") {
      ctx.roundRect(cx - r, cy - r, r * 2, r * 2, r * 0.4);
    } else {
      ctx.arc(cx, cy, r, 0, Math.PI * 2);
    }
    ctx.fillStyle = color;
    ctx.fill();
    ctx.lineWidth = Math.max(1.2, s * 0.05);
    ctx.strokeStyle = u.owner === this.model.you ? "#ffffff" : "#111";
    ctx.stroke();
    // glyph
    ctx.strokeStyle = textColorFor(color);
    ctx.fillStyle = textColorFor(color);
    ctx.lineWidth = Math.max(1, s * 0.045);
    this.drawGlyph(u, cx, cy, r);
    ctx.restore();
    // hp bar
    if (u.hp != null && u.hp < 100) {
      const w = r * 2.2;
      ctx.fillStyle = "#300"; ctx.fillRect(cx - w / 2, cy + r * 1.25, w, Math.max(2, s * 0.07));
      ctx.fillStyle = u.hp > 50 ? "#4cd964" : u.hp > 25 ? "#ffcc00" : "#ff3b30";
      ctx.fillRect(cx - w / 2, cy + r * 1.25, w * u.hp / 100, Math.max(2, s * 0.07));
    }
    // status badges
    if (u.owner === this.model.you && s > 16) {
      let badge = null;
      if (u.promotion_ready) badge = ["▲", "#ffd84a"];
      else if (u.activity === "fortify") badge = ["⛨", "#ddd"];
      else if (u.activity === "sleep") badge = ["z", "#ccc"];
      else if (u.activity === "build") badge = ["⚒", "#f3e2b8"];
      else if (u.activity === "explore" || u.activity === "automate") badge = ["↻", "#9fd"];
      else if (u.activity === "goto") badge = ["→", "#9fd"];
      else if (u.moves > 0) badge = ["●", "#7CFC00"];
      if (badge) this.label(badge[0], cx + r * 1.05, cy - r * 1.05, badge[1], Math.max(9, s * 0.28));
    }
  }

  drawGlyph(u, cx, cy, r) {
    const ctx = this.ctx;
    const t = u.type;
    const line = (x1, y1, x2, y2) => { ctx.beginPath(); ctx.moveTo(cx + x1 * r, cy + y1 * r); ctx.lineTo(cx + x2 * r, cy + y2 * r); ctx.stroke(); };
    switch (u.class) {
      case "melee":
        line(-0.5, 0.5, 0.5, -0.5); line(-0.15, -0.05, 0.05, 0.15); line(-0.3, -0.25, 0.25, 0.3);
        if (u.unit_type === "Gunpowder") { ctx.beginPath(); ctx.arc(cx + r * 0.45, cy - r * 0.45, r * 0.12, 0, 7); ctx.fill(); }
        if (/Spearman|Pikeman|Hoplite|Landsknecht/.test(t)) line(-0.6, 0.6, 0.6, -0.6);
        break;
      case "ranged":
        ctx.beginPath(); ctx.arc(cx - r * 0.2, cy, r * 0.55, -Math.PI / 2, Math.PI / 2); ctx.stroke();
        line(-0.2, -0.55, -0.2, 0.55); line(-0.45, 0, 0.55, 0);
        break;
      case "mounted":
        ctx.beginPath(); ctx.arc(cx, cy + r * 0.1, r * 0.4, Math.PI * 0.9, Math.PI * 2.1); ctx.stroke();
        break;
      case "armor":
        ctx.strokeRect(cx - r * 0.5, cy - r * 0.05, r, r * 0.45); line(0, 0.05, 0.65, -0.3);
        break;
      case "siege":
        ctx.beginPath(); ctx.arc(cx - r * 0.25, cy + r * 0.35, r * 0.25, 0, 7); ctx.stroke(); line(-0.25, 0.35, 0.55, -0.5);
        break;
      case "recon":
        ctx.beginPath(); ctx.ellipse(cx, cy, r * 0.6, r * 0.35, 0, 0, 7); ctx.stroke();
        ctx.beginPath(); ctx.arc(cx, cy, r * 0.15, 0, 7); ctx.fill();
        break;
      case "naval_melee": case "naval_ranged":
        line(0, -0.35, 0, 0.35); line(-0.4, 0.1, 0.4, 0.1);
        if (u.class === "naval_ranged") { ctx.beginPath(); ctx.arc(cx, cy - r * 0.2, r * 0.12, 0, 7); ctx.fill(); }
        break;
      case "anti_air":
        line(0, 0.5, 0, -0.5); line(-0.3, -0.2, 0, -0.5); line(0.3, -0.2, 0, -0.5);
        break;
      case "civilian": {
        ctx.font = `700 ${r * (t.startsWith("Great") ? 0.8 : 1.1)}px system-ui`; ctx.textAlign = "center"; ctx.textBaseline = "middle";
        const g = { "Settler": "S", "Worker": "W", "Work Boats": "B", "Missionary": "M", "Inquisitor": "I",
                    "Great Prophet": "P", "Great Scientist": "Sc", "Great Engineer": "En", "Great Merchant": "Me",
                    "Great Artist": "Ar", "Great General": "Ge", "Great Admiral": "Ad" }[t] || (u.great_person ? "G" : "C");
        ctx.fillText(g, cx, cy + r * 0.05);
        break;
      }
      default:
        ctx.beginPath(); ctx.arc(cx, cy, r * 0.25, 0, 7); ctx.fill();
    }
  }
}

// ----------------------------------------------------------------------------
export function hexAlpha(hex, a) {
  const [r, g, b] = hexToRgb(hex);
  return `rgba(${r},${g},${b},${a})`;
}

function hexToRgb(hex) {
  let h = hex.replace("#", "");
  if (h.length === 3) h = h.split("").map((c) => c + c).join("");
  const n = parseInt(h, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

export function shade(hex, amt) {
  const [r, g, b] = hexToRgb(hex);
  const f = (c) => Math.max(0, Math.min(255, Math.round(amt < 0 ? c * (1 + amt) : c + (255 - c) * amt)));
  return `#${[f(r), f(g), f(b)].map((c) => c.toString(16).padStart(2, "0")).join("")}`;
}

function textColorFor(hex) {
  const [r, g, b] = hexToRgb(hex);
  return (r * 299 + g * 587 + b * 114) / 1000 > 150 ? "#111" : "#fff";
}

// Build a renderer model from a server client_view.
// Tile rows: [idx, terrain, features, wonder, river bitmask, resource, improvement, route, pillaged, route pillaged,
//             owner, visible]
export function modelFromView(view) {
  const size = view.width * view.height;
  const tiles = new Array(size).fill(null);
  for (const t of view.tiles) {
    const features = t[2] || [];
    tiles[t[0]] = {
      terrain: t[1], features, hills: features.includes("Hill"),
      feature: features.filter((f) => f !== "Hill").slice(-1)[0] || null, wonder: t[3], river: t[4], resource: t[5],
      improvement: t[6], route: t[7], pillaged: !!t[8], routePillaged: !!t[9], owner: t[10], visible: !!t[11],
      camp: t[6] === "Barbarian encampment", village: t[6] === "Ancient ruins",
    };
  }
  const players = {};
  for (const p of view.players) players[p.id] = p;
  return { width: view.width, height: view.height, tiles, units: view.units, cities: view.cities, players, you: view.you };
}

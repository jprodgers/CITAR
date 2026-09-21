// Map editor: paint terrain, features, resources, natural wonders, rivers, improvements, routes and start positions,
// then save the map (saves/maps/<id>.json) and play or build scenarios on it. Editing happens in the browser; the
// server validates on save. Controls: left-drag paints, right- or middle-drag pans, wheel zooms, Ctrl+Z undoes.
import { api } from "./api.js";
import { el, clear, toast, modal, prompt as askText, confirmBox } from "./util.js";
import { MapRenderer } from "./render.js";
import { mapOptionsForm } from "./mapoptions.js";
import { hexCenter, hexCorners, neighbor, hexDistance, EDGE_CORNERS } from "./hex.js";
import { pageHeader } from "./nav.js";

const WATER = new Set(["Ocean", "Coast", "Lakes"]);
const MAP_IMPROVEMENTS = ["Ancient ruins", "Barbarian encampment", "City ruins", "Farm", "Mine", "Pasture", "Plantation",
  "Camp", "Quarry", "Lumber mill", "Trading post", "Fishing Boats", "Oil well", "Offshore Platform", "Fort", "Citadel",
  "Moai", "Terrace farm", "Polder"];
const CS_COLOR = "#dddddd";
// tile row layout: [terrain, features, wonder, river, resource, resource_amount, improvement, route]
const T = { terrain: 0, features: 1, wonder: 2, river: 3, resource: 4, amount: 5, improvement: 6, route: 7 };

export async function renderEditor(root, rules, mapId) {
  const ed = new MapEditor(root, rules);
  if (mapId) await ed.open(decodeURIComponent(mapId));
  else ed.load(blank(40, 26, "Ocean"), true);
  return ed;
}

function blank(w, h, terrain) {
  return { format: "citar-map", version: 1, id: "", name: "Untitled map", description: "", width: w, height: h,
    tiles: Array.from({ length: w * h }, () => [terrain, [], null, 0, null, 0, null, null]), starts: [], cs_starts: [] };
}

class MapEditor {
  constructor(root, rules) {
    this.rules = rules;
    this.tool = "terrain";
    this.brush = 0;
    this.choice = { terrain: "Grassland", feature: "Forest", featureMode: "add", resource: "Wheat", amount: 0,
      wonder: Object.keys(rules.terrains).find((k) => rules.terrains[k].type === "NaturalWonder"),
      improvement: "Ancient ruins", route: "Road", start: "civ" };
    this.undo = [];
    this.redo = [];
    this.dirty = false;
    this.page = el("div", { class: "editor" });
    root.appendChild(this.page);
    this.header = pageHeader("editor");
    this.side = el("div", { class: "editor-side" });
    this.canvas = el("canvas", { class: "editor-canvas" });
    this.status = el("div", { class: "editor-status muted small" }, "");
    this.page.append(el("div", { class: "editor-head" }, this.header),
      this.side, el("div", { class: "editor-main" }, this.canvas), this.status);
    this.renderer = new MapRenderer(this.canvas, rules);
    this.renderer.showGrid = true;
    this.bindInput();
    this._beforeUnload = (e) => { if (this.dirty) { e.preventDefault(); e.returnValue = ""; } };
    window.addEventListener("beforeunload", this._beforeUnload);
    this._keys = (e) => this.onKey(e);
    window.addEventListener("keydown", this._keys);
  }

  destroy() {
    window.removeEventListener("beforeunload", this._beforeUnload);
    window.removeEventListener("keydown", this._keys);
  }

  // ------------------------------------------------------------------ data
  load(map, fresh = false) {
    this.map = map;
    this.undo = [];
    this.redo = [];
    this.dirty = false;
    const tiles = map.tiles.map((row) => this.tileObj(row));
    this.renderer.setModel({ width: map.width, height: map.height, tiles, units: [], cities: [], players: {} });
    this.renderer.cam.zoom = Math.max(0.25, Math.min(1, 1100 / (map.width * 30 * 1.8)));
    this.renderer.centerOn(Math.floor(map.width / 2), Math.floor(map.height / 2));
    this.refreshOverlay();
    this.drawSide();
  }

  tileObj(row) {
    const features = row[T.features] || [];
    return { terrain: row[T.terrain], features, hills: features.includes("Hill"),
      feature: features.filter((f) => f !== "Hill").slice(-1)[0] || null, wonder: row[T.wonder], river: row[T.river] || 0,
      resource: row[T.resource], improvement: row[T.improvement], route: row[T.route], pillaged: false,
      routePillaged: false, owner: null, visible: true,
      camp: row[T.improvement] === "Barbarian encampment", village: row[T.improvement] === "Ancient ruins" };
  }

  async open(id) {
    try { this.load(await api.map(id)); } catch (e) { toast(e.message, "error"); this.load(blank(40, 26, "Ocean"), true); }
  }

  idx(x, y) { return y * this.map.width + x; }
  xy(i) { return [i % this.map.width, Math.floor(i / this.map.width)]; }

  // ------------------------------------------------------------------ editing
  beginStroke() { this.stroke = new Map(); }
  endStroke() {
    if (this.stroke && this.stroke.size) {
      this.undo.push(this.stroke);
      if (this.undo.length > 200) this.undo.shift();
      this.redo = [];
    }
    this.stroke = null;
  }

  // record the previous state of tile i (and the start lists) before the first change in this stroke
  touch(i) {
    if (this.stroke && !this.stroke.has(i)) this.stroke.set(i, JSON.stringify(this.map.tiles[i]));
    if (this.stroke && !this.stroke.has("starts")) this.stroke.set("starts", JSON.stringify([this.map.starts, this.map.cs_starts]));
  }

  setRow(i, row) {
    this.touch(i);
    this.map.tiles[i] = row;
    this.renderer.model.tiles[i] = this.tileObj(row);
    this.dirty = true;
  }

  applyUndo(from, to) {
    const st = from.pop();
    if (!st) return;
    const back = new Map();
    for (const [k, v] of st) {
      if (k === "starts") {
        back.set(k, JSON.stringify([this.map.starts, this.map.cs_starts]));
        [this.map.starts, this.map.cs_starts] = JSON.parse(v);
      } else {
        back.set(k, JSON.stringify(this.map.tiles[k]));
        this.map.tiles[k] = JSON.parse(v);
        this.renderer.model.tiles[k] = this.tileObj(this.map.tiles[k]);
      }
    }
    to.push(back);
    this.dirty = true;
    this.refreshOverlay();
    this.renderer.invalidate();
  }

  brushTiles(x, y) {
    const out = [];
    const r = this.brush;
    for (let yy = y - r; yy <= y + r; yy++) for (let xx = x - r - 1; xx <= x + r + 1; xx++) {
      if (xx < 0 || yy < 0 || xx >= this.map.width || yy >= this.map.height) continue;
      if (hexDistance(x, y, xx, yy) <= r) out.push([xx, yy]);
    }
    return out;
  }

  paint(x, y) {
    const R = this.rules;
    const c = this.choice;
    const single = ["wonder", "start"].includes(this.tool);
    for (const [xx, yy] of single ? [[x, y]] : this.brushTiles(x, y)) {
      const i = this.idx(xx, yy);
      const row = JSON.parse(JSON.stringify(this.map.tiles[i]));
      if (this.tool === "terrain") {
        if (row[T.terrain] === c.terrain) continue;
        row[T.terrain] = c.terrain;
        const water = WATER.has(c.terrain);
        row[T.features] = row[T.features].filter((f) => this.featureFits(f, c.terrain, row[T.features]) && (!water || this.waterFeature(f)));
        if (water || c.terrain === "Mountain") { row[T.improvement] = null; row[T.route] = null; }
        if (water) row[T.river] = 0;
        if (row[T.resource] && !this.resourceFits(row[T.resource], row)) { row[T.resource] = null; row[T.amount] = 0; }
        if (row[T.wonder]) row[T.wonder] = null;
      } else if (this.tool === "feature") {
        const has = row[T.features].includes(c.feature);
        if (c.featureMode === "remove") {
          if (!has) continue;
          row[T.features] = row[T.features].filter((f) => f !== c.feature);
        } else {
          if (has || !this.featureFits(c.feature, row[T.terrain], row[T.features])) continue;
          row[T.features] = [...row[T.features], c.feature].sort((a, b) => (a === "Hill" ? -1 : b === "Hill" ? 1 : 0));
        }
      } else if (this.tool === "resource") {
        if (c.resource === "") { row[T.resource] = null; row[T.amount] = 0; }
        else {
          const rd = R.resources[c.resource];
          row[T.resource] = c.resource;
          row[T.amount] = rd && rd.resourceType === "Strategic" ? (c.amount || ((rd.minorDepositAmount || {}).default || 2)) : 0;
        }
      } else if (this.tool === "wonder") {
        if (c.wonder === "") { row[T.wonder] = null; }
        else {
          const wd = R.terrains[c.wonder] || {};
          row[T.wonder] = c.wonder; row[T.features] = []; row[T.resource] = null; row[T.amount] = 0;
          row[T.improvement] = null; row[T.route] = null;
          if (wd.turnsInto && R.terrains[wd.turnsInto]) row[T.terrain] = wd.turnsInto;
          else if (wd.occursOn && wd.occursOn.length && !wd.occursOn.includes(row[T.terrain])) {
            const base = wd.occursOn.find((t) => R.terrains[t] && ["Land", "Water"].includes(R.terrains[t].type));
            if (base) row[T.terrain] = base;
          }
        }
      } else if (this.tool === "improvement") {
        if (c.improvement !== "" && (WATER.has(row[T.terrain]) !== ["Fishing Boats", "Offshore Platform"].includes(c.improvement))) continue;
        row[T.improvement] = c.improvement || null;
      } else if (this.tool === "route") {
        if (c.route && (WATER.has(row[T.terrain]) || row[T.terrain] === "Mountain")) continue;
        row[T.route] = c.route || null;
      } else if (this.tool === "erase") {
        row[T.features] = []; row[T.wonder] = null; row[T.resource] = null; row[T.amount] = 0; row[T.improvement] = null;
        row[T.route] = null;
        this.setRiverMask(i, 0);
      } else if (this.tool === "start") {
        this.toggleStart(i);
        continue;
      }
      this.setRow(i, row);
    }
    this.refreshOverlay();
    this.renderer.invalidate();
  }

  featureFits(f, terrain, features) {
    const fd = this.rules.terrains[f];
    if (!fd) return false;
    if (!fd.occursOn || !fd.occursOn.length) return true;
    return fd.occursOn.includes(terrain) || features.some((x) => x !== f && fd.occursOn.includes(x));
  }

  waterFeature(f) {
    const fd = this.rules.terrains[f] || {};
    return (fd.occursOn || []).some((t) => WATER.has(t));
  }

  resourceFits(res, row) {
    const rd = this.rules.resources[res];
    if (!rd) return false;
    const last = row[T.features].length ? row[T.features][row[T.features].length - 1] : row[T.terrain];
    const ok = rd.terrainsCanBeFoundOn || [];
    return ok.includes(last) || ok.includes(row[T.terrain]);
  }

  setRiverMask(i, mask) {
    const [x, y] = this.xy(i);
    const row = this.map.tiles[i];
    for (let d = 0; d < 6; d++) {
      const had = !!(row[T.river] & (1 << d));
      if (had !== !!(mask & (1 << d))) this.setRiverEdge(x, y, d, !had);
    }
  }

  // rivers live on both tiles of an edge
  setRiverEdge(x, y, d, on) {
    const n = neighbor(x, y, d, this.map.width, this.map.height);
    if (!n) return;
    const i = this.idx(x, y), j = this.idx(n[0], n[1]);
    const a = this.map.tiles[i], b = this.map.tiles[j];
    if (on && (WATER.has(a[T.terrain]) || WATER.has(b[T.terrain]))) return;
    const ra = JSON.parse(JSON.stringify(a)), rb = JSON.parse(JSON.stringify(b));
    const od = (d + 3) % 6;
    ra[T.river] = on ? (ra[T.river] | (1 << d)) : (ra[T.river] & ~(1 << d));
    rb[T.river] = on ? (rb[T.river] | (1 << od)) : (rb[T.river] & ~(1 << od));
    this.setRow(i, ra);
    this.setRow(j, rb);
  }

  toggleStart(i) {
    this.touch(i);
    const m = this.map;
    const row = m.tiles[i];
    const td = this.rules.terrains[row[T.terrain]] || {};
    const inCiv = m.starts.indexOf(i), inCs = m.cs_starts.indexOf(i);
    if (inCiv >= 0) { m.starts.splice(inCiv, 1); }
    else if (inCs >= 0) { m.cs_starts.splice(inCs, 1); }
    else {
      if (td.type === "Water" || td.impassable || row[T.wonder]) { toast("Start positions must be on passable land.", "error"); return; }
      if (this.choice.start === "civ") {
        if (m.starts.length >= (this.rules.max_players || 24)) { toast("That's the maximum number of civilizations.", "error"); return; }
        m.starts.push(i);
      } else m.cs_starts.push(i);
    }
    this.dirty = true;
    this.drawSide();
  }

  refreshOverlay() {
    const colors = this.rules.player_colors || [];
    const markers = [];
    this.map.starts.forEach((i, k) => { const [x, y] = this.xy(i); markers.push({ x, y, label: `${k + 1}`, color: colors[k % colors.length] || "#fff" }); });
    this.map.cs_starts.forEach((i) => { const [x, y] = this.xy(i); markers.push({ x, y, label: "cs", color: CS_COLOR, small: true }); });
    this.renderer.overlay.markers = markers;
  }

  // ------------------------------------------------------------------ input
  bindInput() {
    const cv = this.canvas;
    let mode = null, last = null;
    cv.addEventListener("contextmenu", (e) => e.preventDefault());
    cv.addEventListener("mousedown", (e) => {
      const r = cv.getBoundingClientRect();
      const sx = e.clientX - r.left, sy = e.clientY - r.top;
      if (e.button === 1 || e.button === 2 || this.spaceDown) { mode = "pan"; last = [e.clientX, e.clientY]; return; }
      if (e.button !== 0) return;
      mode = "paint";
      this.beginStroke();
      if (this.tool === "river") { this.riverClick(sx, sy); return; }
      if (this.tool === "inspect") { const t = this.renderer.screenToTile(sx, sy); if (t) this.inspect(t.x, t.y); return; }
      const t = this.renderer.screenToTile(sx, sy);
      if (t) { this.lastTile = `${t.x},${t.y}`; this.paint(t.x, t.y); }
    });
    window.addEventListener("mouseup", () => { if (mode === "paint") { this.endStroke(); this.drawSide(); } mode = null; this.lastEdge = null; });
    cv.addEventListener("mousemove", (e) => {
      const r = cv.getBoundingClientRect();
      const sx = e.clientX - r.left, sy = e.clientY - r.top;
      if (mode === "pan") { this.renderer.pan(e.clientX - last[0], e.clientY - last[1]); last = [e.clientX, e.clientY]; return; }
      const t = this.renderer.screenToTile(sx, sy);
      this.hover(t, sx, sy);
      if (mode === "paint" && t && !["start", "wonder", "inspect"].includes(this.tool)) {
        if (this.tool === "river") { this.riverDrag(sx, sy); return; }
        const key = `${t.x},${t.y}`;
        if (key !== this.lastTile) {
          // fill the gap when the mouse moved several tiles between events
          const [px, py] = this.lastTile ? this.lastTile.split(",").map(Number) : [t.x, t.y];
          for (const [lx, ly] of hexLine(px, py, t.x, t.y).slice(1)) this.paint(lx, ly);
          this.lastTile = key;
        }
      }
    });
    cv.addEventListener("mouseleave", () => { this.renderer.overlay.brush = null; this.renderer.overlay.edge = null; this.renderer.invalidate(); });
    cv.addEventListener("wheel", (e) => {
      e.preventDefault();
      const r = cv.getBoundingClientRect();
      this.renderer.zoomAt(e.deltaY < 0 ? 1.15 : 1 / 1.15, e.clientX - r.left, e.clientY - r.top);
    }, { passive: false });
  }

  onKey(e) {
    if (e.target && ["INPUT", "TEXTAREA", "SELECT"].includes(e.target.tagName)) return;
    if (e.code === "Space") { this.spaceDown = e.type === "keydown"; }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") { e.preventDefault(); e.shiftKey ? this.applyUndo(this.redo, this.undo) : this.applyUndo(this.undo, this.redo); this.drawSide(); }
    else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "y") { e.preventDefault(); this.applyUndo(this.redo, this.undo); this.drawSide(); }
    else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") { e.preventDefault(); this.save(); }
    else if (e.key === "[") { this.brush = Math.max(0, this.brush - 1); this.drawSide(); }
    else if (e.key === "]") { this.brush = Math.min(4, this.brush + 1); this.drawSide(); }
  }

  // the tile edge nearest to a screen point: [x, y, direction]
  edgeAt(sx, sy) {
    const t = this.renderer.screenToTile(sx, sy);
    if (!t) return null;
    const size = this.renderer.size;
    const [wx, wy] = this.renderer.screenToWorld(sx, sy);
    const [cx, cy] = hexCenter(t.x, t.y, size);
    const corners = hexCorners(cx, cy, size);
    let best = null, bd = 1e9;
    for (let d = 0; d < 6; d++) {
      const [a, b] = EDGE_CORNERS[d];
      const mx = (corners[a][0] + corners[b][0]) / 2, my = (corners[a][1] + corners[b][1]) / 2;
      const dist = (mx - wx) ** 2 + (my - wy) ** 2;
      if (dist < bd) { bd = dist; best = d; }
    }
    return { x: t.x, y: t.y, d: best };
  }

  riverClick(sx, sy) {
    const e = this.edgeAt(sx, sy);
    if (!e) return;
    const on = !(this.map.tiles[this.idx(e.x, e.y)][T.river] & (1 << e.d));
    this.riverOn = on;
    this.lastEdge = `${e.x},${e.y},${e.d}`;
    this.setRiverEdge(e.x, e.y, e.d, on);
    this.renderer.invalidate();
  }

  riverDrag(sx, sy) {
    const e = this.edgeAt(sx, sy);
    if (!e) return;
    const key = `${e.x},${e.y},${e.d}`;
    if (key === this.lastEdge) return;
    this.lastEdge = key;
    this.setRiverEdge(e.x, e.y, e.d, this.riverOn);
    this.renderer.invalidate();
  }

  hover(t, sx, sy) {
    const ov = this.renderer.overlay;
    ov.brush = t && !["river", "inspect"].includes(this.tool) ? (["start", "wonder"].includes(this.tool) ? [[t.x, t.y]] : this.brushTiles(t.x, t.y)) : null;
    ov.edge = this.tool === "river" ? this.edgeAt(sx, sy) : null;
    this.renderer.invalidate();
    this.status.textContent = t ? this.describe(t.x, t.y) : "";
  }

  describe(x, y) {
    const i = this.idx(x, y);
    const r = this.map.tiles[i];
    const parts = [`(${x}, ${y})`, [r[T.terrain], ...r[T.features]].join(" + ")];
    if (r[T.wonder]) parts.push(`wonder: ${r[T.wonder]}`);
    if (r[T.resource]) parts.push(`${r[T.resource]}${r[T.amount] ? ` ×${r[T.amount]}` : ""}`);
    if (r[T.improvement]) parts.push(r[T.improvement]);
    if (r[T.route]) parts.push(r[T.route]);
    if (r[T.river]) parts.push("river");
    const s = this.map.starts.indexOf(i);
    if (s >= 0) parts.push(`start of civilization ${s + 1}`);
    if (this.map.cs_starts.includes(i)) parts.push("city-state start");
    return parts.join(" · ");
  }

  inspect(x, y) { toast(this.describe(x, y)); }

  // ------------------------------------------------------------------ side panel
  drawSide() {
    const R = this.rules;
    const side = this.side;
    clear(side);
    const m = this.map;
    const sel = (opts, value, onchange) => {
      const s = el("select", { onchange: (e) => onchange(e.target.value) }, ...opts.map(([v, t]) => el("option", { value: v, selected: v === value }, t)));
      return s;
    };
    const byType = (type) => Object.keys(R.terrains).filter((k) => R.terrains[k].type === type).sort();

    // file
    side.append(el("div", { class: "section" }, el("h4", {}, "Map"),
      el("input", { value: m.name, placeholder: "Map name", oninput: (e) => { m.name = e.target.value; this.dirty = true; } }),
      el("textarea", { rows: 2, placeholder: "Description (what this map is for)", value: m.description || "", oninput: (e) => { m.description = e.target.value; this.dirty = true; } }),
      el("div", { class: "muted small" }, `${m.width} × ${m.height} tiles · ${m.starts.length} civ starts · ${m.cs_starts.length} city-state starts` +
        (m.id ? ` · saved as "${m.id}"` : " · not saved yet") + (this.dirty ? " · unsaved changes" : "")),
      el("div", { class: "row" },
        el("button", { class: "primary small", onclick: () => this.save() }, "Save"),
        el("button", { class: "small", onclick: () => this.save(true) }, "Save as…"),
        el("button", { class: "small", onclick: () => this.openDialog() }, "Open…"),
        el("button", { class: "small", onclick: () => this.newDialog() }, "New…")),
      el("div", { class: "row" },
        el("button", { class: "small", onclick: () => this.validate() }, "Check"),
        el("button", { class: "small", onclick: () => this.download() }, "Download"),
        el("button", { class: "small", onclick: () => this.upload() }, "Upload…"),
        m.id ? el("button", { class: "small", onclick: () => { location.hash = `#/?map=${encodeURIComponent(m.id)}`; } }, "Play on this map") : null)));

    // tools
    const tools = [["terrain", "Terrain"], ["feature", "Features"], ["resource", "Resources"], ["wonder", "Natural wonder"],
      ["river", "Rivers"], ["improvement", "Improvements"], ["route", "Roads"], ["start", "Start positions"],
      ["erase", "Eraser"], ["inspect", "Inspect"]];
    side.append(el("div", { class: "section" }, el("h4", {}, "Tool"),
      el("div", { class: "tool-grid" }, ...tools.map(([k, label]) => el("button", {
        class: `small ${this.tool === k ? "active" : ""}`, onclick: () => { this.tool = k; this.drawSide(); } }, label)))));

    const opt = el("div", { class: "section" });
    const c = this.choice;
    const brushRow = () => el("div", { class: "row" }, el("span", { class: "small muted" }, "Brush"),
      ...[0, 1, 2, 3].map((b) => el("button", { class: `small ${this.brush === b ? "active" : ""}`, onclick: () => { this.brush = b; this.drawSide(); } }, `${b * 2 + 1}`)),
      el("span", { class: "small muted" }, "([ and ])"));
    if (this.tool === "terrain") {
      opt.append(el("h4", {}, "Base terrain"),
        el("div", { class: "swatches" }, ...[...byType("Land"), ...byType("Water")].map((t) => el("button", {
          class: `swatch ${c.terrain === t ? "active" : ""}`, onclick: () => { c.terrain = t; this.drawSide(); } },
          el("span", { class: "chip", style: { background: terrainColor(t) } }), t))), brushRow());
    } else if (this.tool === "feature") {
      opt.append(el("h4", {}, "Feature"),
        sel(byType("TerrainFeature").map((f) => [f, `${f}${R.terrains[f].occursOn ? ` (on ${R.terrains[f].occursOn.join(", ")})` : ""}`]), c.feature, (v) => { c.feature = v; }),
        el("div", { class: "row" }, ...["add", "remove"].map((md) => el("button", { class: `small ${c.featureMode === md ? "active" : ""}`, onclick: () => { c.featureMode = md; this.drawSide(); } }, md === "add" ? "Add" : "Remove"))),
        brushRow(), el("p", { class: "muted small" }, "Features only go on terrain that allows them (e.g. Forest on Grassland, Plains, Tundra or Hill)."));
    } else if (this.tool === "resource") {
      const res = Object.keys(R.resources).sort((a, b) => (R.resources[a].resourceType + a).localeCompare(R.resources[b].resourceType + b));
      opt.append(el("h4", {}, "Resource"),
        sel([["", "— remove resource —"], ...res.map((r) => [r, `${r} (${R.resources[r].resourceType})`])], c.resource, (v) => { c.resource = v; this.drawSide(); }));
      const rd = R.resources[c.resource];
      if (rd && rd.resourceType === "Strategic") opt.append(el("label", { class: "small" }, "Amount (0 = default) ",
        el("input", { type: "number", min: 0, max: 20, value: c.amount, style: { width: "60px" }, oninput: (e) => { c.amount = Math.max(0, +e.target.value || 0); } })));
      if (rd) opt.append(el("p", { class: "muted small" }, `Found on: ${(rd.terrainsCanBeFoundOn || []).join(", ") || "anywhere"}. Placing it elsewhere is allowed but reported by Check.`));
      opt.append(brushRow());
    } else if (this.tool === "wonder") {
      opt.append(el("h4", {}, "Natural wonder"),
        sel([["", "— remove wonder —"], ...byType("NaturalWonder").map((w) => [w, w])], c.wonder, (v) => { c.wonder = v; }),
        el("p", { class: "muted small" }, "Places one wonder tile (some wonders change the terrain under them)."));
    } else if (this.tool === "river") {
      opt.append(el("p", { class: "muted small" }, "Click a tile edge to add or remove a river; drag along edges to draw one. Rivers can't run along water."));
    } else if (this.tool === "improvement") {
      opt.append(el("h4", {}, "Improvement"),
        sel([["", "— remove —"], ...MAP_IMPROVEMENTS.map((k) => [k, k])], c.improvement, (v) => { c.improvement = v; }), brushRow(),
        el("p", { class: "muted small" }, "Ancient ruins and barbarian camps are placed automatically on a map without any; draw your own to control them."));
    } else if (this.tool === "route") {
      opt.append(el("h4", {}, "Route"), sel([["Road", "Road"], ["Railroad", "Railroad"], ["", "— remove —"]], c.route, (v) => { c.route = v; }), brushRow());
    } else if (this.tool === "start") {
      opt.append(el("h4", {}, "Start positions"),
        el("div", { class: "row" }, ...[["civ", "Civilization"], ["cs", "City-state"]].map(([k, l]) => el("button", {
          class: `small ${c.start === k ? "active" : ""}`, onclick: () => { c.start = k; this.drawSide(); } }, l))),
        el("p", { class: "muted small" }, "Click land to add a start; click a start to remove it. Civilization starts are numbered in seat order " +
          "(seat 1 starts at 1). Seats without a start position get one chosen automatically."),
        m.starts.length ? el("button", { class: "small", onclick: () => { this.beginStroke(); this.touch(m.starts[0]); m.starts = []; m.cs_starts = []; this.endStroke(); this.dirty = true; this.refreshOverlay(); this.renderer.invalidate(); this.drawSide(); } }, "Clear all starts") : null);
    } else if (this.tool === "erase") {
      opt.append(el("p", { class: "muted small" }, "Removes features, wonders, resources, improvements, roads and rivers (keeps the base terrain)."), brushRow());
    }
    side.append(opt);
    side.append(el("div", { class: "section" }, el("h4", {}, "Controls"),
      el("p", { class: "muted small" }, "Left-drag paints · right- or middle-drag (or Space+drag) pans · wheel zooms · Ctrl+Z / Ctrl+Y undo and redo · Ctrl+S saves."),
      el("div", { class: "row" },
        el("button", { class: "small", disabled: !this.undo.length, onclick: () => { this.applyUndo(this.undo, this.redo); this.drawSide(); } }, "Undo"),
        el("button", { class: "small", disabled: !this.redo.length, onclick: () => { this.applyUndo(this.redo, this.undo); this.drawSide(); } }, "Redo"),
        el("label", { class: "small" }, el("input", { type: "checkbox", checked: this.renderer.showGrid, onchange: (e) => { this.renderer.showGrid = e.target.checked; this.renderer.invalidate(); } }), " grid"))));
  }

  // ------------------------------------------------------------------ files
  async save(asNew = false) {
    let id = this.map.id;
    if (!id || asNew) {
      const suggested = (this.map.name || "map").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 60) || "map";
      id = await askText("Save map", "Map id (letters, digits and dashes):", suggested);
      if (!id) return;
      id = id.toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-|-$/g, "");
      if (!id) return;
    }
    try {
      const res = await api.saveMap(id, this.map);
      this.map.id = res.map.id;
      this.dirty = false;
      toast(`Saved "${res.map.name}"` + (res.warnings.length ? ` with ${res.warnings.length} fixes (see Check)` : ""), res.warnings.length ? "info" : "success");
      if (res.warnings.length) this.showWarnings(res.warnings, "Fixed while saving");
      // pick up the server's clean version (removed invalid entries, both sides of rivers)
      const fresh = await api.map(this.map.id);
      const cam = { ...this.renderer.cam };
      this.load(fresh);
      Object.assign(this.renderer.cam, cam);
      this.renderer.invalidate();
    } catch (e) { toast(e.message, "error"); }
  }

  async validate() {
    try {
      const res = await api.validateMap(this.map);
      if (!res.warnings.length) toast("No problems found.", "success");
      else this.showWarnings(res.warnings, "Problems found");
    } catch (e) { toast(e.message, "error"); }
  }

  showWarnings(list, title) {
    modal({ title, narrow: true, content: el("div", {}, el("p", { class: "muted small" }, "Coordinates are (x, y). Saving applies these fixes."),
      el("pre", { class: "lab-report" }, list.join("\n"))) });
  }

  async openDialog() {
    let maps = [];
    try { maps = await api.maps(); } catch (e) { toast(e.message, "error"); return; }
    let close;
    const list = el("table", { class: "list" }, el("tr", {}, el("th", {}, "Map"), el("th", {}, "Size"), el("th", {}, "Starts"), el("th", {}, "")),
      ...maps.map((mp) => el("tr", { class: "clickable" },
        el("td", { onclick: () => { close(); this.guard(() => this.open(mp.id)); } }, el("b", {}, mp.name), el("div", { class: "muted small" }, mp.description || mp.id)),
        el("td", {}, `${mp.width}×${mp.height}`), el("td", {}, `${mp.starts} + ${mp.cs_starts} cs`),
        el("td", {}, el("button", { class: "small", onclick: async () => {
          if (!(await confirmBox("Delete map", `Delete map "${mp.name}"? This can't be undone.`, "Delete"))) return;
          try { await api.deleteMap(mp.id); close(); this.openDialog(); } catch (e) { toast(e.message, "error"); }
        } }, "Delete")))));
    close = modal({ title: "Open map", content: maps.length ? list : el("p", { class: "muted" }, "No saved maps yet.") }).close;
  }

  async guard(fn) {
    if (this.dirty && !(await confirmBox("Unsaved changes", "Discard the unsaved changes to this map?", "Discard"))) return;
    fn();
  }

  newDialog() {
    const R = this.rules;
    const sizes = Object.entries(R.map_sizes);
    const f = {
      how: el("select", {}, el("option", { value: "generate" }, "Generate a random map"), el("option", { value: "blank" }, "Blank map")),
      size: el("select", {}, ...sizes.map(([k, v]) => el("option", { value: k, selected: k === "small" }, `${v.name} (${v.width}×${v.height}, ${v.players} civs)`)),
        el("option", { value: "custom" }, "Custom size")),
      w: el("input", { type: "number", min: 8, max: 256, value: 40 }), h: el("input", { type: "number", min: 8, max: 256, value: 26 }),
      type: el("select", {}, ...Object.entries(R.map_types).map(([k, v]) => el("option", { value: k }, v.name))),
      fill: el("select", {}, ...Object.keys(R.terrains).filter((k) => ["Land", "Water"].includes(R.terrains[k].type)).map((k) => el("option", { value: k, selected: k === "Ocean" }, k))),
      players: el("input", { type: "number", min: 1, max: R.max_players || 24, placeholder: "size default" }),
      cs: el("input", { type: "number", min: 0, max: 40, placeholder: "size default" }),
      seed: el("input", { placeholder: "random" }),
      ruins: el("input", { type: "checkbox" }),
    };
    const field = (label, input) => el("div", { class: "field" }, el("label", {}, label), input);
    const body = el("div", { class: "grid2" }, field("Start from", f.how), field("Size", f.size), field("Width", f.w), field("Height", f.h),
      field("Map type (generated)", f.type), field("Fill terrain (blank)", f.fill), field("Civilizations (generated)", f.players),
      field("City-states (generated)", f.cs), field("Seed", f.seed), field("Place ancient ruins", f.ruins));
    const gen = mapOptionsForm(R);
    const go = el("button", { class: "primary", onclick: async () => {
      go.disabled = true;
      go.textContent = "Working…";
      const custom = f.size.value === "custom";
      const req = { map_size: custom ? null : f.size.value, width: custom ? +f.w.value : null, height: custom ? +f.h.value : null,
        map_type: f.type.value, players: f.players.value ? +f.players.value : null, city_states: f.cs.value !== "" ? +f.cs.value : null,
        seed: f.seed.value ? +f.seed.value : null, ruins: f.ruins.checked, blank: f.how.value === "blank" ? f.fill.value : null,
        ...gen.value() };
      try {
        const map = await api.generateMap(req);
        dlg.close();
        this.load(map);
        this.dirty = true;
        this.drawSide();
      } catch (e) { toast(e.message, "error"); go.disabled = false; go.textContent = "Create"; }
    } }, "Create");
    let dlg;
    this.guard(() => { dlg = modal({ title: "New map", content: el("div", {}, body, gen.node, el("p", { class: "muted small" }, "Large generated maps take a few seconds.")), footer: go }); });
  }

  download() {
    const blob = new Blob([JSON.stringify(this.map)], { type: "application/json" });
    const a = el("a", { href: URL.createObjectURL(blob), download: `${this.map.id || "map"}.json` });
    document.body.appendChild(a); a.click(); a.remove();
  }

  upload() {
    const input = el("input", { type: "file", accept: ".json,application/json" });
    input.onchange = async () => {
      const file = input.files[0];
      if (!file) return;
      try {
        const map = JSON.parse(await file.text());
        const res = await api.validateMap(map);
        this.guard(() => { this.load({ ...map, id: "" }); this.dirty = true; this.drawSide(); });
        if (res.warnings.length) this.showWarnings(res.warnings, "Problems in the uploaded map");
      } catch (e) { toast(`Could not load that file: ${e.message}`, "error"); }
    };
    input.click();
  }
}

// tiles on the straight line between two offset coordinates (cube-coordinate interpolation)
function hexLine(x0, y0, x1, y1) {
  const cube = (x, y) => { const q = x - (y - (y & 1)) / 2; return [q, y, -q - y]; };
  const a = cube(x0, y0), b = cube(x1, y1);
  const n = hexDistance(x0, y0, x1, y1);
  const out = [];
  for (let i = 0; i <= n; i++) {
    const t = n ? i / n : 0;
    const f = [0, 1, 2].map((k) => a[k] + (b[k] - a[k]) * t + 1e-6);
    let r = f.map(Math.round);
    const d = r.map((v, k) => Math.abs(v - f[k]));
    if (d[0] > d[1] && d[0] > d[2]) r[0] = -r[1] - r[2];
    else if (d[1] > d[2]) r[1] = -r[0] - r[2];
    else r[2] = -r[0] - r[1];
    out.push([r[0] + (r[1] - (r[1] & 1)) / 2, r[1]]);
  }
  return out;
}

function terrainColor(t) {
  return { Grassland: "#5b8f3d", Plains: "#a19f4c", Desert: "#d9c47f", Tundra: "#8c9b88", Snow: "#e6edf0",
    Mountain: "#7a6e63", Coast: "#3e7fb6", Ocean: "#244f7d", Lakes: "#4a8dca" }[t] || "#666";
}

// The in-game screen: map, top bar, unit/city panels, side feeds, spectator controls.
import { api, connectWS } from "./api.js";
import { el, clear, toast, fmt, signed, nameOf, prompt, confirmBox, modal } from "./util.js";
import { MapRenderer, modelFromView, TERRAIN_COLORS } from "./render.js";
import { hexDistance, neighbors } from "./hex.js";
import { renderUnitPanel, renderCityPanel, openTechTree, openDiplomacy, openEmpire, openNotes, openHelp, openPolicies,
         openReligion, openGreatPeople, openCityStates, openVictory, chooseBeliefs } from "./panels.js";
import { openMetrics } from "./metrics.js";

export class GameScreen {
  constructor(root, rules, gid, token, asPlayer) {
    this.root = root;
    this.rules = rules;
    this.gid = gid;
    this.token = token;
    this.asPlayer = asPlayer != null && asPlayer !== "" ? +asPlayer : null;
    this.view = null;
    this.selectedUnit = null;
    this.unitDetail = null;
    this.selectedCity = null;
    this.cityDetail = null;
    this.mode = null;
    this.feed = [];
    this.thoughts = [];
    this.sideTab = "events";
    this.agentStatus = {};
    this._refreshTimer = null;
    this._inflight = false;
    this._pending = false;
    this._lastTurnKey = null;
    this._centered = false;
    this.build();
    this.refresh();
    this.ws = connectWS(gid, token, (m) => this.onWS(m));
    this._keyHandler = (e) => this.onKey(e);
    document.addEventListener("keydown", this._keyHandler);
  }

  destroy() {
    if (this.ws) this.ws.close();
    document.removeEventListener("keydown", this._keyHandler);
  }

  get you() { return this.view ? this.view.you : null; }
  get isSpectator() { return !!(this.view && this.view.spectator); }
  get myTurn() { return this.view && this.you != null && this.view.current_player === this.you && this.view.phase === "playing" && !this.isSpectator; }

  // ------------------------------------------------------------------
  build() {
    this.topbar = el("div", { class: "topbar" });
    this.canvas = el("canvas", { class: "map" });
    this.main = el("div", { class: "main" }, this.canvas);
    this.unitPanel = el("div", { class: "overlay-panel unit-panel", style: { display: "none" } });
    this.cityPanel = el("div", { class: "overlay-panel city-panel", style: { display: "none" } });
    this.sidePanel = el("div", { class: "overlay-panel side-panel" });
    this.tooltip = el("div", { class: "tooltip", style: { display: "none" } });
    this.banner = el("div", { class: "banner", style: { display: "none" } });
    this.minimap = el("canvas", { class: "minimap", width: 200, height: 130 });
    this.turnBox = el("div", { class: "turn-box" });
    this.main.append(this.unitPanel, this.cityPanel, this.sidePanel, this.tooltip, this.banner, this.minimap, this.turnBox);
    this.root.appendChild(el("div", { class: "game" }, this.topbar, this.main));
    this.renderer = new MapRenderer(this.canvas, this.rules);
    this.renderer.onViewChange = () => this.drawMinimap();
    this.bindMouse();
    this.minimap.addEventListener("click", (e) => {
      if (!this.view) return;
      const r = this.minimap.getBoundingClientRect();
      const x = Math.floor((e.clientX - r.left) / r.width * this.view.width);
      const y = Math.floor((e.clientY - r.top) / r.height * this.view.height);
      this.renderer.centerOn(x, y);
    });
  }

  bindMouse() {
    const c = this.canvas;
    let down = null;
    c.addEventListener("contextmenu", (e) => e.preventDefault());
    c.addEventListener("mousedown", (e) => { down = { x: e.clientX, y: e.clientY, button: e.button, moved: false }; });
    window.addEventListener("mouseup", (e) => {
      if (!down) return;
      const d = down;
      down = null;
      if (d.moved) { c.style.cursor = "default"; return; }
      const r = c.getBoundingClientRect();
      const tile = this.renderer.screenToTile(e.clientX - r.left, e.clientY - r.top);
      if (!tile || e.target !== c) return;
      if (d.button === 2) this.onRightClick(tile);
      else if (d.button === 0) this.onLeftClick(tile, e);
    });
    c.addEventListener("mousemove", (e) => {
      const r = c.getBoundingClientRect();
      if (down) {
        const dx = e.clientX - down.x, dy = e.clientY - down.y;
        if (down.moved || Math.abs(dx) + Math.abs(dy) > 5) {
          down.moved = true;
          c.style.cursor = "grabbing";
          this.renderer.pan(dx, dy);
          down.x = e.clientX; down.y = e.clientY;
          this.drawMinimap();
        }
        return;
      }
      const tile = this.renderer.screenToTile(e.clientX - r.left, e.clientY - r.top);
      const changed = !tile || !this.hover || tile.x !== this.hover.x || tile.y !== this.hover.y;
      this.hover = tile;
      if (changed) this.previewPath(tile);
      this.updateOverlay();
      this.showTooltip(tile, e.clientX - r.left, e.clientY - r.top);
    });
    c.addEventListener("mouseleave", () => { this.hover = null; this.tooltip.style.display = "none"; this.updateOverlay(); });
    c.addEventListener("wheel", (e) => {
      e.preventDefault();
      const r = c.getBoundingClientRect();
      this.renderer.zoomAt(e.deltaY < 0 ? 1.12 : 1 / 1.12, e.clientX - r.left, e.clientY - r.top);
      this.drawMinimap();
    }, { passive: false });
  }

  // ------------------------------------------------------------------
  scheduleRefresh(delay = 150) {
    if (this._refreshTimer) return;
    this._refreshTimer = setTimeout(() => { this._refreshTimer = null; this.refresh(); }, delay);
  }

  async refresh() {
    if (this._inflight) { this._pending = true; return; }
    this._inflight = true;
    try {
      const v = await api.view(this.gid, this.token, this.asPlayer);
      this.view = v;
      if (!this.feed.length) this.feed = (v.events || []).slice(-150);
      if (v.thoughts && !this.thoughts.length) this.thoughts = v.thoughts.slice(-80);
      this.model = modelFromView(v);
      this.renderer.setModel(this.model);
      if (!this._centered) { this.centerHome(); this._centered = true; }
      if (this.selectedUnit != null && !v.units.some((u) => u.id === this.selectedUnit && u.owner === this.you)) this.deselect();
      if (this.selectedCity != null && !v.cities.some((c) => c.id === this.selectedCity && c.owner === this.you)) this.deselect();
      await this.refreshDetails();
      this.renderTopbar();
      this.renderSide();
      this.renderBanner();
      if (this.hover && this._tipPos && this.tooltip.style.display !== "none") this.showTooltip(this.hover, ...this._tipPos);
      this.updateOverlay();
      this.drawMinimap();
      const key = `${v.turn}:${v.current_player}`;
      if (key !== this._lastTurnKey) {
        if (this._lastTurnKey && this.myTurn) toast(`Turn ${v.turn}: your turn.`);
        this._lastTurnKey = key;
        if (this.myTurn && this.selectedUnit == null) this.selectNextIdle(false);
        if (this.myTurn) {
          // pop the list open only when something new turns up, not every turn for a problem you already know about
          const keys = new Set((v.alerts || []).map((a) => `${a.type}:${a.city ?? ""}:${a.unit ?? ""}`));
          const fresh = [...keys].some((k) => !(this._alertKeys || new Set()).has(k));
          this._alertKeys = keys;
          if (fresh && keys.size) this.showAlerts();
        }
      }
      this.checkNegotiations();
    } catch (e) {
      toast(e.message, "error");
    } finally {
      this._inflight = false;
      if (this._pending) { this._pending = false; this.scheduleRefresh(50); }
    }
  }

  async refreshDetails() {
    if (this.selectedUnit != null && !this.isSpectator) {
      const r = await api.tool(this.gid, this.token, "get_unit", { unit_id: this.selectedUnit });
      this.unitDetail = r.ok ? r.result : null;
    }
    if (this.selectedCity != null && !this.isSpectator) {
      const r = await api.tool(this.gid, this.token, "get_city", { city_id: this.selectedCity });
      this.cityDetail = r.ok ? r.result : null;
    }
    this.renderPanels();
  }

  centerHome() {
    const v = this.view;
    const cap = v.cities.find((c) => c.owner === this.you && c.capital) || v.cities.find((c) => c.owner === this.you);
    const unit = v.units.find((u) => u.owner === this.you);
    if (cap) this.renderer.centerOn(cap.x, cap.y);
    else if (unit) this.renderer.centerOn(unit.x, unit.y);
    else this.renderer.centerOn(Math.floor(v.width / 2), Math.floor(v.height / 2));
  }

  // ------------------------------------------------------------------
  onWS(m) {
    if (m.type === "update" || m.type === "turn") this.scheduleRefresh(m.type === "turn" ? 50 : 250);
    else if (m.type === "event") {
      this.feed.push(m.event);
      if (this.feed.length > 400) this.feed.splice(0, this.feed.length - 400);
      this.maybeToastEvent(m.event);
      if (this._diploModal && this._redrawDiplomacy && ["negotiation", "message", "deal", "war_declared", "peace", "first_contact"].includes(m.event.type)) {
        clearTimeout(this._diploTimer);
        this._diploTimer = setTimeout(() => this._redrawDiplomacy(), 250);
      }
      if (this.sideTab === "events" || this.sideTab === "messages") this.renderSide();
    } else if (m.type === "thought") {
      this.thoughts.push(m);
      if (this.thoughts.length > 200) this.thoughts.splice(0, this.thoughts.length - 200);
      if (this.sideTab === "thoughts") this.renderSide();
    } else if (m.type === "agent") {
      this.agentStatus[m.player] = m.status;
      this.renderTopbar();
    } else if (m.type === "control") {
      this.scheduleRefresh(10);
    }
  }

  maybeToastEvent(ev) {
    const important = ["war_declared", "peace", "city_captured", "eliminated", "victory", "first_contact", "deal", "city_destroyed", "era", "agent_error"];
    if (ev.type === "agent_error") { toast(ev.text, "error", 12000); return; }
    const mine = this.you != null && ev.players && ev.players.includes(this.you);
    if (important.includes(ev.type) || (mine && ["unit_killed", "unit_captured", "negotiation", "message", "camp_cleared", "ruins", "tech",
        "wonder_built", "great_person_born", "golden_age", "natural_wonder", "spy", "un_vote"].includes(ev.type))) {
      toast(ev.text, ev.type === "war_declared" || ev.type === "unit_killed" ? "error" : "info", 5000);
    }
  }

  checkNegotiations() {
    if (!this.view.diplomacy || this.isSpectator) return;
    const waiting = this.view.diplomacy.open_negotiations.filter((n) => n.your_move);
    for (const n of waiting) {
      const key = `${n.id}:${n.history.length}`;
      if (this._shownNeg === key) continue;
      this._shownNeg = key;
      openDiplomacy(this, n.with);
      break;
    }
  }

  // ------------------------------------------------------------------
  async tool(name, args = {}, { quiet = false } = {}) {
    try {
      const r = await api.tool(this.gid, this.token, name, args);
      if (!r.ok) { toast(r.error, "error", 5000); return null; }
      this.scheduleRefresh(30);
      return r.result;
    } catch (e) {
      toast(e.message, "error");
      return null;
    }
  }

  deselect() {
    this.selectedUnit = null; this.unitDetail = null;
    this.selectedCity = null; this.cityDetail = null;
    this.mode = null;
    this.renderPanels();
    this.updateOverlay();
  }

  // alerts live in a tab of the side panel (which is hidden while a city is open)
  showAlerts() {
    if (this.selectedCity != null) this.deselect();
    this.sideTab = "alerts";
    this.collapsed = false;
    this.renderSide();
  }

  async selectUnit(id) {
    this.selectedCity = null; this.cityDetail = null;
    this.selectedUnit = id;
    this.mode = null;
    await this.refreshDetails();
    this.updateOverlay();
  }

  async selectCity(id) {
    this.selectedUnit = null; this.unitDetail = null;
    this.selectedCity = id;
    this.mode = null;
    await this.refreshDetails();
    this.updateOverlay();
  }

  idleUnits() {
    return this.view.units.filter((u) => u.owner === this.you && !u.activity && u.moves > 0);
  }

  // center: true = always, false = only when the unit is off screen
  selectNextIdle(center = true, skipId = null, near = null) {
    if (!this.view || this.you == null) return false;
    const idle = this.idleUnits().filter((u) => u.id !== skipId);
    if (!idle.length) { if (center) toast("No units need orders."); return false; }
    let u;
    if (near) {
      u = idle.reduce((best, x) => (hexDistance(x.x, x.y, near.x, near.y) < hexDistance(best.x, best.y, near.x, near.y) ? x : best));
    } else {
      const idx = idle.findIndex((x) => x.id === this.selectedUnit);
      u = idle[(idx + 1) % idle.length];
    }
    this.selectUnit(u.id);
    if (center || !this.renderer.isOnScreen(u.x, u.y)) this.renderer.centerOn(u.x, u.y);
    return true;
  }

  // wait for any in-flight refresh, then fetch a fresh view
  async syncView() {
    while (this._inflight) await new Promise((r) => setTimeout(r, 25));
    if (this._refreshTimer) { clearTimeout(this._refreshTimer); this._refreshTimer = null; }
    await this.refresh();
  }

  // after a unit got an order that finishes its turn: move on to the nearest unit that still needs orders
  async afterOrder(unitId) {
    const prev = this.view.units.find((u) => u.id === unitId);
    await this.syncView();
    if (this.selectedUnit !== unitId && this.selectedUnit != null) return;   // the player already picked something else
    const now = this.view.units.find((u) => u.id === unitId);
    if (now && !now.activity && now.moves > 0) return;                       // still has something to do (e.g. moves after an attack)
    if (!this.myTurn || !this.selectNextIdle(false, unitId, prev)) {
      const still = this.view.units.find((u) => u.id === unitId);
      if (!still || still.activity || still.moves <= 0) this.deselect();
    }
  }

  onLeftClick(tile, e) {
    const v = this.view;
    if (!v) return;
    // city banners are clickable: the quickest way to open a city that has units standing in it
    const r = this.canvas.getBoundingClientRect();
    const bannerCity = e && this.renderer.cityBannerAt(e.clientX - r.left, e.clientY - r.top);
    if (bannerCity && !this.mode) {
      const c = v.cities.find((x) => x.id === bannerCity);
      if (c && c.owner === this.you) { this.selectCity(c.id); return; }
    }
    if (this.mode === "city_attack" && this.selectedCity != null) {
      this.canvas.style.cursor = "default";
      this.tool("city_attack", { city_id: this.selectedCity, x: tile.x, y: tile.y }).then((r) => { if (r) toast(summarizeCombat(r)); });
      this.mode = null;
      return;
    }
    if (this.mode === "buy_tile" && this.selectedCity != null) {
      this.tool("buy_tile", { city_id: this.selectedCity, x: tile.x, y: tile.y });
      this.mode = null;
      return;
    }
    const myUnits = v.units.filter((u) => u.x === tile.x && u.y === tile.y && u.owner === this.you);
    const myCity = v.cities.find((c) => c.x === tile.x && c.y === tile.y && c.owner === this.you);
    // with a city open, clicking one of its tiles locks/unlocks a citizen there
    const cd = this.cityDetail;
    if (cd && this.selectedCity != null && !myUnits.length && !myCity && this.myTurn) {
      const ct = (cd.tiles || []).find((t) => t.x === tile.x && t.y === tile.y);
      if (ct) {
        const locked = (cd.locked_tiles || []).some(([x, y]) => x === tile.x && y === tile.y);
        this.tool("work_tile", { city_id: cd.id, x: tile.x, y: tile.y, locked: !locked });
        return;
      }
    }
    if (myUnits.length || myCity) {
      // cycle: units still needing orders -> the city -> units that already have orders (fortified garrisons etc.)
      const needs = (u) => !u.activity && u.moves > 0;
      const mil = (u) => u.class !== "civilian";
      const order = [...myUnits.filter((u) => needs(u) && mil(u)), ...myUnits.filter((u) => needs(u) && !mil(u))].map((u) => ["u", u.id]);
      if (myCity) order.push(["c", myCity.id]);
      order.push(...[...myUnits.filter((u) => !needs(u) && mil(u)), ...myUnits.filter((u) => !needs(u) && !mil(u))].map((u) => ["u", u.id]));
      let cur = order.findIndex(([k, id]) => (k === "u" && id === this.selectedUnit) || (k === "c" && id === this.selectedCity));
      const [k, id] = order[(cur + 1) % order.length];
      if (k === "u") this.selectUnit(id); else this.selectCity(id);
      return;
    }
    if (this.selectedUnit != null && this.myTurn) { this.orderUnit(tile); return; }
    this.deselect();
  }

  onRightClick(tile) {
    if (this.selectedUnit != null && this.myTurn) this.orderUnit(tile);
  }

  async orderUnit(tile) {
    const d = this.unitDetail;
    if (!d) return;
    const target = (d.attack_targets || []).find((t) => t.x === tile.x && t.y === tile.y);
    const enemyHere = this.view.units.some((u) => u.x === tile.x && u.y === tile.y && u.owner !== this.you && u.class !== "civilian")
      || this.view.cities.some((c) => c.x === tile.x && c.y === tile.y && c.owner !== this.you);
    if (target || (d.domain === "air" && enemyHere)) {
      const r = await this.tool("attack", { unit_id: d.id, x: tile.x, y: tile.y });
      if (r) { toast(summarizeCombat(r), "info", 5000); this.afterOrder(d.id); }
      return;
    }
    if (tile.x === d.x && tile.y === d.y) return;
    let dest = tile;
    // ordering a unit onto a hostile unit or city it can't attack yet: march it next to the target instead
    const hostileHere = this.view.units.some((u) => u.x === tile.x && u.y === tile.y && u.class !== "civilian" && this.isHostile(u.owner))
      || this.view.cities.some((c) => c.x === tile.x && c.y === tile.y && c.owner !== this.you);
    if (hostileHere && d.class !== "civilian") {
      const options = await Promise.all(neighbors(tile.x, tile.y, this.view.width, this.view.height).map(async ([x, y]) => {
        if (x === d.x && y === d.y) return { x, y, turns: 0, len: 0 };
        try { const p = await api.path(this.gid, this.token, d.id, x, y); return p.path ? { x, y, turns: p.turns, len: p.path.length } : null; }
        catch (e) { return null; }
      }));
      const best = options.filter(Boolean).sort((a, b) => a.turns - b.turns || a.len - b.len)[0];
      if (!best) { toast("No route to that target."); return; }
      if (best.turns === 0) { toast("Already next to it, but it can't be attacked right now."); return; }
      dest = best;
    }
    const r = await this.tool("move_unit", { unit_id: d.id, x: dest.x, y: dest.y });
    if (!r) return;
    if (r.stopped && r.stopped !== "out of moves") toast(`Stopped: ${r.stopped}`);
    else if (!r.arrived && r.turns_remaining) toast(`On its way: arrives in about ${r.turns_remaining} turn(s).`, "info", 2500);
    // a unit that used up its moves is done for the turn: hand over to the next one
    const founder = (d.actions || []).some((a) => a.id === "found_city");
    if (r.moves_left <= 0 && !(founder && r.arrived)) this.afterOrder(d.id);
  }

  onKey(e) {
    if (["INPUT", "TEXTAREA", "SELECT"].includes(document.activeElement.tagName)) return;
    if (document.getElementById("modal-root").children.length) return;
    const d = this.unitDetail;
    const k = e.key.toLowerCase();
    if (k === "escape") this.deselect();
    else if (k === "n" || k === "tab") { e.preventDefault(); this.selectNextIdle(); }
    else if (k === "enter" && e.shiftKey) this.endTurn();
    else if (k === "t") openTechTree(this);
    else if (k === "o") openPolicies(this);
    else if (k === "d") openDiplomacy(this);
    else if (k === "c") this.centerHome();
    else if (k === "y") { this.renderer.showYields = !this.renderer.showYields; this.updateOverlay(); }
    else if (d && this.myTurn) {
      const order = { f: "fortify", s: "sleep", " ": "skip", e: "explore", a: "automate", h: "heal", p: "pillage" }[k];
      if (order) { e.preventDefault(); this.tool("unit_order", { unit_id: d.id, order }).then((r) => r && this.afterOrder(d.id)); }
      else if (k === "b" && (d.actions || []).some((a) => a.id === "found_city" && a.available)) this.foundCity(d);
      else if (k === "r" && (d.build_options || []).some((o) => o.name === "Road")) this.tool("build_improvement", { unit_id: d.id, improvement: "Road" }).then((r) => r && this.afterOrder(d.id));
    }
  }

  async foundCity(d) {
    const r = await this.tool("unit_action", { unit_id: d.id, action: "found_city" });
    if (r) { toast(`Founded ${r.name}! (Rename it from the city panel.)`); this.selectCity(r.city_id); }
  }

  // a unit's special action (see get_unit "actions"); founding and enhancing religions ask for beliefs first
  async unitAction(d, a) {
    if (a.id === "found_city") return this.foundCity(d);
    const args = { unit_id: d.id, action: a.id };
    if (a.id === "found_religion" || a.id === "enhance_religion") {
      const pick = await chooseBeliefs(this, a.id === "enhance_religion");
      if (!pick) return;
      args.beliefs = pick.beliefs;
      if (a.id === "found_religion") args.name = pick.name;
    }
    const r = await this.tool("unit_action", args);
    if (r) { toast(`${a.name}: done`, "info", 2500); this.afterOrder(d.id); }
  }

  async endTurn() {
    if (!this.myTurn) return;
    const idleCities = this.view.cities.filter((c) => c.owner === this.you && !c.puppet && (!c.queue || !c.queue.length));
    const noResearch = this.view.empire && !this.view.empire.researching;
    if (idleCities.length || noResearch) {
      const choice = await new Promise((resolve) => {
        let done = false;
        const pick = (v) => { done = true; m.close(); resolve(v); };
        const m = modal({
          title: "End turn?", narrow: true,
          content: el("p", {}, `${idleCities.length ? `${idleCities.map((c) => c.name).join(", ")} ${idleCities.length === 1 ? "has" : "have"} nothing to build. ` : ""}` +
            `${noResearch ? "No research is selected. " : ""}`),
          footer: [
            el("button", { class: "primary", onclick: () => pick("fix") }, noResearch ? "Choose research" : `Open ${idleCities[0].name}`),
            el("button", { onclick: () => pick("end") }, "End turn anyway")],
          onClose: () => { if (!done) resolve(null); },
        });
      });
      if (choice === "fix") {
        if (noResearch) openTechTree(this);
        else { this.renderer.centerOn(idleCities[0].x, idleCities[0].y); this.selectCity(idleCities[0].id); }
        return;
      }
      if (choice !== "end") return;
    }
    this.deselect();
    await this.tool("end_turn");
  }

  // hostile (at war or barbarian) military units a city can bombard
  bombardTargets(c) {
    const v = this.view;
    return v.units.filter((u) => u.owner !== this.you && u.class !== "civilian" && this.isHostile(u.owner) && hexDistance(u.x, u.y, c.x, c.y) <= 2);
  }

  isHostile(pid) {
    const p = this.view.players.find((q) => q.id === pid);
    return !!p && pid !== this.you && (p.kind === "barbarian" || !!p.at_war);
  }

  // route preview while hovering with a unit selected (the path it would take and how many turns)
  previewPath(tile) {
    clearTimeout(this._pathTimer);
    const d = this.unitDetail;
    if (!tile || !d || this.selectedUnit == null || !this.myTurn || this.mode || (tile.x === d.x && tile.y === d.y)
        || (d.attack_targets || []).some((a) => a.x === tile.x && a.y === tile.y)
        || this.view.units.some((u) => u.x === tile.x && u.y === tile.y && u.class !== "civilian" && this.isHostile(u.owner))
        || this.view.cities.some((c) => c.x === tile.x && c.y === tile.y && c.owner !== this.you)) {
      if (this._pathPreview) { this._pathPreview = null; this.updateOverlay(); }
      return;
    }
    const unitId = d.id;
    this._pathTimer = setTimeout(async () => {
      let r = null;
      try { r = await api.path(this.gid, this.token, unitId, tile.x, tile.y); } catch (e) { r = null; }
      if (!this.hover || this.hover.x !== tile.x || this.hover.y !== tile.y || this.selectedUnit !== unitId) return;
      this._pathPreview = r && r.path ? { x: tile.x, y: tile.y, path: r.path, turns: r.turns, unit: unitId } : { x: tile.x, y: tile.y, path: null, unit: unitId };
      this.updateOverlay();
    }, 90);
  }

  // ------------------------------------------------------------------
  updateOverlay() {
    const ov = {};
    const v = this.view;
    if (!v) return;
    if (this.hover) ov.hover = [this.hover.x, this.hover.y];
    const d = this.unitDetail;
    if (d && this.selectedUnit != null) {
      ov.selected = [d.x, d.y];
      if (this.myTurn) {
        ov.reachable = d.reachable_this_turn || [];
        ov.targets = d.attack_targets || [];
      }
      if (d.goto) ov.path = [[d.x, d.y], [d.goto.x, d.goto.y]];
      const pp = this._pathPreview;
      if (pp && pp.unit === d.id && this.hover && pp.x === this.hover.x && pp.y === this.hover.y && this.myTurn) {
        if (pp.path) { ov.preview = pp.path; ov.previewTurns = pp.turns; } else ov.noPath = [pp.x, pp.y];
      }
      if (d.suggested_sites && this.myTurn) ov.sites = d.suggested_sites;
    }
    const c = this.cityDetail;
    if (c && this.selectedCity != null) {
      ov.selected = [c.x, c.y];
      const locked = new Set((c.locked_tiles || []).map(([x, y]) => `${x},${y}`));
      ov.cityTiles = (c.tiles || []).map((t) => [t.x, t.y, t.worked, locked.has(`${t.x},${t.y}`)]);
      ov.yields = (c.tiles || []).map((t) => ({ x: t.x, y: t.y, yields: t.yields }));
      if (this.mode === "buy_tile") ov.buyable = c.buyable_tiles || [];
      if (this.mode === "city_attack") {
        ov.targets = this.bombardTargets(c).map((u) => ({ x: u.x, y: u.y }));
      }
    }
    this.renderer.setOverlay(ov);
  }

  showTooltip(tile, sx, sy) {
    this._tipPos = [sx, sy];
    const tt = this.tooltip;
    if (!tile || !this.model) { tt.style.display = "none"; return; }
    const t = this.model.tiles[tile.y * this.model.width + tile.x];
    if (!t) { tt.style.display = "none"; return; }
    const R = this.rules;
    const lines = [];
    let terr = t.terrain;
    if (t.hills) terr = "Hills (" + terr + ")";
    const feats = t.features.filter((f) => f !== "Hill");
    if (feats.length) terr += ", " + feats.join(", ");
    if (t.river) terr += ", River";
    if (t.wonder) terr = t.wonder + " (" + terr + ")";
    lines.push(el("div", {}, el("b", {}, terr), el("span", { class: "muted" }, ` (${tile.x},${tile.y})`)));
    if (!this.isSpectator && t.visible !== undefined) {
      const key = `${this.view.turn}:${tile.x},${tile.y}`;
      this._tileCache = this._tileCache || new Map();
      const info = this._tileCache.get(key);
      if (info && info.yields) {
        const y = info.yields;
        const parts = [["food", "🍞"], ["production", "⚒"], ["gold", "●"], ["science", "⚗"], ["culture", "✦"], ["faith", "✝"], ["happiness", "☺"]]
          .filter(([k]) => y[k]).map(([k, icon]) => el("span", { class: { food: "food", production: "prod", gold: "gold", science: "sci", culture: "cul", faith: "faith", happiness: "good" }[k] }, `${icon}${fmt(y[k], 1)} `));
        lines.push(el("div", {}, parts.length ? parts : el("span", { class: "muted" }, "no yields")));
      } else if (!info) {
        clearTimeout(this._tileTimer);
        this._tileTimer = setTimeout(async () => {
          if (this._tileCache.size > 400) this._tileCache.clear();
          this._tileCache.set(key, {});
          const r = await api.tool(this.gid, this.token, "get_tile", { x: tile.x, y: tile.y }).catch(() => null);
          this._tileCache.set(key, r && r.ok ? r.result : { yields: null });
          if (this.hover && this.hover.x === tile.x && this.hover.y === tile.y) this.showTooltip(tile, sx, sy);
        }, 120);
      }
    }
    if (t.resource) {
      const rd = R.resources[t.resource] || {};
      lines.push(el("div", {}, `${t.resource} `, el("span", { class: "muted" }, `(${(rd.resourceType || "").toLowerCase()})`)));
    }
    if (t.improvement && !t.camp && !t.village && t.improvement !== "City center") lines.push(el("div", {}, t.improvement + (t.pillaged ? " (pillaged)" : "")));
    if (t.route) lines.push(el("div", {}, t.route + (t.routePillaged ? " (pillaged)" : "")));
    if (t.owner != null && this.model.players[t.owner]) lines.push(el("div", {}, el("span", { class: "swatch", style: { background: this.model.players[t.owner].color } }), this.model.players[t.owner].name || "Unknown"));
    if (t.camp) lines.push(el("div", { class: "bad" }, "Barbarian encampment"));
    if (t.village) lines.push(el("div", { class: "warn" }, "Ancient ruins"));
    const city = this.view.cities.find((c) => c.x === tile.x && c.y === tile.y);
    if (city) lines.push(el("div", {}, el("b", {}, city.name), ` pop ${city.pop ?? "?"}`, city.hp != null ? ` · HP ${city.hp}/${city.max_hp} · Str ${city.strength}` : ""));
    for (const u of this.view.units.filter((u) => u.x === tile.x && u.y === tile.y)) {
      const p = this.model.players[u.owner];
      lines.push(el("div", {}, el("span", { class: "swatch", style: { background: p ? p.color : "#999" } }),
        `${u.name} (${p && p.name ? p.name : "?"}) HP ${u.hp}`));
    }
    if (!t.visible) lines.push(el("div", { class: "muted" }, "Not currently visible"));
    if (this.unitDetail && this.myTurn) {
      const tg = (this.unitDetail.attack_targets || []).find((a) => a.x === tile.x && a.y === tile.y);
      if (tg) {
        const avg = (v) => Array.isArray(v) ? Math.round((v[0] + v[1]) / 2) : (v || 0);
        const dealt = avg(tg.damage_to_defender);
        const theirHp = tg.defender_hp;
        const taken = avg(tg.damage_to_attacker), ourHp = this.unitDetail.hp;
        const outcome = theirHp != null && dealt >= theirHp ? (tg.target === "city" ? "city falls to 0 HP" : "destroys it")
          : theirHp != null ? `leaves it at ${theirHp - dealt} HP` : "";
        const risk = taken >= ourHp ? " — your unit dies!" : taken ? ` — you drop to ${ourHp - taken} HP` : "";
        const rng = (v) => Array.isArray(v) ? `${v[0]}–${v[1]}` : v;
        lines.push(el("div", { class: taken >= ourHp ? "bad" : "warn" }, `Attack: deal ${rng(tg.damage_to_defender)} / take ${rng(tg.damage_to_attacker)}`),
          el("div", { class: "muted" }, `${outcome}${risk}`));
      } else if (this.myTurn && this.unitDetail.class !== "civilian" && this.unitDetail.moves > 0
                 && this.view.units.some((u) => u.x === tile.x && u.y === tile.y && u.class === "civilian" && this.isHostile(u.owner))
                 && !this.view.units.some((u) => u.x === tile.x && u.y === tile.y && u.class !== "civilian")) {
        lines.push(el("div", { class: "warn" }, "Move here to capture it"));
      }
    }
    clear(tt).append(...lines);
    tt.style.display = "block";
    const W = this.main.clientWidth;
    tt.style.left = Math.min(sx + 16, W - 290) + "px";
    tt.style.top = sy + 16 + "px";
  }

  // ------------------------------------------------------------------
  renderTopbar() {
    const v = this.view;
    const tb = clear(this.topbar);
    if (!v) return;
    tb.appendChild(el("button", { class: "small", onclick: () => { location.hash = "#/"; } }, "☰ Lobby"));
    const bench = v.session && v.session.benchmark;
    if (bench) {
      tb.append(el("button", { class: "small", onclick: () => { location.hash = "#/benchmarks"; } }, "← Benchmarks"),
        el("span", { class: "pill live", title: `Run: ${bench.run_name} · server: ${bench.server}` }, `Benchmark: ${bench.model} · ${bench.scenario}`));
    }
    if (!bench && v.session) {
      // games you start yourself keep running in a server's restricted hours: just say so
      const late = (v.session.seats || []).filter((st) => st.type === "llm" && st.llm_info && st.llm_info.restricted_until);
      for (const st of late) tb.append(el("span", { class: "pill quiet", title: "This server is in its restricted hours (Servers page). Benchmarks and probes pause there; this game does not." },
        `🌙 ${st.llm_info.server} restricted until ${st.llm_info.restricted_until}`));
    }
    const cur = v.players.find((p) => p.id === v.current_player);
    if (v.empire) {
      const e = v.empire;
      const pt = e.per_turn || {};
      tb.append(
        el("span", { class: "stat civ", title: e.nation }, el("span", { class: "swatch", style: { background: e.color } }), e.name),
        el("span", { class: "stat", title: v.year || "" }, `Turn ${v.turn_limit ? Math.min(v.turn, v.turn_limit) + "/" + v.turn_limit : v.turn}`),
        el("span", { class: "stat gold", title: gptTitle(e) }, `● ${e.gold} `,
          el("span", { class: (pt.gold || 0) < 0 ? "bad" : "" }, `(${signed(pt.gold || 0)})`)),
        el("span", { class: "stat sci", style: { cursor: "pointer" }, onclick: () => openTechTree(this), title: "Tech tree (T)" },
          `⚗ +${fmt(pt.science || 0)} ${e.researching ? "→ " + e.researching + (e.research_turns ? ` (${e.research_turns})` : "") : "→ choose research!"}`),
        el("span", { class: "stat cul", style: { cursor: "pointer" }, onclick: () => openPolicies(this), title: "Social policies (O)" }, `✦ ${e.culture} (+${fmt(pt.culture || 0)})`),
        el("span", { class: "stat faith", style: { cursor: "pointer" }, onclick: () => openReligion(this), title: "Religion" }, `✝ ${e.faith} (+${fmt(pt.faith || 0)})`),
        el("span", { class: `stat ${e.happiness.total < 0 ? "bad" : "good"}`, title: happyTitle(e.happiness) }, `☺ ${e.happiness.total}`),
        ...(e.golden_age && e.golden_age.turns_left ? [el("span", { class: "stat gold" }, `★ Golden Age ${e.golden_age.turns_left}`)] : []),
        el("span", { class: "stat cul" }, e.era),
        ...Object.entries(e.strategic_resources || {}).filter(([, r]) => r.sources || r.used || r.imported)
          .map(([k, r]) => el("span", { class: `stat ${r.available < 0 ? "bad" : ""}`, title: `${r.sources} sources, ${r.used} used, +${r.imported} imported, -${r.exported} exported` },
            `${k} ${r.available}`)),
      );
    } else {
      tb.append(el("span", { class: "stat civ" }, this.isSpectator ? "Spectating" : ""), el("span", { class: "stat" }, `Turn ${v.turn_limit ? Math.min(v.turn, v.turn_limit) + "/" + v.turn_limit : v.turn}`));
    }
    const alerts = v.alerts || [];
    if (alerts.length && !this.isSpectator) {
      tb.appendChild(el("button", { class: "small alert-btn", title: "Problems that need attention", onclick: () => this.showAlerts() },
        `⚠ ${alerts.length}`));
    }
    tb.appendChild(el("span", { class: "spacer" }));
    if (!this.isSpectator) {
      tb.append(
        el("button", { class: "small", onclick: () => openEmpire(this) }, "Empire"),
        el("button", { class: "small", onclick: () => openTechTree(this) }, "Tech"),
        el("button", { class: "small", onclick: () => openPolicies(this) }, "Policies"),
        el("button", { class: "small", onclick: () => openReligion(this) }, "Religion"),
        el("button", { class: "small", onclick: () => openGreatPeople(this) }, "Great people"),
        el("button", { class: "small", onclick: () => openCityStates(this) }, "City-states"),
        el("button", { class: "small", onclick: () => openDiplomacy(this) }, "Diplomacy"),
        el("button", { class: "small", onclick: () => openVictory(this) }, "Victory"),
        el("button", { class: "small", onclick: () => openNotes(this) }, "Notes"));
    }
    if (this.isSpectator || (v.session && v.session.seats.some((s) => s.type !== "human"))) {
      const sess = v.session;
      tb.append(el("button", { class: "small", onclick: () => api.control(this.gid, { paused: !sess.paused }) }, sess.paused ? "▶ Resume AIs" : "⏸ Pause AIs"));
      if (this.isSpectator) {
        const delay = el("select", { onchange: (ev) => api.control(this.gid, { ai_delay: +ev.target.value }) },
          ...[0, 0.5, 1, 2, 5].map((d) => el("option", { value: d, selected: sess.ai_delay === d }, `delay ${d}s`)));
        const viewAs = el("select", { onchange: (ev) => {
          location.hash = `#/game/${this.gid}/${encodeURIComponent(this.token)}` + (ev.target.value !== "" ? `/${ev.target.value}` : "");
        } }, el("option", { value: "", selected: this.asPlayer == null }, "God view"),
          ...sess.players.filter((p) => p.kind === "major").map((p) => el("option", { value: p.id, selected: this.asPlayer === p.id }, `View as ${p.name}`)));
        tb.append(delay, viewAs);
        if (v.phase !== "playing" || sess.god_view_allowed) tb.append(el("button", { class: "small", onclick: () => { location.hash = `#/replay/${this.gid}/${encodeURIComponent(this.token)}`; } }, "Recap"));
      }
    }
    if (v.session && v.session.seats.some((s) => s.type !== "human")) {
      tb.append(el("button", { class: "small", title: "AI performance metrics", onclick: () => openMetrics(this.gid) }, "📊 AI stats"));
    }
    tb.append(el("button", { class: "small", onclick: async () => { const r = await api.save(this.gid, null); toast(`Saved: ${r.saved}`); } }, "Save"));
    tb.append(el("button", { class: "small", onclick: () => openHelp(this) }, "?"));
    const box = clear(this.turnBox);
    if (v.phase !== "playing") {
      const w = v.players.find((p) => p.id === v.winner);
      box.append(el("span", { class: "pill" }, `Game over: ${w ? w.name : "no winner"} (${v.victory || ""})`),
        el("button", { class: "primary", onclick: () => { location.hash = `#/replay/${this.gid}/${encodeURIComponent(this.token)}`; } }, "Watch recap"));
    } else if (this.myTurn) {
      const needs = v.units.filter((u) => u.owner === this.you && !u.activity && u.moves > 0).length;
      // things that usually need a decision this turn get their own button right next to End Turn
      if (v.empire && !v.empire.researching) {
        box.append(el("button", { class: "primary attention", onclick: () => openTechTree(this) }, "⚗ Choose research"));
      }
      const idleCities = v.cities.filter((c) => c.owner === this.you && !c.puppet && (!c.queue || !c.queue.length));
      if (idleCities.length) {
        const next = idleCities.find((c) => c.id !== this.selectedCity) || idleCities[0];
        box.append(el("button", { class: "primary attention", title: idleCities.map((c) => c.name).join(", "),
          onclick: () => { this.renderer.centerOn(next.x, next.y); this.selectCity(next.id); } },
          idleCities.length === 1 ? `⚒ ${next.name} needs production` : `⚒ ${idleCities.length} cities need production`));
      }
      // cities with an enemy in bombard range that haven't fired yet (view.cities has attacked_this_turn for own cities)
      const gunners = v.cities.filter((c) => c.owner === this.you && this.bombardTargets(c).length);
      if (gunners.length) {
        const c = gunners.find((x) => x.id !== this.selectedCity) || gunners[0];
        box.append(el("button", { class: "primary attention", title: gunners.map((x) => x.name).join(", "), onclick: async () => {
          this.renderer.centerOn(c.x, c.y);
          await this.selectCity(c.id);
          this.mode = "city_attack"; this.updateOverlay();
          toast(`${c.name}: click a red-outlined enemy to bombard it.`);
        } }, gunners.length === 1 ? `🎯 ${c.name} can bombard` : `🎯 ${gunners.length} cities can bombard`));
      }
      const promos = v.units.filter((u) => u.owner === this.you && u.promotion_ready);
      if (promos.length) {
        const u = promos.find((x) => x.id !== this.selectedUnit) || promos[0];
        box.append(el("button", { class: "primary attention", title: "Pick a promotion in the unit panel",
          onclick: () => { this.renderer.centerOn(u.x, u.y); this.selectUnit(u.id); } },
          promos.length === 1 ? `▲ Promote ${u.name}` : `▲ ${promos.length} promotions`));
      }
      box.append(el("button", { disabled: !needs, onclick: () => this.selectNextIdle() }, `Next unit (${needs})`),
        el("button", { class: `end-turn ${needs === 0 ? "ready" : ""}`, onclick: () => this.endTurn() }, "End Turn"));
    } else {
      const st = this.agentStatus[v.current_player];
      box.append(el("span", { class: "pill" }, `Turn ${v.turn} · waiting for ${cur ? cur.name || "?" : "?"}${st === "thinking" ? " (thinking…)" : ""}`));
    }
  }

  renderBanner() {
    const v = this.view;
    const b = this.banner;
    if (v.phase !== "playing") {
      const w = v.players.find((p) => p.id === v.winner);
      b.textContent = `🏆 ${w ? w.name : "No one"} wins — ${v.victory || "game over"}`;
      b.style.display = "block";
    } else if (v.session && v.session.paused) {
      b.textContent = "AI players are paused";
      b.style.display = "block";
    } else b.style.display = "none";
  }

  renderPanels() {
    // re-rendering replaces the panel contents: keep the scroll position while the same unit/city stays selected
    const keepScroll = (panel, key, render) => {
      const top = panel._key === key ? panel.scrollTop : 0;
      render();
      panel._key = key;
      panel.scrollTop = top;
    };
    if (this.unitDetail && this.selectedUnit != null) {
      this.unitPanel.style.display = "block";
      keepScroll(this.unitPanel, `u${this.selectedUnit}`, () => renderUnitPanel(this, clear(this.unitPanel), this.unitDetail));
    } else this.unitPanel.style.display = "none";
    if (this.cityDetail && this.selectedCity != null) {
      this.cityPanel.style.display = "block";
      this.sidePanel.style.display = "none";
      keepScroll(this.cityPanel, `c${this.selectedCity}:${this._cityTab}`, () => renderCityPanel(this, clear(this.cityPanel), this.cityDetail));
    } else {
      this.cityPanel.style.display = "none";
      this.sidePanel.style.display = "flex";
    }
  }

  renderSide() {
    const sp = clear(this.sidePanel);
    const alerts = (this.view && this.view.alerts) || [];
    const tabs = [["events", "Events"], ["messages", "Messages"], ["scores", "Civs"]];
    if (alerts.length && !this.isSpectator) tabs.unshift(["alerts", `⚠ ${alerts.length}`]);
    else if (this.sideTab === "alerts") this.sideTab = "events";
    if (this.isSpectator) tabs.splice(2, 0, ["thoughts", "AI thoughts"]);
    const tabBar = el("div", { class: "tabs" }, ...tabs.map(([k, label]) => el("button", {
      class: this.sideTab === k ? "active" : "", onclick: () => { this.sideTab = k; this.collapsed = false; this.renderSide(); } }, label)),
      el("button", { style: { flex: "0 0 30px" }, onclick: () => { this.collapsed = !this.collapsed; this.renderSide(); } }, this.collapsed ? "▾" : "▴"));
    const body = el("div", { class: "body" });
    sp.className = `overlay-panel side-panel ${this.collapsed ? "collapsed" : ""}`;
    sp.append(tabBar, body);
    const v = this.view;
    if (!v) return;
    const pname = (id) => { const p = v.players.find((q) => q.id === id); return p ? p.name || "?" : "?"; };
    const pcolor = (id) => { const p = v.players.find((q) => q.id === id); return p ? p.color : "#999"; };
    if (this.sideTab === "alerts") {
      const icon = { gold: "●", happiness: "☹", threat: "⚔", bombard: "🎯", starving: "🍞", civilian_danger: "⚠", research: "⚗",
                     free_tech: "⚗", policy: "✦", great_person: "★", pantheon: "✝", promotion: "▲", spy: "🕵", un_vote: "🗳",
                     negotiation: "⚖", conquest: "🏛", idle_city: "⚒", golden_age: "★" };
      for (const a of alerts) {
        body.appendChild(el("div", { class: "event alert-item clickable", onclick: () => {
          if (a.x == null) {
            const open = { gold: openEmpire, happiness: openEmpire, research: openTechTree, free_tech: openTechTree, policy: openPolicies,
                           great_person: openGreatPeople, pantheon: openReligion, spy: openGreatPeople, un_vote: openDiplomacy,
                           negotiation: openDiplomacy }[a.type];
            if (open) open(this);
            return;
          }
          this.renderer.centerOn(a.x, a.y);
          if (a.city != null) this.selectCity(a.city);
          else if (a.unit != null) this.selectUnit(a.unit);
        } }, el("span", { class: "alert-icon" }, icon[a.type] || "⚠"), el("span", {}, a.text)));
      }
    } else if (this.sideTab === "events") {
      const hidden = ["turn_start", "turn_end", "message", "thought"];
      if (this.isSpectator) hidden.push("research_needed", "city_idle", "improvement_built", "promotion_ready", "unit_built");
      const evs = this.feed.filter((e) => !hidden.includes(e.type)).slice(-120).reverse();
      if (!evs.length) body.appendChild(el("div", { class: "muted" }, "No events yet."));
      for (const e of evs) {
        body.appendChild(el("div", { class: `event ${e.x != null ? "clickable" : ""}`, onclick: () => { if (e.x != null) { this.renderer.centerOn(e.x, e.y); this.flash(e.x, e.y); } } },
          el("span", { class: "t" }, `T${e.turn}`), e.text));
      }
    } else if (this.sideTab === "messages") {
      const msgs = v.diplomacy ? v.diplomacy.messages : (v.messages || []).map((m) => ({ ...m, from_name: pname(m.from), to: m.to.map(pname) }));
      if (!msgs.length) body.appendChild(el("div", { class: "muted" }, "No diplomatic messages yet."));
      for (const m of msgs.slice().reverse()) {
        body.appendChild(el("div", { class: "event" }, el("span", { class: "t" }, `T${m.turn}`),
          el("span", { class: "swatch", style: { background: pcolor(m.from) } }), el("b", {}, m.from_name), " → ", (m.to || []).join(", "),
          el("div", { style: { whiteSpace: "pre-wrap" } }, m.text)));
      }
      if (!this.isSpectator) body.appendChild(el("button", { class: "small", onclick: () => openDiplomacy(this) }, "Open diplomacy"));
    } else if (this.sideTab === "thoughts") {
      const ths = this.thoughts.slice(-60).reverse();
      if (!ths.length) body.appendChild(el("div", { class: "muted" }, "AI reasoning appears here as models play."));
      for (const t of ths) {
        body.appendChild(el("div", { class: "thought", style: { borderLeftColor: pcolor(t.player) } },
          el("div", { class: "muted" }, `T${t.turn} · ${pname(t.player)}${t.kind ? " · " + t.kind : ""}`), t.text));
      }
    } else if (this.sideTab === "scores") {
      for (const p of v.players.filter((p) => p.kind === "major")) {
        const emp = v.empires ? v.empires[p.id] : null;
        const seat = v.session ? v.session.seats[p.id] : null;
        body.appendChild(el("div", { class: "event" },
          el("div", {}, el("span", { class: "swatch", style: { background: p.color } }), el("b", {}, p.name || "Unknown civilization"),
            p.alive === false ? el("span", { class: "bad" }, " (eliminated)") : null,
            p.at_war ? el("span", { class: "pill war", style: { marginLeft: "4px" } }, "war") : null,
            v.current_player === p.id ? el("span", { class: "pill", style: { marginLeft: "4px" } }, "to move") : null),
          el("div", { class: "muted" }, [
            p.score != null ? `score ${p.score}` : null, p.cities != null ? `${p.cities} cities` : null, p.era || null,
            seat ? (seat.type === "llm" ? `LLM ${seat.llm.model || ""}` : seat.type) : null,
          ].filter(Boolean).join(" · ")),
          emp ? el("div", { class: "muted" }, `● ${emp.gold} (${signed((emp.per_turn || {}).gold || 0)}) · ⚗ ${fmt((emp.per_turn || {}).science || 0)} · ✦ ${fmt((emp.per_turn || {}).culture || 0)} · ☺ ${emp.happiness.total} · techs ${emp.techs_known}`) : null));
      }
    }
  }

  flash(x, y) {
    const ov = Object.assign({}, this.renderer.overlay, { flash: [x, y] });
    this.renderer.setOverlay(ov);
    setTimeout(() => this.updateOverlay(), 900);
  }

  drawMinimap() {
    const v = this.view, m = this.model;
    if (!v || !m) return;
    const c = this.minimap;
    const ctx = c.getContext("2d");
    const sx = c.width / v.width, sy = c.height / v.height;
    ctx.fillStyle = "#000"; ctx.fillRect(0, 0, c.width, c.height);
    for (let i = 0; i < m.tiles.length; i++) {
      const t = m.tiles[i];
      if (!t) continue;
      const x = i % v.width, y = Math.floor(i / v.width);
      ctx.fillStyle = t.owner != null && m.players[t.owner] ? m.players[t.owner].color : (TERRAIN_COLORS[t.terrain] || "#444");
      ctx.globalAlpha = t.visible ? 1 : 0.55;
      ctx.fillRect((x + (y & 1) * 0.5) * sx, y * sy, sx + 0.5, sy + 0.5);
    }
    ctx.globalAlpha = 1;
    for (const city of v.cities) {
      ctx.fillStyle = "#fff";
      ctx.fillRect((city.x + (city.y & 1) * 0.5) * sx - 1.5, city.y * sy - 1.5, 3.5, 3.5);
    }
    // viewport rectangle
    const r = this.canvas.getBoundingClientRect();
    const size = this.renderer.size;
    const vx = this.renderer.cam.x / (size * Math.sqrt(3)), vy = this.renderer.cam.y / (size * 1.5);
    const vw = r.width / (size * Math.sqrt(3)), vh = r.height / (size * 1.5);
    ctx.strokeStyle = "#ffe066"; ctx.lineWidth = 1;
    ctx.strokeRect(vx * sx, vy * sy, vw * sx, vh * sy);
  }
}

export function summarizeCombat(r) {
  if (!r) return "";
  if (r.captured_city) return `Captured ${r.captured_city}!`;
  const parts = [];
  if (r.intercepted) parts.push(`intercepted (${r.intercepted} damage)`);
  if (r.withdrew) parts.push(`${r.defender} withdrew`);
  if (r.defender && r.damage_to_defender != null) parts.push(`${r.defender} took ${r.damage_to_defender}${r.city_hp != null ? ` (HP ${r.city_hp})` : ""}${r.defender_killed ? " and was destroyed" : ""}`);
  if (r.captured) parts.push(`captured ${r.captured}`);
  if (r.damage_to_attacker) parts.push(`we took ${r.damage_to_attacker}`);
  if (r.attacker_killed) parts.push("our unit was lost");
  return "Attack: " + parts.join(", ");
}

function gptTitle(e) {
  return Object.entries(e.per_turn_breakdown || {}).filter(([, v]) => v.gold).map(([k, v]) => `${k} ${signed(v.gold)}`).join(", ");
}

function happyTitle(h) {
  return Object.entries(h.breakdown || {}).map(([k, v]) => `${k} ${signed(v)}`).join(", ") + ` (luxuries: ${h.luxury_types.join(", ") || "none"}) — ${h.status}`;
}

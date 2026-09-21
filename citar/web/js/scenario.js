// Scenarios: a list page (#/scenarios: create, edit, launch, delete) and the scenario editor (#/scenario/<editor id>).
// The editor works on a game held by the server; every change is a scenario operation (see /api/scenario-ops), so
// what you click here can also be written into probe files as "setup".
import { api } from "./api.js";
import { el, clear, toast, modal, confirmBox } from "./util.js";
import { MapRenderer, modelFromView } from "./render.js";
import { pageHeader } from "./nav.js";
import { llmForm, defaultLLM } from "./lobby.js";

const SEAT_TYPES = [["human", "Human"], ["llm", "LLM (server-run)"], ["bot", "Scripted bot"], ["mcp", "MCP client"],
  ["script", "Probe script (scripted counterparty)"]];

// ---------------------------------------------------------------------------- list page
export async function renderScenarios(root, rules) {
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("scenarios"));
  const listCard = el("div", { class: "card" });
  const newCard = el("div", { class: "card" });
  page.append(listCard, newCard);
  const refresh = async () => {
    let list = [];
    try { list = await api.scenarios(); } catch (e) { toast(e.message, "error"); }
    drawList(listCard, list, rules, refresh);
  };
  await refresh();
  drawNew(newCard, rules);
  return {};
}

function drawList(card, list, rules, refresh) {
  clear(card);
  card.append(el("h2", {}, "Scenarios"),
    el("p", { class: "muted" }, "A scenario is a prepared game — map, eras, cities, units, diplomacy and seats — that can be played " +
      "or probed from the same starting point again and again."));
  if (!list.length) { card.appendChild(el("p", { class: "muted" }, "No scenarios yet. Create one below.")); return; }
  const table = el("table", { class: "list" }, el("tr", {}, el("th", {}, "Scenario"), el("th", {}, "Map"), el("th", {}, "Civilizations"),
    el("th", {}, "Seats"), el("th", {}, "")));
  for (const s of list) {
    table.appendChild(el("tr", {},
      el("td", {}, el("b", {}, s.name), el("div", { class: "muted small" }, s.description || s.id)),
      el("td", {}, `${s.width}×${s.height}, turn ${s.turn}`),
      el("td", { class: "small" }, s.players.map((p) => p.name).join(", "), s.city_states ? el("div", { class: "muted" }, `${s.city_states} city-states`) : null),
      el("td", { class: "small" }, s.seats.map((x) => x.type).join(" / ")),
      el("td", {}, el("div", { class: "row" },
        el("button", { class: "small primary", onclick: () => launchDialog(s, rules) }, "Play…"),
        el("button", { class: "small", onclick: async () => {
          try { const r = await api.editorOpen({ source: "scenario", scenario: s.id }); location.hash = `#/scenario/${r.editor_id}`; }
          catch (e) { toast(e.message, "error"); }
        } }, "Edit"),
        el("button", { class: "small", onclick: () => { location.hash = `#/probes?scenario=${encodeURIComponent(s.id)}`; } }, "Probe…"),
        el("button", { class: "small", onclick: async () => {
          if (!(await confirmBox("Delete scenario", `Delete "${s.name}"? Games already started from it are kept.`))) return;
          try { await api.deleteScenario(s.id); refresh(); } catch (e) { toast(e.message, "error"); }
        } }, "Delete")))));
  }
  card.appendChild(table);
}

function launchDialog(s, rules) {
  const seats = s.seats.map((x) => ({ ...x, type: x.type === "script" ? "human" : x.type }));
  const body = el("div");
  const draw = () => {
    clear(body);
    seats.forEach((seat, i) => {
      const type = el("select", { onchange: (e) => { seat.type = e.target.value; if (seat.type === "llm" && !seat.llm) seat.llm = defaultLLM(); draw(); } },
        ...SEAT_TYPES.filter(([k]) => k !== "script").map(([k, l]) => el("option", { value: k, selected: seat.type === k }, l)));
      const row = el("div", { class: "seat-row" }, el("b", {}, `P${i}`), el("span", {}, s.players[i] ? s.players[i].name : ""), type);
      if (seat.type === "llm") { seat.llm = seat.llm || defaultLLM(); row.appendChild(llmForm(seat.llm, draw)); }
      body.appendChild(row);
    });
  };
  draw();
  const go = el("button", { class: "primary", onclick: async () => {
    go.disabled = true;
    try {
      const info = await api.launchScenario(s.id, { seats });
      dlg.close();
      const human = info.seats.find((x) => x.type === "human");
      location.hash = human ? `#/game/${info.id}/${encodeURIComponent(human.token)}` : `#/game/${info.id}/${encodeURIComponent(info.spectator_token)}`;
    } catch (e) { toast(e.message, "error"); go.disabled = false; }
  } }, "Start game");
  const dlg = modal({ title: `Play "${s.name}"`, content: body, footer: go });
}

function drawNew(card, rules) {
  clear(card);
  card.append(el("h2", {}, "New scenario"));
  const field = (label, input) => el("div", { class: "field" }, el("label", {}, label), input);
  const sel = (opts, value) => el("select", {}, ...opts.map(([v, t]) => el("option", { value: v, selected: v === value }, t)));
  const f = {
    source: sel([["map", "A saved map"], ["generate", "A generated map"], ["game", "A running game (its current state)"], ["save", "A saved game"]], "map"),
    map: sel([], ""), game: sel([], ""), save: sel([], ""),
    size: sel(Object.entries(rules.map_sizes).map(([k, v]) => [k, `${v.name} (${v.width}×${v.height})`]), "duel"),
    type: sel(Object.entries(rules.map_types).map(([k, v]) => [k, v.name]), "continents"),
    speed: sel(Object.keys(rules.speeds).map((k) => [k, k]), rules.benchmark_speed || "Quick"),
    difficulty: sel(rules.difficulty_list.map((k) => [k, k]), rules.default_difficulty),
    era: sel(rules.era_list.map((k) => [k, k]), "Ancient era"),
    cs: el("input", { type: "number", min: 0, max: 40, placeholder: "map default" }),
    barbs: sel(Object.entries(rules.barbarian_levels).map(([k, v]) => [k, v]), "normal"),
  };
  f.era.title = "UnCiv starting era: civilizations get that era's starting units, gold and techs of earlier eras. You can also grant eras later in the editor.";
  const players = [{ type: "script", nation: "" }, { type: "llm", nation: "" }];
  const nations = [["", "Random"], ["BenchmarkCiv", "BenchmarkCiv"], ...rules.major_nations.filter((n) => n !== "BenchmarkCiv").sort().map((n) => [n, n])];
  const pbox = el("div");
  const drawPlayers = () => {
    clear(pbox);
    players.forEach((p, i) => {
      const t = sel(SEAT_TYPES, p.type); t.onchange = () => { p.type = t.value; };
      const n = sel(nations, p.nation); n.onchange = () => { p.nation = n.value; };
      const d = sel([["", "Game difficulty"], ...rules.difficulty_list.map((k) => [k, k])], p.difficulty || ""); d.onchange = () => { p.difficulty = d.value; };
      pbox.appendChild(el("div", { class: "seat-row" }, el("b", {}, `P${i}`), t, n, d,
        el("button", { class: "small", disabled: players.length <= 1, onclick: () => { players.splice(i, 1); drawPlayers(); } }, "✕")));
    });
    pbox.appendChild(el("button", { class: "small", disabled: players.length >= (rules.max_players || 24), onclick: () => { players.push({ type: "bot", nation: "" }); drawPlayers(); } }, "+ Add civilization"));
  };
  drawPlayers();
  const newOnly = el("div", {},
    el("div", { class: "grid2" }, field("Speed", f.speed), field("Difficulty", f.difficulty), field("Starting era", f.era),
      field("City-states", f.cs), field("Barbarians", f.barbs)),
    el("h3", {}, "Civilizations"), pbox);
  const srcBox = el("div", { class: "grid2" });
  const drawSource = () => {
    clear(srcBox);
    const v = f.source.value;
    srcBox.append(field("Start from", f.source));
    if (v === "map") srcBox.append(field("Map", f.map));
    if (v === "generate") srcBox.append(field("Map size", f.size), field("Map type", f.type));
    if (v === "game") srcBox.append(field("Game", f.game));
    if (v === "save") srcBox.append(field("Save", f.save));
    newOnly.style.display = ["map", "generate"].includes(v) ? "" : "none";
  };
  f.source.onchange = drawSource;
  Promise.all([api.maps(), api.games(), api.saves()]).then(([maps, games, saves]) => {
    maps.forEach((m) => f.map.appendChild(el("option", { value: m.id }, `${m.name} (${m.width}×${m.height}, ${m.starts} starts)`)));
    if (!maps.length) f.map.appendChild(el("option", { value: "" }, "No saved maps — make one in the map editor"));
    games.forEach((g) => f.game.appendChild(el("option", { value: g.id }, `${g.name} (turn ${g.turn})`)));
    for (const s of saves.flatMap ? saves.flatMap((x) => x.saves ? x.saves.map((y) => ({ ...y, game_name: x.game_name })) : [x]) : []) {
      if (s.path) f.save.appendChild(el("option", { value: s.path }, `${s.game_name || ""} ${s.name || s.path}`));
    }
  }).catch(() => {});
  drawSource();
  const go = el("button", { class: "primary", onclick: async () => {
    go.disabled = true;
    go.textContent = "Creating…";
    const v = f.source.value;
    const body = { source: v, map: f.map.value || null, game_id: f.game.value || null, save: f.save.value || null,
      config: { map_size: f.size.value, map_type: f.type.value, speed: f.speed.value, difficulty: f.difficulty.value,
        starting_era: f.era.value, barbarians: f.barbs.value, city_states: f.cs.value === "" ? null : +f.cs.value },
      players: players.map((p) => ({ type: p.type, nation: p.nation || null, difficulty: p.difficulty || null })) };
    try {
      const r = await api.editorOpen(body);
      location.hash = `#/scenario/${r.editor_id}`;
    } catch (e) { toast(e.message, "error"); go.disabled = false; go.textContent = "Create and edit"; }
  } }, "Create and edit");
  card.append(srcBox, newOnly, el("div", { style: { marginTop: "12px" } }, go));
}

// ---------------------------------------------------------------------------- editor
export async function renderScenarioEditor(root, rules, eid) {
  const ed = new ScenarioEditor(root, rules, eid);
  await ed.load();
  return ed;
}

class ScenarioEditor {
  constructor(root, rules, eid) {
    this.rules = rules;
    this.eid = eid;
    this.tab = "place";
    this.tool = { kind: "city", player: 0, unit: "Rifleman", count: 1, pop: 3 };
    this.selectedCity = null;
    this.page = el("div", { class: "editor" });
    root.appendChild(this.page);
    this.side = el("div", { class: "editor-side" });
    this.canvas = el("canvas", { class: "editor-canvas" });
    this.status = el("div", { class: "editor-status muted small" });
    this.page.append(el("div", { class: "editor-head" }, pageHeader("scenarios")), this.side,
      el("div", { class: "editor-main" }, this.canvas), this.status);
    this.renderer = new MapRenderer(this.canvas, rules);
    this.renderer.showGrid = true;
    this.bindInput();
    this.first = true;
  }

  destroy() {}

  async load() {
    try { this.apply(await api.editorGet(this.eid)); }
    catch (e) { clear(this.side); this.side.append(el("p", { class: "bad" }, e.message), el("a", { href: "#/scenarios" }, "Back to scenarios")); }
  }

  apply(data) {
    this.data = data;
    const model = modelFromView(data.view);
    this.renderer.setModel(model);
    if (this.first) {
      this.first = false;
      this.renderer.cam.zoom = Math.max(0.25, Math.min(1, 1100 / (data.view.width * 30 * 1.8)));
      const c = data.overview.cities[0];
      if (c) this.renderer.centerOn(c.x, c.y);
      else this.renderer.centerOn(Math.floor(data.view.width / 2), Math.floor(data.view.height / 2));
    }
    if (this.selectedCity != null && !data.overview.cities.some((c) => c.id === this.selectedCity)) this.selectedCity = null;
    this.drawSide();
  }

  async ops(list, quiet = false) {
    try {
      const r = await api.editorOps(this.eid, list);
      this.apply(r);
      if (!quiet) toast("Done", "success", 1200);
      return r;
    } catch (e) { toast(e.message, "error", 6000); return null; }
  }

  majors() { return this.data.overview.players.filter((p) => p.kind === "major"); }

  bindInput() {
    const cv = this.canvas;
    let pan = null, moved = false;
    cv.addEventListener("contextmenu", (e) => e.preventDefault());
    cv.addEventListener("mousedown", (e) => { pan = [e.clientX, e.clientY]; moved = false; });
    window.addEventListener("mouseup", (e) => {
      if (!pan) return;
      const wasPan = moved;
      pan = null;
      if (wasPan || e.target !== cv) return;
      const r = cv.getBoundingClientRect();
      const t = this.renderer.screenToTile(e.clientX - r.left, e.clientY - r.top);
      if (t && e.button === 0) this.click(t.x, t.y);
    });
    cv.addEventListener("mousemove", (e) => {
      const r = cv.getBoundingClientRect();
      if (pan && (Math.abs(e.clientX - pan[0]) + Math.abs(e.clientY - pan[1]) > 4 || moved)) {
        moved = true;
        this.renderer.pan(e.clientX - pan[0], e.clientY - pan[1]);
        pan = [e.clientX, e.clientY];
        return;
      }
      const t = this.renderer.screenToTile(e.clientX - r.left, e.clientY - r.top);
      this.renderer.overlay.hover = t ? [t.x, t.y] : null;
      this.renderer.invalidate();
      this.status.textContent = t ? this.describe(t.x, t.y) : "";
    });
    cv.addEventListener("wheel", (e) => {
      e.preventDefault();
      const r = cv.getBoundingClientRect();
      this.renderer.zoomAt(e.deltaY < 0 ? 1.15 : 1 / 1.15, e.clientX - r.left, e.clientY - r.top);
    }, { passive: false });
  }

  describe(x, y) {
    const m = this.renderer.model;
    const t = m.tiles[y * m.width + x];
    if (!t) return `(${x}, ${y})`;
    const parts = [`(${x}, ${y})`, [t.terrain, ...t.features].join(" + ")];
    if (t.resource) parts.push(t.resource);
    if (t.improvement) parts.push(t.improvement);
    if (t.owner != null) parts.push(`owned by ${this.pname(t.owner)}`);
    const city = this.data.overview.cities.find((c) => c.x === x && c.y === y);
    if (city) parts.push(`city ${city.name} (${this.pname(city.owner)}, size ${city.pop})`);
    const units = (m.units || []).filter((u) => u.x === x && u.y === y);
    if (units.length) parts.push(units.map((u) => `${u.name || u.type} (${this.pname(u.owner)})`).join(", "));
    return parts.join(" · ") + ` · ${this.toolHint()}`;
  }

  pname(pid) { const p = this.data.overview.players.find((q) => q.id === pid); return p ? p.name : `player ${pid}`; }

  toolHint() {
    const t = this.tool;
    return { city: "click: found a city", unit: `click: add ${t.count}× ${t.unit}`, remove: "click: remove units there (and the city)",
      select: "click: select a city", owner: "click: give the tile to the player" }[t.kind] || "";
  }

  async click(x, y) {
    const t = this.tool;
    const city = this.data.overview.cities.find((c) => c.x === x && c.y === y);
    if (t.kind === "select" || (city && t.kind === "city")) { this.selectedCity = city ? city.id : null; this.tab = "place"; this.drawSide(); return; }
    if (t.kind === "city") await this.ops([{ op: "found_city", player: t.player, x, y, pop: t.pop }], true);
    else if (t.kind === "unit") await this.ops([{ op: "add_unit", player: t.player, unit: t.unit, x, y, count: t.count }], true);
    else if (t.kind === "remove") {
      const list = [{ op: "remove_units", x, y }];
      if (city) list.push({ op: "remove_city", city: city.id });
      await this.ops(list, true);
    } else if (t.kind === "owner") await this.ops([{ op: "set_tile", x, y, owner: t.player }], true);
  }

  // ---------------------------------------------------------------- side panel
  drawSide() {
    const side = this.side;
    clear(side);
    const tabs = [["scenario", "Scenario"], ["place", "Place"], ["players", "Players"], ["diplomacy", "Diplomacy"], ["ops", "Operations"]];
    side.append(el("div", { class: "tool-grid", style: { marginTop: "8px" } }, ...tabs.map(([k, l]) => el("button", {
      class: `small ${this.tab === k ? "active" : ""}`, onclick: () => { this.tab = k; this.drawSide(); } }, l))));
    const box = el("div", { class: "section" });
    side.append(box);
    ({ scenario: () => this.drawScenario(box), place: () => this.drawPlace(box), players: () => this.drawPlayers(box),
      diplomacy: () => this.drawDiplomacy(box), ops: () => this.drawOps(box) })[this.tab]();
    side.append(el("div", { class: "section" }, el("div", { class: "row" },
      el("button", { class: "small", disabled: !this.data.can_undo, onclick: async () => {
        try { this.apply(await api.editorUndo(this.eid)); } catch (e) { toast(e.message, "error"); }
      } }, "Undo"),
      el("span", { class: "muted small" }, `Turn ${this.data.overview.turn} · left-click acts, drag pans, wheel zooms`))));
  }

  drawScenario(box) {
    const m = this.data.meta;
    const name = el("input", { value: m.name || "", placeholder: "Scenario name" });
    const desc = el("textarea", { rows: 3, value: m.description || "", placeholder: "What this scenario tests" });
    const majors = this.majors();
    const seats = (m.seats || []).map((s) => ({ ...s }));
    const seatRows = majors.map((p, i) => {
      seats[i] = seats[i] || { type: "bot" };
      const t = el("select", { onchange: (e) => { seats[i].type = e.target.value; } },
        ...SEAT_TYPES.map(([k, l]) => el("option", { value: k, selected: seats[i].type === k }, l)));
      return el("div", { class: "row" }, el("span", { class: "chip", style: { background: p.color, width: "12px", height: "12px", display: "inline-block", borderRadius: "3px" } }),
        el("span", { class: "small", style: { minWidth: "90px" } }, `P${p.id} ${p.name}`), t);
    });
    const save = el("button", { class: "primary small", onclick: async () => {
      if (!name.value.trim()) { toast("Give the scenario a name.", "error"); return; }
      try {
        const s = await api.editorSave(this.eid, { id: m.id || null, name: name.value, description: desc.value, seats });
        this.data.meta = { ...m, id: s.id, name: name.value, description: desc.value, seats: s.seats };
        toast(`Saved scenario "${s.name}"`, "success");
        this.drawSide();
      } catch (e) { toast(e.message, "error"); }
    } }, m.id ? "Save" : "Save scenario");
    const saveAs = m.id ? el("button", { class: "small", onclick: async () => {
      try {
        const s = await api.editorSave(this.eid, { id: `${m.id}-copy`, name: `${name.value} (copy)`, description: desc.value, seats });
        this.data.meta = { ...m, id: s.id, name: s.name };
        toast(`Saved as "${s.name}"`, "success");
        this.drawSide();
      } catch (e) { toast(e.message, "error"); }
    } }, "Save as copy") : null;
    box.append(el("h4", {}, "Scenario"), name, desc,
      el("div", { class: "muted small" }, m.id ? `Saved as "${m.id}".` : "Not saved yet."),
      el("h4", {}, "Default seats"), ...seatRows,
      el("p", { class: "muted small" }, "Seats can be changed when a game starts. \"Probe script\" marks the scripted counterparty in probes (it plays as a human seat in normal games)."),
      el("div", { class: "row" }, save, saveAs,
        m.id ? el("button", { class: "small", onclick: () => { location.hash = "#/scenarios"; } }, "Back to list") : null));
  }

  playerSelect(value, onchange, includeCs = false) {
    const players = this.data.overview.players.filter((p) => includeCs || p.kind === "major");
    return el("select", { onchange: (e) => onchange(+e.target.value) },
      ...players.map((p) => el("option", { value: p.id, selected: p.id === value }, `P${p.id} ${p.name}${p.kind !== "major" ? " (city-state)" : ""}`)));
  }

  drawPlace(box) {
    const R = this.rules;
    const t = this.tool;
    const kinds = [["city", "Found city"], ["unit", "Add unit"], ["remove", "Remove"], ["select", "Select city"], ["owner", "Claim tile"]];
    box.append(el("h4", {}, "Click the map to"), el("div", { class: "tool-grid" }, ...kinds.map(([k, l]) => el("button", {
      class: `small ${t.kind === k ? "active" : ""}`, onclick: () => { t.kind = k; this.drawSide(); } }, l))));
    if (["city", "unit", "owner"].includes(t.kind)) box.append(el("label", { class: "small" }, "Player"), this.playerSelect(t.player, (v) => { t.player = v; }, true));
    if (t.kind === "city") box.append(el("label", { class: "small" }, "Population ",
      el("input", { type: "number", min: 1, max: 40, value: t.pop, style: { width: "70px" }, oninput: (e) => { t.pop = Math.max(1, +e.target.value || 1); } })));
    if (t.kind === "unit") {
      const units = Object.keys(R.units).sort();
      box.append(el("label", { class: "small" }, "Unit"),
        el("select", { onchange: (e) => { t.unit = e.target.value; } }, ...units.map((u) => el("option", { value: u, selected: u === t.unit }, `${u}${R.units[u].strength ? ` (${R.units[u].strength})` : ""}`))),
        el("label", { class: "small" }, "Count ", el("input", { type: "number", min: 1, max: 50, value: t.count, style: { width: "60px" }, oninput: (e) => { t.count = Math.max(1, +e.target.value || 1); } })));
    }
    // selected city
    const c = this.data.overview.cities.find((x) => x.id === this.selectedCity);
    if (c) {
      const pop = el("input", { type: "number", min: 1, max: 60, value: c.pop, style: { width: "70px" } });
      const bsel = el("select", {}, ...Object.keys(R.buildings).filter((b) => !c.buildings.includes(b)).sort().map((b) => el("option", { value: b }, b)));
      box.append(el("h4", {}, `City: ${c.name}`),
        el("div", { class: "muted small" }, `${this.pname(c.owner)} · (${c.x}, ${c.y})`),
        el("div", { class: "row" }, el("span", { class: "small" }, "Population"), pop,
          el("button", { class: "small", onclick: () => this.ops([{ op: "set_city", city: c.id, pop: +pop.value }]) }, "Set")),
        el("div", { class: "row" }, bsel, el("button", { class: "small", onclick: () => this.ops([{ op: "set_city", city: c.id, add_buildings: [bsel.value] }]) }, "Add building")),
        el("div", { class: "row" }, el("button", { class: "small", onclick: () => this.ops([{ op: "set_city", city: c.id, claim_radius: 2 }]) }, "Claim 2-tile border"),
          el("button", { class: "small", onclick: () => this.ops([{ op: "set_city", city: c.id, claim_radius: 3 }]) }, "3-tile")),
        el("div", { class: "small" }, "Buildings: ", c.buildings.length ? "" : el("span", { class: "muted" }, "none")),
        el("div", { class: "chips" }, ...c.buildings.map((b) => el("button", { class: "small", title: "Remove", onclick: () => this.ops([{ op: "set_city", city: c.id, remove_buildings: [b] }]) }, `${b} ✕`))),
        el("button", { class: "small danger", onclick: () => this.ops([{ op: "remove_city", city: c.id }]) }, "Remove city"));
    } else if (t.kind === "select") box.append(el("p", { class: "muted small" }, "Click a city to edit its population and buildings."));
  }

  drawPlayers(box) {
    const R = this.rules;
    for (const p of this.data.overview.players.filter((q) => q.kind === "major")) {
      const gold = el("input", { type: "number", value: p.gold, style: { width: "80px" } });
      const faith = el("input", { type: "number", value: p.faith, style: { width: "70px" } });
      const culture = el("input", { type: "number", value: p.culture, style: { width: "70px" } });
      const era = el("select", {}, ...R.era_list.map((e) => el("option", { value: e, selected: e === p.era }, e)));
      const tech = el("select", {}, ...R.tech_order.map((t) => el("option", { value: t }, `${t} (${R.techs[t].era.replace(" era", "")})`)));
      const pol = el("select", {}, ...Object.keys(R.policy_branches).flatMap((b) => [b, ...Object.keys(R.policies).filter((x) => R.policies[x].branch === b)])
        .filter((x, i, a) => a.indexOf(x) === i && !p.policies.includes(x)).map((x) => el("option", { value: x }, x)));
      box.append(el("div", { class: "card", style: { padding: "8px", marginBottom: "8px" } },
        el("div", { class: "row" }, el("span", { style: { background: p.color, width: "12px", height: "12px", display: "inline-block", borderRadius: "3px" } }),
          el("b", {}, `P${p.id} ${p.name}`), el("span", { class: "muted small" }, `${p.era.replace(" era", "")} · ${p.techs} techs · ${p.cities} cities · ${p.units} units`)),
        el("div", { class: "row small" }, "Gold", gold, "Faith", faith, "Culture", culture,
          el("button", { class: "small", onclick: () => this.ops([{ op: "set_player", player: p.id, gold: +gold.value, faith: +faith.value, culture: +culture.value }]) }, "Set")),
        el("div", { class: "row small" }, "Advance to", era,
          el("button", { class: "small", title: "Grant every tech of earlier eras, so the civilization is in this era (like starting in it)", onclick: () => this.ops([{ op: "grant_era", player: p.id, era: era.value }]) }, "Grant"),
          el("button", { class: "small", title: "Same for every civilization", onclick: () => this.ops([{ op: "grant_era", player: "all", era: era.value }]) }, "All civs")),
        el("div", { class: "row small" }, "Tech", tech, el("button", { class: "small", onclick: () => this.ops([{ op: "grant_tech", player: p.id, tech: tech.value }]) }, "Grant")),
        el("div", { class: "row small" }, "Policy", pol, el("button", { class: "small", onclick: () => this.ops([{ op: "adopt_policy", player: p.id, policy: pol.value }]) }, "Adopt")),
        p.policies.length ? el("div", { class: "muted small" }, `Policies: ${p.policies.join(", ")}`) : null,
        el("div", { class: "row" }, el("button", { class: "small", onclick: () => this.ops([{ op: "reveal", player: p.id }]) }, "Reveal map"),
          el("button", { class: "small", onclick: () => this.ops([{ op: "reveal", player: p.id, meet: true }]) }, "Reveal + meet everyone"))));
    }
  }

  drawDiplomacy(box) {
    const ov = this.data.overview;
    box.append(el("h4", {}, "Relations between civilizations"));
    if (!ov.relations.length) { box.append(el("p", { class: "muted small" }, "Needs at least two civilizations.")); return; }
    for (const r of ov.relations) {
      const tog = (label, key, opFor) => el("label", { class: "small" }, el("input", { type: "checkbox", checked: !!r[key],
        onchange: (e) => this.ops([opFor(e.target.checked)], true) }), ` ${label}`);
      box.append(el("div", { class: "card", style: { padding: "8px", marginBottom: "6px" } },
        el("div", {}, el("b", {}, `${this.pname(r.a)} — ${this.pname(r.b)}`)),
        el("div", { class: "row" },
          tog("met", "met", (v) => v ? { op: "meet", a: r.a, b: r.b } : { op: "meet", a: r.a, b: r.b }),
          tog("at war", "war", (v) => ({ op: "set_relation", a: r.a, b: r.b, state: v ? "war" : "peace" })),
          tog("embassies", "embassies", (v) => ({ op: "set_relation", a: r.a, b: r.b, embassies: v })),
          tog("friends", "friends", (v) => ({ op: "set_relation", a: r.a, b: r.b, friends: v })),
          tog("defensive pact", "defensive_pact", (v) => ({ op: "set_relation", a: r.a, b: r.b, defensive_pact: v })),
          tog("open borders", "open_borders", (v) => ({ op: "set_relation", a: r.a, b: r.b, open_borders: v })))));
    }
    const cs = ov.players.filter((p) => p.kind === "city_state");
    if (cs.length) {
      let csId = cs[0].id, who = this.majors()[0].id;
      const amount = el("input", { type: "number", value: 30, style: { width: "70px" } });
      box.append(el("h4", {}, "City-state influence"), this.playerSelect(csId, (v) => { csId = v; }, true),
        this.playerSelect(who, (v) => { who = v; }),
        el("div", { class: "row" }, amount, el("button", { class: "small", onclick: () => this.ops([{ op: "set_influence", city_state: csId, player: who, amount: +amount.value }]) }, "Set influence")),
        el("p", { class: "muted small" }, "30+ = friends, 60+ = allies (UnCiv thresholds)."));
    }
  }

  async drawOps(box) {
    const ta = el("textarea", { rows: 10, style: { fontFamily: "monospace", fontSize: "12px" },
      value: this._opsDraft || '[\n  {"op": "grant_era", "player": "all", "era": "Industrial era"},\n  {"op": "reveal", "player": "all", "meet": true}\n]',
      oninput: (e) => { this._opsDraft = e.target.value; } });
    const help = el("div", { class: "muted small" }, "Loading the operation list…");
    box.append(el("h4", {}, "Apply operations (JSON)"),
      el("p", { class: "muted small" }, "The same operations probes use for their setup. A list is applied all-or-nothing."),
      ta, el("button", { class: "small primary", onclick: async () => {
        let list;
        try { list = JSON.parse(ta.value); } catch (e) { toast(`Not valid JSON: ${e.message}`, "error"); return; }
        if (!Array.isArray(list)) list = [list];
        const r = await this.ops(list);
        if (r) toast(`${list.length} operation${list.length === 1 ? "" : "s"} applied`, "success");
      } }, "Apply"), el("h4", {}, "Operations"), help);
    try {
      const ops = await api.scenarioOps();
      clear(help);
      help.append(...ops.map((o) => el("div", { style: { marginBottom: "4px" } }, el("code", {}, o.op), " — ", o.params)));
    } catch (e) { help.textContent = e.message; }
  }
}

// Post-game recap: scrub through recorded turns, charts, events, diplomacy transcripts and AI thoughts.
import { api } from "./api.js";
import { el, clear, toast } from "./util.js";
import { MapRenderer } from "./render.js";

const METRICS = [["score", "Score"], ["military", "Military strength"], ["techs", "Technologies"], ["cities", "Cities"],
  ["population", "Population"], ["land", "Land"], ["science", "Science / turn"], ["gold", "Gold"]];

function b64(s) {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export class ReplayScreen {
  constructor(root, rules, gid, token) {
    this.root = root; this.rules = rules; this.gid = gid; this.token = token;
    this.idx = 0; this.playing = false; this.speed = 2; this.fog = ""; this.metric = "score"; this.feedTab = "events";
    this.load();
  }

  destroy() { this.playing = false; clearTimeout(this.timer); }

  async load() {
    try { this.data = await api.replay(this.gid, this.token); }
    catch (e) { toast(e.message, "error"); this.root.appendChild(el("p", { style: { padding: "20px" } }, e.message)); return; }
    if (!this.data.frames.length) { this.root.appendChild(el("p", { style: { padding: "20px" } }, "No turns recorded yet.")); return; }
    this.build();
    this.idx = this.data.frames.length - 1;
    this.show();
    this.renderer.centerOn(Math.floor(this.data.width / 2), Math.floor(this.data.height / 2));
    this.renderer.cam.zoom = Math.max(0.3, Math.min(1, (this.canvas.clientWidth / (this.data.width * 30 * 1.75))));
    this.renderer.centerOn(Math.floor(this.data.width / 2), Math.floor(this.data.height / 2));
  }

  build() {
    const d = this.data;
    const winner = d.players.find((p) => p.id === d.winner);
    const header = el("div", { class: "topbar" },
      el("button", { class: "small", onclick: () => { location.hash = "#/"; } }, "☰ Lobby"),
      el("b", {}, `Recap — ${d.name}`),
      el("span", { class: "muted" }, d.phase === "over" ? `Winner: ${winner ? winner.name : "none"} (${d.victory})` : `In progress, turn ${d.turn}`),
      el("span", { class: "spacer" }),
      ...d.players.filter((p) => p.kind === "major").map((p) => el("span", { class: "stat" }, el("span", { class: "swatch", style: { background: p.color } }),
        p.name, p.seat ? el("span", { class: "muted" }, ` (${p.seat.type === "llm" ? p.seat.llm.model || "LLM" : p.seat.type})`) : null)));
    this.canvas = el("canvas", { class: "map" });
    this.side = el("div", { class: "overlay-panel", style: { right: "10px", top: "10px", bottom: "10px", width: "400px", overflowY: "auto", padding: "10px" } });
    this.turnLabel = el("b", { style: { minWidth: "90px" } });
    this.slider = el("input", { type: "range", min: 0, max: d.frames.length - 1, value: 0, oninput: (e) => { this.idx = +e.target.value; this.show(); } });
    this.playBtn = el("button", { onclick: () => this.toggle() }, "▶ Play");
    const speed = el("select", { onchange: (e) => { this.speed = +e.target.value; } },
      ...[[1, "1 turn/s"], [2, "2 turns/s"], [5, "5 turns/s"], [10, "10 turns/s"]].map(([v, t]) => el("option", { value: v, selected: v === this.speed }, t)));
    const fog = el("select", { onchange: (e) => { this.fog = e.target.value; this.show(); } }, el("option", { value: "" }, "Full map"),
      ...d.players.filter((p) => p.kind === "major").map((p) => el("option", { value: p.id }, `What ${p.name} knew`)));
    const bottom = el("div", { class: "bottom" },
      el("button", { onclick: () => { this.idx = Math.max(0, this.idx - 1); this.show(); } }, "◀"), this.playBtn,
      el("button", { onclick: () => { this.idx = Math.min(d.frames.length - 1, this.idx + 1); this.show(); } }, "▶"),
      this.turnLabel, this.slider, speed, fog);
    const main = el("div", { class: "main" }, this.canvas, this.side);
    this.root.appendChild(el("div", { class: "replay" }, header, main, bottom));
    this.renderer = new MapRenderer(this.canvas, this.rules);
    let drag = null;
    this.canvas.addEventListener("mousedown", (e) => { drag = [e.clientX, e.clientY]; });
    window.addEventListener("mouseup", () => { drag = null; });
    this.canvas.addEventListener("mousemove", (e) => { if (drag) { this.renderer.pan(e.clientX - drag[0], e.clientY - drag[1]); drag = [e.clientX, e.clientY]; } });
    this.canvas.addEventListener("wheel", (e) => {
      e.preventDefault();
      const r = this.canvas.getBoundingClientRect();
      this.renderer.zoomAt(e.deltaY < 0 ? 1.12 : 1 / 1.12, e.clientX - r.left, e.clientY - r.top);
    }, { passive: false });
  }

  toggle() {
    this.playing = !this.playing;
    this.playBtn.textContent = this.playing ? "⏸ Pause" : "▶ Play";
    if (this.playing) {
      if (this.idx >= this.data.frames.length - 1) this.idx = 0;
      this.tick();
    }
  }

  tick() {
    if (!this.playing) return;
    this.show();
    if (this.idx >= this.data.frames.length - 1) { this.toggle(); return; }
    this.idx++;
    this.timer = setTimeout(() => this.tick(), 1000 / this.speed);
  }

  modelFor(frame) {
    const d = this.data;
    const owner = b64(frame.owner), imp = b64(frame.improvement), route = b64(frame.route), feat = b64(frame.feature);
    const explored = this.fog !== "" && frame.explored[this.fog] ? b64(frame.explored[this.fog]) : null;
    const tiles = d.terrain.map((t, i) => {
      if (explored && !explored[i]) return null;
      const r = route[i];
      const feature = feat[i] ? d.feature_ids[feat[i] - 1] : null;
      const improvement = imp[i] ? d.improvement_ids[imp[i] - 1] : null;
      const features = [...(t[1] ? ["Hill"] : []), ...(feature ? [feature] : [])];
      return {
        terrain: t[0], features, hills: !!t[1], feature, wonder: t[4] || null, river: t[2], resource: t[3], improvement,
        route: (r & 3) === 1 ? "Road" : (r & 3) === 2 ? "Railroad" : null, routePillaged: !!(r & 4), pillaged: false,
        owner: owner[i] === 255 ? null : owner[i], camp: improvement === "Barbarian encampment",
        village: improvement === "Ancient ruins", visible: true,
      };
    });
    const W = d.width;
    const units = frame.units.map(([type, o, idx, hp], k) => {
      const ud = this.rules.units[type] || {};
      const cls = ud.unitType === "Civilian" || ud.unitType === "Civilian Water" || !(ud.strength || ud.rangedStrength) ? "civilian"
        : { Archery: "ranged", "Ranged Gunpowder": "ranged", Mounted: "mounted", Armored: "armor", Siege: "siege", Scout: "recon",
            "Melee Water": "naval_melee", "Ranged Water": "naval_ranged", Submarine: "naval_ranged" }[ud.unitType] || "melee";
      const domain = /Water|Submarine|Carrier/.test(ud.unitType || "") ? "sea" : /Fighter|Bomber|Missile/.test(ud.unitType || "") ? "air" : "land";
      return { id: k, type, owner: o, x: idx % W, y: Math.floor(idx / W), hp, class: cls, domain, unit_type: ud.unitType };
    }).filter((u) => !explored || explored[u.y * W + u.x]);
    const cities = frame.cities.map(([id, name, o, idx, pop, capital]) => ({ id, name, owner: o, x: idx % W, y: Math.floor(idx / W), pop, capital }))
      .filter((c) => !explored || explored[c.y * W + c.x]);
    const players = Object.fromEntries(d.players.map((p) => [p.id, p]));
    return { width: d.width, height: d.height, tiles, units, cities, players, you: null };
  }

  show() {
    const d = this.data;
    const frame = d.frames[this.idx];
    this.slider.value = this.idx;
    this.turnLabel.textContent = `Turn ${frame.turn}`;
    this.renderer.setModel(this.modelFor(frame));
    this.renderSide(frame);
  }

  renderSide(frame) {
    const d = this.data;
    const s = clear(this.side);
    const metricSel = el("select", { onchange: (e) => { this.metric = e.target.value; this.renderSide(frame); } },
      ...METRICS.map(([k, t]) => el("option", { value: k, selected: k === this.metric }, t)));
    const chart = el("canvas", { width: 380, height: 170, class: "chart-box" });
    s.append(el("div", { class: "row" }, el("b", {}, "Chart"), metricSel), chart);
    this.drawChart(chart, frame.turn);
    const st = d.stats.find((x) => x.turn === frame.turn);
    if (st) {
      const rows = d.players.filter((p) => p.kind === "major").map((p) => {
        const v = st.players[p.id] || {};
        return el("tr", {}, el("td", {}, el("span", { class: "swatch", style: { background: p.color } }), p.name),
          el("td", {}, v.alive === false ? "—" : v.score), el("td", {}, v.cities ?? ""), el("td", {}, v.techs ?? ""), el("td", {}, v.military ?? ""));
      });
      s.appendChild(el("table", { class: "list" }, el("tr", {}, ...["Civ", "Score", "Cities", "Techs", "Military"].map((h) => el("th", {}, h))), ...rows));
    }
    const tabs = el("div", { class: "row section" }, ...[["events", "Events"], ["diplomacy", "Diplomacy"], ["thoughts", "AI thoughts"]].map(([k, t]) =>
      el("button", { class: `small ${this.feedTab === k ? "primary" : ""}`, onclick: () => { this.feedTab = k; this.renderSide(frame); } }, t)));
    s.appendChild(tabs);
    const pname = (id) => (d.players.find((p) => p.id === id) || {}).name || "?";
    const pcolor = (id) => (d.players.find((p) => p.id === id) || {}).color || "#999";
    const turn = frame.turn;
    const body = el("div");
    s.appendChild(body);
    if (this.feedTab === "events") {
      const evs = d.events.filter((e) => e.turn === turn && !["turn_start", "turn_end", "improvement_built", "city_idle", "research_needed"].includes(e.type));
      if (!evs.length) body.appendChild(el("div", { class: "muted" }, "Nothing notable this turn."));
      for (const e of evs) body.appendChild(el("div", { class: `event ${e.x != null ? "clickable" : ""}`, onclick: () => { if (e.x != null) this.renderer.centerOn(e.x, e.y); } }, e.text));
      const big = d.events.filter((e) => ["war_declared", "peace", "city_captured", "eliminated", "victory", "first_contact", "era", "deal", "city_founded"].includes(e.type));
      body.appendChild(el("h4", { class: "section" }, "Timeline highlights"));
      for (const e of big.slice(0, 300)) {
        body.appendChild(el("div", { class: "event clickable", onclick: () => {
          const fi = d.frames.findIndex((f) => f.turn === e.turn);
          if (fi >= 0) { this.idx = fi; this.show(); }
        } }, el("span", { class: "t" }, `T${e.turn}`), e.text));
      }
    } else if (this.feedTab === "diplomacy") {
      const msgs = d.messages.filter((m) => m.turn === turn);
      if (!msgs.length) body.appendChild(el("div", { class: "muted" }, "No messages this turn."));
      for (const m of msgs) body.appendChild(el("div", { class: "event" }, el("span", { class: "swatch", style: { background: pcolor(m.from) } }),
        el("b", {}, pname(m.from)), " → ", m.to.map(pname).join(", "), el("div", { style: { whiteSpace: "pre-wrap" } }, m.text)));
      const negs = d.negotiations.filter((n) => n.turn === turn);
      for (const n of negs) body.appendChild(el("div", { class: "muted section" }, `Negotiation #${n.id} ${pname(n.initiator)} ↔ ${pname(n.responder)}: ${n.status}`));
    } else {
      const ths = d.thoughts.filter((t) => t.turn === turn);
      if (!ths.length) body.appendChild(el("div", { class: "muted" }, "No recorded AI reasoning this turn."));
      for (const t of ths) body.appendChild(el("div", { class: "thought", style: { borderLeftColor: pcolor(t.player) } },
        el("div", { class: "muted" }, `${pname(t.player)}${t.kind ? " · " + t.kind : ""}`), t.text));
    }
  }

  drawChart(canvas, turn) {
    const d = this.data;
    const ctx = canvas.getContext("2d");
    const W = canvas.width, H = canvas.height, pad = 28;
    ctx.clearRect(0, 0, W, H);
    const stats = d.stats;
    if (!stats.length) return;
    const majors = d.players.filter((p) => p.kind === "major");
    let max = 1;
    for (const s of stats) for (const p of majors) max = Math.max(max, (s.players[p.id] || {})[this.metric] || 0);
    const t0 = stats[0].turn, t1 = stats[stats.length - 1].turn;
    const X = (t) => pad + (W - pad - 8) * (t1 === t0 ? 0 : (t - t0) / (t1 - t0));
    const Y = (v) => H - 18 - (H - 30) * (v / max);
    ctx.strokeStyle = "#2c3342"; ctx.lineWidth = 1;
    ctx.beginPath(); ctx.moveTo(pad, 8); ctx.lineTo(pad, H - 18); ctx.lineTo(W - 8, H - 18); ctx.stroke();
    ctx.fillStyle = "#9aa3b5"; ctx.font = "10px system-ui"; ctx.textAlign = "right";
    ctx.fillText(String(Math.round(max)), pad - 3, 12); ctx.fillText("0", pad - 3, H - 18);
    ctx.textAlign = "center"; ctx.fillText(`T${t0}`, pad, H - 5); ctx.fillText(`T${t1}`, W - 18, H - 5);
    for (const p of majors) {
      ctx.strokeStyle = p.color; ctx.lineWidth = 2; ctx.beginPath();
      let started = false;
      for (const s of stats) {
        const v = s.players[p.id];
        if (!v || v.alive === false) continue;
        const x = X(s.turn), y = Y(v[this.metric] || 0);
        if (!started) { ctx.moveTo(x, y); started = true; } else ctx.lineTo(x, y);
      }
      ctx.stroke();
    }
    ctx.strokeStyle = "#ffe066"; ctx.setLineDash([3, 3]);
    ctx.beginPath(); ctx.moveTo(X(turn), 8); ctx.lineTo(X(turn), H - 18); ctx.stroke(); ctx.setLineDash([]);
  }
}

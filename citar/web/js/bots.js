// Bots page: bot profiles (named, revisioned configurations of the scripted bot), their parameter editor, the
// rankings computed from every lab game, and a form that queues an A/B experiment between profiles.
import { api } from "./api.js";
import { el, clear, toast, modal, confirmBox, fmt } from "./util.js";
import { pageHeader } from "./nav.js";
import * as auth from "./auth.js";

// Categorical series colours (reference palette, dark steps; validated against the panel surface #171b24).
// Assigned to an entry when it is first charted and kept, so a colour follows its entry, never its rank.
const SERIES = ["#3987e5", "#d95926", "#199e70", "#c98500", "#d55181", "#008300", "#9085e9", "#e66767"];
const charted = new Map();          // entry id -> colour
let rankOpts = { difficulty: "Prince", factorial: false, minSeats: 1 };

const isAdmin = () => { const u = auth.user(); return !!u && u.role === "admin"; };
const field = (label, input, hint) => el("div", { class: "field" }, el("label", {}, label), input,
  hint ? el("span", { class: "muted small" }, hint) : null);

function ratingText(r) {
  if (!r) return el("span", { class: "muted" }, "unrated");
  if (!r.rated) return el("span", { class: "muted", title: "Only played against itself so far" }, "no rival games");
  return el("span", { title: `${r.seats} seats in ${r.experiments.length} experiment(s)` },
    el("b", {}, fmt(r.rating)), el("span", { class: "muted small" }, ` ±${fmt(r.se)} · ${r.seats} seats`));
}

export async function renderBots(root, rules, sub) {
  const page = el("div", { class: "lobby bots" });
  root.appendChild(page);
  page.appendChild(pageHeader("bots"));
  if (sub) return renderProfile(page, rules, decodeURIComponent(sub));
  const tab = new URLSearchParams(location.hash.split("?")[1] || "").get("tab") || "profiles";
  const intro = el("div", { class: "card" },
    el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Bots"), el("span", { class: "grow" }),
      isAdmin() ? el("button", { onclick: () => newProfile() }, "New profile") : null,
      el("button", { class: "primary", onclick: () => abDialog(rules) }, "A/B test…")),
    el("p", { class: "muted small" },
      "A profile is the scripted bot with a chosen code version, aggression and parameter overrides — every number " +
      "the bot decides with is a parameter. Profiles are what lab experiments, lobby seats and benchmark opponents " +
      "play. Ratings come from every lab game: each game's finishing order is split into head-to-head results and " +
      "fitted on the Elo scale (400 points ≈ 10:1 odds of finishing ahead). An entry is one exact configuration " +
      "(its fingerprint) at one difficulty, so each revision of a profile, and each code change of the live bot, " +
      "gets its own line."),
    el("div", { class: "tabbar" },
      ...[["profiles", "Profiles"], ["rankings", "Rankings"]].map(([k, label]) =>
        el("button", { class: tab === k ? "active" : "", onclick: () => { location.hash = `#/bots?tab=${k}`; } }, label))));
  const body = el("div");
  page.append(intro, body);
  if (tab === "rankings") await drawRankings(body);
  else await drawProfiles(body, rules);
  return {};
}

// ---------------------------------------------------------------------------------------------------------------------
// profiles list
// ---------------------------------------------------------------------------------------------------------------------
async function drawProfiles(body, rules) {
  const [{ profiles }, { engines }] = await Promise.all([api.botProfiles(), api.botEngines()]);
  const eng = Object.fromEntries(engines.map((e) => [e.id, e]));
  const card = el("div", { class: "card" });
  const table = el("table", { class: "list jobs" }, el("tr", {},
    el("th", {}, "Profile"), el("th", {}, "Code"), el("th", {}, "Aggression"), el("th", {}, "Overrides"),
    el("th", {}, "Rating (Prince)"), el("th", {}, "")));
  for (const p of profiles) {
    const n = Object.keys(p.params || {}).length;
    table.appendChild(el("tr", { class: `clickable ${p.archived ? "muted" : ""}`, onclick: () => { location.hash = `#/bots/${encodeURIComponent(p.id)}`; } },
      el("td", {}, el("b", {}, p.name), " ", ...(p.tags || []).map((t) => el("span", { class: "pill small" }, t)),
        p.builtin ? el("span", { class: "pill muted small", title: "Built in: fork it to change it" }, "built-in") : el("span", { class: "muted small" }, ` r${p.rev}`),
        p.description ? el("div", { class: "muted small clamp" }, p.description) : null),
      el("td", { class: "small" }, (eng[p.engine] || {}).label || p.engine),
      el("td", {}, p.aggression == null ? el("span", { class: "muted small" }, "per seat") : fmt(p.aggression, 2)),
      el("td", {}, n ? `${n}` : el("span", { class: "muted" }, "none")),
      el("td", {}, ratingText(p.rating), p.rating && !p.rating_is_current
        ? el("div", { class: "muted small", title: "The rating shown is from an earlier revision or code version" }, "earlier version") : null),
      el("td", {}, el("button", { class: "small", onclick: (e) => { e.stopPropagation(); forkProfile(p); } }, "Fork"))));
  }
  card.append(el("h3", {}, "Profiles"), table);
  body.appendChild(card);
}

async function newProfile() {
  try {
    const p = await api.forkBotProfile("standard", "New profile");
    location.hash = `#/bots/${encodeURIComponent(p.id)}`;
  } catch (e) { toast(e.message, "error"); }
}

async function forkProfile(p) {
  if (!isAdmin()) { toast("Only administrators can create profiles.", "error"); return; }
  try {
    const f = await api.forkBotProfile(p.id, `${p.name} (copy)`);
    toast(`Forked ${p.name}`);
    location.hash = `#/bots/${encodeURIComponent(f.id)}`;
  } catch (e) { toast(e.message, "error"); }
}

// ---------------------------------------------------------------------------------------------------------------------
// rankings
// ---------------------------------------------------------------------------------------------------------------------
async function drawRankings(body) {
  const status = el("p", { class: "muted" }, "Computing ratings…");
  body.appendChild(status);
  let data;
  try { data = await api.botRankings(rankOpts.factorial); } catch (e) { status.textContent = e.message; return; }
  status.remove();
  const diffs = [...new Set(data.entries.map((e) => e.difficulty))];
  const controls = el("div", { class: "row" },
    field("Difficulty", el("select", { onchange: (e) => { rankOpts.difficulty = e.target.value; redraw(); } },
      el("option", { value: "", selected: !rankOpts.difficulty }, "All"),
      ...diffs.map((d) => el("option", { value: d, selected: rankOpts.difficulty === d }, d)))),
    field("Min seats", el("input", { type: "number", min: 1, value: rankOpts.minSeats, style: { width: "80px" },
      oninput: (e) => { rankOpts.minSeats = Math.max(1, +e.target.value || 1); redraw(); } })),
    el("label", { class: "small", title: "Factorial screens give every seat its own mix of settings; they add many one-off entries." },
      el("input", { type: "checkbox", checked: rankOpts.factorial, onchange: (e) => { rankOpts.factorial = e.target.checked; clear(body); drawRankings(body); } }),
      " include factorial screens"),
    el("span", { class: "grow" }),
    el("span", { class: "muted small" }, `${data.games} games · computed ${new Date(data.generated).toLocaleTimeString()}`));
  const chartCard = el("div", { class: "card" });
  const tableCard = el("div", { class: "card" });
  body.append(el("div", { class: "card" }, controls), chartCard, tableCard);

  const visible = () => data.entries.filter((e) => (!rankOpts.difficulty || e.difficulty === rankOpts.difficulty)
    && (e.seats || 0) >= rankOpts.minSeats);
  if (!charted.size) {
    for (const e of visible().filter((x) => x.rated && (data.history[x.id] || []).length).slice(0, 5)) charted.set(e.id, null);
    assignColours();
  }
  const expanded = new Set();
  function redraw() {
    clear(chartCard);
    chartCard.append(el("h3", {}, "Rating over time"),
      el("p", { class: "muted small" }, "Each day's point refits every game finished by the end of that day. Shaded bands are ±1 standard error. Tick entries in the table to chart them (up to 8)."));
    const series = [...charted.entries()].map(([id, color]) => {
      const e = data.entries.find((x) => x.id === id);
      return e ? { id, name: e.name, color, points: data.history[id] || [] } : null;
    }).filter(Boolean);
    chartCard.appendChild(series.length ? ratingChart(series) : el("p", { class: "muted" }, "Nothing charted."));
    clear(tableCard);
    const rows = visible();
    const table = el("table", { class: "list jobs rankings" }, el("tr", {},
      el("th", {}, ""), el("th", {}, "#"), el("th", {}, "Entry"), el("th", { title: "Elo-scale rating ± standard error" }, "Rating"),
      el("th", { title: "Seats played" }, "Seats"),
      el("th", { title: "Mean score share × players: 1.00 is an even share" }, "Score index"),
      el("th", { title: "Share of games this entry won outright (any victory, or top score at the time limit)" }, "Wins"),
      el("th", {}, "Techs"), el("th", {}, "Cities"), el("th", { title: "Cities captured per game" }, "Captures"),
      el("th", {}, "Last game")));
    rows.forEach((e, i) => {
      const color = charted.get(e.id);
      const tick = el("input", { type: "checkbox", checked: charted.has(e.id), disabled: !charted.has(e.id) && charted.size >= SERIES.length,
        onclick: (ev) => ev.stopPropagation(),
        onchange: (ev) => { if (ev.target.checked) charted.set(e.id, null); else charted.delete(e.id); assignColours(); redraw(); } });
      table.appendChild(el("tr", { class: "clickable", onclick: () => { expanded.has(e.id) ? expanded.delete(e.id) : expanded.add(e.id); redraw(); } },
        el("td", {}, tick, color ? el("span", { class: "swatch", style: { background: color } }) : null),
        el("td", { class: "muted" }, e.rated ? `${i + 1}` : "–"),
        el("td", {}, e.profile ? el("a", { href: `#/bots/${encodeURIComponent(e.profile)}`, onclick: (ev) => ev.stopPropagation() }, e.name) : e.name,
          el("div", { class: "muted small" }, `${e.code}${Object.keys(e.params || {}).length ? ` · ${Object.keys(e.params).length} overrides` : ""}` +
            `${e.aggression != null ? ` · aggression ${e.aggression}` : ""} · ${e.experiments.join(", ")}`)),
        el("td", {}, e.rated ? el("span", {}, el("b", {}, fmt(e.rating)), el("span", { class: "muted small" }, ` ±${fmt(e.se)}`)) : el("span", { class: "muted" }, "no rival games")),
        el("td", {}, e.seats ?? "–"),
        el("td", {}, e.score_index != null ? e.score_index.toFixed(2) : "–"),
        el("td", {}, e.win_rate != null ? `${Math.round(e.win_rate * 100)}%` : "–"),
        el("td", {}, e.techs ?? "–"), el("td", {}, e.cities ?? "–"), el("td", {}, e.captured ?? "–"),
        el("td", { class: "small muted" }, e.last ? e.last.slice(0, 16).replace("T", " ") : "–")));
      if (expanded.has(e.id)) table.appendChild(el("tr", {}, el("td", { colspan: 11 }, headToHead(e, data))));
    });
    tableCard.append(el("h3", {}, "Leaderboard"), rows.length ? table : el("p", { class: "muted" }, "No games recorded yet."));
  }
  redraw();
}

function assignColours() {
  const used = new Set([...charted.values()].filter(Boolean));
  for (const [id, c] of charted) {
    if (c) continue;
    const free = SERIES.find((s) => !used.has(s));
    charted.set(id, free);
    used.add(free);
  }
}

function headToHead(e, data) {
  const rows = [];
  for (const [k, v] of Object.entries(data.head_to_head)) {
    const [a, b] = k.split("|");
    if (a !== e.id && b !== e.id) continue;
    const other = data.entries.find((x) => x.id === (a === e.id ? b : a));
    if (!other) continue;
    const mine = a === e.id ? v.a : v.b, theirs = a === e.id ? v.b : v.a;
    const diff = a === e.id ? v.share_diff : -v.share_diff;
    const exp = e.rated && other.rated ? 1 / (1 + 10 ** ((other.rating - e.rating) / 400)) : null;
    rows.push({ other, games: v.games, mine, theirs, ties: v.ties, diff, ci: v.ci95, sig: v.significant, exp });
  }
  rows.sort((x, y) => y.games - x.games);
  if (!rows.length) return el("p", { class: "muted small" }, "No games against other entries.");
  return el("table", { class: "list small" }, el("tr", {},
    el("th", {}, "Against"), el("th", {}, "Games together"), el("th", { title: "Games in which this entry finished ahead / behind" }, "Ahead–behind"),
    el("th", { title: "Mean score-share difference, 95% interval" }, "Score share diff"), el("th", { title: "From the ratings" }, "Expected ahead")),
  ...rows.map((r) => el("tr", {},
    el("td", {}, r.other.name), el("td", {}, r.games), el("td", {}, `${r.mine}–${r.theirs}${r.ties ? ` (${r.ties} tied)` : ""}`),
    el("td", { class: r.sig ? (r.diff > 0 ? "good" : "bad") : "" }, `${r.diff >= 0 ? "+" : ""}${r.diff.toFixed(3)}${r.ci != null ? ` ±${r.ci.toFixed(3)}` : ""}${r.sig ? " *" : ""}`),
    el("td", {}, r.exp != null ? `${Math.round(r.exp * 100)}%` : "–"))));
}

// Rating history: one 2px line per entry, ±1 SE band, a crosshair tooltip listing every series at the hovered day,
// a legend, and direct labels at the line ends when there are four series or fewer.
function ratingChart(series) {
  const W = 900, H = 300, L = 48, R = series.length <= 4 ? 170 : 16, T = 12, B = 28;
  const NS = "http://www.w3.org/2000/svg";
  const svg = (tag, attrs = {}) => { const n = document.createElementNS(NS, tag); for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v); return n; };
  const days = [...new Set(series.flatMap((s) => s.points.map((p) => p[0])))].sort();
  const all = series.flatMap((s) => s.points.flatMap((p) => [p[1] - (p[2] || 0), p[1] + (p[2] || 0)]));
  let lo = Math.floor((Math.min(...all, 1500) - 20) / 100) * 100, hi = Math.ceil((Math.max(...all, 1500) + 20) / 100) * 100;
  if (hi - lo < 200) hi = lo + 200;
  const t = (d) => new Date(d + "T12:00:00").getTime();
  const t0 = t(days[0]), t1 = t(days[days.length - 1]);
  const x = (d) => days.length === 1 ? L + (W - L - R) / 2 : L + (t(d) - t0) / (t1 - t0) * (W - L - R);
  const y = (v) => T + (hi - v) / (hi - lo) * (H - T - B);
  const root = svg("svg", { viewBox: `0 0 ${W} ${H}`, class: "rating-chart", role: "img", "aria-label": "Rating over time" });
  const step = (hi - lo) / 100 > 8 ? 200 : 100;
  for (let v = lo; v <= hi; v += step) {
    root.appendChild(svg("line", { x1: L, x2: W - R, y1: y(v), y2: y(v), class: v === 1500 ? "grid mid" : "grid" }));
    const tx = svg("text", { x: L - 6, y: y(v) + 4, class: "axis", "text-anchor": "end" }); tx.textContent = v; root.appendChild(tx);
  }
  const every = Math.max(1, Math.ceil(days.length / 8));
  days.forEach((d, i) => {
    if (i % every) return;
    const tx = svg("text", { x: x(d), y: H - 8, class: "axis", "text-anchor": "middle" });
    tx.textContent = new Date(d + "T12:00:00").toLocaleDateString([], { month: "short", day: "numeric" }); root.appendChild(tx);
  });
  for (const s of series) {
    const pts = s.points;
    if (!pts.length) continue;
    if (pts.length > 1) {
      const band = pts.map((p) => `${x(p[0])},${y(p[1] + (p[2] || 0))}`).concat(pts.slice().reverse().map((p) => `${x(p[0])},${y(p[1] - (p[2] || 0))}`));
      root.appendChild(svg("polygon", { points: band.join(" "), fill: s.color, opacity: 0.12 }));
      root.appendChild(svg("polyline", { points: pts.map((p) => `${x(p[0])},${y(p[1])}`).join(" "), fill: "none", stroke: s.color, "stroke-width": 2, "stroke-linejoin": "round" }));
    }
    for (const p of pts) root.appendChild(svg("circle", { cx: x(p[0]), cy: y(p[1]), r: 4, fill: s.color, stroke: "#171b24", "stroke-width": 2 }));
  }
  if (series.length <= 4) {         // direct labels at the line ends, nudged apart so they do not collide
    const ends = series.filter((s) => s.points.length).map((s) => ({ s, y: y(s.points[s.points.length - 1][1]) })).sort((a, b) => a.y - b.y);
    for (let i = 1; i < ends.length; i++) if (ends[i].y - ends[i - 1].y < 14) ends[i].y = ends[i - 1].y + 14;
    for (const e of ends) {
      const tx = svg("text", { x: W - R + 8, y: e.y + 4, class: "endlabel" });
      tx.textContent = e.s.name.length > 24 ? e.s.name.slice(0, 23) + "…" : e.s.name;
      root.appendChild(tx);
    }
  }
  const hair = svg("line", { y1: T, y2: H - B, class: "crosshair", visibility: "hidden" });
  root.appendChild(hair);
  const tip = el("div", { class: "chart-tip", hidden: true });
  const hit = svg("rect", { x: L, y: T, width: W - L - R, height: H - T - B, fill: "transparent" });
  root.appendChild(hit);
  const wrap = el("div", { class: "chart-wrap" }, root, tip);
  hit.addEventListener("pointermove", (ev) => {
    const box = root.getBoundingClientRect();
    const px = (ev.clientX - box.left) / box.width * W;
    const d = days.reduce((best, dd) => Math.abs(x(dd) - px) < Math.abs(x(best) - px) ? dd : best, days[0]);
    hair.setAttribute("x1", x(d)); hair.setAttribute("x2", x(d)); hair.setAttribute("visibility", "visible");
    clear(tip);
    tip.appendChild(el("div", { class: "muted small" }, new Date(d + "T12:00:00").toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" })));
    const rows = series.map((s) => ({ s, p: s.points.filter((p) => p[0] <= d).pop() })).filter((r) => r.p).sort((a, b) => b.p[1] - a.p[1]);
    for (const r of rows) tip.appendChild(el("div", { class: "tip-row" }, el("span", { class: "swatch", style: { background: r.s.color } }),
      el("b", {}, fmt(r.p[1])), el("span", { class: "muted" }, ` ±${fmt(r.p[2])} · ${r.p[3]} seats · `), r.s.name));
    tip.hidden = !rows.length;
    const left = x(d) / W * box.width;
    tip.style.left = `${Math.min(left + 12, box.width - 260)}px`;
    tip.style.top = "8px";
  });
  hit.addEventListener("pointerleave", () => { hair.setAttribute("visibility", "hidden"); tip.hidden = true; });
  const legend = series.length >= 2 ? el("div", { class: "row small legend" },
    ...series.map((s) => el("span", {}, el("span", { class: "swatch", style: { background: s.color } }), s.name))) : null;
  return el("div", {}, wrap, legend);
}

// ---------------------------------------------------------------------------------------------------------------------
// one profile: details, parameter editor, ratings, experiments
// ---------------------------------------------------------------------------------------------------------------------
async function renderProfile(page, rules, pid) {
  let info, engines;
  try { [info, { engines }] = await Promise.all([api.botProfile(pid), api.botEngines()]); }
  catch (e) { page.appendChild(el("div", { class: "card" }, el("p", { class: "bad" }, e.message), el("a", { href: "#/bots" }, "← Bots"))); return {}; }
  const saved = info.profile;
  const editable = !saved.builtin && isAdmin();
  const draft = { name: saved.name, description: saved.description || "", tags: [...(saved.tags || [])], engine: saved.engine,
    aggression: saved.aggression, params: JSON.parse(JSON.stringify(saved.params || {})), archived: !!saved.archived, parent: saved.parent };
  let schema = await api.botSchema(draft.engine);
  let filter = "", changedOnly = false;
  const openGroups = new Set();
  const head = el("div", { class: "card" });
  const editor = el("div", { class: "card" });
  const ratingsCard = el("div", { class: "card" });
  const expCard = el("div", { class: "card" });
  page.append(head, editor, ratingsCard, expCard);
  const specs = () => schema.groups.flatMap((g) => g.params);
  const dirty = () => JSON.stringify([draft.name, draft.description, draft.tags, draft.engine, draft.aggression, draft.params, draft.archived])
    !== JSON.stringify([saved.name, saved.description || "", saved.tags || [], saved.engine, saved.aggression, saved.params || {}, !!saved.archived]);

  function drawHead() {
    clear(head);
    const eng = engines.find((e) => e.id === draft.engine);
    const aggFixed = draft.aggression != null;
    const aggVal = el("span", { class: "muted small" }, aggFixed ? fmt(draft.aggression, 2) : "");
    head.append(
      el("div", { class: "row" }, el("a", { href: "#/bots" }, "← Bots"), el("span", { class: "grow" }),
        saved.builtin ? el("span", { class: "pill muted" }, "built-in: fork it to change it") : el("span", { class: "pill" }, `revision ${saved.rev}`),
        el("button", { onclick: () => forkProfile(saved) }, "Fork"),
        el("button", { onclick: () => abDialog(rules, [saved.id]) }, "A/B test…"),
        editable ? el("button", { class: "danger", onclick: async () => {
          if (!(await confirmBox("Delete profile", `Delete ${saved.name}? Its games stay in the rankings under its fingerprint.`))) return;
          try { await api.deleteBotProfile(saved.id); location.hash = "#/bots"; } catch (e) { toast(e.message, "error"); }
        } }, "Delete") : null),
      el("div", { class: "profile-grid" },
        field("Name", el("input", { value: draft.name, disabled: !editable, oninput: (e) => { draft.name = e.target.value; drawSave(); } })),
        field("Code", el("select", { disabled: !editable, onchange: async (e) => {
          const next = e.target.value;
          const nextSchema = await api.botSchema(next);
          const keys = new Set(nextSchema.groups.flatMap((g) => g.params.map((p) => p.key)));
          const lost = Object.keys(draft.params).filter((k) => !keys.has(k));
          if (lost.length) toast(`${lost.length} override(s) don't exist in that code and were dropped: ${lost.join(", ")}`, "warn", 6000);
          for (const k of lost) delete draft.params[k];
          draft.engine = next; schema = nextSchema; drawAll();
        } }, ...engines.map((e2) => el("option", { value: e2.id, selected: e2.id === draft.engine }, e2.label))),
        eng ? eng.description : ""),
        field("Aggression", el("div", { class: "row" },
          el("label", { class: "small" }, el("input", { type: "checkbox", checked: aggFixed, disabled: !editable,
            onchange: (e) => { draft.aggression = e.target.checked ? 0.4 : null; drawHead(); drawSave(); } }), " fixed"),
          aggFixed ? el("input", { type: "range", min: 0, max: 1, step: 0.05, value: draft.aggression, disabled: !editable,
            oninput: (e) => { draft.aggression = +e.target.value; aggVal.textContent = fmt(draft.aggression, 2); drawSave(); } }) : null,
          aggVal),
        aggFixed ? "Army size and appetite for war, 0–1." : "Open: the seat decides (the lab varies it by start position, so rotation evens it out)."),
        field("Tags", el("input", { value: draft.tags.join(", "), disabled: !editable, placeholder: "comma separated",
          oninput: (e) => { draft.tags = e.target.value.split(",").map((t) => t.trim()).filter(Boolean); drawSave(); } }))),
      field("Description", el("textarea", { rows: 2, disabled: !editable, value: draft.description,
        oninput: (e) => { draft.description = e.target.value; drawSave(); } })),
      el("div", { class: "muted small" }, `Fingerprint ${saved.fingerprint || "–"}` +
        (saved.parent ? ` · forked from ${saved.parent}` : "") + (saved.updated ? ` · saved ${saved.updated.replace("T", " ")}${saved.created_by ? ` by ${saved.created_by}` : ""}` : "")),
      saveBar);
    drawSave();
  }

  const saveBar = el("div", { class: "row save-bar" });
  const note = el("input", { placeholder: "What changed and why (kept with the revision)", style: { flex: 1, minWidth: "240px" } });
  function drawSave() {
    clear(saveBar);
    if (!editable) return;
    const n = Object.keys(draft.params).length;
    saveBar.append(el("span", { class: "small muted" }, `${n} parameter override${n === 1 ? "" : "s"}`), note,
      el("button", { disabled: !dirty(), onclick: () => { Object.assign(draft, { name: saved.name, description: saved.description || "", tags: [...(saved.tags || [])],
        engine: saved.engine, aggression: saved.aggression, params: JSON.parse(JSON.stringify(saved.params || {})) }); api.botSchema(draft.engine).then((s) => { schema = s; drawAll(); }); } }, "Discard changes"),
      el("button", { class: "primary", disabled: !dirty(), onclick: save }, "Save"));
  }
  async function save() {
    try {
      const r = await api.saveBotProfile(saved.id, { ...draft, note: note.value });
      toast(r.rev !== saved.rev ? `Saved as revision ${r.rev}` : "Saved");
      clear(page); page.appendChild(pageHeader("bots")); renderProfile(page, rules, saved.id);
    } catch (e) { toast(e.message, "error"); }
  }

  function drawEditor() {
    clear(editor);
    const search = el("input", { type: "search", placeholder: "Find a parameter…", value: filter, style: { minWidth: "240px" },
      oninput: (e) => { filter = e.target.value.toLowerCase(); drawGroups(); } });
    const groupsBox = el("div");
    editor.append(el("div", { class: "row" }, el("h3", { style: { margin: 0 } }, "Parameters"),
      el("span", { class: "muted small" }, `${specs().length} in ${schema.groups.length} groups`), el("span", { class: "grow" }), search,
      el("label", { class: "small" }, el("input", { type: "checkbox", checked: changedOnly, onchange: (e) => { changedOnly = e.target.checked; drawGroups(); } }), " changed only"),
      el("button", { class: "small", onclick: () => { schema.groups.forEach((g) => openGroups.add(g.name)); drawGroups(); } }, "Expand all"),
      el("button", { class: "small", onclick: () => { openGroups.clear(); drawGroups(); } }, "Collapse all")),
    groupsBox);
    function drawGroups() {
      clear(groupsBox);
      if (!schema.groups.length) { groupsBox.appendChild(el("p", { class: "muted" }, "This code has no parameters.")); return; }
      for (const g of schema.groups) {
        const ps = g.params.filter((p) => (!changedOnly || p.key in draft.params)
          && (!filter || `${p.key} ${p.label} ${p.help}`.toLowerCase().includes(filter)));
        if (!ps.length) continue;
        const nChanged = g.params.filter((p) => p.key in draft.params).length;
        const det = el("details", { class: "param-group", open: openGroups.has(g.name) || !!filter || changedOnly,
          ontoggle: (e) => { e.target.open ? openGroups.add(g.name) : openGroups.delete(g.name); } },
          el("summary", {}, el("b", {}, g.name), el("span", { class: "muted small" }, ` ${g.params.length}`),
            nChanged ? el("span", { class: "pill warn small" }, `${nChanged} changed`) : null),
          g.help ? el("p", { class: "muted small" }, g.help) : null,
          el("div", { class: "param-list" }, ...ps.map((p) => paramRow(p))));
        groupsBox.appendChild(det);
      }
    }
    drawGroups();
  }

  function paramRow(p) {
    const changed = p.key in draft.params;
    const value = changed ? draft.params[p.key] : p.default;
    const row = el("div", { class: `param ${changed ? "changed" : ""}` });
    const set = (v) => {
      if (JSON.stringify(v) === JSON.stringify(p.default)) delete draft.params[p.key]; else draft.params[p.key] = v;
      const again = paramRow(p); row.replaceWith(again); drawSave();
    };
    row.append(
      el("div", { class: "param-label", title: p.key }, p.label || p.key, p.unit ? el("span", { class: "muted small" }, ` (${p.unit})`) : null,
        el("div", { class: "muted small" }, p.help || "")),
      el("div", { class: "param-input" }, paramInput(p, value, set, !editable)),
      el("div", { class: "param-default muted small" }, changed ? `default: ${p.default == null && (p.type === "order" || p.type === "list") ? "built-in" : show(p.default)}` : "",
        changed && editable ? el("button", { class: "link small", title: "Back to the default", onclick: () => set(p.default) }, "reset") : null));
    return row;
  }

  function drawRatings() {
    clear(ratingsCard);
    ratingsCard.appendChild(el("h3", {}, "Ratings"));
    if (!info.entries.length) {
      ratingsCard.appendChild(el("p", { class: "muted" }, "No games yet. Queue an A/B test against another profile to rate it."));
      return;
    }
    const series = info.entries.filter((e) => (info.history[e.id] || []).length).slice(0, SERIES.length)
      .map((e, i) => ({ id: e.id, name: e.name, color: SERIES[i], points: info.history[e.id] }));
    if (series.length) ratingsCard.appendChild(ratingChart(series));
    ratingsCard.appendChild(el("table", { class: "list small" }, el("tr", {}, el("th", {}, "Entry"), el("th", {}, "Rating"),
      el("th", {}, "Seats"), el("th", {}, "Score index"), el("th", {}, "Wins"), el("th", {}, "Techs"), el("th", {}, "Cities"), el("th", {}, "Experiments")),
    ...info.entries.map((e) => el("tr", {}, el("td", {}, e.name, el("div", { class: "muted small" }, e.fingerprint === saved.fingerprint ? "current revision" : `fingerprint ${e.fingerprint}`)),
      el("td", {}, ratingText(e)), el("td", {}, e.seats), el("td", {}, e.score_index?.toFixed(2) ?? "–"),
      el("td", {}, e.win_rate != null ? `${Math.round(e.win_rate * 100)}%` : "–"), el("td", {}, e.techs ?? "–"), el("td", {}, e.cities ?? "–"),
      el("td", { class: "small" }, e.experiments.join(", "))))));
  }

  function drawExperiments() {
    clear(expCard);
    expCard.appendChild(el("h3", {}, "Lab experiments with this profile"));
    if (!info.experiments.length) { expCard.appendChild(el("p", { class: "muted" }, "None yet.")); return; }
    expCard.appendChild(el("table", { class: "list small" }, el("tr", {}, el("th", {}, "Experiment"), el("th", {}, "State"), el("th", {}, "Games"), el("th", {}, "Seats")),
      ...info.experiments.map((x) => el("tr", { class: "clickable", onclick: () => { location.hash = "#/lab"; } },
        el("td", {}, el("b", {}, x.name), x.note ? el("div", { class: "muted" }, x.note) : null), el("td", {}, x.state),
        el("td", {}, `${x.done} / ${x.games}`), el("td", {}, x.seats.join(" / "))))));
    if (saved.history && saved.history.length > 1) {
      expCard.appendChild(el("h3", {}, "Revisions"));
      expCard.appendChild(el("table", { class: "list small" }, el("tr", {}, el("th", {}, "Rev"), el("th", {}, "Saved"), el("th", {}, "Note"), el("th", {}, "Overrides")),
        ...saved.history.slice().reverse().map((h) => el("tr", {}, el("td", {}, `r${h.rev}`), el("td", {}, `${(h.at || "").replace("T", " ")}${h.by ? ` · ${h.by}` : ""}`),
          el("td", {}, h.note || ""), el("td", { class: "small muted" }, Object.entries(h.params || {}).map(([k, v]) => `${k}=${show(v)}`).join(", ") || "none")))));
    }
  }

  function drawAll() { drawHead(); drawEditor(); }
  drawAll();
  drawRatings();
  drawExperiments();
  window.onbeforeunload = () => (dirty() ? true : undefined);
  return { destroy() { window.onbeforeunload = null; } };
}

function show(v) {
  if (v == null) return "none";
  if (Array.isArray(v)) return v.length > 3 ? `${v.slice(0, 3).join(", ")}, … (${v.length})` : v.join(", ");
  return String(v);
}

function paramInput(p, value, set, disabled) {
  if (p.type === "bool") return el("input", { type: "checkbox", checked: !!value, disabled, onchange: (e) => set(e.target.checked) });
  if (p.type === "int" || p.type === "float") {
    const step = p.type === "int" ? 1 : Math.abs(p.default) >= 10 ? 1 : Math.abs(p.default) >= 1 ? 0.1 : 0.01;
    return el("input", { type: "number", value, step, min: p.min ?? undefined, max: p.max ?? undefined, disabled, style: { width: "110px" },
      onchange: (e) => { if (e.target.value === "") return; const v = +e.target.value; set(p.type === "int" && Number.isInteger(v) ? v : v); } });
  }
  if (p.type === "choice") {
    return el("select", { disabled, onchange: (e) => set(e.target.value === "__none" ? null : e.target.value) },
      ...p.choices.map((c) => el("option", { value: c == null ? "__none" : c, selected: c === value }, c == null ? "(none)" : c)));
  }
  if (p.type === "order" || p.type === "list") return listInput(p, value, set, disabled);
  return el("input", { value: JSON.stringify(value), disabled, onchange: (e) => { try { set(JSON.parse(e.target.value)); } catch (err) { toast("Not valid JSON", "error"); } } });
}

// Ordered lists (preferences) and sets. A value may be a preset's name, null (the built-in default) or a custom list.
function listInput(p, value, set, disabled) {
  const presets = p.presets || {};
  const names = Object.keys(presets);
  const box = el("div", { class: "list-input" });
  const resolved = typeof value === "string" ? presets[value] || [] : value == null ? presets.default || p.default || [] : value;
  if (names.length) {
    const mode = typeof value === "string" ? value : value == null ? "__default" : "__custom";
    box.appendChild(el("select", { disabled, onchange: (e) => {
      const v = e.target.value;
      set(v === "__default" ? null : v === "__custom" ? [...resolved] : v);
    } },
    el("option", { value: "__default", selected: mode === "__default" }, "built-in default"),
    ...names.filter((n) => n !== "default").map((n) => el("option", { value: n, selected: mode === n }, n)),
    el("option", { value: "__custom", selected: mode === "__custom" }, "custom order…")));
    if (!Array.isArray(value)) { box.appendChild(el("div", { class: "muted small" }, resolved.join(" → "))); return box; }
  }
  if (p.type === "list") {
    box.appendChild(el("div", { class: "row small" }, ...p.options.map((o) => el("label", {},
      el("input", { type: "checkbox", disabled, checked: resolved.includes(o), onchange: (e) => set(e.target.checked ? [...resolved, o] : resolved.filter((x) => x !== o)) }), ` ${o}`))));
    return box;
  }
  const list = el("ol", { class: "order-list" });
  resolved.forEach((item, i) => list.appendChild(el("li", {}, el("span", { class: "grow" }, item),
    el("button", { class: "link small", disabled: disabled || i === 0, title: "Earlier", onclick: () => { const v = [...resolved]; [v[i - 1], v[i]] = [v[i], v[i - 1]]; set(v); } }, "↑"),
    el("button", { class: "link small", disabled: disabled || i === resolved.length - 1, title: "Later", onclick: () => { const v = [...resolved]; [v[i + 1], v[i]] = [v[i], v[i + 1]]; set(v); } }, "↓"),
    el("button", { class: "link small", disabled, title: "Remove", onclick: () => set(resolved.filter((_, j) => j !== i)) }, "✕"))));
  box.appendChild(list);
  const missing = (p.options || []).filter((o) => !resolved.includes(o));
  if (missing.length && !disabled) {
    box.appendChild(el("select", { onchange: (e) => { if (e.target.value) set([...resolved, e.target.value]); } },
      el("option", { value: "" }, "add…"), ...missing.map((o) => el("option", { value: o }, o))));
  }
  return box;
}

// ---------------------------------------------------------------------------------------------------------------------
// profile picker for bot seats elsewhere (lobby, benchmark scenarios, probes)
// ---------------------------------------------------------------------------------------------------------------------
let profileCache = null;
// {profiles: [...] in ranking order (archived left out), best: id "Best bot" stands for right now}
export function loadProfiles() {
  if (!profileCache) {
    profileCache = api.botProfiles().then((r) => ({ profiles: r.profiles.filter((p) => !p.archived), best: r.best }))
      .catch(() => ({ profiles: [], best: "standard" }));
    setTimeout(() => { profileCache = null; }, 60000);       // rankings move; don't hold a stale list for long
  }
  return profileCache;
}

// The profile a picker value stands for ("best" resolves to today's best-ranked one).
export async function profileFor(value) {
  const { profiles, best } = await loadProfiles();
  return profiles.find((p) => p.id === (value === "best" ? best : value || "standard")) || null;
}

// A select of bot profiles bound to obj[key], listed in ranking order. With `best`, the first option is "Best bot",
// which the server resolves to the best-ranked profile when the game is created (and records in the seat).
// A null value means `fallback`. onChange gets the chosen profile.
export function profileSelect(obj, key = "profile", onChange = null, { best = false, fallback = "standard" } = {}) {
  const s = el("select", { title: "Which bot plays this seat. Listed best first by their rating on this server (Bots page)." });
  if (obj[key] == null) obj[key] = fallback;
  loadProfiles().then(({ profiles, best: bestId }) => {
    const top = profiles.find((p) => p.id === bestId);
    if (best) s.appendChild(el("option", { value: "best" }, `★ Best bot${top ? ` (now: ${top.name})` : ""}`));
    for (const p of profiles) {
      const r = p.rating && p.rating.rated ? p.rating : null;
      const label = r ? `#${p.rank} ${p.name} · ${Math.round(r.rating)}${p.rating_is_current ? "" : " (earlier version)"}`
        : `${p.name} · unrated`;
      s.appendChild(el("option", { value: p.id }, label));
    }
    s.value = obj[key];
    if (s.value !== obj[key]) { obj[key] = fallback; s.value = fallback; }
  });
  s.onchange = async () => {
    obj[key] = s.value;
    if (onChange) onChange(await profileFor(s.value));
  };
  return s;
}

// ---------------------------------------------------------------------------------------------------------------------
// A/B experiment form
// ---------------------------------------------------------------------------------------------------------------------
export async function abDialog(rules, preselect = []) {
  if (!isAdmin()) { toast("Only administrators can queue lab experiments.", "error"); return; }
  const { profiles } = await api.botProfiles();      // ranking order
  const chosen = new Set(preselect.length ? preselect : []);
  if (chosen.size === 1 && !chosen.has("standard")) chosen.add("standard");
  const f = { players: 4, games: 12, turns: 0, size: "small", maps: ["continents", "pangaea", "fractal"], speed: "Quick",
    difficulty: "Prince", priority: 0, seed: 5000 + Math.floor(Math.random() * 90000), name: "", note: "" };
  const est = el("p", { class: "muted small" });
  const pickBox = el("div", { class: "ab-pick" });
  const drawPick = () => {
    clear(pickBox);
    for (const p of profiles.filter((x) => !x.archived)) {
      pickBox.appendChild(el("label", { class: "row small" }, el("input", { type: "checkbox", checked: chosen.has(p.id), onchange: (e) => {
        if (e.target.checked) chosen.add(p.id); else chosen.delete(p.id); update(); } }),
      el("b", {}, p.name), p.rating && p.rating.rated ? el("span", { class: "muted" }, ` ${fmt(p.rating.rating)}`) : null));
    }
  };
  const update = () => {
    const n = chosen.size;
    const perGame = (f.turns ? f.turns / 330 : 1) * (f.players / 4) * 12;
    est.textContent = n < 1 ? "Pick at least one profile." : `${n} profile${n > 1 ? "s" : ""}, seats ${[...Array(f.players)].map((_, i) => [...chosen][i % n] || "?").join(" / ")}, rotated every game. ` +
      `Roughly ${Math.round(perGame)} min per game per worker on a laptop core; ${f.games} games. ` +
      "Twelve games show a large difference; subtle ones need 24–40.";
  };
  const num = (key, attrs) => el("input", { type: "number", value: f[key], ...attrs, oninput: (e) => { f[key] = +e.target.value; update(); } });
  const content = el("div", { class: "col ab-form" },
    el("p", { class: "muted small" }, "The chosen profiles take the seats in turn (A, B, A, B…) and the seat order rotates every game, so each " +
      "profile plays every start position on the same maps and seeds — which removes map and position luck from the comparison. " +
      "Profiles are frozen when the experiment is queued: editing one later doesn't change it."),
    field("Profiles", pickBox),
    el("div", { class: "grid-scenario" },
      field("Players", num("players", { min: 2, max: 8 })),
      field("Games", num("games", { min: 1, max: 500 })),
      field("Turn limit (0 = full game)", num("turns", { min: 0, max: 1000 })),
      field("Map size", el("select", { onchange: (e) => { f.size = e.target.value; } },
        ...Object.entries(rules.map_sizes).map(([k, v]) => el("option", { value: k, selected: k === f.size }, v.name)))),
      field("Speed", el("select", { onchange: (e) => { f.speed = e.target.value; } },
        ...Object.keys(rules.speeds).map((k) => el("option", { value: k, selected: k === f.speed }, k)))),
      field("Difficulty", el("select", { onchange: (e) => { f.difficulty = e.target.value; } },
        ...rules.difficulty_list.map((k) => el("option", { value: k, selected: k === f.difficulty }, k)))),
      field("Priority", num("priority", { min: -100, max: 100 })),
      field("Seed", num("seed", { min: 0 })),
      field("Name (optional)", el("input", { value: "", placeholder: "ab-…", oninput: (e) => { f.name = e.target.value; } }))),
    field("Maps (game i uses map i mod n)", el("div", { class: "row small" }, ...Object.entries(rules.map_types).map(([k, v]) => el("label", {},
      el("input", { type: "checkbox", checked: f.maps.includes(k), onchange: (e) => { f.maps = e.target.checked ? [...f.maps, k] : f.maps.filter((x) => x !== k); } }), ` ${v.name}`)))),
    field("Note", el("input", { placeholder: "The question this answers", oninput: (e) => { f.note = e.target.value; } })),
    est);
  drawPick();
  update();
  const m = modal({ title: "A/B test between profiles", content, footer: [
    el("button", { onclick: () => m.close() }, "Cancel"),
    el("button", { class: "primary", onclick: async () => {
      if (!chosen.size) { toast("Pick a profile.", "error"); return; }
      try {
        const r = await api.queueBotExperiment({ ...f, name: f.name || null, profiles: [...chosen] });
        m.close();
        toast(`Queued ${r.name}: ${r.games} games. Progress is on the Lab page.`, "info", 6000);
      } catch (e) { toast(e.message, "error"); }
    } }, "Queue experiment")] });
}

// The phone site: a read-mostly check-in on the server - what is running, how the games stand, what the AIs are
// doing, benchmark progress, reports and machines. The full game client stays on the desktop site; the server sends
// phones here (see app.py `_wants_mobile`), and the "Desktop site" link at the bottom switches for good.
import { api, NotSignedIn, setCsrfToken } from "./api.js";
import * as auth from "./auth.js";
import { el, clear, toast } from "./util.js";

const root = document.getElementById("app");
const titleEl = document.getElementById("m-title");
const tabsEl = document.getElementById("m-tabs");
let timer = null;
let current = null;           // the render function of the page showing, for refresh

const TABS = [
  ["#/", "⌂", "Overview"],
  ["#/games", "⚑", "Games"],
  ["#/bench", "⏱", "Benchmarks"],
  ["#/reports", "▤", "Reports"],
  ["#/servers", "▣", "Servers"],
];

// ---------------------------------------------------------------------------- small pieces
const ago = (seconds) => {
  const s = Math.max(0, Math.round(seconds));
  if (s < 90) return `${s}s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  if (s < 172800) return `${Math.round(s / 3600)} h`;
  return `${Math.round(s / 86400)} days`;
};
const bytes = (n) => (n >= 1e12 ? `${(n / 1e12).toFixed(1)} TB` : n >= 1e9 ? `${(n / 1e9).toFixed(1)} GB` : `${Math.round(n / 1e6)} MB`);
const pct = (a, b) => (b ? Math.round((100 * a) / b) : 0);

function stat(label, value, cls = "") {
  return el("div", { class: `m-stat ${cls}` }, el("div", { class: "m-stat-v" }, value), el("div", { class: "m-stat-l" }, label));
}

function meter(label, used, total, text) {
  const p = pct(used, total);
  return el("div", { class: "m-meter" },
    el("div", { class: "row" }, el("span", {}, label), el("span", { class: "muted" }, text || `${p}%`)),
    el("div", { class: `m-bar ${p > 90 ? "bad" : p > 75 ? "warn" : ""}` }, el("div", { style: { width: `${Math.min(100, p)}%` } })));
}

function gameState(g) {
  if (g.phase !== "playing") return ["over", g.winner != null ? "finished" : "over"];
  if (g.paused) return (g.pause_reason && g.pause_reason.kind === "disconnect") ? ["bad", "paused: server down"] : ["warn", "paused"];
  const st = Object.values(g.agent_status || {});
  if (st.includes("reconnecting")) return ["warn", "reconnecting"];
  if (Object.keys(g.agent_errors || {}).length) return ["bad", "AI error"];
  if (st.includes("thinking")) return ["live", "AI thinking"];
  return ["live", "playing"];
}

function section(title, ...children) {
  return el("section", { class: "m-card" }, title ? el("h2", {}, title) : null, ...children);
}

function empty(text) { return el("p", { class: "muted m-empty" }, text); }

// ---------------------------------------------------------------------------- pages
async function overview() {
  titleEl.textContent = "Overview";
  const [status, games] = await Promise.all([api.get("/api/status"), api.games()]);
  const g = status.games;
  const out = [];
  out.push(section(null,
    el("div", { class: "m-stats" },
      stat("playing", g.playing, "live"), stat("paused", g.paused, g.paused ? "warn" : ""),
      stat("need attention", g.problems, g.problems ? "bad" : ""), stat("finished", g.over)),
    el("div", { class: "m-stats" },
      stat("AIs thinking", g.ai_thinking), stat("reconnecting", g.ai_reconnecting, g.ai_reconnecting ? "warn" : ""),
      status.benchmarks ? stat("benchmark jobs", `${status.benchmarks.active_jobs} / ${status.benchmarks.queued_jobs}`, "",) : null,
      status.workers_online != null ? stat("workers online", status.workers_online) : null),
    el("p", { class: "muted small" }, `CITAR ${status.version} · up ${ago(status.uptime)} · ${status.mode} mode · signed in as ${status.user}`)));
  const quiet = (status.benchmarks && status.benchmarks.restricted) || [];
  if (quiet.length) {
    out.push(section("Quiet hours", ...quiet.map((r) => el("div", { class: "row m-line" }, el("span", {}, `🌙 ${r.name}`),
      el("span", { class: "muted small" }, `until ${r.until}`))),
      el("p", { class: "muted small" }, "Benchmark work on these servers waits until then.")));
  }
  if (status.host) {
    const h = status.host;
    out.push(section("This server",
      h.load ? el("div", { class: "row" }, el("span", {}, "Load"), el("span", { class: "muted" }, `${h.load.join(" · ")} on ${h.cpus} CPU${h.cpus === 1 ? "" : "s"}`)) : null,
      h.memory ? meter("Memory", h.memory.used, h.memory.total, `${bytes(h.memory.used)} of ${bytes(h.memory.total)}`) : null,
      h.disk ? meter("Disk", h.disk.used, h.disk.total, `${bytes(h.disk.used)} of ${bytes(h.disk.total)}`) : null,
      (status.workers || []).length ? el("div", {}, el("h3", {}, "Connected workers"),
        ...status.workers.map((w) => el("div", { class: "row m-line" }, el("span", {}, `● ${w.hostname || w.server_id}`),
          el("span", { class: "muted" }, `${w.models} models · ${w.in_flight}/${w.max_concurrent} busy`)))) : null));
  }
  const live = games.filter((x) => x.phase === "playing");
  out.push(section("Running games", ...(live.length ? live.map(gameRow) : [empty("Nothing running right now.")]),
    games.length > live.length ? el("a", { href: "#/games", class: "m-more" }, `All games (${games.length}) →`) : null));
  return out;
}

function gameRow(x) {
  const [cls, label] = gameState(x);
  const n = (type) => (x.seats || []).filter((s) => s.type === type).length;
  const seats = [[n("llm"), "AI model"], [n("human"), "human"], [n("bot"), "bot"], [n("mcp"), "MCP client"]]
    .filter(([k]) => k).map(([k, what]) => `${k} ${what}${k === 1 ? "" : "s"}`).join(", ");
  const cur = (x.players || []).find((p) => p.id === x.current_player);
  return el("a", { class: "m-row", href: `#/game/${x.id}` },
    el("div", { class: "m-row-main" },
      el("div", { class: "m-row-title" }, x.name),
      el("div", { class: "muted small" }, `Turn ${x.turn}${x.config && x.config.turn_limit ? ` / ${x.config.turn_limit}` : ""}`
        + (x.phase === "playing" && cur ? ` · ${cur.name} to move` : "") + (seats ? ` · ${seats}` : ""))),
    el("span", { class: `m-pill ${cls}` }, label));
}

async function gamesPage() {
  titleEl.textContent = "Games";
  const games = await api.games();
  if (!games.length) return [section(null, empty("No games yet. Start one from the desktop site."))];
  const order = (x) => (x.phase === "playing" ? (x.paused ? 1 : 0) : 2);
  games.sort((a, b) => order(a) - order(b) || b.created - a.created);
  return [section(null, ...games.map(gameRow))];
}

async function gamePage(gid) {
  const d = await api.get(`/api/games/${gid}/summary`);
  titleEl.textContent = d.name;
  const out = [];
  const [cls, label] = gameState({ ...d, agent_status: Object.fromEntries(d.players.map((p) => [p.id, p.status])),
    agent_errors: Object.fromEntries(d.players.filter((p) => p.error).map((p) => [p.id, p.error])) });
  const head = section(null,
    el("div", { class: "row" }, el("div", {},
      el("div", { class: "m-big" }, `Turn ${d.turn}`, el("span", { class: "muted" }, ` / ${d.turn_limit}`)),
      el("div", { class: "muted small" }, d.phase === "playing" ? `${d.current || "?"} to move` : (d.victory ? `${d.victory}` : "game over"))),
      el("span", { class: `m-pill ${cls}` }, label)),
    d.paused && d.pause_reason && d.pause_reason.kind === "disconnect"
      ? el("p", { class: "m-alert" }, `${d.pause_reason.message}. The game resumes by itself when it answers again.`) : null,
    d.can_manage && d.phase === "playing"
      ? el("div", { class: "row m-actions" },
          el("button", { class: d.paused ? "primary" : "", onclick: async (e) => {
            e.target.disabled = true;
            try { await api.control(gid, { paused: !d.paused }); toast(d.paused ? "Resumed." : "Paused."); render(); }
            catch (err) { toast(err.message, "error"); e.target.disabled = false; }
          } }, d.paused ? "▶ Resume" : "⏸ Pause AI players"))
      : null);
  out.push(head);
  out.push(section(d.full ? "Standings" : "Players", el("div", { class: "m-table" },
    ...d.players.map((p, i) => el("div", { class: `m-player ${p.alive ? "" : "dead"}` },
      el("span", { class: "m-dot", style: { background: p.color } }),
      el("div", { class: "m-row-main" },
        el("div", {}, d.full ? `${i + 1}. ` : "", el("b", {}, p.name), p.alive ? "" : " (eliminated)"),
        el("div", { class: "muted small" },
          [p.seat === "llm" ? (p.model || "AI model") : p.seat, p.era,
           p.cities != null ? `${p.cities} cities · pop ${p.pop} · ${p.techs} techs` : null].filter(Boolean).join(" · ")),
        p.status === "reconnecting" ? el("div", { class: "warn small" }, "reconnecting to its model server…")
          : p.status === "thinking" ? el("div", { class: "live small" }, "thinking…") : null,
        p.error ? el("div", { class: "bad small" }, p.error) : null),
      p.score != null ? el("span", { class: "m-score" }, p.score) : null)))));
  out.push(section("Recent news", ...(d.events.length
    ? d.events.slice().reverse().map((e) => el("div", { class: "m-event" }, el("span", { class: "muted" }, `T${e.turn}`), el("span", {}, e.text)))
    : [empty("Nothing public has happened yet.")])));
  out.push(el("a", { class: "m-more", href: "#/games" }, "← All games"));
  return out;
}

async function benchPage() {
  titleEl.textContent = "Benchmarks";
  let runs;
  try { runs = await api.runs(); } catch (e) { if (e.status === 404) return [section(null, empty("Benchmarks are not available here."))]; throw e; }
  runs = Array.isArray(runs) ? runs : runs.runs || [];
  if (!runs.length) return [section(null, empty("No benchmark runs yet."))];
  return runs.slice(0, 15).map((r) => {
    const jobs = r.jobs || [];
    const done = jobs.filter((j) => ["done", "failed", "cancelled"].includes(j.status)).length;
    return section(null,
      el("div", { class: "row" }, el("div", { class: "m-row-title" }, r.name || r.id), el("span", { class: `m-pill ${r.status === "running" ? "live" : r.status === "done" ? "good" : ""}` }, r.status)),
      meter(`${done} of ${jobs.length} jobs done`, done, jobs.length),
      ...jobs.filter((j) => !["done", "cancelled"].includes(j.status)).slice(0, 8).map((j) => el("div", { class: "row m-line" },
        el("span", {}, j.model || j.label || j.id),
        el("span", { class: `muted small ${j.status === "failed" ? "bad" : ""}` },
          j.status + (j.pause_reason ? ` (${j.pause_reason})` : "") + (j.progress && j.progress.turn ? ` · turn ${j.progress.turn}` : "")))));
  });
}

async function reportsPage() {
  titleEl.textContent = "Reports";
  let list = [];
  if (auth.isAdmin()) {
    const r = await api.reports();
    list = (Array.isArray(r) ? r : r.reports || []).map((x) => ({ id: x.id, name: x.title || x.name || x.id, status: x.status,
      when: x.created || x.created_at, url: x.status === "done" || !x.status ? `/api/reports/${x.id}/html` : null }));
  } else {
    const r = await api.get("/api/shared/reports");
    list = (r.reports || []).map((x) => ({ id: x.id, name: x.name, when: x.created_at, url: x.public_url || `/r/${x.id}` }));
  }
  if (!list.length) return [section(null, empty("No reports yet."))];
  return [section(null, ...list.slice(0, 40).map((x) => el(x.url ? "a" : "div", { class: "m-row", href: x.url || null, target: x.url ? "_blank" : null, rel: "noopener" },
    el("div", { class: "m-row-main" }, el("div", { class: "m-row-title" }, x.name),
      el("div", { class: "muted small" }, x.when ? new Date(typeof x.when === "number" ? x.when * 1000 : x.when).toLocaleString() : "")),
    x.status && x.status !== "done" ? el("span", { class: "m-pill" }, x.status) : el("span", { class: "muted" }, "›"))))];
}

async function serversPage() {
  titleEl.textContent = "Servers";
  const { servers } = await api.poolServers();
  if (!servers.length) return [section(null, empty("No machines yet. Add one from the desktop site, then run the CITAR helper on it."))];
  return [section(null, ...servers.map((s) => el("div", { class: "m-row" },
    el("div", { class: "m-row-main" }, el("div", { class: "m-row-title" }, s.name),
      el("div", { class: "muted small" }, [s.is_mine ? "yours" : "shared with you", s.admission && !s.admission.allowed ? s.admission.reason : null]
        .filter(Boolean).join(" · "))),
    el("span", { class: `m-pill ${s.online ? "live" : s.online === false ? "" : "over"}` }, s.online ? "online" : s.online === false ? "offline" : "direct"))))];
}

// ---------------------------------------------------------------------------- sign in
function loginPage() {
  titleEl.textContent = "Sign in";
  const cfg = auth.authConfig() || {};
  const id = el("input", { autocomplete: "username", placeholder: "username or email" });
  const pw = el("input", { type: "password", autocomplete: "current-password", placeholder: "password" });
  const err = el("p", { class: "bad small" });
  const go = el("button", { class: "primary wide", type: "submit" }, "Sign in");
  const form = el("form", { class: "m-login", onsubmit: async (e) => {
    e.preventDefault();
    go.disabled = true; err.textContent = "";
    try {
      const r = await api.login(id.value.trim(), pw.value, auth.browserTimezone());
      setCsrfToken(r.csrf_token);
      await auth.refresh();
      location.hash = "#/";
      route();
    } catch (x) { err.textContent = x.message; go.disabled = false; }
  } },
  ...(cfg.providers || []).map((p) => el("a", { class: "button wide", href: `/api/auth/oauth/${p.name}/start?next=${encodeURIComponent("/")}&tz=${encodeURIComponent(auth.browserTimezone())}` }, `Continue with ${p.label}`)),
  (cfg.providers || []).length ? el("div", { class: "muted small m-or" }, "or") : null,
  id, pw, err, go);
  return [section("Sign in to CITAR", form)];
}

// ---------------------------------------------------------------------------- shell
function renderTabs() {
  const here = location.hash || "#/";
  clear(tabsEl);
  for (const [href, icon, label] of TABS) {
    const active = href === "#/" ? here === "#/" || here === "" : here.startsWith(href) || (href === "#/games" && here.startsWith("#/game/"));
    tabsEl.appendChild(el("a", { href, class: active ? "active" : "" }, el("span", { class: "m-ico" }, icon), el("span", {}, label)));
  }
}

function footer() {
  return el("footer", { class: "m-foot" },
    el("a", { href: "/?site=desktop" }, "Desktop site"),
    auth.user() && !(auth.session() || {}).local_mode ? el("a", { href: "#", onclick: async (e) => { e.preventDefault(); await api.logout().catch(() => {}); await auth.boot(); route(); } }, "Sign out") : null);
}

async function render() {
  if (!current) return;
  try {
    const nodes = await current();
    clear(root);
    root.append(...nodes, footer());
  } catch (e) {
    if (e instanceof NotSignedIn) { await auth.boot().catch(() => {}); current = loginPage; render(); return; }
    clear(root);
    root.append(section(null, el("p", { class: "bad" }, e.message)), footer());
  }
}

async function route() {
  clearInterval(timer);
  const hash = location.hash || "#/";
  renderTabs();
  if (!auth.user()) {
    tabsEl.style.display = "none";
    current = loginPage;
  } else {
    tabsEl.style.display = "";
    const m = hash.match(/^#\/game\/([^/?]+)/);
    current = m ? () => gamePage(m[1])
      : hash.startsWith("#/games") ? gamesPage
      : hash.startsWith("#/bench") ? benchPage
      : hash.startsWith("#/reports") ? reportsPage
      : hash.startsWith("#/servers") ? serversPage : overview;
    // pages that change by themselves refresh themselves; the rest refresh on the button
    if (current === overview || current === gamesPage || m || current === benchPage) timer = setInterval(render, 10000);
  }
  root.scrollTop = 0;
  await render();
}

// a GET helper for endpoints the shared api module has no wrapper for
api.get = async (url) => {
  const res = await fetch(url, { credentials: "same-origin" });
  let data = null;
  try { data = await res.json(); } catch (e) { data = null; }
  if (res.status === 401) throw new NotSignedIn();
  if (!res.ok) { const err = new Error((data && data.detail) || `${res.status} ${res.statusText}`); err.status = res.status; throw err; }
  return data;
};

document.getElementById("m-refresh").addEventListener("click", () => { render(); toast("Refreshed."); });
document.addEventListener("visibilitychange", () => { if (!document.hidden) render(); });
window.addEventListener("hashchange", route);
auth.boot().catch(() => {}).finally(route);

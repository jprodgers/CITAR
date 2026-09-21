// Benchmarks page: scheduler status, live runs grouped by server, and the suite library/editor. Suites pick servers,
// models and load profiles from the server registry (Servers page), which also holds each server's restricted hours.
import { api } from "./api.js";
import { el, clear, toast, modal, confirmBox } from "./util.js";
import { pageHeader, secs, bar } from "./nav.js";
import { openMetrics } from "./metrics.js";
import { seatServers } from "./lobby.js";
import { mapOptionsForm } from "./mapoptions.js";

const STATUS_CLASS = { running: "live", loading: "live", resuming: "live", paused: "warn", queued: "", done: "good", failed: "bad", cancelled: "muted" };
const COLORS = { llm: "#4f8cff", bot: "#e8c547" };
const expanded = new Set();
const collapsed = new Set();

export async function renderBenchmarks(root, rules) {
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("benchmarks"));
  const statusCard = el("div", { class: "card" });
  const runsCard = el("div", { class: "card" });
  const suitesCard = el("div", { class: "card" });
  page.append(statusCard, runsCard, suitesCard);

  const ctx = { rules, refresh: null, refreshSuites: null };
  let lastStatus = "", lastRuns = "";
  async function refresh(force = true) {
    try {
      const [status, runs, settings] = await Promise.all([api.benchStatus(), api.runs(), api.benchSettings()]);
      // redraw only what changed, so buttons aren't replaced under the mouse between polls
      const statusKey = JSON.stringify([status, settings]);
      const runsKey = JSON.stringify([runs, [...expanded], [...collapsed]]);
      if (force || statusKey !== lastStatus) renderStatus(statusCard, status, settings, ctx);
      if (force || runsKey !== lastRuns) renderRuns(runsCard, runs, ctx);
      lastStatus = statusKey;
      lastRuns = runsKey;
    } catch (e) { toast(e.message, "error"); }
  }
  async function refreshSuites() {
    try { renderSuites(suitesCard, await api.suites(), ctx); } catch (e) { toast(e.message, "error"); }
  }
  ctx.refresh = refresh;
  ctx.refreshSuites = refreshSuites;
  await Promise.all([refresh(), refreshSuites()]);
  const timer = setInterval(() => refresh(false), 3000);
  return { destroy() { clearInterval(timer); } };
}

// ---------------------------------------------------------------------------
// status & settings
// ---------------------------------------------------------------------------
function renderStatus(card, status, settings, ctx) {
  clear(card);
  card.appendChild(el("div", { class: "row" },
    el("h2", { style: { margin: 0 } }, "Scheduler"),
    el("span", { class: `pill ${status.active_jobs ? "live" : ""}` }, status.active_jobs ? `● ${status.active_jobs} running` : "idle"),
    status.queued_jobs ? el("span", { class: "muted" }, `${status.queued_jobs} queued`) : null,
    ...(status.restricted || []).map((r) => el("a", { class: "pill quiet", href: "#/servers" }, `🌙 ${r.name} restricted until ${r.until}`)),
    el("span", { class: "muted" }, "Restricted hours are set per server on the Servers page."),
    el("span", { class: "grow" }),
    el("button", { onclick: () => openSettings(settings, ctx) }, "⚙ Scoring"),
    el("button", { onclick: () => openLog(status) }, "Log")));
}

function openSettings(settings, ctx) {
  const s = JSON.parse(JSON.stringify(settings));
  const field = (label, input, hint) => el("div", { class: "field" }, el("label", {}, label), input, hint ? el("span", { class: "muted small" }, hint) : null);
  const w = s.score_weights;
  const body = el("div", { class: "col" },
    el("h3", {}, "Model score weights"),
    el("p", { class: "muted" }, "Overall model score = weighted average of benchmark performance (wins and score share against bots, " +
      "weighted by turns played), reliability (clean turn endings, few errors and repeats) and speed (turn time)."),
    el("div", { class: "grid2" },
      field("Benchmark performance", el("input", { type: "number", step: 0.05, min: 0, value: w.benchmark, oninput: (e) => { w.benchmark = +e.target.value; } })),
      field("Reliability", el("input", { type: "number", step: 0.05, min: 0, value: w.reliability, oninput: (e) => { w.reliability = +e.target.value; } })),
      field("Speed", el("input", { type: "number", step: 0.05, min: 0, value: w.speed, oninput: (e) => { w.speed = +e.target.value; } }))));
  const m = modal({ title: "Scoring settings", content: body, narrow: true, footer: [
    el("button", { onclick: () => m.close() }, "Cancel"),
    el("button", { class: "primary", onclick: async () => {
      try { await api.saveBenchSettings(s); toast("Settings saved"); m.close(); ctx.refresh(); } catch (e) { toast(e.message, "error"); }
    } }, "Save")] });
}

function openLog(status) {
  const body = el("div", { class: "col" }, ...(status.log.length ? [...status.log].reverse().map((l) =>
    el("div", {}, el("span", { class: "muted" }, new Date(l.t * 1000).toLocaleString() + " "), l.msg)) : [el("p", { class: "muted" }, "Nothing yet.")]));
  modal({ title: "Scheduler log", content: body });
}

// ---------------------------------------------------------------------------
// runs
// ---------------------------------------------------------------------------
function renderRuns(card, runs, ctx) {
  clear(card);
  card.appendChild(el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Runs"),
    el("span", { class: "muted" }, "Click a game to watch it live. Games also appear under Games while they run.")));
  if (!runs.length) { card.appendChild(el("p", { class: "muted" }, "No benchmark runs yet. Create or load a suite below and press Run.")); return; }
  for (const run of runs) card.appendChild(renderRun(run, ctx));
}

function renderRun(run, ctx) {
  const active = ["running", "paused"].includes(run.status);
  const open = expanded.has(run.id) || (active && !collapsed.has(run.id));
  const sm = run.summary;
  const counts = Object.entries(sm.counts).map(([k, v]) => `${v} ${k}`).join(" · ");
  const control = (action, label, cls = "small") => el("button", { class: cls, onclick: async (e) => {
    e.stopPropagation();
    if (action === "cancel" && !(await confirmBox("Cancel run", `Cancel "${run.name}"? Games in progress are saved and closed.`))) return;
    if (action === "remove" && !(await confirmBox("Remove run", `Remove "${run.name}" from the list? Its saved games are kept.`))) return;
    try { await api.runControl(run.id, action); ctx.refresh(); } catch (err) { toast(err.message, "error"); }
  } }, label);
  const toggle = () => {
    if (open) { expanded.delete(run.id); collapsed.add(run.id); } else { expanded.add(run.id); collapsed.delete(run.id); }
    ctx.refresh();
  };
  const eta = sm.eta_seconds != null ? `~${secs(sm.eta_seconds)} left${sm.eta_unknown_jobs ? " (+ jobs without timing yet)" : ""}` : "";
  const box = el("div", { class: "run" });
  box.appendChild(el("div", { class: "run-head", onclick: toggle },
    el("span", { class: "caret" }, open ? "▾" : "▸"),
    el("b", {}, run.name),
    el("span", { class: `pill ${STATUS_CLASS[run.status] || ""}` }, run.status),
    el("span", { class: "muted" }, `${run.mode} · started ${new Date(run.created * 1000).toLocaleString()}`),
    el("div", { class: "grow run-progress" }, bar(sm.turns_total ? sm.turns_done / sm.turns_total : 0,
      `${sm.turns_done.toLocaleString()} / ${sm.turns_total.toLocaleString()} turns`)),
    el("span", { class: "muted" }, counts), eta ? el("span", { class: "muted" }, eta) : null,
    run.status === "running" ? control("pause", "⏸ Pause") : null,
    run.status === "paused" ? control("resume", "▶ Resume", "small primary") : null,
    active ? control("cancel", "Cancel", "small danger") : control("remove", "Remove", "small")));
  if (!open) return box;

  for (const sv of run.servers) {
    const jobs = run.jobs.filter((j) => j.server_id === sv.id);
    if (!jobs.length) continue;
    const table = el("table", { class: "list jobs" }, el("tr", {}, ...["Model", "Scenario", "Status", "Progress", "Score vs best bot", "Trend", "Avg turn", "Errors / repeats", "Clean ends", ""].map((h) => el("th", {}, h))));
    for (const job of jobs) table.append(...renderJob(run, job, ctx));
    box.appendChild(el("div", { class: "server-block" },
      el("div", { class: "row server-head" }, el("span", { class: "server-icon" }, sv.provider === "dryrun" ? "🧪" : "🖥"), el("b", {}, sv.name),
        el("span", { class: "muted" }, sv.base_url || sv.provider), el("span", { class: "pill" }, sv.max_parallel > 1 ? `up to ${sv.max_parallel} at once` : "one at a time"),
        sv.restricted_until ? el("span", { class: "pill quiet" }, `🌙 restricted until ${sv.restricted_until}`) : null),
      el("div", { style: { overflowX: "auto" } }, table)));
  }
  return box;
}

function renderJob(run, job, ctx) {
  const p = job.result || job.progress || {};
  const sc = run.scenarios[job.scenario_id] || {};
  const limit = sc.turn_limit || p.turn_limit || 0;
  const played = p.turn ? Math.max(0, p.turn - 1) : 0;
  const watchable = !!job.game_id;
  const watch = async () => {
    if (!watchable) return;
    try {
      const r = await api.watchJob(run.id, job.id);
      location.hash = `#/game/${r.game_id}/${encodeURIComponent(r.spectator_token)}`;
    } catch (e) { toast(e.message, "error"); }
  };
  const act = (action, label, cls = "small") => el("button", { class: cls, onclick: async (e) => {
    e.stopPropagation();
    try { await api.jobControl(run.id, job.id, action); ctx.refresh(); } catch (err) { toast(err.message, "error"); }
  } }, label);
  let statusText = job.status;
  if (job.status === "paused" && job.pause_reason) statusText = `paused (${job.pause_reason === "restricted" ? "restricted hours" : job.pause_reason})`;
  if (job.status === "queued" && job.waiting) statusText = `queued — ${job.waiting.replace(/^waiting: /, "")}`;
  if (job.status === "loading") statusText = "loading model…";
  if (job.status === "running" && p.llm_to_move) statusText = p.agent_status === "thinking" ? "model thinking" : "model's turn";
  else if (job.status === "running") statusText = "bots moving";
  const outcome = job.status === "done" && p.outcome ? el("div", { class: p.outcome === "won" ? "good" : "muted small" }, p.outcome) : null;
  const perf = p.performance;
  const clean = p.turns_played ? Math.round(100 * ((p.end_reasons || {}).end_turn || 0) / p.turns_played) : null;
  const trend = el("canvas", { width: 120, height: 30, class: "spark" });
  drawSpark(trend, p.series || []);
  const row = el("tr", { class: `job ${watchable ? "clickable" : ""}`, title: watchable ? "Click to watch this game" : "", onclick: watch },
    el("td", {}, el("b", {}, job.label), job.profile || job.tool_mode ? el("div", { class: "muted small" }, [job.profile ? `profile: ${job.profile}` : null, job.tool_mode ? `tools: ${job.tool_mode}` : null].filter(Boolean).join(" · ")) : null,
        job.pause_pending ? el("div", { class: "warn small" }, "pausing after this turn (restricted hours)") : null),
    el("td", {}, sc.name || job.scenario_name, run.jobs.some((j) => j.repeat > 1) ? el("span", { class: "muted" }, ` #${job.repeat}`) : null,
      el("div", { class: "muted small" }, `${sc.map_size || ""} ${sc.map_type || ""} · ${sc.opponents || "?"} bot${sc.opponents > 1 ? "s" : ""}`)),
    el("td", {}, el("span", { class: `pill ${STATUS_CLASS[job.status] || ""}` }, statusText), outcome,
      job.error ? el("div", { class: "bad small", title: job.error }, job.error.slice(0, 80)) : null),
    el("td", { style: { minWidth: "130px" } }, limit ? bar(played / limit, `T${played} / ${limit}`) : (p.turn ? `T${played}` : "–")),
    el("td", {}, p.score != null ? el("span", {}, el("b", { style: { color: COLORS.llm } }, p.score), " vs ", el("span", { style: { color: COLORS.bot } }, p.best_bot_score)) : "–",
      perf != null ? el("div", { class: "small" }, bar(perf / 100, `performance ${Math.round(perf)}`, perf >= 50 ? "good" : "bad")) : null),
    el("td", {}, trend),
    el("td", {}, secs(p.avg_turn_s), p.max_turn_s ? el("div", { class: "muted small" }, `max ${secs(p.max_turn_s)}`) : null),
    el("td", {}, p.turns_played ? `${p.avg_errors ?? 0} / ${p.avg_repeats ?? 0}` : "–"),
    el("td", { class: clean == null ? "" : clean >= 90 ? "good" : "warn" }, clean == null ? "–" : `${clean}%`),
    el("td", { class: "actions" },
      watchable ? el("button", { class: "small primary", onclick: (e) => { e.stopPropagation(); watch(); } }, job.status === "done" ? "Recap" : "Watch") : null,
      watchable ? el("button", { class: "small", title: "AI performance stats", onclick: async (e) => {
        e.stopPropagation();
        try { const r = await api.watchJob(run.id, job.id); openMetrics(r.game_id); } catch (err) { toast(err.message, "error"); }
      } }, "📊") : null,
      ["running", "resuming"].includes(job.status) ? act("pause", "⏸") : null,
      job.status === "paused" && run.status === "running" ? act("resume", "▶") : null,
      ["queued", "running", "paused", "loading", "resuming"].includes(job.status) ? act("skip", "Skip") : null,
      ["failed", "cancelled"].includes(job.status) ? act("retry", "Retry") : null));
  const rows = [row];
  if (job.status === "running" && p.last_thought) {
    rows.push(el("tr", { class: "job-note" }, el("td", { colspan: 10, class: "muted small" }, "💭 ", p.last_thought)));
  }
  return rows;
}

function drawSpark(canvas, series) {
  const c = canvas.getContext("2d");
  const W = canvas.width, H = canvas.height;
  c.clearRect(0, 0, W, H);
  if (series.length < 2) { c.fillStyle = "#9aa3b5"; c.font = "10px system-ui"; c.fillText("no data yet", 4, H / 2 + 3); return; }
  const max = Math.max(1, ...series.map((s) => Math.max(s.llm, s.best_bot)));
  const X = (i) => 2 + (W - 4) * i / (series.length - 1);
  const Y = (v) => H - 2 - (H - 4) * v / max;
  for (const [key, color] of [["best_bot", COLORS.bot], ["llm", COLORS.llm]]) {
    c.strokeStyle = color; c.lineWidth = 1.6; c.beginPath();
    series.forEach((s, i) => (i ? c.lineTo(X(i), Y(s[key])) : c.moveTo(X(i), Y(s[key]))));
    c.stroke();
  }
}

// ---------------------------------------------------------------------------
// suites
// ---------------------------------------------------------------------------
function renderSuites(card, suites, ctx) {
  clear(card);
  const importInput = el("input", { type: "file", accept: ".json,application/json", style: { display: "none" }, onchange: async (e) => {
    const file = e.target.files[0];
    if (!file) return;
    try {
      const data = JSON.parse(await file.text());
      delete data.id;
      const saved = await api.saveSuite(data);
      toast(`Imported "${saved.name}"`);
      ctx.refreshSuites();
    } catch (err) { toast(`Import failed: ${err.message}`, "error"); }
  } });
  card.appendChild(el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Benchmark suites"),
    el("span", { class: "muted" }, "Saved configurations: servers, models and scenarios. Swap the models and run again."),
    el("span", { class: "grow" }), importInput,
    el("button", { onclick: () => importInput.click() }, "Import JSON"),
    el("button", { class: "primary", onclick: async () => { ctx.registry = await seatServers(true); openSuiteEditor(await api.newSuite(), ctx, true); } }, "+ New suite")));
  if (!suites.length) { card.appendChild(el("p", { class: "muted" }, "No suites saved yet.")); return; }
  const table = el("table", { class: "list" }, el("tr", {}, ...["Suite", "Servers & models", "Scenarios", "Games", "Updated", ""].map((h) => el("th", {}, h))));
  for (const s of suites) {
    table.appendChild(el("tr", {},
      el("td", {}, el("b", {}, s.name), s.description ? el("div", { class: "muted small" }, s.description) : null, el("div", { class: "muted small" }, s.mode)),
      el("td", {}, ...s.servers.map((sv) => el("div", { class: sv.missing ? "bad" : "" }, el("span", { class: "muted" }, `${sv.name}: `), sv.models.join(", ") || "(none)"))),
      el("td", {}, s.scenarios.join(", ")),
      el("td", {}, s.jobs),
      el("td", {}, s.updated ? new Date(s.updated * 1000).toLocaleString() : "–"),
      el("td", { class: "actions" },
        el("button", { class: "small primary", onclick: () => startRun({ suite_id: s.id }, s.name, s.jobs, ctx) }, "▶ Run"),
        el("button", { class: "small", onclick: async () => { ctx.registry = await seatServers(true); openSuiteEditor(await api.suite(s.id), ctx, false); } }, "Edit"),
        el("button", { class: "small", onclick: async () => {
          const full = await api.suite(s.id);
          delete full.id; full.name = `${full.name} (copy)`;
          await api.saveSuite(full); ctx.refreshSuites();
        } }, "Duplicate"),
        el("button", { class: "small", onclick: async () => downloadJSON(await api.suite(s.id), `${s.name}.json`) }, "Export"),
        el("button", { class: "small danger", onclick: async () => {
          if (await confirmBox("Delete suite", `Delete the suite "${s.name}"? Runs and saved games are kept.`)) { await api.deleteSuite(s.id); ctx.refreshSuites(); }
        } }, "Delete"))));
  }
  card.appendChild(table);
}

async function startRun(body, name, jobs, ctx) {
  if (!(await confirmBox("Start benchmark", `Start "${name}" (${jobs} game${jobs === 1 ? "" : "s"})? Each game plays until it ends or reaches its turn limit.`))) return;
  try {
    const r = await api.startRun(body);
    toast(`Started ${r.jobs} benchmark game${r.jobs === 1 ? "" : "s"}`);
    ctx.refresh();
    window.scrollTo({ top: 0, behavior: "smooth" });
  } catch (e) { toast(e.message, "error"); }
}

function downloadJSON(data, filename) {
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const a = el("a", { href: URL.createObjectURL(blob), download: filename.replace(/[^\w .()-]/g, "_") });
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}

// ---------------------------------------------------------------------------
// suite editor
// ---------------------------------------------------------------------------
function openSuiteEditor(suite, ctx, isNew) {
  const S = JSON.parse(JSON.stringify(suite));
  const rules = ctx.rules;
  const body = el("div", { class: "col suite-editor" });
  const summary = el("span", { class: "muted" });
  const field = (label, input, extra = {}) => el("div", { class: "field", ...extra }, el("label", {}, label), input);
  const newId = (p) => p + Math.random().toString(16).slice(2, 10);

  const updateSummary = () => {
    const models = S.servers.reduce((n, sv) => n + sv.models.filter((m) => m.enabled).length, 0);
    const scen = S.scenarios.filter((s) => s.enabled);
    const games = models * scen.length * (S.repeats || 1);
    const turns = models * (S.repeats || 1) * scen.reduce((n, s) => n + (s.turn_limit || 0), 0);
    summary.textContent = `${games} game${games === 1 ? "" : "s"} (${models} model${models === 1 ? "" : "s"} × ${scen.length} scenario${scen.length === 1 ? "" : "s"} × ${S.repeats || 1} repeat${S.repeats > 1 ? "s" : ""}) · up to ${turns.toLocaleString()} model turns`;
  };

  function draw() {
    clear(body);
    body.appendChild(el("div", { class: "grid2" },
      field("Suite name", el("input", { value: S.name, oninput: (e) => { S.name = e.target.value; } })),
      field("Description", el("input", { value: S.description || "", placeholder: "optional", oninput: (e) => { S.description = e.target.value; } })),
      field("Scheduling", el("select", { onchange: (e) => { S.mode = e.target.value; } },
        el("option", { value: "sequential", selected: S.mode === "sequential" }, "Sequential — one game at a time"),
        el("option", { value: "parallel", selected: S.mode === "parallel" }, "Parallel — servers run at the same time"))),
      field("Repeats", el("input", { type: "number", min: 1, max: 20, value: S.repeats || 1, oninput: (e) => { S.repeats = Math.max(1, +e.target.value || 1); updateSummary(); } }))));

    body.appendChild(el("div", { class: "row section" }, el("h3", { style: { margin: 0 } }, "Servers & models"),
      el("span", { class: "muted" }, "Pick servers from the Servers page and the models to benchmark on each. In parallel mode each server runs its own games at the same time."),
      el("span", { class: "grow" }),
      el("button", { class: "small", onclick: () => {
        const used = new Set(S.servers.map((g) => g.server_id));
        const next = ctx.registry.servers.find((x) => x.models.length && !used.has(x.id));
        if (!next) { toast("Every server with models is already in this suite (add servers on the Servers page).", "error"); return; }
        S.servers.push({ server_id: next.id, models: [] });
        draw();
      } }, "+ Add server")));
    if (!S.servers.length) body.appendChild(el("p", { class: "muted" }, "Add a server to choose models."));
    S.servers.forEach((sv, i) => body.appendChild(serverEditor(sv, i)));

    body.appendChild(el("div", { class: "row section" }, el("h3", { style: { margin: 0 } }, "Scenarios"),
      el("span", { class: "muted" }, "Every model plays every scenario against scripted bots, on the same seed."),
      el("span", { class: "grow" }),
      el("button", { class: "small", onclick: () => {
        const last = S.scenarios[S.scenarios.length - 1] || {};
        S.scenarios.push({ ...JSON.parse(JSON.stringify(last)), id: newId("sc_"), name: `Scenario ${S.scenarios.length + 1}`, enabled: true });
        draw();
      } }, "+ Add scenario")));
    S.scenarios.forEach((sc, i) => body.appendChild(scenarioEditor(sc, i)));
    updateSummary();
  }

  function serverEditor(sv, i) {
    // a suite's server group: a registry server plus the models (and load profiles) to benchmark on it
    const box = el("div", { class: "server-edit" });
    const reg = ctx.registry;
    const server = reg.servers.find((x) => x.id === sv.server_id);
    const used = new Set(S.servers.map((g) => g.server_id));
    const pick = el("select", { onchange: (e) => { sv.server_id = e.target.value; sv.models = []; draw(); } },
      ...reg.servers.filter((x) => x.models.length && (x.id === sv.server_id || !used.has(x.id))).map((x) =>
        el("option", { value: x.id, selected: x.id === sv.server_id }, x.name + (x.restricted_until ? " 🌙" : ""))));
    if (!server) pick.prepend(el("option", { value: sv.server_id || "", selected: true }, "(deleted server — pick another)"));
    box.append(el("div", { class: "row" }, el("span", { class: "server-icon" }, server && server.kind === "test" ? "🧪" : server && server.kind === "api" ? "🔑" : "🖥"),
      pick, server ? el("span", { class: "muted small" }, `${server.connection.max_parallel} game${server.connection.max_parallel > 1 ? "s" : ""} at once · ` +
        (server.pooled ? `connected through the CITAR helper${server.online ? "" : " (offline now)"}; jobs wait while a game is using it`
          : (server.restricted_hours.enabled ? "has restricted hours" : "no restricted hours") + " · change these on the Servers page")) : null,
      el("span", { class: "grow" }),
      el("button", { class: "small danger", onclick: () => { S.servers.splice(i, 1); draw(); } }, "Remove server")));
    if (!server) return box;
    if (server.kind === "api" && !(server.key_status || {}).present) box.appendChild(el("div", { class: "bad small" }, "This server has no API key yet."));
    const rows = el("div", { class: "col", style: { marginTop: "6px" } });
    for (const m of server.models.filter((x) => x.enabled || sv.models.some((y) => y.model_id === x.id))) {
      let entry = sv.models.find((y) => y.model_id === m.id);
      const include = el("input", { type: "checkbox", checked: !!(entry && entry.enabled), onchange: (e) => {
        if (!entry) { entry = { id: newId("sm_"), model_id: m.id, model: m.key, profile_id: m.default_profile, enabled: true, label: "", persona: "" }; sv.models.push(entry); }
        entry.enabled = e.target.checked;
        updateSummary(); draw();
      } });
      const line = el("div", { class: "model-row" }, el("label", { class: "row", style: { flexWrap: "nowrap" } }, include, el("code", {}, m.label || m.key)),
        el("span", { class: "muted small" }, [m.info && m.info.params, m.info && m.info.size_gb ? m.info.size_gb + " GB" : null].filter(Boolean).join(" · ")));
      if (entry && entry.enabled) {
        if (m.profiles && m.profiles.length) line.append(el("select", { title: "Load profile", onchange: (e) => { entry.profile_id = e.target.value; } },
          ...m.profiles.map((p) => el("option", { value: p.id, selected: p.id === (entry.profile_id || m.default_profile) }, `profile: ${p.name} (${p.context ? (p.context / 1024).toFixed(0) + "k" : "default"})`))));
        if (server.connection.provider === "anthropic") line.append(el("select", { title: "Effort", onchange: (e) => { entry.effort = e.target.value; } },
          ...["", "low", "medium", "high", "xhigh", "max"].map((v) => el("option", { value: v, selected: (entry.effort || "") === v }, `effort: ${v || "server default"}`))));
        else if (server.connection.provider !== "dryrun") line.append(el("select", { title: "Reasoning effort", onchange: (e) => { entry.reasoning_effort = e.target.value; } },
          ...[["", "reasoning: server default"], ["low", "reasoning: low"], ["medium", "reasoning: medium"], ["high", "reasoning: high"]].map(([v, t]) => el("option", { value: v, selected: (entry.reasoning_effort || "") === v }, t))));
        line.append(el("input", { value: entry.label || "", placeholder: "display name (optional)", oninput: (e) => { entry.label = e.target.value; } }));
      }
      rows.appendChild(line);
    }
    if (!server.models.length) rows.appendChild(el("span", { class: "muted small" }, "This server has no models in its catalog: add them on the Servers page."));
    box.appendChild(rows);
    return box;
  }

  function scenarioEditor(sc, i) {
    const sel = (key, opts) => el("select", { onchange: (e) => { sc[key] = e.target.value; } }, ...opts.map(([v, t]) => el("option", { value: v, selected: sc[key] === v }, t)));
    const num = (key, attrs = {}) => el("input", { type: "number", value: sc[key] ?? "", ...attrs, oninput: (e) => { sc[key] = e.target.value === "" ? null : +e.target.value; updateSummary(); } });
    if (!sc.victories || Object.keys(sc.victories).some((k) => !rules.victories[k])) sc.victories = Object.fromEntries(Object.keys(rules.victories).map((k) => [k, true]));
    if (!rules.speeds[sc.speed]) sc.speed = rules.benchmark_speed;
    sc.difficulty = sc.difficulty || rules.default_difficulty;
    sc.bot_difficulty = sc.bot_difficulty || "";
    sc.barbarian_difficulty = sc.barbarian_difficulty || "";
    if (sc.nations == null) sc.nations = "BenchmarkCiv";
    const vic = (k, label) => el("label", {}, el("input", { type: "checkbox", checked: sc.victories[k] !== false, onchange: (e) => { sc.victories[k] = e.target.checked; } }), ` ${label}`);
    return el("div", { class: `scenario-edit ${sc.enabled ? "" : "disabled"}` },
      el("div", { class: "row" },
        el("label", {}, el("input", { type: "checkbox", checked: sc.enabled, onchange: (e) => { sc.enabled = e.target.checked; draw(); } }), " "),
        el("input", { value: sc.name, style: { fontWeight: 600, minWidth: "220px" }, oninput: (e) => { sc.name = e.target.value; } }),
        el("span", { class: "grow" }),
        el("button", { class: "small danger", disabled: S.scenarios.length <= 1, onclick: () => { S.scenarios.splice(i, 1); draw(); } }, "Remove")),
      el("div", { class: "grid-scenario" },
        field("Map size", sel("map_size", Object.entries(rules.map_sizes).map(([k, v]) => [k, `${v.name} (${v.width}×${v.height})`]))),
        field("Map type", sel("map_type", Object.entries(rules.map_types).map(([k, v]) => [k, v.name]))),
        field("Speed", sel("speed", Object.keys(rules.speeds).map((k) => [k, `${k} (${rules.max_turns[k]} turns)`]))),
        field("Difficulty (model)", sel("difficulty", rules.difficulty_list.map((k) => [k, k]))),
        field("Bot difficulty", sel("bot_difficulty", [["", "Same as model"], ...rules.difficulty_list.map((k) => [k, k])])),
        field("Barbarian difficulty", sel("barbarian_difficulty", [["", "Same as model"], ...rules.difficulty_list.map((k) => [k, k])])),
        field("Civilizations", sel("nations", [["BenchmarkCiv", "BenchmarkCiv for every seat"], ["random", "Random civilizations"],
          ...rules.major_nations.filter((n) => n !== "BenchmarkCiv").sort().map((n) => [n, `Everyone plays ${n}`])])),
        field("Barbarians", sel("barbarians", Object.entries(rules.barbarian_levels).map(([k, v]) => [k, v]))),
        field("Bot opponents", num("opponents", { min: 1, max: 23 })),
        field("Bot aggression (0–1)", num("bot_aggression", { min: 0, max: 1, step: 0.1 })),
        field("Seed (blank = random)", num("seed")),
        field("Turn limit (0 = speed's normal length)", num("turn_limit", { min: 0, max: 2000 })),
        field("Max minutes per model turn", num("max_turn_minutes", { min: 1, max: 600 })),
        el("div", { class: "field" }, el("label", {}, "Victory conditions"), el("div", { class: "row" }, ...Object.keys(rules.victories).map((k) => vic(k, k))))),
      mapOptionsForm(rules, {
        initial: { map_edges: sc.map_edges, river_density: sc.river_density, resources: sc.resources },
        onChange: (v) => { sc.map_edges = v.map_edges; sc.river_density = v.river_density; sc.resources = v.resources; },
      }).node);
  }

  draw();
  const save = async (asCopy) => {
    const data = JSON.parse(JSON.stringify(S));
    if (asCopy) { delete data.id; data.name = `${data.name} (copy)`; }
    return api.saveSuite(data);
  };
  const m = modal({ title: isNew ? "New benchmark suite" : `Edit suite — ${S.name}`, content: body, footer: [
    summary, el("span", { class: "grow" }),
    el("button", { onclick: () => m.close() }, "Close"),
    isNew ? null : el("button", { onclick: async () => { try { await save(true); toast("Saved a copy"); ctx.refreshSuites(); m.close(); } catch (e) { toast(e.message, "error"); } } }, "Save as copy"),
    el("button", { onclick: async () => { try { const r = await save(false); S.id = r.id; toast("Suite saved"); ctx.refreshSuites(); m.close(); } catch (e) { toast(e.message, "error"); } } }, "Save"),
    el("button", { class: "primary", onclick: async () => {
      try {
        const r = await save(false); S.id = r.id; ctx.refreshSuites();
        const jobs = r.servers.reduce((n, sv) => n + sv.models.filter((x) => x.enabled).length, 0) * r.scenarios.filter((x) => x.enabled).length * r.repeats;
        m.close();
        await startRun({ suite_id: r.id }, r.name, jobs, ctx);
      } catch (e) { toast(e.message, "error"); }
    } }, "Save & run")] });
}

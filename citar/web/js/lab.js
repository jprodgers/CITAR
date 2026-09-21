// Lab page: live view of the bot-tuning lab (citar.lab): runner health, experiment progress and ETAs,
// games in flight with their current turn, side runs (LLM benchmark jobs) and the runner log.
import { api } from "./api.js";
import { el, clear, toast, keepPlace } from "./util.js";
import { pageHeader, secs, bar } from "./nav.js";

const openReports = new Map();   // experiment name -> report text (null while loading)
let labAutoRefresh = true;

function clock(minutesFromNow) {
  if (minutesFromNow == null) return "–";
  const t = new Date(Date.now() + minutesFromNow * 60000);
  const sameDay = t.toDateString() === new Date().toDateString();
  const hm = t.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return sameDay ? hm : `${t.toLocaleDateString([], { weekday: "short" })} ${hm}`;
}

function eta(minutes) {
  if (minutes == null) return "–";
  if (minutes <= 0.5) return "finishing";
  return `${secs(minutes * 60)} (≈ ${clock(minutes)})`;
}

const STATE = {
  running: ["live", "running"], queued: ["", "queued"], complete: ["good", "complete"],
  stalled: ["bad", "stalled"],
};

export async function renderLab(root) {
  const page = el("div", { class: "lobby lab" });
  root.appendChild(page);
  page.appendChild(pageHeader("lab"));
  const top = el("div", { class: "card" });
  const exps = el("div", { class: "card" });
  const games = el("div", { class: "card" });
  const side = el("div", { class: "card" });
  const logc = el("div", { class: "card" });
  page.append(top, exps, games, side, logc);
  let data = null;
  const refresh = async () => {
    try { data = await api.lab(); } catch (e) { top.replaceChildren(el("p", { class: "bad" }, "Lab status unavailable: " + e.message)); return; }
    keepPlace(top, () => drawTop(top, data));
    keepPlace(exps, () => drawExperiments(exps, data, refresh));
    keepPlace(games, () => drawGames(games, data));
    keepPlace(side, () => drawSide(side, data));
    keepPlace(logc, () => drawLog(logc, data));
  };
  await refresh();
  const timer = setInterval(() => { if (labAutoRefresh) refresh(); }, 5000);
  return { destroy() { clearInterval(timer); } };
}

function drawTop(card, d) {
  clear(card);
  const r = d.runner;
  const stale = r.updated_age != null && r.updated_age > 120;
  const health = !r.pid ? el("span", { class: "pill muted" }, "runner never started")
    : r.alive && !stale ? el("span", { class: "pill live" }, `● runner running (pid ${r.pid})`)
    : r.alive ? el("span", { class: "pill warn" }, `runner silent for ${secs(r.updated_age)}`)
    : el("span", { class: "pill bad" }, "runner stopped");
  const active = d.experiments.filter((e) => e.state === "running" || e.state === "queued");
  const stalled = d.experiments.filter((e) => e.state === "stalled");
  card.append(
    el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Bot-tuning lab"), health,
      el("span", { class: "grow" }), el("span", { class: "muted small" }, `updated ${new Date(d.now).toLocaleTimeString()}`),
      el("label", { class: "small", title: "Turn off to freeze the page while reading" },
        el("input", { type: "checkbox", checked: labAutoRefresh, onchange: (e) => { labAutoRefresh = e.target.checked; } }), " live updates")),
    el("div", { class: "lab-stats" },
      stat("Games running", `${d.running.length}${r.workers ? ` / ${r.workers} workers` : ""}`),
      stat("Finished in the last hour", r.games_last_hour ?? "–"),
      stat("Experiments queued", active.length),
      stat("Queue finishes in", active.length ? eta(d.eta_minutes) : "queue empty",
        "Estimate: remaining games × median minutes per game for that experiment, spread over the workers."),
      stalled.length ? stat("Stalled", stalled.map((e) => e.name).join(", "), "Games that crashed 3 times; see Crashes in the log.", "bad") : null),
    el("p", { class: "muted small" }, "Each game is a separate bot-vs-bot simulation on the laptop CPU. This page refreshes every 5 seconds; " +
      "ETAs assume later turns are slower (more cities and units) and firm up as each experiment finishes games."));
}

function stat(label, value, title, cls = "") {
  return el("div", { class: `lab-stat ${cls}`, title: title || "" }, el("div", { class: "muted small" }, label), el("div", { class: "big" }, `${value}`));
}

function drawExperiments(card, d, refresh) {
  clear(card);
  card.appendChild(el("h3", {}, "Experiments"));
  const table = el("table", { class: "list jobs" }, el("tr", {},
    el("th", {}, "Experiment"), el("th", {}, "State"), el("th", { style: { minWidth: "180px" } }, "Progress"),
    el("th", {}, "Min / game"), el("th", {}, "Finishes"), el("th", {}, "What it tests"), el("th", {}, "")));
  for (const e of d.experiments) {
    const [cls, label] = STATE[e.state] || ["", e.state];
    const expanded = openReports.has(e.name);
    const what = e.kind === "factorial" ? `factors: ${e.factors.join(", ")}` : `seats: ${e.seats.join(" / ")}`;
    table.appendChild(el("tr", { class: "clickable", onclick: () => toggleReport(e.name, refresh) },
      el("td", {}, el("b", {}, e.name), e.note ? el("div", { class: "muted small" }, e.note) : null),
      el("td", {}, el("span", { class: `pill ${cls}` }, label),
        e.failing.length ? el("div", { class: "bad small" }, `crashing: #${e.failing.join(", #")}`) : null),
      el("td", {}, bar(e.done / e.games, `${e.done} / ${e.games}${e.running ? ` (+${e.running} running)` : ""}`, e.state === "complete" ? "good" : "")),
      el("td", {}, e.minutes_per_game != null ? `${e.minutes_per_game}` : "–"),
      el("td", {}, e.state === "complete" ? el("span", { class: "muted" }, e.finished ? new Date(e.finished).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "done")
        : e.state === "stalled" ? el("span", { class: "bad" }, "never (crashing games)") : eta(e.eta_minutes)),
      el("td", { class: "small" }, what, e.turns ? el("span", { class: "muted" }, ` · ${e.turns} turns`) : null),
      el("td", {}, e.done ? (expanded ? "▾ report" : "▸ report") : "")));
    if (expanded) {
      const text = openReports.get(e.name);
      table.appendChild(el("tr", {}, el("td", { colspan: 7 },
        el("pre", { class: "lab-report" }, text == null ? "loading…" : text))));
    }
  }
  card.appendChild(table);
}

async function toggleReport(name, refresh) {
  if (openReports.has(name)) { openReports.delete(name); refresh(); return; }
  openReports.set(name, null);
  refresh();
  try { openReports.set(name, (await api.labReport(name)).text); } catch (e) { openReports.set(name, e.message); }
  refresh();
}

function drawGames(card, d) {
  clear(card);
  card.appendChild(el("h3", {}, `Games in progress (${d.running.length})`));
  if (!d.running.length) { card.appendChild(el("p", { class: "muted" }, "No lab games running.")); return; }
  const table = el("table", { class: "list jobs" }, el("tr", {},
    el("th", {}, "Game"), el("th", { style: { minWidth: "200px" } }, "Turn"), el("th", {}, "Running for"), el("th", {}, "Est. left"), el("th", {}, "Attempt")));
  for (const g of d.running) {
    const frac = g.turn && g.limit ? g.turn / g.limit : null;
    const left = g.left_minutes;
    table.appendChild(el("tr", {},
      el("td", {}, `${g.exp} #${g.i}`),
      el("td", {}, g.turn != null ? bar(frac, `turn ${g.turn}${g.limit ? ` / ${g.limit}` : ""}`) : el("span", { class: "muted small" }, "started before turn reporting")),
      el("td", {}, secs(g.minutes * 60)),
      el("td", {}, left != null ? secs(left * 60) : "–"),
      el("td", {}, g.attempt > 1 ? el("span", { class: "pill warn" }, `${g.attempt} of 3`) : "1")));
  }
  card.appendChild(table);
}

function drawSide(card, d) {
  clear(card);
  card.appendChild(el("h3", {}, "LLM and other side runs"));
  if (!d.side_runs.length) { card.appendChild(el("p", { class: "muted" }, "None.")); return; }
  for (const s of d.side_runs) {
    card.append(el("div", { class: "row" }, el("b", {}, s.name),
      el("span", { class: `pill ${s.active ? "live" : "muted"}` }, s.active ? "● running" : "finished"),
      el("span", { class: "muted small" }, `last output ${new Date(s.modified).toLocaleString()}`)),
      el("pre", { class: "lab-report" }, s.tail.join("\n")));
  }
}

function drawLog(card, d) {
  clear(card);
  card.append(el("h3", {}, "Runner log"), el("pre", { class: "lab-report" }, d.log.slice().reverse().join("\n")));
}

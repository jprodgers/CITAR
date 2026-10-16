// Lab page: live view of the bot-tuning lab (citar.lab), played inside the server by its job dispatcher
// (citar/jobs/dispatcher.py, design/phase4-helper.md P4.5.5): the dispatcher's health and its executors (this server's
// job host, under the host machine's CPU settings), experiment progress and ETAs, each experiment's games with the
// executor that played them and their verification, games in flight with their round, side runs (LLM benchmark jobs)
// and the dispatcher's log. Administrators stop and resume the lab here.
import { api } from "./api.js";
import { el, clear, toast, keepPlace } from "./util.js";
import { pageHeader, secs, bar } from "./nav.js";
import { abDialog } from "./bots.js";
import * as auth from "./auth.js";
import { JOB_STATE, executorText, progressText, verificationBadge } from "./labrules.js";

const openReports = new Map();   // experiment name -> report text (null while loading)
const openGames = new Map();     // experiment name -> its jobs (null while loading)
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
  running: ["live", "● running"], queued: ["", "queued"], complete: ["good", "✓ complete"],
  stalled: ["bad", "stalled"], refused: ["bad", "cannot be played"],
};

export async function renderLab(root, rules) {
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
    for (const name of openGames.keys()) loadGames(name);
    keepPlace(top, () => drawTop(top, data, rules, refresh));
    keepPlace(exps, () => drawExperiments(exps, data, refresh));
    keepPlace(games, () => drawGames(games, data));
    keepPlace(side, () => drawSide(side, data));
    keepPlace(logc, () => drawLog(logc, data));
  };
  await refresh();
  const timer = setInterval(() => { if (labAutoRefresh) refresh(); }, 5000);
  return { destroy() { clearInterval(timer); } };
}

function drawTop(card, d, rules, refresh) {
  clear(card);
  const disp = d.dispatcher || {};
  const executors = disp.executors || [];
  const threads = executors.filter((e) => e.on).reduce((n, e) => n + (e.threads || 0), 0);
  const health = !disp.alive ? el("span", { class: "pill bad" }, "the dispatcher is not running")
    : disp.paused ? el("span", { class: "pill warn" }, "paused: playing games finish, nothing new starts")
      : el("span", { class: "pill live" }, "● playing");
  const admin = auth.isAdmin();
  const toggle = admin && disp.alive
    ? el("button", {
      title: disp.paused ? "Start new games again" : "Stop starting new games (the ones playing finish)",
      onclick: async () => {
        try { await (disp.paused ? api.labResume() : api.labPause()); refresh(); } catch (e) { toast(e.message, "error"); }
      },
    }, disp.paused ? "Resume" : "Stop")
    : null;
  const active = d.experiments.filter((e) => e.state === "running" || e.state === "queued");
  const stalled = d.experiments.filter((e) => e.state === "stalled" || e.state === "refused");
  card.append(
    el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Bot-tuning lab"), health, toggle,
      el("span", { class: "grow" }), el("span", { class: "muted small" }, `updated ${new Date(d.now).toLocaleTimeString()}`),
      rules ? el("button", { title: "Queue an experiment between bot profiles", onclick: () => abDialog(rules) }, "A/B test…") : null,
      el("label", { class: "small", title: "Turn off to freeze the page while reading" },
        el("input", { type: "checkbox", checked: labAutoRefresh, onchange: (e) => { labAutoRefresh = e.target.checked; } }), " live updates")),
    disp.paused ? el("p", { class: "warn small" }, `Paused${disp.paused.by ? ` by ${disp.paused.by}` : ""}`
      + `${disp.paused.at ? ` at ${new Date(disp.paused.at).toLocaleString()}` : ""}.`
      + " The games playing finish; nothing new starts until it is resumed (here, or `citar lab resume`).") : null,
    el("div", { class: "lab-stats" },
      stat("Games playing", `${d.running.length}${threads ? ` / ${threads} threads` : ""}`),
      stat("Finished in the last hour", disp.games_last_hour ?? "–"),
      stat("Experiments queued", active.length),
      stat("Queue finishes in", active.length ? eta(d.eta_minutes) : "queue empty",
        "Estimate: remaining games × median minutes per game for that experiment, spread over the threads."),
      stalled.length ? stat("Stalled", stalled.map((e) => e.name).join(", "), "Games that failed three times, or an experiment that cannot be played; see the log.", "bad") : null),
    el("div", { class: "lab-executors" }, ...executors.map((e) => el("div", { class: "small" },
      el("span", { class: `pill ${e.state === "ready" && !e.available ? "live" : e.state === "failed" ? "bad" : ""}` }, e.kind === "local" ? "this server" : e.name),
      " ", executorText(e),
      e.verifiable === false ? el("span", { class: "muted", title: e.verify_reason || "" }, " · its self-check failed: it plays its own lab and verifies nobody") : null))),
    el("p", { class: "muted small" }, "Each game is a bot-vs-bot job the server plays on its own job host, under the host's CPU settings "
      + "(Servers page, Machine settings: threads, CPU share, memory). This page refreshes every 5 seconds; "
      + "ETAs assume later turns are slower (more cities and units) and firm up as each experiment finishes games."));
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
    const shown = openGames.has(e.name);
    const what = e.kind === "factorial" ? `factors: ${e.factors.join(", ")}`
      : e.kind === "reference" ? "reference run (the baseline's own seats)" : `seats: ${e.seats.join(" / ")}`;
    table.appendChild(el("tr", { class: "clickable", onclick: () => toggleReport(e.name, refresh) },
      el("td", {}, el("b", {}, e.name), e.note ? el("div", { class: "muted small" }, e.note) : null),
      el("td", {}, el("span", { class: `pill ${cls}` }, label),
        e.failing.length ? el("div", { class: "bad small" }, `failed: #${e.failing.join(", #")}`) : null,
        e.crashed && e.crashed.length ? el("div", { class: "muted small" }, `crashed (final): #${e.crashed.join(", #")}`) : null,
        e.refused ? el("div", { class: "bad small" }, e.refused) : null,
        e.waiting && e.state === "queued" ? el("div", { class: "muted small" }, `waiting: ${e.waiting}`) : null),
      el("td", {}, bar(e.done / e.games, progressText(e), e.state === "complete" ? "good" : "")),
      el("td", {}, e.minutes_per_game != null ? `${e.minutes_per_game}` : "–"),
      el("td", {}, e.state === "complete" ? el("span", { class: "muted" }, e.finished ? new Date(e.finished).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "done")
        : e.state === "stalled" ? el("span", { class: "bad" }, "never (failing games)")
        : e.state === "refused" ? el("span", { class: "bad" }, "never (delete it and queue it again)") : eta(e.eta_minutes)),
      el("td", { class: "small" }, what, e.turns ? el("span", { class: "muted" }, ` · ${e.turns} turns`) : null),
      el("td", {},
        el("button", { class: "small", title: "Each game, the machine that played it and its verification",
          onclick: (ev) => { ev.stopPropagation(); toggleGames(e.name, refresh); } }, shown ? "▾ games" : "▸ games"),
        e.done && e.kind !== "reference" ? el("span", { class: "muted small" }, expanded ? " ▾ report" : " ▸ report") : null)));
    if (shown) {
      table.appendChild(el("tr", {}, el("td", { colspan: 7 }, gamesTable(openGames.get(e.name)))));
    }
    if (expanded) {
      const text = openReports.get(e.name);
      table.appendChild(el("tr", {}, el("td", { colspan: 7 },
        el("pre", { class: "lab-report" }, text == null ? "loading…" : text))));
    }
  }
  card.appendChild(table);
}

function gamesTable(jobs) {
  if (jobs == null) return el("p", { class: "muted" }, "loading…");
  if (jobs.error) return el("p", { class: "bad" }, jobs.error);
  if (!jobs.length) return el("p", { class: "muted" }, "No game of it has been queued as a job yet.");
  const shown = jobs.slice(0, 300);
  const table = el("table", { class: "list jobs" }, el("tr", {},
    el("th", {}, "Game"), el("th", {}, "State"), el("th", {}, "Played by"), el("th", {}, "Verification"),
    el("th", {}, "Round / time"), el("th", {}, "Attempts")));
  for (const j of shown) {
    const [cls, label] = JOB_STATE[j.state] || ["", j.state];
    const badge = verificationBadge(j.verification);
    table.appendChild(el("tr", {},
      el("td", {}, `#${j.i}`),
      el("td", {}, el("span", { class: `pill ${cls}` }, label), j.error ? el("div", { class: "bad small" }, j.error) : null),
      el("td", {}, j.executor ? j.executor.name : el("span", { class: "muted" }, "–")),
      el("td", {}, badge ? el("span", { class: `pill ${badge[0]}`, title: badge[2] }, badge[1]) : el("span", { class: "muted" }, "–")),
      el("td", { class: "small" }, j.round != null ? `round ${j.round}${j.of ? ` / ${j.of}` : ""}${j.paused ? " (paused)" : ""}`
        : j.wall_s != null ? `${j.wall_s} s` : "–"),
      el("td", {}, j.attempts ? `${j.attempts}` : "–")));
  }
  return el("div", { class: "table-wrap" }, table,
    jobs.length > shown.length ? el("p", { class: "muted small" }, `and ${jobs.length - shown.length} more`) : null);
}

async function loadGames(name) {
  try { openGames.set(name, (await api.labJobs(name)).jobs); } catch (e) { openGames.set(name, { error: e.message }); }
}

async function toggleGames(name, refresh) {
  if (openGames.has(name)) { openGames.delete(name); refresh(); return; }
  openGames.set(name, null);
  await loadGames(name);
  refresh();
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
  if (!d.running.length) { card.appendChild(el("p", { class: "muted" }, "No lab games playing.")); return; }
  const table = el("table", { class: "list jobs" }, el("tr", {},
    el("th", {}, "Game"), el("th", {}, "Played by"), el("th", { style: { minWidth: "200px" } }, "Round"), el("th", {}, "Playing for"),
    el("th", {}, "Est. left"), el("th", {}, "Attempt")));
  for (const g of d.running) {
    const frac = g.turn && g.limit ? g.turn / g.limit : null;
    const left = g.left_minutes;
    table.appendChild(el("tr", {},
      el("td", {}, `${g.exp} #${g.i}`),
      el("td", {}, g.executor || "–"),
      el("td", {}, g.turn != null ? bar(frac, `round ${g.turn}${g.limit ? ` / ${g.limit}` : ""}${g.paused ? " (paused)" : ""}`)
        : el("span", { class: "muted small" }, "starting")),
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
      el("span", { class: `pill ${s.active ? "live" : "good"}` }, s.active ? "● running" : "✓ finished"),
      el("span", { class: "muted small" }, `last output ${new Date(s.modified).toLocaleString()}`)),
      el("pre", { class: "lab-report" }, s.tail.join("\n")));
  }
}

function drawLog(card, d) {
  clear(card);
  card.append(el("h3", {}, "Lab log"), el("pre", { class: "lab-report" }, d.log.slice().reverse().join("\n")));
}

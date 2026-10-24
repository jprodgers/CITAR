// Models page: overall model scores (benchmark-weighted leaderboard) and the detailed metric comparison, and the
// Diplomacy tab (#/models?tab=diplomacy): hybrid benchmark jobs against their bot-only control games, per model and
// owners preset (design/phase3-features.md P3.6.10).
import { api } from "./api.js";
import { el, clear, toast } from "./util.js";
import { pageHeader, secs, bar } from "./nav.js";
import { renderComparison } from "./metrics.js";
import { helperCard } from "./helper.js";
import { boardRows } from "./diplomacy.js";
import { UNVERIFIED_MACHINE } from "./trustrules.js";

/** The badge of a result played on a machine no administrator has trusted yet (P4.5.8 rule 11). */
function unverifiedBadge() {
  return el("span", { class: "pill warn small", title: UNVERIFIED_MACHINE }, "unverified machine");
}

/** The games or pairs a board left out for their machines (the server counts them, never names them). */
function leftOut(list, what) {
  if (!list || !list.length) return null;
  return el("details", {}, el("summary", { class: "muted" }, `Left out: ${what} on machines not trusted yet`),
    el("p", { class: "muted small" }, UNVERIFIED_MACHINE),
    el("ul", { class: "muted small" }, ...list.map((u) => el("li", {}, el("b", {}, u.owners ? `${u.model} · ${u.owners}`
      : u.model), ": ", u.reason))));
}

const open = new Set();
const openLines = new Set();

export async function renderModels(root) {
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("models"));
  const tab = new URLSearchParams(location.hash.split("?")[1] || "").get("tab") === "diplomacy" ? "diplomacy" : "scores";
  page.appendChild(el("div", { class: "tabbar" },
    ...[["scores", "Model scores"], ["diplomacy", "Diplomacy"]].map(([k, label]) =>
      el("button", { class: tab === k ? "active" : "", onclick: () => { location.hash = k === "scores" ? "#/models" : `#/models?tab=${k}`; } },
        label))));
  if (tab === "diplomacy") return renderDiplomacy(page);
  const board = el("div", { class: "card" });
  const compare = el("div", { class: "card" });
  // models live on the machines people connect: the way to add one is the helper
  page.append(helperCard({ compact: true }), board, compare);
  const refresh = async () => {
    try { drawBoard(board, await api.modelScores(), refresh); } catch (e) { toast(e.message, "error"); }
  };
  await refresh();
  renderComparison(compare);
  const timer = setInterval(refresh, 15000);
  return { destroy() { clearInterval(timer); } };
}

function scoreCell(v, title) {
  if (v == null) return el("td", { class: "muted", title }, "–");
  return el("td", { title, style: { minWidth: "110px" } }, bar(v / 100, `${Math.round(v)}`, v >= 60 ? "good" : v >= 40 ? "" : "bad"));
}

function drawBoard(card, data, refresh) {
  clear(card);
  const w = data.weights;
  const total = (w.benchmark + w.reliability + w.speed) || 1;
  const pct = (x) => `${Math.round(100 * x / total)}%`;
  card.appendChild(el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Model scores"),
    el("span", { class: "muted" }, `Overall = benchmark performance ${pct(w.benchmark)} + reliability ${pct(w.reliability)} + speed ${pct(w.speed)}. ` +
      "Change the weights under Benchmarks → Quiet hours & scoring."),
    el("span", { class: "grow" }), el("button", { class: "small", onclick: refresh }, "Refresh")));
  // a member's figures: their own games and the ones other members pool (the server says what that held)
  if (data.scope) card.appendChild(el("p", { class: "muted small" }, `From ${data.scope.description}. Other members' games count ` +
    `only where at least ${data.scope.min_contributors} accounts are behind a model, and are never named.`));
  const withheld = (data.withheld || []).map((w) => el("li", {}, el("b", {}, w.model), ": ", w.reason));
  if (!data.models.length) {
    card.appendChild(el("p", { class: "muted" }, "No AI games recorded yet. Run a benchmark suite to score models."));
    if (withheld.length) card.appendChild(el("ul", { class: "muted small" }, ...withheld));
    const left = leftOut(data.unverified, "games");
    if (left) card.appendChild(left);
    return;
  }
  const help = {
    overall: "Weighted mix of the three scores. Needs at least one benchmark game.",
    benchmark: "Benchmark games against scripted bots: 100 = win, 0 = eliminated, otherwise the model's share of the score against the strongest bot (50 = level). Weighted by turns played, so long games count most.",
    reliability: "Clean turn endings, few rejected orders and little looping, across all games.",
    speed: "Average turn time on a log scale: 20 seconds or faster = 100, 30 minutes or slower = 0.",
  };
  const table = el("table", { class: "list leaderboard" }, el("tr", {},
    el("th", {}, "#"), el("th", {}, "Model"), el("th", { title: help.overall }, "Overall ⓘ"), el("th", { title: help.benchmark }, "Benchmark ⓘ"),
    el("th", { title: help.reliability }, "Reliability ⓘ"), el("th", { title: help.speed }, "Speed ⓘ"),
    el("th", {}, "Benchmark games"), el("th", {}, "Benchmark turns"), el("th", {}, "Avg turn"), el("th", {}, "")));
  data.models.forEach((r, i) => {
    const expanded = open.has(r.model);
    const m = r.metrics;
    table.appendChild(el("tr", { class: "clickable", onclick: () => { expanded ? open.delete(r.model) : open.add(r.model); drawBoard(card, data, refresh); } },
      el("td", {}, r.overall != null ? i + 1 : ""),
      el("td", {}, el("b", {}, r.model), r.overall == null ? el("div", { class: "muted small" }, r.benchmark_games ? "needs AI turn metrics" : "no benchmark games yet") : null),
      scoreCell(r.overall, help.overall), scoreCell(r.benchmark, help.benchmark), scoreCell(r.reliability, help.reliability), scoreCell(r.speed, help.speed),
      el("td", {}, `${r.benchmark_games}`, r.benchmark_finished ? el("span", { class: "muted" }, ` (${r.benchmark_finished} finished${r.wins ? `, ${r.wins} won` : ""})`) : null,
        r.pooled_games ? el("div", { class: "muted small" }, `${r.pooled_games} pooled`) : null),
      el("td", {}, r.benchmark_turns.toLocaleString(), r.benchmark_turns && r.benchmark_turns < 50 ? el("div", { class: "warn small" }, "provisional — short games") : null),
      el("td", {}, m ? secs(m.avg_turn_s) : "–"),
      el("td", {}, r.games.length ? (expanded ? "▾" : "▸") : "")));
    if (expanded && r.games.length) {
      const inner = el("table", { class: "list" }, el("tr", {}, ...["Game", "Scenario", "Turns", "Result", "Score vs best bot", "Performance"].map((h) => el("th", {}, h))),
        ...r.games.map((g) => el("tr", {},
          el("td", {}, g.game, g.unverified_machine ? [" ", unverifiedBadge()] : null), el("td", {}, g.scenario || "–"),
          el("td", {}, `${g.turns}${g.turn_limit ? " / " + g.turn_limit : ""}`),
          el("td", {}, g.phase === "playing" ? "in progress" : (g.performance === 100 ? el("span", { class: "good" }, "won") : "finished")),
          el("td", {}, g.score != null ? `${g.score} vs ${g.best_bot_score}` : "–"),
          el("td", { style: { minWidth: "110px" } }, bar(g.performance / 100, `${Math.round(g.performance)}`)))));
      table.appendChild(el("tr", { class: "detail" }, el("td", { colspan: 10 }, inner)));
    }
  });
  card.appendChild(el("div", { style: { overflowX: "auto" } }, table));
  if (withheld.length) card.appendChild(el("details", {}, el("summary", { class: "muted" }, "Not enough pooled data yet"),
    el("ul", { class: "muted small" }, ...withheld)));
  const left = leftOut(data.unverified, "games");
  if (left) card.appendChild(left);
}

// ---------------------------------------------------------------------------- the Diplomacy tab
async function renderDiplomacy(page) {
  const card = el("div", { class: "card" });
  page.append(card);
  const refresh = async () => {
    try { drawDiplomacy(card, await api.modelDiplomacy(), refresh); } catch (e) { toast(e.message, "error"); }
  };
  await refresh();
  const timer = setInterval(refresh, 30000);
  return { destroy() { clearInterval(timer); } };
}

function drawDiplomacy(card, data, refresh) {
  clear(card);
  card.appendChild(el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Diplomacy"),
    el("span", { class: "muted" }, "Hybrid seats against bots: each hybrid benchmark job is paired with a control game, the same game with " +
      "the seat's bot deciding everything, so the difference is what the model's diplomacy did."),
    el("span", { class: "grow" }), el("button", { class: "small", onclick: refresh }, "Refresh")));
  if (data.scope) card.appendChild(el("p", { class: "muted small" }, `From ${data.scope.description}. Other members' pairs count ` +
    `only where at least ${data.scope.min_contributors} accounts are behind a line, and are never named.`));
  const rows = boardRows(data);
  const withheld = (data.withheld || []).map((w) => el("li", {}, el("b", {}, `${w.model} · ${w.owners}`), ": ", w.reason));
  if (!rows.length) {
    card.appendChild(el("p", { class: "muted" }, "No hybrid benchmark pairs yet. Give a benchmark scenario the hybrid mode " +
      "(Benchmarks → a suite → a scenario's Mode) and run it."));
    if (withheld.length) card.appendChild(el("ul", { class: "muted small" }, ...withheld));
    const left = leftOut(data.unverified, "pairs");
    if (left) card.appendChild(left);
    return;
  }
  const help = {
    diff: "The hybrid seat's final score minus its control's, averaged over the pairs, with a 90% bootstrap interval (2,000 seeded resamples).",
    deals: "Deals the hybrid seat made per game.",
    value: "What the seat's own bot makes of each deal, in gold from the seat's side, valued when the accepted proposal was made.",
    acceptance: "How many of the seat's own proposals the other side accepted.",
    control: "The control game's deals per game and their mean value, valued the same way.",
    fallbacks: "Chats the bot answered and diplomat calls that did not run because of the model (it timed out or failed); the ones a benchmark's queue or a refusal caused are listed beside, not counted against it.",
  };
  const table = el("table", { class: "list leaderboard diplomacy-board" }, el("tr", {},
    el("th", {}, "Model"), el("th", {}, "Model decides"), el("th", {}, "Pairs"),
    el("th", { title: help.diff }, "Score vs control ⓘ"), el("th", { title: help.deals }, "Deals / game ⓘ"),
    el("th", { title: help.value }, "Mean deal value ⓘ"), el("th", { title: help.acceptance }, "Accepted ⓘ"),
    el("th", {}, "Wars declared / game"), el("th", {}, "Captures / losses per game"),
    el("th", { title: help.control }, "Control ⓘ"), el("th", { title: help.fallbacks }, "Fallbacks ⓘ"), el("th", {}, "")));
  for (const r of rows) {
    const key = `${r.model}|${r.owners}`;
    const expanded = openLines.has(key);
    table.appendChild(el("tr", { class: "clickable", onclick: () => { expanded ? openLines.delete(key) : openLines.add(key); drawDiplomacy(card, data, refresh); } },
      el("td", {}, el("b", {}, r.model)), el("td", {}, r.owners),
      el("td", {}, `${r.pairs}`, r.pooled ? el("div", { class: "muted small" }, `${r.pooled} pooled`) : null),
      el("td", {}, r.diff), el("td", {}, r.deals), el("td", {}, r.value), el("td", {}, r.acceptance),
      el("td", {}, r.wars), el("td", {}, r.captures), el("td", { class: "small" }, r.control),
      el("td", { class: "small" }, r.fallbacks), el("td", {}, r.games.length ? (expanded ? "▾" : "▸") : "")));
    if (expanded && r.games.length) {
      const inner = el("table", { class: "list" }, el("tr", {}, ...["Run", "Scenario", "Seed", "Score vs control", "Deals"].map((h) => el("th", {}, h))),
        ...r.games.map((g) => el("tr", {}, el("td", {}, g.run || g.run_id, g.unverified_machine ? [" ", unverifiedBadge()] : null),
          el("td", {}, g.scenario || "–"),
          el("td", {}, g.seed != null ? `${g.seed}` : "–"),
          el("td", {}, Object.entries(g.score_diff || {}).map(([k, v]) => `${k === "end" ? "end" : `T${k}`} ${v > 0 ? "+" : ""}${v}`).join(" · ")),
          el("td", {}, `${g.deals ?? "–"}`))));
      table.appendChild(el("tr", { class: "detail" }, el("td", { colspan: 12 }, inner)));
    }
  }
  card.appendChild(el("div", { style: { overflowX: "auto" } }, table));
  if (withheld.length) card.appendChild(el("details", {}, el("summary", { class: "muted" }, "Not enough pooled data yet"),
    el("ul", { class: "muted small" }, ...withheld)));
  const left = leftOut(data.unverified, "pairs");
  if (left) card.appendChild(left);
}

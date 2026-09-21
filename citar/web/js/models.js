// Models page: overall model scores (benchmark-weighted leaderboard) and the detailed metric comparison.
import { api } from "./api.js";
import { el, clear, toast } from "./util.js";
import { pageHeader, secs, bar } from "./nav.js";
import { renderComparison } from "./metrics.js";
import { helperCard } from "./helper.js";

const open = new Set();

export async function renderModels(root) {
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("models"));
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
  if (!data.models.length) { card.appendChild(el("p", { class: "muted" }, "No AI games recorded yet. Run a benchmark suite to score models.")); return; }
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
      el("td", {}, `${r.benchmark_games}`, r.benchmark_finished ? el("span", { class: "muted" }, ` (${r.benchmark_finished} finished${r.wins ? `, ${r.wins} won` : ""})`) : null),
      el("td", {}, r.benchmark_turns.toLocaleString(), r.benchmark_turns && r.benchmark_turns < 50 ? el("div", { class: "warn small" }, "provisional — short games") : null),
      el("td", {}, m ? secs(m.avg_turn_s) : "–"),
      el("td", {}, r.games.length ? (expanded ? "▾" : "▸") : "")));
    if (expanded && r.games.length) {
      const inner = el("table", { class: "list" }, el("tr", {}, ...["Game", "Scenario", "Turns", "Result", "Score vs best bot", "Performance"].map((h) => el("th", {}, h))),
        ...r.games.map((g) => el("tr", {},
          el("td", {}, g.game), el("td", {}, g.scenario || "–"),
          el("td", {}, `${g.turns}${g.turn_limit ? " / " + g.turn_limit : ""}`),
          el("td", {}, g.phase === "playing" ? "in progress" : (g.performance === 100 ? el("span", { class: "good" }, "won") : "finished")),
          el("td", {}, g.score != null ? `${g.score} vs ${g.best_bot_score}` : "–"),
          el("td", { style: { minWidth: "110px" } }, bar(g.performance / 100, `${Math.round(g.performance)}`)))));
      table.appendChild(el("tr", { class: "detail" }, el("td", { colspan: 10 }, inner)));
    }
  });
  card.appendChild(el("div", { style: { overflowX: "auto" } }, table));
}

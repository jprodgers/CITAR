// AI performance metrics: per-game stats modal and cross-game model comparison.
import { api } from "./api.js";
import { el, clear, modal, fmt } from "./util.js";

const COLORS = ["#e04040", "#3c78d8", "#e8c547", "#8e44ad", "#27ae60", "#e67e22", "#17becf", "#f06292"];

function secs(s) {
  if (s == null) return "–";
  if (s < 60) return `${fmt(s, 1)}s`;
  const m = Math.floor(s / 60);
  return `${m}m ${Math.round(s - m * 60)}s`;
}

function reasonsText(r) {
  return Object.entries(r || {}).map(([k, v]) => `${k} ${v}`).join(", ") || "–";
}

const HELP = {
  "Avg turn": "Wall-clock time from the start of the seat's turn to its end (includes tool execution and waiting on negotiations).",
  "Steps/turn": "Model requests per turn. Fewer is better for the same quality of play.",
  "Step time": "Average seconds per model request.",
  "Calls/turn": "Tool calls per turn (queries + actions).",
  "OK actions": "Successful game actions per turn.",
  "Errors/turn": "Tool calls rejected by the game (invalid orders).",
  "Repeats/turn": "Identical tool calls (same tool and arguments) made again in the same turn — a looping indicator.",
  "Blocked": "Identical successful actions the guard rail refused to redo.",
  "Malformed": "Tool calls with broken syntax that had to be repaired or could not be parsed.",
  "Stall nudges": "Times the model was told it was going in circles.",
  "Out tok/turn": "Output tokens generated per turn (includes reasoning).",
  "Tok/s": "Output tokens per second of model time.",
  "Peak prompt": "Largest prompt (input tokens) sent in a single model call. Red when it approaches the loaded context length.",
  "Context": "Context length the model was loaded with (LM Studio). Below ~24k, models lose their briefing mid-turn.",
  "Slowest call": "Slowest single model request (model loading shows up here).",
  "Cold starts": "Turns where the model was not loaded when the turn began (LM Studio swapping models between seats).",
  "Turn ends": "How turns ended: end_turn (clean), stalled, step_limit, time_limit, tool_limit, no_tool_calls, error, cancelled, ended_by_server. Turns interrupted by a server restart are excluded from all stats.",
};

function th(label) {
  return el("th", { title: HELP[label] || "" }, label, HELP[label] ? el("span", { class: "muted" }, " ⓘ") : null);
}

export async function openMetrics(gid) {
  const content = el("div", { class: "col" });
  let selected = null;
  let timer = null;
  const m = modal({ title: "AI performance", content, onClose: () => clearInterval(timer) });
  async function draw() {
    let rep;
    try { rep = await api.metrics(gid); } catch (e) { clear(content).appendChild(el("p", { class: "bad" }, e.message)); return; }
    const scroll = content.parentElement ? content.parentElement.scrollTop : 0;
    clear(content);
    const seats = Object.entries(rep.summary).map(([pid, s]) => ({ pid: +pid, ...s }));
    if (selected == null) {
      const firstAI = seats.find((s) => s.controller === "llm" && s.turns) || seats.find((s) => s.turns);
      selected = firstAI ? firstAI.pid : 0;
    }
    content.appendChild(el("div", { class: "row" },
      el("span", { class: "muted" }, `Game turn ${rep.game_turn}. Hover column headers for definitions. Updates every 5s.`),
      el("span", { class: "grow" }),
      el("a", { href: `/api/games/${gid}/metrics.csv` }, "Download per-turn CSV"),
      el("a", { href: `/api/games/${gid}/metrics`, target: "_blank" }, "Raw JSON")));

    const table = el("table", { class: "list" }, el("tr", {},
      th("Civ"), th("Controller"), th("Turns"), th("Avg turn"), th("Median"), th("Max"), th("Steps/turn"), th("Step time"),
      th("Calls/turn"), th("OK actions"), th("Errors/turn"), th("Repeats/turn"), th("Blocked"), th("Malformed"),
      th("Stall nudges"), th("Out tok/turn"), th("Tok/s"), th("Peak prompt"), th("Context"), th("Slowest call"),
      th("Cold starts"), th("Turn ends")));
    for (const s of seats) {
      const tr = el("tr", { style: { cursor: "pointer", background: s.pid === selected ? "#1d2c47" : "" }, onclick: () => { selected = s.pid; draw(); } },
        el("td", {}, el("span", { class: "swatch", style: { background: COLORS[s.pid % COLORS.length] } }), s.name),
        el("td", {}, s.controller === "llm" ? s.model || "llm" : s.controller),
        el("td", {}, s.turns));
      if (s.turns) {
        tr.append(el("td", {}, secs(s.avg_turn_s)), el("td", {}, secs(s.median_turn_s)), el("td", {}, secs(s.max_turn_s)),
          el("td", {}, fmt(s.avg_model_steps, 1)), el("td", {}, s.avg_model_steps ? secs(s.avg_step_s) : "–"),
          el("td", {}, fmt(s.avg_tool_calls, 1)), el("td", {}, fmt(s.avg_actions_ok, 1)),
          el("td", { class: s.avg_errors > 2 ? "warn" : "" }, fmt(s.avg_errors, 1)),
          el("td", { class: s.avg_repeats > 2 ? "bad" : "" }, fmt(s.avg_repeats, 1)),
          el("td", {}, s.blocked_repeats), el("td", { class: s.malformed_calls ? "warn" : "" }, s.malformed_calls),
          el("td", {}, s.stall_nudges), el("td", {}, s.avg_output_tokens || "–"), el("td", {}, s.output_tokens_per_s ?? "–"),
          el("td", { class: s.context_length && s.peak_prompt_tokens > s.context_length * 0.8 ? "bad" : "" }, s.peak_prompt_tokens ? s.peak_prompt_tokens.toLocaleString() : "–"),
          el("td", { class: s.context_length && s.context_length < 24000 ? "bad" : "" }, s.context_length ? s.context_length.toLocaleString() : "–"),
          el("td", {}, s.slowest_step_s ? secs(s.slowest_step_s) : "–"),
          el("td", { class: s.turns_model_not_loaded ? "warn" : "" }, s.turns_model_not_loaded ?? "–"),
          el("td", {}, reasonsText(s.end_reasons)));
      }
      table.appendChild(tr);
    }
    content.appendChild(el("div", { style: { overflowX: "auto" } }, table));

    // per-turn duration chart
    const chart = el("canvas", { width: 1040, height: 190, class: "chart-box", style: { width: "100%" } });
    content.append(el("h4", { class: "section" }, "Turn duration by turn (seconds)"), chart);
    drawTurnChart(chart, rep.turns, seats);

    const s = seats.find((x) => x.pid === selected);
    if (s && s.turns) {
      const tools = el("table", { class: "list" }, el("tr", {}, ...["Tool", "Total", "Per turn", "Max in one turn", "Errors", "Avg ms"].map((h) => el("th", {}, h))));
      for (const [name, t] of Object.entries(s.tools)) {
        tools.appendChild(el("tr", {}, el("td", {}, name), el("td", {}, t.count), el("td", {}, t.per_turn),
          el("td", { class: t.max_in_turn >= 6 ? "warn" : "" }, t.max_in_turn), el("td", { class: t.errors ? "warn" : "" }, t.errors),
          el("td", {}, t.avg_ms)));
      }
      const repeats = el("div", { class: "col" }, ...(s.top_repeats.length ? s.top_repeats.map((r) =>
        el("div", {}, el("b", {}, `×${r.max_times_in_a_turn} `), el("code", {}, r.call))) : [el("span", { class: "muted" }, "No repeated identical calls.")]));
      const errors = el("div", { class: "col" }, ...(s.top_errors.length ? s.top_errors.map((r) =>
        el("div", {}, el("b", {}, `×${r.count} `), r.error)) : [el("span", { class: "muted" }, "No errors.")]));
      const recent = rep.turns.filter((r) => r.player === selected).slice(-15).reverse();
      const turnsTable = el("table", { class: "list" }, el("tr", {}, ...["Turn", "Time", "Steps", "Calls", "OK", "Err", "Rep", "Malf", "Out tok", "Ended", "Most used tools"].map((h) => el("th", {}, h))),
        ...recent.map((r) => el("tr", {}, el("td", {}, r.turn), el("td", {}, r.wall_s == null ? "in progress" : secs(r.wall_s)),
          el("td", {}, r.model_steps), el("td", {}, r.tool_calls), el("td", {}, r.actions_ok), el("td", {}, r.errors),
          el("td", { class: r.repeats > 2 ? "bad" : "" }, r.repeats), el("td", {}, r.malformed), el("td", {}, r.output_tokens),
          el("td", { class: r.end_reason && r.end_reason !== "end_turn" ? "warn" : "" }, r.end_reason || "…"), el("td", {}, r.top_tools))));
      content.append(
        el("h3", { class: "section" }, `Details — ${s.name}${s.model ? " (" + s.model + ")" : ""}`),
        s.settings ? el("div", { class: "muted" }, Object.entries(s.settings).filter(([, v]) => v).map(([k, v]) => `${k}: ${v}`).join(" · ")) : null,
        el("div", { class: "grid2" },
          el("div", {}, el("h4", {}, "Tool usage"), tools),
          el("div", {}, el("h4", {}, "Most repeated identical calls"), repeats, el("h4", { class: "section" }, "Most common errors"), errors)),
        el("h4", { class: "section" }, "Recent turns"), el("div", { style: { overflowX: "auto" } }, turnsTable));
      if (s.negotiation_replies) content.appendChild(el("div", { class: "muted" }, `Negotiation replies: ${s.negotiation_replies}, average ${secs(s.avg_negotiation_s)}`));
    }
    if (content.parentElement) content.parentElement.scrollTop = scroll;
  }
  await draw();
  timer = setInterval(() => { if (!document.body.contains(content)) clearInterval(timer); else draw(); }, 5000);
}

function drawTurnChart(canvas, turns, seats) {
  const ctx = canvas.getContext("2d");
  const W = canvas.width, H = canvas.height, pad = 40;
  ctx.clearRect(0, 0, W, H);
  const done = turns.filter((r) => r.wall_s != null && !r.interrupted && seats.some((s) => s.pid === r.player && s.controller !== "human"));
  if (!done.length) { ctx.fillStyle = "#9aa3b5"; ctx.font = "12px system-ui"; ctx.fillText("No completed AI turns yet.", pad, H / 2); return; }
  const maxT = Math.max(...done.map((r) => r.turn)), minT = Math.min(...done.map((r) => r.turn));
  const maxY = Math.max(...done.map((r) => r.wall_s)) * 1.1 || 1;
  const X = (t) => pad + (W - pad - 10) * (maxT === minT ? 0.5 : (t - minT) / (maxT - minT));
  const Y = (v) => H - 20 - (H - 34) * (v / maxY);
  ctx.strokeStyle = "#2c3342"; ctx.beginPath(); ctx.moveTo(pad, 8); ctx.lineTo(pad, H - 20); ctx.lineTo(W - 10, H - 20); ctx.stroke();
  ctx.fillStyle = "#9aa3b5"; ctx.font = "10px system-ui"; ctx.textAlign = "right";
  ctx.fillText(secs(maxY), pad - 4, 12); ctx.fillText("0", pad - 4, H - 20);
  ctx.textAlign = "center"; ctx.fillText(`T${minT}`, pad, H - 6); ctx.fillText(`T${maxT}`, W - 20, H - 6);
  for (const s of seats) {
    const pts = done.filter((r) => r.player === s.pid).sort((a, b) => a.turn - b.turn);
    if (!pts.length) continue;
    ctx.strokeStyle = COLORS[s.pid % COLORS.length]; ctx.fillStyle = ctx.strokeStyle; ctx.lineWidth = 2;
    ctx.beginPath();
    pts.forEach((r, i) => { i ? ctx.lineTo(X(r.turn), Y(r.wall_s)) : ctx.moveTo(X(r.turn), Y(r.wall_s)); });
    ctx.stroke();
    for (const r of pts) {
      ctx.beginPath(); ctx.arc(X(r.turn), Y(r.wall_s), r.end_reason && r.end_reason !== "end_turn" ? 4 : 2.5, 0, 7);
      ctx.fill();
    }
  }
}

export async function renderComparison(card) {
  clear(card);
  card.appendChild(el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Model comparison"),
    el("span", { class: "muted" }, "Aggregated over running and saved games (autosaves)."), el("span", { class: "grow" }),
    el("button", { class: "small", onclick: () => renderComparison(card) }, "Refresh")));
  let rows;
  try { rows = await api.compareModels(); } catch (e) { card.appendChild(el("p", { class: "bad" }, e.message)); return; }
  rows = rows.filter((r) => r.controller !== "human");
  if (!rows.length) { card.appendChild(el("p", { class: "muted" }, "No AI turns recorded yet.")); return; }
  const table = el("table", { class: "list" }, el("tr", {}, ...["Model", "Games", "Turns", "Avg turn", "Max turn", "Steps/turn", "Calls/turn",
    "Errors/turn", "Repeats/turn", "Malformed", "Stall nudges", "Out tok/turn", "Tok/s", "Clean ends", "Turn ends"].map((h) => th(h))));
  for (const r of rows) {
    table.appendChild(el("tr", { title: r.games_list.join("\n") },
      el("td", {}, el("b", {}, r.model)), el("td", {}, r.games), el("td", {}, r.turns), el("td", {}, secs(r.avg_turn_s)),
      el("td", {}, secs(r.max_turn_s)), el("td", {}, r.avg_model_steps), el("td", {}, r.avg_tool_calls),
      el("td", {}, r.avg_errors), el("td", { class: r.avg_repeats > 2 ? "bad" : "" }, r.avg_repeats), el("td", {}, r.malformed_calls),
      el("td", {}, r.stall_nudges), el("td", {}, r.avg_output_tokens || "–"), el("td", {}, r.output_tokens_per_s ?? "–"),
      el("td", { class: r.clean_turn_end_rate < 0.9 ? "warn" : "good" }, `${Math.round(r.clean_turn_end_rate * 100)}%`),
      el("td", {}, reasonsText(r.end_reasons))));
  }
  card.appendChild(el("div", { style: { overflowX: "auto" } }, table));
}

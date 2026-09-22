// Probes page (#/probes): write probe files (a scenario + a queue of cases: deal offers, messages or whole turns),
// run them against a model and watch the results come in. Every case starts from the scenario's saved state.
import { api } from "./api.js";
import { el, clear, toast, modal, confirmBox, keepPlace } from "./util.js";
import { pageHeader, secs, bar } from "./nav.js";
import { llmForm, defaultLLM } from "./lobby.js";

const ITEM_TYPES = ["gold", "gold_per_turn", "resource", "tech", "city", "open_borders", "embassy", "peace_treaty",
  "declaration_of_friendship", "research_agreement", "defensive_pact", "declare_war"];
const OUTCOME_CLASS = { accept: "good", reject: "bad", counter: "warn", reply: "", no_response: "bad", error: "bad",
  invalid_case: "bad", end_turn: "good" };
const openRuns = new Set();
const openDetails = new Set();
let lastLLM = null;

export async function renderProbes(root, rules) {
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("probes"));
  const live = el("div", { class: "card" });
  const runsCard = el("div", { class: "card" });
  const probesCard = el("div", { class: "card" });
  const editorCard = el("div", { class: "card", style: { display: "none" } });
  page.append(live, runsCard, probesCard, editorCard);
  const ctx = { rules, editorCard, refresh: null, results: new Map(), runsSig: null, runs: [] };
  // redraws keep the reader's place (see keepPlace), and the runs table is only rebuilt when a run changed
  const runsSig = (runs) => JSON.stringify(runs.map((r) => [r.id, r.status, r.done, r.summary && r.summary.runs]));
  const refresh = async (force = true) => {
    try {
      const [probes, runs] = await Promise.all([api.probes(), api.probeRuns()]);
      ctx.runs = runs.runs;
      keepPlace(live, () => drawLive(live, runs.status, runs.runs));
      ctx.runsSig = runsSig(runs.runs);
      keepPlace(runsCard, () => drawRuns(runsCard, runs.runs, ctx));
      keepPlace(probesCard, () => drawProbes(probesCard, probes, ctx));
    } catch (e) { toast(e.message, "error"); }
  };
  ctx.refresh = refresh;
  ctx.redrawRuns = () => keepPlace(runsCard, () => drawRuns(runsCard, ctx.runs, ctx));
  await refresh();
  const want = new URLSearchParams(location.hash.split("?")[1] || "").get("scenario");
  if (want) newProbe(ctx, want);
  const timer = setInterval(async () => {
    if (!autoRefresh) return;
    try {
      const runs = await api.probeRuns();
      ctx.runs = runs.runs;
      keepPlace(live, () => drawLive(live, runs.status, runs.runs));
      const sig = runsSig(runs.runs);
      if (sig !== ctx.runsSig) {
        ctx.runsSig = sig;
        ctx.redrawRuns();
      }
    } catch (e) { /* server restarting */ }
  }, 4000);
  return { destroy() { clearInterval(timer); } };
}

let autoRefresh = true;

// results of a run, fetched again only when the run has finished more cases
async function runResults(ctx, run) {
  const c = ctx.results.get(run.id);
  if (c && c.done === run.done && c.status === run.status) return c.data;
  const data = await api.probeRun(run.id);
  ctx.results.set(run.id, { done: run.done, status: run.status, data });
  return data;
}

// ---------------------------------------------------------------------------- live
function drawLive(card, status, runs) {
  clear(card);
  const lv = status.live || {};
  const run = runs.find((r) => r.id === lv.run);
  card.appendChild(el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Probes"),
    run ? el("span", { class: "pill live" }, `● running ${run.name}`) : el("span", { class: "pill muted" }, "idle"),
    ...(status.running || []).filter((r) => r.run !== lv.run).map((r) => {
      const other = runs.find((x) => x.id === r.run);
      return other ? el("span", { class: "pill live", title: `case ${r.case}` }, `● also running ${other.name}`) : null;
    }),
    status.queue && status.queue.length ? el("span", { class: "pill" }, `${status.queue.length} queued`) : null,
    el("span", { class: "grow" }),
    el("label", { class: "small", title: "Turn off to freeze the page while reading" },
      el("input", { type: "checkbox", checked: autoRefresh, onchange: (e) => { autoRefresh = e.target.checked; } }), " live updates")));
  card.appendChild(el("p", { class: "muted small" }, "A probe replays one scenario many times: before each case the game is reset to the scenario's " +
    "saved state, the case's setup is applied, the model under test responds (to a deal, a message or a whole turn), and the result is recorded."));
  if (!run) return;
  card.append(el("div", { class: "row" }, el("b", {}, `Case ${lv.case}`), lv.rep ? el("span", { class: "muted" }, `repeat ${lv.rep + 1}`) : null,
    el("span", { class: "muted" }, `for ${secs(lv.seconds)}`), el("span", { class: "grow" }),
    el("div", { style: { minWidth: "220px" } }, bar(run.done / Math.max(1, run.total), `${run.done} / ${run.total} cases`))));
  // fixed height, so the page below doesn't jump as the model writes
  const pre = el("pre", { class: "lab-report", "data-follow": "1", style: { height: "220px", maxHeight: "220px" } },
    (lv.thoughts || []).map((t) => `[${t.kind}] ${t.text}`).join("\n\n") || "(waiting for the model…)");
  card.append(pre);
}

// ---------------------------------------------------------------------------- runs
function drawRuns(card, runs, ctx) {
  clear(card);
  card.appendChild(el("h3", {}, "Runs"));
  if (!runs.length) { card.appendChild(el("p", { class: "muted" }, "No runs yet. Run a probe below.")); return; }
  const table = el("table", { class: "list jobs" }, el("tr", {}, el("th", {}, "Run"), el("th", {}, "Status"),
    el("th", { style: { minWidth: "160px" } }, "Progress"), el("th", {}, "Pass rate"), el("th", {}, "Outcomes"), el("th", {}, "")));
  for (const r of runs) {
    const outcomes = {};
    for (const c of Object.values((r.summary || {}).cases || {})) for (const [k, v] of Object.entries(c.outcomes)) outcomes[k] = (outcomes[k] || 0) + v;
    const open = openRuns.has(r.id);
    table.appendChild(el("tr", { class: "clickable", onclick: () => { open ? openRuns.delete(r.id) : openRuns.add(r.id); ctx.redrawRuns(); } },
      el("td", {}, el("b", {}, r.name), el("div", { class: "muted small" }, `${r.provider || ""} ${r.model || ""} · ${new Date(r.created * 1000).toLocaleString()}`)),
      el("td", {}, el("span", { class: `pill ${r.status === "running" ? "live" : r.status === "finished" ? "good" : r.status === "failed" ? "bad" : ""}`, title: r.waiting || null }, r.status)),
      el("td", {}, bar(r.done / Math.max(1, r.total), `${r.done} / ${r.total}`)),
      el("td", {}, r.summary && r.summary.pass_rate != null ? `${Math.round(r.summary.pass_rate * 100)}%` : "–"),
      el("td", { class: "small" }, Object.entries(outcomes).map(([k, v]) => el("span", { class: `pill ${OUTCOME_CLASS[k] || ""}`, style: { marginRight: "3px" } }, `${k} ${v}`))),
      el("td", {}, el("div", { class: "row" },
        (["running", "queued"].includes(r.status) || r.status.startsWith("waiting")) ? el("button", { class: "small", onclick: async (e) => { e.stopPropagation(); await api.probeStop(r.id); ctx.refresh(); } }, "Stop") :
          el("button", { class: "small", onclick: async (e) => {
            e.stopPropagation();
            if (!(await confirmBox("Delete run", `Delete the run "${r.name}" and its results?`))) return;
            try { await api.probeRunDelete(r.id); ctx.refresh(); } catch (err) { toast(err.message, "error"); }
          } }, "Delete"),
        el("span", {}, open ? "▾" : "▸")))));
    if (open) {
      const cell = el("td", { colspan: 6 });
      table.appendChild(el("tr", {}, cell));
      const cached = ctx.results.get(r.id);
      if (cached) drawResults(cell, cached.data, ctx);
      else cell.append(el("span", { class: "muted" }, "Loading…"));
      if (!cached || cached.done !== r.done || cached.status !== r.status) {
        runResults(ctx, r).then((d) => keepPlace(cell, () => drawResults(cell, d, ctx)))
          .catch((e) => { cell.textContent = e.message; });
      }
    }
  }
  card.appendChild(table);
}

function drawResults(cell, d, ctx) {
  clear(cell);
  const cases = Object.fromEntries(d.run.probe.cases.map((c) => [c.id, c]));
  if (d.run.error) cell.append(el("pre", { class: "lab-report bad" }, d.run.error));
  if (!d.results.length) { cell.append(el("p", { class: "muted" }, "No results yet.")); return; }
  const t = el("table", { class: "list" }, el("tr", {}, el("th", {}, "Case"), el("th", {}, "Outcome"), el("th", {}, "Expected"),
    el("th", {}, "What the model said / did"), el("th", {}, "Time"), el("th", {}, "")));
  for (const r of d.results) {
    const c = cases[r.case] || {};
    const key = `${d.run.id}/${r.case}/${r.rep}`;
    const said = r.kind === "turn"
      ? `${r.tool_calls.length} tool calls: ${[...new Set(r.tool_calls.filter((x) => x.ok !== false).map((x) => x.tool))].join(", ")}`
      : [r.subject_messages.join(" / "), ...r.counter_offers.map((o) => `counter: ${o.text}`)].filter(Boolean).join(" — ");
    t.appendChild(el("tr", { class: "clickable", onclick: () => { openDetails.has(key) ? openDetails.delete(key) : openDetails.add(key); drawResults(cell, d, ctx); } },
      el("td", {}, el("b", {}, r.case), d.run.repeats > 1 ? el("span", { class: "muted" }, ` #${r.rep + 1}`) : null,
        el("div", { class: "muted small" }, r.offer_text || (c.message ? `"${c.message}"` : r.kind))),
      el("td", {}, el("span", { class: `pill ${OUTCOME_CLASS[r.outcome] || ""}` }, r.outcome || "?"),
        r.passed === true ? el("span", { class: "good" }, " ✓") : r.passed === false ? el("span", { class: "bad" }, " ✗") : null),
      el("td", { class: "small" }, c.expect || (c.expect_tools ? `uses ${c.expect_tools.join(", ")}` : "–")),
      el("td", { class: "small" }, said || el("span", { class: "muted" }, "–"), r.error ? el("div", { class: "bad" }, r.error) : null),
      el("td", { class: "small" }, `${r.seconds}s`),
      el("td", {}, r.save ? el("button", { class: "small", onclick: async (e) => {
        e.stopPropagation();
        try { const g = await api.probeOpen(d.run.id, r.case, r.rep); location.hash = `#/game/${g.game_id}/${encodeURIComponent(g.spectator_token)}`; }
        catch (err) { toast(err.message, "error"); }
      } }, "Open game") : null)));
    if (openDetails.has(key)) {
      t.appendChild(el("tr", {}, el("td", { colspan: 6 },
        el("div", { class: "muted small" }, `Tokens: ${r.tokens ? `${r.tokens.input_tokens} in / ${r.tokens.output_tokens} out` : "–"}`),
        el("h4", {}, "Model reasoning"),
        el("pre", { class: "lab-report" }, (r.thoughts || []).map((x) => `[${x.kind}] ${x.text}`).join("\n\n") || "(none)"),
        el("h4", {}, "Tool calls"),
        el("pre", { class: "lab-report" }, (r.tool_calls || []).map((x) => `${x.ok === false ? "✗" : "✓"} ${x.tool} ${JSON.stringify(x.args || {})}${x.error ? `\n    ${x.error}` : ""}`).join("\n") || "(none)"))));
    }
  }
  cell.append(t);
}

// ---------------------------------------------------------------------------- probe files
function drawProbes(card, probes, ctx) {
  clear(card);
  card.appendChild(el("div", { class: "row" }, el("h3", { style: { margin: 0 } }, "Probe files"), el("span", { class: "grow" }),
    el("button", { class: "small primary", onclick: () => newProbe(ctx) }, "New probe")));
  if (!probes.length) { card.appendChild(el("p", { class: "muted" }, "No probes yet. A probe needs a scenario — make one on the Scenarios page first.")); return; }
  const table = el("table", { class: "list" }, el("tr", {}, el("th", {}, "Probe"), el("th", {}, "Scenario"), el("th", {}, "Cases"), el("th", {}, "")));
  for (const p of probes) {
    table.appendChild(el("tr", {},
      el("td", {}, el("b", {}, p.name), el("div", { class: "muted small" }, p.description || p.id)),
      el("td", {}, p.scenario, el("div", { class: "muted small" }, `subject P${p.subject}${p.counterparty != null ? `, counterparty P${p.counterparty}` : ""}`)),
      el("td", {}, `${p.cases}${p.repeats > 1 ? ` × ${p.repeats}` : ""}`),
      el("td", {}, el("div", { class: "row" },
        el("button", { class: "small primary", onclick: () => runDialog(p, ctx) }, "Run…"),
        el("button", { class: "small", onclick: async () => { try { openEditor(ctx, await api.probe(p.id)); } catch (e) { toast(e.message, "error"); } } }, "Edit"),
        el("button", { class: "small", onclick: async () => {
          if (!(await confirmBox("Delete probe", `Delete the probe "${p.name}"? Runs are kept.`))) return;
          try { await api.deleteProbe(p.id); ctx.refresh(); } catch (e) { toast(e.message, "error"); }
        } }, "Delete")))));
  }
  card.appendChild(table);
}

async function newProbe(ctx, scenarioId) {
  let scenarios = [];
  try { scenarios = await api.scenarios(); } catch (e) { toast(e.message, "error"); return; }
  if (!scenarios.length) { toast("Make a scenario first (Scenarios page).", "error"); return; }
  const s = scenarios.find((x) => x.id === scenarioId) || scenarios[0];
  const subject = Math.max(0, s.seats.findIndex((x) => x.type === "llm"));
  const cp = s.seats.findIndex((x, i) => i !== subject && ["script", "human"].includes(x.type));
  try { openEditor(ctx, await api.probeExample(s.id, subject, cp >= 0 ? cp : (subject === 0 ? 1 : 0))); } catch (e) { toast(e.message, "error"); }
}

function openEditor(ctx, probe) {
  const card = ctx.editorCard;
  card.style.display = "";
  clear(card);
  const ta = el("textarea", { rows: 26, spellcheck: false, style: { width: "100%", fontFamily: "monospace", fontSize: "12px", boxSizing: "border-box" },
    value: JSON.stringify(probe, null, 2) });
  const id = el("input", { value: probe.id || "", placeholder: "probe-id (letters, digits, dashes)" });
  const msg = el("div", { class: "small" });
  const parse = () => { try { return JSON.parse(ta.value); } catch (e) { msg.className = "small bad"; msg.textContent = `Not valid JSON: ${e.message}`; return null; } };
  const builder = caseBuilder(ctx.rules, (c) => {
    const p = parse(); if (!p) return;
    p.cases = [...(p.cases || []), c];
    ta.value = JSON.stringify(p, null, 2);
    msg.className = "small good"; msg.textContent = `Added case "${c.id}".`;
  });
  card.append(el("div", { class: "row" }, el("h3", { style: { margin: 0 } }, probe.id ? `Edit probe "${probe.id}"` : "New probe"), el("span", { class: "grow" }),
    el("button", { class: "small", onclick: () => { card.style.display = "none"; } }, "Close")),
    el("p", { class: "muted small" }, "\"give\" is what the counterparty offers the subject; \"receive\" is what it asks for. Kinds: offer (a deal), " +
      "message (talk, no deal), turn (the subject plays one whole turn). Optional per case: setup (scenario operations), expect " +
      "(accept | reject | counter | reply), on_counter (reject | accept | leave), followups (scripted replies), expect_tools / forbid_tools."),
    el("div", { class: "grid2", style: { gridTemplateColumns: "2fr 1fr" } },
      el("div", {}, ta),
      el("div", {}, el("h4", {}, "Add a case"), builder)),
    el("div", { class: "row", style: { marginTop: "8px" } }, id,
      el("button", { class: "small", onclick: async () => {
        const p = parse(); if (!p) return;
        try { await api.validateProbe(p); msg.className = "small good"; msg.textContent = "Looks good."; } catch (e) { msg.className = "small bad"; msg.textContent = e.message; }
      } }, "Check"),
      el("button", { class: "small primary", onclick: async () => {
        const p = parse(); if (!p) return;
        const pid = (id.value || p.name || "probe").toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-|-$/g, "");
        try { const saved = await api.saveProbe(pid, p); ta.value = JSON.stringify(saved, null, 2); id.value = saved.id; msg.className = "small good"; msg.textContent = `Saved as "${saved.id}".`; ctx.refresh(); }
        catch (e) { msg.className = "small bad"; msg.textContent = e.message; }
      } }, "Save"), msg));
  card.scrollIntoView({ behavior: "smooth" });
}

function itemEditor(rules, items) {
  const box = el("div");
  const draw = () => {
    clear(box);
    items.forEach((it, i) => {
      const type = el("select", { onchange: (e) => { items[i] = { type: e.target.value }; draw(); } }, ...ITEM_TYPES.map((t) => el("option", { value: t, selected: t === it.type }, t)));
      const extra = [];
      const num = (k, ph) => el("input", { type: "number", placeholder: ph, value: it[k] ?? "", style: { width: "70px" }, oninput: (e) => { it[k] = e.target.value === "" ? undefined : +e.target.value; } });
      if (["gold", "gold_per_turn"].includes(it.type)) extra.push(num("amount", "amount"));
      if (["gold_per_turn", "resource", "open_borders"].includes(it.type)) extra.push(num("turns", "turns"));
      if (it.type === "resource") {
        const res = Object.keys(rules.resources).filter((r) => rules.resources[r].resourceType !== "Bonus").sort();
        it.resource = it.resource || res[0];
        extra.push(el("select", { onchange: (e) => { it.resource = e.target.value; } }, ...res.map((r) => el("option", { value: r, selected: r === it.resource }, r))), num("amount", "qty"));
      }
      if (it.type === "tech") {
        it.tech = it.tech || rules.tech_order[0];
        extra.push(el("select", { onchange: (e) => { it.tech = e.target.value; } }, ...rules.tech_order.map((t) => el("option", { value: t, selected: t === it.tech }, t))));
      }
      if (it.type === "city") extra.push(num("city_id", "city id"));
      if (it.type === "declare_war") extra.push(num("target", "player id"));
      box.appendChild(el("div", { class: "row small" }, type, ...extra, el("button", { class: "small", onclick: () => { items.splice(i, 1); draw(); } }, "✕")));
    });
    box.appendChild(el("button", { class: "small", onclick: () => { items.push({ type: "gold", amount: 100 }); draw(); } }, "+ item"));
  };
  draw();
  return box;
}

function caseBuilder(rules, add) {
  const c = { kind: "offer", give: [], receive: [] };
  const id = el("input", { placeholder: "case id, e.g. oil-for-gold" });
  const kind = el("select", {}, ...["offer", "message", "turn"].map((k) => el("option", { value: k }, k)));
  const message = el("textarea", { rows: 2, placeholder: "What the counterparty says" });
  const expect = el("select", {}, ...[["", "no expectation"], ["accept", "accept"], ["reject", "reject"], ["counter", "counter"], ["reply", "reply"]].map(([v, l]) => el("option", { value: v }, l)));
  const onCounter = el("select", {}, ...[["reject", "script rejects counters"], ["accept", "script accepts counters"], ["leave", "leave counters open"]].map(([v, l]) => el("option", { value: v }, l)));
  const give = itemEditor(rules, c.give), receive = itemEditor(rules, c.receive);
  const offerOnly = el("div", {}, el("div", { class: "small" }, "Counterparty gives"), give, el("div", { class: "small" }, "Counterparty asks for"), receive,
    el("div", { class: "small" }, "Expected answer"), expect, el("div", { class: "small" }, "If the subject counters"), onCounter);
  kind.onchange = () => { offerOnly.style.display = kind.value === "offer" ? "" : "none"; message.style.display = kind.value === "turn" ? "none" : ""; };
  return el("div", { class: "col" }, id, kind, message, offerOnly,
    el("button", { class: "small primary", onclick: () => {
      const out = { id: id.value.trim() || `case-${Date.now() % 100000}`, kind: kind.value };
      if (kind.value !== "turn") out.message = message.value || (kind.value === "offer" ? "I have a proposal." : "Hello.");
      if (kind.value === "offer") {
        out.give = JSON.parse(JSON.stringify(c.give)); out.receive = JSON.parse(JSON.stringify(c.receive));
        out.on_counter = onCounter.value;
        if (expect.value) out.expect = expect.value;
      }
      add(out);
    } }, "Add case to probe"));
}

function runDialog(p, ctx) {
  const L = lastLLM ? JSON.parse(JSON.stringify(lastLLM)) : defaultLLM();
  const box = el("div");
  const repeats = el("input", { type: "number", min: 1, max: 50, value: p.repeats || 1, style: { width: "70px" } });
  const who = el("select", {}, el("option", { value: "model" }, "A model"),
    el("option", { value: "bot" }, "The scripted bot (baseline to compare models with)"));
  const draw = () => { clear(box); if (who.value === "model") box.append(llmForm(L, draw)); };
  who.onchange = draw;
  draw();
  const go = el("button", { class: "primary", onclick: async () => {
    go.disabled = true;
    try {
      if (who.value === "model") lastLLM = L;
      await api.runProbe(p.id, { llm: who.value === "bot" ? { provider: "bot", aggression: 0.4 } : L, repeats: +repeats.value || 1 });
      dlg.close();
      toast("Probe run queued", "success");
      ctx.refresh();
    } catch (e) { toast(e.message, "error"); go.disabled = false; }
  } }, "Start run");
  const dlg = modal({ title: `Run "${p.name}"`, content: el("div", {},
    el("p", { class: "muted small" }, `${p.cases} case${p.cases === 1 ? "" : "s"} on scenario "${p.scenario}". The model plays seat P${p.subject}. ` +
      "Runs go one at a time (they usually share one GPU). Pick the Dry run server to test a probe without a model."),
    el("div", { class: "row" }, el("span", {}, "Subject"), who), el("div", { class: "row" }, el("span", {}, "Repeats per case"), repeats), box,
    el("p", { class: "muted small" }, "Models on servers CITAR manages are loaded with the chosen load profile first. Runs pause during " +
      "their server's restricted hours. The run's cost shows up in Reports.")), footer: go });
}

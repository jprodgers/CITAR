// Reports page: the report builder (what to include), the list of generated reports, and a viewer. Reports are only
// made when you run them; each is a self-contained HTML page with charts and analysis you can download and share.
import { api } from "./api.js";
import { el, clear, toast, confirmBox, keepPlace } from "./util.js";
import { pageHeader, secs } from "./nav.js";
import { llmForm } from "./lobby.js";

const SECTION_INFO = {
  summary: ["Summary", "Headline numbers and the key findings"],
  costs: ["Where the money went", "Cost by model, server and activity type, split into depreciation, fixed costs, energy and tokens"],
  cost_per_unit: ["Cost per unit of work", "Per game, per model turn, per million tokens, per performance point, per win, per probe case"],
  efficiency: ["Energy and efficiency", "Wh per game, per model turn and per probe case, output tokens per Wh, performance per kWh"],
  servers: ["Servers", "Allocated vs unallocated fixed costs, how busy each server was, metered energy"],
  server_trend: ["Costs over time", "Daily and cumulative cost per server"],
  models: ["Model comparison", "Performance with confidence intervals, speed, reliability, tokens, cost-efficiency frontier"],
  benchmarks: ["Benchmark runs", "Every job's outcome, performance and cost"],
  probes: ["Scenario probes", "Outcome of each case per model, pass rates, cost per case"],
  behavior: ["Model behavior", "Tool mix, how turns ended, most common errors"],
  lab: ["Bot lab experiments", "Cost of bot-vs-bot experiments and results per seat label"],
  whatif: ["What if it ran elsewhere", "The same tokens at API prices, the same hours on other machines"],
  depreciation: ["Hardware lifespan sensitivity", "How the cost changes if the hardware lasts 2…6 years"],
  hardware: ["Hardware", "What each server is and its power figures"],
  data_quality: ["Data quality", "Estimated vs measured figures, missing prices, small samples"],
  activities: ["All activities", "Every game, run and experiment in scope with its cost"],
  methodology: ["How costs are calculated", "The method, in plain words"],
};
const KINDS = [["game", "Games"], ["benchmark", "Benchmark games"], ["probe", "Probe runs"], ["lab", "Lab games"], ["report", "Report narratives"]];
const ITEM_KIND = { benchmark_run: "Benchmark run", probe: "Probe", probe_run: "Probe run", lab_experiment: "Lab experiment", game: "Game" };

let draft = null;

function blankSpec(preset) {
  return { title: preset ? preset.name : "CITAR report", preset: preset ? preset.id : null, range: { preset: "all", from: "", to: "" },
    scope: { servers: [], models: [], kinds: [], items: [] }, sections: preset ? [...preset.sections] : ["summary", "costs", "models"],
    options: { electricity_basis: "full", lifespan_range: [2, 6] },
    narrative: { enabled: false, server_id: null, model_id: null, profile_id: null, instructions: "" } };
}

export async function renderReports(root, reportId) {
  if (reportId) return renderViewer(root, reportId);
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("reports"));
  const builder = el("div", { class: "card" });
  const list = el("div", { class: "card" });
  page.append(builder, list);
  let options = null;
  try { options = await api.reportOptions(); } catch (e) { toast(e.message, "error"); return {}; }
  if (!draft) draft = blankSpec(options.presets.find((p) => p.id === "model_comparison"));
  const ctx = { options, refreshList: null };
  const drawBuilder = () => renderBuilder(builder, ctx, drawBuilder);
  let last = "";
  const refreshList = async (force = false) => {
    try {
      const reports = await api.reports();
      const key = JSON.stringify(reports);
      if (force || key !== last) keepPlace(list, () => renderList(list, reports, ctx, drawBuilder));
      last = key;
    } catch (e) { /* server restarting */ }
  };
  ctx.refreshList = refreshList;
  drawBuilder();
  await refreshList(true);
  const timer = setInterval(() => refreshList(false), 2500);
  return { destroy() { clearInterval(timer); } };
}

// ---------------------------------------------------------------------------- builder
function chip(label, on, toggle, title) {
  return el("button", { class: `chip-toggle ${on ? "on" : ""}`, title: title || "", onclick: toggle }, (on ? "✓ " : "") + label);
}

function renderBuilder(card, ctx, redraw) {
  const S = draft;
  const o = ctx.options;
  clear(card);
  card.appendChild(el("div", { class: "row" }, el("h2", { style: { margin: 0 } }, "Report builder"),
    el("span", { class: "muted" }, "Choose what to include, then run it. Reports are made only when you run them."),
    el("span", { class: "grow" }), el("button", { class: "small", onclick: () => { draft = blankSpec(null); redraw(); } }, "Clear")));
  // presets
  card.appendChild(el("div", { class: "preset-grid" }, ...o.presets.map((p) => el("button", {
    class: `preset ${S.preset === p.id ? "on" : ""}`, onclick: () => {
      S.preset = p.id; S.sections = [...p.sections];
      if (!S.title || o.presets.some((q) => q.name === S.title) || S.title === "CITAR report") S.title = p.name;
      redraw();
    } }, el("b", {}, p.name), el("div", { class: "muted small" }, p.description)))));
  const field = (label, input, extra = {}) => el("div", { class: "field", ...extra }, el("label", {}, label), input);
  const range = el("select", { onchange: (e) => { S.range.preset = e.target.value; redraw(); } },
    ...[["all", "All time"], ["today", "Today"], ["24h", "Last 24 hours"], ["7d", "Last 7 days"], ["30d", "Last 30 days"], ["90d", "Last 90 days"],
      ["365d", "Last year"], ["custom", "Custom dates"]].map(([v, t]) => el("option", { value: v, selected: S.range.preset === v }, t)));
  card.appendChild(el("div", { class: "grid2", style: { marginTop: "10px" } },
    field("Title", el("input", { value: S.title, oninput: (e) => { S.title = e.target.value; } })),
    field("Period", range),
    S.range.preset === "custom" ? field("From", el("input", { type: "date", value: S.range.from, onchange: (e) => { S.range.from = e.target.value; } })) : null,
    S.range.preset === "custom" ? field("To (inclusive)", el("input", { type: "date", value: S.range.to, onchange: (e) => { S.range.to = e.target.value; } })) : null));
  // scope
  const toggle = (arr, v) => { const i = arr.indexOf(v); if (i >= 0) arr.splice(i, 1); else arr.push(v); redraw(); };
  card.appendChild(el("h3", { class: "section" }, "Scope ", el("span", { class: "muted small" }, "(nothing selected = everything)")));
  card.appendChild(el("div", { class: "scope-row" }, el("span", { class: "muted" }, "Activity types"),
    ...KINDS.map(([k, t]) => chip(t, S.scope.kinds.includes(k), () => toggle(S.scope.kinds, k)))));
  card.appendChild(el("div", { class: "scope-row" }, el("span", { class: "muted" }, "Servers"),
    ...o.servers.map((s) => chip(s.name, S.scope.servers.includes(s.id), () => toggle(S.scope.servers, s.id)))));
  card.appendChild(el("div", { class: "scope-row" }, el("span", { class: "muted" }, "Models"),
    ...o.models.map((m) => chip(m, S.scope.models.includes(m), () => toggle(S.scope.models, m)))));
  // specific items
  const itemBox = el("div", { class: "item-picker" });
  const search = el("input", { placeholder: "Filter runs, experiments, games…", style: { width: "100%" } });
  const drawItems = () => {
    clear(itemBox);
    const q = search.value.toLowerCase();
    const items = o.items.filter((it) => !q || `${it.name} ${it.id} ${ITEM_KIND[it.kind]}`.toLowerCase().includes(q)).slice(0, 80);
    if (!o.items.length) { itemBox.appendChild(el("div", { class: "muted small" }, "Nothing recorded yet. Games, benchmark runs, probe runs and lab experiments appear here once they have run.")); return; }
    for (const it of items) {
      const on = S.scope.items.some((x) => x.kind === it.kind && x.id === it.id);
      itemBox.appendChild(el("label", { class: "item-line" }, el("input", { type: "checkbox", checked: on, onchange: (e) => {
        if (e.target.checked) S.scope.items.push({ kind: it.kind, id: it.id, name: it.name });
        else S.scope.items = S.scope.items.filter((x) => !(x.kind === it.kind && x.id === it.id));
        selCount.textContent = S.scope.items.length ? `${S.scope.items.length} selected` : "";
      } }), el("span", { class: "pill" }, ITEM_KIND[it.kind] || it.kind), el("span", {}, it.name),
      el("span", { class: "muted small" }, `${it.count > 1 ? it.count + " activities · " : ""}${it.t ? new Date(it.t * 1000).toLocaleString() : ""}`)));
    }
  };
  search.addEventListener("input", drawItems);
  const selCount = el("span", { class: "muted small" }, S.scope.items.length ? `${S.scope.items.length} selected` : "");
  card.appendChild(el("details", { open: S.scope.items.length > 0 }, el("summary", {}, "Specific runs, experiments or games ", selCount), search, itemBox));
  drawItems();
  // sections
  card.appendChild(el("h3", { class: "section" }, "Sections"));
  card.appendChild(el("div", { class: "section-grid" }, ...o.sections.map((k) => {
    const [t, d] = SECTION_INFO[k] || [k, ""];
    return el("label", { class: `section-pick ${S.sections.includes(k) ? "on" : ""}` },
      el("input", { type: "checkbox", checked: S.sections.includes(k), onchange: (e) => {
        if (e.target.checked) S.sections = o.sections.filter((x) => x === k || S.sections.includes(x));
        else S.sections = S.sections.filter((x) => x !== k);
        S.preset = null; redraw();
      } }), el("div", {}, el("b", {}, t), el("div", { class: "muted small" }, d)));
  })));
  // options
  const opt = S.options;
  card.appendChild(el("div", { class: "grid2", style: { marginTop: "10px" } },
    field("Electricity counted", el("select", { onchange: (e) => { opt.electricity_basis = e.target.value; } },
      el("option", { value: "full", selected: opt.electricity_basis === "full" }, "Full draw (idle share + work)"),
      el("option", { value: "marginal", selected: opt.electricity_basis === "marginal" }, "Only the extra power of the work"))),
    field("Lifespan range for sensitivity (years)", el("div", { class: "row" },
      el("input", { type: "number", min: 1, max: 20, value: opt.lifespan_range[0], style: { width: "70px" }, oninput: (e) => { opt.lifespan_range[0] = +e.target.value; } }), "to",
      el("input", { type: "number", min: 1, max: 30, value: opt.lifespan_range[1], style: { width: "70px" }, oninput: (e) => { opt.lifespan_range[1] = +e.target.value; } })))));
  // narrative
  const n = S.narrative;
  const narrBox = el("div");
  const narr = el("div", { class: "card inner" }, el("label", { class: "row" }, el("input", { type: "checkbox", checked: n.enabled, onchange: (e) => { n.enabled = e.target.checked; redraw(); } }),
    el("b", {}, "Have a model write an analysis"), el("span", { class: "muted small" }, "Optional. The computed findings and charts never need a model; this adds a written summary. Its cost is tracked like any other use.")), narrBox);
  if (n.enabled) {
    // the picker writes server_id / model_id / profile_id straight into the narrative settings
    const redrawNarr = () => { clear(narrBox).append(llmForm(n, redrawNarr, { seat: false })); };
    narrBox.append(llmForm(n, redrawNarr, { seat: false }));
    narr.appendChild(el("div", { class: "field" }, el("label", {}, "Extra instructions (optional)"),
      el("textarea", { rows: 2, value: n.instructions, placeholder: "e.g. Focus on whether the desktop is worth it compared with the Claude API.", oninput: (e) => { n.instructions = e.target.value; } })));
  }
  card.appendChild(narr);
  const run = el("button", { class: "primary", style: { marginTop: "10px" }, onclick: async () => {
    if (!S.sections.length) { toast("Pick at least one section", "error"); return; }
    run.disabled = true;
    try {
      const m = await api.startReport(S);
      toast(`Running "${m.title}"…`);
      ctx.refreshList(true);
    } catch (e) { toast(e.message, "error"); } finally { run.disabled = false; }
  } }, "▶ Run report");
  card.appendChild(el("div", { class: "row" }, run, el("span", { class: "muted small" }, `${S.sections.length} section${S.sections.length === 1 ? "" : "s"}`)));
}

// ---------------------------------------------------------------------------- list
function renderList(card, reports, ctx, redrawBuilder) {
  clear(card);
  card.appendChild(el("h2", {}, "Reports"));
  if (!reports.length) { card.appendChild(el("p", { class: "muted" }, "No reports yet. Build one above and press Run report.")); return; }
  const table = el("table", { class: "list" }, el("tr", {}, ...["Report", "Status", "Created", "Total cost", "Findings", ""].map((h) => el("th", {}, h))));
  for (const r of reports) {
    const sm = r.summary || {};
    const busy = ["queued", "running"].includes(r.status);
    table.appendChild(el("tr", {},
      el("td", {}, el("b", {}, r.title), r.narrative ? el("div", { class: "muted small" }, "with written analysis") : null),
      el("td", {}, el("span", { class: `pill ${busy ? "live" : r.status === "done" ? "good" : r.status === "failed" ? "bad" : ""}` }, r.status),
        busy ? el("div", { class: "muted small" }, r.progress || "") : null,
        r.status === "failed" ? el("div", { class: "bad small", title: r.trace || "" }, r.error) : null,
        r.seconds ? el("div", { class: "muted small" }, `took ${secs(r.seconds)}`) : null),
      el("td", {}, new Date(r.created * 1000).toLocaleString()),
      el("td", {}, sm.total != null ? `${sm.symbol || "$"}${sm.total >= 1 ? sm.total.toFixed(2) : sm.total.toFixed(4)}` : "–",
        sm.activities != null ? el("div", { class: "muted small" }, `${sm.activities} activities`) : null),
      el("td", { class: "small", style: { maxWidth: "420px" } }, ...(sm.findings || []).slice(0, 3).map((f) => el("div", { class: "muted" }, "• " + f))),
      el("td", { class: "actions" }, el("div", { class: "row" },
        r.status === "done" ? el("a", { class: "button small primary", href: `#/reports/${r.id}` }, "View") : null,
        r.status === "done" ? el("a", { class: "button small", href: `/api/reports/${r.id}/html`, target: "_blank" }, "Open in new tab") : null,
        r.status === "done" ? el("a", { class: "button small", href: `/api/reports/${r.id}/download` }, "Download") : null,
        !busy ? el("button", { class: "small", title: "Run the same report again with fresh data", onclick: async () => {
          try { await api.rerunReport(r.id); ctx.refreshList(true); } catch (e) { toast(e.message, "error"); }
        } }, "Re-run") : null,
        el("button", { class: "small", title: "Load this report's choices into the builder", onclick: async () => {
          try { draft = (await api.report(r.id)).spec; redrawBuilder(); window.scrollTo({ top: 0, behavior: "smooth" }); } catch (e) { toast(e.message, "error"); }
        } }, "Edit copy"),
        !busy ? el("button", { class: "small danger", onclick: async () => {
          if (!(await confirmBox("Delete report", `Delete "${r.title}"?`))) return;
          try { await api.deleteReport(r.id); ctx.refreshList(true); } catch (e) { toast(e.message, "error"); }
        } }, "Delete") : null))));
  }
  card.appendChild(el("div", { style: { overflowX: "auto" } }, table));
}

// ---------------------------------------------------------------------------- viewer
async function renderViewer(root, id) {
  const page = el("div", { class: "report-viewer" });
  root.appendChild(page);
  page.appendChild(pageHeader("reports"));
  let meta = {};
  try { meta = (await api.report(id)).meta; } catch (e) { toast(e.message, "error"); }
  page.appendChild(el("div", { class: "row viewer-bar" }, el("a", { href: "#/reports" }, "← Reports"), el("b", {}, meta.title || id),
    el("span", { class: "grow" }),
    el("a", { class: "button small", href: `/api/reports/${id}/html`, target: "_blank" }, "Open in new tab"),
    el("a", { class: "button small primary", href: `/api/reports/${id}/download` }, "Download HTML")));
  page.appendChild(el("iframe", { class: "report-frame", src: `/api/reports/${id}/html`, title: meta.title || "Report",
    sandbox: "allow-popups allow-popups-to-escape-sandbox" }));
  return {};
}

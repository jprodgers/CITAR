// Map generation options shared by the new-game form and the map editor's "generate" dialog: what happens at the
// map's edges, how many rivers there are, and how much of each resource is generated (see mapgen.MapOptions).
import { el } from "./util.js";

export const MAP_EDGES = [
  ["ice_caps", "Ice caps north and south", "Polar ice one to four tiles deep at the top and bottom; open ocean at the sides."],
  ["wrap_x", "Wrap east-west (globe)", "Sailing off the east edge comes back on the west. Ice caps north and south."],
  ["wrap_y", "Wrap north-south", "The top and bottom join; no ice. Open ocean east and west."],
  ["wrap_both", "Wrap both ways (no edges)", "A world with no edges and no ice: every direction wraps around."],
  ["boxed", "Boxed in (ice on all sides)", "Ice one to four tiles deep along all four edges."],
];

const MODES = [["normal", "Normal"], ["off", "Off"], ["cap", "At most…"], ["share", "Share %"]];

// resources the generator can place (not the city-state-only ones, not ones that never generate)
function generatable(rules, kind) {
  return Object.entries(rules.resources || {})
    .filter(([, d]) => d.resourceType === kind)
    .filter(([, d]) => !(d.uniques || []).some((u) => u === "Doesn't generate naturally" || u.startsWith("Can only be created by")))
    .map(([k]) => k).sort();
}

// A percentage slider with its number beside it. value() is a fraction (100% -> 1).
function percent(value, max, title) {
  const out = el("span", { class: "mo-pct" }, `${value}%`);
  const input = el("input", { type: "range", min: 0, max, step: 5, value, title,
    oninput: () => { out.textContent = `${input.value}%`; } });
  return { node: el("div", { class: "mo-slider" }, input, out), value: () => +input.value / 100,
           set: (v) => { input.value = Math.round(v * 100); out.textContent = `${input.value}%`; } };
}

// The resource table for one kind: a mode and a number per resource.
function resourceTable(names) {
  const rows = {};
  const body = el("div", { class: "mo-res" });
  for (const name of names) {
    const mode = el("select", {}, ...MODES.map(([v, t]) => el("option", { value: v }, t)));
    const num = el("input", { type: "number", min: 0, max: 1000, step: 1, style: { visibility: "hidden" } });
    const sync = () => {
      num.style.visibility = mode.value === "cap" || mode.value === "share" ? "visible" : "hidden";
      num.title = mode.value === "cap" ? "The most tiles of it on the whole map" : "Percent of all the resources of this kind";
      if (mode.value === "cap" && num.value === "") num.value = 1;
      if (mode.value === "share" && num.value === "") num.value = 25;
      row.classList.toggle("changed", mode.value !== "normal");
    };
    mode.onchange = sync;
    const row = el("div", { class: "mo-res-row" }, el("span", { class: "mo-res-name" }, name), mode, num);
    rows[name] = { mode, num, sync };
    body.appendChild(row);
  }
  return {
    node: body,
    value() {
      const each = {};
      for (const [name, r] of Object.entries(rows)) {
        if (r.mode.value === "normal") continue;
        each[name] = r.mode.value === "off" ? { mode: "off" } : { mode: r.mode.value, value: Math.max(0, +r.num.value || 0) };
      }
      return each;
    },
    reset() { for (const r of Object.values(rows)) { r.mode.value = "normal"; r.num.value = ""; r.sync(); } },
    set(each) {
      this.reset();
      for (const [name, rule] of Object.entries(each || {})) {
        const r = rows[name];
        if (!r || !rule || !MODES.some(([v]) => v === rule.mode)) continue;
        r.mode.value = rule.mode;
        r.num.value = rule.value ?? "";
        r.sync();
      }
    },
  };
}

// The whole form. Returns { node, value() } where value() is the config fragment
// { map_edges, river_density, resources } to merge into a game config or a map-generation request.
// `initial` is such a fragment to start from; `onChange(value)` is called whenever the form changes.
export function mapOptionsForm(rules, { open = false, initial = null, onChange = null } = {}) {
  const edges = el("select", {}, ...MAP_EDGES.map(([v, t, d]) => el("option", { value: v, title: d }, t)));
  const edgeNote = el("div", { class: "muted mo-note" });
  const syncEdge = () => { edgeNote.textContent = (MAP_EDGES.find((e) => e[0] === edges.value) || [])[2] || ""; };
  edges.onchange = syncEdge;
  syncEdge();
  const rivers = percent(100, 300, "0% = no rivers, 100% = normal");
  const overall = percent(100, 300, "Scales every kind of resource");
  const strat = percent(100, 400, "Strategic resources (iron, horses, oil...)");
  const lux = percent(100, 400, "Luxury resources");
  const bonus = percent(100, 300, "Bonus resources (wheat, cattle, fish...)");
  const stratTable = resourceTable(generatable(rules, "Strategic"));
  const luxTable = resourceTable(generatable(rules, "Luxury"));
  const summary = el("span", { class: "muted mo-summary" });
  const field = (label, input) => el("div", { class: "field" }, el("label", {}, label), input);

  const form = {
    value() {
      return {
        map_edges: edges.value,
        river_density: rivers.value(),
        resources: {
          density: overall.value(),
          strategic: { density: strat.value(), each: stratTable.value() },
          luxury: { density: lux.value(), each: luxTable.value() },
          bonus: { density: bonus.value() },
        },
      };
    },
  };
  const refreshSummary = () => {
    const v = form.value();
    const bits = [(MAP_EDGES.find((e) => e[0] === v.map_edges) || [])[1]];
    if (v.river_density !== 1) bits.push(`rivers ${Math.round(v.river_density * 100)}%`);
    const r = v.resources;
    if (r.density !== 1) bits.push(`resources ${Math.round(r.density * 100)}%`);
    if (r.strategic.density !== 1) bits.push(`strategic ${Math.round(r.strategic.density * 100)}%`);
    if (r.luxury.density !== 1) bits.push(`luxury ${Math.round(r.luxury.density * 100)}%`);
    if (r.bonus.density !== 1) bits.push(`bonus ${Math.round(r.bonus.density * 100)}%`);
    const n = Object.keys(r.strategic.each).length + Object.keys(r.luxury.each).length;
    if (n) bits.push(`${n} resource rule${n === 1 ? "" : "s"}`);
    summary.textContent = " — " + bits.join(", ");
  };
  const reset = el("button", { class: "small", type: "button", onclick: () => {
    edges.value = "ice_caps"; syncEdge();
    for (const p of [rivers, overall, strat, lux, bonus]) p.set(1);
    stratTable.reset(); luxTable.reset();
    refreshSummary();
  } }, "Reset to defaults");

  form.node = el("details", { class: "map-options", open },
    el("summary", {}, el("b", {}, "Map generation"), summary),
    el("div", { class: "grid2" },
      field("Map edges", el("div", {}, edges, edgeNote)),
      field("Rivers", rivers.node),
      field("All resources", overall.node),
      field("Strategic resources", strat.node),
      field("Luxury resources", lux.node),
      field("Bonus resources", bonus.node)),
    el("p", { class: "muted mo-note" },
      "Per resource: Off never places it; At most caps how many tiles of it the whole map gets (1 = a single source); ",
      "Share % makes it that percent of every resource of its kind. The kind's density still sets the total."),
    el("div", { class: "mo-tables" },
      el("div", {}, el("h4", {}, "Strategic"), stratTable.node),
      el("div", {}, el("h4", {}, "Luxury"), luxTable.node)),
    el("div", { class: "row" }, reset));
  if (initial) {
    const r = initial.resources || {};
    if (MAP_EDGES.some((e) => e[0] === initial.map_edges)) { edges.value = initial.map_edges; syncEdge(); }
    if (initial.river_density != null) rivers.set(initial.river_density);
    if (r.density != null) overall.set(r.density);
    if ((r.strategic || {}).density != null) strat.set(r.strategic.density);
    if ((r.luxury || {}).density != null) lux.set(r.luxury.density);
    if ((r.bonus || {}).density != null) bonus.set(r.bonus.density);
    stratTable.set((r.strategic || {}).each);
    luxTable.set((r.luxury || {}).each);
  }
  const changed = () => { refreshSummary(); if (onChange) onChange(form.value()); };
  form.node.addEventListener("input", changed);
  form.node.addEventListener("change", changed);
  reset.addEventListener("click", () => { if (onChange) onChange(form.value()); });
  refreshSummary();
  return form;
}

export function edgesLabel(key) {
  return (MAP_EDGES.find((e) => e[0] === key) || [null, key])[1];
}

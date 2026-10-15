// The machine editors of the Servers page (#/pool, pool.js): dialogs opened from a machine's card for its connection and
// models with their load profiles, its hardware and power, its costs, its quiet hours and its API key, and the
// host-wide dialogs an administrator opens from the page's head (electricity plans, the server's settings). Plus the
// registry the seat and suite pickers read.
//
// Every request goes through `call`, which asks machines.js's `mayCall` first, and every control is shown only when
// `may` says its route would take it: a member's page never offers, nor calls, a route that would refuse them (the
// host-wide routes, the host's hardware scan, LM Studio's devices, the routes that dial a machine), and an
// administrator's shows the host's own actions on the host alone (design/phase3-features.md P3.2.8, 3-06).
import { api } from "./api.js";
import { el, clear, toast, modal, confirmBox, prompt } from "./util.js";
import { mayCall } from "./machines.js";
import * as auth from "./auth.js";

export const KIND = { owned: "Owned", leased: "Leased / rented", api: "Online API", test: "Test" };
export const PROVIDER = { lmstudio: "LM Studio", ollama: "Ollama", openai_compatible: "OpenAI-compatible endpoint", anthropic: "Anthropic API",
  dryrun: "Dry run (no model)", none: "None (no models)" };
const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const KEY_BACKENDS = { keyring: "OS credential store", env: "Environment variable", file: "Encrypted file (passphrase)", none: "No key" };
/** The providers whose machine may need an API key on the server (a machine the server dials). */
export const KEYED = ["anthropic", "openai_compatible", "ollama", "lmstudio"];
/** The providers the server can ask for their models. */
const DETECTABLE = ["lmstudio", "ollama", "openai_compatible", "anthropic", "dryrun"];
const clone = (x) => JSON.parse(JSON.stringify(x));
const newId = (p) => p + Math.random().toString(16).slice(2, 10);

// ---------------------------------------------------------------------------- the gate

/** The signed-in viewer as machines.js's gates read them, or null. */
export function viewer() {
  return auth.user() ? { admin: auth.isAdmin(), register: auth.can("register_servers") } : null;
}

/** Whether the viewer may call the route `name` (api.js's name) for `target` (a machine or group, or null). */
export function may(name, target = null) {
  return mayCall(name, viewer(), target);
}

/** Call the route `name` for `target` with `args`, if the viewer may: the page's one way to the machine routes. */
export function call(name, target, ...args) {
  if (!may(name, target)) return Promise.reject(new Error("That is not yours to do on this machine."));
  return api[name](...args);
}

// ---------------------------------------------------------------------------- the registry's view

// The machines the caller manages as the registry has them (every one for an administrator), with the electricity
// plans, the key backends and the token prices the editors offer.
let state = null;

/** Read the registry's view afresh (`GET /api/servers`): what the editors edit. */
export async function loadRegistry() {
  const reg = await call("servers", null);
  for (const sv of reg.servers) sv.can_manage = true;         // the route lists only what the caller manages
  state = reg;
  return reg;
}

export function money(v) {
  if (v == null) return "–";
  const s = (state && state.currency_symbol) || "$";
  const a = Math.abs(v);
  return a >= 100 ? `${s}${v.toFixed(0)}` : a >= 1 ? `${s}${v.toFixed(2)}` : a >= 0.01 ? `${s}${v.toFixed(3)}` : a === 0 ? `${s}0` : `${s}${v.toFixed(4)}`;
}

/** A machine's connection in a line (its registry view). */
export function describeConn(sv) {
  const c = sv.connection;
  if (sv.reach === "worker") return `Through the CITAR helper on that machine (${PROVIDER[c.provider] || c.provider})`;
  if (c.provider === "anthropic") return `Anthropic API${c.base_url ? " at " + c.base_url : ""}`;
  if (c.provider === "dryrun") return "Pretend model for testing setups (costs nothing)";
  if (c.provider === "none") return "No model runtime (runs the game engine and bots only)";
  const via = c.lmstudio_device ? ` · LM Link device “${c.lmstudio_device}” (reached through this PC's LM Studio)` : "";
  return `${PROVIDER[c.provider] || c.provider} at ${c.base_url}${via}`;
}

/** A machine's quiet hours in a line, with the zone its times are in when given. */
export function describeQuiet(rh, zone) {
  if (!rh || !rh.enabled || !(rh.windows || []).length) return "No quiet hours";
  const days = (d) => d.length === 7 ? "every day" : d.join(",") === "0,1,2,3,4" ? "weekdays" : d.join(",") === "5,6" ? "weekends" : d.map((i) => DAYS[i]).join(" ");
  return "Quiet " + rh.windows.map((w) => `${w.start}–${w.end} ${days(w.days || [])}`).join("; ") + (zone ? ` (${zone})` : "")
    + (rh.unload_models ? " · models unloaded" : "");
}

// What a dialog sends to the save route: the editor's copy without what only the page keeps.
function payload(S) {
  const { can_manage: _m, isNew: _n, ...rest } = S;
  return rest;
}

// Save a machine's whole configuration (`POST /api/servers`, MANAGE on it), for every dialog that edits it.
async function saveMachine(S) {
  const saved = await call("saveServer", S, payload(S));
  Object.assign(S, saved, { can_manage: true });
  delete S.isNew;
  return saved;
}

// A dialog over a copy of the machine: tabs, a body drawn by `pages[tab]`, a footer with Close and Save. `onSaved`
// (the page's refresh) runs after each save.
function editorDialog(title, S, pages, onSaved, { extra = null, saveLabel = "Save" } = {}) {
  let tab = Object.keys(pages)[0];
  const body = el("div", { class: "col" });
  const tabs = el("div", { class: "tabbar" });
  const draw = () => {
    const names = Object.keys(pages);
    clear(tabs).append(...(names.length > 1 ? names.map((k) => el("button", { class: tab === k ? "active" : "", onclick: () => { tab = k; draw(); } }, pages[k][0])) : []));
    clear(body).appendChild(pages[tab][1](S, draw));
  };
  draw();
  const save = async (close) => {
    try {
      const saved = await saveMachine(S);
      toast(`${saved.name} saved`);
      if (onSaved) onSaved(saved);
      if (close) m.close(); else draw();
      return saved;
    } catch (e) { toast(e.message, "error"); return null; }
  };
  const m = modal({ title, content: el("div", { class: "col" }, tabs, body, extra), footer: [
    el("button", { onclick: () => m.close() }, "Close"), el("button", { class: "primary", onclick: () => save(true) }, saveLabel)] });
  return m;
}

// ---------------------------------------------------------------------------- small form helpers
const field = (label, input, hint, extra = {}) => el("div", { class: "field", ...extra }, el("label", {}, label), input,
  hint ? el("span", { class: "muted small" }, hint) : null);

function txt(obj, key, attrs = {}) {
  return el("input", { value: obj[key] ?? "", ...attrs, oninput: (e) => { obj[key] = e.target.value; } });
}
function num(obj, key, attrs = {}) {
  return el("input", { type: "number", value: obj[key] ?? "", ...attrs, oninput: (e) => { obj[key] = e.target.value === "" ? null : +e.target.value; } });
}
function sel(obj, key, options, onchange) {
  return el("select", { onchange: (e) => { obj[key] = e.target.value; if (onchange) onchange(); } },
    ...options.map(([v, t]) => el("option", { value: v, selected: String(obj[key] ?? "") === String(v) }, t)));
}
const QUANTS = [["", "default"], ["f32", "f32"], ["f16", "f16"], ["q8_0", "q8_0"], ["q5_1", "q5_1"], ["q5_0", "q5_0"],
  ["q4_1", "q4_1"], ["q4_0", "q4_0"], ["iq4_nl", "iq4_nl"]];

// a three-state setting: leave it to LM Studio, on, or off
function triChk(obj, key) {
  return el("select", { onchange: (e) => { obj[key] = e.target.value === "" ? null : e.target.value === "yes"; } },
    ...[["", "LM Studio default"], ["yes", "on"], ["no", "off"]].map(([v, t]) =>
      el("option", { value: v, selected: (obj[key] == null ? "" : obj[key] ? "yes" : "no") === v }, t)));
}

function chk(obj, key, label, onchange) {
  return el("label", { class: "row", style: { gap: "4px" } }, el("input", { type: "checkbox", checked: !!obj[key], onchange: (e) => { obj[key] = e.target.checked; if (onchange) onchange(); } }), label);
}

// ---------------------------------------------------------------------------- adding a machine the server dials

/** Start a new machine of `kind` and `provider` (`GET /api/servers/new`) and open its connection dialog. An
 *  administrator's choices; a member's is the dry run alone (the page offers no other). */
export async function addDirectMachine(kind, provider, title, onSaved) {
  try {
    const sv = await call("newServer", { isNew: true }, kind, provider);
    sv.name = title.replace(/^\S+ /, "").replace(/^A |^An /, "");
    if (provider === "anthropic") {
      sv.name = "Anthropic API";
      sv.models = Object.keys(state.anthropic_prices).slice(0, 4).map((k) => ({ key: k }));
    }
    if (provider === "dryrun") sv.models = [{ key: "dry-run" }];
    openConnection(sv, onSaved, { isNew: true });
  } catch (e) { toast(e.message, "error"); }
}

// ---------------------------------------------------------------------------- the server's settings (administrators)

/** The host-wide settings: the currency and which machine is the host. */
export function openSettings(onSaved) {
  const s = { currency: state.currency, currency_symbol: state.currency_symbol, host_server_id: state.host_server_id || "" };
  const m = modal({ title: "Server settings", narrow: true, content: el("div", { class: "col" },
    field("Currency code", txt(s, "currency", { placeholder: "USD" })),
    field("Currency symbol", txt(s, "currency_symbol", { placeholder: "$" })),
    field("Machine running CITAR", sel(s, "host_server_id", [["", "(none)"], ...state.servers.filter((x) => x.reach !== "worker" && x.kind !== "api" && x.kind !== "test").map((x) => [x.id, x.name])]),
      "Its CPU runs the game engine and scripted bots, so it gets a share of every game's cost; lab experiments run on it; its power is sampled live.")),
    footer: [el("button", { onclick: () => m.close() }, "Cancel"), el("button", { class: "primary", onclick: async () => {
      try { await call("serverSettings", null, s); m.close(); if (onSaved) onSaved(); } catch (e) { toast(e.message, "error"); }
    } }, "Save")] });
}

// ---------------------------------------------------------------------------- connection and models

// A dialog's copy of a machine (`S`) is what the gates read of it: can_manage, reach, is_host and its connection.

/** The machine's connection and its models (with LM Studio's load profiles), and its name, kind and notes. */
export function openConnection(original, onSaved, { isNew = false } = {}) {
  const S = clone(original);
  S.is_host = !!original.is_host;
  S.can_manage = true;
  if (isNew) S.isNew = true;
  editorDialog(isNew ? "New machine" : `Connection & models — ${S.name}`, S,
    { general: ["General", general], connection: ["Connection", connection], models: ["Models", models] }, onSaved);
}

function kindsFor(S) {
  if (S.reach === "worker") return Object.entries(KIND).filter(([k]) => k === "owned" || k === "leased" || k === S.kind);
  if (!viewer().admin) return Object.entries(KIND).filter(([k]) => k === "test" || k === S.kind);
  return Object.entries(KIND);
}

function general(S, draw) {
  const admin = viewer().admin;
  return el("div", { class: "col" },
    el("div", { class: "grid2" },
      field("Name", txt(S, "name")),
      field("Kind", sel(S, "kind", kindsFor(S), draw), "Owned: you bought it (depreciation + electricity). Leased: you rent it (monthly/hourly). API: pay per token."),
      admin && S.reach !== "worker" && S.kind !== "api" && S.kind !== "test"
        ? field("Role", chk(S, "is_host", " This is the machine running CITAR"), "Runs the game engine and bots; its power is sampled live.") : null),
    field("Description", el("textarea", { rows: 2, value: S.description || "", oninput: (e) => { S.description = e.target.value; } })),
    field("Notes", el("textarea", { rows: 3, value: S.notes || "", oninput: (e) => { S.notes = e.target.value; } })));
}

function providersFor(S) {
  const admin = viewer().admin;
  const list = S.reach === "worker" ? ["lmstudio", "ollama", "openai_compatible"]
    : !admin ? ["dryrun"]                                   // a member's machine the server dials is the dry run
      : S.kind === "api" ? ["anthropic", "openai_compatible"] : S.kind === "test" ? ["dryrun"]
        : ["lmstudio", "ollama", "openai_compatible", "none"];
  return list.includes(S.connection.provider) ? list : [S.connection.provider, ...list];
}

function connection(S, draw) {
  const c = S.connection;
  const box = el("div", { class: "col" });
  const provider = field(S.reach === "worker" ? "What serves the model" : "Provider", sel(c, "provider", providersFor(S).map((p) => [p, PROVIDER[p] || p]), () => {
    if (S.reach !== "worker" && c.provider === "lmstudio" && !c.base_url) c.base_url = "http://localhost:1234/v1";
    if (S.reach !== "worker" && c.provider === "ollama") c.base_url = c.base_url || "http://localhost:11434/v1";
    draw();
  }));
  const slots = c.provider !== "none" ? field("Requests at once", num(c, "max_parallel", { min: 1, max: 16 }),
    "How many model requests the server sends this machine at the same time (its LLM slots).") : null;
  if (S.reach === "worker") {
    box.append(el("div", { class: "grid2" }, provider, slots),
      el("p", { class: "muted small" }, "The CITAR helper on this machine reaches its model server: the address, and an API key if the model ",
        "server needs one, are set there, never on this server. Its tokens are under Helper on the machine's card."));
    return box;
  }
  const admin = viewer().admin;
  let devices = null;
  if (c.provider === "lmstudio" && may("lmDevices")) {
    devices = el("select", { onchange: (e) => { c.lmstudio_device = e.target.value; } }, el("option", { value: "" }, "This PC"));
    if (c.lmstudio_device) devices.appendChild(el("option", { value: c.lmstudio_device, selected: true }, c.lmstudio_device));
    call("lmDevices", null).then((d) => {
      for (const p of (d.peers || [])) {
        if (p.name === c.lmstudio_device) continue;
        devices.appendChild(el("option", { value: p.name, selected: c.lmstudio_device === p.name }, `${p.name} (LM Link, ${p.status})`));
      }
    }).catch(() => {});
  }
  box.appendChild(el("div", { class: "grid2" }, provider,
    ["lmstudio", "ollama", "openai_compatible"].includes(c.provider) ? field("Base URL", txt(c, "base_url", { placeholder: "http://localhost:1234/v1" })) : null,
    c.provider === "anthropic" ? field("Base URL (optional)", txt(c, "base_url", { placeholder: "default Anthropic endpoint" })) : null,
    devices ? field("Where the models run", devices, "LM Link lets this PC's LM Studio run models on another of your PCs; pick that device here.") : null,
    c.provider === "lmstudio" ? field("Model loading", chk(c, "manage_loading", " CITAR loads each model with its load profile"), "Needs LM Studio's lms CLI on this PC.") : null,
    slots,
    ["lmstudio", "ollama", "openai_compatible", "anthropic"].includes(c.provider) ? field("Request timeout (s)", num(c, "timeout", { min: 5, placeholder: "default" })) : null,
    c.provider === "dryrun" ? field("Seconds per pretend call", num(c, "dry_run_delay", { min: 0, step: 0.5 })) : null));
  if (KEYED.includes(c.provider)) {
    const st = S.key_status || { backend: "none", present: false };
    box.appendChild(el("div", { class: "row" },
      el("span", { class: st.present ? "good small" : "muted small" }, st.present ? `🔑 key stored (${KEY_BACKENDS[st.backend]})` : st.backend === "none" ? "No API key." : "🔑 no key found yet"),
      !S.isNew && may("storeKey", S) ? el("button", { class: "small", onclick: () => openKey(S, (r) => { S.key_status = r.key_status; draw(); }) }, "API key…") : null,
      S.isNew ? el("span", { class: "muted small" }, "Save the machine first, then store its key.") : null));
  }
  if (!admin) box.appendChild(el("p", { class: "muted small" }, "A machine the server connects to itself is an administrator's to point at an address: yours is the dry run, which dials nothing."));
  return box;
}

function modelInfo(m) {
  const i = m.info || {};
  return [i.params, i.quant, i.size_gb ? `${i.size_gb} GB` : null, i.max_context ? `${(i.max_context / 1024).toFixed(0)}k ctx max` : null,
    i.device, i.tool_use === false ? "no native tools" : null].filter(Boolean).join(" · ");
}

function models(S, draw) {
  const c = S.connection;
  const box = el("div", { class: "col" });
  const status = el("span", { class: "muted small" });
  const detected = el("div", { class: "col" });
  const loadState = {};
  const helper = S.reach === "worker";
  const lm = c.provider === "lmstudio" && !helper;
  const canDetect = !S.isNew && DETECTABLE.includes(c.provider) && may("detectModels", S);
  const detect = async () => {
    status.textContent = "Contacting the server…";
    try {
      const r = await call("detectModels", S, S.id);
      clear(detected);
      if (!r.ok) { status.textContent = r.error; status.className = "bad small"; return; }
      status.className = "good small";
      const known = new Set(S.models.map((m) => m.key));
      const extra = r.models.filter((m) => !known.has(m.key));
      status.textContent = `${r.models.length} model(s) on the server; ${extra.length} not in the catalog.`;
      if (extra.length) {
        detected.append(el("div", { class: "row" }, el("b", {}, "Available to add"), el("button", { class: "small", onclick: () => {
          for (const m of extra) S.models.push(newModel(m.key, m.info, m.price)); draw();
        } }, "Add all")),
        ...extra.map((m) => el("div", { class: "row model-row" }, el("code", {}, m.key), el("span", { class: "muted small" }, modelInfo(m)),
          m.loaded ? el("span", { class: "pill live" }, "loaded") : null,
          el("button", { class: "small", onclick: () => { S.models.push(newModel(m.key, m.info, m.price)); draw(); } }, "+ Add"))));
      }
      for (const m of r.models) {
        const mine = S.models.find((x) => x.key === m.key);
        if (mine) { mine.info = { ...(mine.info || {}), ...(m.info || {}) }; loadState[m.key] = m.loaded; }
      }
      drawCatalog();
    } catch (e) { status.textContent = e.message; }
  };
  const newModel = (key, info, price) => ({ id: newId("m_"), key, label: "", enabled: true, info: info || {},
    inference: { tool_mode: lm || c.provider !== "anthropic" ? "auto" : "native", reasoning_effort: lm || c.provider === "ollama" ? "low" : "",
      effort: c.provider === "anthropic" ? "medium" : "", max_tokens: null, temperature: null, max_tool_calls_per_turn: 150 },
    profiles: lm ? [{ id: "p_default", name: "Default", context: 32768, gpu: "max", parallel: null, ttl: null, exclusive: true, extra: "" }] : [],
    default_profile: lm ? "p_default" : null, price: null });
  const catalog = el("div", { class: "col" });
  const drawCatalog = () => {
    clear(catalog);
    if (!S.models.length) catalog.appendChild(el("p", { class: "muted" }, helper ? "Its helper has reported no models yet." : "No models yet: detect them or add one by id."));
    for (const m of S.models) catalog.appendChild(modelEditor(S, m, loadState, drawCatalog));
  };
  const manual = el("input", { placeholder: "model id / key", style: { flex: 1 } });
  box.append(
    canDetect ? el("div", { class: "row" }, el("button", { class: "small", onclick: detect }, "Detect models"), status) : null,
    helper ? el("p", { class: "muted small" }, helperNote(S, "models")) : null,
    detected,
    el("h3", { class: "section" }, "Catalog"),
    el("p", { class: "muted small" }, "Models listed here can be picked for seats, suites and probes. " +
      (lm ? "Load profiles say how LM Studio loads the model (context length, GPU offload, parallel slots…); seats pick a profile." : "")),
    catalog,
    helper ? null : el("div", { class: "row" }, manual, el("button", { class: "small", onclick: () => {
      const k = manual.value.trim(); if (!k || S.models.some((m) => m.key === k)) return;
      S.models.push(newModel(k, {}, null)); manual.value = ""; drawCatalog();
    } }, "+ Add model")));
  drawCatalog();
  if (canDetect && c.provider !== "dryrun") setTimeout(detect, 0);
  return box;
}

function modelEditor(S, m, loadState, redraw) {
  const c = S.connection;
  const lm = c.provider === "lmstudio" && S.reach !== "worker";
  const loads = lm && !S.isNew && may("loadModel", S);
  const open = modelEditor.open || (modelEditor.open = new Set());
  const expanded = open.has(m.id);
  const row = el("div", { class: `model-edit ${m.enabled ? "" : "disabled"}` });
  row.appendChild(el("div", { class: "row" },
    el("input", { type: "checkbox", checked: m.enabled, title: "Offer this model", onchange: (e) => { m.enabled = e.target.checked; redraw(); } }),
    el("code", {}, m.key), loadState[m.key] || (S.reach === "worker" && m.loaded) ? el("span", { class: "pill live" }, "loaded") : null,
    S.reach === "worker" && m.reported === false ? el("span", { class: "pill muted", title: "Its helper's last report did not "
      + "name it: kept in the catalogue until you remove it" }, "not reported") : null,
    el("span", { class: "muted small" }, modelInfo(m)), el("span", { class: "grow" }),
    el("input", { value: m.label || "", placeholder: "display name", style: { width: "180px" }, oninput: (e) => { m.label = e.target.value; } }),
    el("button", { class: "small", onclick: () => { expanded ? open.delete(m.id) : open.add(m.id); redraw(); } }, expanded ? "▾ Settings" : "▸ Settings"),
    // a model its helper reports comes back with its next report while models are auto: removable once it is not
    // reported, or when the catalogue is its owner's (models manual)
    S.reach === "worker" && m.reported !== false && (S.modes || {}).models !== "manual" ? null
      : el("button", { class: "small danger", title: "Remove from catalog", onclick: () => { S.models.splice(S.models.indexOf(m), 1); redraw(); } }, "✕")));
  if (!expanded) return row;
  const inf = m.inference = m.inference || {};
  const inner = el("div", { class: "col", style: { paddingLeft: "22px" } });
  inner.appendChild(el("div", { class: "grid2" },
    c.provider !== "anthropic" && c.provider !== "dryrun" ? field("Tool calls", sel(inf, "tool_mode", [["auto", "auto"], ["native", "native"], ["json", "JSON fallback"]])) : null,
    c.provider !== "anthropic" && c.provider !== "dryrun" ? field("Reasoning effort", sel(inf, "reasoning_effort",
          [["", "model default"], ["low", "low"], ["medium", "medium"], ["high", "high"], ["on", "on (models with a thinking switch)"], ["off", "off (no thinking)"]]),
          "Models that only have a thinking on/off switch turn 'low' into 'on'; pick 'off' to stop them writing thousands of reasoning tokens per turn.") : null,
    c.provider === "anthropic" ? field("Effort", sel(inf, "effort", ["", "low", "medium", "high", "xhigh", "max"].map((v) => [v, v || "default"]))) : null,
    field("Max output tokens", num(inf, "max_tokens", { min: 0, placeholder: "default" })),
    c.provider !== "anthropic" ? field("Temperature", num(inf, "temperature", { min: 0, max: 2, step: 0.1, placeholder: "default" })) : null,
    field("Max tool calls / turn", num(inf, "max_tool_calls_per_turn", { min: 5 }))));
  if (lm) {
    inner.appendChild(el("h4", {}, "Load profiles"));
    const est = el("pre", { style: { display: "none" } });
    for (const p of m.profiles) {
      p.loaded_by = p.loaded_by || (p.use_defaults ? "lmstudio_defaults" : "citar");
      const mine = p.loaded_by === "citar";
      const prow = el("div", { class: "profile-row" },
        el("label", { title: "Default profile" }, el("input", { type: "radio", name: `def_${m.id}`, checked: m.default_profile === p.id, onchange: () => { m.default_profile = p.id; } })),
        field("Name", txt(p, "name")),
        field("Loaded by", sel(p, "loaded_by", [["citar", "CITAR, with these settings"], ["lmstudio_defaults", "LM Studio's own saved settings"],
          ["manual", "I load it in LM Studio myself"]], redraw),
          p.loaded_by === "manual" ? "CITAR never loads or unloads it; it must be loaded before a run starts." :
            p.loaded_by === "lmstudio_defaults" ? "CITAR loads it, but leaves every setting to LM Studio." : null),
        mine ? field("Context", num(p, "context", { min: 0, step: 1024 })) : null,
        mine ? field("GPU offload", txt(p, "gpu", { placeholder: "max | off | 0.5", style: { width: "90px" } })) : null,
        field("Parallel", num(p, "parallel", { min: 0, placeholder: "auto", style: { width: "70px" } })),
        field("Unload after idle (s)", num(p, "ttl", { min: 0, placeholder: "never", style: { width: "90px" } })),
        p.loaded_by === "manual" ? null : field("Exclusive", chk(p, "exclusive", " unload others")),
        loads ? el("div", { class: "col" },
          el("button", { class: "small", title: "Memory estimate (lms --estimate-only)", onclick: async () => {
            est.style.display = "block"; est.textContent = "Estimating…";
            try { const r = await call("estimateLoad", S, S.id, m.id, p.id); est.textContent = r.text || r.error; } catch (e) { est.textContent = e.message + " (save the machine first)"; }
          } }, "Estimate"),
          el("button", { class: "small", title: "Load now with this profile", onclick: async () => {
            try { await call("loadModel", S, S.id, m.id, p.id); toast(`Loading ${m.key}…`); pollLoad(S, m.key); } catch (e) { toast(e.message, "error"); }
          } }, "Load now"),
          el("button", { class: "small", title: "Read the settings this model is loaded with right now in LM Studio and copy them into this profile",
            onclick: async () => {
              try {
                const r = await call("loadedConfig", S, S.id, m.id);
                Object.assign(p, r.profile, { id: p.id, name: p.name, loaded_by: "citar" });
                toast("Copied the settings the model is loaded with"); redraw();
              } catch (e) { toast(e.message, "error", 7000); }
            } }, "Copy loaded settings")) : null,
        m.profiles.length > 1 ? el("button", { class: "small danger", onclick: () => { m.profiles.splice(m.profiles.indexOf(p), 1); redraw(); } }, "✕") : null);
      inner.appendChild(prow);
      if (mine) inner.appendChild(el("details", { class: "profile-advanced" }, el("summary", { class: "muted small" }, `Advanced load settings — ${p.name}`),
        el("div", { class: "grid2" },
          field("Flash attention", triChk(p, "flash_attention")),
          field("KV cache on GPU", triChk(p, "offload_kv_to_gpu")),
          field("Strict VRAM cap", triChk(p, "strict_vram_cap"), "Fail instead of spilling into system RAM"),
          field("Keep model in memory", triChk(p, "keep_in_memory")),
          field("Try mmap", triChk(p, "try_mmap")),
          field("Experts (MoE models)", num(p, "num_experts", { min: 0, placeholder: "model default" })),
          field("Eval batch size", num(p, "eval_batch", { min: 1, placeholder: "default" })),
          field("K cache quantization", sel(p, "k_cache_quant", QUANTS)),
          field("V cache quantization", sel(p, "v_cache_quant", QUANTS)),
          field("Main GPU", num(p, "main_gpu", { min: 0, placeholder: "auto" })),
          field("Multi-GPU split", sel(p, "split_strategy", [["", "default"], ["evenly", "evenly"], ["favorMainGpu", "favour main GPU"]])),
          field("Extra lms CLI flags", txt(p, "extra", { placeholder: "--speculative-draft-mtp" }), "Only used if LM Studio's Python SDK is missing")),
        el("div", { class: "field" }, el("label", {}, "Raw LM Studio load settings (one per line, key = value)"),
          el("textarea", { rows: 2, value: (p.fields || []).map((f) => `${f.key} = ${JSON.stringify(f.value)}`).join("\n"),
            placeholder: "llm.load.numCpuExpertLayersRatio = 0", oninput: (e) => {
              p.fields = e.target.value.split("\n").map((ln) => {
                const i = ln.indexOf("=");
                if (i < 0) return null;
                const key = ln.slice(0, i).trim();
                let value = ln.slice(i + 1).trim();
                try { value = JSON.parse(value); } catch (err) { /* keep the text */ }
                return key.startsWith("llm.load.") ? { key, value } : null;
              }).filter(Boolean);
            } }),
          el("span", { class: "muted small" }, "For settings LM Studio has but its SDK doesn't, e.g. forcing a mixture-of-experts model's experts onto the GPU."))));
    }
    inner.append(el("div", { class: "row" },
      el("button", { class: "small", onclick: () => {
        const last = m.profiles[m.profiles.length - 1] || {};
        m.profiles.push({ ...clone(last), id: newId("p_"), name: `Profile ${m.profiles.length + 1}` }); redraw();
      } }, "+ Add profile"),
      loads ? el("button", { class: "small", onclick: async () => {
        try { await call("unloadModel", S, S.id, m.id); toast("Unloaded"); } catch (e) { toast(e.message, "error"); }
      } }, "Unload now") : null), est);
  }
  if (S.kind === "api" || S.kind === "leased") {
    const builtin = state.anthropic_prices[m.key];
    m.price = m.price || null;
    const pr = m.price || {};
    const setp = (k) => el("input", { type: "number", step: 0.01, min: 0, value: pr[k] ?? "", placeholder: builtin ? String(builtin[k]) : "", style: { width: "90px" },
      oninput: (e) => { m.price = m.price || {}; m.price[k] = e.target.value === "" ? null : +e.target.value; if (Object.values(m.price).every((v) => v == null)) m.price = null; } });
    inner.append(el("h4", {}, `Token prices (${state.currency_symbol} per million)`),
      el("div", { class: "row" }, field("Input", setp("input")), field("Output", setp("output")), field("Cache write", setp("cache_write")), field("Cache read", setp("cache_read"))),
      el("span", { class: "muted small" }, builtin ? "Leave blank to use Anthropic's list prices shown as placeholders." : "Prices for this model (otherwise the cost period's default price is used)."));
  }
  row.appendChild(inner);
  return row;
}

async function pollLoad(S, key) {
  for (let i = 0; i < 600; i++) {
    await new Promise((r) => setTimeout(r, 2000));
    try {
      const st = (await call("serverLoading", S, S.id))[key];
      if (st && st.status === "done") { toast(`${key} is loaded${st.seconds ? ` (${Math.round(st.seconds)}s)` : ""}`); return; }
      if (st && st.status === "failed") { toast(st.error, "error", 8000); return; }
    } catch (e) { return; }
  }
}

// What a helper machine's helper does to a part of its configuration, by the part's mode (Machine settings…, on its
// card; citar/machine_settings.py): hardware and models auto and power manual unless its owner changed them.
function helperNote(S, section) {
  const mode = (S.modes || {})[section] || (section === "power" ? "manual" : "auto");
  const what = { hardware: "hardware details", models: "models", power: "power figures" }[section];
  if (section === "models") {
    return mode === "auto"
      ? "Its helper reports this machine's models each time it connects: new ones are added, loaded ones marked, and one "
        + "it no longer reports stays, marked as not reported, until you remove it. Here you choose which to offer, "
        + "their names and how they are prompted. (Models: auto, in Machine settings…)"
      : "Models: manual, in Machine settings…: its helper's reports only mark which models are loaded; nothing is added "
        + "or removed. Here you choose which to offer, their names and how they are prompted.";
  }
  return mode === "auto"
    ? `Its helper's scan replaces the ${what} here each time it connects (${section === "hardware" ? "Hardware" : "Power"}: `
      + "auto, in Machine settings…): set it to manual there to keep what you enter."
    : `${section === "hardware" ? "Hardware" : "Power"}: manual, in Machine settings…: the ${what} here are yours. Its `
      + "helper's scan is kept beside them as the last scan, to compare and apply there.";
}

// ---------------------------------------------------------------------------- hardware and power

/** The machine's hardware (collected, imported or entered) and its power figures. */
export function openHardware(original, onSaved) {
  const S = clone(original);
  S.can_manage = true;
  const rates = el("div", { class: "muted small" });
  const showRates = () => call("serverRates", S, S.id).then((r) => {
    rates.textContent = `With the saved settings an hour costs ≈ ${money(r.idle.total)} idle and ${money(r.busy.total)} with the model generating` +
      (r.busy.kwh_price != null ? ` (electricity ${money(r.busy.kwh_price)}/kWh).` : " (no electricity rate).");
  }).catch(() => {});
  showRates();
  editorDialog(`Hardware & power — ${S.name}`, S, { hardware: ["Hardware", hardware], power: ["Power", power] },
    (saved) => { showRates(); if (onSaved) onSaved(saved); }, { extra: rates });
}

function hardware(S, draw) {
  const hw = S.hardware && S.hardware.source !== "none" ? S.hardware : (S.hardware = { source: "manual", cpu: {}, memory: {}, gpus: [], physical_disks: [] });
  hw.cpu = hw.cpu || {}; hw.memory = hw.memory || {}; hw.gpus = hw.gpus || []; hw.physical_disks = hw.physical_disks || []; hw.system = hw.system || {};
  const file = el("input", { type: "file", accept: ".json,application/json", style: { display: "none" }, onchange: async (e) => {
    const f = e.target.files[0]; if (!f) return;
    try {
      const data = JSON.parse(await f.text());
      const r = await call("importHardware", S, S.id, data);
      Object.assign(S, { hardware: r.hardware, power: r.power, hardware_summary: r.hardware_summary });
      toast(`Imported hardware of ${data.hostname || "the machine"}`); draw();
    } catch (err) { toast(`Import failed: ${err.message}`, "error"); }
  } });
  const box = el("div", { class: "col" });
  box.append(
    el("div", { class: "row" }, el("b", {}, S.hardware_summary || "No hardware details yet"), el("span", { class: "pill" }, hw.source || "manual"),
      hw.collected_at ? el("span", { class: "muted small" }, `collected ${hw.collected_at} on ${hw.hostname}`) : null),
    S.reach === "worker" ? el("p", { class: "muted small" }, helperNote(S, "hardware")) : null,
    el("div", { class: "card inner" },
      el("h3", {}, "Collect automatically"),
      may("collectHardware", S) ? el("div", { class: "row" }, el("button", { class: "primary small", onclick: async () => {
        try {
          toast("Collecting… (a few seconds)");
          const r = await call("collectHardware", S, S.id);
          Object.assign(S, { hardware: r.hardware, power: r.power, hardware_summary: r.hardware_summary }); draw();
        } catch (e) { toast(e.message, "error"); }
      } }, "Collect from this PC"), el("span", { class: "muted small" }, "CPU, GPUs, RAM, disks and local model runtimes.")) : null,
      el("p", { class: "muted small" }, "On the machine itself, run a collector and import its output:"),
      el("div", { class: "col", style: { gap: "2px" } },
        el("div", { class: "row" }, el("a", { href: "/api/hardware-collector/collect_hardware.py", download: "collect_hardware.py" }, "collect_hardware.py"),
          el("span", { class: "muted small" }, "Windows, Linux, macOS — needs Python 3")),
        el("div", { class: "row" }, el("a", { href: "/api/hardware-collector/collect_hardware.sh", download: "collect_hardware.sh" }, "collect_hardware.sh"),
          el("span", { class: "muted small" }, "Linux or macOS, no Python needed")),
        el("div", { class: "row" }, el("a", { href: "/api/hardware-collector/collect_hardware.ps1", download: "collect_hardware.ps1" }, "collect_hardware.ps1"),
          el("span", { class: "muted small" }, "Windows, no Python needed"))),
      el("pre", {}, "python3 collect_hardware.py > my-machine.json\nsh collect_hardware.sh > my-machine.json\npowershell -ExecutionPolicy Bypass -File collect_hardware.ps1 > my-machine.json"),
      may("importHardware", S) ? el("div", { class: "row" }, file, el("button", { class: "small", onclick: () => file.click() }, "Import JSON…")) : null),
    el("h3", { class: "section" }, "Details (edit or enter manually)"),
    el("div", { class: "grid2" },
      field("Form", sel(hw.system, "form", [["", "unknown"], ["desktop", "desktop"], ["laptop", "laptop"], ["server", "server"]])),
      field("CPU", txt(hw.cpu, "model")), field("Cores", num(hw.cpu, "cores", { min: 1 })), field("Threads", num(hw.cpu, "threads", { min: 1 })),
      field("RAM (GB)", num(hw.memory, "ram_gb", { min: 0 })),
      field("Memory", chk(hw.memory, "unified", " unified (shared by CPU and GPU)"))));
  const gpus = el("div", { class: "col" });
  const drawGpus = () => {
    clear(gpus).appendChild(el("div", { class: "row" }, el("b", {}, "GPUs"), el("button", { class: "small", onclick: () => { hw.gpus.push({ name: "", vram_gb: null }); drawGpus(); } }, "+ Add")));
    for (const g of hw.gpus) gpus.appendChild(el("div", { class: "row" }, txt(g, "name", { placeholder: "name", style: { minWidth: "260px" } }),
      num(g, "vram_gb", { placeholder: "VRAM GB", style: { width: "100px" } }), el("span", { class: "muted small" }, g.power_readable ? "power readable" : ""),
      el("button", { class: "small danger", onclick: () => { hw.gpus.splice(hw.gpus.indexOf(g), 1); drawGpus(); } }, "✕")));
  };
  const disks = el("div", { class: "col" });
  const drawDisks = () => {
    clear(disks).appendChild(el("div", { class: "row" }, el("b", {}, "Disks"), el("button", { class: "small", onclick: () => { hw.physical_disks.push({ name: "", size_gb: null, type: "SSD" }); drawDisks(); } }, "+ Add")));
    for (const d of hw.physical_disks) disks.appendChild(el("div", { class: "row" }, txt(d, "name", { placeholder: "name", style: { minWidth: "260px" } }),
      num(d, "size_gb", { placeholder: "GB", style: { width: "100px" } }), sel(d, "type", [["SSD", "SSD"], ["HDD", "HDD"]]),
      el("button", { class: "small danger", onclick: () => { hw.physical_disks.splice(hw.physical_disks.indexOf(d), 1); drawDisks(); } }, "✕")));
  };
  drawGpus(); drawDisks();
  box.append(gpus, disks);
  if (hw.runtimes && hw.runtimes.length) box.appendChild(el("details", {}, el("summary", { class: "muted" }, "Model runtimes found"),
    el("pre", {}, hw.runtimes.map((r) => `${r.name}: ${r.models.map((m) => m.key + (m.device && m.device !== "local" ? " (remote)" : "")).join(", ")}`).join("\n"))));
  if (hw.volumes && hw.volumes.length) box.appendChild(el("div", { class: "muted small" }, "Volumes: " + hw.volumes.map((v) => `${v.mount} ${v.free_gb}/${v.total_gb} GB free`).join(" · ")));
  return box;
}

function power(S, draw) {
  const p = S.power;
  const sug = ((S.hardware || {}).power || {}).suggested;
  return el("div", { class: "col" },
    S.reach === "worker" ? el("p", { class: "muted small" }, helperNote(S, "power")) : null,
    el("p", { class: "muted" }, "Wall-socket watts. Energy is estimated from these whenever CITAR can't measure it: idle + CPU watts × CPU load + GPU watts × " +
      "the share of time the model was generating." + (S.is_host ? " On this machine, the one running CITAR, GPU power (nvidia-smi) and CPU load are also sampled live every few seconds."
        : S.reach === "worker" ? " Its CITAR helper samples GPU power (nvidia-smi) and CPU load every 10 seconds and sends them once a minute, unless it was started with --no-power." : "")),
    el("div", { class: "grid2" },
      field("Idle (W)", num(p, "idle_w", { min: 0 }), "Whole machine, sitting idle"),
      field("Extra at full CPU load (W)", num(p, "cpu_max_w", { min: 0 })),
      field("Extra with the GPU busy (W)", num(p, "gpu_max_w", { min: 0 }), "While a model generates"),
      field("Figures are", sel(p, "source", [["manual", "entered by me"], ["measured", "measured with a meter"], ["guess", "a rough guess"], ["unset", "unset"]])),
      S.is_host || S.reach === "worker" ? field("Live sampling", sel(p, "sampling", [["auto", "on"], ["off", "off"]]),
        S.is_host ? "" : "Off: its helper's samples are not kept") : null,
      field("Measured GPU → wall overhead (%)", num(p, "measured_overhead_pct", { min: 0, max: 100 }), "Power-supply losses on top of the GPU's own reading")),
    sug ? el("div", { class: "row" }, el("span", { class: "muted small" }, `Suggested from the hardware: idle ${sug.idle_w} W, CPU +${sug.cpu_max_w} W, GPU +${sug.gpu_max_w} W. ${sug.note || ""}`),
      el("button", { class: "small", onclick: () => { Object.assign(p, { idle_w: sug.idle_w, cpu_max_w: sug.cpu_max_w, gpu_max_w: sug.gpu_max_w, source: "guess" }); draw(); } }, "Use suggestion")) : null);
}

// ---------------------------------------------------------------------------- costs

/** What the machine costs over time: its components' depreciation and its cost periods. */
export function openCosts(original, onSaved) {
  const S = clone(original);
  S.can_manage = true;
  const rates = el("div", { class: "muted small" });
  const showRates = () => call("serverRates", S, S.id).then((r) => {
    rates.textContent = S.kind === "api" ? (r.idle.total ? `Fixed ${money(r.idle.total)}/h + tokens` : "Pay per token") :
      `With the saved settings an hour costs ≈ ${money(r.idle.total)} idle and ${money(r.busy.total)} with the model generating.`;
  }).catch(() => {});
  showRates();
  editorDialog(`Costs — ${S.name}`, S, { costs: ["Costs", costs] }, (saved) => { showRates(); if (onSaved) onSaved(saved); }, { extra: rates });
}

function costs(S, draw) {
  const box = el("div", { class: "col" });
  if (S.kind === "owned") {
    box.appendChild(el("h3", {}, "Components (depreciated straight-line)"));
    box.appendChild(el("p", { class: "muted small" }, "The purchase price less resale value is spread evenly over the lifespan from the purchase date. " +
      "Add parts bought later (a new GPU) as their own lines. Reports also show the cost under other lifespans."));
    const tbl = el("table", { class: "list" }, el("tr", {}, ...["Component", `Price (${state.currency_symbol})`, "Purchased", "Lifespan (years)", "Resale value", "Retired", ""].map((h) => el("th", {}, h))));
    for (const c of S.components) {
      tbl.appendChild(el("tr", {}, el("td", {}, txt(c, "name")), el("td", {}, num(c, "price", { min: 0, step: 1, style: { width: "100px" } })),
        el("td", {}, el("input", { type: "date", value: c.purchased || "", onchange: (e) => { c.purchased = e.target.value; } })),
        el("td", {}, num(c, "lifespan_years", { min: 0.25, step: 0.5, style: { width: "80px" } })),
        el("td", {}, num(c, "resale", { min: 0, style: { width: "90px" } })),
        el("td", {}, el("input", { type: "date", value: c.retired || "", onchange: (e) => { c.retired = e.target.value || null; } })),
        el("td", {}, el("button", { class: "small danger", onclick: () => { S.components.splice(S.components.indexOf(c), 1); draw(); } }, "✕"))));
    }
    box.append(tbl, el("div", {}, el("button", { class: "small", onclick: () => {
      S.components.push({ id: newId("c_"), name: "", price: null, purchased: "", lifespan_years: 4, resale: 0, retired: null }); draw();
    } }, "+ Add component")));
  }
  box.appendChild(el("h3", { class: "section" }, "Cost periods"));
  box.appendChild(el("p", { class: "muted small" }, "Each period applies from its date until the next one, so a price change never rewrites history: " +
    "add a new period from the day it changed."));
  // a member is given only the plans their machines already carry (an administrator put them there): the site's
  // plans are the household's bill, not a member's to price a machine with
  const plans = [["", "(none)"], ...state.electricity_plans.map((p) => [p.id, p.name])];
  S.costs.forEach((p, i) => {
    const card = el("div", { class: "card inner" });
    card.append(el("div", { class: "row" }, el("b", {}, i === 0 ? "From the start" : "From"),
      i === 0 ? null : el("input", { type: "date", value: p.from, onchange: (e) => { p.from = e.target.value; } }),
      el("span", { class: "grow" }), i > 0 ? el("button", { class: "small danger", onclick: () => { S.costs.splice(i, 1); draw(); } }, "Remove") : null),
      el("div", { class: "grid2" },
        (S.kind === "owned" || S.kind === "leased") && plans.length > 1 ? field("Electricity plan", sel(p, "electricity_plan_id", plans), S.kind === "leased" ? "Only if you pay its power" : null) : null,
        S.kind !== "api" ? field(`Fixed monthly (${state.currency_symbol})`, num(p, "fixed_monthly", { min: 0, step: 0.01 }), S.kind === "leased" ? "Lease / rent" : "Maintenance, internet share, software…") : null,
        S.kind !== "api" ? field("What the fixed cost is", txt(p, "fixed_note")) : null,
        S.kind === "leased" ? field(`Hourly rate while in use (${state.currency_symbol}/h)`, num(p, "hourly_rate", { min: 0, step: 0.01 }), "Cloud GPUs billed by the hour") : null,
        S.kind === "api" ? field(`Subscription / fixed monthly (${state.currency_symbol})`, num(p, "api_fixed_monthly", { min: 0, step: 0.01 })) : null),
      S.kind === "api" || S.kind === "leased" ? priceEditor(p) : null,
      field("Note", txt(p, "note")));
    box.appendChild(card);
  });
  box.appendChild(el("div", { class: "row" }, el("button", { class: "small", onclick: async () => {
    const d = await prompt("New cost period", "Applies from (YYYY-MM-DD):", new Date().toISOString().slice(0, 10));
    if (!d) return;
    S.costs.push({ ...clone(S.costs[S.costs.length - 1]), from: d }); S.costs.sort((a, b) => a.from.localeCompare(b.from)); draw();
  } }, "+ Add a change from a date"),
  may("savePlan") ? el("button", { class: "small", onclick: () => openPlans(async () => { await loadRegistry(); draw(); }) }, "Edit electricity plans") : null));
  if (!may("savePlan") && (S.kind === "owned" || S.kind === "leased")) {
    box.appendChild(el("p", { class: "muted small" }, "The electricity plans are the site's bill, an administrator's to set: put your own power cost in the fixed monthly figure."));
  }
  return box;
}

function priceEditor(p) {
  const d = p.default_price || {};
  const inp = (k) => el("input", { type: "number", min: 0, step: 0.01, value: d[k] ?? "", style: { width: "90px" }, oninput: (e) => {
    p.default_price = p.default_price || {}; p.default_price[k] = e.target.value === "" ? null : +e.target.value;
    if (Object.values(p.default_price).every((v) => v == null)) p.default_price = null;
  } });
  return el("div", { class: "col" }, el("span", { class: "muted small" }, `Default token prices (${state.currency_symbol} per million) for models without their own:`),
    el("div", { class: "row" }, field("Input", inp("input")), field("Output", inp("output")), field("Cache write", inp("cache_write")), field("Cache read", inp("cache_read"))));
}

// ---------------------------------------------------------------------------- quiet hours

function windowsEditor(list, draw, extraFields) {
  const box = el("div", { class: "col" });
  for (const w of list) {
    w.days = w.days || [0, 1, 2, 3, 4, 5, 6];
    box.appendChild(el("div", { class: "row window-row" },
      ...DAYS.map((d, i) => el("label", { class: "day" }, el("input", { type: "checkbox", checked: w.days.includes(i), onchange: (e) => {
        w.days = e.target.checked ? [...new Set([...w.days, i])].sort() : w.days.filter((x) => x !== i);
      } }), d)),
      el("input", { type: "time", value: w.start, onchange: (e) => { w.start = e.target.value; } }), "to",
      el("input", { type: "time", value: w.end, onchange: (e) => { w.end = e.target.value; } }),
      ...(extraFields ? extraFields(w) : []),
      el("button", { class: "small danger", onclick: () => { list.splice(list.indexOf(w), 1); draw(); } }, "✕")));
  }
  return box;
}

/** The hours a machine must rest: nothing runs on it then, for anyone, its owner included. `machine` is its card's
 *  machine (the pool's entry, `reg` its registry view, `owner_tz` the zone a helper machine's hours are in). */
export function openQuietHours(machine, onSaved) {
  const reg = machine.reg || machine;
  const rh = clone(reg.restricted_hours || { enabled: false, windows: [], unload_models: false, grace_minutes: 15 });
  rh.windows = rh.windows || [];
  const helper = machine.reach === "worker";
  const lmLoads = !helper && reg.connection && reg.connection.provider === "lmstudio";
  const body = el("div", { class: "col" });
  const draw = () => {
    clear(body).append(
      el("p", { class: "muted" }, "Nothing runs on the machine in these hours, for anyone, its owner included: games, benchmark jobs, probe runs ",
        "and lab model runs finish the model turn in progress, pause, and carry on by themselves when the hours end. A window that ends before ",
        "it starts runs overnight into the next morning. ",
        helper ? `Times are in its owner's time zone (${machine.owner_tz || "UTC"}).` : "Times are on this server's clock."),
      chk(rh, "enabled", " Quiet hours on", draw),
      windowsEditor(rh.windows, draw),
      el("div", { class: "row" },
        el("button", { class: "small", onclick: () => { rh.windows.push({ days: [0, 1, 2, 3, 4, 5, 6], start: "22:00", end: "06:00" }); rh.enabled = true; draw(); } }, "+ Add window"),
        el("button", { class: "small", onclick: () => { rh.windows = [{ days: [0, 1, 2, 3, 4, 5, 6], start: "22:00", end: "06:00" }]; rh.enabled = true; draw(); } }, "Every night 22:00–06:00")),
      el("div", { class: "grid2" },
        lmLoads ? field("Models", chk(rh, "unload_models", " Unload this machine's models when a window starts"), "Frees the GPU so the fans spin down.") : null,
        field("Grace for a turn in progress (minutes)", num(rh, "grace_minutes", { min: 0, max: 240 }), "After this the turn is abandoned and replayed later.")));
  };
  draw();
  const m = modal({ title: `Quiet hours — when ${machine.name} must rest`, content: body, footer: [
    el("button", { onclick: () => m.close() }, "Cancel"),
    el("button", { class: "primary", onclick: async () => {
      try {
        const saved = await call("setQuietHours", machine.reg || machine, machine.id, rh);
        toast(`${machine.name}: ${describeQuiet(saved.restricted_hours)}`, "good", 6000);
        m.close(); if (onSaved) onSaved(saved);
      } catch (e) { toast(e.message, "error"); }
    } }, "Save")] });
}

// ---------------------------------------------------------------------------- the API key (a machine the server dials)

/** Where the server keeps the API key of a machine it dials, and the key itself (never shown again). `done` gets the
 *  route's answer (`{key_status}`) after each change. */
export function openKey(original, done) {
  const S = clone(original);
  S.can_manage = true;
  const k = { backend: ((S.connection || {}).key || {}).backend || "none", env: ((S.connection || {}).key || {}).env || "" };
  if (S.key_status && S.key_status.backend) k.backend = S.key_status.backend;
  const body = el("div", { class: "col" });
  const changed = (r) => { S.key_status = r.key_status; if (done) done(r); draw(); };
  const draw = () => {
    const be = state.key_backends;
    // the backends this caller may choose: `env` (one of the server's own variables) is an administrator's alone
    const options = Object.entries(KEY_BACKENDS).filter(([v]) => v === "none" || be[v])
      .map(([v, t]) => [v, t + (be[v] && be[v].available === false ? " (unavailable)" : "")]);
    if (!options.some(([v]) => v === k.backend)) k.backend = "none";
    const status = S.key_status || { backend: "none", present: false };
    const secret = el("input", { type: "password", autocomplete: "off", placeholder: "paste the key", style: { minWidth: "320px" } });
    clear(body).append(
      el("p", { class: "muted small" }, "Keys never go in the project folder and are never shown again: after you paste one it lives in the store you pick. " +
        "The OS credential store is Windows Credential Manager, macOS Keychain or the Linux Secret Service."),
      el("div", { class: "grid2" },
        field("Stored in", sel(k, "backend", options, draw), be.keyring && k.backend === "keyring" ? be.keyring.name : null),
        k.backend === "env" ? field("Variable name", txt(k, "env", { placeholder: "ANTHROPIC_API_KEY" })) : null));
    if (k.backend === "file" && be.file && !be.file.unlocked) {
      if (may("unlockKeys")) {
        const pass = el("input", { type: "password", autocomplete: "off", placeholder: "passphrase" });
        body.appendChild(el("div", { class: "row" }, pass, el("button", { class: "small", onclick: async () => {
          try { state.key_backends = await call("unlockKeys", null, pass.value); toast("Key file unlocked for this session"); draw(); } catch (e) { toast(e.message, "error"); }
        } }, be.file.exists ? "Unlock key file" : "Set passphrase"), el("span", { class: "muted small" }, be.file.name)));
      } else {
        body.appendChild(el("p", { class: "warn small" }, "The key file is locked: an administrator unlocks it for this server's run."));
      }
    }
    if (k.backend === "keyring" || k.backend === "file") {
      body.appendChild(el("div", { class: "row" }, secret, el("button", { class: "small primary", onclick: async () => {
        if (!secret.value.trim()) { toast("Paste the key first", "error"); return; }
        try {
          const r = await call("storeKey", S, S.id, k.backend, secret.value.trim(), null);
          secret.value = "";
          toast("Key stored");
          changed(r);
        } catch (e) { toast(e.message, "error"); }
      } }, "Store key")));
    }
    if (k.backend === "env") {
      body.appendChild(el("div", { class: "row" }, el("button", { class: "small", onclick: async () => {
        try { changed(await call("storeKey", S, S.id, "env", null, k.env)); } catch (e) { toast(e.message, "error"); }
      } }, "Use this variable")));
    }
    if (k.backend === "none" && status.backend !== "none") {
      body.appendChild(el("div", { class: "row" }, el("button", { class: "small", onclick: async () => {
        try { changed(await call("storeKey", S, S.id, "none", null, null)); } catch (e) { toast(e.message, "error"); }
      } }, "Use no key")));
    }
    body.appendChild(el("div", { class: status.present ? "good small" : "muted small" }, status.backend === "none" ? "No key configured." :
      status.present ? `A key is stored${status.hint ? " (ends " + status.hint.slice(1) + ")" : ""}.` : status.locked ? "The key file is locked." : "No key found yet."));
    if (status.present && status.backend !== "env") body.appendChild(el("button", { class: "small danger", onclick: async () => {
      if (!(await confirmBox("Remove key", "Remove the stored key for this machine?"))) return;
      try { changed(await call("deleteKey", S, S.id)); } catch (e) { toast(e.message, "error"); }
    } }, "Remove key"));
  };
  draw();
  const m = modal({ title: `API key — ${S.name}`, content: body, footer: [el("button", { onclick: () => m.close() }, "Close")] });
}

// ---------------------------------------------------------------------------- electricity plans (administrators)

/** The site's electricity plans: the household's bill, which the machines' cost periods name. */
export function openPlans(onSaved) {
  const plans = clone(state.electricity_plans);
  const body = el("div", { class: "col" });
  const draw = () => {
    clear(body);
    body.appendChild(el("p", { class: "muted" }, "Your electricity bill. Several machines can share a plan. Each period applies from its date, so rate changes don't rewrite history."));
    for (const p of plans) body.appendChild(planEditor(p, draw, plans));
    body.appendChild(el("button", { class: "small", onclick: () => {
      plans.push({ id: newId("ep_"), name: "New plan", periods: [{ from: "2000-01-01", type: "flat", rate_kwh: null, fixed_monthly: 0, fee_allocation: "household_kwh", household_kwh_month: null, share_pct: 0, tou: [], tiers: [], note: "" }], _new: true });
      draw();
    } }, "+ Add plan"));
  };
  draw();
  const m = modal({ title: "Electricity plans", content: body, footer: [el("button", { onclick: () => m.close() }, "Close"),
    el("button", { class: "primary", onclick: async () => {
      try { for (const p of plans) { const { _new, ...rest } = p; await call("savePlan", null, rest); } toast("Plans saved"); m.close(); if (onSaved) onSaved(); }
      catch (e) { toast(e.message, "error"); }
    } }, "Save plans")] });
}

function planEditor(p, draw, plans) {
  const card = el("div", { class: "card inner" });
  card.appendChild(el("div", { class: "row" }, txt(p, "name", { style: { fontWeight: 600, minWidth: "240px" } }), el("span", { class: "grow" }),
    el("button", { class: "small danger", onclick: async () => {
      if (p._new) { plans.splice(plans.indexOf(p), 1); draw(); return; }
      if (!(await confirmBox("Delete plan", `Delete "${p.name}"?`))) return;
      try { await call("deletePlan", null, p.id); plans.splice(plans.indexOf(p), 1); draw(); } catch (e) { toast(e.message, "error"); }
    } }, "Delete plan")));
  p.periods.forEach((q, i) => {
    q.tou = q.tou || []; q.tiers = q.tiers || [];
    const sym = state.currency_symbol;
    const box = el("div", { class: "period" },
      el("div", { class: "row" }, el("b", {}, i === 0 ? "From the start" : "From"),
        i === 0 ? null : el("input", { type: "date", value: q.from, onchange: (e) => { q.from = e.target.value; } }),
        el("span", { class: "grow" }), i > 0 ? el("button", { class: "small danger", onclick: () => { p.periods.splice(i, 1); draw(); } }, "Remove") : null),
      el("div", { class: "grid2" },
        field("Rate type", sel(q, "type", [["flat", "Flat rate"], ["tou", "Time of use"], ["tiered", "Tiered by monthly use"]], draw)),
        field(q.type === "flat" ? `Rate (${sym}/kWh)` : `Base rate (${sym}/kWh)`, num(q, "rate_kwh", { min: 0, step: 0.001 }), q.type === "tou" ? "Outside the windows below" : null),
        field(`Fixed monthly charge (${sym})`, num(q, "fixed_monthly", { min: 0, step: 0.01 }), "Customer / connection charge"),
        field("Charge the fixed fee", sel(q, "fee_allocation", [["household_kwh", "spread over the household's kWh"], ["share", "a set share to these machines"], ["none", "not at all"]], draw)),
        q.fee_allocation === "household_kwh" || q.type === "tiered" ? field("Household use (kWh / month)", num(q, "household_kwh_month", { min: 1 }), "From your bill") : null,
        q.fee_allocation === "share" ? field("Share of the fee (%)", num(q, "share_pct", { min: 0, max: 100 })) : null));
    if (q.type === "tou") {
      box.append(el("b", {}, "Time-of-use windows"), windowsEditor(q.tou, draw, (w) => [el("span", {}, `${sym}/kWh`), num(w, "rate_kwh", { min: 0, step: 0.001, style: { width: "90px" } }),
        txt(w, "name", { placeholder: "peak", style: { width: "90px" } })]),
      el("button", { class: "small", onclick: () => { q.tou.push({ days: [0, 1, 2, 3, 4], start: "16:00", end: "21:00", rate_kwh: null, name: "peak" }); draw(); } }, "+ Add window"));
    }
    if (q.type === "tiered") {
      box.append(el("b", {}, "Tiers (the rate at your monthly use applies)"), ...q.tiers.map((t) => el("div", { class: "row" }, "up to",
        num(t, "up_to_kwh", { min: 0, placeholder: "no limit", style: { width: "110px" } }), `kWh: ${sym}`, num(t, "rate_kwh", { min: 0, step: 0.001, style: { width: "90px" } }), "/kWh",
        el("button", { class: "small danger", onclick: () => { q.tiers.splice(q.tiers.indexOf(t), 1); draw(); } }, "✕"))),
      el("button", { class: "small", onclick: () => { q.tiers.push({ up_to_kwh: null, rate_kwh: null }); draw(); } }, "+ Add tier"));
    }
    card.appendChild(box);
  });
  card.appendChild(el("button", { class: "small", onclick: async () => {
    const d = await prompt("Rate change", "New rates apply from (YYYY-MM-DD):", new Date().toISOString().slice(0, 10));
    if (!d) return;
    p.periods.push({ ...clone(p.periods[p.periods.length - 1]), from: d }); p.periods.sort((a, b) => a.from.localeCompare(b.from)); draw();
  } }, "+ Add a rate change"));
  return card;
}

// ---------------------------------------------------------------------------- the machine picker (seats, suites, probes, reports)

let pickerCache = null;
/** The machines the caller manages, as the registry has them, cached for 15 s (lobby.js merges in the ones shared
 *  with them). */
export async function registry(force = false) {
  if (!pickerCache || force || Date.now() - pickerCache.t > 15000) pickerCache = { t: Date.now(), data: await call("servers", null) };
  return pickerCache.data;
}

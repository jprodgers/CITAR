// Lobby: running games, saves, and the new-game form.
import { api } from "./api.js";
import { mapOptionsForm } from "./mapoptions.js";
import { el, clear, toast, modal, confirmBox } from "./util.js";
import { openMetrics } from "./metrics.js";
import { setupState, welcomeBanner } from "./setup.js";
import { pageHeader } from "./nav.js";
import { registry } from "./servers.js";

const COLORS = ["#e04040", "#3c78d8", "#e8c547", "#8e44ad", "#27ae60", "#e67e22", "#17becf", "#f06292",
  "#8d6e63", "#9ccc65", "#5c6bc0", "#ff7043", "#26a69a", "#d4e157", "#ab47bc", "#78909c",
  "#b71c1c", "#0d47a1", "#f9a825", "#1b5e20", "#ff80ab", "#00e5ff", "#6d4c41", "#c0ca33"];

export async function renderLobby(root, rules) {
  const meta = await api.meta().catch(() => ({}));
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("games"));

  // First run: offer setup rather than forcing it. Somebody who installed CITAR to look at it
  // should be able to look at it, and the wizard stays one click away until it is dealt with.
  setupState().then((s) => {
    if (s.needed) page.insertBefore(welcomeBanner(() => { location.hash = "#/welcome"; }), page.children[1]);
  });

  const gamesCard = el("div", { class: "card" });
  const savesCard = el("div", { class: "card" });
  const newCard = el("div", { class: "card" });
  page.append(gamesCard, savesCard, newCard);

  async function refresh() {
    let games = [], saves = [];
    try { [games, saves] = await Promise.all([api.games(), api.saves()]); } catch (e) { toast(e.message, "error"); }
    // redraw only on change, so a periodic refresh doesn't reset a dropdown the user is using
    const gk = JSON.stringify(games), sk = JSON.stringify(saves);
    if (gk !== gamesCard._key) { gamesCard._key = gk; renderGames(gamesCard, games, refresh, meta); }
    if (sk !== savesCard._key) { savesCard._key = sk; renderSaves(savesCard, saves, refresh); }
  }
  renderNewGame(newCard, rules, meta, refresh);
  await refresh();
  const timer = setInterval(refresh, 5000);
  return { destroy() { clearInterval(timer); } };
}

function seatLabel(s) {
  if (s.type === "llm") return `LLM: ${(s.llm_info && s.llm_info.label) || s.llm.model || s.llm.provider || "?"}${s.llm_info && s.llm_info.server ? " @ " + s.llm_info.server : ""}`;
  if (s.type === "bot") return "Bot";
  if (s.type === "mcp") return `MCP${s.connected ? " (connected)" : ""}`;
  return "Human";
}

function renderGames(card, games, refresh, meta) {
  clear(card);
  card.appendChild(el("h2", {}, "Games in progress"));
  if (!games.length) { card.appendChild(el("p", { class: "muted" }, "No games running. Create one below or load a save.")); return; }
  const table = el("table", { class: "list" },
    el("tr", {}, el("th", {}, "Game"), el("th", {}, "Turn"), el("th", {}, "Players"), el("th", {}, "")));
  for (const g of games) {
    const players = el("div", { class: "col" });
    g.players.filter((p) => p.kind === "major").forEach((p) => {
      const seat = g.seats[p.id];
      players.appendChild(el("div", {}, el("span", { class: "swatch", style: { background: p.color } }), p.name,
        el("span", { class: "muted" }, ` — ${seat ? seatLabel(seat) : ""}${p.alive ? "" : " (eliminated)"}`),
        g.agent_errors && g.agent_errors[String(p.id)] ? el("div", { class: "bad", style: { fontSize: "12px" } }, `AI error: ${g.agent_errors[String(p.id)]}`) : null,
        g.current_player === p.id && g.phase === "playing" ? el("span", { class: "pill", style: { marginLeft: "6px" } }, "to move") : null));
    });
    const actions = el("div", { class: "btn-grid" });
    const openInfo = async () => {
      const info = await api.game(g.id);
      showConnectInfo(info, meta);
    };
    if (g.phase === "playing" && g.seats.some((s) => s.type === "human")) {
      actions.appendChild(el("button", { class: "primary", title: "Play the first human seat", onclick: async () => playHuman(await api.game(g.id)) }, "▶ Play"));
    }
    actions.appendChild(el("button", { class: g.seats.some((s) => s.type === "human") ? "" : "primary", onclick: openInfo }, "Join / Seats"));
    if (g.seats.some((s) => s.type !== "human")) actions.appendChild(el("button", { onclick: () => openMetrics(g.id) }, "📊 Stats"));
    if (g.god_view_allowed) actions.appendChild(el("button", { onclick: async () => {
      const info = await api.game(g.id);
      location.hash = `#/game/${g.id}/${encodeURIComponent(info.spectator_token)}`;
    } }, "Watch"));
    if (g.phase !== "playing" || g.god_view_allowed) {
      actions.appendChild(el("button", { onclick: async () => {
        const info = await api.game(g.id);
        location.hash = `#/replay/${g.id}/${encodeURIComponent(info.spectator_token)}`;
      } }, "Recap"));
    }
    actions.appendChild(el("button", { class: "danger", onclick: async () => {
      if (await confirmBox("End game", `Stop "${g.name}"? (Saves on disk are kept.)`)) { await api.deleteGame(g.id); refresh(); }
    } }, "Close"));
    const status = g.phase === "playing" ? (g.paused ? "paused" : "playing") :
      `over — ${g.players[g.winner] ? g.players[g.winner].name : "no one"} (${g.victory || ""})`;
    table.appendChild(el("tr", {},
      el("td", {}, el("div", {}, el("b", {}, g.name)), el("div", { class: "muted" }, `${g.config.map_type}, ${g.config.map_size}, ${status}`),
        g.benchmark ? el("a", { class: "pill live", href: "#/benchmarks", title: `Run: ${g.benchmark.run_name}` }, `Benchmark · ${g.benchmark.run_name}`) : null),
      el("td", {}, g.config.turn_limit ? `${Math.min(g.turn, g.config.turn_limit)} / ${g.config.turn_limit}` : `${g.turn}`),
      el("td", {}, players), el("td", {}, actions)));
  }
  card.appendChild(table);
}

// Jump straight into a game as its first human seat. Games with only humans and bots resume automatically;
// ones with LLM seats stay paused so loading a save never starts GPU work by surprise.
async function playHuman(info) {
  const human = info.seats.find((s) => s.type === "human");
  if (!human) return;
  if (info.paused && !info.seats.some((s) => s.type === "llm")) {
    try { await api.control(info.id, { paused: false }); } catch (e) { /* the game screen shows the paused state */ }
  }
  location.hash = `#/game/${info.id}/${encodeURIComponent(human.token)}`;
}

export function showConnectInfo(info, meta) {
  const body = el("div", { class: "col" });
  const serverUrl = (meta && meta.server_url) || location.origin;
  body.appendChild(el("p", { class: "muted" }, "Each seat has its own secret token. Humans play in the browser; MCP seats are driven by an external AI client; LLM and Bot seats are run by the server."));
  for (const s of info.seats) {
    const p = info.players[s.player];
    const row = el("div", { class: "card" });
    row.appendChild(el("div", { class: "row" }, el("span", { class: "swatch", style: { background: p.color } }), el("b", {}, p.name),
      el("span", { class: "pill" }, seatLabel(s)), el("span", { class: "grow" }),
      el("button", { class: "primary", onclick: () => { location.hash = `#/game/${info.id}/${encodeURIComponent(s.token)}`; closeFn(); } },
        s.type === "human" ? "Play this seat" : "Open this seat's view")));
    const typeSel = el("select", {}, ...["human", "bot", "llm", "mcp"].map((t) => el("option", { value: t, selected: s.type === t }, t)));
    const llmCfg = s.llm && s.llm.server_id ? JSON.parse(JSON.stringify(s.llm)) : defaultLLM();
    const llmBox = el("div", { style: { display: typeSel.value === "llm" ? "block" : "none" } });
    const drawLLM = () => { clear(llmBox).appendChild(llmForm(llmCfg, drawLLM)); };
    drawLLM();
    typeSel.onchange = () => { llmBox.style.display = typeSel.value === "llm" ? "block" : "none"; };
    const agentErr = info.agent_errors && info.agent_errors[String(s.player)];
    row.appendChild(el("div", { class: "row", style: { marginTop: "6px" } }, el("span", { class: "muted" }, "Controller:"), typeSel,
      el("button", { class: "small primary", onclick: async () => {
        await api.updateSeat(info.id, s.player, { type: typeSel.value, llm: typeSel.value === "llm" ? llmCfg : undefined });
        toast("Seat updated — takes effect on its next turn");
      } }, "Apply")));
    if (agentErr) row.appendChild(el("div", { class: "bad", style: { marginTop: "4px" } }, `Last AI error: ${agentErr}`));
    row.appendChild(llmBox);
    {
      const script = (meta && meta.mcp_script) || "citar_mcp.py";
      const py = (meta && meta.python) || "python";
      const cmd = `claude mcp add citar-${info.id}-p${s.player} -- "${py}" "${script}" --server ${serverUrl} --game ${info.id} --token ${s.token}`;
      row.appendChild(el("details", { style: { marginTop: "6px" } }, el("summary", { class: "muted" }, "MCP / API connection details"),
        el("div", { class: "muted" }, "Claude Code:"), el("pre", {}, cmd),
        el("div", { class: "muted" }, "Generic MCP client config (stdio):"),
        el("pre", {}, JSON.stringify({ mcpServers: { [`citar-p${s.player}`]: { command: py, args: [script, "--server", serverUrl, "--game", info.id, "--token", s.token] } } }, null, 2)),
        el("div", { class: "muted" }, "Raw HTTP: POST /api/games/" + info.id + "/tool with Authorization: Bearer <token> and {\"tool\": ..., \"args\": {...}}"),
        el("div", {}, "Token: ", el("code", {}, s.token))));
    }
    body.appendChild(row);
  }
  body.appendChild(el("div", {}, "Spectator token: ", el("code", {}, info.spectator_token)));
  const m = modal({ title: `Seats — ${info.name}`, content: body });
  const closeFn = m.close;
}

function renderSaves(card, saves, refresh) {
  clear(card);
  card.appendChild(el("h2", {}, "Saved games"));
  if (!saves.length) { card.appendChild(el("p", { class: "muted" }, "No saves yet. Games autosave every turn.")); return; }
  // one row per game (newest save first); older saves of the same game are in a dropdown
  const games = new Map();
  for (const s of saves) {
    if (!games.has(s.game_id)) games.set(s.game_id, []);
    games.get(s.game_id).push(s);
  }
  const showBench = card._showBench || false;
  const benchCount = [...games.values()].filter((list) => list[0].benchmark).length;
  const table = el("table", { class: "list" }, el("tr", {}, ...["Game", "Turn", "Players", "Saved", ""].map((h) => el("th", {}, h))));
  let shown = 0, hidden = 0;
  const limit = card._all ? 500 : 6;
  for (const [gid, list] of games) {
    const s = list[0];
    if (s.benchmark && !showBench) continue;
    if (shown >= limit) { hidden++; continue; }
    shown++;
    const pick = el("select", { title: "Which save to load" }, ...list.map((x) =>
      el("option", { value: x.path }, `${x.name}${x.turn != null ? " (T" + x.turn + ")" : ""}`)));
    const status = s.phase && s.phase !== "playing" ? el("span", { class: "pill" }, `finished${s.winner ? ": " + s.winner + " won" : ""}`) : null;
    table.appendChild(el("tr", {},
      el("td", {}, el("b", {}, s.game_name || gid), " ", s.benchmark ? el("span", { class: "pill live" }, "benchmark") : null, " ", status,
        el("div", { class: "muted", style: { fontSize: "11px" } }, gid)),
      el("td", {}, s.turn == null ? "?" : s.turn_limit ? `${Math.min(s.turn, s.turn_limit)}/${s.turn_limit}` : `${s.turn}`),
      el("td", { style: { fontSize: "12px" } }, (s.players || []).map((p) =>
        el("div", { class: p.alive === false ? "muted" : "" }, `${p.name}${p.seat ? " · " + p.seat : ""}${p.alive === false ? " (eliminated)" : ""}`))),
      el("td", {}, new Date(s.modified * 1000).toLocaleString()),
      el("td", {}, el("div", { class: "row" }, list.length > 1 ? pick : null,
        el("button", { class: "primary", onclick: async () => {
          try {
            const info = await api.loadSave(list.length > 1 ? pick.value : s.path);
            if (info.phase === "playing" && info.seats.some((x) => x.type === "human")) { await playHuman(info); return; }
            toast(info.phase === "playing" ? `Loaded ${info.name} (paused). Resume or join it from Games in progress.`
              : `Loaded ${info.name}. The game is over: open its Recap from Games in progress.`);
            refresh();
          } catch (e) { toast(e.message, "error"); }
        } }, "Load"),
        el("button", { class: "danger", title: "Delete every save of this game", onclick: async () => {
          if (!(await confirmBox("Delete saves", `Permanently delete all ${list.length} save(s) of "${s.game_name || gid}"?`))) return;
          try { await api.deleteSave(s.path, true); toast("Saves deleted."); refresh(); } catch (e) { toast(e.message, "error"); }
        } }, "Delete")))));
  }
  card.appendChild(table);
  if (hidden || card._all) {
    card.appendChild(el("button", { class: "small", style: { marginTop: "8px" }, onclick: () => { card._all = !card._all; renderSaves(card, saves, refresh); } },
      card._all ? "Show fewer" : `Show all (${hidden} more)`));
  }
  if (benchCount) {
    card.appendChild(el("label", { class: "muted", style: { display: "block", marginTop: "8px" } },
      el("input", { type: "checkbox", checked: showBench, onchange: (e) => { card._showBench = e.target.checked; renderSaves(card, saves, refresh); } }),
      ` Show benchmark games (${benchCount})`));
  }
}

// An LLM seat is a reference into the server registry (Servers page): which server, which of its models, which load
// profile, plus a few per-seat overrides. The server resolves it into an endpoint, key and settings when the AI starts.
export function defaultLLM() {
  return { server_id: null, model_id: null, model: "", profile_id: null, max_tool_calls_per_turn: 150, persona: "" };
}

function pickDefault(reg) {
  // a server with models, preferring the machine running CITAR, then other PCs, then APIs with a key, then the dry run
  const rank = (s) => (s.is_host ? 0 : s.kind === "owned" || s.kind === "leased" ? 1 : s.kind === "api" ? (s.key_status && s.key_status.present ? 2 : 4) : 3);
  return [...reg.servers].filter((s) => s.models.some((m) => m.enabled)).sort((a, b) => rank(a) - rank(b))[0];
}

// Editable LLM seat configuration: server -> model -> load profile, plus overrides. `rerender` redraws the parent.
export function llmForm(L, rerender, { seat = true } = {}) {
  const field = (label, input, extra = {}) => el("div", { class: "field", ...extra }, el("label", {}, label), input);
  const box = el("div", { class: "seat-extra" }, el("span", { class: "muted small" }, "Loading servers…"));
  registry().then((reg) => {
    clear(box);
    if (!reg.servers.length) { box.appendChild(el("span", { class: "warn" }, "No servers yet: add one on the Servers page.")); return; }
    let sv = reg.servers.find((s) => s.id === L.server_id);
    if (!sv) {
      sv = pickDefault(reg);
      if (!sv) { box.appendChild(el("span", { class: "warn" }, "No server has models yet: add some on the Servers page.")); return; }
      L.server_id = sv.id; L.model_id = null; L.profile_id = null;
    }
    const models = sv.models.filter((m) => m.enabled || m.id === L.model_id);
    let m = models.find((x) => x.id === L.model_id) || models.find((x) => x.key === L.model) || models[0];
    if (m) { L.model_id = m.id; L.model = m.key; } else { L.model_id = null; L.model = ""; }
    const profiles = (m && m.profiles) || [];
    if (!profiles.some((p) => p.id === L.profile_id)) L.profile_id = m ? m.default_profile : null;
    L.server = sv.name; L.provider = sv.connection.provider;
    const serverSel = el("select", { onchange: (e) => { L.server_id = e.target.value; L.model_id = null; L.profile_id = null; rerender(); } },
      ...reg.servers.filter((s) => s.models.length).map((s) => el("option", { value: s.id, selected: s.id === sv.id },
        `${s.name}${s.restricted_until ? " 🌙" : ""}${s.kind === "api" && !(s.key_status || {}).present ? " (no key)" : ""}`)));
    const modelSel = el("select", { onchange: (e) => { L.model_id = e.target.value; L.profile_id = null; rerender(); } },
      ...models.map((x) => el("option", { value: x.id, selected: m && x.id === m.id }, (x.label || x.key) + (x.info && x.info.params ? ` (${x.info.params})` : ""))));
    const profSel = profiles.length ? el("select", { onchange: (e) => { L.profile_id = e.target.value; } },
      ...profiles.map((p) => el("option", { value: p.id, selected: p.id === L.profile_id }, `${p.name} — ${p.context ? (p.context / 1024).toFixed(0) + "k ctx" : "default ctx"}, GPU ${p.gpu}`))) : null;
    const notes = [];
    if (sv.restricted_until) notes.push(el("div", { class: "warn small" }, `🌙 ${sv.name} is in its restricted hours until ${sv.restricted_until}.`));
    if (sv.kind === "api" && !(sv.key_status || {}).present) notes.push(el("div", { class: "bad small" }, `${sv.name} has no API key yet (Servers page).`));
    if (!m) notes.push(el("div", { class: "warn small" }, `${sv.name} has no models in its catalog yet (Servers page).`));
    const provider = sv.connection.provider;
    box.append(...[
      field("Server", serverSel),
      field("Model", modelSel, { style: { gridColumn: "span 2", minWidth: "0" } }),
      profSel ? field("Load profile", profSel) : null,
      !seat ? null : field("Reconnect wait (s)", el("input", { type: "number", min: 0, max: 86400, value: L.reconnect_seconds ?? "",
        placeholder: "game default", title: "How long this seat keeps retrying its model server before the game's disconnect rule applies",
        oninput: (e) => { L.reconnect_seconds = e.target.value === "" ? null : +e.target.value; } })),
      !seat ? null : field("Max tool calls / turn", el("input", { type: "number", value: L.max_tool_calls_per_turn || (m && m.inference.max_tool_calls_per_turn) || 150,
        oninput: (e) => { L.max_tool_calls_per_turn = +e.target.value; } })),
      provider === "anthropic" ? field("Effort", el("select", { onchange: (e) => { L.effort = e.target.value; } },
        ...["", "low", "medium", "high", "xhigh", "max"].map((v) => el("option", { value: v, selected: (L.effort || "") === v }, v || `server default (${(m && m.inference.effort) || "high"})`)))) : null,
      ["lmstudio", "ollama", "openai_compatible"].includes(provider) ? field("Reasoning effort", el("select", { onchange: (e) => { L.reasoning_effort = e.target.value; } },
        ...[["", `server default (${(m && m.inference.reasoning_effort) || "model default"})`], ["low", "low"], ["medium", "medium"], ["high", "high"],
          ["on", "on"], ["off", "off"]].map(([v, t]) =>
          el("option", { value: v, selected: (L.reasoning_effort || "") === v }, t)))) : null,
      ...notes.map((n) => el("div", { style: { gridColumn: "1 / -1" } }, n)),
      !seat ? null : el("div", { class: "field", style: { gridColumn: "1 / -1" } }, el("label", {}, "Persona / playstyle (optional)"),
        el("textarea", { rows: 2, value: L.persona || "", placeholder: "e.g. A cunning seafaring merchant republic that prefers trade to war.", oninput: (e) => { L.persona = e.target.value; } }))].filter(Boolean));
  }).catch((e) => { clear(box).appendChild(el("span", { class: "bad" }, e.message)); });
  return box;
}

function renderNewGame(card, rules, meta, refresh) {
  card.appendChild(el("h2", {}, "New game"));
  const f = {};
  const field = (label, input) => el("div", { class: "field" }, el("label", {}, label), input);
  const sel = (opts, value) => el("select", {}, ...opts.map(([v, t]) => el("option", { value: v, selected: v === value }, t)));
  f.name = el("input", { value: "New World" });
  f.size = sel(Object.entries(rules.map_sizes).map(([k, v]) => [k, `${v.name} (${v.width}×${v.height}, ${v.players}p)`]), "small");
  f.type = sel(Object.entries(rules.map_types).map(([k, v]) => [k, v.name]), "continents");
  f.speed = sel(Object.keys(rules.speeds).map((k) => [k, `${k} (${rules.max_turns[k]} turns)`]), rules.default_speed);
  f.difficulty = sel(rules.difficulty_list.map((k) => [k, k]), rules.default_difficulty);
  f.difficulty.title = "Default difficulty for every seat. Humans and AI models get this level's player values; bots get its AI bonuses (at Deity they build and grow far cheaper and start with extra units and techs).";
  f.barbDiff = sel([["", "Same as game"], ...rules.difficulty_list.map((k) => [k, k])], "");
  f.barbDiff.title = "Barbarian strength: the bonus civilizations get against barbarians, how soon camps spawn, and when barbarians may enter civilizations' land.";
  f.barbs = sel(Object.entries(rules.barbarian_levels).map(([k, v]) => [k, v]), "normal");
  f.turns = el("input", { type: "number", value: 0, min: 0, max: 2000, title: "0 = the speed's normal game length" });
  f.cs = el("input", { type: "number", placeholder: "map default", min: 0, max: 40 });
  f.seed = el("input", { placeholder: "random" });
  f.victories = Object.fromEntries(Object.keys(rules.victories).map((k) => [k, el("input", { type: "checkbox", checked: true })]));
  f.tech = el("input", { type: "checkbox", checked: true });
  f.ruins = el("input", { type: "checkbox", checked: true });
  f.religion = el("input", { type: "checkbox", checked: true });
  f.espionage = el("input", { type: "checkbox", checked: true });
  f.onDisconnect = sel([["pause", "Pause the game until it is back"], ["skip", "Skip that AI's turn"]], "pause");
  f.onDisconnect.title = "What happens when an AI model's server (LM Studio, a worker, an API) stays unreachable for the whole reconnect wait. "
    + "A paused game resumes by itself as soon as the server answers again.";
  f.reconnect = el("input", { type: "number", min: 0, max: 86400, value: 180,
    title: "How long an AI keeps retrying its model server before the disconnect rule applies. Short drops inside this window cost nothing." });
  // custom maps from the map editor
  f.map = sel([["", "Generate a new map"]], "");
  f.map.title = "Play on a map made in the map editor, or generate one from the size and type below.";
  const wantMap = new URLSearchParams((location.hash.split("?")[1] || "")).get("map") || "";
  let customMaps = {};
  const gen = mapOptionsForm(rules);
  const syncMap = () => {
    const mp = customMaps[f.map.value];
    f.size.disabled = f.type.disabled = !!mp;
    gen.node.style.display = mp ? "none" : "";
    if (mp && mp.starts && seats.length !== mp.starts) setSeatCount(mp.starts);
    renderSeats();
  };
  f.map.addEventListener("change", syncMap);
  api.maps().then((list) => {
    for (const mp of list) {
      customMaps[mp.id] = mp;
      f.map.appendChild(el("option", { value: mp.id, selected: mp.id === wantMap },
        `${mp.name} (${mp.width}×${mp.height}, ${mp.starts} starts${mp.cs_starts ? `, ${mp.cs_starts} city-states` : ""})`));
    }
    if (wantMap && customMaps[wantMap]) syncMap();
  }).catch(() => {});
  card.appendChild(el("div", { class: "grid2" },
    field("Game name", f.name), field("Map", f.map), field("Map size", f.size), field("Map type", f.type), field("Speed", f.speed),
    field("Difficulty", f.difficulty), field("Barbarians", f.barbs), field("Barbarian difficulty", f.barbDiff),
    field("Turn limit (0 = speed default)", f.turns),
    field("City-states", f.cs), field("Seed", f.seed),
    field("If an AI's model server disconnects", f.onDisconnect), field("Keep reconnecting for (seconds)", f.reconnect),
    el("div", { class: "field" }, el("label", {}, "Victory conditions"),
      el("div", { class: "row" }, ...Object.entries(f.victories).map(([k, cb]) => el("label", {}, cb, ` ${k}`)))),
    el("div", { class: "field" }, el("label", {}, "Options"),
      el("div", { class: "row" }, el("label", {}, f.tech, " Tech trading"), el("label", {}, f.ruins, " Ancient ruins"),
        el("label", {}, f.religion, " Religion"), el("label", {}, f.espionage, " Espionage")))));
  card.appendChild(gen.node);

  card.appendChild(el("h3", { style: { marginTop: "14px" } }, "Seats"));
  const seatsBox = el("div");
  card.appendChild(seatsBox);
  const seats = [
    { type: "human", civ_name: "", nation: "random", color: COLORS[0] },
    { type: "bot", civ_name: "", nation: "random", color: COLORS[1] },
    { type: "bot", civ_name: "", nation: "random", color: COLORS[2] },
    { type: "bot", civ_name: "", nation: "random", color: COLORS[3] },
  ];
  const nationOptions = [["random", "Random civilization"], ["BenchmarkCiv", "BenchmarkCiv (no special abilities)"],
    ...rules.major_nations.filter((n) => n !== "BenchmarkCiv").sort().map((n) => [n, `${n} (${(rules.nations[n] || {}).leaderName || ""})`])];

  const maxSeats = rules.max_players || 8;
  const sizePlayers = () => (customMaps[f.map.value] || {}).starts || (rules.map_sizes[f.size.value] || {}).players || 4;
  function setSeatCount(want) {
    while (seats.length > want && seats.length > 1) seats.pop();
    while (seats.length < want) {
      const i = seats.length;
      seats.push({ type: "bot", civ_name: "", nation: "random", color: COLORS[i % COLORS.length], bot: { aggression: 0.4 } });
    }
  }
  f.size.addEventListener("change", () => renderSeats());

  function renderSeats() {
    clear(seatsBox);
    seats.forEach((s, i) => {
      const type = sel([["human", "Human"], ["bot", "Scripted bot"], ["llm", "LLM (server-run)"], ["mcp", "MCP client"]], s.type);
      type.onchange = () => {
        s.type = type.value;
        if (s.type === "llm" && !s.llm) s.llm = defaultLLM();
        if (s.type === "bot" && !s.bot) s.bot = { aggression: 0.4 };
        renderSeats();
      };
      const nation = sel(nationOptions, s.nation || "random");
      nation.onchange = () => { s.nation = nation.value; };
      nation.title = "The civilization whose special abilities this seat plays with";
      const name = el("input", { value: s.civ_name || "", placeholder: "Custom name (optional)", oninput: (e) => { s.civ_name = e.target.value; } });
      const color = el("input", { type: "color", value: s.color, oninput: (e) => { s.color = e.target.value; } });
      const remove = el("button", { class: "small", disabled: seats.length <= 1, onclick: () => { seats.splice(i, 1); renderSeats(); } }, "✕");
      const diff = sel([["", "Game difficulty"], ...rules.difficulty_list.map((k) => [k, k])], s.difficulty || "");
      diff.onchange = () => { s.difficulty = diff.value; };
      diff.title = "This seat's difficulty. Bots get the level's AI bonuses; humans and AI models get its player values.";
      const row = el("div", { class: "seat-row" }, el("b", {}, `P${i}`), type, nation, diff, name, color, remove);
      if (s.type === "llm") {
        s.llm = s.llm || defaultLLM();
        row.appendChild(llmForm(s.llm, renderSeats));
      } else if (s.type === "bot") {
        s.bot = s.bot || { aggression: 0.4 };
        row.appendChild(el("div", { class: "seat-extra" }, field("Aggression", el("input", { type: "range", min: 0, max: 1, step: 0.1, value: s.bot.aggression, oninput: (e) => { s.bot.aggression = +e.target.value; } }))));
      } else if (s.type === "mcp") {
        row.appendChild(el("div", { class: "seat-extra muted" }, "After creating the game, open Join / Seats for the MCP command to connect Claude Code or another MCP client."));
      }
      seatsBox.appendChild(row);
    });
    seatsBox.appendChild(el("div", { class: "row", style: { marginTop: "8px" } },
      el("button", { disabled: seats.length >= maxSeats, onclick: () => {
        const i = seats.length;
        seats.push({ type: "bot", civ_name: "", nation: "random", color: COLORS[i % COLORS.length], bot: { aggression: 0.4 } });
        renderSeats();
      } }, "+ Add seat"),
      seats.length !== sizePlayers() ? el("button", { title: "Add or remove bot seats to match the map's usual player count", onclick: () => {
        setSeatCount(sizePlayers());
        renderSeats();
      } }, `Match map (${sizePlayers()} seats)`) : null,
      el("span", { class: "muted" }, "Tip: all-AI games can be watched live with full vision.")));
  }
  renderSeats();

  const create = el("button", { class: "primary", style: { marginTop: "12px" }, onclick: async () => {
    create.disabled = true;
    try {
      const body = {
        name: f.name.value,
        config: {
          map: f.map.value || null,
          map_size: f.size.value, map_type: f.type.value, speed: f.speed.value, difficulty: f.difficulty.value,
          barbarian_difficulty: f.barbDiff.value || null,
          barbarians: f.barbs.value, turn_limit: +f.turns.value || null, seed: f.seed.value ? +f.seed.value : null,
          city_states: f.cs.value === "" ? null : +f.cs.value,
          victories: Object.fromEntries(Object.entries(f.victories).map(([k, cb]) => [k, cb.checked])),
          tech_trading: f.tech.checked, ruins: f.ruins.checked, religion: f.religion.checked, espionage: f.espionage.checked,
          on_disconnect: f.onDisconnect.value, reconnect_seconds: f.reconnect.value === "" ? null : +f.reconnect.value,
          ...(f.map.value ? {} : gen.value()),
        },
        seats: seats.map((s) => ({ type: s.type, civ_name: s.civ_name || null, nation: s.nation === "random" ? null : s.nation,
                                   difficulty: s.difficulty || null, color: s.color, llm: s.llm, bot: s.bot })),
      };
      const info = await api.createGame(body);
      toast(`Created ${info.name}`);
      const human = info.seats.find((s) => s.type === "human");
      if (human) location.hash = `#/game/${info.id}/${encodeURIComponent(human.token)}`;
      else if (info.seats.some((s) => s.type === "mcp")) { showConnectInfo(info, meta); refresh(); }
      else location.hash = `#/game/${info.id}/${encodeURIComponent(info.spectator_token)}`;
    } catch (e) {
      toast(e.message, "error");
    } finally { create.disabled = false; }
  } }, "Create game");
  card.appendChild(create);
}

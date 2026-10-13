// Lobby: running games, saves, and the new-game form.
import { api, gameLink, replayLink } from "./api.js";
import { profileSelect, profileFor } from "./bots.js";
import { mapOptionsForm } from "./mapoptions.js";
import { el, clear, toast, modal, confirmBox } from "./util.js";
import { openMetrics } from "./metrics.js";
import { setupState, welcomeBanner } from "./setup.js";
import { pageHeader } from "./nav.js";
import { registry } from "./servers.js";
import { shareButton } from "./share.js";
import { CATEGORIES, HANDICAPS, PRESETS, PROMPT_EVERY, hybridProblem, hybridState, isModelSeat, newHybrid, ownersMap,
  presetOf, seatLabel, seatPayload, seatUpdate } from "./seats.js";

// Civilization colours (the server sends the same palette as rules.player_colors), most distinct first.
const COLORS = ["#aa0000", "#0000aa", "#00aa00", "#aaaa00", "#50006e", "#c85a00", "#ff00e6", "#299bcc",
  "#00998a", "#cc298b", "#b2ff00", "#9900ff", "#00ffff", "#99741f", "#004c99", "#ff0000",
  "#00ff33", "#ff0066", "#0066ff", "#99003d", "#9b29cc", "#00ff80", "#0099ff", "#ffff00"];

const hexRgb = (c) => (/^#[0-9a-f]{6}$/i.test(c || "") ? [1, 3, 5].map((k) => parseInt(c.slice(k, k + 2), 16)) : null);

// Too close to tell apart on the map (the same "redmean" test the server uses to keep colours unique)
export function colorsClash(a, b) {
  const x = hexRgb(a), y = hexRgb(b);
  if (!x || !y) return false;
  const rm = (x[0] + y[0]) / 2, dr = x[0] - y[0], dg = x[1] - y[1], db = x[2] - y[2];
  return Math.sqrt((2 + rm / 256) * dr * dr + 4 * dg * dg + (2 + (255 - rm) / 256) * db * db) < 60;
}

// The first palette colour none of `taken` clashes with
function freeColor(palette, taken) {
  return palette.find((c) => !taken.some((t) => colorsClash(c, t))) || palette[taken.length % palette.length];
}

// A swatch button that opens a palette. Colours another seat holds are shown taken and can't be picked;
// a custom hex value can be typed at the bottom, as long as nobody has it (or one too close to it).
function colorPicker(palette, s, others, onPick) {
  const takenBy = (c) => others.find((o) => colorsClash(c, o.color));
  const btn = el("button", { class: "color-btn", type: "button", title: `Colour ${s.color}`, style: { background: s.color } });
  const wrap = el("span", { class: "color-wrap" }, btn);
  let pop = null;
  const closePop = () => { if (pop) { pop.remove(); pop = null; document.removeEventListener("mousedown", outside); } };
  const outside = (e) => { if (pop && !pop.contains(e.target) && e.target !== btn) closePop(); };
  btn.onclick = () => {
    if (pop) return closePop();
    const grid = el("div", { class: "color-grid" }, ...palette.map((c) => {
      const t = takenBy(c);
      return el("button", { type: "button", class: `color-sw ${t ? "taken" : ""} ${c === s.color ? "current" : ""}`, disabled: !!t,
        title: t ? `${c}: taken by ${t.label}` : c, style: { background: c },
        onclick: () => { closePop(); onPick(c); } });
    }));
    const hex = el("input", { class: "color-hex", value: s.color, maxlength: 7, spellcheck: false, placeholder: "#rrggbb" });
    const msg = el("span", { class: "muted small" });
    const apply = () => {
      let v = hex.value.trim().toLowerCase();
      if (!v.startsWith("#")) v = "#" + v;
      if (!hexRgb(v)) { msg.textContent = "Use the form #rrggbb."; return; }
      const t = takenBy(v);
      if (t) { msg.textContent = `Too close to ${t.label}'s colour.`; return; }
      closePop(); onPick(v);
    };
    hex.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); apply(); } });
    pop = el("div", { class: "color-pop" }, grid,
      el("div", { class: "row color-custom" }, el("span", { class: "muted small" }, "Custom"), hex,
        el("button", { type: "button", class: "small", onclick: apply }, "Use")), msg);
    wrap.appendChild(pop);
    setTimeout(() => document.addEventListener("mousedown", outside), 0);
  };
  return wrap;
}

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

/** Who may play a seat, as the lobby, the Join/Seats dialog and the scenario seats offer it (P3.6.8). */
export const SEAT_KINDS = [["human", "Human"], ["bot", "Scripted bot"], ["llm", "LLM (server-run)"],
  ["hybrid", "Hybrid (bot plays, model negotiates)"], ["mcp", "MCP client"]];

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
    // seats, tokens and closing are the game's managers'; a game shared with this viewer is theirs to watch
    const sharing = g.sharing || {};
    const manages = !!sharing.can_manage;
    const openInfo = async () => {
      const info = await api.game(g.id);
      showConnectInfo(info, meta);
    };
    if (manages && g.phase === "playing" && g.seats.some((s) => s.type === "human")) {
      actions.appendChild(el("button", { class: "primary", title: "Play the first human seat", onclick: async () => playHuman(await api.game(g.id)) }, "▶ Play"));
    }
    if (manages) actions.appendChild(el("button", { class: g.seats.some((s) => s.type === "human") ? "" : "primary", onclick: openInfo }, "Join / Seats"));
    if (g.seats.some((s) => s.type !== "human")) actions.appendChild(el("button", { onclick: () => openMetrics(g.id) }, "📊 Stats"));
    // a manager watches with the spectator token; anyone else by their account (the server checks it)
    const token = async () => (manages ? (await api.game(g.id)).spectator_token : "");
    if (g.god_view_allowed) actions.appendChild(el("button", { onclick: async () => { location.hash = gameLink(g.id, await token()); } }, "Watch"));
    if (g.phase !== "playing" || g.god_view_allowed) {
      actions.appendChild(el("button", { onclick: async () => { location.hash = replayLink(g.id, await token()); } }, "Recap"));
    }
    const share = shareButton("game", g.id, g.name, sharing, refresh);
    if (share) actions.appendChild(share);
    if (manages) {
      actions.appendChild(el("button", { class: "danger", onclick: async () => {
        if (await confirmBox("End game", `Stop "${g.name}"? (Saves on disk are kept.)`)) { await api.deleteGame(g.id); refresh(); }
      } }, "Close"));
    }
    const status = g.crashed ? `stopped by an engine error on turn ${g.crashed.turn}` : g.phase === "playing" ? (g.paused ? "paused" : "playing") :
      `over — ${g.players[g.winner] ? g.players[g.winner].name : "no one"} (${g.victory || ""})`;
    table.appendChild(el("tr", {},
      el("td", {}, el("div", {}, el("b", {}, g.name), !manages ? el("span", { class: "pill", style: { marginLeft: "6px" } }, "shared with you") : null,
        !manages && sharing.owner ? el("a", { class: "muted small", style: { marginLeft: "6px" }, href: `#/u/${encodeURIComponent(sharing.owner.handle)}` },
          `by ${sharing.owner.display_name !== sharing.owner.handle ? sharing.owner.display_name + " " : ""}@${sharing.owner.handle}`) : null,
        sharing.visibility && sharing.visibility !== "private" && manages ? el("span", { class: "pill", style: { marginLeft: "6px" }, title: "Who can see it (Share)" }, VISIBILITY_LABEL[sharing.visibility] || sharing.visibility) : null),
        el("div", { class: "muted" }, `${g.config.map_type}, ${g.config.map_size}, ${status}`),
        g.benchmark ? el("a", { class: "pill live", href: "#/benchmarks", title: `Run: ${g.benchmark.run_name}` }, `Benchmark · ${g.benchmark.run_name}`) : null),
      el("td", {}, g.config.turn_limit ? `${Math.min(g.turn, g.config.turn_limit)} / ${g.config.turn_limit}` : `${g.turn}`),
      el("td", {}, players), el("td", {}, actions)));
  }
  card.appendChild(table);
}

// Jump straight into a game as its first human seat. Games with only humans and bots resume automatically;
// ones with a seat a model plays (an LLM or a hybrid seat) stay paused so loading a save never starts GPU work by
// surprise.
async function playHuman(info) {
  const human = info.seats.find((s) => s.type === "human");
  if (!human) return;
  if (info.paused && !info.seats.some(isModelSeat)) {
    try { await api.control(info.id, { paused: false }); } catch (e) { /* the game screen shows the paused state */ }
  }
  location.hash = gameLink(info.id, human.token);
}

// How a game's visibility reads in the list.
const VISIBILITY_LABEL = { allowlist: "shared with people", link: "shared by link", server: "members can see",
  global: "public" };

export function showConnectInfo(info, meta) {
  const body = el("div", { class: "col" });
  const serverUrl = (meta && meta.server_url) || location.origin;
  body.appendChild(el("p", { class: "muted" }, "Each seat has its own secret token. Humans play in the browser; MCP seats are driven by an external AI client; LLM, hybrid and bot seats are run by the server."));
  const copy = (x) => JSON.parse(JSON.stringify(x));
  for (const s of info.seats) {
    const p = info.players[s.player];
    const row = el("div", { class: "card", "data-seat": s.player });
    const label = el("span", { class: "pill seat-label" }, seatLabel(s));
    row.appendChild(el("div", { class: "row" }, el("span", { class: "swatch", style: { background: p.color } }), el("b", {}, p.name),
      label, el("span", { class: "grow" }),
      el("button", { class: "primary", onclick: () => { location.hash = `#/game/${info.id}/${encodeURIComponent(s.token)}`; closeFn(); } },
        s.type === "human" ? "Play this seat" : "Open this seat's view")));
    // The seat as the dialog changes it: a model block naming a machine (the seat's, or a new one), its bot, a hybrid
    // seat's settings and its handicap (the engine's now for a hybrid seat; the hybrid seat's default for one becoming
    // hybrid). Apply sends what the chosen type plays (seatUpdate), and the server admits a model seat again.
    const st = { type: s.type, llm: s.llm && s.llm.server_id ? copy(s.llm) : defaultLLM(),
                 bot: copy(s.bot && Object.keys(s.bot).length ? s.bot : { aggression: 0.4 }),
                 hybrid: hybridState(s.type === "hybrid" ? s.hybrid : null),
                 handicap: s.type === "hybrid" && p.handicap === "human" ? "human" : "ai" };
    const typeSel = el("select", {}, ...SEAT_KINDS.map(([t, text]) => el("option", { value: t, selected: s.type === t }, text)));
    const extra = el("div", { class: "seat-dialog-extra" });
    const drawExtra = () => {
      clear(extra);
      if (st.type === "llm") extra.appendChild(llmForm(st.llm, drawExtra));
      else if (st.type === "hybrid") extra.appendChild(hybridForm(st, drawExtra, { level: p.difficulty || "" }));
    };
    drawExtra();
    typeSel.onchange = () => { st.type = typeSel.value; drawExtra(); };
    const agentErr = info.agent_errors && info.agent_errors[String(s.player)];
    row.appendChild(el("div", { class: "row", style: { marginTop: "6px" } }, el("span", { class: "muted" }, "Controller:"), typeSel,
      el("button", { class: "small primary", onclick: async () => {
        const problem = st.type === "hybrid" ? hybridProblem(st.hybrid) : null;
        if (problem) { toast(problem, "error"); return; }
        try {
          const out = await api.updateSeat(info.id, s.player, seatUpdate(st));
          const now = (out.seats || []).find((x) => x.player === s.player);
          if (now) label.textContent = seatLabel(now);
          toast("Seat updated — takes effect on its next turn");
        } catch (e) { toast(e.message, "error"); }
      } }, "Apply")));
    if (agentErr) row.appendChild(el("div", { class: "bad", style: { marginTop: "4px" } }, `Last AI error: ${agentErr}`));
    row.appendChild(extra);
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
    const status = s.phase && s.phase !== "playing" ? el("span", { class: "pill good" }, `✓ finished${s.winner ? ": " + s.winner + " won" : ""}`) : null;
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
  // a server with models, preferring a connected machine with a model loaded, then the machine running CITAR, then
  // other PCs, then APIs with a key, then the dry run
  const rank = (s) => (s.pooled ? (s.online && s.models.some((m) => m.loaded) ? -1 : s.online ? 0.5 : 5)
    : s.is_host ? 0 : s.kind === "owned" || s.kind === "leased" ? 1 : s.kind === "api" ? (s.key_status && s.key_status.present ? 2 : 4) : 3);
  return [...reg.servers].filter((s) => s.models.some((m) => m.enabled)).sort((a, b) => rank(a) - rank(b))[0];
}

// The machines the caller may use but does not manage, from the pool's list, in the registry's shape so the seat form
// can offer them beside their own: a helper machine (reach "worker") with the models its helper reports now, loaded
// ones first; a machine the server dials that its owner shared (the site's dry run, say) with its catalogue's keys.
function pooledEntries(pool) {
  return (pool.servers || []).filter((s) => s.enabled !== false).map((s) => {
    const helper = s.reach === "worker";
    const source = (helper && s.live_models && s.live_models.length) ? s.live_models : (s.models || []);
    const models = source.filter((m) => m.key && !/embed/i.test(m.key))
      .map((m) => ({ id: m.key, key: m.key, label: (m.label || m.key) + (m.loaded ? " (loaded)" : ""), enabled: true,
                     loaded: !!m.loaded, profiles: [], inference: {}, info: {} }))
      .sort((a, b) => b.loaded - a.loaded);
    return { id: s.id, name: s.name, kind: helper ? "pool" : s.kind, pooled: helper, online: s.online,
             admission: s.admission, restricted_until: s.restricted_until,
             connection: { provider: helper ? "worker" : s.provider, max_parallel: s.max_concurrent || 1 },
             restricted_hours: { enabled: false }, models };
  });
}

// Every machine a seat may name: the caller's own (every one for an administrator) as the registry lists them, a
// helper machine as its helper reports it, and the machines shared with the caller (P3.2.8: every machine is a row),
// each with whether the caller is admitted there for `purpose`: `game` for a seat that plays every turn, `pooled` for
// a hybrid seat's model, whose prompts are pooled prompts (P3.3.7).
export async function seatServers(force = false, purpose = "game") {
  const [reg, pool] = await Promise.all([
    registry(force).catch(() => ({ servers: [] })),
    api.poolServers(purpose).catch(() => ({ servers: [] }))]);
  const own = (reg.servers || []).filter((s) => s.reach !== "worker");
  const ids = new Set(own.map((s) => s.id));
  return { ...reg, servers: [...own, ...pooledEntries(pool).filter((s) => !ids.has(s.id))] };
}

// Editable LLM seat configuration: server -> model -> load profile, plus overrides. `rerender` redraws the parent.
// `purpose` is what the caller must be admitted for on the machine (`pooled` for a hybrid seat's model); without
// `turns` (a hybrid seat's model, which plays no turns) the per-turn settings are left out.
export function llmForm(L, rerender, { seat = true, purpose = "game", turns = true } = {}) {
  const field = (label, input, extra = {}) => el("div", { class: "field", ...extra }, el("label", {}, label), input);
  const box = el("div", { class: "seat-extra" }, el("span", { class: "muted small" }, "Loading servers…"));
  seatServers(false, purpose).then((reg) => {
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
        `${s.name}${s.restricted_until ? " 🌙" : ""}${s.kind === "api" && !(s.key_status || {}).present ? " (no key)" : ""}`
        + (s.pooled ? (s.online ? " — helper connected" : " — offline") : ""))));
    const modelSel = el("select", { onchange: (e) => { L.model_id = e.target.value; L.profile_id = null; rerender(); } },
      ...models.map((x) => el("option", { value: x.id, selected: m && x.id === m.id }, (x.label || x.key) + (x.info && x.info.params ? ` (${x.info.params})` : ""))));
    const profSel = profiles.length ? el("select", { onchange: (e) => { L.profile_id = e.target.value; } },
      ...profiles.map((p) => el("option", { value: p.id, selected: p.id === L.profile_id }, `${p.name} — ${p.context ? (p.context / 1024).toFixed(0) + "k ctx" : "default ctx"}, GPU ${p.gpu}`))) : null;
    const notes = [];
    if (sv.restricted_until) notes.push(el("div", { class: "warn small" }, `🌙 ${sv.name} is in its restricted hours until ${sv.restricted_until}.`));
    if (sv.kind === "api" && !(sv.key_status || {}).present) notes.push(el("div", { class: "bad small" }, `${sv.name} has no API key yet (Servers page).`));
    if (!m) notes.push(el("div", { class: "warn small" }, `${sv.name} has no models in its catalog yet (Servers page).`));
    if (sv.pooled && !sv.online) notes.push(el("div", { class: "warn small" }, `${sv.name}'s helper is not connected: start it on that machine.`));
    // a machine shared with the caller says why it would refuse this seat's kind of work (a grant for games is not one
    // for a hybrid seat's pooled prompts, and the other way round)
    else if (sv.admission && !sv.admission.allowed) notes.push(el("div", { class: "warn small admission" }, sv.admission.reason));
    if (sv.pooled && m && !m.loaded && sv.models.some((x) => x.loaded)) notes.push(el("div", { class: "muted small" }, `${m.key} is not the loaded model on ${sv.name}; LM Studio will load it on the first turn if it can.`));
    const provider = sv.connection.provider;
    box.append(...[
      field("Server", serverSel),
      field("Model", modelSel, { style: { gridColumn: "span 2", minWidth: "0" } }),
      profSel ? field("Load profile", profSel) : null,
      !seat || !turns ? null : field("Reconnect wait (s)", el("input", { type: "number", min: 0, max: 86400, value: L.reconnect_seconds ?? "",
        placeholder: "game default", title: "How long this seat keeps retrying its model server before the game's disconnect rule applies",
        oninput: (e) => { L.reconnect_seconds = e.target.value === "" ? null : +e.target.value; } })),
      !seat || !turns ? null : field("Max tool calls / turn", el("input", { type: "number", value: L.max_tool_calls_per_turn || (m && m.inference.max_tool_calls_per_turn) || 150,
        oninput: (e) => { L.max_tool_calls_per_turn = +e.target.value; } })),
      provider === "anthropic" ? field("Effort", el("select", { onchange: (e) => { L.effort = e.target.value; } },
        ...["", "low", "medium", "high", "xhigh", "max"].map((v) => el("option", { value: v, selected: (L.effort || "") === v }, v || `server default (${(m && m.inference.effort) || "high"})`)))) : null,
      ["lmstudio", "ollama", "openai_compatible", "worker"].includes(provider) ? field("Reasoning effort", el("select", { onchange: (e) => { L.reasoning_effort = e.target.value; } },
        ...[["", `server default (${(m && m.inference.reasoning_effort) || "model default"})`], ["low", "low"], ["medium", "medium"], ["high", "high"],
          ["on", "on"], ["off", "off"]].map(([v, t]) =>
          el("option", { value: v, selected: (L.reasoning_effort || "") === v }, t)))) : null,
      ...notes.map((n) => el("div", { style: { gridColumn: "1 / -1" } }, n)),
      !seat ? null : el("div", { class: "field", style: { gridColumn: "1 / -1" } }, el("label", {}, "Persona / playstyle (optional)"),
        el("textarea", { rows: 2, value: L.persona || "", placeholder: "e.g. A cunning seafaring merchant republic that prefers trade to war.", oninput: (e) => { L.persona = e.target.value; } }))].filter(Boolean));
  }).catch((e) => { clear(box).appendChild(el("span", { class: "bad" }, e.message)); });
  return box;
}

const fieldOf = (label, input, extra = {}) => el("div", { class: "field", ...extra }, el("label", {}, label), input);

// The bot a bot seat or a hybrid seat plays: its profile ("Best bot" stands for the best-ranked one, pinned when the seat
// is made) and its aggression, unless the profile fixes it.
function botFields(bot) {
  const aggBox = el("div");
  const drawAgg = (prof) => {
    clear(aggBox);
    aggBox.appendChild(prof && prof.aggression != null
      ? fieldOf("Aggression", el("span", { class: "muted small" }, `${prof.aggression} (fixed by the profile)`))
      : fieldOf("Aggression", el("input", { type: "range", min: 0, max: 1, step: 0.1, value: bot.aggression, oninput: (e) => { bot.aggression = +e.target.value; } })));
  };
  if (bot.profile == null) bot.profile = "best";
  profileFor(bot.profile).then(drawAgg);
  return [fieldOf("Bot", profileSelect(bot, "profile", drawAgg, { best: true, fallback: "best" })), aggBox];
}

/** A hybrid seat's handicap, whose numbers it plays with at its difficulty level (P3.6.2): "AI bonuses, like the bot"
 *  (the default) or "a person's values, like a model". Bound to `s.handicap`. */
function handicapSelect(s) {
  if (!s.handicap) s.handicap = "ai";
  return el("select", { class: "handicap",
    title: "Whose numbers this seat plays with at its difficulty level: the AI's bonuses, as a bot seat does, or a person's values, as an LLM seat does.",
    onchange: (e) => { s.handicap = e.target.value; } },
  ...HANDICAPS.map(([v, text]) => el("option", { value: v, selected: s.handicap === v }, text)));
}

/** Who decides each of the ten kinds of a hybrid seat's diplomacy, its bot or its model, with the three presets
 *  (P3.6.8); it redraws itself, and says so when the model would decide nothing. Bound to `H.owners`. */
function ownersGrid(H) {
  const box = el("div", { class: "hybrid-owners" });
  const draw = () => {
    clear(box);
    const preset = presetOf(H.owners);
    const label = (k) => (CATEGORIES.find(([c]) => c === k) || [k, k])[1];
    const presets = el("div", { class: "row" }, el("span", { class: "muted small" }, "The model decides:"),
      ...PRESETS.map(([name, text, cats]) => el("button", { type: "button", class: `small preset ${preset === name ? "active" : ""}`,
        "data-preset": name, title: cats.map(label).join(", "), onclick: () => { H.owners = ownersMap(name); draw(); } }, text)),
      el("span", { class: "muted small" }, preset ? "" : "a choice of its own"));
    const grid = el("div", { class: "hybrid-grid" }, ...CATEGORIES.map(([k, text, help]) => {
      const mine = H.owners[k] === "llm";
      const pick = (owner) => () => { H.owners = { ...H.owners, [k]: owner }; draw(); };
      return el("div", { class: "hybrid-cat", title: help, "data-cat": k },
        el("span", {}, text),
        el("span", { class: "owner-toggle" },
          el("button", { type: "button", class: `small ${mine ? "" : "active"}`, "aria-pressed": String(!mine), onclick: pick("bot") }, "Bot"),
          el("button", { type: "button", class: `small ${mine ? "active" : ""}`, "aria-pressed": String(mine), onclick: pick("llm") }, "Model")));
    }));
    const problem = hybridProblem(H);
    box.append(presets, grid, problem ? el("div", { class: "warn small" }, problem) : null);
  };
  draw();
  return box;
}

/** A hybrid seat's form (P3.6.8): the bot that plays its turns and the seat's handicap at its difficulty level (`level`,
 *  when the form knows it), the model that negotiates (its machine, where the caller needs a grant for pooled
 *  prompts), who decides each kind of diplomacy, the advisor and how often the diplomat is called when nothing else
 *  calls it. Without `model` (a scenario's default seat) the model is picked when the game starts. Works on `s` in
 *  place; `rerender` redraws the parent (the model's form asks it when its machine changes). */
export function hybridForm(s, rerender, { model = true, level = "" } = {}) {
  s.hybrid = hybridState(s.hybrid);
  s.bot = s.bot && Object.keys(s.bot).length ? s.bot : { aggression: 0.4 };
  if (!s.handicap) s.handicap = "ai";
  const H = s.hybrid;
  const every = el("input", { type: "number", min: PROMPT_EVERY.min, max: PROMPT_EVERY.max, step: 1, value: H.prompt_every,
    class: "prompt-every", style: { width: "64px" },
    oninput: (e) => { H.prompt_every = e.target.value === "" ? PROMPT_EVERY.default : +e.target.value; } });
  const box = el("div", { class: "seat-extra hybrid-form" },
    el("div", { class: "hybrid-bot" }, ...botFields(s.bot),
      fieldOf(`Handicap at ${level || "its difficulty level"}`, handicapSelect(s))));
  if (model) {
    if (!s.llm || !Object.keys(s.llm).length) s.llm = defaultLLM();
    box.appendChild(el("div", { class: "hybrid-model" }, el("div", { class: "muted small" }, "Its model (for the diplomacy below):"),
      llmForm(s.llm, rerender, { purpose: "pooled", turns: false })));
  } else {
    box.appendChild(el("div", { class: "muted small" }, "Its model is picked when a game starts from the scenario (Play…)."));
  }
  box.append(ownersGrid(H),
    el("div", { class: "row" },
      el("label", { title: "With the advisor on, each call of the diplomat shows it the bot's view: what each deal is worth in gold, its war readiness, its spare luxuries and what it would ask for." },
        el("input", { type: "checkbox", class: "advisor", checked: H.advisor, onchange: (e) => { H.advisor = e.target.checked; } }), " Advisor (the bot's view)"),
      el("label", { title: "The diplomat is called when something happens (first contact, a war declared on the seat, a deal running out, a vote, a large change in strength), when another civilization talks to it, and at least this often." },
        "Call the diplomat at least every ", every, " turns")));
  return box;
}

/** The lobby's chat settings as the game's configuration keys (P3.5.2): minutes to seconds, an empty field left to the
 *  server's default. */
function chatConfig(modelMinutes, personMinutes, cap) {
  const seconds = (v) => (v === "" || !(+v > 0) ? undefined : Math.round(+v * 60));
  const out = {};
  if (seconds(modelMinutes) !== undefined) out.chat_model_reply_seconds = seconds(modelMinutes);
  if (seconds(personMinutes) !== undefined) out.chat_person_reply_seconds = seconds(personMinutes);
  if (cap !== "" && +cap >= 2) out.diplomacy = { max_chat_messages: Math.round(+cap) };
  return out;
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
  const barbAggrDefault = () => (rules.barbarian_aggression || {})[f.barbs.value] ?? 50;
  f.barbAggr = el("input", { type: "range", min: 0, max: 100, step: 5, value: barbAggrDefault(),
    title: "How hard barbarians hunt: search radius for targets, odds they accept, raids vs attacks, and how fast camps spawn. Resets to the level's default when the level changes." });
  f.barbAggrVal = el("span", { class: "muted" }, String(f.barbAggr.value));
  f.barbAggr.addEventListener("input", () => { f.barbAggrVal.textContent = f.barbAggr.value; });
  f.barbAggrField = el("div", { class: "field" }, el("label", {}, "Barbarian aggression"), el("div", { class: "row" }, f.barbAggr, f.barbAggrVal));
  const syncBarbAggr = () => {
    const off = f.barbs.value === "off";
    f.barbAggr.disabled = off;
    f.barbAggrField.style.display = off ? "none" : "";
    f.barbAggr.value = barbAggrDefault();
    f.barbAggrVal.textContent = f.barbAggr.value;
  };
  f.barbs.addEventListener("change", syncBarbAggr);
  syncBarbAggr();
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
  // Chat (design/phase3-features.md P3.5.2): how long a chat waits on each side, in minutes, and its message cap.
  // The server holds the game to them; the defaults are the server's own (10 and 5 minutes). The cap is sent only
  // when it is filled in: left empty, the game keeps its ruleset's own (30 in the shipped one).
  f.chatModel = el("input", { type: "number", min: 0.5, max: 1440, step: 0.5, value: 10,
    title: "How long a chat waits for an AI model's answer (an LLM, MCP or hybrid seat), its time waiting for its machine "
      + "included. Then the model is taken to have declined (a hybrid seat's bot answers instead)." });
  f.chatPerson = el("input", { type: "number", min: 0.5, max: 1440, step: 0.5, value: 5,
    title: "How long a chat waits for a person's answer, whoever opened it. Then it closes as expired. "
      + "The clock stands still while the game is paused." });
  f.chatCap = el("input", { type: "number", min: 2, max: 200, value: "", placeholder: "rules",
    title: "The most messages one chat may hold; the message that would pass it closes the chat instead. "
      + "Leave it empty for the ruleset's own cap (30 in the standard rules)." });
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
    field("Difficulty", f.difficulty), field("Barbarians", f.barbs), f.barbAggrField, field("Barbarian difficulty", f.barbDiff),
    field("Turn limit (0 = speed default)", f.turns),
    field("City-states", f.cs), field("Seed", f.seed),
    field("If an AI's model server disconnects", f.onDisconnect), field("Keep reconnecting for (seconds)", f.reconnect),
    el("div", { class: "field" }, el("label", {}, "Chat"),
      el("div", { class: "row" },
        el("label", { title: f.chatModel.title }, "AI answers within ", f.chatModel, " min"),
        el("label", { title: f.chatPerson.title }, "people within ", f.chatPerson, " min"),
        el("label", { title: f.chatCap.title }, "at most ", f.chatCap, " messages"))),
    el("div", { class: "field" }, el("label", {}, "Victory conditions"),
      el("div", { class: "row" }, ...Object.entries(f.victories).map(([k, cb]) => el("label", {}, cb, ` ${k}`)))),
    el("div", { class: "field" }, el("label", {}, "Options"),
      el("div", { class: "row" }, el("label", {}, f.tech, " Tech trading"), el("label", {}, f.ruins, " Ancient ruins"),
        el("label", {}, f.religion, " Religion"), el("label", {}, f.espionage, " Espionage")))));
  card.appendChild(gen.node);

  card.appendChild(el("h3", { style: { marginTop: "14px" } }, "Seats"));
  const seatsBox = el("div");
  card.appendChild(seatsBox);
  const palette = rules.player_colors && rules.player_colors.length ? rules.player_colors : COLORS;
  const seats = [
    { type: "human", civ_name: "", nation: "random", color: palette[0] },
    { type: "bot", civ_name: "", nation: "random", color: palette[1] },
    { type: "bot", civ_name: "", nation: "random", color: palette[2] },
    { type: "bot", civ_name: "", nation: "random", color: palette[3] },
  ];
  const nextColor = () => freeColor(palette, seats.map((x) => x.color));
  const nationOptions = [["random", "Random civilization"], ["BenchmarkCiv", "BenchmarkCiv (no special abilities)"],
    ...rules.major_nations.filter((n) => n !== "BenchmarkCiv").sort().map((n) => [n, `${n} (${(rules.nations[n] || {}).leaderName || ""})`])];

  const maxSeats = rules.max_players || 8;
  const sizePlayers = () => (customMaps[f.map.value] || {}).starts || (rules.map_sizes[f.size.value] || {}).players || 4;
  function setSeatCount(want) {
    while (seats.length > want && seats.length > 1) seats.pop();
    while (seats.length < want) {
      const i = seats.length;
      seats.push({ type: "bot", civ_name: "", nation: "random", color: nextColor(), bot: { aggression: 0.4 } });
    }
  }
  f.size.addEventListener("change", () => renderSeats());

  function renderSeats() {
    clear(seatsBox);
    seats.forEach((s, i) => {
      const type = sel(SEAT_KINDS, s.type);
      type.onchange = () => {
        s.type = type.value;
        if (s.type === "llm" && !s.llm) s.llm = defaultLLM();
        if ((s.type === "bot" || s.type === "hybrid") && !s.bot) s.bot = { aggression: 0.4 };
        if (s.type === "hybrid" && !s.hybrid) s.hybrid = newHybrid();
        renderSeats();
      };
      const nation = sel(nationOptions, s.nation || "random");
      nation.onchange = () => { s.nation = nation.value; };
      nation.title = "The civilization whose special abilities this seat plays with";
      const name = el("input", { value: s.civ_name || "", placeholder: "Custom name (optional)", oninput: (e) => { s.civ_name = e.target.value; } });
      const others = seats.map((o, j) => ({ color: o.color, label: `P${j}` })).filter((_, j) => j !== i);
      const color = colorPicker(palette, s, others, (c) => { s.color = c; renderSeats(); });
      const remove = el("button", { class: "small", disabled: seats.length <= 1, onclick: () => { seats.splice(i, 1); renderSeats(); } }, "✕");
      const diff = sel([["", "Game difficulty"], ...rules.difficulty_list.map((k) => [k, k])], s.difficulty || "");
      diff.onchange = () => { s.difficulty = diff.value; };
      diff.title = s.type === "hybrid"
        ? "This seat's difficulty level. Its handicap (below, beside its bot) says whose numbers it plays with at that level."
        : "This seat's difficulty. Bots get the level's AI bonuses; humans and AI models get its player values.";
      const row = el("div", { class: "seat-row", "data-seat": i }, el("b", {}, `P${i}`), type, nation, diff, name, color, remove);
      if (s.type === "llm") {
        s.llm = s.llm || defaultLLM();
        row.appendChild(llmForm(s.llm, renderSeats));
      } else if (s.type === "bot") {
        s.bot = s.bot || { aggression: 0.4 };
        row.appendChild(el("div", { class: "seat-extra" }, ...botFields(s.bot)));
      } else if (s.type === "hybrid") {
        row.appendChild(hybridForm(s, renderSeats, { level: s.difficulty || "the game's difficulty" }));
      } else if (s.type === "mcp") {
        row.appendChild(el("div", { class: "seat-extra muted" }, "After creating the game, open Join / Seats for the MCP command to connect Claude Code or another MCP client."));
      }
      seatsBox.appendChild(row);
    });
    seatsBox.appendChild(el("div", { class: "row", style: { marginTop: "8px" } },
      el("button", { disabled: seats.length >= maxSeats, onclick: () => {
        seats.push({ type: "bot", civ_name: "", nation: "random", color: nextColor(), bot: { aggression: 0.4 } });
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
    const wrong = seats.map((s, i) => [i, s.type === "hybrid" ? hybridProblem(s.hybrid) : null]).find(([, why]) => why);
    if (wrong) { toast(`P${wrong[0]}: ${wrong[1]}`, "error"); return; }
    create.disabled = true;
    try {
      const body = {
        name: f.name.value,
        config: {
          map: f.map.value || null,
          map_size: f.size.value, map_type: f.type.value, speed: f.speed.value, difficulty: f.difficulty.value,
          barbarian_difficulty: f.barbDiff.value || null,
          barbarians: f.barbs.value, barbarian_aggression: f.barbs.value === "off" ? null : +f.barbAggr.value,
          turn_limit: +f.turns.value || null, seed: f.seed.value ? +f.seed.value : null,
          city_states: f.cs.value === "" ? null : +f.cs.value,
          victories: Object.fromEntries(Object.entries(f.victories).map(([k, cb]) => [k, cb.checked])),
          tech_trading: f.tech.checked, ruins: f.ruins.checked, religion: f.religion.checked, espionage: f.espionage.checked,
          on_disconnect: f.onDisconnect.value, reconnect_seconds: f.reconnect.value === "" ? null : +f.reconnect.value,
          ...chatConfig(f.chatModel.value, f.chatPerson.value, f.chatCap.value),
          ...(f.map.value ? {} : gen.value()),
        },
        seats: seats.map(seatPayload),
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

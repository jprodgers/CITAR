// Seats as the pages name and send them (design/phase3-features.md P3.6.7, P3.6.8): what plays each seat, its label
// ("Hybrid: <bot> + <model>"), what an AI seat is doing now, and the hybrid seat's settings (the ten kinds of diplomacy,
// the presets, the handicap) with the payload the lobby, the Join/Seats dialog and the scenario seats send. Pure: the
// lobby, the game screen, the stats screen, the recap and the phone site use it, and citar/web/tests/seats.test.mjs
// checks it (against fixtures/hybrid-seats.json, the payloads tests/test_hybrid_lobby.py posts).

/** The ten kinds of diplomatic decision, in the engine's order (citar.agents.hybrid.CATEGORIES), each with what it
 *  covers. A hybrid seat's model decides those its owners give it; its bot decides the rest. */
export const CATEGORIES = [
  ["trades", "Trades", "Deals of gold, gold per turn, resources, techs, maps and cities"],
  ["agreements", "Agreements", "Embassies, open borders, friendship, research agreements and defensive pacts"],
  ["peace", "Peace", "Offering and accepting peace"],
  ["war", "War", "Declaring war (the bot fights every war, whoever declared it)"],
  ["denounce", "Denouncing", "Denouncing a civilization"],
  ["un", "United Nations", "Voting in the United Nations (a vote the model does not cast is an abstention)"],
  ["city_states", "City-states", "Gifts and actions toward city-states"],
  ["espionage", "Espionage", "Placing spies and staging coups"],
  ["captured_cities", "Captured cities", "Annexing, keeping as a puppet or razing a city the seat takes"],
  ["chat", "Talk", "Messages, and the chats that are only talk"],
];
export const CATEGORY_KEYS = CATEGORIES.map(([k]) => k);

/** The presets the lobby offers (P3.6.2): the categories each gives the model. */
export const PRESETS = [
  ["trade_only", "Trade only", ["trades", "chat"]],
  ["all_but_war", "All but war", CATEGORY_KEYS.filter((k) => k !== "war")],
  ["everything", "Everything", CATEGORY_KEYS],
];

/** A seat's handicap, whose numbers it plays with, as the lobby words it (P3.6.2). */
export const HANDICAPS = [["ai", "AI bonuses, like the bot"], ["human", "A person's values, like a model"]];

/** `prompt_every`'s bounds and default (turns between the diplomat's calls when nothing else calls it). */
export const PROMPT_EVERY = { min: 1, max: 50, default: 10 };

/** The whole owners map {category: "bot" | "llm"} of a preset's name or a map (any category it does not name is the
 *  bot's); an unknown preset gives the model nothing. */
export function ownersMap(owners) {
  if (typeof owners === "string") {
    const preset = PRESETS.find(([name]) => name === owners);
    const mine = new Set(preset ? preset[2] : []);
    return Object.fromEntries(CATEGORY_KEYS.map((k) => [k, mine.has(k) ? "llm" : "bot"]));
  }
  const given = owners && typeof owners === "object" ? owners : {};
  return Object.fromEntries(CATEGORY_KEYS.map((k) => [k, given[k] === "llm" ? "llm" : "bot"]));
}

/** The preset an owners map is, or null for a grid of its own. */
export function presetOf(owners) {
  const map = ownersMap(owners);
  const found = PRESETS.find(([name]) => CATEGORY_KEYS.every((k) => ownersMap(name)[k] === map[k]));
  return found ? found[0] : null;
}

/** The categories the model decides, in the engine's order. */
export function modelCategories(owners) {
  const map = ownersMap(owners);
  return CATEGORY_KEYS.filter((k) => map[k] === "llm");
}

/** A new hybrid seat's settings, as the lobby starts them: trade only, the advisor on, a call every 10 turns. */
export function newHybrid() {
  return { owners: ownersMap("trade_only"), advisor: true, prompt_every: PROMPT_EVERY.default };
}

/** A hybrid seat's settings as a form holds them (from a seat the server sent, a saved scenario's, or none): the whole
 *  owners map, the advisor and `prompt_every`, with `phase_seconds` kept when the seat had one. */
export function hybridState(h) {
  const base = newHybrid();
  if (!h || typeof h !== "object") return base;
  const out = { owners: h.owners === undefined ? base.owners : ownersMap(h.owners),
                advisor: h.advisor === undefined ? true : !!h.advisor,
                prompt_every: Number.isFinite(+h.prompt_every) && h.prompt_every !== "" && h.prompt_every != null
                  ? +h.prompt_every : base.prompt_every };
  if (h.phase_seconds != null) out.phase_seconds = h.phase_seconds;
  return out;
}

/** The `hybrid` block a seat sends (P3.6.2): the owners as their preset's name when they are one, else the whole map;
 *  the advisor; `prompt_every` as the form holds it (the server refuses one that is not a whole number of 1..50, with
 *  the reason, and `hybridProblem` says so first). */
export function hybridBlock(h) {
  const st = hybridState(h);
  const out = { owners: presetOf(st.owners) || st.owners, advisor: st.advisor, prompt_every: st.prompt_every };
  if (st.phase_seconds != null) out.phase_seconds = st.phase_seconds;
  return out;
}

/** What is wrong with a hybrid seat's settings before it is sent, or null: the server checks them too (P3.6.2). */
export function hybridProblem(h) {
  const st = hybridState(h);
  if (!modelCategories(st.owners).length) {
    return "Give the model at least one kind of diplomacy (a hybrid seat with nothing for its model is a bot seat).";
  }
  const n = st.prompt_every;
  if (!Number.isInteger(n) || n < PROMPT_EVERY.min || n > PROMPT_EVERY.max) {
    return `"Call the diplomat every" is a whole number of turns from ${PROMPT_EVERY.min} to ${PROMPT_EVERY.max}.`;
  }
  return null;
}

/** A seat of the lobby's new game as `POST /api/games` takes it: a hybrid seat with its `handicap` (`ai` unless the
 *  lobby chose `human`) and its `hybrid` block; every seat with its difficulty, colour, civilization and the bot and
 *  model blocks it holds (the server ignores those a seat does not play). */
export function seatPayload(s) {
  const out = { type: s.type, civ_name: s.civ_name || null, nation: !s.nation || s.nation === "random" ? null : s.nation,
                difficulty: s.difficulty || null, color: s.color, llm: s.llm, bot: s.bot };
  if (s.type === "hybrid") {
    out.handicap = s.handicap || "ai";
    out.hybrid = hybridBlock(s.hybrid);
  }
  return out;
}

/** A seat change as `POST /api/games/{gid}/seats/{pid}` takes it (the Join/Seats dialog): the new type, and for a
 *  model seat its model block, for a hybrid seat its bot, handicap and settings too. */
export function seatUpdate(s) {
  const out = { type: s.type };
  if (s.type === "llm" || s.type === "hybrid") out.llm = s.llm;
  if (s.type === "hybrid") {
    out.bot = s.bot;
    out.handicap = s.handicap || "ai";
    out.hybrid = hybridBlock(s.hybrid);
  }
  return out;
}

/** Whether a model plays the seat: an LLM seat, or a hybrid seat with a model (citar.server.session.Seat.model_seat). */
export function isModelSeat(seat) {
  if (!seat) return false;
  if (seat.type === "llm") return true;
  return seat.type === "hybrid" && !!seat.llm && Object.keys(seat.llm).length > 0;
}

/** A bot's name: its profile (Standard unless it names one), "(best)" when the seat asked for the best-ranked bot. */
export function botName(bot) {
  const b = bot || {};
  return `${b.profile || "standard"}${b.chosen_as === "best" ? " (best)" : ""}`;
}

/** The model a seat plays: its machine's label for it, its key, or its provider; "" for none. */
export function modelName(seat) {
  const L = (seat && seat.llm) || {};
  const info = (seat && seat.llm_info) || {};
  return info.label || L.model || L.model_id || L.provider || "";
}

/** How a seat is named everywhere a game lists its players (P3.6.7): "Human", "MCP (connected)", "Bot: standard",
 *  "LLM: <model> @ <machine>", and a hybrid seat as "Hybrid: <bot> + <model>" (the summary's `label`), its model
 *  "no model" while it has none. */
export function seatLabel(s) {
  if (!s) return "";
  if (s.type === "llm") {
    const server = s.llm_info && s.llm_info.server;
    return `LLM: ${modelName(s) || "?"}${server ? " @ " + server : ""}`;
  }
  if (s.type === "hybrid") {
    const empty = !s.llm || !Object.keys(s.llm).length;
    return `Hybrid: ${botName(s.bot)} + ${empty ? "no model" : modelName(s) || "its model"}`;
  }
  if (s.type === "bot") return `Bot: ${botName(s.bot)}`;
  if (s.type === "mcp") return `MCP${s.connected ? " (connected)" : ""}`;
  if (s.type === "script") return "Probe script";
  return "Human";
}

/** A row of a game's metrics summary (`GET /api/games/{gid}/metrics`), named as its seat: a hybrid row "Hybrid: <bot> +
 *  <model>", an LLM row its model, any other its controller. */
export function controllerLabel(row) {
  if (!row) return "";
  if (row.controller === "hybrid") return `Hybrid: ${row.bot || "standard"} + ${row.model || "no model"}`;
  if (row.controller === "llm") return row.model || "llm";
  return row.controller || "";
}

function ordinal(n) {
  const tens = n % 100;
  const suffix = tens >= 11 && tens <= 13 ? "th" : ({ 1: "st", 2: "nd", 3: "rd" })[n % 10] || "th";
  return `${n}${suffix}`;
}

/** A model seat's wait for its machine, as the scheduler placed its call (its `agent` message: `machine`, `position`,
 *  `eta_s`; P3.3.3): "waiting for Test GPU box: 2nd in line, about 40 s". Without a place (a page opened mid-wait),
 *  "waiting for its machine…". */
export function queuedText(w) {
  if (!w) return "waiting for its machine…";
  const where = w.machine ? `waiting for ${w.machine}` : "waiting for its machine";
  const parts = [];
  if (w.position) parts.push(`${ordinal(w.position)} in line`);
  if (w.eta_s != null) parts.push(`about ${Math.max(1, Math.round(w.eta_s))} s`);
  return parts.length ? `${where}: ${parts.join(", ")}` : `${where}…`;
}

/** What an AI seat is doing, from its `agent` status (and, while queued, its place), as the game screen's player list
 *  and turn box say it; null when there is nothing to say. A hybrid seat (P3.6.8): "bot playing" while its bot plays,
 *  "diplomat thinking" while its model decides, "queued #n" while the diplomat's prompt waits for its machine, and
 *  "diplomat paused" while a pause holds it. */
export function agentState(seat, status, wait) {
  if (!status || status === "idle") return null;
  if (seat && seat.type === "hybrid") {
    if (status === "thinking") return "bot playing";
    if (status === "diplomat") return "diplomat thinking";
    if (status === "queued") return wait && wait.position ? `queued #${wait.position}` : "queued";
    if (status === "paused") return "diplomat paused";
    if (status === "reconnecting") return "reconnecting to its model server…";
    return status;
  }
  if (status === "thinking") return "thinking…";
  if (status === "reconnecting") return "reconnecting to its model server…";
  if (status === "paused") return "paused mid-turn";
  if (status === "queued") return queuedText(wait);
  return status;
}

/** A hybrid seat's diplomat over the game, from its metrics summary row (`diplomat_phases`, `diplomat_outcomes`,
 *  `avg_diplomat_steps`, `avg_diplomat_gen_s`, `avg_diplomat_queue_s`, `diplomat_refused`): "8 calls (7 done, 1
 *  skipped), 3.5 steps each, model 2.1 s, waited 0.4 s; refused: 2 outside its kinds, 1 vetoed by the bot". Null for
 *  a seat whose diplomat was never called. */
export function diplomatText(row) {
  if (!row || !row.diplomat_phases) {
    return row && row.diplomat_refused ? `0 calls; refused: ${refusedText(row.diplomat_refused)}` : null;
  }
  const outcomes = Object.entries(row.diplomat_outcomes || {}).map(([k, v]) => `${v} ${k}`).join(", ");
  let text = `${row.diplomat_phases} call${row.diplomat_phases === 1 ? "" : "s"}${outcomes ? ` (${outcomes})` : ""}`;
  if (row.avg_diplomat_steps != null) {
    text += `, ${round1(row.avg_diplomat_steps)} steps each, model ${round1(row.avg_diplomat_gen_s)} s, waited ${round1(row.avg_diplomat_queue_s)} s`;
  }
  if (row.diplomat_refused) text += `; refused: ${refusedText(row.diplomat_refused)}`;
  return text;
}

function refusedText(refused) {
  const words = { category: "outside its kinds", veto: "vetoed by the bot" };
  return Object.entries(refused).map(([k, v]) => `${v} ${words[k] || k}`).join(", ");
}

function round1(v) {
  return v == null ? "–" : String(Math.round(v * 10) / 10);
}

/** How one diplomat call ended (a `diplomat` row's `outcome` and, when it did not run, its `cause` code). */
export function outcomeText(row) {
  if (!row || !row.outcome) return "…";
  return row.cause ? `${row.outcome} (${String(row.cause).replace(/_/g, " ")})` : row.outcome;
}

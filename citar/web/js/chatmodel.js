// The chat window's state (design/phase3-features.md P3.5.6): pure, with no DOM, no network and no clock of its own,
// so Node tests it (citar/web/tests/chatmodel.test.mjs). chat.js draws what it decides.
//
// A chat is a negotiation between two civilizations, one message each in turn. What the client learns of one comes
// from three places, in no fixed order: the view's `diplomacy` block (each refresh), the chat route
// (`GET /api/games/{gid}/negotiations/{nid}`, after a `negotiation` event, with where its clock stands) and the
// socket's `negotiation_status` messages (queued, thinking, waiting with a deadline, closed). A chat's history only
// grows, and every entry has a `seq` (1, 2, ...), so the number of entries is the chat's version: whatever says less
// than the client already knows is stale and only adds entries it may lack; a closed chat never opens again.
//
// What the person is writing (each window's draft: the text and the deal builder) lives in `drafts`, a map no
// update touches: the window redraws its log, status and buttons, never the textarea or the builder.
//
// Times: `now` is the caller's monotonic clock in milliseconds (performance.now()). A countdown starts from the
// server's `left_s` at the moment it arrived, so a client never trusts its own wall clock against the server's.

/** The quick buttons' lines when the box is empty: every answer carries a message (the engine refuses one without). */
export const DEFAULT_LINES = { accept: "Agreed.", reject: "No deal.", counter: "How about this instead?", open: "I propose this deal." };

/** How a closed chat's status reads in its window and on End Turn's side. */
export const CLOSED_TEXT = {
  accepted: "accepted: the deal is in effect",
  rejected: "declined",
  expired: "expired",
  cancelled: "cancelled",
};

/** The width at or under which one window shows at a time, full width above the End Turn box (P3.5.6). */
export const NARROW_WIDTH = 700;

/** "4:12", "0:07", "1:02:03": a countdown's seconds. */
export function fmtClock(seconds) {
  const s = Math.max(0, Math.ceil(Number(seconds) || 0));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  const pad = (n) => String(n).padStart(2, "0");
  return h ? `${h}:${pad(m)}:${pad(r)}` : `${m}:${pad(r)}`;
}

/** A chat's entries and `incoming` merged by `seq`: one entry per seq, in order. Anything without a whole positive
 * seq is not an entry. Returns a new array. */
export function mergeEntries(have, incoming) {
  const bySeq = new Map();
  for (const e of [...(have || []), ...(incoming || [])]) {
    if (e && Number.isInteger(e.seq) && e.seq > 0 && !bySeq.has(e.seq)) bySeq.set(e.seq, e);
  }
  return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
}

function maxSeq(entries) {
  let m = 0;
  for (const e of entries || []) if (e && Number.isInteger(e.seq) && e.seq > m) m = e.seq;
  return m;
}

/** Seconds left on a timing (`{leftS, at, paused}`) at `now`, or null when it is not timed. A paused clock stands. */
export function leftAt(timing, now) {
  if (!timing || timing.leftS == null || !Number.isFinite(timing.leftS)) return null;
  if (timing.paused) return timing.leftS;
  return Math.max(0, timing.leftS - Math.max(0, now - timing.at) / 1000);
}

function newChat(nid) {
  return {
    nid, with: null, withName: "", youInitiated: false, turn: null, status: "open",
    entries: [], seq: 0,          // what is known of its history, and its highest seq
    ver: 0,                       // the version (number of entries) its status, side and clock come from
    propVer: -1,                  // ... and its proposal
    waitingOn: null,              // the seat it waits on while it is open
    proposal: null, proposalByYou: null, messages: 0, maxMessages: null,
    clock: null,                  // {waitingOn, kind, leftS, at, paused, held, ver}
    live: null,                   // the answering side's request: {state: queued|thinking, position, etaS, machine, held, at, ver}
  };
}

function timingFrom(src, now, ver) {
  return {
    waitingOn: src.waiting_on ?? null, kind: src.kind ?? null,
    leftS: src.left_s == null ? null : Number(src.left_s), at: now, paused: !!src.paused,
    held: src.held || null, ver,
  };
}

/** A fresh draft: the window's text and its deal builder (what each side gives, whether it is open). */
export function newDraft() {
  return { text: "", give: [], receive: [], builderOpen: false };
}

/** The deal builder's edits, on a draft (never on a chat). Items are the engine's deal items ({type, ...}). */
export const builder = {
  add(draft, side, item) {
    if (side !== "give" && side !== "receive") return false;
    if (!item || typeof item.type !== "string" || !item.type) return false;
    draft[side].push(structuredCopy(item));
    return true;
  },
  remove(draft, side, index) {
    if ((side !== "give" && side !== "receive") || !(index >= 0 && index < draft[side].length)) return false;
    draft[side].splice(index, 1);
    return true;
  },
  clear(draft) { draft.give = []; draft.receive = []; },
  /** The proposal on the table, as the seat sees it ({you_give, you_receive}), copied into the builder. */
  load(draft, proposal) {
    if (!proposal) return false;
    draft.give = (proposal.you_give || []).map(structuredCopy);
    draft.receive = (proposal.you_receive || []).map(structuredCopy);
    return true;
  },
  empty(draft) { return !draft.give.length && !draft.receive.length; },
};

function structuredCopy(x) { return JSON.parse(JSON.stringify(x)); }

/** A deal item in words ("50 gold", "Iron × 30t"); `nameOf(pid)` names a civilization. */
export function describeItem(it, nameOf = (pid) => `#${pid}`) {
  switch (it && it.type) {
    case "gold": return `${it.amount} gold`;
    case "gold_per_turn": return `${it.amount} gold/turn × ${it.turns}`;
    case "resource": return `${it.amount} ${it.resource} × ${it.turns}t`;
    case "open_borders": return `Open borders ${it.turns}t`;
    case "declare_war": return `War on ${nameOf(it.target)}`;
    case "city": return it.name ? it.name : `City #${it.city_id}`;
    case "share_map": return "World map";
    case "tech": return it.tech;
    default: return String((it && it.type) || "?").replace(/_/g, " ");
  }
}

export class ChatModel {
  constructor(you = null) {
    this.you = you;
    this.chats = new Map();       // nid -> chat
    this.drafts = new Map();      // civ -> draft; nothing below writes it but draft() (which only creates)
    this.windows = [];            // the docked windows in the order they opened: {civ, minimised, unread}
    this.active = null;           // the window shown at narrow widths
    this.armed = null;            // End Turn armed: {turnKey, force}
  }

  setYou(pid) { this.you = pid; }

  /** The draft of the window with `civ` (made on first use). The only place `drafts` is written. */
  draft(civ) {
    let d = this.drafts.get(civ);
    if (!d) { d = newDraft(); this.drafts.set(civ, d); }
    return d;
  }

  // ------------------------------------------------------------------ updates (none touches the drafts)

  /** One negotiation as the seat sees it (the view's `open_negotiations`/`recent_negotiations`, or the chat route's
   * answer with its `clock`). Returns what changed: `{chat, created, added: [entries new to the client], moved
   * (it now waits on someone else, or closed), turnedToYou, closed}`, or null for something that is not one. */
  applyNegotiation(nv, now) {
    if (!nv || !Number.isInteger(nv.id)) return null;
    let chat = this.chats.get(nv.id);
    const created = !chat;
    if (!chat) { chat = newChat(nv.id); this.chats.set(nv.id, chat); }
    const was = { waitingOn: chat.waitingOn, status: chat.status };
    const known = new Set(chat.entries.map((e) => e.seq));
    const incoming = Array.isArray(nv.history) ? nv.history : [];
    chat.entries = mergeEntries(chat.entries, incoming);
    const added = chat.entries.filter((e) => !known.has(e.seq));
    chat.seq = maxSeq(chat.entries);
    const ver = maxSeq(incoming);
    if (nv.with != null) chat.with = nv.with;
    if (nv.with_name) chat.withName = nv.with_name;
    if (ver >= chat.ver) {
      chat.ver = ver;
      if (typeof nv.status === "string" && (chat.status === "open" || nv.status !== "open")) chat.status = nv.status;
      chat.youInitiated = !!nv.you_initiated;
      if (nv.turn != null) chat.turn = nv.turn;
      chat.waitingOn = chat.status !== "open" ? null
        : nv.your_move ? this.you : (nv.clock && nv.clock.waiting_on != null ? nv.clock.waiting_on : chat.with);
      if (chat.live && chat.live.ver < ver) chat.live = null;
      if (chat.clock && chat.clock.ver < ver) chat.clock = null;
      if (nv.clock && chat.status === "open") chat.clock = timingFrom(nv.clock, now, ver);
    }
    if (ver >= chat.propVer) {
      chat.propVer = ver;
      chat.proposal = nv.current_proposal || null;
      chat.proposalByYou = nv.proposal_by_you ?? null;
      if (Number.isFinite(nv.messages)) chat.messages = nv.messages;
      if (nv.max_messages != null) chat.maxMessages = nv.max_messages;
    }
    if (chat.status !== "open") { chat.live = null; chat.clock = null; chat.waitingOn = null; }
    return this._changes(chat, was, { created, added });
  }

  /** The view's `diplomacy` block: every open and recent negotiation in it. Returns the changes of each, and the
   * chats the client holds open that the view did not list (`missing`: ask the chat route how they stand; a view
   * fetched before a chat opened does not list it, so absence alone never closes one). */
  applyView(diplomacy, now) {
    const out = { changes: [], missing: [] };
    if (!diplomacy) return out;
    const listed = new Set();
    for (const nv of [...(diplomacy.recent_negotiations || []), ...(diplomacy.open_negotiations || [])]) {
      const c = this.applyNegotiation(nv, now);
      if (c) { out.changes.push(c); listed.add(nv.id); }
    }
    for (const chat of this.chats.values()) {
      if (chat.status === "open" && !listed.has(chat.nid)) out.missing.push(chat.nid);
    }
    return out;
  }

  /** A `negotiation_status` from the socket. Returns `{chat, fetch, ...changes}`: `fetch` when the client should read
   * the chat route (a chat it has not seen, one that moved further than its entries, one that closed). A status
   * older than what the client knows (it names fewer entries) is dropped. */
  applyStatus(msg, now) {
    if (!msg || !Number.isInteger(msg.negotiation)) return null;
    let chat = this.chats.get(msg.negotiation);
    const created = !chat;
    if (!chat) {
      chat = newChat(msg.negotiation);
      const others = (msg.players || []).filter((p) => p !== this.you);
      if (others.length) chat.with = others[0];
      this.chats.set(msg.negotiation, chat);
    }
    const was = { waitingOn: chat.waitingOn, status: chat.status };
    if (msg.state === "closed") {
      if (typeof msg.status === "string" && msg.status !== "open") chat.status = msg.status;
      else if (chat.status === "open") chat.status = "closed";
      chat.live = null; chat.clock = null; chat.waitingOn = null;
      return { ...this._changes(chat, was, { created, added: [] }), fetch: true };
    }
    if (chat.status !== "open") return { ...this._changes(chat, was, { created, added: [] }), fetch: false };
    const ver = Number.isInteger(msg.entries) ? msg.entries : chat.ver;
    if (ver < chat.ver) return { ...this._changes(chat, was, { created, added: [] }), fetch: false };
    chat.ver = ver;
    if (msg.waiting_on != null) chat.waitingOn = msg.waiting_on;
    chat.clock = timingFrom(msg, now, ver);
    chat.live = msg.state === "queued" || msg.state === "thinking"
      ? { state: msg.state, position: msg.position ?? null, etaS: msg.eta_s ?? null, machine: msg.machine || null,
          held: msg.held || null, at: now, ver }
      : null;
    return { ...this._changes(chat, was, { created, added: [] }), fetch: created || ver > chat.seq };
  }

  _changes(chat, was, { created, added }) {
    const open = chat.status === "open";
    return {
      chat, created, added,
      closed: was.status === "open" && !open,
      moved: was.waitingOn !== chat.waitingOn || was.status !== chat.status,
      turnedToYou: open && chat.waitingOn === this.you && this.you != null && was.waitingOn !== this.you,
    };
  }

  // ------------------------------------------------------------------ what the windows show

  /** Every chat with civilization `civ`, oldest first. */
  chatsWith(civ) {
    return [...this.chats.values()].filter((c) => c.with === civ).sort((a, b) => a.nid - b.nid);
  }

  /** The chat a window with `civ` is about: its open one (the engine allows one per pair), else its latest. */
  chatWith(civ) {
    const all = this.chatsWith(civ);
    return all.find((c) => c.status === "open") || all[all.length - 1] || null;
  }

  /** The open chats, oldest first. */
  openChats() {
    return [...this.chats.values()].filter((c) => c.status === "open").sort((a, b) => a.nid - b.nid);
  }

  /** What a window with `civ` shows and allows at `now`:
   * `{mode: new|your_move|their_move|closed, chat, input, can: {send, accept, reject, counter, withdraw, open},
   *   status: {text, leftS, tone}}`. Strict turn-taking: the text box and the answers work only on your move; on
   * theirs you may only withdraw (the way out of waiting, since End Turn waits for the chat). A chat is opened on
   * your own turn (`myTurn`), as the engine requires. `name` names the civilization. */
  windowState(civ, now, { myTurn = false, name = "", draft = null } = {}) {
    const chat = this.chatWith(civ);
    const who = name || (chat && chat.withName) || "They";
    const d = draft || this.drafts.get(civ) || newDraft();
    const hasDeal = !builder.empty(d);
    const none = { send: false, accept: false, reject: false, counter: false, withdraw: false, open: false };
    if (!chat || chat.status !== "open") {
      const mode = chat ? "closed" : "new";
      const lead = chat ? `Chat #${chat.nid} ${CLOSED_TEXT[chat.status] || chat.status}.` : "";
      const text = myTurn ? `${lead}${lead ? " " : ""}Write to start a chat, or build a deal below.`
        : `${lead}${lead ? " " : ""}You can start a chat on your turn.`;
      return { mode, chat, input: myTurn, can: { ...none, open: myTurn }, status: { text: text.trim(), leftS: null, tone: "muted" } };
    }
    const timing = chat.clock;
    const left = leftAt(timing, now);
    if (chat.waitingOn === this.you) {
      const tone = left != null && left < 60 ? "warn" : "you";
      const text = timing && timing.paused ? "Waiting for you (the game is paused)"
        : left != null ? `Waiting for you, ${fmtClock(left)} left` : "Your move";
      const can = { ...none, send: true, reject: true, counter: hasDeal,
                    accept: !!chat.proposal && chat.proposalByYou === false };
      return { mode: "your_move", chat, input: true, can, status: { text, leftS: left, tone } };
    }
    const live = chat.live && chat.live.ver >= chat.ver ? chat.live : null;
    let text;
    if (live && live.state === "thinking") text = `${who} is thinking…`;
    else if (live && live.state === "queued") {
      if (live.held === "benchmark") text = `${who} is queued behind a benchmark`;
      else {
        const parts = [live.position ? `queued #${live.position}` : "queued"];
        if (live.etaS != null) parts.push(`about ${Math.max(1, Math.round(live.etaS))} s`);
        text = `${who}: ${parts.join(", ")}`;
      }
    } else if (timing && timing.paused) text = `Waiting for ${who} (the game is paused)`;
    else if (left != null) text = `Waiting for ${who}, ${fmtClock(left)} left`;
    else text = `Waiting for ${who}…`;
    if (live && left != null && !(timing && timing.paused)) text += ` · ${fmtClock(left)} left`;
    return { mode: "their_move", chat, input: false, can: { ...none, withdraw: true }, status: { text, leftS: left, tone: "them" } };
  }

  // ------------------------------------------------------------------ End Turn

  /** Arm End Turn for turn `turnKey`: it ends the turn once every chat has closed with nothing waiting on you. */
  arm(turnKey, force = false) { this.armed = { turnKey, force: !!force }; }
  disarm() { this.armed = null; }
  /** A new turn (or someone else's) disarms. */
  syncTurn(turnKey) { if (this.armed && this.armed.turnKey !== turnKey) this.armed = null; }

  /** End Turn's state from the open chats (all of them the seat's own):
   * - `answer`: a chat waits on you; End Turn opens it ("Answer Rome first"); the oldest is named;
   * - `waiting`: chats wait on the others ("Waiting for Rome… 4:12": the one that may take longest, `more` others);
   * - `armed`: the same, after a click: the turn ends once they have all closed ("End turn when they answer");
   * - `clear`: no open chat; `fire` when armed (end the turn now).
   * `armed` is true in each state the arming holds through (an answer that comes back to you keeps it). */
  endTurnState(now, { myTurn = true, names = {} } = {}) {
    const open = this.openChats();
    const armed = !!this.armed;
    const label = (c) => names[c.with] || c.withName || `#${c.with}`;
    const mine = open.filter((c) => c.waitingOn === this.you);
    if (mine.length) {
      const c = mine[0];
      return { state: "answer", nid: c.nid, with: c.with, name: label(c), leftS: leftAt(c.clock, now), more: mine.length - 1, armed, fire: false };
    }
    if (open.length) {
      // the chat that may still take longest: an unknown deadline (a bot, a paused game) counts as longest
      let best = null, bestLeft = -1;
      for (const c of open) {
        const left = leftAt(c.clock, now);
        const key = left == null ? Infinity : left;
        if (key > bestLeft) { best = c; bestLeft = key; }
      }
      return { state: armed ? "armed" : "waiting", nid: best.nid, with: best.with, name: label(best),
               leftS: bestLeft === Infinity ? null : bestLeft, more: open.length - 1, armed, fire: false };
    }
    return { state: "clear", nid: null, with: null, name: "", leftS: null, more: 0, armed, fire: armed && !!myTurn };
  }

  // ------------------------------------------------------------------ the windows

  windowOf(civ) { return this.windows.find((w) => w.civ === civ) || null; }

  /** Open (or bring back) the window with `civ`: a new one docks after the others; `restore` expands a minimised
   * one; `focus` makes it the one shown at narrow widths. Returns the window. */
  openWindow(civ, { restore = true, focus = false } = {}) {
    let w = this.windowOf(civ);
    if (!w) { w = { civ, minimised: false, unread: false }; this.windows.push(w); }
    else if (restore) w.minimised = false;
    if (!w.minimised) w.unread = false;
    if (focus || this.active == null || !this.windowOf(this.active)) this.active = civ;
    return w;
  }

  /** Something new for you in the window with `civ`: a closed window opens, a minimised one is marked unread (its
   * tab lights up) rather than taking the screen from what you are doing. */
  notify(civ, { restore = false } = {}) {
    const w = this.windowOf(civ);
    if (!w) {
      const made = this.openWindow(civ, { restore: true });
      if (this.active !== civ) made.unread = true;   // at narrow widths only the active one shows
      return made;
    }
    if (restore) w.minimised = false;
    if (w.minimised || (this.active != null && this.active !== civ)) w.unread = true;
    if (this.active == null) this.active = civ;
    return w;
  }

  minimise(civ) {
    const w = this.windowOf(civ);
    if (!w) return;
    w.minimised = true;
    if (this.active === civ) this.active = null;
  }

  restore(civ) {
    const w = this.windowOf(civ);
    if (!w) return;
    w.minimised = false; w.unread = false;
    this.active = civ;
  }

  closeWindow(civ) {
    this.windows = this.windows.filter((w) => w.civ !== civ);
    if (this.active === civ) {
      const next = [...this.windows].reverse().find((w) => !w.minimised);
      this.active = next ? next.civ : null;
    }
  }
}

/** Where the docked windows go (P3.5.6): along the bottom edge, to the left of the End Turn box, never over it.
 *
 * `width`/`height` are the game area's; `turnBox` is the End Turn box's rectangle in it ({left, top, width,
 * height}); `windows` are the model's, oldest first ({civ, minimised}); `active` the window shown when narrow.
 *
 * Wide: windows side by side from right to left in the order they opened, the first next to the End Turn box;
 * minimised ones are tabs at the left end of the row; when the open ones do not fit, the oldest are made tabs
 * (`overflow`) until they do. Narrow (`width` at most NARROW_WIDTH, or no room beside the box for one window, or
 * for every window as a tab): one window at a time, full width above the End Turn box, under a strip with a tab
 * per window.
 *
 * Returns `{mode, items, overflow, container}`: items `{civ, kind: window|tab, left, width, bottom, maxHeight}` in
 * pixels from the area's left and bottom edges (a tab's `maxHeight` is its height). Narrow, `container` is the box
 * that holds the strip (`tabs`, the `active` one marked) over the one window (`items`' only entry, if any is open):
 * `{left, width, bottom, maxHeight, tabs, active}`. */
export function dockLayout({ width, height, turnBox, windows, active = null, margin = 10, gap = 8,
                             windowWidth = 340, minWidth = 260, tabWidth = 150, minTab = 96, tabHeight = 32,
                             maxWindowHeight = 520, narrowAt = NARROW_WIDTH } = {}) {
  const W = Math.max(0, width || 0), H = Math.max(0, height || 0);
  const box = turnBox || { left: W - margin, top: H - margin, width: 0, height: 0 };
  const list = (windows || []).filter((w) => w && w.civ != null);
  const right = Math.min(W - margin, box.left - gap);   // the row's right edge: left of the End Turn box
  const room = right - margin;
  const winHeight = Math.max(80, Math.min(maxWindowHeight, H - 2 * margin));
  const wide = W > narrowAt && room >= minWidth && list.length * (minTab + gap) - gap <= room;
  if (wide) {
    const order = list.map((w) => ({ civ: w.civ, minimised: !!w.minimised }));
    const overflow = [];
    const fits = () => {
      const open = order.filter((w) => !w.minimised);
      const tabs = order.length - open.length;
      const ww = open.length ? Math.min(windowWidth, Math.floor((room - (open.length - 1) * gap - tabs * (minTab + gap)) / open.length)) : 0;
      return { ok: !open.length || ww >= minWidth, ww, tabs };
    };
    let f = fits();
    while (!f.ok) {
      const oldest = order.find((w) => !w.minimised);
      oldest.minimised = true;
      overflow.push(oldest.civ);
      f = fits();
    }
    const open = order.filter((w) => !w.minimised);
    const tabs = order.filter((w) => w.minimised);
    const items = [];
    let x = right;
    for (const w of open) {                 // the oldest next to the End Turn box, the newer ones to its left
      x -= f.ww;
      items.push({ civ: w.civ, kind: "window", left: x, width: f.ww, bottom: margin, maxHeight: winHeight });
      x -= gap;
    }
    const tabRoom = x - margin;            // x is already a gap left of the last window
    const tw = tabs.length ? Math.max(minTab, Math.min(tabWidth, Math.floor((tabRoom - (tabs.length - 1) * gap) / tabs.length))) : 0;
    let tx = margin;
    for (const w of tabs) {                 // the tabs at the left end, oldest first
      items.push({ civ: w.civ, kind: "tab", left: tx, width: tw, bottom: margin, maxHeight: tabHeight });
      tx += tw + gap;
    }
    return { mode: "wide", items, overflow, container: null };
  }
  // narrow: one window, above the End Turn box, under its tab strip
  const shown = list.find((w) => w.civ === active && !w.minimised) ? active : null;
  const bottom = Math.max(margin, H - box.top + gap);
  const full = Math.max(0, W - 2 * margin);
  const maxHeight = Math.max(tabHeight, Math.min(maxWindowHeight + tabHeight + 4, H - bottom - margin));
  const items = shown == null ? [] : [{ civ: shown, kind: "window", left: margin, width: full, bottom,
                                        maxHeight: Math.max(0, maxHeight - tabHeight - 4) }];
  const container = list.length ? { left: margin, width: full, bottom, maxHeight, tabs: list.map((w) => w.civ), active: shown } : null;
  return { mode: "narrow", items, overflow: [], container };
}

/** Whether two rectangles in the dock's terms ({left, width, bottom, height} from the area's left and bottom) meet. */
export function overlaps(a, b) {
  return a.left < b.left + b.width && b.left < a.left + a.width && a.bottom < b.bottom + b.height && b.bottom < a.bottom + a.height;
}

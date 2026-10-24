// The docked chat windows of the game screen (design/phase3-features.md P3.5.6): one per civilization (the engine
// allows one open chat per pair), along the bottom edge to the left of the End Turn box, which the chats drive and
// which they never cover; at 700 px wide or less one window at a time, full width above the End Turn box, with a
// tab per window. chatmodel.js decides (pure, tested with Node); this draws.
//
// Nothing here redraws a window whole. New entries are appended in `seq` order; the status line, the buttons and the
// proposal on the table are updated in place; the text box and the deal builder are built once per window and keep
// what the person typed and added (their state is the model's draft, which no update touches), so a chat moving, a
// status arriving or the view refreshing never wipes them. A window opens straight from the socket: a `negotiation`
// event naming the seat reads the chat route (`GET /api/games/{gid}/negotiations/{nid}`), and a `negotiation_status`
// moves its status line (thinking, queued with a place, waiting with a countdown), without waiting for a refresh.
//
// Replaces the negotiation half of the Diplomacy modal (panels.js before 3-12), which was rebuilt from get_diplomacy
// on every negotiation event and lost the message being typed and the deal being built.
import { el, clear, toast } from "./util.js";
import { ChatModel, builder, describeItem, dockLayout, fmtClock, leftAt, DEFAULT_LINES, NARROW_WIDTH } from "./chatmodel.js";

/** Agreements both sides make at once: the builder files them under "You give" and the engine applies both. */
const MUTUAL = ["peace_treaty", "declaration_of_friendship", "research_agreement", "defensive_pact"];

/** The item kinds the builder offers, with their labels. */
const ITEM_TYPES = [["gold", "Gold"], ["gold_per_turn", "Gold per turn"], ["resource", "Resource"], ["open_borders", "Open borders"],
  ["embassy", "Embassy"], ["peace_treaty", "Peace treaty"], ["declaration_of_friendship", "Declaration of friendship"],
  ["research_agreement", "Research agreement"], ["defensive_pact", "Defensive pact"], ["declare_war", "Declare war on…"],
  ["city", "City"], ["share_map", "World map"], ["tech", "Technology"]];

const ACTION_TEXT = { open: "opened the chat", counter: "counter-offer", accept: "accepted", reject: "declined" };

export class ChatDock {
  /** `game` is the game screen: its `view`, `you`, `myTurn`, `rules`, `tool(name, args)` (a tool call that reports
   * its own errors and returns the result or null), `endTurn(force)` (which checks the turn's other decisions unless
   * forced), `syncView()` (fetches a view issued after the call, once any in flight has come) and `renderTopbar()`.
   * `host` is the element the windows are placed in (the map's area), `turnBox` the End Turn box in it, `obstacles`
   * the panels over the map that the windows keep clear of when there is room (a unit's, a city's, the side panel,
   * the minimap: those shown count). `fetchChat(nid)` reads the chat route, `fetchOptions(civ)` the trade options
   * route (`{trade_options}`: what the deal builder suggests); `now()` is a monotonic clock in ms; `timers` holds
   * setInterval/clearInterval (tests pass their own). The game's `viewSeq` counts the views it fetched (a 304 that
   * says the one in hand still is counts too). */
  constructor(game, { host, turnBox, obstacles = [], fetchChat, fetchOptions = null, now = () => performance.now(),
                      timers = globalThis } = {}) {
    this.game = game;
    this.host = host;
    this.turnBox = turnBox;
    this.obstacles = obstacles.filter(Boolean);
    this.fetchChat = fetchChat;
    this.fetchOptions = fetchOptions;
    this.now = now;
    this.timers = timers;
    this.model = new ChatModel(null);
    this.windows = new Map();     // civ -> ChatWindow
    this.fetching = new Map();    // nid -> true while a read is out (and asked again meanwhile: "again")
    this.clockAsked = new Map();  // nid -> the chat's version a read was asked at for want of its clock
    this.enabled = false;
    this._endKey = null;          // the End Turn state the turn box was last drawn with
    this._endClock = null;        // its countdown
    this._stripKey = null;        // what the narrow layout's tab strip was last built from
    this._fireAfter = -Infinity;  // an armed End Turn whose fresh view did not come tries again from then
    this.root = el("div", { class: "chat-dock wide" });
    this.strip = el("div", { class: "chat-strip" });
    this.root.appendChild(this.strip);
    if (host) host.appendChild(this.root);
    this._tick = timers.setInterval ? timers.setInterval(() => this.tick(), 1000) : null;
    this._onResize = () => this.layout();
    if (typeof window !== "undefined" && window.addEventListener) window.addEventListener("resize", this._onResize);
    if (typeof ResizeObserver !== "undefined") {
      // the area, End Turn and the panels: a panel shown, hidden or grown moves the windows clear of it
      this._observer = new ResizeObserver(() => this.layout());
      for (const node of [host, turnBox, ...this.obstacles]) if (node) this._observer.observe(node);
    }
  }

  destroy() {
    if (this._tick != null && this.timers.clearInterval) this.timers.clearInterval(this._tick);
    if (typeof window !== "undefined" && window.removeEventListener) window.removeEventListener("resize", this._onResize);
    if (this._observer) this._observer.disconnect();
    this.root.remove();
  }

  // ------------------------------------------------------------------ what comes in

  /** Each view the game screen fetched: the chats in its `diplomacy` block, and whose turn it is. Called before the
   * game screen draws its top bar, which then draws End Turn from the chats as they now stand. The view says nothing
   * of a chat's clock, and the server tells a clock only when it changes, so an open chat the client knows no clock
   * of (after a page load, or one the view showed moving first) is read once at that version for it. */
  onView(view) {
    this.enabled = !!(view && view.you != null && !view.spectator && view.diplomacy);
    this.root.style.display = this.enabled ? "" : "none";
    if (!this.enabled) return;
    this.model.setYou(view.you);
    this.model.syncTurn(this.turnKey());
    const res = this.model.applyView(view.diplomacy, this.now());
    for (const change of res.changes) this.consider(change);
    for (const nid of res.missing) this.read(nid);
    for (const chat of this.model.openChats()) {
      if (chat.clock || this.clockAsked.get(chat.nid) === chat.ver) continue;
      this.clockAsked.set(chat.nid, chat.ver);
      this.read(chat.nid);
    }
    this.render({ topbar: false });
  }

  /** A game event from the socket: a `negotiation` naming the seat reads its chat at once. */
  onEvent(ev) {
    if (!this.enabled || !ev || ev.type !== "negotiation") return;
    const nid = ev.data && ev.data.negotiation;
    if (!Number.isInteger(nid)) return;
    if (ev.players && this.model.you != null && !ev.players.includes(this.model.you)) return;
    this.read(nid);
  }

  /** A `negotiation_status` from the socket. */
  onStatus(msg) {
    if (!this.enabled) return;
    const change = this.model.applyStatus(msg, this.now());
    if (!change) return;
    this.consider(change);
    if (change.fetch) this.read(msg.negotiation);
    this.render();
  }

  /** What a change of a chat means for its window: a new open chat pops up (restored if minimised, and the one shown
   * at narrow widths unless you are answering another); news of a chat the client knew (it comes back to you, the
   * other side or the game said something, it closed) lights the window up: a window closed with ✕ opens again, a
   * minimised one only marks its tab. A chat first seen closed (a page load's recent chats) is not news. */
  consider(change) {
    const chat = change && change.chat;
    if (!chat || chat.with == null) return;
    if (change.created) {
      if (chat.status === "open") this.model.notify(chat.with, { restore: true, focus: true });
      return;
    }
    const theirs = (change.added || []).some((e) => !e.you);
    if (change.turnedToYou || theirs || change.closed) this.model.notify(chat.with);
  }

  /** Read chat `nid` from the chat route; one read at a time per chat, and once more if asked meanwhile. */
  read(nid) {
    if (!this.fetchChat) return;
    if (this.fetching.has(nid)) { this.fetching.set(nid, "again"); return; }
    this.fetching.set(nid, true);
    Promise.resolve().then(() => this.fetchChat(nid)).then((nv) => {
      if (!nv || !this.enabled) return;
      const change = this.model.applyNegotiation(nv, this.now());
      this.consider(change);
      this.render();
    }).catch(() => { /* a chat the seat may not read, or a blip: the next event or view brings it */ })
      .finally(() => {
        const again = this.fetching.get(nid) === "again";
        this.fetching.delete(nid);
        if (again) this.read(nid);
      });
  }

  // ------------------------------------------------------------------ what the person asks for

  /** Open the window with `civ` (Diplomacy's "Open chat", End Turn's "Answer first", an alert) and put the cursor in
   * its text box when it may be typed in. */
  open(civ, { focus = true } = {}) {
    if (!this.enabled || civ == null) return;
    this.model.openWindow(civ, { restore: true, focus: true });
    this.render();
    const w = this.windows.get(civ);
    if (focus && w && !w.text.disabled) w.text.focus();
  }

  minimise(civ) { this.model.minimise(civ); this.render(); }
  restore(civ) { this.model.restore(civ); this.render(); }
  close(civ) {
    this.model.closeWindow(civ);
    const w = this.windows.get(civ);
    if (w) { w.root.remove(); this.windows.delete(civ); }
    this.render();
  }

  turnKey() { const v = this.game.view; return v ? `${v.turn}:${v.current_player}` : ""; }

  /** End Turn's state from the chats (chatmodel's `endTurnState`). */
  endTurnState() {
    return this.model.endTurnState(this.now(), { myTurn: !!this.game.myTurn, names: this.names() });
  }

  arm(force = false) { this.model.arm(this.turnKey(), force); this.render(); this.game.renderTopbar(); }
  disarm() { this.model.disarm(); this.render(); this.game.renderTopbar(); }

  /** End Turn clicked, or Shift+Enter (`GameScreen.onEndTurn`): `force` when Ctrl or ⌘ was held, `blockers` the
   * turn's other open decisions (`GameScreen.turnBlockers`). Does the chats' part and returns what the game screen
   * does next:
   * - `open`: a chat waits on you; its window opened, the cursor in it ("Answer Rome first");
   * - `disarm`: End Turn was armed; the click cancelled it;
   * - `blocked`: decisions are left and Ctrl was not held; the game screen shows the first (nothing armed);
   * - `arm`: chats wait on the others; armed, the turn ends once they have all closed (`force` kept for then);
   * - `end`: nothing holds the turn; the game screen ends it;
   * - `none`: not your turn. */
  endTurnClick(force = false, blockers = []) {
    if (!this.game.myTurn) return "none";
    const st = this.endTurnState();
    if (st.state === "answer") { this.open(st.with); return "open"; }
    if (st.state === "armed") { this.disarm(); return "disarm"; }
    if (blockers.length && !force) return "blocked";
    if (st.state === "waiting") { this.arm(force); return "arm"; }
    return "end";
  }

  /** The End Turn button while a chat holds the turn (null when none does: the game screen draws its own). `onClick`
   * gets whether Ctrl or ⌘ was held. */
  endTurnButton(onClick) {
    const st = this.endTurnState();
    this._endKey = this.endKey(st);
    this._endClock = null;
    if (st.state === "clear") return null;
    const more = st.more ? ` (+${st.more})` : "";
    if (st.state === "answer") {
      return el("button", { class: "end-turn answer", "data-chat": st.state,
        title: `${st.name} waits for your answer: the turn cannot end while a chat you are in is open.${st.armed ? "\nArmed: the turn ends once every chat has closed." : ""}`,
        onclick: (e) => onClick(!!(e.ctrlKey || e.metaKey)) }, `Answer ${st.name} first${more}${st.armed ? " · armed" : ""}`);
    }
    const clock = el("span", { class: "chat-clock" }, st.leftS != null ? ` ${fmtClock(st.leftS)}` : "");
    this._endClock = { span: clock, nid: st.nid };
    if (st.state === "armed") {
      return el("button", { class: "end-turn armed", "data-chat": st.state, title: "The turn ends once every chat has closed. Click to cancel.",
        onclick: (e) => onClick(!!(e.ctrlKey || e.metaKey)) }, `End turn when they answer${more}`, clock, el("span", { class: "chat-cancel" }, " ✕"));
    }
    return el("button", { class: "end-turn waiting", "data-chat": st.state,
      title: `Waiting for ${st.name}'s answer: the turn cannot end while a chat you are in is open.\nClick to end the turn as soon as every chat has closed.`,
      onclick: (e) => onClick(!!(e.ctrlKey || e.metaKey)) }, `Waiting for ${st.name}…${more}`, clock);
  }

  endKey(st) { return `${st.state}:${st.nid}:${st.name}:${st.more}:${st.armed}:${!!this.game.myTurn}`; }

  // ------------------------------------------------------------------ drawing

  names() {
    const out = {};
    for (const p of (this.game.view && this.game.view.players) || []) out[p.id] = p.name || "Unknown";
    return out;
  }

  player(civ) {
    const v = this.game.view;
    return (v && (v.players || []).find((p) => p.id === civ)) || { id: civ, name: `#${civ}`, color: "#999" };
  }

  /** Bring every window up to date, place them, and keep End Turn in step (`topbar`: draw the top bar again when
   * End Turn's state changed; the game screen's refresh draws it itself). */
  render({ topbar = true } = {}) {
    if (!this.enabled) return;
    const now = this.now();
    for (const w of this.model.windows) {
      let win = this.windows.get(w.civ);
      if (!win) { win = new ChatWindow(this, w.civ); this.windows.set(w.civ, win); this.root.appendChild(win.root); }
      win.update(now);
    }
    for (const [civ, win] of [...this.windows]) {
      if (!this.model.windowOf(civ)) { win.root.remove(); this.windows.delete(civ); }
    }
    this.layout();
    const st = this.endTurnState();
    if (topbar && this.game.myTurn && this.endKey(st) !== this._endKey) this.game.renderTopbar();
    this.fire(st);
  }

  /** Armed End Turn with nothing left open: end the turn (once), as the click that armed it would have. Its other
   * decisions were checked when it was armed; a deal may have brought a new one (a technology received that finishes
   * the research, a city to give orders to), which a plain arming checks again and stops at, as End Turn does, and a
   * Ctrl+click arming skips. That check reads the view, and the one in hand may predate the close, so a view
   * fetched after it comes first (`game.syncView`: the game's `viewSeq` moves, whether the fetch brought a new view
   * or a 304 said the one in hand still is). It is disarmed at once, so it fires once; it is armed again when no
   * fresh view came (it tries again a second later, at a tick) or a chat opened meanwhile (it fires when that one
   * closes). */
  fire(st) {
    if (!st.fire || !this.game.myTurn || !this.model.armed || this.now() < this._fireAfter) return;
    const { force, turnKey } = this.model.armed;
    this.model.disarm();
    const before = this.game.viewSeq;
    const again = (wait) => {
      if (!this.enabled || this.model.armed || !this.game.myTurn || this.turnKey() !== turnKey) return;
      this.model.arm(turnKey, force);
      if (wait) this._fireAfter = this.now() + 1000;
      this.render();
      this.game.renderTopbar();
    };
    Promise.resolve().then(() => this.game.syncView()).then(() => {
      if (!this.enabled || !this.game.myTurn || this.turnKey() !== turnKey) return;
      if (this.game.viewSeq === before) { again(true); return; }
      if (this.endTurnState().state !== "clear") { again(false); return; }
      return this.game.endTurn(force);
    }).catch(() => { again(true); });
  }

  /** Every second: the countdowns. */
  tick() {
    if (!this.enabled) return;
    const now = this.now();
    for (const win of this.windows.values()) win.tickStatus(now);
    if (this._endClock && this._endClock.span) {
      const chat = this.model.chats.get(this._endClock.nid);
      const left = chat ? leftAt(chat.clock, now) : null;
      this._endClock.span.textContent = left != null ? ` ${fmtClock(left)}` : "";
    }
    this.fire(this.endTurnState());
  }

  /** Place the windows (chatmodel's `dockLayout`) in the game's map area, clear of End Turn and of the panels shown. */
  layout() {
    if (!this.enabled || !this.host) return;
    const hostRect = rect(this.host);
    const boxRect = rect(this.turnBox);
    const width = this.host.clientWidth || hostRect.width;
    const height = this.host.clientHeight || hostRect.height;
    const inHost = (r) => ({ left: r.left - hostRect.left, top: r.top - hostRect.top, width: r.width, height: r.height });
    const turnBox = boxRect.width || boxRect.height ? inHost(boxRect) : { left: width - 10, top: height - 10, width: 0, height: 0 };
    const obstacles = this.obstacles.filter((n) => !(n.style && n.style.display === "none")).map(rect)
      .filter((r) => r.width > 0 && r.height > 0).map(inHost);
    const plan = dockLayout({ width, height, turnBox, obstacles, windows: this.model.windows, active: this.model.active });
    const placed = new Map(plan.items.map((it) => [it.civ, it]));
    this.root.className = `chat-dock ${plan.mode}`;
    const rs = this.root.style;
    if (plan.mode === "narrow" && plan.container) {
      Object.assign(rs, { left: `${plan.container.left}px`, width: `${plan.container.width}px`, bottom: `${plan.container.bottom}px`,
                          maxHeight: `${plan.container.maxHeight}px`, top: "", right: "" });
    } else {
      Object.assign(rs, { left: "", width: "", bottom: "", maxHeight: "", top: "", right: "" });
    }
    for (const [civ, win] of this.windows) {
      const it = placed.get(civ);
      const s = win.root.style;
      if (!it) { s.display = "none"; continue; }
      s.display = "";
      win.setTab(it.kind === "tab");
      const w = this.model.windowOf(civ);
      if (w && it.kind === "window" && w.unread) { w.unread = false; win.updateHead(); }   // in sight: nothing unread
      if (plan.mode === "wide") {
        Object.assign(s, { left: `${it.left}px`, width: `${it.width}px`, bottom: `${it.bottom}px`, maxHeight: `${it.maxHeight}px` });
      } else {
        Object.assign(s, { left: "", width: "", bottom: "", maxHeight: `${it.maxHeight}px` });
      }
      if (it.kind === "window") win.pin();
    }
    this.drawStrip(plan);
    this.plan = plan;
  }

  /** The narrow layout's tab strip. Its buttons are made again only when the tabs themselves change (which windows,
   * their names and colours); a tab's look (shown, unread, yours) changes in place. `layout` runs on every status and
   * every top bar redraw, several times a second in a fast game, and a tap whose button was replaced between press
   * and release would be lost: on a phone the strip is the only way between windows. */
  drawStrip(plan) {
    const strip = this.strip;
    if (plan.mode !== "narrow" || !plan.container) {
      if (strip.style.display !== "none") strip.style.display = "none";
      return;
    }
    if (strip.style.display !== "") strip.style.display = "";
    const tabs = plan.container.tabs.map((civ) => ({ civ, p: this.player(civ) }));
    const key = JSON.stringify(tabs.map(({ civ, p }) => [civ, p.name, p.color]));
    if (key !== this._stripKey) {
      this._stripKey = key;
      clear(strip);
      for (const { civ, p } of tabs) {
        // the click reads where things stand when it lands, not when the button was made
        strip.appendChild(el("button", { class: "chat-tab", "data-civ": civ,
          onclick: () => (this.plan && this.plan.container && this.plan.container.active === civ ? this.minimise(civ) : this.restore(civ)) },
          el("span", { class: "swatch", style: { background: p.color } }), p.name || `#${civ}`));
      }
    }
    for (const btn of strip.children) {
      const civ = Number(btn.getAttribute("data-civ"));
      const w = this.model.windowOf(civ);
      const chat = this.model.chatWith(civ);
      const yours = !!chat && chat.status === "open" && chat.waitingOn === this.model.you;
      const cls = `chat-tab${civ === plan.container.active ? " active" : ""}${w && w.unread ? " unread" : ""}${yours ? " yours" : ""}`;
      if (btn.className !== cls) btn.className = cls;
      const name = this.player(civ).name;
      const title = yours ? `${name} waits for your answer` : name;
      if (btn.title !== title) btn.title = title;
    }
  }
}

function rect(node) {
  if (!node || !node.getBoundingClientRect) return { left: 0, top: 0, width: 0, height: 0 };
  const r = node.getBoundingClientRect();
  return { left: r.left || 0, top: r.top || 0, width: r.width || 0, height: r.height || 0 };
}

/** One civilization's window: its head (name, minimise, close), the chat's entries, the proposal on the table, the
 * status line, the text box with its buttons, and the deal builder. Built once; `update` changes it in place. */
class ChatWindow {
  constructor(dock, civ) {
    this.dock = dock;
    this.civ = civ;
    this.draft = dock.model.draft(civ);
    this.rendered = [];            // [nid, seq] of every entry and divider in the log, in display order
    this.nodes = new Map();        // `${nid}:${seq}` -> its node (seq 0: a chat's divider)
    this.busy = false;
    this._proposalKey = null;
    this._hintsKey = null;
    this._noteKey = null;
    // the trade options the deal builder suggests, fetched while it is open (`wantOptions`): what they were last
    // asked at (the turn and the chats' entries), the answer, and whether a fetch is out ("again": asked meanwhile)
    this._optionsKey = null;
    this.options = null;
    this._optionsBusy = false;
    const p = dock.player(civ);
    this.swatch = el("span", { class: "swatch", style: { background: p.color } });
    this.title = el("span", { class: "chat-name" }, p.name || `#${civ}`);
    this.dot = el("span", { class: "chat-dot", title: "Something new" });
    this.head = el("div", { class: "chat-head", onclick: (e) => { if (this.tab && !e.defaultPrevented) dock.restore(civ); } },
      this.swatch, this.title, this.dot, el("span", { class: "grow" }),
      el("button", { class: "small chat-min", title: "Minimise", onclick: (e) => { e.preventDefault(); e.stopPropagation(); if (this.tab) dock.restore(civ); else dock.minimise(civ); } }, "–"),
      el("button", { class: "small chat-close", title: "Close (the chat goes on; End Turn or Diplomacy opens it again)",
        onclick: (e) => { e.preventDefault(); e.stopPropagation(); dock.close(civ); } }, "✕"));
    // the log follows its newest entry unless the reader scrolled up to read back
    this.follow = true;
    this.log = el("div", { class: "chat-log", onscroll: () => {
      this.follow = this.log.scrollTop + this.log.clientHeight >= this.log.scrollHeight - 8;
    } });
    this.proposal = el("div", { class: "chat-proposal" });
    this.status = el("div", { class: "chat-status" });
    this.count = el("div", { class: "chat-count" });   // messages against the game's cap
    this.text = el("textarea", { class: "chat-text", rows: 2, placeholder: "Write a message…", value: this.draft.text,
      oninput: () => { this.draft.text = this.text.value; this.updateButtons(); },
      onkeydown: (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); this.primary(); } } });
    this.btn = {
      send: el("button", { class: "primary", onclick: () => this.respond("reply") }, "Send"),
      accept: el("button", { class: "primary", onclick: () => this.respond("accept") }, "Accept"),
      reject: el("button", { class: "danger", onclick: () => this.respond("reject") }, "No deal"),
      counter: el("button", { onclick: () => this.respond("counter") }, "Counter with the deal"),
      withdraw: el("button", { class: "danger", onclick: () => this.respond("reject") }, "Withdraw"),
      open: el("button", { class: "primary", onclick: () => this.openChat() }, "Open chat"),
    };
    this.buttons = el("div", { class: "btn-grid chat-buttons" }, ...Object.values(this.btn));
    this.dealToggle = el("button", { class: "small chat-deal-toggle", onclick: () => { this.draft.builderOpen = !this.draft.builderOpen; this.drawDeal(); } });
    this.giveList = el("div", { class: "deal-side" });
    this.receiveList = el("div", { class: "deal-side" });
    this.hints = el("div", { class: "trade-hints" });
    this.dealNote = el("div", { class: "muted chat-deal-note" });
    this.adder = this.buildAdder();
    this.dealBody = el("div", { class: "chat-deal-body" },
      el("div", { class: "deal-builder" }, this.giveList, this.receiveList), this.hints, this.adder, this.dealNote);
    this.deal = el("div", { class: "chat-deal" }, this.dealToggle, this.dealBody);
    this.body = el("div", { class: "chat-body" }, this.log, this.proposal, this.status, this.count, this.text, this.buttons, this.deal);
    this.root = el("div", { class: "chat-win", "data-civ": civ, role: "dialog", "aria-label": `Chat with ${p.name || civ}` }, this.head, this.body);
    this.tab = false;
    this.drawDeal();
  }

  setTab(tab) {
    if (this.tab === tab) return;
    this.tab = tab;
    this.root.className = `chat-win ${tab ? "tab" : ""}`;
    this.updateHead();
  }

  state(now) {
    const d = this.dock;
    return d.model.windowState(this.civ, now, { myTurn: !!d.game.myTurn, name: d.player(this.civ).name, draft: this.draft });
  }

  /** Bring the window in line with the model: new entries, the proposal, the status, the buttons, the hints. */
  update(now) {
    const p = this.dock.player(this.civ);
    this.title.textContent = p.name || `#${this.civ}`;
    this.swatch.style.background = p.color;
    this.appendEntries();
    const st = this.state(now);
    this.drawProposal(st);
    this.drawStatus(st);
    this.updateButtons(st);
    this.updateHead();
    this.wantOptions();
    this.drawHints();
    this.drawDealNote();
  }

  updateHead() {
    const w = this.dock.model.windowOf(this.civ);
    const chat = this.dock.model.chatWith(this.civ);
    const yours = !!chat && chat.status === "open" && chat.waitingOn === this.dock.model.you;
    this.root.className = `chat-win${this.tab ? " tab" : ""}${w && w.unread ? " unread" : ""}${yours ? " yours" : ""}`;
  }

  tickStatus(now) {
    if (this.tab) return;
    this.drawStatus(this.state(now));
  }

  /** Every entry of every chat with this civilization, in (chat, seq) order: the new ones put in their places, a
   * divider before each chat's first. Nothing already drawn is drawn again. */
  appendEntries() {
    const chats = this.dock.model.chatsWith(this.civ);
    let added = false;
    for (const chat of chats) {
      if (!chat.entries.length) continue;
      if (!this.nodes.has(`${chat.nid}:0`)) {
        this.insert(chat.nid, 0, el("div", { class: "chat-divider" }, `Chat #${chat.nid}${chat.turn != null ? ` · turn ${chat.turn}` : ""}`));
        added = true;
      }
      chat.entries.forEach((e, i) => {
        if (this.nodes.has(`${chat.nid}:${e.seq}`)) return;
        this.insert(chat.nid, e.seq, this.entryNode(e, i ? chat.entries[i - 1] : null));
        added = true;
      });
    }
    if (added) this.pin();
  }

  /** Keep the newest entry in sight (when following): after entries arrive, and after the window's size changed. */
  pin() {
    if (this.follow) this.log.scrollTop = this.log.scrollHeight;
  }

  insert(nid, seq, node) {
    const at = this.rendered.findIndex(([n, s]) => n > nid || (n === nid && s > seq));
    if (at < 0) { this.log.appendChild(node); this.rendered.push([nid, seq]); }
    else {
      const [n, s] = this.rendered[at];
      this.log.insertBefore(node, this.nodes.get(`${n}:${s}`));
      this.rendered.splice(at, 0, [nid, seq]);
    }
    node.setAttribute("data-seq", `${nid}:${seq}`);
    this.nodes.set(`${nid}:${seq}`, node);
  }

  /** An entry: a bubble (yours on the right), with the proposal it made as a card; the game's own close notes in
   * grey. A decline by the side that spoke last is a withdrawal (`prev`: the entry before it). */
  entryNode(e, prev) {
    if (e.action === "close") {
      return el("div", { class: "chat-note" }, e.note || e.message || "closed");
    }
    const who = e.you ? "You" : (e.by || this.dock.player(this.civ).name);
    const what = e.action === "reject" && prev && prev.you === e.you && prev.action !== "close" ? "withdrew" : ACTION_TEXT[e.action];
    return el("div", { class: `msg ${e.you ? "me" : "them"}` },
      el("div", { class: "meta" }, `${who}${what ? ` · ${what}` : ""}`),
      e.message ? el("div", { class: "chat-msg-text" }, e.message) : null,
      e.proposal ? el("div", { class: "chat-card" }, e.proposal.summary || "A proposal") : null,
      e.note ? el("div", { class: "chat-note" }, e.note) : null);
  }

  drawProposal(st) {
    const chat = st.chat;
    const open = chat && chat.status === "open" && chat.proposal;
    const key = open ? JSON.stringify([chat.nid, chat.proposal, chat.proposalByYou, st.mode]) : "";
    if (key === this._proposalKey) return;
    this._proposalKey = key;
    const box = clear(this.proposal);
    if (!open) { box.style.display = "none"; return; }
    box.style.display = "";
    const name = this.dock.player(this.civ).name;
    box.append(el("div", {}, el("span", { class: "muted" }, `On the table (by ${chat.proposalByYou ? "you" : name}): `),
      el("b", {}, chat.proposal.summary || "")),
      st.mode === "your_move" ? el("button", { class: "small", title: "Copy it into the deal builder to change it and counter",
        onclick: () => { builder.load(this.draft, chat.proposal); this.draft.builderOpen = true; this.drawDeal(); this.updateButtons(); } },
        "Load into the builder") : null);
  }

  drawStatus(st) {
    const s = st.status;
    if (this.status.textContent !== s.text) this.status.textContent = s.text;
    const cls = `chat-status ${s.tone || ""}`;
    if (this.status.className !== cls) this.status.className = cls;
    // how many messages it has had against the cap: the one that would pass it closes the chat instead
    const c = st.count;
    const text = c ? (c.left ? `${c.text} (${c.left} left)` : `${c.text}: the next closes the chat`) : "";
    if (this.count.textContent !== text) this.count.textContent = text;
    const ccls = `chat-count ${c ? c.tone : ""}`;
    if (this.count.className !== ccls) this.count.className = ccls;
    const show = c ? "" : "none";
    if (this.count.style.display !== show) this.count.style.display = show;
  }

  /** Strict turn-taking: the text box and the answers only on your move (Withdraw on theirs); opening a chat only on
   * your own turn. */
  updateButtons(st = null) {
    st = st || this.state(this.dock.now());
    const can = st.can;
    const hasDeal = !builder.empty(this.draft);
    this.text.disabled = !st.input || this.busy;
    this.text.placeholder = st.mode === "your_move" ? "Write your answer…" : st.mode === "their_move" ? "Their move: you can write once they answer."
      : st.input ? "Write to open a chat…" : "Chats are opened on your turn.";
    const show = { send: can.send, accept: st.mode === "your_move", reject: st.mode === "your_move", counter: st.mode === "your_move",
                   withdraw: st.mode === "their_move", open: st.mode === "new" || st.mode === "closed" };
    for (const [k, b] of Object.entries(this.btn)) {
      b.style.display = show[k] ? "" : "none";
      b.disabled = this.busy || !can[k] || (k === "counter" && !hasDeal);
    }
    this.btn.open.textContent = hasDeal ? "Propose the deal" : "Open chat";
    this.btn.open.title = st.input ? "" : "Chats are opened on your turn";
    this.btn.accept.title = can.accept ? "" : "Nothing of theirs is on the table";
    this.btn.counter.title = hasDeal ? "" : "Add items to the deal builder first";
  }

  /** Enter in the text box: the answer it would send (a reply), or a new chat. */
  primary() {
    const st = this.state(this.dock.now());
    if (st.mode === "your_move") return this.respond("reply");
    if ((st.mode === "new" || st.mode === "closed") && st.can.open) return this.openChat();
  }

  async respond(action) {
    const st = this.state(this.dock.now());
    const chat = st.chat;
    if (!chat || chat.status !== "open" || this.busy) return;
    const typed = this.text.value.trim();
    if (action === "reply" && !typed) { toast("Write a reply first."); return; }
    const args = { negotiation_id: chat.nid, action, message: typed || DEFAULT_LINES[action] || DEFAULT_LINES.reject };
    if (action === "counter") {
      if (builder.empty(this.draft)) { toast("Add items to the deal builder first."); return; }
      args.give = this.draft.give; args.receive = this.draft.receive;
    }
    const res = await this.run("respond_negotiation", args);
    if (!res) return;
    this.sent(action === "counter");
    this.dock.read(chat.nid);
  }

  async openChat() {
    if (this.busy) return;
    const typed = this.text.value.trim();
    const hasDeal = !builder.empty(this.draft);
    if (!typed && !hasDeal) { toast("Write an opening message, or add items to the deal."); return; }
    const args = { to: this.civ, message: typed || DEFAULT_LINES.open };
    if (hasDeal) { args.give = this.draft.give; args.receive = this.draft.receive; }
    const res = await this.run("open_negotiation", args);
    if (!res) return;
    this.sent(hasDeal);
    if (Number.isInteger(res.negotiation_id)) this.dock.read(res.negotiation_id);
  }

  async run(tool, args) {
    this.busy = true;
    this.updateButtons();
    try {
      return await this.dock.game.tool(tool, args);
    } finally {
      this.busy = false;
      this.updateButtons();
    }
  }

  /** What was sent leaves the draft: the text, and the deal when it went with it. */
  sent(withDeal) {
    this.draft.text = "";
    this.text.value = "";
    if (withDeal) { builder.clear(this.draft); this.drawDeal(); }
    this.updateButtons();
  }

  // ------------------------------------------------------------------ the deal builder

  drawDeal() {
    const d = this.draft;
    const n = d.give.length + d.receive.length;
    this.dealToggle.textContent = `${d.builderOpen ? "▾" : "▸"} Deal builder${n ? ` (${n} item${n === 1 ? "" : "s"})` : ""}`;
    this.dealBody.style.display = d.builderOpen ? "" : "none";
    if (d.builderOpen) this.wantOptions(); else this._optionsKey = null;   // opened again: asked again
    this.pin();
    const name = this.dock.player(this.civ).name;
    const nameOf = (pid) => this.dock.player(pid).name;
    const side = (node, title, key) => {
      clear(node).appendChild(el("div", { class: "muted" }, title));
      d[key].forEach((it, i) => node.appendChild(el("div", { class: "deal-item" }, describeItem(it, nameOf),
        el("button", { class: "small", title: "Take it out", onclick: () => { builder.remove(d, key, i); this.drawDeal(); this.updateButtons(); } }, "✕"))));
    };
    side(this.giveList, "You give", "give");
    side(this.receiveList, `${name} gives`, "receive");
  }

  add(side, item) {
    if (builder.add(this.draft, side, item)) { this.draft.builderOpen = true; this.drawDeal(); this.updateButtons(); }
  }

  dealDuration() {
    const g = this.dock.game;
    const speeds = (g.rules && g.rules.speeds) || {};
    const speed = g.view && g.view.config && speeds[g.view.config.speed];
    return speed && speed.dealDuration ? speed.dealDuration : 30;
  }

  /** The trade options, fetched while the builder is open (the view no longer carries them: design/phase3-features.md
   * P3.5.8): when it opens, and again after each entry in a chat with this civilization and at each turn while it is
   * open, the moments what either side could give moves. One fetch at a time, and once more if asked meanwhile. */
  wantOptions() {
    if (!this.draft.builderOpen || !this.dock.fetchOptions) return;
    const entries = this.dock.model.chatsWith(this.civ).map((c) => `${c.nid}:${c.entries.length}`).join(",");
    const key = `${this.dock.turnKey()}|${entries}`;
    if (key === this._optionsKey) return;
    this._optionsKey = key;
    this.loadOptions();
  }

  loadOptions() {
    if (this._optionsBusy) { this._optionsBusy = "again"; return; }
    this._optionsBusy = true;
    Promise.resolve().then(() => this.dock.fetchOptions(this.civ)).then((r) => {
      this.options = (r && r.trade_options) || null;
    }).catch((e) => {
      // not met, or gone: nothing to suggest (a blip keeps what was there)
      if (e && e.status === 404) this.options = null;
    }).finally(() => {
      const again = this._optionsBusy === "again";
      this._optionsBusy = false;
      this.drawHints();
      if (again) this.loadOptions();
    });
  }

  /** What each side could give, from the trade options fetched (redrawn only when they change). */
  drawHints() {
    const to = this.options;
    const p = this.dock.player(this.civ);
    const key = to ? JSON.stringify(to) : "";
    if (key === this._hintsKey) return;
    this._hintsKey = key;
    const box = clear(this.hints);
    if (!to) return;
    const dd = this.dealDuration();
    const chip = (label, side, item) => el("button", { class: "small chip", title: "Add to the deal", onclick: () => this.add(side, item) }, label);
    const offers = (opts, side) => [
      ...(opts.luxuries || []).map((r) => chip(`+ ${r}`, side, { type: "resource", resource: r, amount: 1, turns: dd })),
      ...Object.entries(opts.strategic || {}).map(([r, k]) => chip(`+ ${r} (${k} spare)`, side, { type: "resource", resource: r, amount: 1, turns: dd })),
      ...(opts.techs || []).map((tid) => chip(`+ ${tid}`, side, { type: "tech", tech: tid })),
    ];
    const theirs = offers(to.they_could_give || {}, "receive");
    const yours = offers(to.you_could_give || {}, "give");
    const agreements = (to.agreements_possible_now || []).map((t) => chip(`⇄ ${t.replace(/_/g, " ")}`, "give", { type: t }));
    box.append(
      el("div", {}, el("span", { class: "muted" }, `${p.name} could give${to.their_gold != null ? ` (${to.their_gold} gold)` : ""}: `),
        theirs.length ? theirs : el("span", { class: "muted" }, "nothing you lack")),
      el("div", {}, el("span", { class: "muted" }, "You could give: "), yours.length ? yours : el("span", { class: "muted" }, "nothing they lack")),
      agreements.length ? el("div", {}, el("span", { class: "muted" }, "Agreements possible now: "), agreements,
        to.research_agreement_cost_each ? el("span", { class: "muted" }, ` (a research agreement costs each side ${to.research_agreement_cost_each} gold)`) : null) : null);
  }

  drawDealNote() {
    const v = this.dock.game.view;
    const emp = v && v.empire;
    const text = `Mutual agreements (${MUTUAL.join(", ").replace(/_/g, " ")}) apply to both sides.` + (emp
      ? ` You have ${emp.gold} gold (${signed((emp.per_turn || {}).gold || 0)}/turn); spare: ${Object.entries(emp.strategic_resources || {}).filter(([, r]) => r.available > 0).map(([k, r]) => `${k} ${r.available}`).join(", ") || "no strategic resources"}; luxuries: ${Object.entries(emp.luxuries || {}).filter(([, r]) => r.net > 0).map(([k, r]) => `${k} ×${r.net}`).join(", ") || "none"}.`
      : "");
    if (text !== this._noteKey) { this._noteKey = text; this.dealNote.textContent = text; }
  }

  /** The item adder: a kind and its fields, and which side it goes to. Built once, so a half-filled item survives. */
  buildAdder() {
    const g = this.dock.game;
    const R = g.rules || { resources: {}, techs: {} };
    const type = el("select", { class: "chat-deal-type" }, ...ITEM_TYPES.map(([v, t]) => el("option", { value: v }, t)));
    const params = el("span", { class: "row" });
    let inputs = {};
    const dd = () => this.dealDuration();
    const renderParams = () => {
      clear(params);
      inputs = {};
      const t = type.value;
      const num = (k, v, ph) => (inputs[k] = el("input", { type: "number", value: v, placeholder: ph, style: { width: "72px" } }));
      if (t === "gold") params.append(num("amount", 50, "amount"));
      if (t === "gold_per_turn") params.append(num("amount", 3, "per turn"), num("turns", dd(), "turns"));
      if (t === "resource") {
        inputs.resource = el("select", {}, ...Object.entries(R.resources || {}).filter(([, r]) => r.resourceType !== "Bonus")
          .map(([k, r]) => el("option", { value: k }, `${k} (${r.resourceType})`)));
        params.append(inputs.resource, num("amount", 1, "qty"), num("turns", dd(), "turns"));
      }
      if (t === "open_borders") params.append(num("turns", dd(), "turns"));
      if (t === "declare_war") {
        const v = g.view;
        const others = ((v && v.diplomacy && v.diplomacy.players) || []).filter((q) => q.id !== this.civ);
        inputs.target = el("select", {}, ...others.map((q) => el("option", { value: q.id }, q.name)));
        params.append(inputs.target);
      }
      if (t === "city") {
        const v = g.view;
        const cities = ((v && v.cities) || []).filter((c) => c.owner === g.you || c.owner === this.civ);
        inputs.city = el("select", {}, ...cities.map((c) => el("option", { value: c.id }, `${c.name}${c.owner === g.you ? " (yours)" : ""}`)));
        params.append(inputs.city);
      }
      if (t === "tech") { inputs.tech = el("select", {}, ...Object.keys(R.techs || {}).map((k) => el("option", { value: k }, k))); params.append(inputs.tech); }
    };
    type.addEventListener("change", renderParams);
    renderParams();
    const build = () => {
      const t = type.value;
      const it = { type: t };
      if (inputs.amount && ["gold", "gold_per_turn", "resource"].includes(t)) it.amount = +inputs.amount.value;
      if (inputs.turns && ["gold_per_turn", "resource", "open_borders"].includes(t)) it.turns = +inputs.turns.value;
      if (t === "resource") it.resource = inputs.resource.value;
      if (t === "declare_war") it.target = +inputs.target.value;
      if (t === "city") it.city_id = +inputs.city.value;
      if (t === "tech") it.tech = inputs.tech.value;
      return it;
    };
    const name = this.dock.player(this.civ).name;
    return el("div", { class: "row chat-deal-adder" }, type, params,
      el("button", { class: "small", onclick: () => this.add("give", build()) }, "+ You give"),
      el("button", { class: "small", onclick: () => this.add("receive", build()) }, `+ ${name} gives`));
  }
}

function signed(n) {
  const v = Math.round((Number(n) || 0) * 10) / 10;
  return (v >= 0 ? "+" : "") + v;
}

export { NARROW_WIDTH };

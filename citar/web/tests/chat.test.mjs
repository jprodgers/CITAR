// The docked chat windows (citar/web/js/chat.js; design/phase3-features.md P3.5.6) over a few stand-ins for the DOM (the
// repository has no DOM library): a window opens from the socket, what is typed and built in it survives every kind of
// update (the same text box, the same builder, the entries appended in order), End Turn's states drive the turn, and
// the narrow layout shows one window with a tab for each. Run with `node --test "citar/web/tests/*.test.mjs"`.
import { test } from "node:test";
import assert from "node:assert/strict";

// ------------------------------------------------------------------ the stand-ins

class FakeNode {
  constructor() { this.parentNode = null; this.childNodes = []; }
  appendChild(child) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.childNodes.push(child);
    return child;
  }
  insertBefore(child, ref) {
    if (!ref) return this.appendChild(child);
    if (child.parentNode) child.parentNode.removeChild(child);
    const at = this.childNodes.indexOf(ref);
    if (at < 0) throw new Error("insertBefore: not a child");
    child.parentNode = this;
    this.childNodes.splice(at, 0, child);
    return child;
  }
  removeChild(child) { this.childNodes.splice(this.childNodes.indexOf(child), 1); child.parentNode = null; return child; }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  get firstChild() { return this.childNodes[0] || null; }
}

class FakeText extends FakeNode {
  constructor(text) { super(); this.data = String(text); }
  get textContent() { return this.data; }
  set textContent(v) { this.data = String(v); }
}

let focused = null;

class FakeElement extends FakeNode {
  constructor(tag) {
    super();
    this.tagName = tag.toUpperCase();
    this.className = ""; this.style = {}; this.attributes = {}; this.listeners = [];
    this.disabled = false; this.placeholder = ""; this.title = ""; this.rows = 0; this.type = ""; this.role = "";
    this.scrollTop = 0; this.scrollHeight = 0; this.clientHeight = 0; this.clientWidth = 0;
    this._value = undefined; this._rect = null;
  }
  get value() {
    if (this._value === undefined && this.tagName === "SELECT") {
      const o = this.children.find((c) => c.tagName === "OPTION");
      return o ? o.value : "";
    }
    return this._value === undefined ? "" : this._value;
  }
  set value(v) { this._value = String(v); }
  append(...nodes) { for (const n of nodes) this.appendChild(n instanceof FakeNode ? n : new FakeText(n)); }
  setAttribute(k, v) { this.attributes[k] = String(v); }
  getAttribute(k) { return k in this.attributes ? this.attributes[k] : null; }
  addEventListener(type, fn) { this.listeners.push({ type, fn }); }
  removeEventListener(type, fn) { this.listeners = this.listeners.filter((l) => !(l.type === type && l.fn === fn)); }
  dispatch(type, init = {}) {
    const event = { type, target: this, defaultPrevented: false, ...init,
                    preventDefault() { this.defaultPrevented = true; }, stopPropagation() {} };
    for (const l of [...this.listeners]) if (l.type === type) l.fn(event);
    return event;
  }
  click(init) { return this.dispatch("click", init); }
  focus() { focused = this; }
  get children() { return this.childNodes.filter((n) => n instanceof FakeElement); }
  get textContent() { return this.childNodes.map((n) => n.textContent).join(""); }
  set textContent(v) { this.childNodes = []; this.appendChild(new FakeText(v)); }
  getBoundingClientRect() { return this._rect || { left: 0, top: 0, width: 0, height: 0 }; }
  /** Every element under it (itself included) that `test` accepts. */
  findAll(test) {
    const out = [];
    const walk = (n) => { if (n instanceof FakeElement) { if (test(n)) out.push(n); n.childNodes.forEach(walk); } };
    walk(this);
    return out;
  }
  find(test) { return this.findAll(test)[0] || null; }
}

const toasts = new FakeElement("div");
globalThis.Node = FakeNode;
globalThis.Element = FakeElement;
globalThis.document = {
  createElement: (tag) => new FakeElement(tag),
  createTextNode: (text) => new FakeText(text),
  getElementById: (id) => (id === "toasts" ? toasts : null),
};

// imported after the stand-ins exist: util.js patches Element.prototype.append when it loads
const { ChatDock } = await import("../js/chat.js");

// ------------------------------------------------------------------ a game, a server and a clock

const YOU = 0;
const flush = () => new Promise((resolve) => setImmediate(resolve));
const byText = (text) => (n) => n.tagName === "BUTTON" && n.textContent.includes(text);

function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The chats as the server keeps them. `view` is a chat as the view's `diplomacy` block lists it (no clock: the real
 * view has none), `route` as the chat route answers it, with its `clock` (`prefix`: answer with only the first n
 * entries, as a read that left before the last entry would). */
class Server {
  constructor() { this.chats = new Map(); this.reads = []; this.prefix = null; this.max = 30; }
  chat(nid, withCiv, name) { const c = { nid, with: withCiv, name, status: "open", history: [], proposal: null, byYou: null }; this.chats.set(nid, c); return c; }
  say(nid, you, action = "reply", message = "", extra = {}) {
    const c = this.chats.get(nid);
    c.history.push({ seq: c.history.length + 1, by: you ? "Greece" : c.name, you, action, message: message || `${action} ${c.history.length + 1}`, proposal: null, ...extra });
    if (action === "accept") c.status = "accepted";
    if (action === "reject") c.status = "rejected";
    return c.history.length;
  }
  view(nid, n = null) {
    const c = this.chats.get(nid);
    if (!c) throw Object.assign(new Error("No such negotiation."), { status: 404 });
    const history = c.history.slice(0, n == null ? c.history.length : n);
    const last = history[history.length - 1];
    const status = history.length === c.history.length ? c.status : "open";
    return { id: nid, with: c.with, with_name: c.name, status, you_initiated: !!(history[0] && history[0].you),
             your_move: status === "open" && !!last && !last.you, turn: 4, messages: history.length, max_messages: this.max,
             current_proposal: c.proposal, proposal_by_you: c.byYou, history };
  }
  route(nid, n = null) {
    const v = this.view(nid, n);
    const last = v.history[v.history.length - 1];
    return { ...v, clock: { waiting_on: v.status !== "open" ? null : last && !last.you ? YOU : v.with, kind: "model", left_s: 300, paused: false } };
  }
  /** The view's chats as they stand: the open ones and the rest. */
  diplomacy() {
    const all = [...this.chats.keys()].map((nid) => this.view(nid));
    return { open_negotiations: all.filter((v) => v.status === "open"), recent_negotiations: all.filter((v) => v.status !== "open") };
  }
}

/** The turn's other decisions, as GameScreen.turnBlockers reads them from the view. */
function blockersOf(view) {
  return (view.alerts || []).filter((a) => ["research", "idle_city"].includes(a.type));
}

function makeGame({ myTurn = true, width = 1366, height = 860, boxWidth = 300, obstacles = [] } = {}) {
  const server = new Server();
  const state = { now: 0, ended: [], blocked: 0, clicks: [], syncs: 0, viewFails: 0, nextView: null, tools: [], topbars: 0, button: null };
  const host = new FakeElement("div");
  host.clientWidth = width; host.clientHeight = height;
  host._rect = { left: 0, top: 44, width, height };
  const turnBox = new FakeElement("div");
  turnBox._rect = { left: width - 10 - boxWidth, top: 44 + height - 10 - 50, width: boxWidth, height: 50 };
  const game = {
    you: YOU, myTurn,
    rules: { resources: { Iron: { resourceType: "Strategic" }, Wheat: { resourceType: "Bonus" } }, techs: { Writing: {} }, speeds: {} },
    view: {
      turn: 4, current_player: YOU, you: YOU, spectator: false, config: { speed: "Standard" }, cities: [], alerts: [],
      empire: { gold: 120, per_turn: { gold: 4 }, strategic_resources: { Iron: { available: 2 } }, luxuries: {} },
      players: [{ id: 0, name: "Greece", color: "#36f" }, { id: 1, name: "Rome", color: "#c33" }, { id: 2, name: "Persia", color: "#cc3" }],
      diplomacy: { players: [{ id: 1, name: "Rome", trade_options: { their_gold: 80, they_could_give: { luxuries: ["Silk"] }, you_could_give: { strategic: { Iron: 2 } }, agreements_possible_now: ["open_borders"] } },
                             { id: 2, name: "Persia", trade_options: { their_gold: 10 } }],
                   open_negotiations: [], recent_negotiations: [], messages: [] },
    },
    async tool(name, args) {
      state.tools.push([name, JSON.parse(JSON.stringify(args))]);
      if (name === "respond_negotiation") { server.say(args.negotiation_id, true, args.action, args.message); return { ok: true }; }
      if (name === "open_negotiation") {
        const nid = 100 + server.chats.size;
        server.chat(nid, args.to, args.to === 1 ? "Rome" : "Persia");
        server.say(nid, true, "open", args.message);
        return { negotiation_id: nid };
      }
      return {};
    },
    // as GameScreen.syncView: a view fetched now (the server's chats as they stand, and what `nextView` adds), or,
    // when the fetch fails (`viewFails`), the view in hand stays
    async syncView() {
      state.syncs++;
      if (state.viewFails > 0) { state.viewFails--; return; }
      const extra = state.nextView ? state.nextView() : {};
      game.view = { ...game.view, diplomacy: { ...game.view.diplomacy, ...server.diplomacy() }, ...extra };
      dock.onView(game.view);
    },
    // as GameScreen.endTurn: the turn's other decisions stop a plain end
    async endTurn(force) {
      if (!game.myTurn) return;
      if (blockersOf(game.view).length && !force) { state.blocked++; return; }
      state.ended.push(force);
    },
    renderTopbar() {
      state.topbars++;
      // the game screen's own End Turn when no chat holds the turn
      state.button = dock.endTurnButton((force) => onEndTurn(force)) || plainEndTurn();
    },
  };
  function plainEndTurn() {
    const b = new FakeElement("button");
    b.setAttribute("data-chat", "none");
    b.append("End Turn");
    b.addEventListener("click", (e) => onEndTurn(!!(e.ctrlKey || e.metaKey)));
    return b;
  }
  // as GameScreen.onEndTurn: a thin caller of the dock's decision
  function onEndTurn(force) {
    if (!game.myTurn) return;
    const blockers = blockersOf(game.view);
    const next = dock.endTurnClick(force, blockers);
    state.clicks.push(next);
    if (next === "blocked") { state.blocked++; return; }
    if (next === "end") return game.endTurn(force);
  }
  const dock = new ChatDock(game, { host, turnBox, obstacles, now: () => state.now, timers: {},
                                    fetchChat: async (nid) => { server.reads.push(nid); return server.route(nid, state.prefix ? state.prefix(nid) : null); } });
  return { game, dock, server, state, host };
}

function logSeqs(win) {
  return win.log.children.map((n) => n.getAttribute("data-seq"));
}

// ------------------------------------------------------------------ the tests

test("a chat opened by the other side pops up from the socket, and keeps what is typed and built through 100 updates", async () => {
  const { game, dock, server, state } = makeGame();
  dock.onView(game.view);
  server.chat(5, 1, "Rome");
  server.chats.get(5).proposal = { you_give: [{ type: "gold", amount: 30 }], you_receive: [{ type: "resource", resource: "Silk", amount: 1, turns: 30 }], summary: "You give 30 gold; you receive Silk." };
  server.chats.get(5).byYou = false;
  server.say(5, false, "open", "Silk for gold?");
  dock.onEvent({ type: "negotiation", players: [1, 0], data: { negotiation: 5, awaiting: 0 } });
  await flush();
  const win = dock.windows.get(1);
  assert.ok(win, "the window opened from the event, without a view refresh");
  assert.ok(win.root.parentNode === dock.root);
  assert.match(win.status.textContent, /^Waiting for you, 5:00 left$/);
  assert.match(win.proposal.textContent, /You give 30 gold; you receive Silk\./);
  assert.deepEqual(logSeqs(win), ["5:0", "5:1"]);

  // half a message, the builder open, an item from a hint and one from the adder
  win.text.value = "Half a thought about ir";
  win.text.dispatch("input");
  win.dealToggle.click();
  win.hints.find(byText("+ Silk")).click();
  const adderType = win.adder.find((n) => n.tagName === "SELECT");
  adderType.value = "gold";
  adderType.dispatch("change");
  win.adder.find(byText("+ You give")).click();
  const text = win.text, adder = win.adder, give = win.giveList;
  const draft = JSON.stringify(dock.model.draft(1));
  assert.equal(dock.model.draft(1).text, "Half a thought about ir");
  assert.deepEqual(dock.model.draft(1).give, [{ type: "gold", amount: 50 }]);
  assert.deepEqual(dock.model.draft(1).receive, [{ type: "resource", resource: "Silk", amount: 1, turns: 30 }]);
  assert.equal(win.dealBody.style.display, "");

  const rand = rng(1212);
  server.chat(6, 2, "Persia");
  server.say(6, true, "open", "Hello, Persia");
  const kinds = new Set();
  for (let i = 0; i < 100; i++) {
    state.now += 400;
    const kind = ["view", "status", "event", "tick", "hints", "persia"][Math.floor(rand() * 6)];
    kinds.add(kind);
    if (kind === "view") {
      // a refresh, sometimes fetched before the latest entry
      const n = server.chats.get(5).history.length;
      game.view.diplomacy.open_negotiations = [server.view(5, Math.max(1, n - Math.floor(rand() * 2))), server.view(6)];
      dock.onView(game.view);
    } else if (kind === "status") {
      const n = server.chats.get(5).history.length;
      dock.onStatus({ type: "negotiation_status", negotiation: 5, players: [0, 1], waiting_on: 0, kind: "person",
                      state: ["waiting", "queued", "thinking"][Math.floor(rand() * 3)], position: 1, eta_s: 5,
                      deadline: 1, left_s: 300 - i, paused: rand() < 0.2, entries: n - Math.floor(rand() * 2) });
    } else if (kind === "event") {
      // a read that answers out of order: only part of the history
      state.prefix = (nid) => (nid === 5 ? Math.max(1, server.chats.get(5).history.length - Math.floor(rand() * 2)) : null);
      dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 5, awaiting: 0 } });
      await flush();
      state.prefix = null;
    } else if (kind === "tick") {
      dock.tick();
    } else if (kind === "hints") {
      game.view.diplomacy.players[0].trade_options.their_gold = 80 + i;
      dock.onView(game.view);
    } else {
      // the other window's chat moves: Persia answers, or Greece does
      const c = server.chats.get(6);
      if (c.status === "open") server.say(6, c.history.length % 2 === 0, "reply");
      dock.onEvent({ type: "negotiation", players: [0, 2], data: { negotiation: 6 } });
      await flush();
    }
    const now = dock.windows.get(1);
    assert.equal(now, win, `update ${i} (${kind}): the same window`);
    assert.equal(now.text, text, `update ${i} (${kind}): the same text box`);
    assert.equal(now.text.value, "Half a thought about ir", `update ${i} (${kind})`);
    assert.equal(now.adder, adder);
    assert.equal(now.giveList, give);
    assert.equal(adderType.value, "gold", "the half-filled item in the adder");
    assert.equal(JSON.stringify(dock.model.draft(1)), draft, `update ${i} (${kind}): the draft`);
    assert.equal(win.dealBody.style.display, "", "the builder stays open");
    const seqs = logSeqs(win);
    assert.equal(new Set(seqs).size, seqs.length, "no entry drawn twice");
  }
  assert.ok(kinds.size === 6, `every kind of update ran: ${[...kinds]}`);
  assert.equal(give.children.length, 2, "the heading and the one item");
  assert.match(give.textContent, /50 gold/);
  assert.match(win.receiveList.textContent, /1 Silk × 30t/);
  // the other window, too, drew each entry once, in order
  const persia = dock.windows.get(2);
  const order = logSeqs(persia).map((k) => +k.split(":")[1]);
  assert.deepEqual(order, [...order].sort((a, b) => a - b));
  assert.equal(order.length, server.chats.get(6).history.length + 1);
});

test("entries that arrive out of order are put in their places", async () => {
  const { game, dock, server, state } = makeGame();
  dock.onView(game.view);
  server.chat(9, 1, "Rome");
  for (let i = 0; i < 5; i++) server.say(9, i % 2 === 1, i ? "reply" : "open");
  state.prefix = () => 2;
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 9 } });
  await flush();
  state.prefix = null;
  const win = dock.windows.get(1);
  assert.deepEqual(logSeqs(win), ["9:0", "9:1", "9:2"]);
  // a later chat with the same civilization, then the rest of the first
  server.chats.get(9).status = "rejected";
  server.chat(12, 1, "Rome");
  server.say(12, false, "open", "Again?");
  game.view.diplomacy.open_negotiations = [server.view(12)];
  game.view.diplomacy.recent_negotiations = [];
  dock.onView(game.view);
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 9 } });
  await flush();
  assert.deepEqual(logSeqs(win), ["9:0", "9:1", "9:2", "9:3", "9:4", "9:5", "12:0", "12:1"]);
  assert.equal(win.log.children[0].textContent, "Chat #9 · turn 4");
});

test("the log follows its newest entry, unless the reader scrolled up to read back", async () => {
  const { game, dock, server } = makeGame();
  dock.onView(game.view);
  server.chat(2, 1, "Rome");
  server.say(2, false, "open", "Hello");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 2 } });
  await flush();
  const log = dock.windows.get(1).log;
  log.scrollHeight = 400; log.clientHeight = 100;
  server.say(2, true, "reply", "Hi");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 2 } });
  await flush();
  assert.equal(log.scrollTop, 400, "at the newest");
  // the reader scrolls up: new entries leave the place they are reading
  log.scrollTop = 50;
  log.dispatch("scroll");
  log.scrollHeight = 500;
  server.say(2, false, "reply", "And?");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 2 } });
  await flush();
  assert.equal(log.scrollTop, 50);
  // back at the bottom, it follows again (and after the window changes size: the builder opened)
  log.scrollTop = 400;
  log.dispatch("scroll");
  log.scrollHeight = 650;
  dock.windows.get(1).dealToggle.click();
  assert.equal(log.scrollTop, 650);
});

test("your move: Enter sends a reply, Counter sends the builder's deal, and what was sent leaves the draft", async () => {
  const { game, dock, server, state } = makeGame();
  dock.onView(game.view);
  server.chat(3, 1, "Rome");
  server.say(3, false, "open", "Talk?");
  dock.onStatus({ type: "negotiation_status", negotiation: 3, players: [0, 1], waiting_on: 0, kind: "person", state: "waiting",
                  position: null, eta_s: null, deadline: 1, left_s: 290, paused: false, entries: 1 });
  await flush();
  const win = dock.windows.get(1);
  assert.equal(win.text.disabled, false);
  win.text.value = "Yes, let us talk.";
  win.text.dispatch("input");
  win.text.dispatch("keydown", { key: "Enter", shiftKey: false });
  await flush(); await flush();
  assert.deepEqual(state.tools[0], ["respond_negotiation", { negotiation_id: 3, action: "reply", message: "Yes, let us talk." }]);
  assert.equal(win.text.value, "");
  assert.equal(dock.model.draft(1).text, "");
  // their move now: the box and the answers are off, Withdraw on
  assert.equal(win.text.disabled, true);
  assert.equal(win.btn.withdraw.style.display, "");
  assert.equal(win.btn.send.style.display, "none");
  assert.match(win.status.textContent, /^Waiting for Rome, 5:00 left$/);
  // they answer; you counter with the builder's deal
  server.say(3, false, "reply", "What do you offer?");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 3 } });
  await flush();
  assert.equal(win.btn.counter.disabled, true, "nothing in the builder yet");
  win.dealToggle.click();
  win.adder.find(byText("+ Rome gives")).click();
  assert.equal(win.btn.counter.disabled, false);
  win.btn.counter.click();
  await flush(); await flush();
  assert.deepEqual(state.tools[1], ["respond_negotiation", { negotiation_id: 3, action: "counter", message: "How about this instead?", give: [], receive: [{ type: "gold", amount: 50 }] }]);
  assert.deepEqual(dock.model.draft(1).receive, [], "the deal went with it");
  // a chat with someone new, opened with a deal on your turn
  dock.open(2);
  const p = dock.windows.get(2);
  assert.equal(focused, p.text, "the cursor is in the new window's box");
  assert.equal(p.btn.open.style.display, "");
  p.dealToggle.click();
  p.adder.find(byText("+ You give")).click();
  assert.equal(p.btn.open.textContent, "Propose the deal");
  p.btn.open.click();
  await flush(); await flush();
  assert.equal(state.tools[2][0], "open_negotiation");
  assert.deepEqual(state.tools[2][1], { to: 2, message: "I propose this deal.", give: [{ type: "gold", amount: 50 }], receive: [] });
  assert.ok(server.reads.includes(101), "the new chat is read at once");
});

test("End Turn: waiting with a countdown, armed by a click, ending the turn once when the last chat closes", async () => {
  const { game, dock, server, state } = makeGame();
  dock.onView(game.view);
  server.chat(7, 1, "Rome");
  server.say(7, true, "open", "A gift for you");
  game.view.diplomacy.open_negotiations = [server.view(7)];
  dock.onView(game.view);
  await flush();                  // the view has no clock: the chat is read for it
  game.renderTopbar();
  let btn = state.button;
  assert.equal(btn.getAttribute("data-chat"), "waiting");
  assert.equal(btn.textContent, "Waiting for Rome… 5:00");
  state.now += 12000;
  dock.tick();
  assert.equal(btn.textContent, "Waiting for Rome… 4:48", "the countdown ticks in place");
  btn.click();
  btn = state.button;
  assert.equal(btn.getAttribute("data-chat"), "armed");
  assert.match(btn.textContent, /^End turn when they answer 4:48 ✕$/);
  // cancelled, and armed again
  btn.click();
  assert.equal(state.button.getAttribute("data-chat"), "waiting");
  state.button.click();
  assert.equal(state.button.getAttribute("data-chat"), "armed");
  assert.deepEqual(state.ended, []);
  // they accept: the chat closes and the turn ends, once, as the plain click that armed it would (its other
  // decisions checked again: a deal may bring one)
  server.say(7, false, "accept", "Thank you!");
  dock.onStatus({ type: "negotiation_status", negotiation: 7, players: [0, 1], waiting_on: null, kind: null, state: "closed",
                  status: "accepted", position: null, eta_s: null, deadline: null });
  await flush(); await flush();
  assert.deepEqual(state.ended, [false]);
  assert.equal(state.syncs, 1, "on a view fetched after the close");
  assert.deepEqual(state.clicks, ["arm", "disarm", "arm"]);
  dock.tick();
  dock.onView(game.view);
  await flush();
  assert.deepEqual(state.ended, [false], "only once");
  assert.equal(state.syncs, 1);
  assert.equal(dock.model.armed, null);
});

test("End Turn armed with Ctrl+click ends the turn anyway when the last chat closes", async () => {
  const { game, dock, server, state } = makeGame();
  dock.onView(game.view);
  server.chat(8, 2, "Persia");
  server.say(8, true, "open", "Peace?");
  game.view.diplomacy.open_negotiations = [server.view(8)];
  dock.onView(game.view);
  game.renderTopbar();
  state.button.click({ ctrlKey: true });
  assert.equal(state.button.getAttribute("data-chat"), "armed");
  assert.deepEqual(dock.model.armed.force, true);
  server.say(8, false, "reject", "No.");
  dock.onStatus({ type: "negotiation_status", negotiation: 8, players: [0, 2], waiting_on: null, kind: null, state: "closed",
                  status: "rejected", position: null, eta_s: null, deadline: null });
  await flush(); await flush();
  assert.deepEqual(state.ended, [true], "forced, as it was armed");
});

test("End Turn: a chat waiting on you is answered first; the click opens it", async () => {
  const { game, dock, server, state } = makeGame();
  dock.onView(game.view);
  server.chat(8, 2, "Persia");
  server.say(8, false, "open", "Peace?");
  game.view.diplomacy.open_negotiations = [server.view(8)];
  dock.onView(game.view);
  dock.close(2);
  assert.equal(dock.windows.get(2), undefined);
  game.renderTopbar();
  assert.equal(state.button.textContent, "Answer Persia first");
  state.button.click();
  assert.ok(dock.windows.get(2), "the chat's window is open again");
  assert.equal(focused, dock.windows.get(2).text);
});

test("minimised, a window is a tab that lights up when the other side speaks", async () => {
  const { game, dock, server } = makeGame();
  dock.onView(game.view);
  server.chat(4, 1, "Rome");
  server.say(4, true, "open", "Hi");
  game.view.diplomacy.open_negotiations = [server.view(4)];
  dock.onView(game.view);
  const win = dock.windows.get(1);
  win.head.find(byText("–")).click();
  assert.match(win.root.className, /\btab\b/);
  assert.doesNotMatch(win.root.className, /unread/);
  server.say(4, false, "reply", "Hello!");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 4 } });
  await flush();
  assert.match(win.root.className, /\btab\b/, "it stays minimised");
  assert.match(win.root.className, /unread/);
  win.head.click();
  assert.doesNotMatch(win.root.className, /\btab\b|unread/);
});

test("a window folded for want of room comes up when its tab is clicked, and the other folds instead", async () => {
  // room beside End Turn for one window and a tab
  const { game, dock, server } = makeGame({ width: 1366, boxWidth: 900 });
  dock.onView(game.view);
  server.chat(4, 1, "Rome");
  server.say(4, true, "open", "Hi");
  server.chat(5, 2, "Persia");
  server.say(5, true, "open", "Hello");
  game.view.diplomacy.open_negotiations = [server.view(4), server.view(5)];
  dock.onView(game.view);
  const rome = dock.windows.get(1), persia = dock.windows.get(2);
  assert.deepEqual([rome.tab, persia.tab], [true, false], "the older folds");
  rome.head.click();
  assert.deepEqual([rome.tab, persia.tab], [false, true], "brought up; the other folds in its place");
  assert.ok(dock.plan.items.every((it) => it.left + it.width <= 1366 - 10 - 900 - 8), "still left of End Turn");
  persia.head.click();
  assert.deepEqual([rome.tab, persia.tab], [true, false]);
});

test("at 700 px one window shows, full width above End Turn, with a tab for each", async () => {
  const { game, dock, server } = makeGame({ width: 700, height: 800, boxWidth: 330 });
  dock.onView(game.view);
  for (const [nid, civ, name] of [[21, 1, "Rome"], [22, 2, "Persia"]]) {
    server.chat(nid, civ, name);
    server.say(nid, false, "open", "Hello");
  }
  game.view.diplomacy.open_negotiations = [server.view(21), server.view(22)];
  dock.onView(game.view);
  assert.equal(dock.root.className, "chat-dock narrow");
  const shown = [...dock.windows.values()].filter((w) => w.root.style.display !== "none");
  assert.equal(shown.length, 1);
  assert.equal(dock.root.style.width, "680px");
  assert.equal(dock.root.style.bottom, `${800 - (800 - 10 - 50) + 8}px`, "just above the End Turn box");
  const tabs = dock.strip.findAll((n) => n.tagName === "BUTTON");
  assert.deepEqual(tabs.map((t) => t.textContent), ["Rome", "Persia"]);
  assert.ok(tabs[1].className.includes("unread") && tabs[1].className.includes("yours"));
  tabs[1].click();
  const now = [...dock.windows.entries()].filter(([, w]) => w.root.style.display !== "none").map(([civ]) => civ);
  assert.deepEqual(now, [2]);
  // wide again: both side by side, left of the End Turn box
  game.view.players = game.view.players.slice();
  dock.host.clientWidth = 1366;
  dock.turnBox._rect = { left: 1366 - 10 - 330, top: 44 + 800 - 60, width: 330, height: 50 };
  dock.layout();
  assert.equal(dock.root.className, "chat-dock wide");
  const lefts = [...dock.windows.values()].map((w) => parseInt(w.root.style.left, 10) + parseInt(w.root.style.width, 10));
  assert.ok(lefts.every((right) => right <= 1366 - 10 - 330 - 8));
});

test("a spectator gets no windows", () => {
  const { game, dock } = makeGame();
  dock.onView({ ...game.view, you: null, spectator: true });
  assert.equal(dock.enabled, false);
  assert.equal(dock.root.style.display, "none");
  dock.onStatus({ type: "negotiation_status", negotiation: 1, players: [1, 2], state: "thinking" });
  assert.equal(dock.windows.size, 0);
});

test("armed End Turn ends the turn on a view fetched after the close: a decision the deal brought stops a plain arming, not a Ctrl one", async () => {
  for (const ctrl of [false, true]) {
    const { game, dock, server, state } = makeGame();
    dock.onView(game.view);
    server.chat(7, 1, "Rome");
    server.say(7, true, "open", "Your gold for my technology?");
    game.view.diplomacy.open_negotiations = [server.view(7)];
    dock.onView(game.view);
    await flush();
    game.renderTopbar();
    state.button.click(ctrl ? { ctrlKey: true } : {});
    assert.equal(dock.model.armed.force, ctrl);
    // the technology they give finishes the research: the view after the close asks for a new one, which the view
    // in hand (fetched before the deal) does not
    state.nextView = () => ({ alerts: [{ type: "research", text: "Choose a research." }] });
    server.say(7, false, "accept", "Done.");
    dock.onStatus({ type: "negotiation_status", negotiation: 7, players: [0, 1], waiting_on: null, kind: null, state: "closed",
                    status: "accepted", position: null, eta_s: null, deadline: null, entries: 2 });
    await flush(); await flush();
    assert.equal(state.syncs, 1, "a view fetched after the close");
    if (ctrl) {
      assert.deepEqual(state.ended, [true], "Ctrl armed it: the turn ends anyway");
    } else {
      assert.deepEqual(state.ended, [], "the new decision stops it, as End Turn would");
      assert.equal(state.blocked, 1);
    }
    assert.equal(dock.model.armed, null, "fired once");
    state.now += 5000;
    dock.tick();
    await flush();
    assert.equal(state.syncs, 1, "and only once");
  }
});

test("armed End Turn whose fresh view did not come stays armed and tries again a second later; a chat opened meanwhile holds it", async () => {
  const { game, dock, server, state } = makeGame();
  dock.onView(game.view);
  server.chat(7, 1, "Rome");
  server.say(7, true, "open", "Peace?");
  game.view.diplomacy.open_negotiations = [server.view(7)];
  dock.onView(game.view);
  await flush();
  game.renderTopbar();
  state.button.click();
  state.viewFails = 1;
  server.say(7, false, "reject", "No.");
  dock.onStatus({ type: "negotiation_status", negotiation: 7, players: [0, 1], waiting_on: null, kind: null, state: "closed",
                  status: "rejected", position: null, eta_s: null, deadline: null, entries: 2 });
  await flush(); await flush();
  assert.equal(state.syncs, 1);
  assert.deepEqual(state.ended, []);
  assert.deepEqual(dock.model.armed, { turnKey: "4:0", force: false }, "armed again");
  dock.tick();
  await flush();
  assert.equal(state.syncs, 1, "not at once: a second later");
  // meanwhile the person opened a chat with Persia: the fresh view lists it, so the arming holds for it
  state.nextView = () => {
    if (!server.chats.has(9)) { server.chat(9, 2, "Persia"); server.say(9, true, "open", "And you?"); }
    return {};
  };
  state.now += 1000;
  dock.tick();
  await flush(); await flush();
  assert.equal(state.syncs, 2);
  assert.deepEqual(state.ended, []);
  assert.ok(dock.model.armed, "armed still, for Persia's answer");
  game.renderTopbar();
  assert.equal(state.button.getAttribute("data-chat"), "armed");
  // Persia answers no: the turn ends, on a view fetched then
  state.nextView = null;
  server.say(9, false, "reject", "No.");
  dock.onStatus({ type: "negotiation_status", negotiation: 9, players: [0, 2], waiting_on: null, kind: null, state: "closed",
                  status: "rejected", position: null, eta_s: null, deadline: null, entries: 2 });
  await flush(); await flush();
  assert.equal(state.syncs, 3);
  assert.deepEqual(state.ended, [false]);
});

test("End Turn's click: answer first, cancel, the turn's other decisions, arm, end", async () => {
  const { game, dock, server, state } = makeGame();
  const click = async (ctrl = false) => { game.renderTopbar(); state.button.click(ctrl ? { ctrlKey: true } : {}); await flush(); };
  dock.onView(game.view);
  // nothing holds the turn: a click ends it, unless decisions are left (Ctrl ends it anyway)
  game.view.alerts = [{ type: "research", text: "Choose a research." }];
  await click();
  await click(true);
  assert.deepEqual(state.clicks, ["blocked", "end"]);
  assert.deepEqual(state.ended, [true]);
  // a chat waiting on Rome: a plain click with decisions left goes to them and arms nothing; Ctrl arms anyway
  server.chat(3, 1, "Rome");
  server.say(3, true, "open", "Hello");
  game.view.diplomacy.open_negotiations = [server.view(3)];
  dock.onView(game.view);
  await flush();
  await click();
  assert.equal(dock.model.armed, null, "nothing armed");
  assert.equal(state.blocked, 2);
  await click(true);
  assert.deepEqual(dock.model.armed, { turnKey: "4:0", force: true });
  // armed: a click cancels
  await click();
  assert.equal(dock.model.armed, null);
  // with nothing left to decide, a plain click arms
  game.view.alerts = [];
  await click();
  assert.deepEqual(dock.model.armed, { turnKey: "4:0", force: false });
  // a chat waiting on you: the click opens it, decisions or not
  server.say(3, false, "reply", "Well?");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 3 } });
  await flush();
  dock.close(1);
  game.view.alerts = [{ type: "idle_city", text: "A city has nothing to build." }];
  await click();
  assert.ok(dock.windows.get(1), "its window opened");
  assert.equal(focused, dock.windows.get(1).text);
  assert.deepEqual(state.clicks, ["blocked", "end", "blocked", "arm", "disarm", "arm", "open"]);
  assert.deepEqual(state.ended, [true]);
  // not your turn: nothing
  game.myTurn = false;
  assert.equal(dock.endTurnClick(false, []), "none");
});

test("after a page load an open chat's clock is read once, so its window and End Turn count down", async () => {
  const { game, dock, server, state } = makeGame();
  server.chat(5, 1, "Rome");
  server.say(5, false, "open", "Silk for gold?");
  server.chat(6, 2, "Persia");
  server.say(6, true, "open", "Peace?");
  // the first view (a reload mid-chat): no clock in it, and nothing has changed for a status to say so
  game.view.diplomacy.open_negotiations = [server.view(5), server.view(6)];
  dock.onView(game.view);
  await flush();
  assert.deepEqual([...server.reads].sort(), [5, 6], "each read once");
  const rome = dock.windows.get(1), persia = dock.windows.get(2);
  assert.equal(rome.status.textContent, "Waiting for you, 5:00 left");
  assert.equal(persia.status.textContent, "Waiting for Persia, 5:00 left");
  game.renderTopbar();
  assert.equal(state.button.textContent, "Answer Rome first");
  state.now += 30000;
  dock.tick();
  assert.equal(rome.status.textContent, "Waiting for you, 4:30 left");
  // more views of the same chats ask nothing more
  dock.onView(game.view);
  dock.onView(game.view);
  await flush();
  assert.equal(server.reads.length, 2);
  // a view that shows a chat moving before its status came: read once more, at the new version
  server.say(6, false, "reply", "What do you offer?");
  game.view.diplomacy.open_negotiations = [server.view(5), server.view(6)];
  dock.onView(game.view);
  dock.onView(game.view);
  await flush();
  assert.deepEqual(server.reads.slice(2), [6]);
  assert.equal(persia.status.textContent, "Waiting for you, 5:00 left");
});

test("a window closed with ✕ comes back with the other side's news: the chat closing, or their word", async () => {
  const { game, dock, server } = makeGame();
  // a page load's recent chats are not news: no window for them
  server.chat(2, 2, "Persia");
  server.say(2, true, "open", "Hello");
  server.say(2, false, "reject", "No.");
  game.view.diplomacy.recent_negotiations = [server.view(2)];
  dock.onView(game.view);
  await flush();
  assert.equal(dock.windows.get(2), undefined);
  // a chat to Rome, its window closed while Rome thinks
  server.chat(4, 1, "Rome");
  server.say(4, true, "open", "Iron for silk?");
  game.view.diplomacy.open_negotiations = [server.view(4)];
  dock.onView(game.view);
  await flush();
  dock.close(1);
  dock.onStatus({ type: "negotiation_status", negotiation: 4, players: [0, 1], waiting_on: 1, kind: "model", state: "thinking",
                  position: null, eta_s: null, deadline: 1, left_s: 500, paused: false, entries: 1 });
  assert.equal(dock.windows.get(1), undefined, "their thinking is not news");
  // Rome accepts: the close brings the window back, and the read their answer
  server.say(4, false, "accept", "Agreed, gladly.");
  dock.onStatus({ type: "negotiation_status", negotiation: 4, players: [0, 1], waiting_on: null, kind: null, state: "closed",
                  status: "accepted", position: null, eta_s: null, deadline: null, entries: 2 });
  assert.ok(dock.windows.get(1), "the close brings it back");
  await flush();
  assert.match(dock.windows.get(1).log.textContent, /Agreed, gladly\./);
  assert.match(dock.windows.get(1).status.textContent, /^Chat #4 accepted/);
  // closed again; a reply in a chat waiting on them brings it back when only the read says so
  server.chat(8, 1, "Rome");
  server.say(8, true, "open", "Again?");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 8 } });
  await flush();
  dock.close(1);
  server.say(8, false, "reply", "Perhaps.");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 8 } });
  await flush();
  assert.ok(dock.windows.get(1), "their word brings it back");
  assert.match(dock.windows.get(1).log.textContent, /Perhaps\./);
});

test("at 700 px the tab strip keeps its buttons through redraws, so a tap between press and release lands", async () => {
  const { game, dock, server } = makeGame({ width: 700, height: 800, boxWidth: 330 });
  dock.onView(game.view);
  for (const [nid, civ, name] of [[21, 1, "Rome"], [22, 2, "Persia"]]) {
    server.chat(nid, civ, name);
    server.say(nid, true, "open", "Hello");
  }
  game.view.diplomacy.open_negotiations = [server.view(21), server.view(22)];
  dock.onView(game.view);
  await flush();
  const tabs = dock.strip.children.slice();
  assert.deepEqual(tabs.map((t) => t.textContent), ["Rome", "Persia"]);
  // statuses, views and top bar redraws: the same buttons
  for (let i = 0; i < 5; i++) {
    dock.onStatus({ type: "negotiation_status", negotiation: 22, players: [0, 2], waiting_on: 2, kind: "model", state: i % 2 ? "queued" : "thinking",
                    position: 1, eta_s: 5, deadline: 1, left_s: 300 - i, paused: false, entries: 1 });
    dock.onView(game.view);
    game.renderTopbar();
    dock.layout();
  }
  assert.ok(dock.strip.children.every((b, i) => b === tabs[i]), "no button made again");
  assert.match(tabs[1].className, /\bactive\b/, "the newest chat is the one shown");
  // news for the tab not shown (Rome's): its look changes in place
  server.say(21, false, "reply", "Hello to you");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 21 } });
  await flush();
  assert.equal(dock.strip.children[0], tabs[0]);
  assert.match(tabs[0].className, /\bunread\b/);
  assert.match(tabs[0].className, /\byours\b/);
  assert.equal(tabs[0].title, "Rome waits for your answer");
  // the tap made before those redraws still lands
  tabs[0].click();
  const shown = [...dock.windows.entries()].filter(([, w]) => w.root.style.display !== "none").map(([civ]) => civ);
  assert.deepEqual(shown, [1]);
  assert.match(tabs[0].className, /\bactive\b/);
  assert.doesNotMatch(tabs[0].className, /\bunread\b/);
  assert.doesNotMatch(tabs[1].className, /\bactive\b/);
  // a tab for a new window: the strip is made again
  server.chat(23, 2, "Persia");
  dock.open(1);
  assert.equal(dock.strip.children.length, 2);
  dock.close(1);
  assert.deepEqual(dock.strip.children.map((t) => t.textContent), ["Persia"]);
});

test("a window shows how many messages its chat has had against the cap, warning near it", async () => {
  const { game, dock, server } = makeGame();
  server.max = 5;
  dock.onView(game.view);
  server.chat(3, 1, "Rome");
  server.say(3, false, "open", "Talk?");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 3 } });
  await flush();
  const win = dock.windows.get(1);
  assert.equal(win.count.textContent, "1/5 messages (4 left)");
  assert.equal(win.count.className.trim(), "chat-count muted");
  server.say(3, true, "reply", "Yes");
  server.say(3, false, "reply", "Then?");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 3 } });
  await flush();
  assert.equal(win.count.textContent, "3/5 messages (2 left)");
  assert.equal(win.count.className.trim(), "chat-count warn");
  server.say(3, true, "reply", "Gold?");
  server.say(3, false, "reply", "No.");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 3 } });
  await flush();
  assert.equal(win.count.textContent, "5/5 messages: the next closes the chat");
  server.say(3, true, "reject", "Goodbye.");
  dock.onEvent({ type: "negotiation", players: [0, 1], data: { negotiation: 3 } });
  await flush();
  assert.equal(win.count.style.display, "none", "a closed chat has no count");
});

test("the windows keep clear of the unit and city panels shown, and come back beside End Turn when they go", async () => {
  const unit = new FakeElement("div"), city = new FakeElement("div"), side = new FakeElement("div");
  const W = 1280, H = 740;
  // the game screen's panels (style.css): the unit panel at the bottom left, the city panel from the top down to just
  // above End Turn, the side panel at the top right; a hidden one is display: none
  unit._rect = { left: 10, top: 44 + H - 10 - 400, width: 330, height: 400 };
  city._rect = { left: W - 10 - 380, top: 44 + 10, width: 380, height: H - 10 - 64 };
  side._rect = { left: W - 10 - 320, top: 44 + 10, width: 320, height: 300 };
  city.style.display = "none";
  const { game, dock, server } = makeGame({ width: W, height: H, boxWidth: 290, obstacles: [unit, city, side] });
  dock.onView(game.view);
  for (const [nid, civ, name] of [[31, 1, "Rome"], [32, 2, "Persia"]]) {
    server.chat(nid, civ, name);
    server.say(nid, true, "open", "Hello");
  }
  game.view.diplomacy.open_negotiations = [server.view(31), server.view(32)];
  dock.onView(game.view);
  await flush();
  const place = (civ) => {
    const s = dock.windows.get(civ).root.style;
    return { left: parseInt(s.left, 10), right: parseInt(s.left, 10) + parseInt(s.width, 10), maxHeight: parseInt(s.maxHeight, 10) };
  };
  // the unit panel and the side panel shown: the row starts right of the unit panel, and the window under the side
  // panel stops below it
  assert.equal(dock.plan.mode, "wide");
  assert.ok(place(1).right <= W - 10 - 290 - 8 && place(2).left >= 10 + 330 + 8, JSON.stringify([place(1), place(2)]));
  assert.ok(place(1).maxHeight <= H - (10 + 300) - 8 - 10, "under the side panel");
  // a city opened: its panel hides the side panel, and the windows move left of it
  side.style.display = "none";
  city.style.display = "";
  unit.style.display = "none";
  dock.layout();
  assert.ok(place(1).right <= W - 10 - 380 - 8, `left of the city panel: ${JSON.stringify(place(1))}`);
  // closed again: next to End Turn
  city.style.display = "none";
  dock.layout();
  assert.equal(place(1).right, W - 10 - 290 - 8);
});

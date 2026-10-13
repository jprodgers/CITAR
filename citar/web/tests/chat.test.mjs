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

/** The chats as the server keeps them, and what the chat route answers (`prefix`: answer with only the first n
 * entries, as a read that left before the last entry would). */
class Server {
  constructor() { this.chats = new Map(); this.reads = []; this.prefix = null; }
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
             your_move: status === "open" && !!last && !last.you, turn: 4, messages: history.length, max_messages: 30,
             current_proposal: c.proposal, proposal_by_you: c.byYou, history,
             clock: { waiting_on: status !== "open" ? null : last && !last.you ? YOU : c.with, kind: "model", left_s: 300, paused: false } };
  }
}

function makeGame({ myTurn = true, width = 1366, height = 860, boxWidth = 300 } = {}) {
  const server = new Server();
  const state = { now: 0, ended: [], tools: [], topbars: 0, button: null };
  const host = new FakeElement("div");
  host.clientWidth = width; host.clientHeight = height;
  host._rect = { left: 0, top: 44, width, height };
  const turnBox = new FakeElement("div");
  turnBox._rect = { left: width - 10 - boxWidth, top: 44 + height - 10 - 50, width: boxWidth, height: 50 };
  const game = {
    you: YOU, myTurn,
    rules: { resources: { Iron: { resourceType: "Strategic" }, Wheat: { resourceType: "Bonus" } }, techs: { Writing: {} }, speeds: {} },
    view: {
      turn: 4, current_player: YOU, you: YOU, spectator: false, config: { speed: "Standard" }, cities: [],
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
    async endTurn(force) { state.ended.push(force); },
    renderTopbar() { state.topbars++; state.button = dock.endTurnButton((force) => onEndTurn(force)); },
  };
  function onEndTurn(force) {
    const st = dock.endTurnState();
    if (st.state === "answer") return dock.open(st.with);
    if (st.state === "armed") return dock.disarm();
    if (st.state === "waiting") return dock.arm(force);
    return game.endTurn(force);
  }
  const dock = new ChatDock(game, { host, turnBox, now: () => state.now, timers: {},
                                    fetchChat: async (nid) => { server.reads.push(nid); return server.view(nid, state.prefix ? state.prefix(nid) : null); } });
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
  // they accept: the chat closes and the turn ends, once
  server.say(7, false, "accept", "Thank you!");
  dock.onStatus({ type: "negotiation_status", negotiation: 7, players: [0, 1], waiting_on: null, kind: null, state: "closed",
                  status: "accepted", position: null, eta_s: null, deadline: null });
  await flush(); await flush();
  assert.deepEqual(state.ended, [true]);
  dock.tick();
  dock.onView(game.view);
  await flush();
  assert.deepEqual(state.ended, [true], "only once");
  assert.equal(dock.model.armed, null);
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

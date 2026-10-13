// The chat window's state (citar/web/js/chatmodel.js; design/phase3-features.md P3.5.6): entries merged by `seq`, drafts
// and the deal builder untouched by updates, each window's state, End Turn's states and the dock's layout. Run with
// `node --test "citar/web/tests/*.test.mjs"` from the checkout, or `npm test` in citar/web.
import { test } from "node:test";
import assert from "node:assert/strict";

import { ChatModel, builder, describeItem, dockLayout, fmtClock, leftAt, mergeEntries, overlaps, CLOSED_TEXT, NARROW_WIDTH }
  from "../js/chatmodel.js";

const YOU = 0;

/** A seeded random source (mulberry32), so a failing run can be run again. */
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

/** An entry as the engine's `negotiation_view` gives it. `you`: whether the seat said it. */
function entry(seq, you, action = "reply", message = `m${seq}`, extra = {}) {
  return { seq, by: you ? "Greece" : "Rome", you, action, message, proposal: null, ...extra };
}

/** A negotiation view with the history `entries` (each chat alternates: who spoke last waits). */
function view(nid, entries, { status = "open", withCiv = 1, proposal = null, byYou = null, clock = undefined, turn = 3 } = {}) {
  const last = entries[entries.length - 1];
  return {
    id: nid, with: withCiv, with_name: withCiv === 1 ? "Rome" : `Civ ${withCiv}`, status, you_initiated: entries[0] ? entries[0].you : false,
    your_move: status === "open" && !!last && !last.you, turn, messages: entries.length, max_messages: 30,
    current_proposal: proposal, proposal_by_you: proposal ? byYou : null, history: entries, ...(clock ? { clock } : {}),
  };
}

function status(nid, state, { waitingOn = 1, entries, kind = "model", left = 600, paused = false, position = null, eta = null, held, closed } = {}) {
  return {
    type: "negotiation_status", negotiation: nid, players: [YOU, 1], waiting_on: state === "closed" ? null : waitingOn,
    kind: state === "closed" ? null : kind, state, position, eta_s: eta, deadline: left == null ? null : 1e9 + left,
    left_s: state === "closed" ? null : left, paused, ...(entries != null ? { entries } : {}), ...(held ? { held } : {}),
    ...(closed ? { status: closed } : {}),
  };
}

test("fmtClock reads like a countdown", () => {
  assert.equal(fmtClock(252), "4:12");
  assert.equal(fmtClock(7), "0:07");
  assert.equal(fmtClock(0), "0:00");
  assert.equal(fmtClock(-3), "0:00");
  assert.equal(fmtClock(3723), "1:02:03");
  assert.equal(fmtClock(59.2), "1:00", "a part second rounds up: 0:00 means the time is up");
});

test("entries merge by seq: duplicates once, out-of-order arrivals in order, non-entries dropped", () => {
  const a = [entry(1, true), entry(3, false)];
  const b = [entry(2, false), entry(3, false, "reply", "again"), entry(1, true)];
  const merged = mergeEntries(a, b);
  assert.deepEqual(merged.map((e) => e.seq), [1, 2, 3]);
  assert.equal(merged[2].message, "m3", "the first copy of a seq stands (a chat's history never changes)");
  assert.deepEqual(mergeEntries([], [entry(5, true), { seq: "6" }, { seq: 0 }, null, { seq: 2.5 }, entry(4, false)]).map((e) => e.seq), [4, 5]);
  assert.deepEqual(mergeEntries(null, undefined), []);
});

test("out-of-order answers: a stale view adds entries but never takes the chat back", () => {
  const m = new ChatModel(YOU);
  const e1 = entry(1, true, "open"), e2 = entry(2, false), e3 = entry(3, true), e4 = entry(4, false, "accept");
  m.applyNegotiation(view(7, [e1, e2, e3]), 0);
  let c = m.chats.get(7);
  assert.equal(c.waitingOn, 1, "after your entry it waits on them");
  // a view fetched before the third entry arrives late: the side stays, nothing is lost
  m.applyNegotiation(view(7, [e1, e2]), 10);
  assert.equal(c.waitingOn, 1);
  assert.deepEqual(c.entries.map((e) => e.seq), [1, 2, 3]);
  // the close, then a late open view: a closed chat never opens again
  const closed = m.applyNegotiation(view(7, [e1, e2, e3, e4], { status: "accepted" }), 20);
  assert.equal(closed.closed, true);
  m.applyNegotiation(view(7, [e1, e2, e3]), 30);
  c = m.chats.get(7);
  assert.equal(c.status, "accepted");
  assert.equal(c.waitingOn, null);
  assert.deepEqual(c.entries.map((e) => e.seq), [1, 2, 3, 4]);
});

test("a chat closed by its status stays closed when a view of the same length still says open", () => {
  const m = new ChatModel(YOU);
  const h = [entry(1, true, "open"), entry(2, false), entry(3, true)];
  m.applyNegotiation(view(5, h), 0);
  m.applyStatus(status(5, "closed", { closed: "rejected" }), 10);
  // a refresh fetched just before the close: as many entries, still open
  m.applyNegotiation(view(5, h), 20);
  const c = m.chats.get(5);
  assert.equal(c.status, "rejected");
  assert.equal(c.waitingOn, null);
  assert.equal(m.endTurnState(30).state, "clear");
});

test("statuses: a late one is dropped, one ahead of the entries asks for the chat, closed closes", () => {
  const m = new ChatModel(YOU);
  m.applyNegotiation(view(4, [entry(1, true, "open"), entry(2, false), entry(3, true)]), 0);
  // thinking about entry 3: current
  let r = m.applyStatus(status(4, "thinking", { entries: 3 }), 100);
  assert.equal(r.fetch, false);
  assert.equal(m.chats.get(4).live.state, "thinking");
  // a waiting status about entry 2 (on you) arrives late: dropped, the chat still waits on them, still thinking
  r = m.applyStatus(status(4, "waiting", { entries: 2, waitingOn: YOU }), 200);
  assert.equal(m.chats.get(4).waitingOn, 1);
  assert.equal(m.chats.get(4).live.state, "thinking");
  // their answer's status comes before the entry: the side moves to you at once, and the chat is read
  r = m.applyStatus(status(4, "waiting", { entries: 4, waitingOn: YOU, kind: "person", left: 300 }), 300);
  assert.equal(r.fetch, true);
  assert.equal(r.turnedToYou, true);
  assert.equal(m.chats.get(4).live, null);
  // a status for a chat never seen: made, and read
  r = m.applyStatus(status(9, "queued", { entries: 1, position: 2, eta: 30 }), 400);
  assert.equal(r.created, true);
  assert.equal(r.fetch, true);
  assert.equal(m.chats.get(9).with, 1);
  // closed: closed, and read for its last entry
  r = m.applyStatus(status(4, "closed", { closed: "expired" }), 500);
  assert.equal(r.closed, true);
  assert.equal(r.fetch, true);
  assert.equal(m.chats.get(4).status, "expired");
  // nothing after it opens it again
  m.applyStatus(status(4, "waiting", { entries: 9, waitingOn: YOU }), 600);
  assert.equal(m.chats.get(4).status, "expired");
  // a status naming two other civilizations makes no chat of the seat's
  assert.equal(m.applyStatus({ ...status(12, "thinking", { entries: 1 }), players: [1, 2] }, 700), null);
  assert.equal(m.chats.has(12), false);
});

function deepFreeze(x) {
  if (x && typeof x === "object") { Object.values(x).forEach(deepFreeze); Object.freeze(x); }
  return x;
}

test("drafts and the deal builder are intact through 100 random updates", () => {
  const rand = rng(312);
  const m = new ChatModel(YOU);
  const civs = [1, 2, 3];
  // a half-typed message and an open builder with items, in two windows
  const d1 = m.draft(1), d2 = m.draft(2);
  d1.text = "Half a thought about iro";
  builder.add(d1, "give", { type: "gold", amount: 50 });
  builder.add(d1, "receive", { type: "resource", resource: "Iron", amount: 1, turns: 30 });
  d1.builderOpen = true;
  d2.text = "Peace?";
  builder.add(d2, "give", { type: "peace_treaty" });
  const before = JSON.stringify([...m.drafts]);
  // frozen: any write by an update throws (ES modules are strict)
  for (const d of m.drafts.values()) deepFreeze(d);
  const history = { 1: [], 2: [], 3: [] };
  const nids = { 1: 11, 2: 12, 3: 13 };
  const kinds = ["view", "chat", "status", "window"];
  const seen = new Set();
  for (let i = 0; i < 100; i++) {
    const civ = civs[Math.floor(rand() * civs.length)];
    const nid = nids[civ];
    const h = history[civ];
    const kind = kinds[Math.floor(rand() * kinds.length)];
    seen.add(kind);
    if (rand() < 0.5 && h.length < 20) h.push(entry(h.length + 1, h.length % 2 === 0, h.length ? "reply" : "open"));
    // answers arrive out of order: a random prefix of what was said
    const prefix = h.slice(0, Math.floor(rand() * (h.length + 1)));
    if (kind === "view") {
      m.applyView({ open_negotiations: civs.map((c) => view(nids[c], history[c].slice(0, Math.floor(rand() * (history[c].length + 1))), { withCiv: c })),
                    recent_negotiations: [] }, i * 1000);
    } else if (kind === "chat") {
      m.applyNegotiation(view(nid, prefix, { withCiv: civ, clock: { waiting_on: 1, kind: "model", left_s: 500, paused: false } }), i * 1000);
    } else if (kind === "status") {
      const states = ["queued", "thinking", "waiting"];
      m.applyStatus({ ...status(nid, states[Math.floor(rand() * 3)], { entries: prefix.length, position: 1, eta: 9 }), players: [YOU, civ] }, i * 1000);
    } else {
      const ops = [() => m.openWindow(civ), () => m.minimise(civ), () => m.restore(civ), () => m.notify(civ), () => m.closeWindow(civ)];
      ops[Math.floor(rand() * ops.length)]();
    }
    m.windowState(civ, i * 1000, { myTurn: rand() < 0.5 });
    m.endTurnState(i * 1000);
    assert.equal(m.draft(1), d1, "the same draft object");
    assert.equal(JSON.stringify([...m.drafts].filter(([k]) => k === 1 || k === 2)), before, `after update ${i} (${kind})`);
  }
  assert.deepEqual([...seen].sort(), [...kinds].sort(), "every kind of update ran");
  assert.equal(d1.text, "Half a thought about iro");
  assert.equal(d1.builderOpen, true);
  assert.deepEqual(d1.give, [{ type: "gold", amount: 50 }]);
  assert.deepEqual(d1.receive, [{ type: "resource", resource: "Iron", amount: 1, turns: 30 }]);
  assert.deepEqual(d2.give, [{ type: "peace_treaty" }]);
});

test("the builder's own edits: add, take out, load what is on the table, clear", () => {
  const d = new ChatModel(YOU).draft(5);
  assert.equal(builder.empty(d), true);
  assert.equal(builder.add(d, "give", { type: "gold", amount: 10 }), true);
  assert.equal(builder.add(d, "sideways", { type: "gold" }), false);
  assert.equal(builder.add(d, "give", { amount: 3 }), false, "an item names its kind");
  assert.equal(builder.add(d, "give", { type: "" }), false);
  const item = { type: "tech", tech: "Writing" };
  builder.add(d, "receive", item);
  item.tech = "changed";
  assert.equal(d.receive[0].tech, "Writing", "the builder keeps a copy");
  assert.equal(builder.remove(d, "give", 3), false);
  assert.equal(builder.remove(d, "give", 0), true);
  assert.deepEqual(d.give, []);
  builder.load(d, { you_give: [{ type: "open_borders", turns: 30 }], you_receive: [{ type: "gold", amount: 60 }] });
  assert.deepEqual(d.give, [{ type: "open_borders", turns: 30 }]);
  assert.deepEqual(d.receive, [{ type: "gold", amount: 60 }]);
  builder.clear(d);
  assert.equal(builder.empty(d), true);
  assert.equal(describeItem({ type: "gold_per_turn", amount: 3, turns: 30 }), "3 gold/turn × 30");
  assert.equal(describeItem({ type: "declare_war", target: 4 }, (p) => (p === 4 ? "Persia" : "?")), "War on Persia");
  assert.equal(describeItem({ type: "research_agreement" }), "research agreement");
});

test("window state: your move, their move, closed, and no chat yet", () => {
  const m = new ChatModel(YOU);
  // no chat yet: written on your own turn only
  let st = m.windowState(1, 0, { myTurn: true, name: "Rome" });
  assert.equal(st.mode, "new");
  assert.equal(st.input, true);
  assert.equal(st.can.open, true);
  st = m.windowState(1, 0, { myTurn: false, name: "Rome" });
  assert.equal(st.input, false);
  assert.equal(st.can.open, false);
  assert.match(st.status.text, /on your turn/);
  // their proposal, your move: send, decline and accept; counter once the builder has something
  m.applyNegotiation(view(3, [entry(1, false, "open", "Gold for iron?")], { proposal: { you_give: [], you_receive: [], summary: "x" }, byYou: false,
                                                                           clock: { waiting_on: YOU, kind: "person", left_s: 192, paused: false } }), 0);
  st = m.windowState(1, 0, { myTurn: false, name: "Rome" });
  assert.equal(st.mode, "your_move");
  assert.equal(st.input, true, "a chat waiting on you is answered on anyone's turn");
  assert.deepEqual(st.can, { send: true, accept: true, reject: true, counter: false, withdraw: false, open: false });
  assert.equal(st.status.text, "Waiting for you, 3:12 left");
  builder.add(m.draft(1), "give", { type: "gold", amount: 5 });
  assert.equal(m.windowState(1, 0, { name: "Rome" }).can.counter, true);
  assert.equal(m.windowState(1, 140000, { name: "Rome" }).status.text, "Waiting for you, 0:52 left");
  assert.equal(m.windowState(1, 140000, { name: "Rome" }).status.tone, "warn", "under a minute");
  // your own proposal on the table: nothing to accept
  m.applyNegotiation(view(3, [entry(1, false, "open"), entry(2, true, "counter"), entry(3, false, "reply")], { proposal: { summary: "y" }, byYou: true }), 1000);
  assert.equal(m.windowState(1, 1000, { name: "Rome" }).can.accept, false);
  // your answer: their move; only withdrawing
  m.applyNegotiation(view(3, [entry(1, false, "open"), entry(2, true, "counter"), entry(3, false, "reply"), entry(4, true, "reply")]), 2000);
  st = m.windowState(1, 2000, { myTurn: true, name: "Rome" });
  assert.equal(st.mode, "their_move");
  assert.equal(st.input, false);
  assert.deepEqual(st.can, { send: false, accept: false, reject: false, counter: false, withdraw: true, open: false });
  assert.equal(st.status.text, "Waiting for Rome…", "no clock heard yet");
  // closed
  m.applyNegotiation(view(3, [entry(1, false, "open"), entry(2, true, "counter"), entry(3, false, "reply"), entry(4, true, "reply"),
                              entry(5, false, "reject", "(no reply in time)")], { status: "rejected" }), 3000);
  st = m.windowState(1, 3000, { myTurn: true, name: "Rome" });
  assert.equal(st.mode, "closed");
  assert.equal(st.can.open, true);
  assert.match(st.status.text, /^Chat #3 declined\./);
  for (const [s, text] of Object.entries(CLOSED_TEXT)) {
    const mm = new ChatModel(YOU);
    mm.applyNegotiation(view(8, [entry(1, true, "open"), { seq: 2, by: null, you: false, action: "close", message: "", note: "(no reply in time)" }], { status: s }), 0);
    assert.equal(mm.windowState(1, 0, { myTurn: false, name: "Rome" }).status.text, `Chat #8 ${text}. You can start a chat on your turn.`);
  }
});

test("window state follows each status of the other side: queued, thinking, waiting, held, paused", () => {
  const m = new ChatModel(YOU);
  m.applyNegotiation(view(6, [entry(1, true, "open")]), 0);
  const at = (msg, now) => { m.applyStatus(msg, now); return m.windowState(1, now, { name: "Rome" }).status.text; };
  assert.equal(at(status(6, "waiting", { entries: 1, left: 600 }), 0), "Waiting for Rome, 10:00 left");
  assert.equal(m.windowState(1, 61500, { name: "Rome" }).status.text, "Waiting for Rome, 8:59 left", "it counts down by itself");
  assert.equal(at(status(6, "queued", { entries: 1, position: 3, eta: 40, left: 500 }), 100000), "Rome: queued #3, about 40 s · 8:20 left");
  assert.equal(at(status(6, "queued", { entries: 1, position: null, eta: null, left: 500 }), 100000), "Rome: queued · 8:20 left");
  assert.equal(at(status(6, "queued", { entries: 1, position: 1, held: "benchmark", left: null }), 100000), "Rome is queued behind a benchmark");
  assert.equal(at(status(6, "thinking", { entries: 1, left: 480 }), 120000), "Rome is thinking… · 8:00 left");
  assert.equal(at(status(6, "waiting", { entries: 1, left: 470, paused: true }), 130000), "Waiting for Rome (the game is paused)");
  assert.equal(leftAt(m.chats.get(6).clock, 999999), 470, "a paused clock stands");
  assert.equal(at(status(6, "waiting", { entries: 1, kind: "bot", left: null }), 140000), "Waiting for Rome…", "a bot is not timed");
  // their answer: the thinking line goes with the move
  m.applyStatus(status(6, "thinking", { entries: 1, left: 400 }), 150000);
  m.applyNegotiation(view(6, [entry(1, true, "open"), entry(2, false, "reply")]), 151000);
  assert.equal(m.chats.get(6).live, null);
  assert.equal(m.windowState(1, 151000, { name: "Rome" }).mode, "your_move");
});

test("End Turn: answer first, waiting with its countdown, armed, clear", () => {
  const names = { 1: "Rome", 2: "Persia", 3: "Egypt" };
  const m = new ChatModel(YOU);
  assert.deepEqual(m.endTurnState(0, { names }), { state: "clear", nid: null, with: null, name: "", leftS: null, more: 0, armed: false, fire: false });
  // two chats waiting on them: the one that may take longest is named, with its time
  m.applyNegotiation(view(1, [entry(1, true, "open")], { withCiv: 1, clock: { waiting_on: 1, kind: "model", left_s: 250, paused: false } }), 0);
  m.applyNegotiation(view(2, [entry(1, true, "open")], { withCiv: 2, clock: { waiting_on: 2, kind: "person", left_s: 120, paused: false } }), 0);
  let st = m.endTurnState(10000, { names });
  assert.equal(st.state, "waiting");
  assert.equal(st.name, "Rome");
  assert.equal(st.more, 1);
  assert.equal(Math.round(st.leftS), 240);
  // a chat waiting on you comes first: answer it
  m.applyNegotiation(view(3, [entry(1, false, "open")], { withCiv: 3 }), 0);
  st = m.endTurnState(10000, { names });
  assert.equal(st.state, "answer");
  assert.equal(st.name, "Egypt");
  assert.equal(st.nid, 3);
  // answered (now it waits on them); armed
  m.applyNegotiation(view(3, [entry(1, false, "open"), entry(2, true, "reject")], { withCiv: 3, status: "rejected" }), 0);
  m.arm("4:0");
  st = m.endTurnState(10000, { names });
  assert.equal(st.state, "armed");
  assert.equal(st.fire, false);
  // one comes back to you: answer first, still armed
  m.applyNegotiation(view(2, [entry(1, true, "open"), entry(2, false, "counter")], { withCiv: 2 }), 0);
  st = m.endTurnState(10000, { names });
  assert.equal(st.state, "answer");
  assert.equal(st.armed, true);
  // everything closes: clear, and armed means end the turn now
  m.applyNegotiation(view(2, [entry(1, true, "open"), entry(2, false, "counter"), entry(3, true, "accept")], { withCiv: 2, status: "accepted" }), 0);
  m.applyStatus(status(1, "closed", { closed: "rejected" }), 0);
  st = m.endTurnState(10000, { names });
  assert.equal(st.state, "clear");
  assert.equal(st.fire, true);
  assert.equal(m.endTurnState(10000, { names, myTurn: false }).fire, false, "never on someone else's turn");
  // another turn disarms
  m.syncTurn("4:0");
  assert.ok(m.armed);
  m.syncTurn("5:0");
  assert.equal(m.armed, null);
  assert.equal(m.endTurnState(10000, { names }).fire, false);
  // an unknown time (a bot, a paused game) counts as the longest wait
  const p = new ChatModel(YOU);
  p.applyNegotiation(view(1, [entry(1, true, "open")], { withCiv: 1, clock: { waiting_on: 1, kind: "model", left_s: 250, paused: false } }), 0);
  p.applyNegotiation(view(2, [entry(1, true, "open")], { withCiv: 2, clock: { waiting_on: 2, kind: "bot", left_s: null, paused: false } }), 0);
  st = p.endTurnState(0, { names });
  assert.equal(st.name, "Persia");
  assert.equal(st.leftS, null);
});

test("windows: open, minimise, restore, close; news marks a minimised one rather than taking the screen", () => {
  const m = new ChatModel(YOU);
  m.openWindow(1);
  m.openWindow(2);
  assert.deepEqual(m.windows.map((w) => w.civ), [1, 2]);
  assert.equal(m.active, 1);
  m.minimise(1);
  assert.equal(m.active, null);
  m.notify(1);
  assert.equal(m.windowOf(1).minimised, true);
  assert.equal(m.windowOf(1).unread, true);
  m.restore(1);
  assert.equal(m.windowOf(1).unread, false);
  assert.equal(m.active, 1);
  m.closeWindow(1);
  assert.equal(m.windowOf(1), null);
  assert.equal(m.active, 2);
  m.notify(3, { restore: true });
  assert.deepEqual(m.windows.map((w) => w.civ), [2, 3], "news from a closed window opens it");
  m.openWindow(3, { focus: true });
  assert.equal(m.active, 3);
});

test("a new chat takes the narrow window, unless you are answering another", () => {
  const m = new ChatModel(YOU);
  m.applyNegotiation(view(1, [entry(1, true, "open"), entry(2, false, "accept")], { withCiv: 1, status: "accepted" }), 0);
  m.openWindow(1, { focus: true });
  // a closed chat on screen: the new one pops up in its place
  m.applyNegotiation(view(2, [entry(1, false, "open")], { withCiv: 2 }), 0);
  m.notify(2, { restore: true, focus: true });
  assert.equal(m.active, 2);
  assert.equal(m.windowOf(1).unread, false);
  // answering Persia's (it waits on you): Egypt's new chat waits its turn as a lit tab
  m.applyNegotiation(view(3, [entry(1, false, "open")], { withCiv: 3 }), 0);
  m.notify(3, { restore: true, focus: true });
  assert.equal(m.active, 2);
  assert.equal(m.windowOf(3).unread, true);
  // the same for an existing window
  m.notify(1, { focus: true });
  assert.equal(m.active, 2);
});

function boxRect(it) { return { left: it.left, width: it.width, bottom: it.bottom, height: it.maxHeight }; }

test("the dock never covers the End Turn box, at any size", () => {
  const rand = rng(7);
  let wides = 0, narrows = 0;
  for (let i = 0; i < 500; i++) {
    const width = 320 + Math.floor(rand() * 2300), height = 360 + Math.floor(rand() * 1100);
    const bw = 90 + Math.floor(rand() * Math.min(900, width - 20)), bh = 40 + Math.floor(rand() * 90);
    const turnBox = { left: width - 10 - bw, top: height - 10 - bh, width: bw, height: bh };
    const n = 1 + Math.floor(rand() * 8);
    const windows = Array.from({ length: n }, (_, k) => ({ civ: k + 1, minimised: rand() < 0.25 }));
    const active = windows[Math.floor(rand() * n)].civ;
    const plan = dockLayout({ width, height, turnBox, windows, active });
    const box = { left: turnBox.left, width: turnBox.width, bottom: height - turnBox.top - turnBox.height, height: turnBox.height };
    const where = `case ${i}: ${width}x${height}, box ${bw}x${bh}, ${n} windows`;
    if (plan.mode === "wide") {
      wides++;
      assert.ok(width > NARROW_WIDTH, where);
      assert.equal(plan.items.length, n, `${where}: every window is a window or a tab`);
      for (const it of plan.items) {
        assert.ok(!overlaps(boxRect(it), box), `${where}: ${it.kind} ${it.civ} over the End Turn box`);
        assert.ok(it.left + it.width <= turnBox.left - 8 + 1e-9, `${where}: ${it.kind} ${it.civ} reaches the box`);
        assert.ok(it.left >= 10 - 1e-9, `${where}: off the left edge`);
        assert.ok(it.kind === "tab" || it.width >= 260, `${where}: a window too narrow to use`);
      }
      for (const a of plan.items) for (const b of plan.items) {
        if (a !== b) assert.ok(!overlaps(boxRect(a), boxRect(b)), `${where}: ${a.civ} and ${b.civ} overlap`);
      }
    } else {
      narrows++;
      assert.ok(plan.items.length <= 1, `${where}: one window at a time`);
      const c = plan.container;
      assert.ok(c.bottom >= height - turnBox.top + 8 - 1e-9, `${where}: the narrow dock reaches into the End Turn box`);
      assert.ok(!overlaps({ left: c.left, width: c.width, bottom: c.bottom, height: c.maxHeight }, box), where);
      assert.deepEqual(c.tabs, windows.map((w) => w.civ), `${where}: a tab per window`);
      if (plan.items.length) assert.equal(plan.items[0].civ, c.active);
    }
  }
  assert.ok(wides > 100 && narrows > 50, `both layouts were tried (${wides} wide, ${narrows} narrow)`);
});

test("at 700 px one window shows, full width above End Turn, with a tab for each", () => {
  const width = 700, height = 800;
  const turnBox = { left: 700 - 10 - 330, top: 800 - 10 - 90, width: 330, height: 90 };
  const windows = [{ civ: 1, minimised: false }, { civ: 2, minimised: false }, { civ: 3, minimised: true }];
  const plan = dockLayout({ width, height, turnBox, windows, active: 2 });
  assert.equal(plan.mode, "narrow");
  assert.deepEqual(plan.items.map((it) => [it.civ, it.kind, it.left, it.width]), [[2, "window", 10, 680]]);
  assert.equal(plan.container.bottom, 800 - 700 + 8, "just above the End Turn box");
  assert.deepEqual(plan.container.tabs, [1, 2, 3]);
  assert.equal(plan.container.active, 2);
  // the shown one minimised: only the tabs
  const none = dockLayout({ width, height, turnBox, windows: [{ civ: 1, minimised: true }], active: 1 });
  assert.deepEqual(none.items, []);
  assert.deepEqual(none.container.tabs, [1]);
  // one pixel wider, with room: side by side again
  assert.equal(dockLayout({ width: 701, height, turnBox: { ...turnBox, left: 701 - 10 - 330 }, windows, active: 2 }).mode, "wide");
});

test("wide: from right to left in the order they opened; the oldest become tabs when they do not fit", () => {
  const width = 1366, height = 860;
  const turnBox = { left: 1366 - 10 - 300, top: 860 - 10 - 50, width: 300, height: 50 };
  const windows = [1, 2, 3, 4].map((civ) => ({ civ, minimised: false }));
  const plan = dockLayout({ width, height, turnBox, windows });
  assert.equal(plan.mode, "wide");
  const open = plan.items.filter((it) => it.kind === "window");
  assert.deepEqual(open.map((it) => it.civ), [2, 3, 4], "the newest three fit, a little narrower than they like");
  assert.ok(open.every((it) => it.width >= 260 && it.width < 340));
  assert.ok(open[0].left > open[1].left && open[1].left > open[2].left, "the first-opened one sits next to End Turn");
  assert.equal(open[0].left + open[0].width, turnBox.left - 8);
  assert.deepEqual(plan.overflow, [1]);
  assert.deepEqual(plan.items.filter((it) => it.kind === "tab").map((it) => it.civ), [1]);
  // with room for two, two
  const narrowBox = { ...turnBox, left: 1366 - 10 - 600, width: 600 };
  const two2 = dockLayout({ width, height, turnBox: narrowBox, windows });
  assert.deepEqual(two2.items.filter((it) => it.kind === "window").map((it) => it.civ), [3, 4]);
  assert.deepEqual(two2.overflow, [1, 2]);
  // a folded tab brought up (it is the active window) comes up, and the next oldest folds in its place
  const up = dockLayout({ width, height, turnBox, windows, active: 1 });
  assert.deepEqual(up.items.filter((it) => it.kind === "window").map((it) => it.civ), [1, 3, 4]);
  assert.deepEqual(up.overflow, [2]);
  assert.ok(up.items.every((it) => it.left + it.width <= turnBox.left - 8));
  const up2 = dockLayout({ width, height, turnBox: narrowBox, windows, active: 2 });
  assert.deepEqual(up2.items.filter((it) => it.kind === "window").map((it) => it.civ), [2, 4]);
  assert.deepEqual(up2.overflow, [1, 3]);
  // a minimised window is a tab whatever room there is
  const two = dockLayout({ width, height, turnBox, windows: [{ civ: 1, minimised: true }, { civ: 2, minimised: false }] });
  assert.deepEqual(two.items.map((it) => [it.civ, it.kind]), [[2, "window"], [1, "tab"]]);
  assert.deepEqual(two.overflow, []);
});

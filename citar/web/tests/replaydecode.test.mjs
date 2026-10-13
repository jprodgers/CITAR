// The recap's decoding (citar/web/js/replaydecode.js; design/phase3-features.md P3.5.8): every frame of a recorded
// game's `Delta` replay decodes to exactly the `Full` frame the engine gave for it before that format was retired
// (fixtures/replay-delta.json.gz, recorded by fixtures/record_replay_delta.py), in order and at random; frames of
// another ruleset are renumbered by name; a record that does not decode is left out until the next keyframe, as the
// engine leaves it out; and the chats read as threads. Run with `node --test "citar/web/tests/*.test.mjs"`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { gunzipSync } from "node:zlib";

import { ReplayFrames, FrameError, b64decode, b64encode, chatThreads, copyFrame, decodeRecord, describeTerms, fullShape, named,
  plainMessages } from "../js/replaydecode.js";

const FIXTURE = JSON.parse(gunzipSync(readFileSync(new URL("./fixtures/replay-delta.json.gz", import.meta.url))).toString("utf-8"));

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

// ------------------------------------------------------------------------------------------------ the recorded game

test("the fixture is a 100-round game with a reload: keyframes at rounds 1, 65 and 70, deltas between", () => {
  assert.equal(FIXTURE.delta.length, 100);
  assert.equal(FIXTURE.full.length, FIXTURE.delta.length);
  assert.deepEqual(FIXTURE.delta.filter((r) => r.keyframe).map((r) => r.turn), [1, 65, 70]);
  assert.deepEqual(FIXTURE.recorded.keyframes, [1, 65, 70]);
  assert.equal(FIXTURE.recorded.reload_at, 70);
});

test("every frame of the Delta replay decodes to exactly its Full frame, in order", () => {
  const frames = new ReplayFrames(FIXTURE.delta);
  assert.equal(frames.length, FIXTURE.full.length);
  assert.equal(frames.dropped, 0);
  for (let i = 0; i < frames.length; i++) {
    const f = frames.at(i);
    assert.equal(frames.turn(i), FIXTURE.full[i].turn);
    assert.deepEqual(fullShape(f, FIXTURE), FIXTURE.full[i], `the frame of turn ${FIXTURE.full[i].turn}`);
  }
  // the units name types the replay's palette has; the explored sets belong to the three majors
  const last = frames.at(frames.length - 1);
  assert.deepEqual([...last.explored.keys()], [0, 1, 2]);
  assert.ok(last.units.every(([t]) => FIXTURE.unit_ids.includes(last.palette.unit[t])));
});

test("frames read at random, backwards and across the reload, are the same frames", () => {
  const frames = new ReplayFrames(FIXTURE.delta);
  const r = rng(1313);
  const order = [99, 0, 64, 63, 65, 69, 70, 71, 50, 49, 98, 1, 2];
  for (let k = 0; k < 200; k++) order.push(Math.floor(r() * frames.length));
  for (const i of order) {
    assert.deepEqual(fullShape(frames.at(i), FIXTURE), FIXTURE.full[i], `frame ${i}`);
    assert.equal(frames.at(i), frames.at(i), "asked again: the frame in hand");
  }
  // a jump decodes from the nearest keyframe (at most 64 records), never on from a frame before it
  frames.at(0);
  frames.at(99);
  assert.equal(frames.steps, 99 - 69 + 1, "from the keyframe of round 70");
  frames.at(60);
  assert.equal(frames.steps, 61, "from round 1's keyframe");
  frames.at(62);
  assert.equal(frames.steps, 2, "on from the frame in hand");
  assert.equal(frames.indexOfTurn(70), 69);
  assert.equal(frames.indexOfTurn(500), -1);
  assert.equal(frames.at(-1), null);
  assert.equal(frames.at(frames.length), null);
});

test("a copy of a frame stays as it was while the frames move on", () => {
  const frames = new ReplayFrames(FIXTURE.delta);
  const kept = copyFrame(frames.at(10));
  const want = fullShape(kept, FIXTURE);
  frames.at(11); frames.at(40);
  assert.deepEqual(fullShape(kept, FIXTURE), want);
  assert.deepEqual(want, FIXTURE.full[10]);
});

// ------------------------------------------------------------------------------------------------ made-up frames

/** Writes frames as the engine's FrameWriter does (save::journal), for the cases no recorded game holds. */
class Writer {
  constructor() { this.last = null; }
  /** The record of `f` ({turn, keyframe, bytes}); `key` forces a keyframe, `base` names another base turn. */
  push(f, { key = false, base = null } = {}) {
    const out = [];
    const u8 = (v) => out.push(v & 0xff);
    const u16 = (v) => { u8(v); u8(v >>> 8); };
    const u32 = (v) => { u16(v & 0xffff); u16(v >>> 16); };
    const str = (s) => { const b = new TextEncoder().encode(s); u16(b.length); out.push(...b); };
    const varint = (v) => { while (v >= 0x80) { u8((v & 0x7f) | 0x80); v >>>= 7; } u8(v); };
    const isKey = key || !this.last || JSON.stringify(this.last.palette) !== JSON.stringify(f.palette);
    out.push(0x43, 0x46, 1, isKey ? 0x4b : 0x44);
    u32(f.turn);
    if (isKey) {
      u32(f.owner.length);
      for (const list of [f.palette.improvement, f.palette.feature, f.palette.unit]) { u16(list.length); list.forEach(str); }
      for (const layer of [f.owner, f.improvement, f.route, f.feature]) out.push(...layer);
      u8(f.explored.size);
      for (const [p, flags] of f.explored) {
        u8(p);
        const words = Math.ceil(flags.length / 64);
        u32(words);
        for (let w = 0; w < words; w++) {
          let lo = 0, hi = 0;
          for (let b = 0; b < 32; b++) if (flags[w * 64 + b]) lo |= 1 << b;
          for (let b = 0; b < 32; b++) if (flags[w * 64 + 32 + b]) hi |= 1 << b;
          u32(lo >>> 0); u32(hi >>> 0);
        }
      }
    } else {
      const last = this.last;
      u32(base ?? last.turn);
      out.push(0, 0, 0, 0, 0, 0, 0, 0);
      const changed = [];
      for (let i = 0; i < f.owner.length; i++) {
        if (f.owner[i] !== last.owner[i] || f.improvement[i] !== last.improvement[i] || f.route[i] !== last.route[i] || f.feature[i] !== last.feature[i]) changed.push(i);
      }
      u32(changed.length);
      for (const i of changed) { u32(i); out.push(f.owner[i], f.improvement[i], f.route[i], f.feature[i]); }
      u8(f.explored.size);
      for (const [p, flags] of f.explored) {
        u8(p);
        const before = last.explored.get(p);
        const fresh = [];
        for (let i = 0; i < flags.length; i++) if (flags[i] && !before[i]) fresh.push(i);
        u32(fresh.length);
        let prev = -1;
        for (const i of fresh) { varint(prev < 0 ? i : i - prev - 1); prev = i; }
      }
    }
    u32(f.cities.length);
    for (const [id, name, owner, tile, pop, cap] of f.cities) { u32(id); u8(owner); u32(tile); u16(pop); u8(cap ? 1 : 0); str(name); }
    u32(f.units.length);
    for (const [type, owner, tile, hp] of f.units) { u16(type); u8(owner); u8(hp); u32(tile); }
    u32(f.eventRange[0]); u32(f.eventRange[1]);
    this.last = copyFrame(f);
    return { turn: f.turn, keyframe: isKey, bytes: b64encode(Uint8Array.from(out)) };
  }
}

const PALETTE = { improvement: ["Farm", "Mine", "Trading post"], feature: ["Forest", "Jungle", "Marsh"], unit: ["Warrior", "Settler"] };

/** A frame of `n` tiles: tile 0 owned by player 1, two majors, a city and a unit. */
function frame(turn, { n = 300, palette = PALETTE } = {}) {
  const owner = new Uint8Array(n).fill(255);
  owner[0] = 1;
  return {
    turn, keyframe: false, palette, tiles: n, owner, improvement: new Uint8Array(n), route: new Uint8Array(n), feature: new Uint8Array(n),
    explored: new Map([[0, new Uint8Array(n)], [1, new Uint8Array(n)]]),
    cities: [[1, "Roma", 1, 0, 3, true]], units: [[0, 1, 0, 100]], eventRange: [turn * 10, turn * 10 + 9],
  };
}

/** Five rounds: tiles change, land is explored (far apart, so the gaps take several bytes), a route is pillaged. */
function rounds() {
  const out = [];
  let f = frame(1);
  f.explored.get(0)[5] = 1;
  out.push(copyFrame(f));
  for (let t = 2; t <= 5; t++) {
    f = copyFrame(f);
    f.turn = t;
    f.eventRange = [t * 10, t * 10 + 9];
    f.improvement[t] = 1 + (t % 3);
    f.feature[t + 10] = 1 + (t % 3);
    f.route[t + 20] = t === 4 ? 1 + 4 : 2;          // a pillaged road
    f.explored.get(1)[t * 50] = 1;                   // 100, 150, 200, 250: gaps of 49 and more
    f.explored.get(0)[299 - t] = 1;
    f.units = [[t % 2, 1, t, 100 - t], [0, 0, 7, 50]];
    f.cities = [[1, "Roma", 1, 0, 3 + t, true], [2, "Ñuñoa", 0, 9, 1, false]];
    out.push(copyFrame(f));
  }
  return out;
}

const REPLAY = { improvement_ids: PALETTE.improvement, feature_ids: PALETTE.feature };

test("made-up rounds round-trip: changed tiles, explored land, pillaged routes, names in UTF-8", () => {
  const w = new Writer();
  const want = rounds();
  const recs = want.map((f) => w.push(f));
  assert.deepEqual(recs.map((r) => r.keyframe), [true, false, false, false, false]);
  const frames = new ReplayFrames(recs);
  assert.equal(frames.length, 5);
  for (let i = 0; i < 5; i++) {
    const got = frames.at(i);
    assert.deepEqual(fullShape(got, REPLAY), fullShape(want[i], REPLAY), `round ${i + 1}`);
    assert.deepEqual(got.cities, want[i].cities);
  }
  const f = frames.at(3);
  assert.equal(f.route[24], 5, "a road (1) pillaged (+4)");
  assert.equal(named(f.palette.improvement, f.improvement[4]), "Mine");
  assert.equal(named(f.palette.improvement, 0), null);
  assert.equal(f.cities[1][1], "Ñuñoa");
});

test("a frame of another ruleset is renumbered by name, and the units are named by its own palette", () => {
  // recorded when the improvements, features and units were numbered otherwise, one of each since gone
  const then = { improvement: ["Mine", "Farm", "Moai"], feature: ["Marsh", "Forest", "Oasis"], unit: ["Settler", "Warrior", "Chariot"] };
  const f = frame(7, { n: 4, palette: then });
  f.improvement = Uint8Array.from([0, 1, 2, 3]);
  f.feature = Uint8Array.from([2, 1, 3, 2]);
  f.units = [[2, 1, 0, 100], [0, 1, 1, 100]];
  f.explored = new Map([[0, new Uint8Array(4)], [1, new Uint8Array(4)]]);
  const rec = new Writer().push(f);
  const v = fullShape(decodeRecord(rec), REPLAY);
  assert.deepEqual([...b64decode(v.improvement)], [0, 2, 1, 0], "none, Mine, Farm, and Moai as none");
  assert.deepEqual([...b64decode(v.feature)], [1, 3, 0, 1], "Forest, Marsh, Oasis as none, Forest");
  assert.deepEqual(v.units.map((u) => u[0]), ["Chariot", "Settler"]);
  // under the replay's own palette the layers are as recorded
  const same = frame(8, { n: 4 });
  same.improvement = Uint8Array.from([0, 1, 2, 3]);
  same.explored = new Map([[0, new Uint8Array(4)], [1, new Uint8Array(4)]]);
  assert.deepEqual([...b64decode(fullShape(decodeRecord(new Writer().push(same)), REPLAY).improvement)], [0, 1, 2, 3]);
});

test("a record that does not decode is left out, and the deltas after it until the next keyframe", () => {
  const want = rounds();
  const recsOf = () => { const w = new Writer(); return want.map((f, i) => w.push(f, { key: i === 3 })); };
  const recs = recsOf();
  assert.deepEqual(recs.map((r) => r.keyframe), [true, false, false, true, false]);
  const turns = (rs) => { const fr = new ReplayFrames(rs); return [...Array(fr.length).keys()].map((i) => fr.turn(i)); };
  assert.deepEqual(turns(recs), [1, 2, 3, 4, 5]);
  // the second cut short: it and the delta taken from it go
  const cut = recsOf();
  cut[1] = { ...cut[1], bytes: b64encode(b64decode(cut[1].bytes).subarray(0, 20)) };
  const fr = new ReplayFrames(cut);
  assert.deepEqual(turns(cut), [1, 4, 5]);
  assert.equal(fr.dropped, 2);
  assert.deepEqual(fullShape(fr.at(1), REPLAY), fullShape(want[3], REPLAY));
  // the second lost: the third is a delta from a frame the decoder never saw
  const lost = recsOf();
  lost.splice(1, 1);
  assert.deepEqual(turns(lost), [1, 4, 5]);
  // no keyframe after the loss: nothing more decodes
  assert.deepEqual(turns(lost.slice(0, 2)), [1]);
  // a delta that names another base
  const w = new Writer();
  const other = [w.push(want[0]), w.push(want[1], { base: 9 })];
  assert.throws(() => decodeRecord(other[1], decodeRecord(other[0])), FrameError);
  assert.deepEqual(turns(other), [1]);
});

test("records that are not frames are refused, never misread", () => {
  const f = rounds()[0];
  const good = new Writer().push(f);
  const bytes = b64decode(good.bytes);
  const variant = (edit) => { const b = bytes.slice(); edit(b); return { ...good, bytes: b64encode(b) }; };
  const refused = (rec, message, why) => assert.throws(() => decodeRecord(rec), { name: "FrameError", message }, why);
  refused(variant((b) => { b[2] = 2; }), /not a version 1 frame/, "another version");
  refused(variant((b) => { b[3] = 0x58; }), /unknown frame kind 88/, "another kind");
  refused({ ...good, keyframe: false }, /disagree on the kind/, "the record says delta, the bytes keyframe");
  refused({ ...good, turn: 2 }, /disagree on the turn/, "the record and its bytes disagree on the turn");
  refused({ ...good, bytes: b64encode(Uint8Array.from([...bytes, 0])) }, /bytes left after the frame/, "bytes left over");
  refused({ ...good, bytes: b64encode(bytes.subarray(0, bytes.length - 1)) }, /ends early/, "cut short");
  refused({ turn: 2, keyframe: false, bytes: b64encode(Uint8Array.from([0x43, 0x46, 1, 0x44, 2, 0, 0, 0])) }, /before any keyframe/,
    "a delta with nothing before it");
  // a unit type past the palette
  const bad = rounds()[0];
  bad.units = [[7, 1, 0, 100]];
  refused(new Writer().push(bad), /unit type 7 is past its palette/, "a unit type past its palette");
  // an improvement past the palette
  const past = rounds()[0];
  past.improvement[3] = 9;
  refused(new Writer().push(past), /tile 3: improvement 9 is past its palette/, "an improvement past its palette");
  // a capital flag that is neither 0 nor 1: the tail's first city's flag sits after count, id, owner, tile and pop
  const tail = bytes.length - 8 - 4 - 8 * f.units.length - (2 + 4 + 2 + 1 + 4 + 1 + 4) - 4;
  refused(variant((b) => { b[tail + 4 + 4 + 1 + 4 + 2] = 2; }), /capital flag 2/, "a capital flag of 2");
  // a count larger than what is left
  refused(variant((b) => { b[8] = 0xff; b[9] = 0xff; b[10] = 0xff; b[11] = 0x0f; }), /cannot fit the bytes left/, "a tile count past the bytes");
  // what is no record at all: atob's own error would be a DOMException, which would fail the whole replay
  refused({ ...good, bytes: "***" }, /the bytes are not base64/, "bytes that are not base64");
  refused({ ...good, bytes: "QUJD=" }, /the bytes are not base64/, "base64 cut in the middle of its padding");
  refused({ turn: 1, keyframe: true }, /has no bytes/, "a record with no bytes");
  refused({ ...good, bytes: 12 }, /has no bytes/, "bytes that are no text");
  refused(null, /no object/, "no record");
  refused("CF", /no object/, "text for a record");
});

test("a record that is no frame at all is left out, never failing the replay", () => {
  const want = rounds();
  const w = new Writer();
  const recs = want.map((f, i) => w.push(f, { key: i === 3 }));
  // before the second keyframe: bytes that are no base64, then a record with none, then no record
  const bad = [...recs.slice(0, 3), { turn: 3, keyframe: false, bytes: "***" }, { turn: 3, keyframe: true }, null, ...recs.slice(3)];
  const fr = new ReplayFrames(bad);
  assert.equal(fr.dropped, 3);
  assert.deepEqual([...Array(fr.length).keys()].map((i) => fr.turn(i)), [1, 2, 3, 4, 5], "the frames around them all served");
  for (const i of [4, 0, 3, 2]) assert.deepEqual(fullShape(fr.at(i), REPLAY), fullShape(want[i], REPLAY), `frame ${i}`);
  // garbage in the middle of a run of deltas: the deltas after it wait for the next keyframe
  const mid = [recs[0], { turn: 2, keyframe: false, bytes: "%%%" }, ...recs.slice(1)];
  const fm = new ReplayFrames(mid);
  assert.equal(fm.dropped, 3, "the garbage, and the two deltas after it");
  assert.deepEqual([...Array(fm.length).keys()].map((i) => fm.turn(i)), [1, 4, 5]);
});

// ------------------------------------------------------------------------------------------------ chats as threads

const NEGOTIATIONS = [
  { id: 1, initiator: 0, responder: 1, turn: 3, status: "accepted", deal_id: 4, proposal: null, history: [
    { seq: 2, by: 1, action: "accept", message: "Agreed.", proposal: null, turn: 4 },
    { seq: 1, by: 0, action: "open", message: "Silk for gold?", proposal: { 0: [{ type: "resource", resource: "Silk", amount: 1, turns: 30 }], 1: [{ type: "gold", amount: 50 }] }, turn: 3 },
  ] },
  { id: 2, initiator: 1, responder: 0, turn: 4, status: "expired", history: [
    { seq: 1, by: 1, action: "open", message: "Peace?", proposal: null, turn: 4 },
    { seq: 2, by: null, action: "close", message: "", note: "(no reply in time)", proposal: null, turn: 6 },
  ] },
  { id: 3, initiator: 0, responder: 2, turn: 6, status: "open", history: [{ seq: 1, by: 0, action: "open", message: "Hello", proposal: null, turn: 6 }] },
];

test("the chats of a round read as threads, each as it stood at the end of that round", () => {
  assert.deepEqual(chatThreads(NEGOTIATIONS, 2), []);
  const t3 = chatThreads(NEGOTIATIONS, 3);
  assert.deepEqual(t3.map((t) => [t.nid, t.status, t.entries.map((e) => e.seq)]), [[1, "open", [1]]]);
  const t4 = chatThreads(NEGOTIATIONS, 4);
  assert.deepEqual(t4.map((t) => [t.nid, t.status, t.entries.map((e) => e.seq)]), [[1, "accepted", [1, 2]], [2, "open", [1]]],
    "entries in seq order, whatever order they came in; a chat closed later reads open");
  assert.equal(t4[0].dealId, 4);
  const t6 = chatThreads(NEGOTIATIONS, 6);
  assert.deepEqual(t6.map((t) => [t.nid, t.status, t.final, t.entries.length]), [[2, "expired", "expired", 2], [3, "open", "open", 1]]);
  assert.equal(t6[0].entries[1].note, "(no reply in time)");
  assert.deepEqual(chatThreads(null, 3), []);
});

test("a turn's messages leave out the chats' words, which their threads show, and keep the rest", () => {
  // as the engine records them: each chat entry is a message to the other side too
  const messages = [
    { id: 1, turn: 3, from: 0, to: [1], text: "Silk for gold?" },          // chat 1's opening
    { id: 2, turn: 3, from: 0, to: [1, 2], text: "Hello, all." },          // a message to everyone
    { id: 3, turn: 3, from: 0, to: [1], text: "Silk for gold?" },          // the same words, sent again outside the chat
    { id: 4, turn: 4, from: 1, to: [0], text: "Agreed." },                 // chat 1's accept, a turn later
    { id: 5, turn: 4, from: 1, to: [0], text: "Peace?" },                  // chat 2's opening
    { id: 6, turn: 4, from: 2, to: [0], text: "Peace?" },                  // the same words from another
  ];
  assert.deepEqual(plainMessages(messages, NEGOTIATIONS, 3).map((m) => m.id), [2, 3], "one entry hides one message");
  assert.deepEqual(plainMessages(messages, NEGOTIATIONS, 4).map((m) => m.id), [6], "the accept and chat 2's opening");
  assert.deepEqual(plainMessages(messages, [], 4).map((m) => m.id), [4, 5, 6], "no chats: every message of the turn");
  assert.deepEqual(plainMessages(null, NEGOTIATIONS, 4), []);
});

test("a stored proposal reads side by side, each side named", () => {
  const names = { 0: "Greece", 1: "Rome" };
  const text = describeTerms(NEGOTIATIONS[0].history[1].proposal, (it) => (it.type === "gold" ? `${it.amount} gold` : it.resource), (p) => names[p]);
  assert.equal(text, "Greece gives Silk; Rome gives 50 gold");
  assert.equal(describeTerms(null, String, String), "");
  assert.equal(describeTerms({ 1: [] }, String, (p) => names[p]), "Rome gives nothing");
});

// The recap's decoding (design/phase3-features.md P3.5.8): the replay's frames as the engine's journal stores them
// (the `Delta` format: keyframes and deltas), and its chats as threads. Pure, with no DOM and no network, so Node
// tests it (citar/web/tests/replaydecode.test.mjs) against a recorded game's frames in both formats.
//
// A frame is the dynamic map at the end of a round. Its record is binary, little-endian (the engine's
// `save::journal`, which documents the layout; crates/citar-engine/DESIGN.md "Replay data"):
//   - a header: "CF", the version (1), "K" or "D", the turn (i32);
//   - a keyframe: the tile count (u32); the palettes of improvement, feature and unit type names (each a u16 count of
//     u16-length UTF-8 strings); the owner, improvement, route and feature layers (a byte per tile); each major's
//     explored tiles (a u8 count of majors, each its id, a u32 word count and the u64 words of a bit set);
//   - a delta: the frame it was taken from (its turn, i32, and 8 bytes of a hash of it); the changed tiles as 8-byte
//     records (u32 tile, then owner, improvement, route and feature); each major's newly explored tiles (its id, a u32
//     count and LEB128 gaps: the first index, then each index less the previous one, less one);
//   - both, last: the cities (u32 count; id u32, owner u8, tile u32, pop u16, capital u8, a u16-length name), the units
//     at 8 bytes each (u32 count; type u16, owner u8, hp u8, tile u32) and the range of event ids (u32, u32).
// The layers are the engine's: owner 255 for none, an improvement or a top feature other than hills as its palette
// position plus 1 (0 for none), the route level plus 4 when pillaged. A keyframe carries its palette, so a frame
// recorded under another ruleset still names what it shows; a delta keeps its keyframe's.
//
// The server sends only records that decode, in order (a frame after a lost journal chunk is left out until the next
// keyframe), and the 8-byte hash a delta names its base by is the server's to check: this decoder checks the base's
// turn, the lengths and the palette positions, and leaves out what fails as the engine does.
//
// Memory stays at one frame: `ReplayFrames.at(i)` walks from the nearest keyframe (at most 63 deltas) or on from
// where it stands, updating one frame in place, so a gargantuan map's 500 rounds are never all expanded at once.

/** A record that does not decode. */
export class FrameError extends Error {
  constructor(message) { super(message); this.name = "FrameError"; }
}

/** Base64 text as bytes. */
export function b64decode(s) {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** Bytes as base64 text. */
export function b64encode(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

const UTF8 = new TextDecoder("utf-8", { fatal: true });

/** Reads a record's bytes; every read past the end is a FrameError. */
class Reader {
  constructor(bytes) { this.b = bytes; this.at = 0; this.dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength); }
  need(n) {
    if (!(n >= 0) || this.at + n > this.b.length) throw new FrameError(`it ends early, at byte ${this.at}`);
  }
  take(n) { this.need(n); const out = this.b.subarray(this.at, this.at + n); this.at += n; return out; }
  u8() { this.need(1); return this.b[this.at++]; }
  u16() { this.need(2); const v = this.dv.getUint16(this.at, true); this.at += 2; return v; }
  u32() { this.need(4); const v = this.dv.getUint32(this.at, true); this.at += 4; return v; }
  i32() { this.need(4); const v = this.dv.getInt32(this.at, true); this.at += 4; return v; }
  str() {
    const n = this.u16();
    try { return UTF8.decode(this.take(n)); } catch (e) { if (e instanceof FrameError) throw e; throw new FrameError("a name is not UTF-8"); }
  }
  /** LEB128, as the engine reads it: five bytes at most, the bits past 32 dropped. */
  varint() {
    let v = 0;
    for (let shift = 0; shift < 35; shift += 7) {
      const b = this.u8();
      v = (v | ((b & 0x7f) << shift)) >>> 0;
      if (!(b & 0x80)) return v;
    }
    throw new FrameError("a varint is too long");
  }
  /** A count of items at least `each` bytes long, refused when the bytes left cannot hold them. */
  count(each) {
    const n = this.u32();
    if (n * each > this.b.length - this.at) throw new FrameError(`${n} items cannot fit the bytes left`);
    return n;
  }
}

/** The bytes of a record as the replay sends it ({turn, keyframe, bytes: base64}), decoded once and kept. */
function recordBytes(rec) {
  if (!rec._bytes) rec._bytes = rec.bytes instanceof Uint8Array ? rec.bytes : b64decode(rec.bytes || "");
  return rec._bytes;
}

/**
 * Decodes one record. A keyframe needs nothing before it; a delta is applied to `last`, the frame of the record before
 * it, which it updates in place and returns (pass a copy to keep `last`). Throws FrameError for a record that does
 * not decode, or a delta whose base is not `last`.
 *
 * A frame: {turn, keyframe, palette: {improvement, feature, unit}, tiles, owner, improvement, route, feature (each a
 * Uint8Array of a byte per tile), explored: Map(major id -> Uint8Array of 0/1 per tile), cities: [[id, name, owner,
 * tile, pop, capital]], units: [[type index, owner, tile, hp]], eventRange: [first, last]}.
 */
export function decodeRecord(rec, last = null) {
  const r = new Reader(recordBytes(rec));
  const head = r.take(3);
  if (head[0] !== 0x43 || head[1] !== 0x46 || head[2] !== 1) throw new FrameError("not a version 1 frame");
  const kind = r.u8();
  if (kind !== 0x4b && kind !== 0x44) throw new FrameError(`unknown frame kind ${kind}`);
  const key = kind === 0x4b;
  if (rec.keyframe != null && !!rec.keyframe !== key) throw new FrameError("the record and its bytes disagree on the kind");
  const turn = r.i32();
  if (rec.turn != null && rec.turn !== turn) throw new FrameError("the record and its bytes disagree on the turn");
  let f;
  if (key) {
    f = readKey(r);
  } else {
    if (!last) throw new FrameError("a delta before any keyframe");
    const base = r.i32();
    r.take(8);   // the hash of the frame it was taken from: the server checked it
    if (base !== last.turn) {
      throw new FrameError(`a delta taken from the frame of turn ${base}, applied to another (the decoder is at turn ${last.turn})`);
    }
    f = last;
    readDelta(r, f);
  }
  f.turn = turn;
  f.keyframe = key;
  readTail(r, f);
  if (r.at !== r.b.length) throw new FrameError("bytes left after the frame");
  checkPalette(f);
  return f;
}

function readKey(r) {
  const n = r.count(4);
  const palette = { improvement: [], feature: [], unit: [] };
  for (const list of [palette.improvement, palette.feature, palette.unit]) {
    const k = r.u16();
    for (let i = 0; i < k; i++) list.push(r.str());
  }
  const f = { turn: 0, keyframe: true, palette, tiles: n };
  f.owner = r.take(n).slice();
  f.improvement = r.take(n).slice();
  f.route = r.take(n).slice();
  f.feature = r.take(n).slice();
  f.explored = new Map();
  const majors = r.u8();
  for (let m = 0; m < majors; m++) {
    const p = r.u8();
    const words = r.count(8);
    const flags = new Uint8Array(n);
    for (let w = 0; w < words; w++) {
      const lo = r.u32(), hi = r.u32();
      for (const [part, base] of [[lo, w * 64], [hi, w * 64 + 32]]) {
        for (let bit = 0, x = part; x; bit++, x >>>= 1) {
          if (!(x & 1)) continue;
          const t = base + bit;
          if (t < n) flags[t] = 1;   // a bit past the map names no tile (the engine's full frames dropped it too)
        }
      }
    }
    f.explored.set(p, flags);
  }
  return f;
}

function readDelta(r, f) {
  const n = f.tiles;
  const changed = r.count(8);
  for (let i = 0; i < changed; i++) {
    const t = r.u32();
    if (t >= n) throw new FrameError(`tile ${t} is off the frame's map`);
    f.owner[t] = r.u8();
    f.improvement[t] = r.u8();
    f.route[t] = r.u8();
    f.feature[t] = r.u8();
  }
  const majors = r.u8();
  if (majors !== f.explored.size) throw new FrameError("a delta changes the majors");
  for (const [p, flags] of f.explored) {
    if (r.u8() !== p) throw new FrameError("a delta lists the majors in another order");
    const k = r.count(1);
    let prev = -1;
    for (let i = 0; i < k; i++) {
      const gap = r.varint();
      const t = prev < 0 ? gap : prev + gap + 1;
      if (t >= n) throw new FrameError(`explored tile ${t} is off the frame's map`);
      flags[t] = 1;
      prev = t;
    }
  }
}

function readTail(r, f) {
  const nc = r.count(14);
  const cities = [];
  for (let i = 0; i < nc; i++) {
    const id = r.u32(), owner = r.u8(), tile = r.u32(), pop = r.u16(), cap = r.u8();
    if (cap > 1) throw new FrameError(`capital flag ${cap}`);
    cities.push([id, r.str(), owner, tile, pop, cap === 1]);
  }
  const nu = r.count(8);
  const units = [];
  for (let i = 0; i < nu; i++) {
    const type = r.u16(), owner = r.u8(), hp = r.u8(), tile = r.u32();
    units.push([type, owner, tile, hp]);
  }
  f.cities = cities;
  f.units = units;
  f.eventRange = [r.u32(), r.u32()];
}

/** Every improvement, feature and unit type of a frame is in its palette, so a reader can name each one. */
function checkPalette(f) {
  for (const [what, layer, len] of [["improvement", f.improvement, f.palette.improvement.length],
                                    ["feature", f.feature, f.palette.feature.length]]) {
    for (let t = 0; t < layer.length; t++) {
      if (layer[t] > len) throw new FrameError(`tile ${t}: ${what} ${layer[t]} is past its palette`);
    }
  }
  for (const u of f.units) if (u[0] >= f.palette.unit.length) throw new FrameError(`unit type ${u[0]} is past its palette`);
}

/** A frame's own copy, which decoding the next delta into the original leaves alone. */
export function copyFrame(f) {
  return {
    ...f, palette: f.palette, owner: f.owner.slice(), improvement: f.improvement.slice(), route: f.route.slice(),
    feature: f.feature.slice(), explored: new Map([...f.explored].map(([p, x]) => [p, x.slice()])),
    cities: f.cities.map((c) => c.slice()), units: f.units.map((u) => u.slice()), eventRange: f.eventRange.slice(),
  };
}

/** What a palette position (1 up, 0 for none) names. */
export function named(list, b) { return b ? (list[b - 1] ?? null) : null; }

/**
 * The replay's frames, read on demand. `records` are the replay's `frames` ({turn, keyframe, bytes}). One pass at the
 * start finds the records that decode (a delta whose base did not, and the deltas after it until the next keyframe,
 * are left out, as the engine leaves them out); `length` and `turn(i)` count those. `at(i)` is the frame of the i-th,
 * valid until the next `at`: it moves on from where it stands, or starts again at the nearest keyframe.
 */
export class ReplayFrames {
  constructor(records) {
    this.records = [];
    this.dropped = 0;
    let last = null;
    for (const rec of records || []) {
      try {
        last = decodeRecord(rec, rec && rec.keyframe ? null : last);
        this.records.push(rec);
      } catch (e) {
        if (!(e instanceof FrameError)) throw e;
        last = null;   // nothing after it decodes until a keyframe
        this.dropped++;
      }
    }
    this.cur = null;   // the frame in hand
    this.idx = -1;     // which record it is
  }

  get length() { return this.records.length; }

  turn(i) { return this.records[i] ? this.records[i].turn : null; }

  /** The index of the frame of `turn`, or -1. */
  indexOfTurn(turn) { return this.records.findIndex((r) => r.turn === turn); }

  at(i) {
    if (!(i >= 0 && i < this.records.length)) return null;
    if (i === this.idx) return this.cur;
    let from = i;
    while (from > 0 && !this.records[from].keyframe) from--;
    // on from the frame in hand when it is between that keyframe and i
    let k = this.idx >= from && this.idx < i ? this.idx + 1 : from;
    let f = k === from ? null : this.cur;
    for (; k <= i; k++) {
      const rec = this.records[k];
      f = decodeRecord(rec, rec.keyframe ? null : f);
    }
    this.cur = f;
    this.idx = i;
    return f;
  }
}

/**
 * A frame as the retired `Full` format gave it (`victory.record_frame`'s shape): its layers in base64, the improvement
 * and feature layers numbered into the replay's `improvement_ids` and `feature_ids` (the current ruleset's: a name it
 * lacks is 0), each major's explored tiles as a byte per tile, units named by type. What replaydecode.test.mjs compares
 * with the recorded `Full` frames.
 */
export function fullShape(f, replay) {
  // as the engine renumbered: a layer under the replay's own palette as it is; else each name to its first place in
  // the replay's (plus 1, and 0 past 255 or for a name it lacks)
  const renumber = (layer, from, to) => {
    if (from.length === to.length && from.every((n, i) => n === to[i])) return layer;
    const index = new Map();
    to.forEach((n, i) => { if (!index.has(n)) index.set(n, i + 1 <= 255 ? i + 1 : 0); });
    const map = [0, ...from.map((n) => index.get(n) || 0)];
    return layer.map((b) => map[b] ?? 0);
  };
  const explored = {};
  for (const [p, flags] of f.explored) explored[String(p)] = b64encode(flags);
  return {
    turn: f.turn,
    owner: b64encode(f.owner),
    improvement: b64encode(renumber(f.improvement, f.palette.improvement, replay.improvement_ids || [])),
    route: b64encode(f.route),
    feature: b64encode(renumber(f.feature, f.palette.feature, replay.feature_ids || [])),
    cities: f.cities.map((c) => c.slice()),
    units: f.units.map(([type, owner, tile, hp]) => [f.palette.unit[type], owner, tile, hp]),
    explored,
    event_range: f.eventRange.slice(),
  };
}

// ---------------------------------------------------------------------------------------------- chats as threads

/**
 * The chats the recap shows for `turn`: each negotiation with an entry that turn, as it stood at the end of it (its
 * entries up to then, in `seq` order, and its status then: its final one once the entry that closed it is in, else
 * "open"). `negotiations` are the replay's (the engine's stored shape: id, initiator, responder, turn, status,
 * history [{seq, by, action, message, proposal, turn, note}]). Oldest chat first.
 */
export function chatThreads(negotiations, turn) {
  const out = [];
  for (const n of negotiations || []) {
    const history = [...(n.history || [])].filter((e) => e && Number.isInteger(e.seq)).sort((a, b) => a.seq - b.seq);
    if (!history.some((e) => e.turn === turn)) continue;
    const entries = history.filter((e) => e.turn == null || e.turn <= turn);
    const lastAll = history[history.length - 1];
    const closedBy = n.status !== "open" && lastAll && entries.includes(lastAll);
    out.push({
      nid: n.id, initiator: n.initiator, responder: n.responder, opened: n.turn, entries,
      status: closedBy ? n.status : "open", final: n.status, dealId: n.deal_id ?? null,
    });
  }
  return out.sort((a, b) => a.nid - b.nid);
}

/** A proposal in the stored shape ({"<giver id>": [items]}) in words, each side named: "Rome gives 50 gold; Greece
 * gives Iron × 30t". `describe(item)` words one item; `nameOf(pid)` names a civilization. */
export function describeTerms(terms, describe, nameOf) {
  if (!terms || typeof terms !== "object") return "";
  const sides = Object.entries(terms).map(([giver, items]) => {
    const what = (items || []).map(describe).join(", ");
    return `${nameOf(Number(giver))} gives ${what || "nothing"}`;
  });
  return sides.join("; ");
}

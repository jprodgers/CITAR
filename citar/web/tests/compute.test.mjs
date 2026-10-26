// The compute ledger's figures as the pages show them (citar/web/js/compute.js): credits, durations, tokens and the
// ratio, the profile's line and the admin users page's column. Run with `node --test "citar/web/tests/*.test.mjs"` from
// the checkout, or `npm test` in citar/web.
import { test } from "node:test";
import assert from "node:assert/strict";

import { adminCell, credits, duration, kindLabel, ratioLine, ratioText, sideCells, tokens } from "../js/compute.js";

test("credits keep the precision their size deserves", () => {
  assert.equal(credits(0), "0");
  assert.equal(credits(undefined), "0");
  assert.equal(credits(0.0421), "0.042");
  assert.equal(credits(3.5), "3.5");
  assert.equal(credits(12.34), "12.3");
  assert.equal(credits(1234.4), "1,234");
});

test("durations read as people say them", () => {
  assert.equal(duration(0), "0 s");
  assert.equal(duration(2.5), "2.5 s");
  assert.equal(duration(42), "42 s");
  assert.equal(duration(185), "3 min 5 s");
  assert.equal(duration(120), "2 min");
  assert.equal(duration(7800), "2 h 10 min");
  assert.equal(duration(-4), "0 s");
});

test("tokens shorten past a thousand", () => {
  assert.equal(tokens(950), "950");
  assert.equal(tokens(1234), "1.2k");
  assert.equal(tokens(45000), "45k");
  assert.equal(tokens(4100000), "4.1M");
});

test("a ratio is its value, or what it means when nothing was used", () => {
  assert.equal(ratioText({ value: 2.5, text: "2.50" }), "2.50");
  assert.equal(ratioText({ value: null, text: "only given" }), "only given");
  assert.equal(ratioText({ value: null, text: "nothing yet" }), "nothing yet");
  assert.equal(ratioText(null), "—");
});

test("a profile's ratio line names the range", () => {
  const line = ratioLine({ range: "30d", given: { credits: 12 }, used: { credits: 4.8 }, ratio: { value: 2.5, text: "2.50" } });
  assert.equal(line, "Gave 12 credits, used 4.8 (ratio 2.50) — last 30 days.");
  assert.equal(ratioLine({ range: "all", given: { credits: 3 }, used: { credits: 0 }, ratio: { value: null, text: "only given" } }),
    "Gave 3 credits, used 0 (only given) — all time.");
  assert.equal(ratioLine(null), "");
});

test("the admin users page's column and a side's cells", () => {
  assert.equal(adminCell({ given: 12, used: 4.8, ratio: { value: 2.5 } }), "12 given · 4.8 used · 2.50");
  assert.equal(adminCell(undefined), "—");
  assert.deepEqual(sideCells({ credits: 3.5, gpu_s: 185, cpu_s: 2.5, tokens_in: 1234, tokens_out: 950 }),
    ["3.5", "3 min 5 s", "2.5 s", "1.2k / 950"]);
  assert.equal(kindLabel("benchmark"), "Benchmarks");
  assert.equal(kindLabel("other"), "other");
});

// The Lab and Queue pages' words for the CPU jobs (citar/web/js/labrules.js): an executor's state, a job's state and
// verification badge, a playing game's line. Run with `node --test "citar/web/tests/*.test.mjs"` from the checkout.
import { test } from "node:test";
import assert from "node:assert/strict";

import { JOB_STATE, executorText, gameLine, progressText, verificationBadge } from "../js/labrules.js";

test("an executor is described with its state, figures and how its limits hold", () => {
  const e = { kind: "local", on: true, state: "ready", threads: 2, cpu_percent: 16, memory_mb: 400,
              held: { cpu: "hard", memory: "hard" }, available: null };
  assert.equal(executorText(e), "this server: ready, 2 threads, 16% of the CPU, 400 MB (CPU hard, memory hard)");
  assert.equal(executorText({ ...e, threads: 1, held: null }), "this server: ready, 1 thread, 16% of the CPU, 400 MB");
  assert.equal(executorText({ ...e, on: false }), "this server: CPU work is off");
  assert.equal(executorText({ ...e, kind: "remote", name: "Bob's box", on: false, online: false }),
    "Bob's box: its helper is not connected", "a helper whose connection went is not 'off'");
  assert.match(executorText({ ...e, available: "a benchmark holds its machine" }), /; a benchmark holds its machine$/);
  assert.match(executorText({ ...e, state: "failed", reason: "its job host could not start: boom" }),
    /^this server: not running, .*; its job host could not start: boom$/);
  // an idle host starts when there is work: no reason is shown for it
  assert.equal(executorText({ ...e, state: "idle", held: null, reason: "" }),
    "this server: idle (starts when there is work), 2 threads, 16% of the CPU, 400 MB");
  assert.equal(executorText({ kind: "remote", name: "Box", on: true, state: "ready", threads: 4 }), "Box: ready, 4 threads");
  assert.equal(executorText(null), "");
});

test("a job's state shows done once its result is in, and a crash as final", () => {
  assert.deepEqual(JOB_STATE.done, ["good", "✓ done"]);
  assert.deepEqual(JOB_STATE.crashed, ["bad", "crashed (final)"]);
  for (const state of ["queued", "running", "done", "accepted", "settling", "crashed", "failed", "cancelled", "withdrawn"]) {
    assert.ok(JOB_STATE[state], state);
  }
});

test("a finished game's verification badge", () => {
  assert.equal(verificationBadge("reference")[1], "reference");
  assert.equal(verificationBadge("verified")[0], "good");
  assert.equal(verificationBadge("unverified")[0], "warn");
  assert.equal(verificationBadge(undefined), null);
});

test("a playing game's line and an experiment's progress", () => {
  assert.equal(gameLine({ label: "ab #3", turn: 12, limit: 30 }), "ab #3 (round 12/30)");
  assert.equal(gameLine({ label: "someone's lab game", turn: null }), "someone's lab game (starting)");
  assert.equal(gameLine({ label: "x #0", turn: 5, limit: null, paused: true }), "x #0 (round 5, paused)");
  assert.equal(progressText({ done: 3, games: 6, running: 2 }), "3 / 6 (+2 playing)");
  assert.equal(progressText({ done: 6, games: 6, running: 0 }), "6 / 6");
});

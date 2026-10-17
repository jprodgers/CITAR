// The words for a helper machine's updates (citar/web/js/updaterules.js): the latest signed helper, whether it is
// newer, and where the helper is with it. Run with `node --test "citar/web/tests/*.test.mjs"` from the checkout.
import { test } from "node:test";
import assert from "node:assert/strict";

import { STATE_TEXT, updateLine } from "../js/updaterules.js";

const latest = { version: "0.1.7", build: "0b412a18fe95", released_at: "2026-11-01T00:00:00Z" };

test("an older helper is told an update is available, in its own words when it said any", () => {
  assert.deepEqual(updateLine({ latest, available: true, status: null }, "0.1.6"),
    { cls: "warn", text: "Helper 0.1.6 · CITAR helper 0.1.7 is signed and relayed: update available" });
  assert.deepEqual(updateLine({ latest, available: true, status: { state: "available", version: "0.1.7",
    message: "CITAR helper 0.1.7 is available; updates are set to notify on this machine" } }, "0.1.6"),
  { cls: "warn", text: "Helper 0.1.6 · CITAR helper 0.1.7 is signed and relayed: update available (0.1.7): CITAR "
    + "helper 0.1.7 is available; updates are set to notify on this machine" });
  const failed = updateLine({ latest, available: true, status: { state: "failed", version: "0.1.7",
    message: "the update failed: 0.1.7 exited with code 1 before it was healthy: 0.1.6 is back" } }, "0.1.6");
  assert.equal(failed.cls, "bad");
  assert.ok(failed.text.endsWith("update failed (0.1.7): the update failed: 0.1.7 exited with code 1 before it was "
    + "healthy: 0.1.6 is back"), failed.text);
  // a refusal of another version is no word about this one
  assert.equal(updateLine({ latest, available: true, status: { state: "refused", version: "0.1.6", message: "x" } },
    "0.1.6").text, "Helper 0.1.6 · CITAR helper 0.1.7 is signed and relayed: update available");
  // a refusal of this one says why it will not come by itself
  assert.deepEqual(updateLine({ latest, available: true, status: { state: "refused", version: "0.1.7",
    message: "updates are off on this machine" } }, "0.1.6"),
  { cls: "warn", text: "Helper 0.1.6 · CITAR helper 0.1.7 is signed and relayed: update available; update refused "
    + "(0.1.7): updates are off on this machine" });
});

test("a helper that cannot update itself is told to download the newer one", () => {
  // every build of a tree whose update-keys.pub is empty says unavailable, with no version
  assert.deepEqual(updateLine({ latest, available: true, status: { state: "unavailable", version: null,
    message: "this build of the helper trusts no update key" } }, "0.1.6"),
  { cls: "warn", text: "Helper 0.1.6 · CITAR helper 0.1.7 is signed and relayed: update available; this helper "
    + "does not update itself: download it from the Servers page" });
  // up to date, it is up to date whatever it can do
  assert.deepEqual(updateLine({ latest, available: false, status: { state: "unavailable", version: null,
    message: "x" } }, "0.1.7"), { cls: "good", text: "Helper 0.1.7 · up to date" });
});

test("an up-to-date helper says so, and that it was updated", () => {
  assert.deepEqual(updateLine({ latest, available: false, status: null }, "0.1.7"),
    { cls: "good", text: "Helper 0.1.7 · up to date" });
  assert.deepEqual(updateLine({ latest, available: false, status: { state: "applied", version: "0.1.7",
    message: "applied" } }, "0.1.7"), { cls: "good", text: "Helper 0.1.7 · up to date (updated)" });
});

test("without a relayed manifest the helper's own word stands; with nothing said, nothing", () => {
  assert.deepEqual(updateLine({ latest: null, available: false, status: { state: "unavailable", version: null,
    message: "this build trusts no update key" } }, "0.1.6"),
  { cls: "muted", text: "Helper 0.1.6 · this helper does not update itself" });
  assert.equal(updateLine({ latest: null, available: false, status: null }, "0.1.6"), null);
  assert.equal(updateLine(null, "0.1.6"), null);
  assert.deepEqual(updateLine({ latest, available: false, status: null }, "0.1.9"),
    { cls: "muted", text: "Helper 0.1.9 · the latest signed helper here is 0.1.7" });
});

test("every protocol state has its words", () => {
  for (const s of ["available", "downloading", "verified", "restarting", "applied", "failed", "refused",
    "unavailable"]) {
    assert.ok(STATE_TEXT[s] && STATE_TEXT[s][1], s);
  }
});

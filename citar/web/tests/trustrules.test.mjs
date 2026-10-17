// The words for machines' trust and verification (citar/web/js/trustrules.js): a machine's standing, a mismatch's
// evidence, its measured speed, its flags, the verification settings' bounds, an experiment's games by verification.
// Run with `node --test "citar/web/tests/*.test.mjs"` from the checkout.
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  SETTINGS, TIER_TEXT, UNVERIFIED_MACHINE, evidenceText, flagText, settingValue, settingsChanges, speedText, trustLine,
  verificationCounts, waitsForVerifier,
} from "../js/trustrules.js";

test("a machine's trust in a line", () => {
  assert.equal(trustLine({ tier: "new", verified: 0, streak: 0 }), "Trust: new");
  assert.equal(trustLine({ tier: "new", verified: 3, streak: 3 }), "Trust: new · 3 verified · streak 3");
  assert.equal(trustLine({ tier: "trusted", verified: 52, streak: 12 }), "Trust: trusted · 52 verified · streak 12");
  assert.equal(trustLine({ tier: "quarantined", verified: 5, streak: 0, mismatches: 1 }),
    "Trust: quarantined · 5 verified · streak 0 · 1 mismatch");
  assert.equal(trustLine(null), "");
  for (const tier of ["new", "provisional", "trusted", "quarantined"]) assert.ok(TIER_TEXT[tier].startsWith(tier));
});

test("a mismatch's evidence names its verdict, its verifier and what was withdrawn", () => {
  const ev = { verdict: "result", words: "same chain, different result: tampering or an incompatible build",
    against: "this server", withdrawn: 2, withdrawn_jobs: [{}, {}], at: "2026-10-17T10:20:30+00:00" };
  assert.equal(evidenceText(ev), "same chain, different result: tampering or an incompatible build (checked against "
    + "this server); 2 results withdrawn and played again on 2026-10-17 10:20.");
  assert.equal(evidenceText({ verdict: "chain", words: "the chains parted at round 12: faulty hardware or an "
    + "incompatible build", withdrawn_jobs: [{}] }),
  "the chains parted at round 12: faulty hardware or an incompatible build; 1 result withdrawn and played again.");
  assert.equal(evidenceText({ verdict: "administrator" }), "administrator.");
  assert.equal(evidenceText(null), "");
});

test("a machine's measured speed beside its CPU class", () => {
  assert.equal(speedText(0.4, { name: "Default CPU", pinned: false }),
    "Measured speed: 0.40× the reference · CPU class Default CPU");
  assert.equal(speedText(1, { name: "Slow CPU", pinned: true }),
    "Measured speed: 1.00× the reference · CPU class Slow CPU (pinned)");
  assert.match(speedText(null, null), /^Measured speed: not yet/);
});

test("each flag the credits' bounds and the class check set has its words; others none", () => {
  assert.deepEqual(flagText("overreported_cpu", { count: 3 }).slice(0, 2), ["bad", "over-reports its CPU (3×)"]);
  assert.deepEqual(flagText("capped_cpu", { count: 1 }).slice(0, 2), ["warn", "CPU capped at 1.5× the reference (1×)"]);
  const cs = flagText("class_suspect", { speed: 0.4, class_median: 1, class_name: "Default CPU" });
  assert.equal(cs[1], "class suspect");
  assert.match(cs[2], /0\.4.*Default CPU.*1:/);
  assert.equal(flagText("overreports", { count: 2 })[1], "over-reports its time (2×)");
  assert.equal(flagText("cancel_unacked", { count: 2 }), null);
  assert.equal(flagText("settings_overclaim", {}), null);
});

test("the settings hold to their bounds as the server does", () => {
  const keys = SETTINGS.map(([k]) => k).sort();
  assert.deepEqual(keys, ["new_per_owner", "new_streak", "new_verify_share", "overreport_days", "promote_verified",
    "provisional_rate", "trusted_rate"]);
  assert.equal(settingValue("rate", "0.05", [0, 1]), 0.05);
  assert.equal(settingValue("rate", "1", [0, 1]), 1);
  assert.equal(settingValue("rate", "0", [0, 1]), 0);
  assert.equal(settingValue("rate", "1.5", [0, 1]), null);
  assert.equal(settingValue("rate", "-0.01", [0, 1]), null);
  assert.equal(settingValue("share", "abc", [0, 1]), null);
  assert.equal(settingValue("count", "5", [1, 10000]), 5);
  assert.equal(settingValue("count", "10000", [1, 10000]), 10000);
  assert.equal(settingValue("count", "0", [1, 10000]), null);
  assert.equal(settingValue("count", "10001", [1, 10000]), null);
  assert.equal(settingValue("count", "2.5", [1, 10000]), null);
  assert.equal(settingValue("count", "", [1, 10000]), null);
});

test("only the settings that changed are sent", () => {
  const current = { new_streak: 5, trusted_rate: 0.02, provisional_rate: 0.1 };
  assert.equal(settingsChanges(current, { ...current }), null);
  assert.deepEqual(settingsChanges(current, { ...current, trusted_rate: 0.05 }), { trusted_rate: 0.05 });
  assert.deepEqual(settingsChanges(current, { ...current, new_streak: 3, provisional_rate: 0.2 }),
    { new_streak: 3, provisional_rate: 0.2 });
});

test("an experiment's games by verification, and withdrawn ones", () => {
  assert.equal(verificationCounts({ verification: { verified: 3, unverified: 2, reference: 4 }, withdrawn: 1 }),
    "3 verified · 2 unverified · 4 reference · 1 withdrawn");
  assert.equal(verificationCounts({ verification: { reference: 6 } }), "6 reference");
  assert.equal(verificationCounts({ verification: {}, withdrawn: 0 }), "");
  assert.equal(verificationCounts({}), "");
});

test("a wait for a verifier is told apart", () => {
  assert.ok(waitsForVerifier("Bob's box: waiting for a verifier: 1 of its results wait for their replicas"));
  assert.ok(!waitsForVerifier("next in line"));
  assert.ok(!waitsForVerifier(null));
  assert.match(UNVERIFIED_MACHINE, /trusted/);
});

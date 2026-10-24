// The Servers page's pure rules (citar/web/js/machines.js): the old page's addresses, what an address of the page
// opens, and which machine routes a viewer may call. Run with `node --test "citar/web/tests/*.test.mjs"` from the
// checkout (Node 22 and later take a pattern, not a folder), or `npm test` in citar/web.
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  GATES, MODE_TEXT, RESCAN_LOOK_MS, RESCAN_WAIT_MS, SECTIONS, classText, cpuInEffectText, inEffectText, labGrantsText,
  legacyServersTarget, mayCall, poolLink, poolTarget, rescanned, settingsBody, sharesCpu, wholeIn,
} from "../js/machines.js";

test("the old Servers page's addresses lead to the Servers page", () => {
  const cases = [
    ["#/servers", "#/pool"],
    ["#/servers/", "#/pool"],
    ["#servers", "#/pool"],
    ["#/servers?tab=keys", "#/pool?tab=keys"],
    ["#/servers/sv_3f2a9c1d", "#/pool/sv_3f2a9c1d"],
    ["#/servers/sv_3f2a9c1d/", "#/pool/sv_3f2a9c1d"],
    ["#/servers/sv_3f2a9c1d?x=1", "#/pool/sv_3f2a9c1d?x=1"],
    ["#/servers/lmstudio-local", "#/pool/lmstudio-local"],
    ["#/servers/my%20box", "#/pool/my%20box"],
    ["#/servers/sv_1/edit", "#/pool/sv_1"],
    ["#/servers/groups/g_7?k=abc", "#/pool/groups/g_7?k=abc"],
    ["#/servers/groups/g_7", "#/pool/groups/g_7"],
    ["#/servers/groups", "#/pool"],
  ];
  for (const [from, to] of cases) assert.equal(legacyServersTarget(from), to, from);
});

test("any other address is not the old page's", () => {
  for (const hash of ["", "#", "#/", "#/pool", "#/pool/sv_1", "#/serversX", "#/server", "#/reports/servers",
    "#/game/servers/x", "#/queue?servers=1", null, undefined]) {
    assert.equal(legacyServersTarget(hash), null, String(hash));
  }
});

test("an address of the Servers page names the machine or group it opens", () => {
  assert.deepEqual(poolTarget("#/pool"), {});
  assert.deepEqual(poolTarget("#/pool/"), {});
  assert.deepEqual(poolTarget("#/pool/sv_1"), { machine: "sv_1" });
  assert.deepEqual(poolTarget("#/pool/sv_1?k=z"), { machine: "sv_1" });
  assert.deepEqual(poolTarget("#/pool/my%20box"), { machine: "my box" });
  assert.deepEqual(poolTarget("#/pool/%E0%A4%A"), { machine: "%E0%A4%A" });          // not decodable: kept
  assert.deepEqual(poolTarget("#/pool/groups/g_7?k=abc"), { group: "g_7" });
  assert.deepEqual(poolTarget("#/pool/groups"), {});
  assert.deepEqual(poolTarget("#/servers/sv_1"), {});
});

test("an old address opens the same machine on the new page", () => {
  for (const id of ["sv_3f2a9c1d", "lmstudio-local", "my%20box"]) {
    assert.deepEqual(poolTarget(legacyServersTarget(`#/servers/${id}`)), { machine: decodeURIComponent(id) });
  }
  assert.deepEqual(poolTarget(legacyServersTarget("#/servers/groups/g_7?k=a")), { group: "g_7" });
  assert.deepEqual(poolTarget(legacyServersTarget("#/servers")), {});
});

test("a machine's link opens it, whatever its id holds", () => {
  for (const id of ["sv_1", "a b/c", "x?y#z", "groups-not"]) {
    assert.deepEqual(poolTarget(poolLink(id)), { machine: id });
    assert.deepEqual(poolTarget(poolLink(id, { group: true })), { group: id });
  }
});

// ---------------------------------------------------------------------------- who may call what

const MEMBER = { admin: false, register: true };
const NEWCOMER = { admin: false, register: false };   // no register_servers (probation, say)
const ADMIN = { admin: true, register: true };

const MACHINES = {
  ownHelper: { can_manage: true, reach: "worker", provider: "openai_compatible" },
  ownHelperLm: { can_manage: true, reach: "worker", connection: { provider: "lmstudio" } },
  ownDryRun: { can_manage: true, reach: "direct", connection: { provider: "dryrun" } },
  sharedHelper: { can_manage: false, reach: "worker", provider: "lmstudio" },
  sharedSiteDryRun: { can_manage: false, reach: "direct", provider: "dryrun" },
  host: { can_manage: false, reach: "direct", is_host: true, provider: "lmstudio" },
  lanLmStudio: { can_manage: false, reach: "direct", provider: "lmstudio" },
  siteApi: { can_manage: false, reach: "direct", provider: "anthropic" },
};

const named = (gate) => Object.keys(GATES).filter((n) => GATES[n] === gate);

test("every route of the table has a known gate", () => {
  const known = new Set(["member", "view", "register", "save", "manage", "direct", "dialled", "admin", "host"]);
  for (const [name, gate] of Object.entries(GATES)) assert.ok(known.has(gate), `${name}: ${gate}`);
});

test("a member's page calls no administrator's route, for any machine or none", () => {
  for (const viewer of [MEMBER, NEWCOMER]) {
    for (const name of [...named("admin"), ...named("host")]) {
      assert.equal(mayCall(name, viewer, null), false, name);
      for (const [what, machine] of Object.entries(MACHINES)) assert.equal(mayCall(name, viewer, machine), false, `${name} on ${what}`);
    }
  }
});

test("a member dials only their own dry-run machine", () => {
  for (const name of named("dialled")) {
    for (const [what, machine] of Object.entries(MACHINES)) {
      assert.equal(mayCall(name, MEMBER, machine), what === "ownDryRun", `${name} on ${what}`);
    }
    assert.equal(mayCall(name, MEMBER, { isNew: true, reach: "direct", provider: "dryrun" }), false, `${name} unsaved`);
  }
});

test("a member manages their own machines only, and keys only on a machine the server dials", () => {
  for (const name of named("manage")) {
    for (const [what, machine] of Object.entries(MACHINES)) {
      assert.equal(mayCall(name, MEMBER, machine), machine.can_manage, `${name} on ${what}`);
    }
  }
  for (const name of named("direct")) {
    assert.equal(mayCall(name, MEMBER, MACHINES.ownHelper), false, name);
    assert.equal(mayCall(name, MEMBER, MACHINES.ownHelperLm), false, name);
    assert.equal(mayCall(name, MEMBER, MACHINES.ownDryRun), true, name);
    assert.equal(mayCall(name, MEMBER, MACHINES.sharedSiteDryRun), false, name);
  }
});

test("saving: a new machine needs register_servers, an existing one MANAGE", () => {
  assert.equal(mayCall("saveServer", MEMBER, { isNew: true }), true);
  assert.equal(mayCall("saveServer", NEWCOMER, { isNew: true }), false);
  assert.equal(mayCall("saveServer", NEWCOMER, MACHINES.ownHelper), true);
  assert.equal(mayCall("saveServer", MEMBER, MACHINES.sharedHelper), false);
  assert.equal(mayCall("saveServer", ADMIN, MACHINES.sharedHelper), true);
  for (const name of named("register")) {
    assert.equal(mayCall(name, MEMBER), true, name);
    assert.equal(mayCall(name, NEWCOMER), false, name);
    assert.equal(mayCall(name, ADMIN), true, name);
  }
});

test("listing and testing: anyone signed in, a test only on a machine listed to them", () => {
  for (const name of named("member")) {
    assert.equal(mayCall(name, NEWCOMER), true, name);
    assert.equal(mayCall(name, null), false, name);
  }
  assert.equal(mayCall("testPoolServer", MEMBER, MACHINES.sharedHelper), true);
  assert.equal(mayCall("testPoolServer", MEMBER, null), false);
  assert.equal(mayCall("testPoolServer", MEMBER, { isNew: true }), false);
});

test("an administrator may call every route on the host, and the host's own only there", () => {
  for (const name of Object.keys(GATES)) assert.equal(mayCall(name, ADMIN, MACHINES.host), true, name);
  for (const name of named("host")) {
    assert.equal(mayCall(name, ADMIN, MACHINES.lanLmStudio), false, name);
    assert.equal(mayCall(name, ADMIN, null), false, name);
  }
  for (const name of named("dialled")) {
    assert.equal(mayCall(name, ADMIN, MACHINES.lanLmStudio), true, name);
    assert.equal(mayCall(name, ADMIN, MACHINES.siteApi), true, name);
    assert.equal(mayCall(name, ADMIN, MACHINES.sharedHelper), false, `${name}: a helper machine is its helper's`);
  }
  for (const name of named("direct")) assert.equal(mayCall(name, ADMIN, MACHINES.ownHelper), false, name);
});

test("nobody signed in may call anything, and a name not in the table is never allowed", () => {
  for (const name of Object.keys(GATES)) assert.equal(mayCall(name, null, MACHINES.ownDryRun), false, name);
  for (const name of ["deleteServer", "machineRates", "setMachineCosting", "probeModels", "setupScan", "constructor", "toString"]) {
    assert.equal(mayCall(name, ADMIN, MACHINES.host), false, name);
  }
});

// ---------------------------------------------------------------------------- the machine settings dialog (3-18)

test("the machine settings dialog: its owner and administrators; class and trust administrators' alone", () => {
  for (const name of ["machineSettings", "saveMachineSettings", "applyScan"]) {
    assert.equal(GATES[name], "manage", name);
    for (const [what, machine] of Object.entries(MACHINES)) {
      assert.equal(mayCall(name, MEMBER, machine), machine.can_manage, `${name} on ${what}`);
      assert.equal(mayCall(name, ADMIN, machine), true, `${name} on ${what}`);
    }
    assert.equal(mayCall(name, MEMBER, { isNew: true }), false, `${name}: not saved yet`);
  }
  for (const name of ["setMachineTrust", "setMachineClass"]) {
    assert.equal(GATES[name], "admin", name);
    for (const machine of Object.values(MACHINES)) {
      assert.equal(mayCall(name, MEMBER, machine), false, name);
      assert.equal(mayCall(name, ADMIN, machine), true, name);
    }
  }
});

const READ = {
  slots: 1,
  capabilities: { llm: { enabled: true, allow_swaps: false },
                  cpu: { enabled: false, threads: 0, cpu_percent: 0, memory_mb: 0, work: [] } },
  modes: { hardware: "auto", models: "auto", power: "manual" },
};

test("the dialog sends only what changed, or nothing", () => {
  const copy = () => JSON.parse(JSON.stringify(READ));
  assert.equal(settingsBody(READ, copy()), null);
  const e = copy();
  e.slots = 3;
  e.capabilities.llm.allow_swaps = true;
  e.capabilities.cpu.work = ["lab_game"];
  e.modes.hardware = "manual";
  assert.deepEqual(settingsBody(READ, e), {
    slots: 3, capabilities: { llm: { allow_swaps: true }, cpu: { work: ["lab_game"] } }, modes: { hardware: "manual" },
  });
  const back = copy();
  back.capabilities.cpu.work = [];                     // a list equal to the read one is no change
  back.modes.power = "manual";
  assert.equal(settingsBody(READ, back), null);
  const off = copy();
  off.capabilities.llm.enabled = false;
  assert.deepEqual(settingsBody(READ, off), { capabilities: { llm: { enabled: false } } });
});

test("whole numbers within bounds, and nothing else", () => {
  assert.equal(wholeIn("3", 1, 16), 3);
  assert.equal(wholeIn("16", 1, 16), 16);
  for (const bad of ["0", "17", "1.5", "", "two", null, undefined, "-1"]) assert.equal(wholeIn(bad, 1, 16), null, String(bad));
  assert.equal(wholeIn("0", 0, 100), 0);
});

test("CPU sharing is for machines with a CPU of their own", () => {
  assert.equal(sharesCpu("owned"), true);
  assert.equal(sharesCpu("leased"), true);
  assert.equal(sharesCpu("api"), false);
  assert.equal(sharesCpu("test"), false);
});

test("every section has both modes' words", () => {
  assert.deepEqual(SECTIONS, ["hardware", "models", "power"]);
  for (const section of SECTIONS) {
    for (const mode of ["auto", "manual"]) assert.ok(MODE_TEXT[section][mode].length > 20, `${section} ${mode}`);
  }
});

test("what is in effect, in a line", () => {
  assert.match(inEffectText(null), /not read this machine yet/);
  assert.match(inEffectText({ known: true, llm: false }), /no model work/);
  assert.equal(inEffectText({ known: true, llm: true, capacity: 2, slots: 3, helper_slots: 2, allow_swaps: false }),
    "In effect now: 2 requests at once (its helper takes at most 2); pooled prompts may not make it swap models.");
  assert.equal(inEffectText({ known: true, llm: true, capacity: 1, slots: 1, helper_slots: 4, allow_swaps: true }),
    "In effect now: 1 request at once; pooled prompts may make it swap models.");
  assert.match(inEffectText({ known: true, llm: true, capacity: null, slots: 2, helper_slots: null }),
    /no requests until its helper connects/);
});

test("the CPU a helper lends, in a line (4-10)", () => {
  assert.match(cpuInEffectText(null), /No CITAR helper of this version/);
  const base = { plays_jobs: true, lent: true, verifiable: true, limits: { cpu: "hard", memory: "hard" },
                 memory_per_run_mb: 460, capped: [],
                 effective: { enabled: true, threads: 2, cpu_percent: 20, memory_mb: 512, work: ["lab_game"] } };
  assert.equal(cpuInEffectText(base),
    "In effect: 2 threads, 20% of the CPU (hard), 512 MB (hard); 460 MB a run.");
  assert.match(cpuInEffectText({ ...base, lent: false }), /^Not lent by this machine's owner/);
  assert.match(cpuInEffectText({ ...base, plays_jobs: false }), /plays no CPU job/);
  assert.match(cpuInEffectText({ ...base, verifiable: false }), /canary or self-check/);
  assert.match(cpuInEffectText({ ...base, effective: { ...base.effective, enabled: false } }), /no CPU work/);
  assert.equal(cpuInEffectText({ ...base, limits: { cpu: "soft", memory: "hard" }, memory_per_run_mb: null,
                                 capped: ["cpu.threads", "cpu.work"],
                                 effective: { ...base.effective, threads: 1 } }),
    "In effect: 1 thread, 20% of the CPU (soft), 512 MB (hard). Its own ceiling capped threads, the kinds of work.");
});

test("the grants that lend a machine's CPU, in a line", () => {
  assert.match(labGrantsText([]), /No grant on its group lists Lab \(CPU work\)/);
  assert.equal(labGrantsText([{ subject: "bob", group: "Alice's" }, { subject: "everyone", group: "Alice's" },
                              { subject: "bob", group: "Alice's" }]),
    "Lent for lab runs (CPU work) to bob, everyone, by grants on “Alice's”.");
});

test("a class in a line", () => {
  assert.equal(classText(null), "none");
  assert.equal(classText({ name: "GPU (unclassified)", kind: "gpu", rate: 1 }),
    "GPU (unclassified) (1 credits per generation-second)");
  assert.equal(classText({ name: "Online API", kind: "api", rate: 1000 }), "Online API (1000 credits per unit of token cost)");
});

test("Rescan now waits for a report other than the one it had", () => {
  const read = (at) => ({ report: { hardware_at: at } });
  assert.equal(rescanned(null, read(null)), false, "nothing reported yet");
  assert.equal(rescanned(null, read("2026-10-15T10:00:00+00:00")), true, "the first scan");
  assert.equal(rescanned("2026-10-15T10:00:00+00:00", read("2026-10-15T10:00:00+00:00")), false, "the same one");
  assert.equal(rescanned("2026-10-15T10:00:00+00:00", read("2026-10-15T10:00:03+00:00")), true, "a new one");
  assert.equal(rescanned("x", {}), false);
  assert.equal(rescanned("x", null), false);
  // it looks often enough to show a scan the protocol sends within 5 s, and gives up well after it
  assert.ok(RESCAN_LOOK_MS <= 1000 && RESCAN_WAIT_MS >= 10000);
  // and only a machine's manager may ask (the route's gate)
  assert.equal(GATES.rescanMachine, "manage");
  assert.equal(mayCall("rescanMachine", { admin: false }, { can_manage: true }), true);
  assert.equal(mayCall("rescanMachine", { admin: false }, { can_manage: false }), false);
});

// The Servers page's pure rules (citar/web/js/machines.js): the old page's addresses, what an address of the page
// opens, and which machine routes a viewer may call. Run with `node --test citar/web/tests/`.
import { test } from "node:test";
import assert from "node:assert/strict";

import { GATES, legacyServersTarget, mayCall, poolLink, poolTarget } from "../js/machines.js";

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

// The Servers page's rules, apart from the page so Node can test them (nothing here touches the DOM or the network):
// where the page's old addresses lead, what an address of the page opens, and which of the machine routes a viewer may
// call for a machine. The page (pool.js, with the editors of servers.js) asks `mayCall` before it shows a control or
// sends a request, so a member's page never calls a route that would refuse them (design/phase3-features.md P3.2.8,
// 3-06). tests/test_servers_page.py holds GATES to the server's own gates (admin_api.py, pool_api.py) and checks that
// every request of servers.js and pool.js goes through it.

/** The gate of each route the Servers page calls, by its name in api.js:
 *  - `member`: anyone signed in (a listing filtered to what the caller may see);
 *  - `view`: a machine listed to the caller (VIEW: its owner, administrators, anyone its group reaches);
 *  - `register`: the `register_servers` capability (a new machine, a new group);
 *  - `save`: saving a machine: MANAGE on it, or `register` for a new one;
 *  - `manage`: MANAGE on the machine or group (its owner or an administrator; the site's machines, the host and its
 *    online APIs, an administrator's whoever is named their owner);
 *  - `direct`: `manage`, on a machine the server dials (a helper machine has no key on the server);
 *  - `dialled`: `manage`, on a machine the server dials or the host, and for anyone but an administrator only the dry
 *    run, whose provider dials nothing (these routes dial the machine or drive LM Studio on the host);
 *  - `admin`: administrators (host-wide: the currency and the host, the key file, electricity plans, LM Studio's
 *    devices);
 *  - `host`: administrators, on the host (it reads the computer CITAR runs on). */
export const GATES = {
  servers: "member",
  poolServers: "member",
  poolGroups: "member",
  testPoolServer: "view",
  newServer: "register",
  createPoolServer: "register",
  createPoolGroup: "register",
  saveServer: "save",
  serverRates: "manage",
  importHardware: "manage",
  updatePoolServer: "manage",
  setQuietHours: "manage",
  deletePoolServer: "manage",
  workerStatus: "manage",
  createWorkerToken: "manage",
  revokeWorkerToken: "manage",
  deletePoolGroup: "manage",
  createGrant: "manage",
  updateGrant: "manage",
  revokeGrant: "manage",
  machineSettings: "manage",
  saveMachineSettings: "manage",
  applyScan: "manage",
  resetMachineKey: "manage",
  rescanMachine: "manage",
  setMachineTrust: "admin",
  replicateMachine: "admin",
  setMachineClass: "admin",
  storeKey: "direct",
  deleteKey: "direct",
  detectModels: "dialled",
  serverState: "dialled",
  serverLoading: "dialled",
  loadModel: "dialled",
  unloadModel: "dialled",
  estimateLoad: "dialled",
  loadedConfig: "dialled",
  serverSettings: "admin",
  lmDevices: "admin",
  unlockKeys: "admin",
  savePlan: "admin",
  deletePlan: "admin",
  collectHardware: "host",
};

/** Whether `viewer` may call the route `name` for `target`. `viewer` is `{admin, register}` (an administrator; holds
 *  the `register_servers` capability) or null for nobody signed in. `target` is the machine or group the call is
 *  about, as the page has it: `can_manage` from the pool's listing, `reach`, `is_host` and the connection's
 *  `provider` (or `connection.provider`), and `isNew` for a machine not saved yet; null for a call about none. A name
 *  missing from GATES is never allowed, so a new call cannot slip past the table. */
export function mayCall(name, viewer, target = null) {
  const gate = Object.prototype.hasOwnProperty.call(GATES, name) ? GATES[name] : null;
  if (!gate || !viewer) return false;
  const admin = !!viewer.admin;
  const t = target || {};
  const manage = !!target && !t.isNew && (admin || !!t.can_manage);
  const helper = (t.reach || "worker") === "worker";
  const provider = t.provider || (t.connection && t.connection.provider) || "";
  switch (gate) {
    case "member": return true;
    case "view": return !!target && !t.isNew;
    case "register": return admin || !!viewer.register;
    case "save": return t.isNew ? admin || !!viewer.register : manage;
    case "manage": return manage;
    case "direct": return manage && !helper;
    case "dialled": return manage && (!helper || !!t.is_host) && (admin || provider === "dryrun");
    case "admin": return admin;
    case "host": return admin && !!target && !!t.is_host;
    default: return false;
  }
}

// The address's path segments and its query ("?k=..." and the like), with "#/" taken off.
function split(hash) {
  const text = String(hash || "");
  const q = text.indexOf("?");
  const path = (q < 0 ? text : text.slice(0, q)).replace(/^#?\/?/, "");
  return { parts: path.split("/").filter((p) => p !== ""), query: q < 0 ? "" : text.slice(q) };
}

/** Where an address of the old Servers page leads now (`#/servers` before 0.1.6): `#/servers` to `#/pool`,
 *  `#/servers/<id>` to `#/pool/<id>` (the page opens that machine), and a group's link `#/servers/groups/<gid>` to
 *  `#/pool/groups/<gid>`, each keeping its query (a share key, say). Null for any other address. */
export function legacyServersTarget(hash) {
  const { parts, query } = split(hash);
  if (parts[0] !== "servers") return null;
  if (parts[1] === "groups") return parts[2] ? `#/pool/groups/${parts[2]}${query}` : `#/pool${query}`;
  return parts[1] ? `#/pool/${parts[1]}${query}` : `#/pool${query}`;
}

function decoded(segment) {
  try { return decodeURIComponent(segment); } catch (e) { return segment; }
}

/** What an address of the Servers page opens: `{machine}` for `#/pool/<id>`, `{group}` for `#/pool/groups/<gid>`,
 *  `{}` for the page alone (ids decoded). */
export function poolTarget(hash) {
  const { parts } = split(hash);
  if (parts[0] !== "pool" || !parts[1]) return {};
  if (parts[1] === "groups") return parts[2] ? { group: decoded(parts[2]) } : {};
  return { machine: decoded(parts[1]) };
}

/** The page's address for a machine (`#/pool/<id>`), or for a group with `group` set. */
export function poolLink(id, { group = false } = {}) {
  return `#/pool/${group ? "groups/" : ""}${encodeURIComponent(id)}`;
}

// ---------------------------------------------------------------------------- the machine settings dialog (P3.8)

/** The sections a helper's report may change, and what it does to each in each mode (the dialog's words; the
 *  server's rule is citar/machine_settings.py's). */
export const SECTIONS = ["hardware", "models", "power"];
export const MODE_TEXT = {
  hardware: {
    auto: "Each scan its helper sends replaces the hardware details (and the hardware class follows them).",
    manual: "The hardware details are yours: a scan is kept as the last scan and changes nothing until you apply it.",
  },
  models: {
    auto: "Each report updates the catalogue: new models are added, loaded ones marked, and a model no longer "
      + "reported is kept, marked as not reported. Your names and settings for a model stay.",
    manual: "The catalogue is yours: a report only marks which of its models are loaded; nothing is added or removed.",
  },
  power: {
    auto: "Each scan's suggested power figures replace yours (they are rough guesses from the hardware).",
    manual: "The power figures are yours: a scan's suggestion changes nothing until you apply it.",
  },
};

/** How long the dialog's "Rescan now" waits for its helper's new scan, and how often it looks (a helper scans in a
 *  few seconds; the protocol's bound is 5). */
export const RESCAN_WAIT_MS = 15000;
export const RESCAN_LOOK_MS = 500;

/** Whether the dialog's fresh read `fresh` holds a hardware report other than the one it had when "Rescan now" was
 *  asked (`before`: that report's `hardware_at`, or null when there was none). */
export function rescanned(before, fresh) {
  const at = ((fresh || {}).report || {}).hardware_at;
  return !!at && at !== before;
}

/** Whether a kind of machine shares CPU (an online API or a test machine has none of its own to share). */
export function sharesCpu(kind) {
  return kind === "owned" || kind === "leased";
}

/** A whole number from the dialog's input within `lo`..`hi`, or null. */
export function wholeIn(text, lo, hi) {
  if (text === "" || text == null) return null;
  const n = Number(text);
  return Number.isInteger(n) && n >= lo && n <= hi ? n : null;
}

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/** The settings route's body for what the dialog changed (`edited`, a copy of the route's read `current` with its
 *  `slots`, `capabilities` and `modes` changed): only the fields that differ, or null when nothing does. */
export function settingsBody(current, edited) {
  const body = {};
  if (edited.slots !== current.slots) body.slots = edited.slots;
  const caps = {};
  for (const part of ["llm", "cpu"]) {
    const changed = {};
    for (const [key, value] of Object.entries((edited.capabilities || {})[part] || {})) {
      if (!same(value, ((current.capabilities || {})[part] || {})[key])) changed[key] = value;
    }
    if (Object.keys(changed).length) caps[part] = changed;
  }
  if (Object.keys(caps).length) body.capabilities = caps;
  const modes = {};
  for (const section of SECTIONS) {
    if ((edited.modes || {})[section] !== (current.modes || {})[section]) modes[section] = edited.modes[section];
  }
  if (Object.keys(modes).length) body.modes = modes;
  return Object.keys(body).length ? body : null;
}

/** What the prompt scheduler has in effect for a machine (the route's `in_effect`), in a line. */
export function inEffectText(ie) {
  if (!ie || !ie.known) return "The prompt scheduler has not read this machine yet.";
  if (!ie.llm) return "In effect now: no model work.";
  const capacity = ie.capacity == null ? "no requests until its helper connects"
    : `${ie.capacity} request${ie.capacity === 1 ? "" : "s"} at once`;
  const ceiling = ie.helper_slots != null && ie.helper_slots < ie.slots
    ? ` (its helper takes at most ${ie.helper_slots})` : "";
  return `In effect now: ${capacity}${ceiling}; pooled prompts ${ie.allow_swaps ? "may" : "may not"} make it swap models.`;
}

const CAPPED_NAMES = { "cpu.enabled": "CPU lending", "cpu.threads": "threads", "cpu.cpu_percent": "CPU %",
                       "cpu.memory_mb": "memory", "cpu.work": "the kinds of work" };

/** What a machine's helper lends of its CPU in effect (the dialog's `cpu_helper`, the helper dialog's `cpu`: package
 *  4-10), in a line: why it lends nothing, or its threads, CPU share and memory, each limit hard or soft as its job host
 *  says it holds them, the memory a run may use, and what its own ceiling capped. */
export function cpuInEffectText(c) {
  if (!c) return "No CITAR helper of this version has connected for it yet: it takes no CPU work.";
  if (!c.plays_jobs) return "Its helper plays no CPU job (a build without jobs).";
  if (!c.lent) {
    return "Not lent by this machine's owner: CPU lending is off in its helper's own settings (citar-helper setup "
      + "asks, or --cpu turns it on for one run).";
  }
  if (!c.verifiable) return "Its helper failed its canary or self-check on this machine: it takes no CPU job.";
  const e = c.effective;
  if (!e || !e.enabled) return "In effect: no CPU work (it is off in these settings).";
  const held = (k) => (c.limits && c.limits[k]) || "soft";
  const perRun = c.memory_per_run_mb != null ? `; ${c.memory_per_run_mb} MB a run` : "";
  const capped = (c.capped || []).filter((k) => k !== "cpu.enabled").map((k) => CAPPED_NAMES[k] || k);
  return `In effect: ${e.threads} thread${e.threads === 1 ? "" : "s"}, ${e.cpu_percent}% of the CPU `
    + `(${held("cpu")}), ${e.memory_mb} MB (${held("memory")})${perRun}.`
    + (capped.length ? ` Its own ceiling capped ${capped.join(", ")}.` : "");
}

/** The grants that lend a machine's CPU for lab runs (the dialog's `lab_grants`), in a line: CPU work is lent only by a
 *  grant that lists Lab (CPU work), never by one with no purposes (design/phase4-helper.md P4.5.5). */
export function labGrantsText(grants) {
  if (!grants || !grants.length) {
    return "No grant on its group lists Lab (CPU work): only its owner's own experiments and the server's collection "
      + "(the ranking and reference games it takes) run on it.";
  }
  const who = [...new Set(grants.map((g) => g.subject))];
  return `Lent for lab runs (CPU work) to ${who.join(", ")}, by grants on “${grants[0].group}”.`;
}

/** A hardware class in a line: its name and rate (credits per generation-second for a GPU, per core-second for a CPU,
 *  per unit of token cost for an online API). */
export function classText(c) {
  if (!c) return "none";
  const per = { gpu: "per generation-second", cpu: "per core-second", api: "per unit of token cost" }[c.kind] || "";
  return `${c.name} (${c.rate} credits ${per})`.replace(" )", ")");
}

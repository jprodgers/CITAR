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

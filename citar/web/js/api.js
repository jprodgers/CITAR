// Thin wrappers around the CITAR HTTP API.

// The CSRF token for the current session. The server binds one to each session and rejects any
// cookie-authenticated POST/PUT/PATCH/DELETE without it, so every write here carries it.
// SameSite=Lax already blocks the straightforward cross-site POST; this is the second line, for
// the same-site cases SameSite does nothing about.
let csrfToken = "";

export function setCsrfToken(token) { csrfToken = token || ""; }
export function getCsrfToken() { return csrfToken; }

// Raised on a 401 so callers can send the person to the sign-in screen instead of showing a
// meaningless error. A session can expire at any moment, including mid-page.
export class NotSignedIn extends Error {
  constructor(message) { super(message || "Sign in to continue."); this.name = "NotSignedIn"; }
}

export class Forbidden extends Error {
  constructor(message) { super(message); this.name = "Forbidden"; }
}

async function request(method, url, body) {
  const opts = { method, headers: {}, credentials: "same-origin" };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  if (!["GET", "HEAD", "OPTIONS"].includes(method) && csrfToken) {
    opts.headers["x-citar-csrf"] = csrfToken;
  }
  // A share link puts its key in the page URL; pass it along so link-shared games and reports
  // work for somebody who is not signed in.
  const shareKey = new URLSearchParams((location.hash.split("?")[1] || "")).get("k");
  if (shareKey) opts.headers["x-citar-share-key"] = shareKey;

  const res = await fetch(url, opts);
  let data = null;
  try { data = await res.json(); } catch (e) { data = null; }
  if (!res.ok) {
    const msg = (data && (data.detail || data.error)) || `${res.status} ${res.statusText}`;
    const text = typeof msg === "string" ? msg : JSON.stringify(msg);
    if (res.status === 401) throw new NotSignedIn(text);
    if (res.status === 403) throw new Forbidden(text);
    const err = new Error(text);
    err.status = res.status;
    err.retryAfter = res.headers.get("retry-after");
    throw err;
  }
  return data;
}

export const api = {
  rules: () => request("GET", "/api/rules"),
  meta: () => request("GET", "/api/meta"),
  probeModels: (body) => request("POST", "/api/llm/models", body),
  games: () => request("GET", "/api/games"),
  game: (gid) => request("GET", `/api/games/${gid}`),
  createGame: (body) => request("POST", "/api/games", body),
  deleteGame: (gid) => request("DELETE", `/api/games/${gid}`),
  updateSeat: (gid, pid, body) => request("POST", `/api/games/${gid}/seats/${pid}`, body),
  control: (gid, body) => request("POST", `/api/games/${gid}/control`, body),
  path: (gid, token, unitId, x, y) =>
    request("GET", `/api/games/${gid}/path?token=${encodeURIComponent(token)}&unit_id=${unitId}&x=${x}&y=${y}`),
  view: (gid, token, asPlayer) =>
    request("GET", `/api/games/${gid}/view?token=${encodeURIComponent(token)}` + (asPlayer != null ? `&as_player=${asPlayer}` : "")),
  tool: (gid, token, tool, args = {}) =>
    request("POST", `/api/games/${gid}/tool?token=${encodeURIComponent(token)}`, { tool, args }),
  replay: (gid, token) => request("GET", `/api/games/${gid}/replay?token=${encodeURIComponent(token || "")}`),
  save: (gid, name) => request("POST", `/api/games/${gid}/save`, { name }),
  saves: () => request("GET", "/api/saves"),
  loadSave: (path) => request("POST", "/api/saves/load", { path }),
  deleteSave: (path, wholeGame = false) => request("POST", "/api/saves/delete", { path, whole_game: wholeGame }),
  errors: (gid) => request("GET", `/api/games/${gid}/debug/errors`),
  metrics: (gid) => request("GET", `/api/games/${gid}/metrics`),
  compareModels: () => request("GET", "/api/metrics/compare"),
  modelScores: () => request("GET", "/api/models/scores"),
  maps: () => request("GET", "/api/maps"),
  map: (id) => request("GET", `/api/maps/${encodeURIComponent(id)}`),
  saveMap: (id, map) => request("PUT", `/api/maps/${encodeURIComponent(id)}`, map),
  deleteMap: (id) => request("DELETE", `/api/maps/${encodeURIComponent(id)}`),
  validateMap: (map) => request("POST", "/api/maps/validate", map),
  generateMap: (body) => request("POST", "/api/maps/generate", body),
  mapFromGame: (gid) => request("POST", `/api/games/${gid}/export_map`),
  scenarios: () => request("GET", "/api/scenarios"),
  deleteScenario: (id) => request("DELETE", `/api/scenarios/${encodeURIComponent(id)}`),
  launchScenario: (id, body) => request("POST", `/api/scenarios/${encodeURIComponent(id)}/launch`, body),
  scenarioOps: () => request("GET", "/api/scenario-ops"),
  editorOpen: (body) => request("POST", "/api/scenario-editor", body),
  editorGet: (eid) => request("GET", `/api/scenario-editor/${eid}`),
  editorOps: (eid, ops) => request("POST", `/api/scenario-editor/${eid}/ops`, { ops }),
  editorUndo: (eid) => request("POST", `/api/scenario-editor/${eid}/undo`),
  editorMeta: (eid, body) => request("POST", `/api/scenario-editor/${eid}/meta`, body),
  editorSave: (eid, body) => request("POST", `/api/scenario-editor/${eid}/save`, body),
  probes: () => request("GET", "/api/probes"),
  probe: (id) => request("GET", `/api/probes/${encodeURIComponent(id)}`),
  probeExample: (scenario, subject, counterparty) => request("GET", `/api/probes/example?scenario=${encodeURIComponent(scenario)}&subject=${subject}&counterparty=${counterparty}`),
  saveProbe: (id, body) => request("PUT", `/api/probes/${encodeURIComponent(id)}`, body),
  validateProbe: (body) => request("POST", "/api/probes/validate", body),
  deleteProbe: (id) => request("DELETE", `/api/probes/${encodeURIComponent(id)}`),
  runProbe: (id, body) => request("POST", `/api/probes/${encodeURIComponent(id)}/run`, body),
  probeRuns: () => request("GET", "/api/probe-runs"),
  probeRun: (rid) => request("GET", `/api/probe-runs/${rid}`),
  probeStop: (rid) => request("POST", `/api/probe-runs/${rid}/stop`),
  probeRunDelete: (rid) => request("DELETE", `/api/probe-runs/${rid}`),
  probeOpen: (rid, c, rep) => request("POST", `/api/probe-runs/${rid}/open/${encodeURIComponent(c)}/${rep}`),
  lab: () => request("GET", "/api/lab"),
  labReport: (name) => request("GET", `/api/lab/report/${encodeURIComponent(name)}`),
  benchStatus: () => request("GET", "/api/benchmarks/status"),
  benchSettings: () => request("GET", "/api/benchmarks/settings"),
  saveBenchSettings: (body) => request("PUT", "/api/benchmarks/settings", body),
  suites: () => request("GET", "/api/benchmarks/suites"),
  newSuite: () => request("GET", "/api/benchmarks/suites/new"),
  suite: (id) => request("GET", `/api/benchmarks/suites/${id}`),
  saveSuite: (suite) => request("POST", "/api/benchmarks/suites", suite),
  deleteSuite: (id) => request("DELETE", `/api/benchmarks/suites/${id}`),
  startRun: (body) => request("POST", "/api/benchmarks/runs", body),
  runs: () => request("GET", "/api/benchmarks/runs"),
  runControl: (id, action) => request("POST", `/api/benchmarks/runs/${id}/control`, { action }),
  jobControl: (runId, jobId, action) => request("POST", `/api/benchmarks/runs/${runId}/jobs/${jobId}/control`, { action }),
  watchJob: (runId, jobId) => request("POST", `/api/benchmarks/runs/${runId}/jobs/${jobId}/watch`),
  // first-run setup and the operator console
  setupState: () => request("GET", "/api/setup/state"),
  setupScan: () => request("POST", "/api/setup/scan"),
  setupApply: (body) => request("POST", "/api/setup/apply", body),
  setupDismiss: () => request("POST", "/api/setup/dismiss"),
  setupReopen: () => request("POST", "/api/setup/reopen"),
  setupConsole: () => request("GET", "/api/setup/console"),
  setPolicy: (key, value) => request("POST", "/api/setup/policy", { key, value }),
  setupTestEmail: (to) => request("POST", "/api/setup/test-email", { to }),
  // servers
  servers: () => request("GET", "/api/servers"),
  newServer: (kind, provider) => request("GET", `/api/servers/new?kind=${kind}&provider=${provider}`),
  saveServer: (sv) => request("POST", "/api/servers", sv),
  deleteServer: (id) => request("DELETE", `/api/servers/${id}`),
  serverSettings: (body) => request("PUT", "/api/servers-settings", body),
  detectModels: (id) => request("POST", `/api/servers/${id}/detect`),
  serverState: (id) => request("GET", `/api/servers/${id}/state`),
  serverLoading: (id) => request("GET", `/api/servers/${id}/loading`),
  loadModel: (id, model_id, profile_id) => request("POST", `/api/servers/${id}/load`, { model_id, profile_id }),
  unloadModel: (id, model_id) => request("POST", `/api/servers/${id}/unload`, model_id ? { model_id } : {}),
  estimateLoad: (id, model_id, profile_id) => request("POST", `/api/servers/${id}/estimate`, { model_id, profile_id }),
  loadedConfig: (id, model_id) => request("POST", `/api/servers/${id}/loaded-config`, { model_id }),
  collectHardware: (id) => request("POST", `/api/servers/${id}/collect`),
  importHardware: (id, hw) => request("POST", `/api/servers/${id}/hardware`, hw),
  serverRates: (id) => request("GET", `/api/servers/${id}/rates`),
  lmDevices: () => request("GET", "/api/lmstudio/devices"),
  storeKey: (id, backend, key, env) => request("POST", `/api/servers/${id}/key`, { backend, key, env }),
  deleteKey: (id) => request("DELETE", `/api/servers/${id}/key`),
  unlockKeys: (passphrase) => request("POST", "/api/keys/unlock", { passphrase }),
  savePlan: (p) => request("POST", "/api/electricity-plans", p),
  deletePlan: (id) => request("DELETE", `/api/electricity-plans/${id}`),
  restricted: () => request("GET", "/api/servers/restricted"),
  // reports
  reports: () => request("GET", "/api/reports"),
  reportOptions: () => request("GET", "/api/reports/options"),
  startReport: (spec) => request("POST", "/api/reports", spec),
  report: (id) => request("GET", `/api/reports/${id}`),
  rerunReport: (id) => request("POST", `/api/reports/${id}/rerun`),
  deleteReport: (id) => request("DELETE", `/api/reports/${id}`),

  // ---------------------------------------------------------------- accounts
  authConfig: () => request("GET", "/api/auth/config"),
  me: () => request("GET", "/api/auth/me"),
  login: (identifier, password, tz) => request("POST", "/api/auth/login", { identifier, password, tz }),
  logout: () => request("POST", "/api/auth/logout"),
  signup: (body) => request("POST", "/api/auth/signup", body),
  resendVerification: (email) => request("POST", "/api/auth/verify/resend", { email }),
  forgotPassword: (email) => request("POST", "/api/auth/password/forgot", { email }),
  resetPassword: (token, password) => request("POST", "/api/auth/password/reset", { token, password }),
  changePassword: (current_password, new_password) =>
    request("POST", "/api/auth/password/change", { current_password, new_password }),
  updateProfile: (body) => request("PUT", "/api/auth/me", body),
  sessions: () => request("GET", "/api/auth/sessions"),
  revokeSession: (id) => request("DELETE", `/api/auth/sessions/${id}`),
  revokeOtherSessions: () => request("POST", "/api/auth/sessions/revoke-others"),
  unlinkProvider: (p) => request("POST", `/api/auth/oauth/${p}/unlink`),
  checkInvite: (code) => request("GET", `/api/invites/check?code=${encodeURIComponent(code)}`),
  invites: (includeSpent = false) => request("GET", `/api/invites?include_spent=${includeSpent}`),
  createInvite: (body) => request("POST", "/api/invites", body),
  revokeInvite: (id) => request("DELETE", `/api/invites/${id}`),

  // ---------------------------------------------------------------- server pool
  poolServers: (purpose = "game") => request("GET", `/api/pool/servers?purpose=${purpose}`),
  poolServer: (id) => request("GET", `/api/pool/servers/${id}`),
  createPoolServer: (body) => request("POST", "/api/pool/servers", body),
  updatePoolServer: (id, body) => request("PUT", `/api/pool/servers/${id}`, body),
  setMachineCosting: (id, body) => request("PUT", `/api/pool/servers/${id}/costing`, body),
  machineRates: (id) => request("GET", `/api/pool/servers/${id}/rates`),
  setQuietHours: (id, restricted_hours) => request("PUT", `/api/pool/servers/${id}/quiet-hours`, { restricted_hours }),
  deletePoolServer: (id) => request("DELETE", `/api/pool/servers/${id}`),
  // Built without a nested template literal: they are valid but awkward to read and easy to
  // mis-nest, and this one did exactly that.
  testPoolServer: (id, model) => {
    const query = model ? "?model=" + encodeURIComponent(model) : "";
    return request("POST", `/api/pool/servers/${id}/test` + query);
  },
  poolGroups: () => request("GET", "/api/pool/groups"),
  createPoolGroup: (body) => request("POST", "/api/pool/groups", body),
  updatePoolGroup: (id, body) => request("PUT", `/api/pool/groups/${id}`, body),
  deletePoolGroup: (id) => request("DELETE", `/api/pool/groups/${id}`),
  createGrant: (groupId, body) => request("POST", `/api/pool/groups/${groupId}/grants`, body),
  updateGrant: (id, body) => request("PUT", `/api/pool/grants/${id}`, body),
  revokeGrant: (id) => request("DELETE", `/api/pool/grants/${id}`),
  poolAvailability: (purpose = "game") => request("GET", `/api/pool/availability?purpose=${purpose}`),
  workerStatus: (serverId) => request("GET", `/api/pool/servers/${serverId}/workers`),
  createWorkerToken: (serverId, label) =>
    request("POST", `/api/pool/servers/${serverId}/workers/tokens`, { label }),
  revokeWorkerToken: (serverId, tokenId) =>
    request("DELETE", `/api/pool/servers/${serverId}/workers/tokens/${tokenId}`),
  importRegistry: () => request("POST", "/api/pool/import-registry"),

  // ---------------------------------------------------------------- sharing
  gameSharing: (gid) => request("GET", `/api/games/${gid}/sharing`),
  setGameSharing: (gid, body) => request("PUT", `/api/games/${gid}/sharing`, body),
  shareGameWith: (gid, handle, permission) =>
    request("POST", `/api/games/${gid}/sharing/users`, { handle, permission }),
  unshareGameWith: (gid, handle) =>
    request("DELETE", `/api/games/${gid}/sharing/users/${encodeURIComponent(handle)}`),
};

export function connectWS(gid, token, onMessage, onClose) {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  let ws = null;
  let closed = false;
  let retry = 500;
  function open() {
    ws = new WebSocket(`${proto}://${location.host}/ws/games/${gid}?token=${encodeURIComponent(token)}`);
    ws.onmessage = (e) => { try { onMessage(JSON.parse(e.data)); } catch (err) { console.error(err); } };
    ws.onopen = () => { retry = 500; };
    ws.onclose = () => {
      if (closed) return;
      if (onClose) onClose();
      setTimeout(open, retry);
      retry = Math.min(retry * 2, 8000);
    };
  }
  open();
  return { close() { closed = true; if (ws) ws.close(); } };
}

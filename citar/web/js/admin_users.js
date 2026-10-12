// The admin users page (design/phase3-features.md P3.2.7, P3.2.10): every account with filters, an edit drawer for
// its role, status, capabilities, limits, trust tier and directory listing, its audit rows, and the audit log.
//
// Administrators only: for anyone else the server answers 404 and the router shows "not found". Every change is
// one PATCH of the fields that changed, which the server checks as a whole and records as one audit row naming them;
// the server also refuses to leave itself without an active administrator.
import { api } from "./api.js";
import { el, clear, toast, modal } from "./util.js";
import { pageHeader } from "./nav.js";
import { avatarOf } from "./people.js";

const ROLES = ["user", "moderator", "admin"];
const STATUSES = ["active", "probation", "suspended"];
const TIERS = ["new", "provisional", "trusted", "quarantined"];
// [field, label, least, most]: the ranges the server takes (user_admin.LIMITS). 0 invitations is none; a member runs
// at least one game at once (the create_games capability is what stops one making any); 0 seats is the ruleset's most.
const LIMITS = [["invite_quota", "Invitations", 0, 1000], ["max_concurrent_games", "Games at once", 1, 100],
  ["max_seats_per_game", "Seats per game (0: no limit)", 0, 64]];

function when(iso) { return iso ? new Date(iso).toLocaleString() : "—"; }

export async function renderAdminUsers(root) {
  const first = await api.adminUsers({});          // a 404 (not an administrator) reaches the router: "not found"
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("admin-users"));

  // ---------------------------------------------------------------- accounts
  const q = el("input", { type: "search", placeholder: "Username, name or email", "aria-label": "Search accounts", class: "grow" });
  const role = el("select", { "aria-label": "Role" }, el("option", { value: "" }, "any role"), ...ROLES.map((r) => el("option", { value: r }, r)));
  const status = el("select", { "aria-label": "Status" }, el("option", { value: "" }, "any status"),
    ...["pending", ...STATUSES, "deleted"].map((s) => el("option", { value: s }, s)));
  const body = el("tbody");
  const more = el("button", { style: { display: "none" } }, "More");
  page.appendChild(el("section", { class: "card" },
    el("h2", {}, "Users"),
    el("div", { class: "row gap wrap" }, q, role, status),
    el("div", { class: "table-wrap" }, el("table", { class: "list admin-users" },
      el("thead", {}, el("tr", {}, ...["Account", "Email", "Role", "Status", "Trust", "Joined", "Last seen"].map((h) => el("th", {}, h)))),
      body)),
    el("div", { class: "row center" }, more)));

  let offset = 0;
  let seq = 0;
  const params = () => ({ q: q.value.trim(), role: role.value, status: status.value });
  function rows(list) {
    for (const u of list) {
      body.appendChild(el("tr", { class: "clickable", tabIndex: 0, title: "Edit",
        onclick: () => openDrawer(u.id, () => load(true)), onkeydown: (e) => { if (e.key === "Enter") openDrawer(u.id, () => load(true)); } },
        el("td", {}, el("div", { class: "row gap" }, avatarOf(u, 24), el("span", {}, el("b", {}, u.handle),
          u.display_name && u.display_name !== u.handle ? el("span", { class: "muted" }, ` ${u.display_name}`) : null))),
        el("td", {}, u.email || "—"),
        el("td", {}, u.role),
        el("td", {}, el("span", { class: `pill status-${u.status}` }, u.status), u.listed ? null : el("span", { class: "muted small" }, " unlisted")),
        el("td", {}, u.trust_tier),
        el("td", {}, when(u.created_at)),
        el("td", {}, when(u.last_seen_at))));
    }
  }
  function show(pageData, reset) {
    if (reset) clear(body);
    rows(pageData.users);
    if (reset && !pageData.users.length) body.appendChild(el("tr", {}, el("td", { colSpan: 7, class: "muted" }, "No account matches.")));
    offset = pageData.next || 0;
    more.style.display = pageData.next ? "" : "none";
  }
  async function load(reset) {
    const mine = ++seq;
    try {
      const data = await api.adminUsers({ ...params(), offset: reset ? 0 : offset });
      if (mine === seq) show(data, reset);
    } catch (e) { toast(e.message, "error"); }
  }
  show(first, true);
  let timer = null;
  q.addEventListener("input", () => { clearTimeout(timer); timer = setTimeout(() => load(true), 250); });
  role.onchange = status.onchange = () => load(true);
  more.onclick = () => load(false);

  // ---------------------------------------------------------------- the audit log
  page.appendChild(auditCard());
  return { destroy() { clearTimeout(timer); } };
}

// One account's edit drawer: the fields an administrator changes, then its audit rows.
async function openDrawer(id, onSaved) {
  let data;
  try { data = await api.adminUser(id); } catch (e) { toast(e.message, "error"); return; }
  const u = data.user;
  const content = el("div", { class: "col gap admin-drawer" });
  const m = modal({ title: `@${u.handle}`, content });

  const roleSel = el("select", { "aria-label": "Role" }, ...ROLES.map((r) => el("option", { value: r, selected: r === u.role }, r)));
  const statusSel = el("select", { "aria-label": "Status" },
    ...(STATUSES.includes(u.status) ? [] : [el("option", { value: u.status, selected: true, disabled: true }, u.status)]),
    ...STATUSES.map((s) => el("option", { value: s, selected: s === u.status }, s)));
  const reason = el("input", { placeholder: "Why (its owner is told when they sign in)", "aria-label": "Suspension reason", class: "grow",
    value: u.suspended_reason || "" });
  const reasonRow = el("label", { class: "field" }, el("span", { class: "field-label" }, "Reason for the suspension"), reason);
  const syncReason = () => { reasonRow.style.display = statusSel.value === "suspended" ? "" : "none"; };
  statusSel.onchange = syncReason;
  syncReason();
  const tierSel = el("select", { "aria-label": "Trust tier" }, ...TIERS.map((t) => el("option", { value: t, selected: t === u.trust_tier }, t)));
  const listed = el("input", { type: "checkbox", checked: !!u.listed });
  const limits = Object.fromEntries(LIMITS.map(([k, , min, max]) => [k, el("input", { type: "number", min, max, step: 1, value: u[k], "aria-label": k, style: { width: "90px" } })]));

  // capabilities: the role's default, or an override on or off
  const capSel = {};
  const capRows = Object.keys(data.role_defaults).map((cap) => {
    const override = Object.prototype.hasOwnProperty.call(u.caps || {}, cap) ? (u.caps[cap] ? "on" : "off") : "default";
    capSel[cap] = el("select", { "aria-label": cap },
      el("option", { value: "default", selected: override === "default" }, `role default (${data.role_defaults[cap] ? "on" : "off"})`),
      el("option", { value: "on", selected: override === "on" }, "on"),
      el("option", { value: "off", selected: override === "off" }, "off"));
    return el("tr", {}, el("td", {}, el("code", {}, cap)), el("td", {}, capSel[cap]),
      el("td", { class: u.capabilities[cap] ? "good" : "muted" }, u.capabilities[cap] ? "allowed" : "not allowed"));
  });

  const err = el("p", { class: "bad small" });
  const save = el("button", { class: "primary", onclick: async () => {
    const change = {};
    err.textContent = "";
    // a limit left empty (or not a number, which a number input reads as empty) is refused here, never sent as 0
    for (const [k, label, min, max] of LIMITS) {
      const raw = limits[k].value.trim();
      const v = Number(raw);
      if (raw === "" || !Number.isInteger(v) || v < min || v > max) {
        err.textContent = `${label} is a whole number from ${min} to ${max}.`;
        limits[k].focus();
        return;
      }
      if (v !== u[k]) change[k] = v;
    }
    if (roleSel.value !== u.role) change.role = roleSel.value;
    if (statusSel.value !== u.status) change.status = statusSel.value;
    if (tierSel.value !== u.trust_tier) change.trust_tier = tierSel.value;
    if (listed.checked !== !!u.listed) change.listed = listed.checked;
    const caps = {};
    for (const [cap, sel] of Object.entries(capSel)) {
      const was = Object.prototype.hasOwnProperty.call(u.caps || {}, cap) ? (u.caps[cap] ? "on" : "off") : "default";
      if (sel.value !== was) caps[cap] = sel.value === "default" ? null : sel.value === "on";
    }
    if (Object.keys(caps).length) change.caps = caps;
    if (!Object.keys(change).length) { m.close(); return; }
    if (change.status === "suspended") change.reason = reason.value.trim();
    save.disabled = true;
    try {
      const r = await api.patchAdminUser(id, change);
      toast(`Saved: ${r.changed.join(", ") || "nothing changed"}.`, "good");
      m.close();
      if (onSaved) onSaved();
    } catch (e) { err.textContent = e.message; save.disabled = false; }
  } }, "Save");

  content.append(
    el("div", { class: "row gap" }, avatarOf(u, 48), el("div", { class: "col" },
      el("b", {}, u.display_name || u.handle), el("span", { class: "muted small" }, `${u.email || "no email"} · joined ${when(u.created_at)} · last seen ${when(u.last_seen_at)}`),
      u.is_local_owner ? el("span", { class: "pill" }, "local owner") : null,
      el("a", { href: `#/u/${encodeURIComponent(u.handle)}`, onclick: () => m.close() }, "Profile"))),
    el("div", { class: "grid2" },
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Role"), roleSel),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Status"), statusSel),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Trust tier"), tierSel),
      el("label", { class: "row gap" }, listed, el("span", {}, "Listed in the members' directory"))),
    reasonRow,
    u.status === "suspended" && u.suspended_reason ? el("p", { class: "warn small" }, `Suspended ${when(u.suspended_at)}: ${u.suspended_reason}`) : null,
    el("h3", {}, "Limits"),
    el("div", { class: "grid2" }, ...LIMITS.map(([k, label]) => el("label", { class: "field" }, el("span", { class: "field-label" }, label), limits[k]))),
    el("h3", {}, "Capabilities"),
    el("table", { class: "list" }, el("tbody", {}, ...capRows)),
    err,
    el("div", { class: "row gap" }, save, el("button", { onclick: () => m.close() }, "Cancel")),
    el("h3", {}, "Audit"),
    auditTable(data.audit));
}

function auditTable(entries) {
  if (!entries.length) return el("p", { class: "muted" }, "Nothing recorded.");
  return el("div", { class: "table-wrap" }, el("table", { class: "list audit" },
    el("thead", {}, el("tr", {}, ...["When", "Who", "What", "On", "Detail"].map((h) => el("th", {}, h)))),
    el("tbody", {}, ...entries.map(auditRow))));
}

function auditRow(e) {
  return el("tr", {},
    el("td", { class: "nowrap" }, when(e.at)),
    el("td", {}, e.actor),
    el("td", {}, el("code", {}, e.action)),
    el("td", { title: e.object_id || "" }, e.object_type ? `${e.object_type} ${e.object_label || e.object_id || ""}` : "—"),
    el("td", { class: "audit-detail" }, describe(e.detail)));
}

// An audit row's detail, readable: "role: user → moderator" for a change, key=value for the rest.
function describe(detail) {
  if (!detail || !Object.keys(detail).length) return "";
  const parts = [];
  if (detail.changes) {
    for (const [field, c] of Object.entries(detail.changes)) parts.push(`${field}: ${fmt(c.from)} → ${fmt(c.to)}`);
  }
  for (const [k, v] of Object.entries(detail)) {
    if (k === "changes" || k === "fields") continue;
    parts.push(`${k}: ${fmt(v)}`);
  }
  return parts.join("; ");
}

function fmt(v) {
  if (v === null || v === undefined) return "—";
  if (typeof v === "object") return Object.keys(v).length ? JSON.stringify(v) : "{}";
  return String(v);
}

// The audit log: filters and pages of 100, newest first.
function auditCard() {
  const actor = el("input", { placeholder: "Who (or admin_cli)", title: "A username, or admin_cli for the command line's changes", "aria-label": "Actor" });
  const action = el("input", { placeholder: "Action (user.* for a prefix)", "aria-label": "Action" });
  const objType = el("input", { placeholder: "Object type", "aria-label": "Object type", style: { width: "120px" } });
  const objId = el("input", { placeholder: "Object id", "aria-label": "Object id" });
  const tbody = el("tbody");
  const older = el("button", { style: { display: "none" } }, "Older");
  let before = null;
  async function load(reset) {
    const params = {};
    for (const [k, input] of [["actor", actor], ["action", action], ["object_type", objType], ["object_id", objId]]) {
      if (input.value.trim()) params[k] = input.value.trim();
    }
    if (!reset && before != null) params.before = before;
    try {
      const page = await api.adminAudit(params);
      if (reset) clear(tbody);
      for (const e of page.entries) tbody.appendChild(auditRow(e));
      if (reset && !page.entries.length) tbody.appendChild(el("tr", {}, el("td", { colSpan: 5, class: "muted" }, "Nothing recorded.")));
      before = page.next_before;
      older.style.display = before != null ? "" : "none";
    } catch (e) { toast(e.message, "error"); }
  }
  const go = el("button", { class: "primary", onclick: () => load(true) }, "Filter");
  for (const input of [actor, action, objType, objId]) input.addEventListener("keydown", (e) => { if (e.key === "Enter") load(true); });
  older.onclick = () => load(false);
  load(true);
  return el("section", { class: "card" }, el("h2", {}, "Audit log"),
    el("div", { class: "row gap wrap" }, actor, action, objType, objId, go),
    el("div", { class: "table-wrap" }, el("table", { class: "list audit" },
      el("thead", {}, el("tr", {}, ...["When", "Who", "What", "On", "Detail"].map((h) => el("th", {}, h)))), tbody)),
    el("div", { class: "row center" }, older));
}

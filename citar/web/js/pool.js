// The Servers page (#/pool): every machine the viewer owns or may use, the ones the server connects to itself
// (direct: the host, its online APIs, a dry run) and the ones behind a CITAR helper alike, each on a card with the
// dialogs that edit it (servers.js), and the server groups that share machines with who may use them on what terms
// (design/phase3-features.md P3.2.8). `#/pool/<id>` opens a machine and `#/pool/groups/<gid>` shows a group; the
// page's old addresses (`#/servers`, `#/servers/<id>`) lead here (machines.js, app.js).
//
// Every request goes through servers.js's `call`, which asks machines.js's gates first: a member's page shows no
// control, and calls no route, that an administrator's alone would take.
//
// The availability editor is the interesting part. A window is entered in the *owner's* zone and shown to everyone
// else in theirs, because "midnight to 6am" means the owner's midnight and a viewer in Tokyo needs to know that is
// their afternoon.
import { el, clear, toast, modal, confirmBox, keepPlace } from "./util.js";
import { pageHeader } from "./nav.js";
import { helperCard } from "./helper.js";
import { openMachineSettings } from "./machine_settings.js";
import { cpuInEffectText, poolTarget } from "./machines.js";
import {
  KIND, KEYED, PROVIDER, addDirectMachine, call, describeConn, describeQuiet, loadRegistry, may, money, openConnection,
  openCosts, openHardware, openKey, openPlans, openQuietHours, openSettings, viewer,
} from "./servers.js";
import * as auth from "./auth.js";

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
// what a grant may be limited to (citar.pool.admission.PURPOSES): "game" is a model seat that plays every turn,
// "pooled" one prompt at a time (a report's written analysis, a probe's offer or message, a machine test, later a
// hybrid seat's diplomacy)
const PURPOSES = [
  ["game", "Ordinary games"], ["pooled", "Pooled prompts"], ["benchmark", "Benchmarks"], ["probe", "Probes"],
  ["scenario", "Scenarios"], ["lab", "Lab (CPU work)"],
];
const ICON = { owned: "🖥", leased: "☁", api: "🔑", test: "🧪" };

function hhmm(minutes) {
  if (minutes >= 1440) return "24:00";
  return `${String(Math.floor(minutes / 60)).padStart(2, "0")}:${String(minutes % 60).padStart(2, "0")}`;
}

function minutesOf(text) {
  const [h, m] = String(text || "0:00").split(":");
  return (parseInt(h, 10) || 0) * 60 + (parseInt(m, 10) || 0);
}

// What the gates read of a card's machine: its registry view when the viewer manages it (`reg`: reach, is_host, the
// connection), else what the pool lists to anyone who may see it.
const gateOf = (m) => m.reg || m;

function statusPill(m) {
  if (!m.enabled) return el("span", { class: "pill", title: "Nothing runs on it until it is enabled again" }, "disabled");
  if (m.reach !== "worker") return el("span", { class: "pill", title: "The server connects to this machine itself" }, "direct");
  if (m.online === true) return el("span", { class: "pill live", title: "Its CITAR helper is connected" }, "● helper online");
  return el("span", { class: "pill quiet", title: "Its CITAR helper is not connected" }, "helper offline");
}

function admissionLine(m) {
  const a = m.admission || {};
  if (a.allowed) return el("span", { class: "good small" }, "available to you now");
  return el("span", { class: "muted small" }, a.reason || "not available to you");
}

// ---------------------------------------------------------------------------- windows editor

function windowEditor(initial, ownerTz) {
  const rows = el("div", { class: "col gap" });
  const model = [...(initial || [])];

  const redraw = () => {
    clear(rows);
    if (!model.length) {
      rows.appendChild(el("span", { class: "muted small" },
        "No limit — they may use it at any hour (the machine's quiet hours still apply)."));
    }
    model.forEach((w, index) => {
      const day = el("select", {}, ...DAYS.map((d, i) =>
        el("option", { value: i, selected: i === w.weekday }, d)));
      const from = el("input", { type: "time", value: hhmm(w.start_min), style: { width: "110px" } });
      const to = el("input", { type: "time", value: hhmm(w.end_min), style: { width: "110px" } });
      day.onchange = () => { w.weekday = parseInt(day.value, 10); };
      from.onchange = () => { w.start_min = minutesOf(from.value); };
      to.onchange = () => { w.end_min = minutesOf(to.value) || 1440; };
      rows.appendChild(el("div", { class: "row gap" }, day, from, el("span", {}, "→"), to,
        el("button", {
          class: "small danger",
          onclick: () => { model.splice(index, 1); redraw(); },
        }, "✕")));
    });
  };
  redraw();

  const addDaily = (start, end) => {
    model.length = 0;
    for (let d = 0; d < 7; d++) model.push({ weekday: d, start_min: start, end_min: end });
    redraw();
  };

  return {
    node: el("div", { class: "col gap" },
      el("span", { class: "muted small" },
        `The hours they MAY use it; outside them they cannot start work. Entered in your zone (${ownerTz}); `
        + "other people see them translated into theirs. This limits the people you share with, never you: "
        + "to keep the machine idle for everyone, set Quiet hours on the machine instead."),
      rows,
      el("div", { class: "row gap wrap" },
        el("button", { class: "small", onclick: () => { model.push({ weekday: 0, start_min: 0, end_min: 360 }); redraw(); } }, "+ window"),
        el("button", { class: "small", onclick: () => addDaily(0, 360) }, "Every night 00:00–06:00"),
        el("button", { class: "small", onclick: () => addDaily(540, 1020) }, "Every day 09:00–17:00"),
        el("button", { class: "small", onclick: () => { model.length = 0; redraw(); } }, "Any time"))),
    value: () => model.filter((w) => w.start_min !== w.end_min),
  };
}

// ---------------------------------------------------------------------------- grant editor

function grantDialog(group, existing, onSaved) {
  const me = auth.user();
  const subject = el("select", {},
    el("option", { value: "everyone" }, "Everyone on this server"),
    el("option", { value: "user" }, "One account"));
  const handle = el("input", { placeholder: "username", style: { display: "none" } });
  subject.onchange = () => { handle.style.display = subject.value === "user" ? "" : "none"; };

  const purposeBoxes = PURPOSES.map(([value, label]) => {
    const box = el("input", { type: "checkbox", value });
    return { value, node: el("label", { class: "checkrow" }, box, label), box };
  });
  const concurrency = el("input", { type: "number", min: "0", value: "0", style: { width: "90px" } });
  const budgetAmount = el("input", { type: "number", min: "0", step: "1", placeholder: "0 = no cap",
                                     style: { width: "120px" } });
  const budgetPeriod = el("select", {},
    el("option", { value: "month" }, "per month"), el("option", { value: "week" }, "per week"),
    el("option", { value: "day" }, "per day"), el("option", { value: "total" }, "in total"));
  const budgetBehavior = el("select", {},
    el("option", { value: "hard" }, "hard — refuse new work and pause at the cap"),
    el("option", { value: "soft" }, "soft — warn but keep going"));
  const note = el("input", { placeholder: "optional note" });
  const windows = windowEditor(existing ? existing.windows : [], me.tz);

  if (existing) {
    subject.value = existing.subject_type;
    if (existing.subject) handle.value = existing.subject.handle;
    handle.style.display = existing.subject_type === "user" ? "" : "none";
    for (const p of purposeBoxes) p.box.checked = (existing.purposes || []).includes(p.value);
    concurrency.value = existing.concurrency || 0;
    if (existing.budget && existing.budget.limited) {
      budgetAmount.value = existing.budget.limit;
      budgetPeriod.value = existing.budget.period;
      budgetBehavior.value = existing.budget.behavior;
    }
    note.value = existing.note || "";
  }

  const save = async () => {
    const purposes = purposeBoxes.filter((p) => p.box.checked).map((p) => p.value);
    const amount = parseFloat(budgetAmount.value) || 0;
    const body = {
      subject_type: subject.value,
      handle: subject.value === "user" ? handle.value.trim() : null,
      purposes,
      concurrency: parseInt(concurrency.value, 10) || 0,
      note: note.value,
      windows: windows.value(),
      budget: amount > 0
        ? { period: budgetPeriod.value, amount, unit: "credits", behavior: budgetBehavior.value }
        : null,
    };
    try {
      if (existing) await call("updateGrant", group, existing.id, body);
      else await call("createGrant", group, group.id, body);
      toast("Access saved.", "good");
      m.close();
      onSaved();
    } catch (e) { toast(e.message, "error"); }
  };

  const m = modal({
    title: existing ? "Edit access" : `Give access to “${group.name}”`,
    content: el("div", { class: "col gap" },
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Who"), subject, handle),
      el("div", { class: "field" },
        el("span", { class: "field-label" }, "What they may run"),
        el("div", { class: "col" }, ...purposeBoxes.map((p) => p.node)),
        el("span", { class: "muted small" }, "Nothing ticked means every kind of model work. Lab (CPU work) is lent "
          + "only when it is ticked: the lab's games then run on these machines' CPUs, within each machine's own CPU "
          + "settings, for the accounts this grant names.")),
      el("div", { class: "field" },
        el("span", { class: "field-label" }, "Allowed hours"), windows.node),
      el("label", { class: "field" },
        el("span", { class: "field-label" }, "Simultaneous games"), concurrency,
        el("span", { class: "muted small" }, "0 uses the server's own limit.")),
      el("div", { class: "field" },
        el("span", { class: "field-label" }, "Spend cap (credits)"),
        el("div", { class: "row gap wrap" }, budgetAmount, budgetPeriod),
        budgetBehavior,
        el("span", { class: "muted small" },
          "Accounting only — nobody is charged and no money moves. Credits are the compute ledger's unit "
          + "(about one per GPU generation-second); the cap stops work being scheduled. Periods end at your midnight.")),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Note"), note)),
    footer: [el("button", { onclick: () => m.close() }, "Cancel"),
             el("button", { class: "primary", onclick: save }, "Save")],
  });
}

// ---------------------------------------------------------------------------- the helper and its tokens

/** What the machine's helper is (its version, build, target and protocol, from its last hello) and the fingerprint of
 *  the machine key it registered, with "Reset key…" (design/phase4-helper.md P4.3.7, P4.5.11): the owner checks the
 *  fingerprint against `citar-helper --version` on the machine, and resets the key when the helper was reinstalled or
 *  its token was used by someone else first. */
function helperFacts(info, m, refresh) {
  const h = info.helper || null;
  const key = info.key || {};
  const rows = [];
  if (h) {
    rows.push(el("span", { class: "muted small" },
      `CITAR helper ${h.version || "?"} · protocol ${h.protocol || 2} · build ${h.build || "?"}`
      + (h.target ? ` · ${h.target}` : "")));
    rows.push(el("span", { class: "muted small" }, `CPU: ${cpuInEffectText(info.cpu)}`));
  }
  rows.push(el("div", { class: "row gap wrap" },
    el("span", { class: "muted small" }, key.fingerprint
      ? ["Machine key ", el("code", {}, key.fingerprint)] : "No machine key registered yet: its helper's next hello registers one."),
    key.fingerprint ? el("button", {
      class: "small danger",
      title: "Forget this key: the next helper to connect with a token of this machine registers its own",
      onclick: async () => {
        if (!(await confirmBox("Reset the machine key?",
              "The next helper that connects with one of this machine's tokens registers its own key; a helper "
              + "connected now is made to reconnect, and registers its key at once. Do this after reinstalling the "
              + "helper. If the fingerprint is not the one citar-helper --version prints on your machine, someone "
              + "else's helper used a token: revoke this machine's tokens first and issue a new one, or theirs may "
              + "register again."))) return;
        try { await call("resetMachineKey", gateOf(m), m.id); toast("Key reset.", "good"); refresh(); }
        catch (e) { toast(e.message, "error"); }
      },
    }, "Reset key…") : null));
  return el("div", { class: "col" }, ...rows);
}

async function helperDialog(m) {
  const body = el("div", { class: "col gap" }, el("span", { class: "muted" }, "Loading…"));
  const dialog = modal({ title: `Helper for “${m.name}”`, content: body });

  const refresh = async () => {
    clear(body);
    try {
      const info = await call("workerStatus", gateOf(m), m.id);
      body.append(
        el("div", { class: "row gap" },
          info.online ? el("span", { class: "pill live" }, "● connected")
                      : el("span", { class: "pill quiet" }, "not connected"),
          info.worker ? el("span", { class: "muted" },
            `${info.worker.hostname} · ${info.worker.models.length} models · `
            + `${info.worker.in_flight}/${info.worker.slots} busy`) : null),
        helperFacts(info, m, refresh),
        el("p", { class: "muted" },
          "The helper dials out to this server, so nothing on your machine needs to be reachable "
          + "from the internet. Run it where the model is:"),
        helperCard({ args: info.args }),
        el("h3", {}, "Tokens"),
        ...(info.tokens.length
          ? info.tokens.map((t) => el("div", { class: "row between listrow" },
              el("div", { class: "col" },
                el("code", {}, `${t.prefix}…`),
                el("span", { class: "muted small" },
                  `${t.label || "no label"} · `
                  + (t.last_seen_at ? `last used ${new Date(t.last_seen_at).toLocaleString()}`
                                    : "never used")
                  + (t.revoked ? " · REVOKED" : ""))),
              t.revoked ? null : el("button", {
                class: "danger small",
                onclick: async () => {
                  if (!(await confirmBox("Revoke this token?",
                        "A helper using it disconnects at once and cannot connect with it again."))) return;
                  try { await call("revokeWorkerToken", gateOf(m), m.id, t.id); refresh(); }
                  catch (e) { toast(e.message, "error"); }
                },
              }, "Revoke")))
          : [el("span", { class: "muted" }, "No tokens yet.")]),
        el("button", {
          class: "primary",
          onclick: async () => {
            try {
              const created = await call("createWorkerToken", gateOf(m), m.id, "");
              clear(body);
              body.append(
                el("h3", {}, "New worker token"),
                el("p", { class: "warn-box" }, created.warning),
                helperCard({ args: created.args }),
                el("p", { class: "muted small" }, "Or, with the helper on that machine's PATH:"),
                el("pre", { class: "code-block" }, created.command),
                el("button", {
                  class: "primary",
                  onclick: () => { navigator.clipboard.writeText(created.command); toast("Copied.", "good"); },
                }, "Copy command"),
                el("button", { onclick: refresh }, "Done"));
            } catch (e) { toast(e.message, "error"); }
          },
        }, "Issue a new token"));
    } catch (e) {
      body.appendChild(el("span", { class: "bad" }, e.message));
    }
  };
  refresh();
  return dialog;
}

// ---------------------------------------------------------------------------- the page

/** The Servers page. The address may name a machine to open (`#/pool/<id>`) or a group to show (`#/pool/groups/<gid>`). */
export async function renderPool(root) {
  if (!auth.user()) { auth.renderSignInWall(root); return {}; }
  clear(root);
  root.appendChild(pageHeader("pool"));
  const page = el("div", { class: "page" });
  const head = el("div");
  const notice = el("div");                 // what the address asked for and the page could not show; never redrawn
  const body = el("div");
  page.append(head, notice, body);
  root.appendChild(page);

  let data = null;
  const refresh = async () => {
    const [pool, sharing, reg] = await Promise.all([call("poolServers", null, "game"), call("poolGroups", null), loadRegistry()]);
    const regs = new Map(reg.servers.map((sv) => [sv.id, sv]));
    const machines = pool.servers.map((s) => ({ ...s, reg: regs.get(s.id) || null }));
    data = { machines, groups: sharing.groups, reg };
    keepPlace(page, () => { drawHead(head, data, refresh); drawBody(body, data, refresh); });
  };
  const quiet = () => refresh().catch((e) => toast(e.message, "error"));
  try {
    await refresh();
  } catch (e) {
    if (e.name === "NotSignedIn") throw e;
    page.appendChild(el("p", { class: "bad" }, e.message));
    return {};
  }

  // the machine or group the address names
  // (one the viewer may not see reads as one that is not there: the server answers 404 for both)
  const target = poolTarget(location.hash);
  const missing = (what) => notice.appendChild(el("div", { class: "card not-found" },
    el("p", {}, `That ${what} is not here, or it has not been shared with you.`)));
  if (target.machine) {
    const m = data.machines.find((x) => x.id === target.machine);
    const card = m && body.querySelector(`[data-machine="${CSS.escape(m.id)}"]`);
    if (!card) missing("machine");
    else {
      focus(card);
      if (m.reg) openConnection(m.reg, quiet);
    }
  } else if (target.group) {
    const card = body.querySelector(`[data-group="${CSS.escape(target.group)}"]`);
    if (card) focus(card);
    else missing("group");
  }

  // live status (a helper connecting, quiet hours starting) without disturbing a dialog or the reader's place
  const timer = setInterval(() => {
    if (!document.body.contains(page)) { clearInterval(timer); return; }
    if (!document.querySelector(".modal-back")) refresh().catch(() => {});
  }, 20000);
  return { destroy() { clearInterval(timer); } };
}

function focus(card) {
  card.classList.add("focus");
  card.scrollIntoView({ block: "center" });
}

function drawHead(head, data, refresh) {
  const reload = () => refresh().catch((e) => toast(e.message, "error"));
  clear(head).append(
    el("div", { class: "row between wrap" },
      el("h1", {}, "Servers"),
      el("div", { class: "row gap wrap" },
        may("savePlan") ? el("button", { onclick: () => openPlans(reload) }, "⚡ Electricity plans") : null,
        may("serverSettings") ? el("button", { onclick: () => openSettings(reload) }, "⚙ Settings") : null,
        may("createPoolGroup") ? el("button", { onclick: () => newGroupDialog(reload) }, "New group") : null,
        may("createPoolServer") ? el("button", { class: "primary", onclick: () => addMachineDialog(data.groups, reload) }, "+ Add a machine") : null)),
    el("p", { class: "muted" }, "The machines that run models: the ones this server connects to itself and the ones behind a CITAR helper, ",
      "yours and the ones shared with you. Games, benchmarks and probes pick a model from here, and reports cost the work with these settings."),
    may("createPoolServer") ? null : el("p", { class: "muted" },
      "Your account cannot register machines yet. New accounts gain this once an administrator clears them."),
    (data.reg.restricted || []).length
      ? el("div", { class: "row wrap" }, ...data.reg.restricted.map((r) =>
          el("span", { class: "pill quiet" }, `🌙 ${r.name}: quiet hours until ${r.until} (its work is paused)`)))
      : null);
}

// The sections a machine goes in: for an administrator the site's machines (the host, the online APIs, any with no
// owner), their own, and the members'; for a member their own and the ones shared with them.
function sections(machines) {
  const admin = viewer().admin;
  const site = (m) => !!(m.reg && (m.reg.is_host || m.reg.server_resource)) || !m.owner;
  const order = { owned: 0, leased: 1, api: 2, test: 3 };
  const sorted = [...machines].sort((a, b) => (!!(b.reg && b.reg.is_host) - !!(a.reg && a.reg.is_host))
    || ((order[a.kind] ?? 9) - (order[b.kind] ?? 9)) || a.name.localeCompare(b.name));
  if (admin) {
    return [
      ["This server's machines", sorted.filter(site), "The host and the services the site provides: administrators', whoever is named their owner."],
      ["Your machines", sorted.filter((m) => !site(m) && m.is_mine), null],
      ["Members' machines", sorted.filter((m) => !site(m) && !m.is_mine), "Each member's own; you manage them as an administrator."],
    ];
  }
  return [
    ["Your machines", sorted.filter((m) => m.can_manage), null],
    ["Shared with you", sorted.filter((m) => !m.can_manage), "Machines a group shares with you, on its terms."],
  ];
}

function drawBody(body, data, refresh) {
  const reload = () => refresh().catch((e) => toast(e.message, "error"));
  clear(body);
  const parts = sections(data.machines);
  const mine = parts.find(([title]) => title === "Your machines")[1];
  for (const [title, list, note] of parts) {
    if (title === "Your machines") {
      body.appendChild(el("h2", {}, title));
      body.appendChild(helperCard({ compact: mine.length > 0 }));
      if (!list.length) {
        body.appendChild(el("p", { class: "muted" },
          "None yet. Add a machine, then run the CITAR helper on the computer that has the model: it connects outward, "
          + "so you do not need to open any ports."));
      }
    } else {
      if (!list.length) continue;
      body.appendChild(el("h2", {}, title));
    }
    if (note) body.appendChild(el("p", { class: "muted small" }, note));
    for (const m of list) body.appendChild(machineCard(m, data, reload));
  }

  body.appendChild(el("h2", {}, "Groups and access"));
  if (!data.groups.length) {
    body.appendChild(el("p", { class: "muted" },
      "A group is how you share machines: put machines in one (Sharing, on a machine's card), then give access to it."));
  }
  for (const group of data.groups) body.appendChild(groupCard(group, data.machines, reload));
}

// The models a card shows: what a connected helper reports (loaded ones marked), else the catalogue.
function modelChips(m) {
  const live = m.reach === "worker" && m.online && (m.live_models || []).length;
  const list = live ? m.live_models.filter((x) => x.key) : m.reg ? m.reg.models.filter((x) => x.enabled) : (m.models || []);
  return list.map((x) => el("span", { class: `pill model-chip${x.loaded ? " live" : ""}`, title: x.key + (x.loaded ? " — loaded" : ""), "data-key": x.key },
    x.label || x.key));
}

function machineCard(m, data, reload) {
  const g = gateOf(m);
  const reg = m.reg;
  const helper = m.reach === "worker";
  const provider = reg ? reg.connection.provider : m.provider;
  const rates = el("span", { class: "muted small" });
  const models = el("div", { class: "row wrap", style: { marginTop: "6px" } });
  const chips = modelChips(m);
  models.append(el("span", { class: "muted small" }, chips.length ? "Models:" : helper ? "No models reported yet." : "No models in the catalog yet."), ...chips);
  // a group the viewer is not given (another member's, on an administrator's page) is named only as "shared"
  const group = m.group_id ? data.groups.find((x) => x.id === m.group_id) || { name: "its owner's group" } : null;
  const card = el("section", { class: "card server-card", "data-machine": m.id },
    el("div", { class: "row wrap" },
      el("span", { class: "server-icon" }, ICON[m.kind] || "🖥"),
      el("h3", {}, m.name),
      el("span", { class: "pill" }, KIND[m.kind] || m.kind),
      reg && reg.is_host ? el("span", { class: "pill live", title: "The machine running CITAR: it runs the game engine and scripted bots" }, "runs CITAR") : null,
      statusPill(m),
      m.restricted_until ? el("span", { class: "pill quiet", title: "Quiet hours: all work on this machine is paused" }, `🌙 quiet until ${m.restricted_until}`) : null,
      group ? el("span", { class: "pill", title: "Shared through this group" }, `shared: ${group.name}`) : null,
      // an administrator's card: what the prompt scheduler noticed of its helper (P3.3.4)
      m.flags && m.flags.overreports ? el("span", { class: "pill warn", title: "Its helper reported more generation time "
        + "than the server observed, by over 10% and a second; its work is credited the observed time. Machine settings…, "
        + "Class and trust" }, `⚠ over-reports its time (${m.flags.overreports.count}×)`) : null,
      m.is_mine || !m.owner ? null : el("span", { class: "muted small" }, `owned by ${m.owner.display_name || m.owner.handle}`)),
    el("div", { class: "muted", style: { marginTop: "4px" } }, reg ? describeConn(reg) : `${PROVIDER[provider] || provider}${helper ? ", through its CITAR helper" : ""}`),
    hardwareLine(m),
    reg ? el("div", { class: "row wrap", style: { marginTop: "2px" } },
      helper || !["owned", "leased"].includes(m.kind) ? null : el("span", { class: "muted small" }, reg.power.idle_w != null
        ? `Power: idle ${reg.power.idle_w} W, +${reg.power.cpu_max_w ?? 0} W CPU, +${reg.power.gpu_max_w ?? 0} W GPU (${reg.power.source})` : "Power: not set"),
      rates,
      el("span", { class: "muted small" }, describeQuiet(reg.restricted_hours, helper ? m.owner_tz : null)),
      !helper && reg.key_status && reg.key_status.backend !== "none" ? el("span", { class: reg.key_status.present ? "good small" : "warn small" },
        reg.key_status.present ? "🔑 key stored" : reg.key_status.locked ? "🔒 key file locked" : "🔑 no key yet") : null)
      : el("div", { class: "muted small", style: { marginTop: "2px" } }, describeQuiet(m.restricted_hours, helper ? m.owner_tz : null)),
    reg && reg.issues.length ? el("ul", { class: "issues" }, ...reg.issues.map((i) => el("li", { class: "warn small" }, i))) : null,
    models,
    el("div", { style: { marginTop: "6px" } }, admissionLine(m)),
    actions(m, data, reload));
  if (reg && m.kind !== "test" && may("serverRates", g)) {
    call("serverRates", g, m.id).then((r) => {
      rates.textContent = m.kind === "api" ? (r.idle.total ? `Fixed ${money(r.idle.total)}/h + tokens` : "Pay per token") :
        `An hour costs ≈ ${money(r.idle.total)} idle · ${money(r.busy.total)} busy`;
    }).catch(() => {});
  }
  // which catalogue models LM Studio has loaded right now, on a machine the server drives
  if (reg && provider === "lmstudio" && !helper && reg.models.some((x) => x.enabled) && may("serverState", g)) {
    call("serverState", g, m.id).then((st) => {
      if (!st.supported) return;
      for (const chip of models.querySelectorAll(".model-chip")) {
        const s = st.models[chip.dataset.key];
        if (s && s.loaded) { chip.classList.add("live"); chip.title += ` — loaded${s.context ? ` (${s.context.toLocaleString()} ctx)` : ""}`; }
      }
    }).catch(() => {});
  }
  return card;
}

function hardwareLine(m) {
  if (m.kind === "api" || m.kind === "test") return null;
  if (m.reg) return m.reg.hardware_summary ? el("div", { style: { marginTop: "2px" } }, m.reg.hardware_summary) : null;
  const h = m.hardware_summary || {};
  const parts = [h.cpu, ...(h.gpus || []), h.ram_gb ? `${h.ram_gb} GB RAM` : null].filter(Boolean);
  return parts.length ? el("div", { class: "small", style: { marginTop: "2px" } }, parts.join(" · ")) : null;
}

// The card's controls: each shown only when its route would take it from this viewer.
function actions(m, data, reload) {
  const g = gateOf(m);
  const reg = m.reg;
  const helper = m.reach === "worker";
  const provider = reg ? reg.connection.provider : m.provider;
  const button = (label, onclick, attrs = {}) => el("button", { class: "small", onclick, ...attrs }, label);
  const out = [];
  if (helper && may("testPoolServer", g)) {
    out.push(button("Test", async (e) => {
      e.target.disabled = true; e.target.textContent = "Testing…";
      try {
        const r = await call("testPoolServer", g, m.id);
        if (r.ok) toast(`${m.name}: ${r.model} replied in ${r.seconds}s`, "good", 7000);
        else toast(`${m.name}: ${r.reason}`, "warn", 9000);
      } catch (err) { toast(err.message, "error", 9000); }
      e.target.disabled = false; e.target.textContent = "Test";
    }, { title: "Send the model a one-line request through the helper" }));
  } else if (!helper && reg && provider !== "none" && may("detectModels", g)) {
    out.push(button("Test connection", async () => {
      try { const r = await call("detectModels", g, m.id); toast(r.ok ? `${m.name}: ${r.models.length} model(s) available` : r.error, r.ok ? "info" : "error"); }
      catch (e) { toast(e.message, "error"); }
    }));
  }
  if (reg && may("saveServer", g)) {
    out.push(button("Connection & models", () => openConnection(reg, reload), { class: "small primary" }));
    if (m.kind !== "api" && m.kind !== "test") out.push(button("Hardware & power", () => openHardware(reg, reload)));
    if (m.kind !== "test") out.push(button("Costs", () => openCosts(reg, reload)));
  }
  if (may("machineSettings", g)) {
    out.push(button("Machine settings…", () => openMachineSettings(m, reload),
      { title: "Slots, model swaps and CPU sharing; what its helper's reports may change; its class and trust tier" }));
  }
  if (may("setQuietHours", g)) out.push(button("Quiet hours", () => openQuietHours(m, reload)));
  if (reg && KEYED.includes(provider) && may("storeKey", g)) out.push(button("API key", () => openKey(reg, reload)));
  if (helper && may("workerStatus", g)) out.push(button("Helper", () => helperDialog(m), { title: "Its worker tokens and how to run the helper" }));
  if (may("updatePoolServer", g)) {
    out.push(button("Sharing", () => sharingDialog(m, data.groups, reload), { title: "The group that shares it" }));
    out.push(button(m.enabled ? "Disable" : "Enable", async () => {
      if (m.enabled && !(await confirmBox(`Disable “${m.name}”?`, "Nothing new runs on it until you enable it again"
        + (helper ? ", and its helper is disconnected at once." : "."))) ) return;
      try { await call("updatePoolServer", g, m.id, { enabled: !m.enabled }); reload(); } catch (e) { toast(e.message, "error"); }
    }));
  }
  if (may("deletePoolServer", g)) {
    out.push(button("Remove", async () => {
      if (!(await confirmBox(`Remove “${m.name}”?`, "Its worker tokens and its stored API key go with it, and a connected helper is "
        + "disconnected. Past usage stays in the ledger but can no longer be costed."))) return;
      try { await call("deletePoolServer", g, m.id); toast("Removed.", "good"); reload(); } catch (e) { toast(e.message, "error"); }
    }, { class: "small danger" }));
  }
  return out.length ? el("div", { class: "row wrap gap", style: { marginTop: "8px" } }, ...out) : null;
}

// Which group shares a machine: everyone the group's access reaches may use it, on those terms.
function sharingDialog(m, groups, reload) {
  const mine = groups.filter((x) => x.can_manage);
  // the group it is in stays the choice until another is picked, also one this page does not list (another
  // member's group, on an administrator's page): saving must never unshare it by accident
  const current = m.group_id && !mine.some((x) => x.id === m.group_id)
    ? (groups.find((x) => x.id === m.group_id) || { id: m.group_id, name: "its owner's group" }) : null;
  const select = el("select", {},
    el("option", { value: "", selected: !m.group_id }, "— not shared —"),
    ...mine.map((x) => el("option", { value: x.id, selected: x.id === m.group_id }, x.name)),
    current ? el("option", { value: current.id, selected: true }, `${current.name} (as it is)`) : null);
  const dialog = modal({
    title: `Sharing “${m.name}”`, narrow: true,
    content: el("div", { class: "col gap" },
      el("p", { class: "muted" }, "A machine is shared through a server group: whoever the group gives access to may use it, on the terms ",
        "of their access (what they may run, the hours, a spend cap). The groups and their access are at the foot of this page."),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Group"), select),
      mine.length ? null : el("p", { class: "muted small" }, "You have no group yet: make one with New group.")),
    footer: [el("button", { onclick: () => dialog.close() }, "Cancel"),
      el("button", { class: "primary", onclick: async () => {
        try {
          await call("updatePoolServer", gateOf(m), m.id, { group_id: select.value || null });
          toast(select.value ? "Shared." : "No longer shared.", "good");
          dialog.close(); reload();
        } catch (e) { toast(e.message, "error"); }
      } }, "Save")],
  });
}

function groupCard(group, machines, reload) {
  const grants = group.grants || [];
  const inside = machines.filter((m) => m.group_id === group.id).map((m) => m.name);
  return el("section", { class: "card", "data-group": group.id },
    el("div", { class: "row between wrap" },
      el("div", { class: "col" },
        el("h3", {}, group.name),
        group.description ? el("span", { class: "muted small" }, group.description) : null,
        el("span", { class: "muted small" },
          `${(group.servers || []).length} machine(s)` + (inside.length ? `: ${inside.join(", ")}` : "")
          + (group.is_mine ? "" : ` · owned by ${group.owner ? group.owner.display_name || group.owner.handle : "someone"}`))),
      el("div", { class: "row gap" },
        may("createGrant", group) ? el("button", { class: "small primary", onclick: () => grantDialog(group, null, reload) }, "Give access") : null,
        may("deletePoolGroup", group) ? el("button", {
          class: "danger small",
          onclick: async () => {
            if (!(await confirmBox(`Delete group “${group.name}”?`,
                  "The machines in it stay registered; they simply stop being shared."))) return;
            try { await call("deletePoolGroup", group, group.id); toast("Deleted.", "good"); reload(); }
            catch (e) { toast(e.message, "error"); }
          },
        }, "Delete") : null)),
    ...(grants.length
      ? grants.map((x) => grantRow(group, x, reload))
      : [el("p", { class: "muted small" },
          group.can_manage ? "Nobody has access yet." : "You have no access terms here.")]));
}

function grantRow(group, grant, reload) {
  const av = grant.availability || {};
  const budget = grant.budget || {};
  return el("div", { class: "grantrow" },
    el("div", { class: "row between wrap" },
      el("div", { class: "col" },
        el("strong", {},
          grant.subject_type === "everyone" ? "Everyone"
            : (grant.subject ? grant.subject.display_name || grant.subject.handle : "somebody")),
        el("span", { class: "muted small" }, `may run ${grant.purpose_label}`)),
      el("div", { class: "row gap" },
        grant.open_now ? el("span", { class: "pill live", title: "Inside the allowed hours" }, "allowed now")
                       : el("span", { class: "pill quiet", title: "Outside the allowed hours" }, "not allowed now"),
        may("updateGrant", group) ? el("button", { class: "small", onclick: () => grantDialog(group, grant, reload) }, "Edit") : null,
        may("revokeGrant", group)
          ? el("button", {
              class: "danger small",
              onclick: async () => {
                if (!(await confirmBox("Revoke this access?", "They lose access immediately."))) return;
                try { await call("revokeGrant", group, grant.id); toast("Revoked.", "good"); reload(); }
                catch (e) { toast(e.message, "error"); }
              },
            }, "Revoke")
          : null)),
    el("div", { class: "row gap wrap small", style: { marginTop: "4px" } },
      el("span", { class: "muted" }, `⏰ allowed ${av.always ? "at any time" : (av.owner_text || "at any time")}`),
      group.is_mine && !av.always ? el("span", { class: "muted" }, "(limits them, not you)") : null,
      // Both readings, because the owner's midnight is somebody else's afternoon.
      av.same_zone ? null : el("span", { class: "muted" }, `(your ${av.viewer_text})`),
      budget.limited
        ? el("span", { class: budget.exhausted ? "bad" : "muted" }, `💳 ${budget.label}`)
        : null,
      // a soft budget past its amount lets the work go on, and says so here (design/phase3-features.md P3.4.6)
      budget.warning ? el("span", { class: "warn" }, budget.warning) : null,
      grant.concurrency ? el("span", { class: "muted" }, `${grant.concurrency} at once`) : null),
    budget.limited
      ? el("div", { class: "meter" },
          el("div", {
            class: `meter-fill ${budget.fraction > 0.9 ? "hot" : ""}`,
            style: { width: `${Math.round(budget.fraction * 100)}%` },
          }))
      : null,
    grant.note ? el("div", { class: "muted small" }, grant.note) : null);
}

// ---------------------------------------------------------------------------- adding things

function newGroupDialog(reload) {
  const name = el("input", { placeholder: "Home GPUs" });
  const description = el("input", { placeholder: "optional" });
  const m = modal({
    title: "New server group", narrow: true,
    content: el("div", { class: "col gap" },
      el("p", { class: "muted" },
        "A group is the unit of sharing: you put machines in it and give access to the group."),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Name"), name),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Description"), description)),
    footer: [el("button", { onclick: () => m.close() }, "Cancel"),
             el("button", {
               class: "primary",
               onclick: async () => {
                 try {
                   await call("createPoolGroup", null, { name: name.value, description: description.value });
                   m.close(); toast("Group created.", "good"); reload();
                 } catch (e) { toast(e.message, "error"); }
               },
             }, "Create")],
  });
  setTimeout(() => name.focus(), 40);
}

// The kinds of machine the viewer may add: a helper machine and the dry run for anyone who may register machines; the
// machines this server connects to itself (an address on its network, an online API) for administrators alone.
function addMachineDialog(groups, reload) {
  const admin = viewer().admin;
  const choice = (title, sub, run) => el("button", { class: "choice", onclick: () => { m.close(); run(); } },
    el("b", {}, title), el("div", { class: "muted small" }, sub));
  const direct = (kind, provider, title, sub) => choice(title, sub, () => addDirectMachine(kind, provider, title, reload));
  const m = modal({
    title: "Add a machine", narrow: true,
    content: el("div", { class: "col" },
      choice("🖥 A machine running the CITAR helper", "LM Studio or Ollama on your computer, wherever it is: the helper connects out to this "
        + "server, so no ports need opening.", () => helperMachineDialog(groups, reload)),
      direct("test", "dryrun", "🧪 Dry run", "A pretend model for checking setups: it dials nothing and costs nothing."),
      admin ? el("h3", { class: "section" }, "Machines this server connects to itself") : null,
      admin ? direct("owned", "lmstudio", "🖥 A PC running LM Studio", "This PC, another linked through LM Studio's LM Link, or one on the network") : null,
      admin ? direct("owned", "ollama", "🖥 A PC running Ollama", "Local or network Ollama server") : null,
      admin ? direct("owned", "none", "🖥 A PC without a model runtime", "E.g. the machine that only runs CITAR's engine and bots") : null,
      admin ? direct("leased", "openai_compatible", "☁ A rented or cloud machine", "vLLM, llama.cpp server, LM Studio… on hardware you rent (fixed monthly and/or hourly costs)") : null,
      admin ? direct("api", "anthropic", "🔑 Anthropic API (Claude)", "Pay per token; prices filled in for current Claude models") : null,
      admin ? direct("api", "openai_compatible", "🔑 Another online API", "OpenAI-compatible service billed per token") : null,
      admin ? null : el("p", { class: "muted small" }, "A machine this server connects to itself (a PC on its network, an online API) is an "
        + "administrator's to add: it would make the server reach an address on its own network.")),
  });
}

function helperMachineDialog(groups, reload) {
  const name = el("input", { placeholder: "Desktop with the 4090" });
  const provider = el("select", {},
    el("option", { value: "lmstudio" }, "LM Studio"),
    el("option", { value: "ollama" }, "Ollama"),
    el("option", { value: "openai_compatible" }, "OpenAI-compatible endpoint"));
  const kind = el("select", {},
    el("option", { value: "owned" }, "owned hardware"),
    el("option", { value: "leased" }, "leased / rented"));
  const group = el("select", {},
    el("option", { value: "" }, "— not shared —"),
    ...groups.filter((g) => g.is_mine).map((g) => el("option", { value: g.id }, g.name)));
  const concurrent = el("input", { type: "number", min: "1", value: "1", style: { width: "90px" } });

  const m = modal({
    title: "Add a machine with the CITAR helper",
    content: el("div", { class: "col gap" },
      el("p", { class: "muted" },
        "Register the machine here, then run the CITAR helper on it. The helper connects outward to "
        + "this server, so you do not need a port forward, a static address or a firewall hole."),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Name"), name),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "What serves the model"), provider),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Kind"), kind),
      el("label", { class: "field" },
        el("span", { class: "field-label" }, "Group"), group,
        el("span", { class: "muted small" }, "Leave unshared to keep it to yourself for now.")),
      el("label", { class: "field" },
        el("span", { class: "field-label" }, "Requests at once"), concurrent)),
    footer: [el("button", { onclick: () => m.close() }, "Cancel"),
             el("button", {
               class: "primary",
               onclick: async () => {
                 try {
                   const created = await call("createPoolServer", { isNew: true }, {
                     config: {
                       name: name.value, kind: kind.value,
                       connection: { provider: provider.value,
                                     max_parallel: parseInt(concurrent.value, 10) || 1 },
                     },
                     group_id: group.value || null, reach: "worker",
                   });
                   m.close();
                   toast("Machine added — now issue a worker token for its helper.", "good");
                   reload();
                   helperDialog(created);
                 } catch (e) { toast(e.message, "error"); }
               },
             }, "Add machine")],
  });
  setTimeout(() => name.focus(), 40);
}

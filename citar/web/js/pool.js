// The server pool: your machines, the groups you share them in, and who may use them when.
//
// The availability editor is the interesting part. A window is entered in the *owner's* zone and
// shown to everyone else in theirs, because "midnight to 6am" means the owner's midnight and a
// viewer in Tokyo needs to know that is their afternoon.
import { api } from "./api.js";
import { el, clear, toast, modal, confirmBox } from "./util.js";
import { pageHeader } from "./nav.js";
import * as auth from "./auth.js";

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const PURPOSES = [
  ["game", "Ordinary games"], ["benchmark", "Benchmarks"], ["probe", "Probes"],
  ["scenario", "Scenarios"], ["lab", "Lab runs"],
];

function hhmm(minutes) {
  if (minutes >= 1440) return "24:00";
  return `${String(Math.floor(minutes / 60)).padStart(2, "0")}:${String(minutes % 60).padStart(2, "0")}`;
}

function minutesOf(text) {
  const [h, m] = String(text || "0:00").split(":");
  return (parseInt(h, 10) || 0) * 60 + (parseInt(m, 10) || 0);
}

function statusPill(server) {
  if (!server.enabled) return el("span", { class: "pill" }, "disabled");
  if (server.online === true) return el("span", { class: "pill live" }, "● online");
  if (server.online === false) return el("span", { class: "pill quiet" }, "offline");
  return el("span", { class: "pill" }, "direct");
}

function admissionLine(server) {
  const a = server.admission || {};
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
        "No windows — available at any time."));
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
        `Entered in your zone (${ownerTz}). Other people see these translated into theirs.`),
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
        ? { period: budgetPeriod.value, amount, currency: "USD", behavior: budgetBehavior.value }
        : null,
    };
    try {
      if (existing) await api.updateGrant(existing.id, body);
      else await api.createGrant(group.id, body);
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
        el("span", { class: "muted small" }, "Nothing ticked means every kind of work.")),
      el("div", { class: "field" },
        el("span", { class: "field-label" }, "When"), windows.node),
      el("label", { class: "field" },
        el("span", { class: "field-label" }, "Simultaneous games"), concurrency,
        el("span", { class: "muted small" }, "0 uses the server's own limit.")),
      el("div", { class: "field" },
        el("span", { class: "field-label" }, "Spend cap"),
        el("div", { class: "row gap wrap" }, budgetAmount, budgetPeriod),
        budgetBehavior,
        el("span", { class: "muted small" },
          "Accounting only — nobody is charged and no money moves. The cap is measured from the "
          + "usage ledger and stops work being scheduled. Periods end at your midnight.")),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Note"), note)),
    footer: [el("button", { onclick: () => m.close() }, "Cancel"),
             el("button", { class: "primary", onclick: save }, "Save")],
  });
}

// ---------------------------------------------------------------------------- worker dialog

async function workerDialog(server) {
  const body = el("div", { class: "col gap" }, el("span", { class: "muted" }, "Loading…"));
  const m = modal({ title: `Worker for “${server.name}”`, content: body });

  const refresh = async () => {
    clear(body);
    try {
      const info = await api.workerStatus(server.id);
      body.append(
        el("div", { class: "row gap" },
          info.online ? el("span", { class: "pill live" }, "● connected")
                      : el("span", { class: "pill quiet" }, "not connected"),
          info.worker ? el("span", { class: "muted" },
            `${info.worker.hostname} · ${info.worker.models.length} models · `
            + `${info.worker.in_flight}/${info.worker.max_concurrent} busy`) : null),
        el("p", { class: "muted" },
          "The worker dials out to this server, so nothing on your machine needs to be reachable "
          + "from the internet. Run this where the model is:"),
        el("pre", { class: "code-block" }, info.command),
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
                        "Any worker using it disconnects immediately and cannot reconnect."))) return;
                  try { await api.revokeWorkerToken(server.id, t.id); refresh(); }
                  catch (e) { toast(e.message, "error"); }
                },
              }, "Revoke")))
          : [el("span", { class: "muted" }, "No tokens yet.")]),
        el("button", {
          class: "primary",
          onclick: async () => {
            try {
              const created = await api.createWorkerToken(server.id, "");
              clear(body);
              body.append(
                el("h3", {}, "New worker token"),
                el("p", { class: "warn-box" }, created.warning),
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
}

// ---------------------------------------------------------------------------- page

export async function renderPool(root) {
  if (!auth.user()) { auth.renderSignInWall(root); return {}; }
  clear(root);
  root.appendChild(pageHeader("pool"));
  const page = el("div", { class: "page" });
  root.appendChild(page);
  const me = auth.user();

  const reload = () => renderPool(root);

  let groups = [];
  let servers = [];
  try {
    [{ groups }, { servers }] = await Promise.all([api.poolGroups(), api.poolServers()]);
  } catch (e) {
    page.appendChild(el("p", { class: "bad" }, e.message));
    return {};
  }

  // ------------------------------------------------------------------ my servers
  const mine = servers.filter((s) => s.is_mine);
  const shared = servers.filter((s) => !s.is_mine);

  page.appendChild(el("div", { class: "row between wrap" },
    el("h1", {}, "Servers"),
    auth.can("register_servers")
      ? el("div", { class: "row gap" },
          el("button", { onclick: () => newGroupDialog(reload) }, "New group"),
          el("button", { class: "primary", onclick: () => newServerDialog(groups, reload) },
             "Add a server"))
      : null));

  if (!auth.can("register_servers")) {
    page.appendChild(el("p", { class: "muted" },
      "Your account cannot register servers yet. New accounts gain this once an administrator "
      + "clears them."));
  }

  page.appendChild(el("h2", {}, "Your machines"));
  if (!mine.length) {
    page.appendChild(el("p", { class: "muted" },
      "None yet. Add a server, then run citar-worker on the machine that has the model — it "
      + "connects outward, so you do not need to open any ports."));
  }
  for (const server of mine) {
    page.appendChild(serverCard(server, groups, reload));
  }

  if (shared.length) {
    page.appendChild(el("h2", {}, "Shared with you"));
    for (const server of shared) page.appendChild(serverCard(server, groups, reload));
  }

  // ------------------------------------------------------------------ groups
  page.appendChild(el("h2", {}, "Groups and access"));
  if (!groups.length) {
    page.appendChild(el("p", { class: "muted" },
      "A group is how you share machines. Put servers in one, then grant access to it."));
  }
  for (const group of groups) {
    page.appendChild(groupCard(group, reload));
  }

  return {};
}

function serverCard(server, groups, reload) {
  const a = server.admission || {};
  return el("section", { class: "card" },
    el("div", { class: "row between wrap" },
      el("div", { class: "row gap" },
        el("h3", {}, server.name), statusPill(server),
        server.is_mine ? null : el("span", { class: "muted small" },
          `owned by ${server.owner ? server.owner.display_name : "someone else"}`)),
      el("div", { class: "row gap" },
        server.can_manage
          ? el("button", { class: "small", onclick: () => workerDialog(server) }, "Worker")
          : null,
        el("button", {
          class: "small",
          onclick: async (e) => {
            e.target.disabled = true;
            e.target.textContent = "Testing…";
            try {
              const r = await api.testPoolServer(server.id);
              if (r.ok) {
                toast(`${server.name}: ${r.model} replied in ${r.seconds}s`, "good", 7000);
              } else {
                toast(`${server.name}: ${r.reason}`, "warn", 9000);
              }
            } catch (err) { toast(err.message, "error", 9000); }
            e.target.disabled = false;
            e.target.textContent = "Test";
          },
        }, "Test"),
        server.can_manage
          ? el("button", {
              class: "danger small",
              onclick: async () => {
                if (!(await confirmBox(`Remove “${server.name}”?`,
                      "Its worker tokens are revoked. Games already running are unaffected."))) return;
                try { await api.deletePoolServer(server.id); toast("Removed.", "good"); reload(); }
                catch (e) { toast(e.message, "error"); }
              },
            }, "Remove")
          : null)),
    el("div", { class: "row gap wrap small muted" },
      el("span", {}, server.provider),
      server.hardware_summary && server.hardware_summary.cpu
        ? el("span", {}, server.hardware_summary.cpu) : null,
      (server.hardware_summary && (server.hardware_summary.gpus || []).length)
        ? el("span", {}, server.hardware_summary.gpus.join(", ")) : null,
      el("span", {}, `${(server.models || []).length} model(s)`)),
    (server.models || []).length
      ? el("div", { class: "row gap wrap", style: { marginTop: "6px" } },
          ...server.models.slice(0, 10).map((m) => el("span", { class: "chip" }, m.label || m.key)))
      : null,
    el("div", { style: { marginTop: "8px" } }, admissionLine(server)));
}

function groupCard(group, reload) {
  const grants = group.grants || [];
  return el("section", { class: "card" },
    el("div", { class: "row between wrap" },
      el("div", { class: "col" },
        el("h3", {}, group.name),
        group.description ? el("span", { class: "muted small" }, group.description) : null,
        el("span", { class: "muted small" },
          `${(group.servers || []).length} server(s)`
          + (group.is_mine ? "" : ` · owned by ${group.owner ? group.owner.display_name : "someone"}`))),
      group.can_manage
        ? el("div", { class: "row gap" },
            el("button", { class: "small primary", onclick: () => grantDialog(group, null, reload) },
               "Give access"),
            el("button", {
              class: "danger small",
              onclick: async () => {
                if (!(await confirmBox(`Delete group “${group.name}”?`,
                      "The servers in it stay registered; they simply stop being shared."))) return;
                try { await api.deletePoolGroup(group.id); toast("Deleted.", "good"); reload(); }
                catch (e) { toast(e.message, "error"); }
              },
            }, "Delete"))
        : null),
    ...(grants.length
      ? grants.map((g) => grantRow(group, g, reload))
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
            : (grant.subject ? grant.subject.display_name : "somebody")),
        el("span", { class: "muted small" }, `may run ${grant.purpose_label}`)),
      el("div", { class: "row gap" },
        grant.open_now ? el("span", { class: "pill live" }, "open now")
                       : el("span", { class: "pill quiet" }, "closed now"),
        group.can_manage
          ? el("button", { class: "small", onclick: () => grantDialog(group, grant, reload) }, "Edit")
          : null,
        group.can_manage
          ? el("button", {
              class: "danger small",
              onclick: async () => {
                if (!(await confirmBox("Revoke this access?", "They lose access immediately."))) return;
                try { await api.revokeGrant(grant.id); toast("Revoked.", "good"); reload(); }
                catch (e) { toast(e.message, "error"); }
              },
            }, "Revoke")
          : null)),
    el("div", { class: "row gap wrap small", style: { marginTop: "4px" } },
      el("span", { class: "muted" }, `⏰ ${av.owner_text || "Any time"}`),
      // Both readings, because the owner's midnight is somebody else's afternoon.
      av.same_zone ? null : el("span", { class: "muted" }, `(your ${av.viewer_text})`),
      budget.limited
        ? el("span", { class: budget.exhausted ? "bad" : "muted" }, `💳 ${budget.label}`)
        : null,
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

// ---------------------------------------------------------------------------- dialogs

function newGroupDialog(reload) {
  const name = el("input", { placeholder: "Home GPUs" });
  const description = el("input", { placeholder: "optional" });
  const m = modal({
    title: "New server group", narrow: true,
    content: el("div", { class: "col gap" },
      el("p", { class: "muted" },
        "A group is the unit of sharing: you put machines in it and grant access to the group."),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Name"), name),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Description"), description)),
    footer: [el("button", { onclick: () => m.close() }, "Cancel"),
             el("button", {
               class: "primary",
               onclick: async () => {
                 try {
                   await api.createPoolGroup({ name: name.value, description: description.value });
                   m.close(); toast("Group created.", "good"); reload();
                 } catch (e) { toast(e.message, "error"); }
               },
             }, "Create")],
  });
  setTimeout(() => name.focus(), 40);
}

function newServerDialog(groups, reload) {
  const name = el("input", { placeholder: "Desktop with the 4090" });
  const provider = el("select", {},
    el("option", { value: "lmstudio" }, "LM Studio"),
    el("option", { value: "ollama" }, "Ollama"),
    el("option", { value: "openai_compatible" }, "OpenAI-compatible endpoint"),
    el("option", { value: "anthropic" }, "Anthropic API"));
  const kind = el("select", {},
    el("option", { value: "owned" }, "owned hardware"),
    el("option", { value: "leased" }, "leased / rented"),
    el("option", { value: "api" }, "paid API"));
  const group = el("select", {},
    el("option", { value: "" }, "— not shared —"),
    ...groups.filter((g) => g.is_mine).map((g) => el("option", { value: g.id }, g.name)));
  const concurrent = el("input", { type: "number", min: "1", value: "1", style: { width: "90px" } });

  const m = modal({
    title: "Add a server",
    content: el("div", { class: "col gap" },
      el("p", { class: "muted" },
        "Register the machine here, then run citar-worker on it. The worker connects outward to "
        + "this server, so you do not need a port forward, a static address or a firewall hole."),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Name"), name),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "What serves the model"), provider),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Kind"), kind),
      el("label", { class: "field" },
        el("span", { class: "field-label" }, "Group"), group,
        el("span", { class: "muted small" }, "Leave unshared to keep it to yourself for now.")),
      el("label", { class: "field" },
        el("span", { class: "field-label" }, "Simultaneous requests"), concurrent)),
    footer: [el("button", { onclick: () => m.close() }, "Cancel"),
             el("button", {
               class: "primary",
               onclick: async () => {
                 try {
                   const created = await api.createPoolServer({
                     config: {
                       name: name.value, kind: kind.value,
                       connection: { provider: provider.value,
                                     max_parallel: parseInt(concurrent.value, 10) || 1 },
                     },
                     group_id: group.value || null, reach: "worker",
                   });
                   m.close();
                   toast("Server added — now issue a worker token.", "good");
                   workerDialog(created);
                 } catch (e) { toast(e.message, "error"); }
               },
             }, "Add server")],
  });
  setTimeout(() => name.focus(), 40);
}

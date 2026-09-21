// The operator console: what this deployment is, what is missing, and what each gap costs.
//
// A public CITAR has about a dozen settings that can be individually wrong in ways nothing
// complains about at the time — mail that goes nowhere, a captcha that is configured but not
// checked, open registration with no protection. Each of those surfaces days later as somebody
// unable to sign up. This page turns them into a list you can read in ten seconds, derived from
// the settings the server is actually using rather than from a document that can drift.
//
// Settings that need a restart (they come from the environment) are shown but not editable, with
// the variable to change. Settings the server can change while running are editable here, and each
// change is written to the audit log.
import { api } from "./api.js";
import { el, clear, toast } from "./util.js";
import { pageHeader } from "./nav.js";

export async function renderConsole(root) {
  clear(root);
  root.appendChild(pageHeader("console"));
  const page = el("div", { class: "page console-page" });
  root.appendChild(page);

  let data;
  try {
    data = await api.setupConsole();
  } catch (e) {
    page.appendChild(el("p", { class: "error" },
      e.name === "Forbidden" ? "This page is for administrators." : e.message));
    return { destroy() {} };
  }

  page.append(
    el("header", { class: "console-head" },
      el("h1", {}, "Server"),
      el("span", { class: `pill ${data.mode === "server" ? "live" : ""}` },
        data.mode === "server" ? "public server" : "local mode"),
      el("span", { class: "muted" }, `CITAR ${data.version}`)),
    checksSection(data),
    policySection(data),
    pathsSection(data));

  return { destroy() {} };
}

/** The health list: one row per thing that can be misconfigured. */
function checksSection(data) {
  const list = el("div", { class: "check-list" });
  for (const check of data.checks) {
    list.append(
      el("div", { class: `check ${check.severity}` },
        el("span", { class: "check-mark" },
          check.ok ? "✓" : (check.severity === "error" ? "✗" : "!")),
        el("div", { class: "grow" },
          el("div", { class: "check-title" }, check.title,
            el("span", { class: "muted" }, ` — ${check.detail}`)),
          !check.ok && check.fix ? el("div", { class: "muted small" }, check.fix) : null)));
  }
  return el("section", { class: "card" }, el("h2", {}, "Configuration"), list, mailTest(data));
}

/**
 * Send a real message through the configured SMTP.
 *
 * Worth a button rather than a note in a runbook: the other way to find out that mail is broken is
 * somebody failing to reset their password, by which time the failure is silent and days old.
 */
function mailTest(data) {
  const emailConfigured = data.checks.some((c) => c.title === "E-mail" && c.ok);
  if (!emailConfigured) return null;
  const to = el("input", { type: "email", placeholder: "you@example.com" });
  const button = el("button", {
    onclick: async () => {
      if (!to.value.trim()) return toast("Give an address to send to.", "warn");
      button.disabled = true;
      try {
        await api.setupTestEmail(to.value.trim());
        toast("Sent. Check the inbox, and the spam folder.", "info", 6000);
      } catch (e) {
        toast(e.message, "error", 9000);
      } finally {
        button.disabled = false;
      }
    },
  }, "Send test message");
  return el("div", { class: "row gap mail-test" },
    el("label", { class: "muted" }, "Test e-mail to"), to, button);
}

/** Runtime policy: the settings an administrator changes without a restart. */
function policySection(data) {
  const policy = data.policy || {};
  const rows = [
    ["registration", "Who may sign up", "select", ["open", "invite", "closed"],
     "Invite-only needs neither e-mail nor a captcha, which is why it is the safe default."],
    ["probation_enabled", "New accounts start on probation", "bool", null,
     "A new account can play and watch, but cannot register hardware, invite anyone or publish."],
    ["default_invite_quota", "Invitations each member may send", "number", null,
     "Above zero, the community can grow without an administrator."],
    ["default_max_concurrent_games", "Games one account may run at once", "number", null,
     "Games are CPU-bound. On a small machine this is the lever that keeps it responsive."],
    ["aggregate_min_contributors", "Contributors before a pooled average is shown", "number", null,
     "Below this, a public number could be read back as one account's private results."],
    ["closed_message", "Message on the sign-in page", "text", null,
     "Shown when registration is invite-only or closed."],
  ];

  const table = el("div", { class: "policy-list" });
  for (const [key, label, kind, options, help] of rows) {
    if (!(key in policy)) continue;
    table.append(
      el("div", { class: "policy-row" },
        el("div", { class: "grow" },
          el("div", {}, label),
          el("div", { class: "muted small" }, help)),
        control(key, kind, options, policy[key])));
  }
  return el("section", { class: "card" },
    el("h2", {}, "Policy"),
    el("p", { class: "muted" }, "Changes take effect immediately and are written to the audit log."),
    table);
}

function control(key, kind, options, value) {
  const save = async (next) => {
    try {
      await api.setPolicy(key, next);
      toast("Saved.", "info", 1500);
    } catch (e) {
      toast(e.message, "error");
    }
  };

  if (kind === "select") {
    const node = el("select", { onchange: () => save(node.value) },
      ...options.map((o) => el("option", { value: o, selected: o === value }, o)));
    return node;
  }
  if (kind === "bool") {
    const node = el("input", { type: "checkbox", checked: !!value, onchange: () => save(node.checked) });
    return node;
  }
  if (kind === "number") {
    const node = el("input", {
      type: "number", value: value ?? 0, style: { width: "6em" },
      onchange: () => save(Number(node.value)),
    });
    return node;
  }
  const node = el("input", { value: value ?? "", onchange: () => save(node.value) });
  return node;
}

/** Where the files are, which is the first question of every support conversation. */
function pathsSection(data) {
  const list = el("dl", { class: "paths" });
  for (const [label, value] of Object.entries(data.paths || {})) {
    list.append(el("dt", {}, label), el("dd", {}, el("code", {}, value)));
  }
  return el("section", { class: "card" },
    el("h2", {}, "Files"),
    el("p", { class: "muted" },
      "Back up the state directory and the environment file. Everything else can be reinstalled."),
    list);
}

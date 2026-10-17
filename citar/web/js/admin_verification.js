// The verification page for administrators (#/admin/verification; design/phase4-helper.md P4.5.10, P4.5.11): the
// verification settings (bounded as the server bounds them), the quarantined machines with their evidence (the
// verdict in words, the round where two chains parted, the results withdrawn) and their quarantined owners, each with
// Release; the recent mismatches and withdrawals; the verifiers; and the machines the credits' bounds or the class
// check flagged, with their measured speed. Linked from the admin users page.
//
// Administrators only: for anyone else the server answers 404 and the router shows "not found".
import { api } from "./api.js";
import { el, clear, toast } from "./util.js";
import { pageHeader } from "./nav.js";
import {
  SETTINGS, evidenceText, flagText, settingValue, settingsChanges, speedText,
} from "./trustrules.js";

function when(iso) { return iso ? new Date(iso).toLocaleString() : "—"; }
function who(o) { return o ? `@${o.handle}` : "—"; }

export async function renderAdminVerification(root) {
  let data = await api.adminVerification();          // a 404 (not an administrator) reaches the router: "not found"
  const page = el("div", { class: "lobby admin-verification" });
  root.appendChild(page);
  page.appendChild(pageHeader("admin-users"));
  const body = el("div", { class: "col" });
  page.append(
    el("p", { class: "muted small" }, el("a", { href: "#/admin/users" }, "← Users"), " · Helpers' CPU work is checked "
      + "by replicas on this server's job host and on machines an administrator marks verifier; a mismatch "
      + "quarantines the machine (its owner too on tampering) and withdraws what it gave unverified."),
    body);

  const reload = async () => {
    try { data = await api.adminVerification(); draw(); } catch (e) { toast(e.message, "error"); }
  };

  function draw() {
    clear(body).append(settingsCard(data, reload), quarantinedCard(data, reload), mismatchesCard(data),
      withdrawalsCard(data), verifiersCard(data), flaggedCard(data));
  }
  draw();
  return { destroy() {} };
}

// ---------------------------------------------------------------- the settings
function settingsCard(data, reload) {
  const edited = { ...data.settings };
  const err = el("p", { class: "bad small" });
  const rows = SETTINGS.map(([key, label, kind, hint]) => {
    const bounds = data.bounds[key];
    const note = el("span", { class: "muted small" }, `${hint} Default ${data.defaults[key]}.`);
    const input = el("input", {
      type: "number", step: kind === "count" ? 1 : 0.01, min: bounds[0], max: bounds[1], value: data.settings[key],
      "aria-label": label, style: { width: "110px" },
      oninput: (e) => {
        const v = settingValue(kind, e.target.value, bounds);
        edited[key] = v;
        note.className = v == null ? "bad small" : "muted small";
        note.textContent = v == null
          ? `${kind === "count" ? "A whole number" : "A number"} from ${bounds[0]} to ${bounds[1].toLocaleString()}.`
          : `${hint} Default ${data.defaults[key]}.`;
      },
    });
    return el("tr", {}, el("td", {}, el("b", {}, label), el("div", {}, el("code", { class: "small" }, key))),
      el("td", {}, input), el("td", {}, note));
  });
  const save = el("button", { class: "primary", onclick: async () => {
    err.textContent = "";
    if (SETTINGS.some(([key]) => edited[key] == null)) { err.textContent = "Some values are out of their bounds."; return; }
    const change = settingsChanges(data.settings, edited);
    if (!change) { toast("Nothing changed."); return; }
    save.disabled = true;
    try { await api.saveVerification(change); toast("Saved: in force at once.", "good"); reload(); }
    catch (e) { err.textContent = e.message; save.disabled = false; }
  } }, "Save");
  const tiers = Object.entries(data.tiers || {}).map(([t, n]) => `${n} ${t}`).join(" · ") || "no helper machine yet";
  return el("section", { class: "card" },
    el("h2", {}, "Settings"),
    el("p", { class: "muted small" }, `Helper machines: ${tiers}.`),
    el("div", { class: "table-wrap" }, el("table", { class: "list" }, el("tbody", {}, ...rows))),
    err, el("div", { class: "row gap" }, save));
}

// ---------------------------------------------------------------- the quarantined
function quarantinedCard(data, reload) {
  const { machines, owners } = data.quarantined;
  const release = async (label, fn) => {
    try { await fn(); toast(`${label} released.`, "good"); reload(); } catch (e) { toast(e.message, "error"); }
  };
  const machineRows = machines.map((m) => el("tr", { "data-machine": m.machine },
    el("td", {}, el("a", { href: `#/pool/${encodeURIComponent(m.machine)}` }, m.name)),
    el("td", {}, who(m.owner), m.owner && m.owner.trust_tier === "quarantined"
      ? el("span", { class: "pill bad small", title: "Its owner is quarantined too (a tampering verdict)" }, " owner quarantined") : null),
    el("td", { class: "evidence" }, evidenceText(m.evidence),
      (m.evidence && m.evidence.withdrawn_jobs || []).length
        ? el("div", { class: "muted small" }, "Withdrawn: ", m.evidence.withdrawn_jobs.map((w) => `${w.exp} #${w.i}`).join(", "))
        : null),
    el("td", { class: "nowrap" }, when(m.quarantined_at)),
    el("td", {}, el("button", { class: "small", title: "Back to new: its counters reset, its standing from now",
      onclick: () => release(m.name, () => api.setMachineTrust(m.machine, { trust_tier: "new" })) }, "Release"))));
  const ownerRows = owners.map((u) => el("tr", {},
    el("td", {}, el("a", { href: `#/u/${encodeURIComponent(u.handle)}` }, `@${u.handle}`)),
    el("td", { class: "evidence" }, u.why || "quarantined by an administrator"),
    el("td", { class: "nowrap" }, when(u.at)),
    el("td", {}, el("button", { class: "small", title: "Their machines may lend CPU again (each machine is released apart)",
      onclick: () => release(`@${u.handle}`, () => api.patchAdminUser(u.id, { trust_tier: "new" })) }, "Release"))));
  return el("section", { class: "card" },
    el("h2", {}, "Quarantined"),
    machines.length ? el("div", { class: "table-wrap" }, el("table", { class: "list" },
      el("thead", {}, el("tr", {}, ...["Machine", "Owner", "Evidence", "Since", ""].map((h) => el("th", {}, h)))),
      el("tbody", {}, ...machineRows))) : el("p", { class: "muted" }, "No machine is quarantined."),
    el("h3", {}, "Owners"),
    owners.length ? el("div", { class: "table-wrap" }, el("table", { class: "list" },
      el("thead", {}, el("tr", {}, ...["Account", "Why", "Since", ""].map((h) => el("th", {}, h)))),
      el("tbody", {}, ...ownerRows))) : el("p", { class: "muted" }, "No owner is quarantined."));
}

// ---------------------------------------------------------------- mismatches, withdrawals
function mismatchesCard(data) {
  const rows = data.mismatches.map((m) => el("tr", {},
    el("td", { class: "nowrap" }, when(m.at)),
    el("td", {}, el("a", { href: `#/pool/${encodeURIComponent(m.machine)}` }, m.name || m.machine)),
    el("td", {}, `${m.exp} #${m.i}`, m.role !== "primary" ? el("span", { class: "muted small" }, ` (${m.role})`) : null),
    el("td", {}, el("code", {}, m.verdict)),
    el("td", { class: "evidence" }, m.words || "", m.against ? el("span", { class: "muted small" }, ` against ${m.against}`) : null)));
  return el("section", { class: "card" }, el("h2", {}, "Recent mismatches"),
    rows.length ? el("div", { class: "table-wrap" }, el("table", { class: "list" },
      el("thead", {}, el("tr", {}, ...["When", "Machine", "Game", "Verdict", "Evidence"].map((h) => el("th", {}, h)))),
      el("tbody", {}, ...rows))) : el("p", { class: "muted" }, "None."));
}

function withdrawalsCard(data) {
  const rows = data.withdrawals.map((w) => el("tr", {},
    el("td", { class: "nowrap" }, when(w.at)),
    el("td", {}, `${w.exp} #${w.i}`),
    el("td", {}, w.machine ? el("a", { href: `#/pool/${encodeURIComponent(w.machine)}` }, w.name || w.machine) : "—"),
    el("td", { class: "evidence" }, w.why || "")));
  return el("section", { class: "card" }, el("h2", {}, "Recent withdrawals"),
    el("p", { class: "muted small" }, "Results taken back from the results files and the ratings, their credits "
      + "rejected; each game is played again elsewhere."),
    rows.length ? el("div", { class: "table-wrap" }, el("table", { class: "list" },
      el("thead", {}, el("tr", {}, ...["When", "Game", "Machine", "Why"].map((h) => el("th", {}, h)))),
      el("tbody", {}, ...rows))) : el("p", { class: "muted" }, "None."));
}

// ---------------------------------------------------------------- verifiers, flagged machines
function verifiersCard(data) {
  const rows = data.verifiers.map((v) => {
    const latest = (v.latest || []).map((r) => (r.void ? "void" : r.verdict)).join(", ");
    const state = v.kind === "local"
      ? (v.verifiable === false ? `its self-check failed: ${v.why || "it verifies nobody"}` : "verifies")
      : `${v.online ? "online" : "offline"}${v.verifiable === false ? ", its self-check failed" : ""}`;
    return el("tr", {},
      el("td", {}, v.kind === "local" ? el("b", {}, v.name || "this server")
        : el("a", { href: `#/pool/${encodeURIComponent(v.machine)}` }, v.name)),
      el("td", {}, v.kind === "local" ? "this server's job host" : who(v.owner)),
      el("td", {}, state),
      el("td", {}, `${(v.in_flight || []).length}`),
      el("td", { class: "small" }, latest || "—"));
  });
  return el("section", { class: "card" }, el("h2", {}, "Verifiers"),
    el("p", { class: "muted small" }, "This server's job host verifies while its canary and self-check pass; a trusted "
      + "machine verifies once an administrator marks it (its card, or Machine settings…, Class and trust). A new "
      + "machine's results are replayed on this server only."),
    el("div", { class: "table-wrap" }, el("table", { class: "list" },
      el("thead", {}, el("tr", {}, ...["Verifier", "Whose", "State", "Replaying", "Latest verdicts"].map((h) => el("th", {}, h)))),
      el("tbody", {}, ...rows))));
}

function flaggedCard(data) {
  const rows = data.flagged.map((m) => el("tr", {},
    el("td", {}, el("a", { href: `#/pool/${encodeURIComponent(m.machine)}` }, m.name)),
    el("td", {}, who(m.owner)),
    el("td", {}, m.tier),
    el("td", { class: "small" }, speedText(m.speed, null).replace("Measured speed: ", "")),
    el("td", {}, ...Object.entries(m.flags).map(([k, f]) => {
      const t = flagText(k, f);
      return t ? el("span", { class: `pill ${t[0]} small`, title: t[2] }, t[1]) : null;
    }))));
  return el("section", { class: "card" }, el("h2", {}, "Flagged machines"),
    el("p", { class: "muted small" }, "What the credits' bounds and the class check noticed: CPU over-reported (blocks "
      + "promotion), capped at the reference (informational), a measured speed out of line with the CPU class."),
    rows.length ? el("div", { class: "table-wrap" }, el("table", { class: "list" },
      el("thead", {}, el("tr", {}, ...["Machine", "Owner", "Tier", "Measured speed", "Flags"].map((h) => el("th", {}, h)))),
      el("tbody", {}, ...rows))) : el("p", { class: "muted" }, "None."));
}

// The machine settings dialog (design/phase3-features.md P3.8): "Machine settings…" on a machine's card on the Servers
// page (pool.js), for its owner and administrators. Three tabs:
// - Capabilities: whether it takes model work, its LLM slots, whether pooled prompts may make it swap models, and its
//   CPU sharing for the CPU jobs of CITAR's next helper;
// - Hardware, models and power: for each, auto or manual (what its helper's report may change), the last report with
//   its time and how the configuration differs from it, and Apply (copy it in, whatever the mode);
// - Class and trust: the hardware classes it is priced by and its trust tier, shown to its owner, set by
//   administrators (with what the prompt scheduler noticed of its helper over-reporting its time).
// Every request goes through servers.js's `call`, which asks machines.js's gates first, as the rest of the page does.
import { el, clear, toast, modal } from "./util.js";
import { call, may } from "./servers.js";
import { MODE_TEXT, SECTIONS, classText, inEffectText, settingsBody, sharesCpu, wholeIn } from "./machines.js";

const NAMES = { hardware: "Hardware", models: "Models", power: "Power" };
const WORK = {
  lab_game: "Lab games (bot experiments)",
  ranking_game: "Ranking games (the bots' ladder)",
  reference_game: "Reference games (the comparisons the bots are held to)",
};
const TIERS = {
  new: "new: nothing known of it yet",
  provisional: "provisional: so far so good",
  trusted: "trusted",
  quarantined: "quarantined: its work is not to be relied on",
};
const clone = (x) => JSON.parse(JSON.stringify(x));
const when = (iso) => (iso ? new Date(iso).toLocaleString() : "never");

/** Open the dialog for `m`, a card's machine (the pool's listing, with its registry view as `reg` when the viewer
 *  manages it). `onSaved` (the page's refresh) runs after each change. */
export async function openMachineSettings(m, onSaved) {
  const target = m.reg || m;
  let current;
  try {
    current = await call("machineSettings", target, m.id);
  } catch (e) {
    toast(e.message, "error");
    return null;
  }
  // the dialog's own copy of what Save sends; Apply and the administrators' controls act at once and keep it
  let edited = clone(current);
  let tab = "capabilities";
  const tabs = el("div", { class: "tabbar" });
  const body = el("div", { class: "col" });
  const pages = { capabilities: ["Capabilities", capabilities], scans: ["Hardware, models and power", scans],
                  trust: ["Class and trust", classAndTrust] };

  const draw = () => {
    clear(tabs).append(...Object.entries(pages).map(([key, [title]]) =>
      el("button", { class: tab === key ? "active" : "", onclick: () => { tab = key; draw(); } }, title)));
    clear(body).appendChild(pages[tab][1]());
  };
  // a fresh read from the server: what Save has not sent yet stays as it was edited
  const reload = (fresh, { keep = true } = {}) => {
    const pending = keep ? { slots: edited.slots, capabilities: edited.capabilities, modes: edited.modes } : {};
    current = fresh;
    edited = { ...clone(fresh), ...pending };
    draw();
    if (onSaved) onSaved();
  };
  const act = async (name, ...args) => {
    try {
      reload(await call(name, target, m.id, ...args));
      return true;
    } catch (e) {
      toast(e.message, "error");
      return false;
    }
  };
  const save = async () => {
    const change = settingsBody(current, edited);
    if (!change) { toast("Nothing has changed."); return; }
    try {
      reload(await call("saveMachineSettings", target, m.id, change), { keep: false });
      toast("Machine settings saved.", "good");
    } catch (e) { toast(e.message, "error"); }
  };

  // ------------------------------------------------------------------ capabilities
  function capabilities() {
    const llm = edited.capabilities.llm;
    const cpu = edited.capabilities.cpu;
    const slotsHint = el("span", { class: "muted small" });
    const showSlots = (text) => {
      const n = wholeIn(text, 1, current.slots_max);
      if (n != null) edited.slots = n;
      slotsHint.className = n == null ? "bad small" : "muted small";
      slotsHint.textContent = n == null ? `A whole number from 1 to ${current.slots_max}.`
        : "How many requests it takes at once: model seats' and pooled prompts' together. Its helper's own "
          + "--max-concurrent caps it.";
    };
    showSlots(String(edited.slots));
    const box = el("div", { class: "col" },
      el("div", { class: "card inner" },
        el("h3", {}, "Models (LLM)"),
        check(llm, "enabled", "Takes model work",
          "Off: no seat, test or pooled prompt runs on it; games and suites that name it are refused, saying why."),
        el("div", { class: "field" }, el("label", {}, "LLM slots"),
          el("input", { type: "number", min: 1, max: current.slots_max, value: edited.slots, style: { width: "90px" },
                        oninput: (e) => showSlots(e.target.value) }),
          slotsHint),
        check(llm, "allow_swaps", "Allow model swaps for pooled prompts",
          "Off: a pooled prompt (a report's analysis, a probe's answer, a test, a hybrid seat's diplomacy) runs only on "
          + "a model the machine has loaded, or one a game is using there. On: it may make the machine load another."),
        el("p", { class: "muted small" }, inEffectText(current.in_effect))));
    if (sharesCpu(current.kind)) {
      const inputs = el("div", { class: "grid2" },
        number(cpu, "threads", "Threads", 0, 1024, "0 leaves it to the helper"),
        number(cpu, "cpu_percent", "CPU %", 0, 100, "0 leaves it to the helper"),
        number(cpu, "memory_mb", "Memory (MB)", 0, 16 * 1024 * 1024, "0 leaves it to the helper"));
      box.appendChild(el("div", { class: "card inner" },
        el("h3", {}, "CPU sharing"),
        check(cpu, "enabled", "Share its CPU for CITAR's CPU work"),
        inputs,
        el("div", { class: "field" }, el("label", {}, "The kinds of work it takes"),
          ...current.cpu_work.map((kind) => el("label", { class: "checkrow" },
            el("input", { type: "checkbox", checked: cpu.work.includes(kind), onchange: (e) => {
              cpu.work = current.cpu_work.filter((k) => (k === kind ? e.target.checked : cpu.work.includes(k)));
            } }), WORK[kind] || kind))),
        el("p", { class: "muted small" }, "These take effect with the 0.1.6 helper, which runs CPU work; the helper of "
          + "this version (worker protocol 1) is sent none of them. The limits are hard where the operating system "
          + "allows, and the helper's own local settings always win.")));
    }
    return box;
  }

  // ------------------------------------------------------------------ hardware, models and power
  function scans() {
    if (current.reach !== "worker") {
      return el("div", { class: "col" }, el("p", { class: "muted" },
        "Hardware, models and power follow the reports of a CITAR helper running on the machine. This machine has no "
        + "helper (the server reaches it itself): edit its hardware and power under Hardware & power, and its models "
        + "under Connection & models."));
    }
    const report = current.report || {};
    const helper = report.helper ? ` from ${report.helper.hostname || "its helper"}`
      + (report.helper.worker_version ? ` (helper ${report.helper.worker_version})` : "") : "";
    const any = Object.values(current.differences || {}).some((d) => d.length);
    const box = el("div", { class: "col" },
      el("div", { class: "row between wrap" },
        el("span", { class: "muted" }, report.at ? `Last report: ${when(report.at)}${helper}.`
          : "No report yet: its helper sends one each time it connects."),
        any ? el("button", { class: "small primary", onclick: () => act("applyScan") }, "Apply all of the last report")
          : null),
      el("p", { class: "muted small" }, "Auto or manual says what its helper's next report may change; Save keeps a change "
        + "of mode. Apply copies the last report in now, whatever the mode."));
    for (const section of SECTIONS) box.appendChild(scanSection(section, report));
    return box;
  }

  function scanSection(section, report) {
    const diffs = (current.differences || {})[section] || [];
    const text = el("p", { class: "muted small" }, MODE_TEXT[section][edited.modes[section]]);
    const mode = el("select", { "aria-label": `${NAMES[section]}: auto or manual`, onchange: (e) => {
      edited.modes[section] = e.target.value;
      text.textContent = MODE_TEXT[section][e.target.value];
    } }, ...["auto", "manual"].map((v) => el("option", { value: v, selected: edited.modes[section] === v }, v)));
    const at = section === "models" ? report.models_at : report.hardware_at;
    const reported = section === "models" ? Array.isArray(report.models) : !!report.hardware;
    let status;
    if (!reported) status = el("p", { class: "muted small" }, "Nothing reported yet.");
    else if (section === "power" && !Object.keys(report.power || {}).length) {
      status = el("p", { class: "muted small" }, "Its last scan suggested no power figures.");
    } else if (!diffs.length) status = el("p", { class: "good small" }, "The configuration matches its last report.");
    else {
      status = el("table", { class: "list" },
        el("tr", {}, el("th", {}, ""), el("th", {}, "Configured"), el("th", {}, "Last report")),
        ...diffs.map((d) => el("tr", {}, el("td", {}, d.what), el("td", {}, d.configured ?? "—"),
          el("td", {}, d.detected ?? "—"))));
    }
    const summary = section === "hardware" && report.hardware_summary
      ? el("p", { class: "small" }, `Last scan: ${report.hardware_summary}`) : null;
    return el("div", { class: "card inner", "data-section": section },
      el("div", { class: "row between wrap" },
        el("h3", {}, NAMES[section]),
        el("label", { class: "row", style: { gap: "6px" } }, el("span", { class: "muted small" }, "Its helper's reports:"), mode)),
      text,
      reported ? el("p", { class: "muted small" }, `Reported ${when(at)}.`) : null,
      summary, status,
      diffs.length ? el("button", { class: "small", onclick: () => act("applyScan", [section]) },
        `Apply the last ${section === "models" ? "report's models" : section === "power" ? "scan's power figures" : "scan"}`)
        : null);
  }

  // ------------------------------------------------------------------ class and trust
  function classAndTrust() {
    const cls = current.class || {};
    const kinds = cls.kinds || ["gpu", "cpu"];
    const flags = (current.flags || {}).overreports;
    const box = el("div", { class: "col" },
      el("div", { class: "card inner" },
        el("h3", {}, "Hardware class"),
        el("p", { class: "muted small" }, "What its work is worth in the compute ledger: credits per generation-second of "
          + "its GPU (per unit of token cost on an online API) and per core-second of its CPU."),
        el("div", { class: "row wrap" }, el("b", {}, kinds[0] === "api" ? "API: " : "GPU: "), classText(cls.gpu)),
        kinds.includes("cpu") ? el("div", { class: "row wrap" }, el("b", {}, "CPU: "), classText(cls.cpu)) : null,
        el("p", { class: "muted small" }, cls.pinned ? "Pinned by an administrator: its hardware does not change it."
          : "Follows its hardware: a change of hardware classifies it again.")),
      el("div", { class: "card inner" },
        el("h3", {}, "Trust tier"),
        el("p", {}, el("b", {}, TIERS[current.trust_tier] || current.trust_tier)),
        el("p", { class: "muted small" }, "Set by administrators. Today it is a label for them; a later version sets it "
          + "by itself and decides with it what work the machine is trusted with.")));
    if (flags) {
      box.appendChild(el("p", { class: "warn-box" }, `Its helper reported more generation time than the server observed `
        + `(by over 10% and a second) ${flags.count} time${flags.count === 1 ? "" : "s"}; the last ${when(flags.last)}. `
        + "Its work is credited the time the server observed."));
    }
    if (current.can_admin && may("setMachineClass", target)) box.appendChild(adminControls(kinds));
    return box;
  }

  function adminControls(kinds) {
    const cls = current.class || {};
    const choice = {};
    const selects = kinds.map((kind) => {
      const now = kind === "cpu" ? cls.cpu : cls.gpu;
      choice[kind] = now ? now.id : "";
      return el("label", { class: "field" }, el("span", {}, kind === "api" ? "API class" : `${kind.toUpperCase()} class`),
        el("select", { onchange: (e) => { choice[kind] = e.target.value; } },
          ...(current.classes || []).filter((c) => c.kind === kind).map((c) =>
            el("option", { value: c.id, selected: c.id === choice[kind] }, classText(c)))));
    });
    const pin = () => act("setMachineClass", kinds[0] === "api" ? { pinned: true, gpu_class_id: choice.api }
      : { pinned: true, gpu_class_id: choice.gpu, cpu_class_id: choice.cpu });
    const tier = el("select", { "aria-label": "Trust tier" },
      ...Object.keys(TIERS).map((t) => el("option", { value: t, selected: t === current.trust_tier }, TIERS[t])));
    return el("div", { class: "card inner" },
      el("h3", {}, "Administrators"),
      el("div", { class: "grid2" }, ...selects),
      el("div", { class: "row wrap gap" },
        el("button", { class: "small", onclick: pin }, "Pin these classes"),
        cls.pinned ? el("button", { class: "small", onclick: () => act("setMachineClass", { pinned: false }) },
          "Follow its hardware again") : null),
      el("p", { class: "muted small" }, "The ledger prices its work by the new class from now on; citar admin reprice "
        + "recomputes a past period."),
      el("div", { class: "row wrap gap" }, tier,
        el("button", { class: "small", onclick: () => act("setMachineTrust", tier.value) }, "Set the trust tier")));
  }

  // ------------------------------------------------------------------ small form parts
  function check(obj, key, label, hint) {
    return el("div", { class: "col", style: { gap: "2px" } },
      el("label", { class: "checkrow" }, el("input", { type: "checkbox", checked: !!obj[key],
        onchange: (e) => { obj[key] = e.target.checked; } }), label),
      hint ? el("span", { class: "muted small" }, hint) : null);
  }

  function number(obj, key, label, lo, hi, hint) {
    const note = el("span", { class: "muted small" }, hint);
    return el("div", { class: "field" }, el("label", {}, label),
      el("input", { type: "number", min: lo, max: hi, value: obj[key], oninput: (e) => {
        const n = wholeIn(e.target.value, lo, hi);
        if (n != null) obj[key] = n;
        note.className = n == null ? "bad small" : "muted small";
        note.textContent = n == null ? `A whole number from ${lo} to ${hi.toLocaleString()}.` : hint;
      } }), note);
  }

  draw();
  const dialog = modal({
    title: `Machine settings — ${m.name}`,
    content: el("div", { class: "col machine-settings" }, tabs, body),
    footer: [el("button", { onclick: () => dialog.close() }, "Close"), el("button", { class: "primary", onclick: save }, "Save")],
  });
  return dialog;
}

// Queue page: all the work waiting to run, in the order it will run, with priorities you can change.
// Each model machine you may see (yours, those your grants reach) shows its slots, the seats that play every turn on it
// (leases), its jobs (benchmark jobs and probe runs, from one queue: citar.pool.queue) and its pooled prompts (reports'
// written analyses, probes' offers and messages, machine tests: the prompt scheduler's tickets), each list in the order
// it will run. Other members' work you may not see is "someone's ..." with no name; machines you may not see are only
// counted. The CPU jobs card shows each executor of the lab's games (this server's own job host, and each helper
// machine lending its CPU) with its games
// playing, each named where you may see its experiment and "someone's lab game" otherwise, then the lab's experiments
// in the order they run. Higher priority goes first; equal priorities go in the order they were queued (and pooled
// prompts take turns between members).
import { getCsrfToken } from "./api.js";
import { el, clear, toast } from "./util.js";
import { pageHeader, bar } from "./nav.js";
import { executorText, gameLine } from "./labrules.js";

async function get(url) {
  const res = await fetch(url, { credentials: "same-origin" });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json();
}

async function post(url, body) {
  const res = await fetch(url, {
    method: "POST", credentials: "same-origin",
    headers: { "Content-Type": "application/json", "x-citar-csrf": getCsrfToken() },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) throw new Error(((await res.json().catch(() => ({}))).detail) || res.statusText);
  return res.json();
}

const setPriority = (kind, id, priority) => post("/api/queue/priority", { kind, id, priority });
const cancelPrompt = (id) => post(`/api/queue/tickets/${encodeURIComponent(id)}/cancel`);

// ▲ / ▼ and the number: one step moves work past everything at the default priority; ⤒ puts it above everything
// on its machine - running work included, which then makes way at its next safe point
function priorityControl(kind, id, value, refresh, top) {
  const change = async (v) => {
    try { await setPriority(kind, id, v); refresh(); } catch (e) { toast(e.message, "error"); }
  };
  const input = el("input", { type: "number", value, min: -100, max: 100, class: "q-prio",
    title: "Priority: higher runs first, 0 is normal", onchange: (e) => change(+e.target.value || 0) });
  const toTop = top != null && value <= top
    ? el("button", { class: "small", title: "Run this first: above everything on this machine", onclick: () => change(top + 1) }, "⤒ top")
    : null;
  return el("span", { class: "q-prio-box" },
    el("button", { class: "small", title: "Raise priority", onclick: () => change(value + 1) }, "▲"),
    input,
    el("button", { class: "small", title: "Lower priority", onclick: () => change(value - 1) }, "▼"), toTop);
}

const KIND_LABEL = { benchmark: "benchmark job", probe: "probe run", report: "report (written analysis)", game: "game" };
const LEASE_KIND = { game: "game", benchmark: "benchmark", probe_case: "probe turn case" };
const LEASE_STATE = {
  active: ["live", "mid-turn"],
  lending: ["", "between turns: lends its slot"],
  held: ["quiet", "holds its slot"],
};
const PROMPT_CLASS = { report: "report", probe: "probe case", chat: "chat answer", diplomat: "diplomat", test: "machine test" };

// a duration in words: "12 s", "3 min", "1 h 05 min"
function secs(s) {
  if (s == null) return "—";
  if (s < 60) return `${Math.round(s)} s`;
  if (s < 3600) return `${Math.round(s / 60)} min`;
  const m = Math.round(s / 60);
  return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, "0")} min`;
}

function leasesBlock(m) {
  if (!(m.leases || []).length) return null;
  return el("div", { class: "q-leases" },
    el("div", { class: "muted small" }, "Seats that play every turn here:"),
    el("ul", { class: "q-list" }, ...m.leases.map((l) => {
      const [pill, text] = LEASE_STATE[l.state] || ["", l.state];
      return el("li", {}, l.label, l.member ? el("span", { class: "muted" }, ` · ${l.member}`) : null, " ",
        el("span", { class: `pill ${pill}` }, text),
        el("span", { class: "muted small" }, ` ${LEASE_KIND[l.kind] || l.kind}${l.waiting ? " · its next step is waiting" : ""}`));
    })));
}

function jobsBlock(m, refresh) {
  const running = m.running || [];
  if (!running.length && !m.waiting.length) return null;
  // one ranked list: what runs now, then what waits, in the order it will run. Runs are prioritised as a whole,
  // so the controls sit on the first item of each run.
  const all = [...running.map((it) => ({ ...it, now: true })), ...m.waiting];
  const top = Math.max(0, ...all.map((it) => it.priority || 0));
  const lowestRunning = running.length ? Math.min(...running.map((r) => r.priority || 0)) : null;
  const seen = new Set();
  const table = el("table", { class: "q-table" }, el("tr", {}, el("th", {}, "#"), el("th", {}, "What"), el("th", {}, "Kind"),
    el("th", {}, "Priority"), el("th", {}, "State")));
  for (const it of all) {
    const first = !seen.has(`${it.kind}:${it.group}`);
    seen.add(`${it.kind}:${it.group}`);
    const outranks = !it.now && lowestRunning != null && (it.priority || 0) > lowestRunning;
    table.append(el("tr", { class: it.now ? "q-now-row" : it.position === 1 ? "q-next" : "" },
      el("td", {}, it.now ? (it.state && it.state.startsWith("paused") ? el("span", { class: "pill quiet" }, "paused")
        : el("span", { class: "pill live" }, "running")) : it.position === 1 ? "next" : it.position),
      el("td", {}, it.label, it.kind === "benchmark" ? el("div", { class: "muted small" }, it.run) : null),
      el("td", {}, KIND_LABEL[it.kind] || it.kind),
      // controls only on work the caller manages; others' work is "someone's <kind>" (design P3.2.4)
      el("td", {}, first && it.can_manage && it.group ? priorityControl(it.kind, it.group, it.priority || 0, refresh, top)
        : el("span", { class: "muted" }, it.priority)),
      el("td", { class: "muted small" }, it.now ? (it.state && it.state !== "running" ? `${it.state}; keeps its place on the machine` : "using the machine")
        : outranks ? "higher priority: starts once the running work finishes its current turn"
          : (it.waiting || "").replace(/^waiting:? ?(for )?/, ""))));
  }
  return el("div", {}, el("div", { class: "muted small" }, "Jobs:"), el("div", { class: "table-wrap" }, table));
}

function promptsBlock(m, refresh) {
  const prompts = m.prompts || { running: [], waiting: [] };
  const rows = [...prompts.running.map((p) => ({ ...p, now: true })), ...prompts.waiting];
  if (!rows.length) return null;
  const table = el("table", { class: "q-table" }, el("tr", {}, el("th", {}, "#"), el("th", {}, "What"), el("th", {}, "Kind"),
    el("th", {}, "Model"), el("th", {}, "Priority"), el("th", {}, "Waited"), el("th", {}, "Starts"), el("th", {}, "")));
  for (const p of rows) {
    const cancel = p.can_cancel && p.id
      ? el("button", { class: "small", title: "Cancel this prompt: its caller goes on without it",
        onclick: async () => { try { await cancelPrompt(p.id); refresh(); } catch (e) { toast(e.message, "error"); } } }, "Cancel")
      : null;
    const state = p.now
      ? el("span", { class: "pill live" }, p.draining ? "finishing" : p.borrowing ? "running on a lent slot" : "running")
      : p.position === 1 ? "next" : p.position;
    table.append(el("tr", { class: p.now ? "q-now-row" : p.position === 1 ? "q-next" : "" },
      el("td", {}, state),
      el("td", {}, p.label, p.member ? el("div", { class: "muted small" }, p.member) : null),
      el("td", {}, PROMPT_CLASS[p.class] || p.class),
      el("td", { class: "muted small" }, p.model || "—"),
      el("td", {}, p.can_manage && p.ref ? priorityControl(p.ref[0], p.ref[1], p.priority || 0, refresh)
        : el("span", { class: "muted" }, p.priority)),
      el("td", { class: "muted small" }, p.now ? `${secs(p.waited_s)} · running ${secs(p.running_s)}` : secs(p.waited_s)),
      el("td", { class: "muted small" }, p.now ? "—"
        : `in about ${secs(p.eta_s)}${p.starts_within_s != null ? ` (gives up after ${secs(p.starts_within_s)})` : ""}`),
      el("td", {}, cancel)));
  }
  // wide: scrolls sideways inside its card on a phone, its Cancel buttons never cut off
  return el("div", {}, el("div", { class: "muted small" }, "Pooled prompts:"), el("div", { class: "table-wrap" }, table));
}

function machineCard(m, refresh) {
  const card = el("div", { class: "card q-machine" });
  const waitingPrompts = ((m.prompts || {}).waiting || []).length;
  const slots = m.capacity != null ? `${m.in_flight} of ${m.capacity} slot${m.capacity === 1 ? "" : "s"} in use` : null;
  card.append(el("div", { class: "row" },
    el("h3", { style: { margin: 0 } }, m.name),
    el("span", { class: `pill ${m.online ? "live" : ""}` }, m.online ? "helper connected" : m.online === false ? "offline" : "—"),
    el("span", { class: "grow" }),
    slots ? el("span", { class: "muted" }, slots) : null,
    el("span", { class: "muted" }, ` · ${m.waiting.length + waitingPrompts} waiting`)));
  const blocks = [leasesBlock(m), jobsBlock(m, refresh), promptsBlock(m, refresh)].filter(Boolean);
  if (!blocks.length) {
    card.append(el("p", { class: "muted" }, m.busy_with.length ? `In use by ${m.busy_with.join(", ")}.` : "Free, and nothing waiting."));
    return card;
  }
  card.append(...blocks);
  return card;
}

function cpuCard(cpu, refresh) {
  const card = el("div", { class: "card" });
  const load = cpu.load ? cpu.load[0] / (cpu.cpus || 1) : null;
  const disp = cpu.dispatcher || {};
  card.append(el("div", { class: "row" }, el("h3", { style: { margin: 0 } }, "CPU jobs — the lab"),
    el("span", { class: `pill ${disp.alive && !disp.paused ? "live" : disp.paused ? "warn" : ""}` },
      !disp.alive ? "the dispatcher is not running" : disp.paused ? "paused" : "playing"),
    el("span", { class: "grow" }),
    cpu.load ? el("span", { class: "muted" }, `load ${cpu.load.join(" · ")} on ${cpu.cpus} CPU${cpu.cpus === 1 ? "" : "s"}`) : null));
  if (load != null) card.append(el("div", { class: "q-load" }, bar(Math.min(1, load), `${Math.round(load * 100)}% of the CPU (1-minute average)`,
    load > 0.9 ? "bad" : load > 0.7 ? "" : "good")));
  card.append(el("p", { class: "muted small" }, "The lab's bot-only games run as CPU jobs on this server's own job host, under the host's CPU ",
    "settings, and on machines whose owners lend their CPU through the CITAR helper, under each machine's own. On this server ",
    "they share the CPU with every game here; games spend most of their time waiting for model machines, so there is usually room."));
  for (const e of cpu.executors || []) {
    const games = e.games || [];
    card.append(el("div", { class: "q-machine-cpu" },
      el("div", { class: "row" }, el("b", {}, e.name), el("span", { class: "muted small" }, executorText(e))),
      games.length ? el("ul", { class: "q-list" }, ...games.map((g) => el("li", {}, gameLine(g))))
        : el("p", { class: "muted small" }, "No game playing.")));
  }
  if (!cpu.experiments.length) { card.append(el("p", { class: "muted" }, "No experiments queued.")); return card; }
  const table = el("table", { class: "q-table" }, el("tr", {}, el("th", {}, "#"), el("th", {}, "Experiment"), el("th", {}, "Games"),
    el("th", {}, "Priority"), el("th", {}, "State")));
  cpu.experiments.forEach((e, n) => table.append(el("tr", { class: n === 0 ? "q-next" : "" },
    el("td", {}, n === 0 ? "next" : n + 1),
    el("td", {}, e.name || e.label, e.note ? el("div", { class: "muted small" }, e.note) : null),
    el("td", {}, `${e.done}/${e.games}${e.running ? ` (+${e.running} playing)` : ""}`),
    el("td", {}, e.can_manage !== false && e.name ? priorityControl("lab", e.name, e.priority || 0, refresh)
      : el("span", { class: "muted" }, e.priority)),
    el("td", { class: "muted small" }, e.state))));
  card.append(table);
  return card;
}

export async function renderQueue(root) {
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("queue"));
  const body = el("div");
  page.append(el("div", { class: "card" }, el("h2", { style: { margin: 0 } }, "Queue"),
    el("p", { class: "muted" }, "Each model machine you may use shows the seats that play every turn on it, its jobs (benchmark jobs and probe runs) ",
      "and its pooled prompts (reports' written analyses, probes' offers and messages, machine tests), each in the order it will run. ",
      "A seat lends its slot to pooled prompts between its turns, and gets it back within one short prompt. ",
      "Higher priority runs first, then whatever has waited longest; pooled prompts also take turns between members. ",
      "Put a job above the running work (⤒ top) and the running work makes way at its next safe point, ",
      "a game after the model's current turn and a probe run between cases, then carries on when it is back on top.")), body);
  const refresh = async () => {
    try {
      const q = await get("/api/queue");
      clear(body);
      if (!q.machines.length) body.append(el("div", { class: "card" }, el("p", { class: "muted" }, "Nothing is waiting for a model machine you may use.")));
      for (const m of q.machines) body.append(machineCard(m, refresh));
      if (q.others_busy) {
        body.append(el("p", { class: "muted small" },
          `${q.others_busy} other machine${q.others_busy === 1 ? " is" : "s are"} busy.`));
      }
      body.append(cpuCard(q.cpu, refresh));
    } catch (e) { toast(e.message, "error"); }
  };
  await refresh();
  const timer = setInterval(refresh, 10000);
  return { destroy() { clearInterval(timer); } };
}

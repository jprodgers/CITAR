// Queue page: all the work waiting to run, in the order it will run, with priorities you can change.
// Model machines take benchmark jobs and probe runs from one queue (citar.pool.queue); this server's CPU takes the
// lab's experiments. Higher priority goes first; equal priorities go in the order they were queued.
import { getCsrfToken } from "./api.js";
import { el, clear, toast } from "./util.js";
import { pageHeader, bar } from "./nav.js";

async function get(url) {
  const res = await fetch(url, { credentials: "same-origin" });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json();
}

async function setPriority(kind, id, priority) {
  const res = await fetch("/api/queue/priority", {
    method: "POST", credentials: "same-origin",
    headers: { "Content-Type": "application/json", "x-citar-csrf": getCsrfToken() },
    body: JSON.stringify({ kind, id, priority }),
  });
  if (!res.ok) throw new Error(((await res.json().catch(() => ({}))).detail) || res.statusText);
  return res.json();
}

// ▲ / ▼ and the number: one step moves work past everything at the default priority
function priorityControl(kind, id, value, refresh) {
  const change = async (v) => {
    try { await setPriority(kind, id, v); refresh(); } catch (e) { toast(e.message, "error"); }
  };
  const input = el("input", { type: "number", value, min: -100, max: 100, class: "q-prio",
    title: "Priority: higher runs first, 0 is normal", onchange: (e) => change(+e.target.value || 0) });
  return el("span", { class: "q-prio-box" },
    el("button", { class: "small", title: "Raise priority", onclick: () => change(value + 1) }, "▲"),
    input,
    el("button", { class: "small", title: "Lower priority", onclick: () => change(value - 1) }, "▼"));
}

function machineCard(m, refresh) {
  const card = el("div", { class: "card q-machine" });
  card.append(el("div", { class: "row" },
    el("h3", { style: { margin: 0 } }, m.name),
    el("span", { class: `pill ${m.online ? "live" : ""}` }, m.online ? "helper connected" : m.online === false ? "offline" : "—"),
    el("span", { class: "grow" }),
    el("span", { class: "muted" }, `${m.waiting.length} waiting`)));
  card.append(el("div", { class: "q-now" }, el("b", {}, "Now: "),
    m.busy_with.length ? m.busy_with.join(", ") : el("span", { class: "muted" }, "free — the first item below starts next")));
  if (!m.waiting.length) { card.append(el("p", { class: "muted" }, "Nothing waiting.")); return card; }
  // runs are prioritised as a whole, so the controls sit on the first item of each run
  const seen = new Set();
  const table = el("table", { class: "q-table" }, el("tr", {}, el("th", {}, "#"), el("th", {}, "What"), el("th", {}, "Kind"),
    el("th", {}, "Priority"), el("th", {}, "Why it is waiting")));
  for (const it of m.waiting) {
    const first = !seen.has(`${it.kind}:${it.group}`);
    seen.add(`${it.kind}:${it.group}`);
    table.append(el("tr", { class: it.position === 1 ? "q-next" : "" },
      el("td", {}, it.position === 1 ? "next" : it.position),
      el("td", {}, it.label, it.kind === "benchmark" ? el("div", { class: "muted small" }, it.run) : null),
      el("td", {}, it.kind === "benchmark" ? "benchmark job" : "probe run"),
      el("td", {}, first ? priorityControl(it.kind, it.group, it.priority, refresh) : el("span", { class: "muted" }, it.priority)),
      el("td", { class: "muted small" }, (it.waiting || "").replace(/^waiting:? ?(for )?/, ""))));
  }
  card.append(table);
  return card;
}

function cpuCard(cpu, refresh) {
  const card = el("div", { class: "card" });
  const load = cpu.load ? cpu.load[0] / (cpu.cpus || 1) : null;
  const runner = cpu.lab_runner || {};
  card.append(el("div", { class: "row" }, el("h3", { style: { margin: 0 } }, "This server's CPU — the lab"),
    el("span", { class: `pill ${runner.alive ? "live" : ""}` }, runner.alive ? `runner on · ${runner.workers || "?"} at a time` : "runner stopped"),
    el("span", { class: "grow" }),
    cpu.load ? el("span", { class: "muted" }, `load ${cpu.load.join(" · ")} on ${cpu.cpus} CPU${cpu.cpus === 1 ? "" : "s"}`) : null));
  if (load != null) card.append(el("div", { class: "q-load" }, bar(Math.min(1, load), `${Math.round(load * 100)}% of the CPU (1-minute average)`,
    load > 0.9 ? "bad" : load > 0.7 ? "" : "good")));
  card.append(el("p", { class: "muted small" }, "The lab plays bot-only games on this server, so it shares the CPU with every game here; ",
    "games spend most of their time waiting for model machines, so there is usually room."));
  if ((cpu.lab_running || []).length) {
    card.append(el("div", {}, el("b", {}, "Playing now: "), cpu.lab_running.map((r) => `${r.exp} #${r.i}${r.turn ? ` (turn ${r.turn}${r.limit ? "/" + r.limit : ""})` : ""}`).join(", ")));
  }
  if (!cpu.experiments.length) { card.append(el("p", { class: "muted" }, "No experiments queued.")); return card; }
  const table = el("table", { class: "q-table" }, el("tr", {}, el("th", {}, "#"), el("th", {}, "Experiment"), el("th", {}, "Games"),
    el("th", {}, "Priority"), el("th", {}, "State")));
  cpu.experiments.forEach((e, n) => table.append(el("tr", { class: n === 0 ? "q-next" : "" },
    el("td", {}, n === 0 ? "next" : n + 1),
    el("td", {}, e.name, e.note ? el("div", { class: "muted small" }, e.note) : null),
    el("td", {}, `${e.done}/${e.games}${e.running ? ` (+${e.running} playing)` : ""}`),
    el("td", {}, priorityControl("lab", e.name, e.priority || 0, refresh)),
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
    el("p", { class: "muted" }, "Benchmark jobs and probe runs share one queue per model machine; the lab has its own on this server's CPU. ",
      "Higher priority starts first, then whatever has waited longest. Work never interrupts a game already using a machine: it starts when the machine is free.")), body);
  const refresh = async () => {
    try {
      const q = await get("/api/queue");
      clear(body);
      if (!q.machines.length) body.append(el("div", { class: "card" }, el("p", { class: "muted" }, "Nothing is waiting for a model machine.")));
      for (const m of q.machines) body.append(machineCard(m, refresh));
      body.append(cpuCard(q.cpu, refresh));
    } catch (e) { toast(e.message, "error"); }
  };
  await refresh();
  const timer = setInterval(refresh, 10000);
  return { destroy() { clearInterval(timer); } };
}

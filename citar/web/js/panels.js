// Unit panel, city panel, and the tech / policies / religion / great people / city-states / diplomacy / empire /
// victory / notes / help modals.
import { api } from "./api.js";
import { el, clear, toast, modal, fmt, signed, prompt, confirmBox } from "./util.js";

const YIELD_ICONS = [["food", "🍞", "food"], ["production", "⚒", "prod"], ["gold", "●", "gold"], ["science", "⚗", "sci"],
                     ["culture", "✦", "cul"], ["faith", "✝", "faith"]];

function yieldSpans(y, digits = 1) {
  return YIELD_ICONS.filter(([k]) => y && y[k]).map(([k, icon, cls]) => el("span", { class: cls }, `${icon} ${fmt(y[k], digits)} `));
}

function uniquesText(list) { return (list || []).join("; "); }

// ============================================================================
// Unit panel
// ============================================================================
export function renderUnitPanel(game, root, d) {
  const R = game.rules;
  const my = game.myTurn;
  root.append(
    el("div", { class: "row" }, el("b", { style: { fontSize: "16px" } }, d.name), el("span", { class: "muted" }, `#${d.id} (${d.x},${d.y})`),
      el("span", { class: "grow" }), el("button", { class: "small", onclick: () => game.deselect() }, "✕")),
    el("div", { class: "muted" }, [
      `HP ${d.hp}/100`, `Moves ${d.moves}/${d.max_moves}`,
      d.strength ? `Str ${d.strength}` : null, d.ranged_strength ? `Ranged ${d.ranged_strength} (range ${d.range})` : null,
      `XP ${d.xp}${d.xp_for_next_promotion ? "/" + d.xp_for_next_promotion : ""}`, d.embarked ? "embarked" : null,
      d.religion ? `${d.religion}${d.religious_strength != null ? " (" + d.religious_strength + ")" : ""}` : null,
    ].filter(Boolean).join(" · ")),
  );
  if (d.promotions && d.promotions.length) root.appendChild(el("div", { class: "muted" }, "Promotions: " + d.promotions.join(", ")));
  if (d.abilities && d.abilities.length) root.appendChild(el("details", { class: "muted", style: { fontSize: "12px" } },
    el("summary", {}, "Abilities"), el("div", {}, uniquesText(d.abilities))));
  if (d.activity || d.building) {
    let act = { fortify: "fortified", sleep: "sleeping", heal: "healing", explore: "exploring", automate: "automated" }[d.activity] || d.activity;
    if (d.building && d.building.length) act = (d.activity === "automate" ? "automated, " : "") + "building " + d.building.map((b) => `${b.improvement} (${b.turns_left}t)`).join(" then ");
    if (d.goto) act = `moving to (${d.goto[0]},${d.goto[1]})`;
    root.appendChild(el("div", {}, "Orders: ", el("b", {}, act || "")));
  }
  const others = game.view.units.filter((u) => u.x === d.x && u.y === d.y && u.owner === game.you && u.id !== d.id);
  const cityHere = game.view.cities.find((c) => c.x === d.x && c.y === d.y && c.owner === game.you);
  if (others.length || cityHere) {
    root.appendChild(el("div", { class: "row stack-chips" }, el("span", { class: "muted" }, "Also here:"),
      cityHere ? el("button", { class: "small chip", onclick: () => game.selectCity(cityHere.id) }, `🏛 ${cityHere.name}`) : null,
      ...others.map((u) => unitChip(game, u))));
  }
  if (!my) { root.appendChild(el("div", { class: "muted section" }, game.isSpectator ? "" : "Not your turn.")); return; }

  const order = (o) => game.tool("unit_order", { unit_id: d.id, order: o }).then((r) => {
    if (r && o === "explore" && r.exploring === false) toast("Nothing left to explore nearby.");
    if (r && ["fortify", "sleep", "skip", "heal", "explore", "automate", "disband"].includes(o)) game.afterOrder(d.id);
  });
  // special actions (found city, religion, great people, great improvements, ...)
  const acts = d.actions || [];
  if (acts.length) {
    const sec = el("div", { class: "section" }, el("h4", {}, "Actions"));
    const grid = el("div", { class: "btn-grid" });
    for (const a of acts) {
      grid.appendChild(el("button", { class: a.available ? "primary" : "", disabled: !a.available, title: a.reason || "",
        onclick: () => game.unitAction(d, a) }, a.name));
    }
    sec.appendChild(grid);
    const blocked = acts.filter((a) => !a.available && a.reason);
    if (blocked.length) sec.appendChild(el("div", { class: "muted", style: { fontSize: "12px" } }, blocked.map((a) => `${a.name}: ${a.reason}`).join(" · ")));
    root.appendChild(sec);
  }
  if ((d.suggested_city_sites || []).length) root.appendChild(el("div", { class: "muted", style: { fontSize: "12px" } }, "Green dashed tiles are suggested city sites."));
  if (d.suggested_sites) root.appendChild(el("div", { class: "muted", style: { fontSize: "12px" } },
    d.suggested_sites.length ? "Green dashed tiles: sea resources in your borders to improve (the boat is used up)."
      : "No unimproved sea resources inside your borders."));
  const civilian = d.class === "civilian";
  const btns = el("div", { class: "btn-grid section" });
  if (!civilian && d.domain !== "air" && d.activity !== "fortify") btns.appendChild(el("button", { onclick: () => order("fortify") }, "Fortify (F)"));
  if (d.activity !== "sleep") btns.appendChild(el("button", { onclick: () => order("sleep") }, "Sleep (S)"));
  btns.appendChild(el("button", { onclick: () => order("skip") }, "Skip (Space)"));
  if (d.hp < 100 && d.activity !== "heal") btns.appendChild(el("button", { onclick: () => order("heal") }, "Heal (H)"));
  if ((!civilian || d.domain === "sea") && d.activity !== "explore") btns.appendChild(el("button", { onclick: () => order("explore") }, "Explore (E)"));
  if (d.build_options && d.activity !== "automate") btns.appendChild(el("button", { onclick: () => order("automate") }, "Automate (A)"));
  if (!civilian && d.domain === "land") btns.appendChild(el("button", { onclick: () => order("pillage") }, "Pillage (P)"));
  if ((d.abilities || []).some((a) => a.startsWith("Must set up"))) btns.appendChild(el("button", { onclick: () => order("setup") }, "Set up"));
  if (d.activity) btns.appendChild(el("button", { onclick: () => order("cancel") }, "Cancel orders"));
  btns.appendChild(el("button", { class: "danger", onclick: async () => { if (await confirmBox("Disband", `Disband this ${d.name}?`)) order("disband"); } }, "Disband"));
  root.appendChild(btns);

  if (d.build_options && d.build_options.length) {
    const sec = el("div", { class: "section" }, el("h4", {}, "Build here"));
    const grid = el("div", { class: "btn-grid" });
    for (const o of d.build_options) {
      const tip = [o.first_removes ? `removes ${o.first_removes} first` : null, o.replaces ? `replaces ${o.replaces}` : null].filter(Boolean).join(", ");
      grid.appendChild(el("button", { title: tip, onclick: () => game.tool("build_improvement", { unit_id: d.id, improvement: o.name }).then((r) => r && game.afterOrder(d.id)) },
        `${o.name} (${o.turns}t)`));
    }
    sec.appendChild(grid);
    root.appendChild(sec);
  }
  if (d.upgrade && (d.upgrade.possible || !/ requires /i.test(d.upgrade.reason || ""))) {
    root.appendChild(el("div", { class: "section" }, el("button", {
      disabled: !d.upgrade.possible, title: d.upgrade.reason || "",
      onclick: () => game.tool("upgrade_unit", { unit_id: d.id }).then((r) => r && toast(`Upgraded to ${d.upgrade.to}`)),
    }, `Upgrade → ${d.upgrade.to} (${d.upgrade.gold} gold)`), d.upgrade.possible ? null : el("div", { class: "muted" }, d.upgrade.reason)));
  }
  if (d.available_promotions && d.available_promotions.length) {
    const sec = el("div", { class: "section" }, el("h4", {}, "Choose a promotion"));
    const grid = el("div", { class: "btn-grid" });
    const descs = el("div", { class: "muted", style: { fontSize: "12px", marginTop: "4px" } });
    for (const p of d.available_promotions) {
      const desc = uniquesText((R.promotions[p] || {}).uniques);
      grid.appendChild(el("button", { title: desc, onclick: () => game.tool("promote_unit", { unit_id: d.id, promotion: p }) }, p));
      descs.appendChild(el("div", {}, el("b", {}, p), `: ${desc}`));
    }
    sec.append(grid, descs);
    root.appendChild(sec);
  }
  if (d.attack_targets && d.attack_targets.length) {
    root.appendChild(el("div", { class: "section muted" }, `${d.attack_targets.length} attack target(s) — click a red tile to attack.`));
  } else if (d.moves > 0) {
    root.appendChild(el("div", { class: "section muted" }, "Click (or right-click) a tile to move there. Highlighted tiles are reachable this turn."));
  }
}

// ============================================================================
// City panel
// ============================================================================
const FOCUSES = ["balanced", "food", "production", "gold", "science", "culture", "faith", "happiness", "gold_growth",
                 "production_growth", "manual"];

export function renderCityPanel(game, root, c) {
  const my = game.myTurn;
  root.append(
    el("div", { class: "row" },
      el("b", { style: { fontSize: "17px" } }, c.name), c.capital ? el("span", { class: "gold" }, "★") : null,
      el("span", { class: "muted" }, `pop ${c.pop}`), c.religion ? el("span", { class: "muted" }, `· ${c.religion}`) : null,
      el("span", { class: "grow" }),
      el("button", { class: "small", onclick: async () => {
        const n = await prompt("Rename city", "New name:", c.name);
        if (n) game.tool("rename_city", { city_id: c.id, name: n });
      } }, "Rename"),
      el("button", { class: "small", onclick: () => game.deselect() }, "✕")),
    el("div", {}, ...yieldSpans(c.yields), el("span", { class: c.happiness < 0 ? "bad" : "good" }, `☺ ${fmt(c.happiness, 1)}`)),
    el("div", { class: "muted" },
      `Food ${fmt(c.food_stored)}/${c.food_to_grow} — ${c.turns_to_grow ? "grows in " + c.turns_to_grow : c.starving ? "STARVING" : "not growing"}` +
      `${c.avoid_growth ? " (avoiding growth)" : ""}`),
    el("div", { class: "muted" }, `HP ${c.hp}/${c.max_hp} · Strength ${c.strength} · Borders ${fmt(c.culture_stored)}/${c.culture_for_next_tile}` +
      `${c.connected_to_capital && !c.capital ? " · connected to capital" : ""}`),
  );
  if (c.we_love_the_king_day_turns) root.appendChild(el("div", { class: "good" }, `We Love The King Day: ${c.we_love_the_king_day_turns} turns`));
  else if (c.demands_resource) root.appendChild(el("div", { class: "muted" }, `Citizens demand ${c.demands_resource} (We Love The King Day)`));
  if (c.puppet || c.razing || c.resistance) {
    const box = el("div", { class: "card", style: { margin: "6px 0" } });
    box.appendChild(el("div", {}, [c.puppet ? "Puppet city" : null, c.razing ? "Being razed" : null,
      c.resistance ? `In resistance (${c.resistance} turns)` : null].filter(Boolean).join(" · ")));
    if (my) {
      const st = (status, label, cls = "") => el("button", { class: `small ${cls}`, onclick: async () => {
        if (["raze", "liberate"].includes(status) && !(await confirmBox(label, `${label} ${c.name}?`))) return;
        game.tool("city_status", { city_id: c.id, status });
      } }, label);
      box.appendChild(el("div", { class: "btn-grid" }, c.puppet ? st("annex", "Annex") : null, !c.razing ? st("raze", "Raze", "danger") : st("stop_razing", "Stop razing"),
        st("liberate", "Liberate"), !c.puppet && !c.razing ? st("puppet", "Make puppet") : null));
    }
    root.appendChild(box);
  }
  const here = game.view.units.filter((u) => u.x === c.x && u.y === c.y && u.owner === game.you);
  root.appendChild(el("div", { class: "row stack-chips" }, el("span", { class: "muted" }, "In city:"),
    here.length ? here.map((u) => unitChip(game, u)) : el("span", { class: "muted chip-empty" }, c.hp < c.max_hp ? "no garrison!" : "no units")));

  const qbox = el("div", { class: "queue-box" });
  const prod = el("div", { class: "section" }, el("h4", {}, `Production queue (${c.queue.length}/10)`), qbox);
  if (c.queue.length) {
    const act = (i, action) => game.tool("change_queue", { city_id: c.id, index: i, action });
    let total = 0;
    c.queue.forEach((q, i) => {
      total += q.turns || 0;
      const eta = q.turns == null ? "" : i === 0 ? `${q.turns}t` : `done in ~${total}t`;
      const pct = q.cost ? Math.min(100, (q.progress / q.cost) * 100) : 0;
      qbox.appendChild(el("div", { class: `queue-row ${i === 0 ? "current" : ""}` },
        el("span", { class: "qi" }, i === 0 ? "▶" : String(i + 1)),
        el("div", { class: "name" },
          el("div", {}, i === 0 ? el("b", {}, q.item) : q.item,
            el("span", { class: "muted" }, q.cost ? ` ${fmt(q.progress)}/${q.cost}⚒ · ${eta}` : " (converts production)")),
          i === 0 && q.cost ? el("div", { class: "pbar" }, el("div", { style: { width: pct + "%" } })) : null),
        my ? el("span", { class: "qbtns" },
          el("button", { class: "small", title: "Move up", disabled: i === 0, onclick: () => act(i, "up") }, "▲"),
          el("button", { class: "small", title: "Move down", disabled: i === c.queue.length - 1, onclick: () => act(i, "down") }, "▼"),
          el("button", { class: "small", title: "Remove from queue", onclick: () => act(i, "remove") }, "✕")) : null));
    });
  } else qbox.append(el("div", { class: c.puppet ? "muted" : "warn", style: { padding: "4px" } }, c.puppet ? "Puppets choose their own production." : "Nothing in production! Pick something below."));
  if (my && !c.puppet) {
    prod.appendChild(el("label", { class: "muted auto-prod", title: "When the queue runs out, the built-in advisor picks the next item" },
      el("input", { type: "checkbox", checked: !!c.auto_production, onchange: async (e) => {
        const r = await game.tool("set_auto_production", { city_id: c.id, enabled: e.target.checked });
        if (r && r.started) toast(`${c.name}: started ${r.started}`);
      } }), " Auto-pick production when the queue is empty"));
  }
  root.appendChild(prod);

  if (my) {
    const focus = el("select", { onchange: (e) => game.tool("set_city_focus", { city_id: c.id, focus: e.target.value }) },
      ...FOCUSES.map((f) => el("option", { value: f, selected: c.focus === f }, f.replace("_", " "))));
    root.appendChild(el("div", { class: "row section" }, "Focus:", focus,
      el("label", { class: "muted" }, el("input", { type: "checkbox", checked: !!c.avoid_growth,
        onchange: (e) => game.tool("set_city_focus", { city_id: c.id, avoid_growth: e.target.checked }) }), " avoid growth"),
      (() => {
        const n = game.bombardTargets(c).length;
        return el("button", { class: `small ${n ? "primary" : ""}`, disabled: !n,
          title: n ? "Pick a target on the map" : "No enemy units in range",
          onclick: () => { game.mode = "city_attack"; game.updateOverlay(); toast("Click a red-outlined enemy unit in range."); } },
          n ? `Bombard (${n})` : "Bombard");
      })(),
      el("button", { class: "small", onclick: () => { game.mode = "buy_tile"; game.updateOverlay(); toast("Click a dashed gold tile to buy it."); } }, "Buy tile")));

    // specialists
    const maxs = c.max_specialists || {};
    if (Object.keys(maxs).length) {
      const cur = { ...(c.specialists || {}) };
      const set = (k, n) => { const next = { ...cur, [k]: Math.max(0, Math.min(maxs[k], n)) }; game.tool("set_specialists", { city_id: c.id, specialists: next }); };
      root.appendChild(el("div", { class: "section" }, el("h4", {}, "Specialists"),
        ...Object.entries(maxs).map(([k, mx]) => el("div", { class: "row" }, el("span", { style: { minWidth: "90px" } }, k),
          el("button", { class: "small", disabled: !cur[k], onclick: () => set(k, (cur[k] || 0) - 1) }, "−"),
          el("span", {}, `${cur[k] || 0}/${mx}`),
          el("button", { class: "small", disabled: (cur[k] || 0) >= mx, onclick: () => set(k, (cur[k] || 0) + 1) }, "+"))),
        Object.keys(cur).length ? el("button", { class: "small", onclick: () => game.tool("set_specialists", { city_id: c.id, specialists: {} }) }, "Automatic") : null));
    }

    if (!c.puppet) {
      const emp = game.view.empire || {};
      const tabs = [["units", "Units"], ["buildings", "Buildings"], ["wonders", "Wonders"], ["other", "Other"]];
      game._cityTab = game._cityTab || "units";
      const list = el("div");
      const can = c.can_build || {};
      root.append(el("div", { class: "row section" }, ...tabs.map(([k, label]) =>
        el("button", { class: `small ${game._cityTab === k ? "primary" : ""}`, onclick: () => { game._cityTab = k; renderCityPanel(game, clear(root), c); } },
          `${label} (${(can[k] || []).length})`))), list);
      const queueFull = c.queue.length >= 10;
      for (const it of can[game._cityTab] || []) {
        const R = game.rules;
        let desc = "";
        if (R.units[it.item]) {
          const u = R.units[it.item];
          desc = [u.strength ? `str ${u.strength}` : null, u.rangedStrength ? `rng ${u.rangedStrength}` : null, `mv ${u.movement}`,
            u.requiredResource ? `needs ${u.requiredResource}` : null].filter(Boolean).join(", ");
        } else if (R.buildings[it.item]) {
          const b = R.buildings[it.item];
          desc = [...YIELD_ICONS.filter(([k]) => b[k]).map(([k]) => `+${b[k]} ${k}`), b.happiness ? `+${b.happiness} happiness` : null,
            ...(b.uniques || []), b.maintenance ? `upkeep ${b.maintenance}g` : null].filter(Boolean).join(", ");
        }
        const queued = c.queue.filter((q) => q.item === it.item).length;
        list.appendChild(el("div", { class: "item-row" },
          el("div", { class: "name" }, el("div", {}, it.item, el("span", { class: "muted" }, it.cost ? ` ${it.cost}⚒${it.turns ? " · " + it.turns + "t" : ""}` : ""),
            queued ? el("span", { class: "pill", style: { marginLeft: "4px" } }, queued > 1 ? `queued ×${queued}` : "queued") : null),
            el("div", { class: "muted", style: { fontSize: "11px" } }, desc)),
          el("button", { class: "small", title: "Start building it now", onclick: () => game.tool("set_production", { city_id: c.id, item: it.item }) }, "Now"),
          el("button", { class: "small", disabled: queueFull, title: queueFull ? "The queue is full" : "Add to the end of the queue",
            onclick: async () => { const r = await game.tool("set_production", { city_id: c.id, item: it.item, append: true }); if (r) toast(`Queued ${it.item}`, "info", 1800); } }, "+ Queue"),
          it.buy_gold != null ? el("button", { class: "small", title: `Buy now for ${it.buy_gold} gold`, disabled: (emp.gold || 0) < it.buy_gold,
            onclick: async () => { if (await confirmBox("Buy", `Buy ${it.item} for ${it.buy_gold} gold?`)) game.tool("buy", { city_id: c.id, item: it.item }); } }, `${it.buy_gold}g`) : null,
          it.buy_faith != null ? el("button", { class: "small", title: `Buy now for ${it.buy_faith} faith`, disabled: (emp.faith || 0) < it.buy_faith,
            onclick: async () => { if (await confirmBox("Buy", `Buy ${it.item} for ${it.buy_faith} faith?`)) game.tool("buy", { city_id: c.id, item: it.item, currency: "Faith" }); } }, `${it.buy_faith}✝`) : null));
      }
      if ((c.buy_with_faith || []).length && game._cityTab === "units") {
        list.appendChild(el("div", { class: "muted section" }, "Faith purchases:"));
        for (const it of c.buy_with_faith) {
          list.appendChild(el("div", { class: "item-row" }, el("div", { class: "name" }, it.item),
            el("button", { class: "small", disabled: (emp.faith || 0) < it.buy_faith,
              onclick: async () => { if (await confirmBox("Buy", `Buy ${it.item} for ${it.buy_faith} faith?`)) game.tool("buy", { city_id: c.id, item: it.item, currency: "Faith" }); } }, `${it.buy_faith}✝`)));
        }
      }
    }
    root.appendChild(el("div", { class: "muted section", style: { fontSize: "12px" } },
      "Tip: click a tile inside the city's area to lock a citizen onto it (🔒); click again to unlock."));
  }
  if (c.yield_breakdown) root.appendChild(el("details", { class: "section" }, el("summary", {}, "Yield breakdown"),
    ...Object.entries(c.yield_breakdown).map(([src, y]) => el("div", { class: "muted", style: { fontSize: "12px" } }, `${src}: `, ...yieldSpans(y)))));
  root.appendChild(el("div", { class: "section" }, el("h4", {}, "Buildings"),
    el("div", {}, c.buildings.length ? c.buildings.join(", ") : el("span", { class: "muted" }, "none"))));
}

const ACTIVITY_ICON = { fortify: "⛨", sleep: "z", heal: "+", build: "⚒", explore: "↻", automate: "↻", goto: "→" };

export function unitChip(game, u) {
  const status = u.activity ? ` ${ACTIVITY_ICON[u.activity] || ""}` : u.moves > 0 ? " ●" : "";
  return el("button", { class: `small chip ${game.selectedUnit === u.id ? "primary" : ""}`,
    title: `${u.name} #${u.id} · HP ${u.hp} · moves ${u.moves}${u.activity ? " · " + u.activity : ""}`,
    onclick: () => game.selectUnit(u.id) }, `${u.name}${status}`);
}

// ============================================================================
// Choosing beliefs (found / enhance a religion)
// ============================================================================
export async function chooseBeliefs(game, enhancing) {
  const r = await api.tool(game.gid, game.token, "get_religion");
  if (!r.ok) { toast(r.error, "error"); return null; }
  const info = r.result;
  const need = info.beliefs_to_choose_when_founding_or_enhancing || {};
  const avail = info.available_beliefs || {};
  return new Promise((resolve) => {
    const picks = {};
    let done = false;
    const name = el("input", { placeholder: "Religion name", value: `Faith of ${game.view.empire ? game.view.empire.name : ""}`, style: { width: "100%" } });
    const content = el("div", { class: "col" }, enhancing ? null : name);
    for (const [bt, n] of Object.entries(need)) {
      if (bt === "Any") continue;
      picks[bt] = [];
      content.appendChild(el("h4", {}, `${bt} belief${n > 1 ? "s" : ""} (choose ${n})`));
      for (const [b, uniques] of Object.entries(avail[bt] || {})) {
        const cb = el("input", { type: "checkbox", onchange: (e) => {
          if (e.target.checked) { if (picks[bt].length >= n) { e.target.checked = false; return; } picks[bt].push(b); }
          else picks[bt] = picks[bt].filter((x) => x !== b);
        } });
        content.appendChild(el("label", { class: "row", style: { alignItems: "flex-start" } }, cb, el("div", {}, el("b", {}, b), el("div", { class: "muted", style: { fontSize: "12px" } }, uniquesText(uniques)))));
      }
    }
    const m = modal({ title: enhancing ? "Enhance your religion" : "Found a religion", content,
      footer: [el("button", { onclick: () => m.close() }, "Cancel"),
        el("button", { class: "primary", onclick: () => { done = true; m.close(); resolve({ name: name.value, beliefs: Object.values(picks).flat() }); } }, "Confirm")],
      onClose: () => { if (!done) resolve(null); } });
  });
}

// ============================================================================
// Tech tree
// ============================================================================
export async function openTechTree(game) {
  if (game.isSpectator) return;
  const R = game.rules;
  const content = el("div", { class: "tech-tree" });
  const m = modal({ title: "Technology", content });
  content.parentElement.addEventListener("wheel", (e) => {
    const sc = content.parentElement;
    if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY) || sc.scrollHeight > sc.clientHeight + 4) return;
    sc.scrollLeft += e.deltaY;
    e.preventDefault();
  }, { passive: false });
  async function draw() {
    const r = await api.tool(game.gid, game.token, "get_tech_tree", { filter: "all" });
    if (!r.ok) { toast(r.error, "error"); return; }
    const tree = r.result;
    clear(content);
    const byId = Object.fromEntries(tree.techs.map((t) => [t.name, t]));
    const sci = game.view.empire ? (game.view.empire.per_turn || {}).science || 0 : 0;
    const remaining = (id) => {
      const seen = new Set();
      const walk = (tid) => {
        if (seen.has(tid) || byId[tid].status === "known") return 0;
        seen.add(tid);
        return Math.max(0, byId[tid].cost - (byId[tid].progress || 0)) + byId[tid].prerequisites.reduce((s, p) => s + walk(p), 0);
      };
      return walk(id);
    };
    const W = 180, H = 58, GX = 30, GY = 8, TOP = tree.free_techs > 0 ? 64 : 26;
    const pos = {};
    const cols = {};
    for (const t of tree.techs) { const col = R.techs[t.name].column; (cols[col] = cols[col] || []).push(t); }
    const colNums = Object.keys(cols).map(Number).sort((a, b) => a - b);
    let lastEra = null;
    colNums.forEach((cn, ci) => {
      const x = ci * (W + GX);
      cols[cn].forEach((t, ri) => { pos[t.name] = [x, TOP + ri * (H + GY)]; });
      const era = cols[cn][0].era;
      if (era !== lastEra) { content.appendChild(el("div", { class: "era-label", style: { left: x + "px" } }, era)); lastEra = era; }
    });
    const width = colNums.length * (W + GX);
    const maxY = Math.max(...Object.values(pos).map((p) => p[1])) + H + 10;
    content.style.width = width + "px";
    content.style.height = maxY + "px";
    const svgNS = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(svgNS, "svg");
    svg.setAttribute("width", width); svg.setAttribute("height", maxY);
    svg.style.position = "absolute"; svg.style.left = "0"; svg.style.top = "0";
    for (const t of tree.techs) {
      for (const p of t.prerequisites) {
        if (!pos[p]) continue;
        const [x1, y1] = pos[p], [x2, y2] = pos[t.name];
        const path = document.createElementNS(svgNS, "path");
        const sx = x1 + W, sy = y1 + H / 2, ex = x2, ey = y2 + H / 2;
        path.setAttribute("d", `M${sx},${sy} C${sx + 20},${sy} ${ex - 20},${ey} ${ex},${ey}`);
        path.setAttribute("stroke", byId[p].status === "known" ? "#2f7a45" : "#3a4254");
        path.setAttribute("fill", "none"); path.setAttribute("stroke-width", "1.5");
        svg.appendChild(path);
      }
    }
    content.appendChild(svg);
    for (const t of tree.techs) {
      const [x, y] = pos[t.name];
      const un = t.unlocks || {};
      const unl = [...(un.units || []), ...(un.buildings || []), ...(un.improvements || []), ...(un.reveals || []).map((r2) => "reveals " + r2),
                   ...(t.effects || [])];
      const cls = ["tech", t.status, tree.researching === t.name ? "researching" : "", (tree.queue || []).includes(t.name) && tree.researching !== t.name ? "goal" : ""].join(" ");
      const box = el("div", { class: cls, style: { left: x + "px", top: y + "px", width: W + "px" }, title: unl.join(", "),
        onclick: async () => {
          if (t.status === "known") return;
          // a granted free technology (Great Library, Liberty, ruins...) is spent before anything else:
          // clicking an available tech takes it for free rather than changing what is being researched
          if (tree.free_techs > 0 && t.status === "available") {
            const res = await game.tool("choose_free_tech", { tech: t.name });
            if (!res) return;
            toast(`Learned ${res.learned || t.name} for free`);
            if (res.free_techs_left > 0) draw(); else m.close();
            return;
          }
          const res = await game.tool("set_research", { tech: t.name });
          if (!res) return;
          toast(`Researching ${res.researching || t.name}`);
          if (!tree.researching) m.close(); else draw();
        } },
        el("div", { class: "n" }, t.name, el("span", { class: "muted", style: { float: "right", fontWeight: 400 }, title: `${t.cost} science` },
          t.status === "known" ? "✓" : sci > 0 ? `${Math.max(1, Math.ceil(remaining(t.name) / sci))} turns` : `${t.cost}`)),
        el("div", { class: "u" }, unl.join(", ") || "—"),
        t.progress ? el("div", { class: "bar" }, el("div", { style: { width: Math.min(100, t.progress / t.cost * 100) + "%" } })) : null);
      content.appendChild(box);
    }
    if (tree.free_techs > 0) {
      content.classList.add("free-pick");
      content.appendChild(el("div", { class: "free-tech-banner" },
        `⚗ Choose ${tree.free_techs} free technolog${tree.free_techs === 1 ? "y" : "ies"}: click any highlighted technology to learn it now.`));
    } else content.classList.remove("free-pick");
    const firstAvail = tree.free_techs > 0 ? tree.techs.find((t) => t.status === "available") : null;
    const cur = firstAvail ? pos[firstAvail.name] : tree.researching ? pos[tree.researching] : null;
    if (cur) setTimeout(() => { content.parentElement.scrollLeft = Math.max(0, cur[0] - 200); }, 10);
  }
  draw();
}

// ============================================================================
// Social policies
// ============================================================================
export async function openPolicies(game) {
  const content = el("div", { class: "col" });
  const m = modal({ title: "Social policies", content });
  async function draw() {
    const r = await api.tool(game.gid, game.token, "get_policies");
    if (!r.ok) { toast(r.error, "error"); return; }
    const p = r.result;
    clear(content);
    content.appendChild(el("div", {}, `Culture ${p.culture} · next policy costs ${p.next_policy_cost}` + (p.free_policies ? ` · ${p.free_policies} free polic${p.free_policies === 1 ? "y" : "ies"}` : "")));
    const grid = el("div", { class: "policy-grid" });
    for (const b of p.branches) {
      const card = el("div", { class: `card policy-branch ${b.status}` },
        el("div", { class: "row" }, el("b", {}, b.branch), el("span", { class: "muted" }, b.era || ""), el("span", { class: "grow" }),
          b.status === "adoptable" && game.myTurn ? el("button", { class: "small primary", onclick: async () => { if (await game.tool("adopt_policy", { policy: b.branch })) draw(); } }, "Open") :
            el("span", { class: "pill" }, b.status)),
        el("div", { class: "muted", style: { fontSize: "12px" } }, uniquesText(b.opening_effects)));
      for (const pol of b.policies) {
        card.appendChild(el("div", { class: `policy ${pol.adopted ? "adopted" : pol.adoptable ? "adoptable" : ""}`, title: uniquesText(pol.effects) },
          el("div", { class: "row" }, el("span", {}, pol.adopted ? "✓ " : "", pol.name), el("span", { class: "grow" }),
            pol.adoptable && game.myTurn ? el("button", { class: "small", onclick: async () => { if (await game.tool("adopt_policy", { policy: pol.name })) draw(); } }, "Adopt") : null),
          el("div", { class: "muted", style: { fontSize: "11px" } }, uniquesText(pol.effects))));
      }
      if (b.completion_effects.length) card.appendChild(el("div", { class: "muted", style: { fontSize: "11px" } }, "Completed: " + uniquesText(b.completion_effects)));
      grid.appendChild(card);
    }
    content.appendChild(grid);
  }
  draw();
  return m;
}

// ============================================================================
// Religion
// ============================================================================
export async function openReligion(game) {
  const content = el("div", { class: "col" });
  modal({ title: "Religion", content });
  async function draw() {
    const r = await api.tool(game.gid, game.token, "get_religion");
    if (!r.ok) { toast(r.error, "error"); return; }
    const d = r.result;
    clear(content);
    if (!d.enabled) { content.appendChild(el("p", { class: "muted" }, "Religion is disabled in this game.")); return; }
    content.appendChild(el("div", {}, `Faith ${d.faith} · ${d.your_religion ? d.your_religion + " (" + d.state + ")" : d.state === "none" ? "no pantheon yet" : d.state}`));
    if (d.your_beliefs) content.appendChild(el("div", { class: "muted" }, "Beliefs: " + d.your_beliefs.join(", ")));
    if (d.faith_for_next_great_prophet) content.appendChild(el("div", { class: "muted" }, `Next Great Prophet at ${d.faith_for_next_great_prophet} faith · religions still available: ${d.religions_remaining_to_found}`));
    if (d.pantheon_beliefs_available && d.state === "none") {
      content.appendChild(el("h4", {}, `Found a pantheon (${d.faith_for_pantheon} faith)`));
      for (const [b, uniques] of Object.entries(d.pantheon_beliefs_available)) {
        content.appendChild(el("div", { class: "item-row" }, el("div", { class: "name" }, el("b", {}, b), el("div", { class: "muted", style: { fontSize: "12px" } }, uniquesText(uniques))),
          el("button", { class: "small primary", disabled: !game.myTurn || d.faith < d.faith_for_pantheon,
            onclick: async () => { if (await game.tool("found_pantheon", { belief: b })) draw(); } }, "Choose")));
      }
    }
    if ((d.world_religions || []).length) {
      content.appendChild(el("h4", {}, "Religions of the world"));
      for (const w of d.world_religions) content.appendChild(el("div", {}, el("b", {}, w.religion), el("span", { class: "muted" }, ` founded by ${w.founder} · ${w.cities_following} cities · ${w.beliefs.join(", ")}`)));
    }
    content.appendChild(el("p", { class: "muted", style: { fontSize: "12px" } },
      "Great Prophets found (in one of your cities) and enhance religions; Missionaries and Prophets spread them; Inquisitors remove other religions. Faith also buys religious units and some buildings from the city screen."));
  }
  draw();
}

// ============================================================================
// Great people & espionage
// ============================================================================
export async function openGreatPeople(game) {
  const content = el("div", { class: "col" });
  modal({ title: "Great people & espionage", content });
  const [gp, sp, cities] = await Promise.all([api.tool(game.gid, game.token, "get_great_people"), api.tool(game.gid, game.token, "get_espionage"),
    api.tool(game.gid, game.token, "get_players")]);
  if (gp.ok) {
    const d = gp.result;
    content.appendChild(el("h4", {}, "Great people"));
    for (const p of d.progress) {
      const pct = p.needed ? Math.min(100, p.points / p.needed * 100) : 0;
      content.appendChild(el("div", {}, `${p.great_person}: ${p.points}/${p.needed}` + (d.points_per_turn[p.great_person] ? ` (+${d.points_per_turn[p.great_person]}/turn)` : ""),
        el("div", { class: "pbar" }, el("div", { style: { width: pct + "%" } }))));
    }
    const ga = d.golden_age;
    content.appendChild(el("div", { class: "section" }, ga.turns_left ? `Golden Age: ${ga.turns_left} turns left` : `Golden Age progress ${ga.progress}/${ga.needed} (from excess happiness)`));
    if (d.free_great_people_to_choose && game.myTurn) {
      const sel = el("select", {}, ...d.progress.filter((p) => !/General|Admiral/.test(p.great_person)).map((p) => el("option", { value: p.great_person }, p.great_person)));
      content.appendChild(el("div", { class: "row" }, `Choose a free Great Person:`, sel,
        el("button", { class: "primary small", onclick: () => game.tool("choose_great_person", { great_person: sel.value }) }, "Choose")));
    }
  }
  if (sp.ok && sp.result.enabled) {
    content.appendChild(el("h4", {}, "Spies"));
    if (!sp.result.spies.length) content.appendChild(el("div", { class: "muted" }, "No spies yet: civilizations recruit spies when entering the Renaissance and later eras."));
    const known = game.view.cities.filter((c) => !c.stale);
    for (const s of sp.result.spies) {
      const sel = el("select", {}, el("option", { value: "hideout" }, "Hideout"),
        ...known.map((c) => el("option", { value: c.id, selected: s.city_id === c.id }, `${c.name}${c.owner === game.you ? " (yours)" : ""}`)));
      content.appendChild(el("div", { class: "row" }, el("b", {}, s.name), el("span", { class: "muted" }, `rank ${s.rank} · ${s.location} · ${s.action}${s.turns ? " (" + s.turns + "t)" : ""}`),
        el("span", { class: "grow" }), game.myTurn ? sel : null,
        game.myTurn ? el("button", { class: "small", onclick: () => game.tool("move_spy", { spy: s.name, city_id: sel.value }) }, "Send") : null,
        game.myTurn && s.coup_chance_percent != null ? el("button", { class: "small danger", title: `Estimated success ${s.coup_chance_percent}%`,
          onclick: () => game.tool("stage_coup", { spy: s.name }) }, `Coup (${s.coup_chance_percent}%)`) : null));
    }
  }
}

// ============================================================================
// City-states
// ============================================================================
export async function openCityStates(game) {
  const content = el("div", { class: "col" });
  modal({ title: "City-states", content });
  async function draw() {
    const r = await api.tool(game.gid, game.token, "get_city_states");
    if (!r.ok) { toast(r.error, "error"); return; }
    clear(content);
    if (!r.result.length) { content.appendChild(el("p", { class: "muted" }, "You have not met any city-state yet.")); return; }
    const gold = game.view.empire ? game.view.empire.gold : 0;
    for (const cs of r.result) {
      const act = (action, extra = {}) => game.tool("city_state_action", { player_id: cs.id, action, ...extra }).then((x) => { if (x) { toast(JSON.stringify(x)); draw(); } });
      content.appendChild(el("div", { class: "card" },
        el("div", { class: "row" }, el("b", {}, cs.name), el("span", { class: "muted" }, `${cs.type} · ${cs.personality}`), el("span", { class: "grow" }),
          el("span", { class: `pill ${cs.at_war ? "war" : "peace"}` }, cs.at_war ? "War" : cs.relationship)),
        el("div", { class: "muted" }, `Influence ${cs.influence} (rests at ${cs.resting_point})${cs.ally ? " · ally: " + cs.ally : ""}${cs.you_protect ? " · under your protection" : ""}`),
        el("div", { class: "muted", style: { fontSize: "12px" } }, "Friend: " + cs.friend_bonuses.join("; ")),
        el("div", { class: "muted", style: { fontSize: "12px" } }, "Ally: " + cs.ally_bonuses.join("; ")),
        cs.quests && cs.quests.length ? el("div", { style: { fontSize: "12px" } }, "Quests: " + cs.quests.join(" · ")) : null,
        game.myTurn ? el("div", { class: "btn-grid" },
          ...[250, 500, 1000].map((g) => el("button", { class: "small", disabled: gold < g || cs.at_war, onclick: () => act("gift_gold", { amount: g }) }, `Gift ${g}g`)),
          el("button", { class: "small", disabled: cs.at_war, onclick: () => act(cs.you_protect ? "withdraw" : "pledge") }, cs.you_protect ? "Withdraw protection" : "Pledge protection"),
          cs.tribute.would_pay ? el("button", { class: "small danger", onclick: () => act("tribute_gold") }, `Demand ${cs.tribute.gold} gold`) : null,
          cs.tribute.would_pay ? el("button", { class: "small danger", onclick: () => act("tribute_worker") }, "Demand a worker") : null,
          cs.at_war ? el("button", { class: "small", onclick: () => act("make_peace") }, "Make peace") : null) : null));
    }
  }
  draw();
}

// ============================================================================
// Diplomacy
// ============================================================================
export async function openDiplomacy(game, focusPid = null) {
  if (game.isSpectator) return;
  if (game._diploModal) { game._diploModal.close(); }
  const content = el("div");
  const m = modal({ title: "Diplomacy", content, onClose: () => { game._diploModal = null; } });
  game._diploModal = m;
  const state = { selected: focusPid, give: [], receive: [] };

  async function draw() {
    const r = await api.tool(game.gid, game.token, "get_diplomacy", { message_limit: 200 });
    if (!r.ok) { toast(r.error, "error"); return; }
    const d = r.result;
    clear(content);
    if (d.united_nations) content.appendChild(el("div", { class: "card" }, `United Nations: next vote on turn ${d.united_nations.next_vote_turn}`,
      d.united_nations.voting_open && game.you != null ? el("span", {}, " · ",
        (() => { const sel = el("select", {}, el("option", { value: "abstain" }, "Abstain"), el("option", { value: game.you }, "Yourself"), ...d.players.map((p) => el("option", { value: p.id }, p.name)));
          return el("span", {}, sel, el("button", { class: "small primary", onclick: () => game.tool("un_vote", { candidate: sel.value }) }, "Vote")); })()) : null));
    if (!d.players.length) { content.appendChild(el("p", { class: "muted" }, "You have not met any other civilization yet. Explore!")); return; }
    if (state.selected == null || !d.players.some((p) => p.id === state.selected)) state.selected = d.players[0].id;
    const list = el("div", { class: "civ-list" });
    for (const p of d.players) {
      const neg = d.open_negotiations.find((n) => n.with === p.id);
      list.appendChild(el("div", { class: `civ ${state.selected === p.id ? "active" : ""}`, onclick: () => { state.selected = p.id; state.give = []; state.receive = []; draw(); } },
        el("div", {}, el("span", { class: "swatch", style: { background: p.color } }), el("b", {}, p.name)),
        el("div", { class: "row" },
          el("span", { class: `pill ${p.at_war ? "war" : "peace"}` }, p.at_war ? "War" : p.peace_treaty_until ? `Treaty → T${p.peace_treaty_until}` : "Peace"),
          el("span", { class: "muted" }, `score ${p.score} · ${p.cities} cities`)),
        neg ? el("div", { class: neg.your_move ? "warn" : "muted" }, neg.your_move ? "⚖ Awaiting your reply" : "⚖ Negotiating…") : null));
    }
    const p = d.players.find((q) => q.id === state.selected);
    const right = el("div", { class: "col" });
    const tags = [p.embassy_in_their_capital ? "your embassy" : null, p.their_embassy_with_you ? "their embassy" : null,
      p.friendship_until ? `friends → T${p.friendship_until}` : null, p.defensive_pact_until ? `defensive pact → T${p.defensive_pact_until}` : null,
      p.research_agreement_until ? `research agreement → T${p.research_agreement_until}` : null,
      p.open_borders_you_grant_until ? `you grant borders → T${p.open_borders_you_grant_until}` : null,
      p.open_borders_they_grant_until ? `they grant borders → T${p.open_borders_they_grant_until}` : null,
      p.you_denounced_them ? "you denounced them" : null, p.they_denounced_you ? "they denounced you" : null].filter(Boolean);
    right.appendChild(el("div", { class: "row" },
      el("h3", { style: { margin: 0 } }, p.name), p.leader ? el("span", { class: "muted" }, `led by ${p.leader}`) : null,
      p.difficulty ? el("span", { class: "muted", title: "This civilization's difficulty level" }, `· ${p.difficulty}`) : null, el("span", { class: "grow" }),
      !p.at_war && !p.you_denounced_them ? el("button", { class: "small", disabled: !game.myTurn, onclick: async () => {
        if (await confirmBox("Denounce", `Publicly denounce ${p.name}?`)) { await game.tool("denounce", { player_id: p.id }); draw(); }
      } }, "Denounce") : null,
      !p.at_war ? el("button", { class: "danger small", disabled: !game.myTurn || !!p.peace_treaty_until, onclick: async () => {
        if (!(await confirmBox("Declare war", `Declare war on ${p.name}? Deals and agreements with them end.`))) return;
        const msg = await prompt("Declaration", "Optional message to accompany the declaration:", "");
        if (msg === null) return;
        await game.tool("declare_war", { player_id: p.id, message: msg || undefined });
        draw();
      } }, "Declare war") : null));
    if (tags.length) right.appendChild(el("div", { class: "muted" }, tags.join(" · ")));

    const chat = el("div", { class: "chat" });
    const neg = d.open_negotiations.find((n) => n.with === p.id);
    const msgs = d.messages.filter((mm) => mm.from === p.id || (mm.from === game.you && mm.to.includes(p.name)));
    for (const mm of msgs) {
      chat.appendChild(el("div", { class: `msg ${mm.from === game.you ? "me" : "them"}` },
        el("div", { class: "meta" }, `T${mm.turn} ${mm.from_name}${mm.to.length > 1 ? " → " + mm.to.join(", ") : ""}`), mm.text));
    }
    for (const n of [...d.recent_negotiations.filter((n) => n.with === p.id).slice(-3), ...(neg ? [neg] : [])]) {
      chat.appendChild(el("div", { class: "muted", style: { textAlign: "center", margin: "6px 0" } },
        `— negotiation #${n.id}: ${n.status}${n.current_proposal ? " · " + n.current_proposal.summary : ""} —`));
    }
    right.appendChild(chat);
    setTimeout(() => { chat.scrollTop = chat.scrollHeight; }, 0);
    const text = el("textarea", { rows: 2, placeholder: "Write a message…", style: { width: "100%" } });
    right.appendChild(text);
    if (neg) {
      const box = el("div", { class: "card", style: { marginBottom: 0 } },
        el("div", {}, el("b", {}, `Negotiation #${neg.id}`), ` (${neg.exchanges}/${neg.max_exchanges} exchanges) — `,
          neg.your_move ? el("span", { class: "warn" }, "your move") : el("span", { class: "muted" }, `waiting for ${p.name}`)),
        neg.current_proposal ? el("div", {}, `Proposal by ${neg.proposal_by_you ? "you" : p.name}: `, el("b", {}, neg.current_proposal.summary)) : el("div", { class: "muted" }, "No concrete proposal on the table."));
      if (neg.your_move) {
        const respond = async (action, extra = {}) => {
          const res = await game.tool("respond_negotiation", { negotiation_id: neg.id, action, message: text.value || undefined, ...extra });
          if (res) { text.value = ""; state.give = []; state.receive = []; setTimeout(draw, 300); }
        };
        box.appendChild(el("div", { class: "btn-grid" },
          el("button", { class: "primary", disabled: !neg.current_proposal || neg.proposal_by_you, onclick: () => respond("accept") }, "Accept"),
          el("button", { class: "danger", onclick: () => respond("reject") }, "Reject / end"),
          el("button", { onclick: () => { if (!text.value) return toast("Write a reply first."); respond("reply"); } }, "Reply (message only)"),
          el("button", { disabled: !state.give.length && !state.receive.length, onclick: () => respond("counter", { give: state.give, receive: state.receive }) }, "Counter with the deal below"),
          neg.current_proposal ? el("button", { class: "small", onclick: () => {
            state.give = JSON.parse(JSON.stringify(neg.current_proposal.you_give));
            state.receive = JSON.parse(JSON.stringify(neg.current_proposal.you_receive));
            draw();
          } }, "Load current proposal into builder") : null));
      }
      right.appendChild(box);
    } else {
      right.appendChild(el("div", { class: "btn-grid" },
        el("button", { onclick: async () => { if (!text.value.trim()) return; const r2 = await game.tool("send_message", { to: p.id, text: text.value }); if (r2) { text.value = ""; draw(); } } }, "Send message"),
        el("button", { class: "primary", disabled: !game.myTurn, title: game.myTurn ? "" : "Negotiations can be opened on your turn",
          onclick: async () => {
            const hasDeal = state.give.length || state.receive.length;
            if (!text.value.trim() && !hasDeal) return toast("Write an opening message, or add items to a deal below.");
            const args = { to: p.id, message: text.value.trim() || "I propose this deal." };
            if (hasDeal) { args.give = state.give; args.receive = state.receive; }
            const r2 = await game.tool("open_negotiation", args);
            if (r2) { text.value = ""; state.give = []; state.receive = []; toast("Negotiation opened — waiting for their reply."); setTimeout(draw, 500); }
          } }, state.give.length || state.receive.length ? "Propose deal" : "Open negotiation")));
    }
    right.appendChild(dealBuilder(game, d, p, state, draw));
    content.appendChild(el("div", { class: "diplo" }, list, right));
  }
  game._redrawDiplomacy = () => { if (document.body.contains(content)) draw(); };
  draw();
}

const MUTUAL = ["peace_treaty", "declaration_of_friendship", "research_agreement", "defensive_pact"];

function dealBuilder(game, d, p, state, redraw) {
  const R = game.rules;
  const emp = game.view.empire;
  const wrap = el("div", { class: "section" }, el("h4", {}, "Deal builder"));
  const sideBox = (title, items) => el("div", { class: "deal-side" }, el("div", { class: "muted" }, title),
    ...items.map((it, i) => el("div", { class: "deal-item" }, describe(d, it),
      el("button", { class: "small", onclick: () => { items.splice(i, 1); redraw(); } }, "✕"))));
  wrap.appendChild(el("div", { class: "deal-builder" }, sideBox("You give", state.give), sideBox(`${p.name} gives`, state.receive)));
  const dd = game.rules.speeds[game.view.config.speed] ? game.rules.speeds[game.view.config.speed].dealDuration : 30;
  const to = p.trade_options;
  if (to) {
    const chip = (label, side, item) => el("button", { class: "small chip", title: "Add to the deal", onclick: () => { side.push(item); redraw(); } }, label);
    const offers = (opts, side) => [
      ...(opts.luxuries || []).map((r) => chip(`+ ${r}`, side, { type: "resource", resource: r, amount: 1, turns: dd })),
      ...Object.entries(opts.strategic || {}).map(([r, n]) => chip(`+ ${r} (${n} spare)`, side, { type: "resource", resource: r, amount: 1, turns: dd })),
      ...(opts.techs || []).map((tid) => chip(`+ ${tid}`, side, { type: "tech", tech: tid })),
    ];
    const agreements = (to.agreements_possible_now || []).map((t) => chip(`⇄ ${t.replace(/_/g, " ")}`, state.give, { type: t }));
    wrap.appendChild(el("div", { class: "trade-hints" },
      el("div", {}, el("span", { class: "muted" }, `${p.name} could give (${to.their_gold} gold): `), offers(to.they_could_give, state.receive).length ? offers(to.they_could_give, state.receive) : el("span", { class: "muted" }, "nothing you lack")),
      el("div", {}, el("span", { class: "muted" }, "You could give: "), offers(to.you_could_give, state.give).length ? offers(to.you_could_give, state.give) : el("span", { class: "muted" }, "nothing they lack")),
      agreements.length ? el("div", {}, el("span", { class: "muted" }, "Agreements possible now: "), agreements,
        to.research_agreement_cost_each ? el("span", { class: "muted" }, ` (research agreement costs each side ${to.research_agreement_cost_each} gold)`) : null) : null));
  }
  const type = el("select", {}, ...[["gold", "Gold"], ["gold_per_turn", "Gold per turn"], ["resource", "Resource"], ["open_borders", "Open borders"],
    ["embassy", "Embassy"], ["peace_treaty", "Peace treaty"], ["declaration_of_friendship", "Declaration of friendship"],
    ["research_agreement", "Research agreement"], ["defensive_pact", "Defensive pact"], ["declare_war", "Declare war on…"],
    ["city", "City"], ["share_map", "World map"], ["tech", "Technology"]].map(([v, t]) => el("option", { value: v }, t)));
  const params = el("span", { class: "row" });
  const inputs = {};
  const renderParams = () => {
    clear(params);
    const t = type.value;
    const num = (k, v, ph) => (inputs[k] = el("input", { type: "number", value: v, placeholder: ph, style: { width: "80px" } }));
    if (t === "gold") params.append(num("amount", 50, "amount"));
    if (t === "gold_per_turn") params.append(num("amount", 3, "per turn"), num("turns", dd, "turns"));
    if (t === "resource") {
      inputs.resource = el("select", {}, ...Object.entries(R.resources).filter(([, r]) => r.resourceType !== "Bonus").map(([k, r]) => el("option", { value: k }, `${k} (${r.resourceType})`)));
      params.append(inputs.resource, num("amount", 1, "qty"), num("turns", dd, "turns"));
    }
    if (t === "open_borders") params.append(num("turns", dd, "turns"));
    if (t === "declare_war") { inputs.target = el("select", {}, ...d.players.filter((q) => q.id !== p.id).map((q) => el("option", { value: q.id }, q.name))); params.append(inputs.target); }
    if (t === "city") { inputs.city = el("select", {}, ...game.view.cities.filter((c) => c.owner === game.you || c.owner === p.id).map((c) => el("option", { value: c.id }, `${c.name}${c.owner === game.you ? " (yours)" : ""}`))); params.append(inputs.city); }
    if (t === "tech") { inputs.tech = el("select", {}, ...Object.keys(R.techs).map((k) => el("option", { value: k }, k))); params.append(inputs.tech); }
  };
  type.onchange = renderParams;
  renderParams();
  const build = () => {
    const t = type.value;
    const it = { type: t };
    if (inputs.amount && ["gold", "gold_per_turn", "resource"].includes(t)) it.amount = +inputs.amount.value;
    if (inputs.turns && ["gold_per_turn", "resource", "open_borders"].includes(t)) it.turns = +inputs.turns.value;
    if (t === "resource") it.resource = inputs.resource.value;
    if (t === "declare_war") it.target = +inputs.target.value;
    if (t === "city") it.city_id = +inputs.city.value;
    if (t === "tech") it.tech = inputs.tech.value;
    return it;
  };
  wrap.appendChild(el("div", { class: "row", style: { marginTop: "6px" } }, type, params,
    el("button", { class: "small", onclick: () => { state.give.push(build()); redraw(); } }, "+ You give"),
    el("button", { class: "small", onclick: () => { state.receive.push(build()); redraw(); } }, `+ ${p.name} gives`)));
  wrap.appendChild(el("div", { class: "muted", style: { fontSize: "12px" } }, `Mutual agreements (${MUTUAL.join(", ").replace(/_/g, " ")}) apply to both sides automatically.`));
  if (emp) wrap.appendChild(el("div", { class: "muted", style: { marginTop: "4px" } },
    `You have ${emp.gold} gold (${signed((emp.per_turn || {}).gold || 0)}/turn). ` +
    `Spare: ${Object.entries(emp.strategic_resources || {}).filter(([, r]) => r.available > 0).map(([k, r]) => `${k} ${r.available}`).join(", ") || "no strategic resources"}; ` +
    `luxuries: ${Object.entries(emp.luxuries || {}).filter(([, r]) => r.net > 0).map(([k, r]) => `${k} ×${r.net}`).join(", ") || "none"}.`));
  return wrap;
}

function describe(d, it) {
  switch (it.type) {
    case "gold": return `${it.amount} gold`;
    case "gold_per_turn": return `${it.amount} gold/turn × ${it.turns}`;
    case "resource": return `${it.amount} ${it.resource} × ${it.turns}t`;
    case "open_borders": return `Open borders ${it.turns}t`;
    case "declare_war": { const q = d.players.find((x) => x.id === it.target); return `War on ${q ? q.name : it.target}`; }
    case "city": return `City #${it.city_id}`;
    case "share_map": return "World map";
    case "tech": return it.tech;
    default: return it.type.replace(/_/g, " ");
  }
}

// ============================================================================
// Empire overview & victory
// ============================================================================
export async function openEmpire(game) {
  const content = el("div", { class: "col" });
  modal({ title: "Empire", content });
  const [emp, cities] = await Promise.all([api.tool(game.gid, game.token, "get_empire"), api.tool(game.gid, game.token, "get_cities")]);
  if (!emp.ok) return;
  const e = emp.result;
  const h = e.happiness;
  const breakdown = (obj, key) => Object.entries(obj || {}).filter(([, v]) => v && v[key]).map(([k, v]) => `${k} ${signed(v[key])}`).join(" · ");
  content.append(
    el("div", { class: "grid2" },
      el("div", { class: "card" }, el("h3", {}, "Gold"), el("div", {}, `Treasury ${e.gold}, ${signed(e.per_turn.gold || 0)}/turn`),
        el("div", { class: "muted", style: { fontSize: "12px" } }, breakdown(e.per_turn_breakdown, "gold"))),
      el("div", { class: "card" }, el("h3", {}, "Happiness"), el("div", {}, `${h.total} (${h.status})`),
        el("div", { class: "muted", style: { fontSize: "12px" } }, Object.entries(h.breakdown).map(([k, v]) => `${k} ${signed(v)}`).join(" · ")),
        el("div", { class: "muted" }, `Luxuries: ${h.luxury_types.join(", ") || "none"}`),
        el("div", { class: "muted" }, e.golden_age.turns_left ? `Golden Age: ${e.golden_age.turns_left} turns` : `Golden Age ${e.golden_age.progress}/${e.golden_age.needed}`)),
      el("div", { class: "card" }, el("h3", {}, "Science"), el("div", {}, `+${fmt(e.per_turn.science || 0)}/turn · ${e.techs_known} techs · ${e.era}`),
        el("div", { class: "muted" }, e.researching ? `Researching ${e.researching} (${e.research_progress}/${e.research_cost}, ${e.research_turns} turns)` : "Nothing selected"),
        el("div", { class: "muted", style: { fontSize: "12px" } }, breakdown(e.per_turn_breakdown, "science"))),
      el("div", { class: "card" }, el("h3", {}, "Culture & faith"),
        el("div", {}, `Culture ${e.culture} (+${fmt(e.per_turn.culture || 0)}) · Faith ${e.faith} (+${fmt(e.per_turn.faith || 0)})`),
        el("div", { class: "muted" }, `Policies: ${(e.policies || []).filter((x) => !x.endsWith(" Complete")).join(", ") || "none"}`),
        e.religion && e.religion.name ? el("div", { class: "muted" }, `Religion: ${e.religion.name}`) : null)),
    el("div", { class: "muted" }, `Units ${e.unit_supply.units}/${e.unit_supply.supply} supply` + (e.unit_supply.production_penalty_percent ? ` (production −${e.unit_supply.production_penalty_percent}%)` : "")),
    el("h3", {}, "Cities"),
    el("table", { class: "list" }, el("tr", {}, ...["City", "Pop", "Food", "Prod", "Gold", "Sci", "Cul", "Faith", "Building"].map((h2) => el("th", {}, h2))),
      ...(cities.ok ? cities.result : []).map((c) => el("tr", { style: { cursor: "pointer" }, onclick: () => { game.renderer.centerOn(c.x, c.y); game.selectCity(c.id); document.querySelector(".modal-back").remove(); } },
        el("td", {}, c.name), el("td", {}, c.pop), el("td", { class: "food" }, fmt(c.yields.food, 1)), el("td", { class: "prod" }, fmt(c.yields.production, 1)),
        el("td", { class: "gold" }, fmt(c.yields.gold, 1)), el("td", { class: "sci" }, fmt(c.yields.science, 1)), el("td", { class: "cul" }, fmt(c.yields.culture, 1)),
        el("td", { class: "faith" }, fmt(c.yields.faith, 1)),
        el("td", {}, c.queue.length ? `${c.queue[0].item}${c.queue[0].turns ? " (" + c.queue[0].turns + ")" : ""}` : el("span", { class: c.puppet ? "muted" : "warn" }, c.puppet ? "puppet" : "idle"))))));
  const units = game.view.units.filter((u) => u.owner === game.you);
  const order = { melee: 0, ranged: 1, mounted: 2, siege: 3, armor: 4, recon: 5, naval_melee: 6, naval_ranged: 7, air: 8, civilian: 9 };
  units.sort((a, b) => (order[a.class] ?? 8) - (order[b.class] ?? 8) || a.type.localeCompare(b.type));
  const close = () => { const back = content.closest(".modal-back"); if (back) back.remove(); };
  const actLabel = { fortify: "fortified", sleep: "sleeping", heal: "healing", explore: "exploring", automate: "automated", build: "building", goto: "moving" };
  content.append(el("h3", {}, `Units (${units.length})`),
    el("table", { class: "list" }, el("tr", {}, ...["Unit", "Where", "HP", "Status", ""].map((h2) => el("th", {}, h2))),
      ...units.map((u) => el("tr", {},
        el("td", {}, u.name, u.promotion_ready ? el("span", { class: "pill live", style: { marginLeft: "4px" } }, "promotion") : null),
        el("td", {}, (() => { const c = game.view.cities.find((x) => x.x === u.x && x.y === u.y); return c ? c.name : `(${u.x},${u.y})`; })()),
        el("td", {}, String(u.hp)),
        el("td", { class: "muted" }, u.activity ? actLabel[u.activity] || u.activity : u.moves > 0 ? el("span", { class: "warn" }, "needs orders") : "done"),
        el("td", {}, el("div", { class: "row" },
          el("button", { class: "small", onclick: () => { close(); game.renderer.centerOn(u.x, u.y); game.selectUnit(u.id); } }, "Show"),
          game.myTurn ? el("button", { class: "small danger", onclick: async () => {
            if (!(await confirmBox("Disband", `Disband ${u.name} #${u.id}?`))) return;
            const r = await game.tool("unit_order", { unit_id: u.id, order: "disband" });
            if (r) { close(); await game.syncView(); openEmpire(game); }
          } }, "Disband") : null))))));
}

export async function openVictory(game) {
  const content = el("div", { class: "col" });
  modal({ title: "Victory", content });
  const r = await api.tool(game.gid, game.token, "get_victory_status");
  if (!r.ok) return;
  const v = r.result;
  content.appendChild(el("div", {}, `Turn ${v.turn}/${v.turn_limit} (${v.year}) · your score ${v.your_score.total}`));
  content.appendChild(el("div", { class: "muted" }, Object.entries(v.scores).map(([n, s]) => `${n}: ${s}`).join(" · ")));
  for (const [name, prog] of Object.entries(v.your_progress)) {
    content.appendChild(el("div", { class: "card" }, el("b", {}, name), el("span", { class: "muted" }, ` ${prog.completed}/${prog.total}`),
      ...prog.milestones.map((m2) => el("div", { class: m2.done ? "good" : "muted" }, `${m2.done ? "✓" : "○"} ${m2.milestone}`))));
  }
  if (v.spaceship) content.appendChild(el("div", { class: "muted" }, `Spaceship: Apollo ${v.spaceship.apollo_program ? "✓" : "✗"} · ` +
    Object.entries(v.spaceship.parts).map(([k, p]) => `${k} ${p.added}/${p.needed}`).join(" · ")));
  if (v.original_capitals.length) content.appendChild(el("div", { class: "muted" }, "Original capitals: " + v.original_capitals.map((c) => `${c.city} (${c.owner})`).join(", ")));
  if (v.united_nations) content.appendChild(el("div", { class: "muted" }, `United Nations vote on turn ${v.united_nations.next_vote_turn}; ${v.united_nations.votes_needed} votes needed.`));
}

export async function openNotes(game) {
  const r = await api.tool(game.gid, game.token, "read_notes");
  const ta = el("textarea", { rows: 16, style: { width: "100%" }, value: r.ok && r.result !== "(empty)" ? r.result : "" });
  const m = modal({ title: "Notebook", content: el("div", { class: "col" }, el("div", { class: "muted" }, "Private notes, saved with the game."), ta),
    footer: [el("button", { class: "primary", onclick: async () => { await game.tool("write_notes", { text: ta.value }); toast("Notes saved"); m.close(); } }, "Save")] });
}

export function openHelp() {
  modal({ title: "How to play", content: el("div", { class: "col" },
    el("p", {}, "Left-click your units or cities to select them. With a unit selected, click (or right-click) a tile to move there; red tiles are attack targets. Drag to pan, scroll to zoom."),
    el("table", { class: "list" }, ...[
      ["N / Tab", "next unit needing orders"], ["F", "fortify"], ["S", "sleep"], ["Space", "skip turn"], ["H", "heal"], ["E", "explore"],
      ["A", "automate worker"], ["B", "found city"], ["R", "build road"], ["P", "pillage"], ["T", "tech tree"], ["O", "social policies"],
      ["D", "diplomacy"], ["C", "center on capital"], ["Y", "tile yields"], ["Shift+Enter", "end turn"], ["Esc", "deselect"]].map(([k, v]) => el("tr", {}, el("td", {}, el("code", {}, k)), el("td", {}, v)))),
    el("p", { class: "muted" }, "Borders show territory. Dimmed tiles are remembered but not currently visible; black is unexplored. Blue edges are rivers; ✦ marks a natural wonder; ! ancient ruins. Resource badges: green = bonus, orange = strategic, purple = luxury.")) });
}

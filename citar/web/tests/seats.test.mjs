// Seats as the pages name and send them (citar/web/js/seats.js; design/phase3-features.md P3.6.7, P3.6.8): the seat
// label for each seat shape ("Hybrid: <bot> + <model>"), the hybrid seat's owners and presets, the lobby's payloads
// (fixtures/hybrid-seats.json, which tests/test_hybrid_lobby.py posts), the seat route's body, and what an AI seat is
// doing as the game screen says it. Run with `node --test "citar/web/tests/*.test.mjs"` from the checkout.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  CATEGORIES, CATEGORY_KEYS, HANDICAPS, PRESETS, agentState, botName, controllerLabel, diplomatText, hybridBlock,
  hybridProblem, hybridState, isModelSeat, modelCategories, modelName, newHybrid, outcomeText, ownersMap, presetOf,
  queuedText, seatLabel, seatPayload, seatUpdate,
} from "../js/seats.js";

const FIXTURE = JSON.parse(readFileSync(new URL("./fixtures/hybrid-seats.json", import.meta.url), "utf-8"));
const DRY = { server_id: "sv_dryrun", model: "dry-run" };

test("the ten kinds of diplomacy are the engine's, in its order", () => {
  assert.deepEqual(CATEGORY_KEYS, ["trades", "agreements", "peace", "war", "denounce", "un", "city_states", "espionage",
    "captured_cities", "chat"]);
  assert.equal(CATEGORIES.length, 10);
  for (const [, label, help] of CATEGORIES) assert.ok(label && help);
  assert.deepEqual(HANDICAPS.map(([k]) => k), ["ai", "human"]);
  assert.equal(HANDICAPS[0][1], "AI bonuses, like the bot");
  assert.equal(HANDICAPS[1][1], "A person's values, like a model");
});

test("the presets give the model what the design says", () => {
  assert.deepEqual(modelCategories("trade_only"), ["trades", "chat"]);
  assert.deepEqual(modelCategories("all_but_war"), CATEGORY_KEYS.filter((k) => k !== "war"));
  assert.deepEqual(modelCategories("everything"), CATEGORY_KEYS);
  assert.deepEqual(PRESETS.map(([k]) => k), ["trade_only", "all_but_war", "everything"]);
  for (const [name] of PRESETS) assert.equal(presetOf(ownersMap(name)), name);
  assert.equal(presetOf({ trades: "llm" }), null, "a grid of its own");
  assert.equal(presetOf({ trades: "llm", chat: "llm" }), "trade_only", "a partial map reads as the whole");
  assert.deepEqual(modelCategories("no_such_preset"), [], "an unknown preset gives the model nothing");
  assert.equal(ownersMap({ war: "llm", bribes: "llm", chat: "model" }).war, "llm");
  assert.equal(ownersMap({ chat: "model" }).chat, "bot", "only \"llm\" is the model's");
  assert.equal(Object.keys(ownersMap({ bribes: "llm" })).length, 10, "unknown categories are not kept");
});

test("a new hybrid seat is trade only, with the advisor, called every 10 turns", () => {
  const h = newHybrid();
  assert.deepEqual(modelCategories(h.owners), ["trades", "chat"]);
  assert.equal(h.advisor, true);
  assert.equal(h.prompt_every, 10);
  assert.deepEqual(hybridBlock(h), { owners: "trade_only", advisor: true, prompt_every: 10 });
  assert.equal(hybridProblem(h), null);
});

test("a seat the server sent comes back to the form as the whole map, and goes out as the lobby sends it", () => {
  const sent = { owners: { trades: "llm", agreements: "bot", peace: "bot", war: "bot", denounce: "bot", un: "bot",
    city_states: "bot", espionage: "bot", captured_cities: "bot", chat: "llm" }, advisor: false, prompt_every: 4,
    phase_seconds: 120, preset: "trade_only" };
  const st = hybridState(sent);
  assert.deepEqual(modelCategories(st.owners), ["trades", "chat"]);
  assert.equal(st.advisor, false);
  assert.equal(st.prompt_every, 4);
  assert.deepEqual(hybridBlock(sent), { owners: "trade_only", advisor: false, prompt_every: 4, phase_seconds: 120 },
    "phase_seconds kept; preset is the server's to say");
  assert.deepEqual(hybridState({ owners: "everything" }).owners, ownersMap("everything"));
  assert.deepEqual(hybridState(null), newHybrid());
  assert.equal(hybridState({ prompt_every: "" }).prompt_every, 10, "an empty field is the default");
});

test("the form refuses what the server would: a model with nothing to decide, a number of turns out of range", () => {
  assert.match(hybridProblem({ owners: {} }), /at least one kind/);
  assert.match(hybridProblem({ owners: "trade_only", prompt_every: 0 }), /1 to 50/);
  assert.match(hybridProblem({ owners: "trade_only", prompt_every: 51 }), /1 to 50/);
  assert.match(hybridProblem({ owners: "trade_only", prompt_every: 2.5 }), /whole number/);
  assert.equal(hybridProblem({ owners: "everything", prompt_every: 50 }), null);
});

test("the lobby's payload for each hybrid seat of the fixture (the payloads the Python test creates games from)", () => {
  assert.ok(FIXTURE.cases.length >= 5);
  const presets = new Set();
  const handicaps = new Set();
  for (const c of FIXTURE.cases) {
    const seat = { ...structuredClone(c.seat), llm: structuredClone(FIXTURE.llm) };
    assert.deepEqual(seatPayload(seat), { ...c.payload, llm: FIXTURE.llm }, c.name);
    presets.add(typeof c.payload.hybrid.owners === "string" ? c.payload.hybrid.owners : "custom");
    handicaps.add(c.payload.handicap);
  }
  assert.deepEqual([...presets].sort(), ["all_but_war", "custom", "everything", "trade_only"], "each preset and a grid");
  assert.deepEqual([...handicaps].sort(), ["ai", "human"], "each handicap");
});

test("other seats are sent as before: no handicap, no hybrid block", () => {
  const bot = seatPayload({ type: "bot", civ_name: "", nation: "random", color: "#aa0000", bot: { profile: "best" },
    hybrid: newHybrid(), handicap: "human" });
  assert.deepEqual(bot, { type: "bot", civ_name: null, nation: null, difficulty: null, color: "#aa0000", llm: undefined,
    bot: { profile: "best" } });
  const llm = seatPayload({ type: "llm", nation: "Rome", difficulty: "King", color: "#00aa00", llm: DRY });
  assert.equal(llm.nation, "Rome");
  assert.equal(llm.difficulty, "King");
  assert.ok(!("handicap" in llm) && !("hybrid" in llm));
});

test("the seat route's body (the Join/Seats dialog) says what the chosen type plays", () => {
  const st = { type: "hybrid", llm: DRY, bot: { profile: "standard" }, hybrid: newHybrid(), handicap: "human" };
  assert.deepEqual(seatUpdate(st), { type: "hybrid", llm: DRY, bot: { profile: "standard" }, handicap: "human",
    hybrid: { owners: "trade_only", advisor: true, prompt_every: 10 } });
  assert.deepEqual(seatUpdate({ ...st, type: "bot" }), { type: "bot" }, "back to a bot: no model, no handicap");
  assert.deepEqual(seatUpdate({ ...st, type: "llm" }), { type: "llm", llm: DRY });
  assert.deepEqual(seatUpdate({ ...st, type: "human" }), { type: "human" });
  assert.equal(seatUpdate({ ...st, handicap: undefined }).handicap, "ai");
});

test("the seat label for each seat shape", () => {
  const info = { label: "Dry run model", server: "Dry run" };
  const cases = [
    [{ type: "hybrid", bot: { profile: "standard" }, llm: { server_id: "sv", model: "dry-run" } }, "Hybrid: standard + dry-run"],
    [{ type: "hybrid", bot: { profile: "aggressive", chosen_as: "best" }, llm: { server_id: "sv", model: "gemma" } },
      "Hybrid: aggressive (best) + gemma"],
    [{ type: "hybrid", bot: {}, llm: { server_id: "sv", model: "dry-run" }, llm_info: info }, "Hybrid: standard + Dry run model"],
    [{ type: "hybrid", llm: { server_id: "sv", model_id: "m_1" } }, "Hybrid: standard + m_1"],
    [{ type: "hybrid", bot: { profile: "standard" }, llm: { provider: "dryrun" } }, "Hybrid: standard + dryrun"],
    [{ type: "hybrid", bot: { profile: "standard" }, llm: { server_id: "sv" } }, "Hybrid: standard + its model"],
    [{ type: "hybrid", bot: { profile: "standard" }, llm: {} }, "Hybrid: standard + no model"],
    [{ type: "hybrid" }, "Hybrid: standard + no model"],
    [{ type: "llm", llm: { model: "dry-run" } }, "LLM: dry-run"],
    [{ type: "llm", llm: { model: "dry-run" }, llm_info: info }, "LLM: Dry run model @ Dry run"],
    [{ type: "llm", llm: {} }, "LLM: ?"],
    [{ type: "bot", bot: { profile: "standard" } }, "Bot: standard"],
    [{ type: "bot", bot: { profile: "rival", chosen_as: "best" } }, "Bot: rival (best)"],
    [{ type: "bot" }, "Bot: standard"],
    [{ type: "mcp", connected: true }, "MCP (connected)"],
    [{ type: "mcp" }, "MCP"],
    [{ type: "human" }, "Human"],
    [{ type: "script" }, "Probe script"],
  ];
  for (const [seat, want] of cases) assert.equal(seatLabel(seat), want, JSON.stringify(seat));
  assert.equal(seatLabel(null), "");
  assert.equal(botName(undefined), "standard");
  assert.equal(modelName({ llm: { provider: "dryrun" } }), "dryrun");
});

test("a hybrid seat without a model plays none; an LLM seat always is a model seat", () => {
  assert.equal(isModelSeat({ type: "hybrid", llm: DRY }), true);
  assert.equal(isModelSeat({ type: "hybrid", llm: {} }), false);
  assert.equal(isModelSeat({ type: "hybrid" }), false);
  assert.equal(isModelSeat({ type: "llm" }), true);
  assert.equal(isModelSeat({ type: "bot", llm: DRY }), false);
  assert.equal(isModelSeat(null), false);
});

test("what a hybrid seat is doing: bot playing, diplomat thinking, queued #n, diplomat paused", () => {
  const hybrid = { type: "hybrid" };
  assert.equal(agentState(hybrid, "thinking"), "bot playing");
  assert.equal(agentState(hybrid, "diplomat"), "diplomat thinking");
  assert.equal(agentState(hybrid, "queued", { machine: "Box", position: 2, eta_s: 30 }), "queued #2");
  assert.equal(agentState(hybrid, "queued"), "queued", "a page opened mid-wait has no place");
  assert.equal(agentState(hybrid, "paused"), "diplomat paused");
  assert.equal(agentState(hybrid, "idle"), null);
  assert.equal(agentState(hybrid, undefined), null);
});

test("what an LLM seat (and a bot) is doing reads as before", () => {
  const llm = { type: "llm" };
  assert.equal(agentState(llm, "thinking"), "thinking…");
  assert.equal(agentState(llm, "reconnecting"), "reconnecting to its model server…");
  assert.equal(agentState(llm, "paused"), "paused mid-turn");
  assert.equal(agentState(llm, "queued", { machine: "Test GPU box", position: 2, eta_s: 40 }),
    "waiting for Test GPU box: 2nd in line, about 40 s");
  assert.equal(agentState({ type: "bot" }, "thinking"), "thinking…");
  assert.equal(queuedText(null), "waiting for its machine…");
  assert.equal(queuedText({ position: 11 }), "waiting for its machine: 11th in line");
  assert.equal(queuedText({ machine: "Box", position: 23 }), "waiting for Box: 23rd in line");
});

test("the stats screen names a hybrid row and sums up its diplomat", () => {
  assert.equal(controllerLabel({ controller: "hybrid", bot: "standard", model: "dry-run" }), "Hybrid: standard + dry-run");
  assert.equal(controllerLabel({ controller: "hybrid", bot: "standard", model: null }), "Hybrid: standard + no model");
  assert.equal(controllerLabel({ controller: "hybrid", bot: "rival", bot_best: true, model: "gemma" }),
    "Hybrid: rival (best) + gemma", "as seatLabel names the seat");
  assert.equal(controllerLabel({ controller: "llm", model: "gemma" }), "gemma");
  assert.equal(controllerLabel({ controller: "llm" }), "llm");
  assert.equal(controllerLabel({ controller: "bot" }), "bot");
  assert.equal(diplomatText({ controller: "hybrid" }), null, "never called");
  assert.equal(diplomatText({ diplomat_phases: 8, diplomat_outcomes: { done: 7, skipped: 1 }, avg_diplomat_steps: 3.46,
    avg_diplomat_gen_s: 2.13, avg_diplomat_queue_s: 0.4, diplomat_refused: { category: 2, veto: 1 } }),
  "8 calls (7 done, 1 skipped), 3.5 steps each, model 2.1 s, waited 0.4 s; refused: 2 outside its kinds, 1 vetoed by the bot");
  assert.equal(diplomatText({ diplomat_phases: 1, diplomat_outcomes: { refused: 1 } }), "1 call (1 refused)");
  assert.equal(diplomatText({ diplomat_phases: 0, diplomat_refused: { veto: 1 } }), "0 calls; refused: 1 vetoed by the bot");
  assert.equal(outcomeText({ outcome: "skipped", cause: "queued_behind_benchmark" }), "skipped (queued behind benchmark)");
  assert.equal(outcomeText({ outcome: "done", cause: null }), "done");
  assert.equal(outcomeText({}), "…");
});

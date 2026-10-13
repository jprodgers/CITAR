// The diplomacy leaderboard as the pages word it (citar/web/js/diplomacy.js; design/phase3-features.md P3.6.10): the
// Models page's Diplomacy tab rows from GET /api/models/diplomacy, a hybrid job's pair line on the Benchmarks page,
// its control's status and a hybrid probe case's answering side. Run with `node --test "citar/web/tests/*.test.mjs"`.
import { test } from "node:test";
import assert from "node:assert/strict";

import { answeredText, boardRows, controlText, diffText, fallbackText, num, ownersText, pairText, pct, signed }
  from "../js/diplomacy.js";

test("numbers keep their sign and their rounding", () => {
  assert.equal(signed(4), "+4");
  assert.equal(signed(-2.345), "−2.3");
  assert.equal(signed(0.04), "0");
  assert.equal(signed(null), "–");
  assert.equal(signed(29, 0), "+29");
  assert.equal(num(3.333), "3.3");
  assert.equal(num(undefined), "–");
  assert.equal(pct(0.875), "88%");
  assert.equal(pct(null), "–");
});

test("owners are named as the lobby names the presets", () => {
  assert.equal(ownersText("all_but_war"), "All but war");
  assert.equal(ownersText("trade_only"), "Trade only");
  assert.equal(ownersText("everything"), "Everything");
  assert.equal(ownersText("custom: trades, chat"), "Custom: Trades, Talk");
  assert.equal(ownersText("custom"), "Custom");
  assert.equal(ownersText(null), "–");
});

const LINE = {
  model: "dry-a", owners: "all_but_war", pairs: 3, pooled_pairs: 2, mean_score_diff: 4, ci90: [-2.33, 9.07],
  deals_per_game: 2, mean_deal_value: 3.33, acceptance_rate: 0.5, wars_declared_per_game: 1, captures_per_game: 0.33,
  losses_per_game: 0.67, control_deals_per_game: 1.33, control_mean_deal_value: 2.5, fallbacks_per_game: 0.33,
  other_fallbacks: { queued_behind_benchmark: 2, refused: 1 },
  games: [{ run: "run A", run_id: "r1", job_id: "j1", scenario: "duel", seed: 7, score_diff: { end: 10, 100: 3 }, deals: 2 }],
};

test("a line's score difference carries its 90% interval", () => {
  assert.equal(diffText(LINE), "+4 (90%: −2.3 to +9.1)");
  assert.equal(diffText({ mean_score_diff: -1, ci90: null }), "−1");
});

test("the model's fallbacks are counted, the others listed beside them", () => {
  assert.equal(fallbackText(LINE), "0.3 a game; not the model's: 2 queued behind a benchmark, 1 refused");
  assert.equal(fallbackText({ fallbacks_per_game: 0, other_fallbacks: {} }), "0 a game");
  assert.equal(fallbackText({ fallbacks_per_game: 1, other_fallbacks: { not_started: 2 } }),
    "1 a game; not the model's: 2 not started in time");
});

test("the board's rows are the lines in the server's order, as text", () => {
  const rows = boardRows({ lines: [LINE, { ...LINE, model: "dry-b", owners: "custom: trades", pooled_pairs: 0, games: [] }] });
  assert.equal(rows.length, 2);
  assert.deepEqual({ ...rows[0], games: undefined }, {
    model: "dry-a", owners: "All but war", pairs: 3, pooled: 2, diff: "+4 (90%: −2.3 to +9.1)", deals: "2",
    value: "3.3", acceptance: "50%", wars: "1", captures: "0.3 / 0.7", control: "1.3 deals, 2.5 each",
    fallbacks: "0.3 a game; not the model's: 2 queued behind a benchmark, 1 refused", games: undefined,
  });
  assert.equal(rows[0].games.length, 1);
  assert.equal(rows[1].owners, "Custom: Trades");
  assert.deepEqual(boardRows({}), []);
  assert.deepEqual(boardRows(null), []);
});

test("a hybrid job's pair line in a few words", () => {
  const pair = { score_diff: { end: 29, 200: -4, 100: 5 }, deals: 16, deal_value_mean: 6.25,
    control: { deals: 9, deal_value_mean: -9.44 } };
  assert.equal(pairText(pair), "+29 vs its control (T100 +5, T200 −4) · 16 deals at 6.3 (control 9 at −9.4)");
  assert.equal(pairText({ score_diff: { end: 0 }, deals: 1, deal_value_mean: null, control: {} }),
    "0 vs its control · 1 deal at – (control 0 at –)");
  assert.equal(pairText(null), null);
});

test("a control game's status and a probe case's answering side", () => {
  assert.equal(controlText({ status: "done" }), "control game played");
  assert.equal(controlText({ status: "queued" }), "control game queued");
  assert.equal(controlText({ status: "failed", error: "boom" }), "control game failed: boom");
  assert.equal(controlText(null), null);
  assert.equal(answeredText({ answered_by: "bot", answers: ["bot"] }), "answered by its bot");
  assert.equal(answeredText({ answered_by: "model", answers: ["model", "bot"] }), "answered by its model (then its bot)");
  assert.equal(answeredText({ answered_by: "model" }), "answered by its model");
  assert.equal(answeredText({}), null);
});

// The diplomacy leaderboard as the pages word it (design/phase3-features.md P3.6.10): the Models page's Diplomacy tab
// (GET /api/models/diplomacy: per model and owners preset, the pairs of hybrid benchmark jobs and their bot-only
// control games), a hybrid job's pair line on the Benchmarks page, and a hybrid probe run's sides. Pure: no DOM, no
// network; citar/web/tests/diplomacy.test.mjs checks it.
import { CATEGORIES, PRESETS } from "./seats.js";

/** A number with its sign ("+4", "−2.5", "0"), rounded to `digits`; "–" for none. */
export function signed(v, digits = 1) {
  if (v == null || Number.isNaN(v)) return "–";
  const r = Math.round(v * 10 ** digits) / 10 ** digits;
  if (r === 0) return "0";
  return `${r > 0 ? "+" : "−"}${Math.abs(r)}`;
}

/** A plain number rounded to `digits` (a minus sign for a negative one, as `signed` writes it); "–" for none. */
export function num(v, digits = 1) {
  if (v == null || Number.isNaN(v)) return "–";
  const r = Math.round(v * 10 ** digits) / 10 ** digits;
  return r < 0 ? `−${Math.abs(r)}` : String(r);
}

/** A rate (0..1) as a percentage; "–" for none. */
export function pct(v) {
  return v == null ? "–" : `${Math.round(v * 100)}%`;
}

/** Who decides what, as the board files a pair (`diplomacy_stats.owners_label`): a preset's name ("All but war"), or
 *  "Custom: Trades, Talk" for a grid of its own. */
export function ownersText(label) {
  if (!label) return "–";
  const preset = PRESETS.find(([name]) => name === label);
  if (preset) return preset[1];
  if (label.startsWith("custom: ")) {
    const words = Object.fromEntries(CATEGORIES.map(([k, w]) => [k, w]));
    return `Custom: ${label.slice(8).split(", ").map((k) => words[k] || k).join(", ")}`;
  }
  return label === "custom" ? "Custom" : label;
}

/** A line's mean score difference with its 90% interval: "+4 (90%: −2.3 to +9.1)". */
export function diffText(line) {
  if (!line) return "–";
  const ci = line.ci90;
  return ci ? `${signed(line.mean_score_diff)} (90%: ${signed(ci[0])} to ${signed(ci[1])})` : signed(line.mean_score_diff);
}

/** A line's fallbacks: the model's per game, and beside them the ones it did not cause, by cause ("0.3 a game; not the
 *  model's: 2 queued behind a benchmark, 1 refused"). */
export function fallbackText(line) {
  if (!line) return "–";
  const words = { queued_behind_benchmark: "queued behind a benchmark", refused: "refused", not_started: "not started in time" };
  const others = Object.entries(line.other_fallbacks || {}).filter(([, v]) => v)
    .map(([k, v]) => `${v} ${words[k] || k.replace(/_/g, " ")}`);
  const mine = `${num(line.fallbacks_per_game)} a game`;
  return others.length ? `${mine}; not the model's: ${others.join(", ")}` : mine;
}

/** The board's lines as table rows of text, in the server's order (best mean score difference first). */
export function boardRows(data) {
  return ((data && data.lines) || []).map((l) => ({
    model: l.model, owners: ownersText(l.owners), pairs: l.pairs, pooled: l.pooled_pairs || 0,
    diff: diffText(l), deals: num(l.deals_per_game), value: num(l.mean_deal_value),
    acceptance: pct(l.acceptance_rate), wars: num(l.wars_declared_per_game),
    captures: `${num(l.captures_per_game)} / ${num(l.losses_per_game)}`,
    control: `${num(l.control_deals_per_game)} deals, ${num(l.control_mean_deal_value)} each`,
    fallbacks: fallbackText(l), games: l.games || [],
  }));
}

/** A hybrid job's pair line (its result's `pair`) in a few words for its row on the Benchmarks page: "+29 vs its
 *  control (T100 +5) · 16 deals at 6.3 (control 9 at −9.4)". */
export function pairText(pair) {
  if (!pair) return null;
  const d = pair.score_diff || {};
  const turns = Object.keys(d).filter((k) => k !== "end").sort((a, b) => +a - +b).map((k) => `T${k} ${signed(d[k], 0)}`);
  const c = pair.control || {};
  return `${signed(d.end, 0)} vs its control${turns.length ? ` (${turns.join(", ")})` : ""} · ${pair.deals || 0} `
    + `deal${pair.deals === 1 ? "" : "s"} at ${num(pair.deal_value_mean)} (control ${c.deals || 0} at `
    + `${num(c.deal_value_mean)})`;
}

/** Where a hybrid job's control game stands, in words (its `control`: `status`, `error`). */
export function controlText(control) {
  if (!control) return null;
  const words = { queued: "control game queued", running: "control game playing", done: "control game played",
    failed: "control game failed", missing: "control game not asked for yet" };
  const text = words[control.status] || `control game ${control.status}`;
  return control.error ? `${text}: ${control.error}` : text;
}

/** Why a finished hybrid job has no pair line when its own record says so (its result's `diplomacy.incomplete`: the
 *  record stopped on an error, and a pair would not compare like with like); null otherwise. */
export function recordText(job) {
  const d = ((job && job.result) || {}).diplomacy;
  return d && d.incomplete ? `no pair line: its diplomacy record stopped (${d.incomplete})` : null;
}

/** Who answered a hybrid probe case (its result's `answered_by`, and `answers` round by round): "nobody" when the
 *  model's prompt did not run, or it left the chat waiting. */
export function answeredText(rec) {
  if (!rec || !rec.answered_by) return null;
  const words = { bot: "its bot", model: "its model", none: "nobody" };
  const rounds = (rec.answers || []).length > 1 ? ` (then ${rec.answers.slice(1).map((a) => words[a] || a).join(", ")})` : "";
  return `answered by ${words[rec.answered_by] || rec.answered_by}${rounds}`;
}

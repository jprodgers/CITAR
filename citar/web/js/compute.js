// The compute ledger as the pages show it (design/phase3-features.md P3.4.4): credits, seconds and the ratio of
// what an account's machines gave other members to what its work used of theirs and the site's. Pure: the Account
// page's Compute card, the profile's ratio line and the admin users page's column format with it, and
// citar/web/tests/compute.test.mjs checks it.

export const RANGES = [["24h", "Last 24 hours"], ["7d", "Last 7 days"], ["30d", "Last 30 days"], ["all", "All time"]];

const KIND_LABEL = { game: "Games", benchmark: "Benchmarks", probe: "Probes", report: "Reports", lab: "Lab", test: "Machine tests" };

// Credits, to the precision their size deserves: "0", "0.042", "3.5", "1,234".
export function credits(n) {
  const v = Number(n) || 0;
  if (v === 0) return "0";
  const abs = Math.abs(v);
  const digits = abs >= 100 ? 0 : abs >= 10 ? 1 : abs >= 1 ? 2 : 3;
  return v.toLocaleString("en-US", { minimumFractionDigits: 0, maximumFractionDigits: digits });
}

// Seconds of work as a duration: "0 s", "42 s", "3 min 5 s", "2 h 10 min".
export function duration(s) {
  const v = Math.max(0, Number(s) || 0);
  if (v < 60) return `${v < 10 && v > 0 ? v.toFixed(1).replace(/\.0$/, "") : Math.round(v)} s`;
  const m = Math.floor(v / 60);
  if (m < 60) return `${m} min${v % 60 >= 1 ? ` ${Math.floor(v % 60)} s` : ""}`;
  const h = Math.floor(m / 60);
  return `${h} h${m % 60 ? ` ${m % 60} min` : ""}`;
}

// Tokens: "0", "950", "12.3k", "4.1M".
export function tokens(n) {
  const v = Math.max(0, Number(n) || 0);
  if (v < 1000) return String(Math.round(v));
  if (v < 1e6) return `${(v / 1000).toFixed(v < 1e4 ? 1 : 0)}k`;
  return `${(v / 1e6).toFixed(1)}M`;
}

// A ratio as the server sends it ({value, text}): "2.50", "only given", "nothing yet".
export function ratioText(r) {
  if (!r) return "—";
  if (r.value === null || r.value === undefined) return r.text || "—";
  return Number(r.value).toFixed(2);
}

// The one line a profile shows: "Gave 12 credits, used 4.8 (ratio 2.50) in the last 30 days."
export function ratioLine(summary) {
  if (!summary) return "";
  const given = summary.given ? summary.given.credits : 0;
  const used = summary.used ? summary.used.credits : 0;
  const span = (RANGES.find(([k]) => k === summary.range) || [null, "the period"])[1].toLowerCase();
  const r = summary.ratio || {};
  const tail = r.value === null || r.value === undefined ? (r.text ? ` (${r.text})` : "") : ` (ratio ${ratioText(r)})`;
  return `Gave ${credits(given)} credits, used ${credits(used)}${tail} — ${span}.`;
}

// The admin users page's column: "12 given · 4.8 used · 2.50".
export function adminCell(c) {
  if (!c) return "—";
  return `${credits(c.given)} given · ${credits(c.used)} used · ${ratioText(c.ratio)}`;
}

// A side's figures as table cells (credits, GPU time, CPU time, tokens in/out).
export function sideCells(side) {
  const s = side || {};
  return [credits(s.credits), duration(s.gpu_s), duration(s.cpu_s), `${tokens(s.tokens_in)} / ${tokens(s.tokens_out)}`];
}

export function kindLabel(kind) {
  return KIND_LABEL[kind] || kind;
}

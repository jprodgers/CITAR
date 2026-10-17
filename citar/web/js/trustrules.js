// The words for machines' trust and the verification of their CPU work (design/phase4-helper.md P4.5.7, P4.5.8,
// P4.5.11), apart from the pages so Node can test them (nothing here touches the DOM or the network): a tier, a
// machine's standing, a mismatch's evidence, its measured speed, the flags the credits' bounds and the class check put
// on it, the verification settings with their bounds, and an experiment's games by verification. Used by the Servers
// page's cards and dialog, the Lab page and #/admin/verification.

/** Each tier in a few words. */
export const TIER_TEXT = {
  new: "new: every result it gives is checked by a replica",
  provisional: "provisional: a share of its results is checked",
  trusted: "trusted",
  quarantined: "quarantined: it plays no CPU job and serves nobody but its owner",
};

/** A machine's trust in a line, as its card shows it to its owner: its tier, verified count and streak (and, for a
 *  quarantined one, its mismatches). `t` is the card's `trust`. */
export function trustLine(t) {
  if (!t) return "";
  const tier = t.tier || "new";
  const parts = [`${tier}`];
  if (t.verified || t.streak || tier !== "new") {
    parts.push(`${t.verified || 0} verified`, `streak ${t.streak || 0}`);
  }
  if (t.mismatches) parts.push(`${t.mismatches} mismatch${t.mismatches === 1 ? "" : "es"}`);
  return `Trust: ${parts.join(" · ")}`;
}

/** A mismatch's (or an administrator's quarantine's) evidence in a sentence: its words (the round where two chains
 *  parted is in them), what it was checked against, and how many results were withdrawn with it. */
export function evidenceText(ev) {
  if (!ev) return "";
  const words = ev.words || ev.verdict || "quarantined";
  const against = ev.against ? ` (checked against ${ev.against})` : "";
  const n = ev.withdrawn != null ? ev.withdrawn : (ev.withdrawn_jobs || []).length;
  const withdrawn = n ? `; ${n} result${n === 1 ? "" : "s"} withdrawn and played again` : "";
  const when = ev.at ? ` on ${String(ev.at).replace("T", " ").slice(0, 16)} UTC` : "";
  return `${words}${against}${withdrawn}${when}.`;
}

/** A machine's measured speed (`trust.speed`: the reference's CPU seconds over its own on its replicated jobs) beside
 *  its CPU class, for administrators. */
export function speedText(speed, cls) {
  const klass = cls ? ` · CPU class ${cls.name}${cls.pinned ? " (pinned)" : ""}` : "";
  if (speed == null) return `Measured speed: not yet (no replicated job long enough to time)${klass}`;
  return `Measured speed: ${Number(speed).toFixed(2)}× the reference${klass}`;
}

/** What each flag on a machine's row says (P3.3.4, P4.5.7), for administrators: [the pill's class, its text, its
 *  title]. Unknown flags give null. */
export function flagText(key, f) {
  const n = f && f.count ? ` (${f.count}×)` : "";
  if (key === "overreported_cpu") {
    return ["bad", `over-reports its CPU${n}`, "Its helper reported more CPU seconds than the server observed of "
      + "the run (by over 5% and a second): no honest helper can, as a game is one thread. Its runs are credited the "
      + "observed bound, and it is not promoted to trusted while this is recent."];
  }
  if (key === "capped_cpu") {
    return ["warn", `CPU capped at 1.5× the reference${n}`, "Some of its runs reported more than 1.5 times the "
      + "reference machine's CPU for the same game and were credited that much: a slow machine, a held result or a "
      + "game too short to time. Informational: it does not block its promotion."];
  }
  if (key === "class_suspect") {
    const s = f || {};
    return ["warn", "class suspect", `Its measured speed (${s.speed}) is under half the median of the other machines `
      + `of its CPU class${s.class_name ? ` (${s.class_name})` : ""}, ${s.class_median}: pin it to a class that prices `
      + "it fairly (Machine settings…, Class and trust)."];
  }
  if (key === "overreports") {
    return ["warn", `over-reports its time${n}`, "Its helper reported more generation time than the server observed, "
      + "by over 10% and a second; its work is credited the observed time."];
  }
  return null;
}

/** The verification settings (`settings_kv.verification`) as #/admin/verification edits them: [key, label, kind,
 *  what it does]. `rate` and `share` are fractions, `count` a whole number. */
export const SETTINGS = [
  ["new_streak", "Matches to leave new", "count", "A new machine whose results this many replicas in a row reproduced "
    + "becomes provisional."],
  ["provisional_rate", "Provisional sample", "rate", "The share of a provisional machine's results a replica checks."],
  ["trusted_rate", "Trusted sample", "rate", "The share of a trusted machine's results a replica checks."],
  ["promote_verified", "Matches to be trusted", "count", "A provisional machine with this many verified matches, no "
    + "mismatch and no recent over-reporting becomes trusted."],
  ["overreport_days", "Over-reporting holds back (days)", "count", "How long over-reporting keeps a machine from "
    + "being trusted."],
  ["new_per_owner", "New machines an owner runs at once", "count", "A further new machine of one owner waits while "
    + "this many have work in hand."],
  ["new_verify_share", "This server's share for new machines", "share", "At most this share of this server's runs "
    + "replay new machines' results while other work waits."],
];

/** A setting's value from its input, or null when it is out of its `bounds` ([lo, hi]) or not of its kind: a count
 *  a whole number, a rate or a share any number. */
export function settingValue(kind, text, bounds) {
  if (text === "" || text == null) return null;
  const n = Number(text);
  if (!Number.isFinite(n)) return null;
  if (kind === "count" && !Number.isInteger(n)) return null;
  const [lo, hi] = bounds || [0, 1];
  return n >= lo && n <= hi ? n : null;
}

/** The settings route's body for what changed: only the values that differ from `current`, or null. */
export function settingsChanges(current, edited) {
  const body = {};
  for (const [key] of SETTINGS) {
    if (edited[key] != null && edited[key] !== current[key]) body[key] = edited[key];
  }
  return Object.keys(body).length ? body : null;
}

/** An experiment's accepted games by verification, and those withdrawn and played again, in a line (the Lab page):
 *  "3 verified · 2 unverified · 4 reference · 1 withdrawn"; "" when it has none. */
export function verificationCounts(e) {
  const v = (e && e.verification) || {};
  const parts = [["verified", v.verified], ["unverified", v.unverified], ["reference", v.reference]]
    .filter(([, n]) => n).map(([k, n]) => `${n} ${k}`);
  if (e && e.withdrawn) parts.push(`${e.withdrawn} withdrawn`);
  return parts.join(" · ");
}

/** Why an experiment's games wait, when that is a verifier (P4.5.11's "waiting for a verifier"): its waiting words
 *  name it. */
export function waitsForVerifier(waiting) {
  return /waiting for a verifier/.test(String(waiting || ""));
}

/** The "unverified machine" badge's title (P4.5.8 rule 11). */
export const UNVERIFIED_MACHINE = "Played on a machine no administrator has trusted yet: left out of the global boards "
  + "until one is (its earlier results then count too).";

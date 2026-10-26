// The Lab and Queue pages' words for the CPU jobs (design/phase4-helper.md P4.5.11), apart from the pages so Node can
// test them (nothing here touches the DOM or the network): how an executor is described, a job's state and its
// verification badge, and a playing game's line.

/** A job's state as its pill shows it: [the pill's class, its text]. ``done`` is a job whose result is in, whatever
 *  verification follows (P4.5.8 rule 2); administrators see ``accepted`` and ``settling`` themselves. */
export const JOB_STATE = {
  queued: ["", "waiting"], running: ["live", "● playing"], done: ["good", "✓ done"], accepted: ["good", "✓ accepted"],
  settling: ["warn", "settling"], crashed: ["bad", "crashed (final)"], failed: ["bad", "failed"],
  cancelled: ["muted", "cancelled"], withdrawn: ["warn", "withdrawn"],
};

/** A finished game's verification badge: [the pill's class, its text, its title]. */
export function verificationBadge(v) {
  if (v === "reference") return ["good", "reference", "Played by this server: its results are the reference."];
  if (v === "verified") return ["good", "verified", "A verifier played it again and got the same result."];
  if (v === "unverified") return ["warn", "unverified", "Accepted without a replica."];
  return null;
}

/** An executor's state in words, for the Lab page's header and the Queue page's CPU card. */
export function executorText(e) {
  if (!e) return "";
  const who = e.kind === "local" ? "this server" : (e.name || "a machine");
  if (e.kind !== "local" && e.online === false) return `${who}: its helper is not connected`;
  if (e.on === false) return `${who}: CPU work is off`;
  const figures = [`${e.threads} thread${e.threads === 1 ? "" : "s"}`];
  if (e.cpu_percent != null) figures.push(`${e.cpu_percent}% of the CPU`);
  if (e.memory_mb != null) figures.push(`${e.memory_mb} MB`);
  const held = e.held ? ` (CPU ${e.held.cpu}, memory ${e.held.memory})` : "";
  const state = {
    ready: "ready", idle: "idle (starts when there is work)", starting: "starting",
    draining: "finishing its games", failed: "not running", off: "off",
  }[e.state] || e.state || "";
  const why = e.available || e.reason;
  return `${who}: ${state}, ${figures.join(", ")}${held}${why && e.state !== "idle" ? `; ${why}` : ""}`;
}

/** A playing game as the Queue page's CPU card lists it: its name (or "someone's lab game") and its round. */
export function gameLine(g) {
  const round = g.turn != null ? `round ${g.turn}${g.limit ? `/${g.limit}` : ""}` : "starting";
  return `${g.label} (${round}${g.paused ? ", paused" : ""})`;
}

/** How many games of an experiment have results (done or crashed), out of its games. */
export function progressText(e) {
  return `${e.done} / ${e.games}${e.running ? ` (+${e.running} playing)` : ""}`;
}

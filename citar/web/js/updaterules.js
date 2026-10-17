// The words for a helper machine's updates (design/phase4-helper.md P4.3.8, P4.5.9, P4.5.11), apart from the pages so
// Node can test them (nothing here touches the DOM or the network): the latest signed CITAR helper this server
// relays, whether the machine's helper is older, and where its helper is with an update (its last update_status).
// Used by the Servers page's card and the Helper dialog.

/** Each `update_status` state in a few words, and the class its line takes. */
export const STATE_TEXT = {
  available: ["warn", "update available"],
  downloading: ["muted", "downloading the update"],
  verified: ["muted", "update downloaded and checked: installed when the helper is idle"],
  restarting: ["muted", "restarting into the update"],
  applied: ["good", "update applied"],
  failed: ["bad", "update failed"],
  refused: ["muted", "update refused"],
  unavailable: ["muted", "this helper does not update itself"],
};

/** A helper's update in a line, and its class: `u` is the card's (or the dialog's) `update`: {latest: {version} or
 *  null, available, status: {state, version, message} or null}; `version` is the machine's helper's. Null when there
 *  is nothing to say (no helper has spoken, no manifest relayed). */
export function updateLine(u, version) {
  if (!u) return null;
  const latest = u.latest && u.latest.version;
  const st = u.status && STATE_TEXT[u.status.state] ? u.status : null;
  const said = st ? `${STATE_TEXT[st.state][1]}${st.version ? ` (${st.version})` : ""}` : "";
  const why = st && st.message && ["failed", "refused", "available"].includes(st.state) ? `: ${st.message}` : "";
  if (u.available && latest) {
    // newer than the helper: its own word when it has one about this version, else that it is there
    const own = st && st.version === latest && st.state !== "refused" ? `${said}${why}` : "update available";
    return { cls: st && st.state === "failed" ? "bad" : "warn",
             text: `Helper ${version || "?"} · CITAR helper ${latest} is signed and relayed: ${own}` };
  }
  if (latest && version && latest === version) {
    const applied = st && st.state === "applied" && st.version === version ? " (updated)" : "";
    return { cls: "good", text: `Helper ${version} · up to date${applied}` };
  }
  if (st && st.state !== "refused") return { cls: STATE_TEXT[st.state][0], text: `Helper ${version || "?"} · ${said}${why}` };
  if (latest) return { cls: "muted", text: `Helper ${version || "?"} · the latest signed helper here is ${latest}` };
  return null;
}

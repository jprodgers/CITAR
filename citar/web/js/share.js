// The sharing dialog (design/phase3-features.md P3.2.4): who can see a thing, its share link, the people it is shared
// with, and a moderator's "Unpublish". One dialog for every shareable thing: games through the game routes
// (/api/games/<gid>/sharing...), everything else through the item routes (/api/items/<kind>/<ref>/sharing...).
// Only someone who manages a thing sees the dialog; a moderator who does not gets only the Unpublish button, for
// a thing published to the internet or a link they were given. The server decides every one of these: the
// buttons only follow its can_manage and can_moderate.
import { api } from "./api.js";
import { el, clear, toast, modal } from "./util.js";
import * as auth from "./auth.js";

// The visibility ladder (P3.2.3), widest last.
export const LEVELS = [
  ["private", "Private", "Only you, and the people you add below."],
  ["allowlist", "People you choose", "The people you add below, and nobody else."],
  ["link", "Anyone with the link", "Not listed anywhere; whoever holds the link can see it, signed in or not."],
  ["server", "Members of this server", "Every signed-in member, with no link needed."],
  ["global", "Everyone on the internet", "Anyone, with no account."],
];

// What each kind is called in the dialog.
const NOUNS = { game: "game", report: "report", benchmark_run: "benchmark run", benchmark_suite: "benchmark suite",
  probe_run: "probe run", probe: "probe", scenario: "scenario", map: "map", bot_profile: "bot profile",
  lab_experiment: "lab experiment" };

// The routes for one thing: a game's own, or an item's.
function routesFor(kind, ref) {
  if (kind === "game") {
    return {
      get: () => api.gameSharing(ref),
      setVisibility: (visibility) => api.setGameSharing(ref, { visibility }),
      setPooling: (data_sharing) => api.setGameSharing(ref, { data_sharing }),
      rotate: () => api.setGameSharing(ref, { rotate_slug: true }),
      share: (handle, permission) => api.shareGameWith(ref, handle, permission),
      unshare: (handle) => api.unshareGameWith(ref, handle),
      unpublish: (reason) => api.unpublishGame(ref, reason),
    };
  }
  return {
    get: () => api.itemSharing(kind, ref),
    setVisibility: (visibility) => api.setItemVisibility(kind, ref, visibility),
    setPooling: null,
    rotate: () => api.rotateItemLink(kind, ref),
    share: (handle, permission) => api.shareItemWith(kind, ref, handle, permission),
    unshare: (handle) => api.unshareItemWith(kind, ref, handle),
    unpublish: (reason) => api.unpublishItem(kind, ref, reason),
  };
}

// The button a listing shows next to a thing: "Share" for whoever manages it, "Unpublish" for a moderator who does
// not, nothing for anyone else. `perms` is the listing's {can_manage, can_moderate}; `onChange` refreshes the page.
export function shareButton(kind, ref, name, perms, onChange) {
  perms = perms || {};
  const stop = (fn) => (e) => { if (e) e.stopPropagation(); fn(); };
  if (perms.can_manage) {
    return el("button", { class: "small", title: `Who can see this ${NOUNS[kind] || "item"}`,
      onclick: stop(() => openSharing(kind, ref, name, onChange)) }, "Share");
  }
  if (perms.can_moderate) {
    return el("button", { class: "small danger", title: "Take it down: private, with a new link; its owner sees why",
      onclick: stop(() => openUnpublish(kind, ref, name, onChange)) }, "Unpublish");
  }
  return null;
}

// Copy text to the clipboard, saying whether it worked.
async function copy(text) {
  try { await navigator.clipboard.writeText(text); toast("Link copied."); }
  catch (e) { toast("Copy it from the box: the browser would not let the page copy it.", "error"); }
}

// The sharing dialog for whoever manages a thing.
export async function openSharing(kind, ref, name, onChange) {
  const routes = routesFor(kind, ref);
  const body = el("div", { class: "col share-dialog" });
  const noun = NOUNS[kind] || "item";
  const m = modal({ title: `Share ${noun}: ${name || ref}`, content: body });
  let state = null;
  const changed = () => { if (onChange) { try { onChange(); } catch (e) { /* the page refreshes itself */ } } };

  const errorLine = el("p", { class: "bad small", style: { display: "none" } });
  const fail = (e) => { errorLine.textContent = e.message; errorLine.style.display = ""; };
  // Run one change, then redraw from what the server answers (a change can be refused).
  const act = async (fn, done) => {
    errorLine.style.display = "none";
    try {
      await fn();
      state = await routes.get();
      draw();
      if (done) toast(done);
      changed();
    } catch (e) { fail(e); }
  };

  function draw() {
    clear(body);
    const s = state;
    const level = s.visibility || "private";
    const mayGlobal = auth.can("publish_global") || auth.isAdmin();
    const select = el("select", { "aria-label": "Who can see it", onchange: (e) => {
      const v = e.target.value;
      act(() => routes.setVisibility(v), "Sharing changed.");
    } }, ...LEVELS.map(([v, label]) => el("option", { value: v, selected: v === level,
      disabled: v === "global" && !mayGlobal && level !== "global" }, label)));
    const hint = (LEVELS.find(([v]) => v === level) || [])[2] || "";
    body.append(el("div", { class: "field" }, el("label", {}, "Who can see it"), select,
      el("span", { class: "muted small" }, hint),
      !mayGlobal ? el("span", { class: "muted small" }, "Publishing to the whole internet needs an administrator's permission.") : null));

    if (s.unpublished) {
      const when = s.unpublished.at ? new Date(s.unpublished.at).toLocaleString() : "";
      body.append(el("p", { class: "warn small" }, `Taken down by ${s.unpublished.by}${when ? " on " + when : ""}` +
        (s.unpublished.reason ? `: “${s.unpublished.reason}”` : ".")));
    }

    if (s.share_url) {
      const box = el("input", { value: s.share_url, readOnly: true, class: "grow", "aria-label": "Share link",
        onfocus: (e) => e.target.select() });
      body.append(el("div", { class: "field" }, el("label", {}, level === "link" ? "Share link (the key is in it)" : "Link"),
        el("div", { class: "row" }, box,
          el("button", { class: "small", onclick: () => copy(s.share_url) }, "Copy"),
          level === "link" ? el("button", { class: "small", title: "Every copy of the old link stops working",
            onclick: () => act(routes.rotate, "A new link: the old one no longer works.") }, "New link") : null)));
    }
    if (s.public_url) {
      body.append(el("div", { class: "field" }, el("label", {}, "Page for people with no account"),
        el("div", { class: "row" }, el("input", { value: s.public_url, readOnly: true, class: "grow", "aria-label": "Public page",
          onfocus: (e) => e.target.select() }), el("button", { class: "small", onclick: () => copy(s.public_url) }, "Copy"))));
    }

    if (routes.setPooling && s.data_sharing) {
      body.append(el("label", { class: "row" }, el("input", { type: "checkbox", checked: s.data_sharing === "pool",
        onchange: (e) => act(() => routes.setPooling(e.target.checked ? "pool" : "private"), "Pooling changed.") }),
        el("span", {}, "Count this game's results in the pooled model averages"),
        el("span", { class: "muted small" }, "(your account's own choice on the Account page comes first)")));
    }

    const people = s.shared_with || [];
    const list = el("div", { class: "col" });
    if (!people.length) list.append(el("p", { class: "muted small" }, "Not shared with anyone by name."));
    for (const p of people) {
      const handle = p.user.handle;
      list.append(el("div", { class: "row share-person" },
        el("b", {}, p.user.display_name && p.user.display_name !== handle ? `${p.user.display_name} (@${handle})` : `@${handle}`),
        el("span", { class: "muted small" }, (p.permissions || []).join(", ")),
        el("span", { class: "grow" }),
        el("button", { class: "small", title: `Stop sharing with @${handle}`,
          onclick: () => act(() => routes.unshare(handle), `No longer shared with @${handle}.`) }, "Remove")));
    }
    const handleIn = el("input", { placeholder: "their username", "aria-label": "Username to share with", class: "grow" });
    const perms = kind === "game" ? [["view", "can watch"], ["play", "can play"], ["manage", "can manage"]]
      : [["view", "can see"], ["manage", "can manage"]];
    const permSel = el("select", { "aria-label": "What they may do" }, ...perms.map(([v, t]) => el("option", { value: v }, t)));
    const add = () => {
      const h = handleIn.value.trim().replace(/^@/, "");
      if (!h) return;
      act(() => routes.share(h, permSel.value), `Shared with @${h}.`);
    };
    handleIn.addEventListener("keydown", (e) => { if (e.key === "Enter") add(); });
    body.append(el("div", { class: "field" }, el("label", {}, "People with access"), list,
      el("div", { class: "row" }, handleIn, permSel, el("button", { class: "small primary", onclick: add }, "Add"))));
    body.append(errorLine);
  }

  try { state = await routes.get(); draw(); }
  catch (e) { m.close(); toast(e.message, "error"); }
  return m;
}

// A moderator's take-down: the thing becomes private and gets a new link; the reason is shown to its owner.
export function openUnpublish(kind, ref, name, onChange) {
  const routes = routesFor(kind, ref);
  const reason = el("textarea", { rows: 3, placeholder: "Why it is being taken down (its owner sees this)",
    "aria-label": "Reason", style: { width: "100%" } });
  const err = el("p", { class: "bad small" });
  let m;
  const go = async () => {
    try {
      await routes.unpublish(reason.value.trim());
      m.close();
      toast("Unpublished: it is private now, and its old links no longer work.");
      if (onChange) onChange();
    } catch (e) { err.textContent = e.message; }
  };
  m = modal({ title: `Unpublish: ${name || ref}`, narrow: true,
    content: el("div", { class: "col" }, el("p", {}, `This makes the ${NOUNS[kind] || "item"} private and gives it a new link, ` +
      "so every copy of the old one stops working. Nothing else of it changes."), reason, err),
    footer: [el("button", { onclick: () => m.close() }, "Cancel"), el("button", { class: "danger", onclick: go }, "Unpublish")] });
  return m;
}

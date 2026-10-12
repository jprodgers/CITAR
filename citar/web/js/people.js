// People (design/phase3-features.md P3.2.6, P3.2.10): the members' directory (#/users) and a profile (#/u/<handle>).
//
// What a page shows is what the server sends: each account decides, field by field, who sees its display name,
// picture, bio, links and the things it owns (only itself, members, or everyone), and the server leaves out whatever
// the viewer may not see. A profile opens without an account when its owner made it public; one the viewer may not
// see is the router's "not found" page, the same as one that does not exist.
import { api } from "./api.js";
import { el, clear, toast } from "./util.js";
import { pageHeader } from "./nav.js";
import * as auth from "./auth.js";
import * as compute from "./compute.js";

// What each kind of thing is called on a profile.
const KIND_LABEL = { game: "Games", report: "Reports", benchmark_run: "Benchmark runs", benchmark_suite: "Benchmark suites",
  probe_run: "Probe runs", probe: "Probes", scenario: "Scenarios", map: "Maps", bot_profile: "Bot profiles",
  lab_experiment: "Lab experiments" };

// How a profile field is named to its owner.
const FIELD_LABEL = { profile: "profile page", display_name: "display name", avatar: "picture", bio: "about", links: "links",
  items: "shared things", ratio: "compute ratio" };

// An account's picture, or its initial on a coloured disc.
export function avatarOf(user, size = 40) {
  const style = { width: `${size}px`, height: `${size}px`, fontSize: `${Math.round(size * 0.45)}px` };
  if (user && user.avatar) return el("img", { class: "avatar", src: user.avatar, alt: "", style, referrerPolicy: "no-referrer" });
  const name = (user && (user.display_name || user.handle)) || "?";
  return el("span", { class: "avatar letter", style }, name[0].toUpperCase());
}

// "Display Name @handle", or "@handle" alone when the name is hidden (the server sends the handle in its place).
export function nameLine(user) {
  const shown = user.display_name && user.display_name !== user.handle;
  return el("span", { class: "person-name" }, shown ? el("b", {}, user.display_name) : null,
    el("span", { class: shown ? "muted" : "" }, `${shown ? " " : ""}@${user.handle}`));
}

// The members' directory.
export async function renderPeople(root) {
  const page = el("div", { class: "lobby" });
  root.appendChild(page);
  page.appendChild(pageHeader("people"));
  const me = auth.user();
  const search = el("input", { type: "search", placeholder: "Search by username or name", "aria-label": "Search people",
    class: "grow" });
  const grid = el("div", { class: "people-grid" });
  const more = el("button", { style: { display: "none" } }, "More");
  page.appendChild(el("section", { class: "card" },
    el("div", { class: "row between wrap" }, el("h2", {}, "People"),
      me ? el("a", { href: `#/u/${encodeURIComponent(me.handle)}` }, "Your profile") : null),
    el("p", { class: "muted" }, "Members of this server who list themselves here. Each shows what they choose to; ",
      el("a", { href: "#/account" }, "your Account page"), " says what others see of you."),
    el("div", { class: "row gap" }, search), grid, el("div", { class: "row center" }, more)));

  let offset = 0;
  let query = "";
  let seq = 0;
  async function load(reset) {
    const mine = ++seq;
    if (reset) offset = 0;
    let page;
    try { page = await api.users(query, offset); }
    catch (e) { toast(e.message, "error"); return; }
    if (mine !== seq) return;                       // a newer search answered first
    if (reset) clear(grid);
    for (const u of page.users) grid.appendChild(personCard(u));
    if (reset && !page.users.length) grid.appendChild(el("p", { class: "muted" }, query ? "Nobody matches." : "Nobody is listed yet."));
    offset = page.next || 0;
    more.style.display = page.next ? "" : "none";
  }
  let timer = null;
  search.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(() => { query = search.value.trim(); load(true); }, 250);
  });
  more.onclick = () => load(false);
  await load(true);
  return { destroy() { clearTimeout(timer); } };
}

function personCard(u) {
  const body = [avatarOf(u, 44), el("div", { class: "col person-text" }, nameLine(u),
    u.bio ? el("span", { class: "muted small person-bio" }, u.bio)
      : (!u.profile ? el("span", { class: "muted small" }, "Profile not shown") : null))];
  return u.profile
    ? el("a", { class: "person-card", href: `#/u/${encodeURIComponent(u.handle)}` }, ...body)
    : el("div", { class: "person-card" }, ...body);
}

// A profile page, as the viewer may see it (or, from the account's own "view as" links, as a member or a visitor would).
export async function renderProfile(root, handle) {
  const as = new URLSearchParams(location.hash.split("?")[1] || "").get("as") || "";
  const profile = await api.user(handle, as);          // a 404 reaches the router: "not found"
  const outer = el("div", { class: "lobby" });
  root.appendChild(outer);
  if (auth.user()) outer.appendChild(pageHeader("people"));
  else outer.appendChild(el("header", { class: "page-header" }, el("h1", {}, el("a", { href: "#/", class: "plain" }, "CITAR")),
    el("span", { class: "grow" }), el("a", { class: "btn small", href: `#/login?next=${encodeURIComponent(location.hash)}` }, "Sign in")));
  const page = el("div", { class: "profile-page" });
  outer.appendChild(page);

  const self = `#/u/${encodeURIComponent(profile.handle)}`;
  if (profile.seen_as === "member" || profile.seen_as === "visitor") {
    const mine = auth.user() && auth.user().handle === profile.handle;
    if (mine) {
      page.appendChild(el("div", { class: "card notice" },
        `This is your profile as ${profile.seen_as === "member" ? "a signed-in member" : "a visitor with no account"} sees it. `,
        el("a", { href: self }, "Back to your own view")));
    }
  }

  const head = el("section", { class: "card profile-head" }, avatarOf(profile, 96),
    el("div", { class: "col" },
      el("h2", { class: "profile-name" }, profile.display_name !== profile.handle ? profile.display_name : `@${profile.handle}`),
      profile.display_name !== profile.handle ? el("span", { class: "muted" }, `@${profile.handle}`) : null,
      profile.is_you && profile.seen_as === "you" ? el("div", { class: "row gap wrap" },
        el("a", { class: "btn small", href: "#/account" }, "Edit profile"),
        el("a", { href: `${self}?as=member` }, "View as a member"),
        el("a", { href: `${self}?as=visitor` }, "View as a visitor")) : null,
      profile.seen_as === "administrator" ? el("span", { class: "pill", title: "Administrators see every field" }, "admin view") : null));
  page.appendChild(head);

  if (profile.bio) page.appendChild(el("section", { class: "card" }, el("h3", {}, "About"), el("p", { class: "profile-bio" }, profile.bio)));
  if (profile.links && profile.links.length) {
    page.appendChild(el("section", { class: "card" }, el("h3", {}, "Links"),
      el("ul", { class: "profile-links" }, ...profile.links.map((link) => el("li", {},
        el("a", { href: link.url, target: "_blank", rel: "nofollow noopener noreferrer ugc" }, link.label || hostOf(link.url)),
        link.label ? el("span", { class: "muted small" }, ` ${hostOf(link.url)}`) : null)))));
  }
  if (profile.ratio) {
    page.appendChild(el("section", { class: "card" }, el("h3", {}, "Compute"),
      el("p", { class: "profile-ratio" }, compute.ratioLine(profile.ratio))));
  }
  if (profile.items) {
    const card = el("section", { class: "card" }, el("h3", {}, "Shared here"));
    if (!profile.items.length) card.appendChild(el("p", { class: "muted" }, "Nothing you can see."));
    const groups = {};
    for (const it of profile.items) (groups[it.kind] = groups[it.kind] || []).push(it);
    for (const [kind, list] of Object.entries(groups)) {
      card.appendChild(el("h4", {}, KIND_LABEL[kind] || kind));
      card.appendChild(el("ul", { class: "profile-items" }, ...list.map((it) => el("li", {},
        it.url ? el("a", { href: it.url }, it.name) : el("span", {}, it.name),
        it.status ? el("span", { class: "muted small" }, ` ${it.status}`) : null,
        profile.is_you && profile.seen_as === "you" ? el("span", { class: "pill small-pill" }, it.visibility) : null))));
    }
    page.appendChild(card);
  }
  if (profile.is_you && profile.seen_as === "you" && profile.levels) {
    const hidden = Object.entries(profile.levels).filter(([, v]) => v === "private").map(([k]) => FIELD_LABEL[k] || k);
    page.appendChild(el("p", { class: "muted small" }, hidden.length
      ? `Only you (and administrators) see your ${hidden.join(", ")}.` : "Everything here shows to someone besides you.",
      " ", el("a", { href: "#/account" }, "Change who sees what")));
  }
  return {};
}

function hostOf(url) {
  try { return new URL(url).host; } catch (e) { return url; }
}

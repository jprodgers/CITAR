// Shared page header with navigation between the pages, plus live badges (restricted servers, benchmark and lab activity).
import { api } from "./api.js";
import { el, clear } from "./util.js";
import * as auth from "./auth.js";

const PAGES = [["games", "#/", "Games"], ["benchmarks", "#/benchmarks", "Benchmarks"], ["queue", "#/queue", "Queue"], ["models", "#/models", "Models"], ["editor", "#/editor", "Map editor"], ["scenarios", "#/scenarios", "Scenarios"], ["probes", "#/probes", "Probes"], ["lab", "#/lab", "Lab"], ["pool", "#/pool", "Servers"], ["reports", "#/reports", "Reports"]];

// Shown only to administrators: everything on it is server-wide configuration.
const ADMIN_PAGES = [["console", "#/console", "Server"]];

/** The signed-in account, with a link to its settings and a way out. */
function accountMenu() {
  const me = auth.user();
  if (!me) return el("a", { class: "btn small", href: "#/login" }, "Sign in");
  return el("div", { class: "account-menu" },
    el("a", { href: "#/account", class: "account-link", title: `${me.handle} (${me.role})` },
      me.avatar_url ? el("img", { class: "avatar", src: me.avatar_url, alt: "" })
                    : el("span", { class: "avatar letter" }, (me.display_name || me.handle)[0].toUpperCase()),
      el("span", { class: "account-name" }, me.display_name || me.handle)),
    auth.session() && auth.session().local_mode
      ? el("span", { class: "pill", title: "This server is in local mode: it signs you in automatically." }, "local")
      : el("button", { class: "link small", onclick: () => auth.logout() }, "Sign out"));
}

export function pageHeader(active) {
  const badge = el("a", { class: "bench-badge", href: "#/benchmarks" });
  const header = el("header", { class: "page-header" },
    el("h1", {}, "CITAR"),
    el("nav", { class: "page-nav" },
      ...PAGES.map(([key, href, label]) => el("a", { href, class: key === active ? "active" : "" }, label)),
      ...(auth.user() && auth.user().role === "admin"
        ? ADMIN_PAGES.map(([key, href, label]) => el("a", { href, class: key === active ? "active" : "" }, label))
        : [])),
    el("span", { class: "muted grow tagline" }, "Civ Inspired Tool for AI Research"),
    badge,
    // someone on a phone who switched to the full site needs a way back to the phone one
    /iPhone|iPod|Android.*Mobile|Windows Phone/i.test(navigator.userAgent) || matchMedia("(max-width: 720px)").matches
      ? el("a", { class: "pill", href: "/?site=mobile", title: "The phone-sized check-in site" }, "📱 Mobile site") : null,
    accountMenu());
  const update = async () => {
    if (!document.body.contains(header)) { clearInterval(timer); return; }
    try {
      const s = await api.benchStatus();
      clear(badge);
      for (const r of s.restricted || []) badge.append(el("a", { class: "pill quiet", href: "#/servers", title: "Restricted hours: queued work on this server is paused" }, `🌙 ${r.name} until ${r.until}`));
      if (s.active_jobs) badge.append(el("span", { class: "pill live" }, `● ${s.active_jobs} benchmark game${s.active_jobs > 1 ? "s" : ""} running`));
      else if (s.queued_jobs) badge.append(el("span", { class: "pill" }, `${s.queued_jobs} queued`));
      const lab = await api.lab();
      if (lab.runner.alive && lab.running.length) badge.append(el("a", { class: "pill live", href: "#/lab" }, `lab: ${lab.running.length} games running`));
    } catch (e) { /* server restarting */ }
  };
  const timer = setInterval(update, 5000);
  update();
  return header;
}

export function secs(s) {
  if (s == null) return "–";
  if (s < 60) return `${Math.round(s * 10) / 10}s`;
  if (s < 3600) { const m = Math.floor(s / 60); return `${m}m ${Math.round(s - m * 60)}s`; }
  if (s < 86400) { const h = Math.floor(s / 3600); return `${h}h ${Math.round((s - h * 3600) / 60)}m`; }
  const d = Math.floor(s / 86400); return `${d}d ${Math.round((s - d * 86400) / 3600)}h`;
}

export function bar(fraction, label, cls = "") {
  const pct = Math.max(0, Math.min(1, fraction || 0)) * 100;
  return el("div", { class: `bar ${cls}`, title: label || "" }, el("div", { class: "fill", style: { width: `${pct}%` } }),
    label ? el("span", { class: "label" }, label) : null);
}

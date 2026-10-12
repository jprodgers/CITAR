import { api, shareKey } from "./api.js";
import { el, clear, toast } from "./util.js";
import { renderLobby } from "./lobby.js";
import { GameScreen } from "./game.js";
import { ReplayScreen } from "./replay.js";
import { renderBenchmarks } from "./benchmarks.js";
import { renderModels } from "./models.js";
import { renderLab } from "./lab.js";
import { renderBots } from "./bots.js";
import { renderEditor } from "./editor.js";
import { renderScenarios, renderScenarioEditor } from "./scenario.js";
import { renderProbes } from "./probes.js";
import { renderServers } from "./servers.js";
import { renderReports } from "./reports.js";
import { renderPool } from "./pool.js";
import { renderAccount } from "./account.js";
import { renderSetup } from "./setup.js";
import { renderConsole } from "./console.js";
import { renderLanding } from "./landing.js";
import { renderQueue } from "./queue.js";
import { renderPeople, renderProfile } from "./people.js";
import { renderAdminUsers } from "./admin_users.js";
import * as auth from "./auth.js";

let rules = null;
let screen = null;
let booted = false;

// Pages reachable without an account. Everything else redirects to sign-in.
// A game or report reached by a share link is NOT listed here: those routes do their own check,
// because the link itself is the credential and the server decides whether it is good.
const PUBLIC_ROUTES = ["login", "signup", "forgot", "reset"];

// Pages of one thing that open without a session too: a game or a report published to the internet, or shared by
// link (its key in the address), and a profile its owner made public (P3.2.6). The server decides by the
// visibility ladder (P3.2.3) and the profile's fields; what it will not show is the "not found" page, which offers
// a visitor the sign-in.
const OPEN_ROUTES = ["game", "replay", "reports", "u"];

function signedIn() { return !!(auth.session() && auth.session().authenticated); }

// The router's "not found" page: for something that does not exist and for something the viewer may not see, alike
// (the server answers 404 for both, so an address never says which).
function notFound(root, text) {
  clear(root);
  const next = encodeURIComponent(location.hash || "#/");
  root.appendChild(el("div", { class: "lobby" }, el("div", { class: "card not-found" },
    el("h2", {}, "Not found"),
    el("p", {}, text || "There is nothing here, or it has not been shared with you."),
    signedIn() ? el("a", { href: "#/" }, "← Back to the games")
      : el("p", {}, "If it was shared with you, ", el("a", { href: `#/login?next=${next}` }, "sign in"), " to see it."))));
  return { notFound: true };
}

// Before a game screen with no token: the game as the server lets this viewer see it. A game they may not see is
// "not found"; one with a person playing in it cannot be watched until it is over (its seats see it, nobody else).
async function gameGate(root, gid, what) {
  let info;
  try { info = await api.game(gid); }
  catch (e) {
    if (e.status === 404) { notFound(root); return null; }
    throw e;
  }
  if (info.phase === "playing" && !info.god_view_allowed) {
    clear(root).appendChild(el("div", { class: "lobby" }, el("div", { class: "card" },
      el("h2", {}, info.name), el("p", { class: "muted" }, `Turn ${info.turn}`),
      el("p", {}, `A person is playing this game, so it cannot be ${what} until it is over.`),
      el("a", { href: "#/" }, "← Back to the games"))));
    return null;
  }
  return info;
}

async function route() {
  const root = document.getElementById("app");
  if (screen && screen.destroy) screen.destroy();
  screen = null;
  clear(root);
  clear(document.getElementById("modal-root"));

  if (!booted) {
    try { await auth.boot(); booted = true; }
    catch (e) { /* the sign-in screen still renders without it */ booted = true; }
  }

  // the path only: a query (`?k=<share key>`, `?next=`, a page's own filters) never ends up in an id
  const parts = location.hash.replace(/^#\/?/, "").split("?")[0].split("/");
  const page = parts[0] || "";

  // The auth screens render before anything else is fetched: an unauthenticated visitor must not
  // have to wait on /api/rules (which is public) or see errors from calls that will 401 anyway.
  if (PUBLIC_ROUTES.includes(page)) {
    if (signedIn() && page !== "reset") { location.hash = "#/"; return; }
    if (page === "login") return auth.renderLogin(root);
    if (page === "signup") return auth.renderSignup(root);
    if (page === "forgot") return auth.renderForgot(root);
    if (page === "reset") return auth.renderReset(root);
  }

  if (!signedIn()) {
    // A share link carries its own credential, so let those routes try the server first; so do the pages of one
    // game or report, which may be published to everyone.
    const hasShareKey = !!shareKey();
    // The bare root gets the front door rather than a sign-in form: somebody arriving from a link
    // has questions ("what is this, can I try it, can I run it myself") that a login box answers
    // none of. Every other route still redirects to sign-in.
    if (!hasShareKey && !page) { screen = await renderLanding(root); return; }
    if (!hasShareKey && !(OPEN_ROUTES.includes(page) && parts[1])) {
      location.hash = `#/login?next=${encodeURIComponent(location.hash || "#/")}`; return;
    }
  }

  if (!rules) {
    try { rules = await api.rules(); }
    catch (e) { root.appendChild(el("p", {}, "Could not reach the CITAR server: " + e.message)); return; }
  }
  try {
    if (page === "game" && parts[1]) {
      // `#/game/<gid>/<token>` plays or watches with a token; `#/game/<gid>` (or `/-/<player>`) watches as the
      // account or the share key allows
      const token = parts[2] && parts[2] !== "-" ? decodeURIComponent(parts[2]) : "";
      if (!token && !(await gameGate(root, parts[1], "watched"))) return;
      screen = new GameScreen(root, rules, parts[1], token, parts[3] != null && parts[3] !== "" ? parts[3] : null);
    } else if (page === "replay" && parts[1]) {
      const token = parts[2] ? decodeURIComponent(parts[2]) : "";
      if (!token && !(await gameGate(root, parts[1], "replayed"))) return;
      screen = new ReplayScreen(root, rules, parts[1], token);
    } else if (parts[0] === "benchmarks") {
      screen = await renderBenchmarks(root, rules);
    } else if (parts[0].startsWith("probes")) {
      screen = await renderProbes(root, rules);
    } else if (parts[0].startsWith("scenarios")) {
      screen = await renderScenarios(root, rules);
    } else if (parts[0] === "scenario" && parts[1]) {
      screen = await renderScenarioEditor(root, rules, parts[1]);
    } else if (parts[0] === "editor") {
      screen = await renderEditor(root, rules, parts[1] || null);
    } else if (parts[0] === "welcome") {
      screen = await renderSetup(root);
    } else if (parts[0] === "console") {
      screen = await renderConsole(root);
    } else if (parts[0] === "account") {
      screen = await renderAccount(root);
    } else if (parts[0] === "pool") {
      screen = await renderPool(root);
    } else if (parts[0] === "servers") {
      screen = await renderServers(root, parts[1] || null);
    } else if (parts[0] === "reports") {
      screen = await renderReports(root, parts[1] || null);
    } else if (parts[0] === "queue") {
      screen = await renderQueue(root);
    } else if (parts[0] === "lab") {
      screen = await renderLab(root, rules);
    } else if (page === "bots") {
      screen = await renderBots(root, rules, parts[1] || null);
    } else if (parts[0] === "models") {
      screen = await renderModels(root);
    } else if (page === "users") {
      screen = await renderPeople(root);
    } else if (page === "u" && parts[1]) {
      screen = await renderProfile(root, decodeURIComponent(parts[1]));
    } else if (page === "admin" && parts[1] === "users") {
      screen = await renderAdminUsers(root);
    } else {
      screen = await renderLobby(root, rules);
    }
  } catch (e) {
    // A session can expire mid-page; send them to sign in rather than showing a bare 401.
    if (e.name === "NotSignedIn") {
      location.hash = `#/login?next=${encodeURIComponent(location.hash || "#/")}`;
      return;
    }
    // something the viewer may not see, or that is not there: the same page for both, never a stack trace
    if (e.status === 404) { if (screen && screen.destroy) screen.destroy(); screen = notFound(root); return; }
    console.error(e);
    toast(e.message, "error");
  }
}

window.addEventListener("hashchange", route);
route();

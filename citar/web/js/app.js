import { api } from "./api.js";
import { el, clear, toast } from "./util.js";
import { renderLobby } from "./lobby.js";
import { GameScreen } from "./game.js";
import { ReplayScreen } from "./replay.js";
import { renderBenchmarks } from "./benchmarks.js";
import { renderModels } from "./models.js";
import { renderLab } from "./lab.js";
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
import * as auth from "./auth.js";

let rules = null;
let screen = null;
let booted = false;

// Pages reachable without an account. Everything else redirects to sign-in.
// A game or report reached by a share link is NOT listed here: those routes do their own check,
// because the link itself is the credential and the server decides whether it is good.
const PUBLIC_ROUTES = ["login", "signup", "forgot", "reset"];

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

  const parts = location.hash.replace(/^#\/?/, "").split("/");
  const page = (parts[0] || "").split("?")[0];

  // The auth screens render before anything else is fetched: an unauthenticated visitor must not
  // have to wait on /api/rules (which is public) or see errors from calls that will 401 anyway.
  if (PUBLIC_ROUTES.includes(page)) {
    const signedIn = auth.session() && auth.session().authenticated;
    if (signedIn && page !== "reset") { location.hash = "#/"; return; }
    if (page === "login") return auth.renderLogin(root);
    if (page === "signup") return auth.renderSignup(root);
    if (page === "forgot") return auth.renderForgot(root);
    if (page === "reset") return auth.renderReset(root);
  }

  if (!(auth.session() && auth.session().authenticated)) {
    // A share link carries its own credential, so let those routes try the server first.
    const hasShareKey = new URLSearchParams(location.hash.split("?")[1] || "").get("k");
    // The bare root gets the front door rather than a sign-in form: somebody arriving from a link
    // has questions ("what is this, can I try it, can I run it myself") that a login box answers
    // none of. Every other route still redirects to sign-in.
    if (!hasShareKey && !page) { screen = await renderLanding(root); return; }
    if (!hasShareKey) { location.hash = `#/login?next=${encodeURIComponent(location.hash || "#/")}`; return; }
  }

  if (!rules) {
    try { rules = await api.rules(); }
    catch (e) { root.appendChild(el("p", {}, "Could not reach the CITAR server: " + e.message)); return; }
  }
  try {
    if (parts[0] === "game" && parts[1] && parts[2]) {
      screen = new GameScreen(root, rules, parts[1], decodeURIComponent(parts[2]), parts[3] != null ? parts[3] : null);
    } else if (parts[0] === "replay" && parts[1]) {
      screen = new ReplayScreen(root, rules, parts[1], decodeURIComponent(parts[2] || ""));
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
      screen = await renderLab(root);
    } else if (parts[0] === "models") {
      screen = await renderModels(root);
    } else {
      screen = await renderLobby(root, rules);
    }
  } catch (e) {
    console.error(e);
    // A session can expire mid-page; send them to sign in rather than showing a bare 401.
    if (e.name === "NotSignedIn") {
      location.hash = `#/login?next=${encodeURIComponent(location.hash || "#/")}`;
      return;
    }
    toast(e.message, "error");
  }
}

window.addEventListener("hashchange", route);
route();

// First-run setup, in the browser.
//
// The job of this screen is to get somebody from "CITAR just started" to "I am in a game" without
// them reading anything first. So it opens with what it already knows about their machine rather
// than with a form: which model servers are running, what the GPU is, and which model would fit.
//
// It is offered when the registry holds no model that could take a seat, and never again once it
// has been finished or dismissed — a person who chose to play against the scripted bots has
// finished setting up, even though "is a model configured" still answers no.
import { api } from "./api.js";
import { el, clear, toast } from "./util.js";

/** Ask the server whether to offer setup. Cheap; safe to call on every page load. */
export async function setupState() {
  try {
    return await api.setupState();
  } catch (e) {
    return { needed: false };          // not permitted to set up, or the server is starting
  }
}

/**
 * A one-line invitation at the top of the lobby.
 *
 * A banner rather than a forced redirect: somebody who installed CITAR to look at it should be able
 * to look at it. The wizard is one click away and stays there until it is dealt with.
 */
export function welcomeBanner(onOpen) {
  return el("div", { class: "welcome-banner" },
    el("span", { class: "welcome-icon" }, "▶"),
    el("div", { class: "grow" },
      el("strong", {}, "No model is set up yet."),
      el("span", { class: "muted" },
        " CITAR can find the ones already running on this computer, or you can play against the built-in bots.")),
    el("button", { class: "primary", onclick: onOpen }, "Set up"),
    el("button", { class: "link", onclick: async () => { await api.setupDismiss(); location.reload(); } }, "Not now"));
}

/** The wizard itself: scan, choose, apply. */
export async function renderSetup(root) {
  clear(root);
  const page = el("div", { class: "setup-page" });
  root.appendChild(page);

  const state = { scan: null, busy: false };
  const draw = () => render(page, state);

  draw();
  try {
    state.scan = await api.setupScan();
  } catch (e) {
    state.error = e.message;
  }
  draw();
  return { destroy() {} };
}

function render(page, state) {
  clear(page);
  page.append(
    el("header", { class: "setup-head" },
      el("h1", {}, "Welcome to CITAR"),
      el("p", { class: "muted" },
        "A Civilization-style game whose players can be language models. ",
        "Let's find something for them to run on.")));

  if (state.error) {
    page.append(el("p", { class: "error" }, state.error));
    return;
  }
  if (!state.scan) {
    page.append(el("p", { class: "muted" }, "Looking at this computer…"));
    return;
  }

  page.append(machineCard(state.scan));

  const running = state.scan.endpoints.filter((e) => e.reachable && e.models.length);
  if (running.length) page.append(foundCard(running, state));
  else page.append(nothingFoundCard(state));

  page.append(
    el("section", { class: "setup-card quiet" },
      el("h2", {}, "Or start without a model"),
      el("p", { class: "muted" },
        "CITAR includes a scripted opponent that needs nothing installed. It is also what models ",
        "are scored against, so it is worth a game of your own."),
      el("button", { onclick: async () => { await api.setupDismiss(); location.hash = "#/"; } },
        "Play against the bots")));
}

function machineCard(scan) {
  return el("section", { class: "setup-card" },
    el("h2", {}, "This computer"),
    el("ul", { class: "plain" }, ...scan.hardware_lines.map((line) => el("li", {}, line))));
}

function foundCard(running, state) {
  const card = el("section", { class: "setup-card" },
    el("h2", {}, running.length === 1 ? "Found a model server" : "Found model servers"),
    el("p", { class: "muted" }, "Pick one to use. You can add the others later on the Servers page."));

  for (const endpoint of running) {
    card.append(
      el("div", { class: "setup-option" },
        el("div", { class: "grow" },
          el("strong", {}, endpoint.label),
          el("span", { class: "muted" }, ` — ${endpoint.base_url}`),
          el("div", { class: "muted small" },
            `${endpoint.models.length} model(s): ${endpoint.models.slice(0, 4).join(", ")}` +
            (endpoint.models.length > 4 ? ", …" : ""))),
        el("button", {
          class: "primary",
          disabled: state.busy,
          onclick: () => useEndpoint(endpoint, state),
        }, "Use this")));
  }
  return card;
}

function nothingFoundCard(scan_state) {
  const scan = scan_state.scan;
  const vram = scan.vram_gb ? `${scan.vram_gb} GB of usable VRAM` : "no dedicated GPU that I could find";
  return el("section", { class: "setup-card" },
    el("h2", {}, "No model server is running here"),
    el("p", {},
      `This machine has ${vram}. `,
      "Install one of these, download the suggested model, then come back and press Look again."),
    el("div", { class: "setup-grid" },
      installOption("LM Studio", "https://lmstudio.ai",
        "A desktop app. Easiest if you have not run a local model before. Turn on its local " +
        "server in the Developer tab; CITAR talks to it on port 1234."),
      installOption("Ollama", "https://ollama.com",
        `A command-line tool. After installing: ollama pull ${scan.suggested_model}`)),
    el("div", { class: "suggestion" },
      el("strong", {}, "Suggested model: "), el("code", {}, scan.suggested_model),
      el("div", { class: "muted small" }, scan.suggested_reason)),
    el("div", { class: "row gap" },
      el("button", { onclick: () => location.reload() }, "Look again"),
      el("button", { class: "link", onclick: () => apiKeyDialog(scan_state) }, "I have an API key instead")));
}

function installOption(name, url, detail) {
  return el("div", { class: "setup-option column" },
    el("a", { href: url, target: "_blank", rel: "noopener noreferrer", class: "big-link" }, name),
    el("p", { class: "muted small" }, detail));
}

async function useEndpoint(endpoint, state) {
  state.busy = true;
  try {
    const result = await api.setupApply({
      provider: endpoint.provider,
      label: `${endpoint.label} (this computer)`,
      base_url: endpoint.base_url,
      models: endpoint.models,
    });
    if (result.warning) toast(result.warning, "warn", 8000);
    await api.setupDismiss();
    toast(`${endpoint.label} registered. Create a game and give a seat to a model.`, "info", 6000);
    location.hash = "#/";
  } catch (e) {
    toast(e.message, "error");
    state.busy = false;
  }
}

/**
 * Collect a hosted-API key.
 *
 * The key goes straight to the server, which puts it in the OS credential store and never sends it
 * back. There is no field anywhere that shows an existing key, because there is nothing to show.
 */
function apiKeyDialog(state) {
  import("./util.js").then(({ modal }) => {
    const provider = el("select", {},
      el("option", { value: "anthropic" }, "Anthropic (Claude)"),
      el("option", { value: "openai_compatible" }, "OpenAI-compatible"));
    const baseUrl = el("input", { placeholder: "https://api.openai.com/v1", disabled: true });
    const model = el("input", { value: "claude-sonnet-5" });
    const key = el("input", { type: "password", placeholder: "sk-…", autocomplete: "off" });

    provider.addEventListener("change", () => {
      const hosted = provider.value === "anthropic";
      baseUrl.disabled = hosted;
      model.value = hosted ? "claude-sonnet-5" : "gpt-4o-mini";
    });

    const close = modal({
      title: "Use a hosted model",
      narrow: true,
      content: el("div", { class: "form" },
        el("label", {}, "Provider", provider),
        el("label", {}, "Base URL", baseUrl),
        el("label", {}, "Model", model),
        el("label", {}, "API key", key),
        el("p", { class: "muted small" },
          "The key is stored in this computer's credential store and is never written to the ",
          "project folder, a save file or a report.")),
      footer: el("button", {
        class: "primary",
        onclick: async () => {
          try {
            await api.setupApply({
              provider: provider.value,
              kind: "api",
              label: provider.value === "anthropic" ? "Anthropic API" : "Hosted API",
              base_url: provider.value === "anthropic" ? "" : baseUrl.value.trim(),
              models: [model.value.trim()],
              api_key: key.value.trim(),
              collect_hardware: false,
            });
            await api.setupDismiss();
            close();
            toast("Registered. Create a game and give a seat to the model.", "info", 6000);
            location.hash = "#/";
          } catch (e) {
            toast(e.message, "error");
          }
        },
      }, "Save"),
    });
  });
}

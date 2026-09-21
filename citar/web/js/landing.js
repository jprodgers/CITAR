// The front door: what a stranger sees at the root of a public CITAR server.
//
// Before this, an anonymous visitor was sent straight to a sign-in form, which answers none of the
// questions somebody arriving from a link actually has: what is this, can I try it, and can I run
// it myself. So the root now explains itself first and offers the sign-in second.
//
// Only in server mode. Local mode signs the operator in automatically, so nobody ever reaches this.
import { api } from "./api.js";
import { el, clear } from "./util.js";

/** The install command for the visitor's platform, guessed from the user agent. */
function installFor(platform) {
  if (platform === "windows") {
    return {
      label: "Windows",
      note: "Download and run. No Python needed.",
      command: "winget install JimmieRodgers.CITAR",
      link: { text: "or download the installer", href: "https://github.com/jprodgers/CITAR/releases/latest" },
    };
  }
  if (platform === "mac") {
    return {
      label: "macOS",
      note: "Installs into its own environment; nothing is added to your system Python.",
      command: "curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash",
      link: { text: "or use Homebrew", href: "https://github.com/jprodgers/CITAR/blob/main/docs/INSTALL.md" },
    };
  }
  return {
    label: "Linux",
    note: "Installs into its own environment; needs no root.",
    command: "curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash",
    link: { text: "or use pipx, Docker, or build from source", href: "https://github.com/jprodgers/CITAR/blob/main/docs/INSTALL.md" },
  };
}

function guessPlatform() {
  const ua = navigator.userAgent || "";
  if (/Windows/i.test(ua)) return "windows";
  if (/Mac OS X|Macintosh/i.test(ua)) return "mac";
  return "linux";
}

/** A command with a copy button, because a command nobody can copy is a screenshot. */
function commandBox(command) {
  const code = el("code", {}, command);
  const button = el("button", {
    class: "copy",
    onclick: async () => {
      try {
        await navigator.clipboard.writeText(command);
        button.textContent = "copied";
        setTimeout(() => { button.textContent = "copy"; }, 1500);
      } catch (e) {
        // Clipboard access is refused in some contexts; select the text instead so it can be
        // copied by hand rather than leaving the button looking broken.
        const range = document.createRange();
        range.selectNodeContents(code);
        window.getSelection().removeAllRanges();
        window.getSelection().addRange(range);
      }
    },
  }, "copy");
  return el("div", { class: "command" }, code, button);
}

/** A hexagon, drawn rather than shipped, so the page needs no images to look like something. */
function hexArt() {
  const hexes = [];
  const colors = ["#3c78d8", "#4f8cff", "#2f5fa8", "#6fb7ff", "#27ae60", "#e8c547"];
  for (let row = 0; row < 5; row++) {
    for (let col = 0; col < 7; col++) {
      const x = col * 52 + (row % 2 ? 26 : 0);
      const y = row * 45;
      const shade = colors[(row * 7 + col * 3) % colors.length];
      const opacity = 0.18 + ((row * 7 + col) % 5) * 0.11;
      hexes.push(`<polygon points="26,0 52,15 52,45 26,60 0,45 0,15"
        transform="translate(${x},${y})" fill="${shade}" opacity="${opacity.toFixed(2)}"/>`);
    }
  }
  const svg = `<svg viewBox="0 0 390 285" width="100%" height="100%" aria-hidden="true">${hexes.join("")}</svg>`;
  return el("div", { class: "hex-art", html: svg });
}

export async function renderLanding(root) {
  clear(root);
  const config = await api.authConfig().catch(() => ({}));
  const install = installFor(guessPlatform());
  const page = el("div", { class: "landing" });
  root.appendChild(page);

  const canSignUp = config.registration === "open";
  const inviteOnly = config.registration === "invite";

  page.append(
    el("header", { class: "landing-hero" },
      el("div", { class: "hero-text" },
        el("h1", {}, "CITAR"),
        el("p", { class: "hero-tagline" },
          "A Civilization-style strategy game whose players can be language models."),
        el("p", { class: "lede" },
          "Play against them, watch them play each other, and measure how well they do it. ",
          "Full Civ V rules — nine eras, religion, policies, espionage, diplomacy — with an ",
          "interface built so a model can sit in any seat."),
        el("div", { class: "hero-buttons" },
          el("a", { class: "btn primary big", href: "#/login" }, "Sign in"),
          canSignUp ? el("a", { class: "btn big", href: "#/signup" }, "Create an account") : null,
          el("a", { class: "btn big", href: "#install" }, "Run it yourself")),
        inviteOnly
          ? el("p", { class: "muted small" },
              config.closed_message || "This server is invite-only. You can still run your own copy below.")
          : null),
      hexArt()),

    el("section", { class: "landing-cards" },
      card("Play", "A complete game of Civ V in the browser: tech tree, policies, religion, "
        + "espionage, city-states, diplomacy with a deal builder, five victory conditions."),
      card("Watch", "Games with no human seat can be watched with full vision — paused, slowed "
        + "down, seen through any civilization's eyes, with each AI's reasoning as it arrives."),
      card("Measure", "Full-game benchmarks on identical maps, scenario probes that test one "
        + "decision at a time, per-turn metrics, and reports that price what it all cost.")),

    el("section", { class: "landing-why" },
      el("h2", {}, "Why a game, for this?"),
      el("p", {},
        "Most model evaluations are short: a question, an answer, a score. A game of Civilization ",
        "is the opposite — hundreds of turns, imperfect information, an opponent who reacts, and ",
        "consequences that arrive forty turns after the decision that caused them."),
      el("p", {},
        "That is a different thing to be good at, and it is hard to fake. Models are not very good ",
        "at it yet, and ",
        el("em", {}, "how"),
        " they fail is the interesting part: rarely the rules, usually holding a plan across forty ",
        "turns and noticing when it has stopped working.")),

    el("section", { class: "landing-install", id: "install" },
      el("h2", {}, "Run your own"),
      el("p", { class: "muted" },
        "CITAR runs on your own machine against models you control — LM Studio, Ollama, ",
        "llama.cpp, an API key, or nothing at all if you want to play the scripted bots. ",
        "It sends nothing anywhere."),
      el("div", { class: "install-box" },
        el("div", { class: "row gap" },
          el("strong", {}, install.label),
          el("span", { class: "muted small" }, install.note)),
        commandBox(install.command),
        el("p", { class: "muted small" },
          el("a", { href: install.link.href, target: "_blank", rel: "noopener noreferrer" },
            install.link.text))),
      el("p", { class: "muted small" },
        "Then ", el("code", {}, "citar setup"), " finds your models and ", el("code", {}, "citar"),
        " starts playing.")),

    el("footer", { class: "landing-footer" },
      el("a", { href: "https://github.com/jprodgers/CITAR" }, "Source"),
      el("a", { href: "https://github.com/jprodgers/CITAR/blob/main/docs/QUICKSTART.md" }, "Documentation"),
      el("a", { href: "https://github.com/jprodgers/CITAR/releases" }, "Releases"),
      el("span", { class: "muted small grow" },
        "MPL-2.0. Rules derived from ",
        el("a", { href: "https://github.com/yairm210/Unciv" }, "UnCiv"),
        ". Not affiliated with Take-Two, 2K or Firaxis.")));

  return { destroy() {} };
}

function card(title, text) {
  return el("div", { class: "landing-card" }, el("h3", {}, title), el("p", {}, text));
}

// Download links for the CITAR helper: one self-contained program, run on the machine that has the model (and, if its
// owner says so, spare CPU to lend). The five builds and what a visitor is offered are helperrules.js's; this module
// draws the card. The files are attached to every GitHub release under names that never change, so the links always
// fetch the newest build (releases/latest/download/<name>) and never go stale when CITAR is updated.
import { el, toast } from "./util.js";
import { HELPERS, RELEASES, REPO, commands, download, offered, platformOf } from "./helperrules.js";

export { HELPERS, RELEASES, REPO };

// The visitor's platform: the user agent, and Chromium's high-entropy client hints when the browser gives them.
export async function detectPlatform() {
  let hints = null;
  try {
    hints = navigator.userAgentData && await navigator.userAgentData.getHighEntropyValues(["architecture", "platform"]);
  } catch (e) { /* no hints: the user agent alone */ }
  return platformOf(navigator.userAgent || "", hints);
}

function copyBox(text) {
  return el("div", { class: "helper-cmd" }, el("code", {}, text),
    el("button", { class: "small", type: "button", onclick: () => {
      navigator.clipboard.writeText(text).then(() => toast("Copied.", "good"), () => toast("Select the text to copy it.", "error"));
    } }, "Copy"));
}

// The full "get the helper" card. `args` (optional) are the helper's arguments for one particular server, so the card
// can show the exact command to run with the file just downloaded.
export function helperCard({ args = null, compact = false } = {}) {
  const card = el("div", { class: `card helper-card${compact ? " compact" : ""}` });
  const body = el("div", {});
  card.append(
    el("div", { class: "row wrap" }, el("h3", { style: { margin: 0 } }, "CITAR helper"),
      el("span", { class: "muted" }, "connects the models on your PC to this server — it dials out, so no ports need opening")),
    body);
  detectPlatform().then((platform) => {
    const { primary, others } = offered(platform);
    body.append(
      primary.length
        ? el("div", { class: "row wrap helper-main" },
            ...primary.map((h) => el("a", { class: "button primary", href: download(h.file), download: h.file },
              `⬇ Download for ${h.label}`)),
            primary.length > 1
              ? el("span", { class: "muted small" },
                  "Which Mac? Apple menu → About This Mac: an Apple M chip is Apple silicon, an Intel processor is Intel.")
              : el("span", { class: "muted small" }, primary[0].file))
        : el("p", { class: "muted" }, platform.os === "mobile"
            ? "The helper runs on the computer with your GPU, not on a phone. Open this page there, or pick a download below."
            : "Pick the download for the machine that runs your models:"),
      el("div", { class: "row wrap helper-others" },
        el("span", { class: "muted small" }, primary.length ? "Other platforms:" : ""),
        ...others.map((h) => el("a", { href: download(h.file), download: h.file, class: "small" }, h.label)),
        el("a", { href: RELEASES, class: "small", target: "_blank", rel: "noopener" }, "All downloads")));
    if (compact) return;
    const steps = el("ol", { class: "helper-steps" },
      el("li", {}, "Start LM Studio (or Ollama) on that machine and load a model."),
      el("li", {}, "Issue a worker token: Servers → your machine → Helper."),
      el("li", {}, "Run the helper. Double-clicked, it asks for this server's address and the token once and remembers them; ",
        "or give them on the command line:"));
    body.append(steps);
    const how = commands(primary.length ? primary : [HELPERS[0]]);
    if (how.setup) body.append(copyBox(how.setup));
    else if (how.note) body.append(el("p", { class: "muted small" }, how.note));
    body.append(copyBox(`${how.run} ${args || `--server ${location.origin} --token YOUR_TOKEN`}`));
    body.append(el("p", { class: "muted small" },
      `Once it connects, ${how.run} service install keeps it running (at sign-in, or as a service) and after its `,
      "updates: the helper's guide, \"Running the helper as a service\", says more."));
  });
  return card;
}

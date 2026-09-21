// Download links for the CITAR helper: the worker as one self-contained program, run on the machine that has the
// model. The files are attached to every GitHub release under names that never change, so the links below always
// fetch the newest build (releases/latest/download/<name>) and never go stale when CITAR is updated.
import { el, toast } from "./util.js";

export const REPO = "jprodgers/CITAR";
export const RELEASES = `https://github.com/${REPO}/releases/latest`;
const download = (file) => `${RELEASES}/download/${file}`;

export const HELPERS = [
  { os: "windows", arch: "x64", file: "citar-helper-windows-x64.exe", label: "Windows (64-bit)",
    run: (f) => `.\\${f}`, setup: "Windows may warn that the file is from an unknown publisher: choose More info → Run anyway." },
  { os: "mac", arch: "arm64", file: "citar-helper-macos-arm64", label: "macOS (Apple silicon)",
    run: (f) => `./${f}`, setup: (f) => `chmod +x ${f} && xattr -d com.apple.quarantine ${f}` },
  { os: "linux", arch: "x64", file: "citar-helper-linux-x64", label: "Linux (x64)",
    run: (f) => `./${f}`, setup: (f) => `chmod +x ${f}` },
  { os: "linux", arch: "arm64", file: "citar-helper-linux-arm64", label: "Linux (ARM64)",
    run: (f) => `./${f}`, setup: (f) => `chmod +x ${f}` },
];

// Best guess at the visitor's platform. The user-agent string is enough for the OS; the architecture needs
// the (Chromium-only) high-entropy client hints, and otherwise defaults to what is most common.
export async function detectPlatform() {
  const ua = navigator.userAgent || "";
  let os = /Windows/i.test(ua) ? "windows" : /iPhone|iPad|iPod|Android/i.test(ua) ? "mobile"
    : /Mac OS X|Macintosh/i.test(ua) ? "mac" : /Linux|X11|CrOS/i.test(ua) ? "linux" : "unknown";
  let arch = /aarch64|arm64/i.test(ua) ? "arm64" : os === "mac" ? "arm64" : "x64";
  try {
    const hints = navigator.userAgentData && await navigator.userAgentData.getHighEntropyValues(["architecture", "platform"]);
    if (hints && hints.architecture) arch = hints.architecture === "arm" ? "arm64" : "x64";
  } catch (e) { /* no hints: keep the guess */ }
  return { os, arch };
}

export function helperFor(platform) {
  return HELPERS.find((h) => h.os === platform.os && h.arch === platform.arch)
    || HELPERS.find((h) => h.os === platform.os) || null;
}

function copyBox(text) {
  return el("div", { class: "helper-cmd" }, el("code", {}, text),
    el("button", { class: "small", type: "button", onclick: () => {
      navigator.clipboard.writeText(text).then(() => toast("Copied.", "good"), () => toast("Select the text to copy it.", "error"));
    } }, "Copy"));
}

// The full "get the helper" card. `args` (optional) are the worker arguments for one particular server, so the
// card can show the exact command to run with the file just downloaded.
export function helperCard({ args = null, compact = false } = {}) {
  const card = el("div", { class: `card helper-card${compact ? " compact" : ""}` });
  const body = el("div", {});
  card.append(
    el("div", { class: "row wrap" }, el("h3", { style: { margin: 0 } }, "CITAR helper"),
      el("span", { class: "muted" }, "connects the models on your PC to this server — it dials out, so no ports need opening")),
    body);
  detectPlatform().then((platform) => {
    const mine = helperFor(platform);
    const others = HELPERS.filter((h) => h !== mine);
    body.append(
      mine
        ? el("div", { class: "row wrap helper-main" },
            el("a", { class: "button primary", href: download(mine.file), download: mine.file }, `⬇ Download for ${mine.label}`),
            el("span", { class: "muted small" }, mine.file))
        : el("p", { class: "muted" }, platform.os === "mobile"
            ? "The helper runs on the computer with your GPU, not on a phone. Open this page there, or pick a download below."
            : platform.os === "mac" && platform.arch === "x64"
              ? "There is no ready-made build for Intel Macs: install CITAR with pipx and run `citar worker`."
              : "Pick the download for the machine that runs your models:"),
      el("div", { class: "row wrap helper-others" },
        el("span", { class: "muted small" }, mine ? "Other platforms:" : ""),
        ...others.map((h) => el("a", { href: download(h.file), download: h.file, class: "small" }, h.label)),
        el("a", { href: RELEASES, class: "small", target: "_blank", rel: "noopener" }, "All downloads")));
    if (compact) return;
    const h = mine || HELPERS[0];
    const steps = el("ol", { class: "helper-steps" },
      el("li", {}, "Start LM Studio (or Ollama) on that machine and load a model."),
      el("li", {}, "Issue a worker token: Servers → your machine → Worker."),
      el("li", {}, "Run the helper. Double-clicked, it asks for this server's address and the token once and remembers them; ",
        "or give them on the command line:"));
    body.append(steps);
    const setup = typeof h.setup === "function" ? h.setup(h.file) : null;
    if (setup) body.append(copyBox(setup));
    else if (h.setup) body.append(el("p", { class: "muted small" }, h.setup));
    body.append(copyBox(`${h.run(h.file)} ${args || `--server ${location.origin} --token YOUR_TOKEN`}`));
    body.append(el("p", { class: "muted small" }, "With CITAR itself installed on that machine, ",
      el("code", {}, "citar worker"), " does the same job."));
  });
  return card;
}

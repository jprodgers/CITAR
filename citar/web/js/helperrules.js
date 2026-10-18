// The CITAR helper's downloads, and which of them a visitor is offered (design/phase4-helper.md P4.5.11, P4.10): the
// pure half of the helper card (helper.js), so citar/web/tests/helper.test.mjs can hold the file names to the release
// workflow's and a browser's user agent to what it is offered. The files are attached to every GitHub release under
// names that never change, so a link to releases/latest/download/<name> always fetches the newest build.

export const REPO = "jprodgers/CITAR";
export const RELEASES = `https://github.com/${REPO}/releases/latest`;
export const download = (file) => `${RELEASES}/download/${file}`;

const unix = {
  run: (f) => `./${f}`,
};

// One entry a release asset: release.yml's build-helper matrix and helper.yml's artifact matrix build these five names
// (the Node test holds them equal).
export const HELPERS = [
  { os: "windows", arch: "x64", file: "citar-helper-windows-x64.exe", label: "Windows (64-bit)",
    run: (f) => `.\\${f}`,
    setup: "Windows may warn that the file is from an unknown publisher: choose More info → Run anyway." },
  { os: "mac", arch: "arm64", file: "citar-helper-macos-arm64", label: "macOS (Apple silicon)", ...unix,
    setup: (f) => `chmod +x ${f} && xattr -d com.apple.quarantine ${f}` },
  { os: "mac", arch: "x64", file: "citar-helper-macos-x64", label: "macOS (Intel)", ...unix,
    setup: (f) => `chmod +x ${f} && xattr -d com.apple.quarantine ${f}` },
  { os: "linux", arch: "x64", file: "citar-helper-linux-x64", label: "Linux (x64)", ...unix,
    setup: (f) => `chmod +x ${f}` },
  { os: "linux", arch: "arm64", file: "citar-helper-linux-arm64", label: "Linux (ARM64)", ...unix,
    setup: (f) => `chmod +x ${f}` },
];

// The platform a browser says it runs on. The OS comes from its user agent. The architecture comes from Chromium's
// high-entropy client hints when the page has them (`hints`, from navigator.userAgentData), else from the user agent
// when it names one; a Mac's user agent never does (Safari and Firefox say "Intel Mac OS X" on Apple silicon too, and
// send no hints), so a Mac without hints is "unknown" and is offered both of its downloads.
export function platformOf(ua = "", hints = null) {
  const os = /Windows/i.test(ua) ? "windows" : /iPhone|iPad|iPod|Android/i.test(ua) ? "mobile"
    : /Mac OS X|Macintosh/i.test(ua) ? "mac" : /Linux|X11|CrOS/i.test(ua) ? "linux" : "unknown";
  let arch = null;
  if (hints && hints.architecture) {
    arch = hints.architecture === "arm" ? "arm64" : "x64";
  } else if (os !== "mac" && /aarch64|arm64/i.test(ua)) {
    arch = "arm64";
  }
  if (!arch) arch = os === "mac" ? "unknown" : "x64";
  return { os, arch };
}

// What the card offers `platform`: `primary`, the downloads for it (both Macs' when its architecture is unknown; the
// x64 build where there is none for its architecture, as on Windows on ARM, which runs it), and `others`, the rest.
export function offered(platform) {
  const sameOs = HELPERS.filter((h) => h.os === platform.os);
  let primary = sameOs.filter((h) => platform.arch === "unknown" || h.arch === platform.arch);
  if (!primary.length) primary = sameOs.filter((h) => h.arch === "x64").slice(0, 1);
  return { primary, others: HELPERS.filter((h) => !primary.includes(h)) };
}

// The steps for the downloads in `primary`: `setup`, a command to run first (or null), `note`, a sentence instead (or
// null), and `run`, the program as typed. With more than one (a Mac of unknown architecture) the shell's pattern stands
// for whichever was downloaded.
export function commands(primary) {
  const h = primary[0] || HELPERS[0];
  const file = primary.length > 1 ? h.file.replace(/-[a-z0-9]+$/, "-*") : h.file;
  const command = typeof h.setup === "function";
  return { setup: command ? h.setup(file) : null, note: command ? null : h.setup, run: h.run(file) };
}

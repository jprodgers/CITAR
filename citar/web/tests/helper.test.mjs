// The helper card's downloads (citar/web/js/helperrules.js, design/phase4-helper.md P4.10, 4-14): its five names are the
// release's, as helper.yml's artifact matrix builds them (one line a target, read by pattern: the project has no YAML
// dependency) and as cargo xtask helper-manifest lists them; and what a browser is offered. release.yml is only its
// gate until Phase 5 rebuilds it (design/phase5-release.md P5.11), so helper.yml's matrix is the one read here (5-02a);
// package.yml's helpers matrix joins it in 5-03. Run with `node --test "citar/web/tests/*.test.mjs"`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { HELPERS, commands, download, offered, platformOf } from "../js/helperrules.js";

const ROOT = new URL("../../../", import.meta.url);
const read = (rel) => readFileSync(new URL(rel, ROOT), "utf8").replace(/\r\n/g, "\n");

// The `- { key: value, ... }` lines of the matrix of job `job` in workflow `file`: each as an object.
function matrix(file, job) {
  const text = read(file);
  const start = text.indexOf(`\n  ${job}:\n`);
  assert.ok(start >= 0, `${file} has a job ${job}`);
  const rest = text.slice(start + 1);
  const next = rest.slice(1).search(/\n  [A-Za-z0-9_-]+:\n/);
  const block = next >= 0 ? rest.slice(0, next + 1) : rest;
  return block.split("\n")
    .map((line) => line.match(/^\s*- \{(.*)\}\s*$/))
    .filter(Boolean)
    .map((m) => Object.fromEntries(m[1].split(",").map((pair) => pair.split(":").map((s) => s.trim()))));
}

// cargo xtask helper-manifest's (target, name) table (xtask/src/manifest.rs).
function manifestTargets() {
  const text = read("xtask/src/manifest.rs");
  return [...text.matchAll(/\("([a-z0-9_]+-[a-z0-9_-]+)", "(citar-helper-[a-z0-9.-]+)"\)/g)].map((m) => ({
    target: m[1], asset: m[2] }));
}

const byTarget = (rows) => rows.map((r) => `${r.target} ${r.asset}`).sort();

test("the card's five names are the release's: helper.yml's artifacts, the manifest's", () => {
  const names = HELPERS.map((h) => h.file).sort();
  assert.equal(names.length, 5);
  const artifacts = matrix(".github/workflows/helper.yml", "artifact");
  assert.equal(artifacts.length, 5, JSON.stringify(artifacts));
  assert.deepEqual(artifacts.map((r) => r.asset).sort(), names, "helper.yml builds the card's names");
  assert.equal(new Set(artifacts.map((r) => r.target)).size, 5, "one build a target");
  assert.deepEqual(byTarget(manifestTargets()), byTarget(artifacts), "the manifest lists each name for its target");
  for (const h of HELPERS) {
    assert.ok(download(h.file).endsWith(`/releases/latest/download/${h.file}`), h.file);
  }
});

test("an Intel Mac has its own download, and no line sends anyone to pipx", () => {
  assert.ok(HELPERS.some((h) => h.os === "mac" && h.arch === "x64" && h.file === "citar-helper-macos-x64"));
  for (const rel of ["citar/web/js/helper.js", "citar/web/js/helperrules.js"]) {
    assert.ok(!/pipx/i.test(read(rel)), `${rel} names pipx`);
  }
});

const SAFARI_MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) "
  + "Version/18.1 Safari/605.1.15";
const FIREFOX_MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14.6; rv:131.0) Gecko/20100101 Firefox/131.0";
const CHROME_MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) "
  + "Chrome/130.0.0.0 Safari/537.36";
const CHROME_WIN = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 "
  + "Safari/537.36";
const FIREFOX_LINUX_ARM = "Mozilla/5.0 (X11; Linux aarch64; rv:131.0) Gecko/20100101 Firefox/131.0";
const FIREFOX_LINUX = "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0";
const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) "
  + "Version/18.1 Mobile/15E148 Safari/604.1";

const primaryOf = (ua, hints = null) => offered(platformOf(ua, hints)).primary.map((h) => h.file);

test("Safari and Firefox on a Mac, which never say its architecture, are offered both macOS downloads", () => {
  for (const ua of [SAFARI_MAC, FIREFOX_MAC]) {
    assert.deepEqual(platformOf(ua), { os: "mac", arch: "unknown" });
    assert.deepEqual(primaryOf(ua), ["citar-helper-macos-arm64", "citar-helper-macos-x64"]);
    const { others } = offered(platformOf(ua));
    assert.ok(!others.some((h) => h.os === "mac"), "neither Mac is relegated to the other platforms");
    // the commands name whichever was downloaded
    assert.deepEqual(commands(offered(platformOf(ua)).primary), {
      setup: "chmod +x citar-helper-macos-* && xattr -d com.apple.quarantine citar-helper-macos-*",
      note: null, run: "./citar-helper-macos-*" });
  }
});

test("Chromium's client hints pick one Mac's download; other systems get theirs", () => {
  assert.deepEqual(primaryOf(CHROME_MAC, { architecture: "arm", platform: "macOS" }), ["citar-helper-macos-arm64"]);
  assert.deepEqual(primaryOf(CHROME_MAC, { architecture: "x86", platform: "macOS" }), ["citar-helper-macos-x64"]);
  assert.deepEqual(primaryOf(CHROME_MAC, {}), ["citar-helper-macos-arm64", "citar-helper-macos-x64"]);
  assert.deepEqual(primaryOf(CHROME_WIN), ["citar-helper-windows-x64.exe"]);
  // Windows on ARM runs the x64 build: it is offered rather than nothing
  assert.deepEqual(primaryOf(CHROME_WIN, { architecture: "arm", platform: "Windows" }), ["citar-helper-windows-x64.exe"]);
  assert.deepEqual(primaryOf(FIREFOX_LINUX), ["citar-helper-linux-x64"]);
  assert.deepEqual(primaryOf(FIREFOX_LINUX_ARM), ["citar-helper-linux-arm64"]);
  assert.deepEqual(primaryOf(IPHONE), [], "a phone is offered nothing as its own");
  assert.equal(offered(platformOf(IPHONE)).others.length, 5);
  const win = commands(offered(platformOf(CHROME_WIN)).primary);
  assert.equal(win.setup, null);
  assert.match(win.note, /More info → Run anyway/);
  assert.equal(win.run, ".\\citar-helper-windows-x64.exe");
  assert.deepEqual(commands(offered(platformOf(FIREFOX_LINUX)).primary),
    { setup: "chmod +x citar-helper-linux-x64", note: null, run: "./citar-helper-linux-x64" });
});

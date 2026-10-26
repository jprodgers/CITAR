// The lobby's MCP commands for a seat (citar/web/js/mcpcommand.js), built from /api/meta's mcp_command
// (design/phase5-release.md P5.6). Run with `node --test "citar/web/tests/*.test.mjs"` from the checkout.
import { test } from "node:test";
import assert from "node:assert/strict";

import { MCP_ANYWHERE, claudeAddCommand, clientConfig, mcpCommand, mcpHint, quoteArg } from "../js/mcpcommand.js";

const seat = { serverUrl: "http://127.0.0.1:8765", gameId: "g42", player: 1, token: "tok-abc" };
const SEAT = ["--server", "http://127.0.0.1:8765", "--game", "g42", "--token", "tok-abc"];

test("a bundle's local server gives its citar.exe mcp, quoted where its folder has a space", () => {
  const meta = { mode: "local", mcp_command: ["C:\\Program Files\\CITAR\\citar.exe", "mcp"] };
  assert.equal(claudeAddCommand(meta, seat),
    'claude mcp add citar-g42-p1 -- "C:\\Program Files\\CITAR\\citar.exe" mcp --server http://127.0.0.1:8765 --game g42 '
    + "--token tok-abc");
  assert.deepEqual(clientConfig(meta, seat), { mcpServers: { "citar-p1": {
    command: "C:\\Program Files\\CITAR\\citar.exe", args: ["mcp", ...SEAT] } } });
  assert.equal(mcpHint(meta), null);
});

test("a wheel's local server gives the citar-mcp script beside its interpreter, quoted for its backslashes", () => {
  const meta = { mode: "local", mcp_command: ["C:\\dev\\venv\\x\\Scripts\\citar-mcp.exe"] };
  assert.equal(claudeAddCommand(meta, seat),
    'claude mcp add citar-g42-p1 -- "C:\\dev\\venv\\x\\Scripts\\citar-mcp.exe" ' + SEAT.join(" "));
  assert.deepEqual(clientConfig(meta, seat).mcpServers["citar-p1"],
    { command: "C:\\dev\\venv\\x\\Scripts\\citar-mcp.exe", args: SEAT });
});

test("a checkout's local server gives its python and citar_mcp.py", () => {
  const meta = { mode: "local", mcp_command: ["/usr/bin/python3", "/home/me/CITAR/citar_mcp.py"] };
  assert.deepEqual(clientConfig(meta, seat).mcpServers["citar-p1"],
    { command: "/usr/bin/python3", args: ["/home/me/CITAR/citar_mcp.py", ...SEAT] });
});

test("a server-mode server names no path of its own, and says the command is the player's own", () => {
  const meta = { mode: "server", mcp_command: ["citar-mcp"] };
  assert.equal(claudeAddCommand(meta, seat), "claude mcp add citar-g42-p1 -- citar-mcp " + SEAT.join(" "));
  assert.match(mcpHint(meta), /your own CITAR install/);
  assert.match(mcpHint(meta), /put citar-mcp on your PATH/);
  assert.doesNotMatch(mcpHint(meta), /citar\.exe/);
});

test("a server-mode server run from the Windows app names its citar.exe mcp, and says where citar.exe is", () => {
  const meta = { mode: "server", mcp_command: ["citar.exe", "mcp"] };
  assert.equal(claudeAddCommand(meta, seat), "claude mcp add citar-g42-p1 -- citar.exe mcp " + SEAT.join(" "));
  const hint = mcpHint(meta);
  assert.match(hint, /your own CITAR install/);
  assert.match(hint, /citar\.exe is the program of CITAR's Windows app/);
  assert.match(hint, /the folder the app is installed in, or give its full path, unless the installer added/);
  assert.match(hint, /write citar-mcp in place of "citar\.exe mcp"/);
  // the hint follows the command the server gives, whatever its case
  assert.equal(mcpHint({ mode: "server", mcp_command: ["CITAR.EXE", "mcp"] }), hint);
  // a local server's command is this computer's own: no hint
  assert.equal(mcpHint({ mode: "local", mcp_command: ["citar.exe", "mcp"] }), null);
});

test("no usable mcp_command (an older server, no answer) falls back to citar-mcp", () => {
  for (const meta of [null, undefined, {}, { mcp_command: [] }, { mcp_command: "citar-mcp" },
    { mcp_command: ["citar-mcp", 3] }, { mcp_command: [""] }, { mcp_script: "x.py", python: "py" }]) {
    assert.deepEqual(mcpCommand(meta), MCP_ANYWHERE, JSON.stringify(meta));
  }
  // the server's list is copied, never handed out to be changed
  const meta = { mcp_command: ["citar-mcp"] };
  mcpCommand(meta).push("x");
  assert.deepEqual(meta.mcp_command, ["citar-mcp"]);
});

test("words are quoted only when a shell would split or read them", () => {
  assert.equal(quoteArg("http://127.0.0.1:8765"), "http://127.0.0.1:8765");
  // a POSIX shell (Git Bash, WSL) would drop a bare backslash; in double quotes it keeps one before a letter
  assert.equal(quoteArg("C:\\dev\\citar.exe"), '"C:\\dev\\citar.exe"');
  assert.equal(quoteArg("C:/dev/citar.exe"), "C:/dev/citar.exe");
  assert.equal(quoteArg("/home/me/My Games/citar_mcp.py"), '"/home/me/My Games/citar_mcp.py"');
  assert.equal(quoteArg('a"b'), '"a\\"b"');
  assert.equal(quoteArg("a&b"), '"a&b"');
  assert.equal(quoteArg(""), '""');
});

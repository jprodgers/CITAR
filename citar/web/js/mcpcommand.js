// The commands that connect an MCP client to one seat (design/phase5-release.md P5.6), apart from the lobby so Node
// can test them (nothing here touches the DOM or the network). The server's /api/meta gives `mcp_command`, the
// command that starts its install's MCP bridge (citar.paths.mcp_command): a bundle's `citar.exe mcp`, a wheel's
// `citar-mcp`, a checkout's `python citar_mcp.py`, and in server mode no folder of the server's at all (a remote
// player runs the bridge from their own install). The seat's `--server`, `--game` and `--token` follow it.

/** What a remote player's own install starts the bridge with, when the server says nothing usable. */
export const MCP_ANYWHERE = ["citar-mcp"];

/** The server's `mcp_command`, or `citar-mcp` when it gave none (an older server, or /api/meta unanswered). */
export function mcpCommand(meta) {
  const cmd = meta && meta.mcp_command;
  if (Array.isArray(cmd) && cmd.length && cmd.every((part) => typeof part === "string" && part.length)) return [...cmd];
  return [...MCP_ANYWHERE];
}

/** The seat's own arguments, after the command. */
export function seatArgs(seat) {
  return ["--server", seat.serverUrl, "--game", String(seat.gameId), "--token", seat.token];
}

/** A word as a shell takes it: as it is when it holds only characters no shell treats specially (a Windows path's
 *  backslashes included), else in double quotes, which cmd, PowerShell and a POSIX shell all read alike for a path
 *  with spaces (a double quote inside, which no Windows path holds, is written \"). */
export function quoteArg(word) {
  const text = String(word);
  if (/^[A-Za-z0-9_\-.,/\\:=@+%]+$/.test(text)) return text;
  return `"${text.replace(/"/g, '\\"')}"`;
}

/** Claude Code's command that adds the seat as an MCP server: `seat` is {serverUrl, gameId, player, token}. */
export function claudeAddCommand(meta, seat) {
  const words = [...mcpCommand(meta), ...seatArgs(seat)];
  return `claude mcp add citar-${seat.gameId}-p${seat.player} -- ${words.map(quoteArg).join(" ")}`;
}

/** A generic MCP client's configuration (stdio) for the seat. */
export function clientConfig(meta, seat) {
  const [command, ...args] = mcpCommand(meta);
  return { mcpServers: { [`citar-p${seat.player}`]: { command, args: [...args, ...seatArgs(seat)] } } };
}

/** A line for a server-mode server's seats: the command runs on the player's own computer, from their own install;
 *  null on a local server, whose command is this computer's own. */
export function mcpHint(meta) {
  if (!meta || meta.mode !== "server") return null;
  return "Run it on your own computer, from your own CITAR install (pip, pipx and install.sh put citar-mcp on your PATH).";
}

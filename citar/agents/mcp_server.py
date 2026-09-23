"""MCP (stdio) bridge that lets any MCP client — Claude Code, Claude Desktop, etc. — play a CITAR seat.

    python citar_mcp.py --server http://127.0.0.1:8765 --game <game id> --token <seat token>

The tool list is fetched from the game server, so it always matches the engine's registry.
"""
from __future__ import annotations

import argparse
import json

import anyio
import httpx
import mcp.types as types
from mcp.server.lowlevel import Server
from mcp.server.stdio import stdio_server

from ..engine.views import RULES_OVERVIEW
from ..engine.briefing import MAP_LEGEND

NEGOTIATION_WAIT = 45.0

INSTRUCTIONS = f"""You are playing a seat in CITAR, a turn-based Civilization-style strategy game, against humans and other AIs.

{RULES_OVERVIEW}

HOW TO PLAY THROUGH THESE TOOLS
1. Call wait_for_turn. It returns when it is your turn, when a negotiation is waiting for your reply, or after a timeout
   (then simply call it again).
2. On your turn: get_briefing, then act (set_research, set_production, move_unit, unit_order, found_city, ...), then
   end_turn. Name your civilization with set_civ_name on your first turn.
3. If a negotiation needs you (even outside your turn), inspect it with get_diplomacy and answer with
   respond_negotiation, always with a message. end_turn is refused while a negotiation you are in is open: answer
   it, wait for the other side's reply (get_diplomacy shows it), or withdraw it with action reject.
4. Keep long-term plans in your notebook (write_notes); it is shown in every briefing. Use log_thought to record your
   reasoning for the replay.
5. Repeat until the game is over.
The briefing already includes options for idle units and cities, available techs and a local map, so most turns need
few extra lookups.

ASCII MAP LEGEND (for get_map and the briefing's local map)
{MAP_LEGEND}
"""

EXTRA_TOOLS = [
    {"name": "wait_for_turn",
     "description": "Block until it is your turn, a negotiation awaits your reply, or the game ends (returns 'waiting' "
                    "after timeout_seconds; just call again).",
     "input_schema": {"type": "object", "properties": {"timeout_seconds": {"type": "number", "description": "default 50"}},
                      "additionalProperties": False}},
]


def build_server(base: str, game: str, token: str) -> Server:
    """Build the MCP server that exposes this seat's tools to an external agent."""
    headers = {"Authorization": f"Bearer {token}"}
    with httpx.Client(timeout=30) as c:
        r = c.get(f"{base}/api/tools")
        r.raise_for_status()
        remote_tools = r.json()
    all_tools = remote_tools + EXTRA_TOOLS

    async def on_list_tools(ctx, params):
        """Advertise every game tool, plus ``wait_for_turn``."""
        return types.ListToolsResult(tools=[
            types.Tool(name=t["name"], description=t["description"], input_schema=t["input_schema"]) for t in all_tools])

    async def on_call_tool(ctx, params):
        """Execute a tool call from the client against the game."""
        name = params.name
        args = dict(params.arguments or {})
        try:
            async with httpx.AsyncClient(timeout=httpx.Timeout(400.0, connect=10.0)) as client:
                if name == "wait_for_turn":
                    timeout = float(args.get("timeout_seconds") or 50)
                    r = await client.get(f"{base}/api/games/{game}/wait", params={"timeout": timeout}, headers=headers)
                    r.raise_for_status()
                    text = json.dumps(r.json())
                    ok = True
                else:
                    wait = NEGOTIATION_WAIT if name in ("open_negotiation", "respond_negotiation") else 0
                    r = await client.post(f"{base}/api/games/{game}/tool", headers=headers,
                                          json={"tool": name, "args": args, "wait_seconds": wait})
                    if r.status_code >= 400:
                        return types.CallToolResult(content=[types.TextContent(type="text", text=f"Server error {r.status_code}: {r.text}")],
                                                    is_error=True)
                    data = r.json()
                    ok = data.get("ok", False)
                    res = data.get("result") if ok else data.get("error")
                    text = res if isinstance(res, str) else json.dumps(res, ensure_ascii=False)
        except httpx.HTTPError as e:
            return types.CallToolResult(content=[types.TextContent(type="text", text=f"Could not reach the CITAR server: {e}")],
                                        is_error=True)
        return types.CallToolResult(content=[types.TextContent(type="text", text=text)], is_error=not ok)

    return Server("citar", version="1.0", instructions=INSTRUCTIONS, on_list_tools=on_list_tools, on_call_tool=on_call_tool)


def main(argv=None):
    """The ``citar mcp`` bridge: connect to a game as one seat and serve MCP."""
    ap = argparse.ArgumentParser(description="CITAR MCP bridge for one seat")
    ap.add_argument("--server", default="http://127.0.0.1:8765")
    ap.add_argument("--game", required=True)
    ap.add_argument("--token", required=True)
    a = ap.parse_args(argv)
    server = build_server(a.server.rstrip("/"), a.game, a.token)

    async def run():
        """Run the bridge until the client disconnects."""
        async with stdio_server() as (read, write):
            await server.run(read, write, server.create_initialization_options())

    anyio.run(run)


if __name__ == "__main__":
    main()

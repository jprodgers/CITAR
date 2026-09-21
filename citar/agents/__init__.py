"""Adapters that let something other than a human hold a seat.

An agent's whole job is to take a turn: read the situation, issue orders, stop. What differs
between them is only *who decides*.

:mod:`citar.agents.llm_agent`   a language model, driven by the server
:mod:`citar.agents.bot_agent`   the scripted bot, wrapped in the same interface
:mod:`citar.agents.mcp_server`  the bridge for an external agent over MCP
:mod:`citar.agents.prompts`     what a model is told, and how the turn is framed
:mod:`citar.agents.providers`   the clients that actually reach a model

The division that matters is between the **turn loop** and the **provider**. The loop —
:mod:`citar.agents.llm_agent` — owns the briefing, the guard rails, the limits and the metrics. A
provider turns one request into one completion and knows nothing about the game. Everything that
would bias a comparison between models therefore lives in the loop, which is identical for all of
them; swapping Anthropic for a local endpoint changes how the text is fetched and nothing else.

None of this is privileged. An agent calls the same tools a browser does, through the same
registry in :mod:`citar.engine.tools`, and is refused by the same rules.
"""

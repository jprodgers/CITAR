"""Clients that turn one request into one completion.

A provider knows nothing about the game. It is handed a conversation and a set of tool definitions
and returns what the model said, including any tool calls. Everything that shapes play — the
briefing, the guard rails, the step and time limits, the metrics — lives in
:mod:`citar.agents.llm_agent` and is the same whichever provider is in use.

That line is where a fair comparison comes from. If the Anthropic path and the local path each had
their own turn loop, a difference between two models would also be a difference between two
implementations, and there would be no way to tell which you were measuring.

:mod:`citar.agents.providers.base`               the shared conversation and step types
:mod:`citar.agents.providers.anthropic_provider` the Anthropic API: thinking, prompt caching
:mod:`citar.agents.providers.openai_provider`    anything OpenAI-compatible, native or JSON tools
:mod:`citar.agents.providers.worker_provider`    sends the request down a worker's WebSocket
:mod:`citar.agents.providers.dryrun`             answers plausibly with no model at all
"""

from .base import Conversation, StepResult, ToolCall, make_conversation  # noqa: F401

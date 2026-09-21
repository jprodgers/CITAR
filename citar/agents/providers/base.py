"""Provider-neutral conversation interface used by the LLM agent loop."""
from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class ToolCall:
    """One tool call a model asked for: its name, arguments and id."""
    id: str
    name: str
    args: dict


@dataclass
class StepResult:
    """What one model step produced: text, reasoning, tool calls and usage."""
    text: str = ""
    thinking: str = ""
    tool_calls: list = field(default_factory=list)
    stop_reason: str = ""
    malformed: int = 0          # tool calls that had to be repaired or could not be parsed


class Conversation:
    """The interface every provider implements.

    Deliberately small: add text, add tool results, take a step. Everything that shapes play lives in
    the turn loop, so a provider cannot accidentally change how a model performs - only how it is
    reached.
    """
    usage: dict
    # Seconds the next step() may take (set by the agent from the time left in the turn); None = provider default.
    request_timeout: float | None = None

    def add_user_text(self, text: str):
        """Add a user message."""
        raise NotImplementedError

    def add_tool_results(self, results: list[tuple[str, str, bool]]):
        """results: [(tool_call_id, content, is_error)]"""
        raise NotImplementedError

    def step(self) -> StepResult:
        """Take one step and return what the model produced."""
        raise NotImplementedError

    def close(self):
        """Close the underlying HTTP client, aborting a request in progress where possible."""
        client = getattr(self, "client", None)
        if client is not None and hasattr(client, "close"):
            client.close()


def make_conversation(cfg: dict, system: str, tools: list[dict]) -> Conversation:
    """Build a conversation for a resolved model configuration.

    The one place provider choice happens. Everything upstream deals in a configuration; everything
    downstream deals in a conversation.
    """
    provider = (cfg.get("provider") or "anthropic").lower()
    if provider == "anthropic":
        from .anthropic_provider import AnthropicConversation
        return AnthropicConversation(cfg, system, tools)
    if provider == "dryrun":
        from .dryrun import DryRunConversation
        return DryRunConversation(cfg, system, tools)
    if provider == "worker":
        # A model on somebody else's machine, reached through the connection their worker opened.
        # The engine cannot tell the difference: it is a Conversation like any other.
        from .worker_provider import WorkerConversation
        return WorkerConversation(cfg, system, tools)
    if provider in ("openai", "openai_compatible", "ollama", "lmstudio"):
        from .openai_provider import OpenAIConversation
        return OpenAIConversation(cfg, system, tools)
    raise ValueError(f"Unknown provider '{provider}'")

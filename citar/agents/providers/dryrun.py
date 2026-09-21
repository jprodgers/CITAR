"""Dry-run 'model' for testing benchmark setups without a GPU: founds a city, keeps research and production going,
sends military units exploring, then ends each turn. Makes no network calls."""
from __future__ import annotations

import json
import threading

from .base import Conversation, StepResult, ToolCall


class DryRunConversation(Conversation):
    """A provider that answers plausibly with no model at all.

    It exists so the rest of the system can be tested end to end - suites, scheduling, probes, the
    usage ledger, the reports - without spending GPU time or tokens. It plays badly on purpose; its
    results never count toward a model score.
    """
    def __init__(self, cfg: dict, system: str, tools: list[dict]):
        self.cfg = cfg
        self.delay = float(cfg.get("dry_run_delay") if cfg.get("dry_run_delay") is not None else 1.0)
        self.usage = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0}
        self._closed = threading.Event()
        self._step = 0
        self._results: list[str] = []
        self._tools = {t.get("name") for t in tools or []}
        self._text = ""

    def add_user_text(self, text: str):
        """Ignore the text; the dry run does not read it."""
        self._text += text

    def add_tool_results(self, results):
        """Ignore tool results."""
        self._results = [content for _, content, _ in results]

    def close(self):
        """Nothing to release."""
        self._closed.set()

    def step(self) -> StepResult:
        """Produce a plausible set of orders, after a configurable delay."""
        if self._closed.wait(self.delay):
            raise RuntimeError("dry run cancelled")
        self._step += 1
        self.usage["input_tokens"] += 1000
        self.usage["output_tokens"] += 20
        if "respond_negotiation" in self._tools and "end_turn" not in self._tools:
            return self._negotiate()
        if self._step == 1:
            return StepResult(text="Checking units, cities and research.",
                              tool_calls=[ToolCall("d_u", "get_units", {}), ToolCall("d_c", "get_cities", {}),
                                          ToolCall("d_e", "get_empire", {})])
        if self._step == 2:
            calls = self._orders()
            if calls:
                return StepResult(text="Giving orders.", tool_calls=calls)
        return StepResult(text="Nothing else to do.", tool_calls=[ToolCall(f"d_end{self._step}", "end_turn", {})])

    def _negotiate(self) -> StepResult:
        """Diplomatic interrupt: answers with cfg["dry_run_negotiation"] (accept | reject | counter | reply), or
        accepts deals that give it at least as many items as it gives."""
        import re
        m = re.search(r"negotiation #(\d+)", self._text)
        if not m or self._step > 1:
            return StepResult(text="Done.", tool_calls=[])
        nid = int(m.group(1))
        mode = self.cfg.get("dry_run_negotiation")
        if '"current_proposal": null' in self._text:
            mode = "reply"          # nothing on the table to accept or counter
        if not mode:
            gives = len(re.findall(r'"type"', self._text.split('"you_receive"')[0])) if '"you_receive"' in self._text else 0
            gets = self._text.count('"type"') - gives
            mode = "accept" if gets >= gives else "reject"
        args = {"negotiation_id": nid, "action": mode, "message": f"(dry run) {mode}"}
        if mode == "counter":
            args.update({"give": [], "receive": [{"type": "gold", "amount": 10}]})
        return StepResult(text=f"Dry run: {mode}.", tool_calls=[ToolCall("d_n", "respond_negotiation", args)])

    def _orders(self) -> list:
        """Make up orders that are legal but not clever."""
        try:
            units, cities, empire = (json.loads(r) for r in self._results[:3])
        except (ValueError, TypeError):
            return []
        calls = []
        settler = next((u for u in units if u.get("type") == "settler"), None)
        if settler and not cities:
            calls.append(ToolCall("d_f", "found_city", {"unit_id": settler["id"]}))
        for u in units:
            if u.get("class") not in ("civilian",) and u.get("type") != "settler" and not u.get("activity"):
                calls.append(ToolCall(f"d_x{u['id']}", "unit_order", {"unit_id": u["id"], "order": "explore"}))
        if not empire.get("research"):
            calls.append(ToolCall("d_r", "set_research", {"tech": "pottery"}))
        for c in cities:
            if not c.get("queue"):
                calls.append(ToolCall(f"d_p{c['id']}", "set_production", {"city_id": c["id"], "item": "warrior"}))
        return calls

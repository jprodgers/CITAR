"""Dry-run 'model' for testing benchmark setups without a GPU: founds a city, keeps research and production going,
sends military units exploring, then ends each turn. Makes no network calls.

It answers chats too (design/phase3-features.md P3.9.4: taught what the QA needs): each message of a chat's
conversation is answered once (``dry_run_negotiation``, or by counting what it gives and gets), a message that holds
``[dry run: wait N s]`` is answered after N seconds rather than the usual delay (a model too slow for the chat clock),
and with ``dry_run_propose_every`` it opens a trade (maps both ways) with the first civilization it has met on every
turn that many turns divide."""
from __future__ import annotations

import json
import re
import threading

from .base import Conversation, StepResult, ToolCall

#: The longest one step waits, whatever asks for more (``servers.DRY_RUN_MAX_DELAY``, which a machine's field holds to).
MAX_DELAY = 60.0
#: What the dry run writes when it is asked for text with no tools (a report's narrative).
DRY_NARRATIVE = ("(Dry run narrative.) This is where a model's written analysis of the figures would appear. The "
                 "computed findings and charts below are produced without a model.")


class DryRunConversation(Conversation):
    """A provider that answers plausibly with no model at all.

    It exists so the rest of the system can be tested end to end - suites, scheduling, probes, the
    usage ledger, the reports - without spending GPU time or tokens. It plays badly on purpose; its
    results never count toward a model score.
    """
    def __init__(self, cfg: dict, system: str, tools: list[dict]):
        self.cfg = cfg
        self.system = system or ""
        try:
            delay = float(cfg.get("dry_run_delay") if cfg.get("dry_run_delay") is not None else 1.0)
        except (TypeError, ValueError):
            delay = 1.0
        # held to the longest a machine's own delay may be (``servers.DRY_RUN_MAX_DELAY``): a seat's block, an
        # administrator's inline one or a saved game's, never a step of minutes that holds a responder
        self.delay = max(0.0, min(MAX_DELAY, delay)) if delay == delay else 1.0
        self.usage = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0}
        self._closed = threading.Event()
        self._step = 0
        self._results: list[str] = []
        self._tools = {t.get("name") for t in tools or []}
        self._text = ""
        self._chat = ""             # the chat's latest message (one naming its negotiation): what an answer reads
        self._pending = False       # that message is not answered yet

    def add_user_text(self, text: str):
        """Keep the text: a turn's proposal reads it (the turn's number), and a chat's answer reads the latest message
        that names its negotiation (a nudge to answer does not replace it)."""
        self._text += text
        if re.search(r"negotiation #\d+", text):
            self._chat = text
            self._pending = True

    def add_tool_results(self, results):
        """Ignore tool results."""
        self._results = [content for _, content, _ in results]

    def close(self):
        """Nothing to release."""
        self._closed.set()

    def _delay(self) -> float:
        """How long this step takes: the configured delay, or a chat message's ``[dry run: wait N s]``."""
        chat = "respond_negotiation" in self._tools and "end_turn" not in self._tools
        m = re.search(r"\[dry run: wait (\d+(?:\.\d+)?) s\]", self._chat) if chat and self._pending else None
        return min(MAX_DELAY, float(m.group(1))) if m else self.delay

    def step(self) -> StepResult:
        """Produce a plausible set of orders, after a configurable delay."""
        if self._closed.wait(self._delay()):
            raise RuntimeError("dry run cancelled")
        self._step += 1
        self.usage["input_tokens"] += 1000
        self.usage["output_tokens"] += 20
        if not self._tools:
            # a plain completion (a report's narrative): text, no orders
            return StepResult(text=DRY_NARRATIVE, stop_reason="stop")
        if "end_turn" not in self._tools and "DIPLOMAT PHASE" in self._text:
            return self._diplomat()
        if "respond_negotiation" in self._tools and "end_turn" not in self._tools:
            return self._negotiate()
        if self._step == 1:
            calls = [ToolCall("d_u", "get_units", {}), ToolCall("d_c", "get_cities", {}),
                     ToolCall("d_e", "get_empire", {})]
            if self._proposing():
                calls.append(ToolCall("d_pl", "get_players", {}))
            return StepResult(text="Checking units, cities and research.", tool_calls=calls)
        if self._step == 2:
            calls = self._orders()
            if calls:
                return StepResult(text="Giving orders.", tool_calls=calls)
        return StepResult(text="Nothing else to do.", tool_calls=[ToolCall(f"d_end{self._step}", "end_turn", {})])

    def _negotiate(self) -> StepResult:
        """A chat's answer: each message once, with cfg["dry_run_negotiation"] (accept | reject | counter | reply),
        else accepting a deal that gives it at least as many items as it gives (a reply when nothing is on the
        table)."""
        text = self._chat
        m = re.search(r"negotiation #(\d+)", text)
        if not m or not self._pending:
            return StepResult(text="Done.", tool_calls=[])
        self._pending = False
        nid = int(m.group(1))
        mode = self.cfg.get("dry_run_negotiation")
        if '"current_proposal": null' in text:
            mode = "reply"          # nothing on the table to accept or counter
        if not mode:
            table = text.split('"current_proposal"', 1)[1] if '"current_proposal"' in text else text
            table = table.split('"proposal_by_you"', 1)[0]
            gives = len(re.findall(r'"type"', table.split('"you_receive"')[0])) if '"you_receive"' in table else 0
            gets = table.count('"type"') - gives
            mode = "accept" if gets >= gives else "reject"
        args = {"negotiation_id": nid, "action": mode, "message": f"(dry run) {mode}"}
        if mode == "counter":
            args.update({"give": [], "receive": [{"type": "gold", "amount": 10}]})
        return StepResult(text=f"Dry run: {mode}.", tool_calls=[ToolCall("d_n", "respond_negotiation", args)])

    # ------------------------------------------------------------------ the dry-run diplomat (P3.6.6, P3.9.4)
    def _owned(self) -> set:
        """The categories the diplomat decides, read from its system prompt's lines (``prompts.CATEGORY_LINES``)."""
        names = {"trades": "- trades:", "agreements": "- agreements:", "peace": "- peace:", "war": "- war:",
                 "denounce": "- denunciations", "un": "- the United Nations vote", "city_states": "- city-states:",
                 "espionage": "- spies:", "captured_cities": "- the cities you capture", "chat": "- talk:"}
        return {c for c, line in names.items() if line in self.system}

    def _diplomat(self) -> StepResult:
        """A diplomat phase, played as the QA needs it ("a diplomat that proposes, votes and annexes"): it looks (the
        diplomacy, its empire, and what its categories let it see), then acts once in each category it decides where it
        can (a deal to a civilization it has met, an abstention when a United Nations vote is open, an annexation of a
        puppet it captured, a war, a denunciation, a gift of gold to a city-state, a spy moved home, a message),
        answers the chats the phase was told of, and stops by answering without a tool call."""
        self._dstep = getattr(self, "_dstep", 0) + 1
        if self._dstep == 1:
            calls = [ToolCall("dd_d", "get_diplomacy", {}), ToolCall("dd_e", "get_empire", {})]
            for name in ("get_cities", "get_city_states", "get_espionage"):
                if name in self._tools:
                    calls.append(ToolCall(f"dd_{name}", name, {}))
            return StepResult(text="Dry run diplomat: looking.", tool_calls=calls)
        if self._dstep == 2:
            calls = self._diplomacy_orders()
            if calls:
                return StepResult(text="Dry run diplomat: acting.", tool_calls=calls)
        answers = self._chat_answers()
        if answers:
            return StepResult(text="Dry run diplomat: answering.", tool_calls=answers)
        return StepResult(text="Dry run diplomat: done.", tool_calls=[])

    def _looked(self) -> dict:
        """What the first step's queries said, by tool (the results come back in the calls' order)."""
        names = ["get_diplomacy", "get_empire"] + [n for n in ("get_cities", "get_city_states", "get_espionage")
                                                   if n in self._tools]
        out = {}
        for name, text in zip(names, self._results):
            try:
                out[name] = json.loads(text)
            except (TypeError, ValueError):
                out[name] = None
        return out

    def _diplomacy_orders(self) -> list:
        """The diplomat's acts (``_diplomat``), from what it looked at."""
        seen = self._looked()
        dip = seen.get("get_diplomacy") if isinstance(seen.get("get_diplomacy"), dict) else {}
        empire = seen.get("get_empire") if isinstance(seen.get("get_empire"), dict) else {}
        owned = self._owned()
        tools = self._tools
        met = [p for p in dip.get("players") or [] if isinstance(p, dict) and p.get("alive") is not False]
        talking = {n.get("with") for n in dip.get("open_negotiations") or [] if isinstance(n, dict)}
        calls: list = []
        free = [p for p in met if p.get("id") not in talking]
        if "open_negotiation" in tools and free:
            deal = None
            at_war = [p for p in free if p.get("at_war")]
            if "peace" in owned and at_war:
                deal = (at_war[0], [{"type": "peace_treaty"}], [{"type": "peace_treaty"}], "Let us make peace.")
            elif "trades" in owned:
                p = next((q for q in free if not q.get("at_war")), None)
                if p is not None:
                    deal = (p, [{"type": "share_map"}], [{"type": "share_map"}], "Shall we share our maps?")
            elif "agreements" in owned:
                for p in free:
                    possible = ((p.get("trade_options") or {}).get("agreements_possible_now") or []) \
                        if not p.get("at_war") else []
                    kind = next((k for k in ("embassy", "declaration_of_friendship", "open_borders") if k in possible),
                                None)
                    if kind:
                        item = {"type": kind, **({"turns": 30} if kind == "open_borders" else {})}
                        deal = (p, [dict(item)], [dict(item)], "Shall we agree on this?")
                        break
            if deal is not None:
                p, give, receive, text = deal
                calls.append(ToolCall("dd_o", "open_negotiation", {"to": p["id"], "message": f"(dry run) {text}",
                                                                    "give": give, "receive": receive}))
        un = dip.get("united_nations") or {}
        if "un_vote" in tools and un.get("voting_open") and un.get("your_vote") == "not cast":
            # an abstention, cast: a vote for anyone could end the game with a diplomatic victory
            calls.append(ToolCall("dd_v", "un_vote", {"candidate": "abstain"}))
        cities = seen.get("get_cities") if isinstance(seen.get("get_cities"), list) else []
        puppet = next((c for c in cities if isinstance(c, dict) and c.get("puppet")), None)
        if "city_status" in tools and puppet is not None:
            calls.append(ToolCall("dd_a", "city_status", {"city_id": puppet.get("id"), "status": "annex"}))
        if "declare_war" in tools and not any(p.get("at_war") for p in met):
            target = next((p for p in met if not p.get("peace_treaty_until")), None)
            if target is not None:
                calls.append(ToolCall("dd_w", "declare_war", {"player_id": target["id"],
                                                               "message": "(dry run) War."}))
        if "denounce" in tools and met:
            calls.append(ToolCall("dd_n", "denounce", {"player_id": met[-1]["id"]}))
        states = seen.get("get_city_states") if isinstance(seen.get("get_city_states"), list) else []
        cs = next((c for c in states if isinstance(c, dict) and c.get("id") is not None), None)
        if "city_state_action" in tools and cs is not None and (empire.get("gold") or 0) >= 100:
            calls.append(ToolCall("dd_g", "city_state_action", {"player_id": cs["id"], "action": "gift_gold",
                                                                 "amount": 100}))
        spies = (seen.get("get_espionage") or {}).get("spies") if isinstance(seen.get("get_espionage"), dict) else []
        spy = next((s for s in spies or [] if isinstance(s, dict) and s.get("city_id") is None), None)
        if "move_spy" in tools and spy is not None and empire.get("capital") is not None:
            calls.append(ToolCall("dd_s", "move_spy", {"spy": spy.get("name"), "city_id": empire["capital"]}))
        if "send_message" in tools and met:
            calls.append(ToolCall("dd_m", "send_message", {"to": met[0]["id"], "text": "(dry run) Greetings."}))
        return calls + self._chat_answers()

    def _chat_answers(self) -> list:
        """An answer to each chat the phase was told of since its last answers (``CHAT: ... negotiation #n``), as a
        chat's answer would be (``dry_run_negotiation``, else accepting when it gets as many items as it gives)."""
        if "respond_negotiation" not in self._tools or not self._pending:
            return []
        self._pending = False
        out = []
        for i, m in enumerate(re.finditer(r"CHAT: .*? negotiation #(\d+): (.*)", self._chat)):
            nid = int(m.group(1))
            mode = self.cfg.get("dry_run_negotiation") or ("reply" if "none (no deal on the table)" in m.group(2)
                                                           else "accept")
            if mode == "counter":
                mode = "reject"
            out.append(ToolCall(f"dd_c{i}", "respond_negotiation", {"negotiation_id": nid, "action": mode,
                                                                    "message": f"(dry run) {mode}"}))
        return out

    def _proposing(self) -> bool:
        """Whether this turn opens a trade (``dry_run_propose_every``: on every turn that many turns divide)."""
        try:
            every = int(self.cfg.get("dry_run_propose_every") or 0)
        except (TypeError, ValueError):
            every = 0
        m = re.search(r"It is your turn \(turn (\d+)\)", self._text)
        return every > 0 and m is not None and int(m.group(1)) % every == 0 and "open_negotiation" in self._tools

    def _orders(self) -> list:
        """Make up orders that are legal but not clever, and a trade on a proposing turn (``_proposing``)."""
        try:
            units, cities, empire = (json.loads(r) for r in self._results[:3])
        except (ValueError, TypeError):
            return []
        calls = []
        if self._proposing() and len(self._results) > 3:
            try:
                players = json.loads(self._results[3])
            except (ValueError, TypeError):
                players = []
            met = [p for p in players if isinstance(p, dict) and p.get("met") and p.get("alive")
                   and p.get("kind") == "major" and p.get("id") != empire.get("id")
                   and p.get("name") != empire.get("name")] if isinstance(players, list) else []
            if met:
                calls.append(ToolCall("d_o", "open_negotiation", {
                    "to": met[0]["id"], "message": "(dry run) Shall we share our maps?",
                    "give": [{"type": "share_map"}], "receive": [{"type": "share_map"}]}))
        # the engine names unit types as the ruleset does ("Settler")
        settler = next((u for u in units if str(u.get("type") or "").lower() == "settler"), None)
        if settler and not cities:
            calls.append(ToolCall("d_f", "found_city", {"unit_id": settler["id"]}))
        for u in units:
            if u.get("class") not in ("civilian",) and u is not settler and not u.get("activity"):
                calls.append(ToolCall(f"d_x{u['id']}", "unit_order", {"unit_id": u["id"], "order": "explore"}))
        if not empire.get("researching"):
            calls.append(ToolCall("d_r", "set_research", {"tech": "pottery"}))
        for c in cities:
            if not c.get("queue"):
                calls.append(ToolCall(f"d_p{c['id']}", "set_production", {"city_id": c["id"], "item": "warrior"}))
        return calls

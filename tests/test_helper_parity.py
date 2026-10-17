"""One parser for every completion, through a helper too (design/phase4-helper.md P4.5.2; package 4-05's gate 2).

4-02's corpus (``tests/data/completions``: raw completions in the shape of protocol 2's ``done``, each with what
``OpenAIConversation`` made of it) is played through ``WorkerConversation`` against the real hub: a fake helper over a
real socket answers each request with the case's raw message, finish reason and usage, and the step and the stored
assistant entry must be the ones ``OpenAIConversation`` gave. The request side is held too: in native mode the
case's tools go as OpenAI function definitions, in ``json`` mode none go and the first message is the system prompt
with the JSON protocol and the tool listing, byte for byte the one ``OpenAIConversation`` sent at 8e7db41
(``tools.json``'s ``json_prompts``). Protocol 1's worker never sent that protocol to the model: its helper parsed the
reply itself (the plan's defect "JSON tool mode is broken through helpers"), which this test fails on.

Usage maps as P4.5.2 says (``usage_of``): cached prompt tokens move to ``cache_read_input_tokens``, out of
``input_tokens``.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import os
import tempfile
import unittest
from pathlib import Path

from citar import db, servers, settings
from citar.agents.providers import completion
from citar.agents.providers.worker_provider import WorkerConversation, usage_of
from citar.auth import tokens
from citar.db.models import Base, Server, WorkerToken
from citar.pool import scheduler as S
from citar.server import workers as W
from tests import helperfake as H
from tests.test_completions import CASES, PROMPTS, TOOLSETS, given_ids, normalized, step_dict

_TMP = Path(tempfile.mkdtemp(prefix="citar_parity_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()


def setUpModule():
    os.environ["CITAR_DATA_DIR"] = str(_TMP)
    os.environ["CITAR_DB_URL"] = _URL
    settings.reset()
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()
    S.reset()


class ThroughAHelper(unittest.TestCase):

    @classmethod
    def setUpClass(cls):
        cls.server = H.HubServer()
        with db.session() as s:
            row = Server(owner_id=None, name="parity box", kind="owned", provider="lmstudio", reach="worker",
                         max_concurrent=1, config={"models": [{"key": "m"}]})
            s.add(row)
            s.flush()
            raw, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=row.id, token_hash=hashed, prefix=tokens.prefix(raw), label="parity"))
            cls.sid = row.id
        servers.invalidate()
        cls.by_name = {c["name"]: c for c in CASES}
        helper = H.FakeHelper(cls.server.url)
        helper.handshake(H.hello(token=raw, models=[H.model("m")]))
        cls.helper = helper
        cls.served = H.Responder(helper, cls.answer)
        if not H.until(lambda: W.hub().get(cls.sid) is not None and W.hub().get(cls.sid).ack is not None, 10):
            raise RuntimeError("the fake helper was not welcomed")

    @classmethod
    def tearDownClass(cls):
        cls.helper.close()
        cls.server.stop()
        S.reset()

    @classmethod
    def answer(cls, frame: dict) -> dict:
        """The case the request names (its last message's text) answered raw, as its endpoint gave it."""
        c = cls.by_name[frame["messages"][-1]["content"]]["completion"]
        return H.done_fields(frame, message=c["message"], finish_reason=c["finish_reason"], usage=c["usage"])

    def conversation(self, case: dict, system: str = "sys") -> WorkerConversation:
        conv = WorkerConversation({"server_id": self.sid, "model": "m", "tool_mode": case["mode"], "timeout": 30},
                                  system, TOOLSETS[case["tools"]])
        conv.add_user_text(case["name"])
        return conv

    def test_every_case_parses_as_through_openai_conversation(self):
        self.assertGreaterEqual(len(CASES), 59)
        for case in CASES:
            with self.subTest(case=case["name"]):
                conv = self.conversation(case)
                before = len(conv.messages)
                result = conv.step()
                self.assertEqual(len(conv.messages), before + 1)
                step, entry = normalized(step_dict(result), conv.messages[-1], given_ids(case))
                self.assertEqual(step, case["expect"]["step"])
                self.assertEqual(entry, case["expect"]["entry"])
                request = self.served.requests[-1]
                tools = completion.openai_tools(TOOLSETS[case["tools"]])
                if case["mode"] == "native" and tools:
                    self.assertEqual(request["tools"], tools)
                else:
                    self.assertIsNone(request["tools"], "json mode, or a plain completion, offers no tools")
                usage = case["completion"]["usage"]
                want = usage_of(usage)
                self.assertEqual({k: conv.usage.get(k, 0) for k in want}, want)

    def test_a_json_mode_seats_request_carries_the_protocol_and_the_listing(self):
        """The plan's defect: through a protocol 1 helper a json-mode model never got the JSON protocol."""
        for name in ("small", "game"):
            with self.subTest(tools=name):
                case = {"name": "json-fenced", "mode": "json", "tools": name}
                conv = self.conversation(case, system=PROMPTS["system"])
                conv.step()
                request = self.served.requests[-1]
                self.assertEqual(request["messages"][0], {"role": "system", "content": PROMPTS[name]})
                self.assertIn(completion.JSON_PROTOCOL, request["messages"][0]["content"])
                self.assertIsNone(request["tools"])

    def test_usage_moves_cached_prompt_tokens_out_of_the_input(self):
        self.assertEqual(usage_of({"prompt_tokens": 120, "completion_tokens": 14, "reasoning_tokens": 3,
                                   "cached_tokens": 64}),
                         {"input_tokens": 56, "output_tokens": 14, "reasoning_tokens": 3,
                          "cache_read_input_tokens": 64})
        self.assertEqual(usage_of({"prompt_tokens": 10, "completion_tokens": 2, "reasoning_tokens": None,
                                   "cached_tokens": None}),
                         {"input_tokens": 10, "output_tokens": 2, "reasoning_tokens": 0})
        self.assertEqual(usage_of(None), {})

    def test_seat_parameters_are_brought_within_the_protocol(self):
        """4-01's note: a seat's saved overrides go through ``request_params`` (temperature 3, max_tokens 0 and "4096"
        as text), the names changed logged once per conversation."""
        case = self.by_name["native-one-call"]
        conv = WorkerConversation({"server_id": self.sid, "model": "m", "tool_mode": "native", "timeout": 30,
                                   "temperature": 3, "max_tokens": "4096", "top_p": "0.5", "seed": -1},
                                  "sys", TOOLSETS[case["tools"]])
        conv.add_user_text(case["name"])
        with self.assertLogs("citar.worker_provider", level="INFO") as logged:
            conv.step()
            conv.add_user_text(case["name"])
            conv.cfg["max_tokens"] = 0
            conv.step()
        self.assertEqual(self.served.requests[-2]["params"], {"temperature": 2.0, "max_tokens": 4096, "top_p": 0.5})
        self.assertEqual(self.served.requests[-1]["params"], {"temperature": 2.0, "top_p": 0.5})
        self.assertEqual(len([line for line in logged.output if "sampling parameters" in line]), 1)


if __name__ == "__main__":
    unittest.main()
